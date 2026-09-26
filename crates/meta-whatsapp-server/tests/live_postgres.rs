//! The service on a real Postgres. Skipped unless
//! `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set; `just test-live` sets
//! `META_WHATSAPP_RS_REQUIRE_LIVE=1`, which turns the skip into a failure.
//! Every test runs in a fresh schema, which `common::TestDb` drops when
//! the test ends, a panicking one included.
//!
//! Acceptance test M1.7: parallel migrations of two instances succeed, and
//! two instances on one database deduplicate the same webhook (the log
//! capture on Postgres, every route and webhook, sends included, is
//! `live_logs.rs`). M1.4's idempotency on Postgres, and two replicas racing
//! for one key.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::meta::{
    EXAMPLE_PN, EXAMPLE_WABA, EXAMPLE_WAMID, bytes, dated, example_text, fixture,
    template_approved, text, with_ids,
};
use common::{ALL_SCOPES, Call, Harness, Stores, TestDb};
use meta_whatsapp_rs::adapters::store::postgres::sqlx;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::{Claim, DedupGuard, WebhookPayload};
use meta_whatsapp_server::model::{AllowedTenants, Scope, TenantId};
use meta_whatsapp_server::store::events::NewEvent;
use meta_whatsapp_server::store::{
    HOUSEKEEPING_LOCK, MIGRATION_LOCK, MIGRATIONS_TABLE, Outbox as EventStore, PgEventStore,
    PgStore, migrate,
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

#[tokio::test]
async fn live_postgres_backend_hands_out_the_same_data_on_every_call() {
    use meta_whatsapp_server::store::PgBackend;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(5).await;
    migrate(&pool).await.unwrap();
    common::backend_suite::run(&PgBackend::new(pool)).await;
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
    assert_eq!(service, [1, 2, 3]);
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
/// Decisive: the tenant's stream row, locked from the drawing of its
/// sequence to the commit (without it, a sequence drawn before another's
/// commit can commit after a poll moved past it).
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

/// Security review L3 end to end: with a WABA bound for 30 days, Meta's
/// delivery of a message dated 8 days ago (past the dedup lease's 7 days:
/// a replay) goes to nobody, one dated 6 days ago (a late retry) to the
/// tenant. Decisive: the replay window in the routing.
#[tokio::test]
async fn live_postgres_a_replay_older_than_the_dedup_window_is_nobodys() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = harness(&db).await;
    h.tenant("tenant-a").await;
    h.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
        .await;
    let pool = db.pool(1).await;
    sqlx::query("UPDATE wa_server_wabas SET attached_at = now() - interval '30 days'")
        .execute(&pool)
        .await
        .unwrap();
    let days_ago = |days: i64, wamid: &str| {
        bytes(&common::meta::dated(
            text(EXAMPLE_WABA, EXAMPLE_PN, wamid),
            common::meta::now() - days * 24 * 3600,
        ))
    };
    for body in [days_ago(8, "wamid.replayed"), days_ago(6, "wamid.late")] {
        assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    }
    let rows = outbox_rows(&pool).await;
    assert_eq!(
        rows,
        [
            (None, "message_received".to_owned()),
            (Some("tenant-a".to_owned()), "message_received".to_owned())
        ]
    );
}

/// Security review M2: the webhook path and the API share one pool of 10
/// connections. A burst of one tenant's deliveries waiting on its locked
/// outbox stream takes at most `MAX_DELIVERIES_RECORDING` of them, so an
/// API call (its key lookup first) is answered at once; and a delivery
/// waits at most 2 s for the lock (`lock_timeout`), then `503` (Meta
/// retries). Decisive: the recording turns and the lock timeout.
#[tokio::test]
async fn live_postgres_a_busy_outbox_never_starves_the_api() {
    use std::time::Duration;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = harness(&db).await;
    h.tenant("tenant-a").await;
    h.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
        .await;
    let key = h.tenant_key("tenant-a", &[Scope::Numbers]).await;
    let first = bytes(&text(EXAMPLE_WABA, EXAMPLE_PN, "wamid.first"));
    assert_eq!(h.webhook(&first).await.status, StatusCode::OK);
    // Another session holds tenant-a's stream, for longer than the
    // deliveries may wait.
    let holder_pool = db.pool(1).await;
    let mut holder = holder_pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM wa_server_event_streams WHERE stream = 'tenant-a' FOR UPDATE")
        .execute(&mut *holder)
        .await
        .unwrap();
    // Held until the burst is answered (at most 30 s: without the lock
    // timeout, the burst would wait for it and go through).
    let (release, released_now) = tokio::sync::oneshot::channel::<()>();
    let released = tokio::spawn(async move {
        let _ = tokio::time::timeout(Duration::from_secs(30), released_now).await;
        holder.rollback().await.unwrap();
    });
    let bodies: Vec<Vec<u8>> = (0..12)
        .map(|i| bytes(&text(EXAMPLE_WABA, EXAMPLE_PN, &format!("wamid.burst-{i}"))))
        .collect();
    let burst = futures::future::join_all(bodies.iter().map(|body| h.webhook(body)));
    let api = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let started = std::time::Instant::now();
        let reply = h.call(Call::get("/v1/numbers").key(&key)).await;
        (reply.status, started.elapsed())
    };
    let (replies, (status, took)) = tokio::join!(burst, api);
    release.send(()).unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(took < Duration::from_secs(1), "the API waited {took:?}");
    let statuses: Vec<StatusCode> = replies.iter().map(|r| r.status).collect();
    assert!(
        statuses
            .iter()
            .all(|s| *s == StatusCode::SERVICE_UNAVAILABLE),
        "{statuses:?}"
    );
    assert_eq!(
        metric(&h, "wa_server_webhook_deliveries_total{outcome=\"busy\"}"),
        12
    );
    released.await.unwrap();
    // Meta's retries go through once the stream is free.
    for body in &bodies {
        assert_eq!(h.webhook(body).await.status, StatusCode::OK);
    }
    assert_eq!(outbox_rows(&db.pool(1).await).await.len(), 13);
}

/// Keyless events are deduplicated within an hour only, on Postgres
/// (`common::scenarios`): the window is the pipeline's clock on both ends,
/// never the database's.
#[tokio::test]
async fn live_postgres_keyless_events_are_deduplicated_within_the_window_only() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = harness(&db).await;
    common::scenarios::keyless_events_are_deduplicated_within_the_window_only(&h).await;
}

/// What the sink handed the outbox for its only insert: the tenant it
/// routed the event to.
fn routed_to(h: &Harness) -> Option<String> {
    let [(row, _)] = h.outbox.inserts().try_into().unwrap();
    row.tenant.map(|t| t.as_str().to_owned())
}

/// The insert's re-check keeps the tenant only while the binding names
/// *it*: an event routed to tenant-a whose WABA and number move to
/// tenant-b before the insert is operator-only, not tenant-b's. For an
/// event naming a number, then one naming only its WABA. Decisive: the
/// re-check's `n.tenant_id = $3` (then `w.tenant_id = $3`).
#[tokio::test]
async fn live_postgres_an_event_whose_binding_moved_before_its_insert_is_nobodys() {
    use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
    let Some(db) = TestDb::new().await else {
        return;
    };
    for names_a_number in [true, false] {
        let h = harness(&db).await;
        sqlx::query("DELETE FROM wa_server_events")
            .execute(&db.pool(1).await)
            .await
            .unwrap();
        for id in ["tenant-a", "tenant-b"] {
            let _ = h
                .store
                .create_tenant(&TenantId::parse(id).unwrap(), "")
                .await
                .unwrap();
        }
        let _ = h
            .store
            .unbind_waba(&WabaId::new(EXAMPLE_WABA))
            .await
            .unwrap();
        h.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
            .await;
        // Dated now, after the binding began.
        let (body, numbers) = if names_a_number {
            (
                text(EXAMPLE_WABA, EXAMPLE_PN, "wamid.MOVED-TO-B"),
                vec![PhoneNumberId::new(EXAMPLE_PN)],
            )
        } else {
            (template_approved(EXAMPLE_WABA), vec![])
        };
        let store = h.store.clone();
        let b = TenantId::parse("tenant-b").unwrap();
        h.outbox.before_next_insert(Box::new(move || {
            Box::pin(async move {
                assert!(store.unbind_waba(&WabaId::new(EXAMPLE_WABA)).await.unwrap());
                store
                    .bind_waba(&b, &WabaId::new(EXAMPLE_WABA), &numbers)
                    .await
                    .unwrap();
            })
        }));
        assert_eq!(h.webhook(&bytes(&body)).await.status, StatusCode::OK);
        assert_eq!(routed_to(&h).as_deref(), Some("tenant-a"), "{body}");
        let rows = outbox_rows(&db.pool(1).await).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, None, "tenant-b got tenant-a's event: {body}");
        let key = h.tenant_key("tenant-b", &[Scope::Events]).await;
        let polled = h.call(Call::get("/v1/events").key(&key)).await;
        assert_eq!(polled.json()["data"], serde_json::json!([]));
    }
}

/// An insert in flight (waiting for its stream's row) holds the binding it
/// was routed by: an unbinding waits for its commit, so it cannot slip a
/// tenant's deletion and re-creation in between. For an event naming a
/// number (its row locked), then one naming only a WABA (the WABA's row).
/// Decisive: `FOR KEY SHARE` in each branch of the re-check.
#[tokio::test]
async fn live_postgres_an_unbinding_waits_for_an_insert_in_flight() {
    use meta_whatsapp_rs::core::ids::WabaId;
    use std::time::Duration;
    let Some(db) = TestDb::new().await else {
        return;
    };
    for names_a_number in [true, false] {
        let h = harness(&db).await;
        sqlx::query("DELETE FROM wa_server_events")
            .execute(&db.pool(1).await)
            .await
            .unwrap();
        let _ = h
            .store
            .create_tenant(&TenantId::parse("tenant-a").unwrap(), "")
            .await
            .unwrap();
        h.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
            .await;
        // Dated now, after the binding began.
        let body = if names_a_number {
            text(EXAMPLE_WABA, EXAMPLE_PN, "wamid.in-flight")
        } else {
            template_approved(EXAMPLE_WABA)
        };
        let delivered = bytes(&body);
        // The stream's row exists: another session holds it.
        sqlx::query(
            "INSERT INTO wa_server_event_streams (stream, last_sequence) \
             VALUES ('tenant-a', 0) ON CONFLICT (stream) DO NOTHING",
        )
        .execute(&db.pool(1).await)
        .await
        .unwrap();
        let holder_pool = db.pool(1).await;
        let mut holder = holder_pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM wa_server_event_streams WHERE stream = 'tenant-a' FOR UPDATE")
            .execute(&mut *holder)
            .await
            .unwrap();
        let store = h.store.clone();
        let admin = async move {
            // The insert reaches the stream's row and waits there.
            tokio::time::sleep(Duration::from_millis(500)).await;
            let unbind =
                tokio::spawn(async move { store.unbind_waba(&WabaId::new(EXAMPLE_WABA)).await });
            tokio::time::sleep(Duration::from_millis(500)).await;
            let waited = !unbind.is_finished();
            holder.rollback().await.unwrap();
            (waited, unbind.await.unwrap().unwrap())
        };
        let (reply, (waited, unbound)) = tokio::join!(h.webhook(&delivered), admin);
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        assert!(unbound);
        assert!(waited, "the unbinding did not wait for the insert: {body}");
        let rows = outbox_rows(&db.pool(1).await).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0.as_deref(), Some("tenant-a"), "{body}");
    }
}

/// The race the tenant re-check alone leaves open: an event routed to
/// tenant-a (bound for an hour, the event dated a minute ago), then, before
/// the insert, the WABA unbound, tenant-a deleted, created again and bound
/// to the same WABA and number. The re-check finds a binding naming
/// "tenant-a", but one that began after Meta dated the event: operator-only,
/// the new tenant polls nothing. For an event naming a number, then one
/// naming only its WABA. An event Meta did not date (an error) cannot be
/// told apart, and goes to the new tenant: the documented limit. Decisive:
/// the insert's `attached_at` re-check in each branch.
#[tokio::test]
async fn live_postgres_an_event_dated_before_its_tenant_was_bound_again_is_nobodys() {
    use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
    use meta_whatsapp_server::model::DeleteTenantOutcome;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let a_minute_ago = common::meta::now() - 60;
    let error = with_ids(fixture("messages/errors.json"), EXAMPLE_WABA, EXAMPLE_PN);
    for (body, dated_event) in [
        (
            dated(
                text(EXAMPLE_WABA, EXAMPLE_PN, "wamid.OLD-TENANT"),
                a_minute_ago,
            ),
            true,
        ),
        (dated(template_approved(EXAMPLE_WABA), a_minute_ago), true),
        (error, false),
    ] {
        let h = harness(&db).await;
        sqlx::query("DELETE FROM wa_server_events")
            .execute(&db.pool(1).await)
            .await
            .unwrap();
        let tenant = TenantId::parse("tenant-a").unwrap();
        let _ = h.store.create_tenant(&tenant, "").await.unwrap();
        h.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
            .await;
        // The old tenant has held the WABA for an hour.
        sqlx::query("UPDATE wa_server_wabas SET attached_at = now() - interval '1 hour'")
            .execute(&db.pool(1).await)
            .await
            .unwrap();
        let store = h.store.clone();
        h.outbox.before_next_insert(Box::new(move || {
            Box::pin(async move {
                assert!(store.unbind_waba(&WabaId::new(EXAMPLE_WABA)).await.unwrap());
                assert_eq!(
                    store.delete_tenant(&tenant).await.unwrap(),
                    DeleteTenantOutcome::Deleted
                );
                store.create_tenant(&tenant, "").await.unwrap().unwrap();
                store
                    .bind_waba(
                        &tenant,
                        &WabaId::new(EXAMPLE_WABA),
                        &[PhoneNumberId::new(EXAMPLE_PN)],
                    )
                    .await
                    .unwrap();
            })
        }));
        assert_eq!(h.webhook(&bytes(&body)).await.status, StatusCode::OK);
        assert_eq!(routed_to(&h).as_deref(), Some("tenant-a"), "{body}");
        let rows = outbox_rows(&db.pool(1).await).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        let key = h.tenant_key("tenant-a", &[Scope::Events]).await;
        let polled = h.call(Call::get("/v1/events").key(&key)).await.json()["data"].clone();
        if dated_event {
            assert_eq!(
                rows[0].0, None,
                "the new tenant-a got the old one's event: {polled}"
            );
            assert_eq!(polled, serde_json::json!([]));
        } else {
            // Undated: the re-check has only the tenant's id to go by.
            assert_eq!(rows[0].0.as_deref(), Some("tenant-a"));
            assert_eq!(polled.as_array().unwrap().len(), 1);
        }
    }
}

/// The insert's `attached_at` re-check agrees with the routing to the
/// second: an event Meta dated in the very second its WABA's binding began
/// (half a second in) is the tenant's, through both. For an event naming a
/// number, then one naming only its WABA. Decisive: the re-check's bound
/// (`attached_at < to_timestamp(t + 1)`, not `<= to_timestamp(t)`).
#[tokio::test]
async fn live_postgres_an_event_dated_in_its_binding_s_first_second_is_the_tenant_s() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let second = common::meta::now() - 600;
    for body in [
        dated(text(EXAMPLE_WABA, EXAMPLE_PN, "wamid.SAME-SECOND"), second),
        dated(template_approved(EXAMPLE_WABA), second),
    ] {
        let h = harness(&db).await;
        sqlx::query("DELETE FROM wa_server_events")
            .execute(&db.pool(1).await)
            .await
            .unwrap();
        let _ = h
            .store
            .create_tenant(&TenantId::parse("tenant-a").unwrap(), "")
            .await
            .unwrap();
        h.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
            .await;
        sqlx::query(
            "UPDATE wa_server_wabas \
             SET attached_at = to_timestamp($1::bigint) + interval '0.5 second'",
        )
        .bind(second)
        .execute(&db.pool(1).await)
        .await
        .unwrap();
        assert_eq!(h.webhook(&bytes(&body)).await.status, StatusCode::OK);
        assert_eq!(routed_to(&h).as_deref(), Some("tenant-a"), "{body}");
        let rows = outbox_rows(&db.pool(1).await).await;
        assert_eq!(
            rows,
            [(Some("tenant-a".to_owned()), rows[0].1.clone())],
            "{body}"
        );
    }
}

/// The insert's `attached_at` re-check agrees with the routing on the
/// other side of the second too: an event routed to tenant-a (bound for an
/// hour), then, before the insert, its WABA bound again (tenant-a deleted
/// and created again) from the second after Meta dated it, is nobody's:
/// that binding began after the event, as the routing counts it. For an
/// event naming a number, then one naming only its WABA. Decisive: the
/// re-check's bound is `to_timestamp(t + 1)` in each branch, no later.
#[tokio::test]
async fn live_postgres_an_event_dated_the_second_before_its_binding_is_nobodys() {
    use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
    use meta_whatsapp_server::model::DeleteTenantOutcome;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let second = common::meta::now() - 600;
    for body in [
        dated(
            text(EXAMPLE_WABA, EXAMPLE_PN, "wamid.SECOND-BEFORE"),
            second,
        ),
        dated(template_approved(EXAMPLE_WABA), second),
    ] {
        let h = harness(&db).await;
        let pool = db.pool(1).await;
        sqlx::query("DELETE FROM wa_server_events")
            .execute(&pool)
            .await
            .unwrap();
        let tenant = TenantId::parse("tenant-a").unwrap();
        let _ = h.store.create_tenant(&tenant, "").await.unwrap();
        h.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
            .await;
        // The old tenant has held the WABA for an hour: routed to it.
        sqlx::query("UPDATE wa_server_wabas SET attached_at = now() - interval '1 hour'")
            .execute(&pool)
            .await
            .unwrap();
        let store = h.store.clone();
        let bound_again = pool.clone();
        h.outbox.before_next_insert(Box::new(move || {
            Box::pin(async move {
                assert!(store.unbind_waba(&WabaId::new(EXAMPLE_WABA)).await.unwrap());
                assert_eq!(
                    store.delete_tenant(&tenant).await.unwrap(),
                    DeleteTenantOutcome::Deleted
                );
                store.create_tenant(&tenant, "").await.unwrap().unwrap();
                store
                    .bind_waba(
                        &tenant,
                        &WabaId::new(EXAMPLE_WABA),
                        &[PhoneNumberId::new(EXAMPLE_PN)],
                    )
                    .await
                    .unwrap();
                // Bound again from the second after the event's.
                sqlx::query(
                    "UPDATE wa_server_wabas SET attached_at = to_timestamp($1::bigint + 1)",
                )
                .bind(second)
                .execute(&bound_again)
                .await
                .unwrap();
            })
        }));
        assert_eq!(h.webhook(&bytes(&body)).await.status, StatusCode::OK);
        assert_eq!(routed_to(&h).as_deref(), Some("tenant-a"), "{body}");
        let rows = outbox_rows(&pool).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, None, "the new tenant-a got the event: {body}");
    }
}

/// M1.4 on Postgres: a timeout is kept and replayed without a second
/// request; a 131047 releases the key, which then sends.
#[tokio::test]
async fn live_postgres_a_timeout_is_replayed_and_a_131047_releases_its_key() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = harness(&db).await;
    h.tenant("merchant-42").await;
    h.connect(
        "merchant-42",
        "102290129340398",
        &["106540352242922"],
        "TOKEN",
    )
    .await;
    let key = h
        .tenant_key("merchant-42", &[meta_whatsapp_server::model::Scope::Send])
        .await;
    let send = |idempotency_key: &str| {
        Call::new(
            meta_whatsapp_rs::webhooks::axum::http::Method::POST,
            "/v1/numbers/106540352242922/messages",
        )
        .key(&key)
        .header("idempotency-key", idempotency_key)
        .json(
            &serde_json::json!({"to": {"phone": "+16505551234"}, "type": "text",
                                  "text": {"body": "Your order has shipped."}}),
        )
    };
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    let first = h.call(send("k-timeout")).await;
    assert_eq!(first.status, StatusCode::GATEWAY_TIMEOUT, "{}", first.text);
    assert_eq!(first.json()["error"]["may_have_been_sent"], true);
    let again = h.call(send("k-timeout")).await;
    assert_eq!(again.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(h.graph.requests().len(), 1);
    h.graph.push_json(
        400,
        serde_json::json!({"error": {"message": "x", "type": "OAuthException", "code": 131047}}),
    );
    let closed = h.call(send("k-window")).await;
    assert_eq!(closed.status, StatusCode::CONFLICT, "{}", closed.text);
    h.graph.push_json(
        200,
        serde_json::json!({"messaging_product": "whatsapp", "messages": [{"id": "wamid.X"}]}),
    );
    let accepted = h.call(send("k-window")).await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{}", accepted.text);
    assert_eq!(h.graph.requests().len(), 3);
    assert_eq!(h.graph.remaining(), 0);
}

/// Two replicas racing for one key: exactly one claims it. Decisive: the
/// unique (tenant, key) row and the conditional insert.
#[tokio::test]
async fn live_postgres_one_of_two_racing_claims_wins() {
    use meta_whatsapp_server::model::{IdempotencyClaim, IdempotencyKey};
    use meta_whatsapp_server::store::{IdempotencyRecords as _, RecordStore as _};
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
    lock: &meta_whatsapp_server::store::PgLeaderLock,
    name: &str,
) -> meta_whatsapp_server::store::LeaderTurn {
    use meta_whatsapp_server::store::LeaderLock as _;
    for _ in 0..500 {
        if let Some(turn) = lock.try_exclusive(name).await.unwrap() {
            return turn;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("another holder kept {name} for 5 s");
}

/// The leader lock on Postgres: one turn per name, whichever replica asks,
/// until it is released or dropped; its housekeeping turn is the lock the
/// purges take, so an outbox purge skips while it is held. The turns'
/// rules run on a name of this test's own (no other test takes it, so
/// every answer is certain); the housekeeping turn is held no longer than
/// it takes to show a purge skipping. Decisive: the transaction-scoped
/// advisory lock in `PgLeaderLock::try_exclusive` (a session lock would
/// outlive its turn), and `lock_key`'s derivation.
#[tokio::test]
async fn live_postgres_the_leader_lock_gives_one_turn_at_a_time() {
    use meta_whatsapp_server::store::{HOUSEKEEPING, LeaderLock as _, PgLeaderLock};
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
    let name = format!("test-{}", common::unique());
    let turn = a
        .try_exclusive(&name)
        .await
        .unwrap()
        .expect("nobody holds it");
    assert!(b.try_exclusive(&name).await.unwrap().is_none(), "one turn");
    assert!(
        a.try_exclusive(&name).await.unwrap().is_none(),
        "one turn, on the same replica too"
    );
    turn.release().await.unwrap();
    let again = b.try_exclusive(&name).await.unwrap();
    assert!(again.is_some(), "released");
    drop(again);
    // Dropped: its transaction is rolled back as its connection goes back
    // to the pool, which ends the turn.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(turn) = a.try_exclusive(&name).await.unwrap() {
            turn.release().await.unwrap();
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a dropped turn held on"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // The housekeeping turn is the purges' lock; other names are not.
    let housekeeping = turn_of(&a, HOUSEKEEPING).await;
    assert!(b.try_exclusive(HOUSEKEEPING).await.unwrap().is_none());
    let purged = PgEventStore::new(pool.clone())
        .purge(std::time::Duration::ZERO)
        .await
        .unwrap();
    let other = b.try_exclusive(&name).await.unwrap();
    housekeeping.release().await.unwrap();
    assert_eq!(purged, None, "the purges take the housekeeping lock");
    assert!(other.is_some(), "each name its own lock");
}

/// Housekeeping on the Postgres backend sweeps the library's expired
/// key/value rows under the housekeeping turn it takes right after its
/// outbox purge released the same advisory lock (the purge neither blocks
/// nor starves the sweep), and leaves a live row. Other tests' purges take
/// that lock too: a round that finds it taken skips, and a later one
/// sweeps. Decisive: the sweep in `serve::housekeeping`,
/// `PgBackend::janitor`, and `PgJanitor::purge_expired`.
#[tokio::test]
async fn live_postgres_housekeeping_sweeps_expired_key_value_rows() {
    use meta_whatsapp_server::serve::{Sweep, housekeeping};
    use meta_whatsapp_server::store::{Backend as _, PgBackend};
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
    let backend = PgBackend::new(pool.clone());
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(housekeeping(
        backend.outbox(),
        backend.idempotency(),
        Some(Sweep {
            leader: backend.leader_lock(),
            janitor: backend.janitor(),
        }),
        meta_whatsapp_server::events::DEFAULT_OUTBOX_RETENTION,
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
