//! The event outbox port (`wa_server_events` on Postgres,
//! docs/design/server.md, section 2.3): every webhook event the service
//! received, with the tenant it was routed to, or none (an operator-only
//! row, never shown to a tenant).
//!
//! **Each tenant has its own stream of sequences**, from 1, and the
//! operator-only rows one of their own: a tenant's cursor and its events'
//! sequences say nothing about other tenants' traffic.
//!
//! Every [`Outbox`] keeps the contract polling relies on: **a stream's
//! inserts commit in sequence order**, so a reader that sees sequence `n`
//! of a tenant sees every event of that tenant before it, and
//! `next_after` never skips one that commits later.

use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::model::TenantId;
use crate::store::StoreResult;

/// An event to append to the outbox.
#[derive(Clone, PartialEq, Eq)]
pub struct NewEvent {
    /// Its public id (`evt_…`).
    pub id: String,
    /// Its idempotency key (`crate::events::outbox_key`): a second insert
    /// with the same key is a no-op. The sink gives every event one; a row
    /// without one is never deduplicated.
    pub dedup_key: Option<String>,
    /// How long its row holds [`Self::dedup_key`]: `None`, for as long as
    /// it is stored (the library's dedup keys); for an event the library
    /// gives no dedup key (`error_reported`, `unparsed`), a window
    /// (`crate::events::KEYLESS_DEDUP_WINDOW`) on the webhook pipeline's
    /// clock.
    pub dedup_window: Option<DedupWindow>,
    /// When Meta dated the event (`crate::events::meta_time`), `None` when
    /// it carries no date. An outbox that re-checks the routing (Postgres)
    /// keeps [`Self::tenant`] only if the WABA binding it was routed by
    /// began no later than this second.
    pub meta_time: Option<OffsetDateTime>,
    /// The tenant it is routed to; `None` for an operator-only row.
    pub tenant: Option<TenantId>,
    /// The business phone number it is about, when it names one.
    pub phone_number_id: Option<String>,
    /// Its WABA, when it names one.
    pub waba_id: Option<String>,
    /// Its type: the library's `WebhookEvent::kind`.
    pub event_type: String,
    /// The event's JSON (the library's `WebhookEvent` serialization).
    pub data: String,
}

impl std::fmt::Debug for NewEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `data` carries customers' messages and numbers.
        f.debug_struct("NewEvent")
            .field("id", &self.id)
            .field("tenant", &self.tenant)
            .field("event_type", &self.event_type)
            .field("data_bytes", &self.data.len())
            .finish_non_exhaustive()
    }
}

/// A keyless event's dedup window, on the webhook pipeline's clock (both
/// ends: the stores never compare it with their own clock).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DedupWindow {
    /// When the event was received. A stored row holding the same key whose
    /// window ended at or before it is an earlier occurrence, not a
    /// redelivery: it gives the key up (keeping its row, sequence and id)
    /// and this event is recorded.
    pub now: OffsetDateTime,
    /// Until when this event's row holds the key: the same key before it
    /// is a redelivery, recorded nothing.
    pub until: OffsetDateTime,
}

/// An event as stored.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// Its position in its tenant's stream: increasing, never reused
    /// within one database's history (a point-in-time restore rolls the
    /// stream's `last_sequence` back, and later events draw its sequences
    /// again: docs/design/server.md, section 2.3).
    pub sequence: i64,
    /// Its public id.
    pub id: String,
    /// Its tenant; `None` for an operator-only row.
    pub tenant: Option<TenantId>,
    /// The business phone number, when it names one.
    pub phone_number_id: Option<String>,
    /// Its WABA, when it names one.
    pub waba_id: Option<String>,
    /// Its type.
    pub event_type: String,
    /// Its JSON, as written.
    pub data: String,
    /// When the service received it.
    pub created_at: OffsetDateTime,
}

impl std::fmt::Debug for StoredEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredEvent")
            .field("sequence", &self.sequence)
            .field("id", &self.id)
            .field("tenant", &self.tenant)
            .field("event_type", &self.event_type)
            .field("data_bytes", &self.data.len())
            .finish_non_exhaustive()
    }
}

/// Which of a tenant's events a poll asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventQuery {
    /// The tenant. Operator-only rows (no tenant) never match.
    pub tenant: TenantId,
    /// Events after this sequence of the tenant's stream; `None` starts at
    /// the oldest retained.
    pub after: Option<i64>,
    /// Only these types; `None` for every type.
    pub types: Option<Vec<String>>,
    /// Only this phone number's events.
    pub phone_number_id: Option<String>,
    /// At most this many events.
    pub limit: usize,
    /// At most this many bytes of `data`, except that the first event
    /// always comes: the store stops before the event that would pass it,
    /// and reads no event's data past it.
    pub max_bytes: usize,
}

/// A poll's rows and the tenant's stream's bounds, read at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPage {
    /// The matching events after the cursor, in sequence order: at most
    /// `limit`, within `max_bytes` (the first always).
    pub events: Vec<StoredEvent>,
    /// Whether more matching events follow the last one.
    pub more: bool,
    /// Every event of the stream up to this sequence was purged, by
    /// retention or with a deleted tenant (0 before the first purge): a
    /// cursor below it may have missed some.
    pub purged_through: i64,
    /// The stream's highest sequence, stored or purged: every event up to
    /// it was visible to this read.
    pub high_water: i64,
}

/// The outbox waited too long for a lock (another insert of the same
/// tenant, a binding changing): the insert did nothing, and trying again
/// later may succeed. Carried as the source of a
/// `StorageError::Backend`; the webhook pipeline answers Meta `503` for
/// it (Meta retries).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the outbox is busy")]
pub struct OutboxBusy;

/// The event outbox.
#[async_trait]
pub trait Outbox: Send + Sync + 'static {
    /// Append `event` to its tenant's stream (or the operator-only one),
    /// committed in the stream's sequence order; its sequence. `None` when
    /// an event with the same dedup key is stored (nothing is written),
    /// unless `event` has a [`NewEvent::dedup_window`] and the stored row's
    /// window ended at or before its `now`: that row gives the key up, and
    /// `event` is recorded.
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>>;

    /// A tenant's events after the query's cursor, and its stream's bounds,
    /// from one consistent read.
    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage>;

    /// Delete the events received more than `older_than` ago, as a prefix
    /// of each stream, and record how far each went. `None` when another
    /// replica is purging right now.
    async fn purge(&self, older_than: Duration) -> StoreResult<Option<u64>>;
}
