//! The event outbox (`wa_server_events`, docs/design/server.md, section
//! 2.3): the core's port ([`Outbox`]) and its types, re-exported here, and
//! its two implementations: [`PgEventStore`] and [`MemoryEventStore`]
//! (development and tests). Both keep the port's two contracts: **a
//! stream's inserts commit in sequence order**, so a reader that sees
//! sequence `n` of a tenant sees every event of that tenant before it, and
//! `next_after` never skips one that commits later; and **an insert checks
//! the routing again** ([`RouteGuard`]), atomically with it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
pub use meta_whatsapp_server_core::outbox::{
    DedupWindow, EventPage, EventQuery, GuardedBinding, NewEvent, Outbox, RouteGuard, StoredEvent,
    began_by,
};
use time::OffsetDateTime;

pub use super::events_postgres::PgEventStore;
use super::memory::{State, lock_state};
use crate::model::TenantId;
use crate::store::StoreResult;

/// The outbox in memory: one process, emptied on restart
/// (`WA_SERVER_ENV=development` and tests). It is always a
/// [`super::MemoryStore`]'s ([`super::MemoryStore::outbox`]): it reads that
/// store's bindings to check an insert's [`RouteGuard`], under the store's
/// lock, so no binding moves between the check and the insert; and
/// deleting a tenant there deletes its events here.
pub struct MemoryEventStore {
    /// The records of the store it belongs to: locked before [`Self::state`]
    /// (the store's own order, [`super::MemoryStore::delete_tenant`]).
    records: Arc<Mutex<State>>,
    state: Mutex<MemoryState>,
}

impl std::fmt::Debug for MemoryEventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The rows hold customers' messages: counts only, and without
        // waiting for the lock (a `{:?}` while this thread holds it).
        let mut out = f.debug_struct("MemoryEventStore");
        if let Ok(state) = self.state.try_lock() {
            out.field("streams", &state.streams.len()).field(
                "rows",
                &state.streams.values().map(|s| s.rows.len()).sum::<usize>(),
            );
        }
        out.finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct MemoryState {
    /// By tenant id; `""` is the operator-only stream.
    streams: HashMap<String, Stream>,
    /// Dedup key → the stream and sequence of the row holding it.
    dedup: HashMap<String, (String, i64)>,
}

#[derive(Debug, Default)]
struct Stream {
    rows: BTreeMap<i64, MemoryRow>,
    last: i64,
    purged_through: i64,
}

#[derive(Debug)]
struct MemoryRow {
    /// The dedup key it holds (`None` once a later occurrence took it).
    dedup_key: Option<String>,
    /// Until when it holds it: `None`, for as long as it is stored.
    dedup_until: Option<OffsetDateTime>,
    event: StoredEvent,
}

/// The stream of `tenant`: its id, or `""` for operator-only rows.
fn stream_of(tenant: Option<&TenantId>) -> String {
    tenant.map(|t| t.as_str().to_owned()).unwrap_or_default()
}

/// Whether `guard` holds for `tenant` in `records`: the check of
/// [`Outbox::insert`] ([`RouteGuard`]), on the memory store's bindings.
fn holds(records: &State, tenant: &TenantId, guard: &RouteGuard) -> bool {
    let waba = guard.binding.waba_id();
    let number_holds = match &guard.binding {
        GuardedBinding::Number {
            phone_number_id,
            waba_id,
        } => records
            .numbers
            .get(phone_number_id.as_str())
            .is_some_and(|n| &n.tenant_id == tenant && &n.waba_id == waba_id),
        GuardedBinding::Waba(_) => true,
    };
    number_holds
        && records
            .wabas
            .get(waba.as_str())
            .is_some_and(|w| &w.tenant_id == tenant && began_by(w.attached_at, guard.not_after))
}

impl MemoryEventStore {
    /// The outbox of the store whose records are `records`
    /// ([`super::MemoryStore::outbox`]).
    pub(super) fn of(records: Arc<Mutex<State>>) -> Self {
        Self {
            records,
            state: Mutex::new(MemoryState::default()),
        }
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
        state.dedup.retain(|_, (s, _)| s != tenant.as_str());
    }
}

#[async_trait]
impl Outbox for MemoryEventStore {
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>> {
        // The records first, as the store locks them: no binding moves
        // until this insert is done.
        let records = lock_state(&self.records);
        let mut guard = self.lock();
        let state = &mut *guard;
        if let Some(key) = &event.dedup_key
            && let Some((held_by, held_at)) = state.dedup.get(key)
        {
            let held = state
                .streams
                .get_mut(held_by)
                .and_then(|stream| stream.rows.get_mut(held_at));
            match (held, event.dedup_window) {
                // An earlier occurrence, whose window ended: it gives the
                // key up and keeps its row.
                (Some(row), Some(window))
                    if row.dedup_until.is_some_and(|until| until <= window.now) =>
                {
                    row.dedup_key = None;
                    state.dedup.remove(key);
                }
                _ => return Ok(None),
            }
        }
        // The routing, again: the tenant only while the binding it was
        // routed by still holds it (none without a guard).
        let tenant = event.tenant.as_ref().filter(|tenant| {
            event
                .route_guard
                .as_ref()
                .is_some_and(|guard| holds(&records, tenant, guard))
        });
        if event.tenant.is_some() && tenant.is_none() {
            tracing::warn!(
                event_type = %event.event_type,
                "an event's tenant lost the binding it was routed by while it was recorded \
                 (or holds it again, bound after the event): kept operator-only"
            );
        }
        let name = stream_of(tenant);
        let stream = state.streams.entry(name.clone()).or_default();
        stream.last += 1;
        let sequence = stream.last;
        stream.rows.insert(
            sequence,
            MemoryRow {
                dedup_key: event.dedup_key.clone(),
                dedup_until: event.dedup_window.map(|window| window.until),
                event: StoredEvent {
                    sequence,
                    id: event.id.clone(),
                    tenant: tenant.cloned(),
                    phone_number_id: event.phone_number_id.clone(),
                    waba_id: event.waba_id.clone(),
                    event_type: event.event_type.clone(),
                    data: event.data.clone(),
                    created_at: OffsetDateTime::now_utc(),
                },
            },
        );
        if let Some(key) = &event.dedup_key {
            state.dedup.insert(key.clone(), (name, sequence));
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
            .map(|(_, row)| &row.event)
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

    async fn purge(&self, older_than: Duration) -> StoreResult<u64> {
        let mut state = self.lock();
        let cutoff = OffsetDateTime::now_utc() - older_than;
        let mut purged = 0u64;
        let mut gone: Vec<String> = Vec::new();
        for stream in state.streams.values_mut() {
            // The prefix up to the newest event older than the cutoff.
            let Some(through) = stream
                .rows
                .values()
                .filter(|row| row.event.created_at < cutoff)
                .map(|row| row.event.sequence)
                .max()
            else {
                continue;
            };
            let kept = stream.rows.split_off(&(through + 1));
            let removed = std::mem::replace(&mut stream.rows, kept);
            purged += u64::try_from(removed.len()).unwrap_or(u64::MAX);
            gone.extend(removed.into_values().filter_map(|row| row.dedup_key));
            stream.purged_through = stream.purged_through.max(through);
        }
        for key in gone {
            state.dedup.remove(&key);
        }
        Ok(purged)
    }
}
