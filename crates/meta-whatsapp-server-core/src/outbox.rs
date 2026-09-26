//! The event outbox port (`wa_server_events` on Postgres,
//! docs/design/server.md, section 2.3): every webhook event the service
//! received, with the tenant it was routed to, or none (an operator-only
//! row, never shown to a tenant).
//!
//! **Each tenant has its own stream of sequences**, from 1, and the
//! operator-only rows one of their own: a tenant's cursor and its events'
//! sequences say nothing about other tenants' traffic.
//!
//! Every [`Outbox`] keeps two contracts, which a backend meets however
//! its database allows (the Postgres one with row locks, the memory one
//! with one lock over the records and the outbox):
//!
//! - **A stream's inserts commit in sequence order**, so a reader that
//!   sees sequence `n` of a tenant sees every event of that tenant before
//!   it, and `next_after` never skips one that commits later.
//! - **An insert checks the routing again** ([`RouteGuard`]): a row keeps
//!   its tenant only while, atomically with the insert, the binding the
//!   event was routed by still names that tenant and began no later than
//!   the event's second. Between the routing (a read) and the insert, a
//!   WABA can move to another tenant, or its tenant be deleted, created
//!   again under the same id and bound again; the row is then
//!   operator-only, never the new holder's.

use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
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
    /// The tenant it is routed to; `None` for an operator-only row. The
    /// row keeps it only through [`Self::route_guard`]: see
    /// [`Outbox::insert`].
    pub tenant: Option<TenantId>,
    /// The binding [`Self::tenant`] was routed by, which the insert checks
    /// again (`crate::events::outbox_row` sets it for every routed event).
    /// A row with a tenant and no guard is operator-only.
    pub route_guard: Option<RouteGuard>,
    /// The business phone number it is about, when it names one.
    pub phone_number_id: Option<PhoneNumberId>,
    /// Its WABA, when it names one.
    pub waba_id: Option<WabaId>,
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
            .field("route_guard", &self.route_guard)
            .field("event_type", &self.event_type)
            .field("data_bytes", &self.data.len())
            .finish_non_exhaustive()
    }
}

/// What an insert checks before it keeps a row's tenant: the binding the
/// event was routed by (`crate::events::owner`), typed, so that no backend
/// redoes the routing rules. [`Outbox::insert`] keeps the row's tenant
/// only if, atomically with the insert:
///
/// - [`Self::binding`] still names that tenant: for
///   [`GuardedBinding::Number`], the number is bound to the tenant under
///   that WABA; for [`GuardedBinding::Waba`], the WABA is bound to the
///   tenant;
/// - and, when the event is dated ([`Self::not_after`]), that WABA's
///   binding began no later than the event's second (its `attached_at`,
///   truncated to the second, is at most `not_after`'s): a binding made
///   after Meta dated the event is another holder's, even under the same
///   tenant id.
///
/// Otherwise the row is recorded operator-only (no tenant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteGuard {
    /// The binding the event was routed by.
    pub binding: GuardedBinding,
    /// When Meta dated the event (`crate::events::meta_time`); `None` for
    /// an undated event (an error, a sync), whose binding's start is not
    /// checked: in the race of a tenant deleted, created again under the
    /// same id and bound again, it reaches the new tenant.
    pub not_after: Option<OffsetDateTime>,
}

/// Which binding an event was routed by ([`RouteGuard::binding`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardedBinding {
    /// An event naming a business number: the number, bound under this
    /// WABA (the one it was bound under when the event was routed; the
    /// event's own WABA, when it names one).
    Number {
        /// The number.
        phone_number_id: PhoneNumberId,
        /// Its WABA.
        waba_id: WabaId,
    },
    /// An event naming only a WABA.
    Waba(WabaId),
}

impl GuardedBinding {
    /// The WABA whose binding's start [`RouteGuard::not_after`] is
    /// compared with.
    pub fn waba_id(&self) -> &WabaId {
        match self {
            Self::Number { waba_id, .. } | Self::Waba(waba_id) => waba_id,
        }
    }
}

/// Whether a binding that began at `attached_at` began no later than the
/// second of `not_after` (`None`: undated, always): the rule of
/// [`RouteGuard::not_after`], the same as the routing's
/// (`crate::events::owner`), for a backend that compares in Rust.
pub fn began_by(attached_at: OffsetDateTime, not_after: Option<OffsetDateTime>) -> bool {
    not_after.is_none_or(|at| attached_at.unix_timestamp() <= at.unix_timestamp())
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
    pub phone_number_id: Option<PhoneNumberId>,
    /// Its WABA, when it names one.
    pub waba_id: Option<WabaId>,
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
    pub phone_number_id: Option<PhoneNumberId>,
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

/// The event outbox.
#[async_trait]
pub trait Outbox: Send + Sync + 'static {
    /// Append `event` to its tenant's stream (or the operator-only one),
    /// committed in the stream's sequence order; its sequence.
    ///
    /// **The routing, checked again.** A row keeps [`NewEvent::tenant`]
    /// only if [`NewEvent::route_guard`] holds atomically with the insert
    /// (see [`RouteGuard`] for what holds means): its guarded binding
    /// cannot move between the check and the commit, and deleting the
    /// binding (or its tenant) waits for the insert or comes before the
    /// check. Otherwise, a tenant without a guard included, the row is
    /// recorded operator-only (no tenant, in the operator-only stream).
    /// This is a requirement of every backend, not a Postgres property.
    ///
    /// **Once.** `None` when an event with the same dedup key is stored
    /// (nothing is written, no sequence drawn), unless `event` has a
    /// [`NewEvent::dedup_window`] and the stored row's window ended at or
    /// before its `now`: that row gives the key up, and `event` is
    /// recorded.
    ///
    /// # Errors
    ///
    /// The library's `StorageError`; `StorageError::Busy` when the insert
    /// gave up waiting for another writer (another insert of the same
    /// stream, a binding changing) and wrote nothing: the webhook pipeline
    /// answers Meta `503` for it, and Meta retries.
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>>;

    /// A tenant's events after the query's cursor, and its stream's bounds,
    /// from one consistent read.
    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage>;

    /// Delete the events received more than `older_than` ago, as a prefix
    /// of each stream, and record how far each went; how many went. It
    /// takes no lock of its own (housekeeping runs it under a
    /// `LeaderLock` turn) and is safe to run on two replicas at once: a
    /// stream's `purged_through` only moves forward, and a row two purges
    /// both chose is deleted once.
    async fn purge(&self, older_than: Duration) -> StoreResult<u64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binding that began in the event's second, or before it, is the
    /// event's; one that began in the next second is not; an undated
    /// event's is. Decisive: the comparison of seconds, both ways.
    #[test]
    fn a_binding_began_by_the_events_second() {
        let t = time::macros::datetime!(2026-09-26 12:00:00.5 UTC);
        let at = |secs: f64| t + time::Duration::seconds_f64(secs);
        assert!(began_by(at(-10.0), Some(t)));
        assert!(began_by(at(0.4), Some(t)), "later in the same second");
        assert!(began_by(at(-0.5), Some(t)), "the second's first instant");
        assert!(!began_by(at(0.5), Some(t)), "the next second");
        assert!(began_by(at(3600.0), None), "undated");
    }
}
