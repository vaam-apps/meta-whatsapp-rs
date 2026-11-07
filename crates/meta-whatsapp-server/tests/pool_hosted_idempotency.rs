//! Roadmap M5j3 (design D292, D350): the operator's pool and hosted
//! routes that have an effect twice take an `Idempotency-Key`, scoped to
//! the admin key as M5j2's are (`admin/<key id>`): the pool's `request-code`
//! (a second code is a second message to the number), its `add` (a second
//! addition is a second pool entry) and the hosted claim
//! (`POST /v1/admin/signup/hosted`, whose arrival is claimed once).
//!
//! The decisive tests: the same key and body after a failure Meta may have
//! received is one request to Meta; a different body (or another number)
//! with the key is `422 idempotency_key_reused`. For the hosted claim the
//! key composes with the one-claim CAS: a retry after a failure does not
//! run the onboarding again and does not claim again, a failure that
//! proves nothing was sent puts the arrival back and releases the key.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::signup::{
    BUSINESS, HOSTED_TOKEN, PHONE, PHONE_2, PIN, POOL_BUSINESS, POOL_TOKEN, PRE_VERIFIED, TOKEN,
    WABA, harness, partner_added, script_waba, success, tech_provider,
};
use common::{Call, Harness};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::{BusinessId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const A: &str = "merchant-a";
const POOL: &str = "/v1/admin/pre-verified-numbers";
const HOSTED: &str = "/v1/admin/signup/hosted";

fn both() -> Harness {
    harness(
        tech_provider()
            .hosted_signup(AccessToken::new(HOSTED_TOKEN))
            .pre_verified_pool(
                BusinessId::new(POOL_BUSINESS).unwrap(),
                AccessToken::new(POOL_TOKEN),
            ),
    )
}

fn keyed(path: &str, admin: &str, key: &str, body: &Value) -> Call {
    Call::new(Method::POST, path)
        .key(admin)
        .header("idempotency-key", key)
        .json(body)
}

fn code_body(method: &str) -> Value {
    json!({"method": method, "language": "en_US"})
}

fn hosted_body(tenant: &str, pin: Option<&str>) -> Value {
    match pin {
        Some(pin) => json!({
            "tenant_id": tenant, "waba_id": WABA, "owner_business_id": BUSINESS, "pin": pin
        }),
        None => json!({"tenant_id": tenant, "waba_id": WABA, "owner_business_id": BUSINESS}),
    }
}

fn script_hosted(h: &Harness) {
    h.graph.push_json(200, json!({"access_token": TOKEN}));
    h.graph.push_json(200, common::signup::one_grant_debug());
    script_waba(&h.graph, WABA, &[PHONE, PHONE_2]);
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(200, success()); // register
}

async fn arrived(h: &Harness) -> bool {
    use meta_whatsapp_rs::core::store::{KvStore, StoreKey};
    h.kv.get(&StoreKey::new("\x77a.es.hosted", WABA))
        .await
        .unwrap()
        .is_some()
}

async fn holder(h: &Harness) -> Option<String> {
    h.store
        .waba(&WabaId::new(WABA).unwrap())
        .await
        .unwrap()
        .map(|binding| binding.tenant_id.as_str().to_owned())
}

/// The roadmap's decisive test. A code request whose answer was a `504` is
/// kept: the same key and body is one request to Meta, however often it is
/// repeated; a different body, or another number, with the key is `422
/// idempotency_key_reused` and Meta is not asked; the same request under
/// another key is another message. Decisive: the key (ignored, the repeat
/// reaches Meta), the body in the fingerprint (dropped, the `VOICE` request
/// replays the `504`), the number in it (dropped, another number replays it).
#[tokio::test]
async fn a_code_request_retried_after_a_504_is_one_request_to_meta() {
    let h = both();
    let admin = h.admin_key().await;
    let route = format!("{POOL}/{PRE_VERIFIED}/code");
    h.graph.push_error(|| TransportError::Timeout);
    let first = h
        .call(keyed(&route, &admin, "code-1", &code_body("SMS")))
        .await;
    assert_eq!(first.status, StatusCode::GATEWAY_TIMEOUT, "{}", first.text);
    assert_eq!(first.json()["error"]["may_have_been_sent"], true);
    assert_eq!(h.graph.requests().len(), 1);

    for _ in 0..2 {
        let again = h
            .call(keyed(&route, &admin, "code-1", &code_body("SMS")))
            .await;
        assert_eq!(again.status, StatusCode::GATEWAY_TIMEOUT, "{}", again.text);
        assert_eq!(again.headers["idempotent-replayed"], "true");
        assert_eq!(again.text, first.text, "byte for byte");
    }
    assert_eq!(h.graph.requests().len(), 1, "one request, not three");

    for (path, body) in [
        (route.clone(), code_body("VOICE")),
        (format!("{POOL}/1972385232742998/code"), code_body("SMS")),
    ] {
        let reused = h.call(keyed(&path, &admin, "code-1", &body)).await;
        assert_eq!(
            (reused.status, reused.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused"),
            "{path}: {}",
            reused.text
        );
    }
    assert_eq!(h.graph.requests().len(), 1, "Meta was not asked");

    h.graph.push_json(200, success());
    let fresh = h
        .call(keyed(&route, &admin, "code-2", &code_body("SMS")))
        .await;
    assert_eq!(fresh.status, StatusCode::NO_CONTENT, "{}", fresh.text);
    assert!(fresh.text.is_empty());
    let replayed = h
        .call(keyed(&route, &admin, "code-2", &code_body("SMS")))
        .await;
    assert_eq!(replayed.status, StatusCode::NO_CONTENT);
    assert_eq!(replayed.headers["idempotent-replayed"], "true");
    assert_eq!(h.graph.requests().len(), 2);
    assert_eq!(h.graph.remaining(), 0);

    // Without a key nothing is kept: each request sends a code, as before.
    for _ in 0..2 {
        h.graph.push_json(200, success());
        let plain = h
            .call(
                Call::new(Method::POST, &route)
                    .key(&admin)
                    .json(&code_body("SMS")),
            )
            .await;
        assert_eq!(plain.status, StatusCode::NO_CONTENT);
        assert!(!plain.headers.contains_key("idempotent-replayed"));
    }
    assert_eq!(h.graph.requests().len(), 4);
}

/// The code route and the addition do not share a fingerprint (one key,
/// two operations is `422`), a refusal Meta made (nothing sent) releases
/// the key, and a tenant's key is refused before the key is read.
/// Decisive: the operation id in the fingerprint (shared, the second
/// route replays the first's answer).
#[tokio::test]
async fn the_pool_routes_do_not_share_a_fingerprint_and_a_refusal_releases_the_key() {
    let h = both();
    let admin = h.admin_key().await;
    h.tenant(A).await;
    let tenant = h.tenant_key(A, &[Scope::Signup, Scope::Numbers]).await;
    let route = format!("{POOL}/{PRE_VERIFIED}/code");

    // One key on two routes: the addition's answer is kept, and the code
    // route, whose body is another, meets the addition's record. (The
    // routes' bodies cannot be equal, as each refuses the other's fields,
    // so the operation's id in the fingerprint is pinned by the core's
    // `the_pool_and_hosted_fingerprints_are_per_route_body_and_number`.)
    let body = json!({"phone_number": "+1 555 078 3881"});
    h.graph.push_json(200, json!({"id": PRE_VERIFIED}));
    let added = h.call(keyed(POOL, &admin, "shared", &body)).await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.text);
    assert_eq!(added.json(), json!({"id": PRE_VERIFIED}));
    let reused = h
        .call(keyed(&route, &admin, "shared", &code_body("SMS")))
        .await;
    assert_eq!(
        (reused.status, reused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused"),
        "{}",
        reused.text
    );
    let again = h.call(keyed(POOL, &admin, "shared", &body)).await;
    assert_eq!(again.text, added.text);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(h.graph.requests().len(), 1, "one addition");

    // Meta refuses (400): nothing was sent, the key is free for the retry.
    h.graph.push_json(
        400,
        json!({"error": {"message": "x", "type": "OAuthException", "code": 100, "fbtrace_id": "T"}}),
    );
    let refused = h
        .call(keyed(&route, &admin, "code-fix", &code_body("SMS")))
        .await;
    assert_eq!(refused.code(), "invalid_parameter", "{}", refused.text);
    h.graph.push_json(200, success());
    let fixed = h
        .call(keyed(&route, &admin, "code-fix", &code_body("SMS")))
        .await;
    assert_eq!(fixed.status, StatusCode::NO_CONTENT, "{}", fixed.text);
    assert!(!fixed.headers.contains_key("idempotent-replayed"));

    // A malformed id or body never touches the key.
    let invalid = h
        .call(keyed(&format!("{POOL}/12%2F34/code"), &admin, "code-bad", &code_body("SMS")))
        .await;
    assert_eq!(invalid.code(), "invalid_request", "{}", invalid.text);
    let bad_body = h
        .call(keyed(&route, &admin, "code-bad", &json!({"method": "FAX"})))
        .await;
    assert_eq!(bad_body.code(), "invalid_request", "{}", bad_body.text);
    // The addition, the refused request and its retry: nothing more.
    assert_eq!(h.graph.requests().len(), 3, "nothing more reached Meta");

    // A tenant's key is refused whatever the key header is.
    let forbidden = h
        .call(keyed(&route, &tenant, "has a space", &code_body("SMS")))
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN, "{}", forbidden.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// An addition retried after a `504` is one request to Meta (a second one
/// is a second pool entry); a different number with the key is `422`.
#[tokio::test]
async fn an_addition_retried_after_a_504_is_one_request_to_meta() {
    let h = both();
    let admin = h.admin_key().await;
    let body = json!({"phone_number": "+1 555 078 3881"});
    h.graph.push_error(|| TransportError::Timeout);
    let first = h.call(keyed(POOL, &admin, "add-1", &body)).await;
    assert_eq!(first.status, StatusCode::GATEWAY_TIMEOUT, "{}", first.text);
    let again = h.call(keyed(POOL, &admin, "add-1", &body)).await;
    assert_eq!(again.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.text, first.text);
    let reused = h
        .call(keyed(
            POOL,
            &admin,
            "add-1",
            &json!({"phone_number": "+1 555 078 3882"}),
        ))
        .await;
    assert_eq!(
        (reused.status, reused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused"),
        "{}",
        reused.text
    );
    assert_eq!(h.graph.requests().len(), 1);
}

/// The decisive test for the hosted claim. A claim that Meta may have
/// received and failed on is kept: the same key and body is not a second
/// onboarding and not a second claim of the arrival; a different body with
/// the key is `422 idempotency_key_reused`; a new key claims again (the
/// arrival was put back, since the token was not stored). The PIN is in no
/// answer. Decisive: the key (ignored, the repeat finds the arrival back
/// and asks Meta again), the body in the fingerprint (dropped, the other
/// tenant replays), the hosted fingerprint apart from the pool's.
#[tokio::test]
async fn a_hosted_claim_retried_after_a_failure_meta_may_have_seen_runs_once() {
    let h = both();
    let admin = h.admin_key().await;
    h.tenant(A).await;
    h.tenant("merchant-b").await;
    h.webhook(&partner_added(WABA, BUSINESS)).await;

    h.graph.push_error(|| TransportError::Timeout);
    let body = hosted_body(A, Some(PIN));
    let first = h.call(keyed(HOSTED, &admin, "claim-1", &body)).await;
    assert_eq!(first.code(), "onboarding_failed", "{}", first.text);
    assert_eq!(first.json()["error"]["may_have_been_sent"], true);
    assert!(arrived(&h).await, "the arrival was put back");
    assert_eq!(h.graph.requests().len(), 1);

    for _ in 0..2 {
        let again = h.call(keyed(HOSTED, &admin, "claim-1", &body)).await;
        assert_eq!(again.status, first.status, "{}", again.text);
        assert_eq!(again.headers["idempotent-replayed"], "true");
        assert_eq!(again.text, first.text, "byte for byte");
    }
    assert_eq!(h.graph.requests().len(), 1, "the onboarding ran once");
    assert!(arrived(&h).await);

    let other = h
        .call(keyed(
            HOSTED,
            &admin,
            "claim-1",
            &hosted_body("merchant-b", Some(PIN)),
        ))
        .await;
    assert_eq!(
        (other.status, other.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused"),
        "{}",
        other.text
    );
    assert_eq!(h.graph.requests().len(), 1);
    assert_eq!(holder(&h).await, None);

    // A new key claims the put-back arrival.
    script_hosted(&h);
    let done = h.call(keyed(HOSTED, &admin, "claim-2", &body)).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    assert_eq!(holder(&h).await.as_deref(), Some(A));
    assert!(!arrived(&h).await);
    let asked = h.graph.requests().len();

    // The kept success is the answer, never the PIN or a token, and a
    // repeat (even with another PIN, which is not compared) is not the
    // `404` a second claim meets, and asks Meta nothing.
    for pin in [PIN, "123456"] {
        let replayed = h
            .call(keyed(HOSTED, &admin, "claim-2", &hosted_body(A, Some(pin))))
            .await;
        assert_eq!(replayed.status, StatusCode::OK, "{}", replayed.text);
        assert_eq!(replayed.headers["idempotent-replayed"], "true");
        assert_eq!(replayed.text, done.text);
        for secret in [PIN, "123456", TOKEN, HOSTED_TOKEN] {
            assert!(!replayed.text.contains(secret), "{secret} kept");
        }
    }
    // A claim with no PIN is another request.
    let no_pin = h
        .call(keyed(HOSTED, &admin, "claim-2", &hosted_body(A, None)))
        .await;
    assert_eq!(no_pin.code(), "idempotency_key_reused", "{}", no_pin.text);
    assert_eq!(h.graph.requests().len(), asked);
    assert_eq!(h.graph.remaining(), 0);
}

/// A claim that stopped where Meta refused (400: nothing sent) puts the
/// arrival back **and** releases the key: the retry under the same key
/// claims and onboards. A claim that found no arrival is the same `404`
/// with its key released. Decisive: the release (kept instead, the retry
/// replays the `502`).
#[tokio::test]
async fn a_claim_that_stopped_before_the_store_puts_the_arrival_back_and_releases_the_key() {
    let h = both();
    let admin = h.admin_key().await;
    h.tenant(A).await;

    // No arrival yet: `404`, the key stays free.
    let none = h
        .call(keyed(HOSTED, &admin, "claim-1", &hosted_body(A, Some(PIN))))
        .await;
    assert_eq!(none.code(), "not_found", "{}", none.text);
    assert!(!none.headers.contains_key("idempotent-replayed"));
    h.webhook(&partner_added(WABA, BUSINESS)).await;

    h.graph.push_json(
        400,
        json!({"error": {"message": "x", "type": "OAuthException", "code": 100, "fbtrace_id": "T"}}),
    );
    let failed = h
        .call(keyed(HOSTED, &admin, "claim-1", &hosted_body(A, Some(PIN))))
        .await;
    assert_eq!(failed.code(), "onboarding_failed", "{}", failed.text);
    assert_eq!(failed.json()["error"]["resumable"], false);
    assert_eq!(failed.json()["error"]["may_have_been_sent"], false);
    assert!(arrived(&h).await, "put back");

    script_hosted(&h);
    let done = h
        .call(keyed(HOSTED, &admin, "claim-1", &hosted_body(A, Some(PIN))))
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    assert!(!done.headers.contains_key("idempotent-replayed"));
    assert_eq!(holder(&h).await.as_deref(), Some(A));
    assert_eq!(h.graph.remaining(), 0);
}

/// The hosted claim's order of answers is unchanged by a key: a tenant's
/// key is `403` and none `401` before the key is read; a malformed key is
/// `422` on `Idempotency-Key`; a body that does not parse is `422` and
/// keeps no key; and a claim without a key behaves as before.
#[tokio::test]
async fn the_hosted_claim_answers_the_gate_before_the_key() {
    let h = both();
    let admin = h.admin_key().await;
    h.tenant(A).await;
    let tenant = h.tenant_key(A, &[Scope::Signup]).await;
    h.webhook(&partner_added(WABA, BUSINESS)).await;

    let forbidden = h
        .call(keyed(HOSTED, &tenant, "has a space", &hosted_body(A, None)))
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN, "{}", forbidden.text);
    let anonymous = h
        .call(
            Call::new(Method::POST, HOSTED)
                .header("idempotency-key", "k")
                .json(&hosted_body(A, None)),
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let malformed = h
        .call(keyed(HOSTED, &admin, "has a space", &hosted_body(A, None)))
        .await;
    assert_eq!(
        malformed.json()["error"]["field"].as_str(),
        Some("Idempotency-Key"),
        "{}",
        malformed.text
    );
    let unknown_field = h
        .call(keyed(HOSTED, &admin, "k", &json!({"tenant_id": A, "extra": 1})))
        .await;
    assert_eq!(unknown_field.code(), "invalid_request", "{}", unknown_field.text);
    assert!(h.graph.requests().is_empty());
    assert!(arrived(&h).await);

    // No key: a claim as it was.
    script_hosted(&h);
    let plain = h
        .call(
            Call::new(Method::POST, HOSTED)
                .key(&admin)
                .json(&hosted_body(A, Some(PIN))),
        )
        .await;
    assert_eq!(plain.status, StatusCode::OK, "{}", plain.text);
    let second = h
        .call(
            Call::new(Method::POST, HOSTED)
                .key(&admin)
                .json(&hosted_body(A, Some(PIN))),
        )
        .await;
    assert_eq!(second.code(), "not_found", "{}", second.text);
}
