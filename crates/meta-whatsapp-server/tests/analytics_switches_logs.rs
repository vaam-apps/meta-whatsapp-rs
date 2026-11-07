//! Roadmap M5g2's audit events (docs/design/server.md, D286): confirming
//! template insights and a template's click tracking are the caller's audit
//! events, by ids and a flag, with an outcome, and Meta's texts are in no
//! answer and no log. Its own test binary: the `tracing` callsites'
//! interest is cached per process, and a test in a binary of many, run on
//! another thread without the capture's subscriber, can switch the events
//! off.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::capture::{Captured, subscriber};
use common::{Call, Harness};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const TEMPLATE: &str = "245435364965041";
const NAME: &str = "seasonal_sale_promo";
const SENTINEL: &str = "SENTINEL-META-TEXT";

fn refusal() -> Value {
    json!({"error": {"message": format!("(#100) {SENTINEL}"), "type": "OAuthException", "code": 100,
                     "error_data": {"details": SENTINEL}, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}})
}

fn lookup(h: &Harness) {
    h.graph
        .push_json(200, json!({"name": NAME, "id": TEMPLATE}));
    h.graph.push_json(
        200,
        json!({"data": [{"id": TEMPLATE, "category": "MARKETING"}]}),
    );
}

/// Both switches are audit events: the action, the key's public id, the
/// tenant, the WABA, the template and the flag for the opt-out, and an
/// outcome: `answered`, or `unknown` when the call may have gone through
/// unanswered (a timeout). A change Meta refuses outright changed nothing
/// and is not audited. No line holds Meta's sentinel text or the template's
/// name. Decisive: `Analytics::audit` and `switch_failed`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario: confirm, set, cut, refuse, then every line
async fn the_switches_are_audited_by_ids_and_never_with_metas_texts() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::with_settings(Settings {
        legal_acts: true,
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Analytics]).await;
    let insights = || {
        Call::new(
            Method::POST,
            format!("/v1/wabas/{WABA}/analytics/template-insights"),
        )
        .key(&key)
    };
    let tracking = |opted_out: bool| {
        Call::new(
            Method::PUT,
            format!("/v1/wabas/{WABA}/analytics/templates/{TEMPLATE}/click-tracking"),
        )
        .key(&key)
        .json(&json!({ "opted_out": opted_out }))
    };

    h.graph
        .push_json(200, json!({"id": 102_290_129_340_398_u64}));
    assert_eq!(h.call(insights()).await.status, StatusCode::NO_CONTENT);
    lookup(&h);
    h.graph.push_json(200, json!({"success": true}));
    assert_eq!(h.call(tracking(true)).await.status, StatusCode::NO_CONTENT);
    // Cut off: the change may have been made.
    lookup(&h);
    h.graph.push_error(|| TransportError::Timeout);
    assert_eq!(
        h.call(tracking(false)).await.status,
        StatusCode::GATEWAY_TIMEOUT
    );
    // Refused outright, quoting a sentinel: nothing changed, nothing audited.
    h.graph.push_json(400, refusal());
    let refused = h.call(insights()).await;
    assert!(refused.status.is_client_error(), "{}", refused.text);
    lookup(&h);
    h.graph.push_json(400, refusal());
    let refused_too = h.call(tracking(true)).await;
    assert!(refused_too.status.is_client_error(), "{}", refused_too.text);
    assert_eq!(h.graph.remaining(), 0);

    let logs = captured.text();
    let events: Vec<Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["target"] == "audit")
        .map(|e| e["fields"].clone())
        .collect();
    let summary: Vec<(String, Option<String>, Option<bool>, String)> = events
        .iter()
        .map(|f| {
            (
                f["action"].as_str().unwrap().to_owned(),
                f["template_id"].as_str().map(str::to_owned),
                f["opted_out"].as_bool(),
                f["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                "template_insights_enabled".to_owned(),
                None,
                None,
                "answered".to_owned()
            ),
            (
                "template_click_tracking_set".to_owned(),
                Some(TEMPLATE.to_owned()),
                Some(true),
                "answered".to_owned()
            ),
            (
                "template_click_tracking_set".to_owned(),
                Some(TEMPLATE.to_owned()),
                Some(false),
                "unknown".to_owned()
            ),
        ]
    );
    for fields in &events {
        assert_eq!(fields["tenant"], TENANT, "{fields}");
        assert_eq!(fields["waba_id"], WABA, "{fields}");
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["message"], "tenant change");
    }
    assert!(!logs.contains(SENTINEL), "Meta's text was logged:\n{logs}");
    assert!(
        !logs.contains(NAME),
        "a template's name was logged:\n{logs}"
    );
    assert!(!logs.contains(TOKEN), "the token was logged:\n{logs}");
}

/// With the legal acts off, the confirmation writes no audit event (it
/// did not happen) and asks nothing of Meta. Decisive: the gate before the
/// audit.
#[tokio::test]
async fn a_refused_confirmation_is_not_an_audit_event() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Analytics]).await;
    let reply = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/wabas/{WABA}/analytics/template-insights"),
            )
            .key(&key),
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.code(), "feature_not_available");
    assert!(h.graph.requests().is_empty());
    let logs = captured.text();
    assert!(
        !logs.contains("template_insights_enabled"),
        "audited:\n{logs}"
    );
}
