//! Reference code for the `meta-whatsapp-rs-live-updates` skill: where webhook events
//! go after the endpoint accepts them — a durable sink of your own, a
//! broadcast for live views (SSE), a channel to a worker — and what each
//! costs.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use async_trait::async_trait;
use meta_whatsapp_rs::adapters::sink::{BroadcastSink, ChannelMode, FanoutSink, channel};
use meta_whatsapp_rs::core::error::SinkError;
use meta_whatsapp_rs::prelude::*;
use tokio::sync::{broadcast, mpsc};

/// Your durable sink: whatever it returns decides the webhook's answer
/// (an error → 500 → Meta redelivers the batch), so make it idempotent.
#[derive(Debug, Default)]
pub struct Deliveries;

#[async_trait]
impl EventSink<WebhookEvent> for Deliveries {
    async fn deliver(&self, event: WebhookEvent) -> Result<(), SinkError> {
        if let WebhookEvent::StatusUpdated { status, .. } = &event
            && let Some(tag) = status.biz_opaque_callback_data.as_deref()
        {
            // An upsert keyed by `tag` and `status.id`: a redelivery is harmless.
            tracing::debug!(tag, status = status.status.as_str(), "delivery update");
        }
        Ok(())
    }
}

/// Fan out: durable first, live view, slow work queued.
pub fn sinks(
    live: broadcast::Sender<WebhookEvent>,
) -> (FanoutSink<WebhookEvent>, mpsc::Receiver<WebhookEvent>) {
    let (worker, jobs) = channel::<WebhookEvent>(1024);
    let sink = FanoutSink::new()
        .with(Deliveries) // durable: its error → 500 → Meta retries
        .with(BroadcastSink::from_sender(live)) // live views: never fails, drops when nobody listens
        .with(worker.with_mode(ChannelMode::TryOrFail)); // full queue → 500 instead of waiting
    (sink, jobs)
}

/// The worker: `Ok` from the channel sink meant "queued", so a crash
/// loses what is queued here. Only put work you can lose or rebuild here.
pub async fn worker(mut jobs: mpsc::Receiver<WebhookEvent>) {
    while let Some(event) = jobs.recv().await {
        tracing::info!(kind = event.kind(), "background job"); // kind only: no customer data in logs
    }
}

/// Live events of one business number as Server-Sent Events (feature
/// `axum`). Behind YOUR auth, after checking the caller owns the number.
pub fn live_view(
    live: &broadcast::Sender<WebhookEvent>,
    owned_number: PhoneNumberId,
) -> impl meta_whatsapp_rs::webhooks::axum::response::IntoResponse {
    // An allow-list: `Unknown` and `Unparsed` events carry no number and may
    // belong to any merchant, so `is_none_or(…)` would leak them.
    let only_this_number = move |e: &WebhookEvent| e.phone_number_id() == Some(&owned_number);
    meta_whatsapp_rs::webhooks::sse(live.subscribe(), only_this_number) // `event: whatsapp`; `event: lagged` → reload
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn text_event(number: &str) -> WebhookEvent {
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": "messages", "value": {"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": number},
                "messages": [{"from_user_id": "US.1", "id": "wamid.1", "timestamp": "1749416383",
                    "type": "text", "text": {"body": "Hi"}}]}}]}]});
        meta_whatsapp_rs::webhooks::WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events()
            .remove(0)
    }

    #[tokio::test]
    async fn every_sink_sees_the_event() {
        let (live, mut watching) = broadcast::channel(16);
        let (sink, mut jobs) = sinks(live);
        sink.deliver(text_event("106540352242922")).await.unwrap();
        assert_eq!(watching.recv().await.unwrap().kind(), "message_received");
        assert_eq!(jobs.recv().await.unwrap().kind(), "message_received");
    }

    #[tokio::test]
    async fn a_full_queue_fails_the_delivery() {
        let (worker, _jobs) = channel::<WebhookEvent>(1);
        let worker = worker.with_mode(ChannelMode::TryOrFail);
        worker.deliver(text_event("1")).await.unwrap();
        assert!(worker.deliver(text_event("1")).await.is_err()); // → 500, Meta retries later
    }

    #[tokio::test]
    async fn nobody_watching_is_not_an_error() {
        let (live, watching) = broadcast::channel::<WebhookEvent>(4);
        drop(watching);
        let sink = BroadcastSink::from_sender(live);
        assert!(sink.deliver(text_event("1")).await.is_ok());
    }
}
