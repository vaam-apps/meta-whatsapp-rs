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
use meta_whatsapp_core::ids::PhoneNumberId;
use meta_whatsapp_core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_webhooks::deadletter::{
    DEAD_LETTER_INDEX_NAMESPACE, DEAD_LETTER_NAMESPACE, slot_key,
};
use meta_whatsapp_webhooks::dedup::store_key;
use meta_whatsapp_webhooks::{
    DeadLetters, DedupGuard, Replay, SignatureVerifier, WebhookEvent, WebhookHandler, sign,
};
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::oneshot;

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
    assert_eq!(letter.contacts, ["16505551234", "US.13491208655302741918"]);
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

/// A `KvStore` that refuses every write to the namespaces `refuses` picks.
#[derive(Debug)]
struct NoDeadLetters(MemoryKvStore, fn(&str) -> bool);

impl NoDeadLetters {
    /// Refuses the ring, its head and the index.
    fn at_all() -> Self {
        Self(MemoryKvStore::new(), |ns| {
            ns.starts_with(DEAD_LETTER_NAMESPACE)
        })
    }

    /// Takes the letter, refuses its index.
    fn unindexed() -> Self {
        Self(MemoryKvStore::new(), |ns| ns == DEAD_LETTER_INDEX_NAMESPACE)
    }

    fn refused(&self, key: &StoreKey) -> Result<(), StorageError> {
        if (self.1)(key.namespace()) {
            return Err(StorageError::Busy);
        }
        Ok(())
    }
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
        self.refused(key)?;
        self.0.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.refused(key)?;
        self.0.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.refused(key)?;
        self.0.compare_and_swap(key, expected, new, expiry).await
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.refused(key)?;
        self.0.delete(key).await
    }
}

/// The dead-letter write comes before the acknowledgement: when it fails,
/// the delivery fails (`500`) and the event's claim is released, so Meta
/// redelivers it. Never lost silently.
#[tokio::test]
async fn a_failed_dead_letter_write_fails_the_delivery() {
    let kv: Arc<dyn KvStore> = Arc::new(NoDeadLetters::at_all());
    let sink = Sink::refusing(&["wamid.M1"], &[]);
    let h = handler(sink.clone(), &kv, Some(DeadLetters::new(kv.clone())));
    let (body, header) = batch(3);
    let err = h.deliver(Some(&header), &body).await.unwrap_err();
    assert!(matches!(err, Error::Storage(_)), "{err:?}");
    assert_eq!(sink.delivered(), ["wamid.M0"]);
    assert_eq!(marker(&kv, "wamid.M1").await, None, "claim released");
    assert_eq!(marker(&kv, "wamid.M2").await, None, "never claimed");
}

/// A letter written but not indexed could not be erased: the store takes
/// it back and the delivery fails (`500`, the claim released), so Meta
/// redelivers rather than the handler acknowledging an event kept where
/// no erasure reaches it.
#[tokio::test]
async fn a_letter_that_cannot_be_indexed_is_taken_back_and_fails_the_delivery() {
    let kv: Arc<dyn KvStore> = Arc::new(NoDeadLetters::unindexed());
    let sink = Sink::refusing(&["wamid.M1"], &[]);
    let store = DeadLetters::new(kv.clone());
    let h = handler(sink.clone(), &kv, Some(store.clone()));
    let (body, header) = batch(3);
    let err = h.deliver(Some(&header), &body).await.unwrap_err();
    assert!(matches!(err, Error::Storage(_)), "{err:?}");
    assert_eq!(sink.delivered(), ["wamid.M0"]);
    assert_eq!(marker(&kv, "wamid.M1").await, None, "claim released");
    assert_eq!(marker(&kv, "wamid.M2").await, None, "never claimed");
    assert!(kv.get(&slot_key(0)).await.unwrap().is_none(), "taken back");
    assert!(store.list().await.unwrap().is_empty());
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

/// Whether `s` has a BSUID's shape (`business-scoped-user-ids`: an ISO
/// country code, a period, `ENT.` for a parent BSUID, then up to 128
/// alphanumerics), whatever field it sits in.
fn looks_like_bsuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 4 || !b[0].is_ascii_uppercase() || !b[1].is_ascii_uppercase() || b[2] != b'.' {
        return false;
    }
    let rest = s[3..].strip_prefix("ENT.").unwrap_or(&s[3..]);
    !rest.is_empty() && rest.len() <= 128 && rest.bytes().all(|c| c.is_ascii_alphanumeric())
}

/// A phone number as Meta prints one (`wa_id`, `from`, …), without `+`.
fn as_phone(s: &str) -> Option<String> {
    let digits = s.strip_prefix('+').unwrap_or(s);
    (digits.len() >= 6 && digits.len() <= 15 && digits.bytes().all(|c| c.is_ascii_digit()))
        .then(|| digits.to_owned())
}

/// The fields of Meta's payloads that carry a person's phone number
/// (`webhooks/reference/*`, `business-scoped-user-ids`, `groups/webhooks`,
/// `smb_app_state_sync`), as the oracle of the next test.
const PHONE_FIELDS: [&str; 8] = [
    "wa_id",
    "from",
    "to",
    "recipient_id",
    "recipient_participant_id",
    "participant_recipient_id",
    "phone_number",
    "input",
];

/// Every person `value` names: each BSUID-shaped string anywhere, and the
/// phone number in each person field; the business's own numbers left out.
fn people(value: &serde_json::Value, found: &mut Vec<String>, business: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, v) in map {
                if let serde_json::Value::String(s) = v {
                    if key == "display_phone_number" {
                        business.push(s.chars().filter(char::is_ascii_digit).collect());
                    } else if looks_like_bsuid(s) {
                        found.push(s.clone());
                    } else if (PHONE_FIELDS.contains(&key.as_str()) || key.ends_with("wa_id"))
                        && let Some(phone) = as_phone(s)
                    {
                        found.push(phone);
                    }
                }
                people(v, found, business);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                people(item, found, business);
            }
        }
        _ => {}
    }
}

/// A dead letter is erased through the identities it is indexed under, so
/// every person its event names must be among them, by every identifier
/// the event gives (an erasure may know the person by their phone number
/// only: the link to their BSUID came with the very event the sink
/// refused). Every event of every fixture, against an oracle written
/// from Meta's field names and the BSUID format, not from the store's
/// own list.
#[tokio::test]
async fn every_person_a_fixture_event_names_is_indexed() {
    let store = DeadLetters::new(Arc::new(MemoryKvStore::new()));
    let refused = SinkError::permanent(std::io::Error::other("refused"));
    let mut checked = 0;
    let mut missed = Vec::new();
    for name in common::all_fixtures() {
        let Ok(payload) =
            meta_whatsapp_webhooks::WebhookPayload::from_slice(&common::fixture_bytes(&name))
        else {
            continue;
        };
        for event in payload.into_events() {
            let json = serde_json::to_value(&event).unwrap();
            let (mut found, mut business) = (Vec::new(), Vec::new());
            people(&json, &mut found, &mut business);
            found.retain(|id| !business.contains(id));
            let id = store.record(&event, &refused).await.unwrap();
            let letter = store.get(id).await.unwrap().unwrap();
            for person in found {
                checked += 1;
                if !letter.contacts.contains(&person) {
                    missed.push(format!("{name} ({}): {person}", event.kind()));
                }
            }
        }
    }
    missed.sort();
    missed.dedup();
    assert!(missed.is_empty(), "not indexed:\n{}", missed.join("\n"));
    assert!(checked > 200, "only {checked} identities");
}

/// The events of a signed `batch(n)`.
fn batch_events(n: usize) -> Vec<WebhookEvent> {
    meta_whatsapp_webhooks::WebhookPayload::from_slice(&batch(n).0)
        .unwrap()
        .into_events()
}

/// A `MemoryKvStore` that pauses the first write to the index namespace,
/// before (`after == false`) or after it reaches the store, until told to
/// go on: an erasure can run at that exact point of an append.
#[derive(Debug)]
struct Gate {
    inner: MemoryKvStore,
    after: bool,
    armed: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
}

impl Gate {
    fn new(after: bool) -> (Arc<Self>, oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (reached, at_gate) = oneshot::channel();
        let (go_on, proceed) = oneshot::channel();
        let gate = Arc::new(Self {
            inner: MemoryKvStore::new(),
            after,
            armed: Mutex::new(Some((reached, proceed))),
        });
        (gate, at_gate, go_on)
    }

    async fn pause(&self, key: &StoreKey, after: bool) {
        if key.namespace() != DEAD_LETTER_INDEX_NAMESPACE || after != self.after {
            return;
        }
        let armed = self.armed.lock().unwrap().take();
        if let Some((reached, proceed)) = armed {
            reached.send(()).unwrap();
            proceed.await.unwrap();
        }
    }
}

#[async_trait]
impl KvStore for Gate {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        self.inner.get(key).await
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.pause(key, false).await;
        let written = self.inner.put(key, value, expiry).await;
        self.pause(key, true).await;
        written
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.pause(key, false).await;
        let written = self.inner.put_if_absent(key, value, expiry).await;
        self.pause(key, true).await;
        written
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.pause(key, false).await;
        let written = self
            .inner
            .compare_and_swap(key, expected, new, expiry)
            .await;
        self.pause(key, true).await;
        written
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.inner.delete(key).await
    }
}

fn their_ids() -> Vec<String> {
    vec![
        "US.13491208655302741918".to_owned(),
        "16505551234".to_owned(),
    ]
}

fn number() -> PhoneNumberId {
    PhoneNumberId::new("106540352242922").unwrap()
}

/// An erasure that reads the index between an append's letter and its
/// index entry misses that letter (it is in no index yet); the append
/// then indexes it, so the next erasure removes it (the guides' "erase
/// again"). Documented in `deadletter` § Concurrency.
#[tokio::test]
async fn an_erasure_racing_an_append_leaves_it_to_the_next_erasure() {
    let (kv, at_gate, go_on) = Gate::new(false);
    let store = DeadLetters::new(kv);
    let append = {
        let store = store.clone();
        let event = batch_events(1).remove(0);
        tokio::spawn(async move { store.record(&event, &permanent()).await })
    };
    at_gate.await.unwrap();
    assert_eq!(store.erase(&number(), &their_ids()).await.unwrap(), 0);
    go_on.send(()).unwrap();
    let id = append.await.unwrap().unwrap();
    assert!(store.get(id).await.unwrap().is_some(), "indexed after");
    assert_eq!(store.erase(&number(), &their_ids()).await.unwrap(), 1);
    assert_eq!(store.get(id).await.unwrap(), None);
}

/// The letter is written before it is indexed: an erasure that reads an
/// index entry always finds its letter, so it never deletes an entry
/// whose letter lands afterwards, unreachable. Indexing first fails this.
#[tokio::test]
async fn an_erasure_that_reads_an_index_entry_finds_its_letter() {
    let (kv, at_gate, go_on) = Gate::new(true);
    let store = DeadLetters::new(kv);
    let append = {
        let store = store.clone();
        let event = batch_events(1).remove(0);
        tokio::spawn(async move { store.record(&event, &permanent()).await })
    };
    at_gate.await.unwrap();
    assert_eq!(store.erase(&number(), &their_ids()).await.unwrap(), 1);
    go_on.send(()).unwrap();
    let id = append.await.unwrap().unwrap();
    assert_eq!(
        store.get(id).await.unwrap(),
        None,
        "erased, not left unindexed"
    );
    assert!(store.list().await.unwrap().is_empty());
}

/// A `MemoryKvStore` whose reads hand control to the other tasks before
/// they return: every append of a burst reads the head before any of them
/// swaps it.
#[derive(Debug, Default)]
struct Interleaving(MemoryKvStore);

#[async_trait]
impl KvStore for Interleaving {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        let read = self.0.get(key).await;
        tokio::task::yield_now().await;
        read
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.0.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.0.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.0.compare_and_swap(key, expected, new, expiry).await
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.0.delete(key).await
    }
}

/// Appends racing for the head each take their own id (a compare-and-swap,
/// retried a bounded number of times), and every letter is kept and
/// indexed: none overwrites another. Taking the id with a plain write
/// fails this.
#[tokio::test]
async fn racing_appends_take_distinct_ids_and_keep_every_letter() {
    let kv: Arc<dyn KvStore> = Arc::new(Interleaving::default());
    let store = DeadLetters::new(kv);
    let events = batch_events(8);
    let refused = permanent();
    let ids = futures::future::join_all(events.iter().map(|e| store.record(e, &refused))).await;
    let mut ids: Vec<u64> = ids.into_iter().map(|id| id.unwrap().get()).collect();
    ids.sort_unstable();
    assert_eq!(ids, (0..8).collect::<Vec<_>>());
    assert_eq!(store.list().await.unwrap().len(), 8);
    assert_eq!(store.erase(&number(), &their_ids()).await.unwrap(), 8);
}

fn permanent() -> SinkError {
    SinkError::permanent(std::io::Error::other("NUL in a Meta-assigned id"))
}
