//! Roadmap M5d: the Flows data endpoint on the public listener
//! (`POST /webhooks/flows/{waba_id}`; parity row 62), as Meta calls it
//! (`flows/guides/implementingyourflowendpoint`).
//!
//! The decisive tests answer requests sealed with the library's test key
//! (generated for its unit tests, never registered with Meta): the
//! requests are made by `tests/flows_data/kat_endpoint.mjs`, which wraps a
//! random AES key the way Meta's client does and checks each request with
//! `decryptRequest` from Meta's own Node.js example; the answers it seals
//! with that example's `encryptResponse` are compared byte for byte with
//! the service's (AES-GCM under a fixed key and IV is deterministic). A
//! request with a bad signature has no side effect at all.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::flows::{self, Stub, deployment, kat, kat_body, signed, signed_with};
use common::{APP_SECRET, Call, Harness, PREVIOUS_APP_SECRET, send};
use meta_whatsapp_rs::client::flows::endpoint::{EndpointAction, FlowResponse};
use meta_whatsapp_rs::webhooks::axum::body::Body;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{TenantId, TenantStatus};
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::flows_endpoint::HandlerError;
use pretty_assertions::assert_eq;
use serde_json::json;

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";

/// The library's known-answer vector from Meta's own sample code: a
/// `data_exchange` request, and the responses its `encryptResponse` sealed
/// under the same key and the flipped IV.
const KAT_NODE: &str =
    include_str!("../../meta-whatsapp-client/src/flows/endpoint/testdata/kat_node.json");

/// An app that never answers.
struct Slow;

#[async_trait::async_trait]
impl meta_whatsapp_server_core::flows_endpoint::FlowHandler for Slow {
    async fn exchange(
        &self,
        _call: meta_whatsapp_server_core::flows_endpoint::FlowCall,
    ) -> Result<FlowResponse, HandlerError> {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Err(HandlerError::Failed)
    }
}

/// A service whose Flows data endpoint is answered by `handler`, with the
/// tenant's WABA bound.
async fn with_handler(handler: Arc<Stub>) -> Harness {
    let h = Harness::with_settings(Settings {
        flows: Some(deployment(handler)),
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], "TOKEN").await;
    h
}

async fn post(h: &Harness, call: Call) -> common::Reply {
    send(&h.public, call.build()).await
}

/// Meta's requests, answered. The health check (`ping`) and the error
/// notification are answered by the service, the data exchange by the
/// integrating app; each answer is the text Meta's own Node code seals for
/// it, `200 text/plain`. The app is asked the decrypted request, with the
/// tenant and the WABA, and told of the error notification. Decisive:
/// RSA-OAEP key unwrap, AES-GCM, the response's flipped IV, and the
/// dispatch of `FlowsEndpoint::respond`.
#[tokio::test]
async fn metas_requests_are_answered_with_what_metas_code_seals() {
    let stub = Stub::summary();
    let h = with_handler(stub.clone()).await;

    for name in ["ping", "error_notification", "data_exchange"] {
        let scenario = kat(name);
        let reply = post(&h, signed(WABA, &kat_body(name))).await;
        assert_eq!(reply.status, StatusCode::OK, "{name}: {}", reply.text);
        assert_eq!(
            reply.headers["content-type"], "text/plain",
            "{name}: Meta reads text, not JSON"
        );
        assert_eq!(
            reply.text,
            scenario["sealed_response"].as_str().unwrap(),
            "{name}: byte for byte what Meta's encryptResponse seals"
        );
    }

    // The app is asked the data exchange alone, and told of the error.
    let exchanges = stub.exchanges();
    assert_eq!(exchanges.len(), 1);
    assert_eq!(exchanges[0].tenant, TENANT);
    assert_eq!(exchanges[0].waba_id, WABA);
    let request = &exchanges[0].request;
    assert_eq!(request.action, EndpointAction::DataExchange);
    assert_eq!(request.version, "3.0");
    assert_eq!(request.screen.as_deref(), Some("APPOINTMENT"));
    assert_eq!(
        request.data,
        Some(json!({"department": "shopping", "is_new_customer": true, "guests": 2}))
    );
    assert_eq!(request.flow_token.as_deref(), Some("flowtoken-1234"));
    let notifications = stub.notifications();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].1.error, "invalid-screen-transition");
    assert_eq!(
        notifications[0].1.error_message.as_deref(),
        Some("The response was invalid")
    );
    assert!(h.graph.requests().is_empty(), "Meta is never called");
}

/// The library's vector (`kat_node.json`), through the service: a
/// completion sealed under the request's key and the flipped IV is the
/// text Meta's sample code produced. Decisive: the response's key and IV.
#[tokio::test]
async fn a_completion_is_sealed_as_metas_sample_code_seals_it() {
    let vector: serde_json::Value = serde_json::from_str(KAT_NODE).unwrap();
    let stub = Stub::new(Ok(FlowResponse::complete("flowtoken-1234")));
    let h = with_handler(stub.clone()).await;
    let body = serde_json::to_vec(&vector["request"]).unwrap();
    let reply = post(&h, signed(WABA, &body)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.text, vector["sealed_success"].as_str().unwrap());
    // The health check's sealed text is the same vector's.
    assert_eq!(stub.exchanges().len(), 1);
}

/// A request whose signature is bad is refused `432` and has no side
/// effect whatever: the integrating app is not asked, no record is read or
/// written (the vault included), nothing is recorded in the outbox or the
/// audit log, Meta is not called, and the answer has no body. A missing
/// header, a malformed one, another secret's, and a valid signature of
/// another body are all alike. Decisive: the signature checked before the
/// WABA is looked up, the key loaded or a byte parsed
/// (`FlowsEndpoint::handle`).
#[tokio::test]
async fn a_bad_signature_is_refused_with_no_side_effect() {
    let stub = Stub::summary();
    let h = with_handler(stub.clone()).await;
    let body = kat_body("data_exchange");
    let other_body = kat_body("ping");
    let reads = h.kv.vault_reads();
    let writes = h.kv.written().len();
    let inserted = h.outbox.inserts().len();
    let good = meta_whatsapp_rs::webhooks::sign(
        &meta_whatsapp_rs::core::secret::AppSecret::new(APP_SECRET),
        &body,
    );
    let calls: Vec<(&str, Call)> = vec![
        (
            "another secret",
            signed_with(WABA, &body, "not-the-apps-secret"),
        ),
        (
            "another body's signature",
            Call::new(Method::POST, format!("/webhooks/flows/{WABA}"))
                .header(
                    "x-hub-signature-256",
                    &meta_whatsapp_rs::webhooks::sign(
                        &meta_whatsapp_rs::core::secret::AppSecret::new(APP_SECRET),
                        &other_body,
                    ),
                )
                .body(Body::from(body.clone())),
        ),
        (
            "no signature",
            Call::new(Method::POST, format!("/webhooks/flows/{WABA}"))
                .body(Body::from(body.clone())),
        ),
        (
            "a malformed signature",
            Call::new(Method::POST, format!("/webhooks/flows/{WABA}"))
                .header("x-hub-signature-256", "sha256=not-hex")
                .body(Body::from(body.clone())),
        ),
        (
            "the signature without its prefix",
            Call::new(Method::POST, format!("/webhooks/flows/{WABA}"))
                .header("x-hub-signature-256", good.trim_start_matches("sha256="))
                .body(Body::from(body.clone())),
        ),
        (
            "a signature of the body with a byte added",
            Call::new(Method::POST, format!("/webhooks/flows/{WABA}"))
                .header("x-hub-signature-256", &good)
                .body(Body::from([body.clone(), b" ".to_vec()].concat())),
        ),
        (
            "an unbound WABA, forged",
            signed_with("999999999999", &body, "not-the-apps-secret"),
        ),
    ];
    for (what, call) in calls {
        let reply = post(&h, call).await;
        assert_eq!(reply.status.as_u16(), 432, "{what}: {}", reply.text);
        assert!(reply.text.is_empty(), "{what}: no body: {}", reply.text);
    }
    assert_eq!(stub.calls(), 0, "the app was asked");
    assert_eq!(h.kv.vault_reads(), reads, "the vault was read");
    assert_eq!(h.kv.written().len(), writes, "something was written");
    assert_eq!(h.outbox.inserts().len(), inserted, "an event was recorded");
    assert!(h.graph.requests().is_empty(), "Meta was called");
    // And the same request, signed, is answered.
    let reply = post(&h, signed(WABA, &body)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(stub.exchanges().len(), 1);
}

/// The unsigned or forged requests are refused before their body is read:
/// a body over the route's 256 KiB is `432` when unsigned and `413` when
/// signed, never buffered for a request that cannot be Meta's. Decisive:
/// the header check before `Bytes::from_request`, and the route's body
/// limit.
#[tokio::test]
async fn the_body_is_bounded_and_read_only_for_a_signed_request() {
    let stub = Stub::summary();
    let h = with_handler(stub.clone()).await;
    let big = vec![b' '; 256 * 1024 + 1];
    let unsigned =
        Call::new(Method::POST, format!("/webhooks/flows/{WABA}")).body(Body::from(big.clone()));
    assert_eq!(post(&h, unsigned).await.status.as_u16(), 432);
    let signed_big = post(&h, signed(WABA, &big)).await;
    assert_eq!(
        signed_big.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        signed_big.text
    );
    // At the limit it is read (and is not a Flow request: 421).
    let at_limit = post(&h, signed(WABA, &vec![b' '; 256 * 1024])).await;
    assert_eq!(at_limit.status.as_u16(), 421, "{}", at_limit.text);
    assert_eq!(stub.calls(), 0);
}

/// A signed request that does not decrypt is `421`, whatever the cause
/// (Meta's client then downloads the public key again): ciphertext
/// changed, a wrapped key from another key pair, an IV of the wrong
/// length, text that is not base64, a body that is not the three fields.
/// No body says which; the app is not asked. Decisive:
/// `FlowEndpointKey::decrypt_request`'s single failure.
#[tokio::test]
async fn a_request_that_does_not_decrypt_is_421() {
    let stub = Stub::summary();
    let h = with_handler(stub.clone()).await;
    let request = kat("data_exchange")["request"].clone();
    let mut tampered = request.clone();
    let data = tampered["encrypted_flow_data"].as_str().unwrap().to_owned();
    tampered["encrypted_flow_data"] = json!(format!("A{}", &data[1..]));
    let mut short_iv = request.clone();
    short_iv["initial_vector"] = json!("AAAA");
    let mut not_base64 = request.clone();
    not_base64["encrypted_aes_key"] = json!("!!!!");
    let mut other_key = request.clone();
    other_key["encrypted_aes_key"] = json!(
        "x".repeat(8) // not a wrapped key at all
    );
    for (what, body) in [
        ("changed ciphertext", tampered.to_string()),
        ("a short IV", short_iv.to_string()),
        ("not base64", not_base64.to_string()),
        ("not a wrapped key", other_key.to_string()),
        (
            "missing fields",
            json!({"encrypted_flow_data": "AAAA"}).to_string(),
        ),
        ("not JSON", "plain text".to_owned()),
        ("empty", String::new()),
    ] {
        let reply = post(&h, signed(WABA, body.as_bytes())).await;
        assert_eq!(reply.status.as_u16(), 421, "{what}: {}", reply.text);
        assert!(reply.text.is_empty(), "{what}: {}", reply.text);
    }
    assert_eq!(stub.calls(), 0);
}

/// A WABA bound to no tenant, one whose tenant is suspended, and a
/// malformed id have no endpoint: `404`, for a signed request, the app not
/// asked; the previous app secret still verifies (a rotation). Decisive:
/// `FlowsEndpoint::tenant_of`'s binding and status checks.
#[tokio::test]
async fn an_unbound_waba_has_no_endpoint_and_a_rotation_keeps_the_old_secret() {
    let stub = Stub::summary();
    let h = with_handler(stub.clone()).await;
    let body = kat_body("data_exchange");
    for waba in ["999999999999", "not-a-waba", "0123", "1%2F2"] {
        let reply = post(&h, signed(waba, &body)).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{waba}: {}",
            reply.text
        );
    }
    assert_eq!(stub.calls(), 0);

    let reply = post(&h, signed_with(WABA, &body, PREVIOUS_APP_SECRET)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(stub.exchanges().len(), 1);

    h.store
        .update_tenant(
            &TenantId::parse(TENANT).unwrap(),
            None,
            Some(TenantStatus::Suspended),
        )
        .await
        .unwrap();
    let reply = post(&h, signed(WABA, &body)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    assert_eq!(
        stub.exchanges().len(),
        1,
        "a suspended tenant's app is not asked"
    );
}

/// The app that cannot answer, fails or is too slow does not leave Meta
/// waiting on a body: `503`, `500` and `503` (its time limit), with
/// nothing sealed; a health check never depends on it. Decisive: the
/// `HandlerError` mapping and the handler's timeout.
#[tokio::test]
async fn an_app_that_cannot_answer_is_503_or_500() {
    let body = kat_body("data_exchange");
    for (reply, expected) in [
        (Err(HandlerError::Unavailable), 503),
        (Err(HandlerError::Failed), 500),
    ] {
        let stub = Stub::new(reply);
        let h = with_handler(stub.clone()).await;
        let reply = post(&h, signed(WABA, &body)).await;
        assert_eq!(reply.status.as_u16(), expected, "{}", reply.text);
        assert!(reply.text.is_empty());
        assert_eq!(stub.exchanges().len(), 1);
        // The health check is the service's own.
        let ping = post(&h, signed(WABA, &kat_body("ping"))).await;
        assert_eq!(ping.status, StatusCode::OK);
        assert_eq!(ping.text, kat("ping")["sealed_response"].as_str().unwrap());
    }

    let h = Harness::with_settings(Settings {
        flows: Some(deployment(Arc::new(Slow)).with_handler_timeout(Duration::from_millis(50))),
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], "TOKEN").await;
    let reply = post(&h, signed(WABA, &body)).await;
    assert_eq!(
        reply.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        reply.text
    );
}

/// Without a Flow key the route is `404` even for a signed request, and
/// the binary's own handler answers a data exchange `503` while the health
/// check and the error notification are the service's. Decisive:
/// `AppState::flows_endpoint`'s `None`, and `NoHandler`.
#[tokio::test]
async fn without_a_key_there_is_no_endpoint_and_without_a_handler_no_exchange() {
    let h = Harness::with_settings(Settings {
        flows: None,
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], "TOKEN").await;
    let reply = post(&h, signed(WABA, &kat_body("ping"))).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    let h = Harness::with_settings(Settings {
        flows: Some(deployment(Arc::new(
            meta_whatsapp_server_core::flows_endpoint::NoHandler,
        ))),
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], "TOKEN").await;
    for (name, status) in [
        ("ping", StatusCode::OK),
        ("error_notification", StatusCode::OK),
        ("data_exchange", StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let reply = post(&h, signed(WABA, &kat_body(name))).await;
        assert_eq!(reply.status, status, "{name}: {}", reply.text);
    }
}

/// The private key is a secret: the deployment's `Debug` shows neither the
/// key nor an app secret, and the app is handed no `data` in its `Debug`.
/// Decisive: `FlowsDeployment`'s and `FlowCall`'s `Debug`.
#[test]
fn the_deployment_and_a_call_show_no_secret() {
    let shown = format!("{:?}", deployment(Stub::summary()));
    for secret in [APP_SECRET, PREVIOUS_APP_SECRET, "MIIE", "PRIVATE KEY"] {
        assert!(!shown.contains(secret), "{shown}");
    }
    let key = flows::KEY_PEM.lines().nth(1).unwrap();
    assert!(!format!("{:?}", deployment(Stub::summary())).contains(key));
    let call = meta_whatsapp_server_core::flows_endpoint::FlowCall {
        tenant: TenantId::parse(TENANT).unwrap(),
        waba_id: meta_whatsapp_rs::core::ids::WabaId::new(WABA).unwrap(),
        request: serde_json::from_str(
            r#"{"version":"3.0","action":"data_exchange","screen":"S","data":{"name":"Ada Lovelace"},"flow_token":"TOKEN-SECRET"}"#,
        )
        .unwrap(),
    };
    let shown = format!("{call:?}");
    assert!(
        !shown.contains("Ada") && !shown.contains("TOKEN-SECRET"),
        "{shown}"
    );
    assert!(shown.contains("merchant-42") && shown.contains('S'));
}
