//! Housekeeping over a bundle (roadmap S4: the core's
//! `meta_whatsapp_server_core::housekeeping`, which the service spawns on
//! the backend it serves): one round at a time under the backend's
//! leader lock, purging its outbox past retention, its expired
//! idempotency records and what its janitor sweeps, round after round
//! until stopped. On a memory backend with a counting janitor, defined
//! here (Postgres, live: `live_postgres.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_server::model::{IdempotencyClaim, IdempotencyKey, TenantId};
use meta_whatsapp_server::store::events::{
    EventPage, EventQuery, GuardedBinding, NewEvent, RouteGuard,
};
use meta_whatsapp_server::store::{
    Backend, BackendKind, HOUSEKEEPING, HOUSEKEEPING_LEASE, IdempotencyRecords, Janitor,
    LeaderLock, MemoryBackend, Outbox, RecordStore, SchemaMigrator, StoreResult,
};
use meta_whatsapp_server_core::housekeeping::{round, run};
use tokio::sync::watch;

/// A janitor counting its sweeps.
#[derive(Debug, Default)]
struct Counting(AtomicUsize);

impl Counting {
    fn sweeps(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Janitor for Counting {
    async fn purge_expired(&self) -> StoreResult<u64> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(0)
    }
}

/// A memory backend whose janitor counts its sweeps.
struct Swept {
    memory: MemoryBackend,
    janitor: Arc<Counting>,
}

impl Swept {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            memory: MemoryBackend::new(),
            janitor: Arc::new(Counting::default()),
        })
    }
}

#[async_trait]
impl Backend for Swept {
    fn kind(&self) -> BackendKind {
        BackendKind::Other("swept")
    }
    fn records(&self) -> Arc<dyn RecordStore> {
        self.memory.records()
    }
    fn idempotency(&self) -> Arc<dyn IdempotencyRecords> {
        self.memory.idempotency()
    }
    fn outbox(&self) -> Arc<dyn Outbox> {
        self.memory.outbox()
    }
    fn leader_lock(&self) -> Arc<dyn LeaderLock> {
        self.memory.leader_lock()
    }
    fn janitor(&self) -> Arc<dyn Janitor> {
        self.janitor.clone()
    }
    fn migrator(&self) -> Arc<dyn SchemaMigrator> {
        self.memory.migrator()
    }
    fn kv(&self) -> Arc<dyn KvStore> {
        self.memory.kv()
    }
    fn conversations(&self) -> Arc<dyn ConversationStore> {
        self.memory.conversations()
    }
    async fn close(&self) {}
}

const TENANT: &str = "tenant-a";
const WABA: &str = "102290129340398";

/// `backend` with tenant-a, holding [`WABA`].
async fn with_a_tenant(backend: &dyn Backend) -> TenantId {
    let tenant = TenantId::parse(TENANT).unwrap();
    let records = backend.records();
    records.create_tenant(&tenant, "").await.unwrap().unwrap();
    records
        .bind_waba(&tenant, &WabaId::new(WABA), &[])
        .await
        .unwrap();
    tenant
}

/// An event of tenant-a's, routed by its WABA.
fn row(id: &str) -> NewEvent {
    NewEvent {
        id: id.to_owned(),
        dedup_key: None,
        dedup_window: None,
        tenant: TenantId::parse(TENANT),
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Waba(WabaId::new(WABA)),
            not_after: None,
        }),
        phone_number_id: None,
        waba_id: Some(WabaId::new(WABA)),
        event_type: "message_received".to_owned(),
        data: "{}".to_owned(),
    }
}

/// tenant-a's first page of events.
async fn page(backend: &dyn Backend) -> EventPage {
    backend
        .outbox()
        .page(&EventQuery {
            tenant: TenantId::parse(TENANT).unwrap(),
            after: None,
            types: None,
            phone_number_id: None,
            limit: 10,
            max_bytes: meta_whatsapp_server::events::MAX_PAGE_DATA_BYTES,
        })
        .await
        .unwrap()
}

/// An idempotency record of tenant-a's under `key`, expired a millisecond
/// after it was claimed.
async fn expiring_record(backend: &dyn Backend, key: &str) {
    let brief = Duration::from_millis(1);
    let claimed = backend
        .idempotency()
        .claim_idempotency_key(
            &TenantId::parse(TENANT).unwrap(),
            &IdempotencyKey::parse(key).unwrap(),
            &[7; 32],
            "claim",
            brief,
            brief,
        )
        .await
        .unwrap();
    assert_eq!(claimed, IdempotencyClaim::Claimed);
}

/// Completes once `stop` says so.
async fn stopped(mut stop: watch::Receiver<bool>) {
    let _ = stop.wait_for(|stop| *stop).await;
}

/// Housekeeping purges the outbox past retention, round after round, and
/// the expired idempotency records (M1b's purge, in the same loop), and
/// stops with the service. Decisive: each purge in the round.
#[tokio::test]
async fn housekeeping_purges_past_retention_until_stopped() {
    let backend = Swept::new();
    with_a_tenant(backend.as_ref()).await;
    // An idempotency record expired before the first round.
    expiring_record(backend.as_ref(), "order:1234:shipped").await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let first = backend
        .outbox()
        .insert(&row("evt_1"))
        .await
        .unwrap()
        .unwrap();
    let (stop, stop_rx) = watch::channel(false);
    let task = tokio::spawn(run(
        backend.clone(),
        Duration::from_millis(1),
        Duration::from_millis(20),
        stopped(stop_rx),
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while page(backend.as_ref()).await.purged_through < first {
        assert!(tokio::time::Instant::now() < deadline, "never purged");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // A later event goes in a later round.
    let second = backend
        .outbox()
        .insert(&row("evt_2"))
        .await
        .unwrap()
        .unwrap();
    while page(backend.as_ref()).await.purged_through < second {
        assert!(tokio::time::Instant::now() < deadline, "no second round");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The first round, done before the second's outbox purge, took the
    // expired record: nothing is left to purge.
    assert_eq!(
        backend
            .idempotency()
            .purge_idempotency_keys()
            .await
            .unwrap(),
        0,
        "the expired idempotency record was never purged"
    );
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("stopped with the service")
        .unwrap();
}

/// A round runs under one housekeeping turn, all its purges (roadmap S2):
/// a replica finding the turn taken purges nothing this round (no outbox
/// event, no idempotency record, no sweep), and the turn is free again
/// after a round. Decisive: the turn in `round`, and each purge under it.
#[tokio::test]
async fn a_round_runs_under_one_housekeeping_turn() {
    let backend = Swept::new();
    with_a_tenant(backend.as_ref()).await;
    // An event and an idempotency record, both past their time when the
    // rounds run.
    let sequence = backend
        .outbox()
        .insert(&row("evt_round"))
        .await
        .unwrap()
        .unwrap();
    expiring_record(backend.as_ref(), "first").await;
    tokio::time::sleep(Duration::from_millis(5)).await;

    // Another replica's turn, on the backend's own leader lock.
    let held = backend
        .leader_lock()
        .try_exclusive(HOUSEKEEPING, HOUSEKEEPING_LEASE)
        .await
        .unwrap()
        .unwrap();
    let ran = round(backend.as_ref(), Duration::ZERO).await;
    assert!(!ran, "a round ran during another replica's turn");
    assert_eq!(backend.janitor.sweeps(), 0, "swept outside the turn");
    assert_eq!(
        page(backend.as_ref()).await.events.len(),
        1,
        "purged outside the turn"
    );
    assert_eq!(
        backend
            .idempotency()
            .purge_idempotency_keys()
            .await
            .unwrap(),
        1,
        "the idempotency records were purged outside the turn"
    );

    held.release().await.unwrap();
    expiring_record(backend.as_ref(), "second").await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert!(round(backend.as_ref(), Duration::ZERO).await);
    assert_eq!(backend.janitor.sweeps(), 1, "the round swept");
    let after = page(backend.as_ref()).await;
    assert!(after.events.is_empty(), "the round purged the outbox");
    assert_eq!(after.purged_through, sequence);
    assert_eq!(
        backend
            .idempotency()
            .purge_idempotency_keys()
            .await
            .unwrap(),
        0,
        "the round purged the idempotency records"
    );
    assert!(
        backend
            .leader_lock()
            .try_exclusive(HOUSEKEEPING, HOUSEKEEPING_LEASE)
            .await
            .unwrap()
            .is_some(),
        "the round released its turn"
    );
}

/// Housekeeping sweeps the backend's expired rows round after round.
/// Decisive: the sweep in the round.
#[tokio::test]
async fn housekeeping_sweeps_the_expired_rows() {
    let backend = Swept::new();
    let (stop, stop_rx) = watch::channel(false);
    let task = tokio::spawn(run(
        backend.clone(),
        Duration::from_secs(60),
        Duration::from_millis(20),
        stopped(stop_rx),
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while backend.janitor.sweeps() < 2 {
        assert!(tokio::time::Instant::now() < deadline, "never swept twice");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("stopped with the service")
        .unwrap();
}

/// Once `stop` completed, no round starts: housekeeping stopped before its
/// first tick sweeps nothing. Decisive: `stop` polled before the tick.
#[tokio::test]
async fn a_stopped_housekeeping_starts_no_round() {
    let backend = Swept::new();
    run(
        backend.clone(),
        Duration::from_secs(60),
        Duration::from_millis(20),
        async {},
    )
    .await;
    assert_eq!(backend.janitor.sweeps(), 0, "a round ran after the stop");
}
