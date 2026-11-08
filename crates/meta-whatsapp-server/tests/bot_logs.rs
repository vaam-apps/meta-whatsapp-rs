//! Roadmap M5k's (slice 2) audit events and logs: a change to a bot is
//! audited by the rule's id and counts (docs/design/server.md, D464), and no
//! line of the service's logs holds a trigger word, a reply text, a
//! customer's message, their phone number or BSUID, or Meta's text. Its
//! own test binary: the `tracing` callsites' interest is cached per
//! process, and a test in a binary of many, run on another thread without
//! the capture's subscriber, can switch the events off.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::bot::*;
use common::capture::{Captured, subscriber};
use common::{Call, Harness};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TRIGGER: &str = "TRIGGER-saffron-latte";
const REPLY: &str = "REPLY-text-with-secret-discount";
const HEARD: &str = "HEARD-the-customers-own-words";
const BANNED: &str = "16505559999";
const OWNER: &str = "16505558888";

/// Changing a bot (switching it, adding, replacing and deleting a rule) is
/// audited with the tenant, the WABA, the number, the key's public id, the
/// rule's id and counts; answering messages (a success, Meta's refusal
/// quoting the customer, a banned sender, a cooldown) logs rule ids and
/// outcomes only. Decisive: `bot_audit` and the logs of `BotReplies`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, then every line
async fn a_bot_is_audited_by_id_and_logs_nobody() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let w = world().await;
    let mut rule = json!({
        "trigger": {"type": "keyword", "words": [TRIGGER], "mode": "contains"},
        "action": {"type": "reply", "text": REPLY},
        "cooldown_seconds": 60,
    });
    let created = w.h.call(create(&w.key, PN, &rule)).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let id = created.json()["id"].as_str().unwrap().to_owned();
    rule["action"]["text"] = json!(format!("{REPLY} again"));
    let replaced = w
        .h
        .call(
            Call::new(Method::PUT, rule_path(PN, &id))
                .key(&w.key)
                .json(&rule),
        )
        .await;
    assert_eq!(replaced.status, StatusCode::OK, "{}", replaced.text);
    let on = w
        .h
        .call(put(
            &w.key,
            PN,
            &json!({"enabled": true, "owners": [OWNER], "banned": [BANNED]}),
        ))
        .await;
    assert_eq!(on.status, StatusCode::OK, "{}", on.text);

    // An answer, Meta's refusal quoting the customer, a repeat within the
    // cooldown, a banned sender.
    w.h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#131026) {CUSTOMER} {CUSTOMER_BSUID} {HEARD} cannot receive"),
                         "type": "OAuthException", "code": 131_026,
                         "error_data": {"details": format!("{CUSTOMER} {HEARD}")},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    script(&w.h, 2);
    deliver(&w.h, &inbound("wamid.LOG1", &format!("one {TRIGGER} {HEARD}"))).await;
    deliver(&w.h, &inbound("wamid.LOG2", &format!("two {TRIGGER} {HEARD}"))).await;
    deliver(
        &w.h,
        &inbound_from(BANNED, "wamid.LOG3", &format!("three {TRIGGER} {HEARD}")),
    )
    .await;
    deliver(
        &w.h,
        &inbound_from(OWNER, "wamid.LOG4", &format!("four {TRIGGER} {HEARD}")),
    )
    .await;
    assert_eq!(w.h.graph.requests().len(), 2, "one refused, one sent");

    let gone = w
        .h
        .call(Call::new(Method::DELETE, rule_path(PN, &id)).key(&w.key))
        .await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT, "{}", gone.text);
    // A refused creation is no event.
    let refused = w.h.call(create(&w.key, PN, &json!({"stray": 1}))).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);

    let logs = captured.text();
    let lines: Vec<Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    let events: Vec<Value> = lines
        .iter()
        .filter(|e| e["target"] == "audit")
        .map(|e| e["fields"].clone())
        .collect();
    let summary: Vec<(String, Option<String>, u64, bool)> = events
        .iter()
        .map(|f| {
            (
                f["action"].as_str().unwrap().to_owned(),
                f["rule_id"].as_str().map(str::to_owned),
                f["rules"].as_u64().unwrap(),
                f["enabled"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("bot_rule_created".to_owned(), Some(id.clone()), 1, false),
            ("bot_rule_updated".to_owned(), Some(id.clone()), 1, false),
            ("bot_settings_updated".to_owned(), None, 1, true),
            ("bot_rule_deleted".to_owned(), Some(id.clone()), 0, true),
        ]
    );
    for fields in &events {
        assert_eq!(fields["tenant"], TENANT, "{fields}");
        assert_eq!(fields["waba_id"], WABA, "{fields}");
        assert_eq!(fields["phone_number_id"], PN, "{fields}");
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["message"], "tenant change");
    }
    // The bot's own lines: rule ids and outcomes.
    let bot: Vec<(String, String)> = lines
        .iter()
        .filter(|e| e["target"] == "meta_whatsapp_server::api::bot")
        .filter(|e| e["fields"]["outcome"].is_string())
        .map(|e| {
            (
                e["fields"]["rule_id"].as_str().unwrap_or_default().to_owned(),
                e["fields"]["outcome"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        bot,
        [
            (id.clone(), "failed".to_owned()),
            (id.clone(), "cooling_down".to_owned()),
            (id.clone(), "sent".to_owned()),
        ],
        "{logs}"
    );
    for secret in [
        TRIGGER,
        REPLY,
        HEARD,
        CUSTOMER,
        CUSTOMER_BSUID,
        BANNED,
        OWNER,
        "cannot receive",
        TOKEN,
    ] {
        assert!(!logs.contains(secret), "{secret} was logged:\n{logs}");
    }
}

/// A `Harness` is not needed to know the log target of this module is the
/// one the design names.
#[test]
fn the_bot_logs_under_the_api_target() {
    let source = include_str!("../../meta-whatsapp-server-core/src/bot.rs");
    assert!(source.contains("\"meta_whatsapp_server::api::bot\""));
    let _ = std::any::type_name::<Harness>();
}
