//! Roadmap M5h2 (docs/design/server.md, D430 to D435): what M5h did not
//! route. The max price agreement's state read, the partner Intent API
//! (`request_onboarding`) and the client WABAs listing by onboarding
//! status, through the service.
//!
//! Each test sends the documented request and answer of Meta's pages
//! (`marketing-messages/pricing/enroll-max-price`,
//! `marketing-messages/onboard-business-customers`,
//! `marketing-messages/onboarding`,
//! `reference/business/whatsapp-business-partner-onboarding-to-mm-lite-api`):
//! the exact requests (method, path, query, the token, the body),
//! `remaining() == 0`. M5.1 for the partner routes is `tests/authz.rs`
//! (it walks every route of the document); the audit events and the planted
//! signer in the logs are `partner_rest_m5h2_logs.rs`; the retry walk is
//! `legal_acts_once.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::signup::{POOL_BUSINESS, POOL_TOKEN, tech_provider};
use common::{Call, Harness};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::BusinessId;
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
/// `enroll-max-price`'s example business.
const BUSINESS: &str = "529216684107530";
/// The operator's own token, in the body of the state read.
const BUSINESS_TOKEN: &str = "EAAB-the-operators-business-token";
/// `onboard-business-customers`: the end business and the request's id.
const END_BUSINESS: &str = "2780023991704";
const REQUEST_ID: &str = "893436979557695";
const SOLUTION: &str = "795033096057724";
/// The signer Meta's state read answers (`enroll-max-price`'s example).
const SIGNER_NAME: &str = "Jane Doe";
const SIGNER_EMAIL: &str = "jane.doe@bsp.example";

fn settings(legal_acts: bool, partner: bool) -> Settings {
    Settings {
        signup: partner.then(|| {
            tech_provider().pre_verified_pool(
                BusinessId::new(POOL_BUSINESS).unwrap(),
                AccessToken::new(POOL_TOKEN),
            )
        }),
        legal_acts,
        ..common::test_settings()
    }
}

struct Fixture {
    h: Harness,
    admin: String,
}

async fn fixture(legal_acts: bool, partner: bool) -> Fixture {
    let h = Harness::with_settings(settings(legal_acts, partner));
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let admin = h.admin_key().await;
    Fixture { h, admin }
}

fn onboard(path_business: &str, f: &Fixture) -> Call {
    Call::new(
        Method::POST,
        format!("/v1/admin/partners/businesses/{path_business}/marketing-onboarding"),
    )
    .key(&f.admin)
}

fn wabas(query: &str, f: &Fixture) -> Call {
    Call::new(
        Method::GET,
        format!("/v1/admin/partners/client-wabas{query}"),
    )
    .key(&f.admin)
}

fn state(business: &str, token: &Value, f: &Fixture) -> Call {
    Call::new(
        Method::POST,
        format!("/v1/admin/businesses/{business}/max-price-agreement/state"),
    )
    .key(&f.admin)
    .json(&json!({ "token": token }))
}

// ─── The partner Intent API: a legal act ─────────────────────────────────

/// With the legal acts off (the default), the invitation is `403
/// feature_not_available` and **nothing is asked of Meta**, whatever the
/// body says (even a malformed one) and whatever the business id is; a key
/// that is not an admin's is still `403 forbidden` first. Decisive: the
/// `AdminOperation::RequestMarketingOnboarding` arm of `is_legal_admin_act`
/// (and the `LegalActsOn` extractor).
#[tokio::test]
async fn with_the_legal_acts_off_the_invitation_is_refused_without_a_request_to_meta() {
    let f = fixture(false, true).await;
    for call in [
        onboard(END_BUSINESS, &f),
        onboard("not-an-id", &f),
        onboard(END_BUSINESS, &f).json(&json!({"solution_id": SOLUTION})),
        onboard(END_BUSINESS, &f).json(&json!({"solution_id": 5, "extra": true})),
        onboard(END_BUSINESS, &f)
            .header("content-type", "application/json")
            .body(meta_whatsapp_rs::webhooks::axum::body::Body::from(
                "{not json",
            )),
    ] {
        let reply = f.h.call(call).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text);
        assert_eq!(reply.code(), "feature_not_available");
    }
    assert!(
        f.h.graph.requests().is_empty(),
        "{:?}",
        f.h.graph.requests()
    );
    assert_eq!(f.h.graph.remaining(), 0);
    // A tenant's key, whatever its scopes, is not an admin's.
    let tenant = f.h.tenant_key(TENANT, &common::ALL_SCOPES).await;
    let reply =
        f.h.call(
            Call::new(
                Method::POST,
                format!("/v1/admin/partners/businesses/{END_BUSINESS}/marketing-onboarding"),
            )
            .key(&tenant),
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.code(), "forbidden");
    assert!(f.h.graph.requests().is_empty());
}

/// With the legal acts on, the admin key's call is exactly the page's
/// request: `POST /{END_BUSINESS_ID}/onboard_partners_to_mm_lite` with the
/// deployment's system token, no body, no query; with a `solution_id` it
/// is the reference's query parameter. The answer is the request's id.
#[tokio::test]
async fn with_the_legal_acts_on_the_invitation_is_the_pages_request() {
    let f = fixture(true, true).await;
    f.h.graph.push_json(200, json!({"request_id": REQUEST_ID}));
    let reply = f.h.call(onboard(END_BUSINESS, &f)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json(), json!({"request_id": REQUEST_ID}));
    f.h.graph.push_json(200, json!({"request_id": REQUEST_ID}));
    let with_solution =
        f.h.call(onboard(END_BUSINESS, &f).json(&json!({"solution_id": SOLUTION})))
            .await;
    assert_eq!(
        with_solution.status,
        StatusCode::OK,
        "{}",
        with_solution.text
    );
    let requests = f.h.graph.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, Method::POST);
        assert_eq!(
            request.path(),
            format!("/v25.0/{END_BUSINESS}/onboard_partners_to_mm_lite")
        );
        assert_eq!(request.bearer(), Some(POOL_TOKEN));
        assert_eq!(request.json(), None, "the page's call has no body");
    }
    assert_eq!(requests[0].query("solution_id"), None);
    assert_eq!(requests[1].query("solution_id").as_deref(), Some(SOLUTION));
    assert_eq!(f.h.graph.remaining(), 0);
}

/// The inputs are checked before anything is sent; no partner business
/// configured is `409 signup_not_configured`.
#[tokio::test]
async fn the_invitation_checks_its_inputs_and_needs_the_partner_credentials() {
    let f = fixture(true, true).await;
    for (call, field) in [
        (onboard("not-an-id", &f), "business_id"),
        (onboard("12x34", &f), "business_id"),
        (
            onboard(END_BUSINESS, &f).json(&json!({"solution_id": "sol/1"})),
            "solution_id",
        ),
        (
            onboard(END_BUSINESS, &f).json(&json!({"solution_id": 1})),
            "body",
        ),
        (
            onboard(END_BUSINESS, &f).json(&json!({"solution": SOLUTION})),
            "body",
        ),
    ] {
        let reply = f.h.call(call).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(reply.json()["error"]["field"], field);
    }
    assert!(f.h.graph.requests().is_empty());
    let bare = fixture(true, false).await;
    let reply = bare.h.call(onboard(END_BUSINESS, &bare)).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.text);
    assert_eq!(reply.code(), "signup_not_configured");
    assert!(bare.h.graph.requests().is_empty());
}

/// Meta's refusals are answered by their code, never their text (a duplicate
/// request quotes the business), and nothing is replayed: a refusal and a
/// timeout are one request each. Decisive: `refused` withholding the texts.
#[tokio::test]
async fn the_invitations_refusals_answer_the_code_and_are_never_replayed() {
    let f = fixture(true, true).await;
    f.h.graph.push_json(
        400,
        json!({"error": {"message": "Your business has already sent this request. SENTINEL-TEXT",
                         "type": "OAuthException", "code": 1_752_041,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = f.h.call(onboard(END_BUSINESS, &f)).await;
    assert!(refused.status.is_client_error(), "{}", refused.text);
    assert!(!refused.text.contains("SENTINEL-TEXT"), "{}", refused.text);
    f.h.graph.push_error(|| TransportError::Timeout);
    let cut = f.h.call(onboard(END_BUSINESS, &f)).await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    assert_eq!(f.h.graph.requests().len(), 2);
    assert_eq!(f.h.graph.remaining(), 0);
}

// ─── The client WABAs by onboarding status ───────────────────────────────

/// `onboarding`'s example: the filter is Meta's, the token the
/// deployment's, the business the deployment's partner business; the answer
/// is the page's fields, with the next page's cursor.
#[tokio::test]
async fn the_client_wabas_are_listed_by_status_with_the_pages_request() {
    let f = fixture(false, true).await;
    f.h.graph.push_json(
        200,
        json!({"data": [{"id": "46302397361990", "name": "San Andreas Roofing",
                         "timezone_id": "1",
                         "message_template_namespace": "93d3e793_8a4f_49c4_b903_fd72aac80f71"}],
               "paging": {"cursors": {"before": "QVFI", "after": "QVFJ"},
                          "next": "https://graph.facebook.com/v25.0/next"}}),
    );
    let reply = f.h.call(wabas("?onboarding_status=ELIGIBLE", &f)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"data": [{"id": "46302397361990", "name": "San Andreas Roofing",
                         "timezone_id": "1",
                         "message_template_namespace": "93d3e793_8a4f_49c4_b903_fd72aac80f71"}],
               "next_cursor": "QVFJ"})
    );
    f.h.graph.push_json(200, json!({"data": []}));
    let last =
        f.h.call(wabas("?onboarding_status=ELIGIBLE&cursor=QVFJ", &f))
            .await;
    assert_eq!(last.json(), json!({"data": [], "next_cursor": null}));
    f.h.graph.push_json(200, json!({"data": []}));
    let both = f.h.call(wabas("", &f)).await;
    assert_eq!(both.status, StatusCode::OK, "{}", both.text);
    f.h.graph.push_json(200, json!({"data": []}));
    let onboarded =
        f.h.call(wabas("?onboarding_status=ONBOARDED,ELIGIBLE", &f))
            .await;
    assert_eq!(onboarded.status, StatusCode::OK, "{}", onboarded.text);
    let requests = f.h.graph.requests();
    assert_eq!(requests.len(), 4);
    let filtering = |request: &meta_whatsapp_rs::core::testing::RecordedRequest| {
        serde_json::from_str::<Value>(&request.query("filtering").unwrap()).unwrap()
    };
    for request in &requests {
        assert_eq!(request.method, Method::GET);
        assert_eq!(
            request.path(),
            format!("/v25.0/{POOL_BUSINESS}/client_whatsapp_business_accounts")
        );
        assert_eq!(request.bearer(), Some(POOL_TOKEN));
    }
    let one = |statuses: Value| {
        json!([{"field": "marketing_messages_onboarding_status",
                "operator": "IN", "value": statuses}])
    };
    assert_eq!(filtering(&requests[0]), one(json!(["ELIGIBLE"])));
    assert_eq!(requests[0].query("after"), None);
    assert_eq!(requests[1].query("after").as_deref(), Some("QVFJ"));
    assert_eq!(
        filtering(&requests[2]),
        one(json!(["ELIGIBLE", "ONBOARDED"]))
    );
    assert_eq!(
        filtering(&requests[3]),
        one(json!(["ONBOARDED", "ELIGIBLE"]))
    );
    assert_eq!(f.h.graph.remaining(), 0);
}

/// A status Meta's pages do not spell, a repeated one, an empty one and a
/// bad cursor are `422` with nothing sent; without the partner credentials
/// it is `409`; a tenant key is `403`.
#[tokio::test]
async fn the_client_wabas_listing_checks_its_inputs() {
    let f = fixture(false, true).await;
    for (query, field) in [
        ("?onboarding_status=PENDING", "onboarding_status"),
        ("?onboarding_status=eligible", "onboarding_status"),
        ("?onboarding_status=ELIGIBLE,ELIGIBLE", "onboarding_status"),
        ("?onboarding_status=", "onboarding_status"),
        ("?onboarding_status=ELIGIBLE,", "onboarding_status"),
        ("?onboarding_status=ELIGIBLE&cursor=", "cursor"),
        ("?filtering=x", "query"),
    ] {
        let reply = f.h.call(wabas(query, &f)).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert_eq!(reply.json()["error"]["field"], field, "{query}");
    }
    assert!(f.h.graph.requests().is_empty());
    let bare = fixture(false, false).await;
    let reply = bare.h.call(wabas("", &bare)).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.text);
    let tenant = f.h.tenant_key(TENANT, &common::ALL_SCOPES).await;
    let denied =
        f.h.call(Call::new(Method::GET, "/v1/admin/partners/client-wabas").key(&tenant))
            .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(denied.code(), "forbidden");
    assert!(f.h.graph.requests().is_empty());
}

// ─── The agreement's state ───────────────────────────────────────────────

/// `enroll-max-price`'s three example answers, two of which name the
/// signer: the request is `GET /{BUSINESS_ID}/max_price_agreements` with the
/// token of the body, and the answer is the status alone. It is not a legal
/// act: it works with the setting off. The token is stored nowhere.
/// Decisive: `MaxPriceAgreements::state` mapping only the status.
#[tokio::test]
async fn the_agreements_state_is_read_without_the_signer() {
    let f = fixture(false, false).await;
    let writes = f.h.kv.writes().len();
    for (meta, status) in [
        (
            json!({"status": "accepted", "signer_name": SIGNER_NAME, "signer_email": SIGNER_EMAIL}),
            "accepted",
        ),
        (
            json!({"status": "pending", "signer_name": SIGNER_NAME, "signer_email": SIGNER_EMAIL,
                   "message": format!("Agreement email sent to {SIGNER_EMAIL}")}),
            "pending",
        ),
        (json!({"status": "none"}), "none"),
    ] {
        f.h.graph.push_json(200, meta);
        let reply = f.h.call(state(BUSINESS, &json!(BUSINESS_TOKEN), &f)).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        assert_eq!(reply.json(), json!({"status": status}));
        for planted in [SIGNER_NAME, SIGNER_EMAIL, "Jane", "bsp.example"] {
            assert!(!reply.text.contains(planted), "{}", reply.text);
        }
    }
    let requests = f.h.graph.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(request.method, Method::GET);
        assert_eq!(
            request.path(),
            format!("/v25.0/{BUSINESS}/max_price_agreements")
        );
        assert_eq!(request.bearer(), Some(BUSINESS_TOKEN));
        assert_eq!(request.json(), None);
    }
    assert_eq!(f.h.graph.remaining(), 0);
    assert_eq!(f.h.kv.writes().len(), writes, "the token is stored nowhere");
}

/// Meta's failures quote the signer: a refusal, an answer that is not
/// Meta's envelope and one that does not decode answer the code and the
/// kind, and the planted signer and token are in no answer.
#[tokio::test]
async fn the_agreements_state_failures_keep_the_signer_and_the_token_out() {
    let f = fixture(false, false).await;
    f.h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#100) {SIGNER_NAME} {SIGNER_EMAIL}"),
                         "type": "OAuthException", "code": 100,
                         "error_data": {"details": format!("{SIGNER_NAME} {SIGNER_EMAIL}")},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = f.h.call(state(BUSINESS, &json!(BUSINESS_TOKEN), &f)).await;
    assert!(refused.status.is_client_error(), "{}", refused.text);
    f.h.graph.push_json(
        200,
        json!({"status": SIGNER_EMAIL, "signer_name": SIGNER_NAME, "extra": [SIGNER_EMAIL]}),
    );
    let odd = f.h.call(state(BUSINESS, &json!(BUSINESS_TOKEN), &f)).await;
    // A status outside the three the page spells is not echoed.
    assert_eq!(odd.status, StatusCode::OK, "{}", odd.text);
    assert_eq!(odd.json(), json!({"status": null}));
    f.h.graph.push_bytes(
        502,
        "text/html",
        format!("<html>{SIGNER_NAME} {SIGNER_EMAIL} {BUSINESS_TOKEN}</html>"),
    );
    let html = f.h.call(state(BUSINESS, &json!(BUSINESS_TOKEN), &f)).await;
    f.h.graph.push_json(200, json!([SIGNER_EMAIL]));
    let array = f.h.call(state(BUSINESS, &json!(BUSINESS_TOKEN), &f)).await;
    for reply in [&refused, &odd, &html, &array] {
        for planted in [
            SIGNER_NAME,
            SIGNER_EMAIL,
            "Jane",
            "bsp.example",
            BUSINESS_TOKEN,
        ] {
            assert!(!reply.text.contains(planted), "{}", reply.text);
        }
    }
    assert_eq!(f.h.graph.remaining(), 0);
}

/// Only an admin key reads it; a tenant key (any scopes) and a missing key
/// never reach Meta; the inputs are checked before anything is sent.
#[tokio::test]
async fn the_agreements_state_is_the_operators_and_checks_its_inputs() {
    let f = fixture(false, false).await;
    let tenant = f.h.tenant_key(TENANT, &common::ALL_SCOPES).await;
    let denied =
        f.h.call(
            Call::new(
                Method::POST,
                format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement/state"),
            )
            .key(&tenant)
            .json(&json!({"token": BUSINESS_TOKEN})),
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.text);
    assert_eq!(denied.code(), "forbidden");
    let anonymous =
        f.h.call(
            Call::new(
                Method::POST,
                format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement/state"),
            )
            .json(&json!({"token": BUSINESS_TOKEN})),
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    for (business, token, field) in [
        ("not-an-id", json!(BUSINESS_TOKEN), "business_id"),
        (BUSINESS, json!(" "), "token"),
        (BUSINESS, json!(""), "token"),
        (BUSINESS, json!(5), "body"),
    ] {
        let reply = f.h.call(state(business, &token, &f)).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{business}");
        assert_eq!(reply.json()["error"]["field"], field);
    }
    let unknown =
        f.h.call(
            Call::new(
                Method::POST,
                format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement/state"),
            )
            .key(&f.admin)
            .json(&json!({"token": BUSINESS_TOKEN, "signer_name": SIGNER_NAME})),
        )
        .await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!unknown.text.contains(BUSINESS_TOKEN) && !unknown.text.contains(SIGNER_NAME));
    // GET, and a body-less POST, are no way to read it.
    let get =
        f.h.call(
            Call::new(
                Method::GET,
                format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement/state"),
            )
            .key(&f.admin),
        )
        .await;
    assert!(get.status.is_client_error(), "{}", get.text);
    assert!(f.h.graph.requests().is_empty());
}

// ─── No other route ──────────────────────────────────────────────────────

/// **No other route makes either call**, with the legal acts on and the
/// partner credentials configured: every route of the document that is not
/// the admin's own is called with a key of every scope, Meta answering
/// each request it gets, and no request to `/onboard_partners_to_mm_lite`
/// or `/client_whatsapp_business_accounts` is ever made, and no `POST` to
/// `/max_price_agreements`. Decisive: any other operation calling
/// `request_onboarding`.
#[tokio::test]
async fn no_other_route_sends_the_invitation_or_the_agreement() {
    let f = fixture(true, true).await;
    let key = f.h.tenant_key(TENANT, &common::ALL_SCOPES).await;
    let sample = common::Sample {
        tenant: TENANT.to_owned(),
        waba: WABA.to_owned(),
        pn: PN.to_owned(),
        key_id: "placeholder".to_owned(),
    };
    let mut walked = 0;
    for operation in common::spec_operations() {
        if operation.admin() || operation.template.contains("stream") {
            continue;
        }
        for _ in 0..8 {
            f.h.graph.push_json(200, json!({}));
        }
        let _ =
            f.h.call(common::sample_call(&operation, &sample, Some(&key)))
                .await;
        walked += 1;
    }
    assert!(walked >= 100, "{walked} routes walked");
    let asked = f.h.graph.requests();
    assert!(asked.len() > 50, "Meta was asked {} times", asked.len());
    for request in asked {
        let path = request.path();
        assert!(!path.contains("onboard_partners_to_mm_lite"), "{path}");
        assert!(
            !path.contains("client_whatsapp_business_accounts"),
            "{path}"
        );
        assert!(!path.contains("max_price_agreements"), "{path}");
    }
}

/// The invitation's call is named by one module of the service: no source
/// of the service's crates but the core's `partners` names the library's
/// `request_onboarding`. Decisive: a second caller.
#[test]
fn only_the_partners_core_names_the_invitation() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let mut found = Vec::new();
    let mut stack = vec![crates.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let service = path
                    .parent()
                    .is_some_and(|p| p == crates)
                    .then(|| name.starts_with("meta-whatsapp-server"));
                if service == Some(false) || name == "target" || name == "tests" {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let source = std::fs::read_to_string(&path).unwrap();
                let code: String = source
                    .lines()
                    .filter(|line| !line.trim_start().starts_with("//"))
                    .collect::<Vec<_>>()
                    .join("\n");
                if code.contains(".request_onboarding(")
                    || code.contains("onboard_partners_to_mm_lite")
                {
                    found.push(path.strip_prefix(crates).unwrap().display().to_string());
                }
            }
        }
    }
    found.sort();
    assert_eq!(
        found,
        ["meta-whatsapp-server-core/src/partners.rs".to_owned()]
    );
}
