//! The event outbox (`wa_server_events`, docs/design/server.md, section
//! 2.3): every webhook event the service received, with the tenant it was
//! routed to, or none (an operator-only row, never shown to a tenant).
//!
//! **Each tenant has its own stream of sequences**, from 1, and the
//! operator-only rows one of their own: a tenant's cursor and its events'
//! sequences say nothing about other tenants' traffic.
//!
//! [`EventStore`] has two implementations: [`PgEventStore`] and
//! [`MemoryEventStore`] (development and tests). Both keep the contract
//! polling relies on: **a stream's inserts commit in sequence order**, so a
//! reader that sees sequence `n` of a tenant sees every event of that
//! tenant before it, and `next_after` never skips one that commits later.

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
    /// Its position in its tenant's stream: increasing, never reused.
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

/// The event outbox.
#[async_trait]
pub trait EventStore: Send + Sync + 'static {
    /// Append `event` to its tenant's stream (or the operator-only one),
    /// committed in the stream's sequence order; its sequence. `None` when
    /// an event with the same dedup key is stored (nothing is written).
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>>;

    /// A tenant's events after the query's cursor, and its stream's bounds,
    /// from one consistent read.
    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage>;

    /// Delete the events received more than `older_than` ago, as a prefix
    /// of each stream, and record how far each went. `None` when another
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
    /// By tenant id; `""` is the operator-only stream.
    streams: HashMap<String, Stream>,
    /// Dedup key → the stream holding it.
    dedup: HashMap<String, String>,
}

#[derive(Debug, Default)]
struct Stream {
    rows: BTreeMap<i64, (Option<String>, StoredEvent)>,
    last: i64,
    purged_through: i64,
}

/// The stream of `tenant`: its id, or `""` for operator-only rows.
fn stream_of(tenant: Option<&TenantId>) -> String {
    tenant.map(|t| t.as_str().to_owned()).unwrap_or_default()
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
    /// the store's lock): its events go, and its stream records them
    /// purged, so a tenant created later with the same id polls none of
    /// them and its sequences go on after them.
    pub(crate) fn forget_tenant(&self, tenant: &TenantId) {
        let mut state = self.lock();
        let Some(stream) = state.streams.get_mut(tenant.as_str()) else {
            return;
        };
        stream.rows.clear();
        stream.purged_through = stream.last;
        state.dedup.retain(|_, s| s != tenant.as_str());
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
        let name = stream_of(event.tenant.as_ref());
        let stream = state.streams.entry(name.clone()).or_default();
        stream.last += 1;
        let sequence = stream.last;
        stream.rows.insert(
            sequence,
            (
                event.dedup_key.clone(),
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
            ),
        );
        if let Some(key) = &event.dedup_key {
            state.dedup.insert(key.clone(), name);
        }
        Ok(Some(sequence))
    }

    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage> {
        let state = self.lock();
        let Some(stream) = state.streams.get(query.tenant.as_str()) else {
            return Ok(EventPage {
                events: Vec::new(),
                more: false,
                purged_through: 0,
                high_water: 0,
            });
        };
        let after = query.after.unwrap_or(stream.purged_through);
        let mut matching = stream
            .rows
            .range(after.saturating_add(1)..)
            .map(|(_, (_, row))| row)
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
            });
        let mut events = Vec::new();
        let mut bytes = 0usize;
        let mut more = false;
        for row in matching.by_ref() {
            bytes = bytes.saturating_add(row.data.len());
            if events.len() == query.limit || (!events.is_empty() && bytes > query.max_bytes) {
                more = true;
                break;
            }
            events.push(row.clone());
        }
        Ok(EventPage {
            events,
            more,
            purged_through: stream.purged_through,
            high_water: stream.last,
        })
    }

    async fn purge(&self, older_than: Duration) -> StoreResult<Option<u64>> {
        let mut state = self.lock();
        let cutoff = OffsetDateTime::now_utc() - older_than;
        let mut purged = 0u64;
        let mut gone: Vec<String> = Vec::new();
        for stream in state.streams.values_mut() {
            // The prefix up to the newest event older than the cutoff.
            let Some(through) = stream
                .rows
                .values()
                .filter(|(_, row)| row.created_at < cutoff)
                .map(|(_, row)| row.sequence)
                .max()
            else {
                continue;
            };
            let kept = stream.rows.split_off(&(through + 1));
            let removed = std::mem::replace(&mut stream.rows, kept);
            purged += u64::try_from(removed.len()).unwrap_or(u64::MAX);
            gone.extend(removed.into_values().filter_map(|(key, _)| key));
            stream.purged_through = stream.purged_through.max(through);
        }
        for key in gone {
            state.dedup.remove(&key);
        }
        Ok(Some(purged))
    }
}
