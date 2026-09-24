//! Postgres adapters against a real server. Skipped unless
//! `WA_RS_TEST_POSTGRES_URL` is set; `WA_RS_REQUIRE_LIVE=1` (as in
//! `just test-live`) turns the skip into a failure.
//!
//! Every test runs in its own freshly created schema (the pool's
//! `search_path`), so runs in parallel — and repeated runs against the same
//! database — never see each other's tables.
#![cfg(feature = "postgres")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{AssertSqlSafe, PgPool};
use time::macros::datetime;
use wa_adapters::store::postgres::{self, TablePrefix};
use wa_adapters::store::{
    PostgresConversationStore, PostgresKvStore, conformance, conversation_conformance,
};
use wa_core::store::{ConversationStore, Expiry, KvStore, StoreKey};

/// A schema of our own on the test database.
struct TestDb {
    url: String,
    schema: String,
    admin: PgPool,
    pool: PgPool,
}

impl TestDb {
    async fn new() -> Option<Self> {
        let url = common::service_url("WA_RS_TEST_POSTGRES_URL")?;
        let schema = format!("wa_test_{}", common::unique());
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect to WA_RS_TEST_POSTGRES_URL");
        sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin)
            .await
            .unwrap();
        let pool = Self::pool_on(&url, &schema, 10).await;
        Some(Self {
            url,
            schema,
            admin,
            pool,
        })
    }

    /// Another, independent pool on the same schema: a second "process".
    async fn pool_on(url: &str, schema: &str, size: u32) -> PgPool {
        let options = PgConnectOptions::from_str(url)
            .unwrap()
            .options([("search_path", schema)]);
        PgPoolOptions::new()
            .max_connections(size)
            .connect_with(options)
            .await
            .unwrap()
    }

    async fn table_names(&self) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = $1 ORDER BY table_name",
        )
        .bind(&self.schema)
        .fetch_all(&self.admin)
        .await
        .unwrap()
    }

    async fn drop(self) {
        self.pool.close().await;
        sqlx::query(AssertSqlSafe(format!(
            "DROP SCHEMA {} CASCADE",
            self.schema
        )))
        .execute(&self.admin)
        .await
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_kv_conformance() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let store = PostgresKvStore::new(db.pool.clone());
    conformance::run_with_real_time(&store, Duration::from_millis(500)).await;
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_conversation_conformance() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let store = PostgresConversationStore::new(db.pool.clone());
    conversation_conformance::run(&store).await;
    db.drop().await;
}

/// The history and inbox order must be byte order whatever the server's
/// default collation. Alpine (musl) Postgres collates `en_US.utf8` byte-wise
/// anyway, so the test above cannot tell; a database created with an ICU
/// locale orders `a` before `B` and can.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_conversation_order_ignores_the_database_collation() {
    let Some(url) = common::service_url("WA_RS_TEST_POSTGRES_URL") else {
        return;
    };
    let name = format!("wa_test_icu_{}", common::unique());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    sqlx::query(AssertSqlSafe(format!(
        "CREATE DATABASE {name} TEMPLATE template0 \
         LOCALE_PROVIDER icu ICU_LOCALE 'en-US' LOCALE 'C.UTF-8'"
    )))
    .execute(&admin)
    .await
    .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect_with(PgConnectOptions::from_str(&url).unwrap().database(&name))
        .await
        .unwrap();
    let locale_orders_a_first: bool = sqlx::query_scalar("SELECT 'a'::text < 'B'::text")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        locale_orders_a_first,
        "the probe database must use a non-byte collation"
    );

    postgres::migrate(&pool).await.unwrap();
    conversation_conformance::run(&PostgresConversationStore::new(pool.clone())).await;

    pool.close().await;
    sqlx::query(AssertSqlSafe(format!("DROP DATABASE {name} WITH (FORCE)")))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_races_across_independent_pools() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let other = TestDb::pool_on(&db.url, &db.schema, 10).await;
    let a = PostgresKvStore::new(db.pool.clone());
    let b = PostgresKvStore::new(other.clone());

    let k = StoreKey::new("wa.race", common::unique());
    let racers = (0..32u8).map(|i| {
        let (store, k) = (if i % 2 == 0 { &a } else { &b }, k.clone());
        async move {
            store
                .put_if_absent(&k, vec![i], Expiry::Never)
                .await
                .unwrap()
        }
    });
    let wins = futures::future::join_all(racers)
        .await
        .into_iter()
        .flatten()
        .count();
    assert_eq!(wins, 1, "one put_if_absent wins across two pools");

    let v = a.get(&k).await.unwrap().unwrap().version;
    let racers = (0..32u8).map(|i| {
        let (store, k) = (if i % 2 == 0 { &a } else { &b }, k.clone());
        async move {
            store
                .compare_and_swap(&k, v, Some(vec![i]), Expiry::Never)
                .await
                .unwrap()
        }
    });
    let wins = futures::future::join_all(racers)
        .await
        .into_iter()
        .flatten()
        .count();
    assert_eq!(wins, 1, "one compare_and_swap wins across two pools");
    other.close().await;
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_migrate_is_idempotent_under_concurrency() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let runs = (0..4).map(|_| postgres::migrate(&db.pool));
    for result in futures::future::join_all(runs).await {
        result.unwrap();
    }
    postgres::migrate(&db.pool).await.unwrap();
    assert_eq!(
        db.table_names().await,
        [
            "wa_conversations",
            "wa_kv",
            "wa_messages",
            "wa_sqlx_migrations"
        ]
    );
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_custom_prefix_is_isolated() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let tenant = TablePrefix::new("tenant_x_").unwrap();
    postgres::migrate(&db.pool).await.unwrap();
    postgres::migrate_with_prefix(&db.pool, &tenant)
        .await
        .unwrap();
    assert_eq!(
        db.table_names().await,
        [
            "tenant_x_conversations",
            "tenant_x_kv",
            "tenant_x_messages",
            "tenant_x_sqlx_migrations",
            "wa_conversations",
            "wa_kv",
            "wa_messages",
            "wa_sqlx_migrations",
        ]
    );

    let default = PostgresKvStore::new(db.pool.clone());
    let custom = PostgresKvStore::with_prefix(db.pool.clone(), tenant.clone());
    let k = StoreKey::new("wa.prefix", "same-key");
    default
        .put(&k, b"default".to_vec(), Expiry::Never)
        .await
        .unwrap();
    custom
        .put(&k, b"custom".to_vec(), Expiry::Never)
        .await
        .unwrap();
    assert_eq!(default.get(&k).await.unwrap().unwrap().value, b"default");
    assert_eq!(custom.get(&k).await.unwrap().unwrap().value, b"custom");

    conformance::run_with_real_time(&custom, Duration::from_millis(500)).await;
    conversation_conformance::run(&PostgresConversationStore::with_prefix(
        db.pool.clone(),
        tenant,
    ))
    .await;
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_purge_expired_keeps_versions_unique() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let store = PostgresKvStore::new(db.pool.clone());
    let ns = format!("wa.purge.{}", common::unique());
    let k = |name: &str| StoreKey::new(ns.clone(), name.to_owned());
    let past = Expiry::At(datetime!(2000-01-01 0:00 UTC));

    store
        .put(&k("expired-old"), b"x".to_vec(), past)
        .await
        .unwrap();
    store
        .put(&k("live-old"), b"x".to_vec(), Expiry::Never)
        .await
        .unwrap();
    let deleted_version = store
        .put(&k("deleted-old"), b"x".to_vec(), Expiry::Never)
        .await
        .unwrap();
    assert!(store.delete(&k("deleted-old")).await.unwrap());
    store
        .put(&k("expired-new"), b"x".to_vec(), past)
        .await
        .unwrap();
    store
        .put(&k("deleted-new"), b"x".to_vec(), Expiry::Never)
        .await
        .unwrap();
    assert!(store.delete(&k("deleted-new")).await.unwrap());

    // Age the "-old" rows past the purge grace period.
    sqlx::query(
        "UPDATE wa_kv SET updated_at = now() - interval '1 hour' \
         WHERE namespace = $1 AND key LIKE '%-old'",
    )
    .bind(&ns)
    .execute(&db.pool)
    .await
    .unwrap();

    assert_eq!(
        store.purge_expired().await.unwrap(),
        2,
        "old dead rows only"
    );
    let remaining: Vec<String> =
        sqlx::query_scalar("SELECT key FROM wa_kv WHERE namespace = $1 ORDER BY key")
            .bind(&ns)
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        remaining,
        ["deleted-new", "expired-new", "live-old"],
        "live rows and recently dead rows survive the purge"
    );
    assert!(store.get(&k("live-old")).await.unwrap().is_some());

    let recreated = store
        .put_if_absent(&k("deleted-old"), b"y".to_vec(), Expiry::Never)
        .await
        .unwrap()
        .unwrap();
    assert!(
        recreated > deleted_version,
        "a purged key never gets back an old version ({deleted_version} then {recreated})"
    );
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_debug_is_redacted_and_missing_tables_are_errors() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let kv = format!("{:?}", PostgresKvStore::new(db.pool.clone()));
    let inbox = format!("{:?}", PostgresConversationStore::new(db.pool.clone()));
    for rendered in [&kv, &inbox] {
        assert!(
            !rendered.contains("55432") && !rendered.contains("wa:wa"),
            "{rendered}"
        );
        assert!(rendered.contains("wa_"), "{rendered}");
    }
    // Not connection details, but a sanity check the store is usable.
    assert!(
        PostgresConversationStore::new(db.pool.clone())
            .last_inbound_at(&wa_core::store::ConversationKey::new("x", "y"))
            .await
            .is_err(),
        "no tables yet: queries fail with a backend error, not a panic"
    );
    db.drop().await;
}
