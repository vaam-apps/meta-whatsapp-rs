//! The `embedded_signup` example's app, driven in-process with
//! `tower::ServiceExt::oneshot`: the launch page and options, the tenant
//! binding of an attempt, a full onboarding against a scripted Graph API,
//! and recovery with `/signup/resume`.
//!
//! Like `cms_inbox.rs`, this compiles the example file itself. The Graph
//! responses are the ones `wa-client`'s onboarding tests take from the docs
//! (`embedded-signup/*`, `access-tokens`, `solution-providers/*`).

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(all(feature = "reqwest", feature = "memory", feature = "axum"))]

#[allow(dead_code)]
#[path = "../examples/embedded_signup.rs"]
mod example;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::{BodyExt, Limited};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tower::ServiceExt;
use wa_rs::adapters::store::MemoryKvStore;
use wa_rs::client::embedded_signup::{TokenVault, VaultKey, VaultKeys};
use wa_rs::client::phone_numbers::TwoStepPin;
use wa_rs::core::testing::ScriptedTransport;
use wa_rs::prelude::*;

const APP_ID: &str = "236484624622562";
const APP_SECRET: &str = "614fc2afde15eee07a26b2fe3eaee9b9";
const CONFIG_ID: &str = "<CONFIGURATION_ID>";
const CODE: &str = "AQBhlXsctMxJYbwbrpybxlo9tLPGy";
const TOKEN: &str = "EAAAN6tcBzAUBOwtDtTfmZCJ9n3FHpSDcDTH86ekf89Xnn";
/// `embedded-signup/implementation`, session logging example values.
const WABA: &str = "524126980791429";
const PHONE: &str = "106540352242922";
const BUSINESS: &str = "2729063490586005";
const PIN: &str = "581063";

const BODY_LIMIT: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);

struct Harness {
    app: Router,
    graph: ScriptedTransport,
    vault: TokenVault,
    signup: example::Signup,
}

fn harness() -> Harness {
    let graph = ScriptedTransport::new();
    let client = Client::builder()
        .transport(graph.clone())
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap();
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
    let vault = TokenVault::new(
        kv.clone(),
        VaultKeys::new(VaultKey::generate("test").unwrap()),
    )
    .unwrap();
    let signup = example::Signup::new(
        client.embedded_signup(AppCredentials::new(APP_ID, APP_SECRET)),
        kv,
        vault.clone(),
        CONFIG_ID,
        Some(TwoStepPin::new(PIN).unwrap()),
    );
    Harness {
        app: example::app(signup.clone()),
        graph,
        vault,
        signup,
    }
}

async fn call(app: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = tokio::time::timeout(
        READ_TIMEOUT,
        Limited::new(response.into_body(), BODY_LIMIT).collect(),
    )
    .await
    .expect("the response body ends")
    .expect("the response body fits the limit")
    .to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

async fn call_json(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let (status, body) = call(app, request).await;
    let json = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{e}: {body}"));
    (status, json)
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn post(uri: &str, body: &Value) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// A new attempt for `tenant`; returns its state.
async fn start(app: &Router, tenant: &str) -> String {
    let (status, body) = call_json(app, get(&format!("/signup/start?tenant={tenant}"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["state"].as_str().unwrap().to_owned()
}

/// What the page posts after a successful Cloud API flow.
fn finished(state: &str) -> Value {
    json!({
        "state": state,
        "code": CODE,
        "event": {
            "data": {"phone_number_id": PHONE, "waba_id": WABA, "business_id": BUSINESS},
            "type": "WA_EMBEDDED_SIGNUP",
            "event": "FINISH"
        }
    })
}

fn complete(tenant: &str, body: &Value) -> Request<Body> {
    post(&format!("/signup/complete?tenant={tenant}"), body)
}

/// The onboarding answers up to and including `verify_assets`.
fn script_until_verified(graph: &ScriptedTransport) {
    graph.push_json(200, json!({"access_token": TOKEN, "token_type": "bearer"}));
    graph.push_json(
        200,
        json!({"data": {
          "app_id": APP_ID, "type": "SYSTEM_USER", "application": "Jaspers", "is_valid": true,
          "expires_at": 0, "data_access_expires_at": 0,
          "scopes": ["whatsapp_business_management", "whatsapp_business_messaging"],
          "granular_scopes": [
            {"scope": "whatsapp_business_management", "target_ids": [WABA]},
            {"scope": "whatsapp_business_messaging", "target_ids": [WABA]}
          ],
          "user_id": "1"
        }}),
    );
    graph.push_json(
        200,
        json!({"owner_business_info": {"name": "Wind & Wool", "id": BUSINESS}, "id": WABA}),
    );
    graph.push_json(200, json!({"data": [{"id": PHONE}]}));
}

fn success() -> Value {
    json!({"success": true})
}

#[tokio::test]
async fn the_page_and_start_give_fb_login_what_it_needs() {
    let Harness { app, graph, .. } = harness();

    let (status, page) = call(&app, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains(&format!("appId: '{APP_ID}'")), "{page}");
    assert!(page.contains("version: 'v25.0'"), "{page}");
    assert!(!page.contains("__"), "a placeholder was left: {page}");

    let (status, body) = call_json(&app, get("/signup/start?tenant=merchant-a")).await;
    assert_eq!(status, StatusCode::OK);
    // embedded-signup/implementation, "Launch method and callback registration".
    assert_eq!(
        body["launch_options"],
        json!({
          "config_id": CONFIG_ID,
          "response_type": "code",
          "override_default_response_type": true,
          "extras": {"setup": {}}
        })
    );
    let state = body["state"].as_str().unwrap();
    assert_eq!(state.len(), 22, "128 random bits, base64url");
    assert!(graph.requests().is_empty());
}

#[tokio::test]
async fn a_finished_signup_is_onboarded_for_the_merchant_who_started_it() {
    let Harness {
        app,
        graph,
        vault,
        signup,
    } = harness();
    let state = start(&app, "merchant-a").await;

    // Another merchant presenting it: refused, nothing sent, not burnt.
    let (status, body) = call_json(&app, complete("merchant-b", &finished(&state))).await;
    assert_eq!(
        (status, body),
        (StatusCode::FORBIDDEN, json!({"error": "stale_attempt"}))
    );
    assert!(graph.requests().is_empty());

    script_until_verified(&graph);
    graph.push_json(200, success()); // subscribe
    graph.push_json(200, success()); // register
    let (status, body) = call_json(&app, complete("merchant-a", &finished(&state))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({
            "status": "connected",
            "waba_id": WABA,
            "phone_number_ids": [PHONE],
            "steps_completed": [
                "exchange_code", "debug_token", "verify_assets",
                "store_token", "subscribe_app", "register_phone"
            ],
            "needs_coexistence_sync": false
        })
    );
    assert!(
        !body.to_string().contains(TOKEN),
        "the token reached the page"
    );

    let requests = graph.requests();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[0].path(), "/v25.0/oauth/access_token");
    assert_eq!(requests[0].query("client_id").as_deref(), Some(APP_ID));
    assert_eq!(requests[0].query("code").as_deref(), Some(CODE));
    assert_eq!(requests[0].header("authorization"), None);
    assert_eq!(requests[4].path(), format!("/v25.0/{WABA}/subscribed_apps"));
    assert_eq!(requests[4].bearer(), Some(TOKEN));
    assert_eq!(requests[5].method, Method::POST);
    assert_eq!(requests[5].path(), format!("/v25.0/{PHONE}/register"));
    assert_eq!(
        requests[5].json(),
        Some(json!({"messaging_product": "whatsapp", "pin": PIN}))
    );
    assert_eq!(graph.remaining(), 0);

    // The token is in the vault, routed by the number: what `cms_inbox` uses.
    let stored = vault
        .get_by_phone_number(&PhoneNumberId::new(PHONE))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.token.expose_secret(), TOKEN);
    assert_eq!(
        signup.merchants.lock().unwrap().get("merchant-a"),
        Some(&WabaId::new(WABA))
    );

    // The state was single use.
    let (status, _) = call(&app, complete("merchant-a", &finished(&state))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(graph.requests().len(), 6);
}

#[tokio::test]
async fn a_wrong_pin_stops_at_register_and_resume_finishes_with_a_new_one() {
    let Harness { app, graph, .. } = harness();
    let state = start(&app, "merchant-a").await;
    script_until_verified(&graph);
    graph.push_json(200, success()); // subscribe
    graph.push_json(
        400,
        json!({"error": {"message": "(#133005) Two step verification PIN Mismatch", "type": "OAuthException", "code": 133005, "fbtrace_id": "A"}}),
    );
    let (status, body) = call_json(&app, complete("merchant-a", &finished(&state))).await;
    assert_eq!(
        (status, body),
        (
            StatusCode::BAD_GATEWAY,
            json!({"error": "onboarding_failed", "step": "register_phone", "resumable": true})
        )
    );

    // A malformed PIN is refused locally and keeps the attempt resumable.
    let resume = |body: Value| post("/signup/resume?tenant=merchant-a", &body);
    let (status, _) = call(&app, resume(json!({"pin": "12"}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // Another merchant has nothing to resume.
    let (status, _) = call(
        &app,
        post(
            "/signup/resume?tenant=merchant-b",
            &json!({"pin": "123456"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(graph.requests().len(), 6);

    graph.push_json(200, success()); // subscribe again
    graph.push_json(200, success()); // register with the new PIN
    let (status, body) = call_json(&app, resume(json!({"pin": "123456"}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["steps_completed"],
        json!([
            "load_token",
            "verify_assets",
            "subscribe_app",
            "register_phone"
        ])
    );
    let last = graph.last_request().unwrap();
    assert_eq!(last.path(), format!("/v25.0/{PHONE}/register"));
    assert_eq!(
        last.bearer(),
        Some(TOKEN),
        "the stored token, not a new code"
    );
    assert_eq!(
        last.json(),
        Some(json!({"messaging_product": "whatsapp", "pin": "123456"}))
    );
    assert_eq!(graph.remaining(), 0);

    let (status, _) = call(&app, resume(json!({}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "finished: nothing left");
}

#[tokio::test]
async fn cancelled_or_malformed_posts_do_not_burn_the_attempt() {
    let Harness { app, graph, .. } = harness();
    let state = start(&app, "merchant-a").await;

    // embedded-signup/implementation, "Abandoned flow structure".
    let cancel = json!({
        "state": state, "code": CODE,
        "event": {"data": {"current_step": "PHONE_NUMBER_SETUP"}, "type": "WA_EMBEDDED_SIGNUP", "event": "CANCEL"}
    });
    let (status, body) = call_json(&app, complete("merchant-a", &cancel)).await;
    assert_eq!(
        (status, body),
        (
            StatusCode::OK,
            json!({"status": "cancelled", "current_step": "PHONE_NUMBER_SETUP"})
        )
    );
    let mut not_an_event = finished(&state);
    not_an_event["event"]["type"] = json!("SOMETHING_ELSE");
    let (status, body) = call_json(&app, complete("merchant-a", &not_an_event)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(graph.requests().is_empty());

    // The same state still completes.
    script_until_verified(&graph);
    graph.push_json(200, success());
    graph.push_json(200, success());
    let (status, body) = call_json(&app, complete("merchant-a", &finished(&state))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(graph.remaining(), 0);
}
