//! `WebhookHandler::deliver`: order of checks, dedup (claim, complete,
//! release, leases), sink failures, unparseable bodies.

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use pretty_assertions::assert_eq;
use serde_json::json;
use wa_adapters::store::MemoryKvStore;
use wa_core::Error;
use wa_core::clock::ManualClock;
use wa_core::error::{SinkError, StorageError, WebhookError};
use wa_core::secret::{AppSecret, VerifyToken};
use wa_core::sink::EventSink;
use wa_core::store::{Expiry, KvStore, StoreKey, Versioned};
use wa_webhooks::dedup::store_key;
use wa_webhooks::{
    Claim, ClaimInFlight, DEFAULT_CLAIM_LEASE, DedupGuard, DeliveryReport, SignatureVerifier,
    VerificationQuery, WebhookEvent, WebhookHandler, WebhookPayload, sign,
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

/// The marker for dedup key `key`: `Some(b"pending")`, `Some(b"done")` or
/// `None`.
async fn marker(kv: &MemoryKvStore, key: &str) -> Option<Vec<u8>> {
    kv.get(&store_key(key)).await.unwrap().map(|v| v.value)
}

fn done() -> Vec<u8> {
    b"done".to_vec()
}

fn is_in_flight(err: &Error) -> bool {
    matches!(err, Error::Other(e) if e.downcast_ref::<ClaimInFlight>().is_some())
}

fn clock() -> ManualClock {
    ManualClock::new(time::macros::datetime!(2026-09-24 12:00 UTC))
}

/// Hangs forever on its first delivery (a stuck database, a slow
/// downstream), then records like [`RecordingSink`].
#[derive(Debug, Default)]
struct HangOnceSink {
    inner: RecordingSink,
    hung: AtomicBool,
}

#[async_trait]
impl EventSink<WebhookEvent> for HangOnceSink {
    async fn deliver(&self, event: WebhookEvent) -> Result<(), SinkError> {
        if !self.hung.swap(true, Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.inner.deliver(event).await
    }
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
    assert_eq!(marker(&kv, "wamid.M0").await, Some(done()));
    assert_eq!(marker(&kv, "wamid.M1").await, Some(done()));
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
/// must deliver exactly the events that did not get through: nothing
/// delivered twice, nothing swallowed.
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
    assert_eq!(
        marker(&kv, "wamid.M0").await,
        Some(done()),
        "delivered event is marked done"
    );
    assert_eq!(
        marker(&kv, "wamid.M1").await,
        None,
        "failed event's claim released"
    );
    assert_eq!(
        marker(&kv, "wamid.M2").await,
        None,
        "undelivered event not claimed"
    );

    // Meta redelivers the same body; the sink works again.
    sink.stop_failing();
    let r = h.deliver(Some(&header), &body).await.unwrap();
    assert_eq!(r, report(2, 1, 0));
    assert_eq!(ids(&sink.delivered()), ["wamid.M0", "wamid.M1", "wamid.M2"]);
}

/// The failure the single-marker draft had: the request is dropped while
/// the sink holds the event (Meta timed out, a `tower` timeout fired, the
/// process restarted), so nothing runs to release the claim. The draft kept
/// its marker for 7 days and acknowledged every retry as a duplicate: the
/// event was lost. Now a retry within the lease is refused (Meta tries
/// again later) and a retry after it delivers.
#[tokio::test]
async fn a_request_dropped_mid_delivery_does_not_swallow_the_event() {
    let clock = clock();
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let sink = Arc::new(HangOnceSink::default());
    let h = WebhookHandler::builder(
        SignatureVerifier::new(vec![secret()]).unwrap(),
        VerifyToken::new("vibecoding"),
        sink.clone(),
    )
    .dedup(DedupGuard::new(kv.clone()))
    .build();
    let body = batch(1);
    let header = sign(&secret(), &body);

    let first =
        tokio::time::timeout(Duration::from_millis(50), h.deliver(Some(&header), &body)).await;
    assert!(first.is_err(), "the sink hangs, so the request is dropped");
    assert_eq!(marker(&kv, "wamid.M0").await, Some(b"pending".to_vec()));

    // Meta's immediate retry: not acknowledged.
    let err = h.deliver(Some(&header), &body).await.unwrap_err();
    assert!(is_in_flight(&err), "{err:?}");
    assert!(sink.inner.delivered().is_empty());

    // The next retry, after the lease: delivered, then a duplicate.
    clock.advance(DEFAULT_CLAIM_LEASE + Duration::from_secs(1));
    assert_eq!(
        h.deliver(Some(&header), &body).await.unwrap(),
        report(1, 0, 0)
    );
    assert_eq!(ids(&sink.inner.delivered()), ["wamid.M0"]);
    assert_eq!(
        h.deliver(Some(&header), &body).await.unwrap(),
        report(0, 1, 0)
    );
}

/// Another request is delivering one event of the batch: the events before
/// it are delivered and done, the batch is not acknowledged, and the
/// redelivery finishes the rest without repeating anything.
#[tokio::test]
async fn an_event_in_flight_elsewhere_fails_the_batch_without_losing_or_repeating() {
    let sink = Arc::new(RecordingSink::default());
    let kv = Arc::new(MemoryKvStore::new());
    let h = handler(sink.clone(), Some(kv.clone()));
    let body = batch(3);
    let header = sign(&secret(), &body);

    let other_request = DedupGuard::new(kv.clone());
    let events = WebhookPayload::from_slice(&body).unwrap().into_events();
    let Claim::Acquired(held) = other_request.claim(&events[1]).await.unwrap() else {
        panic!()
    };

    let err = h.deliver(Some(&header), &body).await.unwrap_err();
    assert!(is_in_flight(&err), "{err:?}");
    assert_eq!(ids(&sink.delivered()), ["wamid.M0"]);
    assert_eq!(marker(&kv, "wamid.M0").await, Some(done()));
    assert_eq!(marker(&kv, "wamid.M2").await, None);

    // The other request finishes delivering M1 (to its own sink).
    assert!(other_request.complete(&held).await.unwrap());
    assert_eq!(
        h.deliver(Some(&header), &body).await.unwrap(),
        report(1, 2, 0)
    );
    assert_eq!(ids(&sink.delivered()), ["wamid.M0", "wamid.M2"]);
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
    // A forgery under the empty key: what an unset `META_APP_SECRET` would
    // have made valid.
    let forged = sign(&AppSecret::new(""), body);
    assert!(matches!(
        h.deliver(Some(&forged), body).await,
        Err(Error::Webhook(WebhookError::SignatureMismatch))
    ));
    assert_eq!(sink.calls(), 0, "nothing reaches the sink unverified");
}

#[tokio::test]
async fn body_size_is_checked_first_and_exactly() {
    let sink = Arc::new(RecordingSink::default());
    let body = batch(1);
    let limit = body.len();
    let build = |max: usize| {
        WebhookHandler::builder(
            SignatureVerifier::new(vec![secret()]).unwrap(),
            VerifyToken::new("t"),
            sink.clone(),
        )
        .max_body_bytes(max)
        .build()
    };
    let header = sign(&secret(), &body);

    // One byte over: rejected before the signature is even looked at (a
    // missing or malformed one would otherwise be the error).
    let h = build(limit - 1);
    assert_eq!(h.max_body_bytes(), limit - 1);
    for signature in [Some(header.as_str()), None, Some("sha256=zz")] {
        let err = h.deliver(signature, &body).await.unwrap_err();
        assert!(
            matches!(
                err,
                Error::Webhook(WebhookError::PayloadTooLarge { size, limit: l })
                    if size == limit && l == limit - 1
            ),
            "{err:?}"
        );
    }
    assert_eq!(sink.calls(), 0);

    // Exactly at the limit: accepted.
    assert_eq!(
        build(limit).deliver(Some(&header), &body).await.unwrap(),
        report(1, 0, 0)
    );

    assert_eq!(
        wa_webhooks::DEFAULT_MAX_BODY_BYTES,
        3 * 1024 * 1024,
        "Meta: payloads up to 3 MB, read as MiB (the larger)"
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
async fn done_markers_last_seven_days_and_an_hour() {
    let clock = clock();
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let guard = DedupGuard::new(kv);
    assert_eq!(guard.ttl(), Duration::from_hours(169));
    assert_eq!(guard.lease(), DEFAULT_CLAIM_LEASE);
    let event = common::events("messages/text.json").remove(0);
    let Claim::Acquired(ticket) = guard.claim(&event).await.unwrap() else {
        panic!()
    };
    assert!(guard.complete(&ticket).await.unwrap());
    clock.advance(Duration::from_hours(7 * 24));
    assert_eq!(
        guard.claim(&event).await.unwrap(),
        Claim::Duplicate,
        "still a duplicate on day 7"
    );
    clock.advance(Duration::from_hours(1));
    assert!(
        matches!(guard.claim(&event).await.unwrap(), Claim::Acquired(_)),
        "forgotten after 7 days + 1 h"
    );

    let error = common::events("messages/errors.json").remove(0);
    assert_eq!(guard.claim(&error).await.unwrap(), Claim::Untracked);
    assert_eq!(
        guard.claim(&error).await.unwrap(),
        Claim::Untracked,
        "keyless events are never duplicates"
    );
}

/// Claims are leases, and a ticket only ever acts on its own claim.
#[tokio::test]
async fn stale_tickets_cannot_touch_a_newer_claim() {
    let clock = clock();
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let guard = DedupGuard::new(kv).with_lease(Duration::from_secs(10));
    // Distinct message ids (Meta's examples all reuse one `wamid`).
    let [event, other, third]: [WebhookEvent; 3] = WebhookPayload::from_slice(&batch(3))
        .unwrap()
        .into_events()
        .try_into()
        .unwrap();

    let Claim::Acquired(a) = guard.claim(&event).await.unwrap() else {
        panic!()
    };
    assert_eq!(guard.claim(&event).await.unwrap(), Claim::InFlight);

    // A's request stalls past its lease; a retry takes the event over.
    clock.advance(Duration::from_secs(11));
    let Claim::Acquired(b) = guard.claim(&event).await.unwrap() else {
        panic!("an expired lease is claimable")
    };
    assert!(
        !guard.release(&a).await.unwrap(),
        "A's stale ticket must not delete B's claim"
    );
    assert!(
        !guard.complete(&a).await.unwrap(),
        "nor mark it done under B"
    );
    assert_eq!(guard.claim(&event).await.unwrap(), Claim::InFlight);
    assert!(guard.complete(&b).await.unwrap());
    assert_eq!(guard.claim(&event).await.unwrap(), Claim::Duplicate);
    assert!(!guard.release(&b).await.unwrap(), "done is not a claim");
    assert_eq!(guard.claim(&event).await.unwrap(), Claim::Duplicate);

    // Lease lost but nobody took over: completing still records it.
    let Claim::Acquired(c) = guard.claim(&other).await.unwrap() else {
        panic!()
    };
    clock.advance(Duration::from_secs(11));
    assert!(guard.complete(&c).await.unwrap());
    assert_eq!(guard.claim(&other).await.unwrap(), Claim::Duplicate);

    // A released claim is claimable at once.
    let Claim::Acquired(d) = guard.claim(&third).await.unwrap() else {
        panic!()
    };
    assert!(guard.release(&d).await.unwrap());
    assert!(matches!(
        guard.claim(&third).await.unwrap(),
        Claim::Acquired(_)
    ));
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

#[test]
fn a_blank_verify_token_verifies_nothing() {
    let h = WebhookHandler::builder(
        SignatureVerifier::new(vec![secret()]).unwrap(),
        VerifyToken::new(""),
        Arc::new(RecordingSink::default()),
    )
    .build();
    for token in ["", "anything"] {
        let q: VerificationQuery = serde_json::from_value(json!({
            "hub.mode": "subscribe", "hub.verify_token": token, "hub.challenge": "42"
        }))
        .unwrap();
        assert!(matches!(h.verify(&q), Err(Error::Config(_))), "{token:?}");
    }
}
