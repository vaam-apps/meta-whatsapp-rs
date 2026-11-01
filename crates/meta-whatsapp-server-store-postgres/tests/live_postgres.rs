//! The Postgres backend on a real Postgres, with no service around it.
//! Skipped unless `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set; `just
//! test-live` sets `META_WHATSAPP_RS_REQUIRE_LIVE=1`, which turns the skip
//! into a failure. Every test runs in a fresh schema, which
//! `support::TestDb` drops when the test ends, a panicking one included.
//!
//! Roadmap S3 and S7: the core's conformance suite on this backend. M1.7's
//! parallel migrations, and the Postgres-only cases that reach into its
//! own SQL (locks held across sessions, two purges at once, a deadlocked
//! insert). The service's live tests over its whole harness stay in
//! `crates/meta-whatsapp-server/tests/live_postgres.rs` (all of these were
//! there until roadmap S7 split the crates), with the migration
//! continuity of the split (`live_postgres_a_database_main_migrated_*`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod support;

use meta_whatsapp_rs::adapters::store::postgres::sqlx;
use meta_whatsapp_server_core::conformance;
use meta_whatsapp_server_core::conformance::outbox as outbox_suite;
use meta_whatsapp_server_core::model::TenantId;
use meta_whatsapp_server_core::outbox::Outbox as EventStore;
use meta_whatsapp_server_store_postgres::{
    HOUSEKEEPING_LOCK, MIGRATION_LOCK, MIGRATIONS_TABLE, PgEventStore, PgStore, migrate,
};
use support::TestDb;

/// Roadmap S3: the core's conformance suites
/// (`meta_whatsapp_server_core::conformance`) on Postgres, each suite on a
/// schema of its own (dropped when the test ends), raced on four threads
/// over a pool of ten.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_backend_passes_the_conformance_suite() {
    use meta_whatsapp_server_core::backend::Backend;
    use meta_whatsapp_server_store_postgres::PgBackend;
    use std::sync::{Arc, Mutex};
    if support::postgres_url().is_none() {
        return;
    }
    // Every suite's schema, kept until the test ends.
    let schemas: Mutex<Vec<TestDb>> = Mutex::new(Vec::new());
    let report = conformance::run(async || {
        let db = TestDb::new().await.expect("the database was configured");
        let pool = db.pool(10).await;
        migrate(&pool).await.unwrap();
        schemas.lock().unwrap().push(db);
        // A retention, so that the inbox suite's housekeeping case purges
        // (roadmap M2a).
        Arc::new(
            PgBackend::new(pool)
                .with_inbox_retention(meta_whatsapp_rs::core::store::Retention::days(30)),
        ) as Arc<dyn Backend>
    })
    .await;
    eprintln!("postgres, the races: {report:?}");
}

/// Two instances starting at once on an empty database both migrate, and
/// each migration is applied once.
#[tokio::test]
async fn live_postgres_parallel_migrations_succeed() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let first = db.pool(3).await;
    let second = db.pool(3).await;
    let third = db.pool(3).await;
    let (a, b, c) = tokio::join!(migrate(&first), migrate(&second), migrate(&third));
    a.unwrap();
    b.unwrap();
    c.unwrap();
    let service: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT version FROM {MIGRATIONS_TABLE} WHERE success ORDER BY version"
    )))
    .fetch_all(&first)
    .await
    .unwrap();
    assert_eq!(service, [1, 2, 3, 4]);
    let library: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM wa_sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(&first)
            .await
            .unwrap();
    assert_eq!(library, [1, 2, 3, 4]);
    // And again, on a migrated database: a no-op. It also proves the lock
    // was released: a session of `first` still holding it would block this
    // forever. (Counting holders in pg_locks would race with other tests:
    // the lock is per database, not per schema.)
    migrate(&second).await.unwrap();
}

/// Sessions holding the service's migration lock (any test's).
async fn lock_holders(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND granted \
         AND classid::bigint = (($1::bigint >> 32) & 4294967295) \
         AND objid::bigint = ($1::bigint & 4294967295) AND objsubid = 1",
    )
    .bind(MIGRATION_LOCK)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The service's advisory lock spans both migrations: while another
/// session holds it, `migrate` waits. Decisive: taking the lock in
/// `migrate`.
#[tokio::test]
async fn live_postgres_migrations_wait_for_the_service_lock() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let holder = db.pool(2).await;
    let mut held = holder.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *held)
        .await
        .unwrap();
    assert!(
        lock_holders(&holder).await >= 1,
        "the query sees a held lock"
    );
    let migrating = db.pool(3).await;
    let task = tokio::spawn(async move { migrate(&migrating).await });
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    assert!(
        !task.is_finished(),
        "migrate ran while the service lock was held"
    );
    let applied: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables \
         WHERE table_schema = current_schema() AND table_name = 'wa_server_tenants'",
    )
    .fetch_one(&holder)
    .await
    .unwrap();
    assert_eq!(applied, 0, "nothing created before the lock was granted");
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *held)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// Housekeeping runs on one replica at a time: while another session holds
/// its lock (`HOUSEKEEPING_LOCK`, a replica of the previous release
/// included, whose purges took it themselves), a round purges nothing;
/// once it is free, a round purges. Since roadmap S2 the purges take no
/// lock of their own: the round's one `LeaderLock` turn is the lock.
/// Decisive: the turn in the core's `housekeeping::round`.
#[tokio::test]
async fn live_postgres_one_replica_purges_at_a_time() {
    use meta_whatsapp_server_core::housekeeping::round;
    use meta_whatsapp_server_store_postgres::PgBackend;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(4).await;
    migrate(&pool).await.unwrap();
    let backend = PgBackend::new(pool.clone());
    let store = PgEventStore::new(pool.clone());
    store
        .insert(&outbox_suite::row(None, "message_received", "1", None))
        .await
        .unwrap();
    let stored = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM wa_server_events")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let mut held = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(HOUSEKEEPING_LOCK)
        .execute(&mut *held)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let ran = round(&backend, std::time::Duration::ZERO).await;
    assert!(!ran, "a round ran under another replica's lock");
    assert_eq!(stored().await, 1, "purged under another replica's lock");
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(HOUSEKEEPING_LOCK)
        .execute(&mut *held)
        .await
        .unwrap();
    // Other tests' rounds may hold the database's lock for a moment.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !round(&backend, std::time::Duration::ZERO).await {
        assert!(std::time::Instant::now() < deadline, "never a turn");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(stored().await, 0, "the round purged");
}

/// Two replicas racing for one key: exactly one claims it. Decisive: the
/// unique (tenant, key) row and the conditional insert.
#[tokio::test]
async fn live_postgres_one_of_two_racing_claims_wins() {
    use meta_whatsapp_server_core::model::{IdempotencyClaim, IdempotencyKey};
    use meta_whatsapp_server_core::store::{IdempotencyRecords as _, RecordStore as _};
    let Some(db) = TestDb::new().await else {
        return;
    };
    let first = db.pool(3).await;
    migrate(&first).await.unwrap();
    let second = db.pool(3).await;
    let (a, b) = (PgStore::new(first), PgStore::new(second));
    let tenant = TenantId::parse("merchant-42").unwrap();
    a.create_tenant(&tenant, "").await.unwrap().unwrap();
    let minute = std::time::Duration::from_secs(60);
    for round in 0..20 {
        let key = IdempotencyKey::parse(&format!("race-{round}")).unwrap();
        let (x, y) = tokio::join!(
            a.claim_idempotency_key(&tenant, &key, &[1; 32], "a", minute, minute * 60),
            b.claim_idempotency_key(&tenant, &key, &[1; 32], "b", minute, minute * 60),
        );
        let claimed = [x.unwrap(), y.unwrap()]
            .iter()
            .filter(|c| **c == IdempotencyClaim::Claimed)
            .count();
        assert_eq!(claimed, 1, "round {round}");
    }
}

/// `lock`'s turn of `name`, waiting while another holder has it: advisory
/// locks are the database's, and the purges of every test running on it
/// take the housekeeping one.
async fn turn_of(
    lock: &meta_whatsapp_server_store_postgres::PgLeaderLock,
    name: &str,
) -> meta_whatsapp_server_core::store::LeaderTurn {
    use meta_whatsapp_server_core::store::LeaderLock as _;
    for _ in 0..500 {
        if let Some(turn) = lock.try_exclusive(name, LEASE).await.unwrap() {
            return turn;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("another holder kept {name} for 5 s");
}

/// A lease long enough for any test's turn.
const LEASE: std::time::Duration = std::time::Duration::from_secs(60);

/// The leader lock on Postgres: one turn per name, whichever replica asks,
/// until it is released or dropped; its housekeeping turn is
/// `HOUSEKEEPING_LOCK` (what a replica of the previous release's purges
/// take), so another session cannot take that key while it is held. The
/// turns' rules run on a name of this test's own (no other test takes it,
/// so every answer is certain); the housekeeping turn is held no longer
/// than it takes to show the key taken. Decisive: the transaction-scoped
/// advisory lock in `PgLeaderLock::try_exclusive` (a session lock would
/// outlive its turn), and `lock_key`'s derivation.
#[tokio::test]
async fn live_postgres_the_leader_lock_gives_one_turn_at_a_time() {
    use meta_whatsapp_server_core::store::{HOUSEKEEPING, LeaderLock as _};
    use meta_whatsapp_server_store_postgres::PgLeaderLock;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(4).await;
    migrate(&pool).await.unwrap();
    // Two replicas.
    let (a, b) = (
        PgLeaderLock::new(pool.clone()),
        PgLeaderLock::new(db.pool(2).await),
    );
    let name = format!("test-{}", support::unique());
    let turn = a
        .try_exclusive(&name, LEASE)
        .await
        .unwrap()
        .expect("nobody holds it");
    assert!(
        b.try_exclusive(&name, LEASE).await.unwrap().is_none(),
        "one turn"
    );
    assert!(
        a.try_exclusive(&name, LEASE).await.unwrap().is_none(),
        "one turn, on the same replica too"
    );
    turn.release().await.unwrap();
    let again = b.try_exclusive(&name, LEASE).await.unwrap();
    assert!(again.is_some(), "released");
    drop(again);
    // Dropped: its transaction is rolled back as its connection goes back
    // to the pool, which ends the turn.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(turn) = a.try_exclusive(&name, LEASE).await.unwrap() {
            turn.release().await.unwrap();
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a dropped turn held on"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // The housekeeping turn is `HOUSEKEEPING_LOCK`; other names are not.
    let housekeeping = turn_of(&a, HOUSEKEEPING).await;
    assert!(
        b.try_exclusive(HOUSEKEEPING, LEASE)
            .await
            .unwrap()
            .is_none()
    );
    let mut session = pool.acquire().await.unwrap();
    let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(HOUSEKEEPING_LOCK)
        .fetch_one(&mut *session)
        .await
        .unwrap();
    let other = b.try_exclusive(&name, LEASE).await.unwrap();
    housekeeping.release().await.unwrap();
    assert!(!taken, "the housekeeping turn is HOUSEKEEPING_LOCK");
    assert!(other.is_some(), "each name its own lock");
}

/// A turn is a lease (roadmap S2): one its holder never releases (a
/// replica paused, or its connection wedged) ends once the lease has
/// passed, and another replica gets the name; the stale turn's release
/// then reports that it had ended. Decisive: the
/// `idle_in_transaction_session_timeout` of `PgLeaderLock::try_exclusive`
/// (without it the turn is held until its connection dies).
#[tokio::test]
async fn live_postgres_a_leader_turn_ends_with_its_lease() {
    use meta_whatsapp_server_core::store::LeaderLock as _;
    use meta_whatsapp_server_store_postgres::PgLeaderLock;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(2).await;
    migrate(&pool).await.unwrap();
    let (a, b) = (
        PgLeaderLock::new(pool.clone()),
        PgLeaderLock::new(db.pool(2).await),
    );
    let name = format!("test-{}", support::unique());
    let lease = std::time::Duration::from_millis(300);
    let stale = a
        .try_exclusive(&name, lease)
        .await
        .unwrap()
        .expect("nobody holds it");
    assert!(
        b.try_exclusive(&name, LEASE).await.unwrap().is_none(),
        "within the lease"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let fresh = loop {
        if let Some(turn) = b.try_exclusive(&name, LEASE).await.unwrap() {
            break turn;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the lease never ended"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert!(
        stale.release().await.is_err(),
        "the stale turn's session was ended"
    );
    assert!(
        a.try_exclusive(&name, LEASE).await.unwrap().is_none(),
        "the fresh turn holds"
    );
    fresh.release().await.unwrap();
}

/// Housekeeping on the Postgres backend sweeps the library's expired
/// key/value rows under the round's housekeeping turn, and leaves a live
/// row. Other tests' rounds take that lock too: a round that finds it
/// taken skips, and a later one sweeps. Decisive: the sweep in
/// the core's `housekeeping::round`, `PgBackend::janitor`, and
/// `PgJanitor::purge_expired`.
#[tokio::test]
async fn live_postgres_housekeeping_sweeps_expired_key_value_rows() {
    use meta_whatsapp_server_core::housekeeping;
    use meta_whatsapp_server_store_postgres::PgBackend;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(10).await;
    migrate(&pool).await.unwrap();
    // Both last written a day ago, past the library's purge grace: one
    // expired a day ago, one expires in a day.
    sqlx::query(
        "INSERT INTO wa_kv (namespace, key, value, version, expires_at, updated_at) VALUES \
         ('wa.test', 'expired', decode('00', 'hex'), nextval('wa_kv_version_seq'), \
          now() - interval '1 day', now() - interval '1 day'), \
         ('wa.test', 'live', decode('00', 'hex'), nextval('wa_kv_version_seq'), \
          now() + interval '1 day', now() - interval '1 day')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let keys = || async {
        sqlx::query_scalar::<_, String>(
            "SELECT key FROM wa_kv WHERE namespace = 'wa.test' ORDER BY key",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    assert_eq!(keys().await, ["expired", "live"]);
    let backend = std::sync::Arc::new(PgBackend::new(pool.clone()));
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(housekeeping::run(
        backend,
        meta_whatsapp_server_core::events::DEFAULT_OUTBOX_RETENTION,
        std::time::Duration::from_millis(50),
        async move {
            let _ = stopped.wait_for(|stop| *stop).await;
        },
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while keys().await != ["live"] {
        assert!(
            std::time::Instant::now() < deadline,
            "the expired row was never swept"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("stopped with the service")
        .unwrap();
}

/// Two outbox purges at once (a round that outlived its lease, and the
/// next replica's): a row both chose is deleted once and counted once, and
/// the stream's `purged_through` never moves back, though the purge with
/// the older cutoff commits last. The first purge deletes all ten rows,
/// then waits to mark the stream (another session holds its row); the
/// second chooses the older five, which the first holds, and waits for it.
/// Decisive: `GREATEST` in the purge's mark (without it the later commit
/// sets the older cut), and the count of the rows deleted, not chosen.
#[tokio::test]
async fn live_postgres_two_purges_at_once_count_each_row_once() {
    use std::time::Duration;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(6).await;
    migrate(&pool).await.unwrap();
    let store = std::sync::Arc::new(PgEventStore::new(pool.clone()));
    outbox_suite::bind(&PgStore::new(pool.clone()), "overlap", "41").await;
    let insert = || async {
        store
            .insert(&outbox_suite::row(
                Some("overlap"),
                "message_received",
                "41",
                None,
            ))
            .await
            .unwrap()
            .unwrap()
    };
    for _ in 0..5 {
        insert().await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    for _ in 0..5 {
        insert().await;
    }
    let holder_pool = db.pool(1).await;
    let mut holder = holder_pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM wa_server_event_streams WHERE stream = 'overlap' FOR UPDATE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let purge = |older_than: Duration| {
        let store = store.clone();
        tokio::spawn(async move { store.purge(older_than).await })
    };
    let first = purge(Duration::ZERO);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!first.is_finished(), "the first purge did not wait to mark");
    // Between the two batches: the older five.
    let second = purge(Duration::from_secs(1));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!second.is_finished(), "the second purge did not wait");
    holder.rollback().await.unwrap();
    let first = first.await.unwrap().unwrap();
    let second = second.await.unwrap().unwrap();
    assert_eq!(
        (first, second),
        (10, 0),
        "each row deleted and counted once"
    );
    let page = store
        .page(&meta_whatsapp_server_core::outbox::EventQuery {
            tenant: TenantId::parse("overlap").unwrap(),
            after: None,
            types: None,
            phone_number_id: None,
            limit: 100,
            max_bytes: 1024 * 1024,
        })
        .await
        .unwrap();
    assert!(page.events.is_empty());
    assert_eq!(
        (page.purged_through, page.high_water),
        (10, 10),
        "the older cut, committed last, moved purged_through back"
    );
}

/// A deadlock Postgres breaks by aborting an outbox insert is contention:
/// the library's typed `StorageError::Busy` (the webhook pipeline answers
/// Meta `503`, and Meta retries), never an opaque backend failure. The
/// insert holds its guarded binding (the number's row, `FOR KEY SHARE`)
/// and waits for its stream's row, which another session holds and which
/// then waits for the number's row; the insert, waiting first, is the one
/// Postgres aborts (40P01), and the other session goes on. Decisive:
/// `40P01` in `busy_or_backend`.
#[tokio::test]
async fn live_postgres_a_deadlocked_insert_is_busy() {
    use std::time::Duration;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(4).await;
    migrate(&pool).await.unwrap();
    let store = std::sync::Arc::new(PgEventStore::new(pool.clone()));
    outbox_suite::bind(&PgStore::new(pool.clone()), "deadlock", "43").await;
    let row = || outbox_suite::row(Some("deadlock"), "message_received", "43", None);
    // The stream's row exists.
    store.insert(&row()).await.unwrap().unwrap();
    let holder_pool = db.pool(1).await;
    let mut holder = holder_pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM wa_server_event_streams WHERE stream = 'deadlock' FOR UPDATE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let inserted = {
        let (store, event) = (store.clone(), row());
        tokio::spawn(async move { store.insert(&event).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !inserted.is_finished(),
        "the insert did not wait for its stream"
    );
    // The number's row, which the insert holds: a cycle.
    let locked =
        sqlx::query("SELECT 1 FROM wa_server_numbers WHERE phone_number_id = '43' FOR UPDATE")
            .execute(&mut *holder)
            .await;
    let inserted = inserted.await.unwrap();
    assert!(
        locked.is_ok(),
        "Postgres aborted the other session instead: {locked:?}"
    );
    holder.rollback().await.unwrap();
    match inserted {
        Err(error) => assert!(error.is_busy(), "a deadlock reported as {error}"),
        Ok(sequence) => panic!("inserted {sequence:?} through a deadlock"),
    }
}
