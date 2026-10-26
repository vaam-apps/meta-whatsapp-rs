//! The core's conformance suites (`meta_whatsapp_server_core::conformance`)
//! on the memory backend (Postgres: `live_postgres.rs`), and what they
//! refuse: a backend whose `kv()` is a new store on each call, and an
//! outbox that ignores the route guard (S2's security review, L4).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt as _;
use meta_whatsapp_rs::adapters::store::MemoryKvStore;
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_server::store::events::{EventPage, EventQuery, NewEvent, StoredEvent};
use meta_whatsapp_server::store::{
    Backend, BackendKind, IdempotencyRecords, Janitor, LeaderLock, MemoryBackend, Outbox,
    RecordStore, SchemaMigrator, StoreResult,
};
use meta_whatsapp_server_core::conformance;

/// Roadmap S3: every suite, each on a memory backend of its own, raced on
/// four threads (`conformance::races`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_memory_backend_passes_the_conformance_suite() {
    let report =
        conformance::run(async || Arc::new(MemoryBackend::new()) as Arc<dyn Backend>).await;
    eprintln!("memory, the races: {report:?}");
}

/// A memory backend with one port swapped: what the suites must refuse.
struct Altered {
    inner: MemoryBackend,
    /// `kv()` builds a new store on each call (each call's writes lost to
    /// the next).
    fresh_kv: bool,
    /// The outbox, instead of the memory store's own.
    outbox: Option<Arc<dyn Outbox>>,
}

#[async_trait]
impl Backend for Altered {
    fn kind(&self) -> BackendKind {
        BackendKind::Other("altered")
    }
    fn records(&self) -> Arc<dyn RecordStore> {
        self.inner.records()
    }
    fn idempotency(&self) -> Arc<dyn IdempotencyRecords> {
        self.inner.idempotency()
    }
    fn outbox(&self) -> Arc<dyn Outbox> {
        self.outbox.clone().unwrap_or_else(|| self.inner.outbox())
    }
    fn leader_lock(&self) -> Arc<dyn LeaderLock> {
        self.inner.leader_lock()
    }
    fn janitor(&self) -> Arc<dyn Janitor> {
        self.inner.janitor()
    }
    fn migrator(&self) -> Arc<dyn SchemaMigrator> {
        self.inner.migrator()
    }
    fn kv(&self) -> Arc<dyn KvStore> {
        if self.fresh_kv {
            Arc::new(MemoryKvStore::new())
        } else {
            self.inner.kv()
        }
    }
    fn conversations(&self) -> Arc<dyn ConversationStore> {
        self.inner.conversations()
    }
    async fn close(&self) {}
}

/// The message `case` panicked with; `None` when it passed.
async fn failure(case: impl Future<Output = ()>) -> Option<String> {
    let payload = AssertUnwindSafe(case).catch_unwind().await.err()?;
    Some(
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_default(),
    )
}

/// Roadmap S3's decisive case for the bundle: a `Backend` whose `kv()`
/// returns a new store on each call fails the same-data case, at the
/// read through the second handle.
#[tokio::test]
async fn a_backend_whose_kv_is_new_on_each_call_fails_the_same_data_case() {
    let backend = Altered {
        inner: MemoryBackend::new(),
        fresh_kv: true,
        outbox: None,
    };
    let failed =
        failure(conformance::invariants::every_accessor_hands_out_the_same_data(&backend)).await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("written through another kv() handle")),
        "{failed:?}"
    );
    // The control: the same kind of backend, with its own kv(), passes.
    let backend = Altered {
        inner: MemoryBackend::new(),
        fresh_kv: false,
        outbox: None,
    };
    assert_eq!(
        failure(conformance::invariants::every_accessor_hands_out_the_same_data(&backend)).await,
        None
    );
}

/// An outbox that keeps every row's tenant, whatever its route guard says
/// (each tenant's stream from 1, no dedup, no purge): a third-party outbox
/// that ignores the guard, which fails open.
#[derive(Default)]
struct IgnoresTheGuard {
    rows: Mutex<Vec<StoredEvent>>,
}

#[async_trait]
impl Outbox for IgnoresTheGuard {
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>> {
        let mut rows = self.rows.lock().unwrap();
        let sequence =
            1 + i64::try_from(rows.iter().filter(|row| row.tenant == event.tenant).count())
                .unwrap();
        rows.push(StoredEvent {
            sequence,
            id: event.id.clone(),
            tenant: event.tenant.clone(),
            phone_number_id: event.phone_number_id.clone(),
            waba_id: event.waba_id.clone(),
            event_type: event.event_type.clone(),
            data: event.data.clone(),
            created_at: time::OffsetDateTime::now_utc(),
        });
        Ok(Some(sequence))
    }

    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage> {
        let rows = self.rows.lock().unwrap();
        let stream: Vec<&StoredEvent> = rows
            .iter()
            .filter(|row| row.tenant.as_ref() == Some(&query.tenant))
            .collect();
        let after = query.after.unwrap_or(0);
        let events: Vec<StoredEvent> = stream
            .iter()
            .filter(|row| row.sequence > after)
            .take(query.limit)
            .map(|row| (*row).clone())
            .collect();
        let high_water = stream.last().map_or(0, |row| row.sequence);
        Ok(EventPage {
            more: events.last().is_some_and(|last| last.sequence < high_water),
            events,
            purged_through: 0,
            high_water,
        })
    }

    async fn purge(&self, _older_than: Duration) -> StoreResult<u64> {
        Ok(0)
    }
}

/// S2's security review, L4, closed by roadmap S3: an outbox that ignores
/// the route guard (a tenant kept though its binding moved, began after
/// the event, or was never named) fails the guard's cases.
#[tokio::test]
async fn an_outbox_that_ignores_the_route_guard_fails_the_suite() {
    let backend = Altered {
        inner: MemoryBackend::new(),
        fresh_kv: false,
        outbox: Some(Arc::new(IgnoresTheGuard::default())),
    };
    let failed =
        failure(conformance::invariants::the_route_guard_is_checked_with_the_insert(&backend))
            .await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("only the rows whose guard held")),
        "{failed:?}"
    );
    let failed = failure(
        conformance::invariants::a_binding_of_the_events_own_second_fails_the_guard(&backend),
    )
    .await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("only the rows dated after the binding's own second")),
        "{failed:?}"
    );
}
