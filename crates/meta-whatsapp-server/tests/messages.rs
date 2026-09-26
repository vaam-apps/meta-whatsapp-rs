//! Messages (scope `send`) and idempotency keys (docs/design/server.md,
//! sections 4.2, 4.3, 5.3 and 5.4), and acceptance test M1.4: sends carry
//! the merchant's vault token; a digits-only `to.phone` is refused before
//! any request; a scripted timeout is `504` with `may_have_been_sent:
//! true`, and the same `Idempotency-Key` replays it with no second
//! request; a scripted 131047 is `409` and releases the key.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::time::Duration;

use common::{Call, Harness, Reply};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::idempotency::{Fingerprint, Success, run};
use meta_whatsapp_server::model::{
    AllowedTenants, IdempotencyClaim, IdempotencyKey, IdempotencyState, Scope, TenantId,
};
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const TO: &str = "+16505551234";

async fn connected_with(settings: Settings) -> (Harness, String) {
    let h = Harness::with_settings(settings);
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Send]).await;
    (h, key)
}

async fn connected() -> (Harness, String) {
    connected_with(common::test_settings()).await
}

/// messages/text-messages, "Example response".
fn accepted() -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": "+16505551234", "wa_id": "16505551234"}],
        "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
    })
}

/// messages/text-messages, "Example request", as the service's body.
fn text() -> Value {
    json!({
        "to": {"phone": TO},
        "type": "text",
        "text": {"preview_url": true, "body": "As requested, here's the link to our latest product: https://www.meta.com/quest/quest-3/"}
    })
}

fn send(key: &str, body: &Value) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{PN}/messages"))
        .key(key)
        .json(body)
}

fn with_key(call: Call, idempotency_key: &str) -> Call {
    call.header("idempotency-key", idempotency_key)
}

/// `error` of an error answer.
fn error(reply: &Reply) -> Value {
    reply.json()["error"].clone()
}

/// messages/mark-message-as-read's example id.
const RECEIVED: &str = "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBJDQjZCMzlEQUE4OTJBMTE4RTUA";

/// M1.4, first part: the send goes out with the merchant's token from the
/// vault, as Meta's documented request, and answers `202` with the id.
#[tokio::test]
async fn a_send_carries_the_merchants_token_and_metas_request() {
    let (h, key) = connected().await;
    h.graph.push_json(200, accepted());
    let reply = h.call(send(&key, &text())).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({
            "message_id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA",
            "contacts": [{"input": "+16505551234", "wa_id": "16505551234", "user_id": null, "parent_user_id": null}]
        })
    );
    assert!(reply.headers.get("idempotent-replayed").is_none());
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{PN}/messages"));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(
        request.json().unwrap(),
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505551234",
            "type": "text",
            "text": {
                "preview_url": true,
                "body": "As requested, here's the link to our latest product: https://www.meta.com/quest/quest-3/"
            }
        })
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// Replies, callback data, BSUIDs, templates and reactions reach Meta as
/// its pages write them.
#[tokio::test]
async fn envelopes_bsuids_templates_and_reactions_are_metas_objects() {
    let (h, key) = connected().await;
    // business-scoped-user-ids, "Send message response" to a BSUID.
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp",
               "contacts": [{"input": "US.13491208655302741918", "user_id": "US.13491208655302741918"}],
               "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]}),
    );
    let template = json!({
        "to": {"user_id": "US.13491208655302741918"},
        "type": "template",
        "template": {"name": "order_confirmation", "language": {"code": "en_US"},
                     "components": [{"type": "body", "parameters": [{"type": "text", "text": "Pablo"}]}]},
        "reply_to": RECEIVED,
        "callback_data": "order:1234:shipped"
    });
    let reply = h.call(send(&key, &template)).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    assert_eq!(
        reply.json()["contacts"][0],
        json!({"input": "US.13491208655302741918", "wa_id": null,
               "user_id": "US.13491208655302741918", "parent_user_id": null})
    );
    assert_eq!(
        h.graph.last_request().unwrap().json().unwrap(),
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "recipient": "US.13491208655302741918",
            "context": {"message_id": RECEIVED},
            "type": "template",
            "template": {"name": "order_confirmation", "language": {"code": "en_US"},
                         "components": [{"type": "body", "parameters": [{"type": "text", "text": "Pablo"}]}]},
            "biz_opaque_callback_data": "order:1234:shipped"
        })
    );
    // messages/reaction-messages, "Example request".
    h.graph.push_json(200, accepted());
    let reaction = json!({"to": {"phone": TO}, "type": "reaction",
                          "reaction": {"message_id": "wamid.HBgLMTY0NjcwNDM1OTUVAgASGBQzQUZCMTY0MDc2MUYwNzBDNTY5MAA=", "emoji": "\u{1F600}"}});
    assert_eq!(
        h.call(send(&key, &reaction)).await.status,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        h.graph.last_request().unwrap().json().unwrap(),
        json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": "+16505551234",
               "type": "reaction",
               "reaction": {"message_id": "wamid.HBgLMTY0NjcwNDM1OTUVAgASGBQzQUZCMTY0MDc2MUYwNzBDNTY5MAA=", "emoji": "\u{1F600}"}})
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// M1.4: a phone number without its `+` is refused before any request
/// (Meta would read it as local to the sender's country), and its
/// `Idempotency-Key` stays free. Decisive: the `+` check.
#[tokio::test]
async fn a_digits_only_phone_is_refused_before_any_request() {
    let (h, key) = connected().await;
    let mut local = text();
    local["to"]["phone"] = json!("16505551234");
    let reply = h.call(with_key(send(&key, &local), "k-local")).await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.text
    );
    let body = error(&reply);
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["field"], "to.phone");
    assert_eq!(body["may_have_been_sent"], false);
    assert!(
        !reply.text.contains("16505551234"),
        "the value is not echoed"
    );
    assert!(h.graph.requests().is_empty(), "no request reached Meta");
    // The key was never taken: the corrected request uses it.
    h.graph.push_json(200, accepted());
    let fixed = h.call(with_key(send(&key, &text()), "k-local")).await;
    assert_eq!(fixed.status, StatusCode::ACCEPTED, "{}", fixed.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// M1.4: a timeout may have sent the message: `504`, `may_have_been_sent:
/// true`, and the same key replays that answer without asking Meta again.
/// Decisive: keeping (not releasing) a key whose outcome may have been
/// sent, and the replay.
#[tokio::test]
async fn a_timeout_is_504_and_its_key_replays_it_without_a_second_request() {
    let (h, key) = connected().await;
    h.graph.push_error(|| TransportError::Timeout);
    let first = h
        .call(with_key(send(&key, &text()), "order:1234:shipped"))
        .await;
    assert_eq!(first.status, StatusCode::GATEWAY_TIMEOUT, "{}", first.text);
    let body = error(&first);
    assert_eq!(body["code"], "timeout");
    assert_eq!(body["may_have_been_sent"], true);
    assert_eq!(h.graph.requests().len(), 1);
    // The same key and body: the kept answer, no second send.
    let again = h
        .call(with_key(send(&key, &text()), "order:1234:shipped"))
        .await;
    assert_eq!(again.status, StatusCode::GATEWAY_TIMEOUT, "{}", again.text);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.text, first.text, "byte for byte");
    assert_eq!(h.graph.requests().len(), 1, "no second request");
    assert_eq!(h.graph.remaining(), 0);
}

/// M1.4: 131047 proves nothing was sent: `409
/// customer_service_window_closed`, and the key is released, so the same
/// key sends once the window is open. Decisive: the release.
#[tokio::test]
async fn a_131047_is_409_and_releases_the_key() {
    let (h, key) = connected().await;
    // support/error-codes: 131047 re-engagement message.
    h.graph.push_json(
        400,
        json!({"error": {"message": "(#131047) Re-engagement message", "type": "OAuthException",
                         "code": 131047,
                         "error_data": {"messaging_product": "whatsapp",
                                        "details": "Message failed to send because more than 24 hours have passed since the customer last replied to this number."},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let closed = h.call(with_key(send(&key, &text()), "k-window")).await;
    assert_eq!(closed.status, StatusCode::CONFLICT, "{}", closed.text);
    let body = error(&closed);
    assert_eq!(body["code"], "customer_service_window_closed");
    assert_eq!(body["may_have_been_sent"], false);
    assert_eq!(body["graph"]["code"], 131047);
    assert_eq!(
        body["graph"]["details"],
        "Message failed to send because more than 24 hours have passed since the customer last replied to this number."
    );
    assert!(
        !closed.text.contains("Re-engagement"),
        "Meta's message stays out"
    );
    // Released: the same key and body send.
    h.graph.push_json(200, accepted());
    let sent = h.call(with_key(send(&key, &text()), "k-window")).await;
    assert_eq!(sent.status, StatusCode::ACCEPTED, "{}", sent.text);
    assert!(sent.headers.get("idempotent-replayed").is_none());
    assert_eq!(
        h.graph.requests().len(),
        2,
        "the second request reached Meta"
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// Throttling is released too (nothing was processed); a 5xx is kept.
#[tokio::test]
async fn throttling_is_released_and_a_5xx_is_kept() {
    let (h, key) = connected().await;
    h.graph.push_json(
        429,
        json!({"error": {"message": "x", "type": "OAuthException", "code": 130429}}),
    );
    let throttled = h.call(with_key(send(&key, &text()), "k-429")).await;
    assert_eq!(error(&throttled)["code"], "rate_limited");
    assert_eq!(error(&throttled)["may_have_been_sent"], false);
    h.graph.push_json(
        500,
        json!({"error": {"message": "x", "type": "OAuthException", "code": 131000}}),
    );
    let failed = h.call(with_key(send(&key, &text()), "k-429")).await;
    assert_eq!(failed.status, StatusCode::BAD_GATEWAY, "{}", failed.text);
    assert_eq!(error(&failed)["may_have_been_sent"], true);
    let again = h.call(with_key(send(&key, &text()), "k-429")).await;
    assert_eq!(again.status, StatusCode::BAD_GATEWAY);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(h.graph.requests().len(), 2);
    assert_eq!(h.graph.remaining(), 0);
}

/// A success is replayed byte for byte to the same request, however its
/// JSON is spelled; another body under the key is refused.
#[tokio::test]
async fn a_success_is_replayed_and_another_body_is_refused() {
    let (h, key) = connected().await;
    h.graph.push_json(200, accepted());
    let first = h.call(with_key(send(&key, &text()), "k-ok")).await;
    assert_eq!(first.status, StatusCode::ACCEPTED);
    // The same request, keys in another order.
    let respelled: Value = serde_json::from_str(&format!(
        r#"{{"text": {{"body": {}, "preview_url": true}}, "type": "text", "to": {{"phone": "{TO}"}}}}"#,
        serde_json::to_string(text()["text"]["body"].as_str().unwrap()).unwrap()
    ))
    .unwrap();
    let again = h.call(with_key(send(&key, &respelled), "k-ok")).await;
    assert_eq!(again.status, StatusCode::ACCEPTED, "{}", again.text);
    assert_eq!(again.text, first.text);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    // Another body under the same key.
    let mut other = text();
    other["text"]["body"] = json!("Another message");
    let reused = h.call(with_key(send(&key, &other), "k-ok")).await;
    assert_eq!(
        (reused.status, error(&reused)["code"].as_str().unwrap()),
        (StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused")
    );
    assert_eq!(error(&reused)["may_have_been_sent"], false);
    assert_eq!(h.graph.requests().len(), 1);
    assert_eq!(h.graph.remaining(), 0);
}

/// Keys are the tenant's: another tenant's same key is another record,
/// and a platform key acting for the tenant shares the tenant's. Decisive:
/// the tenant in the record's key.
#[tokio::test]
async fn keys_are_scoped_to_the_tenant() {
    let (h, key) = connected().await;
    h.tenant("merchant-43").await;
    h.connect(
        "merchant-43",
        "102290129340399",
        &["106540352242923"],
        "TOKEN-43",
    )
    .await;
    let other = h.tenant_key("merchant-43", &[Scope::Send]).await;
    let platform = h
        .platform_key(
            AllowedTenants::Only(vec![TenantId::parse(TENANT).unwrap()]),
            &[Scope::Send],
        )
        .await;
    h.graph.push_json(200, accepted());
    let first = h.call(with_key(send(&key, &text()), "shared-key")).await;
    assert_eq!(first.status, StatusCode::ACCEPTED);
    // The same key at another tenant: its own send.
    h.graph.push_json(200, accepted());
    let theirs = h
        .call(with_key(
            Call::new(Method::POST, "/v1/numbers/106540352242923/messages")
                .key(&other)
                .json(&text()),
            "shared-key",
        ))
        .await;
    assert_eq!(theirs.status, StatusCode::ACCEPTED, "{}", theirs.text);
    assert!(theirs.headers.get("idempotent-replayed").is_none());
    assert_eq!(h.graph.last_request().unwrap().bearer(), Some("TOKEN-43"));
    // The platform key acting for the first tenant: its record.
    let replayed = h
        .call(with_key(
            send(&platform, &text()).tenant(TENANT),
            "shared-key",
        ))
        .await;
    assert_eq!(replayed.headers["idempotent-replayed"], "true");
    assert_eq!(h.graph.requests().len(), 2);
    assert_eq!(h.graph.remaining(), 0);
}

/// A request still holding the key: `409 idempotency_in_progress`; one
/// whose lease ran out (the process died): `409 outcome_unknown`, never a
/// new send. Decisive: the lease.
#[tokio::test]
async fn a_running_key_is_in_progress_and_a_lapsed_lease_is_unknown() {
    let (h, key) = connected_with(Settings {
        idempotency_lease: Duration::from_millis(150),
        ..common::test_settings()
    })
    .await;
    let tenant = TenantId::parse(TENANT).unwrap();
    let fingerprint = Fingerprint::json(
        &Method::POST,
        &format!("/v1/numbers/{PN}/messages"),
        &text(),
    );
    // Another request claimed it and is running (or died).
    let claimed = h
        .store
        .claim_idempotency_key(
            &tenant,
            &IdempotencyKey::parse("k-running").unwrap(),
            fingerprint.as_bytes(),
            "claim-of-another-request",
            Duration::from_millis(150),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    assert_eq!(claimed, IdempotencyClaim::Claimed);
    let running = h.call(with_key(send(&key, &text()), "k-running")).await;
    assert_eq!(running.status, StatusCode::CONFLICT, "{}", running.text);
    let body = error(&running);
    assert_eq!(body["code"], "idempotency_in_progress");
    assert_eq!(body["retryable"], true);
    assert_eq!(body["may_have_been_sent"], true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let unknown = h.call(with_key(send(&key, &text()), "k-running")).await;
    assert_eq!(unknown.status, StatusCode::CONFLICT, "{}", unknown.text);
    let body = error(&unknown);
    assert_eq!(body["code"], "outcome_unknown");
    assert_eq!(body["may_have_been_sent"], true);
    assert_eq!(body["retryable"], false);
    assert!(h.graph.requests().is_empty(), "never a new send");
}

/// A request dropped before it settled its key (the deadline cut it)
/// leaves a kept `504 timeout` that may have been sent, not a free key.
#[tokio::test]
async fn a_request_cut_before_settling_keeps_a_timeout() {
    let (h, _) = connected().await;
    let tenant = TenantId::parse(TENANT).unwrap();
    let key = IdempotencyKey::parse("k-cut").unwrap();
    let fingerprint = Fingerprint::json(&Method::POST, "/v1/numbers/1/messages", &json!({}));
    let never = std::future::pending::<Result<Success, meta_whatsapp_server::error::ApiError>>();
    let cut = tokio::time::timeout(
        Duration::from_millis(20),
        run(&h.state, &tenant, Some(key.clone()), fingerprint, never),
    )
    .await;
    assert!(cut.is_err(), "the request was cut");
    // The settlement runs on a task of its own.
    let mut kept = None;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let claim = h
            .store
            .claim_idempotency_key(
                &tenant,
                &key,
                fingerprint.as_bytes(),
                "probe",
                Duration::from_secs(60),
                Duration::from_secs(3600),
            )
            .await
            .unwrap();
        if let IdempotencyClaim::Existing(record) = claim
            && let IdempotencyState::Completed { status, body } = record.state
        {
            kept = Some((status, body));
            break;
        }
    }
    let (status, body) = kept.expect("the cut request's key was settled");
    assert_eq!(status, 504);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "timeout");
    assert_eq!(body["error"]["may_have_been_sent"], true);
}

#[tokio::test]
async fn malformed_keys_and_unsupported_types_send_nothing() {
    let (h, key) = connected().await;
    for bad in ["", "with space", &"k".repeat(256)] {
        let reply = h.call(with_key(send(&key, &text()), bad)).await;
        assert_eq!(
            (reply.status, error(&reply)["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("Idempotency-Key")),
            "{bad:?}"
        );
    }
    for body in [
        json!({"to": {"phone": TO}, "type": "order_details", "order_details": {}}),
        json!({"to": {"phone": TO}, "type": "text", "text": {"body": "x"}, "category": "utility"}),
        json!({"to": {"phone": TO}, "type": "interactive", "interactive": {"type": "product"}}),
    ] {
        let reply = h.call(send(&key, &body)).await;
        assert_eq!(
            (reply.status, error(&reply)["code"].as_str().unwrap()),
            (StatusCode::UNPROCESSABLE_ENTITY, "unsupported_message_type"),
            "{body}"
        );
    }
    assert!(h.graph.requests().is_empty());
}

/// A key without the `send` scope cannot send; a `send` key cannot read
/// the numbers.
#[tokio::test]
async fn sending_needs_the_send_scope() {
    let (h, send_key) = connected().await;
    let numbers = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    let reply = h.call(send(&numbers, &text())).await;
    assert_eq!(
        (reply.status, error(&reply)["code"].as_str().unwrap()),
        (StatusCode::FORBIDDEN, "forbidden")
    );
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}")).key(&send_key))
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(h.graph.requests().is_empty());
}

/// messages/mark-message-as-read and typing-indicators, as documented; an
/// empty body marks read.
#[tokio::test]
async fn read_receipts_and_typing_indicators_are_metas_requests() {
    let (h, key) = connected().await;
    let read = |body: Option<Value>| {
        let call = Call::new(
            Method::POST,
            format!("/v1/numbers/{PN}/messages/{RECEIVED}/read"),
        )
        .key(&key);
        match body {
            Some(body) => call.json(&body),
            None => call,
        }
    };
    for (body, typing) in [
        (None, false),
        (Some(json!({})), false),
        (Some(json!({"typing_indicator": true})), true),
    ] {
        h.graph.push_json(200, json!({"success": true}));
        let reply = h.call(read(body.clone())).await;
        assert_eq!(
            reply.status,
            StatusCode::NO_CONTENT,
            "{body:?}: {}",
            reply.text
        );
        let request = h.graph.last_request().unwrap();
        assert_eq!(request.path(), format!("/v25.0/{PN}/messages"));
        assert_eq!(request.bearer(), Some(TOKEN));
        let mut expected =
            json!({"messaging_product": "whatsapp", "status": "read", "message_id": RECEIVED});
        if typing {
            expected["typing_indicator"] = json!({"type": "text"});
        }
        assert_eq!(request.json().unwrap(), expected, "{body:?}");
    }
    let bad = h.call(read(Some(json!({"typing_indicator": "yes"})))).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(h.graph.remaining(), 0);
}

/// The tenant's `send` bucket: past its burst `429 too_many_requests` with
/// `Retry-After`, before ownership, the vault and Meta; other tenants and
/// other classes untouched; a platform key acting for the tenant draws
/// from the same bucket. Decisive: keying the bucket by tenant.
#[tokio::test]
async fn sends_past_the_tenants_limit_are_429_with_retry_after() {
    use meta_whatsapp_server::ratelimit::{Rate, RateLimits};
    let limits = RateLimits {
        send: Rate {
            per_second: 1,
            burst: 2,
        },
        ..common::unlimited()
    };
    let (h, key) = connected_with(Settings {
        rate_limits: limits,
        ..common::test_settings()
    })
    .await;
    h.tenant("merchant-43").await;
    h.connect(
        "merchant-43",
        "102290129340399",
        &["106540352242923"],
        "TOKEN-43",
    )
    .await;
    let other = h.tenant_key("merchant-43", &[Scope::Send]).await;
    let platform = h
        .platform_key(AllowedTenants::All, &[Scope::Send, Scope::Numbers])
        .await;
    for _ in 0..2 {
        h.graph.push_json(200, accepted());
        assert_eq!(
            h.call(send(&key, &text())).await.status,
            StatusCode::ACCEPTED
        );
    }
    let reads = h.kv.vault_reads();
    for limited in [send(&key, &text()), send(&platform, &text()).tenant(TENANT)] {
        let reply = h.call(limited).await;
        assert_eq!(
            reply.status,
            StatusCode::TOO_MANY_REQUESTS,
            "{}",
            reply.text
        );
        let body = error(&reply);
        assert_eq!(body["code"], "too_many_requests");
        assert_eq!(body["retryable"], true);
        assert_eq!(body["may_have_been_sent"], false);
        assert_eq!(reply.headers["retry-after"], "1");
    }
    assert_eq!(h.kv.vault_reads(), reads, "refused before the vault");
    // Another tenant, and another class of the same tenant: untouched.
    h.graph.push_json(200, accepted());
    let theirs = h
        .call(
            Call::new(Method::POST, "/v1/numbers/106540352242923/messages")
                .key(&other)
                .json(&text()),
        )
        .await;
    assert_eq!(theirs.status, StatusCode::ACCEPTED, "{}", theirs.text);
    let listed = h
        .call(Call::get("/v1/numbers").key(&platform).tenant(TENANT))
        .await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(h.graph.requests().len(), 3);
    assert_eq!(h.graph.remaining(), 0);
    let metrics = h.state.metrics().render();
    assert!(
        metrics.contains("wa_server_rate_limited_total{class=\"send\"} 2"),
        "{metrics}"
    );
}

/// Tenant ids are the integrator's (slugs, say): a tenant deleted and
/// created again under the same id starts without the deleted one's
/// idempotency records. Its first send under a key the deleted tenant
/// used goes to Meta, and never replays the old answer (another
/// merchant's message id). Decisive: deleting a tenant's records with it.
#[tokio::test]
async fn a_tenant_created_again_never_replays_the_deleted_ones_answers() {
    let (h, key) = connected().await;
    let admin = h.admin_key().await;
    h.graph.push_json(200, accepted());
    let first = h
        .call(with_key(send(&key, &text()), "order:1234:shipped"))
        .await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.text);
    // The operator deletes the tenant (its WABA disconnected first) and
    // an integrator creates one again with the same id.
    h.graph.push_json(200, json!({"success": true}));
    let deleted = h
        .call(Call::new(Method::DELETE, format!("/v1/admin/tenants/{TENANT}")).key(&admin))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.text);
    let created = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants")
                .key(&admin)
                .json(&json!({"id": TENANT})),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    h.connect(TENANT, WABA, &[PN], "EAAG-new-merchant-token")
        .await;
    let new_key = h.tenant_key(TENANT, &[Scope::Send]).await;
    // Same key, same body: a send of its own.
    let mut answer = accepted();
    answer["messages"][0]["id"] = json!("wamid.OF-THE-NEW-TENANT");
    h.graph.push_json(200, answer);
    let again = h
        .call(with_key(send(&new_key, &text()), "order:1234:shipped"))
        .await;
    assert_eq!(again.status, StatusCode::ACCEPTED, "{}", again.text);
    assert!(again.headers.get("idempotent-replayed").is_none());
    assert_eq!(again.json()["message_id"], "wamid.OF-THE-NEW-TENANT");
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.path(), format!("/v25.0/{PN}/messages"));
    assert_eq!(request.bearer(), Some("EAAG-new-merchant-token"));
    assert_eq!(h.graph.remaining(), 0);
}
