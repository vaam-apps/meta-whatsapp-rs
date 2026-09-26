//! Postgres adapters against a real server. Skipped unless
//! `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set; `META_WHATSAPP_RS_REQUIRE_LIVE=1` (as in
//! `just test-live`) turns the skip into a failure.
//!
//! Every test runs in its own freshly created schema (the pool's
//! `search_path`), so runs in parallel — and repeated runs against the same
//! database — never see each other's tables. A `common::PgCleanup` guard drops
//! each schema and private database, when the test panics too.
#![cfg(feature = "postgres")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::fmt::Write as _;
use std::str::FromStr;
use std::time::Duration;

use meta_whatsapp_adapters::store::postgres::{self, TablePrefix};
use meta_whatsapp_adapters::store::{
    PostgresConversationStore, PostgresKvStore, conformance, conversation_conformance,
};
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::ids::{AppId, MessageId, UserId};
use meta_whatsapp_core::store::{
    ConversationKey, ConversationStore, DeliveryStatus, Direction, Erased, Expiry, KvStore, Purged,
    Retention, StoreKey, StoredContact, StoredMessage, ThreadOwner, ThreadOwnership, WindowEvent,
    WindowEventKind,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{AssertSqlSafe, PgPool};
use time::macros::datetime;

use common::PgCleanup;

/// A schema of our own on the test database, dropped with it.
struct TestDb {
    url: String,
    schema: String,
    admin: PgPool,
    pool: PgPool,
    /// Last: fields drop in order, so the pools go before the schema.
    _cleanup: PgCleanup,
}

impl TestDb {
    async fn new() -> Option<Self> {
        let url = common::service_url("META_WHATSAPP_RS_TEST_POSTGRES_URL")?;
        let schema = format!("wa_test_{}", common::unique());
        let cleanup = PgCleanup::schema(&url, &schema);
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect to META_WHATSAPP_RS_TEST_POSTGRES_URL");
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
            _cleanup: cleanup,
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

    /// The versions in the migration history, all applied successfully.
    async fn applied_migrations(&self) -> Vec<i64> {
        sqlx::query_scalar("SELECT version FROM wa_sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }

    /// `table.column: type` of every content column, old names or new.
    async fn content_columns(&self) -> Vec<String> {
        let columns: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT table_name::text, column_name::text, data_type::text \
             FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name IN ('wa_messages', 'wa_conversations') \
               AND column_name NOT IN ('id', 'phone_number_id', 'contact', 'direction', \
                 'status', 'ts', 'status_at', 'last_message_at', 'last_message_id', \
                 'last_inbound_at', 'unread') \
             ORDER BY 1, 2",
        )
        .bind(&self.schema)
        .fetch_all(&self.admin)
        .await
        .unwrap();
        columns
            .into_iter()
            .map(|(table, column, ty)| format!("{table}.{column}: {ty}"))
            .collect()
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_kv_conformance() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let store = PostgresKvStore::new(db.pool.clone());
    conformance::run_with_real_time(&store, Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_conversation_conformance() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let store = PostgresConversationStore::new(db.pool.clone());
    conversation_conformance::run(&store).await;
}

/// The search recipe of `store::postgres`' module docs finds a row holding a
/// NUL (the one `live_postgres_keeps_nul_in_content_and_refuses_it_in_ids`
/// stores), and the conversions they warn against fail on it.
async fn the_documented_sql_holds(pool: &PgPool) {
    let found: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM wa_messages WHERE position(convert_to($1, 'UTF8') IN text_utf8) > 0",
    )
    .bind("42")
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(found, ["wamid.nul"]);
    for warned in [
        "SELECT convert_from(text_utf8, 'UTF8') FROM wa_messages",
        "SELECT payload_json -> 'a\u{FFFD}' FROM wa_messages",
        "SELECT payload_json::jsonb FROM wa_messages",
    ] {
        assert!(
            sqlx::query(AssertSqlSafe(warned))
                .fetch_all(pool)
                .await
                .is_err(),
            "{warned}"
        );
    }
}

/// U+0000 is content like any other character (the owner's decision of
/// 2026-09-25: stored losslessly), and identifiers keep `TEXT`, which refuses
/// it. `meta_whatsapp_rs::inbox`'s unit tests use a store double with exactly these
/// rules: this pins that double to the real server. (The conformance suite
/// checks the round trips in detail; this is the refusing half, and the
/// SQL the module docs recommend for search.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_keeps_nul_in_content_and_refuses_it_in_ids() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let store = PostgresConversationStore::new(db.pool.clone());
    let message = |id: &str, pn: &str, contact: &str| StoredMessage {
        id: MessageId::new(id),
        conversation: ConversationKey::new(pn, contact),
        direction: Direction::Inbound,
        kind: "te\0xt".to_owned(),
        text: Some("order\u{0}42".to_owned()),
        payload: serde_json::json!({"a\0": ["b\0"], "a\u{FFFD}": ["b\u{FFFD}"]}),
        status: DeliveryStatus::Received,
        timestamp: datetime!(2026-09-24 12:00 UTC),
        status_at: None,
        error: None,
    };
    let clean = message("wamid.nul", "pn-nul", "US.1");
    assert!(store.append(clean.clone()).await.unwrap());
    assert_eq!(
        store.messages(&clean.conversation, None, 10).await.unwrap(),
        std::slice::from_ref(&clean)
    );

    for (case, bad) in [
        ("message id", message("wamid.\0", "pn-nul", "US.2")),
        ("contact", message("wamid.c", "pn-nul", "US.\0")),
        ("phone number id", message("wamid.p", "pn-\0", "US.3")),
    ] {
        assert!(store.append(bad.clone()).await.is_err(), "append: {case}");
        assert!(
            store.append_synced(vec![bad.clone()]).await.is_err(),
            "append_synced: {case}"
        );
        assert!(
            store
                .revoke(
                    &bad.conversation,
                    &bad.id,
                    Direction::Inbound,
                    bad.timestamp
                )
                .await
                .is_err(),
            "revoke: {case}"
        );
    }
    let nul_id = MessageId::new("wamid.\0");
    assert!(
        store
            .update_status(
                &"pn-nul".into(),
                &nul_id,
                DeliveryStatus::Read,
                datetime!(2026-09-24 12:01 UTC),
                None
            )
            .await
            .is_err()
    );
    assert!(
        store
            .fill_media_placeholder(
                &"pn-nul".into(),
                &nul_id,
                "image".to_owned(),
                None,
                serde_json::json!({})
            )
            .await
            .is_err()
    );
    assert_eq!(
        store.messages(&clean.conversation, None, 10).await.unwrap(),
        [clean],
        "nothing else was stored"
    );

    the_documented_sql_holds(&db.pool).await;

    // Key/value: values are bytes, keys are text and refuse U+0000.
    let kv = PostgresKvStore::new(db.pool.clone());
    let key = StoreKey::new("wa.nul", "k\0");
    assert!(kv.put(&key, b"v".to_vec(), Expiry::Never).await.is_err());
    let key = StoreKey::new("wa.nul", "k");
    kv.put(&key, b"\0v\0".to_vec(), Expiry::Never)
        .await
        .unwrap();
    assert_eq!(kv.get(&key).await.unwrap().unwrap().value, b"\0v\0");
}

/// The history and inbox order must be byte order whatever the server's
/// default collation. Alpine (musl) Postgres collates `en_US.utf8` byte-wise
/// anyway, so the test above cannot tell; a database created with an ICU
/// locale orders `a` before `B` and can.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_conversation_order_ignores_the_database_collation() {
    let Some(url) = common::service_url("META_WHATSAPP_RS_TEST_POSTGRES_URL") else {
        return;
    };
    let name = format!("wa_test_icu_{}", common::unique());
    let _cleanup = PgCleanup::database(&url, &name);
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_migrate_is_idempotent_under_concurrency() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    // Four independent "processes", each with its connection already open,
    // so their migrations start together rather than one after the other.
    let mut instances = Vec::new();
    for _ in 0..4 {
        instances.push(TestDb::pool_on(&db.url, &db.schema, 1).await);
    }
    let runs = instances.iter().map(postgres::migrate);
    for result in futures::future::join_all(runs).await {
        result.unwrap();
    }
    for pool in instances {
        pool.close().await;
    }
    postgres::migrate(&db.pool).await.unwrap();
    assert_eq!(db.table_names().await, DEFAULT_TABLES);
    assert_eq!(db.applied_migrations().await, [1, 2, 3, 4]);
    assert_eq!(db.content_columns().await, LOSSLESS_CONTENT_COLUMNS);
}

/// Every table of the default prefix, by name.
const DEFAULT_TABLES: [&str; 7] = [
    "wa_conversations",
    "wa_kv",
    "wa_messages",
    "wa_sqlx_migrations",
    "wa_synced_contacts",
    "wa_thread_owners",
    "wa_window_events",
];

/// The content columns after migration 3: bytes and `json`, no `text` or
/// `jsonb` left.
const LOSSLESS_CONTENT_COLUMNS: [&str; 5] = [
    "wa_conversations.last_text_utf8: bytea",
    "wa_messages.error_json: json",
    "wa_messages.kind_utf8: bytea",
    "wa_messages.payload_json: json",
    "wa_messages.text_utf8: bytea",
];

/// SQLSTATE `undefined_column`.
const UNDEFINED_COLUMN: Option<&str> = Some("42703");

/// The two migrations of the revision before lossless content (09db4aa),
/// as its `migrate` ran them: same SQL, so the same checksums, under the
/// same history table.
async fn migrate_like_09db4aa(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    use sqlx::SqlSafeStr;
    use sqlx::migrate::{Migration, MigrationType, Migrator};
    let old = [
        (1, "kv", include_str!("../migrations/0001_kv.sql")),
        (
            2,
            "conversations",
            include_str!("../migrations/0002_conversations.sql"),
        ),
    ]
    .into_iter()
    .map(|(version, description, sql)| {
        Migration::new(
            version,
            description.into(),
            MigrationType::Simple,
            AssertSqlSafe(sql.replace("{prefix}", "wa_")).into_sql_str(),
            false,
        )
    })
    .collect();
    let mut migrator = Migrator::with_migrations(old);
    migrator.dangerous_set_table_name("wa_sqlx_migrations");
    migrator.run(pool).await
}

/// The three migrations of the revision before migration 4 (b66972c, PR
/// #21), as its `migrate` ran them.
async fn migrate_like_b66972c(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    use sqlx::SqlSafeStr;
    use sqlx::migrate::{Migration, MigrationType, Migrator};
    let old = [
        (1, "kv", include_str!("../migrations/0001_kv.sql")),
        (
            2,
            "conversations",
            include_str!("../migrations/0002_conversations.sql"),
        ),
        (
            3,
            "lossless content",
            include_str!("../migrations/0003_lossless_content.sql"),
        ),
    ]
    .into_iter()
    .map(|(version, description, sql)| {
        Migration::new(
            version,
            description.into(),
            MigrationType::Simple,
            AssertSqlSafe(sql.replace("{prefix}", "wa_")).into_sql_str(),
            false,
        )
    })
    .collect();
    let mut migrator = Migrator::with_migrations(old);
    migrator.dangerous_set_table_name("wa_sqlx_migrations");
    migrator.run(pool).await
}

/// Migration 4 adds tables and indexes and changes no column: a database
/// the previous revision (b66972c) wrote keeps its rows, that revision's
/// `append` (which takes no lock) keeps working beside the new one, and
/// only its `migrate` refuses the database.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_migration_4_keeps_the_previous_revision_working() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    migrate_like_b66972c(&db.pool).await.unwrap();
    assert_eq!(db.applied_migrations().await, [1, 2, 3]);
    let store = PostgresConversationStore::new(db.pool.clone());
    let [inbound, outbound, other] = written_by_09db4aa();
    for m in [&inbound, &outbound, &other] {
        append_like_b66972c(&db.pool, m).await.unwrap();
    }
    let key = inbound.conversation.clone();
    let before = store.messages(&key, None, 10).await.unwrap();
    let summaries = store
        .conversations(&key.phone_number_id, None, 10)
        .await
        .unwrap();

    let runs = (0..3).map(|_| postgres::migrate(&db.pool));
    for result in futures::future::join_all(runs).await {
        result.unwrap();
    }
    assert_eq!(db.applied_migrations().await, [1, 2, 3, 4]);
    assert_eq!(db.table_names().await, DEFAULT_TABLES);
    assert_eq!(store.messages(&key, None, 10).await.unwrap(), before);
    assert_eq!(
        store
            .conversations(&key.phone_number_id, None, 10)
            .await
            .unwrap(),
        summaries,
        "rows written before keep their content, window and count"
    );
    let late = StoredMessage {
        id: MessageId::new("wamid.after-4"),
        timestamp: datetime!(2026-09-24 16:00 UTC),
        ..inbound.clone()
    };
    append_like_b66972c(&db.pool, &late)
        .await
        .expect("the previous revision's append still works");
    let later = StoredMessage {
        id: MessageId::new("wamid.after-4-new"),
        timestamp: datetime!(2026-09-24 16:01 UTC),
        ..inbound.clone()
    };
    assert!(
        store.append(later.clone()).await.unwrap(),
        "and the new one"
    );
    assert!(
        matches!(
            migrate_like_b66972c(&db.pool).await,
            Err(sqlx::migrate::MigrateError::VersionMissing(4))
        ),
        "the previous revision's migrate refuses the upgraded database"
    );
    assert_eq!(
        store.message(&key.phone_number_id, &late.id).await.unwrap(),
        Some(late)
    );
    assert_eq!(
        store
            .message(&key.phone_number_id, &later.id)
            .await
            .unwrap(),
        Some(later.clone())
    );
    let summary = store
        .conversations(&key.phone_number_id, None, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.key == key)
        .unwrap();
    assert_eq!(
        (summary.last_message_at, summary.unread),
        (later.timestamp, 3),
        "both revisions' appends kept the summary"
    );
}

/// b66972c's `append` statement, verbatim (default prefix): lossless
/// content, and no lock (the number lock came with the review of L5).
const APPEND_B66972C: &str = "WITH inserted AS ( \
       INSERT INTO wa_messages (id, phone_number_id, contact, direction, kind_utf8, text_utf8, \
         payload_json, status, ts, status_at, error_json) \
       VALUES ($1, $2, $3, $4, $5, $6, $7::json, $8, $9, $10, $11::json) \
       ON CONFLICT (id) DO NOTHING \
       RETURNING id, phone_number_id, contact, direction, text_utf8, ts \
     ) \
     INSERT INTO wa_conversations AS c \
       (phone_number_id, contact, last_message_at, last_message_id, last_text_utf8, \
        last_inbound_at, unread) \
     SELECT phone_number_id, contact, ts, id, text_utf8, \
       CASE WHEN direction = 'inbound' THEN ts END, \
       CASE WHEN direction = 'inbound' THEN 1 ELSE 0 END \
     FROM inserted \
     ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
       last_message_at = CASE WHEN (EXCLUDED.last_message_at, EXCLUDED.last_message_id) \
         > (c.last_message_at, c.last_message_id) THEN EXCLUDED.last_message_at ELSE c.last_message_at END, \
       last_message_id = CASE WHEN (EXCLUDED.last_message_at, EXCLUDED.last_message_id) \
         > (c.last_message_at, c.last_message_id) THEN EXCLUDED.last_message_id ELSE c.last_message_id END, \
       last_text_utf8 = CASE WHEN (EXCLUDED.last_message_at, EXCLUDED.last_message_id) \
         > (c.last_message_at, c.last_message_id) THEN EXCLUDED.last_text_utf8 ELSE c.last_text_utf8 END, \
       last_inbound_at = GREATEST(c.last_inbound_at, EXCLUDED.last_inbound_at), \
       unread = c.unread + EXCLUDED.unread \
     RETURNING 1 AS appended";

/// Append `m` the way b66972c did.
async fn append_like_b66972c(pool: &PgPool, m: &StoredMessage) -> Result<(), sqlx::Error> {
    let status = serde_json::to_value(m.status).unwrap();
    let appended = sqlx::query(APPEND_B66972C)
        .bind(m.id.as_str())
        .bind(m.conversation.phone_number_id.as_str())
        .bind(m.conversation.contact.as_str())
        .bind(match m.direction {
            Direction::Inbound => "inbound",
            Direction::Outbound => "outbound",
        })
        .bind(m.kind.as_bytes())
        .bind(m.text.as_deref().map(str::as_bytes))
        .bind(serde_json::to_string(&m.payload).unwrap())
        .bind(status.as_str().unwrap())
        .bind(m.timestamp)
        .bind(m.status_at)
        .bind(m.error.as_ref().map(|e| serde_json::to_string(e).unwrap()))
        .execute(pool)
        .await?;
    assert_eq!(appended.rows_affected(), 1, "{} appended", m.id);
    Ok(())
}

/// 09db4aa's `append` statement, verbatim (default prefix): what an
/// instance of the older revision still running during an upgrade sends.
const APPEND_09DB4AA: &str = "WITH inserted AS ( \
       INSERT INTO wa_messages (id, phone_number_id, contact, direction, kind, text, payload, \
         status, ts, status_at, error) \
       VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
       ON CONFLICT (id) DO NOTHING \
       RETURNING id, phone_number_id, contact, direction, text, ts \
     ) \
     INSERT INTO wa_conversations AS c \
       (phone_number_id, contact, last_message_at, last_message_id, last_text, \
        last_inbound_at, unread) \
     SELECT phone_number_id, contact, ts, id, text, \
       CASE WHEN direction = 'inbound' THEN ts END, \
       CASE WHEN direction = 'inbound' THEN 1 ELSE 0 END \
     FROM inserted \
     ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
       last_message_at = CASE WHEN (EXCLUDED.last_message_at, EXCLUDED.last_message_id) \
         > (c.last_message_at, c.last_message_id) THEN EXCLUDED.last_message_at ELSE c.last_message_at END, \
       last_message_id = CASE WHEN (EXCLUDED.last_message_at, EXCLUDED.last_message_id) \
         > (c.last_message_at, c.last_message_id) THEN EXCLUDED.last_message_id ELSE c.last_message_id END, \
       last_text = CASE WHEN (EXCLUDED.last_message_at, EXCLUDED.last_message_id) \
         > (c.last_message_at, c.last_message_id) THEN EXCLUDED.last_text ELSE c.last_text END, \
       last_inbound_at = GREATEST(c.last_inbound_at, EXCLUDED.last_inbound_at), \
       unread = c.unread + EXCLUDED.unread \
     RETURNING 1 AS appended";

/// Append `m` the way 09db4aa did: `text` and `jsonb` parameters.
async fn append_like_09db4aa(pool: &PgPool, m: &StoredMessage) -> Result<(), sqlx::Error> {
    let status = serde_json::to_value(m.status).unwrap();
    sqlx::query(APPEND_09DB4AA)
        .bind(m.id.as_str())
        .bind(m.conversation.phone_number_id.as_str())
        .bind(m.conversation.contact.as_str())
        .bind(match m.direction {
            Direction::Inbound => "inbound",
            Direction::Outbound => "outbound",
        })
        .bind(m.kind.as_str())
        .bind(m.text.as_deref())
        .bind(&m.payload)
        .bind(status.as_str().unwrap())
        .bind(m.timestamp)
        .bind(m.status_at)
        .bind(m.error.as_ref())
        .execute(pool)
        .await
        .map(drop)
}

/// The SQLSTATE of a database error: `42703` is `undefined_column`.
fn sqlstate(result: Result<impl Sized, sqlx::Error>) -> Option<String> {
    match result {
        Err(sqlx::Error::Database(e)) => e.code().map(std::borrow::Cow::into_owned),
        _ => None,
    }
}

/// What an instance of 09db4aa still running after the upgrade gets: an
/// error on every statement that touches content (`inbound` is one of its
/// messages), and a `migrate` that refuses to run.
async fn old_revision_is_locked_out(pool: &PgPool, inbound: &StoredMessage) {
    let late = StoredMessage {
        id: MessageId::new("wamid.late"),
        timestamp: datetime!(2026-09-24 13:00 UTC),
        ..inbound.clone()
    };
    assert_eq!(
        sqlstate(append_like_09db4aa(pool, &late).await).as_deref(),
        UNDEFINED_COLUMN,
        "its append"
    );
    for old in [
        "SELECT kind, text, payload, error FROM wa_messages",
        "SELECT last_text FROM wa_conversations",
    ] {
        assert_eq!(
            sqlstate(sqlx::query(AssertSqlSafe(old)).fetch_all(pool).await).as_deref(),
            UNDEFINED_COLUMN,
            "{old}"
        );
    }
    assert_eq!(
        sqlstate(
            sqlx::query(
                "UPDATE wa_messages SET status = $4, status_at = $5, error = COALESCE($6, error) \
                 WHERE id = $1 AND phone_number_id = $2 AND status = $3"
            )
            .bind("wamid.before-2")
            .bind(inbound.conversation.phone_number_id.as_str())
            .bind("failed")
            .bind("read")
            .bind(inbound.timestamp)
            .bind(Some(serde_json::json!({"x": 1})))
            .execute(pool)
            .await
        )
        .as_deref(),
        UNDEFINED_COLUMN,
        "its status update"
    );
    assert!(
        matches!(
            migrate_like_09db4aa(pool).await,
            Err(sqlx::migrate::MigrateError::VersionMissing(3))
        ),
        "its migrate refuses the upgraded database"
    );
}

/// Messages as 09db4aa's inbox recorded them: a customer's NUL already
/// replaced by U+FFFD, a failed template without text, an empty text.
fn written_by_09db4aa() -> [StoredMessage; 3] {
    let inbound = StoredMessage {
        id: MessageId::new("wamid.before-1"),
        conversation: ConversationKey::new("pn-upgrade", "US.1"),
        direction: Direction::Inbound,
        kind: "text".to_owned(),
        // What 09db4aa's inbox stored for a customer's "order\042".
        text: Some("order\u{FFFD}42".to_owned()),
        payload: serde_json::json!({
            "type": "text",
            "text": {"body": "order\u{FFFD}42"},
            "k\u{FFFD}": [1, 2.5, true, null, "é"]
        }),
        status: DeliveryStatus::Received,
        timestamp: datetime!(2026-09-24 12:00 UTC),
        status_at: None,
        error: None,
    };
    let outbound = StoredMessage {
        id: MessageId::new("wamid.before-2"),
        direction: Direction::Outbound,
        kind: "template".to_owned(),
        text: None,
        payload: serde_json::json!({"type": "template", "template": {"name": "order_update"}}),
        status: DeliveryStatus::Failed,
        timestamp: datetime!(2026-09-24 12:05 UTC),
        status_at: Some(datetime!(2026-09-24 12:06 UTC)),
        error: Some(serde_json::json!([{"code": 131_026, "title": "bad\u{FFFD}"}])),
        ..inbound.clone()
    };
    let other = StoredMessage {
        id: MessageId::new("wamid.before-3"),
        conversation: ConversationKey::new("pn-upgrade", "US.2"),
        text: Some(String::new()),
        ..inbound.clone()
    };
    [inbound, outbound, other]
}

/// Upgrading a database written by 09db4aa: existing rows keep their
/// content byte for byte (a U+FFFD that replaced a NUL stays U+FFFD), the
/// conversion survives concurrent `migrate` calls, and an instance of the
/// older revision left running can no longer read or write content: each
/// of its statements that touches content fails on a column that no longer
/// exists, and its `migrate` refuses to run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_upgrade_keeps_existing_content_and_stops_old_writers() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    migrate_like_09db4aa(&db.pool).await.unwrap();
    assert_eq!(db.applied_migrations().await, [1, 2]);
    let [inbound, outbound, other] = written_by_09db4aa();
    let key = inbound.conversation.clone();
    for m in [&inbound, &outbound, &other] {
        append_like_09db4aa(&db.pool, m).await.unwrap();
    }

    let runs = (0..4).map(|_| postgres::migrate(&db.pool));
    for result in futures::future::join_all(runs).await {
        result.unwrap();
    }
    assert_eq!(db.applied_migrations().await, [1, 2, 3, 4]);
    assert_eq!(db.content_columns().await, LOSSLESS_CONTENT_COLUMNS);

    let store = PostgresConversationStore::new(db.pool.clone());
    let pn = key.phone_number_id.clone();
    let check = async || {
        assert_eq!(
            store.messages(&key, None, 10).await.unwrap(),
            [outbound.clone(), inbound.clone()],
            "rows written before the upgrade read back exactly"
        );
        assert_eq!(
            store.messages(&other.conversation, None, 10).await.unwrap(),
            std::slice::from_ref(&other),
            "an empty text stays empty, not absent"
        );
        let summaries = store.conversations(&pn, None, 10).await.unwrap();
        let got: Vec<_> = summaries
            .iter()
            .map(|s| {
                (
                    s.key.contact.as_str(),
                    s.last_message_at,
                    s.last_text.as_deref(),
                    s.last_inbound_at,
                    s.unread,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("US.1", outbound.timestamp, None, Some(inbound.timestamp), 1),
                ("US.2", other.timestamp, Some(""), Some(other.timestamp), 1),
            ],
            "summaries keep their preview, window and count"
        );
    };
    check().await;

    // An instance of the older revision, still running.
    old_revision_is_locked_out(&db.pool, &inbound).await;
    check().await;

    // The upgraded store writes U+0000 into the same tables.
    let nul = StoredMessage {
        id: MessageId::new("wamid.after"),
        text: Some("order\u{0}42".to_owned()),
        payload: serde_json::json!({"text": {"body": "order\u{0}42"}, "k\0": 1, "k\u{FFFD}": 2}),
        timestamp: datetime!(2026-09-24 14:00 UTC),
        ..inbound.clone()
    };
    assert!(store.append(nul.clone()).await.unwrap());
    assert_eq!(
        store.messages(&key, None, 1).await.unwrap(),
        std::slice::from_ref(&nul)
    );
    assert_eq!(
        store.conversations(&pn, None, 1).await.unwrap()[0].last_text,
        nul.text
    );
}

/// The pre-flight query of the `store::postgres` module docs ("Upgrading to
/// lossless content"), read from them: the ```` ```sql ```` block. Tests
/// run the query the operators copy.
fn preflight() -> String {
    let docs = include_str!("../src/store/postgres/mod.rs");
    let lines: Vec<&str> = docs
        .lines()
        .map(|l| l.trim_start_matches("//!").trim_start())
        .skip_while(|l| *l != "```sql")
        .skip(1)
        .take_while(|l| *l != "```")
        .collect();
    assert!(
        lines.len() > 10,
        "no pre-flight query in the module docs: {lines:?}"
    );
    lines.join("\n")
}

/// The objects the pre-flight query lists in the pool's schema.
async fn preflight_objects(pool: &PgPool) -> Vec<String> {
    sqlx::query_as::<_, (String, Option<String>, Option<String>)>(AssertSqlSafe(preflight()))
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|(object, _, _)| object)
        .collect()
}

/// A database of its own (the trigram case installs an extension, which is
/// per database), one schema per case in it, dropped with it.
struct PrivateDb {
    url: String,
    name: String,
    _cleanup: PgCleanup,
}

impl PrivateDb {
    async fn new(label: &str) -> Option<Self> {
        let url = common::service_url("META_WHATSAPP_RS_TEST_POSTGRES_URL")?;
        let name = format!("wa_test_{label}_{}", common::unique());
        let cleanup = PgCleanup::database(&url, &name);
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        sqlx::query(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
        Some(Self {
            url,
            name,
            _cleanup: cleanup,
        })
    }

    /// A pool on a new schema of this database.
    async fn schema(&self, schema: &str) -> PgPool {
        let options = PgConnectOptions::from_str(&self.url)
            .unwrap()
            .database(&self.name);
        let setup = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .unwrap();
        sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&setup)
            .await
            .unwrap();
        setup.close().await;
        PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options.options([("search_path", schema)]))
            .await
            .unwrap()
    }
}

/// `table.column: type` of the content columns in the pool's schema.
async fn content_columns_of(pool: &PgPool) -> Vec<String> {
    let columns: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text, data_type::text \
         FROM information_schema.columns \
         WHERE table_schema = current_schema() \
           AND table_name IN ('wa_messages', 'wa_conversations') \
           AND column_name NOT IN ('id', 'phone_number_id', 'contact', 'direction', \
             'status', 'ts', 'status_at', 'last_message_at', 'last_message_id', \
             'last_inbound_at', 'unread') \
         ORDER BY 1, 2",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    columns
        .into_iter()
        .map(|(table, column, ty)| format!("{table}.{column}: {ty}"))
        .collect()
}

/// The content columns before migration 3.
const PRE_LOSSLESS_CONTENT_COLUMNS: [&str; 5] = [
    "wa_conversations.last_text: text",
    "wa_messages.error: jsonb",
    "wa_messages.kind: text",
    "wa_messages.payload: jsonb",
    "wa_messages.text: text",
];

/// A message holding U+0000 in its text and, under a key of its own, in its
/// payload.
fn with_nul(id: &str) -> StoredMessage {
    StoredMessage {
        id: MessageId::new(id),
        text: Some("order\u{0}42".to_owned()),
        payload: serde_json::json!({"type": "text", "text": {"body": "order"}, "note": "a\0b"}),
        timestamp: datetime!(2026-09-24 15:00 UTC),
        ..written_by_09db4aa()[0].clone()
    }
}

/// `object`, created in a fresh schema written by 09db4aa, makes `migrate`
/// fail and change nothing; the pre-flight query lists it as `listed`.
async fn assert_refused(db: &PrivateDb, schema: &str, case: &str, object: &str, listed: &str) {
    let pool = db.schema(schema).await;
    migrate_like_09db4aa(&pool).await.unwrap();
    let rows = written_by_09db4aa();
    for m in &rows {
        append_like_09db4aa(&pool, m).await.unwrap();
    }
    let before = preflight_objects(&pool).await;
    assert!(before.is_empty(), "{case}: none of ours: {before:?}");
    sqlx::query(AssertSqlSafe(object.to_owned()))
        .execute(&pool)
        .await
        .unwrap();
    let found = preflight_objects(&pool).await;
    assert!(
        found.iter().any(|o| o.contains(listed)),
        "{case}: the pre-flight lists it: {found:?}"
    );

    let error = postgres::migrate(&pool).await.expect_err(case).to_string();
    assert!(!error.contains("order"), "{case}: no content: {error}");
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM wa_sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(versions, [1, 2], "{case}: {error}");
    assert_eq!(
        content_columns_of(&pool).await,
        PRE_LOSSLESS_CONTENT_COLUMNS,
        "{case}: nothing changed"
    );
    append_like_09db4aa(
        &pool,
        &StoredMessage {
            id: MessageId::new("wamid.still-old"),
            ..rows[0].clone()
        },
    )
    .await
    .unwrap_or_else(|e| panic!("{case}: the old schema still works: {e}"));
    pool.close().await;
}

/// Objects of an operator's own on the content columns, against the
/// upgrade. Those that would keep failing inserts of a payload holding
/// U+0000 after the conversion (an expression or partial index, or a check
/// constraint, on `payload` or `error`), and those Postgres cannot convert
/// (a view, a trigram index), make `migrate` fail and change nothing. The
/// documented pre-flight query lists each, and none of the adapter's own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_upgrade_refuses_objects_that_would_fail_content() {
    let Some(db) = PrivateDb::new("objects").await else {
        return;
    };
    let owner = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(
            PgConnectOptions::from_str(&db.url)
                .unwrap()
                .database(&db.name),
        )
        .await
        .unwrap();
    sqlx::query("CREATE EXTENSION pg_trgm SCHEMA public")
        .execute(&owner)
        .await
        .unwrap();
    owner.close().await;

    for (i, (case, object, listed)) in [
        (
            "an expression index on payload",
            "CREATE INDEX my_type ON wa_messages ((payload->>'type'))",
            "index my_type",
        ),
        (
            "a partial index on error",
            "CREATE INDEX my_failed ON wa_messages (id) WHERE error->>'code' IS NOT NULL",
            "index my_failed",
        ),
        (
            "a check constraint on payload",
            "ALTER TABLE wa_messages ADD CONSTRAINT my_typed \
             CHECK (payload->>'type' IS NOT NULL)",
            "constraint my_typed",
        ),
        (
            "a view on text",
            "CREATE VIEW my_texts AS SELECT id, text FROM wa_messages",
            "rule _RETURN on view my_texts",
        ),
        (
            "a trigram index on the preview",
            "CREATE INDEX my_preview ON wa_conversations \
             USING gin (last_text public.gin_trgm_ops)",
            "index my_preview",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        assert_refused(&db, &format!("refused_{i}"), case, object, listed).await;
    }
}

/// What the upgrade keeps: a b-tree index on text, rebuilt on the bytes,
/// and a trigger that names no content column (the pre-flight lists both
/// for review). And why an expression on the payload is refused: after the
/// upgrade, one fails the insert of any payload holding a NUL, under any
/// key, and cannot be created once such a row exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_upgrade_keeps_plain_indexes_and_json_expressions_fail_nul() {
    let Some(db) = PrivateDb::new("kept").await else {
        return;
    };
    let pool = db.schema("kept").await;
    migrate_like_09db4aa(&pool).await.unwrap();
    for object in [
        "CREATE INDEX my_text ON wa_messages (text)",
        "CREATE FUNCTION my_notify() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN PERFORM pg_notify('inbox', NEW.id); RETURN NEW; END $$",
        "CREATE TRIGGER my_notify AFTER INSERT ON wa_messages \
         FOR EACH ROW EXECUTE FUNCTION my_notify()",
    ] {
        sqlx::query(AssertSqlSafe(object))
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(
        preflight_objects(&pool).await,
        ["index my_text", "trigger my_notify on table wa_messages"]
    );
    postgres::migrate(&pool).await.unwrap();
    assert_eq!(content_columns_of(&pool).await, LOSSLESS_CONTENT_COLUMNS);
    let index: String = sqlx::query_scalar(
        "SELECT indexdef FROM pg_indexes \
         WHERE schemaname = current_schema() AND indexname = 'my_text'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(index.ends_with("(text_utf8)"), "{index}");

    let type_index = "CREATE INDEX my_type ON wa_messages ((payload_json->>'type'))";
    sqlx::query(type_index).execute(&pool).await.unwrap();
    let store = PostgresConversationStore::new(pool.clone());
    assert!(
        store.append(with_nul("wamid.kept")).await.is_err(),
        "an index on payload_json->>'type' refuses a NUL under another key"
    );
    sqlx::query("DROP INDEX my_type")
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.append(with_nul("wamid.kept")).await.unwrap());
    assert!(
        sqlx::query(type_index).execute(&pool).await.is_err(),
        "nor can it be created over a row holding one"
    );
    pool.close().await;
}

/// A content column that is not UTF-8 (only a hand edit can make one:
/// every write stores a `str`'s bytes) reads as `StorageError::Corrupt`
/// naming the column, never as a panic or a replacement character, and
/// never with the content in the error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_reads_content_that_is_not_utf8_as_corrupt() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let store = PostgresConversationStore::new(db.pool.clone());
    let [message, _, other] = written_by_09db4aa();
    let secret = StoredMessage {
        text: Some("secret".to_owned()),
        ..message
    };
    assert!(store.append(secret.clone()).await.unwrap());
    assert!(store.append(other.clone()).await.unwrap());
    let pn = secret.conversation.phone_number_id.clone();

    for (column, update) in [
        (
            "kind_utf8",
            "UPDATE wa_messages SET kind_utf8 = '\\x736563ff'::bytea WHERE id = $1",
        ),
        (
            "text_utf8",
            "UPDATE wa_messages SET text_utf8 = '\\x736563726574c328'::bytea WHERE id = $1",
        ),
    ] {
        sqlx::query(AssertSqlSafe(update))
            .bind(secret.id.as_str())
            .execute(&db.pool)
            .await
            .unwrap();
        match store.messages(&secret.conversation, None, 10).await {
            Err(e @ StorageError::Corrupt { .. }) => {
                let StorageError::Corrupt { key, .. } = &e else {
                    unreachable!()
                };
                assert_eq!(key, column);
                assert!(!e.to_string().contains("sec"), "{e}");
            }
            other => panic!("{column}: {other:?}"),
        }
        assert_eq!(
            store.messages(&other.conversation, None, 10).await.unwrap(),
            std::slice::from_ref(&other),
            "{column}: other conversations still read"
        );
        // Put the row back.
        sqlx::query(
            "UPDATE wa_messages SET kind_utf8 = convert_to('text', 'UTF8'), \
             text_utf8 = convert_to('secret', 'UTF8') WHERE id = $1",
        )
        .bind(secret.id.as_str())
        .execute(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            store
                .messages(&secret.conversation, None, 10)
                .await
                .unwrap(),
            std::slice::from_ref(&secret)
        );
    }

    sqlx::query(
        "UPDATE wa_conversations SET last_text_utf8 = '\\x736563ff'::bytea WHERE contact = $1",
    )
    .bind(secret.conversation.contact.as_str())
    .execute(&db.pool)
    .await
    .unwrap();
    match store.conversations(&pn, None, 10).await {
        Err(e @ StorageError::Corrupt { .. }) => {
            let StorageError::Corrupt { key, .. } = &e else {
                unreachable!()
            };
            assert_eq!(key, "last_text_utf8");
            assert!(!e.to_string().contains("sec"), "{e}");
        }
        other => panic!("last_text_utf8: {other:?}"),
    }
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
    let mut tables: Vec<String> = DEFAULT_TABLES
        .iter()
        .map(|t| t.replacen("wa_", "tenant_x_", 1))
        .chain(DEFAULT_TABLES.iter().map(|t| (*t).to_owned()))
        .collect();
    tables.sort_unstable();
    assert_eq!(db.table_names().await, tables);

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
    // With a retention, the suite's retention case takes its other branch.
    conversation_conformance::run(
        &PostgresConversationStore::with_prefix(db.pool.clone(), tenant)
            .with_retention(Retention::days(30)),
    )
    .await;
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
            .last_inbound_at(&meta_whatsapp_core::store::ConversationKey::new("x", "y"))
            .await
            .is_err(),
        "no tables yet: queries fail with a backend error, not a panic"
    );
}

/// Rows of every table of the schema whose JSON rendering holds one of
/// `needles`, as text or (for a `BYTEA` column, which renders as hex) as
/// the hex of its UTF-8 bytes, by table. Every table: one added later is
/// swept too.
async fn rows_mentioning(db: &TestDb, needles: &[&str]) -> Vec<(String, i64)> {
    let mut found = Vec::new();
    for table in db.table_names().await {
        let mut total = 0;
        for needle in needles {
            let hex = needle.bytes().fold(String::new(), |mut hex, b| {
                write!(hex, "{b:02x}").unwrap();
                hex
            });
            let n: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
                "SELECT count(*) FROM {schema}.{table} t \
                 WHERE position($1 IN row_to_json(t)::text) > 0 \
                    OR position($2 IN row_to_json(t)::text) > 0",
                schema = db.schema
            )))
            .bind(*needle)
            .bind(hex)
            .fetch_one(&db.admin)
            .await
            .unwrap();
            total += n;
        }
        found.push((table, total));
    }
    found
}

/// Everything of one contact: a message of every origin and a tombstone,
/// the summary, window events, the ownership record and synced contacts
/// naming them, content marked with `marker`.
#[allow(clippy::too_many_lines)] // one record of every kind
async fn record_a_contact(store: &PostgresConversationStore, key: &ConversationKey, marker: &str) {
    let id = |local: &str| MessageId::new(format!("wamid.{}.{local}", key.contact));
    let message = |local: &str, direction, minute: i64| StoredMessage {
        id: id(local),
        conversation: key.clone(),
        direction,
        kind: "text".to_owned(),
        text: Some(format!("{marker} said {local}")),
        payload: serde_json::json!({"text": {"body": format!("{marker} \0 {local}")}}),
        status: match direction {
            Direction::Inbound => DeliveryStatus::Received,
            Direction::Outbound => DeliveryStatus::Accepted,
        },
        timestamp: datetime!(2026-09-24 12:00 UTC) + time::Duration::minutes(minute),
        status_at: None,
        error: None,
    };
    assert!(
        store
            .append(message("in", Direction::Inbound, 1))
            .await
            .unwrap()
    );
    assert!(
        store
            .append(message("out", Direction::Outbound, 2))
            .await
            .unwrap()
    );
    assert!(
        store
            .update_status(
                &key.phone_number_id,
                &id("out"),
                DeliveryStatus::Failed,
                datetime!(2026-09-24 12:03 UTC),
                Some(serde_json::json!({"title": format!("{marker} failed")})),
            )
            .await
            .unwrap()
    );
    let placeholder = StoredMessage {
        kind: StoredMessage::MEDIA_PLACEHOLDER.to_owned(),
        text: None,
        ..message("media", Direction::Inbound, -2)
    };
    assert_eq!(
        store
            .append_synced(vec![message("synced", Direction::Inbound, -1), placeholder])
            .await
            .unwrap(),
        [true, true]
    );
    assert!(
        store
            .fill_media_placeholder(
                &key.phone_number_id,
                &id("media"),
                "image".to_owned(),
                Some(format!("{marker} caption")),
                serde_json::json!({"image": {"caption": format!("{marker} caption")}}),
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .revoke(
                key,
                &id("gone"),
                Direction::Inbound,
                datetime!(2026-09-24 12:04 UTC)
            )
            .await
            .unwrap()
    );
    for (local, kind) in [
        ("call", WindowEventKind::CustomerCall),
        ("standby", WindowEventKind::StandbyMessage),
    ] {
        assert!(
            store
                .record_window_event(WindowEvent {
                    conversation: key.clone(),
                    kind,
                    id: format!("wacid.{}.{local}", key.contact),
                    at: datetime!(2026-09-24 12:05 UTC),
                })
                .await
                .unwrap()
        );
    }
    assert!(
        store
            .set_thread_owner(
                key,
                ThreadOwnership {
                    owner: ThreadOwner::AnotherApp,
                    role: Some("escalation".to_owned()),
                    app_id: Some(AppId::new("1234")),
                    since: datetime!(2026-09-24 12:06 UTC),
                },
            )
            .await
            .unwrap()
    );
    let contact = StoredContact {
        key: key.clone(),
        full_name: Some(format!("{marker} Full")),
        first_name: Some(format!("{marker} First")),
        phone_number: None,
        user_id: Some(UserId::new(key.contact.clone())),
        parent_user_id: None,
        username: Some(format!("{marker}_user")),
        synced_at: datetime!(2026-09-24 12:07 UTC),
    };
    assert!(store.put_contact(contact.clone()).await.unwrap());
    // Keyed by phone number, naming the contact's BSUID.
    assert!(
        store
            .put_contact(StoredContact {
                key: ConversationKey::new(
                    key.phone_number_id.clone(),
                    format!("1650555{}", key.contact.len())
                ),
                phone_number: Some(format!("1650555{}", key.contact.len())),
                ..contact
            })
            .await
            .unwrap()
    );
}

/// Decisive for erasure on Postgres: after `erase`, no row of any table
/// of the schema mentions the erased contact or holds its content (text,
/// payloads, errors, the preview, window events, the owner, synced
/// contacts' names), while another contact of the number keeps every one
/// of its records. An erase that leaves any table out fails here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_erase_leaves_nothing_of_the_contact_in_any_table() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let store = PostgresConversationStore::new(db.pool.clone());
    let erased = ConversationKey::new("106540352242922", "US.ERASED.13491208655302741918");
    let kept = ConversationKey::new("106540352242922", "US.KEPT.13491208655302741918");
    let (erased_marker, kept_marker) = ("erase-me-7f3a", "keep-me-c21d");
    record_a_contact(&store, &erased, erased_marker).await;
    record_a_contact(&store, &kept, kept_marker).await;

    let holding = |found: &[(String, i64)]| -> Vec<String> {
        found
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(t, _)| t.clone())
            .collect()
    };
    let every_record_table = [
        "wa_conversations",
        "wa_messages",
        "wa_synced_contacts",
        "wa_thread_owners",
        "wa_window_events",
    ];
    let needles = [erased.contact.as_str(), erased_marker];
    assert_eq!(
        holding(&rows_mentioning(&db, &needles).await),
        every_record_table,
        "the sweep sees the contact in every table before the erasure"
    );

    assert_eq!(
        store.erase(&erased).await.unwrap(),
        Erased {
            messages: 5,
            conversations: 1,
            window_events: 2,
            thread_owners: 1,
            contacts: 2,
        }
    );
    let left = rows_mentioning(&db, &needles).await;
    assert!(
        holding(&left).is_empty(),
        "rows mentioning the erased contact remain: {left:?}"
    );
    assert_eq!(
        holding(&rows_mentioning(&db, &[kept.contact.as_str(), kept_marker]).await),
        every_record_table,
        "the other contact keeps every record"
    );
}

/// `with_retention` is what `apply_retention` applies; the default keeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_apply_retention_purges_by_the_configured_retention() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let keep = PostgresConversationStore::new(db.pool.clone());
    let thirty_days =
        PostgresConversationStore::new(db.pool.clone()).with_retention(Retention::days(30));
    assert_eq!(keep.retention(), Retention::Keep);
    assert!(format!("{thirty_days:?}").contains("retention: For("));
    let now = datetime!(2026-09-24 12:00 UTC);
    let key = ConversationKey::new("pn-retention", "US.1");
    for (id, days) in [("old", 31), ("new", 29)] {
        keep.append(StoredMessage {
            id: MessageId::new(id),
            conversation: key.clone(),
            direction: Direction::Inbound,
            kind: "text".to_owned(),
            text: Some(id.to_owned()),
            payload: serde_json::json!({}),
            status: DeliveryStatus::Received,
            timestamp: now - time::Duration::days(days),
            status_at: None,
            error: None,
        })
        .await
        .unwrap();
    }
    assert!(keep.apply_retention(now).await.unwrap().is_empty());
    assert_eq!(keep.messages(&key, None, 10).await.unwrap().len(), 2);
    assert_eq!(
        thirty_days.apply_retention(now).await.unwrap(),
        Purged {
            messages: 1,
            ..Purged::default()
        }
    );
    let left: Vec<String> = keep
        .messages(&key, None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id.into_inner())
        .collect();
    assert_eq!(left, ["new"]);
}

/// The new records refuse U+0000 in their identifiers, as messages do,
/// and store nothing then.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_records_refuse_nul_in_ids() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let store = PostgresConversationStore::new(db.pool.clone());
    let at = datetime!(2026-09-24 12:00 UTC);
    for key in [
        ConversationKey::new("pn-\0", "US.1"),
        ConversationKey::new("pn-nul", "US.\0"),
    ] {
        let event = WindowEvent {
            conversation: key.clone(),
            kind: WindowEventKind::CustomerCall,
            id: "wacid.1".to_owned(),
            at,
        };
        assert!(store.record_window_event(event).await.is_err(), "{key:?}");
        let owner = ThreadOwnership {
            owner: ThreadOwner::ThisApp,
            role: None,
            app_id: None,
            since: at,
        };
        assert!(store.set_thread_owner(&key, owner).await.is_err());
        let contact = StoredContact {
            key: key.clone(),
            full_name: None,
            first_name: None,
            phone_number: None,
            user_id: None,
            parent_user_id: None,
            username: None,
            synced_at: at,
        };
        assert!(store.put_contact(contact).await.is_err());
        assert!(store.erase(&key).await.is_err());
    }
    let event = WindowEvent {
        conversation: ConversationKey::new("pn-nul", "US.1"),
        kind: WindowEventKind::CustomerCall,
        id: "wacid.\0".to_owned(),
        at,
    };
    assert!(store.record_window_event(event).await.is_err());
    let counts: Vec<i64> = futures::future::join_all(
        ["wa_window_events", "wa_thread_owners", "wa_synced_contacts"].map(|t| {
            let pool = db.pool.clone();
            async move {
                sqlx::query_scalar(AssertSqlSafe(format!("SELECT count(*) FROM {t}")))
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        }),
    )
    .await;
    assert_eq!(counts, [0, 0, 0], "nothing stored");
}

// Races between erase, purge, append and fill (review of roadmap L5).
//
// Each case parks one operation on a row lock a plain transaction holds
// (the "blocker"), starts the other, waits until both sessions wait on a
// lock (`pg_stat_activity`, by the pool's `application_name`), then lets
// the blocker go. That makes an interleaving that is a matter of
// milliseconds in production happen every run.

/// A pool on `db`'s schema whose sessions are named `app` (so the case can
/// see its own sessions wait) and carry the planner `settings`.
async fn racing_pool(db: &TestDb, app: &str, settings: &[(&str, &str)]) -> PgPool {
    let mut options = vec![("search_path", db.schema.as_str())];
    options.extend_from_slice(settings);
    let connect = PgConnectOptions::from_str(&db.url)
        .unwrap()
        .application_name(app)
        .options(options);
    PgPoolOptions::new()
        .max_connections(4)
        .connect_with(connect)
        .await
        .unwrap()
}

/// Wait until `n` sessions named `app` wait on a lock, or `done` says the
/// racing operation finished without waiting.
async fn until_waiting(db: &TestDb, app: &str, n: i64, done: impl Fn() -> bool) {
    for _ in 0..1000 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE application_name = $1 AND wait_event_type = 'Lock'",
        )
        .bind(app)
        .fetch_one(&db.admin)
        .await
        .unwrap();
        if waiting >= n || done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{n} sessions of {app} never waited on a lock");
}

/// A transaction holding `FOR UPDATE` on the rows `select` returns.
async fn blocker(
    db: &TestDb,
    select: &str,
    binds: &[&str],
) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut tx = db.pool.begin().await.unwrap();
    let mut query = sqlx::query(AssertSqlSafe(format!("{select} FOR UPDATE")));
    for b in binds {
        query = query.bind(b.to_string());
    }
    let locked = query.execute(&mut *tx).await.unwrap().rows_affected();
    assert!(locked > 0, "the blocker locks something: {select}");
    tx
}

fn race_message(key: &ConversationKey, local: &str, minute: i64) -> StoredMessage {
    StoredMessage {
        id: MessageId::new(format!("wamid.race.{local}")),
        conversation: key.clone(),
        direction: Direction::Inbound,
        kind: "text".to_owned(),
        text: Some(format!("text of {local}")),
        payload: serde_json::json!({"text": {"body": local}}),
        status: DeliveryStatus::Received,
        timestamp: datetime!(2026-09-24 12:00 UTC) + time::Duration::minutes(minute),
        status_at: None,
        error: None,
    }
}

fn placeholder(key: &ConversationKey, local: &str, minute: i64) -> StoredMessage {
    StoredMessage {
        kind: StoredMessage::MEDIA_PLACEHOLDER.to_owned(),
        text: None,
        payload: serde_json::json!({"type": "media_placeholder"}),
        ..race_message(key, local, minute)
    }
}

/// The history and the summary of `key` agree: a conversation with
/// messages (tombstones aside) has a summary, of its latest one, and one
/// without has none.
async fn assert_summary_matches_history(store: &PostgresConversationStore, key: &ConversationKey) {
    let messages: Vec<StoredMessage> = store
        .messages(key, None, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter(|m| m.kind != StoredMessage::REVOKED)
        .collect();
    let summary = store
        .conversations(&key.phone_number_id, None, 1000)
        .await
        .unwrap()
        .into_iter()
        .find(|s| &s.key == key);
    match (messages.first(), &summary) {
        (None, None) => {}
        (Some(latest), Some(s)) => {
            assert_eq!(s.last_message_at, latest.timestamp, "{key}: latest message");
            assert_eq!(s.last_text, latest.text, "{key}: preview");
            let inbound = messages
                .iter()
                .filter(|m| m.direction == Direction::Inbound)
                .count();
            assert!(
                s.unread <= inbound as u64,
                "{key}: unread {} of {inbound}",
                s.unread
            );
        }
        (latest, _) => panic!(
            "{key}: history and summary disagree: latest message {:?}, summary {summary:?}",
            latest.map(|m| m.id.as_str())
        ),
    }
}

/// Risk (a), append first: an append has inserted its message and waits
/// on the conversation's summary row when the erasure starts. The erasure
/// must take it along (it committed before the erasure deleted anything),
/// never leave the message behind without its summary.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_erase_takes_an_append_in_flight_along() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let app = format!("race_{}", common::unique());
    let store = PostgresConversationStore::new(racing_pool(&db, &app, &[]).await);
    let key = ConversationKey::new("106540352242922", "US.13491208655302741918");
    assert!(store.append(race_message(&key, "first", 0)).await.unwrap());

    let lock = blocker(
        &db,
        "SELECT 1 FROM wa_conversations WHERE phone_number_id = $1 AND contact = $2",
        &[key.phone_number_id.as_str(), &key.contact],
    )
    .await;
    let append = tokio::spawn({
        let (store, m) = (store.clone(), race_message(&key, "second", 1));
        async move { store.append(m).await }
    });
    until_waiting(&db, &app, 1, || false).await;
    let erase = tokio::spawn({
        let (store, key) = (store.clone(), key.clone());
        async move { store.erase(&key).await }
    });
    until_waiting(&db, &app, 2, || erase.is_finished()).await;
    lock.commit().await.unwrap();

    assert!(append.await.unwrap().unwrap());
    let erased = erase.await.unwrap().unwrap();
    assert_summary_matches_history(&store, &key).await;
    assert!(
        store.messages(&key, None, 10).await.unwrap().is_empty(),
        "the append committed before the erasure deleted: erased with the rest"
    );
    assert_eq!((erased.messages, erased.conversations), (2, 1));
}

/// Risk (a), erasure first: an append that starts while the erasure is
/// deleting is recorded after it, with a summary of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_an_append_during_an_erasure_is_recorded_after_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let app = format!("race_{}", common::unique());
    let store = PostgresConversationStore::new(racing_pool(&db, &app, &[]).await);
    let key = ConversationKey::new("106540352242922", "US.13491208655302741918");
    let first = race_message(&key, "first", 0);
    assert!(store.append(first.clone()).await.unwrap());

    let lock = blocker(
        &db,
        "SELECT 1 FROM wa_messages WHERE id = $1",
        &[first.id.as_str()],
    )
    .await;
    let erase = tokio::spawn({
        let (store, key) = (store.clone(), key.clone());
        async move { store.erase(&key).await }
    });
    until_waiting(&db, &app, 1, || false).await;
    let second = race_message(&key, "second", 1);
    let append = tokio::spawn({
        let (store, m) = (store.clone(), second.clone());
        async move { store.append(m).await }
    });
    until_waiting(&db, &app, 2, || append.is_finished()).await;
    lock.commit().await.unwrap();

    let erased = erase.await.unwrap().unwrap();
    assert!(append.await.unwrap().unwrap());
    assert_eq!(erased.messages, 1);
    assert_summary_matches_history(&store, &key).await;
    assert_eq!(
        store.messages(&key, None, 10).await.unwrap(),
        std::slice::from_ref(&second)
    );
    let summary = store
        .conversations(&key.phone_number_id, None, 10)
        .await
        .unwrap();
    assert_eq!(
        summary.len(),
        1,
        "the append after the erasure has its summary"
    );
    assert_eq!(summary[0].unread, 1);
    assert_eq!(summary[0].last_inbound_at, Some(second.timestamp));
}

/// Risk (a), unorchestrated: appends and erasures of one conversation at
/// once, from two pools, never leave a message without its summary or a
/// summary without messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_erasures_racing_appends_keep_history_and_summary_together() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let a = PostgresConversationStore::new(db.pool.clone());
    let b = PostgresConversationStore::new(TestDb::pool_on(&db.url, &db.schema, 10).await);
    for round in 0..40 {
        let key = ConversationKey::new("106540352242922", format!("US.{round}"));
        assert!(
            a.append(race_message(&key, &format!("{round}-0"), 0))
                .await
                .unwrap()
        );
        let appends = (1..6).map(|i| {
            let store = if i % 2 == 0 { &a } else { &b };
            store.append(race_message(&key, &format!("{round}-{i}"), i))
        });
        let (appended, erased) = tokio::join!(futures::future::join_all(appends), async {
            tokio::task::yield_now().await;
            b.erase(&key).await
        });
        for r in appended {
            r.unwrap();
        }
        erased.unwrap();
        assert_summary_matches_history(&a, &key).await;
    }
}

/// Risk (b): two purges whose plans visit the old rows in opposite
/// orders (two replicas' `apply_retention`, their cutoffs picking
/// different plans) never deadlock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_two_purges_at_once_never_deadlock() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let app = format!("race_{}", common::unique());
    // Oldest first by the `ts` index; physical (insertion) order by a
    // sequential scan: newest first below.
    let by_index = PostgresConversationStore::new(
        racing_pool(
            &db,
            &app,
            &[("enable_seqscan", "off"), ("enable_bitmapscan", "off")],
        )
        .await,
    );
    let by_scan = PostgresConversationStore::new(
        racing_pool(
            &db,
            &app,
            &[("enable_indexscan", "off"), ("enable_bitmapscan", "off")],
        )
        .await,
    );
    let key = ConversationKey::new("106540352242922", "US.13491208655302741918");
    let rows: Vec<StoredMessage> = (1..=3)
        .rev()
        .map(|m| race_message(&key, &format!("t{m}"), m))
        .collect();
    for m in &rows {
        assert!(by_index.append(m.clone()).await.unwrap());
    }
    let cutoff = datetime!(2026-09-25 0:00 UTC);

    let lock = blocker(
        &db,
        "SELECT 1 FROM wa_messages WHERE id = $1",
        &["wamid.race.t2"],
    )
    .await;
    let first = tokio::spawn({
        let store = by_scan.clone();
        async move { store.purge_before(None, cutoff).await }
    });
    until_waiting(&db, &app, 1, || false).await;
    let second = tokio::spawn({
        let store = by_index.clone();
        async move { store.purge_before(None, cutoff).await }
    });
    until_waiting(&db, &app, 2, || second.is_finished()).await;
    lock.commit().await.unwrap();
    let (first, second) = (first.await.unwrap(), second.await.unwrap());
    assert!(
        first.is_ok() && second.is_ok(),
        "both purges succeed: {first:?}, {second:?}"
    );
    let purged = first.unwrap().messages + second.unwrap().messages;
    assert_eq!(purged, 3, "the three rows, once");
    assert_summary_matches_history(&by_index, &key).await;
}

/// Risk (b) again: an erasure (newest first, by the conversation's index)
/// and a purge (oldest first, by the `ts` index) over the same old rows
/// never deadlock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_an_erasure_and_a_purge_at_once_never_deadlock() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let app = format!("race_{}", common::unique());
    let store = PostgresConversationStore::new(
        racing_pool(
            &db,
            &app,
            &[("enable_seqscan", "off"), ("enable_bitmapscan", "off")],
        )
        .await,
    );
    let key = ConversationKey::new("106540352242922", "US.13491208655302741918");
    for m in 1..=3 {
        assert!(
            store
                .append(race_message(&key, &format!("t{m}"), m))
                .await
                .unwrap()
        );
    }
    let cutoff = datetime!(2026-09-25 0:00 UTC);

    let lock = blocker(
        &db,
        "SELECT 1 FROM wa_messages WHERE id = $1",
        &["wamid.race.t2"],
    )
    .await;
    let erase = tokio::spawn({
        let (store, key) = (store.clone(), key.clone());
        async move { store.erase(&key).await }
    });
    until_waiting(&db, &app, 1, || false).await;
    let purge = tokio::spawn({
        let store = store.clone();
        async move { store.purge_before(None, cutoff).await }
    });
    until_waiting(&db, &app, 2, || purge.is_finished()).await;
    lock.commit().await.unwrap();
    let (erase, purge) = (erase.await.unwrap(), purge.await.unwrap());
    assert!(
        erase.is_ok() && purge.is_ok(),
        "both succeed: {erase:?}, {purge:?}"
    );
    assert_eq!(erase.unwrap().messages + purge.unwrap().messages, 3);
    assert_summary_matches_history(&store, &key).await;
}

/// Risk (c): `fill_media_placeholder` racing an erasure or a purge, in
/// either order, never deadlocks, never stores the content after the
/// deletion, and leaves history and summary together.
#[allow(clippy::too_many_lines)] // four orders of one race, read top to bottom
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_fills_racing_erasures_and_purges() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let cutoff = datetime!(2026-09-25 0:00 UTC);
    for (case, delete_first, erase) in [
        ("fill-then-erase", false, true),
        ("erase-then-fill", true, true),
        ("fill-then-purge", false, false),
        ("purge-then-fill", true, false),
    ] {
        let app = format!("race_{}", common::unique());
        let store = PostgresConversationStore::new(racing_pool(&db, &app, &[]).await);
        let key = ConversationKey::new("106540352242922", format!("US.{case}"));
        assert!(
            store
                .append(race_message(&key, &format!("{case}-text"), 0))
                .await
                .unwrap()
        );
        let media = placeholder(&key, &format!("{case}-media"), 1);
        assert_eq!(
            store.append_synced(vec![media.clone()]).await.unwrap(),
            [true]
        );

        // Fill first: the placeholder's row is held, the fill queues on it,
        // then the deletion. Deletion first: the summary's row is held,
        // the deletion (which takes the messages first) queues on it, then
        // the fill queues on the placeholder the deletion took.
        let lock = if delete_first {
            blocker(
                &db,
                "SELECT 1 FROM wa_conversations WHERE phone_number_id = $1 AND contact = $2",
                &[key.phone_number_id.as_str(), &key.contact],
            )
            .await
        } else {
            blocker(
                &db,
                "SELECT 1 FROM wa_messages WHERE id = $1",
                &[media.id.as_str()],
            )
            .await
        };
        let fill = {
            let (store, pn, id) = (store.clone(), key.phone_number_id.clone(), media.id.clone());
            async move {
                store
                    .fill_media_placeholder(
                        &pn,
                        &id,
                        "image".to_owned(),
                        Some("caption".to_owned()),
                        serde_json::json!({"image": {"caption": "caption"}}),
                    )
                    .await
            }
        };
        let delete = {
            let (store, key) = (store.clone(), key.clone());
            async move {
                if erase {
                    store.erase(&key).await.map(|e| e.messages)
                } else {
                    store
                        .purge_before(Some(&key.phone_number_id), cutoff)
                        .await
                        .map(|p| p.messages)
                }
            }
        };
        let (fill, delete) = if delete_first {
            let delete = tokio::spawn(delete);
            until_waiting(&db, &app, 1, || false).await;
            let fill = tokio::spawn(fill);
            until_waiting(&db, &app, 2, || fill.is_finished()).await;
            lock.commit().await.unwrap();
            (fill.await.unwrap(), delete.await.unwrap())
        } else {
            let fill = tokio::spawn(fill);
            until_waiting(&db, &app, 1, || false).await;
            let delete = tokio::spawn(delete);
            until_waiting(&db, &app, 2, || delete.is_finished()).await;
            lock.commit().await.unwrap();
            (fill.await.unwrap(), delete.await.unwrap())
        };
        assert!(
            fill.is_ok() && delete.is_ok(),
            "{case}: {fill:?}, {delete:?}"
        );
        assert_eq!(
            fill.unwrap(),
            !delete_first,
            "{case}: filled only before the deletion"
        );
        assert_eq!(delete.unwrap(), 2, "{case}: both messages deleted");
        assert!(
            store.messages(&key, None, 10).await.unwrap().is_empty(),
            "{case}"
        );
        assert_eq!(
            store
                .message(&key.phone_number_id, &media.id)
                .await
                .unwrap(),
            None,
            "{case}"
        );
        assert_summary_matches_history(&store, &key).await;
    }
}

/// The locks are the ones `store::postgres::conversation` documents (a
/// stable identifier: replicas of two revisions must take the same ones),
/// written out here apart from the code: held from outside, the number
/// lock holds back an append and an erasure of that number only, and the
/// purge lock a purge and an erasure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_the_locks_are_the_documented_ones() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    postgres::migrate(&db.pool).await.unwrap();
    let app = format!("race_{}", common::unique());
    let store = PostgresConversationStore::new(racing_pool(&db, &app, &[]).await);
    let key = ConversationKey::new("106540352242922", "US.13491208655302741918");
    let elsewhere = ConversationKey::new("106540352242923", "US.13491208655302741918");
    let cutoff = datetime!(2026-09-25 0:00 UTC);

    for (lock, held, number_lock) in [
        (
            "SELECT pg_advisory_xact_lock('wa_messages'::regclass::oid::int4, hashtext('106540352242922'))",
            ["append", "append_synced", "erase"],
            true,
        ),
        (
            "SELECT pg_advisory_xact_lock('wa_conversations'::regclass::oid::int4, 0)",
            ["purge", "purge_number", "erase"],
            false,
        ),
    ] {
        let mut outside = db.pool.begin().await.unwrap();
        sqlx::query(AssertSqlSafe(lock))
            .execute(&mut *outside)
            .await
            .unwrap();
        let n = common::unique();
        // Not held back: under the number lock, another number's append
        // and erasure; under the purge lock, appends.
        if number_lock {
            assert!(
                store
                    .append(race_message(&elsewhere, &format!("{n}-free"), 0))
                    .await
                    .unwrap()
            );
            store.erase(&elsewhere).await.unwrap();
        } else {
            assert!(
                store
                    .append(race_message(&key, &format!("{n}-free"), 0))
                    .await
                    .unwrap()
            );
            assert_eq!(
                store
                    .append_synced(vec![race_message(&key, &format!("{n}-free-synced"), 0)])
                    .await
                    .unwrap(),
                [true]
            );
        }
        let mut waiting = Vec::new();
        for (i, op) in held.into_iter().enumerate() {
            let (store, key) = (store.clone(), key.clone());
            let m = race_message(&key, &format!("{n}-{i}"), 0);
            waiting.push(tokio::spawn(async move {
                match op {
                    "append" => store.append(m).await.map(drop),
                    "append_synced" => store.append_synced(vec![m]).await.map(drop),
                    "erase" => store.erase(&key).await.map(drop),
                    "purge" => store.purge_before(None, cutoff).await.map(drop),
                    _ => store
                        .purge_before(Some(&key.phone_number_id), cutoff)
                        .await
                        .map(drop),
                }
            }));
            until_waiting(&db, &app, i64::try_from(i).unwrap() + 1, || false).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            waiting.iter().all(|w| !w.is_finished()),
            "{lock}: every operation waits"
        );
        outside.commit().await.unwrap();
        for w in waiting {
            w.await.unwrap().unwrap();
        }
    }
}
