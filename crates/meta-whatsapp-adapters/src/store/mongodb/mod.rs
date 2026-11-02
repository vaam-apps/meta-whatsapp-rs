//! MongoDB adapters (feature `mongodb`, roadmap S17): [`MongoKvStore`] and
//! [`MongoConversationStore`] over a [`Database`] of a
//! replica set.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use meta_whatsapp_adapters::store::mongodb::{self, MongoConversationStore, MongoKvStore};
//!
//! let client = mongodb::mongodb::Client::with_uri_str(&std::env::var("MONGODB_URL")?).await?;
//! let database = client.database("whatsapp");
//! mongodb::migrate(&database).await?; // idempotent; run it at startup
//! let kv = MongoKvStore::new(database.clone());
//! let inbox = MongoConversationStore::new(database);
//! # Ok(()) }
//! ```
//!
//! # A replica set
//!
//! [`MongoConversationStore`] writes what touches several documents (a
//! message and its conversation's summary, an erasure) in multi-document
//! transactions, which MongoDB runs on a replica set or a sharded cluster
//! only: a standalone server refuses them. A single-node replica set is
//! enough (the dev container's and CI's is one, roadmap S8).
//! [`MongoKvStore`] writes one document per operation and needs no
//! transaction, but is tested on the same replica set. Every write is
//! acknowledged by a majority of the set (`w: "majority"`), so an
//! acknowledged write survives a failover.
//!
//! # Collections and indexes
//!
//! [`migrate`] creates the collections and their indexes, every name in
//! [`schema`]: `wa_kv` for the key/value records, and `wa_messages`,
//! `wa_conversations`, `wa_window_events`, `wa_thread_owners`,
//! `wa_synced_contacts` and `wa_identity_links` for the conversations (the
//! Postgres adapter's table names). It is safe to run again, and from two
//! instances at once: an existing collection, or an identical index, is
//! left as it is. There is no name prefix: give each deployment a database
//! of its own.
//!
//! # The database server's clock is the clock
//!
//! Expiry is evaluated with the server's `$$NOW`, never with the
//! application's clock, as on Postgres: `Expiry::After(d)` means `d` after
//! the server's time, and a record is invisible once the server's time
//! reaches `expires_at`. MongoDB keeps **milliseconds**: `expires_at`,
//! message timestamps and every other instant come back truncated to the
//! millisecond (an `Expiry::After` is rounded up to it, so a sub-millisecond
//! TTL is not dead on arrival). An `Expiry::After` that would land after
//! year 9999 is stored as "never", as `MemoryKvStore` does.
//!
//! # Content keeps U+0000
//!
//! A BSON string holds U+0000 (a field name cannot), so a message's `kind`
//! and `text`, the summary's preview and a synced contact's names are BSON
//! strings, read back exactly; a payload and a status error are stored as
//! their JSON text (`payload_json`, `error_json`, BSON strings), because a
//! JSON object key may hold U+0000 and a BSON field name may not. Query
//! them in your application, not with MongoDB's operators on the payload's
//! fields: there are none. Identifiers (message ids, contacts, phone
//! number ids, a [`StoreKey`](meta_whatsapp_core::store::StoreKey)'s
//! namespace and key) are BSON strings too, kept exactly, U+0000 included.
//!
//! Ordering never involves content: history pages by `(ts, _id)`, the inbox
//! by `(last_message_at, contact)`, strings compared byte by byte (MongoDB's
//! default, without a collation: the order of Rust's `str`).
//!
//! # What an erasure leaves
//!
//! Every collection the adapter keeps about a contact is keyed by business
//! number and contact (a link by business number and its two identities),
//! and `ConversationStore::erase_all` deletes from each of them, and
//! redacts or deletes the person's group messages, in one transaction.
//! MongoDB's own remnants are outside its reach: the oplog (a capped
//! collection, overwritten as it fills), replicas being caught up, backups.

mod conversation;
mod kv;
pub mod schema;

use std::time::Duration;

/// The MongoDB driver these adapters are built against: they take its
/// [`Database`], so build clients with this re-export
/// rather than pinning the driver yourself (it is `mongodb` 3.9, with
/// `rustls-tls-aws-lc`, `compat-3-3-0` and `bson-3`).
pub use ::mongodb;
pub use conversation::{BUSY_AFTER, MongoConversationStore};
pub use kv::MongoKvStore;

use ::mongodb::bson::{self, Document, doc};
use ::mongodb::error::{ErrorKind, WriteFailure};
use ::mongodb::options::{CollectionOptions, IndexOptions, WriteConcern};
use ::mongodb::{Collection, Database, IndexModel};
use meta_whatsapp_core::error::StorageError;
use time::OffsetDateTime;

use schema as s;

/// MongoDB's `DuplicateKey`.
const DUPLICATE_KEY: i32 = 11000;

/// MongoDB's `NamespaceExists`: `create_collection` of one that exists.
const NAMESPACE_EXISTS: i32 = 48;

/// 9999-12-31T23:59:59.999Z in unix milliseconds: the latest instant
/// `OffsetDateTime` represents, so every stored instant reads back.
const MAX_MILLIS: i64 = 253_402_300_799_999;

/// Create the collections and indexes both adapters store in ([`schema`]).
/// Run it once at startup, before the first read or write; safe to run
/// again, and on two instances at once.
///
/// # Errors
///
/// `StorageError::Backend` when the database refuses a collection or an
/// index (an index of the same name with other keys or options among
/// them).
pub async fn migrate(database: &Database) -> Result<(), StorageError> {
    for name in [
        s::KV,
        s::MESSAGES,
        s::CONVERSATIONS,
        s::WINDOW_EVENTS,
        s::THREAD_OWNERS,
        s::SYNCED_CONTACTS,
        s::IDENTITY_LINKS,
    ] {
        match database.create_collection(name).await {
            Ok(()) => {}
            Err(error) if code_of(&error) == Some(NAMESPACE_EXISTS) => {}
            Err(error) => return Err(backend(error)),
        }
    }
    for (collection, indexes) in indexes() {
        database
            .collection::<Document>(collection)
            .create_indexes(indexes)
            .await
            .map_err(backend)?;
    }
    Ok(())
}

/// Every index [`migrate`] creates, by collection.
#[allow(clippy::too_many_lines)] // one list, in the order of `schema`
fn indexes() -> Vec<(&'static str, Vec<IndexModel>)> {
    let index = |keys: Document, options: IndexOptions| {
        IndexModel::builder().keys(keys).options(options).build()
    };
    let named = |name: &str| IndexOptions::builder().name(name.to_owned()).build();
    let unique = |name: &str| {
        IndexOptions::builder()
            .name(name.to_owned())
            .unique(true)
            .build()
    };
    vec![
        (
            s::KV,
            vec![index(
                doc! { s::KV_PURGE_AFTER: 1 },
                IndexOptions::builder()
                    .name(s::INDEX_KV_PURGE.to_owned())
                    .expire_after(Duration::ZERO)
                    .build(),
            )],
        ),
        (
            s::MESSAGES,
            vec![
                index(
                    doc! { s::PHONE_NUMBER_ID: 1, s::CONTACT: 1, s::MESSAGE_TS: -1, "_id": -1 },
                    named(s::INDEX_MESSAGES_CONVERSATION),
                ),
                index(doc! { s::MESSAGE_TS: 1 }, named(s::INDEX_MESSAGES_TS)),
                // `$exists`, which an `$in` of strings implies, so that an
                // erasure's `$in` alone gives the index's bounds.
                index(
                    doc! { s::PHONE_NUMBER_ID: 1, s::MESSAGE_SENDER: 1 },
                    IndexOptions::builder()
                        .name(s::INDEX_MESSAGES_SENDER.to_owned())
                        .partial_filter_expression(doc! { s::MESSAGE_SENDER: { "$exists": true } })
                        .build(),
                ),
            ],
        ),
        (
            s::CONVERSATIONS,
            vec![
                index(
                    doc! { s::PHONE_NUMBER_ID: 1, s::CONTACT: 1 },
                    unique(s::INDEX_CONVERSATIONS_KEY),
                ),
                index(
                    doc! {
                        s::PHONE_NUMBER_ID: 1,
                        s::SUMMARY_LAST_MESSAGE_AT: -1,
                        s::CONTACT: -1,
                    },
                    named(s::INDEX_CONVERSATIONS_RECENT),
                ),
                index(
                    doc! { s::SUMMARY_LAST_MESSAGE_AT: 1 },
                    named(s::INDEX_CONVERSATIONS_LAST),
                ),
            ],
        ),
        (
            s::WINDOW_EVENTS,
            vec![
                index(
                    doc! { s::PHONE_NUMBER_ID: 1, s::WINDOW_EVENT_ID: 1 },
                    unique(s::INDEX_WINDOW_EVENTS_KEY),
                ),
                index(
                    doc! {
                        s::PHONE_NUMBER_ID: 1,
                        s::CONTACT: 1,
                        s::WINDOW_EVENT_TS: -1,
                        s::WINDOW_EVENT_ID: -1,
                    },
                    named(s::INDEX_WINDOW_EVENTS_CONVERSATION),
                ),
                index(
                    doc! { s::WINDOW_EVENT_TS: 1 },
                    named(s::INDEX_WINDOW_EVENTS_TS),
                ),
            ],
        ),
        (
            s::THREAD_OWNERS,
            vec![index(
                doc! { s::PHONE_NUMBER_ID: 1, s::CONTACT: 1 },
                unique(s::INDEX_THREAD_OWNERS_KEY),
            )],
        ),
        (
            s::SYNCED_CONTACTS,
            vec![
                index(
                    doc! { s::PHONE_NUMBER_ID: 1, s::CONTACT: 1 },
                    unique(s::INDEX_SYNCED_CONTACTS_KEY),
                ),
                index(
                    doc! { s::CONTACT_SYNCED_AT: 1 },
                    IndexOptions::builder()
                        .name(s::INDEX_CONTACT_REMOVALS.to_owned())
                        .partial_filter_expression(doc! { s::CONTACT_REMOVED: true })
                        .build(),
                ),
            ],
        ),
        (
            s::IDENTITY_LINKS,
            vec![
                index(
                    doc! { s::PHONE_NUMBER_ID: 1, s::LINK_PREVIOUS: 1, s::LINK_CURRENT: 1 },
                    unique(s::INDEX_IDENTITY_LINKS_KEY),
                ),
                index(
                    doc! { s::PHONE_NUMBER_ID: 1, s::LINK_CURRENT: 1 },
                    named(s::INDEX_IDENTITY_LINKS_CURRENT),
                ),
            ],
        ),
    ]
}

/// Collection `name` of `database`, its writes acknowledged by a majority.
fn collection(database: &Database, name: &str) -> Collection<Document> {
    database.collection_with_options(
        name,
        CollectionOptions::builder()
            .write_concern(WriteConcern::majority())
            .build(),
    )
}

fn backend(error: impl std::error::Error + Send + Sync + 'static) -> StorageError {
    StorageError::Backend(anyhow::Error::new(error))
}

/// A stored document that does not read as this adapter wrote it (only a
/// hand edit, or another program, makes one).
fn corrupt(collection: &str, field: &str) -> StorageError {
    StorageError::Backend(anyhow::anyhow!(
        "unreadable value of `{field}` in `{collection}`"
    ))
}

/// The server's error code, when it answered one.
fn code_of(error: &::mongodb::error::Error) -> Option<i32> {
    match &*error.kind {
        ErrorKind::Command(command) => Some(command.code),
        ErrorKind::Write(WriteFailure::WriteError(write)) => Some(write.code),
        ErrorKind::InsertMany(many) => many
            .write_errors
            .as_ref()
            .and_then(|errors| errors.first())
            .map(|error| error.code),
        _ => None,
    }
}

/// Whether `error` is a duplicate key: an insert of a unique key stored
/// already.
fn is_duplicate_key(error: &::mongodb::error::Error) -> bool {
    code_of(error) == Some(DUPLICATE_KEY)
}

/// `t` as a BSON date, its milliseconds rounded down (what every stored
/// instant is: MongoDB keeps milliseconds).
fn date(t: OffsetDateTime) -> bson::DateTime {
    let millis = t.unix_timestamp_nanos().div_euclid(1_000_000);
    bson::DateTime::from_millis(i64::try_from(millis).unwrap_or(i64::MAX))
}

/// `t` as a BSON date, its milliseconds rounded up: a purge's cutoff, so
/// that a record stored at the millisecond below an instant inside it,
/// which may be older than the cutoff, is purged.
fn date_ceil(t: OffsetDateTime) -> bson::DateTime {
    let nanos = t.unix_timestamp_nanos();
    let millis = nanos.div_euclid(1_000_000) + i128::from(nanos.rem_euclid(1_000_000) > 0);
    bson::DateTime::from_millis(i64::try_from(millis).unwrap_or(i64::MAX))
}

/// A stored BSON date as an instant.
fn time_of(
    date: bson::DateTime,
    collection: &str,
    field: &str,
) -> Result<OffsetDateTime, StorageError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(date.timestamp_millis()) * 1_000_000)
        .map_err(|_| corrupt(collection, field))
}

/// A limit as MongoDB takes it: `None` for one no collection reaches
/// (MongoDB reads `0` as no limit, so a zero limit is the caller's to
/// answer before asking).
fn limit_of(limit: usize) -> Option<i64> {
    i64::try_from(limit).ok().filter(|n| *n < i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn instants_are_milliseconds_rounded_down_or_up() {
        let t = datetime!(2026-09-24 12:00:00.000_999_999 UTC);
        assert_eq!(date(t).timestamp_millis(), 1_790_251_200_000);
        assert_eq!(date_ceil(t).timestamp_millis(), 1_790_251_200_001);
        let whole = datetime!(2026-09-24 12:00:00.123 UTC);
        assert_eq!(date(whole), date_ceil(whole), "a whole millisecond is kept");
        let before_epoch = datetime!(1969-12-31 23:59:59.999_5 UTC);
        assert_eq!(date(before_epoch).timestamp_millis(), -1);
        assert_eq!(date_ceil(before_epoch).timestamp_millis(), 0);
        assert_eq!(
            time_of(bson::DateTime::from_millis(MAX_MILLIS), "c", "f").unwrap(),
            datetime!(9999-12-31 23:59:59.999 UTC)
        );
        assert!(time_of(bson::DateTime::MAX, "c", "f").is_err());
    }

    #[test]
    fn limits_past_what_mongodb_counts_are_none() {
        assert_eq!(limit_of(10), Some(10));
        assert_eq!(limit_of(usize::MAX), None);
        assert_eq!(limit_of(usize::try_from(i64::MAX).unwrap()), None);
    }

    /// The indexes [`migrate`] creates, by name, keys and options: the
    /// stored names of [`schema`], on the fields they index.
    #[test]
    #[allow(clippy::too_many_lines)] // one list
    fn the_indexes_are_pinned() {
        let found: Vec<(String, String, Document, Option<Document>, bool, bool)> = indexes()
            .into_iter()
            .flat_map(|(collection, models)| {
                models.into_iter().map(move |model| {
                    let options = model.options.unwrap();
                    (
                        collection.to_owned(),
                        options.name.unwrap(),
                        model.keys,
                        options.partial_filter_expression,
                        options.unique.unwrap_or(false),
                        options.expire_after == Some(Duration::ZERO),
                    )
                })
            })
            .collect();
        let expected = [
            (
                "wa_kv",
                "wa_kv_purge_idx",
                doc! { "purge_after": 1 },
                None,
                false,
                true,
            ),
            (
                "wa_messages",
                "wa_messages_conversation_idx",
                doc! { "phone_number_id": 1, "contact": 1, "ts": -1, "_id": -1 },
                None,
                false,
                false,
            ),
            (
                "wa_messages",
                "wa_messages_ts_idx",
                doc! { "ts": 1 },
                None,
                false,
                false,
            ),
            (
                "wa_messages",
                "wa_messages_sender_idx",
                doc! { "phone_number_id": 1, "sender": 1 },
                Some(doc! { "sender": { "$exists": true } }),
                false,
                false,
            ),
            (
                "wa_conversations",
                "wa_conversations_key_idx",
                doc! { "phone_number_id": 1, "contact": 1 },
                None,
                true,
                false,
            ),
            (
                "wa_conversations",
                "wa_conversations_recent_idx",
                doc! { "phone_number_id": 1, "last_message_at": -1, "contact": -1 },
                None,
                false,
                false,
            ),
            (
                "wa_conversations",
                "wa_conversations_last_idx",
                doc! { "last_message_at": 1 },
                None,
                false,
                false,
            ),
            (
                "wa_window_events",
                "wa_window_events_key_idx",
                doc! { "phone_number_id": 1, "id": 1 },
                None,
                true,
                false,
            ),
            (
                "wa_window_events",
                "wa_window_events_conv_idx",
                doc! { "phone_number_id": 1, "contact": 1, "ts": -1, "id": -1 },
                None,
                false,
                false,
            ),
            (
                "wa_window_events",
                "wa_window_events_ts_idx",
                doc! { "ts": 1 },
                None,
                false,
                false,
            ),
            (
                "wa_thread_owners",
                "wa_thread_owners_key_idx",
                doc! { "phone_number_id": 1, "contact": 1 },
                None,
                true,
                false,
            ),
            (
                "wa_synced_contacts",
                "wa_synced_contacts_key_idx",
                doc! { "phone_number_id": 1, "contact": 1 },
                None,
                true,
                false,
            ),
            (
                "wa_synced_contacts",
                "wa_contact_removals_idx",
                doc! { "synced_at": 1 },
                Some(doc! { "removed": true }),
                false,
                false,
            ),
            (
                "wa_identity_links",
                "wa_identity_links_key_idx",
                doc! { "phone_number_id": 1, "previous": 1, "current": 1 },
                None,
                true,
                false,
            ),
            (
                "wa_identity_links",
                "wa_identity_links_idx",
                doc! { "phone_number_id": 1, "current": 1 },
                None,
                false,
                false,
            ),
        ];
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(c, n, k, p, u, t)| (c.to_owned(), n.to_owned(), k, p, u, t))
            .collect();
        assert_eq!(found, expected);
    }
}
