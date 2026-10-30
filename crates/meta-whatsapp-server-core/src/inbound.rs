//! Meta's webhook deliveries, from a signed body to the inbox and the event
//! outbox (docs/design/server.md, sections 2.3 and 6): the pipeline behind
//! the service's `POST /webhooks/meta`, with no HTTP framework (roadmap
//! S5e). An API adapter keeps what an HTTP body needs (refusing a request
//! whose `X-Hub-Signature-256` is missing or misshapen before a byte of it
//! is read, its places among the deliveries read at once, the body's size
//! and how long it may take to arrive) and hands the body it read, with
//! that header's value, to [`Pipeline::deliver`], which answers what to
//! tell Meta: `200` for [`Delivered`], else [`Refused::status`]. What an
//! event becomes (its route, its outbox row, its key and id) is
//! [`crate::events`]'.
//!
//! ```text
//! Pipeline::deliver(signature, body)
//!   1. the signature, against every app secret: Refused::Unauthenticated (401),
//!      before a turn, a parse or any store is touched
//!   2. a turn to record: MAX_DELIVERIES_RECORDING at once, within RECORDING_WAIT,
//!      else Refused::Busy (503)
//!   3. the library's WebhookHandler: 3 MiB (Refused::TooLarge, 413), the
//!      signature, parse (a signed body that is not a webhook: one `unparsed`
//!      event, acknowledged), then per event: its DedupGuard lease on the
//!      backend's KvStore (Refused::InFlight, 503, while another request holds
//!      it; a duplicate skipped) ─► the service's sink:
//!        a. route: the tenant owning the number, else the WABA, since before the
//!           event, by the backend's records (crate::events::route)
//!        b. the inbox (the facade's InboxSink), when a tenant owns it
//!        c. the outbox row: that tenant, or none (operator-only); the outbox
//!           checks the routing again, atomically with the insert
//!      then the lease marked done (released when the sink failed)
//!   4. every event recorded, or already: Delivered (200)
//! ```
//!
//! **Routing is an allow-list** ([`crate::events`]): an event belongs to the
//! tenant bound to its number (else its WABA) since before Meta dated it,
//! by the backend's records and nothing the payload says of a tenant, and
//! its outbox row carries that tenant only for the types in
//! [`crate::events::TENANT_EVENT_TYPES`]; every other event is an
//! operator-only row (no tenant: never polled, logged with its size and
//! digest, counted), and the inbox records only an owned event.
//!
//! **Meta's retries are safe.** A sink error is [`Refused::Failed`]
//! (`500`), the dedup claim is released, and Meta redelivers the batch:
//! the events before it are duplicates, the failed one runs again. Both
//! writes are idempotent: the inbox stores a message id once and moves
//! statuses forward only, and the outbox inserts on the event's key at
//! most once. So a failure between the inbox write and the outbox write (a
//! database error: `500`; a crash or the request's deadline: the claim's
//! lease ends after 60 s) leaves the message in the inbox and no row, and
//! the redelivery records the row without a second message. The events
//! the library gives no dedup key (`error_reported`, `unparsed`) are keyed
//! by the signed body they came in and their position in it
//! ([`EventKey::Delivery`]). Meta documents a retry at once, then with
//! decreasing frequency over 7 days (`webhooks/create-webhook-endpoint`);
//! that a redelivery carries the same bytes is assumed, not documented. On
//! that assumption a batch redelivered within
//! [`crate::events::KEYLESS_DEDUP_WINDOW`] records them once too. The
//! library never deduplicates them (the same error recurs, and carries no
//! date): an identical body after the window is recorded again, as a new
//! occurrence with an id of its own, so an outage longer than the window
//! (the database answering `500`) records the batch's keyless events
//! twice.
//!
//! **Logs.** The pipeline's own lines keep the target the service's
//! operators have always filtered on, `meta_whatsapp_server::events`
//! ([`LOG_TARGET`]), with the same fields; a refused delivery is the
//! adapter's to log (the service writes at most one line a minute per
//! reason), so nothing here logs one. Nothing derived from an event's
//! content is logged: an operator-only row's size and SHA-256 only.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use meta_whatsapp_rs::core::clock::{Clock, SystemClock};
use meta_whatsapp_rs::core::error::{ConfigError, SinkError, StorageError, WebhookError};
use meta_whatsapp_rs::core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_rs::core::sink::EventSink;
use meta_whatsapp_rs::inbox::InboxSink;
use meta_whatsapp_rs::webhooks::{
    DEFAULT_MAX_BODY_BYTES, DedupGuard, DeliveryReport, SignatureVerifier, WebhookEvent,
    WebhookHandler,
};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::backend::Backend;
use crate::events::{EventIdKey, EventKey, outbox_row, route};
use crate::observe::Observer;
use crate::outbox::Outbox;
use crate::store::RecordStore;

/// Largest webhook body read: 3 MiB, the library's default (Meta documents
/// payloads of up to 3 MB, `webhooks/overview`). One byte more is
/// [`Refused::TooLarge`]; an adapter refuses it as it reads (`413`).
pub const MAX_WEBHOOK_BODY_BYTES: usize = DEFAULT_MAX_BODY_BYTES;

/// Deliveries a [`Pipeline`] records at once: each holds at most one
/// database connection at a time, so the webhook path never takes more
/// than these of a replica's pool, and API calls (key lookups first)
/// always find one (security review M2). Below the pool's size (the
/// server's `serve`).
pub const MAX_DELIVERIES_RECORDING: usize = 4;

/// How long a delivery waits for its turn to record: past it,
/// [`Refused::Busy`] (`503`).
pub const RECORDING_WAIT: Duration = Duration::from_secs(10);

/// The target of the pipeline's log lines: the service's own, where they
/// were written from before roadmap S5e moved them here, so that a
/// `RUST_LOG` directive or a log query naming it still finds them.
pub const LOG_TARGET: &str = "meta_whatsapp_server::events";

/// What the webhook pipeline checks Meta's deliveries with, and reads "now"
/// from. Its stores are not here: the pipeline is built on the backend the
/// service runs on ([`Pipeline::new`]), its dedup leases in the bundle's
/// `KvStore`, the inbox in its `ConversationStore`, the events in its
/// `Outbox`, routed by its `RecordStore`, so that no store of another
/// database is mixed in.
pub struct Inbound {
    verifier: SignatureVerifier,
    ids: EventIdKey,
    verify_token: VerifyToken,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Inbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The verifier's `Debug` counts the secrets and shows none; the
        // event id key and the verify token are not shown at all.
        f.debug_struct("Inbound")
            .field("verifier", &self.verifier)
            .finish_non_exhaustive()
    }
}

impl Inbound {
    /// Verify deliveries against `app_secrets` (the active one, then the
    /// previous one while rotating), and Meta's subscription handshake
    /// (`GET /webhooks/meta`) against `verify_token`. Event ids are
    /// derived from the first app secret ([`EventIdKey`]): after a
    /// rotation, an event recorded again (its row purged) gets another id.
    ///
    /// # Errors
    ///
    /// No app secret, or a blank one (the library's `SignatureVerifier`
    /// refuses them: an HMAC under a blank key is anyone's).
    pub fn new(
        app_secrets: Vec<AppSecret>,
        verify_token: VerifyToken,
    ) -> meta_whatsapp_rs::Result<Self> {
        let ids = app_secrets
            .first()
            .and_then(EventIdKey::from_app_secret)
            .ok_or_else(|| ConfigError::new("the webhook pipeline needs an app secret"))?;
        Ok(Self {
            verifier: SignatureVerifier::new(app_secrets)?,
            ids,
            verify_token,
            clock: Arc::new(SystemClock),
        })
    }

    /// Read "now" from `clock` (the system clock by default): the replay
    /// window and [`crate::events::KEYLESS_DEDUP_WINDOW`] are measured on
    /// it, and the service's authorization (a key's or a stored token's
    /// expiry) too: it is the service's one clock (the server's
    /// `AppState` hands it to the core's `Authorizer`).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The service's clock.
    pub fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }
}

/// What a recorded delivery did: the library's report. Answer `200`.
pub type Delivered = DeliveryReport;

/// Why a delivery was not recorded, and what to answer Meta, which retries
/// anything but `200`.
#[derive(Debug)]
pub enum Refused {
    /// No app secret produced the signature (or there was none, or it was
    /// malformed): `401`, before a turn, a parse or a store.
    Unauthenticated,
    /// No turn to record within [`RECORDING_WAIT`], or the outbox gave up
    /// waiting for another writer (`StorageError::Busy`): `503`, Meta
    /// retries.
    Busy,
    /// The body is over [`MAX_WEBHOOK_BODY_BYTES`]: `413`.
    TooLarge,
    /// Another request holds one of its events' dedup lease (another
    /// replica is delivering it): `503`, Meta retries.
    InFlight,
    /// Recording failed (a store, the inbox, the outbox): `500`, the
    /// event's lease released, and Meta redelivers the batch.
    Failed(meta_whatsapp_rs::Error),
}

impl Refused {
    /// The HTTP status to answer Meta with.
    pub fn status(&self) -> u16 {
        match self {
            Self::Unauthenticated => 401,
            Self::Busy | Self::InFlight => 503,
            Self::TooLarge => 413,
            Self::Failed(_) => 500,
        }
    }

    /// The outcome's label for the adapter's metrics (the service's
    /// `wa_server_webhook_deliveries_total`): `unauthenticated`, `busy`,
    /// `payload_too_large`, `in_flight` or `failed`.
    pub fn outcome(&self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::Busy => "busy",
            Self::TooLarge => "payload_too_large",
            Self::InFlight => "in_flight",
            Self::Failed(_) => "failed",
        }
    }

    /// The library handler's error, as what to answer.
    fn of(error: meta_whatsapp_rs::Error) -> Self {
        match error {
            meta_whatsapp_rs::Error::Webhook(
                WebhookError::MissingSignature
                | WebhookError::MalformedSignature
                | WebhookError::SignatureMismatch,
            ) => Self::Unauthenticated,
            meta_whatsapp_rs::Error::Webhook(WebhookError::PayloadTooLarge { .. }) => {
                Self::TooLarge
            }
            meta_whatsapp_rs::Error::Webhook(WebhookError::ClaimInFlight) => Self::InFlight,
            // The outbox gave up waiting for another writer
            // (`StorageError::Busy`, which the sink reports as full).
            meta_whatsapp_rs::Error::Sink(SinkError::Full) => Self::Busy,
            error => Self::Failed(error),
        }
    }
}

/// The outcome label of a recorded delivery, for the adapter's metrics
/// (beside [`Refused::outcome`]'s).
pub const DELIVERED: &str = "delivered";

/// The webhook pipeline over one backend's stores: Meta's deliveries
/// verified, deduplicated, routed and recorded ([module](self)).
pub struct Pipeline {
    verifier: SignatureVerifier,
    verify_token: VerifyToken,
    dedup: DedupGuard,
    sink: ServiceSink,
    outbox: Arc<dyn Outbox>,
    observer: Arc<dyn Observer>,
    /// Turns to record ([`MAX_DELIVERIES_RECORDING`]).
    recording: Semaphore,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("verifier", &self.verifier)
            .finish_non_exhaustive()
    }
}

impl Pipeline {
    /// The pipeline `inbound` describes, on `backend`'s stores alone:
    /// dedup leases in its `KvStore`, the inbox in its `ConversationStore`,
    /// the events in its `Outbox`, routed by its `RecordStore` (the records
    /// of which tenant holds which number and WABA). What it counts is told
    /// to `observer`.
    pub fn new(inbound: Inbound, backend: &dyn Backend, observer: Arc<dyn Observer>) -> Self {
        let outbox = backend.outbox();
        let dedup = DedupGuard::new(backend.kv());
        let sink = ServiceSink {
            store: backend.records(),
            inbox: InboxSink::new(backend.conversations()),
            outbox: outbox.clone(),
            observer: observer.clone(),
            ids: inbound.ids,
            replay_window: dedup.ttl(),
            clock: inbound.clock,
        };
        Self {
            verifier: inbound.verifier,
            verify_token: inbound.verify_token,
            dedup,
            sink,
            outbox,
            observer,
            recording: Semaphore::new(MAX_DELIVERIES_RECORDING),
        }
    }

    /// One of Meta's deliveries: `body` as it arrived, `signature` its
    /// `X-Hub-Signature-256` header's value. The signature is checked
    /// first, against every app secret, before a turn to record is taken
    /// or any store touched; then, at most [`MAX_DELIVERIES_RECORDING`] at
    /// once, the library's `WebhookHandler` (size, parse, each event under
    /// its dedup lease) hands each event to the service's sink, which
    /// routes and records it ([module](self)). The answer comes once every
    /// event is recorded, or was already: never before.
    ///
    /// # Errors
    ///
    /// [`Refused`], with the status to answer.
    pub async fn deliver(
        &self,
        signature: Option<&str>,
        body: Bytes,
    ) -> Result<Delivered, Refused> {
        // The signature first, here: the library's handler would log each
        // forged delivery (the adapter logs them at most once a minute), and
        // a forged one needs no turn to record.
        // Its error is a signature's (`Refused::Unauthenticated`), classified
        // as the handler's errors are.
        if let Err(error) = self.verifier.verify(signature, &body) {
            return Err(Refused::of(error.into()));
        }
        // Its turn to record: at most MAX_DELIVERIES_RECORDING hold the
        // database at once.
        let Ok(Ok(_turn)) = tokio::time::timeout(RECORDING_WAIT, self.recording.acquire()).await
        else {
            return Err(Refused::Busy);
        };
        // The handler is the delivery's own: its sink knows the signed body,
        // which keys the events that have no dedup key of their own.
        let sink = DeliverySink {
            sink: self.sink.clone(),
            body: body.clone(),
            body_sha256: OnceLock::new(),
            keyless: AtomicUsize::new(0),
        };
        let handler = WebhookHandler::builder(
            self.verifier.clone(),
            self.verify_token.clone(),
            Arc::new(sink),
        )
        .dedup(self.dedup.clone())
        .max_body_bytes(MAX_WEBHOOK_BODY_BYTES)
        .build();
        let report = handler
            .deliver(signature, &body)
            .await
            .map_err(Refused::of)?;
        if report.duplicates > 0 {
            self.observer.webhook_duplicates(
                "dedup",
                u64::try_from(report.duplicates).unwrap_or(u64::MAX),
            );
        }
        Ok(report)
    }

    /// The outbox the events are recorded in (what polling reads).
    pub fn outbox(&self) -> &dyn Outbox {
        self.outbox.as_ref()
    }

    /// Meta's webhook verify token, for the subscription handshake
    /// (`GET /webhooks/meta`: the library's `verify_subscription`).
    pub fn verify_token(&self) -> &VerifyToken {
        &self.verify_token
    }
}

/// One delivery's view of [`ServiceSink`]: the library's handler hands it
/// the body's events in order, and it keys each one ([`EventKey`]). The
/// library delivers every keyless event of a body (it has no dedup lease
/// to skip one by), in order, so their positions are the same on every
/// delivery of the body.
struct DeliverySink {
    sink: ServiceSink,
    body: Bytes,
    /// Computed at the first keyless event: after the signature checked
    /// out, and only for bodies that need it.
    body_sha256: OnceLock<[u8; 32]>,
    keyless: AtomicUsize,
}

impl std::fmt::Debug for DeliverySink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The body carries customers' messages and numbers.
        f.debug_struct("DeliverySink")
            .field("body_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl EventSink<WebhookEvent> for DeliverySink {
    async fn deliver(&self, event: WebhookEvent) -> Result<(), SinkError> {
        let key = match event.dedup_key() {
            Some(key) => EventKey::Library(key),
            None => EventKey::Delivery {
                body_sha256: *self
                    .body_sha256
                    .get_or_init(|| Sha256::digest(&self.body).into()),
                position: self.keyless.fetch_add(1, Ordering::Relaxed),
            },
        };
        self.sink.record(event, &key).await
    }
}

/// What the service does with an event: route, then the inbox, then the
/// outbox, in that order (docs/design/server.md, section 2.3), so whoever
/// sees an event can already read its history. Each delivery reaches it
/// through a sink of its own, which keys its events ([`EventKey`]).
/// Crate-private: an event reaches the stores only through
/// [`Pipeline::deliver`], after its body's signature.
#[derive(Clone)]
struct ServiceSink {
    store: Arc<dyn RecordStore>,
    inbox: InboxSink,
    outbox: Arc<dyn Outbox>,
    observer: Arc<dyn Observer>,
    ids: EventIdKey,
    /// Events Meta dated longer ago than this are replays: the dedup
    /// lease's memory (`DedupGuard::ttl`), past which a captured body would
    /// otherwise be recorded again.
    replay_window: Duration,
    /// "Now", for the replay window and
    /// [`crate::events::KEYLESS_DEDUP_WINDOW`].
    clock: Arc<dyn Clock>,
}

impl ServiceSink {
    /// Count and log a failed stage; the error the handler logs and
    /// answers `500` for.
    fn failed(&self, stage: &'static str, kind: &'static str, error: SinkError) -> SinkError {
        self.observer.webhook_failure(stage);
        tracing::warn!(
            target: LOG_TARGET,
            stage,
            event_type = kind,
            "recording a webhook event failed"
        );
        error
    }

    /// Record `event`, keyed `key`: route it, then the inbox, then the
    /// outbox.
    async fn record(&self, event: WebhookEvent, key: &EventKey) -> Result<(), SinkError> {
        let kind = event.kind();
        let now = self.clock.now();
        let not_before = now - self.replay_window;
        let route = route(self.store.as_ref(), &event, not_before)
            .await
            .map_err(|e| self.failed("routing", kind, storage(e)))?;
        // The row: its data, key and id (`crate::events::outbox_row`).
        let row = outbox_row(&event, key, &route, now, &self.ids).map_err(|e| {
            // serde's message could quote content: `RowError` says its
            // category only.
            self.failed(
                "serialization",
                kind,
                SinkError::Delivery(anyhow::Error::msg(e.to_string())),
            )
        })?;
        if route.owner.is_some() {
            self.inbox
                .deliver(event)
                .await
                .map_err(|e| self.failed("inbox", kind, e))?;
        }
        let inserted = self.outbox.insert(&row).await.map_err(|e| {
            // Contention, typed by the backend (the port's contract): Meta
            // retries a `503`.
            if e.is_busy() {
                self.observer.webhook_failure("outbox_busy");
                SinkError::Full
            } else {
                self.failed("outbox", kind, storage(e))
            }
        })?;
        let Some(sequence) = inserted else {
            self.observer.webhook_duplicates("outbox", 1);
            tracing::debug!(
                target: LOG_TARGET,
                event_type = kind,
                "the event was already in the outbox"
            );
            return Ok(());
        };
        if let Some(tenant) = &route.tenant {
            self.observer.webhook_event(kind, "tenant");
            tracing::debug!(
                target: LOG_TARGET,
                event_type = kind,
                sequence,
                tenant = tenant.as_str(),
                "event recorded"
            );
        } else {
            self.observer.webhook_event(kind, "operator");
            tracing::info!(
                target: LOG_TARGET,
                event_type = kind,
                sequence,
                reason = route.operator_only.unwrap_or("unowned"),
                data_bytes = row.data.len(),
                data_sha256 = %hex::encode(Sha256::digest(row.data.as_bytes())),
                "operator-only event recorded"
            );
        }
        Ok(())
    }
}

/// A store's failure, as the sink reports it.
fn storage(error: StorageError) -> SinkError {
    SinkError::Delivery(anyhow::Error::new(error))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a panic is the report
mod tests {
    use super::*;

    /// What each refusal answers Meta, and the label it is counted under:
    /// the service's `wa_server_webhook_deliveries_total` outcomes
    /// (docs/design/server.md, section 2.3). Decisive: each status and
    /// label.
    #[test]
    fn each_refusal_answers_its_status_under_its_label() {
        let failed = Refused::Failed(meta_whatsapp_rs::Error::Sink(SinkError::Closed));
        let cases = [
            (Refused::Unauthenticated, 401, "unauthenticated"),
            (Refused::Busy, 503, "busy"),
            (Refused::TooLarge, 413, "payload_too_large"),
            (Refused::InFlight, 503, "in_flight"),
            (failed, 500, "failed"),
        ];
        for (refused, status, outcome) in cases {
            assert_eq!((refused.status(), refused.outcome()), (status, outcome));
        }
        assert_eq!(DELIVERED, "delivered");
    }

    /// The library handler's errors, as what to answer: a signature error
    /// of any kind is `401`, the body's size `413`, a held lease `503`
    /// (`in_flight`), a busy outbox (the sink full) `503` (`busy`), and
    /// anything else `500`. Decisive: each arm.
    #[test]
    fn the_handlers_errors_are_answered_as_before() {
        use meta_whatsapp_rs::Error;
        for error in [
            WebhookError::MissingSignature,
            WebhookError::MalformedSignature,
            WebhookError::SignatureMismatch,
        ] {
            assert!(matches!(
                Refused::of(Error::Webhook(error)),
                Refused::Unauthenticated
            ));
        }
        assert!(matches!(
            Refused::of(Error::Webhook(WebhookError::PayloadTooLarge {
                size: MAX_WEBHOOK_BODY_BYTES + 1,
                limit: MAX_WEBHOOK_BODY_BYTES,
            })),
            Refused::TooLarge
        ));
        assert!(matches!(
            Refused::of(Error::Webhook(WebhookError::ClaimInFlight)),
            Refused::InFlight
        ));
        assert!(matches!(
            Refused::of(Error::Sink(SinkError::Full)),
            Refused::Busy
        ));
        assert!(matches!(
            Refused::of(Error::Sink(SinkError::Closed)),
            Refused::Failed(Error::Sink(SinkError::Closed))
        ));
        assert!(matches!(
            Refused::of(Error::Storage(StorageError::Busy)),
            Refused::Failed(Error::Storage(StorageError::Busy))
        ));
    }

    /// The numbers docs/design/server.md (sections 2.3 and 6) and the
    /// guide state, and the log target the service's operators filter on.
    /// Decisive: each value.
    #[test]
    fn the_pipelines_numbers_and_target_are_the_designs() {
        assert_eq!(MAX_WEBHOOK_BODY_BYTES, 3 * 1024 * 1024, "3 MiB");
        assert_eq!(MAX_DELIVERIES_RECORDING, 4);
        assert_eq!(RECORDING_WAIT, Duration::from_secs(10));
        assert_eq!(LOG_TARGET, "meta_whatsapp_server::events");
    }

    /// No pipeline without an app secret, or with a blank one; and the
    /// `Debug` of what holds them shows none.
    #[test]
    fn a_blank_or_missing_app_secret_builds_no_inbound() {
        let token = || VerifyToken::new("verify-token");
        assert!(Inbound::new(Vec::new(), token()).is_err());
        assert!(Inbound::new(vec![AppSecret::new("")], token()).is_err());
        let inbound = Inbound::new(vec![AppSecret::new("s3cr3t-app-secret")], token()).unwrap();
        let shown = format!("{inbound:?}");
        assert!(!shown.contains("s3cr3t-app-secret"), "{shown}");
        assert!(!shown.contains("verify-token"), "{shown}");
    }
}
