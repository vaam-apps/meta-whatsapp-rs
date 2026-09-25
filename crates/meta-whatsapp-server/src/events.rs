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
//!                          1. route: the tenant owning the number, else the WABA
//!                          2. inbox (InboxSink), when a tenant owns it
//!                          3. outbox row: the tenant, or none (operator-only)
//! GET /v1/events ─► poll: the caller's tenant's rows after a sequence
//! ```
//!
//! **Routing is an allow-list.** An event naming a business phone number
//! belongs to the tenant that number is bound to, and only when the event's
//! WABA, if it names one, is the number's WABA in the bindings; an event
//! naming only a WABA belongs to the WABA's tenant. The outbox row carries
//! that tenant only for the types in [`TENANT_EVENT_TYPES`]; `unknown`,
//! `unparsed`, `partner_solution_updated`, any type a later library adds,
//! and every event of a number or WABA no tenant holds are operator-only
//! rows (no tenant: never polled, logged with size and digest, counted).
//!
//! **Meta's retries are safe.** A sink error answers `500`, the dedup claim
//! is released, and Meta redelivers the batch: the events before it are
//! duplicates, the failed one runs again. Both writes are idempotent: the
//! inbox stores a message id once and moves statuses forward only, and the
//! outbox inserts on the event's dedup key at most once. So a failure
//! between the inbox write and the outbox write (a database error: `500`;
//! a crash or the request deadline: the claim's lease ends after 60 s)
//! leaves the message in the inbox and no row, and the redelivery records
//! the row without a second message. The library's keyless events
//! (`error_reported`, `unparsed`) are not deduplicated: a redelivered batch
//! records them again.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::adapters::store::MemoryConversationStore;
use meta_whatsapp_rs::core::error::{SinkError, StorageError};
use meta_whatsapp_rs::core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_rs::core::sink::EventSink;
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_rs::inbox::InboxSink;
use meta_whatsapp_rs::webhooks::{
    DEFAULT_MAX_BODY_BYTES, DedupGuard, SignatureVerifier, WebhookEvent, WebhookHandler,
};
use sha2::{Digest, Sha256};

use crate::error::ApiError;
use crate::metrics::Metrics;
use crate::model::TenantId;
use crate::store::events::{EventQuery, EventStore, NewEvent, StoredEvent};
use crate::store::{MemoryEventStore, Store, StoreResult};

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

/// The most `data` a page of `GET /v1/events` carries: past it the page
/// ends early (with at least one event), and `next_after` continues from
/// there. A history sync event alone can approach 3 MiB.
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

    /// On memory stores, for unit tests.
    ///
    /// # Errors
    ///
    /// A blank `app_secret`.
    pub fn in_memory(
        app_secret: AppSecret,
        kv: Arc<dyn KvStore>,
    ) -> meta_whatsapp_rs::Result<Self> {
        Self::new(
            vec![app_secret],
            kv,
            Arc::new(MemoryConversationStore::new()),
            Arc::new(MemoryEventStore::new()),
        )
    }

    /// The outbox.
    pub fn outbox(&self) -> &Arc<dyn EventStore> {
        &self.outbox
    }
}

/// The webhook handler and the outbox, as the routes use them.
pub(crate) struct Events {
    handler: WebhookHandler,
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
        let handler = WebhookHandler::builder(inbound.verifier, verify_token, Arc::new(sink))
            .dedup(DedupGuard::new(inbound.kv))
            .max_body_bytes(MAX_WEBHOOK_BODY_BYTES)
            .build();
        Self {
            handler,
            outbox: inbound.outbox,
        }
    }

    /// The library's handler: signature, parsing, dedup, the sink.
    pub(crate) fn handler(&self) -> &WebhookHandler {
        &self.handler
    }

    /// The outbox.
    pub(crate) fn outbox(&self) -> &dyn EventStore {
        self.outbox.as_ref()
    }
}

/// Where an event goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The tenant holding the event's number (or, for an event naming no
    /// number, its WABA): the inbox records the event.
    pub owner: Option<TenantId>,
    /// The outbox row's tenant: the owner, for a [`tenant_visible`] type;
    /// else `None`, an operator-only row.
    pub tenant: Option<TenantId>,
}

/// The tenant holding `event`'s number or WABA, by the bindings (see the
/// module docs).
///
/// # Errors
///
/// The store failing.
pub async fn owner(store: &dyn Store, event: &WebhookEvent) -> StoreResult<Option<TenantId>> {
    if let Some(pn) = event.phone_number_id() {
        let Some(binding) = store.number(pn).await? else {
            return Ok(None);
        };
        // A number bound under another WABA than the event names: the
        // bindings are stale, and neither tenant is certainly the owner.
        if event.waba_id().is_some_and(|waba| *waba != binding.waba_id) {
            return Ok(None);
        }
        return Ok(Some(binding.tenant_id));
    }
    let Some(waba) = event.waba_id() else {
        return Ok(None);
    };
    Ok(store.waba(waba).await?.map(|binding| binding.tenant_id))
}

/// Where `event` goes: its owner, and the outbox row's tenant.
///
/// # Errors
///
/// The store failing.
pub async fn route(store: &dyn Store, event: &WebhookEvent) -> StoreResult<Route> {
    let owner = owner(store, event).await?;
    let tenant = owner.clone().filter(|_| tenant_visible(event.kind()));
    Ok(Route { owner, tenant })
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

/// The outbox's idempotency key of `event`: SHA-256 (hex) of its dedup
/// key; `None` for the library's keyless events.
pub fn outbox_key(event: &WebhookEvent) -> Option<String> {
    event
        .dedup_key()
        .map(|key| hex::encode(Sha256::digest(key.as_bytes())))
}

/// A new event id: `evt_` and 32 random hex digits.
fn new_event_id() -> Result<String, SinkError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| SinkError::Delivery(anyhow::anyhow!("no randomness for an event id")))?;
    Ok(format!("evt_{}", hex::encode(bytes)))
}

/// The library's sink for the service: route, then the inbox, then the
/// outbox, in that order (docs/design/server.md, section 2.3), so whoever
/// sees an event can already read its history.
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

#[async_trait]
impl EventSink<WebhookEvent> for ServiceSink {
    async fn deliver(&self, event: WebhookEvent) -> Result<(), SinkError> {
        let kind = event.kind();
        let route = route(self.store.as_ref(), &event)
            .await
            .map_err(|e| self.failed("routing", kind, storage(e)))?;
        let data = event_data(&event).map_err(|e| {
            // serde's message could quote content: its category only.
            let error = anyhow::anyhow!("an event did not serialize ({:?})", e.classify());
            self.failed("serialization", kind, SinkError::Delivery(error))
        })?;
        let row = NewEvent {
            id: new_event_id()?,
            dedup_key: outbox_key(&event),
            tenant: route.tenant.clone(),
            phone_number_id: event.phone_number_id().map(|pn| pn.as_str().to_owned()),
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
                reason = if route.owner.is_some() {
                    "type"
                } else {
                    "unowned"
                },
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
    /// follow, else the newest sequence of the whole outbox.
    pub next_after: i64,
}

/// A tenant's events after `query.after` (docs/design/server.md, section
/// 4.2).
///
/// # Errors
///
/// `410 cursor_expired` for a cursor below what housekeeping purged (events
/// after it may be gone); `422 invalid_request` on `after` for a cursor
/// this outbox never issued (past its newest sequence: a restored or
/// another database); `503 storage_unavailable`.
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
    let mut events = page.events;
    let mut more = events.len() > query.limit;
    events.truncate(query.limit);
    let mut bytes = 0usize;
    let fits = events
        .iter()
        .enumerate()
        .take_while(|(i, event)| {
            bytes = bytes.saturating_add(event.data.len());
            *i == 0 || bytes <= MAX_PAGE_DATA_BYTES
        })
        .count();
    if fits < events.len() {
        events.truncate(fits);
        more = true;
    }
    let next_after = if more {
        events.last().map_or(start, |event| event.sequence)
    } else {
        page.high_water.max(start)
    };
    Ok(Polled { events, next_after })
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
