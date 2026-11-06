//! Hosted signup, several WABAs in one signup and pre-verified numbers,
//! over HTTP (roadmap M3f; docs/design/server.md, section 4.6, decisions
//! D200 to D205).
//!
//! - **Several WABAs** (`WA_SIGNUP_EVERY_GRANTED_WABA`): with the option
//!   on, a signup granting two WABAs binds both to the tenant; with it
//!   off, one (the primary, `waba_id`), the answer listing the other as
//!   `not_bound_waba_ids`. A WABA held by another tenant is refused
//!   (`409 waba_owned_by_another_tenant`) with nothing bound, and never
//!   taken.
//! - **Hosted signup** (`WA_HOSTED_SIGNUP`): a signed `PARTNER_ADDED` is
//!   remembered, a tenant claims it, the business token is fetched with
//!   the deployment's system token and its `appsecret_proof`.
//! - **Pre-verified numbers**: the pool's routes (admin key), the pool's
//!   token on every call, a tenant's launch offering only its own
//!   reserved ids.
//!
//! The logs' half is `signup_logs.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::signup::settings;
use common::signup::{
    BUSINESS, HOSTED_PROOF, HOSTED_TOKEN, PHONE, PHONE_2, PHONE_3, PIN, POOL_BUSINESS, POOL_TOKEN,
    PRE_VERIFIED, PRE_VERIFIED_CODE, SYSTEM_TOKEN, TOKEN, WABA, WABA_2, complete, finished,
    finished_with, harness, partner_added, script_two_grants, script_until_verified, script_waba,
    start, success, tech_provider,
};
use common::{Call, Harness, Stores, TestDb};
use meta_whatsapp_rs::core::ids::{BusinessId, PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::testing::RecordedBody;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{Scope, TenantId};
use meta_whatsapp_server_core::signup::SignupDeployment;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const A: &str = "merchant-a";
const B: &str = "merchant-b";
const PN_B: &str = "106540352242999";

async fn tenants(h: &Harness) -> (String, String) {
    h.tenant(A).await;
    h.tenant(B).await;
    let scopes = [Scope::Signup, Scope::Numbers];
    (
        h.tenant_key(A, &scopes).await,
        h.tenant_key(B, &scopes).await,
    )
}

async fn holder(h: &Harness, waba: &str) -> Option<String> {
    h.store
        .waba(&WabaId::new(waba).unwrap())
        .await
        .unwrap()
        .map(|binding| binding.tenant_id.as_str().to_owned())
}

async fn stored(h: &Harness, waba: &str) -> bool {
    h.vault
        .get(&WabaId::new(waba).unwrap())
        .await
        .unwrap()
        .is_some()
}

/// Meta's answers for a two-WABA signup with the option on: the shared
/// steps, then the primary WABA's (numbers, subscription, registration),
/// then the other's (numbers, subscription).
fn script_two_wabas(h: &Harness) {
    script_two_grants(&h.graph);
    script_waba(&h.graph, WABA, &[PHONE, PHONE_2]);
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(200, success()); // register
    script_waba(&h.graph, WABA_2, &[PHONE_3]);
    h.graph.push_json(200, success()); // subscribed_apps
}

// ─── Several WABAs ───────────────────────────────────────────────────────

/// M3f decisive: with the option on, a signup granting two WABAs binds
/// both to the tenant, each with its own token record; the primary WABA
/// registers the PIN's number, the other registers nothing. Decisive: the
/// option ignored (one WABA bound), the second WABA's approval skipped.
#[tokio::test]
async fn with_the_option_on_a_signup_granting_two_wabas_binds_both() {
    with_the_option_on_a_signup_granting_two_wabas_binds_both_on(harness(
        tech_provider().every_granted_waba(true),
    ))
    .await;
}

/// The scenario of [`with_the_option_on_a_signup_granting_two_wabas_binds_both`], on any backend.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn with_the_option_on_a_signup_granting_two_wabas_binds_both_on(h: Harness) {
    let (a, _) = tenants(&h).await;
    let state = start(&h, &a).await;
    script_two_wabas(&h);
    let done = h
        .call(complete(&a, &finished_with(&state, &[WABA, WABA_2])))
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    let answer = done.json();
    assert_eq!(answer["status"], "connected");
    assert_eq!(answer["waba_id"], WABA);
    assert_eq!(
        answer["wabas"],
        json!([
            {"waba_id": WABA, "primary": true, "status": "connected",
             "phone_number_ids": [PHONE, PHONE_2], "resumable": false},
            {"waba_id": WABA_2, "primary": false, "status": "connected",
             "phone_number_ids": [PHONE_3], "resumable": false}
        ])
    );
    assert!(answer.get("not_bound_waba_ids").is_none(), "{answer}");
    assert!(!done.text.contains(TOKEN));
    assert_eq!(holder(&h, WABA).await.as_deref(), Some(A));
    assert_eq!(holder(&h, WABA_2).await.as_deref(), Some(A));
    assert!(stored(&h, WABA).await && stored(&h, WABA_2).await);
    let numbers = h.call(Call::get("/v1/numbers").key(&a)).await;
    let ids: Vec<&str> = numbers.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["phone_number_id"].as_str().unwrap().to_owned())
        .map(|n| match n.as_str() {
            PHONE => PHONE,
            PHONE_2 => PHONE_2,
            _ => PHONE_3,
        })
        .collect();
    assert_eq!(ids.len(), 3, "{}", numbers.text);
    let requests = h.graph.requests();
    // exchange, debug, [owner, numbers, subscribe, register], [owner,
    // numbers, subscribe]
    assert_eq!(requests.len(), 9);
    assert_eq!(requests[2].path(), format!("/v25.0/{WABA}"));
    assert_eq!(requests[5].path(), format!("/v25.0/{PHONE}/register"));
    assert_eq!(requests[6].path(), format!("/v25.0/{WABA_2}"));
    assert_eq!(
        requests[8].path(),
        format!("/v25.0/{WABA_2}/subscribed_apps")
    );
    assert!(
        requests
            .iter()
            .all(|r| !r.path().ends_with(&format!("{PHONE_3}/register"))),
        "the other WABA registers nothing"
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// M3f decisive: with the option off, the same signup binds one WABA, the
/// primary (the one the event claims); the answer says which, and lists
/// the other as not bound. Meta is asked nothing about it.
#[tokio::test]
async fn with_the_option_off_it_binds_the_primary_waba_and_says_so() {
    let h = harness(tech_provider());
    let (a, _) = tenants(&h).await;
    let state = start(&h, &a).await;
    script_two_grants(&h.graph);
    script_waba(&h.graph, WABA, &[PHONE, PHONE_2]);
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(200, success()); // register
    let done = h
        .call(complete(&a, &finished_with(&state, &[WABA, WABA_2])))
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    let answer = done.json();
    assert_eq!(answer["waba_id"], WABA);
    assert_eq!(answer["not_bound_waba_ids"], json!([WABA_2]));
    assert!(answer.get("wabas").is_none(), "{answer}");
    assert_eq!(holder(&h, WABA).await.as_deref(), Some(A));
    assert_eq!(holder(&h, WABA_2).await, None);
    assert!(!stored(&h, WABA_2).await);
    assert_eq!(h.graph.requests().len(), 6);
    assert_eq!(h.graph.remaining(), 0);
}

/// M3f decisive: a granted WABA another tenant holds, named by the
/// event, refuses the whole signup (`409 waba_owned_by_another_tenant`)
/// before the attempt is redeemed or Meta asked: nothing is bound, the
/// other tenant's binding and token are untouched, and the attempt still
/// completes without that WABA.
#[tokio::test]
async fn a_granted_waba_another_tenant_holds_refuses_the_signup_with_nothing_bound() {
    a_granted_waba_another_tenant_holds_refuses_the_signup_with_nothing_bound_on(harness(
        tech_provider().every_granted_waba(true),
    ))
    .await;
}

/// The scenario of [`a_granted_waba_another_tenant_holds_refuses_the_signup_with_nothing_bound`], on any backend.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn a_granted_waba_another_tenant_holds_refuses_the_signup_with_nothing_bound_on(h: Harness) {
    let (a, _) = tenants(&h).await;
    h.connect(B, WABA_2, &[PN_B], "TOKEN-OF-B").await;
    let state = start(&h, &a).await;
    let refused = h
        .call(complete(&a, &finished_with(&state, &[WABA, WABA_2])))
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.text);
    assert_eq!(refused.code(), "waba_owned_by_another_tenant");
    assert!(h.graph.requests().is_empty(), "Meta was not asked");
    assert_eq!(holder(&h, WABA).await, None, "nothing bound");
    assert!(!stored(&h, WABA).await, "nothing stored");
    assert_eq!(holder(&h, WABA_2).await.as_deref(), Some(B));
    let token = h
        .vault
        .get(&WabaId::new(WABA_2).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(token.token.expose_secret(), "TOKEN-OF-B");
    // The attempt was not burnt: it completes with the WABA the tenant
    // can have.
    script_until_verified(&h.graph);
    h.graph.push_json(200, success());
    h.graph.push_json(200, success());
    let done = h.call(complete(&a, &finished(&state))).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
}

/// A grant the event did not name, held by another tenant (token and
/// binding), is left alone: `held`, never replaced, and the tenant's
/// primary WABA is bound. One the vault does not hold but another tenant
/// is bound to is refused at its approval (`refused`,
/// `waba_owned_by_another_tenant`): its binding stays the other tenant's.
#[tokio::test]
async fn an_unnamed_grant_another_tenant_holds_is_left_alone() {
    let h = harness(tech_provider().every_granted_waba(true));
    let (a, _) = tenants(&h).await;

    // Held: B's token is in the vault.
    h.connect(B, WABA_2, &[PN_B], "TOKEN-OF-B").await;
    let state = start(&h, &a).await;
    script_two_grants(&h.graph);
    script_waba(&h.graph, WABA, &[PHONE, PHONE_2]);
    h.graph.push_json(200, success());
    h.graph.push_json(200, success());
    let done = h.call(complete(&a, &finished(&state))).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    let wabas = done.json()["wabas"].clone();
    assert_eq!(wabas[0]["status"], "connected");
    assert_eq!(wabas[1]["waba_id"], WABA_2);
    assert_eq!(wabas[1]["status"], "held", "{wabas}");
    assert_eq!(holder(&h, WABA_2).await.as_deref(), Some(B));
    let token = h
        .vault
        .get(&WabaId::new(WABA_2).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(token.token.expose_secret(), "TOKEN-OF-B");

    // Refused: bound to B, no token in the vault.
    let h = harness(tech_provider().every_granted_waba(true));
    let (a, _) = tenants(&h).await;
    h.store
        .bind_waba(
            &TenantId::parse(B).unwrap(),
            &WabaId::new(WABA_2).unwrap(),
            &[PhoneNumberId::new(PN_B).unwrap()],
        )
        .await
        .unwrap();
    let state = start(&h, &a).await;
    script_two_grants(&h.graph);
    script_waba(&h.graph, WABA, &[PHONE, PHONE_2]);
    h.graph.push_json(200, success());
    h.graph.push_json(200, success());
    script_waba(&h.graph, WABA_2, &[PHONE_3]);
    let done = h.call(complete(&a, &finished(&state))).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    let wabas = done.json()["wabas"].clone();
    assert_eq!(wabas[1]["status"], "refused", "{wabas}");
    assert_eq!(wabas[1]["code"], "waba_owned_by_another_tenant");
    assert_eq!(holder(&h, WABA_2).await.as_deref(), Some(B), "not taken");
    assert!(!stored(&h, WABA_2).await, "nothing stored for it");
    assert_eq!(holder(&h, WABA).await.as_deref(), Some(A));
}

/// A second WABA that fails after its token was stored leaves the first
/// connected, and is kept as an attempt `POST /v1/signup/resume` finishes.
#[tokio::test]
async fn a_second_wabas_failure_leaves_the_first_and_is_resumable() {
    let h = harness(tech_provider().every_granted_waba(true));
    let (a, _) = tenants(&h).await;
    let state = start(&h, &a).await;
    script_two_grants(&h.graph);
    script_waba(&h.graph, WABA, &[PHONE, PHONE_2]);
    h.graph.push_json(200, success());
    h.graph.push_json(200, success());
    script_waba(&h.graph, WABA_2, &[PHONE_3]);
    h.graph.push_json(
        500,
        json!({"error": {"message": "Unknown error", "type": "OAuthException", "code": 1, "fbtrace_id": "T"}}),
    );
    let done = h
        .call(complete(&a, &finished_with(&state, &[WABA, WABA_2])))
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    let wabas = done.json()["wabas"].clone();
    assert_eq!(wabas[0]["status"], "connected");
    assert_eq!(wabas[1]["status"], "failed", "{wabas}");
    assert_eq!(wabas[1]["failed_step"], "subscribe_app");
    assert_eq!(wabas[1]["resumable"], true);
    assert_eq!(holder(&h, WABA_2).await.as_deref(), Some(A));
    let attempts = h.call(Call::get("/v1/signup/attempts").key(&a)).await;
    assert_eq!(attempts.json()["data"][0]["waba_id"], WABA_2);
    h.graph.push_json(200, success()); // subscribed_apps
    let resumed = h
        .call(common::signup::resume(&a, &json!({"waba_id": WABA_2})))
        .await;
    assert_eq!(resumed.status, StatusCode::OK, "{}", resumed.text);
}

// ─── Hosted signup ───────────────────────────────────────────────────────

fn hosted_body(pin: Option<&str>) -> Value {
    match pin {
        Some(pin) => json!({"waba_id": WABA, "owner_business_id": BUSINESS, "pin": pin}),
        None => json!({"waba_id": WABA, "owner_business_id": BUSINESS}),
    }
}

fn claim(key: &str, body: &Value) -> Call {
    Call::new(Method::POST, "/v1/signup/hosted")
        .key(key)
        .json(body)
}

fn hosted_deployment() -> SignupDeployment {
    tech_provider().hosted_signup(AccessToken::new(HOSTED_TOKEN))
}

fn hosted() -> Harness {
    harness(hosted_deployment())
}

/// Meta's answers to a hosted onboarding after the token fetch.
fn script_hosted(h: &Harness) {
    h.graph.push_json(200, json!({"access_token": TOKEN}));
    h.graph.push_json(200, common::signup::one_grant_debug());
    script_waba(&h.graph, WABA, &[PHONE, PHONE_2]);
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(200, success()); // register
}

/// A signed `PARTNER_ADDED` is remembered and asks Meta nothing; the
/// tenant that claims it gets the WABA bound, its business token fetched
/// with the system token and the `appsecret_proof` of the app secret (the
/// hosted page's step 5), stored in the vault, never answered. Decisive:
/// the proof (a wrong secret changes it), the token in the vault, the
/// binding in the approval.
#[tokio::test]
async fn a_partner_added_is_claimed_by_a_tenant_and_onboarded() {
    a_partner_added_is_claimed_by_a_tenant_and_onboarded_on(hosted()).await;
}

/// The scenario of [`a_partner_added_is_claimed_by_a_tenant_and_onboarded`], on any backend.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn a_partner_added_is_claimed_by_a_tenant_and_onboarded_on(h: Harness) {
    let (a, _) = tenants(&h).await;
    let arrived = h.webhook(&partner_added(WABA, BUSINESS)).await;
    assert_eq!(arrived.status, StatusCode::OK);
    assert!(
        h.graph.requests().is_empty(),
        "the arrival asks Meta nothing"
    );
    assert_eq!(holder(&h, WABA).await, None, "nothing bound by a webhook");
    script_hosted(&h);
    let done = h.call(claim(&a, &hosted_body(Some(PIN)))).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    let answer = done.json();
    assert_eq!(answer["status"], "connected");
    assert_eq!(answer["waba_id"], WABA);
    assert_eq!(answer["phone_number_ids"], json!([PHONE, PHONE_2]));
    assert_eq!(answer["steps_completed"][0], "fetch_token");
    for secret in [TOKEN, HOSTED_TOKEN, PIN] {
        assert!(!done.text.contains(secret), "{secret} answered");
    }
    let requests = h.graph.requests();
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(
        requests[0].path(),
        format!("/v25.0/{BUSINESS}/system_user_access_tokens")
    );
    assert_eq!(requests[0].bearer(), Some(HOSTED_TOKEN));
    let RecordedBody::Bytes { content_type, data } = &requests[0].body else {
        panic!("a buffered form body");
    };
    assert_eq!(content_type, "application/x-www-form-urlencoded");
    assert_eq!(
        std::str::from_utf8(data).unwrap(),
        format!("appsecret_proof={HOSTED_PROOF}&fetch_only=true"),
        "nothing else in the body, nothing in the URL"
    );
    assert_eq!(requests[0].url.query(), None);
    assert_eq!(requests[1].path(), "/v25.0/debug_token");
    for request in &requests[2..] {
        assert_eq!(request.bearer(), Some(TOKEN), "{}", request.path());
    }
    assert_eq!(
        requests.last().unwrap().path(),
        format!("/v25.0/{PHONE}/register")
    );
    assert_eq!(holder(&h, WABA).await.as_deref(), Some(A));
    assert!(stored(&h, WABA).await);
    assert_eq!(h.graph.remaining(), 0);
    // Claimed once: the arrival is gone.
    let again = h.call(claim(&a, &hosted_body(Some(PIN)))).await;
    assert_eq!(again.code(), "not_found", "{}", again.text);
    assert_eq!(h.graph.requests().len(), requests.len());
}

/// Nothing is claimable without an arrival, or under another business;
/// the same `404` as no WABA. Another tenant's WABA is `409` before
/// anything is read; a deployment without hosted signup remembers nothing
/// and answers `409 signup_not_configured`.
#[tokio::test]
async fn a_claim_needs_an_arrival_and_respects_ownership() {
    let h = hosted();
    let (a, b) = tenants(&h).await;
    let none = h.call(claim(&a, &hosted_body(Some(PIN)))).await;
    assert_eq!(none.code(), "not_found", "{}", none.text);
    h.webhook(&partner_added(WABA, BUSINESS)).await;
    let other_business = json!({"waba_id": WABA, "owner_business_id": "1972555232742222"});
    let wrong = h.call(claim(&a, &other_business)).await;
    assert_eq!(wrong.code(), "not_found", "{}", wrong.text);
    // Inputs first, before the arrival is touched.
    let bad_pin = h.call(claim(&a, &hosted_body(Some("12")))).await;
    assert_eq!(bad_pin.code(), "invalid_request");
    assert_eq!(bad_pin.json()["error"]["field"], "pin");
    let bad_id = h
        .call(claim(
            &a,
            &json!({"waba_id": "x/y", "owner_business_id": BUSINESS}),
        ))
        .await;
    assert_eq!(bad_id.code(), "invalid_request");
    assert!(h.graph.requests().is_empty());
    // B holds the WABA: A is refused before the arrival is claimed.
    h.connect(B, WABA, &[PN_B], "TOKEN-OF-B").await;
    let refused = h.call(claim(&a, &hosted_body(Some(PIN)))).await;
    assert_eq!(refused.code(), "waba_owned_by_another_tenant");
    assert!(h.graph.requests().is_empty());
    assert_eq!(holder(&h, WABA).await.as_deref(), Some(B));
    let _ = b;

    // Off: the webhook is not remembered, the route is not configured.
    let off = harness(tech_provider());
    let (a, _) = tenants(&off).await;
    off.webhook(&partner_added(WABA, BUSINESS)).await;
    let unconfigured = off.call(claim(&a, &hosted_body(Some(PIN)))).await;
    assert_eq!(unconfigured.code(), "signup_not_configured");
}

/// A `PARTNER_ADDED` for a WABA a tenant holds (the code flow sends one
/// too) is not remembered, and a delivery twice is one arrival. A claim
/// that stopped before the token was stored is put back, to claim again.
#[tokio::test]
async fn a_held_waba_is_not_remembered_and_a_failed_claim_is_put_back() {
    let h = hosted();
    let (a, _) = tenants(&h).await;
    h.connect(A, WABA, &[PHONE], "TOKEN-OF-A").await;
    h.webhook(&partner_added(WABA, BUSINESS)).await;
    let held = h.call(claim(&a, &hosted_body(None))).await;
    assert_eq!(held.code(), "not_found", "{}", held.text);

    let h = hosted();
    let (a, _) = tenants(&h).await;
    h.webhook(&partner_added(WABA, BUSINESS)).await;
    h.webhook(&partner_added(WABA, BUSINESS)).await;
    // The fetch fails: nothing stored, the arrival is claimable again.
    h.graph.push_json(
        400,
        json!({"error": {"message": "x", "type": "OAuthException", "code": 100, "fbtrace_id": "T"}}),
    );
    let failed = h.call(claim(&a, &hosted_body(Some(PIN)))).await;
    assert_eq!(failed.status, StatusCode::BAD_GATEWAY, "{}", failed.text);
    assert_eq!(failed.code(), "onboarding_failed");
    assert_eq!(failed.json()["error"]["resumable"], false);
    assert_eq!(holder(&h, WABA).await, None);
    script_hosted(&h);
    let done = h.call(claim(&a, &hosted_body(Some(PIN)))).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
}

// ─── Pre-verified numbers ────────────────────────────────────────────────

fn pooled() -> Harness {
    harness(tech_provider().pre_verified_pool(
        BusinessId::new(POOL_BUSINESS).unwrap(),
        AccessToken::new(POOL_TOKEN),
    ))
}

/// The pool's routes are the operator's: each call is Meta's, with the
/// pool's token and the documented body; a tenant key is `403`; the code
/// is in no answer.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn the_pool_is_managed_with_the_pools_token_by_the_operator() {
    let h = pooled();
    let (a, _) = tenants(&h).await;
    let admin = h.admin_key().await;
    let route = "/v1/admin/pre-verified-numbers";

    let added = {
        h.graph.push_json(200, json!({"id": PRE_VERIFIED}));
        h.call(
            Call::new(Method::POST, route)
                .key(&admin)
                .json(&json!({"phone_number": "+1 (555) 078-3881"})),
        )
        .await
    };
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.text);
    assert_eq!(added.json(), json!({"id": PRE_VERIFIED}));
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(
        request.path(),
        format!("/v25.0/{POOL_BUSINESS}/add_phone_numbers")
    );
    assert_eq!(request.bearer(), Some(POOL_TOKEN));
    assert_eq!(
        request.json(),
        Some(json!({"phone_number": "+1 (555) 078-3881"}))
    );

    h.graph.push_json(200, success());
    let code = h
        .call(
            Call::new(Method::POST, format!("{route}/{PRE_VERIFIED}/code"))
                .key(&admin)
                .json(&json!({"method": "SMS", "language": "en_US"})),
        )
        .await;
    assert_eq!(code.status, StatusCode::NO_CONTENT, "{}", code.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(
        request.path(),
        format!("/v25.0/{PRE_VERIFIED}/request_code")
    );
    assert_eq!(request.bearer(), Some(POOL_TOKEN));
    assert_eq!(
        request.json(),
        Some(json!({"code_method": "SMS", "language": "en_US"}))
    );

    h.graph.push_json(200, success());
    let verified = h
        .call(
            Call::new(Method::POST, format!("{route}/{PRE_VERIFIED}/verify"))
                .key(&admin)
                .json(&json!({"code": PRE_VERIFIED_CODE})),
        )
        .await;
    assert_eq!(verified.status, StatusCode::NO_CONTENT, "{}", verified.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.path(), format!("/v25.0/{PRE_VERIFIED}/verify_code"));
    assert_eq!(request.json(), Some(json!({"code": PRE_VERIFIED_CODE})));
    assert!(!verified.text.contains(PRE_VERIFIED_CODE));

    // A refused code is never quoted.
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("wrong code {PRE_VERIFIED_CODE}"), "type": "OAuthException", "code": 100, "fbtrace_id": "T"}}),
    );
    let wrong = h
        .call(
            Call::new(Method::POST, format!("{route}/{PRE_VERIFIED}/verify"))
                .key(&admin)
                .json(&json!({"code": PRE_VERIFIED_CODE})),
        )
        .await;
    assert!(wrong.status.is_client_error() || wrong.status.is_server_error());
    assert!(!wrong.text.contains(PRE_VERIFIED_CODE), "{}", wrong.text);

    h.graph.push_json(
        200,
        json!({"id": PRE_VERIFIED, "phone_number": "+15550783881",
               "code_verification_status": "VERIFIED", "verification_expiry_time": 1_790_000_000}),
    );
    let one = h
        .call(Call::get(format!("{route}/{PRE_VERIFIED}")).key(&admin))
        .await;
    assert_eq!(one.status, StatusCode::OK, "{}", one.text);
    assert_eq!(one.json()["code_verification_status"], "VERIFIED");

    h.graph.push_json(
        200,
        json!({"data": [{"id": PRE_VERIFIED, "code_verification_status": "VERIFIED"}],
               "paging": {"cursors": {"after": "NEXT"}, "next": "https://graph.facebook.com/x"}}),
    );
    let listed = h
        .call(Call::get(format!("{route}?status=VERIFIED&limit=5")).key(&admin))
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(listed.json()["data"][0]["id"], PRE_VERIFIED);
    assert_eq!(listed.json()["next_cursor"], "NEXT");
    let request = h.graph.last_request().unwrap();
    assert_eq!(
        request.path(),
        format!("/v25.0/{POOL_BUSINESS}/preverified_numbers")
    );
    assert_eq!(
        request.query("code_verification_status").as_deref(),
        Some("VERIFIED")
    );
    assert_eq!(request.query("limit").as_deref(), Some("5"));
    let unlisted = h
        .call(Call::get(format!("{route}?status=UNVERIFIED")).key(&admin))
        .await;
    assert_eq!(unlisted.code(), "invalid_request");

    // The operator's key only; and ids that are not digits never reach Meta.
    let asked = h.graph.requests().len();
    let tenant = h.call(Call::get(route).key(&a)).await;
    assert_eq!(tenant.status, StatusCode::FORBIDDEN);
    let hostile = h
        .call(Call::get(format!("{route}/12%2F34")).key(&admin))
        .await;
    assert_eq!(hostile.code(), "invalid_request");
    assert_eq!(h.graph.requests().len(), asked);

    h.graph.push_json(200, success());
    let deleted = h
        .call(Call::new(Method::DELETE, format!("{route}/{PRE_VERIFIED}")).key(&admin))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::DELETE);
    assert_eq!(request.path(), format!("/v25.0/{PRE_VERIFIED}"));
    assert_eq!(request.bearer(), Some(POOL_TOKEN));
    assert_eq!(h.graph.remaining(), 0);

    // Without a pool: not configured.
    let off = harness(tech_provider());
    let admin = off.admin_key().await;
    let none = off.call(Call::get(route).key(&admin)).await;
    assert_eq!(none.code(), "signup_not_configured");
}

/// A launch offers a pre-verified number only to the tenant it is
/// reserved for: an unreserved id, another tenant's and a malformed one
/// are the same `422` on `pre_verified_phone_ids`; a reserved one reaches
/// `launch_options` as Meta's `preVerifiedPhone`.
#[tokio::test]
async fn a_launch_offers_only_the_ids_reserved_for_the_tenant() {
    let h = pooled();
    let (a, b) = tenants(&h).await;
    let admin = h.admin_key().await;
    let launch = |key: &str, ids: &Value| {
        Call::new(Method::POST, "/v1/signup/sessions")
            .key(key)
            .json(&json!({"pre_verified_phone_ids": ids}))
    };
    let unreserved = h.call(launch(&a, &json!([PRE_VERIFIED]))).await;
    assert_eq!(unreserved.code(), "invalid_request", "{}", unreserved.text);
    assert_eq!(
        unreserved.json()["error"]["field"],
        "pre_verified_phone_ids"
    );

    let reserved = h
        .call(
            Call::new(
                Method::PUT,
                format!("/v1/admin/pre-verified-numbers/{PRE_VERIFIED}/reservation"),
            )
            .key(&admin)
            .json(&json!({"tenant_id": A})),
        )
        .await;
    assert_eq!(reserved.status, StatusCode::NO_CONTENT, "{}", reserved.text);
    let unknown_tenant = h
        .call(
            Call::new(
                Method::PUT,
                "/v1/admin/pre-verified-numbers/1972385232742998/reservation",
            )
            .key(&admin)
            .json(&json!({"tenant_id": "nobody"})),
        )
        .await;
    assert_eq!(unknown_tenant.code(), "not_found");

    let mine = h.call(launch(&a, &json!([PRE_VERIFIED]))).await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.text);
    assert_eq!(
        mine.json()["launch_options"]["extras"]["setup"]["preVerifiedPhone"],
        json!({"ids": [PRE_VERIFIED]}),
        "{}",
        mine.text
    );
    let theirs = h.call(launch(&b, &json!([PRE_VERIFIED]))).await;
    assert_eq!(theirs.code(), "invalid_request");
    assert_eq!(
        theirs.json()["error"]["field"],
        unreserved.json()["error"]["field"]
    );
    assert_eq!(
        theirs.json()["error"]["message"],
        unreserved.json()["error"]["message"]
    );
    let hostile = h.call(launch(&a, &json!(["1/2"]))).await;
    assert_eq!(hostile.code(), "invalid_request");

    // Released: no longer offered.
    let released = h
        .call(
            Call::new(
                Method::DELETE,
                format!("/v1/admin/pre-verified-numbers/{PRE_VERIFIED}/reservation"),
            )
            .key(&admin),
        )
        .await;
    assert_eq!(released.status, StatusCode::NO_CONTENT);
    let after = h.call(launch(&a, &json!([PRE_VERIFIED]))).await;
    assert_eq!(after.code(), "invalid_request");
    assert!(
        h.graph.requests().is_empty(),
        "a reservation asks Meta nothing"
    );
    let _ = (SYSTEM_TOKEN, PHONE_2);
}

// ─── Live on Postgres ────────────────────────────────────────────────────

/// The decisive scenarios of M3f on Postgres (the records, the token
/// vault and the key/value store the arrivals live in): two WABAs bound to
/// one tenant, a WABA another tenant holds refusing a signup with nothing
/// bound, a hosted arrival claimed once. Each on a schema of its own.
#[tokio::test]
async fn live_postgres_m3f_several_wabas_the_refusal_and_a_hosted_claim() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(5).await;
    meta_whatsapp_server::store::migrate(&pool).await.unwrap();
    let h = Harness::with_stores_and_settings(
        Stores::postgres(&pool),
        settings(tech_provider().every_granted_waba(true)),
    );
    with_the_option_on_a_signup_granting_two_wabas_binds_both_on(h).await;

    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(5).await;
    meta_whatsapp_server::store::migrate(&pool).await.unwrap();
    let h = Harness::with_stores_and_settings(
        Stores::postgres(&pool),
        settings(tech_provider().every_granted_waba(true)),
    );
    a_granted_waba_another_tenant_holds_refuses_the_signup_with_nothing_bound_on(h).await;

    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(5).await;
    meta_whatsapp_server::store::migrate(&pool).await.unwrap();
    let h =
        Harness::with_stores_and_settings(Stores::postgres(&pool), settings(hosted_deployment()));
    a_partner_added_is_claimed_by_a_tenant_and_onboarded_on(h).await;
}
