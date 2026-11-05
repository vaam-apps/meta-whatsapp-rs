//! [`ConversationStore`] on MongoDB.
//!
//! - **What touches several documents is one multi-document
//!   transaction** (snapshot reads, majority writes), tried again from the
//!   start when MongoDB aborts it for a write conflict, for at most
//!   [`MongoConversationStore::with_busy_after`]'s budget
//!   ([`BUSY_AFTER`]), after which it answers `StorageError::Busy` having
//!   written nothing: `append` and `append_synced` (the messages and their
//!   conversations' summaries), `fill_media_placeholder` (a message and,
//!   when it is the latest, its summary's preview), `set_thread_owner`,
//!   `put_contact` and `remove_contact` (a read, then a write that depends
//!   on it), and `erase_all`.
//! - **Ids are stored once per store** (`OPEN_QUESTIONS.md` #33): a
//!   message's `_id` is its id. An append reads the batch's ids in its
//!   snapshot first; one stored meanwhile by another writer makes the
//!   insert fail (a write conflict, or a duplicate key), and the next
//!   attempt reads it.
//! - **The summary is written by every append**, an increment of its
//!   `appends` counter beside whatever else changes: MongoDB's snapshot
//!   transactions conflict on a write, never on a read, and an update
//!   that changes nothing writes nothing. That write is what orders an
//!   append against an erasure of its conversation (the erasure deletes
//!   the summary): one of the two commits first and the other starts over,
//!   so a message is erased with the rest or recorded after the erasure
//!   with a summary of its own, never kept without one. Two appends to
//!   one conversation take turns the same way; appends to different
//!   conversations never meet.
//! - **One document's writes need no transaction**: `update_status` and a
//!   revoke's status change are one update each, filtered on the stored
//!   status being one the new status
//!   [supersedes](DeliveryStatus::supersedes) (the rule is core's, not
//!   re-encoded: the filter lists the statuses it says yes to), so
//!   concurrent updates end on the highest status; `revoke` first inserts
//!   the tombstone and, when the id is taken, changes the status;
//!   `record_window_event` and `link_identity` are an insert that a
//!   duplicate key answers `false`; `mark_read` is one update.
//! - **Purges and trims are a sequence of deletes**, each atomic per
//!   document, not one step: a purge deletes the old messages, then the
//!   summaries whose latest message was older than the cutoff, then the
//!   old window events, ownership records and contact removals; a trim
//!   reads each conversation's rows beyond its cap and deletes those, by
//!   id, then its summary only if its latest message is among them (so a
//!   summary an append moved to a newer message meanwhile stays). A
//!   reader during a purge may see a summary whose latest message is gone
//!   already, until the purge's next delete. A message recorded during a
//!   purge or a trim may be kept over the cutoff or the cap until the
//!   next one, as on Postgres.
//!
//! Instants are milliseconds (see the [module docs](super#the-database-servers-clock-is-the-clock)):
//! Meta's timestamps are seconds, so nothing it sends loses precision.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use ::mongodb::bson::{self, Bson, Document, doc};
use ::mongodb::error::{TRANSIENT_TRANSACTION_ERROR, UNKNOWN_TRANSACTION_COMMIT_RESULT};
use ::mongodb::options::{ReadConcern, WriteConcern};
use ::mongodb::{ClientSession, Collection, Database};
use async_trait::async_trait;
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::ids::{AppId, MessageId, PhoneNumberId, UserId};
use meta_whatsapp_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, DeliveryStatus, Direction, Erased,
    ErasureMode, IdentityLink, Purged, Retention, StoredContact, StoredMessage, ThreadOwner,
    ThreadOwnership, WindowEvent, WindowEventKind,
};
use time::OffsetDateTime;

use super::schema as s;
use super::{backend, collection, corrupt, date, date_ceil, is_duplicate_key, limit_of, time_of};

/// Every log line of this store.
const LOG_TARGET: &str = "meta_whatsapp_adapters::store::mongodb";

/// How long a transaction keeps trying again after write conflicts before
/// it answers `StorageError::Busy`, by default
/// ([`MongoConversationStore::with_busy_after`]).
pub const BUSY_AFTER: Duration = Duration::from_secs(10);

/// How many ids one delete of a trim names at most.
const TRIM_CHUNK: usize = 1000;

/// Every status this crate names, for the update filters: those a new
/// status [supersedes](DeliveryStatus::supersedes). A status a later
/// version of core adds is never matched, so never overwritten, until it
/// is listed here.
const STATUSES: [DeliveryStatus; 8] = [
    DeliveryStatus::Received,
    DeliveryStatus::Accepted,
    DeliveryStatus::Sent,
    DeliveryStatus::Delivered,
    DeliveryStatus::Read,
    DeliveryStatus::Played,
    DeliveryStatus::Failed,
    DeliveryStatus::Deleted,
];

/// `ConversationStore` over a MongoDB [`Database`] of a replica set (its
/// collections created by [`migrate`](super::migrate)). Cheap to clone.
///
/// Unread counts are by **arrival** (inbound messages appended since the
/// last `mark_read`; synced history never counts), like the other stores.
/// Instants are kept to the millisecond.
///
/// Retention is kept by default; [`with_retention`](Self::with_retention)
/// sets what [`ConversationStore::apply_retention`] purges. Nothing purges
/// on its own: schedule `apply_retention` (and `trim_conversations`, for a
/// cap on each conversation's history). An erasure redacts an erased
/// person's messages in other conversations (a group's) by default;
/// [`with_erasure_mode`](Self::with_erasure_mode) deletes them instead.
///
/// # Limits
///
/// - An erasure is one transaction: a person with more records than one
///   transaction holds (MongoDB aborts one after
///   `transactionLifetimeLimitSeconds`, 60 by default) cannot be erased
///   in one call. Neither can an `append_synced` batch that large.
/// - Appends to one conversation take turns by aborting and starting
///   over, not by queueing on a lock as on Postgres, so a conversation
///   written by many writers at once answers `Busy` sooner under load
///   than Postgres would wait.
/// - A trim reads, per conversation over its cap, the ids of the rows it
///   deletes: on a large store, trim one number at a time.
#[derive(Clone)]
pub struct MongoConversationStore {
    database: Database,
    retention: Retention,
    erasure_mode: ErasureMode,
    busy_after: Duration,
}

impl fmt::Debug for MongoConversationStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The database's name, never the client's options (they can carry
        // credentials), nor a message.
        f.debug_struct("MongoConversationStore")
            .field("database", &self.database.name())
            .field("retention", &self.retention)
            .field("erasure_mode", &self.erasure_mode)
            .field("busy_after", &self.busy_after)
            .finish_non_exhaustive()
    }
}

/// Why an attempt of a transaction failed: the driver's error (which may be
/// worth another attempt), or one of the store's own (which is not).
enum Failure {
    Mongo(::mongodb::error::Error),
    Store(StorageError),
}

impl From<::mongodb::error::Error> for Failure {
    fn from(error: ::mongodb::error::Error) -> Self {
        Self::Mongo(error)
    }
}

impl From<StorageError> for Failure {
    fn from(error: StorageError) -> Self {
        Self::Store(error)
    }
}

/// One attempt of a transaction's work.
type Attempt<'s, T> = Pin<Box<dyn Future<Output = Result<T, Failure>> + Send + 's>>;

/// The work of one transaction, on its input, which
/// [`MongoConversationStore::transaction`] runs again from the start after
/// a write conflict.
type Work<I, T> =
    for<'s> fn(&'s MongoConversationStore, &'s I, &'s mut ClientSession) -> Attempt<'s, T>;

/// Whether a transaction that failed this way may succeed from the start:
/// MongoDB labels a write conflict (and a primary stepping down) a
/// transient transaction error; a duplicate key inside a transaction is a
/// key another writer committed after its snapshot, which the next attempt
/// reads.
fn worth_another_attempt(error: &::mongodb::error::Error) -> bool {
    error.contains_label(TRANSIENT_TRANSACTION_ERROR) || is_duplicate_key(error)
}

/// The pause before attempt `attempt + 1`: a random part of 1, 2, 4, … up
/// to 64 milliseconds, so that writers aborted by one another do not meet
/// again in step.
fn backoff(attempt: u32) -> Duration {
    use std::hash::BuildHasher as _;
    let ceiling_micros = 1000u64 << attempt.min(6);
    let random = std::collections::hash_map::RandomState::new().hash_one(attempt);
    Duration::from_micros(100 + random % ceiling_micros)
}

// ─── Documents ───────────────────────────────────────────────────────────

/// A JSON value's text, as `payload_json` and `error_json` hold it.
fn json_text(value: &serde_json::Value, field: &str) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|source| StorageError::Corrupt {
        key: format!("{}.{field}", s::MESSAGES),
        source,
    })
}

fn json_of(text: &str, field: &str) -> Result<serde_json::Value, StorageError> {
    serde_json::from_str(text).map_err(|source| StorageError::Corrupt {
        key: format!("{}.{field}", s::MESSAGES),
        source,
    })
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Inbound => "inbound",
        Direction::Outbound => "outbound",
    }
}

fn direction_of(name: &str) -> Result<Direction, StorageError> {
    match name {
        "inbound" => Ok(Direction::Inbound),
        "outbound" => Ok(Direction::Outbound),
        _ => Err(corrupt(s::MESSAGES, s::MESSAGE_DIRECTION)),
    }
}

/// A status's serde name, as Postgres stores it too.
fn status_name(status: DeliveryStatus) -> Result<String, StorageError> {
    match serde_json::to_value(status) {
        Ok(serde_json::Value::String(name)) => Ok(name),
        _ => Err(corrupt(s::MESSAGES, s::MESSAGE_STATUS)),
    }
}

fn status_of(name: &str) -> Result<DeliveryStatus, StorageError> {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .map_err(|_| corrupt(s::MESSAGES, s::MESSAGE_STATUS))
}

/// The names of the stored statuses `status` supersedes.
fn superseded_by(status: DeliveryStatus) -> Result<Vec<String>, StorageError> {
    STATUSES
        .into_iter()
        .filter(|stored| status.supersedes(*stored))
        .map(status_name)
        .collect()
}

fn owner_name(owner: ThreadOwner) -> Result<String, StorageError> {
    match serde_json::to_value(owner) {
        Ok(serde_json::Value::String(name)) => Ok(name),
        _ => Err(corrupt(s::THREAD_OWNERS, s::OWNER_OWNER)),
    }
}

fn owner_of(name: &str) -> Result<ThreadOwner, StorageError> {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .map_err(|_| corrupt(s::THREAD_OWNERS, s::OWNER_OWNER))
}

fn str_of<'d>(
    document: &'d Document,
    collection: &str,
    field: &str,
) -> Result<&'d str, StorageError> {
    document
        .get_str(field)
        .map_err(|_| corrupt(collection, field))
}

/// An optional string field: absent (or null) is `None`.
fn optional_str<'d>(
    document: &'d Document,
    collection: &str,
    field: &str,
) -> Result<Option<&'d str>, StorageError> {
    match document.get(field) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::String(value)) => Ok(Some(value)),
        Some(_) => Err(corrupt(collection, field)),
    }
}

fn date_of(
    document: &Document,
    collection: &str,
    field: &str,
) -> Result<bson::DateTime, StorageError> {
    document
        .get_datetime(field)
        .copied()
        .map_err(|_| corrupt(collection, field))
}

fn time_field(
    document: &Document,
    collection: &str,
    field: &str,
) -> Result<OffsetDateTime, StorageError> {
    time_of(date_of(document, collection, field)?, collection, field)
}

/// An optional date field: absent (or null) is `None`.
fn optional_time(
    document: &Document,
    collection: &str,
    field: &str,
) -> Result<Option<OffsetDateTime>, StorageError> {
    match document.get(field) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::DateTime(at)) => time_of(*at, collection, field).map(Some),
        Some(_) => Err(corrupt(collection, field)),
    }
}

fn i64_of(document: &Document, collection: &str, field: &str) -> Result<i64, StorageError> {
    match document.get(field) {
        Some(Bson::Int64(n)) => Ok(*n),
        Some(Bson::Int32(n)) => Ok(i64::from(*n)),
        _ => Err(corrupt(collection, field)),
    }
}

fn key_filter(key: &ConversationKey) -> Document {
    doc! { s::PHONE_NUMBER_ID: key.phone_number_id.as_str(), s::CONTACT: key.contact.as_str() }
}

/// `key`'s business number and contact as stored.
fn key_of(document: &Document, collection: &str) -> Result<ConversationKey, StorageError> {
    Ok(ConversationKey {
        phone_number_id: PhoneNumberId::new_unchecked(str_of(
            document,
            collection,
            s::PHONE_NUMBER_ID,
        )?),
        contact: str_of(document, collection, s::CONTACT)?.to_owned(),
    })
}

/// A message's document, as `append`, `append_synced` and `revoke` (its
/// tombstone) store it. Its sender is [`StoredMessage::sender`], as it is
/// appended.
fn message_document(message: &StoredMessage) -> Result<Document, StorageError> {
    let mut document = doc! {
        "_id": message.id.as_str(),
        s::PHONE_NUMBER_ID: message.conversation.phone_number_id.as_str(),
        s::CONTACT: message.conversation.contact.as_str(),
        s::MESSAGE_DIRECTION: direction_name(message.direction),
        s::MESSAGE_KIND: message.kind.as_str(),
        s::MESSAGE_PAYLOAD: json_text(&message.payload, s::MESSAGE_PAYLOAD)?,
        s::MESSAGE_STATUS: status_name(message.status)?,
        s::MESSAGE_TS: date(message.timestamp),
    };
    if let Some(text) = &message.text {
        document.insert(s::MESSAGE_TEXT, text.as_str());
    }
    if let Some(at) = message.status_at {
        document.insert(s::MESSAGE_STATUS_AT, date(at));
    }
    if let Some(error) = &message.error {
        document.insert(s::MESSAGE_ERROR, json_text(error, s::MESSAGE_ERROR)?);
    }
    if let Some(sender) = message.sender() {
        document.insert(s::MESSAGE_SENDER, sender);
    }
    Ok(document)
}

/// A message, from its document.
fn message_of(document: &Document) -> Result<StoredMessage, StorageError> {
    let m = s::MESSAGES;
    Ok(StoredMessage {
        id: MessageId::new(str_of(document, m, "_id")?),
        conversation: key_of(document, m)?,
        direction: direction_of(str_of(document, m, s::MESSAGE_DIRECTION)?)?,
        kind: str_of(document, m, s::MESSAGE_KIND)?.to_owned(),
        text: optional_str(document, m, s::MESSAGE_TEXT)?.map(str::to_owned),
        payload: json_of(str_of(document, m, s::MESSAGE_PAYLOAD)?, s::MESSAGE_PAYLOAD)?,
        status: status_of(str_of(document, m, s::MESSAGE_STATUS)?)?,
        timestamp: time_field(document, m, s::MESSAGE_TS)?,
        status_at: optional_time(document, m, s::MESSAGE_STATUS_AT)?,
        error: optional_str(document, m, s::MESSAGE_ERROR)?
            .map(|text| json_of(text, s::MESSAGE_ERROR))
            .transpose()?,
    })
}

/// A summary, from its document.
fn summary_of(document: &Document) -> Result<ConversationSummary, StorageError> {
    let c = s::CONVERSATIONS;
    Ok(ConversationSummary {
        key: key_of(document, c)?,
        last_message_at: time_field(document, c, s::SUMMARY_LAST_MESSAGE_AT)?,
        last_inbound_at: optional_time(document, c, s::SUMMARY_LAST_INBOUND_AT)?,
        last_text: optional_str(document, c, s::SUMMARY_LAST_TEXT)?.map(str::to_owned),
        unread: u64::try_from(i64_of(document, c, s::SUMMARY_UNREAD)?)
            .map_err(|_| corrupt(c, s::SUMMARY_UNREAD))?,
    })
}

fn window_event_document(event: &WindowEvent) -> Document {
    doc! {
        s::PHONE_NUMBER_ID: event.conversation.phone_number_id.as_str(),
        s::WINDOW_EVENT_ID: event.id.as_str(),
        s::CONTACT: event.conversation.contact.as_str(),
        s::WINDOW_EVENT_KIND: event.kind.as_str(),
        s::WINDOW_EVENT_TS: date(event.at),
    }
}

fn window_event_of(document: &Document) -> Result<WindowEvent, StorageError> {
    let w = s::WINDOW_EVENTS;
    Ok(WindowEvent {
        conversation: key_of(document, w)?,
        kind: WindowEventKind::from_name(str_of(document, w, s::WINDOW_EVENT_KIND)?),
        id: str_of(document, w, s::WINDOW_EVENT_ID)?.to_owned(),
        at: time_field(document, w, s::WINDOW_EVENT_TS)?,
    })
}

fn owner_document(
    key: &ConversationKey,
    ownership: &ThreadOwnership,
) -> Result<Document, StorageError> {
    let mut document = key_filter(key);
    document.insert(s::OWNER_OWNER, owner_name(ownership.owner)?);
    if let Some(role) = &ownership.role {
        document.insert(s::OWNER_ROLE, role.as_str());
    }
    if let Some(app) = &ownership.app_id {
        document.insert(s::OWNER_APP_ID, app.as_str());
    }
    document.insert(s::OWNER_SINCE, date(ownership.since));
    Ok(document)
}

fn owner_of_document(document: &Document) -> Result<ThreadOwnership, StorageError> {
    let o = s::THREAD_OWNERS;
    Ok(ThreadOwnership {
        owner: owner_of(str_of(document, o, s::OWNER_OWNER)?)?,
        role: optional_str(document, o, s::OWNER_ROLE)?.map(str::to_owned),
        app_id: optional_str(document, o, s::OWNER_APP_ID)?.map(AppId::new_unchecked),
        since: time_field(document, o, s::OWNER_SINCE)?,
    })
}

/// A synced contact's document: its key, the fields present, its time,
/// and `removed: false`.
fn contact_document(contact: &StoredContact) -> Document {
    let mut document = key_filter(&contact.key);
    for (field, value) in [
        (s::CONTACT_FULL_NAME, contact.full_name.as_deref()),
        (s::CONTACT_FIRST_NAME, contact.first_name.as_deref()),
        (s::CONTACT_PHONE_NUMBER, contact.phone_number.as_deref()),
        (
            s::CONTACT_USER_ID,
            contact.user_id.as_ref().map(UserId::as_str),
        ),
        (
            s::CONTACT_PARENT_USER_ID,
            contact.parent_user_id.as_ref().map(UserId::as_str),
        ),
        (s::CONTACT_USERNAME, contact.username.as_deref()),
    ] {
        if let Some(value) = value {
            document.insert(field, value);
        }
    }
    document.insert(s::CONTACT_SYNCED_AT, date(contact.synced_at));
    document.insert(s::CONTACT_REMOVED, false);
    document
}

/// A kept removal's document: the key and the removal's time, nothing of
/// the contact.
fn removal_document(key: &ConversationKey, at: OffsetDateTime) -> Document {
    let mut document = key_filter(key);
    document.insert(s::CONTACT_SYNCED_AT, date(at));
    document.insert(s::CONTACT_REMOVED, true);
    document
}

fn contact_of(document: &Document) -> Result<StoredContact, StorageError> {
    let c = s::SYNCED_CONTACTS;
    let text = |field| optional_str(document, c, field).map(|v| v.map(str::to_owned));
    Ok(StoredContact {
        key: key_of(document, c)?,
        full_name: text(s::CONTACT_FULL_NAME)?,
        first_name: text(s::CONTACT_FIRST_NAME)?,
        phone_number: text(s::CONTACT_PHONE_NUMBER)?,
        user_id: text(s::CONTACT_USER_ID)?.map(UserId::new),
        parent_user_id: text(s::CONTACT_PARENT_USER_ID)?.map(UserId::new),
        username: text(s::CONTACT_USERNAME)?,
        synced_at: time_field(document, c, s::CONTACT_SYNCED_AT)?,
    })
}

fn link_document(link: &IdentityLink) -> Document {
    doc! {
        s::PHONE_NUMBER_ID: link.phone_number_id.as_str(),
        s::LINK_PREVIOUS: link.previous.as_str(),
        s::LINK_CURRENT: link.current.as_str(),
        s::LINK_TS: date(link.at),
    }
}

fn link_of(document: &Document) -> Result<IdentityLink, StorageError> {
    let l = s::IDENTITY_LINKS;
    Ok(IdentityLink {
        phone_number_id: PhoneNumberId::new_unchecked(str_of(document, l, s::PHONE_NUMBER_ID)?),
        previous: str_of(document, l, s::LINK_PREVIOUS)?.to_owned(),
        current: str_of(document, l, s::LINK_CURRENT)?.to_owned(),
        at: time_field(document, l, s::LINK_TS)?,
    })
}

/// The filter of the rows strictly before an `(instant, id)` cursor, newest
/// first by `(instant, id)`.
fn before_filter(
    filter: &mut Document,
    at_field: &str,
    id_field: &str,
    at: OffsetDateTime,
    id: &str,
) {
    let at = date(at);
    filter.insert(
        "$or",
        vec![
            doc! { at_field: { "$lt": at } },
            doc! { at_field: at, id_field: { "$lt": id } },
        ],
    );
}

/// The scope of a purge or a trim: one business number, or every one.
fn scope(phone_number_id: Option<&PhoneNumberId>) -> Document {
    phone_number_id.map_or_else(Document::new, |pn| doc! { s::PHONE_NUMBER_ID: pn.as_str() })
}

// ─── Inputs of the transactions ───────────────────────────────────────────

/// Messages to insert: `live` ones (`append`) move the window and count as
/// unread when inbound; synced ones (`append_synced`) do neither.
struct Incoming {
    messages: Vec<StoredMessage>,
    live: bool,
}

struct Fill {
    phone_number_id: PhoneNumberId,
    id: MessageId,
    kind: String,
    text: Option<String>,
    payload: serde_json::Value,
}

struct Owner {
    key: ConversationKey,
    ownership: ThreadOwnership,
}

struct Removal {
    key: ConversationKey,
    at: OffsetDateTime,
}

struct Erasure {
    phone_number_id: PhoneNumberId,
    contacts: Vec<String>,
}

/// What a batch changes of one conversation's summary: its newest message
/// by `(timestamp, id)` (milliseconds), and, for live inbound messages,
/// the latest one's time and how many.
struct Touch {
    at: i64,
    id: String,
    text: Option<String>,
    inbound_at: Option<i64>,
    unread: i64,
}

impl Touch {
    fn new(message: &StoredMessage, live: bool) -> Self {
        let at = date(message.timestamp).timestamp_millis();
        let inbound = live && message.direction == Direction::Inbound;
        Self {
            at,
            id: message.id.as_str().to_owned(),
            text: message.text.clone(),
            inbound_at: inbound.then_some(at),
            unread: i64::from(inbound),
        }
    }

    fn add(&mut self, message: &StoredMessage, live: bool) {
        let next = Self::new(message, live);
        if (next.at, next.id.as_str()) > (self.at, self.id.as_str()) {
            self.at = next.at;
            self.id = next.id;
            self.text = next.text;
        }
        self.inbound_at = self.inbound_at.max(next.inbound_at);
        self.unread += next.unread;
    }
}

/// How an append writes a conversation's summary.
#[derive(Debug, PartialEq)]
enum SummaryWrite {
    /// A new summary: the conversation had none.
    Insert(Document),
    /// The update of the stored one.
    Update(Document),
}

/// What `touch` makes of `key`'s summary, `stored` as read in the
/// transaction: always a write, `appends` incremented, so that the
/// transaction conflicts with an erasure of the conversation (see the
/// module docs), even when nothing else changes (an older synced message).
fn summary_write(
    key: &ConversationKey,
    stored: Option<&Document>,
    touch: Touch,
) -> Result<SummaryWrite, StorageError> {
    let c = s::CONVERSATIONS;
    let Some(stored) = stored else {
        let mut document = key_filter(key);
        document.insert(
            s::SUMMARY_LAST_MESSAGE_AT,
            bson::DateTime::from_millis(touch.at),
        );
        document.insert(s::SUMMARY_LAST_MESSAGE_ID, touch.id);
        if let Some(text) = touch.text {
            document.insert(s::SUMMARY_LAST_TEXT, text);
        }
        if let Some(at) = touch.inbound_at {
            document.insert(s::SUMMARY_LAST_INBOUND_AT, bson::DateTime::from_millis(at));
        }
        document.insert(s::SUMMARY_UNREAD, touch.unread);
        document.insert(s::SUMMARY_APPENDS, 1_i64);
        return Ok(SummaryWrite::Insert(document));
    };
    let latest = (
        date_of(stored, c, s::SUMMARY_LAST_MESSAGE_AT)?.timestamp_millis(),
        str_of(stored, c, s::SUMMARY_LAST_MESSAGE_ID)?,
    );
    let mut set = Document::new();
    let mut unset = Document::new();
    if (touch.at, touch.id.as_str()) > latest {
        set.insert(
            s::SUMMARY_LAST_MESSAGE_AT,
            bson::DateTime::from_millis(touch.at),
        );
        set.insert(s::SUMMARY_LAST_MESSAGE_ID, touch.id.as_str());
        match &touch.text {
            Some(text) => {
                set.insert(s::SUMMARY_LAST_TEXT, text.as_str());
            }
            None => {
                unset.insert(s::SUMMARY_LAST_TEXT, "");
            }
        }
    }
    if let Some(at) = touch.inbound_at {
        let window = optional_time(stored, c, s::SUMMARY_LAST_INBOUND_AT)?
            .map(|t| date(t).timestamp_millis());
        if window.is_none_or(|current| current < at) {
            set.insert(s::SUMMARY_LAST_INBOUND_AT, bson::DateTime::from_millis(at));
        }
    }
    let mut update =
        doc! { "$inc": { s::SUMMARY_UNREAD: touch.unread, s::SUMMARY_APPENDS: 1_i64 } };
    if !set.is_empty() {
        update.insert("$set", set);
    }
    if !unset.is_empty() {
        update.insert("$unset", unset);
    }
    Ok(SummaryWrite::Update(update))
}

impl MongoConversationStore {
    /// The store in `database`, which [`migrate`](super::migrate)
    /// prepared, keeping everything ([`Retention::Keep`]) and redacting an
    /// erased person's group messages ([`ErasureMode::Redact`]). The
    /// database must be on a replica set (or a sharded cluster): MongoDB
    /// runs no transaction on a standalone server.
    pub fn new(database: Database) -> Self {
        Self {
            database,
            retention: Retention::Keep,
            erasure_mode: ErasureMode::Redact,
            busy_after: BUSY_AFTER,
        }
    }

    /// The same store applying `retention` in
    /// [`ConversationStore::apply_retention`].
    #[must_use]
    pub fn with_retention(mut self, retention: Retention) -> Self {
        self.retention = retention;
        self
    }

    /// The same store redacting or deleting an erased person's messages in
    /// other conversations (a group's) as `mode` says, in
    /// [`ConversationStore::erase_all`] ([`ErasureMode::Redact`] by
    /// default).
    #[must_use]
    pub fn with_erasure_mode(mut self, mode: ErasureMode) -> Self {
        self.erasure_mode = mode;
        self
    }

    /// How long a transaction keeps trying again after write conflicts
    /// before it answers `StorageError::Busy` ([`BUSY_AFTER`] by default).
    #[must_use]
    pub fn with_busy_after(mut self, budget: Duration) -> Self {
        self.busy_after = budget;
        self
    }

    /// The database it stores in.
    pub fn database(&self) -> &Database {
        &self.database
    }

    fn messages_collection(&self) -> Collection<Document> {
        collection(&self.database, s::MESSAGES)
    }

    fn conversations_collection(&self) -> Collection<Document> {
        collection(&self.database, s::CONVERSATIONS)
    }

    fn window_events_collection(&self) -> Collection<Document> {
        collection(&self.database, s::WINDOW_EVENTS)
    }

    fn owners_collection(&self) -> Collection<Document> {
        collection(&self.database, s::THREAD_OWNERS)
    }

    fn contacts_collection(&self) -> Collection<Document> {
        collection(&self.database, s::SYNCED_CONTACTS)
    }

    fn links_collection(&self) -> Collection<Document> {
        collection(&self.database, s::IDENTITY_LINKS)
    }

    /// Run `work` on `input` in a transaction: snapshot reads, majority
    /// writes; from the start again after a write conflict (or another
    /// error MongoDB labels transient), until the budget is spent (`Busy`,
    /// nothing written). A commit whose result is unknown is tried again
    /// as it is (MongoDB's rule: committing again is safe), never redone.
    async fn transaction<I: Sync, T: Send>(
        &self,
        input: &I,
        work: Work<I, T>,
    ) -> Result<T, StorageError> {
        let mut session = self
            .database
            .client()
            .start_session()
            .await
            .map_err(backend)?;
        let started = Instant::now();
        let mut attempt = 0u32;
        loop {
            session
                .start_transaction()
                .read_concern(ReadConcern::snapshot())
                .write_concern(WriteConcern::majority())
                .await
                .map_err(backend)?;
            let error = match work(self, input, &mut session).await {
                Ok(output) => match self.commit(&mut session, started).await {
                    Ok(()) => return Ok(output),
                    Err(error) => error,
                },
                Err(failure) => {
                    // The server may have aborted it already: the driver
                    // ignores what an abort answers.
                    session.abort_transaction().await.ok();
                    match failure {
                        Failure::Store(error) => return Err(error),
                        Failure::Mongo(error) => error,
                    }
                }
            };
            if error.contains_label(UNKNOWN_TRANSACTION_COMMIT_RESULT)
                || !worth_another_attempt(&error)
            {
                return Err(backend(error));
            }
            if started.elapsed() >= self.busy_after {
                tracing::warn!(
                    target: LOG_TARGET,
                    attempts = attempt + 1,
                    "a transaction kept meeting write conflicts past its budget: busy"
                );
                return Err(StorageError::Busy);
            }
            tokio::time::sleep(backoff(attempt)).await;
            attempt = attempt.saturating_add(1);
        }
    }

    /// Commit, trying the commit itself again while its result is unknown
    /// and the budget lasts.
    async fn commit(
        &self,
        session: &mut ClientSession,
        started: Instant,
    ) -> Result<(), ::mongodb::error::Error> {
        let mut attempt = 0u32;
        loop {
            match session.commit_transaction().await {
                Ok(()) => return Ok(()),
                Err(error)
                    if error.contains_label(UNKNOWN_TRANSACTION_COMMIT_RESULT)
                        && started.elapsed() < self.busy_after =>
                {
                    tokio::time::sleep(backoff(attempt)).await;
                    attempt = attempt.saturating_add(1);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Insert the messages whose ids are not stored (nor earlier in the
    /// batch), then write each touched conversation's summary.
    fn insert_work<'s>(
        &'s self,
        input: &'s Incoming,
        session: &'s mut ClientSession,
    ) -> Attempt<'s, Vec<bool>> {
        Box::pin(async move {
            if input.messages.is_empty() {
                return Ok(Vec::new());
            }
            let messages = self.messages_collection();
            let ids: Vec<&str> = input.messages.iter().map(|m| m.id.as_str()).collect();
            let mut taken: HashSet<String> = HashSet::new();
            let mut cursor = messages
                .find(doc! { "_id": { "$in": ids } })
                .projection(doc! { "_id": 1 })
                .session(&mut *session)
                .await?;
            while let Some(row) = cursor.next(&mut *session).await {
                taken.insert(str_of(&row?, s::MESSAGES, "_id")?.to_owned());
            }
            let mut inserted = Vec::with_capacity(input.messages.len());
            let mut documents = Vec::new();
            // By business number and contact, so that a batch writes its
            // summaries in one order.
            let mut touched: BTreeMap<(&str, &str), (&ConversationKey, Touch)> = BTreeMap::new();
            for message in &input.messages {
                let fresh = taken.insert(message.id.as_str().to_owned());
                inserted.push(fresh);
                if !fresh {
                    continue;
                }
                documents.push(message_document(message)?);
                let key = &message.conversation;
                touched
                    .entry((key.phone_number_id.as_str(), key.contact.as_str()))
                    .and_modify(|(_, touch)| touch.add(message, input.live))
                    .or_insert_with(|| (key, Touch::new(message, input.live)));
            }
            if !documents.is_empty() {
                messages
                    .insert_many(documents)
                    .session(&mut *session)
                    .await?;
            }
            for (key, touch) in touched.into_values() {
                self.summarize(session, key, touch).await?;
            }
            Ok(inserted)
        })
    }

    /// Write `touch` into `key`'s summary, or create it: always a write
    /// (`appends` incremented), so that the transaction conflicts with an
    /// erasure of the conversation (see the module docs).
    async fn summarize(
        &self,
        session: &mut ClientSession,
        key: &ConversationKey,
        touch: Touch,
    ) -> Result<(), Failure> {
        let conversations = self.conversations_collection();
        let filter = key_filter(key);
        let stored = conversations
            .find_one(filter.clone())
            .session(&mut *session)
            .await?;
        match summary_write(key, stored.as_ref(), touch)? {
            SummaryWrite::Insert(document) => {
                conversations
                    .insert_one(document)
                    .session(&mut *session)
                    .await?;
            }
            SummaryWrite::Update(update) => {
                conversations
                    .update_one(filter, update)
                    .session(&mut *session)
                    .await?;
            }
        }
        Ok(())
    }

    fn fill_work<'s>(
        &'s self,
        input: &'s Fill,
        session: &'s mut ClientSession,
    ) -> Attempt<'s, bool> {
        Box::pin(async move {
            // Never a revoked one: the content is what its sender deleted.
            let filter = doc! {
                "_id": input.id.as_str(),
                s::PHONE_NUMBER_ID: input.phone_number_id.as_str(),
                s::MESSAGE_KIND: StoredMessage::MEDIA_PLACEHOLDER,
                s::MESSAGE_STATUS: { "$ne": status_name(DeliveryStatus::Deleted)? },
            };
            let mut set = doc! {
                s::MESSAGE_KIND: input.kind.as_str(),
                s::MESSAGE_PAYLOAD: json_text(&input.payload, s::MESSAGE_PAYLOAD)?,
            };
            let mut update = Document::new();
            match &input.text {
                Some(text) => {
                    set.insert(s::MESSAGE_TEXT, text.as_str());
                }
                None => {
                    update.insert("$unset", doc! { s::MESSAGE_TEXT: "" });
                }
            }
            update.insert("$set", set);
            let Some(filled) = self
                .messages_collection()
                .find_one_and_update(filter, update)
                .projection(doc! { s::PHONE_NUMBER_ID: 1, s::CONTACT: 1 })
                .session(&mut *session)
                .await?
            else {
                return Ok(false);
            };
            // The preview follows when it is the latest message.
            let mut latest = key_filter(&key_of(&filled, s::MESSAGES)?);
            latest.insert(s::SUMMARY_LAST_MESSAGE_ID, input.id.as_str());
            #[allow(clippy::single_match_else)] // the two shapes of one update
            let preview = match &input.text {
                Some(text) => doc! { "$set": { s::SUMMARY_LAST_TEXT: text.as_str() } },
                None => doc! { "$unset": { s::SUMMARY_LAST_TEXT: "" } },
            };
            self.conversations_collection()
                .update_one(latest, preview)
                .session(&mut *session)
                .await?;
            Ok(true)
        })
    }

    fn owner_work<'s>(
        &'s self,
        input: &'s Owner,
        session: &'s mut ClientSession,
    ) -> Attempt<'s, bool> {
        Box::pin(async move {
            let owners = self.owners_collection();
            let filter = key_filter(&input.key);
            let since = date(input.ownership.since);
            if let Some(stored) = owners
                .find_one(filter.clone())
                .projection(doc! { s::OWNER_SINCE: 1 })
                .session(&mut *session)
                .await?
                && since < date_of(&stored, s::THREAD_OWNERS, s::OWNER_SINCE)?
            {
                return Ok(false);
            }
            owners
                .replace_one(filter, owner_document(&input.key, &input.ownership)?)
                .upsert(true)
                .session(&mut *session)
                .await?;
            Ok(true)
        })
    }

    fn put_contact_work<'s>(
        &'s self,
        input: &'s StoredContact,
        session: &'s mut ClientSession,
    ) -> Attempt<'s, bool> {
        Box::pin(async move {
            let contacts = self.contacts_collection();
            let filter = key_filter(&input.key);
            // A removal counts as stored: an older `add` is refused.
            if let Some(stored) = contacts
                .find_one(filter.clone())
                .projection(doc! { s::CONTACT_SYNCED_AT: 1 })
                .session(&mut *session)
                .await?
                && date(input.synced_at)
                    < date_of(&stored, s::SYNCED_CONTACTS, s::CONTACT_SYNCED_AT)?
            {
                return Ok(false);
            }
            contacts
                .replace_one(filter, contact_document(input))
                .upsert(true)
                .session(&mut *session)
                .await?;
            Ok(true)
        })
    }

    fn remove_contact_work<'s>(
        &'s self,
        input: &'s Removal,
        session: &'s mut ClientSession,
    ) -> Attempt<'s, bool> {
        Box::pin(async move {
            let contacts = self.contacts_collection();
            let filter = key_filter(&input.key);
            let at = date(input.at);
            let removed = match contacts
                .find_one(filter.clone())
                .projection(doc! { s::CONTACT_SYNCED_AT: 1, s::CONTACT_REMOVED: 1 })
                .session(&mut *session)
                .await?
            {
                Some(stored)
                    if date_of(&stored, s::SYNCED_CONTACTS, s::CONTACT_SYNCED_AT)? > at =>
                {
                    return Ok(false);
                }
                Some(stored) => !stored
                    .get_bool(s::CONTACT_REMOVED)
                    .map_err(|_| corrupt(s::SYNCED_CONTACTS, s::CONTACT_REMOVED))?,
                None => false,
            };
            contacts
                .replace_one(filter, removal_document(&input.key, input.at))
                .upsert(true)
                .session(&mut *session)
                .await?;
            Ok(removed)
        })
    }

    #[allow(clippy::too_many_lines)] // one step, every record kind in turn
    fn erase_work<'s>(
        &'s self,
        input: &'s Erasure,
        session: &'s mut ClientSession,
    ) -> Attempt<'s, Erased> {
        Box::pin(async move {
            let pn = input.phone_number_id.as_str();
            let ids: Vec<&str> = input
                .contacts
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let mut erased = Erased::default();
            if ids.is_empty() {
                return Ok(erased);
            }
            // Every record keyed by one of the ids.
            let keyed = doc! { s::PHONE_NUMBER_ID: pn, s::CONTACT: { "$in": ids.clone() } };
            erased.messages = self
                .messages_collection()
                .delete_many(keyed.clone())
                .session(&mut *session)
                .await?
                .deleted_count;
            erased.conversations = self
                .conversations_collection()
                .delete_many(keyed.clone())
                .session(&mut *session)
                .await?
                .deleted_count;
            erased.window_events = self
                .window_events_collection()
                .delete_many(keyed.clone())
                .session(&mut *session)
                .await?
                .deleted_count;
            erased.thread_owners = self
                .owners_collection()
                .delete_many(keyed)
                .session(&mut *session)
                .await?
                .deleted_count;
            // An empty id names no one: it matches what is keyed by it,
            // never a contact's other ids nor a link's sides.
            let named: Vec<&str> = ids.iter().copied().filter(|id| !id.is_empty()).collect();
            erased.contacts = self
                .contacts_collection()
                .delete_many(doc! {
                    s::PHONE_NUMBER_ID: pn,
                    "$or": [
                        { s::CONTACT: { "$in": ids.clone() } },
                        { s::CONTACT_USER_ID: { "$in": named.clone() } },
                        { s::CONTACT_PARENT_USER_ID: { "$in": named.clone() } },
                        { s::CONTACT_PHONE_NUMBER: { "$in": named.clone() } },
                    ],
                })
                .session(&mut *session)
                .await?
                .deleted_count;
            erased.identity_links = self
                .links_collection()
                .delete_many(doc! {
                    s::PHONE_NUMBER_ID: pn,
                    "$or": [
                        { s::LINK_PREVIOUS: { "$in": named.clone() } },
                        { s::LINK_CURRENT: { "$in": named.clone() } },
                    ],
                })
                .session(&mut *session)
                .await?
                .deleted_count;
            // Their messages in conversations keyed by someone else (a
            // group's): theirs were deleted above.
            let messages = self.messages_collection();
            let mut theirs: Vec<String> = Vec::new();
            let mut cursor = messages
                .find(doc! { s::PHONE_NUMBER_ID: pn, s::MESSAGE_SENDER: { "$in": ids.clone() } })
                .projection(doc! { "_id": 1 })
                .session(&mut *session)
                .await?;
            while let Some(row) = cursor.next(&mut *session).await {
                theirs.push(str_of(&row?, s::MESSAGES, "_id")?.to_owned());
            }
            if theirs.is_empty() {
                return Ok(erased);
            }
            // The summaries whose latest message is one of them.
            let conversations = self.conversations_collection();
            let latest = doc! {
                s::PHONE_NUMBER_ID: pn,
                s::SUMMARY_LAST_MESSAGE_ID: { "$in": theirs.clone() },
            };
            let mut touched: Vec<ConversationKey> = Vec::new();
            let mut cursor = conversations
                .find(latest.clone())
                .projection(doc! { s::PHONE_NUMBER_ID: 1, s::CONTACT: 1 })
                .session(&mut *session)
                .await?;
            while let Some(row) = cursor.next(&mut *session).await {
                touched.push(key_of(&row?, s::CONVERSATIONS)?);
            }
            let theirs = doc! { "_id": { "$in": theirs } };
            if self.erasure_mode == ErasureMode::Delete {
                erased.group_messages = messages
                    .delete_many(theirs)
                    .session(&mut *session)
                    .await?
                    .deleted_count;
                for key in &touched {
                    self.resummarize(session, key).await?;
                }
            } else {
                // `Redact`, and any mode added later: never keep the content.
                erased.group_messages = messages
                    .update_many(theirs, redaction())
                    .session(&mut *session)
                    .await?
                    .matched_count;
                conversations
                    .update_many(latest, doc! { "$unset": { s::SUMMARY_LAST_TEXT: "" } })
                    .session(&mut *session)
                    .await?;
            }
            Ok(erased)
        })
    }

    /// Make `key`'s summary follow its latest remaining message (never a
    /// tombstone), or delete it when none is left, after its latest message
    /// was deleted. Its window and unread count stay.
    async fn resummarize(
        &self,
        session: &mut ClientSession,
        key: &ConversationKey,
    ) -> Result<(), Failure> {
        let mut filter = key_filter(key);
        filter.insert(s::MESSAGE_KIND, doc! { "$ne": StoredMessage::REVOKED });
        let latest = self
            .messages_collection()
            .find_one(filter)
            .sort(doc! { s::MESSAGE_TS: -1, "_id": -1 })
            .projection(doc! { s::MESSAGE_TS: 1, s::MESSAGE_TEXT: 1 })
            .session(&mut *session)
            .await?;
        let conversations = self.conversations_collection();
        match latest {
            Some(row) => {
                let mut update = doc! { "$set": {
                    s::SUMMARY_LAST_MESSAGE_AT: date_of(&row, s::MESSAGES, s::MESSAGE_TS)?,
                    s::SUMMARY_LAST_MESSAGE_ID: str_of(&row, s::MESSAGES, "_id")?,
                } };
                match optional_str(&row, s::MESSAGES, s::MESSAGE_TEXT)? {
                    Some(text) => {
                        if let Ok(set) = update.get_document_mut("$set") {
                            set.insert(s::SUMMARY_LAST_TEXT, text);
                        }
                    }
                    None => {
                        update.insert("$unset", doc! { s::SUMMARY_LAST_TEXT: "" });
                    }
                }
                conversations
                    .update_one(key_filter(key), update)
                    .session(&mut *session)
                    .await?;
            }
            None => {
                conversations
                    .delete_one(key_filter(key))
                    .session(&mut *session)
                    .await?;
            }
        }
        Ok(())
    }

    /// Read a query's rows in its order, each through `parse`.
    async fn read_all<T>(
        &self,
        collection: &Collection<Document>,
        filter: Document,
        sort: Document,
        limit: usize,
        parse: fn(&Document) -> Result<T, StorageError>,
    ) -> Result<Vec<T>, StorageError> {
        if limit == 0 {
            // MongoDB reads a limit of 0 as none.
            return Ok(Vec::new());
        }
        let mut find = collection.find(filter).sort(sort);
        if let Some(limit) = limit_of(limit) {
            find = find.limit(limit);
        }
        let mut cursor = find.await.map_err(backend)?;
        let mut rows = Vec::new();
        while cursor.advance().await.map_err(backend)? {
            rows.push(parse(&cursor.deserialize_current().map_err(backend)?)?);
        }
        Ok(rows)
    }
}

/// The update that redacts a message in place ([`StoredMessage::redact`]):
/// kind `erased`, `{}` as payload, no text, no error, and no sender.
fn redaction() -> Document {
    doc! {
        "$set": { s::MESSAGE_KIND: StoredMessage::ERASED, s::MESSAGE_PAYLOAD: "{}" },
        "$unset": { s::MESSAGE_TEXT: "", s::MESSAGE_ERROR: "", s::MESSAGE_SENDER: "" },
    }
}

#[async_trait]
impl ConversationStore for MongoConversationStore {
    async fn append(&self, message: StoredMessage) -> Result<bool, StorageError> {
        let input = Incoming {
            messages: vec![message],
            live: true,
        };
        let inserted = self.transaction(&input, Self::insert_work).await?;
        Ok(inserted.first().copied().unwrap_or(false))
    }

    async fn append_synced(&self, messages: Vec<StoredMessage>) -> Result<Vec<bool>, StorageError> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        let input = Incoming {
            messages,
            live: false,
        };
        self.transaction(&input, Self::insert_work).await
    }

    async fn fill_media_placeholder(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        kind: String,
        text: Option<String>,
        payload: serde_json::Value,
    ) -> Result<bool, StorageError> {
        let input = Fill {
            phone_number_id: phone_number_id.clone(),
            id: id.clone(),
            kind,
            text,
            payload,
        };
        self.transaction(&input, Self::fill_work).await
    }

    async fn revoke(
        &self,
        key: &ConversationKey,
        id: &MessageId,
        direction: Direction,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError> {
        // The tombstone first: a message insert alone (a tombstone is
        // never part of the summary), which the `_id` serializes against a
        // concurrent append of the message.
        let tombstone = message_document(&StoredMessage::tombstone(key, id, direction, at))?;
        let messages = self.messages_collection();
        match messages.insert_one(tombstone).await {
            Ok(_) => return Ok(true),
            Err(error) if is_duplicate_key(&error) => {}
            Err(error) => return Err(backend(error)),
        }
        // The id is taken: the message, of this number and direction.
        let deleted = messages
            .update_one(
                doc! {
                    "_id": id.as_str(),
                    s::PHONE_NUMBER_ID: key.phone_number_id.as_str(),
                    s::MESSAGE_DIRECTION: direction_name(direction),
                    s::MESSAGE_STATUS: { "$in": superseded_by(DeliveryStatus::Deleted)? },
                },
                doc! { "$set": {
                    s::MESSAGE_STATUS: status_name(DeliveryStatus::Deleted)?,
                    s::MESSAGE_STATUS_AT: date(at),
                } },
            )
            .await
            .map_err(backend)?;
        Ok(deleted.matched_count > 0)
    }

    async fn update_status(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<serde_json::Value>,
    ) -> Result<bool, StorageError> {
        let mut set =
            doc! { s::MESSAGE_STATUS: status_name(status)?, s::MESSAGE_STATUS_AT: date(at) };
        // An update without an error object keeps the one stored.
        if let Some(error) = &error {
            set.insert(s::MESSAGE_ERROR, json_text(error, s::MESSAGE_ERROR)?);
        }
        let updated = self
            .messages_collection()
            .update_one(
                doc! {
                    "_id": id.as_str(),
                    s::PHONE_NUMBER_ID: phone_number_id.as_str(),
                    s::MESSAGE_STATUS: { "$in": superseded_by(status)? },
                },
                doc! { "$set": set },
            )
            .await
            .map_err(backend)?;
        Ok(updated.matched_count > 0)
    }

    async fn messages(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, MessageId)>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        let mut filter = key_filter(key);
        if let Some((at, id)) = &before {
            before_filter(&mut filter, s::MESSAGE_TS, "_id", *at, id.as_str());
        }
        self.read_all(
            &self.messages_collection(),
            filter,
            doc! { s::MESSAGE_TS: -1, "_id": -1 },
            limit,
            message_of,
        )
        .await
    }

    async fn conversations(
        &self,
        phone_number_id: &PhoneNumberId,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>, StorageError> {
        let mut filter = doc! { s::PHONE_NUMBER_ID: phone_number_id.as_str() };
        if let Some((at, contact)) = &before {
            before_filter(
                &mut filter,
                s::SUMMARY_LAST_MESSAGE_AT,
                s::CONTACT,
                *at,
                contact,
            );
        }
        self.read_all(
            &self.conversations_collection(),
            filter,
            doc! { s::SUMMARY_LAST_MESSAGE_AT: -1, s::CONTACT: -1 },
            limit,
            summary_of,
        )
        .await
    }

    async fn mark_read(&self, key: &ConversationKey) -> Result<(), StorageError> {
        self.conversations_collection()
            .update_one(
                key_filter(key),
                doc! { "$set": { s::SUMMARY_UNREAD: 0_i64 } },
            )
            .await
            .map_err(backend)?;
        Ok(())
    }

    async fn last_inbound_at(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<OffsetDateTime>, StorageError> {
        let Some(summary) = self
            .conversations_collection()
            .find_one(key_filter(key))
            .projection(doc! { s::SUMMARY_LAST_INBOUND_AT: 1 })
            .await
            .map_err(backend)?
        else {
            return Ok(None);
        };
        optional_time(&summary, s::CONVERSATIONS, s::SUMMARY_LAST_INBOUND_AT)
    }

    async fn message(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
    ) -> Result<Option<StoredMessage>, StorageError> {
        self.messages_collection()
            .find_one(doc! { "_id": id.as_str(), s::PHONE_NUMBER_ID: phone_number_id.as_str() })
            .await
            .map_err(backend)?
            .as_ref()
            .map(message_of)
            .transpose()
    }

    async fn record_window_event(&self, event: WindowEvent) -> Result<bool, StorageError> {
        match self
            .window_events_collection()
            .insert_one(window_event_document(&event))
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if is_duplicate_key(&error) => Ok(false),
            Err(error) => Err(backend(error)),
        }
    }

    async fn window_events(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<WindowEvent>, StorageError> {
        let mut filter = key_filter(key);
        if let Some((at, id)) = &before {
            before_filter(&mut filter, s::WINDOW_EVENT_TS, s::WINDOW_EVENT_ID, *at, id);
        }
        self.read_all(
            &self.window_events_collection(),
            filter,
            doc! { s::WINDOW_EVENT_TS: -1, s::WINDOW_EVENT_ID: -1 },
            limit,
            window_event_of,
        )
        .await
    }

    async fn set_thread_owner(
        &self,
        key: &ConversationKey,
        ownership: ThreadOwnership,
    ) -> Result<bool, StorageError> {
        let input = Owner {
            key: key.clone(),
            ownership,
        };
        self.transaction(&input, Self::owner_work).await
    }

    async fn thread_owner(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<ThreadOwnership>, StorageError> {
        self.owners_collection()
            .find_one(key_filter(key))
            .await
            .map_err(backend)?
            .as_ref()
            .map(owner_of_document)
            .transpose()
    }

    async fn put_contact(&self, contact: StoredContact) -> Result<bool, StorageError> {
        self.transaction(&contact, Self::put_contact_work).await
    }

    async fn remove_contact(
        &self,
        key: &ConversationKey,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError> {
        let input = Removal {
            key: key.clone(),
            at,
        };
        self.transaction(&input, Self::remove_contact_work).await
    }

    async fn contact(&self, key: &ConversationKey) -> Result<Option<StoredContact>, StorageError> {
        let mut filter = key_filter(key);
        filter.insert(s::CONTACT_REMOVED, false);
        self.contacts_collection()
            .find_one(filter)
            .await
            .map_err(backend)?
            .as_ref()
            .map(contact_of)
            .transpose()
    }

    async fn contacts(
        &self,
        phone_number_id: &PhoneNumberId,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<StoredContact>, StorageError> {
        let mut filter = doc! {
            s::PHONE_NUMBER_ID: phone_number_id.as_str(),
            s::CONTACT_REMOVED: false,
        };
        if let Some(after) = after {
            filter.insert(s::CONTACT, doc! { "$gt": after });
        }
        self.read_all(
            &self.contacts_collection(),
            filter,
            doc! { s::CONTACT: 1 },
            limit,
            contact_of,
        )
        .await
    }

    async fn link_identity(&self, link: IdentityLink) -> Result<bool, StorageError> {
        match self
            .links_collection()
            .insert_one(link_document(&link))
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if is_duplicate_key(&error) => Ok(false),
            Err(error) => Err(backend(error)),
        }
    }

    async fn identity_links(
        &self,
        key: &ConversationKey,
    ) -> Result<Vec<IdentityLink>, StorageError> {
        self.read_all(
            &self.links_collection(),
            doc! {
                s::PHONE_NUMBER_ID: key.phone_number_id.as_str(),
                "$or": [
                    { s::LINK_PREVIOUS: key.contact.as_str() },
                    { s::LINK_CURRENT: key.contact.as_str() },
                ],
            },
            doc! { s::LINK_TS: 1, s::LINK_PREVIOUS: 1, s::LINK_CURRENT: 1 },
            usize::MAX,
            link_of,
        )
        .await
    }

    async fn identities(&self, key: &ConversationKey) -> Result<BTreeSet<String>, StorageError> {
        let pn = key.phone_number_id.as_str();
        let mut found = BTreeSet::from([key.contact.clone()]);
        // An empty id names no one: never followed, nor found.
        let mut frontier: Vec<String> = Some(key.contact.clone())
            .filter(|id| !id.is_empty())
            .into_iter()
            .collect();
        while !frontier.is_empty() {
            let mut next: Vec<String> = Vec::new();
            let contacts = self
                .read_all(
                    &self.contacts_collection(),
                    doc! {
                        s::PHONE_NUMBER_ID: pn,
                        s::CONTACT_REMOVED: false,
                        "$or": [
                            { s::CONTACT: { "$in": frontier.clone() } },
                            { s::CONTACT_USER_ID: { "$in": frontier.clone() } },
                            { s::CONTACT_PARENT_USER_ID: { "$in": frontier.clone() } },
                            { s::CONTACT_PHONE_NUMBER: { "$in": frontier.clone() } },
                        ],
                    },
                    doc! { s::CONTACT: 1 },
                    usize::MAX,
                    contact_of,
                )
                .await?;
            for contact in contacts {
                next.push(contact.key.contact);
                next.extend(contact.user_id.map(|id| id.as_str().to_owned()));
                next.extend(contact.parent_user_id.map(|id| id.as_str().to_owned()));
                next.extend(contact.phone_number);
            }
            let links = self
                .read_all(
                    &self.links_collection(),
                    doc! {
                        s::PHONE_NUMBER_ID: pn,
                        "$or": [
                            { s::LINK_PREVIOUS: { "$in": frontier.clone() } },
                            { s::LINK_CURRENT: { "$in": frontier.clone() } },
                        ],
                    },
                    doc! { s::LINK_TS: 1 },
                    usize::MAX,
                    link_of,
                )
                .await?;
            for link in links {
                next.push(link.previous);
                next.push(link.current);
            }
            frontier = next
                .into_iter()
                .filter(|id| !id.is_empty() && found.insert(id.clone()))
                .collect();
        }
        Ok(found)
    }

    async fn erase_all(
        &self,
        phone_number_id: &PhoneNumberId,
        contacts: &[String],
    ) -> Result<Erased, StorageError> {
        if contacts.is_empty() {
            return Ok(Erased::default());
        }
        let input = Erasure {
            phone_number_id: phone_number_id.clone(),
            contacts: contacts.to_vec(),
        };
        self.transaction(&input, Self::erase_work).await
    }

    fn erasure_mode(&self) -> ErasureMode {
        self.erasure_mode
    }

    async fn purge_before(
        &self,
        phone_number_id: Option<&PhoneNumberId>,
        cutoff: OffsetDateTime,
    ) -> Result<Purged, StorageError> {
        // Rounded up: what is stored at the millisecond below an instant
        // inside the cutoff's millisecond may be older than it.
        let cutoff = date_ceil(cutoff);
        let older = |field: &str| {
            let mut filter = scope(phone_number_id);
            filter.insert(field, doc! { "$lt": cutoff });
            filter
        };
        let delete = |collection: Collection<Document>, filter: Document| async move {
            collection
                .delete_many(filter)
                .await
                .map(|deleted| deleted.deleted_count)
                .map_err(backend)
        };
        let mut purged = Purged {
            messages: delete(self.messages_collection(), older(s::MESSAGE_TS)).await?,
            // Its latest message is purged exactly when the summary's time
            // is before the cutoff: the summary holds that message's
            // preview.
            conversations: delete(
                self.conversations_collection(),
                older(s::SUMMARY_LAST_MESSAGE_AT),
            )
            .await?,
            window_events: delete(self.window_events_collection(), older(s::WINDOW_EVENT_TS))
                .await?,
            thread_owners: delete(self.owners_collection(), older(s::OWNER_SINCE)).await?,
            contact_removals: 0,
        };
        let mut removals = older(s::CONTACT_SYNCED_AT);
        removals.insert(s::CONTACT_REMOVED, true);
        purged.contact_removals = delete(self.contacts_collection(), removals).await?;
        Ok(purged)
    }

    fn retention(&self) -> Retention {
        self.retention
    }

    async fn trim_conversations(
        &self,
        phone_number_id: Option<&PhoneNumberId>,
        keep_last: usize,
    ) -> Result<Purged, StorageError> {
        let mut purged = Purged::default();
        // No conversation holds more rows than MongoDB counts.
        let Ok(keep) = i64::try_from(keep_last) else {
            return Ok(purged);
        };
        let messages = self.messages_collection();
        // The conversations over the cap, each row counting (tombstones
        // and redacted rows included).
        let mut over = Vec::new();
        let mut cursor = messages
            .aggregate([
                doc! { "$match": scope(phone_number_id) },
                doc! { "$group": {
                    "_id": { "p": format!("${}", s::PHONE_NUMBER_ID), "c": format!("${}", s::CONTACT) },
                    "rows": { "$sum": 1_i64 },
                } },
                doc! { "$match": { "rows": { "$gt": keep } } },
            ])
            .await
            .map_err(backend)?;
        while cursor.advance().await.map_err(backend)? {
            let row = cursor.deserialize_current().map_err(backend)?;
            let key = row
                .get_document("_id")
                .map_err(|_| corrupt(s::MESSAGES, s::CONTACT))?;
            over.push(ConversationKey {
                phone_number_id: PhoneNumberId::new_unchecked(str_of(key, s::MESSAGES, "p")?),
                contact: str_of(key, s::MESSAGES, "c")?.to_owned(),
            });
        }
        let skip = u64::try_from(keep).unwrap_or(u64::MAX);
        for key in over {
            // The rows beyond the newest `keep_last`, as read now: a row
            // appended meanwhile is not among them, so never deleted.
            let mut doomed: Vec<String> = Vec::new();
            let mut cursor = messages
                .find(key_filter(&key))
                .sort(doc! { s::MESSAGE_TS: -1, "_id": -1 })
                .skip(skip)
                .projection(doc! { "_id": 1 })
                .await
                .map_err(backend)?;
            while cursor.advance().await.map_err(backend)? {
                let row = cursor.deserialize_current().map_err(backend)?;
                doomed.push(str_of(&row, s::MESSAGES, "_id")?.to_owned());
            }
            for chunk in doomed.chunks(TRIM_CHUNK) {
                let mut filter = key_filter(&key);
                filter.insert("_id", doc! { "$in": chunk.to_vec() });
                purged.messages += messages
                    .delete_many(filter)
                    .await
                    .map_err(backend)?
                    .deleted_count;
            }
            // The summary goes when its latest message did: one an append
            // moved to a newer message meanwhile no longer matches.
            for chunk in doomed.chunks(TRIM_CHUNK) {
                let mut filter = key_filter(&key);
                filter.insert(s::SUMMARY_LAST_MESSAGE_ID, doc! { "$in": chunk.to_vec() });
                purged.conversations += self
                    .conversations_collection()
                    .delete_one(filter)
                    .await
                    .map_err(backend)?
                    .deleted_count;
            }
        }
        Ok(purged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const AT: OffsetDateTime = datetime!(2026-09-24 12:00 UTC);

    fn millis(t: OffsetDateTime) -> bson::DateTime {
        bson::DateTime::from_millis(i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap())
    }

    fn key() -> ConversationKey {
        ConversationKey::new(PhoneNumberId::new("106540352242922").unwrap(), "US.1")
    }

    fn inbound() -> StoredMessage {
        StoredMessage {
            id: MessageId::new("wamid.1"),
            conversation: key(),
            direction: Direction::Inbound,
            kind: "te\0xt".to_owned(),
            text: Some("hi\0there".to_owned()),
            payload: serde_json::json!({
                "from": "16505551234",
                "from_user_id": "US.1",
                "a\u{0}b": "c\u{0}d",
                "ab": 1.5,
            }),
            status: DeliveryStatus::Read,
            timestamp: AT,
            status_at: Some(AT + time::Duration::seconds(1)),
            error: Some(serde_json::json!({"code": 131047, "title": "x\u{0}"})),
        }
    }

    /// A message's document, field by field: the stored names of
    /// `schema`, content as BSON strings (U+0000 kept), the payload and the
    /// error as their JSON text, instants as BSON dates, and the sender
    /// beside them. It reads back as it was written.
    #[test]
    fn a_message_document_is_pinned_and_reads_back() {
        let message = inbound();
        let document = message_document(&message).unwrap();
        assert_eq!(
            document,
            doc! {
                "_id": "wamid.1",
                "phone_number_id": "106540352242922",
                "contact": "US.1",
                "direction": "inbound",
                "kind": "te\0xt",
                "payload_json": serde_json::to_string(&message.payload).unwrap(),
                "status": "read",
                "ts": millis(AT),
                "text": "hi\0there",
                "status_at": millis(AT + time::Duration::seconds(1)),
                "error_json": "{\"code\":131047,\"title\":\"x\\u0000\"}",
                "sender": "US.1",
            }
        );
        assert_eq!(message_of(&document).unwrap(), message);
        // The BSON itself keeps the NUL (a string's bytes, not a cstring).
        let bytes = document.to_vec().unwrap();
        let back = Document::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(message_of(&back).unwrap(), message);
    }

    /// A tombstone, an outbound message, a message without text or error:
    /// absent fields, not nulls, and no sender.
    #[test]
    fn absent_fields_are_absent() {
        let tombstone =
            StoredMessage::tombstone(&key(), &MessageId::new("wamid.2"), Direction::Outbound, AT);
        let document = message_document(&tombstone).unwrap();
        assert_eq!(
            document,
            doc! {
                "_id": "wamid.2",
                "phone_number_id": "106540352242922",
                "contact": "US.1",
                "direction": "outbound",
                "kind": "revoked",
                "payload_json": "{}",
                "status": "deleted",
                "ts": millis(AT),
                "status_at": millis(AT),
            }
        );
        assert_eq!(message_of(&document).unwrap(), tombstone);
        let mut outbound = inbound();
        outbound.direction = Direction::Outbound;
        assert!(
            !message_document(&outbound).unwrap().contains_key("sender"),
            "an outbound message's sender is the business: none stored"
        );
    }

    #[test]
    fn a_corrupt_message_is_an_error_naming_the_field() {
        let mut document = message_document(&inbound()).unwrap();
        document.insert("payload_json", "{not json");
        let error = message_of(&document).unwrap_err();
        assert!(
            matches!(&error, StorageError::Corrupt { key, .. } if key == "wa_messages.payload_json"),
            "{error:?}"
        );
        let mut document = message_document(&inbound()).unwrap();
        document.insert("status", "lost");
        assert!(message_of(&document).is_err());
        let mut document = message_document(&inbound()).unwrap();
        document.insert("direction", "sideways");
        assert!(message_of(&document).is_err());
    }

    fn touch(id: &str, secs: i64, text: Option<&str>, live: bool, direction: Direction) -> Touch {
        let message = StoredMessage {
            id: MessageId::new(id),
            direction,
            text: text.map(str::to_owned),
            timestamp: AT + time::Duration::seconds(secs),
            ..inbound()
        };
        Touch::new(&message, live)
    }

    fn stored_summary() -> Document {
        doc! {
            "_id": bson::oid::ObjectId::new(),
            "phone_number_id": "106540352242922",
            "contact": "US.1",
            "last_message_at": millis(AT),
            "last_message_id": "wamid.5",
            "last_text": "five",
            "last_inbound_at": millis(AT),
            "unread": 3_i64,
            "appends": 9_i64,
        }
    }

    /// A conversation's first message creates its summary.
    #[test]
    fn a_first_message_creates_the_summary() {
        let write = summary_write(
            &key(),
            None,
            touch("wamid.1", 0, Some("hi"), true, Direction::Inbound),
        );
        assert_eq!(
            write.unwrap(),
            SummaryWrite::Insert(doc! {
                "phone_number_id": "106540352242922",
                "contact": "US.1",
                "last_message_at": millis(AT),
                "last_message_id": "wamid.1",
                "last_text": "hi",
                "last_inbound_at": millis(AT),
                "unread": 1_i64,
                "appends": 1_i64,
            })
        );
        let synced = summary_write(
            &key(),
            None,
            touch("wamid.1", 0, None, false, Direction::Inbound),
        );
        assert_eq!(
            synced.unwrap(),
            SummaryWrite::Insert(doc! {
                "phone_number_id": "106540352242922",
                "contact": "US.1",
                "last_message_at": millis(AT),
                "last_message_id": "wamid.1",
                "unread": 0_i64,
                "appends": 1_i64,
            }),
            "synced history opens no window and is never unread"
        );
    }

    /// Decisive for the ordering against an erasure: an append that
    /// changes nothing else of the summary (an older synced message) still
    /// writes it. Without the `appends` increment it would be no write at
    /// all, and an erasure could delete the summary while the message
    /// stays.
    #[test]
    fn every_append_writes_the_summary() {
        let stored = stored_summary();
        let older = touch("wamid.0", -10, Some("old"), false, Direction::Inbound);
        assert_eq!(
            summary_write(&key(), Some(&stored), older).unwrap(),
            SummaryWrite::Update(doc! { "$inc": { "unread": 0_i64, "appends": 1_i64 } })
        );
    }

    #[test]
    fn a_newer_message_moves_the_summary() {
        let stored = stored_summary();
        let newer = touch("wamid.6", 5, Some("six"), true, Direction::Inbound);
        assert_eq!(
            summary_write(&key(), Some(&stored), newer).unwrap(),
            SummaryWrite::Update(doc! {
                "$inc": { "unread": 1_i64, "appends": 1_i64 },
                "$set": {
                    "last_message_at": millis(AT + time::Duration::seconds(5)),
                    "last_message_id": "wamid.6",
                    "last_text": "six",
                    "last_inbound_at": millis(AT + time::Duration::seconds(5)),
                },
            })
        );
        // The same second, a larger id: newer; no text: the preview goes.
        let tie = touch("wamid.9", 0, None, false, Direction::Outbound);
        assert_eq!(
            summary_write(&key(), Some(&stored), tie).unwrap(),
            SummaryWrite::Update(doc! {
                "$inc": { "unread": 0_i64, "appends": 1_i64 },
                "$set": { "last_message_at": millis(AT), "last_message_id": "wamid.9" },
                "$unset": { "last_text": "" },
            })
        );
        // A late live inbound message: unread, the window kept at the later
        // time, the latest message kept.
        let late = touch("wamid.0", -10, Some("late"), true, Direction::Inbound);
        assert_eq!(
            summary_write(&key(), Some(&stored), late).unwrap(),
            SummaryWrite::Update(doc! { "$inc": { "unread": 1_i64, "appends": 1_i64 } })
        );
    }

    #[test]
    fn a_batch_touch_follows_its_newest_message() {
        let mut batch = touch("wamid.1", 0, Some("one"), true, Direction::Inbound);
        let later = StoredMessage {
            id: MessageId::new("wamid.3"),
            text: None,
            timestamp: AT + time::Duration::seconds(3),
            direction: Direction::Outbound,
            ..inbound()
        };
        batch.add(&later, true);
        let between = StoredMessage {
            id: MessageId::new("wamid.2"),
            timestamp: AT + time::Duration::seconds(2),
            ..inbound()
        };
        batch.add(&between, true);
        assert_eq!(
            (batch.at, batch.id.as_str(), batch.text.as_deref()),
            (
                millis(AT + time::Duration::seconds(3)).timestamp_millis(),
                "wamid.3",
                None
            )
        );
        assert_eq!(
            (batch.inbound_at, batch.unread),
            (
                Some(millis(AT + time::Duration::seconds(2)).timestamp_millis()),
                2
            ),
            "the outbound message neither opens the window nor counts"
        );
    }

    /// The update filters list the statuses core's rule supersedes.
    #[test]
    fn a_status_replaces_only_what_it_supersedes() {
        assert_eq!(
            superseded_by(DeliveryStatus::Read).unwrap(),
            ["received", "accepted", "sent", "delivered"]
        );
        assert_eq!(
            superseded_by(DeliveryStatus::Deleted).unwrap(),
            [
                "received",
                "accepted",
                "sent",
                "delivered",
                "read",
                "played"
            ]
        );
        assert!(superseded_by(DeliveryStatus::Received).unwrap().is_empty());
        for status in STATUSES {
            assert_eq!(status_of(&status_name(status).unwrap()).unwrap(), status);
        }
    }

    #[test]
    fn a_redaction_keeps_the_place_and_drops_the_content() {
        assert_eq!(
            redaction(),
            doc! {
                "$set": { "kind": "erased", "payload_json": "{}" },
                "$unset": { "text": "", "error_json": "", "sender": "" },
            }
        );
    }

    #[test]
    fn a_cursor_is_exclusive_on_instant_then_id() {
        let mut filter = key_filter(&key());
        before_filter(&mut filter, "ts", "_id", AT, "wamid.5");
        assert_eq!(
            filter,
            doc! {
                "phone_number_id": "106540352242922",
                "contact": "US.1",
                "$or": [
                    { "ts": { "$lt": millis(AT) } },
                    { "ts": millis(AT), "_id": { "$lt": "wamid.5" } },
                ],
            }
        );
        assert_eq!(scope(None), doc! {});
        assert_eq!(
            scope(Some(&PhoneNumberId::new("1").unwrap())),
            doc! { "phone_number_id": "1" }
        );
    }

    /// The records beside the history, as stored and read back.
    #[test]
    fn the_other_records_are_pinned_and_read_back() {
        let event = WindowEvent {
            conversation: key(),
            kind: WindowEventKind::CustomerCall,
            id: "wacid.1".to_owned(),
            at: AT,
        };
        let document = window_event_document(&event);
        assert_eq!(
            document,
            doc! {
                "phone_number_id": "106540352242922",
                "id": "wacid.1",
                "contact": "US.1",
                "kind": "customer_call",
                "ts": millis(AT),
            }
        );
        assert_eq!(window_event_of(&document).unwrap(), event);

        let ownership = ThreadOwnership {
            owner: ThreadOwner::AnotherApp,
            role: Some("escalation".to_owned()),
            app_id: Some(AppId::new("123").unwrap()),
            since: AT,
        };
        let document = owner_document(&key(), &ownership).unwrap();
        assert_eq!(
            document,
            doc! {
                "phone_number_id": "106540352242922",
                "contact": "US.1",
                "owner": "another_app",
                "role": "escalation",
                "app_id": "123",
                "since": millis(AT),
            }
        );
        assert_eq!(owner_of_document(&document).unwrap(), ownership);
        let idle = ThreadOwnership {
            owner: ThreadOwner::Idle,
            role: None,
            app_id: None,
            since: AT,
        };
        assert_eq!(
            owner_of_document(&owner_document(&key(), &idle).unwrap()).unwrap(),
            idle
        );

        let contact = StoredContact {
            key: key(),
            full_name: Some("Pablo\0Morales".to_owned()),
            first_name: None,
            phone_number: Some("16505551234".to_owned()),
            user_id: Some(UserId::new("US.1")),
            parent_user_id: None,
            username: Some("pablo".to_owned()),
            synced_at: AT,
        };
        let document = contact_document(&contact);
        assert_eq!(
            document,
            doc! {
                "phone_number_id": "106540352242922",
                "contact": "US.1",
                "full_name": "Pablo\0Morales",
                "phone_number": "16505551234",
                "user_id": "US.1",
                "username": "pablo",
                "synced_at": millis(AT),
                "removed": false,
            }
        );
        assert_eq!(contact_of(&document).unwrap(), contact);
        assert_eq!(
            removal_document(&key(), AT),
            doc! {
                "phone_number_id": "106540352242922",
                "contact": "US.1",
                "synced_at": millis(AT),
                "removed": true,
            },
            "a kept removal holds nothing of the contact"
        );

        let link = IdentityLink::new(
            PhoneNumberId::new("106540352242922").unwrap(),
            "US.0",
            "US.1",
            AT,
        );
        let document = link_document(&link);
        assert_eq!(
            document,
            doc! {
                "phone_number_id": "106540352242922",
                "previous": "US.0",
                "current": "US.1",
                "ts": millis(AT),
            }
        );
        assert_eq!(link_of(&document).unwrap(), link);
    }

    #[test]
    fn a_summary_reads_back() {
        assert_eq!(
            summary_of(&stored_summary()).unwrap(),
            ConversationSummary {
                key: key(),
                last_message_at: AT,
                last_inbound_at: Some(AT),
                last_text: Some("five".to_owned()),
                unread: 3,
            }
        );
    }

    /// Neither store prints more than its database's name and settings:
    /// no client options (credentials), no message.
    #[tokio::test]
    async fn debug_shows_the_database_name_only() {
        use ::mongodb::options::{ClientOptions, ServerAddress};
        let options = ClientOptions::builder()
            .hosts(vec![ServerAddress::Tcp {
                host: "127.0.0.1".to_owned(),
                port: Some(9),
            }])
            .app_name("secret-app-name".to_owned())
            .build();
        let client = ::mongodb::Client::with_options(options).unwrap();
        let database = client.database("wa_test_debug");
        assert_eq!(
            format!("{:?}", MongoConversationStore::new(database.clone())),
            "MongoConversationStore { database: \"wa_test_debug\", retention: Keep, \
             erasure_mode: Redact, busy_after: 10s, .. }"
        );
        assert_eq!(
            format!("{:?}", super::super::MongoKvStore::new(database)),
            "MongoKvStore { database: \"wa_test_debug\", .. }"
        );
    }
}
