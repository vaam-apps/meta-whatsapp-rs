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

/// Roadmap M2h: migration 5 rewrites no table (its columns are metadata
/// only on Postgres 11 and later: one without a default, one with a
/// constant default, and a default set for the rows to come), so a
/// database with many numbers migrates without holding its numbers table
/// for a copy; and a number bound before it reads its WABA's start and a
/// previous tenant unknown (`UNRECORDED_MOVE`), and keeps that start when
/// a binding of its tenant's other WABA takes it (it would read the new
/// WABA's otherwise). Decisive: a volatile default or a column's type
/// changed in the migration (either rewrites the table into a new file);
/// for the start, the refresh's `COALESCE` of it. An update of the
/// existing rows would not change the file; the migration has none.
#[tokio::test]
async fn live_postgres_migration_5_rewrites_no_table() {
    use meta_whatsapp_rs::adapters::store::postgres::sqlx::migrate::Migrator;
    use meta_whatsapp_rs::core::ids::PhoneNumberId;
    use meta_whatsapp_server_core::model::UNRECORDED_MOVE;
    use meta_whatsapp_server_core::store::RecordStore as _;
    use meta_whatsapp_server_store_postgres::migrations;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(3).await;
    meta_whatsapp_rs::adapters::store::postgres::migrate(&pool)
        .await
        .unwrap();
    let before_5: Vec<_> = migrations().into_iter().filter(|m| m.version < 5).collect();
    assert_eq!(before_5.len(), 4);
    let mut migrator = Migrator::with_migrations(before_5);
    migrator.dangerous_set_table_name(MIGRATIONS_TABLE);
    migrator.run(&pool).await.unwrap();
    sqlx::query("INSERT INTO wa_server_tenants (id, name) VALUES ('legacy', '')")
        .execute(&pool)
        .await
        .unwrap();
    let attached_at: time::OffsetDateTime = sqlx::query_scalar(
        "INSERT INTO wa_server_wabas (waba_id, tenant_id) VALUES ('5001', 'legacy') \
         RETURNING attached_at",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO wa_server_numbers (phone_number_id, waba_id, tenant_id) \
         VALUES ('6001', '5001', 'legacy')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let file = || async {
        let node: i64 = sqlx::query_scalar(
            "SELECT relfilenode::bigint FROM pg_class \
             WHERE oid = 'wa_server_numbers'::regclass",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        node
    };
    let before = file().await;
    migrate(&pool).await.unwrap();
    assert_eq!(
        file().await,
        before,
        "migration 5 rewrote wa_server_numbers"
    );
    let number = PgStore::new(pool.clone())
        .number(&PhoneNumberId::new("6001").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(number.attached_at, attached_at, "its WABA's start");
    assert_eq!(number.previous_tenant_released_at, Some(UNRECORDED_MOVE));

    // The tenant's other WABA, bound later, takes the number: its start
    // stays its first WABA's.
    let store = PgStore::new(pool.clone());
    let legacy = TenantId::parse("legacy").unwrap();
    let pn = PhoneNumberId::new("6001").unwrap();
    store
        .bind_waba(
            &legacy,
            &meta_whatsapp_rs::core::ids::WabaId::new("5002").unwrap(),
            std::slice::from_ref(&pn),
        )
        .await
        .unwrap();
    let moved = store.number(&pn).await.unwrap().unwrap();
    assert_eq!(moved.waba_id.as_str(), "5002");
    assert_eq!(
        moved.attached_at, attached_at,
        "its first WABA's start, not the second's"
    );
    assert_eq!(moved.previous_tenant_released_at, Some(UNRECORDED_MOVE));
}

/// Roadmap M2f: migration 8 adds `wa_server_events.contacts` and its
/// partial GIN index without rewriting the outbox (no default, no update),
/// so a deployment with a large outbox migrates without holding it for a
/// copy. A row written before it names nobody to an erasure, whatever its
/// data says (it goes with the outbox's retention); an insert of the
/// previous release (no `contacts`) still works after it, and names
/// nobody either; a row this release inserts is erased by its contacts,
/// the business's own number left out of them. Decisive: a default on the
/// column (it rewrites the table into a new file), an update of the
/// existing rows (they would be erased).
#[tokio::test]
async fn live_postgres_migration_8_rewrites_no_table_and_names_from_its_release_on() {
    use meta_whatsapp_rs::adapters::store::postgres::sqlx::migrate::Migrator;
    use meta_whatsapp_rs::core::ids::PhoneNumberId;
    use meta_whatsapp_server_store_postgres::migrations;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(3).await;
    meta_whatsapp_rs::adapters::store::postgres::migrate(&pool)
        .await
        .unwrap();
    let before_8: Vec<_> = migrations().into_iter().filter(|m| m.version < 8).collect();
    assert_eq!(before_8.len(), 7);
    let mut migrator = Migrator::with_migrations(before_8);
    migrator.dangerous_set_table_name(MIGRATIONS_TABLE);
    migrator.run(&pool).await.unwrap();
    // A row of the previous release: an operator-only message from the
    // customer on 6101 (its sequence far past the stream's, which a raw
    // insert does not draw).
    let message = |sequence: i64| {
        format!(
            "INSERT INTO wa_server_events \
             (tenant_id, sequence, id, phone_number_id, event_type, data, data_bytes) \
             VALUES (NULL, {sequence}, 'evt_m7_{sequence}', '6101', 'message_received', \
                     '{{\"event\":\"message_received\",\"message\":{{\"from\":\"16505551234\"}}}}'::json, 10)"
        )
    };
    sqlx::query(sqlx::AssertSqlSafe(message(1001)))
        .execute(&pool)
        .await
        .unwrap();
    let file = || async {
        let node: i64 = sqlx::query_scalar(
            "SELECT relfilenode::bigint FROM pg_class \
             WHERE oid = 'wa_server_events'::regclass",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        node
    };
    let before = file().await;
    migrate(&pool).await.unwrap();
    assert_eq!(file().await, before, "migration 8 rewrote wa_server_events");
    // The previous release's insert, after the migration.
    sqlx::query(sqlx::AssertSqlSafe(message(1002)))
        .execute(&pool)
        .await
        .unwrap();
    let events = PgEventStore::new(pool.clone());
    let anyone = TenantId::parse("anyone").unwrap();
    let pn = PhoneNumberId::new("6101").unwrap();
    let contacts = ["16505551234".to_owned()];
    assert_eq!(
        events
            .erase_contacts(&anyone, &pn, &contacts)
            .await
            .unwrap(),
        0,
        "a row without contacts names nobody"
    );
    // This release's insert names its customer, not the business.
    events
        .insert(&outbox_suite::from_row(None, "6101", "16505551234", None))
        .await
        .unwrap()
        .unwrap();
    let named: Vec<Vec<String>> =
        sqlx::query_scalar("SELECT contacts FROM wa_server_events WHERE contacts IS NOT NULL")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(named, [vec!["16505551234".to_owned()]]);
    assert_eq!(
        events
            .erase_contacts(&anyone, &pn, &contacts)
            .await
            .unwrap(),
        1
    );
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM wa_server_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 2, "the rows of before stay");
}

/// Roadmap M2f: the erasure's statement (`ERASE_SQL`, as
/// `PgEventStore::erase_contacts` runs it) probes migration 8's GIN index,
/// never a walk of the outbox: its definition; its plan with sequential
/// scans off (no other index can serve it: the tenant is read by
/// `tenant_id`, which no index leads with); and its plan on an outbox of
/// 20,000 rows never analyzed (autovacuum off for the table, so
/// `contacts` has no statistics, as right after the migration), with
/// sequential scans on. Decisive: the statement reading the tenant by
/// `stream` (the primary key's prefix: its plan then walks the key), a
/// B-tree index on the column, an index predicate the statement does not
/// imply.
#[tokio::test]
async fn live_postgres_the_erasure_probes_the_contacts_index() {
    use meta_whatsapp_server_store_postgres::ERASE_SQL;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(3).await;
    migrate(&pool).await.unwrap();
    let definition: String =
        sqlx::query_scalar("SELECT pg_get_indexdef('wa_server_events_contacts_idx'::regclass)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        definition.ends_with("USING gin (contacts) WHERE (contacts IS NOT NULL)"),
        "{definition}"
    );
    let mut conn = pool.acquire().await.unwrap();
    let sql = format!("EXPLAIN {ERASE_SQL}");
    let contacts = vec![
        "16505551234".to_owned(),
        "US.13491208655302741918".to_owned(),
    ];
    let probes = |plan: &[String]| {
        plan.iter()
            .any(|line| line.contains("Bitmap Index Scan on wa_server_events_contacts_idx"))
    };
    sqlx::query("SET enable_seqscan = off")
        .execute(&mut *conn)
        .await
        .unwrap();
    let plan: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.clone()))
        .bind("merchant-42")
        .bind("106540352242922")
        .bind(&contacts)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
    assert!(probes(&plan), "{plan:?}");
    sqlx::query("RESET enable_seqscan")
        .execute(&mut *conn)
        .await
        .unwrap();
    // Many rows, no statistics.
    sqlx::query("ALTER TABLE wa_server_events SET (autovacuum_enabled = false)")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO wa_server_events \
         (tenant_id, sequence, id, phone_number_id, event_type, data, data_bytes, contacts) \
         SELECT NULL, n, 'evt_plan_' || n, '106540352242922', 'message_received', \
                '{\"event\":\"message_received\"}'::json, 30, ARRAY['1650' || n::text] \
         FROM generate_series(1, 20000) AS n",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    let analyzed: Option<time::OffsetDateTime> = sqlx::query_scalar(
        "SELECT greatest(last_analyze, last_autoanalyze) FROM pg_stat_user_tables \
         WHERE relid = 'wa_server_events'::regclass",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(analyzed, None, "the outbox was analyzed");
    let plan: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind("merchant-42")
        .bind("106540352242922")
        .bind(&contacts)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
    assert!(probes(&plan), "without statistics: {plan:?}");
}

/// Roadmap M2e: migration 7 adds `wa_server_events.media_id` and its
/// partial index without rewriting the outbox (the column has no default),
/// so a deployment with a large outbox migrates without holding it for a
/// copy. A row written before it records no media id, whatever its data
/// says: its media goes through Meta's `phone_number_id` check, as every
/// id did before. An insert the previous release makes (no `media_id`)
/// still works after it, and records none either. A row this release
/// inserts records its media id, found by the index: its definition, and
/// a plan of the lookup that probes it. Decisive: a default on the column
/// (it rewrites the table into a new file), an update of the existing rows
/// (they would record theirs), an index whose predicate or collation the
/// lookup cannot use.
#[tokio::test]
async fn live_postgres_migration_7_rewrites_no_table_and_records_from_its_release_on() {
    use meta_whatsapp_rs::adapters::store::postgres::sqlx::migrate::Migrator;
    use meta_whatsapp_rs::core::ids::MediaId;
    use meta_whatsapp_server_store_postgres::migrations;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(3).await;
    meta_whatsapp_rs::adapters::store::postgres::migrate(&pool)
        .await
        .unwrap();
    let before_7: Vec<_> = migrations().into_iter().filter(|m| m.version < 7).collect();
    assert_eq!(before_7.len(), 6);
    let mut migrator = Migrator::with_migrations(before_7);
    migrator.dangerous_set_table_name(MIGRATIONS_TABLE);
    migrator.run(&pool).await.unwrap();
    // A row of the previous release: an image received, operator-only (its
    // sequence far past the stream's, which a raw insert does not draw).
    let image = |id: &str, sequence: i64| {
        format!(
            "INSERT INTO wa_server_events \
             (tenant_id, sequence, id, phone_number_id, event_type, data, data_bytes) \
             VALUES (NULL, {sequence}, 'evt_m6_{sequence}', '6101', 'message_received', \
                     '{{\"event\":\"message_received\",\"message\":{{\"type\":\"image\",\
                     \"image\":{{\"id\":\"{id}\"}}}}}}'::json, 10)"
        )
    };
    sqlx::query(sqlx::AssertSqlSafe(image("1003383421387261", 1001)))
        .execute(&pool)
        .await
        .unwrap();
    let file = || async {
        let node: i64 = sqlx::query_scalar(
            "SELECT relfilenode::bigint FROM pg_class \
             WHERE oid = 'wa_server_events'::regclass",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        node
    };
    let before = file().await;
    migrate(&pool).await.unwrap();
    assert_eq!(file().await, before, "migration 7 rewrote wa_server_events");
    let events = PgEventStore::new(pool.clone());
    let media = |id: &str| MediaId::new(id).unwrap();
    assert!(
        events
            .received_media(&media("1003383421387261"))
            .await
            .unwrap()
            .is_empty(),
        "a row from before migration 7 records a media id"
    );
    // The previous release's insert, after the migration.
    sqlx::query(sqlx::AssertSqlSafe(image("1003383421387262", 1002)))
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        events
            .received_media(&media("1003383421387262"))
            .await
            .unwrap()
            .is_empty()
    );
    // This release's insert records it.
    let row = meta_whatsapp_server_core::outbox::NewEvent {
        data: r#"{"event":"message_received","message":{"type":"image","image":{"id":"1003383421387263"}}}"#
            .to_owned(),
        ..outbox_suite::row(None, "message_received", "6101", None)
    };
    events.insert(&row).await.unwrap();
    let found = events
        .received_media(&media("1003383421387263"))
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].tenant, None);
    assert_eq!(
        found[0]
            .phone_number_id
            .as_ref()
            .map(meta_whatsapp_rs::core::ids::PhoneNumberId::as_str),
        Some("6101")
    );
    the_media_lookup_probes_its_index(&pool).await;
}

/// Migration 7's index, as `PgEventStore::received_media` reads it: its
/// definition, and a plan of the lookup that probes it.
async fn the_media_lookup_probes_its_index(pool: &sqlx::PgPool) {
    // The lookup is an index probe, never a walk of the outbox: the
    // partial index covers only the rows that record a media id, and its
    // predicate, collation and order fit `PgEventStore::received_media`'s
    // statement (copied here, with its parameters' types): with
    // sequential scans off, its plan reads the index backwards. (On
    // `media_id` alone the planner walks `wa_server_events_created_idx`
    // here, as it does on a large outbox before `media_id` has
    // statistics: every row read and filtered.)
    let definition: String =
        sqlx::query_scalar("SELECT pg_get_indexdef('wa_server_events_media_idx'::regclass)")
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(
        definition.ends_with("USING btree (media_id, created_at) WHERE (media_id IS NOT NULL)"),
        "{definition}"
    );
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("SET enable_seqscan = off")
        .execute(&mut *conn)
        .await
        .unwrap();
    let plan: Vec<String> = sqlx::query_scalar(
        "EXPLAIN SELECT tenant_id, phone_number_id, created_at FROM wa_server_events \
         WHERE media_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind("1003383421387263")
    .bind(16_i64)
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    sqlx::query("RESET enable_seqscan")
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("Index Scan Backward using wa_server_events_media_idx")),
        "{plan:?}"
    );
}

/// Roadmap M2a and M2h: a WABA unbound from one tenant before migration 5
/// (migration 4 recorded the WABA's move, nothing recorded its numbers')
/// and bound to another tenant after it, with its number: the number
/// reads as never released, so the WABA's cut alone hides the previous
/// tenant's history on it (`authz::history_from`: its binding's start).
/// Decisive: the WABA's cut in `history_from` (the number's is `None`).
#[tokio::test]
async fn live_postgres_a_waba_moved_before_migration_5_still_cuts_its_numbers() {
    use meta_whatsapp_rs::adapters::store::postgres::sqlx::migrate::Migrator;
    use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
    use meta_whatsapp_server_core::authz::history_from;
    use meta_whatsapp_server_core::store::RecordStore as _;
    use meta_whatsapp_server_store_postgres::migrations;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(3).await;
    meta_whatsapp_rs::adapters::store::postgres::migrate(&pool)
        .await
        .unwrap();
    let mut migrator =
        Migrator::with_migrations(migrations().into_iter().filter(|m| m.version < 5).collect());
    migrator.dangerous_set_table_name(MIGRATIONS_TABLE);
    migrator.run(&pool).await.unwrap();
    for tenant in ["before-5-a", "before-5-b"] {
        sqlx::query("INSERT INTO wa_server_tenants (id, name) VALUES ($1, '')")
            .bind(tenant)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO wa_server_wabas (waba_id, tenant_id, previous_tenant_unbound_at) \
         VALUES ('5301', 'before-5-a', NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO wa_server_numbers (phone_number_id, waba_id, tenant_id) \
         VALUES ('6301', '5301', 'before-5-a')",
    )
    .execute(&pool)
    .await
    .unwrap();
    // The unbind of the release before migration 5: the WABA's row and
    // its tombstone, its numbers by the cascade.
    sqlx::query(
        "WITH gone AS (DELETE FROM wa_server_wabas WHERE waba_id = '5301' \
           RETURNING waba_id, tenant_id, previous_tenant_unbound_at) \
         INSERT INTO wa_server_waba_tombstones \
           (waba_id, tenant_id, unbound_at, previous_tenant_unbound_at) \
         SELECT waba_id, tenant_id, now(), previous_tenant_unbound_at FROM gone",
    )
    .execute(&pool)
    .await
    .unwrap();
    migrate(&pool).await.unwrap();
    let store = PgStore::new(pool.clone());
    let (waba_id, pn) = (
        WabaId::new("5301").unwrap(),
        PhoneNumberId::new("6301").unwrap(),
    );
    store
        .bind_waba(
            &TenantId::parse("before-5-b").unwrap(),
            &waba_id,
            std::slice::from_ref(&pn),
        )
        .await
        .unwrap();
    let waba = store.waba(&waba_id).await.unwrap().unwrap();
    let number = store.number(&pn).await.unwrap().unwrap();
    assert!(waba.previous_tenant_unbound_at.is_some(), "a recorded move");
    assert_eq!(
        number.previous_tenant_released_at, None,
        "no release recorded before migration 5"
    );
    assert_eq!(
        history_from(&waba, &number),
        Some(waba.attached_at),
        "the WABA's cut"
    );
}

/// Roadmap M2h, a rolling deploy: a replica of the previous release
/// releases numbers with its own statements (a refresh's `DELETE` of the
/// numbers its list lacks, an unbind's cascade), which know nothing of the
/// tombstones; migration 5's trigger records the release all the same, so
/// a binding of this release to another tenant reads a move, its start
/// after the release. Decisive: the trigger (without it, the number reads
/// as never held, and the inbox shows the new tenant the previous one's
/// history).
#[tokio::test]
async fn live_postgres_a_release_by_the_previous_release_is_recorded() {
    use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
    use meta_whatsapp_server_core::store::RecordStore as _;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(3).await;
    migrate(&pool).await.unwrap();
    let store = PgStore::new(pool.clone());
    let (a, b) = (
        TenantId::parse("rolling-a").unwrap(),
        TenantId::parse("rolling-b").unwrap(),
    );
    store.create_tenant(&a, "").await.unwrap();
    store.create_tenant(&b, "").await.unwrap();
    let (pn1, pn2) = (
        PhoneNumberId::new("6101").unwrap(),
        PhoneNumberId::new("6102").unwrap(),
    );
    store
        .bind_waba(
            &a,
            &WabaId::new("5101").unwrap(),
            std::slice::from_ref(&pn1),
        )
        .await
        .unwrap();
    store
        .bind_waba(
            &a,
            &WabaId::new("5102").unwrap(),
            std::slice::from_ref(&pn2),
        )
        .await
        .unwrap();
    // The previous release's refresh of 5101 listing no number, and its
    // unbind of 5102 (the WABA's row, its numbers by the cascade).
    sqlx::query(
        "DELETE FROM wa_server_numbers WHERE waba_id = $1 AND NOT (phone_number_id = ANY($2))",
    )
    .bind("5101")
    .bind(Vec::<String>::new())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM wa_server_wabas WHERE waba_id = '5102'")
        .execute(&pool)
        .await
        .unwrap();
    for (pn, released_by) in [(&pn1, "5201"), (&pn2, "5202")] {
        let (tenant, released_at): (Option<String>, time::OffsetDateTime) = sqlx::query_as(
            "SELECT tenant_id, released_at FROM wa_server_number_tombstones              WHERE phone_number_id = $1",
        )
        .bind(pn.as_str())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(tenant.as_deref(), Some("rolling-a"), "{pn}");
        store
            .bind_waba(
                &b,
                &WabaId::new(released_by).unwrap(),
                std::slice::from_ref(pn),
            )
            .await
            .unwrap();
        let number = store.number(pn).await.unwrap().unwrap();
        assert_eq!(number.tenant_id, b);
        assert_eq!(
            number.previous_tenant_released_at,
            Some(released_at),
            "{pn}: a move"
        );
        assert!(number.attached_at >= released_at, "{pn}");
    }
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
    assert_eq!(service, [1, 2, 3, 4, 5, 6, 7, 8]);
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

/// Roadmap M2b, M2.2 at the bundle: two replicas (two backends, each on a
/// pool of its own, one database): an event recorded through one wakes a
/// stream on the other within 2 s (`NOTIFY` on the deployment's channel,
/// each replica's own `LISTEN` connection), another tenant's stream there
/// sees none of it, and a stream resumed there replays exactly the events
/// after its cursor. Decisive: `LISTEN`/`NOTIFY` (each replica's notifier
/// local to it fails the first step).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_an_event_through_one_replica_wakes_a_stream_on_another() {
    use meta_whatsapp_server_core::backend::Backend as _;
    use meta_whatsapp_server_store_postgres::PgBackend;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (pool_a, pool_b) = (db.pool(4).await, db.pool(4).await);
    migrate(&pool_a).await.unwrap();
    let (a, b) = (PgBackend::new(pool_a), PgBackend::new(pool_b));
    conformance::live::an_event_through_one_replica_wakes_a_stream_on_another(&a, &b).await;
    a.close().await;
    b.close().await;
}

/// A `NOTIFY` on another channel (another deployment sharing the
/// database) wakes nobody here; a subscription made through a closed
/// backend's notifier listens again.
#[tokio::test]
async fn live_postgres_the_channel_is_the_deployments() {
    use meta_whatsapp_server_core::notify::{EventNotifier, Wakeup};
    use meta_whatsapp_server_store_postgres::{EVENTS_CHANNEL, PgNotifier};
    use std::time::Duration;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(2).await;
    let ours = PgNotifier::new(pool.clone());
    assert_eq!(ours.channel(), EVENTS_CHANNEL);
    let theirs = PgNotifier::with_channel(pool.clone(), "wa_server_events_of_another");
    let tenant = TenantId::parse(&format!("live-channel-{}", support::unique())).unwrap();
    let mut subscription = ours.subscribe(&tenant).await.unwrap();
    let wakeup = Wakeup {
        tenant: tenant.clone(),
        sequence: 1,
    };
    // The first wake-up after listening (anything missed before it).
    let _ = tokio::time::timeout(Duration::from_millis(200), subscription.woken()).await;
    theirs.notify(&wakeup).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), subscription.woken())
            .await
            .is_err(),
        "a notification on another channel woke this deployment's stream"
    );
    ours.notify(&wakeup).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), subscription.woken())
            .await
            .is_ok()
    );
    drop(subscription);
    ours.close().await;
    let mut again = ours.subscribe(&tenant).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(200), again.woken()).await;
    ours.notify(&wakeup).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), again.woken())
            .await
            .is_ok(),
        "closed, then subscribed again: not listening"
    );
    ours.close().await;
    theirs.close().await;
}

/// The listening connection killed (an operator, a failover, a
/// connection limit): the notifier listens again on a new one, wakes every
/// subscription once it listens (what was sent meanwhile was lost to it),
/// not before, and a notification sent after that wake-up reaches the
/// subscription. A channel of the test's own, so only its session is
/// killed. Decisive: `listen`'s reconnect, and its wake-up after `LISTEN`
/// (woken before it, a notification sent at once could be lost).
#[tokio::test]
async fn live_postgres_a_killed_listener_listens_again() {
    use meta_whatsapp_server_core::notify::{EventNotifier, Wakeup};
    use meta_whatsapp_server_store_postgres::PgNotifier;
    use std::time::Duration;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(2).await;
    let channel = format!("wa_server_events_killed_{}", support::unique());
    let notifier = PgNotifier::with_channel(pool.clone(), &channel);
    let tenant = TenantId::parse(&format!("live-killed-{}", support::unique())).unwrap();
    let mut subscription = notifier.subscribe(&tenant).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(200), subscription.woken()).await;
    let killed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
         WHERE pid <> pg_backend_pid() AND query LIKE 'LISTEN %' AND query LIKE $1) AS t",
    )
    .bind(format!("%{channel}%"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(killed, 1, "the listening session");
    assert!(
        tokio::time::timeout(Duration::from_secs(10), subscription.woken())
            .await
            .is_ok(),
        "not woken once listening again"
    );
    let wakeup = Wakeup {
        tenant: tenant.clone(),
        sequence: 1,
    };
    notifier.notify(&wakeup).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), subscription.woken())
            .await
            .is_ok(),
        "a notification after it was lost"
    );
    notifier.close().await;
}

/// M2.5 live: 200 open streams (ten tenants of 20, half never read) at 100
/// events a second for 3 seconds, on a pool of ten: every read stream gets
/// every event of its tenant in order, and the data held for clients never
/// passes each stream's budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_two_hundred_streams_at_a_hundred_events_a_second_hold_bounded_memory() {
    use meta_whatsapp_server_core::backend::Backend as _;
    use meta_whatsapp_server_store_postgres::PgBackend;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(10).await;
    migrate(&pool).await.unwrap();
    let backend = PgBackend::new(pool);
    let load =
        conformance::live::many_streams_at_a_steady_rate_hold_bounded_memory(&backend, 200, 100, 3)
            .await;
    eprintln!("postgres, M2.5: {load:?}");
    assert_eq!(load.delivered, load.events * 10);
    assert!(load.peak_buffered_bytes <= load.streams * load.budget_per_stream);
    backend.close().await;
}
