//! A number's registration and its two-step PIN (roadmap M3d, decision
//! D6): the exact Graph request each route makes, with the number's own
//! token; the PIN and the code in no store and no answer; the tenant's
//! isolation; the per-number attempt budgets; the idempotency keys, never
//! over the PIN; the deregistration's confirmation. The logs are
//! `registration_logs.rs`'s, every table on Postgres `live_postgres.rs`'s.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::time::Duration;

use common::registration::{
    self as flow, CODE_DIGITS, KEYS, PIN, PN, TENANT, TOKEN, WABA, WRONG_PIN, connected, post,
    success,
};
use common::{Call, Harness, test_settings};
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::core::store::KvStore;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{IdempotencyState, Scope};
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::registration::{
    AttemptBudget, AttemptLimit, AttemptLimits, RegistrationAttempts,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

/// Each route's request is Meta's documented one, byte for byte, to the
/// number's own path, with the number's token; each answer names the
/// number and what was done. Examples: `business-phone-numbers/registration`
/// (register, with and without local storage; deregister),
/// `business-phone-numbers/two-step-verification`, and the request-code
/// and verify-code references (`{"success": true}` each).
#[tokio::test]
async fn each_route_sends_metas_documented_request_with_the_numbers_token() {
    let h = Harness::new();
    let key = connected(&h).await;
    let cases = [
        (
            "request-code",
            json!({"code_method": "VOICE", "language": "en_US"}),
            "request_code",
            Some(json!({"code_method": "VOICE", "language": "en_US"})),
            "code_requested",
        ),
        (
            "verify-code",
            json!({"code": "123-830"}),
            "verify_code",
            Some(json!({"code": "123830"})),
            "code_verified",
        ),
        (
            "register",
            json!({"pin": "212834"}),
            "register",
            Some(json!({"messaging_product": "whatsapp", "pin": "212834"})),
            "registered",
        ),
        (
            "register",
            json!({"pin": "212834", "data_localization_region": "CH"}),
            "register",
            Some(
                json!({"messaging_product": "whatsapp", "pin": "212834", "data_localization_region": "CH"}),
            ),
            "registered",
        ),
        (
            "two-step-pin",
            json!({"pin": "212834"}),
            "",
            Some(json!({"pin": "212834"})),
            "pin_set",
        ),
        (
            "deregister",
            json!({"confirm": PN}),
            "deregister",
            None,
            "deregistered",
        ),
    ];
    for (route, body, edge, sent, status) in cases {
        h.graph.push_json(200, success());
        let reply = h.call(post(&key, route, &body)).await;
        assert_eq!(reply.status, StatusCode::OK, "{route}: {}", reply.text);
        assert_eq!(
            reply.json(),
            json!({"phone_number_id": PN, "status": status}),
            "{route}"
        );
        let request = h.graph.last_request().unwrap();
        assert_eq!(request.method, Method::POST, "{route}");
        let path = if edge.is_empty() {
            format!("/v25.0/{PN}")
        } else {
            format!("/v25.0/{PN}/{edge}")
        };
        assert_eq!(request.path(), path, "{route}");
        assert_eq!(request.bearer(), Some(TOKEN), "{route}");
        assert_eq!(request.json(), sent, "{route}");
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// Meta's documented refusals (the verify-code reference's `400`, "The
/// verification code is invalid or has expired", code `100`; the
/// request-code reference's `429`, code `4`): Meta's code and trace id,
/// never its message.
#[tokio::test]
async fn metas_documented_refusals_keep_their_code_and_drop_their_text() {
    let h = Harness::new();
    let key = connected(&h).await;
    h.graph.push_json(
        400,
        json!({"error": {"message": "The verification code is invalid or has expired",
            "type": "OAuthException", "code": 100, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let wrong = h
        .call(post(&key, "verify-code", &json!({"code": CODE_DIGITS})))
        .await;
    assert_eq!(
        (wrong.status, wrong.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_parameter")
    );
    assert_eq!(wrong.json()["error"]["graph"]["code"], 100);
    assert!(!wrong.text.contains("invalid or has expired"));
    h.graph.push_json(
        429,
        json!({"error": {"message": "Application request limit reached", "type": "OAuthException",
            "code": 4, "error_subcode": 2446079, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn",
            "is_transient": true}}),
    );
    let throttled = h
        .call(post(
            &key,
            "request-code",
            &json!({"code_method": "SMS", "language": "en_US"}),
        ))
        .await;
    assert_eq!(
        (throttled.status, throttled.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
    );
    assert_eq!(throttled.json()["error"]["graph"]["subcode"], 2446079);
    assert!(!throttled.text.contains("request limit reached"));
    assert_eq!(h.graph.remaining(), 0);
}

/// The decisive test of roadmap M3d (decision D6): after a registration's
/// whole flow (a code, a wrong PIN Meta refuses quoting it, the right one
/// under a key and its repeat, malformed bodies, the PIN set, a failure
/// kept under a key, the deregistration), the sentinel PIN, the wrong one
/// and the code are in no answer, no value written to the `KvStore` (the
/// vault, the attempt counters), no idempotency record (its fingerprint
/// is the number's alone, and its kept answers hold none of them), and no
/// outbox row; and each went to Meta (the control). Storing the PIN
/// anywhere the service writes fails it (on Postgres, every table:
/// `live_postgres_a_registration_leaves_its_pin_in_no_table`).
#[tokio::test]
async fn a_registrations_pin_and_code_are_in_no_store_and_no_answer() {
    let h = Harness::new();
    let key = connected(&h).await;
    let replies = flow::flow(&h, &key).await;
    for reply in &replies {
        flow::holds_no_secret(reply.text.as_bytes(), "an answer");
        for (name, value) in &reply.headers {
            flow::holds_no_secret(value.as_bytes(), name.as_str());
        }
    }
    flow::meta_got_the_secrets(&h);
    let writes = h.kv.writes();
    assert!(
        writes
            .iter()
            .any(|(key, _)| key.namespace() == "wa.server.registration"),
        "the counters were written"
    );
    for (key, value) in &writes {
        flow::holds_no_secret(value, &format!("the store's {key}"));
        flow::holds_no_secret(key.key().as_bytes(), "a store key");
    }
    for key in KEYS {
        let record = flow::record(&h, key).await;
        match &record.state {
            IdempotencyState::Completed { body, .. } => {
                flow::holds_no_secret(body, &format!("{key}'s kept answer"));
            }
            other => panic!("{key}: {other:?}"),
        }
    }
    // The registrations' records hold the number's fingerprint, which no
    // PIN is an input of.
    for key in &KEYS[1..] {
        assert_eq!(
            &flow::record(&h, key).await.fingerprint,
            flow::registration_fingerprint().as_bytes(),
            "{key}"
        );
    }
    for row in h.outbox.rows() {
        flow::holds_no_secret(format!("{row:?}").as_bytes(), "an outbox row");
    }
}

/// A repeat under the key of a kept registration, with another PIN, is
/// the same request: the kept answer, replayed, and nothing asked of Meta
/// (the fingerprint holds no PIN). Decisive: `register_number`'s
/// fingerprint taking the PIN (the repeat would be `422
/// idempotency_key_reused`).
#[tokio::test]
async fn a_repeat_under_a_kept_registrations_key_replays_whatever_its_pin() {
    let h = Harness::new();
    let key = connected(&h).await;
    h.graph.push_json(200, success());
    let first = h
        .call(post(&key, "register", &json!({"pin": PIN})).header("idempotency-key", "r"))
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    let again = h
        .call(post(&key, "register", &json!({"pin": WRONG_PIN})).header("idempotency-key", "r"))
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(
        again.headers.get("idempotent-replayed").unwrap(),
        "true",
        "replayed"
    );
    assert_eq!(again.text, first.text);
    assert_eq!(h.graph.requests().len(), 1, "Meta asked once");
    // Another region is another request.
    let other = h
        .call(
            post(
                &key,
                "register",
                &json!({"pin": PIN, "data_localization_region": "CH"}),
            )
            .header("idempotency-key", "r"),
        )
        .await;
    assert_eq!(other.code(), "idempotency_key_reused", "{}", other.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// A wrong PIN Meta refused releases the key (nothing was registered):
/// the corrected PIN reuses it and is sent. A failure that may have
/// registered keeps it.
#[tokio::test]
async fn a_wrong_pin_frees_the_key_and_a_failure_keeps_it() {
    let h = Harness::new();
    let key = connected(&h).await;
    h.graph.push_json(400, flow::quoting(133005, WRONG_PIN));
    let wrong = h
        .call(post(&key, "register", &json!({"pin": WRONG_PIN})).header("idempotency-key", "k"))
        .await;
    assert_eq!(
        (wrong.status, wrong.code().as_str()),
        (StatusCode::CONFLICT, "two_step_verification")
    );
    let error = &wrong.json()["error"];
    assert_eq!(error["may_have_been_sent"], false);
    assert_eq!(error["graph"]["code"], 133005);
    assert_eq!(error["graph"]["fbtrace_id"], "AXsgnV2Cm3ZMGF3dF_cfYIn");
    assert_eq!(
        error["graph"]["details"],
        Value::Null,
        "Meta's text dropped"
    );
    h.graph.push_json(200, success());
    let right = h
        .call(post(&key, "register", &json!({"pin": PIN})).header("idempotency-key", "k"))
        .await;
    assert_eq!(right.status, StatusCode::OK, "{}", right.text);
    assert_eq!(
        h.graph.last_request().unwrap().json(),
        Some(json!({"messaging_product": "whatsapp", "pin": PIN}))
    );
    // A timeout may have registered: kept, never sent again.
    h.graph.push_json(
        500,
        json!({"error": {"message": "x", "type": "OAuthException", "code": 2}}),
    );
    let failed = h
        .call(post(&key, "register", &json!({"pin": PIN})).header("idempotency-key", "k2"))
        .await;
    assert_eq!(failed.status, StatusCode::BAD_GATEWAY, "{}", failed.text);
    let repeat = h
        .call(post(&key, "register", &json!({"pin": PIN})).header("idempotency-key", "k2"))
        .await;
    assert_eq!(repeat.headers.get("idempotent-replayed").unwrap(), "true");
    assert_eq!(h.graph.requests().len(), 3);
    assert_eq!(h.graph.remaining(), 0);
}

/// Malformed bodies never echo the PIN or the code back, and spend
/// nothing: no request to Meta, no attempt counted (the counter is never
/// written). Decisive: `ApiJson`'s fixed refusal, and the local checks
/// before the budget.
#[tokio::test]
async fn a_malformed_body_never_echoes_the_pin_or_code_and_spends_nothing() {
    let h = Harness::new();
    let key = connected(&h).await;
    for (route, body, field) in [
        ("register", json!({"pin": 581063}), "body"),
        ("register", json!({"pin": "58106"}), "pin"),
        ("register", json!({"pin": "5810634"}), "pin"),
        ("register", json!({"pin": "58106x"}), "pin"),
        ("register", json!({"pin": PIN, "pin_again": PIN}), "body"),
        (
            "register",
            json!({"pin": PIN, "data_localization_region": "ch"}),
            "data_localization_region",
        ),
        ("register", json!({}), "body"),
        ("two-step-pin", json!({"pin": 581063}), "body"),
        ("two-step-pin", json!({"pin": "5810 63"}), "pin"),
        ("verify-code", json!({"code": 774216}), "body"),
        ("verify-code", json!({"code": "77421x"}), "code"),
        ("verify-code", json!({"code": "-"}), "code"),
        (
            "request-code",
            json!({"code_method": "SMS", "language": " "}),
            "language",
        ),
        (
            "request-code",
            json!({"code_method": "EMAIL", "language": "en_US"}),
            "body",
        ),
        (
            "deregister",
            json!({"confirm": "106540352242923"}),
            "confirm",
        ),
        ("deregister", json!({}), "body"),
    ] {
        let reply = h.call(post(&key, route, &body)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{route} {body}"
        );
        assert_eq!(reply.json()["error"]["field"], field, "{route} {body}");
        for secret in ["581063", "58106", "77421", "774216", "5810 63"] {
            assert!(!reply.text.contains(secret), "{route}: {}", reply.text);
        }
    }
    assert!(h.graph.requests().is_empty(), "Meta was asked");
    assert!(
        h.kv.writes()
            .iter()
            .all(|(key, _)| key.namespace() != "wa.server.registration"),
        "an attempt was counted"
    );
}

/// Another tenant's number is `404` before any detail, on every route:
/// the body is not read (a malformed one is `404` too), the vault is not
/// read, nothing is asked of Meta, and the number's budget is untouched
/// (another tenant cannot lock a number out). Decisive: the ownership
/// step, and the charge after it.
#[tokio::test]
async fn another_tenants_number_is_not_found_and_spends_none_of_its_budget() {
    let h = Harness::new();
    connected(&h).await;
    h.tenant("merchant-43").await;
    h.connect("merchant-43", "999", &["998"], "TOKEN-OF-43")
        .await;
    let other = h.tenant_key("merchant-43", &Scope::ALL).await;
    let reads = h.kv.vault_reads();
    for route in [
        "request-code",
        "verify-code",
        "register",
        "deregister",
        "two-step-pin",
    ] {
        for body in [
            json!({"pin": PIN, "code": CODE_DIGITS}),
            json!({"pin": 581063}),
        ] {
            let reply = h.call(post(&other, route, &body)).await;
            assert_eq!(
                (reply.status, reply.code().as_str()),
                (StatusCode::NOT_FOUND, "not_found"),
                "{route}"
            );
            assert!(!reply.text.contains(PIN));
        }
    }
    assert_eq!(h.kv.vault_reads(), reads, "the vault was read");
    assert!(h.graph.requests().is_empty(), "Meta was asked");
    assert!(
        h.kv.writes()
            .iter()
            .all(|(key, _)| key.namespace() != "wa.server.registration"),
        "the number's budget was spent"
    );
}

/// A number's registration budget: past its limit, `429
/// too_many_requests` with `Retry-After` the rest of the window, nothing
/// asked of Meta; deregistering spends the same budget; the verification
/// budget, and another number of the tenant, are untouched; the PIN
/// change spends none. Shared by every replica (the counter is on the
/// bundle's `KvStore`: a second service on the same store sees it).
/// Decisive: the charge before each registration call.
#[tokio::test]
async fn a_numbers_registration_attempts_are_bounded_across_replicas() {
    let settings = Settings {
        registration_attempts: AttemptLimits {
            registration: AttemptLimit {
                attempts: 2,
                window: Duration::from_hours(72),
            },
            verification: AttemptLimit {
                attempts: 1,
                window: Duration::from_hours(1),
            },
        },
        ..test_settings()
    };
    let h = Harness::with_settings(settings.clone());
    let key = connected(&h).await;
    h.connect(TENANT, WABA, &[PN, "106540352242923"], TOKEN)
        .await;
    h.graph.push_json(400, flow::quoting(133005, WRONG_PIN));
    let wrong = h
        .call(post(&key, "register", &json!({"pin": WRONG_PIN})))
        .await;
    assert_eq!(wrong.code(), "two_step_verification");
    h.graph.push_json(200, success());
    let dereg = h
        .call(post(&key, "deregister", &json!({"confirm": PN})))
        .await;
    assert_eq!(dereg.status, StatusCode::OK, "{}", dereg.text);
    let asked = h.graph.requests().len();
    for (route, body) in [
        ("register", json!({"pin": PIN})),
        ("deregister", json!({"confirm": PN})),
    ] {
        let refused = h.call(post(&key, route, &body)).await;
        assert_eq!(
            (refused.status, refused.code().as_str()),
            (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
            "{route}"
        );
        let wait: u64 = refused.headers["retry-after"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((72 * 3600 - 60..=72 * 3600).contains(&wait), "{wait}");
        assert_eq!(refused.json()["error"]["may_have_been_sent"], false);
    }
    assert_eq!(h.graph.requests().len(), asked, "Meta was asked");
    // The PIN change spends no attempt; another budget and another
    // number are whole.
    for _ in 0..3 {
        h.graph.push_json(200, success());
        let set = h
            .call(post(&key, "two-step-pin", &json!({"pin": PIN})))
            .await;
        assert_eq!(set.status, StatusCode::OK);
    }
    h.graph.push_json(200, success());
    let code = h
        .call(post(
            &key,
            "request-code",
            &json!({"code_method": "SMS", "language": "en_US"}),
        ))
        .await;
    assert_eq!(code.status, StatusCode::OK);
    let refused = h
        .call(post(&key, "verify-code", &json!({"code": CODE_DIGITS})))
        .await;
    assert_eq!(
        refused.code(),
        "too_many_requests",
        "the verification budget"
    );
    h.graph.push_json(200, success());
    let another = h
        .call(
            Call::new(Method::POST, "/v1/numbers/106540352242923/register")
                .key(&key)
                .json(&json!({"pin": PIN})),
        )
        .await;
    assert_eq!(another.status, StatusCode::OK, "{}", another.text);
    // A second replica on the same `KvStore` books from the same budget.
    let counter =
        h.kv.get(&RegistrationAttempts::store_key(
            AttemptBudget::Registration,
            &PhoneNumberId::new(PN).unwrap(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert!(
        String::from_utf8(counter.value)
            .unwrap()
            .ends_with(r#""count":2}"#)
    );
    let replica = Harness::on_with(h.store.clone(), h.kv.clone(), settings);
    let refused = replica
        .call(post(&key, "register", &json!({"pin": PIN})))
        .await;
    assert_eq!(refused.code(), "too_many_requests", "the other replica");
    assert!(replica.graph.requests().is_empty());
    assert_eq!(h.graph.remaining(), 0);
}

/// Deregistering takes the number's id again: missing or another, `422`
/// on `confirm` with nothing asked and nothing counted (the key is not
/// even claimed: the same key then deregisters).
#[tokio::test]
async fn deregistering_takes_the_numbers_id_again() {
    let h = Harness::new();
    let key = connected(&h).await;
    let refused = h
        .call(
            post(&key, "deregister", &json!({"confirm": "106540352242923"}))
                .header("idempotency-key", "d"),
        )
        .await;
    assert_eq!(refused.json()["error"]["field"], "confirm");
    assert!(h.graph.requests().is_empty());
    h.graph.push_json(200, success());
    let done = h
        .call(post(&key, "deregister", &json!({"confirm": PN})).header("idempotency-key", "d"))
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    assert!(done.headers.get("idempotent-replayed").is_none());
    assert_eq!(
        h.graph.last_request().unwrap().path(),
        format!("/v25.0/{PN}/deregister")
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// The routes take the scope `numbers`: a key without it is `403
/// forbidden`, before ownership and the body.
#[tokio::test]
async fn the_routes_need_the_numbers_scope() {
    let h = Harness::new();
    connected(&h).await;
    let send_only = h.tenant_key(TENANT, &[Scope::Send, Scope::Otp]).await;
    for route in [
        "request-code",
        "verify-code",
        "register",
        "deregister",
        "two-step-pin",
    ] {
        let reply = h.call(post(&send_only, route, &json!({"pin": PIN}))).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{route}"
        );
    }
    assert!(h.graph.requests().is_empty());
}
