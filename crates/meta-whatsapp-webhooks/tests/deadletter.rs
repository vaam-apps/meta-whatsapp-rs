//! `WebhookHandler` with a dead-letter store (roadmap L21a): a permanent
//! sink error dead-letters that one event and delivers the rest; anything
//! else, or a failed dead-letter write, answers non-`200` as before.

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use meta_whatsapp_adapters::store::MemoryKvStore;
use meta_whatsapp_core::Error;
use meta_whatsapp_core::error::{SinkError, StorageError};
use meta_whatsapp_core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_webhooks::deadletter::DEAD_LETTER_NAMESPACE;
use meta_whatsapp_webhooks::dedup::store_key;
use meta_whatsapp_webhooks::{
    DeadLetters, DedupGuard, Replay, SignatureVerifier, WebhookEvent, WebhookHandler, sign,
};
use pretty_assertions::assert_eq;
use serde_json::json;

const SECRET: &str = "0d6f4d2c9b8a7e6f5a4b3c2d1e0f9a8b";

fn secret() -> AppSecret {
    AppSecret::new(SECRET)
}

/// Refuses the message ids in `permanent` permanently and those in
/// `transient` transiently; records the rest.
#[derive(Debug, Default)]
struct Sink {
    delivered: Mutex<Vec<String>>,
    permanent: Mutex<Vec<String>>,
    transient: Mutex<Vec<String>>,
}

impl Sink {
    fn refusing(permanent: &[&str], transient: &[&str]) -> Arc<Self> {
        let sink = Self::default();
        *sink.permanent.lock().unwrap() = permanent.iter().map(|s| (*s).to_owned()).collect();
        *sink.transient.lock().unwrap() = transient.iter().map(|s| (*s).to_owned()).collect();
        Arc::new(sink)
    }

    fn delivered(&self) -> Vec<String> {
        self.delivered.lock().unwrap().clone()
    }

    fn heal(&self) {
        self.permanent.lock().unwrap().clear();
        self.transient.lock().unwrap().clear();
    }
}

fn id_of(event: &WebhookEvent) -> String {
    match event {
        WebhookEvent::MessageReceived { message, .. } => message.id.to_string(),
        other => other.kind().to_owned(),
    }
}

#[async_trait]
impl EventSink<WebhookEvent> for Sink {
    async fn deliver(&self, event: WebhookEvent) -> Result<(), SinkError> {
        let id = id_of(&event);
        if self.permanent.lock().unwrap().contains(&id) {
            return Err(SinkError::permanent(std::io::Error::other(
                "NUL in a Meta-assigned id",
            )));
        }
        if self.transient.lock().unwrap().contains(&id) {
            return Err(SinkError::Closed);
        }
        self.delivered.lock().unwrap().push(id);
        Ok(())
    }
}

fn handler(
    sink: Arc<Sink>,
    kv: &Arc<dyn KvStore>,
    dead_letters: Option<DeadLetters>,
) -> WebhookHandler {
    let builder = WebhookHandler::builder(
        SignatureVerifier::new(vec![secret()]).unwrap(),
        VerifyToken::new("vibecoding"),
        sink,
    )
    .dedup(DedupGuard::new(kv.clone()));
    match dead_letters {
        Some(store) => builder.dead_letters(store).build(),
        None => builder.build(),
    }
}

/// A signed `messages` body with `n` text messages `wamid.M0`…, from a
/// customer with a BSUID.
fn batch(n: usize) -> (Vec<u8>, String) {
    let messages: Vec<_> = (0..n)
        .map(|i| {
            json!({"from": "16505551234", "from_user_id": "US.13491208655302741918",
                   "id": format!("wamid.M{i}"), "timestamp": "1749416383",
                   "type": "text", "text": {"body": format!("message {i}")}})
        })
        .collect();
    let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398", "changes": [{
        "field": "messages",
        "value": {"messaging_product": "whatsapp",
                  "metadata": {"display_phone_number": "15550783881", "phone_number_id": "106540352242922"},
                  "contacts": [{"profile": {"name": "Sheena Nelson"}, "wa_id": "16505551234",
                                "user_id": "US.13491208655302741918"}],
                  "messages": messages}
    }]}]})
    .to_string()
    .into_bytes();
    let header = sign(&secret(), &body);
    (body, header)
}

async fn marker(kv: &Arc<dyn KvStore>, key: &str) -> Option<Vec<u8>> {
    kv.get(&store_key(key)).await.unwrap().map(|v| v.value)
}

/// The decisive test of L21a: one permanently failing event in a batch is
/// dead-lettered and the others delivered; the delivery is acknowledged.
/// Without the dead-letter write the handler answers `500` again, and
/// this fails.
#[tokio::test]
async fn a_batch_with_one_permanent_failure_delivers_the_others_and_dead_letters_it() {
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
    let sink = Sink::refusing(&["wamid.M1"], &[]);
    let store = DeadLetters::new(kv.clone());
    let h = handler(sink.clone(), &kv, Some(store.clone()));
    let (body, header) = batch(3);

    let report = h.deliver(Some(&header), &body).await.unwrap();
    assert_eq!(
        (report.delivered, report.dead_lettered, report.duplicates),
        (2, 1, 0)
    );
    assert_eq!(sink.delivered(), ["wamid.M0", "wamid.M2"]);

    let letters = store.list().await.unwrap();
    assert_eq!(letters.len(), 1, "{letters:?}");
    let letter = &letters[0];
    assert_eq!(letter.kind, "message_received");
    assert_eq!(letter.phone_number_id.as_deref(), Some("106540352242922"));
    assert_eq!(letter.contacts, ["US.13491208655302741918"]);
    assert!(
        letter.error.contains("NUL in a Meta-assigned id"),
        "{}",
        letter.error
    );
    let event: WebhookEvent = serde_json::from_value(letter.event.clone().unwrap()).unwrap();
    assert_eq!(id_of(&event), "wamid.M1");

    // Acknowledged, so marked done like a delivered event: Meta's
    // redelivery of the body is three duplicates, not a second letter.
    assert_eq!(marker(&kv, "wamid.M1").await, Some(b"done".to_vec()));
    let again = h.deliver(Some(&header), &body).await.unwrap();
    assert_eq!((again.duplicates, again.dead_lettered), (3, 0));
    assert_eq!(store.list().await.unwrap().len(), 1);

    // Replay once the sink is fixed: delivered, and the letter is gone.
    sink.heal();
    assert_eq!(
        store.replay(letter.id, sink.as_ref()).await.unwrap(),
        Replay::Delivered
    );
    assert_eq!(sink.delivered(), ["wamid.M0", "wamid.M2", "wamid.M1"]);
    assert!(store.list().await.unwrap().is_empty());
}

/// Without a dead-letter store, a permanent error answers non-`200` as
/// every sink error did before L21a.
#[tokio::test]
async fn without_a_dead_letter_store_a_permanent_error_fails_the_delivery() {
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
    let sink = Sink::refusing(&["wamid.M1"], &[]);
    let h = handler(sink.clone(), &kv, None);
    let (body, header) = batch(3);
    let err = h.deliver(Some(&header), &body).await.unwrap_err();
    assert!(
        matches!(err, Error::Sink(SinkError::Permanent(_))),
        "{err:?}"
    );
    assert_eq!(sink.delivered(), ["wamid.M0"]);
    assert_eq!(marker(&kv, "wamid.M1").await, None, "claim released");
}

/// A transient (or unclassified) error is never dead-lettered: `500`, the
/// claim released, nothing written.
#[tokio::test]
async fn a_transient_error_is_not_dead_lettered() {
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
    let sink = Sink::refusing(&[], &["wamid.M1"]);
    let store = DeadLetters::new(kv.clone());
    let h = handler(sink.clone(), &kv, Some(store.clone()));
    let (body, header) = batch(3);
    let err = h.deliver(Some(&header), &body).await.unwrap_err();
    assert!(matches!(err, Error::Sink(SinkError::Closed)), "{err:?}");
    assert_eq!(sink.delivered(), ["wamid.M0"]);
    assert_eq!(marker(&kv, "wamid.M1").await, None, "claim released");
    assert!(store.list().await.unwrap().is_empty());
}

/// One permanent and one transient failure in a batch: the permanent one
/// is dead-lettered (and done), the transient one fails the delivery;
/// Meta's redelivery then delivers just the transient one.
#[tokio::test]
async fn a_redelivery_after_a_transient_failure_does_not_dead_letter_twice() {
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
    let sink = Sink::refusing(&["wamid.M0"], &["wamid.M1"]);
    let store = DeadLetters::new(kv.clone());
    let h = handler(sink.clone(), &kv, Some(store.clone()));
    let (body, header) = batch(3);
    assert!(h.deliver(Some(&header), &body).await.is_err());
    assert_eq!(store.list().await.unwrap().len(), 1);

    sink.transient.lock().unwrap().clear();
    let report = h.deliver(Some(&header), &body).await.unwrap();
    assert_eq!(
        (report.delivered, report.duplicates, report.dead_lettered),
        (2, 1, 0)
    );
    assert_eq!(sink.delivered(), ["wamid.M1", "wamid.M2"]);
    assert_eq!(store.list().await.unwrap().len(), 1);
}

/// A `KvStore` that refuses every write to the dead-letter namespace.
#[derive(Debug)]
struct NoDeadLetters(MemoryKvStore);

fn refused(key: &StoreKey) -> Result<(), StorageError> {
    if key.namespace().starts_with(DEAD_LETTER_NAMESPACE) {
        return Err(StorageError::Busy);
    }
    Ok(())
}

#[async_trait]
impl KvStore for NoDeadLetters {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        self.0.get(key).await
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        refused(key)?;
        self.0.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        refused(key)?;
        self.0.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        refused(key)?;
        self.0.compare_and_swap(key, expected, new, expiry).await
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        refused(key)?;
        self.0.delete(key).await
    }
}

/// The dead-letter write comes before the acknowledgement: when it fails,
/// the delivery fails (`500`) and the event's claim is released, so Meta
/// redelivers it. Never lost silently.
#[tokio::test]
async fn a_failed_dead_letter_write_fails_the_delivery() {
    let kv: Arc<dyn KvStore> = Arc::new(NoDeadLetters(MemoryKvStore::new()));
    let sink = Sink::refusing(&["wamid.M1"], &[]);
    let h = handler(sink.clone(), &kv, Some(DeadLetters::new(kv.clone())));
    let (body, header) = batch(3);
    let err = h.deliver(Some(&header), &body).await.unwrap_err();
    assert!(matches!(err, Error::Storage(_)), "{err:?}");
    assert_eq!(sink.delivered(), ["wamid.M0"]);
    assert_eq!(marker(&kv, "wamid.M1").await, None, "claim released");
    assert_eq!(marker(&kv, "wamid.M2").await, None, "never claimed");
}

/// Replay reads the event back from its serialized form: every event of
/// every fixture must come back equal, or its letter could not be
/// replayed.
#[test]
fn every_fixture_event_reads_back_from_its_dead_letter_form() {
    let mut checked = 0;
    for name in common::all_fixtures() {
        let Ok(payload) =
            meta_whatsapp_webhooks::WebhookPayload::from_slice(&common::fixture_bytes(&name))
        else {
            continue;
        };
        for event in payload.into_events() {
            let json = serde_json::to_value(&event).unwrap();
            let back: WebhookEvent = serde_json::from_value(json)
                .unwrap_or_else(|e| panic!("{name} ({}): {e}", event.kind()));
            assert_eq!(back, event, "{name}");
            checked += 1;
        }
    }
    assert!(checked > 100, "only {checked} events");
}

/// A signed body that is not a webhook envelope, refused permanently by
/// the sink, is dead-lettered too.
#[tokio::test]
async fn an_unparsed_body_refused_permanently_is_dead_lettered() {
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
    let sink = Sink::refusing(&["unparsed"], &[]);
    let store = DeadLetters::new(kv.clone());
    let h = handler(sink, &kv, Some(store.clone()));
    let body = br#"{"object": "whatsapp_business_account", "entry": "not a list"}"#;
    let report = h.deliver(Some(&sign(&secret(), body)), body).await.unwrap();
    assert_eq!(
        (report.unparsed, report.delivered, report.dead_lettered),
        (1, 0, 1)
    );
    assert_eq!(store.list().await.unwrap()[0].kind, "unparsed");
}
