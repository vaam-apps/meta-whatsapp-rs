//! Dead letters: events a sink refused permanently, kept for an operator.
//!
//! Without this store, a sink that fails on one event every time
//! (a value its database refuses, a shape its code cannot handle) makes
//! the handler answer `500` to every redelivery: Meta retries the whole
//! body, every event in it, for up to 7 days and then drops it, so that
//! one event holds back the ones after it until all are lost
//! (`OPEN_QUESTIONS.md` #30).
//!
//! With [`WebhookHandlerBuilder::dead_letters`](crate::WebhookHandlerBuilder::dead_letters),
//! a sink error the sink classified as permanent
//! ([`SinkError::Permanent`], [`SinkError::is_permanent`]) is written here,
//! *then* the event counts as handled and the rest of the batch is
//! delivered. Every other sink error is transient and answers `500` as
//! before; an error a sink does not classify is transient, so a sink that
//! never opts in keeps that behaviour. If the write here fails, the
//! handler answers `500` too: an event is acknowledged only once it is
//! delivered or dead-lettered, never dropped silently.
//!
//! # Layout
//!
//! Two namespaces of the [`KvStore`] (pending the owner's confirmation
//! before the first release: `docs/architecture.md` § Stable identifiers):
//!
//! - [`DEAD_LETTER_NAMESPACE`] (`wa.webhook.deadletter`): a ring of
//!   [`DeadLetters::capacity`] slots, keys `0`…`capacity - 1` (decimal),
//!   and the ring's head under the key `head` (`{"v":1,"next":<id>}`, never
//!   expiring). Each letter takes the next id; it is stored in slot
//!   `id % capacity`, so the letter `capacity` ids older is overwritten:
//!   the count bound evicts the oldest without a scan (and logs it).
//! - [`DEAD_LETTER_INDEX_NAMESPACE`] (`wa.webhook.deadletter.contact`): per
//!   business number and contact, the ids and slots of the letters naming
//!   them ([`index_key`]: SHA-256 hex of `<len>:<phone_number_id>,<len>:<contact>,`,
//!   so no phone number sits in the keyspace in plain text). An erasure
//!   reads it; the store has no scan to read anything else.
//!
//! Each record is JSON with `"v":1`, in a fixed field order: the id, the
//! time it was dead-lettered (unix seconds), the attempts (1, plus one per
//! replay the sink refused permanently again), the event's kind, the sink's
//! error text (at most 512 bytes), the business phone number id, the
//! contacts it is indexed under, the size of the event, and the event in
//! this crate's serialized form (the JSON the SSE helper streams), or
//! `null` when it is larger than [`DeadLetters::max_event_bytes`].
//!
//! # Bounds, and personal data
//!
//! A dead letter holds the event, so it is personal data (a customer's
//! message, their phone number). It is bounded three ways: by count
//! ([`DEFAULT_CAPACITY`], [`DeadLetters::with_capacity`]), by age (each
//! record and index expires [`DEFAULT_MAX_AGE`] after it was dead-lettered,
//! [`DeadLetters::with_max_age`]: that is the retention to document), and
//! by size (an event over [`DEFAULT_MAX_EVENT_BYTES`] is recorded without
//! its content, [`DeadLetters::with_max_event_bytes`], and cannot be
//! replayed). No secret is ever in a record: events carry none.
//!
//! [`DeadLetters::erase`] removes every letter naming a contact on a
//! number, through the index; the facade's eraser
//! (`meta_whatsapp_rs::erasure::Eraser`) calls it beside the conversation
//! store's erasure, which cannot reach this store (`KvStore` has no scan).
//!
//! The contacts a letter is indexed under are every person the event
//! names, found in its serialized form: per object, the BSUID
//! (`user_id`, `from_user_id`, `to_user_id`, `recipient_user_id`,
//! `recipient_participant_user_id`, and both sides of a BSUID change)
//! first, and the phone number beside it (`wa_id`, `from`, `to`,
//! `recipient_id`, `recipient_participant_id`) only when that object has
//! no BSUID; the business's own number (`display_phone_number`) is left
//! out. A letter whose event names no business number
//! ([`WebhookEvent::phone_number_id`], else a `phone_number_id` in its
//! body) is indexed under the empty number, which every number's erasure
//! also reads.
//!
//! # Alert
//!
//! Every dead-lettering is a `tracing::error!` ("webhook event
//! dead-lettered") with the letter's id, the event's kind and the business
//! number, never its content or the sink's error text, and is counted in
//! [`DeliveryReport::dead_lettered`](crate::DeliveryReport::dead_lettered).
//! An eviction by the count bound is a `tracing::warn!`.
//!
//! # Replay, and the dedup guard
//!
//! A dead-lettered event's dedup claim is marked done, like a delivered
//! one's: the delivery was acknowledged, so a redelivery of the same body
//! (Meta's retry after another event of it failed transiently) is a
//! duplicate and does not dead-letter it again. Without a dedup guard it
//! would be dead-lettered once per redelivery.
//!
//! [`DeadLetters::replay`] and [`DeadLetters::replay_all`] hand the stored
//! event to a sink directly, past the dedup guard (whose marker says
//! done): delivered, the letter is removed; refused permanently again, it
//! stays with one more attempt; a transient failure leaves it as it was.
//! Replay is at least once: two replays of one letter at the same time may
//! both deliver it, so the sink must be idempotent, as every sink must
//! for Meta's redeliveries.
//!
//! # Concurrency
//!
//! Ids come from a compare-and-swap on the head, so two replicas never
//! share one. The index is updated by compare-and-swap. A letter is
//! written before it is indexed: an append racing an erasure may land
//! after it, and is then indexed, so the next erasure (or the age bound)
//! removes it. When the index cannot be updated, the letter is deleted
//! again and the handler answers `500`. The count bound evicts the oldest
//! letter except in a race of more than `capacity` appends at once, where
//! a delayed one may overwrite a newer one; either way one of them is
//! evicted, as the bound says. Changing the capacity moves slots: letters
//! written before stay reachable by an erasure (the index keeps their
//! slot) but not by [`DeadLetters::get`], `list` or `replay` until they
//! age out.

use std::collections::BTreeSet;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use meta_whatsapp_core::clock::{Clock, SystemClock};
use meta_whatsapp_core::error::{SinkError, StorageError};
use meta_whatsapp_core::ids::PhoneNumberId;
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::event::WebhookEvent;

/// Namespace of the dead letters' ring (slots `0`…`capacity - 1`) and its
/// head (`head`). Pending the owner's confirmation before the first
/// release (`docs/architecture.md` § Stable identifiers).
pub const DEAD_LETTER_NAMESPACE: &str = "wa.webhook.deadletter";

/// Namespace of the per-contact index of the dead letters ([`index_key`]).
/// Pending the owner's confirmation before the first release.
pub const DEAD_LETTER_INDEX_NAMESPACE: &str = "wa.webhook.deadletter.contact";

/// Default count bound: 1000 letters, the oldest evicted first.
pub const DEFAULT_CAPACITY: NonZeroU64 = NonZeroU64::MIN.saturating_add(999);

/// Default age bound, the retention of a dead letter: 14 days after it was
/// dead-lettered (Meta's 7-day redelivery window, and as long again for an
/// operator to fix the sink and replay).
pub const DEFAULT_MAX_AGE: Duration = Duration::from_hours(14 * 24);

/// Default size bound of one stored event, serialized: 256 KiB. With
/// [`DEFAULT_CAPACITY`], the store holds at most about 256 MiB of events.
pub const DEFAULT_MAX_EVENT_BYTES: usize = 256 * 1024;

/// The key of the ring's head in [`DEAD_LETTER_NAMESPACE`].
const HEAD_KEY: &str = "head";

/// At most this many bytes of the sink's error text are kept.
const ERROR_TEXT_CAP: usize = 512;

/// Compare-and-swap rounds before giving up with [`StorageError::Busy`].
const CAS_ROUNDS: usize = 16;

/// The first byte-identical field of every record: its layout's version.
const RECORD_VERSION: u8 = 1;

/// A dead letter's id: its place in the order letters were written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeadLetterId(u64);

impl DeadLetterId {
    /// The id `id` (as [`DeadLetterId::get`] returned it, or as logged).
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// The number.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for DeadLetterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One dead-lettered event, as [`DeadLetters::get`] and
/// [`DeadLetters::list`] read it. Its `Debug` shows no content (the
/// event, the contacts and the error text are personal data).
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct DeadLetter {
    /// Its id.
    pub id: DeadLetterId,
    /// When it was dead-lettered.
    pub at: OffsetDateTime,
    /// 1, plus one per replay the sink refused permanently again.
    pub attempts: u32,
    /// The event's [`WebhookEvent::kind`].
    pub kind: String,
    /// The sink's error text, at most 512 bytes.
    pub error: String,
    /// The business phone number id the event names, if any.
    pub phone_number_id: Option<String>,
    /// The contacts it is indexed under (BSUIDs, else phone numbers).
    pub contacts: Vec<String>,
    /// The size of the serialized event, in bytes.
    pub event_bytes: u64,
    /// The event, serialized (the JSON the SSE helper streams); `None`
    /// when it was larger than [`DeadLetters::max_event_bytes`].
    pub event: Option<Value>,
}

impl fmt::Debug for DeadLetter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeadLetter")
            .field("id", &self.id)
            .field("at", &self.at)
            .field("attempts", &self.attempts)
            .field("kind", &self.kind)
            .field("phone_number_id", &self.phone_number_id)
            .field("contacts", &self.contacts.len())
            .field("event_bytes", &self.event_bytes)
            .field("event", &self.event.as_ref().map(|_| "…"))
            .finish_non_exhaustive()
    }
}

/// What [`DeadLetters::replay`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Replay {
    /// The sink took the event; the letter is removed.
    Delivered,
    /// The sink refused it permanently again; the letter stays, with one
    /// more attempt.
    StillPermanent,
    /// The letter was stored without its event (over the size bound): there
    /// is nothing to replay. [`DeadLetters::remove`] it once handled.
    NoPayload,
    /// No letter has this id (replayed, removed, erased, evicted, or aged
    /// out).
    Missing,
}

/// What [`DeadLetters::replay_all`] did, by letter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReplayReport {
    /// Delivered and removed.
    pub delivered: usize,
    /// Refused permanently again, kept.
    pub still_permanent: usize,
    /// Stored without their event, kept.
    pub no_payload: usize,
    /// Failed transiently (the sink's error is logged), kept.
    pub failed: usize,
}

/// The dead-letter store: a typed store on a [`KvStore`]. Cheap to clone.
/// See the [module docs](self).
#[derive(Debug, Clone)]
pub struct DeadLetters {
    kv: Arc<dyn KvStore>,
    clock: Arc<dyn Clock>,
    capacity: NonZeroU64,
    max_age: Duration,
    max_event_bytes: usize,
}

/// The stored record, field order pinned (`the_record_bytes_are_pinned`).
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    v: u8,
    id: u64,
    at: i64,
    attempts: u32,
    kind: String,
    error: String,
    phone_number_id: Option<String>,
    contacts: Vec<String>,
    event_bytes: u64,
    event: Option<Value>,
}

/// An index record: the `[id, slot]` of each letter naming the contact.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    v: u8,
    letters: Vec<(u64, u64)>,
}

/// The ring's head: the id the next letter takes.
#[derive(Debug, Serialize, Deserialize)]
struct Head {
    v: u8,
    next: u64,
}

/// The [`KvStore`] key of slot `slot` of the ring.
pub fn slot_key(slot: u64) -> StoreKey {
    StoreKey::new(DEAD_LETTER_NAMESPACE, slot.to_string())
}

/// The [`KvStore`] key of the index of `contact` on business number
/// `phone_number_id` (empty for letters whose event names no number): the
/// SHA-256 hex of `<len>:<phone_number_id>,<len>:<contact>,` (lengths in
/// bytes), in [`DEAD_LETTER_INDEX_NAMESPACE`]. For operators who need to
/// inspect or delete one index by hand.
pub fn index_key(phone_number_id: &str, contact: &str) -> StoreKey {
    let mut hasher = Sha256::new();
    for part in [phone_number_id, contact] {
        hasher.update(format!("{}:", part.len()));
        hasher.update(part);
        hasher.update(",");
    }
    StoreKey::new(DEAD_LETTER_INDEX_NAMESPACE, hex::encode(hasher.finalize()))
}

fn corrupt(key: &StoreKey, source: serde_json::Error) -> StorageError {
    StorageError::Corrupt {
        key: key.to_string(),
        source,
    }
}

fn encode<T: Serialize>(key: &StoreKey, value: &T) -> Result<Vec<u8>, StorageError> {
    serde_json::to_vec(value).map_err(|e| corrupt(key, e))
}

fn decode<T: DeserializeOwned>(key: &StoreKey, stored: &Versioned) -> Result<T, StorageError> {
    serde_json::from_slice(&stored.value).map_err(|e| corrupt(key, e))
}

/// `text` cut to at most `cap` bytes, at a character boundary.
fn truncate(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The BSUID fields, each with the phone number field beside it.
const IDENTITY_PAIRS: [(&str, &str); 5] = [
    ("user_id", "wa_id"),
    ("from_user_id", "from"),
    ("to_user_id", "to"),
    ("recipient_user_id", "recipient_id"),
    ("recipient_participant_user_id", "recipient_participant_id"),
];

fn non_empty(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Every person `value` names (BSUID first, per object), and every
/// business number it displays (to leave out).
fn walk_identities(value: &Value, found: &mut BTreeSet<String>, business: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (bsuid, phone) in IDENTITY_PAIRS {
                // A BSUID change: `user_id` is `{previous, current}`.
                if let Some(Value::Object(change)) = map.get(bsuid) {
                    for side in ["previous", "current"] {
                        if let Some(id) = non_empty(change.get(side)) {
                            found.insert(id.to_owned());
                        }
                    }
                } else if let Some(id) = non_empty(map.get(bsuid)) {
                    found.insert(id.to_owned());
                } else if let Some(number) = non_empty(map.get(phone)) {
                    found.insert(number.to_owned());
                }
            }
            if let Some(number) = non_empty(map.get("display_phone_number")) {
                business.insert(number.chars().filter(char::is_ascii_digit).collect());
            }
            for nested in map.values() {
                walk_identities(nested, found, business);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk_identities(item, found, business);
            }
        }
        _ => {}
    }
}

/// The contacts a letter of `event` (serialized as `json`) is indexed
/// under, sorted. See the module docs.
fn contacts_of(json: &Value) -> Vec<String> {
    let mut found = BTreeSet::new();
    let mut business = BTreeSet::new();
    walk_identities(json, &mut found, &mut business);
    found
        .into_iter()
        .filter(|id| !business.contains(id))
        .collect()
}

/// The first `phone_number_id` string in `value`, depth first.
fn find_phone_number_id(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => non_empty(map.get("phone_number_id"))
            .map(str::to_owned)
            .or_else(|| map.values().find_map(find_phone_number_id)),
        Value::Array(items) => items.iter().find_map(find_phone_number_id),
        _ => None,
    }
}

impl DeadLetters {
    /// A store on `kv`, with the system clock and the default bounds
    /// ([`DEFAULT_CAPACITY`], [`DEFAULT_MAX_AGE`], [`DEFAULT_MAX_EVENT_BYTES`]).
    pub fn new(kv: Arc<dyn KvStore>) -> Self {
        Self {
            kv,
            clock: Arc::new(SystemClock),
            capacity: DEFAULT_CAPACITY,
            max_age: DEFAULT_MAX_AGE,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        }
    }

    /// Date letters with `clock` (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Keep at most `capacity` letters, the oldest evicted first. Changing
    /// it on a store in use moves the slots: see the module docs.
    #[must_use]
    pub fn with_capacity(mut self, capacity: NonZeroU64) -> Self {
        self.capacity = capacity;
        self
    }

    /// Keep each letter (and its index) this long after it was
    /// dead-lettered: the retention of the personal data it holds.
    #[must_use]
    pub fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = max_age;
        self
    }

    /// Store an event's content only up to `bytes` (serialized); a larger
    /// one is recorded without it, and cannot be replayed.
    #[must_use]
    pub fn with_max_event_bytes(mut self, bytes: usize) -> Self {
        self.max_event_bytes = bytes;
        self
    }

    /// The count bound.
    pub fn capacity(&self) -> NonZeroU64 {
        self.capacity
    }

    /// The age bound.
    pub fn max_age(&self) -> Duration {
        self.max_age
    }

    /// The size bound of one stored event.
    pub fn max_event_bytes(&self) -> usize {
        self.max_event_bytes
    }

    fn slot_of(&self, id: u64) -> u64 {
        id % self.capacity.get()
    }

    /// Dead-letter `event`, which the sink refused with `error`: write it,
    /// index it under each contact it names, and raise the alert. The
    /// webhook handler calls it for a [`SinkError::Permanent`]; call it
    /// yourself from a pipeline of your own.
    ///
    /// # Errors
    ///
    /// [`meta_whatsapp_core::Error::Storage`] when the store fails (the
    /// handler then answers `500`, so Meta redelivers): nothing is left
    /// half-written but, if deleting the letter again failed too, a letter
    /// missing from the index, which the age bound removes.
    pub async fn record(
        &self,
        event: &WebhookEvent,
        error: &SinkError,
    ) -> meta_whatsapp_core::Result<DeadLetterId> {
        let label = StoreKey::new(DEAD_LETTER_NAMESPACE, "event");
        let json = serde_json::to_value(event).map_err(|e| corrupt(&label, e))?;
        let event_bytes = serde_json::to_vec(&json)
            .map_err(|e| corrupt(&label, e))?
            .len();
        let phone_number_id = event
            .phone_number_id()
            .map(ToString::to_string)
            .or_else(|| find_phone_number_id(&json));
        let contacts = contacts_of(&json);
        let id = self.next_id().await?;
        let slot = self.slot_of(id);
        let key = slot_key(slot);
        let record = Record {
            v: RECORD_VERSION,
            id,
            at: self.clock.now().unix_timestamp(),
            attempts: 1,
            kind: event.kind().to_owned(),
            error: truncate(&error.to_string(), ERROR_TEXT_CAP),
            phone_number_id: phone_number_id.clone(),
            contacts: contacts.clone(),
            event_bytes: u64::try_from(event_bytes).unwrap_or(u64::MAX),
            event: (event_bytes <= self.max_event_bytes).then_some(json),
        };
        if let Some(evicted) = self.kv.get(&key).await? {
            let evicted_id = decode::<Record>(&key, &evicted).map(|r| r.id).ok();
            tracing::warn!(
                evicted = ?evicted_id,
                letter = id,
                capacity = self.capacity.get(),
                "dead-letter store full; evicting the oldest letter"
            );
        }
        let version = self
            .kv
            .put(&key, encode(&key, &record)?, Expiry::After(self.max_age))
            .await?;
        let number = phone_number_id.as_deref().unwrap_or("");
        for contact in &contacts {
            if let Err(index_error) = self.index(number, contact, id, slot).await {
                // Unindexed, an erasure could not reach it: take it back,
                // and let the handler answer `500` so Meta redelivers.
                if let Err(delete_error) = self
                    .kv
                    .compare_and_swap(&key, version, None, Expiry::Keep)
                    .await
                {
                    tracing::error!(
                        error = %delete_error,
                        letter = id,
                        "could not delete an unindexed dead letter; it ages out"
                    );
                }
                return Err(index_error.into());
            }
        }
        if record.event.is_none() {
            tracing::error!(
                letter = id,
                event_bytes,
                limit = self.max_event_bytes,
                "dead-lettered event is over the size bound; recorded without its content"
            );
        }
        tracing::error!(
            letter = id,
            kind = event.kind(),
            phone_number_id = number,
            "webhook event dead-lettered: the sink refused it permanently"
        );
        Ok(DeadLetterId(id))
    }

    /// Take the next id: a compare-and-swap on the head.
    async fn next_id(&self) -> Result<u64, StorageError> {
        let key = StoreKey::new(DEAD_LETTER_NAMESPACE, HEAD_KEY);
        for _ in 0..CAS_ROUNDS {
            match self.kv.get(&key).await? {
                None => {
                    let head = encode(
                        &key,
                        &Head {
                            v: RECORD_VERSION,
                            next: 1,
                        },
                    )?;
                    if self
                        .kv
                        .put_if_absent(&key, head, Expiry::Never)
                        .await?
                        .is_some()
                    {
                        return Ok(0);
                    }
                }
                Some(stored) => {
                    let id = decode::<Head>(&key, &stored)?.next;
                    let head = encode(
                        &key,
                        &Head {
                            v: RECORD_VERSION,
                            next: id.wrapping_add(1),
                        },
                    )?;
                    if self
                        .kv
                        .compare_and_swap(&key, stored.version, Some(head), Expiry::Never)
                        .await?
                        .is_some()
                    {
                        return Ok(id);
                    }
                }
            }
        }
        Err(StorageError::Busy)
    }

    /// Add `[id, slot]` to `contact`'s index on `number`, dropping the
    /// entries the ring has evicted since.
    async fn index(
        &self,
        number: &str,
        contact: &str,
        id: u64,
        slot: u64,
    ) -> Result<(), StorageError> {
        let key = index_key(number, contact);
        let capacity = self.capacity.get();
        for _ in 0..CAS_ROUNDS {
            let stored = self.kv.get(&key).await?;
            let mut index = match &stored {
                Some(stored) => decode::<Index>(&key, stored)?,
                None => Index::default(),
            };
            index.v = RECORD_VERSION;
            index
                .letters
                .retain(|&(old, _)| old.saturating_add(capacity) > id && old != id);
            index.letters.push((id, slot));
            let value = encode(&key, &index)?;
            let expiry = Expiry::After(self.max_age);
            let written = match &stored {
                Some(stored) => {
                    self.kv
                        .compare_and_swap(&key, stored.version, Some(value), expiry)
                        .await?
                }
                None => self.kv.put_if_absent(&key, value, expiry).await?,
            };
            if written.is_some() {
                return Ok(());
            }
        }
        Err(StorageError::Busy)
    }

    /// The letter in `slot`, if it is the letter `id`.
    async fn read(&self, id: u64, slot: u64) -> Result<Option<(Versioned, Record)>, StorageError> {
        let key = slot_key(slot);
        let Some(stored) = self.kv.get(&key).await? else {
            return Ok(None);
        };
        let record = decode::<Record>(&key, &stored)?;
        Ok((record.id == id).then_some((stored, record)))
    }

    /// The letter `id`, if it is still kept.
    ///
    /// # Errors
    ///
    /// When the store fails or the record does not decode.
    pub async fn get(&self, id: DeadLetterId) -> meta_whatsapp_core::Result<Option<DeadLetter>> {
        Ok(self
            .read(id.0, self.slot_of(id.0))
            .await?
            .map(|(_, record)| letter(record)))
    }

    /// Every letter kept, oldest first: one read per slot of the ring
    /// ([`Self::capacity`] reads).
    ///
    /// # Errors
    ///
    /// When the store fails or a record does not decode.
    pub async fn list(&self) -> meta_whatsapp_core::Result<Vec<DeadLetter>> {
        let mut letters = Vec::new();
        for slot in 0..self.capacity.get() {
            let key = slot_key(slot);
            if let Some(stored) = self.kv.get(&key).await? {
                letters.push(letter(decode::<Record>(&key, &stored)?));
            }
        }
        letters.sort_by_key(|l| l.id);
        Ok(letters)
    }

    /// Delete the letter `id` (handled by hand). Returns whether it was
    /// there. Its index entries go when the index is next written, or
    /// when it ages out; they hold ids only.
    ///
    /// # Errors
    ///
    /// When the store fails.
    pub async fn remove(&self, id: DeadLetterId) -> meta_whatsapp_core::Result<bool> {
        Ok(self.remove_at(id.0, self.slot_of(id.0), None).await?)
    }

    /// Delete the letter `id` from `slot`, if it is still that letter (and,
    /// with `contact`, still names them), by compare-and-swap.
    async fn remove_at(
        &self,
        id: u64,
        slot: u64,
        contact: Option<&str>,
    ) -> Result<bool, StorageError> {
        let key = slot_key(slot);
        for _ in 0..CAS_ROUNDS {
            let Some((stored, record)) = self.read(id, slot).await? else {
                return Ok(false);
            };
            if contact.is_some_and(|c| !record.contacts.iter().any(|known| known == c)) {
                return Ok(false);
            }
            if self
                .kv
                .compare_and_swap(&key, stored.version, None, Expiry::Keep)
                .await?
                .is_some()
            {
                return Ok(true);
            }
        }
        Err(StorageError::Busy)
    }

    /// Hand the letter `id`'s event to `sink` again, past the dedup guard:
    /// see [`Replay`] for what happens to the letter.
    ///
    /// # Errors
    ///
    /// A transient sink error ([`meta_whatsapp_core::Error::Sink`]; the
    /// letter is left as it was), a store failure, or a stored event that
    /// no longer decodes ([`StorageError::Corrupt`]).
    pub async fn replay(
        &self,
        id: DeadLetterId,
        sink: &dyn EventSink<WebhookEvent>,
    ) -> meta_whatsapp_core::Result<Replay> {
        let slot = self.slot_of(id.0);
        let key = slot_key(slot);
        let Some((stored, mut record)) = self.read(id.0, slot).await? else {
            return Ok(Replay::Missing);
        };
        let Some(json) = record.event.clone() else {
            return Ok(Replay::NoPayload);
        };
        let event: WebhookEvent = serde_json::from_value(json).map_err(|e| corrupt(&key, e))?;
        match sink.deliver(event).await {
            Ok(()) => {
                self.remove_at(id.0, slot, None).await?;
                Ok(Replay::Delivered)
            }
            Err(error) if error.is_permanent() => {
                record.attempts = record.attempts.saturating_add(1);
                record.error = truncate(&error.to_string(), ERROR_TEXT_CAP);
                // The age bound runs from the first dead-lettering.
                let updated = self
                    .kv
                    .compare_and_swap(
                        &key,
                        stored.version,
                        Some(encode(&key, &record)?),
                        Expiry::Keep,
                    )
                    .await?;
                if updated.is_none() {
                    tracing::warn!(
                        letter = id.0,
                        "dead letter changed during its replay; attempts not counted"
                    );
                }
                tracing::error!(
                    letter = id.0,
                    kind = %record.kind,
                    attempts = record.attempts,
                    "replayed dead letter refused permanently again; kept"
                );
                Ok(Replay::StillPermanent)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// [`Self::replay`] every letter kept, oldest first. A transient sink
    /// failure is logged and counted, and the next letter is tried.
    ///
    /// # Errors
    ///
    /// When the store fails or a record does not decode.
    pub async fn replay_all(
        &self,
        sink: &dyn EventSink<WebhookEvent>,
    ) -> meta_whatsapp_core::Result<ReplayReport> {
        let mut report = ReplayReport::default();
        for letter in self.list().await? {
            match self.replay(letter.id, sink).await {
                Ok(Replay::Delivered) => report.delivered += 1,
                Ok(Replay::StillPermanent) => report.still_permanent += 1,
                Ok(Replay::NoPayload) => report.no_payload += 1,
                Ok(Replay::Missing) => {}
                Err(meta_whatsapp_core::Error::Sink(error)) => {
                    tracing::warn!(%error, letter = letter.id.0, "dead letter replay failed; kept");
                    report.failed += 1;
                }
                Err(other) => return Err(other),
            }
        }
        Ok(report)
    }

    /// Erase one person on `phone_number_id`, known by `contacts` (their
    /// BSUIDs and phone numbers): delete every letter indexed under one of
    /// them on that number, or on no number (a letter whose event named
    /// none), then their indexes. Returns how many letters it deleted.
    ///
    /// Call it through the facade's eraser, beside the conversation
    /// store's erasure (`meta_whatsapp_rs::erasure::Eraser`). An index
    /// that changes while it is read (a letter dead-lettered at the same
    /// moment) is read again.
    ///
    /// # Errors
    ///
    /// When the store fails, or an index keeps changing
    /// ([`StorageError::Busy`]); what it deleted before stays deleted, and
    /// erasing again finishes the job.
    pub async fn erase(
        &self,
        phone_number_id: &PhoneNumberId,
        contacts: &[String],
    ) -> meta_whatsapp_core::Result<u64> {
        let contacts: BTreeSet<&str> = contacts
            .iter()
            .map(String::as_str)
            .filter(|c| !c.is_empty())
            .collect();
        let mut erased = 0;
        for number in [phone_number_id.as_str(), ""] {
            for contact in &contacts {
                erased += self.erase_index(number, contact).await?;
            }
        }
        Ok(erased)
    }

    async fn erase_index(&self, number: &str, contact: &str) -> Result<u64, StorageError> {
        let key = index_key(number, contact);
        let mut erased = 0;
        for _ in 0..CAS_ROUNDS {
            let Some(stored) = self.kv.get(&key).await? else {
                return Ok(erased);
            };
            let index = decode::<Index>(&key, &stored)?;
            for &(id, slot) in &index.letters {
                if self.remove_at(id, slot, Some(contact)).await? {
                    erased += 1;
                }
            }
            if self
                .kv
                .compare_and_swap(&key, stored.version, None, Expiry::Keep)
                .await?
                .is_some()
            {
                return Ok(erased);
            }
        }
        Err(StorageError::Busy)
    }
}

fn letter(record: Record) -> DeadLetter {
    DeadLetter {
        id: DeadLetterId(record.id),
        at: OffsetDateTime::from_unix_timestamp(record.at).unwrap_or(OffsetDateTime::UNIX_EPOCH),
        attempts: record.attempts,
        kind: record.kind,
        error: record.error,
        phone_number_id: record.phone_number_id,
        contacts: record.contacts,
        event_bytes: record.event_bytes,
        event: record.event,
    }
}

#[cfg(test)]
mod tests;
