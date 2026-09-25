//! Meta's webhooks into the inbox and the event outbox, and polling the
//! outbox (docs/design/server.md, sections 2.3 and 4.2). Plain functions
//! and a sink: the HTTP handlers (`api::webhooks`, `api::events`) only
//! translate.
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
//! **Routing is an allow-list.** An event naming a business phone number
//! (or, untyped, whose raw `metadata` names one) belongs to the tenant that
//! number is bound to, and only when the event's WABA, if it names one, is
//! the number's WABA in the bindings; an event naming only a WABA belongs
//! to the WABA's tenant. And only when Meta dated it no earlier than that
//! WABA's binding began ([`meta_time`]): a WABA moved from one tenant to
//! another does not bring the first one's retried events to the second.
//! The outbox row carries that tenant only for the types in
//! [`TENANT_EVENT_TYPES`]; `unknown`, `unparsed`, `partner_solution_updated`,
//! any type a later library adds, and every event of a number or WABA no
//! tenant holds (or held then) are operator-only rows (no tenant: never
//! polled, logged with size and digest, counted), and the inbox records
//! only an owned event.
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
//! and their position in it ([`EventKey::Delivery`]): Meta redelivers a
//! body byte for byte, so a redelivered batch records them once too (and an
//! identical body sent again later is taken for a redelivery: nothing in it
//! tells the two apart).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::core::error::{SinkError, StorageError};
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_rs::core::sink::EventSink;
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_rs::inbox::InboxSink;
use meta_whatsapp_rs::webhooks::axum::body::Bytes;
use meta_whatsapp_rs::webhooks::{
    DEFAULT_MAX_BODY_BYTES, DedupGuard, DeliveryReport, SignatureVerifier, WebhookEvent,
    WebhookHandler,
};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::error::ApiError;
use crate::metrics::Metrics;
use crate::model::TenantId;
use crate::store::events::{EventQuery, EventStore, NewEvent, StoredEvent};
use crate::store::{Store, StoreResult};

/// Largest webhook body read: 3 MiB, the library's default (Meta documents
/// payloads of up to 3 MB, `webhooks/overview`). One byte more is `413`.
pub const MAX_WEBHOOK_BODY_BYTES: usize = DEFAULT_MAX_BODY_BYTES;

/// Default `WA_SERVER_OUTBOX_RETENTION`: 7 days, the design's proposal.
/// Retention is decision D10 of the design, still open: this default
/// stands until the owner decides.
pub const DEFAULT_OUTBOX_RETENTION: Duration = Duration::from_hours(7 * 24);

/// How often a replica runs housekeeping (the outbox purge, and the
/// library's expired key/value rows on Postgres).
pub const HOUSEKEEPING_INTERVAL: Duration = Duration::from_secs(600);

/// The most `data` a page of `GET /v1/events` carries: the store stops
/// before the event that would pass it (the first event always comes) and
/// reads no data past it, and `next_after` continues from there. A history
/// sync event alone can approach 3 MiB.
pub const MAX_PAGE_DATA_BYTES: usize = 8 * 1024 * 1024;

/// The event types a tenant receives: the library's `WebhookEvent::kind`s
/// the service has reviewed and pinned (`tests/event_data.rs`). Every
/// other type, today's `unknown`, `unparsed` and `partner_solution_updated`
/// and any a later library adds, is operator-only
/// ([`OPERATOR_EVENT_TYPES`] lists today's).
pub const TENANT_EVENT_TYPES: [&str; 28] = [
    "message_received",
    "status_updated",
    "error_reported",
    "message_echoed",
    "history_synced",
    "app_state_synced",
    "call_updated",
    "call_status_updated",
    "user_preference_changed",
    "user_id_changed",
    "automatic_event_detected",
    "group_updated",
    "flow_updated",
    "account_alert",
    "account_review_updated",
    "account_updated",
    "account_settings_updated",
    "business_capability_updated",
    "business_username_updated",
    "payment_configuration_updated",
    "phone_number_name_updated",
    "phone_number_quality_updated",
    "security_updated",
    "template_components_updated",
    "template_quality_updated",
    "template_status_updated",
    "template_category_updated",
    "template_category_misuse_detected",
];

/// The library's event types that are operator-only whoever owns the
/// number: a field the library does not type (`unknown`: a raw body of any
/// shape), a signed body that is not a webhook (`unparsed`), and a
/// business portfolio's partner solution (`partner_solution_updated`: no
/// WABA to route it by).
pub const OPERATOR_EVENT_TYPES: [&str; 3] = ["unknown", "unparsed", "partner_solution_updated"];

/// Whether events of `kind` may reach a tenant.
pub fn tenant_visible(kind: &str) -> bool {
    TENANT_EVENT_TYPES.contains(&kind)
}

/// What the webhook pipeline is built from.
pub struct Inbound {
    verifier: SignatureVerifier,
    kv: Arc<dyn KvStore>,
    conversations: Arc<dyn ConversationStore>,
    outbox: Arc<dyn EventStore>,
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
    /// into `conversations` and `outbox`.
    ///
    /// # Errors
    ///
    /// No app secret, or a blank one (the library's `SignatureVerifier`
    /// refuses them: an HMAC under a blank key is anyone's).
    pub fn new(
        app_secrets: Vec<AppSecret>,
        kv: Arc<dyn KvStore>,
        conversations: Arc<dyn ConversationStore>,
        outbox: Arc<dyn EventStore>,
    ) -> meta_whatsapp_rs::Result<Self> {
        Ok(Self {
            verifier: SignatureVerifier::new(app_secrets)?,
            kv,
            conversations,
            outbox,
        })
    }
}

/// The webhook pipeline and the outbox, as the routes use them.
pub(crate) struct Events {
    verifier: SignatureVerifier,
    verify_token: VerifyToken,
    dedup: DedupGuard,
    sink: ServiceSink,
    outbox: Arc<dyn EventStore>,
}

impl Events {
    /// The pipeline over `inbound`, routing with `store`.
    pub(crate) fn new(
        inbound: Inbound,
        store: Arc<dyn Store>,
        verify_token: VerifyToken,
        metrics: Metrics,
    ) -> Self {
        let sink = ServiceSink {
            store,
            inbox: InboxSink::new(inbound.conversations),
            outbox: inbound.outbox.clone(),
            metrics,
        };
        Self {
            verifier: inbound.verifier,
            verify_token,
            dedup: DedupGuard::new(inbound.kv),
            sink,
            outbox: inbound.outbox,
        }
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
    ) -> meta_whatsapp_rs::Result<DeliveryReport> {
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
        handler.deliver(signature, &body).await
    }

    /// The outbox.
    pub(crate) fn outbox(&self) -> &dyn EventStore {
        self.outbox.as_ref()
    }
}

/// The key an event is recorded under, before the outbox scopes and
/// hashes it ([`outbox_key`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKey {
    /// The library's `WebhookEvent::dedup_key`.
    Library(String),
    /// An event the library gives no key (`error_reported`, `unparsed`):
    /// the SHA-256 of the signed body it came in and its position among
    /// that body's keyless events. Meta redelivers a batch byte for byte,
    /// so a redelivery reproduces the key, and the event is recorded once.
    Delivery {
        /// SHA-256 of the signed body.
        body_sha256: [u8; 32],
        /// Its position among the body's keyless events, from 0.
        position: usize,
    },
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

/// Where an event goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The tenant holding the event's number (or, for an event naming no
    /// number, its WABA) since before the event: the inbox records the
    /// event.
    pub owner: Option<TenantId>,
    /// The outbox row's tenant: the owner, for a [`tenant_visible`] type;
    /// else `None`, an operator-only row.
    pub tenant: Option<TenantId>,
    /// Why the row is operator-only (a fixed set, for the log): `unowned`
    /// (no binding holds its number or WABA, or a stale one), `before_binding`
    /// (Meta dated it before its binding: a previous holder's), or `type`
    /// (a type no tenant receives). `None` for a tenant's row.
    pub operator_only: Option<&'static str>,
}

/// When Meta says the event happened: the message's, status's, call's…
/// own time, else its entry's. `None` for events with no date of their
/// own: history and contact syncs (they carry the past on purpose),
/// errors, bodies that are not webhooks.
pub fn meta_time(event: &WebhookEvent) -> Option<OffsetDateTime> {
    use WebhookEvent as E;
    match event {
        E::MessageReceived { message, .. } => Some(message.timestamp),
        E::StatusUpdated { status, .. } => Some(status.timestamp),
        E::MessageEchoed { echo, .. } => Some(echo.timestamp),
        E::CallUpdated { call, .. } => Some(call.timestamp),
        E::CallStatusUpdated { status, .. } => Some(status.timestamp),
        E::UserPreferenceChanged { preference, .. } => Some(preference.timestamp),
        E::UserIdChanged { update, .. } => Some(update.timestamp),
        E::AutomaticEventDetected { detected, .. } => Some(detected.timestamp),
        E::GroupUpdated { update, .. } => update.timestamp,
        E::AccountSettingsUpdated { time, update, .. } => update.timestamp.or(*time),
        E::FlowUpdated { time, .. }
        | E::AccountAlert { time, .. }
        | E::AccountReviewUpdated { time, .. }
        | E::AccountUpdated { time, .. }
        | E::BusinessCapabilityUpdated { time, .. }
        | E::BusinessUsernameUpdated { time, .. }
        | E::PartnerSolutionUpdated { time, .. }
        | E::PaymentConfigurationUpdated { time, .. }
        | E::PhoneNumberNameUpdated { time, .. }
        | E::PhoneNumberQualityUpdated { time, .. }
        | E::SecurityUpdated { time, .. }
        | E::TemplateComponentsUpdated { time, .. }
        | E::TemplateQualityUpdated { time, .. }
        | E::TemplateStatusUpdated { time, .. }
        | E::TemplateCategoryUpdated { time, .. }
        | E::TemplateCategoryMisuseDetected { time, .. }
        | E::Unknown { time, .. } => *time,
        // History and contact syncs import the past; errors and bodies
        // that are not webhooks carry no date. A type a later library adds
        // has none until listed here.
        _ => None,
    }
}

/// The business number an event is about: the one it names, or, for a
/// change the library did not type, the `metadata.phone_number_id` of its
/// raw value (the inbox still reads an untyped `history` change by it).
pub fn event_number(event: &WebhookEvent) -> Option<PhoneNumberId> {
    if let Some(pn) = event.phone_number_id() {
        return Some(pn.clone());
    }
    let WebhookEvent::Unknown { raw, .. } = event else {
        return None;
    };
    raw.get("metadata")?
        .get("phone_number_id")?
        .as_str()
        .filter(|pn| !pn.is_empty())
        .map(PhoneNumberId::new)
}

/// The tenant holding `event`'s number or WABA, by the bindings, and why
/// nobody does (see [`Route::operator_only`]).
///
/// - Number first: the event's number (or an untyped change's
///   `metadata.phone_number_id`), bound under the WABA the event names;
///   else, naming no number, its WABA.
/// - Since before the event: Meta's date for it ([`meta_time`]) is not
///   before the second the WABA's binding began. A WABA unbound from one
///   tenant and bound to another does not bring the first one's events
///   (Meta retries for up to 7 days) to the second.
///
/// # Errors
///
/// The store failing.
pub async fn owner(
    store: &dyn Store,
    event: &WebhookEvent,
) -> StoreResult<Result<TenantId, &'static str>> {
    let binding = if let Some(pn) = event_number(event) {
        let Some(number) = store.number(&pn).await? else {
            return Ok(Err("unowned"));
        };
        // A number bound under another WABA than the event names: the
        // bindings are stale, and neither tenant is certainly the owner.
        if event.waba_id().is_some_and(|waba| *waba != number.waba_id) {
            return Ok(Err("unowned"));
        }
        store.waba(&number.waba_id).await?
    } else if let Some(waba) = event.waba_id() {
        store.waba(waba).await?
    } else {
        None
    };
    let Some(binding) = binding else {
        return Ok(Err("unowned"));
    };
    if meta_time(event).is_some_and(|at| at.unix_timestamp() < binding.attached_at.unix_timestamp())
    {
        return Ok(Err("before_binding"));
    }
    Ok(Ok(binding.tenant_id))
}

/// Where `event` goes: its owner, and the outbox row's tenant.
///
/// # Errors
///
/// The store failing.
pub async fn route(store: &dyn Store, event: &WebhookEvent) -> StoreResult<Route> {
    Ok(match owner(store, event).await? {
        Ok(owner) if tenant_visible(event.kind()) => Route {
            tenant: Some(owner.clone()),
            owner: Some(owner),
            operator_only: None,
        },
        Ok(owner) => Route {
            owner: Some(owner),
            tenant: None,
            operator_only: Some("type"),
        },
        Err(reason) => Route {
            owner: None,
            tenant: None,
            operator_only: Some(reason),
        },
    })
}

/// The JSON an event is stored and served as (`data` of the envelope): the
/// library's `WebhookEvent` serialization, pinned by `tests/event_data.rs`.
///
/// # Errors
///
/// Serialization failing (it does not for the library's events).
pub fn event_data(event: &WebhookEvent) -> Result<String, serde_json::Error> {
    serde_json::to_string(event)
}

/// The outbox's idempotency key of an event keyed `key`, about the
/// business number `phone_number_id` (when it names one), whose JSON is
/// `data`: SHA-256 (hex) over the number and the key, so two numbers never
/// share one (hashed: some dedup keys hold a group participant's phone
/// number). A keyless event's key also covers its `data`, so a library
/// that splits a body into other events never takes one for another.
pub fn outbox_key(key: &EventKey, phone_number_id: Option<&str>, data: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"meta-whatsapp-server/outbox-key/v1\0");
    hash.update(phone_number_id.unwrap_or_default().as_bytes());
    hash.update(b"\0");
    match key {
        EventKey::Library(key) => {
            hash.update(b"library\0");
            hash.update(key.as_bytes());
        }
        EventKey::Delivery {
            body_sha256,
            position,
        } => {
            hash.update(b"delivery\0");
            hash.update(body_sha256);
            hash.update(position.to_be_bytes());
            hash.update(Sha256::digest(data.as_bytes()));
        }
    }
    hex::encode(hash.finalize())
}

/// A new event id: `evt_` and 32 random hex digits.
fn new_event_id() -> Result<String, SinkError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| SinkError::Delivery(anyhow::anyhow!("no randomness for an event id")))?;
    Ok(format!("evt_{}", hex::encode(bytes)))
}

/// What the service does with an event: route, then the inbox, then the
/// outbox, in that order (docs/design/server.md, section 2.3), so whoever
/// sees an event can already read its history. Each delivery reaches it
/// through a sink of its own, which keys its events ([`EventKey`]).
#[derive(Clone)]
pub struct ServiceSink {
    store: Arc<dyn Store>,
    inbox: InboxSink,
    outbox: Arc<dyn EventStore>,
    metrics: Metrics,
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
        let route = route(self.store.as_ref(), &event)
            .await
            .map_err(|e| self.failed("routing", kind, storage(e)))?;
        let data = event_data(&event).map_err(|e| {
            // serde's message could quote content: its category only.
            let error = anyhow::anyhow!("an event did not serialize ({:?})", e.classify());
            self.failed("serialization", kind, SinkError::Delivery(error))
        })?;
        let phone_number_id = event.phone_number_id().map(|pn| pn.as_str().to_owned());
        let row = NewEvent {
            id: new_event_id()?,
            dedup_key: Some(outbox_key(key, phone_number_id.as_deref(), &data)),
            tenant: route.tenant.clone(),
            phone_number_id,
            waba_id: event.waba_id().map(|waba| waba.as_str().to_owned()),
            event_type: kind.to_owned(),
            data,
        };
        if route.owner.is_some() {
            self.inbox
                .deliver(event)
                .await
                .map_err(|e| self.failed("inbox", kind, e))?;
        }
        let inserted = self
            .outbox
            .insert(&row)
            .await
            .map_err(|e| self.failed("outbox", kind, storage(e)))?;
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

/// A poll's events and the cursor to continue from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Polled {
    /// The events, in sequence order.
    pub events: Vec<StoredEvent>,
    /// Pass as `after` next time: the last event's sequence when more
    /// follow, else the newest sequence of the tenant's stream.
    pub next_after: i64,
}

/// A tenant's events after `query.after`, in its own stream of sequences
/// (docs/design/server.md, sections 2.3 and 4.2).
///
/// # Errors
///
/// `410 cursor_expired` for a cursor below what was purged (by retention,
/// or with a deleted tenant of the same id: events after it may be gone);
/// `422 invalid_request` on `after` for a cursor the tenant's stream never
/// reached (a restored or another database); `503 storage_unavailable`.
pub async fn poll(outbox: &dyn EventStore, query: &EventQuery) -> Result<Polled, ApiError> {
    let page = outbox.page(query).await?;
    let start = match query.after {
        Some(after) if after < page.purged_through => {
            return Err(ApiError::new("cursor_expired"));
        }
        Some(after) if after > page.high_water => return Err(ApiError::invalid("after")),
        Some(after) => after,
        None => page.purged_through,
    };
    let next_after = match page.events.last() {
        Some(last) if page.more => last.sequence,
        _ => page.high_water.max(start),
    };
    Ok(Polled {
        events: page.events,
        next_after,
    })
}

/// One round of housekeeping: purge the outbox past `retention`. `None`
/// when another replica holds the housekeeping lock.
///
/// # Errors
///
/// The store failing.
pub async fn purge_outbox(
    outbox: &dyn EventStore,
    retention: Duration,
) -> StoreResult<Option<u64>> {
    let purged = outbox.purge(retention).await?;
    if let Some(purged) = purged
        && purged > 0
    {
        tracing::info!(purged, "outbox events past retention purged");
    }
    Ok(purged)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two lists split the library's kinds: none is both.
    #[test]
    fn tenant_and_operator_types_are_disjoint() {
        for kind in OPERATOR_EVENT_TYPES {
            assert!(!tenant_visible(kind), "{kind}");
        }
        let mut all: Vec<&str> = TENANT_EVENT_TYPES.to_vec();
        all.extend(OPERATOR_EVENT_TYPES);
        let unique: std::collections::BTreeSet<&str> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len());
    }
}
