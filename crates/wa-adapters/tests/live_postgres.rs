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
use wa_core::error::StorageError;
use wa_core::ids::MessageId;
use wa_core::store::{
    ConversationKey, ConversationStore, DeliveryStatus, Direction, Expiry, KvStore, StoreKey,
    StoredMessage,
};

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
/// it. `wa_rs::inbox`'s unit tests use a store double with exactly these
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
    assert_eq!(
        db.table_names().await,
        [
            "wa_conversations",
            "wa_kv",
            "wa_messages",
            "wa_sqlx_migrations"
        ]
    );
    assert_eq!(db.applied_migrations().await, [1, 2, 3]);
    assert_eq!(db.content_columns().await, LOSSLESS_CONTENT_COLUMNS);
    db.drop().await;
}

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
    assert_eq!(db.applied_migrations().await, [1, 2, 3]);
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
    db.drop().await;
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
/// per database), one schema per case in it.
struct PrivateDb {
    url: String,
    name: String,
    admin: PgPool,
}

impl PrivateDb {
    async fn new(label: &str) -> Option<Self> {
        let url = common::service_url("WA_RS_TEST_POSTGRES_URL")?;
        let name = format!("wa_test_{label}_{}", common::unique());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        sqlx::query(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&admin)
            .await
            .unwrap();
        Some(Self { url, name, admin })
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

    async fn drop(self) {
        sqlx::query(AssertSqlSafe(format!(
            "DROP DATABASE {} WITH (FORCE)",
            self.name
        )))
        .execute(&self.admin)
        .await
        .unwrap();
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
    db.drop().await;
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
    db.drop().await;
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
