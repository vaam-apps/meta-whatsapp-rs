//! `WebhookHandler::deliver`: order of checks, dedup, sink failures,
//! unparseable bodies.

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pretty_assertions::assert_eq;
use serde_json::json;
use wa_adapters::store::MemoryKvStore;
use wa_core::Error;
use wa_core::clock::ManualClock;
use wa_core::error::{StorageError, WebhookError};
use wa_core::secret::{AppSecret, VerifyToken};
use wa_core::store::{Expiry, KvStore, StoreKey, Versioned};
use wa_webhooks::{
    DEDUP_NAMESPACE, DedupGuard, DeliveryReport, SignatureVerifier, VerificationQuery,
    WebhookEvent, WebhookHandler, sign,
};

use common::RecordingSink;

const SECRET: &str = "0d6f4d2c9b8a7e6f5a4b3c2d1e0f9a8b";

fn secret() -> AppSecret {
    AppSecret::new(SECRET)
}

fn handler(sink: Arc<RecordingSink>, kv: Option<Arc<dyn KvStore>>) -> WebhookHandler {
    let builder = WebhookHandler::builder(
        SignatureVerifier::new(vec![secret()]).unwrap(),
        VerifyToken::new("vibecoding"),
        sink,
    );
    match kv {
        Some(kv) => builder.dedup(DedupGuard::new(kv)).build(),
        None => builder.build(),
    }
}

fn report(delivered: usize, duplicates: usize, unparsed: usize) -> DeliveryReport {
    let mut r = DeliveryReport::default();
    r.delivered = delivered;
    r.duplicates = duplicates;
    r.unparsed = unparsed;
    r
}

/// A signed `messages` body with `n` text messages `wamid.M0`…
fn batch(n: usize) -> Vec<u8> {
    let messages: Vec<_> = (0..n)
        .map(|i| {
            json!({"from": "16505551234", "id": format!("wamid.M{i}"), "timestamp": "1749416383",
                   "type": "text", "text": {"body": format!("message {i}")}})
        })
        .collect();
    json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398", "changes": [{
        "field": "messages",
        "value": {"messaging_product": "whatsapp",
                  "metadata": {"display_phone_number": "15550783881", "phone_number_id": "106540352242922"},
                  "contacts": [{"profile": {"name": "Sheena Nelson"}, "wa_id": "16505551234"}],
                  "messages": messages}
    }]}]})
    .to_string()
    .into_bytes()
}

fn ids(events: &[WebhookEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            WebhookEvent::MessageReceived { message, .. } => message.id.to_string(),
            other => panic!("{other:?}"),
        })
        .collect()
}

async fn marker(kv: &MemoryKvStore, key: &str) -> bool {
    kv.get(&StoreKey::new(DEDUP_NAMESPACE, key))
        .await
        .unwrap()
        .is_some()
}

#[tokio::test]
async fn delivers_every_event_of_a_signed_fixture() {
    let sink = Arc::new(RecordingSink::default());
    let h = handler(sink.clone(), None);
    let body = common::fixture_bytes("messages/group_statuses_aggregated.json");
    let r = h
        .deliver(Some(&sign(&secret(), &body)), &body)
        .await
        .unwrap();
    assert_eq!(r, report(3, 0, 0));
    assert_eq!(sink.delivered().len(), 3);
}

#[tokio::test]
async fn duplicates_are_skipped_with_a_dedup_guard() {
    let sink = Arc::new(RecordingSink::default());
    let kv = Arc::new(MemoryKvStore::new());
    let h = handler(sink.clone(), Some(kv.clone()));
    let body = batch(2);
    let header = sign(&secret(), &body);
    assert_eq!(
        h.deliver(Some(&header), &body).await.unwrap(),
        report(2, 0, 0)
    );
    assert_eq!(
        h.deliver(Some(&header), &body).await.unwrap(),
        report(0, 2, 0)
    );
    assert_eq!(ids(&sink.delivered()), ["wamid.M0", "wamid.M1"]);
    assert!(marker(&kv, "wamid.M0").await);
}

#[tokio::test]
async fn without_a_dedup_guard_retries_are_delivered_again() {
    let sink = Arc::new(RecordingSink::default());
    let h = handler(sink.clone(), None);
    let body = batch(1);
    let header = sign(&secret(), &body);
    h.deliver(Some(&header), &body).await.unwrap();
    h.deliver(Some(&header), &body).await.unwrap();
    assert_eq!(sink.delivered().len(), 2);
}

/// The scenario Meta's retries depend on: the sink fails part-way through a
/// batch; the delivery must fail (so Meta redelivers), and the redelivery
/// must deliver exactly the events that did not get through.
#[tokio::test]
async fn sink_failure_releases_claims_so_the_retry_is_not_swallowed() {
    let sink = Arc::new(RecordingSink::failing_on(1)); // 2nd event fails
    let kv = Arc::new(MemoryKvStore::new());
    let h = handler(sink.clone(), Some(kv.clone()));
    let body = batch(3);
    let header = sign(&secret(), &body);

    let err = h.deliver(Some(&header), &body).await.unwrap_err();
    assert!(matches!(err, Error::Sink(_)), "{err:?}");
    assert_eq!(ids(&sink.delivered()), ["wamid.M0"]);
    assert!(
        marker(&kv, "wamid.M0").await,
        "delivered event keeps its claim"
    );
    assert!(
        !marker(&kv, "wamid.M1").await,
        "failed event's claim released"
    );
    assert!(
        !marker(&kv, "wamid.M2").await,
        "undelivered event not claimed"
    );

    // Meta redelivers the same body; the sink works again.
    sink.stop_failing();
    let r = h.deliver(Some(&header), &body).await.unwrap();
    assert_eq!(r, report(2, 1, 0));
    assert_eq!(ids(&sink.delivered()), ["wamid.M0", "wamid.M1", "wamid.M2"]);
}

#[tokio::test]
async fn signed_body_that_does_not_parse_is_acknowledged_as_unparsed() {
    let sink = Arc::new(RecordingSink::default());
    let kv = Arc::new(MemoryKvStore::new());
    let h = handler(sink.clone(), Some(kv));

    let body = b"definitely not json \xff";
    let r = h.deliver(Some(&sign(&secret(), body)), body).await.unwrap();
    assert_eq!(r, report(1, 0, 1));

    let body = br#"{"hello": "world"}"#;
    let r = h.deliver(Some(&sign(&secret(), body)), body).await.unwrap();
    assert_eq!(r, report(1, 0, 1));

    let delivered = sink.delivered();
    let WebhookEvent::Unparsed { raw, error } = &delivered[0] else {
        panic!("{delivered:?}")
    };
    assert_eq!(raw, &json!("definitely not json \u{fffd}"));
    assert!(!error.is_empty());
    let WebhookEvent::Unparsed { raw, .. } = &delivered[1] else {
        panic!("{delivered:?}")
    };
    assert_eq!(
        raw,
        &json!({"hello": "world"}),
        "JSON bodies are kept as JSON"
    );
    assert_eq!(delivered[1].dedup_key(), None);
}

#[tokio::test]
async fn unparsed_delivery_still_fails_when_the_sink_fails() {
    let sink = Arc::new(RecordingSink::failing_on(0));
    let h = handler(sink.clone(), None);
    let body = b"[]";
    let err = h
        .deliver(Some(&sign(&secret(), body)), body)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Sink(_)));
}

#[tokio::test]
async fn signature_is_checked_before_anything_is_parsed() {
    let sink = Arc::new(RecordingSink::default());
    let h = handler(sink.clone(), None);
    let body = b"not json";
    let bad = sign(&AppSecret::new("wrong"), body);
    assert!(matches!(
        h.deliver(Some(&bad), body).await,
        Err(Error::Webhook(WebhookError::SignatureMismatch))
    ));
    assert!(matches!(
        h.deliver(None, body).await,
        Err(Error::Webhook(WebhookError::MissingSignature))
    ));
    assert!(matches!(
        h.deliver(Some("sha256=zz"), body).await,
        Err(Error::Webhook(WebhookError::MalformedSignature))
    ));
    assert_eq!(sink.calls(), 0, "nothing reaches the sink unverified");
}

#[tokio::test]
async fn body_size_is_checked_first() {
    let sink = Arc::new(RecordingSink::default());
    let h = WebhookHandler::builder(
        SignatureVerifier::new(vec![secret()]).unwrap(),
        VerifyToken::new("t"),
        sink.clone(),
    )
    .max_body_bytes(16)
    .build();
    assert_eq!(h.max_body_bytes(), 16);
    let body = batch(1);
    let header = sign(&secret(), &body);
    for signature in [Some(header.as_str()), None] {
        let err = h.deliver(signature, &body).await.unwrap_err();
        let Error::Validation(v) = &err else {
            panic!("{err:?}")
        };
        assert_eq!(v.field, "body");
    }
    assert!(
        h.deliver(Some(&sign(&secret(), b"{}")), b"{}")
            .await
            .is_ok()
    );
    assert_eq!(
        wa_webhooks::DEFAULT_MAX_BODY_BYTES,
        3 * 1024 * 1024,
        "Meta: payloads up to 3 MB"
    );
}

#[derive(Debug)]
struct BrokenKv;

#[async_trait]
impl KvStore for BrokenKv {
    async fn get(&self, _: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        Err(down())
    }
    async fn put(&self, _: &StoreKey, _: Vec<u8>, _: Expiry) -> Result<u64, StorageError> {
        Err(down())
    }
    async fn put_if_absent(
        &self,
        _: &StoreKey,
        _: Vec<u8>,
        _: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        Err(down())
    }
    async fn compare_and_swap(
        &self,
        _: &StoreKey,
        _: u64,
        _: Option<Vec<u8>>,
        _: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        Err(down())
    }
    async fn delete(&self, _: &StoreKey) -> Result<bool, StorageError> {
        Err(down())
    }
}

/// Any storage failure will do; `Corrupt` avoids an `anyhow` dev-dependency.
fn down() -> StorageError {
    StorageError::Corrupt {
        key: "down".into(),
        source: serde_json::from_str::<()>("down").unwrap_err(),
    }
}

#[tokio::test]
async fn dedup_store_failure_fails_the_delivery_before_the_sink() {
    let sink = Arc::new(RecordingSink::default());
    let h = handler(sink.clone(), Some(Arc::new(BrokenKv)));
    let body = batch(1);
    let err = h
        .deliver(Some(&sign(&secret(), &body)), &body)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Storage(_)), "{err:?}");
    assert_eq!(sink.calls(), 0);
}

#[tokio::test]
async fn dedup_markers_last_seven_days_and_an_hour() {
    let clock = ManualClock::new(time::macros::datetime!(2026-09-24 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let guard = DedupGuard::new(kv);
    assert_eq!(guard.ttl(), Duration::from_hours(169));
    let event = common::events("messages/text.json").remove(0);
    assert!(guard.claim(&event).await.unwrap());
    clock.advance(Duration::from_hours(7 * 24));
    assert!(
        !guard.claim(&event).await.unwrap(),
        "still a duplicate on day 7"
    );
    clock.advance(Duration::from_hours(1));
    assert!(
        guard.claim(&event).await.unwrap(),
        "forgotten after 7 days + 1 h"
    );
    assert!(guard.release(&event).await.unwrap());
    assert!(
        guard.claim(&event).await.unwrap(),
        "claimable again after release"
    );

    let error = common::events("messages/errors.json").remove(0);
    assert!(guard.claim(&error).await.unwrap());
    assert!(
        guard.claim(&error).await.unwrap(),
        "keyless events are never duplicates"
    );
    assert!(!guard.release(&error).await.unwrap());
}

#[test]
fn verify_uses_the_configured_token() {
    let h = handler(Arc::new(RecordingSink::default()), None);
    let q: VerificationQuery = serde_json::from_value(json!({
        "hub.mode": "subscribe", "hub.verify_token": "vibecoding", "hub.challenge": "42"
    }))
    .unwrap();
    assert_eq!(h.verify(&q).unwrap(), "42");
    assert!(!format!("{h:?}").contains("vibecoding"));
    assert!(!format!("{h:?}").contains(SECRET));
}
