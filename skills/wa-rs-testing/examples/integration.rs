//! Reference code for the `wa-rs-testing` skill: testing code that uses
//! wa-rs without a network or a database — scripted Graph answers, the
//! memory stores, a manual clock, and signed webhook fixtures.
//!
//! wa-rs compiles this file and runs its tests in its own gate
//! (`crates/wa-rs/tests/skills.rs`). In your crate, `ScriptedTransport`
//! needs wa-core's `testing` feature as a dev-dependency (see the skill).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use time::OffsetDateTime;
use wa_rs::adapters::sink::channel;
use wa_rs::adapters::store::{MemoryConversationStore, MemoryKvStore};
use wa_rs::client::messages::Text;
use wa_rs::core::clock::ManualClock;
use wa_rs::core::error::TransportError;
use wa_rs::core::testing::ScriptedTransport;
use wa_rs::prelude::*;

/// A client whose requests go to `transport` instead of Meta.
pub fn scripted_client(transport: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(transport.clone())
        .access_token("TEST-TOKEN")
        .retry(RetryPolicy::NONE) // one request per call: counts stay exact
        .build()
        .expect("a transport is set")
}

/// A text message webhook from a BSUID-only customer (no `wa_id`, as
/// Meta sends them since 2026), shaped like Meta's documented example.
pub fn text_webhook(phone_number_id: &str, user_id: &str, text: &str, at: i64) -> Value {
    json!({
        "object": "whatsapp_business_account",
        "entry": [{"id": "102290129340398", "changes": [{"field": "messages", "value": {
            "messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": phone_number_id},
            "contacts": [{"profile": {"name": "Sheena Nelson"}, "user_id": user_id}],
            "messages": [{
                "from_user_id": user_id,
                "id": format!("wamid.TEST-{at}"),
                "timestamp": at.to_string(),
                "type": "text",
                "text": {"body": text}
            }]
        }}]}]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NUMBER: &str = "106540352242922";
    const CUSTOMER: &str = "US.13491208655302741918";
    const SECRET: &str = "test-app-secret";

    #[tokio::test]
    async fn assert_the_exact_request() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"messages": [{"id": "wamid.1"}]}));
        let client = scripted_client(&transport);

        let to = Recipient::user(CUSTOMER);
        client
            .messages(NUMBER)
            .send(&OutboundMessage::text(to, "Your order has shipped."))
            .await
            .unwrap();

        let request = transport.last_request().unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path(), "/v25.0/106540352242922/messages");
        assert_eq!(request.bearer(), Some("TEST-TOKEN"));
        assert_eq!(
            request.json().unwrap(),
            json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "recipient": CUSTOMER,
                "type": "text",
                "text": {"body": "Your order has shipped."}
            })
        );
        assert_eq!(transport.remaining(), 0); // every scripted answer was used
    }

    #[tokio::test]
    async fn script_failures() {
        let transport = ScriptedTransport::new();
        transport.push_error(|| TransportError::Timeout);
        transport.push_json(
            400,
            json!({"error": {"message": "(#131047) Re-engagement message", "code": 131047}}),
        );
        let messages = scripted_client(&transport).messages(NUMBER);
        let text = OutboundMessage::text(Recipient::user(CUSTOMER), "Hi");

        let timeout = messages.send(&text).await.unwrap_err();
        assert!(matches!(timeout, Error::Transport(TransportError::Timeout)));
        let closed = messages.send(&text).await.unwrap_err();
        assert_eq!(closed.kind(), ErrorKind::CustomerServiceWindowClosed);
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn deliver_a_signed_webhook() {
        let (sink, mut events) = channel::<WebhookEvent>(16);
        let kv = Arc::new(MemoryKvStore::new());
        let handler = WebhookHandler::builder(
            SignatureVerifier::new(vec![AppSecret::new(SECRET)]).unwrap(),
            VerifyToken::new("test-verify-token"),
            Arc::new(sink),
        )
        .dedup(DedupGuard::new(kv))
        .build();

        let body = text_webhook(NUMBER, CUSTOMER, "Does it come in navy?", 1_749_416_383);
        let body = serde_json::to_vec(&body).unwrap();
        let signature = wa_rs::webhooks::sign(&AppSecret::new(SECRET), &body);

        let first = handler.deliver(Some(&signature), &body).await.unwrap();
        assert_eq!(first.delivered, 1);
        let retry = handler.deliver(Some(&signature), &body).await.unwrap();
        assert_eq!(retry.duplicates, 1); // Meta's retry is recorded once

        let Some(WebhookEvent::MessageReceived { message, .. }) = events.recv().await else {
            panic!("expected a message");
        };
        assert_eq!(
            message.from_user_id.as_ref().map(UserId::as_str),
            Some(CUSTOMER)
        );
        assert!(handler.deliver(None, &body).await.is_err()); // unsigned: answer 401
    }

    #[tokio::test]
    async fn pin_the_clock_for_the_24_hour_window() {
        let store = Arc::new(MemoryConversationStore::new());
        let sent_at = OffsetDateTime::from_unix_timestamp(1_749_416_383).unwrap();
        let event = WebhookEvent::MessageReceived {
            waba_id: "102290129340398".into(),
            phone_number_id: NUMBER.into(),
            display_phone_number: "15550783881".into(),
            contact: None,
            message: Box::new(
                serde_json::from_value(json!({
                    "from_user_id": CUSTOMER, "id": "wamid.IN1",
                    "timestamp": "1749416383", "type": "text", "text": {"body": "Hi"}
                }))
                .unwrap(),
            ),
        };
        InboxSink::new(store.clone()).deliver(event).await.unwrap();

        let transport = ScriptedTransport::new(); // nothing scripted: no request may leave
        let clock = Arc::new(ManualClock::new(sent_at + Duration::from_hours(25)));
        let inbox = Inbox::new(scripted_client(&transport), NUMBER, store).with_clock(clock);
        let key = inbox.key(CUSTOMER);

        let refused = inbox
            .reply(&key, Text::new("Still there?").into())
            .await
            .unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::CustomerServiceWindowClosed);
        assert!(transport.requests().is_empty()); // refused locally
    }
}
