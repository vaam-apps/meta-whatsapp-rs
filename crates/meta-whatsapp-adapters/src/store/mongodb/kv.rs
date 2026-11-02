//! [`KvStore`] on MongoDB.
//!
//! Every operation is **one document's** update (or read), atomic on its
//! own, so the store needs no transaction:
//!
//! - `put` is an upsert whose update is an aggregation pipeline: the new
//!   version, value and expiry are computed by the server from the
//!   document as it was, under its write lock.
//! - `put_if_absent` is the same upsert, filtered on the record being dead
//!   (no value, or expired by `$$NOW`). A live record makes the filter miss
//!   and the upsert's insert collide with it (a duplicate `_id`), which is
//!   the answer "present"; the store reads the key to tell that from a
//!   racer that inserted first (it then tries again).
//! - `compare_and_swap` is a `findOneAndUpdate` filtered on the expected
//!   version *and* on the record being live: of N racers on one version,
//!   one matches, and the others find the version moved.
//! - `delete` (and a compare-and-swap to `None`) unsets the value and
//!   keeps the version: the document stays as a tombstone, so the next
//!   write goes on from that version.
//!
//! # Versions
//!
//! A write's version is the larger of the document's version plus one and
//! the server's clock in milliseconds times 2^20 ([`VERSION_SCALE`]). The
//! first term makes successive versions of a document strictly increase,
//! the second keeps a key's versions increasing across the removal of its
//! document: dead documents (tombstones, expired records) are removed by
//! the TTL index [`schema::INDEX_KV_PURGE`] once their grace period
//! ([`PURGE_GRACE`], ten minutes, as on Postgres) is over, and a key
//! written again after that starts from the clock, past every version its
//! earlier document handed out (they stay below the clock's term of the
//! millisecond after their last write, unless one key was written more
//! than 2^20 times in a millisecond). What that relies on: the primary's
//! clock never goes back by more than the grace period, across a failover
//! included (keep the members on NTP). The clock's term fits a signed
//! 64-bit integer until the year 2248.
//!
//! [`schema::INDEX_KV_PURGE`]: super::schema::INDEX_KV_PURGE

use std::fmt;
use std::time::Duration;

use ::mongodb::bson::spec::BinarySubtype;
use ::mongodb::bson::{Binary, Bson, Document, doc};
use ::mongodb::options::ReturnDocument;
use ::mongodb::{Collection, Database};
use async_trait::async_trait;
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};

use super::schema as s;
use super::{MAX_MILLIS, backend, collection, corrupt, date, is_duplicate_key, time_of};

/// How long a dead record's document (deleted, or expired) stays before the
/// TTL index may remove it: what keeps a key's versions increasing without
/// the clock while it lasts, and the margin the clock's term is given
/// against a primary's clock going back.
pub(super) const PURGE_GRACE: Duration = Duration::from_secs(600);

/// The clock's weight in a version: `$$NOW` in milliseconds times this.
pub(super) const VERSION_SCALE: i64 = 1 << 20;

/// How many times `put_if_absent` tries again after losing an insert to a
/// racer whose record is dead by the time it looks: more than any real
/// contention produces.
const PUT_IF_ABSENT_ROUNDS: usize = 32;

/// `KvStore` over a MongoDB [`Database`] (collection `wa_kv`, created by
/// [`migrate`](super::migrate)). Cheap to clone (the driver's handles are).
///
/// Expiry uses the **database server's** clock (`$$NOW`), at millisecond
/// precision; see the [module docs](super#the-database-servers-clock-is-the-clock).
/// Dead records are removed by MongoDB's TTL monitor (about once a
/// minute), ten minutes after they died: nothing to schedule.
#[derive(Clone)]
pub struct MongoKvStore {
    database: Database,
    records: Collection<Document>,
}

impl fmt::Debug for MongoKvStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The database's name, never the client's options (they can carry
        // credentials).
        f.debug_struct("MongoKvStore")
            .field("database", &self.database.name())
            .finish_non_exhaustive()
    }
}

impl MongoKvStore {
    /// The store in `database`, which [`migrate`](super::migrate) prepared.
    pub fn new(database: Database) -> Self {
        let records = collection(&database, s::KV);
        Self { database, records }
    }

    /// The database it stores in.
    pub fn database(&self) -> &Database {
        &self.database
    }
}

/// A record's `_id`: its namespace and key, in that order (an `_id` that
/// is a document matches field by field, in order).
fn id(key: &StoreKey) -> Document {
    doc! { s::KV_NAMESPACE: key.namespace(), s::KV_KEY: key.key() }
}

/// `$<field>`, a field's value in an aggregation expression.
fn field(name: &str) -> String {
    format!("${name}")
}

/// Whether the record is live, as an aggregation expression: it holds a
/// value and has not expired by the server's clock.
fn live() -> Document {
    doc! { "$and": [
        { "$ne": [ { "$type": field(s::KV_VALUE) }, "missing" ] },
        { "$or": [
            { "$eq": [ { "$type": field(s::KV_EXPIRES_AT) }, "missing" ] },
            { "$gt": [ field(s::KV_EXPIRES_AT), "$$NOW" ] },
        ] },
    ] }
}

/// The filter of a record that is live.
fn live_filter(key: &StoreKey) -> Document {
    doc! { "_id": id(key), "$expr": live() }
}

/// The filter of a record that is not live (or does not exist).
fn dead_filter(key: &StoreKey) -> Document {
    doc! { "_id": id(key), "$expr": { "$not": [live()] } }
}

/// The filter of a live record at version `expected`.
fn version_filter(key: &StoreKey, expected: i64) -> Document {
    doc! { "_id": id(key), s::KV_VERSION: expected, "$expr": live() }
}

/// The new version: the record's plus one, or the server's clock term when
/// that is larger (see the module docs).
fn next_version() -> Document {
    doc! { "$max": [
        { "$add": [ { "$ifNull": [ field(s::KV_VERSION), 0_i64 ] }, 1_i64 ] },
        { "$multiply": [ { "$toLong": "$$NOW" }, VERSION_SCALE ] },
    ] }
}

/// The new `expires_at`, as an aggregation expression over the record as
/// it was (`$$REMOVE`: none).
fn new_expiry(expiry: Expiry) -> Bson {
    match expiry {
        Expiry::Never => Bson::String("$$REMOVE".to_owned()),
        Expiry::After(d) => Bson::Document(doc! { "$let": {
            "vars": { "at": { "$add": [ { "$toLong": "$$NOW" }, ttl_millis(d) ] } },
            // Past year 9999 (what `OffsetDateTime` can read back): never.
            "in": { "$cond": [ { "$gt": [ "$$at", MAX_MILLIS ] }, "$$REMOVE", { "$toDate": "$$at" } ] },
        } }),
        // Rounded down: the record never outlives the requested instant.
        Expiry::At(t) => Bson::DateTime(date(t)),
        // The live record's deadline; none (never) for a new or dead one.
        Expiry::Keep => Bson::Document(doc! {
            "$cond": [ live(), field(s::KV_EXPIRES_AT), "$$REMOVE" ]
        }),
    }
}

/// A TTL in milliseconds, rounded **up** so that a sub-millisecond TTL is
/// not dead on arrival, and capped at [`MAX_MILLIS`]: any longer TTL lands
/// after year 9999, which the pipeline turns into "never", and the cap
/// keeps its sum inside a signed 64-bit integer.
fn ttl_millis(d: Duration) -> i64 {
    let ms = d.as_nanos().div_ceil(1_000_000);
    i64::try_from(ms).map_or(MAX_MILLIS, |ms| ms.min(MAX_MILLIS))
}

fn grace_millis() -> i64 {
    i64::try_from(PURGE_GRACE.as_millis()).unwrap_or(i64::MAX)
}

/// The update of a write: value, version and expiry from the record as it
/// was (one stage, so every expression reads the old document), then when
/// the TTL index may remove it: the grace period after its expiry, or after
/// this write when it expired already (born expired), never for a record
/// that does not expire. A document so outlives its last write by the
/// grace period at least, which the versions rely on (module docs).
fn write_pipeline(value: &[u8], expiry: Expiry) -> Vec<Document> {
    let value = Binary {
        subtype: BinarySubtype::Generic,
        bytes: value.to_vec(),
    };
    vec![
        doc! { "$set": {
            s::KV_VALUE: { "$literal": value },
            s::KV_VERSION: next_version(),
            s::KV_EXPIRES_AT: new_expiry(expiry),
        } },
        doc! { "$set": {
            s::KV_PURGE_AFTER: { "$cond": [
                { "$eq": [ { "$type": field(s::KV_EXPIRES_AT) }, "missing" ] },
                "$$REMOVE",
                { "$add": [ { "$max": [ field(s::KV_EXPIRES_AT), "$$NOW" ] }, grace_millis() ] },
            ] },
        } },
    ]
}

/// The update of a delete: the value and expiry go, the version stays,
/// and the TTL index may remove the tombstone once the grace period is
/// over.
fn tombstone_pipeline() -> Vec<Document> {
    vec![
        doc! { "$set": { s::KV_PURGE_AFTER: { "$add": [ "$$NOW", grace_millis() ] } } },
        doc! { "$unset": [s::KV_VALUE, s::KV_EXPIRES_AT] },
    ]
}

/// A version as stored (a 64-bit integer, or a 32-bit one for a value
/// written by hand).
fn version_of(record: &Document) -> Result<u64, StorageError> {
    let version = match record.get(s::KV_VERSION) {
        Some(Bson::Int64(v)) => *v,
        Some(Bson::Int32(v)) => i64::from(*v),
        _ => return Err(corrupt(s::KV, s::KV_VERSION)),
    };
    u64::try_from(version).map_err(|_| corrupt(s::KV, s::KV_VERSION))
}

/// A live record as read.
fn versioned(record: &Document) -> Result<Versioned, StorageError> {
    let value = record
        .get_binary_generic(s::KV_VALUE)
        .map_err(|_| corrupt(s::KV, s::KV_VALUE))?
        .clone();
    let expires_at = match record.get(s::KV_EXPIRES_AT) {
        None => None,
        Some(Bson::DateTime(at)) => Some(time_of(*at, s::KV, s::KV_EXPIRES_AT)?),
        Some(_) => return Err(corrupt(s::KV, s::KV_EXPIRES_AT)),
    };
    Ok(Versioned {
        value,
        version: version_of(record)?,
        expires_at,
    })
}

impl MongoKvStore {
    /// Write `value` over whatever `filter` matches (or create the record),
    /// returning the new version: `None` when the filter missed a record
    /// that exists (the upsert's insert met its `_id`).
    async fn upsert(
        &self,
        filter: Document,
        value: &[u8],
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        match self
            .records
            .find_one_and_update(filter, write_pipeline(value, expiry))
            .upsert(true)
            .return_document(ReturnDocument::After)
            .projection(doc! { s::KV_VERSION: 1 })
            .await
        {
            Ok(Some(record)) => version_of(&record).map(Some),
            // An upsert always returns the document it wrote.
            Ok(None) => Err(corrupt(s::KV, s::KV_VERSION)),
            Err(error) if is_duplicate_key(&error) => Ok(None),
            Err(error) => Err(backend(error)),
        }
    }
}

#[async_trait]
impl KvStore for MongoKvStore {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        self.records
            .find_one(live_filter(key))
            .projection(doc! { s::KV_VALUE: 1, s::KV_VERSION: 1, s::KV_EXPIRES_AT: 1 })
            .await
            .map_err(backend)?
            .as_ref()
            .map(versioned)
            .transpose()
    }

    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        // Filtered on the `_id` alone, so it matches any document there is:
        // the insert of a concurrent first write collides only when both
        // insert, which MongoDB retries itself (an upsert on an equality of
        // `_id`), and the loop covers it if it does not.
        for _ in 0..PUT_IF_ABSENT_ROUNDS {
            if let Some(version) = self.upsert(doc! { "_id": id(key) }, &value, expiry).await? {
                return Ok(version);
            }
        }
        Err(StorageError::Busy)
    }

    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        for _ in 0..PUT_IF_ABSENT_ROUNDS {
            if let Some(version) = self.upsert(dead_filter(key), &value, expiry).await? {
                return Ok(Some(version));
            }
            // The insert met a document: a live one (the answer), or one a
            // racer inserted and that is dead already (born expired), when
            // the record is absent after all and the write goes again.
            if self.get(key).await?.is_some() {
                return Ok(None);
            }
        }
        Err(StorageError::Busy)
    }

    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        // No stored version is past `i64::MAX`.
        let Ok(expected) = i64::try_from(expected) else {
            return Ok(None);
        };
        let filter = version_filter(key, expected);
        #[allow(clippy::single_match_else)] // a swap, or a delete
        match new {
            Some(value) => self
                .records
                .find_one_and_update(filter, write_pipeline(&value, expiry))
                .return_document(ReturnDocument::After)
                .projection(doc! { s::KV_VERSION: 1 })
                .await
                .map_err(backend)?
                .as_ref()
                .map(version_of)
                .transpose(),
            None => {
                let deleted = self
                    .records
                    .update_one(filter, tombstone_pipeline())
                    .await
                    .map_err(backend)?;
                Ok((deleted.matched_count > 0).then_some(0))
            }
        }
    }

    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        let deleted = self
            .records
            .update_one(live_filter(key), tombstone_pipeline())
            .await
            .map_err(backend)?;
        Ok(deleted.matched_count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;
    use time::macros::datetime;

    /// A stored record's document, read as `get` reads it.
    fn record(
        value: &[u8],
        version: i64,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<Versioned, StorageError> {
        let mut record = doc! {
            "_id": { "namespace": "ns", "key": "k" },
            "value": Binary { subtype: BinarySubtype::Generic, bytes: value.to_vec() },
            "version": version,
        };
        if let Some(at) = expires_at {
            record.insert("expires_at", date(at));
        }
        versioned(&record)
    }

    /// The `_id` a record written before the rename to meta-whatsapp-rs
    /// would live under is a stored name: its namespace (a typed store's,
    /// `wa.token` here), then its key, in fields `namespace` and `key`.
    #[test]
    fn a_record_is_keyed_by_its_namespace_then_its_key() {
        let key = StoreKey::new("\x77a.token", "\x77aba/W1");
        assert_eq!(
            id(&key),
            doc! { "namespace": "\x77a.token", "key": "\x77aba/W1" }
        );
        assert_eq!(
            id(&key).keys().collect::<Vec<_>>(),
            ["namespace", "key"],
            "an `_id` document matches in order"
        );
    }

    /// Decisive for compare-and-swap: its filter names the expected
    /// version, beside the key and liveness. Removing the version check
    /// fails this test, and the conformance suite live (a stale version
    /// would swap).
    #[test]
    fn compare_and_swap_filters_on_the_expected_version_of_a_live_record() {
        let key = StoreKey::new("wa.otp", "user-1");
        assert_eq!(
            version_filter(&key, 42),
            doc! {
                "_id": { "namespace": "wa.otp", "key": "user-1" },
                "version": 42_i64,
                "$expr": live(),
            }
        );
        assert_eq!(
            live(),
            doc! { "$and": [
                { "$ne": [ { "$type": "$value" }, "missing" ] },
                { "$or": [
                    { "$eq": [ { "$type": "$expires_at" }, "missing" ] },
                    { "$gt": [ "$expires_at", "$$NOW" ] },
                ] },
            ] }
        );
        assert_eq!(
            dead_filter(&key),
            doc! {
                "_id": { "namespace": "wa.otp", "key": "user-1" },
                "$expr": { "$not": [live()] },
            }
        );
    }

    /// A write's update, exactly: one stage computing the value, the
    /// version and the expiry from the record as it was, then the TTL
    /// index's deadline from the new expiry.
    #[test]
    fn a_write_is_one_pipeline_over_the_record_as_it_was() {
        let at = datetime!(2100-01-01 00:00:00.123_9 UTC);
        assert_eq!(
            write_pipeline(&[0, 1, 0xff], Expiry::At(at)),
            vec![
                doc! { "$set": {
                    "value": { "$literal": Binary { subtype: BinarySubtype::Generic, bytes: vec![0, 1, 0xff] } },
                    "version": { "$max": [
                        { "$add": [ { "$ifNull": [ "$version", 0_i64 ] }, 1_i64 ] },
                        { "$multiply": [ { "$toLong": "$$NOW" }, 1_048_576_i64 ] },
                    ] },
                    "expires_at": date(datetime!(2100-01-01 00:00:00.123 UTC)),
                } },
                doc! { "$set": {
                    "purge_after": { "$cond": [
                        { "$eq": [ { "$type": "$expires_at" }, "missing" ] },
                        "$$REMOVE",
                        { "$add": [ { "$max": [ "$expires_at", "$$NOW" ] }, 600_000_i64 ] },
                    ] },
                } },
            ]
        );
        assert_eq!(
            tombstone_pipeline(),
            vec![
                doc! { "$set": { "purge_after": { "$add": [ "$$NOW", 600_000_i64 ] } } },
                doc! { "$unset": ["value", "expires_at"] },
            ],
            "a delete keeps the version"
        );
    }

    #[test]
    fn expiries_as_the_pipeline_computes_them() {
        assert_eq!(
            new_expiry(Expiry::Never),
            Bson::String("$$REMOVE".to_owned())
        );
        assert_eq!(
            new_expiry(Expiry::After(Duration::from_micros(1_500))),
            Bson::Document(doc! { "$let": {
                "vars": { "at": { "$add": [ { "$toLong": "$$NOW" }, 2_i64 ] } },
                "in": { "$cond": [
                    { "$gt": [ "$$at", 253_402_300_799_999_i64 ] },
                    "$$REMOVE",
                    { "$toDate": "$$at" },
                ] },
            } })
        );
        assert_eq!(
            new_expiry(Expiry::Keep),
            Bson::Document(doc! { "$cond": [ live(), "$expires_at", "$$REMOVE" ] })
        );
    }

    #[test]
    fn ttl_rounds_up_to_milliseconds() {
        assert_eq!(ttl_millis(Duration::from_nanos(1)), 1);
        assert_eq!(ttl_millis(Duration::from_millis(1500)), 1500);
        assert_eq!(ttl_millis(Duration::ZERO), 0);
        // Capped, not an error: the pipeline maps it to "never".
        assert_eq!(ttl_millis(Duration::MAX), MAX_MILLIS);
    }

    /// The clock's term of a version stays inside a signed 64-bit integer
    /// for two centuries.
    #[test]
    fn the_clock_term_fits_until_2248() {
        let millis = datetime!(2248-01-01 0:00 UTC).unix_timestamp() * 1000;
        assert!(millis.checked_mul(VERSION_SCALE).is_some());
        let millis = datetime!(2249-01-01 0:00 UTC).unix_timestamp() * 1000;
        assert!(millis.checked_mul(VERSION_SCALE).is_none());
    }

    #[test]
    fn a_record_reads_back_exactly() {
        let at = datetime!(2100-01-01 00:00:00.123 UTC);
        assert_eq!(
            record(&[0, 0xc3, 0x28], 7, Some(at)).unwrap(),
            Versioned {
                value: vec![0, 0xc3, 0x28],
                version: 7,
                expires_at: Some(at),
            }
        );
        assert_eq!(record(b"", 1, None).unwrap().expires_at, None);
        assert!(
            record(b"", -1, None).is_err(),
            "a negative version is corrupt"
        );
    }
}
