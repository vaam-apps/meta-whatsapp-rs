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
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::core::store::{ConversationKey, ThreadOwner};
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

/// A key is one request: the same key and the same body as another action
/// (a pass, then a take) or on another number of the same tenant is
/// `422 idempotency_key_reused` with nothing sent, never the first answer.
/// Decisive: the `Fingerprint::*_thread` each handler picks, and the
/// number it gives.
#[tokio::test]
async fn a_key_is_never_shared_between_actions_or_numbers() {
    const PN2: &str = "106540352242923";
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN, PN2], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Routing]).await;
    let body = json!({"user": {"user_id": BSUID}});
    let on = |pn: &str, action: &str| {
        Call::new(
            Method::POST,
            format!("/v1/numbers/{pn}/thread-control/{action}"),
        )
        .key(&key)
        .header("idempotency-key", "shared")
        .json(&body)
    };
    h.graph.push_json(200, accepted());
    assert_eq!(h.call(on(PN, "take")).await.status, StatusCode::OK);
    for (pn, action) in [(PN, "pass"), (PN, "release"), (PN2, "take")] {
        let reply = h.call(on(pn, action)).await;
        assert_eq!(
            (reply.status, error(&reply)["code"].clone()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                json!("idempotency_key_reused")
            ),
            "{pn} {action}: {}",
            reply.text
        );
    }
    // Each handler's own fingerprint: the key is free again for each
    // other action once it was used by that one alone.
    for action in ["pass", "release"] {
        let h = Harness::new();
        h.tenant(TENANT).await;
        h.connect(TENANT, WABA, &[PN], TOKEN).await;
        let key = h.tenant_key(TENANT, &[Scope::Routing]).await;
        h.graph.push_json(200, accepted());
        let first = h
            .call(
                post(action, &key)
                    .header("idempotency-key", "k")
                    .json(&body),
            )
            .await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.text);
        let take = h
            .call(
                post("take", &key)
                    .header("idempotency-key", "k")
                    .json(&body),
            )
            .await;
        assert_eq!(
            error(&take)["code"],
            "idempotency_key_reused",
            "{action}: {}",
            take.text
        );
        assert_eq!(h.graph.requests().len(), 1);
    }
    assert_eq!(h.graph.requests().len(), 1);
}

// ─── The service's own actions in the inbox (roadmap M5l2, D390–D393) ─────

/// A conversation of merchant-42's number, keyed by the customer's BSUID
/// (the message carries both, so the inbox links the phone number to it),
/// and a key with the scopes `routing` and `inbox`.
async fn with_a_bsuid_conversation() -> (Harness, String) {
    use common::meta::{EXAMPLE_BSUID, EXAMPLE_WAMID, bytes, text};
    let (h, _) = connected().await;
    let mut payload = text(WABA, PN, EXAMPLE_WAMID);
    let value = &mut payload["entry"][0]["changes"][0]["value"];
    value["contacts"][0]["user_id"] = json!(EXAMPLE_BSUID);
    value["messages"][0]["from_user_id"] = json!(EXAMPLE_BSUID);
    let received = h.webhook(&bytes(&payload)).await;
    assert_eq!(received.status, StatusCode::OK, "{}", received.text);
    // The service's clock past the message (dated the next second).
    h.clock
        .set(time::OffsetDateTime::now_utc() + time::Duration::seconds(2));
    let key = h.tenant_key(TENANT, &[Scope::Routing, Scope::Inbox]).await;
    (h, key)
}

/// A free-form reply to the BSUID's conversation.
fn reply_to_customer(key: &str, text: &str) -> Call {
    let slug: String = text.chars().filter(char::is_ascii_alphanumeric).collect();
    common::inbox::reply(key, PN, BSUID, &format!("k-{slug}"), text)
}

/// Meta's `control_passed` for the customer, dated `at` (Unix seconds).
fn control_passed(at: i64) -> Value {
    common::meta::dated(
        json!({
            "object": "whatsapp_business_account",
            "entry": [{
                "id": WABA,
                "changes": [{
                    "field": "messaging_handovers",
                    "value": {
                        "messaging_product": "whatsapp",
                        "sender": {"phone_number": WA_ID},
                        "recipient": {"phone_number_id": PN, "display_phone_number": "15550783881"},
                        "type": "control_passed",
                        "timestamp": "1750101000",
                        "control_passed": {
                            "previous_owner_role": "customer_service",
                            "new_owner_role": "escalation"
                        }
                    }
                }]
            }]
        }),
        at,
    )
}

/// Decisive (M5l2): after a pass Meta answered, a free-form reply to that
/// user is `409 thread_owned_elsewhere` with no request to Meta, also when
/// the pass named the user by a phone number the inbox linked to the BSUID
/// (the record is under the BSUID's conversation, where the sink records a
/// handover, never under the phone number); after a release the reply goes
/// to Meta; a take returns the thread to this app. Decisive: the
/// `Routing::record` of each action, and the key it resolves.
#[tokio::test]
async fn a_pass_a_release_and_a_take_reach_the_replys_guard() {
    use common::inbox::accepted as sent;
    let (h, key) = with_a_bsuid_conversation().await;
    // The pass names the customer by phone number, the thread is the BSUID's.
    h.graph.push_json(200, accepted());
    let passed = h
        .call(
            post("pass", &key).json(&json!({"user": {"phone": PHONE}, "target_role": "ai_agent"})),
        )
        .await;
    assert_eq!(passed.status, StatusCode::OK, "{}", passed.text);
    let refused = h.call(reply_to_customer(&key, "Still there?")).await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "thread_owned_elsewhere"),
        "{}",
        refused.text
    );
    assert_eq!(h.graph.requests().len(), 1, "the 409 sent nothing to Meta");

    // A release Meta refused, and one that timed out, record nothing.
    h.graph.push_json(
        400,
        json!({"error": {"message": "refused", "type": "OAuthException", "code": 100,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let failed = h
        .call(post("release", &key).json(&json!({"user": {"user_id": BSUID}})))
        .await;
    assert!(!failed.status.is_success(), "{}", failed.text);
    h.graph.push_error(|| TransportError::Timeout);
    let cut = h
        .call(post("release", &key).json(&json!({"user": {"user_id": BSUID}})))
        .await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    let refused = h.call(reply_to_customer(&key, "Still there?")).await;
    assert_eq!(refused.code(), "thread_owned_elsewhere", "{}", refused.text);
    assert_eq!(h.graph.requests().len(), 3, "the 409 sent nothing to Meta");

    // The release Meta answered: the reply goes on to Meta's window check.
    h.graph.push_json(200, accepted());
    let released = h
        .call(post("release", &key).json(&json!({"user": {"wa_id": WA_ID}})))
        .await;
    assert_eq!(released.status, StatusCode::OK, "{}", released.text);
    h.graph.push_json(200, sent("wamid.AFTER-RELEASE"));
    let after = h.call(reply_to_customer(&key, "Back with you")).await;
    assert_eq!(after.status, StatusCode::ACCEPTED, "{}", after.text);

    // A pass without a role goes to the escalation partner; a take is
    // this app's again.
    h.graph.push_json(200, accepted());
    h.call(post("pass", &key).json(&json!({"user": {"user_id": BSUID}})))
        .await;
    let refused = h.call(reply_to_customer(&key, "Hello again")).await;
    assert_eq!(refused.code(), "thread_owned_elsewhere", "{}", refused.text);
    h.graph.push_json(200, accepted());
    h.call(post("take", &key).json(&json!({"user": {"phone": PHONE}})))
        .await;
    h.graph.push_json(200, sent("wamid.AFTER-TAKE"));
    let after = h.call(reply_to_customer(&key, "Mine again")).await;
    assert_eq!(after.status, StatusCode::ACCEPTED, "{}", after.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// An action whose outcome Meta did not answer records nothing, one Meta
/// refused records nothing, and one refused by the service sends and
/// records nothing: the guard keeps believing what it believed. Decisive:
/// the failure arms of `Routing::pass` (record only on `Ok`).
#[tokio::test]
async fn an_unanswered_or_refused_action_records_nothing() {
    use common::inbox::accepted as sent;
    let (h, key) = with_a_bsuid_conversation().await;
    // Timeout, a refusal of Meta's, a 5xx and a request the service refuses.
    h.graph.push_error(|| TransportError::Timeout);
    let cut = h
        .call(
            post("pass", &key)
                .json(&json!({"user": {"user_id": BSUID}, "target_role": "marketing"})),
        )
        .await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    h.graph.push_json(
        400,
        json!({"error": {"message": "refused", "type": "OAuthException", "code": 100,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    h.call(post("pass", &key).json(&json!({"user": {"user_id": BSUID}})))
        .await;
    h.graph.push_json(
        500,
        json!({"error": {"message": "unexpected", "type": "OAuthException", "code": 2,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    h.call(post("pass", &key).json(&json!({"user": {"user_id": BSUID}})))
        .await;
    let invalid = h
        .call(
            post("pass", &key).json(&json!({"user": {"user_id": BSUID}, "target_role": "billing"})),
        )
        .await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(h.graph.requests().len(), 3);
    h.graph.push_json(200, sent("wamid.FREE"));
    let free = h.call(reply_to_customer(&key, "Still mine")).await;
    assert_eq!(free.status, StatusCode::ACCEPTED, "{}", free.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// The webhook path and the service path agree on the order: a handover
/// webhook later than the service's pass wins (this app owns the thread
/// again and the reply goes), one older than a service pass does not.
/// Decisive: the record's `since` (the inbox's clock, to the second) against
/// the handover's `timestamp`.
#[tokio::test]
async fn a_later_handover_webhook_wins_over_the_service_record() {
    use common::inbox::accepted as sent;
    let (h, key) = with_a_bsuid_conversation().await;
    h.graph.push_json(200, accepted());
    h.call(post("pass", &key).json(&json!({"user": {"phone": PHONE}, "target_role": "ai_agent"})))
        .await;
    // Older than the pass (the service's clock is `now + 2`): not stored.
    let late = h
        .webhook(&common::meta::bytes(&control_passed(common::meta::now())))
        .await;
    assert_eq!(late.status, StatusCode::OK, "{}", late.text);
    let refused = h.call(reply_to_customer(&key, "Hello?")).await;
    assert_eq!(refused.code(), "thread_owned_elsewhere", "{}", refused.text);
    // Later than the pass: the new owner passed the thread back to this app.
    let back = h
        .webhook(&common::meta::bytes(&control_passed(
            common::meta::now() + 10,
        )))
        .await;
    assert_eq!(back.status, StatusCode::OK, "{}", back.text);
    h.graph.push_json(200, sent("wamid.BACK"));
    let answered = h.call(reply_to_customer(&key, "Hello again")).await;
    assert_eq!(answered.status, StatusCode::ACCEPTED, "{}", answered.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// The inbox's record of a thread the service acted on: owner and role.
async fn recorded(h: &Harness, contact: &str) -> Option<(ThreadOwner, Option<String>)> {
    let key = ConversationKey::new(PhoneNumberId::new(PN).unwrap(), contact);
    h.conversations
        .thread_owner(&key)
        .await
        .unwrap()
        .map(|o| (o.owner, o.role))
}

/// What each action leaves in the store, under the BSUID's conversation
/// however the request named the user: a targeted pass `another_app` with
/// that role, an untargeted one the `escalation`, a take `this_app` with
/// `escalation`, a release `idle` with no role; the phone number's own key
/// is never written. Decisive: the owner and the role each arm of
/// `Routing::pass`, `take` and `release` hands to `record` (swap `ThisApp`
/// and `AnotherApp`, record `AnotherApp` on a release, or a wrong role,
/// and this fails).
#[tokio::test]
async fn each_action_leaves_its_owner_and_role_under_the_bsuids_conversation() {
    let (h, key) = with_a_bsuid_conversation().await;
    let owner = |o: ThreadOwner, r: Option<&str>| Some((o, r.map(str::to_owned)));
    for (action, user, body_extra, expected) in [
        (
            "pass",
            json!({"phone": PHONE}),
            json!({"target_role": "ai_agent"}),
            owner(ThreadOwner::AnotherApp, Some("ai_agent")),
        ),
        (
            "release",
            json!({"wa_id": WA_ID}),
            json!({}),
            owner(ThreadOwner::Idle, None),
        ),
        (
            "pass",
            json!({"user_id": BSUID}),
            json!({}),
            owner(ThreadOwner::AnotherApp, Some("escalation")),
        ),
        (
            "take",
            json!({"phone": PHONE}),
            json!({}),
            owner(ThreadOwner::ThisApp, Some("escalation")),
        ),
    ] {
        let mut body = json!({ "user": user });
        body.as_object_mut()
            .unwrap()
            .extend(body_extra.as_object().unwrap().clone());
        h.graph.push_json(200, accepted());
        let done = h.call(post(action, &key).json(&body)).await;
        assert_eq!(done.status, StatusCode::OK, "{action}: {}", done.text);
        assert_eq!(recorded(&h, BSUID).await, expected, "{action} {body}");
        assert_eq!(recorded(&h, WA_ID).await, None, "never the phone alone");
        assert_eq!(recorded(&h, PHONE).await, None);
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// A 200 that does not say success records nothing, for the three actions:
/// the service answers an error, and the guard keeps believing what it
/// believed. Decisive: the client's `send_checked` in `ThreadControl::send`
/// (reading only the HTTP status records a thread Meta did not move).
#[tokio::test]
async fn a_200_that_says_success_false_records_nothing() {
    let (h, key) = with_a_bsuid_conversation().await;
    for (action, body) in [
        ("pass", json!({"user": {"user_id": BSUID}})),
        ("take", json!({"user": {"user_id": BSUID}})),
        ("release", json!({"user": {"user_id": BSUID}})),
    ] {
        h.graph.push_json(200, json!({"success": false}));
        let reply = h.call(post(action, &key).json(&body)).await;
        assert!(!reply.status.is_success(), "{action}: {}", reply.text);
        assert_eq!(recorded(&h, BSUID).await, None, "{action}");
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// D393: a record that cannot be stored does not fail the action. All three
/// still answer `200 {request_id}` (nothing panics, nothing invites a
/// retry), the `warn` line is on the module's explicit target and carries
/// the action and the error's kind, and no line holds the user (phone
/// number, `wa_id` or BSUID), the `metadata` or the store's own message.
/// Decisive: the `Err` arm of `Routing::record` (propagating the error, or
/// logging the user, fails it).
#[tokio::test]
async fn a_record_that_cannot_be_stored_does_not_fail_the_action_or_name_the_user() {
    use common::capture::{Captured, Secrets, subscriber};
    use common::failing_conversations::FailingOwnerWrites;
    use common::meta::{EXAMPLE_BSUID, EXAMPLE_WAMID, bytes, text};

    let store = std::sync::Arc::new(FailingOwnerWrites::default());
    let h = Harness::with(common::Stores {
        conversations: store.clone(),
        ..common::Stores::memory()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let mut payload = text(WABA, PN, EXAMPLE_WAMID);
    let value = &mut payload["entry"][0]["changes"][0]["value"];
    value["contacts"][0]["user_id"] = json!(EXAMPLE_BSUID);
    value["messages"][0]["from_user_id"] = json!(EXAMPLE_BSUID);
    assert_eq!(h.webhook(&bytes(&payload)).await.status, StatusCode::OK);
    h.clock
        .set(time::OffsetDateTime::now_utc() + time::Duration::seconds(2));
    let key = h.tenant_key(TENANT, &[Scope::Routing, Scope::Inbox]).await;
    store.fail_owner_writes(true);

    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let note = "the customer's own words, METADATA-PLANT";
    for (action, body) in [
        (
            "pass",
            json!({"user": {"phone": PHONE}, "target_role": "marketing", "metadata": note}),
        ),
        (
            "take",
            json!({"user": {"user_id": BSUID}, "metadata": note}),
        ),
        ("release", json!({"user": {"wa_id": WA_ID}})),
    ] {
        h.graph.push_json(200, accepted());
        let done = h.call(post(action, &key).json(&body)).await;
        assert_eq!(done.status, StatusCode::OK, "{action}: {}", done.text);
        assert_eq!(done.json(), json!({"request_id": "REQ-1"}), "{action}");
    }
    assert_eq!(h.graph.remaining(), 0);
    assert_eq!(recorded(&h, BSUID).await, None, "nothing was stored");

    let logs = captured.text();
    let lines: Vec<Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["level"] == "WARN" && e["fields"]["action"].is_string())
        .collect();
    let actions: Vec<&str> = lines
        .iter()
        .map(|e| e["fields"]["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        ["thread_passed", "thread_taken", "thread_released"],
        "{logs}"
    );
    for line in &lines {
        assert_eq!(
            line["target"], "meta_whatsapp_server::api::routing",
            "{line}"
        );
        assert!(line["fields"]["kind"].is_string(), "{line}");
    }
    let mut secrets = Secrets::new(
        "the user and the note",
        vec![
            PHONE.to_owned(),
            WA_ID.to_owned(),
            BSUID.to_owned(),
            "METADATA-PLANT".to_owned(),
            "the store is down".to_owned(),
        ],
    );
    secrets.section("the token");
    secrets.push(TOKEN.to_owned());
    assert_eq!(secrets.leaks(&logs), None);

    // Writes work again: the next action is recorded.
    store.fail_owner_writes(false);
    h.graph.push_json(200, accepted());
    h.call(post("pass", &key).json(&json!({"user": {"user_id": BSUID}})))
        .await;
    assert_eq!(
        recorded(&h, BSUID).await,
        Some((ThreadOwner::AnotherApp, Some("escalation".to_owned())))
    );
}
