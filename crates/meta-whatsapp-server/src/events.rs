//! Meta's webhooks into the inbox and the event outbox, and polling the
//! outbox (docs/design/server.md, sections 2.3 and 4.2): the pipeline
//! behind `POST /webhooks/meta` (limits, the library's `WebhookHandler`, a
//! sink). What an event becomes (its route, its outbox row, key and id)
//! and polling are the core's [`meta_whatsapp_server_core::events`],
//! re-exported here; the HTTP handlers (`api::webhooks`, `api::events`)
//! only translate.
//!
//! ```text
//! POST /webhooks/meta ─► WebhookHandler (the library): 3 MiB, X-Hub-Signature-256
//!                        against every app secret (401 before parsing), parse,
//!                        per event: DedupGuard lease (Postgres KvStore, 503 while
//!                        another request holds it) ─► ServiceSink:
//!                          1. route: the tenant owning the number, else the WABA,
//!                             since before the event
//!                          2. inbox (InboxSink), when a tenant owns it
//!                          3. outbox row: the tenant, or none (operator-only)
//! GET /v1/events ─► poll: the caller's tenant's rows after a sequence of its own
//! ```
//!
//! **Routing is an allow-list** (the rules, and the event types a tenant
//! receives, are the core's: [`meta_whatsapp_server_core::events`], whose
//! items this module re-exports): an event belongs to the tenant bound to
//! its number (else its WABA) since before Meta dated it, and its outbox
//! row carries that tenant only for the types in [`TENANT_EVENT_TYPES`];
//! every other event is an operator-only row (no tenant: never polled,
//! logged with size and digest, counted), and the inbox records only an
//! owned event.
//!
//! **Meta's retries are safe.** A sink error answers `500`, the dedup claim
//! is released, and Meta redelivers the batch: the events before it are
//! duplicates, the failed one runs again. Both writes are idempotent: the
//! inbox stores a message id once and moves statuses forward only, and the
//! outbox inserts on the event's key at most once. So a failure between
//! the inbox write and the outbox write (a database error: `500`; a crash
//! or the request deadline: the claim's lease ends after 60 s) leaves the
//! message in the inbox and no row, and the redelivery records the row
//! without a second message. The events the library gives no dedup key
//! (`error_reported`, `unparsed`) are keyed by the signed body they came in
//! and their position in it ([`EventKey::Delivery`]). Meta documents a
//! retry at once, then with decreasing frequency over 7 days
//! (`webhooks/create-webhook-endpoint`); that a redelivery carries the same
//! bytes is assumed, not documented. On that assumption a batch redelivered
//! within [`KEYLESS_DEDUP_WINDOW`] records them once too. The library never
//! deduplicates them (the same error recurs, and carries no date): an
//! identical body after the window is recorded again, as a new occurrence
//! with an id of its own, so an outage longer than the window (the
//! database answering `500`) records the batch's keyless events twice.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::core::clock::{Clock, SystemClock};
use meta_whatsapp_rs::core::error::{SinkError, StorageError};
use meta_whatsapp_rs::core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_rs::core::sink::EventSink;
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_rs::inbox::InboxSink;
use meta_whatsapp_rs::webhooks::axum::body::Bytes;
use meta_whatsapp_rs::webhooks::{
    DEFAULT_MAX_BODY_BYTES, DedupGuard, DeliveryReport, SignatureVerifier, WebhookEvent,
    WebhookHandler,
};
pub use meta_whatsapp_server_core::events::{
    DEFAULT_OUTBOX_RETENTION, EventIdKey, EventKey, HOUSEKEEPING_INTERVAL, KEYLESS_DEDUP_WINDOW,
    MAX_PAGE_DATA_BYTES, OPERATOR_EVENT_TYPES, Polled, Route, RowError, TENANT_EVENT_TYPES,
    event_data, event_number, meta_time, outbox_key, outbox_row, owner, poll, purge_outbox, route,
    tenant_visible,
};
use meta_whatsapp_server_core::outbox::{Outbox, OutboxBusy};
use meta_whatsapp_server_core::store::RecordStore;
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::metrics::Metrics;

/// Largest webhook body read: 3 MiB, the library's default (Meta documents
/// payloads of up to 3 MB, `webhooks/overview`). One byte more is `413`.
pub const MAX_WEBHOOK_BODY_BYTES: usize = DEFAULT_MAX_BODY_BYTES;

/// Meta's deliveries a replica reads and records at once: past it, a
/// delivery is answered `503` before its body is read (Meta retries). The
/// bodies read at once stay under 64 × 3 MiB, whoever sends them: the
/// signature can only be checked once a body is read (security review
/// M1).
pub const MAX_DELIVERIES_IN_FLIGHT: usize = 64;

/// How long a delivery's body may take to arrive: past it, `408` (Meta
/// retries), so slow bodies cannot hold the deliveries' places.
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(15);

/// Deliveries a replica records at once: each holds at most one database
/// connection at a time, so the webhook path never takes more than these
/// of a replica's pool, and API calls (key lookups first) always find one
/// (security review M2). Below the pool's size (`crate::serve`).
pub const MAX_DELIVERIES_RECORDING: usize = 4;

/// How long a delivery waits for its turn to record: past it, `503`.
pub const RECORDING_WAIT: Duration = Duration::from_secs(10);

/// What the webhook pipeline is built from.
pub struct Inbound {
    verifier: SignatureVerifier,
    ids: EventIdKey,
    kv: Arc<dyn KvStore>,
    conversations: Arc<dyn ConversationStore>,
    outbox: Arc<dyn Outbox>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Inbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inbound")
            .field("verifier", &self.verifier)
            .finish_non_exhaustive()
    }
}

impl Inbound {
    /// Verify deliveries against `app_secrets` (the active one, then the
    /// previous one while rotating), lease dedup claims in `kv`, record
    /// into `conversations` and `outbox`. Event ids are derived from the
    /// first app secret ([`EventIdKey`]): after a rotation, an event
    /// recorded again (its row purged) gets another id.
    ///
    /// # Errors
    ///
    /// No app secret, or a blank one (the library's `SignatureVerifier`
    /// refuses them: an HMAC under a blank key is anyone's).
    pub fn new(
        app_secrets: Vec<AppSecret>,
        kv: Arc<dyn KvStore>,
        conversations: Arc<dyn ConversationStore>,
        outbox: Arc<dyn Outbox>,
    ) -> meta_whatsapp_rs::Result<Self> {
        let ids = app_secrets
            .first()
            .and_then(EventIdKey::from_app_secret)
            .ok_or_else(|| {
                meta_whatsapp_rs::core::error::ConfigError::new(
                    "the webhook pipeline needs an app secret",
                )
            })?;
        Ok(Self {
            verifier: SignatureVerifier::new(app_secrets)?,
            ids,
            kv,
            conversations,
            outbox,
            clock: Arc::new(SystemClock),
        })
    }

    /// Read "now" from `clock` (the system clock by default): the replay
    /// window and [`KEYLESS_DEDUP_WINDOW`] are measured on it.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

/// The webhook pipeline and the outbox, as the routes use them.
pub(crate) struct Events {
    verifier: SignatureVerifier,
    verify_token: VerifyToken,
    dedup: DedupGuard,
    sink: ServiceSink,
    outbox: Arc<dyn Outbox>,
    /// Places for deliveries being read and recorded
    /// ([`MAX_DELIVERIES_IN_FLIGHT`]).
    in_flight: Arc<Semaphore>,
    /// Turns to record ([`MAX_DELIVERIES_RECORDING`]).
    recording: Arc<Semaphore>,
    rejections: RejectionLog,
}

/// What a refused delivery is refused for, as the rejection log counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rejection {
    /// No well-formed signature header: `401` before the body.
    Unsigned = 0,
    /// A signature no app secret produced: `401`.
    Forged = 1,
    /// Over 3 MiB: `413`.
    TooLarge = 2,
    /// The body took too long: `408`.
    SlowBody = 3,
    /// No place or turn: `503`.
    Busy = 4,
    /// The body broke off before it all arrived (a reset connection).
    Broken = 5,
}

/// Warnings about refused deliveries, at most one a minute for each
/// [`Rejection`], saying how many were not written since: anyone who
/// reaches the public listener can have requests refused, and each must
/// not cost a log line (security review L5). The metric
/// (`wa_server_webhook_deliveries_total`) counts every one.
#[derive(Debug)]
pub(crate) struct RejectionLog {
    started: std::time::Instant,
    /// Per rejection: the millisecond (since `started`) from which the next
    /// line may be written, and the refusals not written since the last.
    slots: [(AtomicU64, AtomicU64); 6],
}

impl RejectionLog {
    /// Between two lines about the same rejection.
    pub(crate) const EVERY: Duration = Duration::from_secs(60);

    fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
            slots: Default::default(),
        }
    }

    /// Whether to write a line about `rejection` now: `Some(n)` with the
    /// `n` refusals not written since the last line, else `None` (this one
    /// is counted for the next line).
    pub(crate) fn admit(&self, rejection: Rejection) -> Option<u64> {
        let (next, suppressed) = &self.slots[rejection as usize];
        let now = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let every = u64::try_from(Self::EVERY.as_millis()).unwrap_or(u64::MAX);
        let due = next.load(Ordering::Relaxed);
        if now >= due
            && next
                .compare_exchange(
                    due,
                    now.saturating_add(every),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
        {
            return Some(suppressed.swap(0, Ordering::Relaxed));
        }
        suppressed.fetch_add(1, Ordering::Relaxed);
        None
    }
}

/// Why a delivery was not recorded.
#[derive(Debug)]
pub(crate) enum Refused {
    /// The replica is at capacity, or the outbox too busy: `503`, Meta
    /// retries.
    Busy,
    /// The library's handler refused or failed it.
    Handler(meta_whatsapp_rs::Error),
}

impl Events {
    /// The pipeline over `inbound`, routing with `store`.
    pub(crate) fn new(
        inbound: Inbound,
        store: Arc<dyn RecordStore>,
        verify_token: VerifyToken,
        metrics: Metrics,
    ) -> Self {
        let dedup = DedupGuard::new(inbound.kv);
        let sink = ServiceSink {
            store,
            inbox: InboxSink::new(inbound.conversations),
            outbox: inbound.outbox.clone(),
            metrics,
            ids: inbound.ids,
            replay_window: dedup.ttl(),
            clock: inbound.clock,
        };
        Self {
            verifier: inbound.verifier,
            verify_token,
            dedup,
            sink,
            outbox: inbound.outbox,
            in_flight: Arc::new(Semaphore::new(MAX_DELIVERIES_IN_FLIGHT)),
            recording: Arc::new(Semaphore::new(MAX_DELIVERIES_RECORDING)),
            rejections: RejectionLog::new(),
        }
    }

    /// The log of refused deliveries.
    pub(crate) fn rejections(&self) -> &RejectionLog {
        &self.rejections
    }

    /// A place for one delivery, or `None` when
    /// [`MAX_DELIVERIES_IN_FLIGHT`] are being read or recorded: hold it
    /// while reading the body and recording it.
    pub(crate) fn admit(&self) -> Option<OwnedSemaphorePermit> {
        self.in_flight.clone().try_acquire_owned().ok()
    }

    /// One of Meta's deliveries, through the library's `WebhookHandler`
    /// (size, signature, parsing, the dedup lease), into the service's
    /// sink. The handler is the delivery's own: its sink knows the signed
    /// body, which keys the events that have no dedup key of their own
    /// ([`DeliverySink`]).
    ///
    /// # Errors
    ///
    /// The handler's: see `WebhookHandler::deliver`.
    pub(crate) async fn deliver(
        &self,
        signature: Option<&str>,
        body: Bytes,
    ) -> Result<DeliveryReport, Refused> {
        // The signature first, here: the library's handler would log each
        // forged delivery (the caller logs them at most once a minute), and
        // a forged one needs no turn to record.
        if let Err(error) = self.verifier.verify(signature, &body) {
            return Err(Refused::Handler(error.into()));
        }
        // Its turn to record: at most MAX_DELIVERIES_RECORDING hold the
        // database at once.
        let Ok(Ok(_turn)) = tokio::time::timeout(RECORDING_WAIT, self.recording.acquire()).await
        else {
            return Err(Refused::Busy);
        };
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
        match handler.deliver(signature, &body).await {
            Ok(report) => Ok(report),
            // The outbox waited too long for a lock (`OutboxBusy`).
            Err(meta_whatsapp_rs::Error::Sink(SinkError::Full)) => Err(Refused::Busy),
            Err(error) => Err(Refused::Handler(error)),
        }
    }

    /// The outbox.
    pub(crate) fn outbox(&self) -> &dyn Outbox {
        self.outbox.as_ref()
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
#[derive(Clone)]
pub struct ServiceSink {
    store: Arc<dyn RecordStore>,
    inbox: InboxSink,
    outbox: Arc<dyn Outbox>,
    metrics: Metrics,
    ids: EventIdKey,
    /// Events Meta dated longer ago than this are replays: the dedup
    /// lease's memory (`DedupGuard::ttl`), past which a captured body would
    /// otherwise be recorded again.
    replay_window: Duration,
    /// "Now", for the replay window and [`KEYLESS_DEDUP_WINDOW`].
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ServiceSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceSink").finish_non_exhaustive()
    }
}

impl ServiceSink {
    /// Count and log a failed stage; the error the handler logs and
    /// answers `500` for.
    fn failed(&self, stage: &'static str, kind: &'static str, error: SinkError) -> SinkError {
        self.metrics.webhook_failure(stage);
        tracing::warn!(stage, event_type = kind, "recording a webhook event failed");
        error
    }
}

fn storage(error: StorageError) -> SinkError {
    SinkError::Delivery(anyhow::Error::new(error))
}

impl ServiceSink {
    /// Record `event`, keyed `key`: route it, then the inbox, then the
    /// outbox.
    async fn record(&self, event: WebhookEvent, key: &EventKey) -> Result<(), SinkError> {
        let kind = event.kind();
        let now = self.clock.now();
        let not_before = now - self.replay_window;
        let route = route(self.store.as_ref(), &event, not_before)
            .await
            .map_err(|e| self.failed("routing", kind, storage(e)))?;
        // The row: its data, key and id (the core's `outbox_row`).
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
            let busy = matches!(&e, StorageError::Backend(e) if e.is::<OutboxBusy>());
            if busy {
                self.metrics.webhook_failure("outbox_busy");
                SinkError::Full
            } else {
                self.failed("outbox", kind, storage(e))
            }
        })?;
        let Some(sequence) = inserted else {
            self.metrics.webhook_duplicates("outbox", 1);
            tracing::debug!(event_type = kind, "the event was already in the outbox");
            return Ok(());
        };
        if let Some(tenant) = &route.tenant {
            self.metrics.webhook_event(kind, "tenant");
            tracing::debug!(
                event_type = kind,
                sequence,
                tenant = tenant.as_str(),
                "event recorded"
            );
        } else {
            self.metrics.webhook_event(kind, "operator");
            tracing::info!(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One line a minute per rejection, counting the ones left out.
    #[test]
    fn the_rejection_log_writes_a_line_a_minute_per_rejection() {
        let log = RejectionLog::new();
        assert_eq!(log.admit(Rejection::Unsigned), Some(0));
        for _ in 0..5 {
            assert_eq!(log.admit(Rejection::Unsigned), None);
        }
        assert_eq!(log.admit(Rejection::Forged), Some(0), "each its own");
        // A minute later: the next line says how many were left out.
        log.slots[Rejection::Unsigned as usize]
            .0
            .store(0, Ordering::Relaxed);
        assert_eq!(log.admit(Rejection::Unsigned), Some(5));
        assert_eq!(log.admit(Rejection::Unsigned), None);
    }

    /// The numbers docs/design/server.md (sections 2.3 and 6) and the guide
    /// state. The behaviour tests time themselves by these constants (a
    /// test stepping `KEYLESS_DEDUP_WINDOW` passes whatever the window
    /// is), so only this pins their values. Decisive: each value.
    #[test]
    fn the_pipelines_numbers_are_the_designs() {
        assert_eq!(MAX_WEBHOOK_BODY_BYTES, 3 * 1024 * 1024, "3 MiB");
        assert_eq!(MAX_DELIVERIES_IN_FLIGHT, 64);
        assert_eq!(BODY_READ_TIMEOUT, Duration::from_secs(15));
        assert_eq!(MAX_DELIVERIES_RECORDING, 4);
        assert_eq!(RECORDING_WAIT, Duration::from_secs(10));
        assert_eq!(KEYLESS_DEDUP_WINDOW, Duration::from_hours(1), "an hour");
        assert_eq!(RejectionLog::EVERY, Duration::from_mins(1), "a minute");
        assert_eq!(DEFAULT_OUTBOX_RETENTION, Duration::from_hours(7 * 24));
        assert_eq!(HOUSEKEEPING_INTERVAL, Duration::from_mins(10));
        assert_eq!(MAX_PAGE_DATA_BYTES, 8 * 1024 * 1024, "8 MiB");
    }
}
