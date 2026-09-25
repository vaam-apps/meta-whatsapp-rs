//! The service on a real Postgres. Skipped unless
//! `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set; `just test-live` sets
//! `META_WHATSAPP_RS_REQUIRE_LIVE=1`, which turns the skip into a failure.
//! Every test runs in a fresh schema.
//!
//! Acceptance test M1.7: parallel migrations of two instances succeed, and
//! two instances on one database deduplicate the same webhook (the log
//! capture on Postgres is `live_logs.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::meta::{EXAMPLE_PN, EXAMPLE_WABA, EXAMPLE_WAMID, bytes, example_text, text};
use common::{ALL_SCOPES, Call, Harness, Stores, TestDb};
use meta_whatsapp_rs::adapters::store::postgres::sqlx;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::{Claim, DedupGuard, WebhookPayload};
use meta_whatsapp_server::model::{AllowedTenants, Scope, TenantId};
use meta_whatsapp_server::store::events::NewEvent;
use meta_whatsapp_server::store::{
    EventStore, HOUSEKEEPING_LOCK, MIGRATION_LOCK, MIGRATIONS_TABLE, PgEventStore, PgStore, migrate,
};

/// An instance of the service on the test database: everything on
/// Postgres.
async fn harness(db: &TestDb) -> Harness {
    let pool = db.pool(10).await;
    migrate(&pool).await.unwrap();
    Harness::with(Stores::postgres(&pool))
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

#[tokio::test]
async fn live_postgres_event_store_passes_the_suite() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(5).await;
    migrate(&pool).await.unwrap();
    common::events_suite::run(&PgEventStore::new(pool.clone()), &PgStore::new(pool)).await;
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
    // Version 2 belongs to milestone M1b's migration.
    assert_eq!(service, [1, 3]);
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
/// The value of a metric series of `h`, 0 when absent.
fn metric(h: &Harness, series: &str) -> u64 {
    h.state
        .metrics()
        .render()
        .lines()
        .find_map(|line| line.strip_prefix(series)?.trim().parse().ok())
        .unwrap_or(0)
}

/// Rows of the outbox, whatever their tenant.
async fn outbox_rows(pool: &sqlx::PgPool) -> Vec<(Option<String>, String)> {
    sqlx::query_as("SELECT tenant_id, event_type FROM wa_server_events ORDER BY created_at")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// M1.7: two instances (two pools, two states, one database) deduplicate
/// the same webhook: delivered to both, in turn and at once, it is one
/// row, one inbox message, and the second instance counts it a duplicate
/// or answers `503` while the first holds its lease. Decisive: the dedup
/// lease in the shared key/value store.
#[tokio::test]
async fn live_postgres_two_instances_deduplicate_a_webhook() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let one = harness(&db).await;
    let two = harness(&db).await;
    one.tenant("tenant-a").await;
    one.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
        .await;
    let pool = db.pool(2).await;

    // In turn: the second is a duplicate for the dedup lease.
    let body = example_text();
    assert_eq!(one.webhook(&body).await.status, StatusCode::OK);
    assert_eq!(two.webhook(&body).await.status, StatusCode::OK);
    assert_eq!(
        metric(
            &two,
            "wa_server_webhook_duplicate_events_total{stage=\"dedup\"}"
        ),
        1,
        "the second instance saw the first one's claim"
    );
    assert!(two.outbox.inserts().is_empty(), "its sink never ran");
    assert_eq!(outbox_rows(&pool).await.len(), 1);
    // The marker is under the event's hashed key, never the message id.
    let markers: Vec<String> =
        sqlx::query_scalar("SELECT key FROM wa_kv WHERE namespace = 'wa.webhook.dedup'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(markers.len(), 1);
    assert!(!markers[0].contains("wamid"), "{markers:?}");
    assert_eq!(
        markers[0],
        meta_whatsapp_rs::webhooks::dedup::store_key(EXAMPLE_WAMID).key()
    );

    // At once, many times: still one row per event.
    for round in 0..5 {
        let body = bytes(&text(
            EXAMPLE_WABA,
            EXAMPLE_PN,
            &format!("wamid.race-{round}"),
        ));
        let (a, b) = tokio::join!(one.webhook(&body), two.webhook(&body));
        for status in [a.status, b.status] {
            assert!(
                status == StatusCode::OK || status == StatusCode::SERVICE_UNAVAILABLE,
                "{status}"
            );
        }
        assert!(a.status == StatusCode::OK || b.status == StatusCode::OK);
        // Meta retries whichever got 503: now a duplicate.
        for (reply, h) in [(a, &one), (b, &two)] {
            if reply.status == StatusCode::SERVICE_UNAVAILABLE {
                assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
            }
        }
    }
    let rows = outbox_rows(&pool).await;
    assert_eq!(rows.len(), 6, "{rows:?}");
    assert!(rows.iter().all(|(t, _)| t.as_deref() == Some("tenant-a")));
    let key = one.tenant_key("tenant-a", &[Scope::Events]).await;
    for h in [&one, &two] {
        let polled = h.call(Call::get("/v1/events").key(&key)).await;
        assert_eq!(polled.json()["data"].as_array().unwrap().len(), 6);
    }
    let messages: i64 = sqlx::query_scalar("SELECT count(*) FROM wa_messages")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(messages, 6, "one inbox message per event");
}

/// A claim held by one instance (its request still delivering) makes the
/// other answer `503`, so Meta retries later, and records nothing.
#[tokio::test]
async fn live_postgres_a_claim_held_by_another_instance_is_503() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let one = harness(&db).await;
    let two = harness(&db).await;
    one.tenant("tenant-a").await;
    one.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
        .await;
    let body = example_text();
    let [event] = WebhookPayload::from_slice(&body)
        .unwrap()
        .into_events()
        .try_into()
        .unwrap();
    let guard = DedupGuard::new(one.kv.clone());
    let Claim::Acquired(ticket) = guard.claim(&event).await.unwrap() else {
        panic!("claimed");
    };
    assert_eq!(
        two.webhook(&body).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(outbox_rows(&db.pool(1).await).await.is_empty());
    assert!(guard.complete(&ticket).await.unwrap());
    assert_eq!(two.webhook(&body).await.status, StatusCode::OK);
    assert!(
        outbox_rows(&db.pool(1).await).await.is_empty(),
        "the claim's holder marked it done: a duplicate"
    );
}

/// A message text holding U+0000 is received, stored and polled back
/// exactly (the outbox's `json` column; `jsonb` would refuse it and Meta
/// would retry the delivery for 7 days).
#[tokio::test]
async fn live_postgres_a_message_with_u0000_round_trips() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = harness(&db).await;
    h.tenant("tenant-a").await;
    h.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
        .await;
    let mut payload = text(EXAMPLE_WABA, EXAMPLE_PN, "wamid.nul");
    payload["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"] =
        serde_json::json!("before\u{0}after");
    assert_eq!(h.webhook(&bytes(&payload)).await.status, StatusCode::OK);
    let key = h.tenant_key("tenant-a", &[Scope::Events]).await;
    let polled = h.call(Call::get("/v1/events").key(&key)).await;
    assert_eq!(
        polled.json()["data"][0]["data"]["message"]["text"]["body"],
        "before\u{0}after"
    );
}

/// Inserts from several instances commit in sequence order: a poller
/// following `next_after` while they insert sees every event exactly once.
/// Decisive: the outbox insert's advisory lock (without it, a sequence
/// drawn before another's commit can commit after a poll moved past it).
#[tokio::test]
async fn live_postgres_polling_during_concurrent_inserts_misses_nothing() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(12).await;
    migrate(&pool).await.unwrap();
    let h = Harness::with(Stores::postgres(&pool));
    common::events_suite::bind(h.store.as_ref(), "tenant-a", "1").await;
    let key = h.tenant_key("tenant-a", &[Scope::Events]).await;
    let writers: Vec<_> = (0..8)
        .map(|w| {
            let store = PgEventStore::new(pool.clone());
            tokio::spawn(async move {
                let mut ids = Vec::new();
                for i in 0..40 {
                    let row = NewEvent {
                        id: format!("evt_w{w}_{i}_{}", common::unique()),
                        ..common::events_suite::row(Some("tenant-a"), "message_received", "1", None)
                    };
                    store.insert(&row).await.unwrap().unwrap();
                    ids.push(row.id);
                }
                ids
            })
        })
        .collect();
    let mut seen = Vec::new();
    let mut after: Option<i64> = None;
    let mut done = false;
    let mut idle_rounds = 0;
    while idle_rounds < 3 {
        let path = after.map_or_else(
            || "/v1/events?limit=7".to_owned(),
            |a| format!("/v1/events?limit=7&after={a}"),
        );
        let reply = h.call(Call::get(path).key(&key)).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        let body = reply.json();
        let page: Vec<String> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_owned())
            .collect();
        after = Some(body["next_after"].as_i64().unwrap());
        if page.is_empty() && done {
            idle_rounds += 1;
        }
        seen.extend(page);
        if !done && writers.iter().all(tokio::task::JoinHandle::is_finished) {
            done = true;
        }
    }
    let mut written = Vec::new();
    for writer in writers {
        written.extend(writer.await.unwrap());
    }
    let unique: std::collections::BTreeSet<&String> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "an event polled twice");
    let mut seen_sorted = seen.clone();
    seen_sorted.sort();
    written.sort();
    assert_eq!(seen_sorted, written, "an event was skipped");
}

/// An insert in flight on another replica holds back every later insert of
/// the same tenant until it commits: a poll in between sees neither, so
/// its cursor never moves past the one in flight. Another tenant's inserts
/// do not wait. Decisive: the stream's row, locked from the drawing of a
/// sequence to the commit (without it the later insert draws the same
/// sequence, or commits first and the poll moves past the earlier one).
#[tokio::test]
async fn live_postgres_an_insert_in_flight_is_never_skipped() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(6).await;
    migrate(&pool).await.unwrap();
    let h = Harness::with(Stores::postgres(&pool));
    common::events_suite::bind(h.store.as_ref(), "tenant-a", "1").await;
    common::events_suite::bind(h.store.as_ref(), "tenant-b", "2").await;
    let key = h.tenant_key("tenant-a", &[Scope::Events]).await;
    let events_after = |after: Option<i64>| {
        let path = after.map_or_else(
            || "/v1/events".to_owned(),
            |a| format!("/v1/events?after={a}"),
        );
        let h = &h;
        let key = key.clone();
        async move {
            let reply = h.call(Call::get(path).key(&key)).await;
            assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
            let body = reply.json();
            let ids: Vec<String> = body["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["id"].as_str().unwrap().to_owned())
                .collect();
            (ids, body["next_after"].as_i64().unwrap())
        }
    };
    // Another replica's insert, in flight: sequence drawn (the stream's
    // row locked), row written, not committed (what the store's insert
    // does, stopped before its commit).
    let first = common::events_suite::row(Some("tenant-a"), "message_received", "1", None);
    let mut in_flight = pool.begin().await.unwrap();
    let drawn: i64 = sqlx::query_scalar(
        "INSERT INTO wa_server_event_streams AS s (stream, last_sequence) \
         VALUES ('tenant-a', 1) \
         ON CONFLICT (stream) DO UPDATE SET last_sequence = s.last_sequence + 1 \
         RETURNING s.last_sequence",
    )
    .fetch_one(&mut *in_flight)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO wa_server_events \
         (tenant_id, sequence, id, phone_number_id, event_type, data, data_bytes) \
         VALUES ('tenant-a', $1, $2, '1', 'message_received', $3::json, $4)",
    )
    .bind(drawn)
    .bind(&first.id)
    .bind(&first.data)
    .bind(i32::try_from(first.data.len()).unwrap())
    .execute(&mut *in_flight)
    .await
    .unwrap();
    // A later insert of the same tenant through the store waits.
    let second = common::events_suite::row(Some("tenant-a"), "message_received", "1", None);
    let later = {
        let store = PgEventStore::new(pool.clone());
        let second = second.clone();
        tokio::spawn(async move { store.insert(&second).await })
    };
    // Another tenant's does not.
    let other = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        PgEventStore::new(pool.clone()).insert(&common::events_suite::row(
            Some("tenant-b"),
            "message_received",
            "2",
            None,
        )),
    )
    .await
    .expect("tenant-b's insert waited for tenant-a's")
    .unwrap();
    assert_eq!(other, Some(1));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!later.is_finished(), "the later insert did not wait");
    let (mut seen, after) = events_after(None).await;
    assert!(seen.is_empty(), "{seen:?}");
    in_flight.commit().await.unwrap();
    assert_eq!(later.await.unwrap().unwrap(), Some(drawn + 1));
    let (more, _) = events_after(Some(after)).await;
    seen.extend(more);
    assert_eq!(seen, [first.id, second.id], "an event was skipped");
}

/// Housekeeping runs on one replica at a time: while another session holds
/// its lock, a purge does nothing.
#[tokio::test]
async fn live_postgres_one_replica_purges_at_a_time() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(4).await;
    migrate(&pool).await.unwrap();
    let store = PgEventStore::new(pool.clone());
    store
        .insert(&common::events_suite::row(
            None,
            "message_received",
            "1",
            None,
        ))
        .await
        .unwrap();
    let mut held = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(HOUSEKEEPING_LOCK)
        .execute(&mut *held)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(store.purge(std::time::Duration::ZERO).await.unwrap(), None);
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(HOUSEKEEPING_LOCK)
        .execute(&mut *held)
        .await
        .unwrap();
    assert_eq!(
        store.purge(std::time::Duration::ZERO).await.unwrap(),
        Some(1)
    );
}

/// Fix #1 of the M1c review, on Postgres: a tenant deleted and created
/// again under the same id polls nothing of the deleted one's events
/// (`common::scenarios`), which were deleted with it (security review M4).
/// Decisive: the outbox's foreign key to the tenant.
#[tokio::test]
async fn live_postgres_a_recreated_tenant_polls_nothing_from_before() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = harness(&db).await;
    common::scenarios::a_recreated_tenant_polls_nothing_from_before(&h).await;
    let old: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT tenant_id FROM wa_server_events WHERE data::text LIKE '%wamid.OLD-A%'",
    )
    .fetch_all(&db.pool(1).await)
    .await
    .unwrap();
    assert!(old.is_empty(), "deleted with the tenant: {old:?}");
}

/// The race fix #1 leaves without the insert's re-check: an event routed to
/// a tenant whose number is unbound, and the tenant deleted and created
/// again under the same id, before the event reaches the outbox (all
/// between the routing and the insert), is recorded operator-only: the
/// new tenant never polls it. Decisive: the insert keeping the tenant only
/// while the binding it was routed by still names it (the foreign key
/// alone accepts the new tenant).
#[tokio::test]
async fn live_postgres_an_event_routed_before_its_tenant_was_recreated_is_nobodys() {
    use meta_whatsapp_rs::core::ids::WabaId;
    use meta_whatsapp_server::model::DeleteTenantOutcome;

    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = harness(&db).await;
    let tenant = h.tenant("tenant-a").await;
    h.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
        .await;
    let store = h.store.clone();
    h.outbox.before_next_insert(Box::new(move || {
        Box::pin(async move {
            assert!(store.unbind_waba(&WabaId::new(EXAMPLE_WABA)).await.unwrap());
            assert_eq!(
                store.delete_tenant(&tenant).await.unwrap(),
                DeleteTenantOutcome::Deleted
            );
            store.create_tenant(&tenant, "").await.unwrap().unwrap();
        })
    }));
    let body = bytes(&text(EXAMPLE_WABA, EXAMPLE_PN, "wamid.IN-FLIGHT"));
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    let rows = outbox_rows(&db.pool(1).await).await;
    assert_eq!(rows, [(None, "message_received".to_owned())]);
    let key = h.tenant_key("tenant-a", &[Scope::Events]).await;
    let polled = h.call(Call::get("/v1/events").key(&key)).await;
    assert_eq!(polled.json()["data"], serde_json::json!([]));

    // The same rule as the routing: a number the tenant now holds under
    // another WABA than the event names is a stale binding.
    h.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
        .await;
    let store = h.store.clone();
    let tenant = TenantId::parse("tenant-a").unwrap();
    h.outbox.before_next_insert(Box::new(move || {
        Box::pin(async move {
            assert!(store.unbind_waba(&WabaId::new(EXAMPLE_WABA)).await.unwrap());
            store
                .bind_waba(
                    &tenant,
                    &WabaId::new("102290129349999"),
                    &[meta_whatsapp_rs::core::ids::PhoneNumberId::new(EXAMPLE_PN)],
                )
                .await
                .unwrap();
        })
    }));
    let body = bytes(&text(EXAMPLE_WABA, EXAMPLE_PN, "wamid.MOVED"));
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    let rows = outbox_rows(&db.pool(1).await).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1], (None, "message_received".to_owned()));
    let polled = h.call(Call::get("/v1/events").key(&key)).await;
    assert_eq!(polled.json()["data"], serde_json::json!([]));
}
