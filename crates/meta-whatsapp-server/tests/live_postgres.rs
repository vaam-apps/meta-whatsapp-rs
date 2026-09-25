//! The service on a real Postgres. Skipped unless
//! `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set; `just test-live` sets
//! `META_WHATSAPP_RS_REQUIRE_LIVE=1`, which turns the skip into a failure.
//! Every test runs in a fresh schema.
//!
//! Acceptance test M1.7, the M1a part: parallel migrations of two
//! instances succeed, and the log capture of `logs.rs` holds on Postgres.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;

use common::capture::{Captured, check, exercise, subscriber};
use common::{ALL_SCOPES, Call, Harness, TestDb};
use meta_whatsapp_rs::adapters::store::PostgresKvStore;
use meta_whatsapp_rs::adapters::store::postgres::sqlx;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_server::model::{AllowedTenants, TenantId};
use meta_whatsapp_server::store::{MIGRATION_LOCK, MIGRATIONS_TABLE, PgStore, migrate};

async fn harness(db: &TestDb) -> Harness {
    let pool = db.pool(10).await;
    migrate(&pool).await.unwrap();
    Harness::on(
        Arc::new(PgStore::new(pool.clone())),
        Arc::new(PostgresKvStore::new(pool)),
    )
}

#[tokio::test]
async fn live_postgres_store_passes_the_suite() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(5).await;
    migrate(&pool).await.unwrap();
    common::store_suite::run(&PgStore::new(pool)).await;
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
    assert_eq!(service, [1]);
    let library: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM wa_sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(&first)
            .await
            .unwrap();
    assert_eq!(library, [1, 2, 3]);
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

/// M1.3 on Postgres: B's key on A's number and WABA is 404, and the vault
/// is not read.
#[tokio::test]
async fn live_postgres_another_tenants_number_is_not_found_before_the_vault() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = harness(&db).await;
    h.tenant("tenant-a").await;
    h.tenant("tenant-b").await;
    h.connect(
        "tenant-a",
        "102290129340398",
        &["106540352242922"],
        "TOKEN-OF-A",
    )
    .await;
    let b = h.tenant_key("tenant-b", &ALL_SCOPES).await;
    let platform = h
        .platform_key(
            AllowedTenants::Only(vec![TenantId::parse("tenant-b").unwrap()]),
            &ALL_SCOPES,
        )
        .await;
    let before = h.kv.vault_reads();
    for call in [
        Call::get("/v1/numbers/106540352242922").key(&b),
        Call::get("/v1/numbers/106540352242922/profile")
            .key(&platform)
            .tenant("tenant-b"),
        Call::new(
            meta_whatsapp_rs::webhooks::axum::http::Method::DELETE,
            "/v1/wabas/102290129340398",
        )
        .key(&b),
    ] {
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found")
        );
    }
    assert_eq!(h.kv.vault_reads(), before);
    assert!(h.graph.requests().is_empty());
}

/// M1.7 on Postgres: sqlx's own events join the capture, and still no
/// secret, key or phone number is logged.
#[tokio::test]
async fn live_postgres_logs_hold_no_secret_key_or_phone_number() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = harness(&db).await;
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let secrets = exercise(&h).await;
    let logs = captured.text();
    check(&logs, &secrets);
    // The Postgres store's secret digests are bound parameters, never SQL
    // text.
    assert!(!logs.contains("secret_sha256 = '"));
}
