//! Embedded Signup over HTTP (roadmap M3a; docs/design/server.md, sections
//! 3.4, 4.2 and 4.6): `POST /v1/signup/sessions`, `POST
//! /v1/signup/complete`, `GET /v1/signup/attempts`, `POST
//! /v1/signup/resume`, the tenant's disconnection in Solution Partner mode,
//! the `account_update` webhooks that offboard a merchant, and the
//! operator's re-funding of a revoked business.
//!
//! Acceptance tests M3.1 to M3.4, exactly as the design writes them:
//!
//! - **M3.1**: the happy path over HTTP with Meta's documented responses
//!   binds the verified WABA and all its numbers; no response contains the
//!   token.
//! - **M3.2**: another tenant's state is `403 stale_attempt` and the
//!   rightful tenant still completes; a malformed PIN is `422` and the
//!   state survives.
//! - **M3.3**: a scripted `133005` at registration is `502
//!   onboarding_failed` (`register_phone`, resumable); a new process on the
//!   same database resumes with a corrected PIN; another tenant's resume is
//!   `404`. (Live on Postgres too: `live_postgres_m3_3_…`.)
//! - **M3.4**: partner mode: the credit-line request carries the partner's
//!   system token (asserted header), never the merchant's; missing partner
//!   settings refuse the start. D4 is enforced before any credit call,
//!   through the library's post-verification gate: a second tenant
//!   onboarding a bound WABA gets `409`, nothing is stored, subscribed or
//!   shared, binding unchanged. A business whose line was revoked is not
//!   re-funded by `resume` or a new signup without an explicit operator
//!   action. Offboarding revokes first and deletes second: a CMS
//!   disconnect, `PARTNER_APP_UNINSTALLED` and `PARTNER_REMOVED` (in any
//!   order) end with the line revoked (from the stored or the webhook's
//!   owner business id) before the vault entry and bindings go; tests
//!   replay both orders.
//!
//! The logs' half (no token, PIN, code or state at any level) is
//! `signup_logs.rs`, alone in its binary as `logs.rs` is.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::moving::{Moving, move_waba};
use common::signup::{
    ALLOCATION, APP_ID, BUSINESS, CODE, CONFIG_ID, LINE, PHONE, PHONE_2, PIN, SYSTEM_TOKEN,
    SYSTEM_USER, TOKEN, WABA, account_update, complete, finished, harness, removed, resume,
    script_already_revoked, script_confirmed, script_revocation, script_share,
    script_until_verified, settings, solution_partner, start, success, tech_provider, uninstalled,
};
use common::{APP_SECRET, Call, Harness, Stores, TestDb};
use meta_whatsapp_rs::adapters::store::MemoryKvStore;
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::{BusinessId, PhoneNumberId, WabaId};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::config::{Config, ConfigError, Env, NotUnicode, OnboardingMode};
use meta_whatsapp_server::model::{AllowedTenants, Scope, TenantId, VaultEntry};
use meta_whatsapp_server::store::MemoryStore;
use meta_whatsapp_server_core::signup::AccountUpdateConfirmation;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const A: &str = "merchant-a";
const B: &str = "merchant-b";

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

/// The tenants and a `signup` key each (and `numbers`, to disconnect).
async fn tenants(h: &Harness) -> (String, String) {
    h.tenant(A).await;
    h.tenant(B).await;
    let scopes = [Scope::Signup, Scope::Numbers];
    (
        h.tenant_key(A, &scopes).await,
        h.tenant_key(B, &scopes).await,
    )
}

/// `body` answers neither the merchant's token nor the PIN.
fn assert_no_secret(text: &str) {
    for secret in [TOKEN, PIN, CODE, SYSTEM_TOKEN] {
        assert!(!text.contains(secret), "{secret} answered: {text}");
    }
}

/// A Cloud API onboarding of [`WABA`] by `key`, with the PIN, as a Tech
/// Provider: connected.
async fn onboard_tech_provider(h: &Harness, key: &str) -> Value {
    let state = start(h, key).await;
    script_until_verified(&h.graph);
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(200, success()); // register
    let done = h.call(complete(key, &finished(&state))).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    done.json()
}

/// The same as a Solution Partner: the credit line shared.
async fn onboard_partner(h: &Harness, key: &str) -> Value {
    let state = start(h, key).await;
    script_until_verified(&h.graph);
    h.graph.push_json(200, success()); // subscribed_apps
    script_share(&h.graph);
    h.graph.push_json(200, success()); // register
    let done = h.call(complete(key, &finished(&state))).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    assert_eq!(h.graph.remaining(), 0);
    done.json()
}

// ─── M3.1 ────────────────────────────────────────────────────────────────

/// M3.1: the happy path over HTTP with Meta's documented responses binds
/// the verified WABA and all its numbers; no response contains the token.
/// The page's launch data first (no Meta call), then each of Meta's
/// requests exactly. Decisive: the approval's binding (removing it binds
/// nothing) and the answers' types (no token field).
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn m3_1_the_happy_path_binds_the_verified_waba_and_all_its_numbers() {
    let h = harness(tech_provider());
    let (a, _) = tenants(&h).await;
    let started = h
        .call(
            Call::new(Method::POST, "/v1/signup/sessions")
                .key(&a)
                .json(&json!({})),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.text);
    let launch = started.json();
    let state = launch["state"].as_str().unwrap().to_owned();
    assert_eq!(state.len(), 22, "128 random bits, base64url");
    assert_eq!(launch["app_id"], APP_ID);
    assert_eq!(launch["config_id"], CONFIG_ID);
    assert_eq!(launch["graph_api_version"], "v25.0");
    // `embedded-signup/implementation`, "Launch method and callback
    // registration": what `FB.login` takes.
    assert_eq!(
        launch["launch_options"],
        json!({
          "config_id": CONFIG_ID,
          "response_type": "code",
          "override_default_response_type": true,
          "extras": {"setup": {}}
        })
    );
    assert!(launch["expires_at"].as_str().unwrap().ends_with('Z'));
    assert!(
        h.graph.requests().is_empty(),
        "the launch asks Meta nothing"
    );

    script_until_verified(&h.graph);
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(200, success()); // register
    let done = h.call(complete(&a, &finished(&state))).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    assert_eq!(
        done.json(),
        json!({
            "status": "connected",
            "waba_id": WABA,
            "phone_number_ids": [PHONE, PHONE_2],
            "steps_completed": [
                "exchange_code", "debug_token", "verify_assets", "approve",
                "store_token", "subscribe_app", "register_phone"
            ],
            "needs_coexistence_sync": false
        })
    );
    assert_no_secret(&done.text);
    assert_no_secret(&started.text);

    let requests = h.graph.requests();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[0].method, Method::GET);
    assert_eq!(requests[0].path(), "/v25.0/oauth/access_token");
    assert_eq!(requests[0].query("client_id").as_deref(), Some(APP_ID));
    assert_eq!(requests[0].query("code").as_deref(), Some(CODE));
    assert_eq!(requests[0].header("authorization"), None);
    assert_eq!(requests[1].path(), "/v25.0/debug_token");
    assert_eq!(
        requests[1].bearer(),
        Some(format!("{APP_ID}|{APP_SECRET}").as_str()),
        "the app token inspects the merchant's"
    );
    assert_eq!(requests[2].path(), format!("/v25.0/{WABA}"));
    assert_eq!(
        requests[2].query("fields").as_deref(),
        Some("owner_business_info")
    );
    assert_eq!(requests[3].path(), format!("/v25.0/{WABA}/phone_numbers"));
    assert_eq!(requests[4].method, Method::POST);
    assert_eq!(requests[4].path(), format!("/v25.0/{WABA}/subscribed_apps"));
    assert_eq!(requests[5].method, Method::POST);
    assert_eq!(requests[5].path(), format!("/v25.0/{PHONE}/register"));
    assert_eq!(
        requests[5].json(),
        Some(json!({"messaging_product": "whatsapp", "pin": PIN}))
    );
    for request in &requests[2..] {
        assert_eq!(request.bearer(), Some(TOKEN), "{}", request.path());
    }
    assert_eq!(h.graph.remaining(), 0);

    // Bound: the WABA and both of its numbers, the tenant's; the token in
    // the vault only, routed by each number.
    let wabas = h.call(Call::get("/v1/wabas").key(&a)).await;
    assert_eq!(wabas.json()["data"][0]["waba_id"], WABA, "{}", wabas.text);
    let numbers = h.call(Call::get("/v1/numbers").key(&a)).await;
    let listed = numbers.json();
    let ids: Vec<&str> = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["phone_number_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [PHONE, PHONE_2], "{}", numbers.text);
    for (reply, what) in [(&wabas, "wabas"), (&numbers, "numbers")] {
        assert_no_secret(&reply.text);
        assert_eq!(reply.status, StatusCode::OK, "{what}");
    }
    for pn in [PHONE, PHONE_2] {
        let stored = h
            .vault
            .get_by_phone_number(&PhoneNumberId::new(pn).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.token.expose_secret(), TOKEN);
    }
    let binding = h
        .store
        .waba(&WabaId::new(WABA).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(binding.tenant_id, tenant(A));
    // The state was single use: replayed, it is stale, and asks Meta
    // nothing.
    let again = h.call(complete(&a, &finished(&state))).await;
    assert_eq!(again.code(), "stale_attempt", "{}", again.text);
    assert_eq!(h.graph.requests().len(), 6);
    // Nothing kept: the onboarding finished.
    let attempts = h.call(Call::get("/v1/signup/attempts").key(&a)).await;
    assert_eq!(attempts.json(), json!({"data": [], "next_cursor": null}));
}

/// A coexistence onboarding (the WhatsApp Business app's number) launches
/// with its feature type, never registers, and runs the two syncs as its
/// last steps (decision D7, the library's L11d), once each.
#[tokio::test]
async fn a_coexistence_signup_syncs_and_never_registers() {
    let h = harness(tech_provider());
    let (a, _) = tenants(&h).await;
    let started = h
        .call(
            Call::new(Method::POST, "/v1/signup/sessions")
                .key(&a)
                .json(&json!({"coexistence": true})),
        )
        .await;
    let launch = started.json();
    assert_eq!(
        launch["launch_options"]["extras"]["featureType"], "whatsapp_business_app_onboarding",
        "{}",
        started.text
    );
    let state = launch["state"].as_str().unwrap();
    script_until_verified(&h.graph);
    h.graph.push_json(200, success()); // subscribed_apps
    // `onboarding-business-app-users`, steps 1 and 2: the example answer.
    for request_id in ["R-CONTACTS", "R-HISTORY"] {
        h.graph.push_json(
            200,
            json!({"messaging_product": "whatsapp", "request_id": request_id}),
        );
    }
    let body = json!({
        "state": state, "code": CODE, "pin": PIN,
        "event": {"type": "WA_EMBEDDED_SIGNUP", "event": "FINISH_WHATSAPP_BUSINESS_APP_ONBOARDING",
            "data": {"waba_id": WABA}}
    });
    let done = h.call(complete(&a, &body)).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    assert_eq!(done.json()["needs_coexistence_sync"], true);
    assert_eq!(
        done.json()["steps_completed"],
        json!([
            "exchange_code",
            "debug_token",
            "verify_assets",
            "approve",
            "store_token",
            "subscribe_app",
            "sync_contacts",
            "sync_history"
        ])
    );
    let requests = h.graph.requests();
    assert!(
        requests.iter().all(|r| !r.path().ends_with("/register")),
        "a coexistence number is registered already"
    );
    let syncs: Vec<Value> = requests
        .iter()
        .filter(|r| r.path().ends_with("/smb_app_data"))
        .map(|r| r.json().unwrap())
        .collect();
    assert_eq!(
        syncs,
        [
            json!({"messaging_product": "whatsapp", "sync_type": "smb_app_state_sync"}),
            json!({"messaging_product": "whatsapp", "sync_type": "history"}),
        ]
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// A `CANCEL` event redeems nothing: the attempt still completes.
/// `embedded-signup/implementation`, "Abandoned flow structure".
#[tokio::test]
async fn a_cancelled_flow_redeems_nothing() {
    let h = harness(tech_provider());
    let (a, _) = tenants(&h).await;
    let state = start(&h, &a).await;
    let cancel = json!({
        "state": state, "code": CODE,
        "event": {"data": {"current_step": "PHONE_NUMBER_SETUP"}, "type": "WA_EMBEDDED_SIGNUP", "event": "CANCEL"}
    });
    let cancelled = h.call(complete(&a, &cancel)).await;
    assert_eq!(
        cancelled.json(),
        json!({"status": "cancelled", "current_step": "PHONE_NUMBER_SETUP"}),
        "{}",
        cancelled.text
    );
    assert!(h.graph.requests().is_empty());
    script_until_verified(&h.graph);
    h.graph.push_json(200, success());
    h.graph.push_json(200, success());
    let done = h.call(complete(&a, &finished(&state))).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
}

// ─── M3.2 ────────────────────────────────────────────────────────────────

/// M3.2: another tenant's state is `403 stale_attempt` and the rightful
/// tenant still completes; a malformed PIN is `422` and the state
/// survives. Another tenant's key, or a platform key naming another
/// tenant, never burns the state, and asks Meta nothing; a body that is
/// not the documented JSON never quotes the PIN or the code back.
/// Decisive: `SignupSessions::redeem` for the caller's tenant (redeeming
/// for whoever posts lets B onboard A's attempt), and the local checks
/// before it.
#[tokio::test]
async fn m3_2_another_tenants_state_is_stale_and_the_rightful_tenant_still_completes() {
    let h = harness(tech_provider());
    let (a, b) = tenants(&h).await;
    let platform = h.platform_key(AllowedTenants::All, &[Scope::Signup]).await;
    let state = start(&h, &a).await;

    let stolen = h.call(complete(&b, &finished(&state))).await;
    assert_eq!(
        (stolen.status, stolen.code().as_str()),
        (StatusCode::FORBIDDEN, "stale_attempt"),
        "{}",
        stolen.text
    );
    let named = h
        .call(
            Call::new(Method::POST, "/v1/signup/complete")
                .key(&platform)
                .tenant(B)
                .json(&finished(&state)),
        )
        .await;
    assert_eq!(named.code(), "stale_attempt", "{}", named.text);
    assert!(h.graph.requests().is_empty(), "nothing reached Meta");

    // A malformed PIN: refused before the state is read.
    let mut short = finished(&state);
    short["pin"] = json!("12345");
    let refused = h.call(complete(&a, &short)).await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
        "{}",
        refused.text
    );
    assert_eq!(refused.json()["error"]["field"], "pin");
    // A PIN or a code of the wrong JSON type: a deserializer's message
    // would quote it back.
    for body in [
        json!({"state": state, "code": CODE, "event": {}, "pin": 581_063}),
        json!({"state": state, "code": 424_242_424_242_u64, "event": {}}),
    ] {
        let reply = h.call(complete(&a, &body)).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], "body");
        assert!(
            !reply.text.contains("581063") && !reply.text.contains("424242424242"),
            "{}",
            reply.text
        );
    }
    for (field, value) in [("state", "not a state!"), ("code", "")] {
        let mut bad = finished(&state);
        bad[field] = json!(value);
        let reply = h.call(complete(&a, &bad)).await;
        assert_eq!(reply.json()["error"]["field"], field, "{}", reply.text);
    }
    let mut not_an_event = finished(&state);
    not_an_event["event"]["type"] = json!("SOMETHING_ELSE");
    assert_eq!(
        h.call(complete(&a, &not_an_event)).await.json()["error"]["field"],
        "event"
    );
    assert!(h.graph.requests().is_empty());

    // The state survived all of it: A completes.
    script_until_verified(&h.graph);
    h.graph.push_json(200, success());
    h.graph.push_json(200, success());
    let done = h.call(complete(&a, &finished(&state))).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    assert_eq!(done.json()["waba_id"], WABA);
    assert_eq!(h.graph.remaining(), 0);
}

/// The completion's body is at most 16 KiB (design section 4.1): one byte
/// more is `413`, before anything is read.
#[tokio::test]
async fn a_completion_over_16_kib_is_too_large() {
    let h = harness(tech_provider());
    let (a, _) = tenants(&h).await;
    let state = start(&h, &a).await;
    let mut body = finished(&state);
    let base = body.to_string().len();
    body["event"]["data"]["padding"] = json!("x".repeat(16 * 1024 - base));
    let over = h.call(complete(&a, &body)).await;
    assert_eq!(over.code(), "payload_too_large", "{}", over.text);
    assert!(h.graph.requests().is_empty());
}

// ─── M3.3 ────────────────────────────────────────────────────────────────

/// Meta refusing the PIN at registration
/// (`support/error-codes`: `133005`, two-step verification PIN mismatch).
fn pin_mismatch() -> Value {
    json!({"error": {"message": "(#133005) Two step verification PIN Mismatch",
        "type": "OAuthException", "code": 133_005, "fbtrace_id": "AbCdEfGhIjK"}})
}

/// M3.3 on `first` and `second`, two processes on the same stores.
async fn m3_3(first: &Harness, second: &Harness) {
    let (a, b) = tenants(first).await;
    let state = start(first, &a).await;
    script_until_verified(&first.graph);
    first.graph.push_json(200, success()); // subscribed_apps
    first.graph.push_json(400, pin_mismatch());
    let stopped = first.call(complete(&a, &finished(&state))).await;
    assert_eq!(stopped.status, StatusCode::BAD_GATEWAY, "{}", stopped.text);
    let error = &stopped.json()["error"];
    assert_eq!(error["code"], "onboarding_failed");
    assert_eq!(error["step"], "register_phone");
    assert_eq!(error["resumable"], true);
    assert_eq!(error["graph"]["code"], 133_005);
    assert_eq!(error["graph"]["fbtrace_id"], "AbCdEfGhIjK");
    assert!(error["graph"]["details"].is_null(), "no details on signup");
    assert!(
        !stopped.text.contains("Mismatch"),
        "Meta's text is not answered"
    );
    assert_no_secret(&stopped.text);
    // The attempt is kept: the WABA, the step.
    let listed = first.call(Call::get("/v1/signup/attempts").key(&a)).await;
    let data = &listed.json()["data"];
    assert_eq!(data.as_array().unwrap().len(), 1, "{}", listed.text);
    assert_eq!(data[0]["waba_id"], WABA);
    assert_eq!(data[0]["failed_step"], "register_phone");
    // Another tenant lists none of it, and resumes nothing: 404, nothing
    // asked of Meta, nothing read from the vault.
    let theirs = first.call(Call::get("/v1/signup/attempts").key(&b)).await;
    assert_eq!(theirs.json()["data"], json!([]));
    let asked = first.graph.requests().len();
    let before = first.kv.vault_reads();
    let stolen = first
        .call(resume(&b, &json!({"waba_id": WABA, "pin": "999999"})))
        .await;
    assert_eq!(
        (stolen.status, stolen.code().as_str()),
        (StatusCode::NOT_FOUND, "nothing_to_resume"),
        "{}",
        stolen.text
    );
    assert_eq!(first.graph.requests().len(), asked);
    assert_eq!(first.kv.vault_reads(), before, "B's resume read no token");
    // A malformed PIN is refused before anything is read, and the attempt
    // survives.
    let short = first
        .call(resume(&a, &json!({"waba_id": WABA, "pin": "12"})))
        .await;
    assert_eq!(short.json()["error"]["field"], "pin", "{}", short.text);
    let bad_id = first
        .call(resume(&a, &json!({"waba_id": "../1", "pin": "123456"})))
        .await;
    assert_eq!(
        bad_id.json()["error"]["field"],
        "waba_id",
        "{}",
        bad_id.text
    );

    // A new process on the same database resumes with a corrected PIN,
    // from the stored token: no code.
    second.graph.push_json(200, success()); // subscribed_apps
    second.graph.push_json(200, success()); // register
    let done = second
        .call(resume(&a, &json!({"waba_id": WABA, "pin": "123456"})))
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    assert_eq!(
        done.json(),
        json!({
            "status": "connected",
            "waba_id": WABA,
            "phone_number_ids": [PHONE, PHONE_2],
            "steps_completed": [
                "load_token", "verify_assets", "approve", "subscribe_app", "register_phone"
            ],
            "needs_coexistence_sync": false
        })
    );
    assert_no_secret(&done.text);
    let requests = second.graph.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|r| !r.path().ends_with("oauth/access_token")),
        "no code exchanged"
    );
    assert_eq!(requests[1].path(), format!("/v25.0/{PHONE}/register"));
    assert_eq!(requests[1].bearer(), Some(TOKEN), "the stored token");
    assert_eq!(
        requests[1].json(),
        Some(json!({"messaging_product": "whatsapp", "pin": "123456"}))
    );
    assert_eq!(second.graph.remaining(), 0);
    // Done: nothing left to resume.
    let again = second
        .call(resume(&a, &json!({"waba_id": WABA, "pin": "123456"})))
        .await;
    assert_eq!(again.code(), "nothing_to_resume");
    assert_eq!(
        second
            .call(Call::get("/v1/signup/attempts").key(&a))
            .await
            .json()["data"],
        json!([])
    );
}

/// M3.3, in memory: a scripted 133005 at registration is `502
/// onboarding_failed` (`register_phone`, resumable); a new process on the
/// same stores resumes with a corrected PIN; another tenant's resume is
/// `404`. Decisive: the attempt kept in the records (not the process), and
/// read for the caller's tenant only.
#[tokio::test]
async fn m3_3_a_133005_at_registration_resumes_in_a_new_process() {
    let stores = Stores::memory();
    let (store, kv) = (stores.store.clone(), stores.kv.clone());
    let first = Harness::with_stores_and_settings(stores, settings(tech_provider()));
    let second = Harness::on_with(store, kv, settings(tech_provider()));
    m3_3(&first, &second).await;
}

/// M3.3 live on Postgres: the second process is another pool on the same
/// database.
#[tokio::test]
async fn live_postgres_m3_3_a_133005_at_registration_resumes_in_a_new_process() {
    use meta_whatsapp_rs::adapters::store::postgres::sqlx;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let first_pool = db.pool(5).await;
    meta_whatsapp_server::store::migrate(&first_pool)
        .await
        .unwrap();
    let second_pool = db.pool(5).await;
    let first =
        Harness::with_stores_and_settings(Stores::postgres(&first_pool), settings(tech_provider()));
    let second = Harness::with_stores_and_settings(
        Stores::postgres(&second_pool),
        settings(tech_provider()),
    );
    m3_3(&first, &second).await;
    // No stored row of any table holds the code, either PIN or the token
    // in clear (the vault's records are sealed).
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables \
         WHERE table_schema = $1 AND table_type = 'BASE TABLE'",
    )
    .bind(&db.schema)
    .fetch_all(&first_pool)
    .await
    .unwrap();
    assert!(tables.iter().any(|t| t == "wa_server_signup_attempts"));
    for table in tables {
        let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT row_to_json(t)::text FROM {}.{table} t",
            db.schema
        )))
        .fetch_all(&first_pool)
        .await
        .unwrap();
        for row in rows {
            assert_no_secret(&row);
            assert!(!row.contains("123456"), "the corrected PIN in {table}");
        }
    }
}

/// A resume whose WABA moves to another tenant, with that tenant's token,
/// right after the resume's first check of the binding: the approval reads
/// the binding again before any step that acts, so the resume is `404`,
/// nothing is asked of Meta with the new holder's token, and the new
/// holder's token and binding are left alone. Decisive: the binding read
/// again in the resume's approval (`approve_held`), not only before it.
#[tokio::test]
async fn a_resume_whose_waba_moves_meanwhile_acts_on_nothing() {
    let moving = Arc::new(Moving::new(Arc::new(MemoryStore::new())));
    let h = Harness::on_with(
        moving.clone(),
        Arc::new(MemoryKvStore::new()),
        settings(tech_provider()),
    );
    let (a, _) = tenants(&h).await;
    let state = start(&h, &a).await;
    script_until_verified(&h.graph);
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(400, pin_mismatch());
    let stopped = h.call(complete(&a, &finished(&state))).await;
    assert_eq!(stopped.status, StatusCode::BAD_GATEWAY, "{}", stopped.text);
    let asked = h.graph.requests().len();
    // Moved to merchant-b once the resume has checked the binding.
    let waba = WabaId::new(WABA).unwrap();
    let (store, vault) = (h.store.clone(), h.vault.clone());
    let to_b = waba.clone();
    moving.after_waba_read(
        1,
        Box::pin(async move {
            move_waba(
                store.as_ref(),
                &vault,
                &to_b,
                &PhoneNumberId::new(PHONE).unwrap(),
                B,
                Some("TOKEN-OF-B"),
            )
            .await;
        }),
    );
    let resumed = h
        .call(resume(&a, &json!({"waba_id": WABA, "pin": "123456"})))
        .await;
    assert!(!moving.armed(), "the move happened");
    assert_eq!(
        (resumed.status, resumed.code().as_str()),
        (StatusCode::NOT_FOUND, "nothing_to_resume"),
        "{}",
        resumed.text
    );
    assert!(!resumed.text.contains("TOKEN-OF-B"));
    assert_eq!(h.graph.requests().len(), asked, "nothing asked of Meta");
    let kept = h.vault.get(&waba).await.unwrap().unwrap();
    assert_eq!(
        kept.token.expose_secret(),
        "TOKEN-OF-B",
        "the new holder's token left alone"
    );
    assert_eq!(
        h.store.waba(&waba).await.unwrap().unwrap().tenant_id,
        tenant(B),
        "the new holder's binding left alone"
    );
}

// ─── M3.4 ────────────────────────────────────────────────────────────────

/// M3.4: partner mode: the credit-line request carries the partner's
/// system token (asserted header), never the merchant's; the system user
/// is added first, in Meta's order (between `subscribe_app` and
/// `register_phone`); the allocation is recorded on the WABA's binding.
/// Decisive: the deployment's `SolutionPartner` (a Tech Provider makes no
/// credit call), and the library's system client.
#[tokio::test]
async fn m3_4_the_credit_line_request_carries_the_partners_system_token() {
    let h = harness(solution_partner());
    let (a, _) = tenants(&h).await;
    let done = onboard_partner(&h, &a).await;
    assert_eq!(done["credit_allocation_id"], ALLOCATION);
    assert_eq!(
        done["steps_completed"],
        json!([
            "exchange_code",
            "debug_token",
            "verify_assets",
            "approve",
            "store_token",
            "subscribe_app",
            "assign_system_user",
            "share_credit_line",
            "register_phone"
        ])
    );
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 9);
    assert_eq!(requests[5].method, Method::POST);
    assert_eq!(requests[5].path(), format!("/v25.0/{WABA}/assigned_users"));
    assert_eq!(requests[5].query("user").as_deref(), Some(SYSTEM_USER));
    assert_eq!(requests[6].method, Method::GET);
    assert_eq!(
        requests[6].path(),
        format!("/v25.0/{LINE}/owning_credit_allocation_configs")
    );
    assert_eq!(
        requests[6].query("receiving_business_id").as_deref(),
        Some(BUSINESS),
        "the owner Meta verified, never the browser's"
    );
    let share = &requests[7];
    assert_eq!(share.method, Method::POST);
    assert_eq!(
        share.path(),
        format!("/v25.0/{LINE}/whatsapp_credit_sharing_and_attach")
    );
    assert_eq!(share.query("waba_id").as_deref(), Some(WABA));
    assert_eq!(share.query("waba_currency").as_deref(), Some("USD"));
    assert_eq!(
        share.header("authorization"),
        Some(format!("Bearer {SYSTEM_TOKEN}").as_str()),
        "the partner's system token"
    );
    for credit in &requests[5..=7] {
        assert_eq!(credit.bearer(), Some(SYSTEM_TOKEN), "{}", credit.path());
        assert_ne!(credit.bearer(), Some(TOKEN), "never the merchant's");
    }
    for merchant in [&requests[4], &requests[8]] {
        assert_eq!(merchant.bearer(), Some(TOKEN), "{}", merchant.path());
    }
    let binding = h
        .store
        .waba(&WabaId::new(WABA).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(binding.credit_allocation_id.as_deref(), Some(ALLOCATION));
    // The ledger outlives the token: remembered for a key rotation.
    let entries = h
        .store
        .vault_entries(&meta_whatsapp_server::model::PageRequest {
            after: None,
            limit: 10,
        })
        .await
        .unwrap();
    let keys: Vec<String> = entries.items.iter().map(VaultEntry::key).collect();
    assert_eq!(
        keys,
        [format!("business/{BUSINESS}"), format!("waba/{WABA}")]
    );

    // A Tech Provider onboarding the same way makes no credit call.
    let tp = harness(tech_provider());
    let (a, _) = tenants(&tp).await;
    onboard_tech_provider(&tp, &a).await;
    assert!(
        tp.graph
            .requests()
            .iter()
            .all(|r| r.bearer() != Some(SYSTEM_TOKEN) && !r.path().contains("credit")),
        "a Tech Provider shares nothing"
    );
}

/// Variables, instead of the process's.
#[derive(Default, Clone)]
struct Vars(HashMap<String, String>);

impl Env for Vars {
    fn var(&self, name: &str) -> Result<Option<String>, NotUnicode> {
        Ok(self.0.get(name).cloned())
    }
    fn read_file(&self, _: &str) -> std::io::Result<String> {
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    }
}

impl Vars {
    fn set(mut self, name: &str, value: &str) -> Self {
        self.0.insert(name.to_owned(), value.to_owned());
        self
    }
    fn unset(mut self, name: &str) -> Self {
        self.0.remove(name);
        self
    }
}

/// M3.4: missing partner settings refuse the start: Solution Partner mode
/// without its system token, system user, credit line or currency is a
/// configuration error (`config.rs` holds every refusal; this is M3.4's
/// own statement of it), and with them the deployment the routes run is a
/// Solution Partner's (the serve's `signup_deployment`); without the app's
/// id or the configuration, the routes are `409 signup_not_configured`.
/// The account update confirmation is `graph` unless set otherwise.
#[tokio::test]
async fn m3_4_missing_partner_settings_refuse_the_start() {
    let partner = Vars::default()
        .set("DATABASE_URL", "postgres://wa:secret@127.0.0.1:5432/wa")
        .set("WA_APP_SECRET", APP_SECRET)
        .set("WA_VERIFY_TOKEN", "verify-token")
        .set(
            "WA_VAULT_KEY",
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
        )
        .set("WA_OTP_PEPPER", "a-pepper-of-at-least-thirty-two-bytes")
        .set("WA_APP_ID", APP_ID)
        .set("WA_ES_CONFIG_ID", CONFIG_ID)
        .set("WA_ONBOARDING_MODE", "solution_partner")
        .set("WA_PARTNER_SYSTEM_TOKEN", SYSTEM_TOKEN)
        .set("WA_PARTNER_SYSTEM_USER_ID", SYSTEM_USER)
        .set("WA_CREDIT_LINE_ID", LINE)
        .set("WA_WABA_CURRENCY", "EUR");
    for missing in [
        "WA_PARTNER_SYSTEM_TOKEN",
        "WA_PARTNER_SYSTEM_USER_ID",
        "WA_CREDIT_LINE_ID",
        "WA_WABA_CURRENCY",
    ] {
        assert_eq!(
            Config::from_env(&partner.clone().unset(missing)).err(),
            Some(ConfigError::PartnerSettingMissing(missing)),
            "{missing}"
        );
    }
    let config = Config::from_env(&partner).unwrap();
    assert!(matches!(
        config.onboarding,
        OnboardingMode::SolutionPartner(_)
    ));
    assert_eq!(
        config.account_update_confirmation,
        AccountUpdateConfirmation::Graph
    );
    let deployment = meta_whatsapp_server::serve::signup_deployment(
        config.app_id.clone(),
        config.app_secrets.first(),
        config.es_config_id.clone(),
        config.onboarding.clone(),
        config.account_update_confirmation,
    )
    .unwrap();
    assert!(deployment.is_solution_partner());
    assert!(
        !format!("{deployment:?}").contains(SYSTEM_TOKEN),
        "no token in its Debug"
    );
    // Without WA_ES_CONFIG_ID: no signup, and the routes say so.
    let none = Config::from_env(&partner.clone().unset("WA_ES_CONFIG_ID")).unwrap();
    assert!(
        meta_whatsapp_server::serve::signup_deployment(
            none.app_id,
            none.app_secrets.first(),
            none.es_config_id,
            none.onboarding,
            none.account_update_confirmation,
        )
        .is_none()
    );
    let h = Harness::new();
    let (a, _) = tenants(&h).await;
    let refused = h
        .call(
            Call::new(Method::POST, "/v1/signup/sessions")
                .key(&a)
                .json(&json!({})),
        )
        .await;
    assert_eq!(refused.code(), "signup_not_configured", "{}", refused.text);
    // The confirmation setting: graph or signature, nothing else.
    let signature = Config::from_env(
        &partner
            .clone()
            .set("WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION", "signature"),
    )
    .unwrap();
    assert_eq!(
        signature.account_update_confirmation,
        AccountUpdateConfirmation::Signature
    );
    assert!(matches!(
        Config::from_env(&partner.set("WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION", "trust")),
        Err(ConfigError::Invalid {
            name: "WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION",
            ..
        })
    ));
}

/// M3.4: D4 is enforced before any credit call, through the library's
/// post-verification gate: a second tenant onboarding a bound WABA gets
/// `409`, nothing is stored, subscribed or shared, binding unchanged.
/// Claiming the WABA is refused before the attempt is redeemed (no
/// request); claiming nothing, Meta's verification finds it and the
/// approval refuses it after Meta's reads only. Decisive: the approval's
/// `bind_waba` outcome (accepting `OwnedByAnotherTenant` overwrites the
/// holder's token and shares B's line).
#[tokio::test]
async fn m3_4_d4_is_enforced_before_any_credit_call() {
    let h = harness(solution_partner());
    let (a, b) = tenants(&h).await;
    onboard_partner(&h, &a).await;
    let waba = WabaId::new(WABA).unwrap();
    let bound = h.store.waba(&waba).await.unwrap().unwrap();
    let (_, version) = h.vault.get_versioned(&waba).await.unwrap().unwrap();
    let asked = h.graph.requests().len();

    let state = start(&h, &b).await;
    let claimed = h.call(complete(&b, &finished(&state))).await;
    assert_eq!(
        (claimed.status, claimed.code().as_str()),
        (StatusCode::CONFLICT, "waba_owned_by_another_tenant"),
        "{}",
        claimed.text
    );
    assert_eq!(h.graph.requests().len(), asked, "refused before Meta");

    script_until_verified(&h.graph);
    let unclaimed = json!({
        "state": state, "code": CODE,
        "event": {"type": "WA_EMBEDDED_SIGNUP", "event": "FINISH_ONLY_WABA",
            "data": {"business_id": BUSINESS}}
    });
    let found = h.call(complete(&b, &unclaimed)).await;
    assert_eq!(
        (found.status, found.code().as_str()),
        (StatusCode::CONFLICT, "waba_owned_by_another_tenant"),
        "{}",
        found.text
    );
    assert_eq!(h.graph.remaining(), 0);
    let after = &h.graph.requests()[asked..];
    assert_eq!(
        after.len(),
        4,
        "the exchange, debug_token, the owner, the numbers"
    );
    assert!(
        after.iter().all(|r| r.method == Method::GET),
        "nothing subscribed, assigned or shared"
    );
    assert!(
        after.iter().all(|r| r.bearer() != Some(SYSTEM_TOKEN)),
        "no credit call"
    );
    assert_eq!(
        h.store.waba(&waba).await.unwrap().unwrap().epoch(),
        bound.epoch(),
        "the binding unchanged"
    );
    let (stored, now) = h.vault.get_versioned(&waba).await.unwrap().unwrap();
    assert_eq!(now, version, "the holder's token not rewritten");
    assert_eq!(stored.token.expose_secret(), TOKEN);
    assert_eq!(
        h.call(Call::get("/v1/signup/attempts").key(&b))
            .await
            .json()["data"],
        json!([])
    );
}

/// M3.4: a business whose line was revoked is not re-funded by `resume` or
/// a new signup without an explicit operator action. A partner onboarding
/// stops at the registration (the attempt kept); the merchant's
/// `PARTNER_REMOVED` revokes the line (confirmed with Graph first); the
/// resume and a new signup each stop at `share_credit_line`, posting no
/// share; the operator's `POST
/// /v1/admin/wabas/{waba_id}/credit-line/reshare` funds it again, once,
/// audited, and finishes the attempt. Decisive: the tenant routes never set
/// `reshare_after_revocation`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn m3_4_a_revoked_business_is_refunded_only_by_the_operator() {
    let h = harness(solution_partner());
    let (a, _) = tenants(&h).await;
    let admin = h.admin_key().await;
    let state = start(&h, &a).await;
    script_until_verified(&h.graph);
    h.graph.push_json(200, success()); // subscribed_apps
    script_share(&h.graph);
    h.graph.push_json(400, pin_mismatch());
    let stopped = h.call(complete(&a, &finished(&state))).await;
    assert_eq!(
        stopped.json()["error"]["step"],
        "register_phone",
        "{}",
        stopped.text
    );

    // The merchant unshares the WABA: confirmed, then revoked at once.
    script_confirmed(&h.graph);
    script_revocation(&h.graph);
    assert_eq!(h.webhook(&removed()).await.status, StatusCode::OK);
    assert_eq!(h.graph.remaining(), 0);
    let business = BusinessId::new(BUSINESS).unwrap();
    assert!(h.vault.revoked_business(&business).await.unwrap().is_some());
    assert!(
        h.vault
            .get(&WabaId::new(WABA).unwrap())
            .await
            .unwrap()
            .is_some(),
        "PARTNER_REMOVED revokes: the token stays"
    );

    // The resume: subscribe, the system user, then the line's check finds
    // the business marked revoked and refuses before any credit call.
    let sent = h.graph.requests().len();
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(200, success()); // assigned_users
    let resumed = h
        .call(resume(&a, &json!({"waba_id": WABA, "pin": "123456"})))
        .await;
    assert_eq!(
        resumed.json()["error"]["code"],
        "onboarding_failed",
        "{}",
        resumed.text
    );
    assert_eq!(resumed.json()["error"]["step"], "share_credit_line");
    // A new signup of the same WABA: the same.
    let state = start(&h, &a).await;
    script_until_verified(&h.graph);
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(200, success()); // assigned_users
    let again = h.call(complete(&a, &finished(&state))).await;
    assert_eq!(
        again.json()["error"]["step"],
        "share_credit_line",
        "{}",
        again.text
    );
    assert_eq!(h.graph.remaining(), 0);
    let shares = |from: usize| {
        h.graph.requests()[from..]
            .iter()
            .filter(|r| r.path().ends_with("whatsapp_credit_sharing_and_attach"))
            .count()
    };
    assert_eq!(shares(sent), 0, "no share posted for a revoked business");
    assert!(
        h.graph.requests()[sent..]
            .iter()
            .all(|r| !r.path().contains("credit")),
        "no credit call at all"
    );

    // The operator's explicit action: funded again, once.
    let before = h.graph.requests().len();
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(200, success()); // assigned_users
    script_already_revoked(&h.graph);
    h.graph.push_json(
        200,
        json!({"allocation_config_id": "58501441721239", "waba_id": WABA}),
    );
    let refunded = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/admin/wabas/{WABA}/credit-line/reshare"),
            )
            .key(&admin),
        )
        .await;
    assert_eq!(refunded.status, StatusCode::OK, "{}", refunded.text);
    assert_eq!(refunded.json()["credit_allocation_id"], "58501441721239");
    assert_eq!(shares(before), 1);
    assert_eq!(h.graph.remaining(), 0);
    assert!(
        h.vault.revoked_business(&business).await.unwrap().is_none(),
        "the re-share cleared the marker"
    );
    assert_eq!(
        h.call(Call::get("/v1/signup/attempts").key(&a))
            .await
            .json()["data"],
        json!([]),
        "the operator's re-share finished the attempt"
    );
    // A tenant's key is not an admin's.
    let tenant_try = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/admin/wabas/{WABA}/credit-line/reshare"),
            )
            .key(&a),
        )
        .await;
    assert_eq!(
        tenant_try.status,
        StatusCode::FORBIDDEN,
        "{}",
        tenant_try.text
    );
}

/// M3.4, the CMS disconnect: in Solution Partner mode `DELETE
/// /v1/wabas/{waba_id}` unsubscribes, revokes the line, then deletes the
/// token and the bindings; a revocation that fails deletes nothing (the
/// answer names `revoke_credit_line`), and repeating the call finishes it.
/// Decisive: the revocation before the deletion (`EmbeddedSignup::offboard`
/// before `unbind_waba_if`; the other order leaves the line funding a WABA
/// nobody holds).
#[tokio::test]
async fn m3_4_a_cms_disconnect_revokes_first_and_deletes_second() {
    let h = harness(solution_partner());
    let (a, _) = tenants(&h).await;
    onboard_partner(&h, &a).await;
    let waba = WabaId::new(WABA).unwrap();
    // Unsubscribed, then Meta unreachable for the revocation: the lookup,
    // the recorded allocation's status and its DELETE.
    h.graph.push_json(200, success());
    for _ in 0..3 {
        h.graph
            .push_error(|| TransportError::Connect(anyhow::anyhow!("unreachable")));
    }
    let disconnect = || Call::new(Method::DELETE, format!("/v1/wabas/{WABA}")).key(&a);
    let failed = h.call(disconnect()).await;
    assert_eq!(failed.status, StatusCode::BAD_GATEWAY, "{}", failed.text);
    assert_eq!(failed.json()["error"]["step"], "revoke_credit_line");
    assert_eq!(h.graph.remaining(), 0);
    assert!(
        h.vault.get(&waba).await.unwrap().is_some(),
        "the token kept"
    );
    assert!(
        h.store.waba(&waba).await.unwrap().is_some(),
        "the binding kept"
    );
    // Again: unsubscribed, revoked, deleted.
    let failed_at = h.graph.requests().len();
    h.graph.push_json(200, success());
    script_revocation(&h.graph);
    let done = h.call(disconnect()).await;
    assert_eq!(done.status, StatusCode::NO_CONTENT, "{}", done.text);
    assert_eq!(h.graph.remaining(), 0);
    let revoked = h.graph.requests()[failed_at..]
        .iter()
        .filter(|r| r.method == Method::DELETE && r.path() == format!("/v25.0/{ALLOCATION}"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(revoked.len(), 1, "the allocation revoked");
    assert_eq!(revoked[0].bearer(), Some(SYSTEM_TOKEN));
    assert!(
        h.vault.get(&waba).await.unwrap().is_none(),
        "the token deleted"
    );
    assert!(h.store.waba(&waba).await.unwrap().is_none(), "unbound");
    assert!(
        h.vault
            .revoked_business(&BusinessId::new(BUSINESS).unwrap())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        h.vault.credit(&waba).await.unwrap().is_some(),
        "the ledger outlives the token"
    );
}

/// The end of either order: the line revoked, the token and the bindings
/// gone.
async fn assert_offboarded(h: &Harness) {
    let waba = WabaId::new(WABA).unwrap();
    assert!(
        h.vault
            .revoked_business(&BusinessId::new(BUSINESS).unwrap())
            .await
            .unwrap()
            .is_some(),
        "revoked"
    );
    assert!(h.vault.get(&waba).await.unwrap().is_none(), "token deleted");
    assert!(h.store.waba(&waba).await.unwrap().is_none(), "unbound");
    for pn in [PHONE, PHONE_2] {
        assert!(
            h.store
                .number(&PhoneNumberId::new(pn).unwrap())
                .await
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// M3.4, the webhooks, uninstall first: `PARTNER_APP_UNINSTALLED` of this
/// app, confirmed with Graph (the stored token no longer valid), revokes
/// (from the owner the vault recorded) before the token and the bindings
/// go; a revocation that fails is answered `500` with nothing deleted, and
/// Meta's redelivery finishes it; the `PARTNER_REMOVED` that follows finds
/// the line revoked already (from the ledger, the token gone).
#[tokio::test]
async fn m3_4_uninstall_then_removal_end_revoked_then_deleted() {
    let h = harness(solution_partner());
    let (a, _) = tenants(&h).await;
    onboard_partner(&h, &a).await;
    let waba = WabaId::new(WABA).unwrap();
    // Meta unreachable during the revocation: redelivered, nothing deleted.
    script_confirmed(&h.graph);
    for _ in 0..3 {
        h.graph
            .push_error(|| TransportError::Connect(anyhow::anyhow!("unreachable")));
    }
    assert_eq!(
        h.webhook(&uninstalled()).await.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a retryable failure is redelivered"
    );
    assert!(
        h.vault.get(&waba).await.unwrap().is_some(),
        "the token kept"
    );
    assert!(
        h.store.waba(&waba).await.unwrap().is_some(),
        "the binding kept"
    );
    // Redelivered: revoked, then deleted.
    script_confirmed(&h.graph);
    script_revocation(&h.graph);
    assert_eq!(h.webhook(&uninstalled()).await.status, StatusCode::OK);
    let requests = h.graph.requests();
    let lookup = requests
        .iter()
        .rev()
        .find(|r| r.path().ends_with("owning_credit_allocation_configs"))
        .unwrap();
    assert_eq!(
        lookup.query("receiving_business_id").as_deref(),
        Some(BUSINESS),
        "the owner the vault recorded"
    );
    // Then the removal: nothing left to confirm with, the line found
    // revoked.
    script_already_revoked(&h.graph);
    assert_eq!(h.webhook(&removed()).await.status, StatusCode::OK);
    assert_offboarded(&h).await;
    // The uninstall reached its tenant, once: recorded while the WABA was
    // still bound, before the offboarding unbound it (the redelivery found
    // it recorded). The removal came after the unbinding: operator-only.
    let events_key = h.tenant_key(A, &[Scope::Events]).await;
    let polled = h
        .call(Call::get("/v1/events").key(&events_key))
        .await
        .json();
    let updates: Vec<&str> = polled["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "account_updated")
        .map(|e| e["data"]["update"]["event"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(updates, ["PARTNER_APP_UNINSTALLED"], "{polled}");
}

/// M3.4, the webhooks, removal first: `PARTNER_REMOVED` revokes at once
/// (decision D14), the token kept; the `PARTNER_APP_UNINSTALLED` that
/// follows finds the line revoked and deletes the token and the bindings.
/// And when the vault recorded no owner, the webhook's `owner_business_id`
/// is the one revoked from.
#[tokio::test]
async fn m3_4_removal_then_uninstall_end_revoked_then_deleted() {
    let h = harness(solution_partner());
    let (a, _) = tenants(&h).await;
    onboard_partner(&h, &a).await;
    script_confirmed(&h.graph);
    script_revocation(&h.graph);
    assert_eq!(h.webhook(&removed()).await.status, StatusCode::OK);
    assert!(
        h.vault
            .get(&WabaId::new(WABA).unwrap())
            .await
            .unwrap()
            .is_some(),
        "revoked, the token kept"
    );
    script_confirmed(&h.graph);
    script_already_revoked(&h.graph);
    assert_eq!(h.webhook(&uninstalled()).await.status, StatusCode::OK);
    assert_offboarded(&h).await;
}

/// The webhook's owner business is revoked from when the vault names none:
/// a `PARTNER_REMOVED` naming no WABA, only its owner, revokes by business
/// (`revoke_business_credit_line`), the line having records for it.
#[tokio::test]
async fn a_removal_naming_only_the_owner_revokes_by_business() {
    let h = harness(solution_partner());
    let (a, _) = tenants(&h).await;
    onboard_partner(&h, &a).await;
    let business = json!({"name": "Wind & Wool", "id": BUSINESS});
    // The business's records (it is the line's), then the revocation.
    h.graph.push_json(
        200,
        json!({"id": ALLOCATION, "receiving_business": business}),
    );
    script_revocation(&h.graph);
    let body = account_update("PARTNER_REMOVED", &json!({"owner_business_id": BUSINESS}));
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    assert_eq!(h.graph.remaining(), 0);
    assert!(
        h.vault
            .revoked_business(&BusinessId::new(BUSINESS).unwrap())
            .await
            .unwrap()
            .is_some()
    );
    let lookups: Vec<String> = h
        .graph
        .requests()
        .iter()
        .filter(|r| r.path().ends_with("owning_credit_allocation_configs"))
        .filter_map(|r| r.query("receiving_business_id"))
        .collect();
    assert!(lookups.iter().all(|b| b == BUSINESS), "{lookups:?}");
}

/// Design section 6: a destructive webhook is confirmed with Graph first.
/// An uninstall Meta's own answer contradicts (the stored token still
/// valid and still granted the WABA) is recorded and not acted on; another
/// partner's app being uninstalled, or a Multi-Partner Solution's removal,
/// is not this deployment's. Decisive: the confirmation (acting on the
/// signature alone deletes the merchant's token on a forged delivery).
#[tokio::test]
async fn an_unconfirmed_or_foreign_account_update_changes_nothing() {
    let h = harness(solution_partner());
    let (a, _) = tenants(&h).await;
    onboard_partner(&h, &a).await;
    let asked = h.graph.requests().len();
    h.graph.push_json(
        200,
        json!({"data": {"app_id": APP_ID, "is_valid": true, "granular_scopes": [
            {"scope": "whatsapp_business_management", "target_ids": [WABA]}]}}),
    );
    assert_eq!(h.webhook(&uninstalled()).await.status, StatusCode::OK);
    assert_eq!(h.graph.requests().len(), asked + 1, "debug_token only");
    let foreign_app = account_update(
        "PARTNER_APP_UNINSTALLED",
        &json!({"waba_id": WABA, "owner_business_id": BUSINESS, "partner_app_id": "869361281603019"}),
    );
    assert_eq!(h.webhook(&foreign_app).await.status, StatusCode::OK);
    let multi = account_update(
        "PARTNER_REMOVED",
        &json!({"waba_id": WABA, "owner_business_id": BUSINESS,
            "solution_id": "1715120619246906",
            "solution_partner_business_ids": ["520744086200222", "506914307656634"]}),
    );
    assert_eq!(h.webhook(&multi).await.status, StatusCode::OK);
    assert_eq!(h.graph.requests().len(), asked + 1, "nothing else asked");
    assert_eq!(h.graph.remaining(), 0);
    let waba = WabaId::new(WABA).unwrap();
    assert!(
        h.vault.get(&waba).await.unwrap().is_some(),
        "the token kept"
    );
    assert!(
        h.store.waba(&waba).await.unwrap().is_some(),
        "the binding kept"
    );
    assert!(
        h.vault
            .revoked_business(&BusinessId::new(BUSINESS).unwrap())
            .await
            .unwrap()
            .is_none(),
        "nothing revoked"
    );
    // The events still reach their tenant.
    let events_key = h.tenant_key(A, &[Scope::Events]).await;
    let events = h.call(Call::get("/v1/events").key(&events_key)).await;
    let polled = events.json();
    let kinds: Vec<&str> = polled["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds.iter().filter(|k| **k == "account_updated").count(),
        3,
        "{}",
        events.text
    );
}

/// A Tech Provider's uninstall deletes the token and the bindings (no
/// credit call), confirmed first; the tenant's own disconnection makes no
/// credit call either (as before roadmap M3a).
#[tokio::test]
async fn a_tech_providers_offboarding_revokes_nothing() {
    let h = harness(tech_provider());
    let (a, _) = tenants(&h).await;
    onboard_tech_provider(&h, &a).await;
    let asked = h.graph.requests().len();
    script_confirmed(&h.graph);
    assert_eq!(h.webhook(&uninstalled()).await.status, StatusCode::OK);
    assert_eq!(h.graph.requests().len(), asked + 1, "debug_token only");
    let waba = WabaId::new(WABA).unwrap();
    assert!(h.vault.get(&waba).await.unwrap().is_none());
    assert!(h.store.waba(&waba).await.unwrap().is_none());

    let tp = harness(tech_provider());
    let (a, _) = tenants(&tp).await;
    onboard_tech_provider(&tp, &a).await;
    let asked = tp.graph.requests().len();
    tp.graph.push_json(200, success());
    let done = tp
        .call(Call::new(Method::DELETE, format!("/v1/wabas/{WABA}")).key(&a))
        .await;
    assert_eq!(done.status, StatusCode::NO_CONTENT, "{}", done.text);
    assert_eq!(tp.graph.requests().len(), asked + 1, "the unsubscribe only");
}

/// The scope: a key without `signup` reaches none of the routes; the
/// tenant is the credential's, never the body's (an unknown field is
/// refused).
#[tokio::test]
async fn the_signup_routes_need_the_signup_scope() {
    let h = harness(tech_provider());
    h.tenant(A).await;
    let numbers_only = h.tenant_key(A, &[Scope::Numbers]).await;
    for call in [
        Call::new(Method::POST, "/v1/signup/sessions").json(&json!({})),
        Call::new(Method::POST, "/v1/signup/complete").json(&json!({})),
        Call::get("/v1/signup/attempts"),
        Call::new(Method::POST, "/v1/signup/resume").json(&json!({})),
    ] {
        let reply = h.call(call.key(&numbers_only)).await;
        assert_eq!(reply.code(), "forbidden", "{}", reply.text);
    }
    let key = h.tenant_key(A, &[Scope::Signup]).await;
    let named = h
        .call(
            Call::new(Method::POST, "/v1/signup/sessions")
                .key(&key)
                .json(&json!({"tenant": B})),
        )
        .await;
    assert_eq!(named.json()["error"]["field"], "body", "{}", named.text);
    assert!(h.graph.requests().is_empty());
}
