//! [`MongoOutbox`]: the event outbox on a MongoDB replica set (roadmap
//! S9), in the collections of [`crate::schema`]. Every write it makes runs
//! in a multi-document transaction (snapshot reads, majority writes), tried
//! again when MongoDB aborts it for a write conflict, for at most
//! [`MongoOutbox::with_busy_after`]'s budget.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{MediaId, PhoneNumberId, WabaId};
use meta_whatsapp_server_core::model::{TenantId, WabaBinding};
use meta_whatsapp_server_core::outbox::{
    EventPage, EventQuery, GuardedBinding, NewEvent, Outbox, RECEIVED_MEDIA_ROWS, ReceivedMedia,
    StoredEvent,
};
use meta_whatsapp_server_core::store::StoreResult;
use mongodb::bson::oid::ObjectId;
use mongodb::bson::{self, Bson, Document, doc};
use mongodb::error::{
    ErrorKind, TRANSIENT_TRANSACTION_ERROR, UNKNOWN_TRANSACTION_COMMIT_RESULT, WriteFailure,
};
use mongodb::options::{IndexOptions, ReadConcern, ReturnDocument, WriteConcern};
use mongodb::{Client, ClientSession, Collection, Database, IndexModel};
use time::OffsetDateTime;

use crate::schema as s;

/// Every log line of this outbox.
const LOG_TARGET: &str = "meta_whatsapp_server_store_mongodb::outbox";

/// How long an insert (or a purge's step) keeps trying again after write
/// conflicts before it answers `StorageError::Busy`: the Postgres outbox's
/// `lock_timeout`.
pub const BUSY_AFTER: Duration = Duration::from_secs(2);

/// MongoDB's `DuplicateKey`: inside a transaction, another writer stored
/// the same key after this transaction's snapshot (a dedup key, a new
/// stream's document). Tried again, the transaction sees it.
const DUPLICATE_KEY: i32 = 11000;

/// MongoDB's `NamespaceExists`: `create_collection` of one that exists.
const NAMESPACE_EXISTS: i32 = 48;

/// How many events one transaction of a purge deletes at most: a stream's
/// prefix is cut in steps of this many, each recording how far it went.
const PURGE_CHUNK: i64 = 1000;

/// A pause inside an insert's transaction, after its routing check (the
/// guarded binding read and written) and before its sequence is drawn
/// (feature `test-hooks`, for this crate's live tests).
#[cfg(feature = "test-hooks")]
pub type InsertHook =
    std::sync::Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// The outbox on MongoDB. Cheap to clone (the driver's handles are).
///
/// Built on a [`Database`] of a replica set, [`Self::migrate`]d once (its
/// collections and indexes). The bindings its inserts check are the
/// [`crate::schema::WABAS`] documents [`Self::record_binding`] writes: the
/// crate's docs say why, for this spike, the records' owner writes them.
#[derive(Clone)]
pub struct MongoOutbox {
    database: Database,
    busy_after: Duration,
    #[cfg(feature = "test-hooks")]
    hook: Option<InsertHook>,
}

impl std::fmt::Debug for MongoOutbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MongoOutbox")
            .field("database", &self.database.name())
            .field("busy_after", &self.busy_after)
            .finish_non_exhaustive()
    }
}

/// Why an attempt of a transaction failed: the driver's error (which may be
/// worth another attempt), or one of the store's own (which is not).
enum Failure {
    Mongo(mongodb::error::Error),
    Store(StorageError),
}

impl From<mongodb::error::Error> for Failure {
    fn from(error: mongodb::error::Error) -> Self {
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

/// The work of one transaction, which [`MongoOutbox::transaction`] runs
/// again, from the start, after a write conflict.
trait Step: Sync {
    type Output: Send;
    fn attempt<'s>(
        &'s self,
        outbox: &'s MongoOutbox,
        session: &'s mut ClientSession,
    ) -> Attempt<'s, Self::Output>;
}

/// How an insert ended: its sequence (`None`: a redelivery), and whether
/// its routed tenant lost the binding it was routed by.
struct Inserted {
    sequence: Option<i64>,
    lost_its_tenant: bool,
}

struct InsertStep<'a>(&'a NewEvent);

impl Step for InsertStep<'_> {
    type Output = Inserted;
    fn attempt<'s>(
        &'s self,
        outbox: &'s MongoOutbox,
        session: &'s mut ClientSession,
    ) -> Attempt<'s, Inserted> {
        Box::pin(outbox.insert_attempt(session, self.0))
    }
}

struct PageStep<'a>(&'a EventQuery);

impl Step for PageStep<'_> {
    type Output = EventPage;
    fn attempt<'s>(
        &'s self,
        outbox: &'s MongoOutbox,
        session: &'s mut ClientSession,
    ) -> Attempt<'s, EventPage> {
        Box::pin(outbox.page_attempt(session, self.0))
    }
}

/// One step of a stream's purge: the events up to `through` (at most
/// [`PURGE_CHUNK`] of them per step).
struct PurgeStep<'a> {
    stream: &'a str,
    through: i64,
}

/// What one step purged: how many events, and whether the stream's cut is
/// done.
struct Purged {
    deleted: u64,
    done: bool,
}

impl Step for PurgeStep<'_> {
    type Output = Purged;
    fn attempt<'s>(
        &'s self,
        outbox: &'s MongoOutbox,
        session: &'s mut ClientSession,
    ) -> Attempt<'s, Purged> {
        Box::pin(outbox.purge_attempt(session, self.stream, self.through))
    }
}

struct ForgetTenantStep<'a>(&'a TenantId);

impl Step for ForgetTenantStep<'_> {
    type Output = ();
    fn attempt<'s>(
        &'s self,
        outbox: &'s MongoOutbox,
        session: &'s mut ClientSession,
    ) -> Attempt<'s, ()> {
        Box::pin(outbox.forget_tenant_attempt(session, self.0))
    }
}

fn backend(error: impl std::error::Error + Send + Sync + 'static) -> StorageError {
    StorageError::Backend(anyhow::Error::new(error))
}

fn corrupt(collection: &'static str, field: &'static str) -> StorageError {
    StorageError::Backend(anyhow::anyhow!(
        "unreadable value of `{field}` in `{collection}`"
    ))
}

/// The server's error code, when it answered one.
fn code_of(error: &mongodb::error::Error) -> Option<i32> {
    match &*error.kind {
        ErrorKind::Command(command) => Some(command.code),
        ErrorKind::Write(WriteFailure::WriteError(write)) => Some(write.code),
        _ => None,
    }
}

/// Whether a transaction that failed this way may succeed from the start:
/// MongoDB labels a write conflict (and a primary stepping down) a
/// transient transaction error; a duplicate key inside a transaction is a
/// key another writer committed after its snapshot, which the next attempt
/// reads.
fn worth_another_attempt(error: &mongodb::error::Error) -> bool {
    error.contains_label(TRANSIENT_TRANSACTION_ERROR) || code_of(error) == Some(DUPLICATE_KEY)
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

/// `t` as a BSON date, its milliseconds rounded down (a binding's start:
/// the guard compares its second, which rounding down keeps).
fn bson_floor(t: OffsetDateTime) -> bson::DateTime {
    let millis = t.unix_timestamp_nanos().div_euclid(1_000_000);
    bson::DateTime::from_millis(i64::try_from(millis).unwrap_or(i64::MAX))
}

/// `t` as a BSON date, its milliseconds rounded up (the end of a dedup
/// window: a key held a fraction of a millisecond longer, never shorter).
fn bson_ceil(t: OffsetDateTime) -> bson::DateTime {
    let nanos = t.unix_timestamp_nanos();
    let millis = nanos.div_euclid(1_000_000) + i128::from(nanos.rem_euclid(1_000_000) > 0);
    bson::DateTime::from_millis(i64::try_from(millis).unwrap_or(i64::MAX))
}

fn time_of(
    date: bson::DateTime,
    collection: &'static str,
    field: &'static str,
) -> StoreResult<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(date.timestamp_millis()) * 1_000_000)
        .map_err(|_| corrupt(collection, field))
}

fn i64_of(document: &Document, collection: &'static str, field: &'static str) -> StoreResult<i64> {
    document
        .get_i64(field)
        .map_err(|_| corrupt(collection, field))
}

fn str_of<'d>(
    document: &'d Document,
    collection: &'static str,
    field: &'static str,
) -> StoreResult<&'d str> {
    document
        .get_str(field)
        .map_err(|_| corrupt(collection, field))
}

/// An optional string field: absent (or null) is `None`.
fn optional_str<'d>(
    document: &'d Document,
    collection: &'static str,
    field: &'static str,
) -> StoreResult<Option<&'d str>> {
    match document.get(field) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::String(value)) => Ok(Some(value)),
        Some(_) => Err(corrupt(collection, field)),
    }
}

/// An optional date field: absent (or null) is `None`.
fn optional_time(
    document: &Document,
    collection: &'static str,
    field: &'static str,
) -> StoreResult<Option<OffsetDateTime>> {
    match document.get(field) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::DateTime(date)) => time_of(*date, collection, field).map(Some),
        Some(_) => Err(corrupt(collection, field)),
    }
}

fn i64_saturating(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

impl MongoOutbox {
    /// The outbox in `database`, which must be on a replica set (or a
    /// sharded cluster): MongoDB runs no transaction on a standalone
    /// server. Nothing is read or written until [`Self::migrate`].
    pub fn new(database: Database) -> Self {
        Self {
            database,
            busy_after: BUSY_AFTER,
            #[cfg(feature = "test-hooks")]
            hook: None,
        }
    }

    /// How long a write keeps trying again after write conflicts before it
    /// answers `StorageError::Busy` ([`BUSY_AFTER`] by default).
    #[must_use]
    pub fn with_busy_after(mut self, budget: Duration) -> Self {
        self.busy_after = budget;
        self
    }

    /// Pause every insert's transaction at `hook`, after its routing check
    /// and before its sequence is drawn: what this crate's live tests hold
    /// an insert in flight with. Test code (feature `test-hooks`).
    #[cfg(feature = "test-hooks")]
    #[must_use]
    pub fn with_insert_hook(mut self, hook: InsertHook) -> Self {
        self.hook = Some(hook);
        self
    }

    /// The database it stores in.
    pub fn database(&self) -> &Database {
        &self.database
    }

    fn client(&self) -> &Client {
        self.database.client()
    }

    fn events(&self) -> Collection<Document> {
        self.database.collection(s::EVENTS)
    }

    fn streams(&self) -> Collection<Document> {
        self.database.collection(s::STREAMS)
    }

    fn wabas(&self) -> Collection<Document> {
        self.database.collection(s::WABAS)
    }

    /// Create the collections and the indexes (none is created inside a
    /// transaction). Safe to run again, and on two replicas at once: an
    /// existing collection or an identical index is left as it is.
    ///
    /// # Errors
    ///
    /// `StorageError::Backend` when the database refuses one.
    pub async fn migrate(&self) -> StoreResult<()> {
        for name in [s::EVENTS, s::STREAMS, s::WABAS] {
            match self.database.create_collection(name).await {
                Ok(()) => {}
                Err(error) if code_of(&error) == Some(NAMESPACE_EXISTS) => {}
                Err(error) => return Err(backend(error)),
            }
        }
        let index = |keys: Document, options: IndexOptions| {
            IndexModel::builder().keys(keys).options(options).build()
        };
        let indexes = [
            index(
                doc! { s::EVENT_STREAM: 1, s::EVENT_SEQUENCE: 1 },
                IndexOptions::builder()
                    .name(s::INDEX_STREAM_SEQUENCE.to_owned())
                    .unique(true)
                    .build(),
            ),
            index(
                doc! { s::EVENT_DEDUP_KEY: 1 },
                IndexOptions::builder()
                    .name(s::INDEX_DEDUP_KEY.to_owned())
                    .unique(true)
                    .partial_filter_expression(doc! { s::EVENT_DEDUP_KEY: { "$type": "string" } })
                    .build(),
            ),
            index(
                doc! { s::EVENT_CREATED_AT: 1 },
                IndexOptions::builder()
                    .name(s::INDEX_CREATED_AT.to_owned())
                    .build(),
            ),
            // `received_media`'s lookup, read in its order and up to its
            // limit: only the rows that record a media id.
            index(
                doc! { s::EVENT_MEDIA_ID: 1, s::EVENT_CREATED_AT: -1, s::EVENT_MEDIA_ORDER: -1 },
                IndexOptions::builder()
                    .name(s::INDEX_MEDIA.to_owned())
                    .partial_filter_expression(doc! { s::EVENT_MEDIA_ID: { "$type": "string" } })
                    .build(),
            ),
            // `erase_contacts`' lookup (roadmap M2f): only the rows that
            // name a customer, one entry per identifier (multikey). The
            // filter is `$exists`, which an `$in` of strings implies, so
            // that the erasure's `$in` alone gives the index's bounds.
            index(
                doc! { s::EVENT_CONTACTS: 1 },
                IndexOptions::builder()
                    .name(s::INDEX_CONTACTS.to_owned())
                    .partial_filter_expression(doc! { s::EVENT_CONTACTS: { "$exists": true } })
                    .build(),
            ),
        ];
        self.events()
            .create_indexes(indexes)
            .await
            .map_err(backend)?;
        Ok(())
    }

    /// Write `binding` and exactly `numbers` as the binding the inserts
    /// check ([`crate::schema::WABAS`]): what the records' owner calls once
    /// a binding of its records began or changed (a refresh included), and
    /// before it answers. One write of the WABA's document, so it waits for
    /// an insert in flight that checked the binding, or makes one that has
    /// not committed start over. For this spike only (the crate's docs):
    /// roadmap S18's records write it in their own transaction.
    ///
    /// # Errors
    ///
    /// `StorageError::Backend` when the write fails.
    pub async fn record_binding(
        &self,
        binding: &WabaBinding,
        numbers: &[PhoneNumberId],
    ) -> StoreResult<()> {
        let mut document = doc! {
            s::WABA_TENANT: binding.tenant_id.as_str(),
            s::WABA_ATTACHED_AT: bson_floor(binding.attached_at),
            s::WABA_NUMBERS: numbers.iter().map(PhoneNumberId::as_str).collect::<Vec<_>>(),
        };
        if let Some(left) = binding.previous_tenant_unbound_at {
            document.insert(s::WABA_PREVIOUS_TENANT_UNBOUND_AT, bson_floor(left));
        }
        self.wabas()
            .replace_one(doc! { "_id": binding.waba_id.as_str() }, document)
            .upsert(true)
            .write_concern(WriteConcern::majority())
            .await
            .map_err(backend)?;
        Ok(())
    }

    /// Remove the binding of `waba_id` the inserts check: what the records'
    /// owner calls once it unbound the WABA, before it answers (as
    /// [`Self::record_binding`]: it waits for an insert in flight).
    ///
    /// # Errors
    ///
    /// `StorageError::Backend` when the write fails.
    pub async fn forget_binding(&self, waba_id: &WabaId) -> StoreResult<()> {
        self.wabas()
            .delete_one(doc! { "_id": waba_id.as_str() })
            .write_concern(WriteConcern::majority())
            .await
            .map_err(backend)?;
        Ok(())
    }

    /// `tenant` was deleted: its events go, and its stream records them
    /// purged through its last sequence, in one transaction, so a tenant
    /// created later with the same id polls none of them and its sequences
    /// go on after them (the port's `RecordStore::delete_tenant`).
    ///
    /// What keeps an insert in flight from landing in the stream after
    /// this is the unbindings before it (a tenant is deleted only once it
    /// has no WABA): each waits for an insert that checked its binding
    /// ([`Self::forget_binding`]). This adds a second guard where it
    /// costs nothing: it writes the stream's document, and creates it when
    /// the tenant has no event yet, so an insert drawing a sequence of the
    /// stream (its first included) conflicts with it, commits first (its
    /// row deleted here) or starts over and meets the binding gone. (A
    /// stream already marked through its last sequence is left as it is,
    /// a write MongoDB skips: there the unbindings alone hold.)
    ///
    /// # Errors
    ///
    /// `StorageError::Busy` past the write-conflict budget;
    /// `StorageError::Backend` when the database fails.
    pub async fn forget_tenant(&self, tenant: &TenantId) -> StoreResult<()> {
        self.transaction(&ForgetTenantStep(tenant)).await
    }

    /// Run `step` in a transaction: snapshot reads, majority writes; from
    /// the start again after a write conflict (or another error MongoDB
    /// labels transient), until the budget is spent (`Busy`, nothing
    /// written). A commit whose result is unknown is tried again as it is
    /// (MongoDB's rule: committing again is safe), never redone.
    async fn transaction<S: Step>(&self, step: &S) -> StoreResult<S::Output> {
        let mut session = self.client().start_session().await.map_err(backend)?;
        let started = Instant::now();
        let mut attempt = 0u32;
        loop {
            session
                .start_transaction()
                .read_concern(ReadConcern::snapshot())
                .write_concern(WriteConcern::majority())
                .await
                .map_err(backend)?;
            let error = match step.attempt(self, &mut session).await {
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
    ) -> Result<(), mongodb::error::Error> {
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

    /// The tenant the insert keeps: the routed one, only while the binding
    /// its guard names still names it and is one the event may reach
    /// ([`RouteGuard::admits`](meta_whatsapp_server_core::outbox::RouteGuard::admits),
    /// the core's rule, not redone here). The guarded WABA's document is
    /// read *and written* (its `insert_checks` incremented) in the insert's
    /// transaction: a snapshot transaction conflicts with a write, not with
    /// a read, so the write is what makes an unbinding wait for this insert
    /// or this insert start over after it.
    async fn admitted<'e>(
        &self,
        session: &mut ClientSession,
        event: &'e NewEvent,
    ) -> Result<Option<&'e TenantId>, Failure> {
        let (Some(tenant), Some(guard)) = (&event.tenant, &event.route_guard) else {
            return Ok(None);
        };
        let mut filter = doc! {
            "_id": guard.binding.waba_id().as_str(),
            s::WABA_TENANT: tenant.as_str(),
        };
        if let GuardedBinding::Number {
            phone_number_id, ..
        } = &guard.binding
        {
            filter.insert(s::WABA_NUMBERS, phone_number_id.as_str());
        }
        let Some(binding) = self
            .wabas()
            .find_one_and_update(filter, doc! { "$inc": { s::WABA_INSERT_CHECKS: 1_i64 } })
            .projection(doc! {
                s::WABA_ATTACHED_AT: 1,
                s::WABA_PREVIOUS_TENANT_UNBOUND_AT: 1,
            })
            .session(&mut *session)
            .await?
        else {
            return Ok(None);
        };
        let attached_at = binding
            .get_datetime(s::WABA_ATTACHED_AT)
            .map_err(|_| corrupt(s::WABAS, s::WABA_ATTACHED_AT))?;
        let attached_at = time_of(*attached_at, s::WABAS, s::WABA_ATTACHED_AT)?;
        let previous = optional_time(&binding, s::WABAS, s::WABA_PREVIOUS_TENANT_UNBOUND_AT)?;
        Ok(guard.admits(attached_at, previous).then_some(tenant))
    }

    async fn insert_attempt(
        &self,
        session: &mut ClientSession,
        event: &NewEvent,
    ) -> Result<Inserted, Failure> {
        let events = self.events();
        // Once: a stored key is a redelivery (nothing written, no sequence
        // drawn), unless its row's window ended by this event's arrival
        // (both on the webhook pipeline's clock), when that row gives the
        // key up and keeps its row.
        if let Some(key) = &event.dedup_key {
            let held = events
                .find_one(doc! { s::EVENT_DEDUP_KEY: key.as_str() })
                .projection(doc! { s::EVENT_DEDUP_UNTIL: 1 })
                .session(&mut *session)
                .await?;
            if let Some(held) = held {
                let until = optional_time(&held, s::EVENTS, s::EVENT_DEDUP_UNTIL)?;
                let released = match (event.dedup_window, until) {
                    (Some(window), Some(until)) => until <= window.now,
                    _ => false,
                };
                if !released {
                    return Ok(Inserted {
                        sequence: None,
                        lost_its_tenant: false,
                    });
                }
                let id = held
                    .get("_id")
                    .cloned()
                    .ok_or_else(|| corrupt(s::EVENTS, "_id"))?;
                events
                    .update_one(
                        doc! { "_id": id },
                        doc! { "$unset": { s::EVENT_DEDUP_KEY: "" } },
                    )
                    .session(&mut *session)
                    .await?;
            }
        }
        // The routing, again.
        let tenant = self.admitted(session, event).await?;
        #[cfg(feature = "test-hooks")]
        if let Some(hook) = &self.hook {
            hook().await;
        }
        // The stream's next sequence: its document written by this
        // transaction until it commits, so the stream's inserts commit one
        // at a time, in sequence order.
        let stream = tenant.map_or("", TenantId::as_str);
        let drawn = self
            .streams()
            .find_one_and_update(
                doc! { "_id": stream },
                doc! {
                    "$inc": { s::STREAM_LAST: 1_i64 },
                    "$setOnInsert": { s::STREAM_PURGED: 0_i64 },
                },
            )
            .upsert(true)
            .return_document(ReturnDocument::After)
            .projection(doc! { s::STREAM_LAST: 1 })
            .session(&mut *session)
            .await?
            .ok_or_else(|| corrupt(s::STREAMS, s::STREAM_LAST))?;
        let sequence = i64_of(&drawn, s::STREAMS, s::STREAM_LAST)?;
        let row = event_document(event, stream, sequence, tenant);
        // An upsert, so that `created_at` is the server's clock; one that
        // matches is an event id stored already (not a redelivery: those
        // share a dedup key, above), refused as Postgres' unique id is.
        let written = events
            .update_one(
                doc! { "_id": event.id.as_str() },
                doc! {
                    "$setOnInsert": row,
                    "$currentDate": { s::EVENT_CREATED_AT: true },
                },
            )
            .upsert(true)
            .session(&mut *session)
            .await?;
        if written.matched_count > 0 {
            return Err(Failure::Store(StorageError::Backend(anyhow::anyhow!(
                "an event with this id is stored already"
            ))));
        }
        Ok(Inserted {
            sequence: Some(sequence),
            lost_its_tenant: event.tenant.is_some() && tenant.is_none(),
        })
    }

    async fn page_attempt(
        &self,
        session: &mut ClientSession,
        query: &EventQuery,
    ) -> Result<EventPage, Failure> {
        let tenant = query.tenant.as_str();
        let Some(stream) = self
            .streams()
            .find_one(doc! { "_id": tenant })
            .session(&mut *session)
            .await?
        else {
            return Ok(EventPage {
                events: Vec::new(),
                more: false,
                purged_through: 0,
                high_water: 0,
            });
        };
        let purged_through = i64_of(&stream, s::STREAMS, s::STREAM_PURGED)?;
        let high_water = i64_of(&stream, s::STREAMS, s::STREAM_LAST)?;
        let after = query.after.unwrap_or(purged_through);
        let mut filter = doc! {
            s::EVENT_STREAM: tenant,
            s::EVENT_SEQUENCE: { "$gt": after },
        };
        if let Some(types) = &query.types {
            filter.insert(s::EVENT_TYPE, doc! { "$in": types.clone() });
        }
        if let Some(pn) = &query.phone_number_id {
            filter.insert(s::EVENT_PHONE_NUMBER, pn.as_str());
        }
        // The page's rows chosen from their sizes first (one more than the
        // limit, to tell `more`), then only those rows' data read.
        let mut candidates = Vec::new();
        let mut cursor = self
            .events()
            .find(filter.clone())
            .sort(doc! { s::EVENT_SEQUENCE: 1 })
            .limit(i64_saturating(query.limit.saturating_add(1)))
            .projection(doc! { "_id": 0, s::EVENT_SEQUENCE: 1, s::EVENT_DATA_BYTES: 1 })
            .session(&mut *session)
            .await?;
        while let Some(candidate) = cursor.next(&mut *session).await {
            let candidate = candidate?;
            candidates.push((
                i64_of(&candidate, s::EVENTS, s::EVENT_SEQUENCE)?,
                usize::try_from(i64_of(&candidate, s::EVENTS, s::EVENT_DATA_BYTES)?)
                    .unwrap_or(usize::MAX),
            ));
        }
        let mut kept = Vec::new();
        let mut bytes = 0usize;
        let mut more = false;
        for (sequence, size) in candidates {
            bytes = bytes.saturating_add(size);
            if kept.len() == query.limit || (!kept.is_empty() && bytes > query.max_bytes) {
                more = true;
                break;
            }
            kept.push(sequence);
        }
        let mut events = Vec::with_capacity(kept.len());
        if !kept.is_empty() {
            let mut cursor = self
                .events()
                .find(doc! { s::EVENT_STREAM: tenant, s::EVENT_SEQUENCE: { "$in": kept } })
                .sort(doc! { s::EVENT_SEQUENCE: 1 })
                .session(&mut *session)
                .await?;
            while let Some(row) = cursor.next(&mut *session).await {
                events.push(stored_event(&row?)?);
            }
        }
        Ok(EventPage {
            events,
            more,
            purged_through,
            high_water,
        })
    }

    async fn purge_attempt(
        &self,
        session: &mut ClientSession,
        stream: &str,
        through: i64,
    ) -> Result<Purged, Failure> {
        // The step's end: the stream's `PURGE_CHUNK`-th event up to
        // `through`, or `through` when fewer are left.
        let mut cursor = self
            .events()
            .find(doc! { s::EVENT_STREAM: stream, s::EVENT_SEQUENCE: { "$lte": through } })
            .sort(doc! { s::EVENT_SEQUENCE: 1 })
            .skip(u64::try_from(PURGE_CHUNK - 1).unwrap_or_default())
            .limit(1)
            .projection(doc! { "_id": 0, s::EVENT_SEQUENCE: 1 })
            .session(&mut *session)
            .await?;
        let mut end = through;
        while let Some(row) = cursor.next(&mut *session).await {
            end = i64_of(&row?, s::EVENTS, s::EVENT_SEQUENCE)?;
        }
        let deleted = self
            .events()
            .delete_many(doc! { s::EVENT_STREAM: stream, s::EVENT_SEQUENCE: { "$lte": end } })
            .session(&mut *session)
            .await?
            .deleted_count;
        // Only forward: another replica's purge may have gone further.
        self.streams()
            .update_one(
                doc! { "_id": stream },
                doc! { "$max": { s::STREAM_PURGED: end } },
            )
            .session(&mut *session)
            .await?;
        Ok(Purged {
            deleted,
            done: end >= through,
        })
    }

    async fn forget_tenant_attempt(
        &self,
        session: &mut ClientSession,
        tenant: &TenantId,
    ) -> Result<(), Failure> {
        // Purged through the last sequence (one stage: both fields read the
        // document as it was), an empty stream created at 0.
        let last = doc! { "$ifNull": [format!("${}", s::STREAM_LAST), 0_i64] };
        self.streams()
            .update_one(
                doc! { "_id": tenant.as_str() },
                vec![doc! { "$set": { s::STREAM_LAST: last.clone(), s::STREAM_PURGED: last } }],
            )
            .upsert(true)
            .session(&mut *session)
            .await?;
        self.events()
            .delete_many(doc! { s::EVENT_STREAM: tenant.as_str() })
            .session(&mut *session)
            .await?;
        Ok(())
    }

    /// The database's clock: the primary's `localTime`.
    async fn database_now(&self) -> StoreResult<OffsetDateTime> {
        let hello = self
            .database
            .run_command(doc! { "hello": 1 })
            .await
            .map_err(backend)?;
        let now = hello
            .get_datetime("localTime")
            .map_err(|_| corrupt("hello", "localTime"))?;
        time_of(*now, "hello", "localTime")
    }
}

/// The document of `event`, as its insert stores it (`created_at` aside:
/// the server's, set by the upsert).
fn event_document(
    event: &NewEvent,
    stream: &str,
    sequence: i64,
    tenant: Option<&TenantId>,
) -> Document {
    let data_bytes = i64_saturating(event.data.len());
    let mut row = doc! {
        s::EVENT_STREAM: stream,
        s::EVENT_SEQUENCE: sequence,
        s::EVENT_TYPE: event.event_type.as_str(),
        s::EVENT_DATA: event.data.as_str(),
        s::EVENT_DATA_BYTES: data_bytes,
    };
    if let Some(tenant) = tenant {
        row.insert(s::EVENT_TENANT, tenant.as_str());
    }
    if let Some(key) = &event.dedup_key {
        row.insert(s::EVENT_DEDUP_KEY, key.as_str());
        if let Some(window) = event.dedup_window {
            row.insert(s::EVENT_DEDUP_UNTIL, bson_ceil(window.until));
        }
    }
    if let Some(pn) = &event.phone_number_id {
        row.insert(s::EVENT_PHONE_NUMBER, pn.as_str());
    }
    if let Some(waba) = &event.waba_id {
        row.insert(s::EVENT_WABA, waba.as_str());
    }
    // The media id the event records as received (roadmap M2e), with
    // what orders it among the same millisecond's rows.
    if let Some(media) = event.received_media() {
        row.insert(s::EVENT_MEDIA_ID, media.as_str());
        row.insert(s::EVENT_MEDIA_ORDER, ObjectId::new());
    }
    // The identifiers the event names its customers by (roadmap M2f),
    // absent when it names none: outside the partial index, as Postgres
    // stores `NULL`.
    let contacts = event.contacts();
    if !contacts.is_empty() {
        row.insert(s::EVENT_CONTACTS, contacts);
    }
    row
}

/// An event as stored, from its document.
fn stored_event(row: &Document) -> StoreResult<StoredEvent> {
    let tenant = optional_str(row, s::EVENTS, s::EVENT_TENANT)?
        .map(|t| TenantId::parse(t).ok_or_else(|| corrupt(s::EVENTS, s::EVENT_TENANT)))
        .transpose()?;
    let created_at = row
        .get_datetime(s::EVENT_CREATED_AT)
        .map_err(|_| corrupt(s::EVENTS, s::EVENT_CREATED_AT))?;
    Ok(StoredEvent {
        sequence: i64_of(row, s::EVENTS, s::EVENT_SEQUENCE)?,
        id: str_of(row, s::EVENTS, "_id")?.to_owned(),
        tenant,
        phone_number_id: optional_str(row, s::EVENTS, s::EVENT_PHONE_NUMBER)?
            .map(PhoneNumberId::new_unchecked),
        waba_id: optional_str(row, s::EVENTS, s::EVENT_WABA)?.map(WabaId::new_unchecked),
        event_type: str_of(row, s::EVENTS, s::EVENT_TYPE)?.to_owned(),
        data: str_of(row, s::EVENTS, s::EVENT_DATA)?.to_owned(),
        created_at: time_of(*created_at, s::EVENTS, s::EVENT_CREATED_AT)?,
    })
}

#[async_trait]
impl Outbox for MongoOutbox {
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>> {
        let inserted = self.transaction(&InsertStep(event)).await?;
        if inserted.lost_its_tenant {
            tracing::warn!(
                target: LOG_TARGET,
                sequence = inserted.sequence,
                event_type = %event.event_type,
                "an event's tenant lost the binding it was routed by while it was recorded \
                 (or holds it again, bound after the event): kept operator-only"
            );
        }
        Ok(inserted.sequence)
    }

    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage> {
        self.transaction(&PageStep(query)).await
    }

    async fn get(&self, tenant: &TenantId, id: &str) -> StoreResult<Option<StoredEvent>> {
        // By `_id` (the event's public id), in the tenant's own stream
        // only: another tenant's row, or an operator-only one (stream
        // `""`, which no tenant's id is), is never read.
        self.events()
            .find_one(doc! { "_id": id, s::EVENT_STREAM: tenant.as_str() })
            .await
            .map_err(backend)?
            .as_ref()
            .map(stored_event)
            .transpose()
    }

    async fn received_media(&self, media_id: &MediaId) -> StoreResult<Vec<ReceivedMedia>> {
        // Every stream's rows, the operator-only ones included, by the
        // partial index on `media_id`: never a scan of the rows' data. The
        // filter repeats the index's own (`$type`), so that the planner
        // can tell the index covers it.
        let mut cursor = self
            .events()
            .find(doc! { s::EVENT_MEDIA_ID: { "$type": "string", "$eq": media_id.as_str() } })
            .sort(doc! { s::EVENT_CREATED_AT: -1, s::EVENT_MEDIA_ORDER: -1 })
            .limit(i64_saturating(RECEIVED_MEDIA_ROWS))
            .projection(doc! {
                "_id": 0,
                s::EVENT_TENANT: 1,
                s::EVENT_PHONE_NUMBER: 1,
                s::EVENT_CREATED_AT: 1,
            })
            .await
            .map_err(backend)?;
        let mut found = Vec::new();
        while cursor.advance().await.map_err(backend)? {
            let row = cursor.deserialize_current().map_err(backend)?;
            let created_at = row
                .get_datetime(s::EVENT_CREATED_AT)
                .map_err(|_| corrupt(s::EVENTS, s::EVENT_CREATED_AT))?;
            found.push(ReceivedMedia {
                tenant: optional_str(&row, s::EVENTS, s::EVENT_TENANT)?
                    .map(|t| TenantId::parse(t).ok_or_else(|| corrupt(s::EVENTS, s::EVENT_TENANT)))
                    .transpose()?,
                phone_number_id: optional_str(&row, s::EVENTS, s::EVENT_PHONE_NUMBER)?
                    .map(PhoneNumberId::new_unchecked),
                received_at: time_of(*created_at, s::EVENTS, s::EVENT_CREATED_AT)?,
            });
        }
        Ok(found)
    }

    async fn erase_contacts(
        &self,
        tenant: &TenantId,
        phone_number_id: &PhoneNumberId,
        contacts: &[String],
    ) -> StoreResult<u64> {
        if contacts.is_empty() {
            return Ok(0);
        }
        // The tenant's own rows on the number, as Postgres' `ERASE_SQL`:
        // `tenant_id` never matches an operator-only row (it has none:
        // design D89), and is read rather than `stream`, which leads the
        // stream's index, so that the planner's only index is the partial
        // multikey one on `contacts` (its `$exists` implied by the `$in`).
        // Whole rows, each counted once however many of `contacts` it
        // names; no stream's document is written, so its sequences and
        // `purged_through` stay. One statement, not a transaction: each
        // row's deletion is atomic, and a repeat deletes what is left.
        let deleted = self
            .events()
            .delete_many(doc! {
                s::EVENT_TENANT: tenant.as_str(),
                s::EVENT_PHONE_NUMBER: phone_number_id.as_str(),
                s::EVENT_CONTACTS: { "$in": contacts },
            })
            .write_concern(WriteConcern::majority())
            .await
            .map_err(backend)?
            .deleted_count;
        Ok(deleted)
    }

    async fn purge(&self, older_than: Duration) -> StoreResult<u64> {
        // No lock of its own (housekeeping holds its turn): two purges at
        // once write the same stream document, so one starts over and
        // deletes what the other left, and `purged_through` only moves
        // forward (`$max`).
        let millis = i64::try_from(older_than.as_millis()).unwrap_or(i64::MAX);
        let now = self.database_now().await?;
        let cutoff =
            bson::DateTime::from_millis(bson_floor(now).timestamp_millis().saturating_sub(millis));
        // Per stream, the prefix of sequences up to its newest event past
        // the cutoff: a clock step never leaves an older event behind a
        // purged one.
        let mut cursor = self
            .events()
            .aggregate([
                doc! { "$match": { s::EVENT_CREATED_AT: { "$lt": cutoff } } },
                doc! {
                    "$group": {
                        "_id": format!("${}", s::EVENT_STREAM),
                        "through": { "$max": format!("${}", s::EVENT_SEQUENCE) },
                    }
                },
            ])
            .await
            .map_err(backend)?;
        let mut cuts = Vec::new();
        while cursor.advance().await.map_err(backend)? {
            let cut = cursor.deserialize_current().map_err(backend)?;
            cuts.push((
                str_of(&cut, s::EVENTS, "_id")?.to_owned(),
                i64_of(&cut, s::EVENTS, "through")?,
            ));
        }
        let mut purged = 0u64;
        for (stream, through) in cuts {
            loop {
                let step = self
                    .transaction(&PurgeStep {
                        stream: &stream,
                        through,
                    })
                    .await?;
                purged = purged.saturating_add(step.deleted);
                if step.done {
                    break;
                }
            }
        }
        Ok(purged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binding's start keeps its second (the guard compares seconds), a
    /// dedup window's end is never earlier than given, both exact on whole
    /// milliseconds, before the epoch too. Decisive: the rounding of each.
    #[test]
    fn instants_round_to_milliseconds_in_the_safe_direction() {
        let t = time::macros::datetime!(2026-09-26 12:00:00.999_999_999 UTC);
        assert_eq!(bson_floor(t).timestamp_millis() % 1000, 999);
        assert_eq!(bson_ceil(t).timestamp_millis() % 1000, 0);
        let whole = time::macros::datetime!(2026-09-26 12:00:00.250 UTC);
        assert_eq!(bson_floor(whole), bson_ceil(whole));
        assert_eq!(
            time_of(bson_floor(whole), "c", "f").unwrap(),
            whole,
            "a whole millisecond comes back as it was"
        );
        let before = time::macros::datetime!(1969-12-31 23:59:59.9995 UTC);
        assert_eq!(bson_floor(before).timestamp_millis(), -1);
        assert_eq!(bson_ceil(before).timestamp_millis(), 0);
    }

    /// An event recording a received media id (roadmap M2e) stores it,
    /// with its order, under the index's fields; any other event stores
    /// neither, so the partial index holds the media rows only. Decisive:
    /// the fields written, and only for `NewEvent::received_media`.
    #[test]
    fn a_media_row_stores_its_media_id_and_no_other_row_does() {
        let event = |event_type: &str, message: &str| NewEvent {
            id: "evt_1".to_owned(),
            dedup_key: None,
            dedup_window: None,
            tenant: None,
            route_guard: None,
            phone_number_id: None,
            waba_id: None,
            event_type: event_type.to_owned(),
            data: format!(r#"{{"event":"{event_type}","message":{message}}}"#),
        };
        let image = r#"{"type":"image","image":{"id":"1003383421387256"}}"#;
        let first = event_document(&event("message_received", image), "", 1, None);
        assert_eq!(
            first.get_str(s::EVENT_MEDIA_ID).unwrap(),
            "1003383421387256"
        );
        let order = |row: &Document| row.get_object_id(s::EVENT_MEDIA_ORDER).unwrap();
        let second = event_document(&event("message_received", image), "", 2, None);
        assert!(
            order(&second) > order(&first),
            "one process's rows ordered as inserted"
        );
        for (event_type, message) in [
            (
                "message_received",
                r#"{"type":"text","text":{"body":"hi"}}"#,
            ),
            ("status_updated", image),
        ] {
            let row = event_document(&event(event_type, message), "", 3, None);
            assert!(!row.contains_key(s::EVENT_MEDIA_ID), "{event_type}");
            assert!(!row.contains_key(s::EVENT_MEDIA_ORDER), "{event_type}");
        }
    }

    /// An event naming customers (roadmap M2f) stores the identifiers
    /// `NewEvent::contacts` gives, under the index's field; an event naming
    /// none stores no field (not an empty array), so the partial index
    /// holds the rows that name someone only. Decisive: the field written,
    /// from `NewEvent::contacts`, and only when it is not empty.
    #[test]
    fn a_row_naming_customers_stores_its_contacts_and_no_other_row_does() {
        let event = |data: &str| NewEvent {
            id: "evt_1".to_owned(),
            dedup_key: None,
            dedup_window: None,
            tenant: None,
            route_guard: None,
            phone_number_id: None,
            waba_id: None,
            event_type: "message_received".to_owned(),
            data: data.to_owned(),
        };
        let naming = event(
            r#"{"event":"message_received","display_phone_number":"15550783881",
                "message":{"from":"16505551234","from_user_id":"US.13491208655302741918",
                "type":"text","text":{"body":"16505550000"}}}"#,
        );
        let contacts = naming.contacts();
        assert_eq!(
            contacts,
            ["16505551234", "US.13491208655302741918"],
            "the core's contacts of the event"
        );
        let row = event_document(&naming, "", 1, None);
        let stored: Vec<&str> = row
            .get_array(s::EVENT_CONTACTS)
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert_eq!(stored, contacts);
        for data in [
            // A text quoting a number names nobody.
            r#"{"event":"message_received","text":"16505551234"}"#,
            // Nor does a body that is not JSON.
            "not json",
        ] {
            let row = event_document(&event(data), "", 2, None);
            assert!(!row.contains_key(s::EVENT_CONTACTS), "{data}: {row:?}");
        }
    }

    /// The pause between two attempts stays between 0.1 and 64.1
    /// milliseconds, however many attempts came before.
    #[test]
    fn the_backoff_stays_within_its_bounds() {
        let backoffs: Vec<Duration> = (0..40).map(backoff).collect();
        assert!(backoffs.iter().all(|b| *b <= Duration::from_micros(64_100)));
        assert!(backoffs.iter().all(|b| *b >= Duration::from_micros(100)));
    }
}
