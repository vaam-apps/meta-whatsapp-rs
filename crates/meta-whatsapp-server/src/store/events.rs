//! The event outbox (`wa_server_events`, docs/design/server.md, section
//! 2.3): every webhook event the service received, in sequence order, with
//! the tenant it was routed to, or none (an operator-only row, never shown
//! to a tenant).
//!
//! [`EventStore`] has two implementations: [`PgEventStore`] and
//! [`MemoryEventStore`] (development and tests). Both keep the contract
//! polling relies on: **inserts commit in sequence order**, so a reader
//! that sees sequence `n` sees every event before it, and `next_after`
//! never skips one that commits later.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;

use super::StoreResult;
pub use super::events_postgres::PgEventStore;
use crate::model::TenantId;

/// An event to append to the outbox.
#[derive(Clone, PartialEq, Eq)]
pub struct NewEvent {
    /// Its public id (`evt_…`).
    pub id: String,
    /// Its idempotency key (`crate::events::outbox_key`): a second insert
    /// with the same key is a no-op. The sink gives every event one; a row
    /// without one is never deduplicated.
    pub dedup_key: Option<String>,
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

/// An event as stored.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// Its position in the outbox: increasing, never reused, with gaps.
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
    /// Events after this sequence; `None` starts at the oldest retained.
    pub after: Option<i64>,
    /// Only these types; `None` for every type.
    pub types: Option<Vec<String>>,
    /// Only this phone number's events.
    pub phone_number_id: Option<String>,
    /// At most this many events (the store returns one more when more
    /// follow).
    pub limit: usize,
}

/// A poll's rows and the outbox's bounds, read at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPage {
    /// The matching events after the cursor, in sequence order: at most
    /// `limit + 1`.
    pub events: Vec<StoredEvent>,
    /// Every event up to this sequence was purged (0 before the first
    /// purge): a cursor below it may have missed some.
    pub purged_through: i64,
    /// The highest sequence stored or purged: every event up to it was
    /// visible to this read.
    pub high_water: i64,
}

/// The event outbox.
#[async_trait]
pub trait EventStore: Send + Sync + 'static {
    /// Append `event`, committed in sequence order. `None` when an event
    /// with the same dedup key is stored (nothing is written).
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>>;

    /// A tenant's events after the query's cursor, and the outbox's bounds,
    /// from one consistent read.
    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage>;

    /// Delete the events received more than `older_than` ago, as a prefix
    /// of sequences, and record how far it went. `None` when another
    /// replica is purging right now.
    async fn purge(&self, older_than: Duration) -> StoreResult<Option<u64>>;
}

/// The outbox in memory: one process, emptied on restart
/// (`WA_SERVER_ENV=development` and tests).
#[derive(Debug, Default)]
pub struct MemoryEventStore {
    state: Mutex<MemoryState>,
}

#[derive(Debug, Default)]
struct MemoryState {
    rows: BTreeMap<i64, StoredEvent>,
    dedup: HashMap<String, i64>,
    last_sequence: i64,
    purged_through: i64,
}

impl MemoryEventStore {
    /// An empty outbox.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, MemoryState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `tenant` was deleted ([`super::MemoryStore::delete_tenant`], under
    /// the store's lock): its events become operator-only rows, so a
    /// tenant created later with the same id never polls them.
    pub(crate) fn forget_tenant(&self, tenant: &TenantId) {
        let mut state = self.lock();
        for row in state.rows.values_mut() {
            if row.tenant.as_ref() == Some(tenant) {
                row.tenant = None;
            }
        }
    }
}

#[async_trait]
impl EventStore for MemoryEventStore {
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>> {
        let mut state = self.lock();
        if let Some(key) = &event.dedup_key
            && state.dedup.contains_key(key)
        {
            return Ok(None);
        }
        state.last_sequence += 1;
        let sequence = state.last_sequence;
        if let Some(key) = &event.dedup_key {
            state.dedup.insert(key.clone(), sequence);
        }
        state.rows.insert(
            sequence,
            StoredEvent {
                sequence,
                id: event.id.clone(),
                tenant: event.tenant.clone(),
                phone_number_id: event.phone_number_id.clone(),
                waba_id: event.waba_id.clone(),
                event_type: event.event_type.clone(),
                data: event.data.clone(),
                created_at: OffsetDateTime::now_utc(),
            },
        );
        Ok(Some(sequence))
    }

    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage> {
        let state = self.lock();
        let after = query.after.unwrap_or(state.purged_through);
        let events = state
            .rows
            .range(after.saturating_add(1)..)
            .map(|(_, row)| row)
            .filter(|row| row.tenant.as_ref() == Some(&query.tenant))
            .filter(|row| {
                query
                    .types
                    .as_ref()
                    .is_none_or(|types| types.contains(&row.event_type))
            })
            .filter(|row| {
                query
                    .phone_number_id
                    .as_ref()
                    .is_none_or(|pn| row.phone_number_id.as_ref() == Some(pn))
            })
            .take(query.limit.saturating_add(1))
            .cloned()
            .collect();
        let stored = state.rows.keys().next_back().copied().unwrap_or(0);
        Ok(EventPage {
            events,
            purged_through: state.purged_through,
            high_water: stored.max(state.purged_through),
        })
    }

    async fn purge(&self, older_than: Duration) -> StoreResult<Option<u64>> {
        let mut state = self.lock();
        let cutoff = OffsetDateTime::now_utc() - older_than;
        // The prefix up to the newest event older than the cutoff.
        let Some(through) = state
            .rows
            .values()
            .filter(|row| row.created_at < cutoff)
            .map(|row| row.sequence)
            .max()
        else {
            return Ok(Some(0));
        };
        let kept = state.rows.split_off(&(through + 1));
        let purged = std::mem::replace(&mut state.rows, kept);
        state.dedup.retain(|_, sequence| *sequence > through);
        state.purged_through = state.purged_through.max(through);
        Ok(Some(u64::try_from(purged.len()).unwrap_or(u64::MAX)))
    }
}
