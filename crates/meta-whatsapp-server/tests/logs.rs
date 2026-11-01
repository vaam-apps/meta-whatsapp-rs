//! Acceptance test M1.7, in process: the captured `tracing` output of
//! every operation of the committed document (admin, numbers, sends,
//! media, templates, events) and of Meta's webhook deliveries (signed,
//! unsigned, forged, refused), at `TRACE` for every target (the library's
//! included), holds no secret, key, token, message text, phone number or
//! contact. `live_logs.rs` repeats it on Postgres.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::Harness;
use common::capture::{Captured, check, exercise, subscriber};

#[tokio::test]
async fn every_route_logs_no_secret_key_message_text_phone_number_or_contact() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    let secrets = exercise(&h).await;
    check(&captured.text(), &secrets);
}

/// The request span names the tenant and the key a request acted as, for
/// the operator's logs: the authorization order records both on it once it
/// knows them (`Authorizer::tenant_caller`, in the core). Decisive: each
/// `record` there.
#[tokio::test]
async fn the_request_span_names_the_tenant_and_the_key() {
    use common::Call;
    use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
    use meta_whatsapp_server::model::Scope;

    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    h.tenant("merchant-7").await;
    let key = h.tenant_key("merchant-7", &[Scope::Numbers]).await;
    let reply = h.call(Call::get("/v1/numbers").key(&key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let line = captured
        .text()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|e| e["fields"]["message"] == "request" && e["span"]["route"] == "/v1/numbers")
        .expect("the request's log line");
    assert_eq!(line["span"]["tenant"], "merchant-7", "{line}");
    let key_id = key.split('_').nth(1).unwrap();
    assert_eq!(line["span"]["key_id"], key_id, "{line}");
}

/// Roadmap S7 moved the service's modules into crates of their own; their
/// log lines keep the targets they had (`meta_whatsapp_server::<module>`,
/// named explicitly), which an operator's filters and alerts name. Over
/// every route and webhook delivery of `exercise`: no line's target is a
/// moved module's new path, and the request line, the refused deliveries'
/// lines and the webhook pipeline's keep theirs. Decisive: an implicit
/// target in a moved module (its new crate's path).
#[tokio::test]
async fn every_line_keeps_its_target_from_before_the_crate_split() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    exercise(&h).await;
    let targets: std::collections::BTreeSet<String> = captured
        .text()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|e| e["target"].as_str().map(str::to_owned))
        .collect();
    let moved: Vec<&String> = targets
        .iter()
        .filter(|t| {
            [
                "meta_whatsapp_server_http",
                "meta_whatsapp_server_api_axum",
                "meta_whatsapp_server_store_",
            ]
            .iter()
            .any(|krate| t.starts_with(krate))
        })
        .collect();
    assert!(moved.is_empty(), "a moved module's path: {moved:?}");
    for kept in [
        "meta_whatsapp_server::telemetry",
        "meta_whatsapp_server::api::webhooks",
        "meta_whatsapp_server::events",
    ] {
        assert!(targets.contains(kept), "{kept} missing from {targets:?}");
    }
}
