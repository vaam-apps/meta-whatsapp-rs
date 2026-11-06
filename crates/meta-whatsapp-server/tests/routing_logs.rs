//! Roadmap M5l's audit events and logs: a thread action is audited by the
//! number and the role passed to, never by user or `metadata`
//! (docs/design/server.md, D221), and no line of the service's logs holds
//! a user's phone number, `wa_id` or BSUID, nor a `metadata`. Its own test
//! binary: the `tracing` callsites' interest is cached per process, and a
//! test in a binary of many, run on another thread without the capture's
//! subscriber, can switch the events off.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::capture::{Captured, subscriber};
use common::{Call, Harness};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const PHONE: &str = "+16505551234";
const DIGITS: &str = "16505551234";
const BSUID: &str = "US.13491208655302741918";
const NOTE: &str = "ROUTING-NOTE-the-customer-asked-for-a-human";

fn act(action: &str, key: &str, body: &Value) -> Call {
    Call::new(
        Method::POST,
        format!("/v1/numbers/{PN}/thread-control/{action}"),
    )
    .key(key)
    .json(body)
}

/// Every action is an audit event (the design's D221): the action, the
/// key's public id, the tenant, the WABA and the number by id, the role a
/// pass named and an outcome: `answered`, or `unknown` when the call may
/// have gone through unanswered (a timeout). Refused by Meta outright,
/// nothing is audited (nothing changed). Never who: the event and every
/// other line hold no phone number, no `wa_id`, no BSUID and no
/// `metadata`, also when Meta's refusal quotes them. Decisive:
/// `Routing::pass`, `take`, `release` and `action_failed`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario: each action, a cut, a refusal, then every line
async fn actions_are_audited_without_the_user_or_the_note() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Routing]).await;
    let accepted = json!({"messaging_product": "whatsapp", "request_id": "REQ-1"});

    h.graph.push_json(200, accepted.clone());
    let passed = h
        .call(act(
            "pass",
            &key,
            &json!({"user": {"user_id": BSUID}, "target_role": "ai_agent", "metadata": NOTE}),
        ))
        .await;
    assert_eq!(passed.status, StatusCode::OK, "{}", passed.text);
    h.graph.push_json(200, accepted.clone());
    let untargeted = h
        .call(act("pass", &key, &json!({"user": {"phone": PHONE}})))
        .await;
    assert_eq!(untargeted.status, StatusCode::OK, "{}", untargeted.text);
    h.graph.push_json(200, accepted.clone());
    let taken = h
        .call(act(
            "take",
            &key,
            &json!({"user": {"wa_id": DIGITS}, "metadata": NOTE}),
        ))
        .await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.text);
    h.graph.push_json(200, accepted);
    let released = h
        .call(act("release", &key, &json!({"user": {"phone": PHONE}})))
        .await;
    assert_eq!(released.status, StatusCode::OK, "{}", released.text);
    h.graph.push_error(|| TransportError::Timeout);
    let cut = h
        .call(act(
            "pass",
            &key,
            &json!({"user": {"phone": PHONE}, "target_role": "ctwa", "metadata": NOTE}),
        ))
        .await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#100) recipient is invalid. {BSUID} {PHONE} {NOTE}"),
                         "type": "OAuthException", "code": 100,
                         "error_data": {"details": format!("{DIGITS} {NOTE}")},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h
        .call(act(
            "take",
            &key,
            &json!({"user": {"user_id": BSUID}, "metadata": NOTE}),
        ))
        .await;
    assert!(refused.status.is_client_error(), "{}", refused.text);

    let logs = captured.text();
    let events: Vec<Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["target"] == "audit")
        .map(|e| e["fields"].clone())
        .collect();
    let summary: Vec<(String, Option<String>, String)> = events
        .iter()
        .map(|f| {
            (
                f["action"].as_str().unwrap().to_owned(),
                f["target_role"].as_str().map(str::to_owned),
                f["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let row = |action: &str, role: Option<&str>, outcome: &str| {
        (
            action.to_owned(),
            role.map(str::to_owned),
            outcome.to_owned(),
        )
    };
    assert_eq!(
        summary,
        [
            row("thread_passed", Some("ai_agent"), "answered"),
            row("thread_passed", None, "answered"),
            row("thread_taken", None, "answered"),
            row("thread_released", None, "answered"),
            row("thread_passed", Some("ctwa"), "unknown"),
        ]
    );
    for fields in &events {
        assert_eq!(fields["tenant"], TENANT, "{fields}");
        assert_eq!(fields["waba_id"], WABA, "{fields}");
        assert_eq!(fields["phone_number_id"], PN, "{fields}");
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["message"], "tenant change");
    }
    for who in [PHONE, DIGITS, BSUID, NOTE] {
        assert!(!logs.contains(who), "{who} was logged:\n{logs}");
    }
}
