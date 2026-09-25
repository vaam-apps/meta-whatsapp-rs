//! Acceptance test M1.6, second half: the OpenAPI document generated from
//! the code equals the committed `openapi/v1.json`, and what the internal
//! listener serves is that document. Plus the operations routes.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness, VERIFY_TOKEN, send};
use meta_whatsapp_rs::webhooks::axum::body::Body;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::api::{MAX_BODY_BYTES, PUBLIC_ROUTES, internal_routes, openapi_document};
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
            assert_eq!(
                operation["responses"]["default"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/ErrorBody",
                "{label}: default"
            );
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
    // The codes are in the document, for generated clients, as an open
    // set: the known ones, or any string.
    let error_code = &spec["components"]["schemas"]["ErrorCode"]["anyOf"];
    assert_eq!(error_code[0]["$ref"], "#/components/schemas/KnownErrorCode");
    assert_eq!(error_code[1], serde_json::json!({"type": "string"}));
    let codes = spec["components"]["schemas"]["KnownErrorCode"]["enum"]
        .as_array()
        .unwrap();
    assert!(codes.iter().any(|c| c == "waba_owned_by_another_tenant"));
    // Paging: 1 to 100, 50 by default.
    let limit = spec["paths"]["/v1/numbers"]["get"]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "limit")
        .unwrap()["schema"]
        .clone();
    assert_eq!(
        (&limit["minimum"], &limit["maximum"], &limit["default"]),
        (
            &serde_json::json!(1),
            &serde_json::json!(100),
            &serde_json::json!(50)
        ),
        "{limit}"
    );
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
    // Deliveries: an unsigned one is a bare 401 (tests/webhooks.rs has the
    // rest); any other method is 405.
    let post = send(&h.public, Call::new(Method::POST, "/webhooks/meta").build()).await;
    assert_eq!(post.status, StatusCode::UNAUTHORIZED);
    assert!(post.text.is_empty());
    let put = send(&h.public, Call::new(Method::PUT, "/webhooks/meta").build()).await;
    assert_eq!(
        (put.status, put.code().as_str()),
        (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
    );
    assert_eq!(put.headers["allow"], "GET,HEAD,POST");
    assert_eq!(
        send(&h.public, Call::get("/livez").build()).await.status,
        StatusCode::OK
    );
    // Nothing of the API, the operations or the admin is on it: every
    // operation of the internal listener, with a valid admin key and
    // tenant key for good measure, is a 404 there.
    let admin = h.admin_key().await;
    h.tenant("merchant-42").await;
    h.connect(
        "merchant-42",
        "102290129340398",
        &["106540352242922"],
        "TOKEN",
    )
    .await;
    let tenant_key = h.tenant_key("merchant-42", &common::ALL_SCOPES).await;
    let sample = common::Sample {
        tenant: "merchant-42".to_owned(),
        waba: "102290129340398".to_owned(),
        pn: "106540352242922".to_owned(),
        key_id: "placeholder".to_owned(),
    };
    let mut checked = 0;
    for operation in common::spec_operations() {
        if PUBLIC_ROUTES.contains(&operation.template.as_str()) {
            continue;
        }
        let key = if operation.admin() {
            &admin
        } else {
            &tenant_key
        };
        let reply = send(
            &h.public,
            common::sample_call(&operation, &sample, Some(key)).build(),
        )
        .await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{} on the public listener",
            operation.label()
        );
        checked += 1;
    }
    assert!(checked >= 20, "{checked}");
    // The document and the listener's route templates are one list.
    let templates: std::collections::BTreeSet<String> = common::spec_operations()
        .into_iter()
        .map(|o| o.template)
        .collect();
    assert_eq!(templates, internal_routes().into_iter().collect());
    assert!(h.graph.requests().is_empty());
    assert_eq!(PUBLIC_ROUTES, ["/webhooks/meta", "/livez"]);
}

/// Bodies over 64 KiB are refused with `413 payload_too_large`, at 64 KiB
/// they are read. Decisive: the body limit layer.
#[tokio::test]
async fn bodies_over_64_kib_are_413() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    let body = |len: usize| {
        let head = r#"{"id": "merchant-42", "name": ""#;
        let tail = r#""}"#;
        format!("{head}{}{tail}", "x".repeat(len - head.len() - tail.len()))
    };
    let at_limit = body(MAX_BODY_BYTES);
    assert_eq!(at_limit.len(), 64 * 1024);
    let over = body(MAX_BODY_BYTES + 1);
    for (text, expected) in [
        (
            at_limit,
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
        ),
        (over, (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")),
    ] {
        let reply = h
            .call(
                Call::new(Method::POST, "/v1/admin/tenants")
                    .key(&admin)
                    .header("content-type", "application/json")
                    .body(Body::from(text)),
            )
            .await;
        assert_eq!((reply.status, reply.code().as_str()), expected);
    }
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

/// A method a path does not take is `405 method_not_allowed` with the
/// error body and `Allow`, on either listener, before any key is checked
/// (conventions review S2).
#[tokio::test]
async fn a_wrong_method_is_405_with_the_error_body() {
    let h = Harness::new();
    for (path, allow) in [
        ("/v1/numbers", "GET,HEAD"),
        ("/v1/admin/tenants", "POST,GET,HEAD"),
        ("/readyz", "GET,HEAD"),
    ] {
        let reply = h.call(Call::new(Method::PUT, path)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"),
            "{path}"
        );
        assert_eq!(reply.headers["allow"], allow, "{path}");
    }
}
