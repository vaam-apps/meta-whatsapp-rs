//! Roadmap M5g's audit events and logs: a block and an unblock are audited
//! by count and never by person (docs/design/server.md, D143), and no line
//! of the service's logs holds a phone number or a BSUID of the block list.
//! Its own test binary: the `tracing` callsites' interest is cached per
//! process, and a test in a binary of many, run on another thread without
//! the capture's subscriber, can switch the events off.
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
const BLOCKED: &str = "+16505551234";
const BLOCKED_DIGITS: &str = "16505551234";
const BLOCKED_2: &str = "+14155559876";
const BLOCKED_USER_ID: &str = "US.13491208655302741918";

fn call(method: Method, path: String, key: &str) -> Call {
    Call::new(method, path).key(key)
}

/// A block and an unblock are audit events (the design's D143): the action,
/// the key's public id, the tenant, the WABA and the number by id, and the
/// counts (people named, done, refused) with an outcome: `answered`, or
/// `unknown` when the call may have gone through unanswered (a timeout).
/// Never who: the event and every other line hold no phone number and no
/// BSUID. Decisive: `BlockedUsers::block` and `change_failed`.
#[tokio::test]
async fn blocking_is_audited_by_count_and_never_by_person() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Contacts]).await;
    h.graph.push_json(
        200,
        json!({"block_users": {"added_users": [{"input": BLOCKED, "wa_id": BLOCKED_DIGITS}],
            "failed_users": [{"input": BLOCKED_2, "errors": [{"code": 131_047}]}]}}),
    );
    let body = json!({"users": [{"phone": BLOCKED}, {"phone": BLOCKED_2}]});
    let blocked = h
        .call(
            call(
                Method::POST,
                format!("/v1/numbers/{PN}/blocked-users"),
                &key,
            )
            .json(&body),
        )
        .await;
    assert_eq!(blocked.status, StatusCode::OK, "{}", blocked.text);
    h.graph.push_json(
        200,
        json!({"block_users": {"removed_users": [{"input": BLOCKED, "wa_id": BLOCKED_DIGITS}]}}),
    );
    let unblocked = h
        .call(
            call(
                Method::POST,
                format!("/v1/numbers/{PN}/blocked-users/unblock"),
                &key,
            )
            .json(&json!({"users": [{"phone": BLOCKED}]})),
        )
        .await;
    assert_eq!(unblocked.status, StatusCode::OK, "{}", unblocked.text);
    h.graph.push_error(|| TransportError::Timeout);
    let cut = h
        .call(
            call(
                Method::POST,
                format!("/v1/numbers/{PN}/blocked-users"),
                &key,
            )
            .json(&body),
        )
        .await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    // Meta's `139100` with an error status: some people may have been
    // blocked and which is not said, so the outcome is unknown.
    h.graph.push_json(
        400,
        json!({"error": {"message": "(#139100) Failed to block/unblock users", "type": "OAuthException",
                         "code": 139_100, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let partial = h
        .call(
            call(
                Method::POST,
                format!("/v1/numbers/{PN}/blocked-users"),
                &key,
            )
            .json(&body),
        )
        .await;
    assert_eq!(partial.status, StatusCode::BAD_GATEWAY, "{}", partial.text);
    // Refused by Meta outright: nothing changed, nothing audited.
    h.graph.push_json(
        400,
        json!({"error": {"message": "Invalid parameter", "type": "OAuthException", "code": 100,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h
        .call(
            call(
                Method::POST,
                format!("/v1/numbers/{PN}/blocked-users"),
                &key,
            )
            .json(&body),
        )
        .await;
    assert!(refused.status.is_client_error(), "{}", refused.text);

    let logs = captured.text();
    let events: Vec<Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["target"] == "audit")
        .map(|e| e["fields"].clone())
        .collect();
    let summary: Vec<(String, u64, u64, u64, String)> = events
        .iter()
        .map(|f| {
            (
                f["action"].as_str().unwrap().to_owned(),
                f["users"].as_u64().unwrap(),
                f["done"].as_u64().unwrap(),
                f["failed"].as_u64().unwrap(),
                f["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("users_blocked".to_owned(), 2, 1, 1, "answered".to_owned()),
            ("users_unblocked".to_owned(), 1, 1, 0, "answered".to_owned()),
            ("users_blocked".to_owned(), 2, 0, 0, "unknown".to_owned()),
            ("users_blocked".to_owned(), 2, 0, 0, "unknown".to_owned()),
        ]
    );
    for fields in &events {
        assert_eq!(fields["tenant"], TENANT, "{fields}");
        assert_eq!(fields["waba_id"], WABA, "{fields}");
        assert_eq!(fields["phone_number_id"], PN, "{fields}");
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["message"], "tenant change");
    }
    for who in [
        BLOCKED,
        BLOCKED_DIGITS,
        BLOCKED_2,
        "14155559876",
        BLOCKED_USER_ID,
    ] {
        assert!(!logs.contains(who), "{who} was logged:\n{logs}");
    }
}
