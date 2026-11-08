//! Roadmap M5k's audit events and logs: a broadcast is audited by its id
//! and a count when it is created and when it is cancelled (docs/design/
//! server.md, D423), and no line of the service's logs, the workers' and
//! the bot crate's among them, holds a phone number, a business-scoped
//! user id, a template parameter or Meta's text. Its own test binary: the
//! `tracing` callsites' interest is cached per process, and a test in a
//! binary of many, run on another thread without the capture's
//! subscriber, can switch the events off.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;

use common::capture::{Captured, subscriber};
use common::{Call, Harness, Stores};
use meta_whatsapp_rs::core::clock::ManualClock;
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::broadcasts::BroadcastSettings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use time::OffsetDateTime;

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const PHONES: [&str; 3] = ["+16505550101", "+16505550102", "+16505550103"];
const BSUID: &str = "US.13491208655302741918";
const PARAM: &str = "PARAM-Aloe-Vera-wants-20-percent";
const PERSONAL: &str = "PERSONAL-PARAM-for-one-person";

fn accepted(n: usize) -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": PHONES[0], "wa_id": "16505550101"}],
        "messages": [{"id": format!("wamid.LOG{n}")}]
    })
}

/// Creating, sending (a success, Meta's refusal quoting the person, a
/// timeout), reading and cancelling a broadcast: the audit events are
/// `broadcast_created` twice and `broadcast_cancelled` once, with the
/// tenant, the WABA, the number, the key's public id, the broadcast's id
/// and how many people; nothing in any line names a person, a parameter or
/// quotes Meta. Decisive: `Broadcasts::create`, `cancel` and the logs of
/// the sender.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, then every line
async fn broadcasts_are_audited_by_count_and_log_nobody() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let settings = Settings {
        broadcasts: BroadcastSettings {
            timer: Some(Arc::new(clock.clone())),
            ..BroadcastSettings::default()
        },
        ..common::test_settings()
    };
    let h = Harness::with_clock(Stores::memory(), settings, clock.clone(), |graph| {
        Arc::new(graph)
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Broadcasts]).await;
    let template = json!({
        "name": "spring_sale", "language": {"code": "en_US"},
        "components": [{"type": "body", "parameters": [{"type": "text", "text": PARAM}]}]
    });
    let personal = json!([{"type": "body", "parameters": [{"type": "text", "text": PERSONAL}]}]);
    let create = |people: Value| {
        Call::new(Method::POST, format!("/v1/numbers/{PN}/broadcasts"))
            .key(&key)
            .json(&json!({"template": template, "recipients": people}))
    };

    let first = h
        .call(create(json!([
            {"phone": PHONES[0]},
            {"phone": PHONES[1], "components": personal},
            {"user_id": BSUID},
        ])))
        .await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.text);
    let first = first.json()["id"].as_str().unwrap().to_owned();
    h.graph.push_json(200, accepted(0));
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#131050) {} {BSUID} stopped marketing: {PARAM}", PHONES[1]),
                         "type": "OAuthException", "code": 131_050,
                         "error_data": {"details": format!("{} {PERSONAL}", PHONES[1])},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    h.graph.push_error(|| TransportError::Timeout);
    h.state.broadcast_workers().unwrap()[0]
        .tick()
        .await
        .unwrap();
    let view = h
        .call(Call::get(format!("/v1/numbers/{PN}/broadcasts/{first}")).key(&key))
        .await;
    assert_eq!(view.status, StatusCode::OK, "{}", view.text);
    assert_eq!(view.json()["sent"], 1);
    assert_eq!(view.json()["unknown"], 1);
    assert_eq!(view.json()["failures"], json!({"marketing_opted_out": 1}));
    let shown = view.text.clone();

    let second = h.call(create(json!([{"phone": PHONES[2]}]))).await;
    assert_eq!(second.status, StatusCode::ACCEPTED, "{}", second.text);
    let second = second.json()["id"].as_str().unwrap().to_owned();
    let cancelled = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/numbers/{PN}/broadcasts/{second}/cancel"),
            )
            .key(&key),
        )
        .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.text);
    // Cancelling it again changes nothing, and audits nothing.
    let again = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/numbers/{PN}/broadcasts/{second}/cancel"),
            )
            .key(&key),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    let refused = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{PN}/broadcasts"))
                .key(&key)
                .json(&json!({"template": template, "recipients": [{"phone": PHONES[2][1..]}]})),
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);

    let logs = captured.text();
    let events: Vec<Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["target"] == "audit")
        .map(|e| e["fields"].clone())
        .collect();
    let summary: Vec<(String, String, u64)> = events
        .iter()
        .map(|f| {
            (
                f["action"].as_str().unwrap().to_owned(),
                f["broadcast_id"].as_str().unwrap().to_owned(),
                f["recipients"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("broadcast_created".to_owned(), first.clone(), 3),
            ("broadcast_created".to_owned(), second.clone(), 1),
            ("broadcast_cancelled".to_owned(), second, 1),
        ]
    );
    for fields in &events {
        assert_eq!(fields["tenant"], TENANT, "{fields}");
        assert_eq!(fields["waba_id"], WABA, "{fields}");
        assert_eq!(fields["phone_number_id"], PN, "{fields}");
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["message"], "tenant change");
    }
    for who in
        PHONES
            .iter()
            .copied()
            .chain([BSUID, PARAM, PERSONAL, "stopped marketing", &PHONES[0][1..]])
    {
        assert!(!logs.contains(who), "{who} was logged:\n{logs}");
        assert!(!shown.contains(who), "{who} was answered:\n{shown}");
    }
    for answer in [&view.text, &cancelled.text, &again.text] {
        assert!(!answer.contains(TOKEN));
    }
    assert!(!logs.contains(TOKEN), "the token was logged");
}
