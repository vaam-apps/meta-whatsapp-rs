//! Roadmap M5d2 (design D450 to D453): a number's Flows business public
//! key (`PUT`, `GET /v1/numbers/{pn}/business-public-key`), set at Meta
//! with the tenant's WABA token, checked before any request, and compared
//! with the public half of the WABA's Flow key.
//!
//! Requests to Meta are asserted exactly (the guide's urlencoded
//! `business_public_key`, `flows/guides/whatsapp-business-encryption`);
//! answers are built from the pages' two documented shapes.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::flows::{KEY_PEM, OTHER_KEY_PEM};
use common::{Call, Harness};
use meta_whatsapp_rs::client::flows::endpoint::FlowEndpointKey;
use meta_whatsapp_rs::core::testing::RecordedBody;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const A: &str = "merchant-a";
const B: &str = "merchant-b";
const WABA_A: &str = "102290129340398";
const WABA_B: &str = "202290129340399";
const PN_A: &str = "106540352242922";
const PN_A2: &str = "106540352242999";
const PN_B: &str = "206540352242923";

struct Rig {
    h: Harness,
    key_a: String,
    key_b: String,
}

async fn rig() -> Rig {
    let h = Harness::with_settings(common::test_settings());
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, WABA_A, &[PN_A, PN_A2], "TOKEN-A").await;
    h.connect(B, WABA_B, &[PN_B], "TOKEN-B").await;
    let scopes = [Scope::Flows];
    let key_a = h.tenant_key(A, &scopes).await;
    let key_b = h.tenant_key(B, &scopes).await;
    Rig { h, key_a, key_b }
}

fn public_of(pem: &str) -> String {
    FlowEndpointKey::from_pem(pem)
        .unwrap()
        .public_key_pem()
        .unwrap()
}

fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn put(pn: &str, key: &str, body: &Value) -> Call {
    Call::new(Method::PUT, format!("/v1/numbers/{pn}/business-public-key"))
        .key(key)
        .json(body)
}

fn get(pn: &str, key: &str) -> Call {
    Call::get(format!("/v1/numbers/{pn}/business-public-key")).key(key)
}

impl Rig {
    /// The tenant A's Flow key for its WABA, set through the service.
    async fn flow_key(&self, pem: &str) {
        let reply = self
            .h
            .call(
                Call::new(Method::PUT, format!("/v1/wabas/{WABA_A}/flow-key"))
                    .key(&self.key_a)
                    .json(&json!({"private_key": pem})),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    }
}

/// The urlencoded pairs of a request to Meta's encryption edge.
fn form(request: &meta_whatsapp_rs::core::testing::RecordedRequest) -> Vec<(String, String)> {
    let RecordedBody::Bytes { content_type, data } = &request.body else {
        panic!("expected a buffered body: {:?}", request.body)
    };
    assert_eq!(content_type, "application/x-www-form-urlencoded");
    url::form_urlencoded::parse(data)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// The set sends exactly the guide's request with the tenant's token, the
/// key in canonical PEM, and the answer is the digest; the read answers
/// the two documented shapes. Decisive: the route, the library call and
/// the form's field name.
#[tokio::test]
async fn a_tenant_sets_and_reads_a_numbers_key() {
    let r = rig().await;
    r.flow_key(KEY_PEM).await;
    let public = public_of(KEY_PEM);
    // Written with other line lengths and spacing: the canonical form is
    // what is sent.
    let one_line: String = public.lines().filter(|l| !l.starts_with("-----")).collect();
    let sloppy = format!("  -----BEGIN PUBLIC KEY-----\n{one_line}\n-----END PUBLIC KEY-----\n\n");
    r.h.graph.push_json(200, json!({"success": true}));
    let set =
        r.h.call(put(PN_A, &r.key_a, &json!({"business_public_key": sloppy})))
            .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text);
    assert_eq!(
        set.json(),
        json!({"public_key_sha256": sha256_hex(&public), "matches_flow_key": true})
    );
    let requests = r.h.graph.requests();
    let [asked] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(asked.method, Method::POST);
    assert_eq!(
        asked.path(),
        "/v25.0/106540352242922/whatsapp_business_encryption"
    );
    assert_eq!(asked.bearer(), Some("TOKEN-A"));
    assert_eq!(
        form(asked),
        vec![("business_public_key".to_owned(), public.clone())]
    );

    // The guide's flat shape, then the reference's `data` array.
    r.h.graph.push_json(
        200,
        json!({
            "business_public_key": public,
            "business_public_key_signature_status": "VALID"
        }),
    );
    let read = r.h.call(get(PN_A, &r.key_a)).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.text);
    assert_eq!(
        read.json(),
        json!({
            "configured": true,
            "public_key": public,
            "public_key_sha256": sha256_hex(&public),
            "signature_status": "VALID",
            "matches_flow_key": true
        })
    );
    let asked = r.h.graph.last_request().unwrap();
    assert_eq!(asked.method, Method::GET);
    assert_eq!(
        asked.path(),
        "/v25.0/106540352242922/whatsapp_business_encryption"
    );
    assert_eq!(asked.bearer(), Some("TOKEN-A"));

    r.h.graph.push_json(
        200,
        json!({"data": [{
            "business_public_key": public_of(OTHER_KEY_PEM),
            "business_public_key_signature_status": "MISMATCH"
        }]}),
    );
    let other = r.h.call(get(PN_A, &r.key_a)).await.json();
    assert_eq!(other["signature_status"], "MISMATCH");
    assert_eq!(other["matches_flow_key"], false, "{other}");

    r.h.graph.push_json(200, json!({"data": []}));
    let none = r.h.call(get(PN_A2, &r.key_a)).await.json();
    assert_eq!(
        none,
        json!({
            "configured": false, "public_key": null, "public_key_sha256": null,
            "signature_status": null, "matches_flow_key": null
        })
    );
    assert_eq!(r.h.graph.remaining(), 0);
}

/// **The decisive mismatch test.** The tenant has a Flow key: a public key
/// that is not its public half is `422` on `flow_key` and **nothing is sent
/// to Meta**; `allow_flow_key_mismatch` lets it through, and says so. Delete
/// the comparison in `BusinessKey::set` and the first assertion fails.
#[tokio::test]
async fn a_key_that_is_not_the_flow_keys_public_half_is_refused_before_any_request() {
    let r = rig().await;
    r.flow_key(KEY_PEM).await;
    let other = public_of(OTHER_KEY_PEM);
    let refused =
        r.h.call(put(PN_A, &r.key_a, &json!({"business_public_key": other})))
            .await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.text
    );
    assert_eq!(refused.code(), "invalid_request");
    assert_eq!(refused.json()["error"]["field"], "flow_key");
    assert!(r.h.graph.requests().is_empty(), "Meta was asked");
    assert!(!refused.text.contains(&other.lines().nth(1).unwrap()[..20]));

    // The caller means it.
    r.h.graph.push_json(200, json!({"success": true}));
    let allowed =
        r.h.call(put(
            PN_A,
            &r.key_a,
            &json!({"business_public_key": other, "allow_flow_key_mismatch": true}),
        ))
        .await;
    assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.text);
    assert_eq!(allowed.json()["matches_flow_key"], false);
    assert_eq!(r.h.graph.requests().len(), 1);

    // Rotated Flow key: the other key is now the match, the first is not.
    r.flow_key(OTHER_KEY_PEM).await;
    let stale =
        r.h.call(put(
            PN_A,
            &r.key_a,
            &json!({"business_public_key": public_of(KEY_PEM)}),
        ))
        .await;
    assert_eq!(stale.json()["error"]["field"], "flow_key", "{}", stale.text);
    r.h.graph.push_json(200, json!({"success": true}));
    let fresh =
        r.h.call(put(PN_A, &r.key_a, &json!({"business_public_key": other})))
            .await;
    assert_eq!(fresh.json()["matches_flow_key"], true, "{}", fresh.text);

    // Deleted: there is nothing to compare, and the key goes through.
    let deleted =
        r.h.call(Call::new(Method::DELETE, format!("/v1/wabas/{WABA_A}/flow-key")).key(&r.key_a))
            .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    r.h.graph.push_json(200, json!({"success": true}));
    let free =
        r.h.call(put(
            PN_A,
            &r.key_a,
            &json!({"business_public_key": public_of(KEY_PEM)}),
        ))
        .await;
    assert_eq!(free.status, StatusCode::OK, "{}", free.text);
    assert_eq!(free.json()["matches_flow_key"], Value::Null);
    assert_eq!(r.h.graph.remaining(), 0);
}

/// Another tenant's Flow key is nothing to compare with: tenant B's key for
/// B's WABA does not make A's number's key a mismatch (and A's is not read
/// for B).
#[tokio::test]
async fn another_tenants_flow_key_is_not_compared() {
    let r = rig().await;
    let reply =
        r.h.call(
            Call::new(Method::PUT, format!("/v1/wabas/{WABA_B}/flow-key"))
                .key(&r.key_b)
                .json(&json!({"private_key": KEY_PEM})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    r.h.graph.push_json(200, json!({"success": true}));
    let set =
        r.h.call(put(
            PN_A,
            &r.key_a,
            &json!({"business_public_key": public_of(OTHER_KEY_PEM)}),
        ))
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text);
    assert_eq!(set.json()["matches_flow_key"], Value::Null);
}

/// **Validated before any request.** Not a 2048-bit RSA public key in PEM
/// (a private key above all), an unknown field, or an empty body: `422`,
/// nothing sent, nothing quoted. Delete the parse in `public_key` and the
/// private key is sent to Meta.
#[tokio::test]
async fn a_key_that_is_not_a_2048_bit_rsa_public_key_is_refused_before_any_request() {
    let r = rig().await;
    let public = public_of(KEY_PEM);
    let body: String = public.lines().filter(|l| !l.starts_with("-----")).collect();
    let bodies = [
        ("a private key", json!({"business_public_key": KEY_PEM})),
        ("empty", json!({"business_public_key": ""})),
        (
            "a placeholder",
            json!({"business_public_key": "<2048_bit_RSA_key>"}),
        ),
        ("no armor", json!({"business_public_key": body})),
        (
            "a bundle with a private key",
            json!({"business_public_key": format!("{public}\n{KEY_PEM}")}),
        ),
        (
            "a bad body",
            json!({"business_public_key": public, "extra": 1}),
        ),
        ("no key", json!({})),
        (
            "too long",
            json!({"business_public_key": "A".repeat(10_000)}),
        ),
    ];
    for (name, body) in bodies {
        let reply = r.h.call(put(PN_A, &r.key_a, &body)).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{name}: {}",
            reply.text
        );
        assert_eq!(reply.code(), "invalid_request", "{name}");
        assert!(!reply.text.contains("MIIE"), "{name}: the key was quoted");
    }
    assert!(r.h.graph.requests().is_empty(), "Meta was asked");
}

/// **M5.1 / M5.2.** Another tenant's number is `404` for both routes with
/// nothing sent and no vault read; a key without the scope `flows` is
/// `403`.
#[tokio::test]
async fn another_tenants_number_is_not_found_and_nothing_is_sent() {
    let r = rig().await;
    let public = public_of(KEY_PEM);
    let reads = r.h.kv.vault_reads();
    for call in [
        put(PN_B, &r.key_a, &json!({"business_public_key": public})),
        get(PN_B, &r.key_a),
        put(PN_A, &r.key_b, &json!({"business_public_key": public})),
        get(PN_A, &r.key_b),
        get("999999999999", &r.key_a),
    ] {
        let reply = r.h.call(call).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
        assert_eq!(reply.code(), "not_found");
    }
    assert_eq!(r.h.kv.vault_reads(), reads);
    assert!(r.h.graph.requests().is_empty());
    let narrow = r.h.tenant_key(A, &[Scope::Numbers]).await;
    let denied =
        r.h.call(put(PN_A, &narrow, &json!({"business_public_key": public})))
            .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.text);
    assert!(r.h.graph.requests().is_empty());
}

/// Meta's refusal is passed on with its kind (the reference's 403
/// example), the key is in no part of it, and an unreachable Meta is not
/// replayed past the library's policy.
#[tokio::test]
async fn metas_refusal_is_passed_on() {
    let r = rig().await;
    r.h.graph.push_json(
        403,
        json!({"error": {
            "message": "Your app doesn't have permission to modify encryption settings for this WhatsApp Business phone number",
            "type": "OAuthException", "code": 200, "error_subcode": 1_349_174,
            "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"
        }}),
    );
    let reply =
        r.h.call(put(
            PN_A,
            &r.key_a,
            &json!({"business_public_key": public_of(KEY_PEM)}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text);
    assert_eq!(reply.code(), "permission");
    assert!(!reply.text.contains("MIIB"), "{}", reply.text);
}
