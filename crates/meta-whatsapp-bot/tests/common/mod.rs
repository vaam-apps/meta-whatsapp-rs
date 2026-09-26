//! Shared by the integration tests: Meta's documented webhook fixtures
//! (from `meta-whatsapp-webhooks/tests/fixtures`), a recording `Outbound`
//! and a client on a `ScriptedTransport`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use meta_whatsapp_bot::Outbound;
use meta_whatsapp_client::messages::{OutboundMessage, SendResponse};
use meta_whatsapp_client::{Client, RetryPolicy};
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_core::testing::ScriptedTransport;
use meta_whatsapp_webhooks::{WebhookEvent, WebhookPayload};
use serde_json::{Value, json};

/// The business number of every fixture used here.
pub const NUMBER: &str = "106540352242922";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../meta-whatsapp-webhooks/tests/fixtures")
}

/// A fixture's JSON, e.g. `messages/text.json`.
pub fn fixture_json(path: &str) -> Value {
    let text =
        std::fs::read_to_string(fixtures().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

/// The single event of a fixture's payload.
pub fn events_of(json: &Value) -> Vec<WebhookEvent> {
    WebhookPayload::from_slice(json.to_string().as_bytes())
        .unwrap()
        .into_events()
}

/// The one event of a fixture.
pub fn event(path: &str) -> WebhookEvent {
    let mut events = events_of(&fixture_json(path));
    assert_eq!(events.len(), 1, "{path}");
    events.remove(0)
}

/// A text-message fixture with its body replaced (the envelope, contacts
/// and ids stay Meta's).
pub fn text_event(path: &str, body: &str) -> WebhookEvent {
    let mut json = fixture_json(path);
    let text = &mut json["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"];
    assert!(text.is_string(), "{path} is not a text message");
    *text = json!(body);
    let mut events = events_of(&json);
    assert_eq!(events.len(), 1, "{path}");
    events.remove(0)
}

/// The id of the first message of a fixture.
pub fn message_id(path: &str) -> String {
    fixture_json(path)["entry"][0]["changes"][0]["value"]["messages"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Everything a bot sent or marked read.
#[derive(Debug, Clone, Default)]
pub struct Recording {
    pub sent: Arc<Mutex<Vec<(PhoneNumberId, Value)>>>,
    pub reads: Arc<Mutex<Vec<(PhoneNumberId, MessageId, bool)>>>,
}

impl Recording {
    pub fn sent(&self) -> Vec<Value> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|(_, v)| v.clone())
            .collect()
    }

    pub fn bodies(&self) -> Vec<String> {
        self.sent()
            .iter()
            .map(|v| v["text"]["body"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

#[async_trait]
impl Outbound for Recording {
    async fn send(
        &self,
        from: &PhoneNumberId,
        message: &OutboundMessage,
    ) -> meta_whatsapp_core::Result<SendResponse> {
        // The same checks the client makes before a request.
        message.validate()?;
        self.sent
            .lock()
            .unwrap()
            .push((from.clone(), serde_json::to_value(message).unwrap()));
        Ok(serde_json::from_value(json!({
            "messaging_product": "whatsapp",
            "contacts": [],
            "messages": [{"id": "wamid.sent"}]
        }))
        .unwrap())
    }

    async fn mark_read(
        &self,
        from: &PhoneNumberId,
        message_id: &MessageId,
        typing_indicator: bool,
    ) -> meta_whatsapp_core::Result<()> {
        if message_id.as_str().is_empty() {
            return Err(ValidationError::new("message_id", "empty").into());
        }
        self.reads
            .lock()
            .unwrap()
            .push((from.clone(), message_id.clone(), typing_indicator));
        Ok(())
    }
}

/// A client whose requests go to `t`, with token `TOKEN` and no retries.
pub fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

/// Meta's example response of a send (`messages/text-messages`).
pub fn send_response() -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": "+16505551234", "wa_id": "16505551234"}],
        "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
    })
}
