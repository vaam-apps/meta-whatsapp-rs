//! Roadmap M5d2's log half for a number's business public key (design
//! D453): captured at `TRACE` for every target, setting and reading the
//! key, refusing it (malformed, a private key, not the Flow key's public
//! half) and Meta refusing it log no part of any key. The change is
//! audited with the tenant, the WABA, the number, the key's public id and
//! the key's SHA-256, and nothing of the key. Alone in its binary, as
//! `logs.rs` is: the capture is a thread's default subscriber.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::capture::{Captured, subscriber};
use common::flows::{KEY_PEM, OTHER_KEY_PEM};
use common::{Call, Harness};
use meta_whatsapp_rs::client::flows::endpoint::FlowEndpointKey;
use meta_whatsapp_rs::webhooks::axum::http::Method;
use meta_whatsapp_server::model::Scope;
use serde_json::json;
use sha2::{Digest, Sha256};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";

fn public_of(pem: &str) -> String {
    FlowEndpointKey::from_pem(pem)
        .unwrap()
        .public_key_pem()
        .unwrap()
}

/// Decisive: every line of `business_key`'s services, the extractors and
/// the error rendering.
#[tokio::test]
async fn a_business_key_is_in_no_log_line_and_its_change_is_audited() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::with_settings(common::test_settings());
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], "TOKEN").await;
    let key = h.tenant_key(TENANT, &[Scope::Flows]).await;
    let public = public_of(KEY_PEM);
    let other = public_of(OTHER_KEY_PEM);
    let put = |body: serde_json::Value| {
        Call::new(Method::PUT, format!("/v1/numbers/{PN}/business-public-key"))
            .key(&key)
            .json(&body)
    };

    // The Flow key, so that a mismatch is refused.
    let set = h
        .call(
            Call::new(Method::PUT, format!("/v1/wabas/{WABA}/flow-key"))
                .key(&key)
                .json(&json!({"private_key": KEY_PEM})),
        )
        .await;
    assert_eq!(set.status.as_u16(), 200, "{}", set.text);
    // Refused: malformed, a private key, a mismatch.
    for body in [
        json!({"business_public_key": "not-a-key-SENTINEL-91c4"}),
        json!({"business_public_key": KEY_PEM}),
        json!({"business_public_key": other}),
    ] {
        assert_eq!(h.call(put(body)).await.status.as_u16(), 422);
    }
    // Set (the first), refused by Meta (the second), read.
    h.graph.push_json(200, json!({"success": true}));
    assert_eq!(
        h.call(put(json!({"business_public_key": public})))
            .await
            .status
            .as_u16(),
        200
    );
    h.graph.push_json(
        403,
        json!({"error": {"message": "(#200) denied", "type": "OAuthException", "code": 200,
                         "fbtrace_id": "A1b2C3"}}),
    );
    assert_eq!(
        h.call(put(json!({"business_public_key": public})))
            .await
            .status
            .as_u16(),
        403
    );
    h.graph.push_json(
        200,
        json!({"business_public_key": public, "business_public_key_signature_status": "VALID"}),
    );
    assert_eq!(
        h.call(Call::get(format!("/v1/numbers/{PN}/business-public-key")).key(&key))
            .await
            .status
            .as_u16(),
        200
    );

    let logs = captured.text();
    for pem in [KEY_PEM, public.as_str(), other.as_str()] {
        for line in pem.lines().filter(|l| !l.starts_with("-----")) {
            assert!(!logs.contains(line), "a line of a key was logged: {line}");
        }
    }
    for text in ["PRIVATE KEY", "PUBLIC KEY", "SENTINEL-91c4", "BEGIN"] {
        assert!(!logs.contains(text), "logged: {text}");
    }
    let audited: Vec<serde_json::Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|e| e["target"] == "audit")
        .map(|e| e["fields"].clone())
        .collect();
    let actions: Vec<&str> = audited
        .iter()
        .filter_map(|f| f["action"].as_str())
        .collect();
    // The Flow key's own change, then the number's key set; the refused
    // calls and Meta's refusal (which cannot have gone through) are not.
    assert_eq!(
        actions,
        ["flow_key_set", "business_public_key_set"],
        "{audited:?}"
    );
    let key_id = key.split('_').nth(1).unwrap();
    let fields = &audited[1];
    assert_eq!(fields["tenant"], TENANT, "{fields}");
    assert_eq!(fields["waba_id"], WABA, "{fields}");
    assert_eq!(fields["phone_number_id"], PN, "{fields}");
    assert_eq!(fields["key_id"], key_id, "{fields}");
    assert_eq!(fields["outcome"], "answered", "{fields}");
    assert_eq!(
        fields["public_key_sha256"],
        hex::encode(Sha256::digest(public.as_bytes())).as_str(),
        "{fields}"
    );
    assert_eq!(
        fields.as_object().unwrap().len(),
        8,
        "no other field: {fields}"
    );
}
