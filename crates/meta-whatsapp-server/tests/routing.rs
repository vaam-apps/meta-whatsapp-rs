//! Roadmap M5l: conversation routing through the service (scope
//! `routing`; docs/design/server.md, section 4.2): a number's routing
//! roles and the thread control actions.
//!
//! Each test sends the requests the pages document and checks the exact
//! requests that reach Meta (method, path, the tenant's token, the JSON
//! body) and `remaining() == 0`. The design's M5.2: a thread is a user's
//! with the number, no request field names an app, and a refusal sends no
//! request. M5.1 (another tenant's number, zero vault reads) is
//! `tests/authz.rs`, which iterates every route of the document, these
//! included.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const OTHER_TENANT: &str = "merchant-43";
const OTHER_WABA: &str = "102290129340399";
const OTHER_PN: &str = "106540352242924";
const PHONE: &str = "+16505551234";
const WA_ID: &str = "16505551234";
const BSUID: &str = "US.13491208655302741918";

async fn connected() -> (Harness, String) {
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], "EAAG-other")
        .await;
    let key = h.tenant_key(TENANT, &[Scope::Routing]).await;
    (h, key)
}

fn post(action: &str, key: &str) -> Call {
    Call::new(
        Method::POST,
        format!("/v1/numbers/{PN}/thread-control/{action}"),
    )
    .key(key)
}

fn error(reply: &common::Reply) -> Value {
    reply.json()["error"].clone()
}

fn accepted() -> Value {
    json!({"messaging_product": "whatsapp", "request_id": "REQ-1"})
}

/// routing-roles, "Response" and "Empty roles" (their examples): the
/// request is a bare `GET` with the tenant's token and the answer is the
/// two fields, Meta's order kept. Meta's refusal for a number without
/// routing keeps its code and its text (no user is named). Decisive:
/// `Routing::roles` and its conversion.
#[tokio::test]
async fn the_roles_are_read_from_the_numbers_own_endpoint() {
    let (h, key) = connected().await;
    h.graph.push_json(
        200,
        json!({"roles": ["customer_service", "escalation"], "is_standby": false}),
    );
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}/routing-roles")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"roles": ["customer_service", "escalation"], "is_standby": false})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{PN}/routing_roles"));
    assert_eq!(request.url.query(), None);
    assert_eq!(request.bearer(), Some(TOKEN));

    h.graph
        .push_json(200, json!({"roles": [], "is_standby": true}));
    let empty = h
        .call(Call::get(format!("/v1/numbers/{PN}/routing-roles")).key(&key))
        .await;
    assert_eq!(empty.json(), json!({"roles": [], "is_standby": true}));

    h.graph.push_json(
        400,
        json!({"error": {"message": "Conversation routing is not enabled for this phone number.",
                         "type": "OAuthException", "code": 100,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let off = h
        .call(Call::get(format!("/v1/numbers/{PN}/routing-roles")).key(&key))
        .await;
    assert!(off.status.is_client_error(), "{}", off.text);
    assert_eq!(error(&off)["graph"]["code"], 100);
    assert_eq!(h.graph.remaining(), 0);
}

/// thread-control, "Untargeted pass example", "Targeted pass example",
/// "Release" and "Take" (their examples): the exact body per action and
/// per way of naming the user (`to` for a phone or a `wa_id`, `recipient`
/// for a BSUID), the tenant's token, `POST /{pn}/thread_control`, and the
/// shared answer. Decisive: the body each service method builds.
#[tokio::test]
#[allow(clippy::too_many_lines)] // every action, every way of naming the user
async fn each_action_is_metas_body_for_the_user_named() {
    let (h, key) = connected().await;
    let sent = |h: &Harness| {
        let request = h.graph.last_request().unwrap();
        assert_eq!(request.method, Method::POST);
        assert_eq!(request.path(), format!("/v25.0/{PN}/thread_control"));
        assert_eq!(request.bearer(), Some(TOKEN));
        request.json().unwrap()
    };
    for (action, body, wire) in [
        (
            "pass",
            json!({"user": {"phone": PHONE}}),
            json!({"messaging_product": "whatsapp", "to": PHONE, "action": "pass"}),
        ),
        (
            "pass",
            json!({"user": {"wa_id": WA_ID}, "target_role": "ai_agent"}),
            json!({"messaging_product": "whatsapp", "to": WA_ID, "action": "pass",
                   "control_pass": {"target_role": "ai_agent"}}),
        ),
        (
            "pass",
            json!({"user": {"user_id": BSUID}, "target_role": "customer_service",
                   "metadata": "WhatsApp user requested human agent"}),
            json!({"messaging_product": "whatsapp", "recipient": BSUID, "action": "pass",
                   "control_pass": {"target_role": "customer_service"},
                   "metadata": "WhatsApp user requested human agent"}),
        ),
        (
            "take",
            json!({"user": {"phone": PHONE}}),
            json!({"messaging_product": "whatsapp", "to": PHONE, "action": "take"}),
        ),
        (
            "take",
            json!({"user": {"user_id": BSUID}, "metadata": "Human agent stepping in"}),
            json!({"messaging_product": "whatsapp", "recipient": BSUID, "action": "take",
                   "metadata": "Human agent stepping in"}),
        ),
        (
            "release",
            json!({"user": {"phone": PHONE}}),
            json!({"messaging_product": "whatsapp", "to": PHONE, "action": "release"}),
        ),
        (
            "release",
            json!({"user": {"user_id": BSUID}}),
            json!({"messaging_product": "whatsapp", "recipient": BSUID, "action": "release"}),
        ),
    ] {
        h.graph.push_json(200, accepted());
        let reply = h.call(post(action, &key).json(&body)).await;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "{action} {body}: {}",
            reply.text
        );
        assert_eq!(reply.json(), json!({"request_id": "REQ-1"}));
        assert_eq!(sent(&h), wire, "{action} {body}");
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// An action is not idempotent at Meta: the same key and body replay the
/// first answer without a second request, the same key with another body
/// is `422 idempotency_key_reused` with nothing sent, and a timeout is
/// `504` with the thread possibly acted on, never retried (one request). A
/// key does not carry from a pass to a release. Decisive:
/// `Fingerprint::pass_thread` and the library's no-replay rule.
#[tokio::test]
async fn an_action_is_keyed_and_never_replayed_after_a_timeout() {
    let (h, key) = connected().await;
    let body = json!({"user": {"user_id": BSUID}, "target_role": "marketing"});
    h.graph.push_json(200, accepted());
    let first = h
        .call(
            post("pass", &key)
                .header("idempotency-key", "k-1")
                .json(&body),
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    let again = h
        .call(
            post("pass", &key)
                .header("idempotency-key", "k-1")
                .json(&body),
        )
        .await;
    assert_eq!(again.json(), first.json());
    assert_eq!(h.graph.requests().len(), 1, "the key replayed it");

    let other = h
        .call(
            post("pass", &key)
                .header("idempotency-key", "k-1")
                .json(&json!({"user": {"user_id": BSUID}, "target_role": "utility"})),
        )
        .await;
    assert_eq!(
        (other.status, error(&other)["code"].clone()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!("idempotency_key_reused")
        ),
        "{}",
        other.text
    );
    let release = h
        .call(
            post("release", &key)
                .header("idempotency-key", "k-1")
                .json(&json!({"user": {"user_id": BSUID}})),
        )
        .await;
    assert_eq!(
        error(&release)["code"],
        "idempotency_key_reused",
        "{}",
        release.text
    );
    assert_eq!(h.graph.requests().len(), 1);

    h.graph.push_error(|| TransportError::Timeout);
    let cut = h
        .call(post("release", &key).json(&json!({"user": {"phone": PHONE}})))
        .await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    assert_eq!(error(&cut)["may_have_been_sent"], true, "{}", cut.text);
    assert_eq!(
        h.graph.requests().len(),
        2,
        "a timed-out action is not replayed"
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// Meta's refusals of an action: its code and the service's kind, never
/// its text (a text may quote the user): `2494191` for a take by one that
/// is not the escalation partner, `100` for an account not enrolled.
/// Decisive: `failed_withholding_texts` in `Routing::action_failed`.
#[tokio::test]
async fn a_refusal_answers_metas_code_and_never_its_text() {
    let (h, key) = connected().await;
    for (code, message) in [
        (
            2_494_191,
            format!("You are not permitted to take thread control for {PHONE} SENTINEL-META-TEXT"),
        ),
        (
            100,
            format!("(#100) Could not resolve recipient phone number. {BSUID} SENTINEL-META-TEXT"),
        ),
    ] {
        h.graph.push_json(
            400,
            json!({"error": {"message": message, "type": "OAuthException", "code": code,
                             "error_data": {"details": format!("{PHONE} SENTINEL-META-TEXT")},
                             "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
        );
        let reply = h
            .call(post("take", &key).json(&json!({"user": {"phone": PHONE}})))
            .await;
        assert!(!reply.status.is_success(), "{}", reply.text);
        assert_eq!(error(&reply)["graph"]["code"], code, "{}", reply.text);
        for secret in ["SENTINEL-META-TEXT", PHONE, BSUID] {
            assert!(!reply.text.contains(secret), "{secret} in {}", reply.text);
        }
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// Refused before any request: no user, a user named two ways, a phone
/// without its `+`, a `wa_id` that is not digits, a parent BSUID, a group,
/// an app named instead of a role (`target_role` of Meta's six only), a
/// role in another case, an app id, `metadata` over 2,000 characters
/// (counted in characters), `control_pass` or `metadata` on a release,
/// and a key outside the body. Decisive: `routing::user`, `pass_of`, the
/// `deny_unknown_fields` of the three bodies.
#[tokio::test]
async fn a_request_naming_no_one_or_something_meta_would_refuse_sends_nothing() {
    let (h, key) = connected().await;
    let long = "é".repeat(2001);
    let edge = "é".repeat(2000);
    for (action, body, field) in [
        ("pass", json!({}), "body"),
        ("pass", json!({"user": {}}), "user"),
        (
            "pass",
            json!({"user": {"phone": PHONE, "user_id": BSUID}}),
            "user",
        ),
        ("pass", json!({"user": {"phone": WA_ID}}), "user.phone"),
        ("pass", json!({"user": {"wa_id": PHONE}}), "user.wa_id"),
        (
            "take",
            json!({"user": {"user_id": "US.ENT.11815799212886844830"}}),
            "user.user_id",
        ),
        ("take", json!({"user": {"group_id": "120363"}}), "body"),
        (
            "pass",
            json!({"user": {"phone": PHONE}, "target_role": "billing"}),
            "target_role",
        ),
        (
            "pass",
            json!({"user": {"phone": PHONE}, "target_role": "Escalation"}),
            "target_role",
        ),
        (
            "pass",
            json!({"user": {"phone": PHONE}, "target_role": "calls"}),
            "target_role",
        ),
        (
            "pass",
            json!({"user": {"phone": PHONE}, "target_app_id": "1234567890"}),
            "body",
        ),
        (
            "pass",
            json!({"user": {"phone": PHONE}, "control_pass": {"target_role": "ctwa"}}),
            "body",
        ),
        (
            "pass",
            json!({"user": {"phone": PHONE}, "metadata": long}),
            "metadata",
        ),
        (
            "take",
            json!({"user": {"phone": PHONE}, "metadata": long}),
            "metadata",
        ),
        (
            "take",
            json!({"user": {"phone": PHONE}, "target_role": "ctwa"}),
            "body",
        ),
        (
            "release",
            json!({"user": {"phone": PHONE}, "metadata": "note"}),
            "body",
        ),
        ("release", json!({"to": PHONE}), "body"),
    ] {
        let reply = h.call(post(action, &key).json(&body)).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{action} {body}: {}",
            reply.text
        );
        assert_eq!(error(&reply)["field"], field, "{action} {body}");
    }
    assert_eq!(h.graph.requests().len(), 0, "refused before any request");
    // The bound is inclusive and in characters.
    h.graph.push_json(200, accepted());
    let ok = h
        .call(post("take", &key).json(&json!({"user": {"phone": PHONE}, "metadata": edge})))
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Scopes and tenants ──────────────────────────────────────────────────

/// Every route takes the scope `routing` and no other stands in for it
/// (D220): a key with every other scope is `403 forbidden` on each, with
/// no request to Meta; a key with `routing` alone passes the guard.
/// Decisive: `Operation::scope`, and the router's `tenant(…, Scope::Routing)`.
#[tokio::test]
async fn the_routes_take_their_own_scope() {
    let (h, _) = connected().await;
    let others: Vec<Scope> = Scope::ALL
        .into_iter()
        .filter(|s| *s != Scope::Routing)
        .collect();
    let wrong = h.tenant_key(TENANT, &others).await;
    for (method, path, body) in [
        (Method::GET, format!("/v1/numbers/{PN}/routing-roles"), None),
        (
            Method::POST,
            format!("/v1/numbers/{PN}/thread-control/pass"),
            Some(json!({"user": {"phone": PHONE}})),
        ),
        (
            Method::POST,
            format!("/v1/numbers/{PN}/thread-control/take"),
            Some(json!({"user": {"phone": PHONE}})),
        ),
        (
            Method::POST,
            format!("/v1/numbers/{PN}/thread-control/release"),
            Some(json!({"user": {"phone": PHONE}})),
        ),
    ] {
        let mut call = Call::new(method.clone(), path.clone()).key(&wrong);
        if let Some(body) = &body {
            call = call.json(body);
        }
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, error(&reply)["code"].clone()),
            (StatusCode::FORBIDDEN, json!("forbidden")),
            "{method} {path}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), 0);
}

/// Another tenant's number: tenant B's key on A's number is `404` on each
/// route, with no request to Meta and the vault unread (the design's M5.1,
/// here by name; `tests/authz.rs` iterates them all).
#[tokio::test]
async fn another_tenants_number_is_not_found_before_the_vault() {
    let (h, _) = connected().await;
    let b_key = h.tenant_key(OTHER_TENANT, &[Scope::Routing]).await;
    let reads = h.kv.vault_reads();
    for (method, action) in [
        (Method::GET, None),
        (Method::POST, Some("pass")),
        (Method::POST, Some("take")),
        (Method::POST, Some("release")),
    ] {
        let path = match action {
            Some(action) => format!("/v1/numbers/{PN}/thread-control/{action}"),
            None => format!("/v1/numbers/{PN}/routing-roles"),
        };
        let mut call = Call::new(method.clone(), path.clone()).key(&b_key);
        if action.is_some() {
            call = call.json(&json!({"user": {"phone": PHONE}}));
        }
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, error(&reply)["code"].clone()),
            (StatusCode::NOT_FOUND, json!("not_found")),
            "{method} {path}: {}",
            reply.text
        );
    }
    assert_eq!(h.kv.vault_reads(), reads, "the vault was not read");
    assert_eq!(h.graph.requests().len(), 0);
}
