//! OTP over HTTP (roadmap M3c; docs/design/server.md, sections 4.2 and
//! 4.7): `POST /v1/otp/issue`, `POST /v1/otp/verify`, the tenants' OTP
//! settings (`/v1/admin/tenants/{id}/otp`, decision D8) and authentication
//! templates (`POST /v1/wabas/{waba_id}/templates/authentication`).
//!
//! Acceptance test M3.5, exactly as the design writes it: every outcome;
//! tenant A's code verifies at no other tenant on the same number
//! (decisive: the namespace); logs hold neither code nor number; a
//! sentinel in a scripted Graph error on issue reaches no response (the
//! logs' case is `otp_logs.rs`, alone in its binary as `logs.rs` is). And
//! the guards the item adds: the sending number is the tenant's own
//! binding (or the default OTP tenant's), the rate limits per tenant and
//! per recipient, the issue's `Idempotency-Key`, the scope.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness, Reply, otp_deployment, test_settings};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{AllowedTenants, OtpSettings, Scope, TenantId};
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

/// The platform's tenant, holding the platform's number: the default OTP
/// tenant (decision D8's default).
const PLATFORM: &str = "platform";
const PLATFORM_WABA: &str = "102290129340398";
const PLATFORM_PN: &str = "106540352242922";
const PLATFORM_TOKEN: &str = "EAAG-platform-token";
/// Two merchants without settings of their own: both send from the
/// platform's number.
const A: &str = "shop-a";
const B: &str = "shop-b";
/// A merchant with its own number and settings.
const OWN: &str = "shop-own";
const OWN_WABA: &str = "102290129340399";
const OWN_PN: &str = "106540352242923";
const OWN_TOKEN: &str = "EAAG-own-token";
const PHONE: &str = "+16505551234";
const PHONE_DIGITS: &str = "16505551234";
const OTHER_PHONE: &str = "+16505559876";
const TEMPLATE: &str = "login_code";

/// Meta's answer to a send (`messages/send-messages`, example response).
fn accepted() -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": PHONE, "wa_id": PHONE_DIGITS}],
        "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
    })
}

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

/// The platform holding its number, with its settings, the default OTP
/// tenant; A and B without settings; `OWN` with its own number and
/// settings. Each merchant has a key with scope `otp`.
struct Otp {
    h: Harness,
    a: String,
    b: String,
    own: String,
}

async fn otp_with(settings: Settings, platform: OtpSettings) -> Otp {
    let h = Harness::with_settings(settings);
    for t in [PLATFORM, A, B, OWN] {
        h.tenant(t).await;
    }
    h.connect(PLATFORM, PLATFORM_WABA, &[PLATFORM_PN], PLATFORM_TOKEN)
        .await;
    h.connect(OWN, OWN_WABA, &[OWN_PN], OWN_TOKEN).await;
    assert!(
        h.store
            .set_otp_settings(&tenant(PLATFORM), Some(&platform))
            .await
            .unwrap()
    );
    let own = OtpSettings::new(PhoneNumberId::new(OWN_PN).unwrap(), "own_code", "fr");
    assert!(
        h.store
            .set_otp_settings(&tenant(OWN), Some(&own))
            .await
            .unwrap()
    );
    Otp {
        a: h.tenant_key(A, &[Scope::Otp]).await,
        b: h.tenant_key(B, &[Scope::Otp]).await,
        own: h.tenant_key(OWN, &[Scope::Otp]).await,
        h,
    }
}

fn platform_settings() -> OtpSettings {
    OtpSettings::new(PhoneNumberId::new(PLATFORM_PN).unwrap(), TEMPLATE, "en_US")
}

fn with_default() -> Settings {
    Settings {
        otp: Some(otp_deployment(Some(PLATFORM))),
        ..test_settings()
    }
}

async fn otp() -> Otp {
    otp_with(with_default(), platform_settings()).await
}

fn issue(key: &str, body: &Value) -> Call {
    Call::new(Method::POST, "/v1/otp/issue").key(key).json(body)
}

fn verify(key: &str, phone: &str, code: &str) -> Call {
    Call::new(Method::POST, "/v1/otp/verify")
        .key(key)
        .json(&json!({"phone": phone, "code": code}))
}

/// The code the last send carried, read from the request Meta got, and
/// that request checked against the copy-code page's send example: the
/// code as the body parameter and as the URL button's.
fn sent_code(h: &Harness, from: &str, token: &str, name: &str, language: &str) -> String {
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{from}/messages"));
    assert_eq!(request.bearer(), Some(token));
    let body = request.json().unwrap();
    let code = body["template"]["components"][0]["parameters"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(code.len(), 6);
    assert!(code.bytes().all(|b| b.is_ascii_digit()), "{code}");
    assert_eq!(
        body,
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": PHONE,
            "type": "template",
            "template": {
                "name": name,
                "language": {"code": language},
                "components": [
                    {"type": "body", "parameters": [{"type": "text", "text": code}]},
                    {"type": "button", "sub_type": "url", "index": "0",
                     "parameters": [{"type": "text", "text": code}]}
                ]
            }
        })
    );
    code
}

fn status(reply: &Reply) -> (StatusCode, String) {
    (reply.status, reply.code())
}

/// M3.5, every outcome. An issue is `sent` (Meta's documented send, with
/// the platform's token from its number), then `cooling_down` with no
/// second request; a check is `invalid` with the attempts left, then
/// `verified`, then `not_found` (used up); `not_found` for a recipient
/// never sent a code; `expired` past the code's lifetime;
/// `too_many_attempts` once the attempts are used, even with the right
/// code; `rate_limited` once the recipient's issue limit is used.
#[tokio::test]
async fn m3_5_every_outcome() {
    let mut settings = platform_settings();
    // A cooldown of 0, so that the issue limit is reached at once.
    settings.resend_cooldown_seconds = 0;
    settings.max_issues = 2;
    let t = otp_with(with_default(), settings).await;
    let h = &t.h;

    // sent, then invalid, verified, not_found.
    h.graph.push_json(200, accepted());
    let sent = h.call(issue(&t.a, &json!({"phone": PHONE}))).await;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.text);
    let body = sent.json();
    assert_eq!(body["status"], "sent");
    let challenge = body["challenge_id"].as_str().unwrap();
    assert_eq!(challenge.len(), 32, "the library's random id");
    assert!(body["expires_at"].is_string());
    assert_eq!(body.as_object().unwrap().len(), 3, "{body}");
    let code = sent_code(h, PLATFORM_PN, PLATFORM_TOKEN, TEMPLATE, "en_US");
    assert!(!sent.text.contains(&code), "the code is in no answer");
    let wrong = if code == "000000" { "111111" } else { "000000" };
    let invalid = h.call(verify(&t.a, PHONE, wrong)).await;
    assert_eq!(invalid.status, StatusCode::OK, "{}", invalid.text);
    assert_eq!(
        invalid.json(),
        json!({"result": "invalid", "attempts_left": 4})
    );
    let verified = h.call(verify(&t.a, PHONE, &code)).await;
    assert_eq!(verified.json(), json!({"result": "verified"}));
    let used = h.call(verify(&t.a, PHONE, &code)).await;
    assert_eq!(used.json(), json!({"result": "not_found"}), "used up");
    let never = h.call(verify(&t.a, OTHER_PHONE, &code)).await;
    assert_eq!(never.json(), json!({"result": "not_found"}));

    // expired.
    h.graph.push_json(200, accepted());
    let sent = h.call(issue(&t.a, &json!({"phone": PHONE}))).await;
    assert_eq!(sent.json()["status"], "sent", "{}", sent.text);
    let code = sent_code(h, PLATFORM_PN, PLATFORM_TOKEN, TEMPLATE, "en_US");
    h.clock.advance(std::time::Duration::from_secs(601));
    let expired = h.call(verify(&t.a, PHONE, &code)).await;
    assert_eq!(expired.json(), json!({"result": "expired"}));

    // too_many_attempts, the right code included; then rate_limited: the
    // third code in the hour was this one.
    h.graph.push_json(200, accepted());
    let sent = h
        .call(issue(&t.a, &json!({"phone": PHONE, "purpose": "reset"})))
        .await;
    assert_eq!(sent.json()["status"], "sent", "{}", sent.text);
    let code = sent_code(h, PLATFORM_PN, PLATFORM_TOKEN, TEMPLATE, "en_US");
    let wrong = if code == "000000" { "111111" } else { "000000" };
    let check = |code: &str| {
        Call::new(Method::POST, "/v1/otp/verify")
            .key(&t.a)
            .json(&json!({"phone": PHONE, "purpose": "reset", "code": code}))
    };
    for left in (0..5).rev() {
        let reply = h.call(check(wrong)).await;
        assert_eq!(
            reply.json(),
            json!({"result": "invalid", "attempts_left": left})
        );
    }
    let locked = h.call(check(&code)).await;
    assert_eq!(locked.json(), json!({"result": "too_many_attempts"}));
    let asked = h.graph.requests().len();
    let limited = h.call(issue(&t.a, &json!({"phone": PHONE}))).await;
    assert_eq!(limited.status, StatusCode::OK, "{}", limited.text);
    let body = limited.json();
    assert_eq!(body["status"], "rate_limited", "{body}");
    let wait = body["retry_after_seconds"].as_u64().unwrap();
    assert!((2990..=3600).contains(&wait), "{wait}");
    assert_eq!(body.as_object().unwrap().len(), 2, "{body}");
    assert_eq!(h.graph.requests().len(), asked, "nothing sent");

    // cooling_down: on the default settings (30 s).
    let t = otp().await;
    let h = &t.h;
    h.graph.push_json(200, accepted());
    assert_eq!(
        h.call(issue(&t.a, &json!({"phone": PHONE}))).await.json()["status"],
        "sent"
    );
    let again = h.call(issue(&t.a, &json!({"phone": PHONE}))).await;
    assert_eq!(
        again.json(),
        json!({"status": "cooling_down", "retry_after_seconds": 30})
    );
    assert_eq!(h.graph.requests().len(), 1, "nothing sent");
    assert_eq!(h.graph.remaining(), 0);
}

/// M3.5, decisive, the scenario `live_postgres.rs` runs on Postgres too
/// (the settings set through the admin route).
#[tokio::test]
async fn m3_5_the_namespace_scenario() {
    let h = Harness::with_settings(with_default());
    common::otp::a_code_verifies_at_no_other_tenant_on_the_same_number(&h).await;
}

/// M3.5, decisive: the namespace. Tenant A's code, sent from the
/// platform's number that tenant B sends from too, verifies at no other
/// tenant: B's checks (its own key, or a platform key naming B) are
/// `not_found`, with nothing said of A's code, and count no attempt
/// against it: A still verifies it after B's five tries. B's cooldown is
/// its own as well. Removing the namespace (the holder's id, or none)
/// fails it.
#[tokio::test]
async fn m3_5_a_tenants_code_verifies_at_no_other_tenant_on_the_same_number() {
    let t = otp().await;
    let h = &t.h;
    let platform_key = h
        .platform_key(AllowedTenants::Only(vec![tenant(B)]), &[Scope::Otp])
        .await;
    h.graph.push_json(200, accepted());
    let sent = h.call(issue(&t.a, &json!({"phone": PHONE}))).await;
    assert_eq!(sent.json()["status"], "sent", "{}", sent.text);
    let code = sent_code(h, PLATFORM_PN, PLATFORM_TOKEN, TEMPLATE, "en_US");
    for _ in 0..5 {
        let by_b = h.call(verify(&t.b, PHONE, &code)).await;
        assert_eq!(by_b.json(), json!({"result": "not_found"}));
        let as_b = h.call(verify(&platform_key, PHONE, &code).tenant(B)).await;
        assert_eq!(as_b.json(), json!({"result": "not_found"}));
    }
    // Nor at the number's holder.
    let holder = h.tenant_key(PLATFORM, &[Scope::Otp]).await;
    let by_holder = h.call(verify(&holder, PHONE, &code)).await;
    assert_eq!(by_holder.json(), json!({"result": "not_found"}));
    // B's cooldown and limit are its own: it sends to the same recipient
    // from the same number at once.
    h.graph.push_json(200, accepted());
    let b_sent = h.call(issue(&t.b, &json!({"phone": PHONE}))).await;
    assert_eq!(b_sent.json()["status"], "sent", "{}", b_sent.text);
    let b_code = sent_code(h, PLATFORM_PN, PLATFORM_TOKEN, TEMPLATE, "en_US");
    // A's code, untouched by B's checks, verifies at A, and B's at B.
    let by_a = h.call(verify(&t.a, PHONE, &code)).await;
    assert_eq!(by_a.json(), json!({"result": "verified"}));
    if b_code != code {
        let a_with_b = h.call(verify(&t.a, PHONE, &b_code)).await;
        assert_eq!(a_with_b.json(), json!({"result": "not_found"}), "used up");
    }
    assert_eq!(
        h.call(verify(&t.b, PHONE, &b_code)).await.json(),
        json!({"result": "verified"})
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// M3.5: a sentinel in a scripted Graph error on issue reaches no
/// response: Meta's message, title, user texts and `details` (which every
/// other route of the service keeps, bounded) are dropped on the OTP
/// route; its code, status and trace id stay. A 5xx and a timeout may
/// have been sent (the code stays verifiable), a 4xx may not.
#[tokio::test]
async fn m3_5_a_sentinel_in_a_graph_error_on_issue_reaches_no_response() {
    const SENTINEL: &str = "SENTINEL-c0de-5ee";
    let error = |code: i64| {
        json!({"error": {
            "message": format!("({code}) {SENTINEL} message"),
            "title": format!("{SENTINEL} title"),
            "type": "OAuthException",
            "code": code,
            "error_user_title": format!("{SENTINEL} user title"),
            "error_user_msg": format!("{SENTINEL} user message"),
            "error_data": {"messaging_product": "whatsapp", "details": format!("{SENTINEL} details")},
            "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"
        }})
    };
    for (http, code, answered, sent) in [
        (400, 132000, (422, "template_parameter_mismatch"), false),
        (400, 131062, (422, "recipient_not_supported"), false),
        (400, 100, (422, "invalid_parameter"), false),
        (500, 131000, (502, "service_unavailable"), true),
    ] {
        let t = otp().await;
        t.h.graph.push_json(http, error(code));
        let reply = t.h.call(issue(&t.a, &json!({"phone": PHONE}))).await;
        assert!(!reply.text.contains(SENTINEL), "{code}: {}", reply.text);
        assert_eq!(
            (reply.status.as_u16(), reply.code().as_str()),
            answered,
            "{code}: {}",
            reply.text
        );
        let body = reply.json();
        assert_eq!(body["error"]["graph"]["code"], code);
        assert_eq!(
            body["error"]["graph"]["fbtrace_id"],
            "AXsgnV2Cm3ZMGF3dF_cfYIn"
        );
        assert!(body["error"]["graph"]["details"].is_null(), "{body}");
        assert_eq!(body["error"]["may_have_been_sent"], sent, "{code}");
    }
    // A timeout: `504`, may have been sent.
    let t = otp().await;
    t.h.graph.push_error(|| TransportError::Timeout);
    let reply = t.h.call(issue(&t.a, &json!({"phone": PHONE}))).await;
    assert_eq!(
        status(&reply),
        (StatusCode::GATEWAY_TIMEOUT, "timeout".to_owned())
    );
    assert_eq!(reply.json()["error"]["may_have_been_sent"], true);
}

/// The sending number is the tenant's own binding (decision D8): a tenant
/// with settings sends from its own number, with its own token and
/// template; one whose settings name a number it no longer holds is `409
/// otp_not_configured` with the vault unread and Meta unasked; a tenant
/// without settings and no default OTP tenant is `409
/// otp_not_configured`, and so is every tenant on a service without OTP.
/// Decisive: the holder check of the authorization order on the sending
/// number.
#[tokio::test]
async fn the_sending_number_is_its_holders_own() {
    let t = otp().await;
    let h = &t.h;
    h.graph.push_json(200, accepted());
    let sent = h.call(issue(&t.own, &json!({"phone": PHONE}))).await;
    assert_eq!(sent.json()["status"], "sent", "{}", sent.text);
    sent_code(h, OWN_PN, OWN_TOKEN, "own_code", "fr");

    // OWN's number moves to A: OWN's settings no longer name its own.
    h.store
        .unbind_waba(&meta_whatsapp_rs::core::ids::WabaId::new(OWN_WABA).unwrap())
        .await
        .unwrap();
    h.connect(A, OWN_WABA, &[OWN_PN], "EAAG-a-token").await;
    let reads = h.kv.vault_reads();
    let stale = h.call(issue(&t.own, &json!({"phone": OTHER_PHONE}))).await;
    assert_eq!(
        status(&stale),
        (StatusCode::CONFLICT, "otp_not_configured".to_owned()),
        "{}",
        stale.text
    );
    assert_eq!(h.kv.vault_reads(), reads, "A's token was never read");
    assert_eq!(h.graph.requests().len(), 1, "Meta was not asked");

    // No settings, no default OTP tenant.
    let t = otp_with(
        Settings {
            otp: Some(otp_deployment(None)),
            ..test_settings()
        },
        platform_settings(),
    )
    .await;
    let reply = t.h.call(issue(&t.a, &json!({"phone": PHONE}))).await;
    assert_eq!(status(&reply).1, "otp_not_configured", "{}", reply.text);
    let reply = t.h.call(verify(&t.a, PHONE, "123456")).await;
    assert_eq!(status(&reply).1, "otp_not_configured", "{}", reply.text);
    // A service without OTP.
    let t = otp_with(
        Settings {
            otp: None,
            ..test_settings()
        },
        platform_settings(),
    )
    .await;
    for call in [
        issue(&t.own, &json!({"phone": PHONE})),
        verify(&t.own, PHONE, "123456"),
    ] {
        let reply = t.h.call(call).await;
        assert_eq!(status(&reply).1, "otp_not_configured", "{}", reply.text);
    }
    assert!(t.h.graph.requests().is_empty());
}

/// The rate limits are per tenant (the service's `otp_issue` and
/// `otp_verify` budgets: `429 too_many_requests`, `Retry-After`, nothing
/// sent, another tenant's budget whole) and per recipient (the library's
/// cooldown and issue limit, under the tenant's namespace: another
/// recipient of the same tenant still gets a code).
#[tokio::test]
async fn rate_limits_are_per_tenant_and_per_recipient() {
    use meta_whatsapp_server::ratelimit::Rate;
    let one = Rate {
        per_second: 1,
        burst: 1,
    };
    let mut settings = with_default();
    settings.rate_limits.otp_issue = one;
    settings.rate_limits.otp_verify = one;
    let t = otp_with(settings, platform_settings()).await;
    let h = &t.h;
    h.graph.push_json(200, accepted());
    assert_eq!(
        h.call(issue(&t.a, &json!({"phone": PHONE}))).await.json()["status"],
        "sent"
    );
    let limited = h.call(issue(&t.a, &json!({"phone": OTHER_PHONE}))).await;
    assert_eq!(
        status(&limited),
        (
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_requests".to_owned()
        )
    );
    assert!(limited.headers.contains_key("retry-after"));
    assert!(
        h.call(verify(&t.a, PHONE, "123456"))
            .await
            .status
            .is_success()
    );
    assert_eq!(
        h.call(verify(&t.a, PHONE, "123456")).await.code(),
        "too_many_requests"
    );
    // B's budgets are whole.
    h.graph.push_json(200, accepted());
    assert_eq!(
        h.call(issue(&t.b, &json!({"phone": OTHER_PHONE})))
            .await
            .json()["status"],
        "sent"
    );
    assert_eq!(h.graph.requests().len(), 2);

    // Per recipient, under the tenant's namespace.
    let t = otp().await;
    let h = &t.h;
    h.graph.push_json(200, accepted());
    h.graph.push_json(200, accepted());
    assert_eq!(
        h.call(issue(&t.a, &json!({"phone": PHONE}))).await.json()["status"],
        "sent"
    );
    assert_eq!(
        h.call(issue(&t.a, &json!({"phone": PHONE}))).await.json()["status"],
        "cooling_down"
    );
    assert_eq!(
        h.call(issue(&t.a, &json!({"phone": OTHER_PHONE})))
            .await
            .json()["status"],
        "sent",
        "another recipient"
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// The request is checked before anything is read or sent: a phone that
/// is not strict E.164 with `+` (a BSUID, a `wa_id`, separators), a bad
/// purpose, an unknown key, an empty or long code. A key without scope
/// `otp` is `403 forbidden`.
#[tokio::test]
async fn requests_are_checked_first_and_need_the_otp_scope() {
    let t = otp().await;
    let h = &t.h;
    for (body, field) in [
        (json!({"phone": PHONE_DIGITS}), "phone"),
        (json!({"phone": "US.13491208655302741918"}), "phone"),
        (json!({"phone": "+1 650 555 1234"}), "phone"),
        (json!({"phone": PHONE, "purpose": "two words"}), "purpose"),
        (json!({"phone": PHONE, "namespace": "shop-b"}), "body"),
        (json!({"phone": PHONE, "code": "123456"}), "body"),
    ] {
        let reply = h.call(issue(&t.a, &body)).await;
        assert_eq!(
            status(&reply).1,
            "invalid_request",
            "{body}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
    }
    for (code, field) in [("", "code"), ("1234567890123456", "code")] {
        let reply = h.call(verify(&t.a, PHONE, code)).await;
        assert_eq!(reply.json()["error"]["field"], field, "{code:?}");
    }
    let send_only = h.tenant_key(A, &[Scope::Send]).await;
    for call in [
        issue(&send_only, &json!({"phone": PHONE})),
        verify(&send_only, PHONE, "123456"),
    ] {
        let reply = h.call(call).await;
        assert_eq!(
            status(&reply),
            (StatusCode::FORBIDDEN, "forbidden".to_owned())
        );
    }
    assert!(h.graph.requests().is_empty());
}

/// `Idempotency-Key` on an issue (design section 5.4): a repeat with the
/// same body gets the kept answer (`Idempotent-Replayed: true`) and sends
/// no second code; another body is `422 idempotency_key_reused`; a timeout
/// keeps the key (the code may be on its way), a `4xx` releases it.
#[tokio::test]
async fn an_issue_with_an_idempotency_key_never_sends_twice() {
    let t = otp().await;
    let h = &t.h;
    let keyed = |body: &Value, key: &str| issue(&t.a, body).header("idempotency-key", key);
    h.graph.push_json(200, accepted());
    let first = h.call(keyed(&json!({"phone": PHONE}), "login:42")).await;
    assert_eq!(first.json()["status"], "sent", "{}", first.text);
    let again = h.call(keyed(&json!({"phone": PHONE}), "login:42")).await;
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.json(), first.json());
    assert_eq!(h.graph.requests().len(), 1);
    let other = h
        .call(keyed(&json!({"phone": OTHER_PHONE}), "login:42"))
        .await;
    assert_eq!(status(&other).1, "idempotency_key_reused");

    h.graph.push_error(|| TransportError::Timeout);
    let cut = h
        .call(keyed(&json!({"phone": OTHER_PHONE}), "login:43"))
        .await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT);
    let replayed = h
        .call(keyed(&json!({"phone": OTHER_PHONE}), "login:43"))
        .await;
    assert_eq!(replayed.headers["idempotent-replayed"], "true");
    assert_eq!(replayed.status, StatusCode::GATEWAY_TIMEOUT);

    h.graph.push_json(
        400,
        json!({"error": {"message": "(#100) Invalid parameter", "type": "OAuthException", "code": 100}}),
    );
    let refused = h
        .call(keyed(&json!({"phone": PHONE, "purpose": "x"}), "login:44"))
        .await;
    assert_eq!(status(&refused).1, "invalid_parameter");
    h.graph.push_json(200, accepted());
    let retried = h
        .call(keyed(&json!({"phone": PHONE, "purpose": "x"}), "login:44"))
        .await;
    assert_eq!(
        retried.json()["status"],
        "sent",
        "released: {}",
        retried.text
    );
    assert!(retried.headers.get("idempotent-replayed").is_none());
    assert_eq!(h.graph.remaining(), 0);
}

/// The admin's OTP settings: set (defaults filled in, audited), read,
/// replaced, cleared; the sending number must be the tenant's own (`422`
/// on `phone_number_id` for another tenant's or nobody's, nothing stored);
/// each limit checked; an unknown tenant `404`. The issue limit cannot be
/// turned off.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn the_admin_sets_a_tenants_otp_settings_on_its_own_number_only() {
    let t = otp().await;
    let h = &t.h;
    let admin = h.admin_key().await;
    let put = |tenant: &str, body: &Value| {
        Call::new(Method::PUT, format!("/v1/admin/tenants/{tenant}/otp"))
            .key(&admin)
            .json(body)
    };
    let saved = h
        .call(put(
            OWN,
            &json!({"phone_number_id": OWN_PN, "template": {"name": "login_code", "language": "en_US"},
                    "ttl_seconds": 300, "issue_limit": {"max_issues": 3, "window_seconds": 3600}}),
        ))
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);
    let expected = json!({
        "phone_number_id": OWN_PN,
        "template": {"name": "login_code", "language": "en_US"},
        "code_length": 6,
        "ttl_seconds": 300,
        "max_attempts": 5,
        "resend_cooldown_seconds": 30,
        "issue_limit": {"max_issues": 3, "window_seconds": 3600}
    });
    assert_eq!(saved.json(), expected);
    let read = h
        .call(Call::get(format!("/v1/admin/tenants/{OWN}/otp")).key(&admin))
        .await;
    assert_eq!(read.json(), expected);
    let stored = h.store.otp_settings(&tenant(OWN)).await.unwrap().unwrap();
    assert_eq!(stored.ttl_seconds, 300);

    // Another tenant's number, or nobody's: refused, nothing stored.
    for pn in [PLATFORM_PN, "999999999999"] {
        let refused = h
            .call(put(
                A,
                &json!({"phone_number_id": pn, "template": {"name": "login_code", "language": "en_US"}}),
            ))
            .await;
        assert_eq!(status(&refused).1, "invalid_request", "{}", refused.text);
        assert_eq!(refused.json()["error"]["field"], "phone_number_id");
    }
    assert!(h.store.otp_settings(&tenant(A)).await.unwrap().is_none());
    for (body, field) in [
        (
            json!({"phone_number_id": OWN_PN, "template": {"name": "Login", "language": "en_US"}}),
            "template.name",
        ),
        (
            json!({"phone_number_id": OWN_PN, "template": {"name": "login", "language": ""}}),
            "template.language",
        ),
        (
            json!({"phone_number_id": OWN_PN, "template": {"name": "login", "language": "en"}, "code_length": 9}),
            "code_length",
        ),
        (
            json!({"phone_number_id": OWN_PN, "template": {"name": "login", "language": "en"}, "ttl_seconds": 0}),
            "ttl_seconds",
        ),
        (
            json!({"phone_number_id": OWN_PN, "template": {"name": "login", "language": "en"}, "max_attempts": 0}),
            "max_attempts",
        ),
        (
            json!({"phone_number_id": OWN_PN, "template": {"name": "login", "language": "en"},
                "issue_limit": {"max_issues": 0, "window_seconds": 3600}}),
            "issue_limit",
        ),
        (
            json!({"phone_number_id": OWN_PN, "template": {"name": "login", "language": "en"}, "issue_limit": null,
                "namespace": "x"}),
            "body",
        ),
        (
            json!({"phone_number_id": "abc", "template": {"name": "login", "language": "en"}}),
            "phone_number_id",
        ),
    ] {
        let refused = h.call(put(OWN, &body)).await;
        assert_eq!(
            refused.json()["error"]["field"],
            field,
            "{body}: {}",
            refused.text
        );
    }
    assert_eq!(
        h.store
            .otp_settings(&tenant(OWN))
            .await
            .unwrap()
            .unwrap()
            .ttl_seconds,
        300,
        "a refused change kept the settings"
    );
    let unknown = h
        .call(put(
            "nobody",
            &json!({"phone_number_id": OWN_PN, "template": {"name": "login", "language": "en"}}),
        ))
        .await;
    assert_eq!(
        status(&unknown),
        (StatusCode::NOT_FOUND, "not_found".to_owned())
    );

    let cleared = h
        .call(Call::new(Method::DELETE, format!("/v1/admin/tenants/{OWN}/otp")).key(&admin))
        .await;
    assert_eq!(cleared.status, StatusCode::NO_CONTENT);
    let gone = h
        .call(Call::get(format!("/v1/admin/tenants/{OWN}/otp")).key(&admin))
        .await;
    assert_eq!(status(&gone).1, "not_found");
    // Cleared, OWN borrows the platform's settings.
    h.graph.push_json(200, accepted());
    let sent = h.call(issue(&t.own, &json!({"phone": PHONE}))).await;
    assert_eq!(sent.json()["status"], "sent", "{}", sent.text);
    sent_code(h, PLATFORM_PN, PLATFORM_TOKEN, TEMPLATE, "en_US");
    // A tenant key is no admin key.
    let refused = h
        .call(Call::get(format!("/v1/admin/tenants/{OWN}/otp")).key(&t.own))
        .await;
    assert_eq!(status(&refused).1, "forbidden");
}

/// Authentication templates (`templates/authentication-templates/bulk-management`):
/// Meta's copy-code and one-tap examples, in three languages, sent as
/// Meta's request and answered with Meta's example response; zero-tap and
/// a misspelled button refused before any request; another tenant's WABA
/// `404` with the vault unread (the M1.3 table covers the route too).
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn authentication_templates_are_created_as_meta_documents() {
    let h = Harness::new();
    h.tenant(OWN).await;
    h.connect(OWN, OWN_WABA, &[OWN_PN], OWN_TOKEN).await;
    let key = h.tenant_key(OWN, &[Scope::Templates]).await;
    let response = json!({"data": [
        {"id": "954638012257287", "status": "APPROVED", "language": "en_US"},
        {"id": "969725527415202", "status": "APPROVED", "language": "es_ES"},
        {"id": "969725530748535", "status": "APPROVED", "language": "fr"}
    ]});
    let create = |body: &Value| {
        Call::new(
            Method::POST,
            format!("/v1/wabas/{OWN_WABA}/templates/authentication"),
        )
        .key(&key)
        .json(body)
    };
    h.graph.push_json(200, response.clone());
    let created = h
        .call(create(&json!({
            "name": "authentication_code_copy_code_button",
            "languages": ["en_US", "es_ES", "fr"],
            "otp_type": "copy_code",
            "add_security_recommendation": true,
            "code_expiration_minutes": 10
        })))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    assert_eq!(created.json(), response);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(
        request.path(),
        format!("/v25.0/{OWN_WABA}/upsert_message_templates")
    );
    assert_eq!(request.bearer(), Some(OWN_TOKEN));
    assert_eq!(
        request.json(),
        Some(json!({
            "name": "authentication_code_copy_code_button",
            "languages": ["en_US", "es_ES", "fr"],
            "category": "AUTHENTICATION",
            "components": [
                {"type": "BODY", "add_security_recommendation": true},
                {"type": "FOOTER", "code_expiration_minutes": 10},
                {"type": "BUTTONS", "buttons": [{"type": "OTP", "otp_type": "COPY_CODE"}]}
            ]
        }))
    );
    h.graph.push_json(200, response);
    let one_tap = h
        .call(create(&json!({
            "name": "authentication_code_autofill_button",
            "languages": ["en_US", "es_ES", "fr"],
            "otp_type": "one_tap",
            "supported_apps": [{"package_name": "com.example.luckyshrub", "signature_hash": "K8a/AINcGX7"}],
            "add_security_recommendation": true,
            "code_expiration_minutes": 15
        })))
        .await;
    assert_eq!(one_tap.status, StatusCode::CREATED, "{}", one_tap.text);
    assert_eq!(
        h.graph.last_request().unwrap().json(),
        Some(json!({
            "name": "authentication_code_autofill_button",
            "languages": ["en_US", "es_ES", "fr"],
            "category": "AUTHENTICATION",
            "components": [
                {"type": "BODY", "add_security_recommendation": true},
                {"type": "FOOTER", "code_expiration_minutes": 15},
                {"type": "BUTTONS", "buttons": [{
                    "type": "OTP",
                    "otp_type": "ONE_TAP",
                    "supported_apps": [{"package_name": "com.example.luckyshrub", "signature_hash": "K8a/AINcGX7"}]
                }]}
            ]
        }))
    );
    for (body, field) in [
        (
            json!({"name": "zero_tap_auth_template", "languages": ["en_US"], "otp_type": "zero_tap",
                   "supported_apps": [{"package_name": "com.example.luckyshrub", "signature_hash": "K8a/AINcGX7"}]}),
            "otp_type",
        ),
        (
            json!({"name": "c", "languages": ["en_US"], "otp_type": "copy-code"}),
            "otp_type",
        ),
        (
            json!({"name": "c", "languages": [], "otp_type": "copy_code"}),
            "languages",
        ),
        (
            json!({"name": "C", "languages": ["en_US"], "otp_type": "copy_code"}),
            "name",
        ),
        (
            json!({"name": "c", "languages": ["en_US"], "otp_type": "one_tap"}),
            "supported_apps",
        ),
        (
            json!({"name": "c", "languages": ["en_US"], "otp_type": "copy_code", "code_expiration_minutes": 91}),
            "code_expiration_minutes",
        ),
        (
            json!({"name": "c", "languages": ["en_US"], "otp_type": "copy_code", "zero_tap_terms_accepted": true}),
            "body",
        ),
    ] {
        let refused = h.call(create(&body)).await;
        assert_eq!(
            status(&refused).1,
            "invalid_request",
            "{body}: {}",
            refused.text
        );
        assert_eq!(refused.json()["error"]["field"], field, "{body}");
    }
    assert_eq!(h.graph.requests().len(), 2, "no refused one reached Meta");
    assert_eq!(h.graph.remaining(), 0);
}
