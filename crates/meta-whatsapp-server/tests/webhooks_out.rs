//! Webhooks out over HTTP (roadmap M2c; design §4.2, §4.4; acceptance
//! test M2.4 end to end): the routes, a Meta webhook through
//! `/webhooks/meta`, the dispatcher on the harness's bundle, and the
//! service's own sender (TCP, HTTP/1.1) to receivers on 127.0.0.1, which
//! a development deployment's allow-list admits by CIDR. The cases that
//! need a stub resolver (a lookup turning private) and a fake clock's
//! schedule are the core's conformance suite's
//! (`conformance::webhooks_out`), on every backend.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use common::meta::{EXAMPLE_PN, EXAMPLE_WABA, example_text};
use common::{Call, Harness, test_settings};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::store::Outbox as _;
use meta_whatsapp_server_core::conformance::outbox::{row, waba_of};
use meta_whatsapp_server_core::webhooks_out::MAX_DELIVERED_DATA_BYTES;
use meta_whatsapp_server_core::webhooks_out::destination::DestinationPolicy;
use meta_whatsapp_server_core::webhooks_out::dispatch::{Dispatcher, WebhooksOut};
use meta_whatsapp_server_core::webhooks_out::seal::{DataKey, DataKeys};
use meta_whatsapp_server_core::webhooks_out::signature::{EndpointSecret, verify};
use meta_whatsapp_server_http::webhooks_out::{HyperSender, SystemResolver};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

const A: &str = "tenant-a";
const B: &str = "tenant-b";

/// A request a receiver got: its headers (lower-case names) and body.
#[derive(Debug, Clone)]
struct Received {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Received {
    fn header(&self, name: &str) -> &str {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .unwrap()
    }
}

/// An HTTP receiver on 127.0.0.1 answering `204` to every request, and
/// what it got.
async fn receiver() -> (SocketAddr, Arc<Mutex<Vec<Received>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let got: Arc<Mutex<Vec<Received>>> = Arc::default();
    let seen = got.clone();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buf = vec![0u8; 65536];
                let head_end = loop {
                    if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                    let n = tcp.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buf[..n]);
                };
                let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
                let headers: Vec<(String, String)> = head
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split_once(':'))
                    .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let length: usize = headers
                    .iter()
                    .find(|(n, _)| n == "content-length")
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap_or(0);
                while data.len() < head_end + length {
                    let n = tcp.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..n]);
                }
                seen.lock().unwrap().push(Received {
                    headers,
                    body: data[head_end..].to_vec(),
                });
                let _ = tcp
                    .write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n")
                    .await;
            });
        }
    });
    (addr, got)
}

/// Webhooks out set up as a development deployment would: a data key, the
/// allow-list `127.0.0.0/8` (loopback passes only because it is listed),
/// plain `http` allowed, the system's resolver and the service's sender.
fn webhooks_out() -> WebhooksOut {
    WebhooksOut::new(
        DataKeys::new(DataKey::generate("k1").unwrap()),
        DestinationPolicy::parse("127.0.0.0/8", true).unwrap(),
        Arc::new(SystemResolver),
        Arc::new(HyperSender::new().unwrap()),
    )
}

async fn harness() -> (Harness, Dispatcher) {
    let out = webhooks_out();
    let h = Harness::with_settings(meta_whatsapp_server::state::Settings {
        webhooks_out: Some(out.clone()),
        ..test_settings()
    });
    h.tenant(A).await;
    h.tenant(B).await;
    let dispatcher = Dispatcher::new(h.backend.as_ref(), h.state.clock(), out);
    (h, dispatcher)
}

async fn round(dispatcher: &Dispatcher) -> usize {
    dispatcher.fan_out().await.unwrap();
    dispatcher.dispatch_due().await
}

async fn create(h: &Harness, key: &str, url: &str, types: &[&str]) -> Value {
    let reply = h
        .call(
            Call::new(Method::POST, "/v1/webhook-endpoints")
                .key(key)
                .json(&json!({"url": url, "types": types})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    reply.json()
}

fn check(received: &Received, secret: &EndpointSecret) -> Result<(), String> {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let ts = received.header("webhook-timestamp");
    verify(
        secret,
        received.header("webhook-id"),
        ts,
        received.header("webhook-signature"),
        &received.body,
        now,
    )
    .map_err(|e| e.to_string())
}

/// M2.4 end to end: Meta's text webhook for tenant A's number, through
/// `/webhooks/meta`, reaches A's endpoint as a Standard Webhooks delivery
/// verified with the secret the creation answered (and no other), its body
/// the event's envelope; B's endpoint gets nothing of it; the delivery is
/// listed `delivered`; B's key finds none of A's endpoint's routes.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn a_metas_webhook_reaches_its_tenants_endpoint_signed() {
    let (h, dispatcher) = harness().await;
    h.connect(A, EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
        .await;
    let key_a = h.tenant_key(A, &[Scope::Webhooks, Scope::Events]).await;
    let key_b = h.tenant_key(B, &[Scope::Webhooks]).await;
    let (addr_a, got_a) = receiver().await;
    let (addr_b, got_b) = receiver().await;
    let created = create(
        &h,
        &key_a,
        &format!("http://{addr_a}/hook?shop=1"),
        &["message_received"],
    )
    .await;
    let secret = EndpointSecret::parse(created["secret"].as_str().unwrap()).unwrap();
    assert!(created["secret"].as_str().unwrap().starts_with("whsec_"));
    let id = created["id"].as_str().unwrap().to_owned();
    create(
        &h,
        &key_b,
        &format!("http://{addr_b}/hook"),
        &["message_received"],
    )
    .await;
    // The secret is shown once: never in a read.
    let read = h
        .call(Call::get(format!("/v1/webhook-endpoints/{id}")).key(&key_a))
        .await;
    assert_eq!(read.status, StatusCode::OK);
    assert!(read.json().get("secret").is_none());
    assert!(
        !read
            .text
            .contains(&created["secret"].as_str().unwrap()[6..])
    );
    assert_eq!(h.webhook(&example_text()).await.status, StatusCode::OK);
    assert_eq!(round(&dispatcher).await, 1);
    let got = got_a.lock().unwrap().clone();
    assert_eq!(got.len(), 1);
    check(&got[0], &secret).unwrap();
    assert!(check(&got[0], &EndpointSecret::generate().unwrap()).is_err());
    let body: Value = serde_json::from_slice(&got[0].body).unwrap();
    assert_eq!(
        (
            body["tenant_id"].as_str(),
            body["type"].as_str(),
            body["truncated"].as_bool()
        ),
        (Some(A), Some("message_received"), Some(false))
    );
    assert_eq!(body["phone_number_id"], EXAMPLE_PN);
    assert_eq!(got[0].header("webhook-id"), body["id"].as_str().unwrap());
    assert_eq!(got[0].header("content-type"), "application/json");
    // The same event, polled: the same envelope.
    let polled = h
        .call(Call::get(format!("/v1/events/{}", body["id"].as_str().unwrap())).key(&key_a))
        .await;
    assert_eq!(polled.json(), body);
    assert!(
        got_b.lock().unwrap().is_empty(),
        "B's endpoint got none of A's events"
    );
    let deliveries = h
        .call(Call::get(format!("/v1/webhook-endpoints/{id}/deliveries")).key(&key_a))
        .await;
    assert_eq!(deliveries.status, StatusCode::OK);
    let listed = &deliveries.json()["data"][0];
    assert_eq!(
        (
            listed["status"].as_str(),
            listed["attempts"].as_u64(),
            listed["last_status"].as_u64()
        ),
        (Some("delivered"), Some(1), Some(204))
    );
    // Another tenant: every route of A's endpoint is 404.
    for (method, path) in [
        (Method::GET, format!("/v1/webhook-endpoints/{id}")),
        (Method::PATCH, format!("/v1/webhook-endpoints/{id}")),
        (Method::DELETE, format!("/v1/webhook-endpoints/{id}")),
        (
            Method::POST,
            format!("/v1/webhook-endpoints/{id}/rotate-secret"),
        ),
        (Method::POST, format!("/v1/webhook-endpoints/{id}/test")),
        (
            Method::GET,
            format!("/v1/webhook-endpoints/{id}/deliveries"),
        ),
        (
            Method::POST,
            format!(
                "/v1/webhook-endpoints/{id}/deliveries/{}/retry",
                listed["id"].as_str().unwrap()
            ),
        ),
    ] {
        let mut call = Call::new(method.clone(), path.clone()).key(&key_b);
        if method == Method::PATCH {
            call = call.json(&json!({"enabled": false}));
        }
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, reply.code()),
            (StatusCode::NOT_FOUND, "not_found".to_owned()),
            "{method} {path}"
        );
    }
    let listed_b = h.call(Call::get("/v1/webhook-endpoints").key(&key_b)).await;
    assert_eq!(
        listed_b.json()["data"].as_array().unwrap().len(),
        1,
        "B lists its own only"
    );
    assert!(
        h.call(Call::get(format!("/v1/webhook-endpoints/{id}")).key(&key_a))
            .await
            .json()["enabled"]
            .as_bool()
            .unwrap()
    );
}

/// A destination outside the allow-list is `422` on `url` at creation and
/// at a change: a public host not listed, a private address outside the
/// listed CIDR, the metadata address, unparsable or with credentials.
#[tokio::test]
async fn a_destination_outside_the_allow_list_is_refused_at_creation() {
    let (h, _) = harness().await;
    let key = h.tenant_key(A, &[Scope::Webhooks]).await;
    for url in [
        "http://93.184.216.34/hook",
        "http://10.0.0.1/hook",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/hook",
        "http://user:pw@127.0.0.1/hook",
        "not a url",
    ] {
        let reply = h
            .call(
                Call::new(Method::POST, "/v1/webhook-endpoints")
                    .key(&key)
                    .json(&json!({"url": url, "types": ["message_received"]})),
            )
            .await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("url")),
            "{url}"
        );
    }
    for types in [json!([]), json!(["unknown"]), json!(["ping"])] {
        let reply = h
            .call(
                Call::new(Method::POST, "/v1/webhook-endpoints")
                    .key(&key)
                    .json(&json!({"url": "http://127.0.0.1:9/hook", "types": types})),
            )
            .await;
        assert_eq!(
            reply.json()["error"]["field"].as_str(),
            Some("types"),
            "{types}"
        );
    }
    let created = create(&h, &key, "http://127.0.0.1:9/hook", &["message_received"]).await;
    let id = created["id"].as_str().unwrap();
    let changed = h
        .call(
            Call::new(Method::PATCH, format!("/v1/webhook-endpoints/{id}"))
                .key(&key)
                .json(&json!({"url": "http://93.184.216.34/"})),
        )
        .await;
    assert_eq!(changed.json()["error"]["field"].as_str(), Some("url"));
    // Unknown fields, platform-key `tenants` among them (not built), refused.
    let reply = h
        .call(Call::new(Method::POST, "/v1/webhook-endpoints").key(&key).json(&json!({"url": "http://127.0.0.1:9/", "types": ["message_received"], "tenants": "*"})))
        .await;
    assert_eq!(reply.json()["error"]["field"].as_str(), Some("body"));
    // A key without the scope.
    let other = h.tenant_key(A, &[Scope::Events]).await;
    assert_eq!(
        h.call(Call::get("/v1/webhook-endpoints").key(&other))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

/// M2.4: an event over 256 KiB is delivered truncated (`data: null`), and
/// `GET /v1/events/{id}` answers it whole to its tenant, `404` to another.
#[tokio::test]
async fn a_large_event_is_delivered_truncated_and_fetched_by_its_tenant_only() {
    let (h, dispatcher) = harness().await;
    let key_a = h.tenant_key(A, &[Scope::Webhooks, Scope::Events]).await;
    let key_b = h.tenant_key(B, &[Scope::Events]).await;
    let (addr, got) = receiver().await;
    create(&h, &key_a, &format!("http://{addr}/"), &["history_synced"]).await;
    let pn = "106540352242999";
    let waba = meta_whatsapp_rs::core::ids::WabaId::new(waba_of(pn)).unwrap();
    h.store
        .bind_waba(
            &meta_whatsapp_server::model::TenantId::parse(A).unwrap(),
            &waba,
            &[meta_whatsapp_rs::core::ids::PhoneNumberId::new(pn).unwrap()],
        )
        .await
        .unwrap();
    let mut big = row(Some(A), "history_synced", pn, Some("big-history"));
    big.id = format!("evt_{:032x}", 0x5ca1_ab1e_u128);
    big.data = format!(
        r#"{{"event":"history_synced","pad":"{}"}}"#,
        "x".repeat(MAX_DELIVERED_DATA_BYTES)
    );
    h.outbox.insert(&big).await.unwrap().unwrap();
    assert_eq!(round(&dispatcher).await, 1);
    let delivered = got.lock().unwrap()[0].clone();
    let body: Value = serde_json::from_slice(&delivered.body).unwrap();
    assert_eq!(
        (
            body["truncated"].as_bool(),
            body["data"].is_null(),
            body["id"].as_str()
        ),
        (Some(true), true, Some(big.id.as_str()))
    );
    let whole = h
        .call(Call::get(format!("/v1/events/{}", big.id)).key(&key_a))
        .await;
    assert_eq!(whole.status, StatusCode::OK);
    assert_eq!(
        whole.json()["data"]["pad"].as_str().map(str::len),
        Some(MAX_DELIVERED_DATA_BYTES)
    );
    let theirs = h
        .call(Call::get(format!("/v1/events/{}", big.id)).key(&key_b))
        .await;
    assert_eq!(
        (theirs.status, theirs.code()),
        (StatusCode::NOT_FOUND, "not_found".to_owned())
    );
}

/// M2.4: after `rotate-secret`, deliveries carry two signatures, the new
/// secret's (answered once by the rotation) first, each verifying.
#[tokio::test]
async fn a_rotation_signs_with_both_secrets() {
    let (h, dispatcher) = harness().await;
    h.connect(A, EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
        .await;
    let key = h.tenant_key(A, &[Scope::Webhooks]).await;
    let (addr, got) = receiver().await;
    let created = create(&h, &key, &format!("http://{addr}/"), &["message_received"]).await;
    let old = EndpointSecret::parse(created["secret"].as_str().unwrap()).unwrap();
    let id = created["id"].as_str().unwrap();
    let rotated = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/webhook-endpoints/{id}/rotate-secret"),
            )
            .key(&key),
        )
        .await;
    assert_eq!(rotated.status, StatusCode::OK);
    let new = EndpointSecret::parse(rotated.json()["secret"].as_str().unwrap()).unwrap();
    assert!(rotated.json()["previous_secret_expires_at"].is_string());
    assert_eq!(h.webhook(&example_text()).await.status, StatusCode::OK);
    assert_eq!(round(&dispatcher).await, 1);
    let delivered = got.lock().unwrap()[0].clone();
    let signatures: Vec<&str> = delivered.header("webhook-signature").split(' ').collect();
    assert_eq!(signatures.len(), 2);
    let ts: i64 = delivered.header("webhook-timestamp").parse().unwrap();
    assert_eq!(
        signatures[0],
        new.sign(delivered.header("webhook-id"), ts, &delivered.body)
    );
    check(&delivered, &new).unwrap();
    check(&delivered, &old).unwrap();
    // A ping: one attempt, `202`, signed with the new secret too.
    let ping = h
        .call(Call::new(Method::POST, format!("/v1/webhook-endpoints/{id}/test")).key(&key))
        .await;
    assert_eq!(
        (ping.status, ping.json()["type"].as_str()),
        (StatusCode::ACCEPTED, Some("ping"))
    );
    assert_eq!(round(&dispatcher).await, 1);
    let pinged = got.lock().unwrap()[1].clone();
    check(&pinged, &new).unwrap();
}

/// Without a data key every route is `409 webhooks_not_configured`.
#[tokio::test]
async fn without_a_data_key_the_routes_are_not_configured() {
    let h = Harness::new();
    h.tenant(A).await;
    let key = h.tenant_key(A, &[Scope::Webhooks]).await;
    let reply = h
        .call(
            Call::new(Method::POST, "/v1/webhook-endpoints")
                .key(&key)
                .json(&json!({"url": "https://example.com/", "types": ["message_received"]})),
        )
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::CONFLICT, "webhooks_not_configured".to_owned())
    );
    let listed = h.call(Call::get("/v1/webhook-endpoints").key(&key)).await;
    assert_eq!(listed.code(), "webhooks_not_configured");
}

/// The value of the unlabelled gauge `name` on `/metrics`.
fn gauge(metrics: &str, name: &str) -> i64 {
    let prefix = format!("\n{name} ");
    let at = metrics.find(&prefix).unwrap_or_else(|| panic!("{metrics}"));
    metrics[at + prefix.len()..]
        .lines()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

/// Roadmap M2k: the deliveries held while nothing delivers them are a
/// gauge (`wa_server_webhooks_out_pending_deliveries`), the age of the
/// oldest due one another (`..._oldest_due_age_seconds`, read on the
/// harness's fake clock), both falling to 0 once they are delivered; neither
/// carries a label, and nothing on `/metrics` names an endpoint, a URL or a
/// tenant.
#[tokio::test]
async fn the_pending_deliveries_are_a_gauge_that_falls_when_they_are_delivered() {
    const PENDING: &str = "wa_server_webhooks_out_pending_deliveries";
    const AGE: &str = "wa_server_webhooks_out_oldest_due_age_seconds";
    let (h, dispatcher) = harness().await;
    let key = h.tenant_key(A, &[Scope::Webhooks]).await;
    let (addr, got) = receiver().await;
    let url = format!("http://{addr}/secret-path");
    let created = create(&h, &key, &url, &["history_synced"]).await;
    let endpoint_id = created["id"].as_str().unwrap().to_owned();
    let pn = "106540352242999";
    let waba = meta_whatsapp_rs::core::ids::WabaId::new(waba_of(pn)).unwrap();
    h.store
        .bind_waba(
            &meta_whatsapp_server::model::TenantId::parse(A).unwrap(),
            &waba,
            &[meta_whatsapp_rs::core::ids::PhoneNumberId::new(pn).unwrap()],
        )
        .await
        .unwrap();
    let metrics = h.call(Call::get("/metrics")).await.text;
    assert_eq!((gauge(&metrics, PENDING), gauge(&metrics, AGE)), (0, 0));
    for n in 0..3 {
        let event = row(Some(A), "history_synced", pn, Some(&format!("held-{n}")));
        h.outbox.insert(&event).await.unwrap().unwrap();
    }
    // Recorded as deliveries, none attempted: they are held.
    dispatcher.fan_out().await.unwrap();
    h.clock.advance(std::time::Duration::from_secs(90));
    let metrics = h.call(Call::get("/metrics")).await.text;
    assert_eq!(
        (gauge(&metrics, PENDING), gauge(&metrics, AGE)),
        (3, 90),
        "{metrics}"
    );
    // One round delivers them all (an endpoint has attempts in parallel).
    assert_eq!(round(&dispatcher).await, 3);
    assert_eq!(got.lock().unwrap().len(), 3);
    let metrics = h.call(Call::get("/metrics")).await.text;
    assert_eq!(
        (gauge(&metrics, PENDING), gauge(&metrics, AGE)),
        (0, 0),
        "{metrics}"
    );
    // No label, and nothing that names an endpoint, a URL or a tenant.
    for line in metrics.lines().filter(|l| l.contains("webhooks_out")) {
        assert!(!line.contains('{'), "{line}");
    }
    for secret in [endpoint_id.as_str(), "secret-path", "127.0.0.1", A] {
        assert!(!metrics.contains(secret), "{secret}: {metrics}");
    }
}
