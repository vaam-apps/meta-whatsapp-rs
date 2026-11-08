//! What the bot tests share (roadmap M5k, slice 2): the world they run in,
//! Meta's inbound text as the webhook carries it, and the requests they
//! make.
#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    clippy::unwrap_used,
    clippy::expect_used
)] // not every test binary uses every item

use std::sync::Arc;

use meta_whatsapp_rs::adapters::store::MemoryKvStore;
use meta_whatsapp_rs::core::clock::ManualClock;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::bot::BotSettings;
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::meta::{bytes, sent_now, text, with_ids};
use super::{Call, Harness, Stores};

pub const TENANT: &str = "merchant-42";
pub const WABA: &str = "102290129340398";
pub const PN: &str = "106540352242922";
pub const TOKEN: &str = "EAAG-merchant-token";
pub const OTHER_TENANT: &str = "merchant-43";
pub const OTHER_WABA: &str = "102290129340399";
pub const OTHER_PN: &str = "106540352242924";
pub const CUSTOMER: &str = "16505551234";
pub const CUSTOMER_BSUID: &str = "US.13491208655302741918";

pub struct World {
    pub h: Harness,
    pub key: String,
    pub clock: ManualClock,
}

/// A service on memory whose clock, key/value store (cooldowns) and
/// pacer are one fake clock, the tenant bound to its number, a key
/// holding the scope `bot`.
pub async fn world_with(bot: BotSettings) -> World {
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let stores = Stores {
        kv: Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone()))),
        ..Stores::memory()
    };
    let settings = Settings {
        bot,
        broadcasts: meta_whatsapp_server_core::broadcasts::BroadcastSettings {
            timer: Some(Arc::new(clock.clone())),
            ..Default::default()
        },
        ..super::test_settings()
    };
    let h = Harness::with_clock(stores, settings, clock.clone(), |graph| Arc::new(graph));
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], "EAAG-other")
        .await;
    let key = h.tenant_key(TENANT, &[Scope::Bot]).await;
    World { h, key, clock }
}

pub async fn world() -> World {
    world_with(BotSettings::default()).await
}

/// Meta's text example, as `from` (a `wa_id`) wrote `body`, message `wamid`.
pub fn inbound_from(from: &str, wamid: &str, body: &str) -> Vec<u8> {
    let mut payload = text(WABA, PN, wamid);
    let value = &mut payload["entry"][0]["changes"][0]["value"];
    value["messages"][0]["text"]["body"] = json!(body);
    value["messages"][0]["from"] = json!(from);
    value["contacts"][0]["wa_id"] = json!(from);
    bytes(&payload)
}

pub fn inbound(wamid: &str, body: &str) -> Vec<u8> {
    inbound_from(CUSTOMER, wamid, body)
}

/// A tapped reply button of id `id`.
pub fn tapped(wamid: &str, id: &str) -> Vec<u8> {
    let mut payload = text(WABA, PN, wamid);
    let message = &mut payload["entry"][0]["changes"][0]["value"]["messages"][0];
    message["type"] = json!("interactive");
    message["text"] = Value::Null;
    message.as_object_mut().unwrap().remove("text");
    message["interactive"] =
        json!({"type": "button_reply", "button_reply": {"id": id, "title": "Menu"}});
    bytes(&payload)
}

/// An image, with no caption.
pub fn image(wamid: &str) -> Vec<u8> {
    let mut payload = text(WABA, PN, wamid);
    let message = &mut payload["entry"][0]["changes"][0]["value"]["messages"][0];
    message["type"] = json!("image");
    message.as_object_mut().unwrap().remove("text");
    message["image"] = json!({"id": "1234", "mime_type": "image/jpeg", "sha256": "abc"});
    bytes(&payload)
}

/// A message `seconds` before the harness's "now" (so it is, say, an
/// outage's backlog), message `wamid` of the customer.
pub fn dated(payload: &[u8], at: i64) -> Vec<u8> {
    let payload: Value = serde_json::from_slice(payload).unwrap();
    bytes(&super::meta::dated(payload, at))
}

pub fn put(key: &str, pn: &str, body: &Value) -> Call {
    Call::new(Method::PUT, format!("/v1/numbers/{pn}/bot"))
        .key(key)
        .json(body)
}

pub fn settings(key: &str, pn: &str) -> Call {
    Call::get(format!("/v1/numbers/{pn}/bot")).key(key)
}

pub fn rules(key: &str, pn: &str) -> Call {
    Call::get(format!("/v1/numbers/{pn}/bot/rules")).key(key)
}

pub fn create(key: &str, pn: &str, body: &Value) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{pn}/bot/rules"))
        .key(key)
        .json(body)
}

pub fn rule_path(pn: &str, id: &str) -> String {
    format!("/v1/numbers/{pn}/bot/rules/{id}")
}

/// A command rule `/name` answering `reply`.
pub fn command(name: &str, reply: &str) -> Value {
    json!({
        "trigger": {"type": "command", "names": [name]},
        "action": {"type": "reply", "text": reply},
    })
}

/// Turn the bot of `pn` on.
pub async fn enable(w: &World) {
    let reply = w
        .h
        .call(put(&w.key, PN, &json!({"enabled": true})))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
}

/// Add `body`; its rule's id.
pub async fn add(w: &World, body: &Value) -> String {
    let reply = w.h.call(create(&w.key, PN, body)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    reply.json()["id"].as_str().unwrap().to_owned()
}

/// Meta's documented answer to a send (`messages/send-messages`).
pub fn accepted(n: usize) -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": format!("+{CUSTOMER}"), "wa_id": CUSTOMER}],
        "messages": [{"id": format!("wamid.BOT{n}")}]
    })
}

pub fn script(h: &Harness, sends: usize) {
    for n in 0..sends {
        h.graph.push_json(200, accepted(n));
    }
}

/// Deliver `body`; `200`.
pub async fn deliver(h: &Harness, body: &[u8]) {
    let reply = h.webhook(body).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
}

/// The JSON of every request Graph got.
pub fn bodies(h: &Harness) -> Vec<Value> {
    h.graph
        .requests()
        .iter()
        .map(|r| r.json().unwrap())
        .collect()
}

/// A time inside the second a message sent now is dated.
pub fn dated_now() -> i64 {
    sent_now()
}

/// `with_ids` re-exported for a test that builds its own payload.
pub fn with(payload: Value) -> Value {
    with_ids(payload, WABA, PN)
}
