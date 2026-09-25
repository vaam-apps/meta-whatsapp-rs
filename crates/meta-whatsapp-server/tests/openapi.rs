//! Acceptance test M1.6, second half: the OpenAPI document generated from
//! the code equals the committed `openapi/v1.json`, and what the internal
//! listener serves is that document. Plus the operations routes.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness, VERIFY_TOKEN, send};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::api::{PUBLIC_ROUTES, openapi_document};
use serde_json::Value;

const COMMITTED: &str = include_str!("../openapi/v1.json");

#[test]
fn the_generated_document_is_the_committed_one() {
    let generated = openapi_document();
    assert!(
        generated == COMMITTED,
        "crates/meta-whatsapp-server/openapi/v1.json is stale: regenerate it with \
         `cargo run -p meta-whatsapp-server -- openapi > crates/meta-whatsapp-server/openapi/v1.json` \
         and review the diff (a change within v1 must be additive)"
    );
    let spec: Value = serde_json::from_str(COMMITTED).unwrap();
    assert_eq!(spec["openapi"], "3.1.0");
    assert_eq!(spec["info"]["version"], env!("CARGO_PKG_VERSION"));
}

/// Every keyed route declares the key, every tenant route the `WA-Tenant`
/// header, and every error answer the one error body.
#[test]
fn the_document_describes_keys_tenants_and_errors_everywhere() {
    let spec: Value = serde_json::from_str(COMMITTED).unwrap();
    let mut operations = 0;
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            operations += 1;
            let label = format!("{method} {path}");
            let keyed = path.starts_with("/v1/")
                && !matches!(path.as_str(), "/v1/openapi.json" | "/v1/version");
            assert_eq!(
                operation.get("security").is_some(),
                keyed,
                "{label}: security"
            );
            if keyed {
                assert!(operation["responses"].get("401").is_some(), "{label}: 401");
                assert!(operation["responses"].get("403").is_some(), "{label}: 403");
            }
            let tenant_route = keyed && !path.starts_with("/v1/admin");
            let names_tenant = operation["parameters"].as_array().is_some_and(|ps| {
                ps.iter()
                    .any(|p| p["name"] == "WA-Tenant" && p["in"] == "header")
            });
            assert_eq!(names_tenant, tenant_route, "{label}: WA-Tenant");
            for (status, response) in operation["responses"].as_object().unwrap() {
                if status.starts_with('4') || status.starts_with('5') {
                    assert_eq!(
                        response["content"]["application/json"]["schema"]["$ref"],
                        "#/components/schemas/ErrorBody",
                        "{label}: {status}"
                    );
                }
            }
        }
    }
    assert!(operations >= 24, "{operations} operations");
    // The codes are in the document, for generated clients.
    let codes = spec["components"]["schemas"]["ErrorCode"]["enum"]
        .as_array()
        .unwrap();
    assert!(codes.iter().any(|c| c == "waba_owned_by_another_tenant"));
}

#[tokio::test]
async fn the_internal_listener_serves_the_document_health_metrics_and_version() {
    let h = Harness::new();
    let served = h.call(Call::get("/v1/openapi.json")).await;
    assert_eq!(served.status, StatusCode::OK);
    assert_eq!(served.text, COMMITTED);

    let live = h.call(Call::get("/livez")).await;
    assert_eq!(
        (live.status, live.json()["status"].as_str()),
        (StatusCode::OK, Some("ok"))
    );
    let ready = h.call(Call::get("/readyz")).await;
    assert_eq!(
        (ready.status, ready.json()["status"].as_str()),
        (StatusCode::OK, Some("ready"))
    );

    let version = h.call(Call::get("/v1/version")).await.json();
    assert_eq!(version["server"], env!("CARGO_PKG_VERSION"));
    assert_eq!(version["graph_api_version"], "v25.0");
    assert_eq!(version["api_version"], "v1");
    assert!(version["meta_whatsapp_rs_revision"].is_string());

    // Requests are counted by route template, never by id.
    let _ = h.call(Call::get("/v1/numbers/106540352242922")).await;
    let metrics = h.call(Call::get("/metrics")).await;
    assert_eq!(metrics.status, StatusCode::OK);
    assert!(
        metrics.text.contains("route=\"/v1/numbers/{pn}\""),
        "{}",
        metrics.text
    );
    assert!(!metrics.text.contains("106540352242922"));
    assert!(metrics.text.contains("code=\"unauthenticated\""));

    // Shutting down: /readyz fails first.
    h.state.begin_shutdown();
    let draining = h.call(Call::get("/readyz")).await;
    assert_eq!(
        (draining.status, draining.code().as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "shutting_down")
    );

    // An unknown route is a JSON 404, with the request id echoed.
    let unknown = h
        .call(Call::get("/v2/anything").header("x-request-id", "abc-123"))
        .await;
    assert_eq!(unknown.code(), "not_found");
    assert_eq!(unknown.headers["x-request-id"], "abc-123");
    assert_eq!(unknown.json()["error"]["request_id"], "abc-123");
}

#[tokio::test]
async fn the_public_listener_serves_metas_check_and_livez_only() {
    let h = Harness::new();
    let challenge = send(
        &h.public,
        Call::get(format!(
            "/webhooks/meta?hub.mode=subscribe&hub.challenge=1158201444&hub.verify_token={VERIFY_TOKEN}"
        ))
        .build(),
    )
    .await;
    assert_eq!(challenge.status, StatusCode::OK);
    assert_eq!(challenge.text, "1158201444");
    assert_eq!(
        challenge.headers["content-type"],
        "text/plain; charset=utf-8"
    );
    for query in [
        "hub.mode=subscribe&hub.challenge=1&hub.verify_token=wrong",
        "hub.mode=unsubscribe&hub.challenge=1&hub.verify_token=verify-token-for-tests",
        "hub.challenge=1",
        "",
    ] {
        let reply = send(
            &h.public,
            Call::get(format!("/webhooks/meta?{query}")).build(),
        )
        .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{query}");
        assert!(reply.text.is_empty());
    }
    // Deliveries arrive with M1c: no POST yet, so Meta retries.
    let post = send(&h.public, Call::new(Method::POST, "/webhooks/meta").build()).await;
    assert_eq!(post.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        send(&h.public, Call::get("/livez").build()).await.status,
        StatusCode::OK
    );
    // Nothing of the API, the operations or the admin is on it.
    for path in [
        "/v1/numbers",
        "/v1/admin/tenants",
        "/metrics",
        "/readyz",
        "/v1/openapi.json",
        "/v1/version",
    ] {
        let reply = send(&h.public, Call::get(path).build()).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(PUBLIC_ROUTES, ["/webhooks/meta", "/livez"]);
}

/// Anyone on the internet may send any token as a method to the public
/// listener: the metrics keep a bounded label set, never the token.
/// Decisive: labelling requests by their raw method (security review H1).
#[tokio::test]
async fn made_up_methods_are_counted_as_other() {
    let h = Harness::new();
    let paths = ["/livez", "/webhooks/meta", "/nowhere"];
    for i in 0..100 {
        let method = Method::from_bytes(format!("MADEUP{i}").as_bytes()).unwrap();
        let path = paths[i % paths.len()];
        let reply = send(&h.public, Call::new(method, path).build()).await;
        assert!(reply.status.is_client_error(), "{path}: {}", reply.status);
    }
    let metrics = h.state.metrics().render();
    assert!(!metrics.contains("MADEUP"), "{metrics}");
    let series = metrics
        .lines()
        .filter(|l| l.starts_with("wa_server_http_requests_total{"))
        .count();
    assert!(
        (1..=paths.len()).contains(&series),
        "{series} request series for 100 made-up methods:\n{metrics}"
    );
    assert!(
        metrics.contains("listener=\"public\",method=\"other\""),
        "{metrics}"
    );
}
