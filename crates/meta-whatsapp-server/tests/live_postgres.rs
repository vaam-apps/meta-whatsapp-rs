//! The service on a real Postgres. Skipped unless
//! `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set; `just test-live` sets
//! `META_WHATSAPP_RS_REQUIRE_LIVE=1`, which turns the skip into a failure.
//! Every test runs in a fresh schema, which `common::TestDb` drops when
//! the test ends, a panicking one included.
//!
//! Acceptance test M1.7: two instances on one database deduplicate the
//! same webhook (the log capture on Postgres, every route and webhook,
//! sends included, is `live_logs.rs`). M1.4's idempotency on Postgres, and
//! the routing races over the whole service. Since roadmap S7 the
//! backend's own live tests (the core's conformance suite, parallel
//! migrations, locks, purges, a deadlocked insert) are the
//! store-postgres crate's
//! (`crates/meta-whatsapp-server-store-postgres/tests/live_postgres.rs`);
//! the migration continuity of the split is `binary.rs`'s.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::meta::{
    EXAMPLE_PN, EXAMPLE_WABA, EXAMPLE_WAMID, bytes, dated, example_text, fixture,
    template_approved, text, with_ids,
};
use common::{ALL_SCOPES, Call, Harness, Stores, TestDb};
use meta_whatsapp_rs::adapters::store::postgres::sqlx;
use meta_whatsapp_rs::webhooks::axum::body::Body;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_rs::webhooks::{Claim, DedupGuard, WebhookPayload};
use meta_whatsapp_server::model::{AllowedTenants, Scope, TenantId};
use meta_whatsapp_server::store::events::NewEvent;
use meta_whatsapp_server::store::{Outbox as EventStore, PgEventStore, PgStore, migrate};
use meta_whatsapp_server_core::conformance::outbox as outbox_suite;
use serde_json::json;

/// An instance of the service on the test database: everything on
/// Postgres.
async fn harness(db: &TestDb) -> Harness {
    let pool = db.pool(10).await;
    migrate(&pool).await.unwrap();
    Harness::with(Stores::postgres(&pool))
}

/// Acceptance test M2.1 on Postgres (roadmap M2a): an inbound webhook,
/// the conversation list, the history, a reply within the window recorded
/// `accepted`, which a status webhook moves to `delivered`; a free-form
/// reply outside the window `409` with zero requests
/// (`common::inbox::m2_1`).
/// M3.5's decisive case on Postgres (roadmap M3c): the tenants' OTP
/// settings in `wa_server_tenants.settings`, the library's codes in the
/// Postgres key/value store; tenant A's code verifies at no other tenant
/// on the same number.
#[tokio::test]
async fn live_postgres_m3_5_a_code_verifies_at_no_other_tenant_on_the_same_number() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(10).await;
    migrate(&pool).await.unwrap();
    let h = Harness::with_stores_and_settings(
        Stores::postgres(&pool),
        meta_whatsapp_server::state::Settings {
            otp: Some(common::otp_deployment(Some(common::otp::PLATFORM))),
            ..common::test_settings()
        },
    );
    common::otp::a_code_verifies_at_no_other_tenant_on_the_same_number(&h).await;
    // Stored as the layout pins it, under the tenant's `settings`.
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT settings -> 'otp' FROM wa_server_tenants WHERE id = 'platform'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored["v"], 1);
    assert_eq!(stored["phone_number_id"], "106540352242922");
    // No code and no phone number in any row the service or the library
    // keeps: the codes are keyed hashes.
    let dump: String = sqlx::query_scalar(
        "SELECT coalesce(string_agg(t::text, ' '), '') FROM ( \
           SELECT row_to_json(k)::text AS t FROM wa_kv k \
           UNION ALL SELECT row_to_json(i)::text FROM wa_server_idempotency i \
           UNION ALL SELECT row_to_json(s)::text FROM wa_server_tenants s) rows",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!dump.contains("6505551234"), "a phone number is stored");
}

/// Roadmap M3e's decisive case on Postgres (`common::token_expiry`): the
/// task's turn is Postgres' advisory lock, the event's key the outbox's
/// unique index.
#[tokio::test]
async fn live_postgres_a_token_inside_the_lead_is_told_to_its_tenant_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    common::token_expiry::a_token_inside_the_lead_is_told_to_its_tenant_once(&harness(&db).await)
        .await;
}

#[tokio::test]
async fn live_postgres_m2_1_an_inbound_message_is_listed_read_and_answered() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    common::inbox::m2_1(&harness(&db).await).await;
}

/// Roadmap M2e's decisive case on Postgres: the media id received by
/// another tenant's number (`wa_server_events.media_id`, migration 7) is
/// `404` with zero requests; the receiving tenant downloads it without
/// `phone_number_id` (`common::media`).
#[tokio::test]
async fn live_postgres_another_tenants_received_media_id_is_not_found_with_no_request() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    common::media::another_tenants_received_media_id_is_not_found_with_no_request(
        &harness(&db).await,
    )
    .await;
}

/// Rows of every table of the test's schema whose JSON rendering holds
/// one of `needles`, as text or (a `BYTEA` column renders as hex) as the
/// hex of its UTF-8 bytes, by table: `(table, rows)`, the tables with
/// none left out. Every table, the library's and the service's: one added
/// later is swept too (as the library's live erasure test sweeps its own).
/// Every row a tenant may see: of `wa_server_events`, the rows with a
/// tenant (an operator-only row, `tenant_id` `NULL`, is the operator's,
/// which an erasure leaves: design D89; [`operator_rows_mentioning`]).
async fn rows_mentioning(db: &TestDb, pool: &sqlx::PgPool, needles: &[&str]) -> Vec<(String, i64)> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables \
         WHERE table_schema = $1 AND table_type = 'BASE TABLE' ORDER BY 1",
    )
    .bind(&db.schema)
    .fetch_all(pool)
    .await
    .unwrap();
    assert!(
        tables.iter().any(|t| t == "wa_server_events") && tables.iter().any(|t| t == "wa_messages"),
        "{tables:?}"
    );
    let mut found = Vec::new();
    for table in tables {
        let mut total = 0;
        let tenants_only = if table == "wa_server_events" {
            "t.tenant_id IS NOT NULL AND"
        } else {
            ""
        };
        for needle in needles {
            let hex = hex::encode(needle.as_bytes());
            let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM \"{table}\" t \
                 WHERE {tenants_only} (position($1 IN row_to_json(t)::text) > 0 \
                    OR position($2 IN row_to_json(t)::text) > 0)"
            )))
            .bind(*needle)
            .bind(hex)
            .fetch_one(pool)
            .await
            .unwrap();
            total += n;
        }
        if total > 0 {
            found.push((table, total));
        }
    }
    found
}

/// The operator-only rows of `wa_server_events` (`tenant_id` `NULL`)
/// whose JSON rendering holds one of `needles`.
async fn operator_rows_mentioning(pool: &sqlx::PgPool, needles: &[&str]) -> i64 {
    let mut total = 0;
    for needle in needles {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM wa_server_events t \
             WHERE t.tenant_id IS NULL AND position($1 IN row_to_json(t)::text) > 0",
        )
        .bind(*needle)
        .fetch_one(pool)
        .await
        .unwrap();
        total += n;
    }
    total
}

/// Roadmap M2f's decisive test on Postgres: after the erasure of a
/// customer by their BSUID alone (`common::erasure`), no row a tenant may
/// see of any table mentions them (their phone number, BSUID, parent
/// BSUID, username or name), the inbox's tables and `wa_server_events`
/// included; before it, both did. The number's operator-only row naming
/// them (a message Meta dated before the binding) stays: the operator's (design D89, narrow by
/// default). Decisive: the outbox's erasure (`wa_server_events` keeps
/// their rows without it), the closure over `identities` (the
/// phone-keyed thread stays), the inbox's erasure, and the tenant clause
/// of `ERASE_SQL` (an erasure reaching operator-only rows deletes that
/// one).
#[tokio::test]
async fn live_postgres_after_an_erasure_no_row_of_any_table_mentions_the_customer() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(10).await;
    migrate(&pool).await.unwrap();
    let h = Harness::with(Stores::postgres(&pool));
    let needles = common::erasure::needles();
    // Before: the customer is in the inbox and in the outbox.
    {
        let (_, _) = common::erasure::merchant(&h).await;
        common::erasure::the_customer_writes(&h).await;
        let before = rows_mentioning(&db, &pool, &needles).await;
        let tables: Vec<&str> = before.iter().map(|(t, _)| t.as_str()).collect();
        for table in ["wa_server_events", "wa_messages", "wa_conversations"] {
            assert!(tables.contains(&table), "{table} before: {before:?}");
        }
    }
    let db2 = TestDb::new().await.unwrap();
    let pool2 = db2.pool(10).await;
    migrate(&pool2).await.unwrap();
    let h2 = Harness::with(Stores::postgres(&pool2));
    common::erasure::an_erasure_by_bsuid_leaves_nothing_of_the_customer(&h2).await;
    assert_eq!(
        rows_mentioning(&db2, &pool2, &needles).await,
        Vec::<(String, i64)>::new(),
        "rows mention the customer after the erasure"
    );
    assert_eq!(
        operator_rows_mentioning(&pool2, &[common::meta::EXAMPLE_WA_ID]).await,
        1,
        "the operator-only row was erased"
    );
    // The other customer's rows stay.
    let other = rows_mentioning(&db2, &pool2, &[common::erasure::OTHER]).await;
    let tables: Vec<&str> = other.iter().map(|(t, _)| t.as_str()).collect();
    for table in ["wa_server_events", "wa_messages", "wa_conversations"] {
        assert!(tables.contains(&table), "{table}: {other:?}");
    }
    drop(h);
}

/// Roadmap M2f on Postgres (design D90, narrow by default): on a number
/// moved from another tenant, the erasure is `409 erasure_crosses_binding`
/// and erases nothing (`common::erasure`).
#[tokio::test]
async fn live_postgres_an_erasure_on_a_moved_number_erases_nothing() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    common::erasure::an_erasure_on_a_moved_number_erases_nothing(&harness(&db).await).await;
}

/// Roadmap M2f on Postgres: another tenant's key cannot erase on the
/// number, and an erasure stays on its number and tenant
/// (`common::erasure`).
#[tokio::test]
async fn live_postgres_an_erasure_stays_on_its_number_and_tenant() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    common::erasure::an_erasure_stays_on_its_number_and_tenant(&harness(&db).await).await;
}

/// The binding epoch on Postgres (roadmap M2a, decision D10): a moved
/// number's history is hidden from its new tenant, and kept.
#[tokio::test]
async fn live_postgres_a_moved_numbers_history_is_hidden_from_its_new_tenant() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    common::inbox::a_moved_numbers_history_is_hidden_from_its_new_tenant(&harness(&db).await).await;
}

/// Roadmap M2g on Postgres, decisive: a message of the number's previous
/// holder is `404` by id to the new holder, its window is not shown, and
/// no receipt goes for it (`common::inbox`).
#[tokio::test]
async fn live_postgres_the_previous_holders_message_window_and_receipt_never_reach_the_new_holder()
{
    let Some(db) = TestDb::new().await else {
        return;
    };
    common::inbox::the_previous_holders_message_window_and_receipt_never_reach_the_new_holder(
        &harness(&db).await,
    )
    .await;
}

/// Roadmap M2g on Postgres, decisive: after the read route, `unread` is 0,
/// and Meta got one read receipt for the latest inbound message only with
/// `notify_customer`; its key replays (`common::inbox`).
#[tokio::test]
async fn live_postgres_the_read_route_resets_unread_and_receipts_the_latest_inbound_message() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    common::inbox::the_read_route_resets_unread_and_receipts_the_latest_inbound_message(
        &harness(&db).await,
    )
    .await;
}

/// Roadmap M2h on Postgres: a number migrated between two tenants' WABAs,
/// neither WABA moving, shows its new tenant none of the previous one's
/// history, and keeps it.
#[tokio::test]
async fn live_postgres_a_number_migrated_between_two_tenants_wabas_hides_its_history() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    common::inbox::a_number_migrated_between_two_tenants_wabas_hides_its_history(
        &harness(&db).await,
    )
    .await;
}

/// [`harness`] on a pool whose sessions are named `app`, for
/// [`until_waiting`].
async fn named_harness(db: &TestDb, app: &str) -> Harness {
    let pool = db.named_pool(10, app).await;
    migrate(&pool).await.unwrap();
    Harness::with(Stores::postgres(&pool))
}

/// Wait until `n` sessions named `app` wait on a lock, or `done` says the
/// racing operation finished without waiting. A wait, not a sleep: on a
/// slow runner a fixed sleep can end before the session reaches its lock.
async fn until_waiting(db: &TestDb, app: &str, n: i64, done: impl Fn() -> bool) {
    use std::time::Duration;
    let watch = db.pool(1).await;
    for _ in 0..3000 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE application_name = $1 AND wait_event_type = 'Lock'",
        )
        .bind(app)
        .fetch_one(&watch)
        .await
        .unwrap();
        if waiting >= n || done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{n} sessions of {app} never waited on a lock");
}

/// The attach's confirmation on Postgres (records and vault): an attach
/// whose WABA moves to another tenant while it binds and stores leaves no
/// token of its own under the other tenant's binding, and answers `503`
/// (`common::moving::an_attach_whose_waba_moves_takes_back_its_token`).
#[tokio::test]
async fn live_postgres_an_attach_whose_waba_moves_takes_back_its_token() {
    use meta_whatsapp_rs::adapters::store::PostgresKvStore;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(5).await;
    migrate(&pool).await.unwrap();
    common::moving::an_attach_whose_waba_moves_takes_back_its_token(
        std::sync::Arc::new(PgStore::new(pool.clone())),
        std::sync::Arc::new(PostgresKvStore::new(pool)),
    )
    .await;
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
        // Roadmap P2: the payment routes, by WABA and by number.
        Call::get("/v1/wabas/102290129340398/payment-configurations").key(&b),
        Call::get("/v1/wabas/102290129340398/payment-configurations/test-payment-configuration")
            .key(&platform)
            .tenant("tenant-b"),
        Call::get("/v1/numbers/106540352242922/payments/test-payment-configuration/BM345A-12")
            .key(&b),
        Call::new(
            meta_whatsapp_rs::webhooks::axum::http::Method::POST,
            "/v1/numbers/106540352242922/payments/refunds",
        )
        .key(&b)
        .header("idempotency-key", "refund-b-1")
        .json(
            &json!({"reference_id": "BM345A-12", "payment_config_id": "test-payment-configuration",
                      "amount": {"currency": "INR", "value": "100", "offset": "100"}}),
        ),
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

/// Roadmap P2 on Postgres: a key with the new `payments` scope is stored
/// and read back (`wa_server_api_keys.scopes`), and it reaches its own
/// WABA's payment configurations with that WABA's token; a key without it
/// is `403 forbidden`.
#[tokio::test]
async fn live_postgres_a_payments_key_reaches_its_wabas_configurations() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = harness(&db).await;
    h.tenant("tenant-a").await;
    h.connect(
        "tenant-a",
        "102290129340398",
        &["106540352242922"],
        "TOKEN-OF-A",
    )
    .await;
    let payments = h.tenant_key("tenant-a", &[Scope::Payments]).await;
    let send = h.tenant_key("tenant-a", &[Scope::Send]).await;
    let path = "/v1/wabas/102290129340398/payment-configurations";
    let refused = h.call(Call::get(path).key(&send)).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.text);
    h.graph
        .push_json(200, json!({"data": [{"payment_configurations": []}]}));
    let listed = h.call(Call::get(path).key(&payments)).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(listed.json(), json!({"data": []}));
    let request = h.graph.last_request().unwrap();
    assert_eq!(
        request.path(),
        "/v25.0/102290129340398/payment_configurations"
    );
    assert_eq!(request.bearer(), Some("TOKEN-OF-A"));
    assert_eq!(h.graph.remaining(), 0);
}

/// The design's M5.2 on Postgres (roadmap M5f): the records of the groups a
/// number holds live in the Postgres key/value store, written by Meta's
/// creation event, found by the listing, and read before the vault: a
/// group the number does not hold (another tenant's, another number's, one
/// never reported) is `404` with zero vault reads, and the owner's own is
/// found with the token opened after it.
#[tokio::test]
async fn live_postgres_a_group_the_number_does_not_hold_is_not_found_before_the_vault() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = harness(&db).await;
    let (a_pn, a_pn_2, b_pn) = ("106540352242922", "106540352242923", "106540352242924");
    h.tenant("tenant-a").await;
    h.connect("tenant-a", "102290129340398", &[a_pn, a_pn_2], "TOKEN-OF-A")
        .await;
    h.tenant("tenant-b").await;
    h.connect("tenant-b", "102290129340399", &[b_pn], "TOKEN-OF-B")
        .await;
    let a = h.tenant_key("tenant-a", &[Scope::Groups]).await;
    let b = h.tenant_key("tenant-b", &[Scope::Groups]).await;
    let group = "Y2FwaV9ncm91cDoxOTUwNTU1MDA3OToxMjAzNjMzOTQzMjAdOTY0MTUZD";
    let reported = h
        .webhook(&common::meta::group_created("102290129340398", a_pn, group))
        .await;
    assert_eq!(reported.status, StatusCode::OK, "{}", reported.text);
    for (key, pn) in [(&b, b_pn), (&a, a_pn_2), (&a, a_pn)] {
        let reads = h.kv.vault_reads();
        let reply = h
            .call(Call::get(format!("/v1/numbers/{pn}/groups/{group}")).key(key))
            .await;
        if pn == a_pn {
            // The owner's own: the token is opened, Meta asked.
            h.graph.push_json(200, json!({"id": group}));
            let reply = h
                .call(Call::get(format!("/v1/numbers/{pn}/groups/{group}")).key(key))
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
            assert!(h.kv.vault_reads() > reads);
        } else {
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{pn}: {}", reply.text);
            assert_eq!(h.kv.vault_reads(), reads, "{pn} read the vault");
        }
    }
    // Deleted by Meta's event: forgotten.
    let mut gone = common::meta::with_ids(
        serde_json::from_slice(&common::meta::group_created("102290129340398", a_pn, group))
            .unwrap(),
        "102290129340398",
        a_pn,
    );
    gone["entry"][0]["changes"][0]["value"]["groups"][0]["type"] = json!("group_delete");
    let reply = h.webhook(&bytes(&gone)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let reads = h.kv.vault_reads();
    let reply = h
        .call(Call::get(format!("/v1/numbers/{a_pn}/groups/{group}")).key(&a))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    assert_eq!(h.kv.vault_reads(), reads);
    assert_eq!(h.graph.remaining(), 0);
}

/// The design's M5.1 and M5.2 on Postgres (roadmap M5c2): a resumable
/// upload's record lives in the Postgres key/value store, and a profile
/// picture is set from the handle in the number's own record only: another
/// tenant's upload, and another number's of the same tenant, are `404` with
/// zero requests to Meta, and the owner's own sets the picture.
#[tokio::test]
async fn live_postgres_a_picture_upload_of_another_tenant_or_number_is_not_found() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(10).await;
    migrate(&pool).await.unwrap();
    let h = Harness::with_stores_and_settings(
        Stores::postgres(&pool),
        meta_whatsapp_server::state::Settings {
            app_id: Some(meta_whatsapp_rs::core::ids::AppId::new("634974688087057").unwrap()),
            ..common::test_settings()
        },
    );
    let (a_pn, a_pn_2, b_pn) = ("106540352242922", "106540352242923", "106540352242924");
    h.tenant("tenant-a").await;
    h.connect("tenant-a", "102290129340398", &[a_pn, a_pn_2], "TOKEN-OF-A")
        .await;
    h.tenant("tenant-b").await;
    h.connect("tenant-b", "102290129340399", &[b_pn], "TOKEN-OF-B")
        .await;
    let scopes = [Scope::Numbers, Scope::Media];
    let a = h.tenant_key("tenant-a", &scopes).await;
    let b = h.tenant_key("tenant-b", &scopes).await;
    let mut ids = Vec::new();
    for (key, pn) in [(&a, a_pn), (&a, a_pn_2), (&b, b_pn)] {
        h.graph.push_json(200, json!({"id": "upload:SESSION"}));
        let started = h
            .call(
                Call::new(Method::POST, format!("/v1/numbers/{pn}/media-uploads"))
                    .key(key)
                    .json(&json!({"file_name": "logo.png", "file_length": 12, "file_type": "image/png"})),
            )
            .await;
        assert_eq!(started.status, StatusCode::CREATED, "{}", started.text);
        let id = started.json()["upload_id"].as_str().unwrap().to_owned();
        h.graph.push_json(200, json!({"h": "4::HANDLE"}));
        let chunk = h
            .call(
                Call::new(
                    Method::POST,
                    format!("/v1/numbers/{pn}/media-uploads/{id}?file_offset=0"),
                )
                .key(key)
                .header("content-type", "application/octet-stream")
                .body(Body::from(b"png bytes...".to_vec())),
            )
            .await;
        assert_eq!(chunk.status, StatusCode::OK, "{}", chunk.text);
        ids.push(id);
    }
    let patch = |pn: &str, key: &str, id: &str| {
        Call::new(Method::PATCH, format!("/v1/numbers/{pn}/profile"))
            .key(key)
            .json(&json!({"profile_picture_upload_id": id}))
    };
    let asked = h.graph.requests().len();
    // Another tenant's upload, and the same tenant's other number's.
    for (key, pn, id) in [
        (&a, a_pn, &ids[2]),
        (&b, b_pn, &ids[0]),
        (&a, a_pn, &ids[1]),
    ] {
        let reply = h.call(patch(pn, key, id)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{pn}: {}", reply.text);
    }
    assert_eq!(h.graph.requests().len(), asked, "Meta was asked");
    // The owner's own sets the picture, with the handle of the record.
    h.graph.push_json(200, json!({"success": true}));
    h.graph.push_json(200, json!({"data": [{"about": "Hi"}]}));
    let set = h.call(patch(a_pn, &a, &ids[0])).await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text);
    let requests = h.graph.requests();
    assert_eq!(
        requests[requests.len() - 2].json().unwrap(),
        json!({"messaging_product": "whatsapp", "profile_picture_handle": "4::HANDLE"})
    );
    assert_eq!(h.graph.remaining(), 0);
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
    outbox_suite::bind(h.store.as_ref(), "tenant-a", "1").await;
    let key = h.tenant_key("tenant-a", &[Scope::Events]).await;
    let writers: Vec<_> = (0..8)
        .map(|w| {
            let store = PgEventStore::new(pool.clone());
            tokio::spawn(async move {
                let mut ids = Vec::new();
                for i in 0..40 {
                    let row = NewEvent {
                        id: format!("evt_w{w}_{i}_{}", common::unique()),
                        ..outbox_suite::row(Some("tenant-a"), "message_received", "1", None)
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
    outbox_suite::bind(h.store.as_ref(), "tenant-a", "1").await;
    outbox_suite::bind(h.store.as_ref(), "tenant-b", "2").await;
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
    let first = outbox_suite::row(Some("tenant-a"), "message_received", "1", None);
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
    let second = outbox_suite::row(Some("tenant-a"), "message_received", "1", None);
    let later = {
        let store = PgEventStore::new(pool.clone());
        let second = second.clone();
        tokio::spawn(async move { store.insert(&second).await })
    };
    // Another tenant's does not.
    let other = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        PgEventStore::new(pool.clone()).insert(&outbox_suite::row(
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
            assert!(
                store
                    .unbind_waba(&WabaId::new(EXAMPLE_WABA).unwrap())
                    .await
                    .unwrap()
            );
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
            assert!(
                store
                    .unbind_waba(&WabaId::new(EXAMPLE_WABA).unwrap())
                    .await
                    .unwrap()
            );
            store
                .bind_waba(
                    &tenant,
                    &WabaId::new("102290129349999").unwrap(),
                    &[meta_whatsapp_rs::core::ids::PhoneNumberId::new(EXAMPLE_PN).unwrap()],
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
            .unbind_waba(&WabaId::new(EXAMPLE_WABA).unwrap())
            .await
            .unwrap();
        h.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
            .await;
        // Dated now, after the binding began.
        let (body, numbers) = if names_a_number {
            (
                text(EXAMPLE_WABA, EXAMPLE_PN, "wamid.MOVED-TO-B"),
                vec![PhoneNumberId::new(EXAMPLE_PN).unwrap()],
            )
        } else {
            (template_approved(EXAMPLE_WABA), vec![])
        };
        let store = h.store.clone();
        let b = TenantId::parse("tenant-b").unwrap();
        h.outbox.before_next_insert(Box::new(move || {
            Box::pin(async move {
                assert!(
                    store
                        .unbind_waba(&WabaId::new(EXAMPLE_WABA).unwrap())
                        .await
                        .unwrap()
                );
                store
                    .bind_waba(&b, &WabaId::new(EXAMPLE_WABA).unwrap(), &numbers)
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
    let Some(db) = TestDb::new().await else {
        return;
    };
    // Its sessions, by name: the insert and the unbinding are seen to wait.
    let app = format!("unbind-wait-{}", db.schema);
    for names_a_number in [true, false] {
        let h = named_harness(&db, &app).await;
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
        let (db, app) = (&db, app.as_str());
        let admin = async move {
            // The insert reaches the stream's row and waits there.
            until_waiting(db, app, 1, || false).await;
            let unbind = tokio::spawn(async move {
                store.unbind_waba(&WabaId::new(EXAMPLE_WABA).unwrap()).await
            });
            // The unbinding waits behind the insert, or finishes without
            // waiting (the defect this case is for).
            until_waiting(db, app, 2, || unbind.is_finished()).await;
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
/// naming only its WABA. An event Meta did not date (an error) has no date
/// to tell the bindings apart by; since roadmap S2b the re-check finds the
/// new binding a move (the deleted tenant's tombstone is another
/// tenant's), within the window after it: operator-only too (it went to
/// the new tenant before, the limit S2 documented). Each case starts from
/// a WABA no other tenant held (a first onboarding: the cases before it
/// left a move behind). Decisive: the insert's `attached_at` re-check in
/// each branch, and, for the error, its window check and the tombstone
/// forgetting a deleted tenant.
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
        // The old tenant has held the WABA for an hour, as its first
        // onboarding.
        sqlx::query(
            "UPDATE wa_server_wabas SET attached_at = now() - interval '1 hour', \
             previous_tenant_unbound_at = NULL",
        )
        .execute(&db.pool(1).await)
        .await
        .unwrap();
        let store = h.store.clone();
        h.outbox.before_next_insert(Box::new(move || {
            Box::pin(async move {
                assert!(
                    store
                        .unbind_waba(&WabaId::new(EXAMPLE_WABA).unwrap())
                        .await
                        .unwrap()
                );
                assert_eq!(
                    store.delete_tenant(&tenant).await.unwrap(),
                    DeleteTenantOutcome::Deleted
                );
                store.create_tenant(&tenant, "").await.unwrap().unwrap();
                store
                    .bind_waba(
                        &tenant,
                        &WabaId::new(EXAMPLE_WABA).unwrap(),
                        &[PhoneNumberId::new(EXAMPLE_PN).unwrap()],
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
        let why = if dated_event {
            "a binding begun after the event"
        } else {
            "an undated event within the window after a move"
        };
        assert_eq!(
            rows[0].0, None,
            "the new tenant-a got the old one's event ({why}): {polled}"
        );
        assert_eq!(polled, serde_json::json!([]), "{why}");
    }
}

/// Roadmap S2, the security review's L3, and S2b: after a move, an event
/// Meta dated in the very second its WABA's binding began is nobody's,
/// through the routing and through the insert's re-check, which agree to
/// the second. Meta's dates are whole seconds, so an event of that second
/// may be a previous holder's. A first onboarding (no previous tenant)
/// keeps it (S2b: `attached_at < to_timestamp(t + 1)`). Through the
/// routing: bound half a second into the event's second, after a move the
/// event is routed to nobody, as a first onboarding to tenant-a. Through
/// the re-check: routed to tenant-a (bound for an hour), then, before the
/// insert, bound again (tenant-a deleted and created again: a move, the
/// deleted tenant being another) half a second into the event's second,
/// or at its very first instant: operator-only; half a second before it:
/// tenant-a's. For an event naming a number, then one naming only its
/// WABA; each case on a database of its own (a body is delivered once per
/// database). Decisive: the routing's `RouteGuard::admits`, and the
/// re-check's bound after a move (`attached_at < to_timestamp(t -
/// margin)`) in each branch, both ways, and strict (`<=` keeps the first
/// instant's row), and the first onboarding's relaxed one.
#[tokio::test]
async fn live_postgres_an_event_dated_in_its_binding_s_first_second_is_nobodys() {
    use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
    use meta_whatsapp_server::model::DeleteTenantOutcome;
    let second = common::meta::now() - 600;
    // Bound again before the insert at this offset into the event's
    // second, and whether tenant-a keeps the row; `None`: bound in the
    // event's second before the routing, after a move (`true`) or as a
    // first onboarding (`false`).
    for (bound_again, moved) in [
        (None, true),
        (None, false),
        (Some(("0.5 second", false)), true),
        (Some(("0 second", false)), true),
        (Some(("-0.5 second", true)), true),
    ] {
        let Some(db) = TestDb::new().await else {
            return;
        };
        let pool = db.pool(1).await;
        for body in [
            dated(text(EXAMPLE_WABA, EXAMPLE_PN, "wamid.SAME-SECOND"), second),
            dated(template_approved(EXAMPLE_WABA), second),
        ] {
            let h = harness(&db).await;
            sqlx::query("DELETE FROM wa_server_events")
                .execute(&pool)
                .await
                .unwrap();
            let tenant = TenantId::parse("tenant-a").unwrap();
            let _ = h.store.create_tenant(&tenant, "").await.unwrap();
            h.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
                .await;
            let Some((offset, kept)) = bound_again else {
                // Through the routing.
                sqlx::query(
                    "UPDATE wa_server_wabas \
                     SET attached_at = to_timestamp($1::bigint) + interval '0.5 second', \
                     previous_tenant_unbound_at = \
                       CASE WHEN $2 THEN now() - interval '30 days' ELSE NULL END",
                )
                .bind(second)
                .bind(moved)
                .execute(&pool)
                .await
                .unwrap();
                let holder = (!moved).then(|| "tenant-a".to_owned());
                assert_eq!(h.webhook(&bytes(&body)).await.status, StatusCode::OK);
                assert_eq!(routed_to(&h), holder, "routed, moved {moved}: {body}");
                let rows = outbox_rows(&pool).await;
                assert_eq!(rows, [(holder, rows[0].1.clone())], "moved {moved}: {body}");
                continue;
            };
            // Through the re-check. The old tenant has held the WABA for an
            // hour: routed to it.
            sqlx::query("UPDATE wa_server_wabas SET attached_at = now() - interval '1 hour'")
                .execute(&pool)
                .await
                .unwrap();
            let (store, bound) = (h.store.clone(), pool.clone());
            h.outbox.before_next_insert(Box::new(move || {
                Box::pin(async move {
                    assert!(
                        store
                            .unbind_waba(&WabaId::new(EXAMPLE_WABA).unwrap())
                            .await
                            .unwrap()
                    );
                    assert_eq!(
                        store.delete_tenant(&tenant).await.unwrap(),
                        DeleteTenantOutcome::Deleted
                    );
                    store.create_tenant(&tenant, "").await.unwrap().unwrap();
                    store
                        .bind_waba(
                            &tenant,
                            &WabaId::new(EXAMPLE_WABA).unwrap(),
                            &[PhoneNumberId::new(EXAMPLE_PN).unwrap()],
                        )
                        .await
                        .unwrap();
                    sqlx::query(
                        "UPDATE wa_server_wabas \
                         SET attached_at = to_timestamp($1::bigint) + $2::interval",
                    )
                    .bind(second)
                    .bind(offset)
                    .execute(&bound)
                    .await
                    .unwrap();
                })
            }));
            assert_eq!(h.webhook(&bytes(&body)).await.status, StatusCode::OK);
            assert_eq!(
                routed_to(&h).as_deref(),
                Some("tenant-a"),
                "{offset}: {body}"
            );
            let rows = outbox_rows(&pool).await;
            assert_eq!(rows.len(), 1, "{offset}: {rows:?}");
            assert_eq!(
                rows[0].0.as_deref(),
                kept.then_some("tenant-a"),
                "bound again {offset} into the event's second: {body}"
            );
        }
    }
}

/// The insert's `attached_at` re-check agrees with the routing on the
/// other side of the second too: an event routed to tenant-a (bound for an
/// hour), then, before the insert, its WABA bound again (tenant-a deleted
/// and created again) from the second after Meta dated it, is nobody's:
/// that binding began after the event, as the routing counts it. For an
/// event naming a number, then one naming only its WABA. Decisive: the
/// re-check's bound is no later than `to_timestamp(t + 1)` in each branch
/// (the bound itself, `to_timestamp(t)`: the test above).
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
                assert!(
                    store
                        .unbind_waba(&WabaId::new(EXAMPLE_WABA).unwrap())
                        .await
                        .unwrap()
                );
                assert_eq!(
                    store.delete_tenant(&tenant).await.unwrap(),
                    DeleteTenantOutcome::Deleted
                );
                store.create_tenant(&tenant, "").await.unwrap().unwrap();
                store
                    .bind_waba(
                        &tenant,
                        &WabaId::new(EXAMPLE_WABA).unwrap(),
                        &[PhoneNumberId::new(EXAMPLE_PN).unwrap()],
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
    let key = h.tenant_key("merchant-42", &[Scope::Send]).await;
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

/// Acceptance test M2.2 (roadmap M2b), live: two replicas on one database
/// (two services, each on a pool of its own: `NOTIFY` on the deployment's
/// channel, each replica's own `LISTEN` connection). A webhook delivered to
/// A reaches a stream open on B within 2 s; `Last-Event-ID` on B replays
/// exactly the events it missed, those recorded while it was away
/// included; a stream on another tenant's number on B sees none of them.
/// Decisive: the notification crossing replicas (B's streams have no
/// re-poll in the test's time).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_a_webhook_into_one_replica_reaches_a_stream_on_another() {
    use common::sse::open;
    use meta_whatsapp_server::state::Settings;
    use meta_whatsapp_server_core::live::LiveSettings;
    use std::time::Duration;
    const WITHIN: Duration = Duration::from_secs(2);
    let Some(db) = TestDb::new().await else {
        return;
    };
    let woken_only = Settings {
        live: LiveSettings {
            heartbeat: Duration::from_secs(3600),
            repoll: Duration::from_secs(3600),
            ..LiveSettings::default()
        },
        ..common::test_settings()
    };
    let replica = async |settings: Settings| {
        let pool = db.pool(10).await;
        migrate(&pool).await.unwrap();
        Harness::with_stores_and_settings(Stores::postgres(&pool), settings)
    };
    let a = replica(common::test_settings()).await;
    let b = replica(woken_only).await;
    a.tenant("tenant-a").await;
    a.connect("tenant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
        .await;
    a.tenant("tenant-b").await;
    a.connect(
        "tenant-b",
        "102290129340399",
        &["106540352242923"],
        "TOKEN-OF-B",
    )
    .await;
    let key = b.tenant_key("tenant-a", &[Scope::Events]).await;
    let b_key = b.tenant_key("tenant-b", &[Scope::Events]).await;
    let mut on_b = open(&b.internal, Call::get("/v1/events/stream").key(&key))
        .await
        .unwrap_or_else(|reply| panic!("{}", reply.text));
    let mut bs_number = open(
        &b.internal,
        Call::get("/v1/numbers/106540352242923/events/stream").key(&b_key),
    )
    .await
    .unwrap_or_else(|reply| panic!("{}", reply.text));
    let mut seen = Vec::new();
    for i in 0..3 {
        let started = tokio::time::Instant::now();
        let reply = a
            .webhook(&bytes(&text(
                EXAMPLE_WABA,
                EXAMPLE_PN,
                &format!("wamid.m22.{i}"),
            )))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        let frame = on_b.event(WITHIN).await;
        assert!(
            started.elapsed() <= WITHIN,
            "reached the other replica after {:?}",
            started.elapsed()
        );
        assert_eq!(frame.json()["tenant_id"], "tenant-a");
        seen.push(frame.sequence());
    }
    assert_eq!(seen, [1, 2, 3]);
    // The client goes away after the first; two more arrive meanwhile.
    drop(on_b);
    for i in 3..5 {
        let reply = a
            .webhook(&bytes(&text(
                EXAMPLE_WABA,
                EXAMPLE_PN,
                &format!("wamid.m22.{i}"),
            )))
            .await;
        assert_eq!(reply.status, StatusCode::OK);
    }
    let mut resumed = open(
        &b.internal,
        Call::get("/v1/events/stream")
            .key(&key)
            .header("last-event-id", "1"),
    )
    .await
    .unwrap_or_else(|reply| panic!("{}", reply.text));
    let mut replayed = Vec::new();
    for _ in 0..4 {
        replayed.push(resumed.event(WITHIN).await.sequence());
    }
    assert_eq!(
        replayed,
        [2, 3, 4, 5],
        "exactly the events after Last-Event-ID"
    );
    assert!(resumed.next(Duration::from_millis(400)).await.is_none());
    assert!(
        bs_number.next(Duration::from_millis(400)).await.is_none(),
        "a stream on tenant b's number saw tenant a's events"
    );
}

/// Roadmap M3d's decisive test on Postgres (decision D6): after a
/// registration's whole flow (`common::registration::flow`), no row of any
/// table of the service's schema (the records, the idempotency records
/// and their kept answers, the outbox, the inbox, the library's `wa_kv`:
/// the vault and the attempt counters) holds the PIN, the wrong PIN or the
/// code, as text or as the hex Postgres prints a `bytea` in; while the
/// sweep does find the number's id where it is stored (the control).
/// Storing the PIN in any table fails it.
#[tokio::test]
async fn live_postgres_a_registration_leaves_its_pin_in_no_table() {
    use common::registration::{self as flow, PN, SECRETS};
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = harness(&db).await;
    let key = flow::connected(&h).await;
    flow::flow(&h, &key).await;
    flow::meta_got_the_secrets(&h);
    let pool = db.pool(2).await;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables \
         WHERE table_schema = current_schema() AND table_type = 'BASE TABLE'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        tables.iter().any(|t| t == "wa_kv") && tables.iter().any(|t| t == "wa_server_idempotency"),
        "{tables:?}"
    );
    let mut rows = 0;
    let mut found_number = false;
    for table in &tables {
        let texts: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT t::text FROM \"{table}\" t"
        )))
        .fetch_all(&pool)
        .await
        .unwrap();
        for text in texts {
            rows += 1;
            found_number |= text.contains(PN);
            for secret in SECRETS {
                assert!(!text.contains(secret), "{table} holds {secret}: {text}");
                let hex = hex::encode(secret);
                assert!(
                    !text.contains(&hex),
                    "{table} holds {secret} as bytes: {text}"
                );
            }
        }
    }
    assert!(rows > 5, "{rows} rows swept");
    assert!(found_number, "the sweep reads the stored rows");
}

/// Roadmap M3d's budget across replicas on Postgres: two instances on one
/// database, twelve registrations of one number at once (six on each), a
/// budget of 3: no more than 3 reach Meta, the stored count is the number
/// that did, and every other one is refused before Meta (`429`, or `503`
/// when the compare-and-swap kept losing). Decisive: the charge's
/// compare-and-swap on the shared `KvStore` (a plain write lets racing
/// replicas all through).
#[tokio::test]
async fn live_postgres_a_numbers_budget_holds_across_racing_replicas() {
    use common::registration::{self as flow, PIN, PN, post, success};
    use meta_whatsapp_rs::core::ids::PhoneNumberId;
    use meta_whatsapp_rs::core::store::KvStore;
    use meta_whatsapp_server_core::registration::{
        AttemptBudget, AttemptLimit, AttemptLimits, RegistrationAttempts,
    };
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(20).await;
    migrate(&pool).await.unwrap();
    let settings = meta_whatsapp_server::state::Settings {
        registration_attempts: AttemptLimits {
            registration: AttemptLimit {
                attempts: 3,
                window: std::time::Duration::from_hours(72),
            },
            ..AttemptLimits::default()
        },
        ..common::test_settings()
    };
    let a = Harness::with_stores_and_settings(Stores::postgres(&pool), settings.clone());
    let b = Harness::with_stores_and_settings(Stores::postgres(&pool), settings);
    let key = flow::connected(&a).await;
    for h in [&a, &b] {
        for _ in 0..6 {
            h.graph.push_json(200, success());
        }
    }
    let calls = (0..12).map(|i| {
        let h = if i % 2 == 0 { &a } else { &b };
        h.call(post(&key, "register", &serde_json::json!({"pin": PIN})))
    });
    let replies = futures::future::join_all(calls).await;
    let passed = replies
        .iter()
        .filter(|r| r.status == StatusCode::OK)
        .count();
    for reply in &replies {
        assert!(
            [200, 429, 503].contains(&reply.status.as_u16()),
            "{}: {}",
            reply.status,
            reply.text
        );
    }
    let asked = a.graph.requests().len() + b.graph.requests().len();
    assert_eq!(asked, passed, "only the passed ones reached Meta");
    assert!((1..=3).contains(&passed), "{passed} passed");
    let stored =
        a.kv.get(&RegistrationAttempts::store_key(
            AttemptBudget::Registration,
            &PhoneNumberId::new(PN).unwrap(),
        ))
        .await
        .unwrap()
        .unwrap();
    let count = serde_json::from_slice::<serde_json::Value>(&stored.value).unwrap()["count"]
        .as_u64()
        .unwrap();
    assert_eq!(usize::try_from(count).unwrap(), passed);
}
