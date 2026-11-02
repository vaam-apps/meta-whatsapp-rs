//! Roadmap M3d's sentinel, shared by `registration.rs` (memory),
//! `registration_logs.rs` (the logs) and `live_postgres.rs` (every table):
//! a number's whole registration over HTTP with a PIN and a code no store,
//! log or answer may hold.

use std::time::Duration;

use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::idempotency::Fingerprint;
use meta_whatsapp_server::model::{
    IdempotencyClaim, IdempotencyKey, IdempotencyRecord, Scope, TenantId,
};
use serde_json::{Value, json};

use super::{Call, Harness, Reply};

pub const TENANT: &str = "merchant-42";
pub const WABA: &str = "102290129340398";
/// `business-phone-numbers/registration`'s example number.
pub const PN: &str = "106540352242922";
pub const TOKEN: &str = "EAAG-merchant-token-of-the-registration";

/// The sentinel PIN the number is registered with.
pub const PIN: &str = "581063";
/// A wrong PIN, which Meta refuses (`133005`) quoting it.
pub const WRONG_PIN: &str = "315927";
/// The verification code, as the SMS shows it (`WhatsApp code 774-216`).
pub const CODE: &str = "774-216";
/// The code, as it goes to Meta.
pub const CODE_DIGITS: &str = "774216";

/// Every secret of the flow, each as it would be leaked.
pub const SECRETS: [&str; 4] = [PIN, WRONG_PIN, CODE, CODE_DIGITS];

/// The idempotency keys the flow uses.
pub const KEYS: [&str; 4] = ["code:1", "register:1", "register:2", "deregister:1"];

/// A tenant with the number bound and its token stored, and a key of
/// scope `numbers`.
pub async fn connected(h: &Harness) -> String {
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    h.tenant_key(TENANT, &[Scope::Numbers]).await
}

/// `POST /v1/numbers/{PN}/{route}` with `body`.
pub fn post(key: &str, route: &str, body: &Value) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{PN}/{route}"))
        .key(key)
        .json(body)
}

/// `{"success": true}`, Meta's answer to each of the calls.
pub fn success() -> Value {
    json!({"success": true})
}

/// A refusal of Meta's whose every text quotes `secret`.
pub fn quoting(code: i64, secret: &str) -> Value {
    json!({"error": {
        "message": format!("(#{code}) the PIN {secret} is not this number's"),
        "type": "OAuthException", "code": code, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn",
        "error_user_title": format!("PIN {secret}"),
        "error_user_msg": format!("pin={secret}"),
        "error_data": {"messaging_product": "whatsapp", "details": format!("PIN {secret}")}
    }})
}

/// The whole flow, every answer checked: a code requested (under a key)
/// and checked; a wrong PIN Meta refuses quoting it; the right one under
/// a key, then repeated under it; malformed bodies, most of them carrying
/// the PIN (as a number, too long, signed, padded, in other digits, in a
/// body that is not JSON), and a malformed code carrying the code; the
/// PIN set; a registration Meta fails on (its answer kept under its key,
/// and quoting the PIN); the deregistration under a key, then repeated
/// under it (Meta asked once). Returns every answer.
pub async fn flow(h: &Harness, key: &str) -> Vec<Reply> {
    let mut replies = Vec::new();
    let mut call = async |call: Call, status: StatusCode| {
        let reply = h.call(call).await;
        assert_eq!(reply.status, status, "{}", reply.text);
        replies.push(reply);
    };
    h.graph.push_json(200, success());
    call(
        post(
            key,
            "request-code",
            &json!({"code_method": "SMS", "language": "en_US"}),
        )
        .header("idempotency-key", KEYS[0]),
        StatusCode::OK,
    )
    .await;
    h.graph.push_json(200, success());
    call(
        post(key, "verify-code", &json!({"code": CODE})),
        StatusCode::OK,
    )
    .await;
    h.graph.push_json(400, quoting(133005, WRONG_PIN));
    call(
        post(key, "register", &json!({"pin": WRONG_PIN})).header("idempotency-key", KEYS[1]),
        StatusCode::CONFLICT,
    )
    .await;
    // The wrong PIN freed the key: the right one reuses it.
    h.graph.push_json(200, success());
    call(
        post(key, "register", &json!({"pin": PIN})).header("idempotency-key", KEYS[1]),
        StatusCode::OK,
    )
    .await;
    call(
        post(key, "register", &json!({"pin": PIN})).header("idempotency-key", KEYS[1]),
        StatusCode::OK,
    )
    .await;
    for (route, malformed) in [
        ("register", json!({"pin": 581063})),
        ("register", json!({"pin": 581_063.0})),
        ("register", json!({"pin": "58106x"})),
        ("register", json!({"pin": format!("{PIN}1")})),
        ("register", json!({"pin": format!("+{PIN}")})),
        ("register", json!({"pin": format!(" {PIN}")})),
        ("register", json!({"pin": format!("{PIN}\n")})),
        (
            "register",
            json!({"pin": "\u{0665}\u{0668}\u{0661}\u{0660}\u{0666}\u{0663}"}),
        ),
        ("register", json!({"pin": PIN, "unknown": PIN})),
        ("register", json!({"pin": [PIN]})),
        ("register", json!({"pin": {"value": PIN}})),
        ("two-step-pin", json!({"pin": format!("{PIN}0")})),
        ("two-step-pin", json!({"pin": 581063})),
        ("verify-code", json!({"code": "774-21x"})),
        ("verify-code", json!({"code": format!("{CODE}x")})),
        ("verify-code", json!({"code": 774216})),
    ] {
        call(
            post(key, route, &malformed),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    // Not JSON at all, the PIN inside: the parser's message would quote it.
    call(
        post(key, "register", &json!({})).body(format!("{{\"pin\": {PIN}x}}").into()),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    h.graph.push_json(200, success());
    call(
        post(key, "two-step-pin", &json!({"pin": PIN})),
        StatusCode::OK,
    )
    .await;
    // Meta fails, quoting the PIN in a body that is not Graph's: the
    // answer is kept under the key (it may have registered).
    h.graph.push_bytes(
        502,
        "text/html",
        format!("<html>upstream failed for pin {PIN}</html>").into_bytes(),
    );
    call(
        post(key, "register", &json!({"pin": PIN})).header("idempotency-key", KEYS[2]),
        StatusCode::BAD_GATEWAY,
    )
    .await;
    h.graph.push_json(200, success());
    for _ in 0..2 {
        call(
            post(key, "deregister", &json!({"confirm": PN})).header("idempotency-key", KEYS[3]),
            StatusCode::OK,
        )
        .await;
    }
    assert_eq!(h.graph.remaining(), 0);
    replies
}

/// `key`'s record for the flow's tenant, read with a probe that leaves it
/// as it is (a claim of an existing key changes nothing).
pub async fn record(h: &Harness, key: &str) -> IdempotencyRecord {
    let claimed = h
        .store
        .claim_idempotency_key(
            &TenantId::parse(TENANT).unwrap(),
            &IdempotencyKey::parse(key).unwrap(),
            &[0; 32],
            "probe",
            Duration::from_mins(1),
            Duration::from_hours(1),
        )
        .await
        .unwrap();
    match claimed {
        IdempotencyClaim::Existing(record) => record,
        other @ IdempotencyClaim::Claimed => panic!("{key}: no record ({other:?})"),
    }
}

/// The registrations' fingerprint: the number, no region, no PIN.
pub fn registration_fingerprint() -> Fingerprint {
    Fingerprint::register_number(&PhoneNumberId::new(PN).unwrap(), None)
}

/// `haystack` holds none of the secrets (as bytes, anywhere).
pub fn holds_no_secret(haystack: &[u8], what: &str) {
    for secret in SECRETS {
        assert!(
            !haystack
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "{what} holds {secret}"
        );
    }
}

/// Every maximal run of ASCII digits in `text` that is not a fraction of
/// a second (a log's times are `….581063Z`): a PIN logged anywhere else
/// is one of them.
pub fn digit_runs(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut runs = Vec::new();
    let mut start = None;
    for (i, b) in bytes.iter().chain(std::iter::once(&b' ')).enumerate() {
        match (b.is_ascii_digit(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if s == 0 || bytes[s - 1] != b'.' {
                    runs.push(&text[s..i]);
                }
                start = None;
            }
            _ => {}
        }
    }
    runs
}

/// Logs hold no secret: none inside a run of digits (a malformed PIN such
/// as `5810631` holds it too), nor the code with its hyphen.
pub fn logs_hold_no_secret(logs: &str) {
    for run in digit_runs(logs) {
        assert!(
            ![PIN, WRONG_PIN, CODE_DIGITS]
                .iter()
                .any(|secret| run.contains(secret)),
            "a secret was logged ({run}):\n{logs}"
        );
    }
    assert!(!logs.contains(CODE), "the code was logged:\n{logs}");
}

/// What Meta was sent: the right PIN, the wrong one and the code each
/// went to Meta (the control: the flow carried them), with the number's
/// own token.
pub fn meta_got_the_secrets(h: &Harness) {
    let requests = h.graph.requests();
    let bodies: Vec<Value> = requests
        .iter()
        .filter_map(meta_whatsapp_rs::core::testing::RecordedRequest::json)
        .collect();
    assert!(bodies.contains(&json!({"messaging_product": "whatsapp", "pin": PIN})));
    assert!(bodies.contains(&json!({"messaging_product": "whatsapp", "pin": WRONG_PIN})));
    assert!(bodies.contains(&json!({"code": CODE_DIGITS})));
    for request in &requests {
        assert_eq!(request.bearer(), Some(TOKEN), "{}", request.path());
    }
}
