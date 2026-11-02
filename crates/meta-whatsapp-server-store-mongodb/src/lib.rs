//! `meta-whatsapp-server-store-mongodb`: the meta-whatsapp-rs service's
//! MongoDB backend, a **spike** (roadmap S9, docs/design/server.md, section
//! 8.2): the core's [`Outbox`](meta_whatsapp_server_core::outbox::Outbox)
//! port alone, [`MongoOutbox`], on a MongoDB replica set (transactions need
//! one), written before the port shapes freeze to show they fit a database
//! with neither row locks nor foreign keys. The binary does not compose it;
//! the rest of the bundle (the records, the idempotency records, the leader
//! lock, the library's stores) is roadmap S17 and S18.
//!
//! What the port asks, and how MongoDB gives it:
//!
//! - **Each tenant's inserts commit in sequence order.** An insert draws
//!   its sequence by incrementing its stream's document (the tenant's id,
//!   or `""` for operator-only rows) inside the insert's multi-document
//!   transaction. Two transactions writing one document cannot both
//!   commit (the second aborts with a write conflict and tries again), so
//!   sequence `n + 1` is drawn only once `n` committed: a poll that sees
//!   `n + 1` sees `n`.
//! - **The routing, checked again, atomically with the insert.** MongoDB's
//!   snapshot transactions see a conflict between two writes, never
//!   between a read and a write: the insert *writes* the guarded binding's
//!   document (an increment of its `insert_checks`) in the transaction that
//!   reads it, as the port's text prescribes, so an unbinding (a write of
//!   that document) waits for the insert in flight or makes it start over,
//!   and the check is the core's own [`RouteGuard::admits`].
//! - **A tenant's events go with it** ([`MongoOutbox::forget_tenant`]: its
//!   stream records them purged, in the transaction that deletes them),
//!   and **retention** ([`Outbox::purge`]) deletes a prefix of each
//!   stream with the mark of how far it went, both in one transaction per
//!   chunk, with no lock of its own: two replicas purging at once write
//!   the same stream document, so one of them starts over and counts only
//!   what it deleted.
//! - **One clock**: the database's. An event's `created_at` is the
//!   server's (`$currentDate`), and a purge's cutoff is measured from the
//!   server's `localTime`.
//! - **What a row received** (roadmap M2e): an insert keeps the media id
//!   its event records as received (`NewEvent::received_media`) beside
//!   the row, and [`Outbox::received_media`] reads the rows of one id from
//!   a partial index, newest first: by the server's millisecond, then by
//!   an `ObjectId` the inserting process drew. [`Outbox::get`] is one
//!   lookup by the event's id, in its tenant's stream only.
//! - **Contention is typed**: an insert that kept meeting write conflicts
//!   for its budget ([`MongoOutbox::with_busy_after`], 2 seconds by
//!   default, as Postgres' `lock_timeout`) wrote nothing and answers
//!   `StorageError::Busy`.
//!
//! **The bindings it reads are a projection, for the spike only.** The
//! records stay out of scope until roadmap S18, so the outbox reads a
//! minimal `wa_server_wabas` collection (a WABA's tenant, when its binding
//! began, when it last left another tenant, its numbers) that whoever owns
//! the records keeps in step with them through
//! [`MongoOutbox::record_binding`] and [`MongoOutbox::forget_binding`]: in
//! this crate's tests, a memory `RecordStore` wrapped to write each binding
//! it changes here before it returns. S18's `RecordStore` writes these
//! documents itself, in the transactions that change a binding.
//!
//! [`RouteGuard::admits`]: meta_whatsapp_server_core::outbox::RouteGuard::admits
//! [`Outbox::purge`]: meta_whatsapp_server_core::outbox::Outbox::purge
//! [`Outbox::received_media`]: meta_whatsapp_server_core::outbox::Outbox::received_media
//! [`Outbox::get`]: meta_whatsapp_server_core::outbox::Outbox::get

mod outbox;
pub mod schema;

#[cfg(feature = "test-hooks")]
pub use outbox::InsertHook;
pub use outbox::{BUSY_AFTER, MongoOutbox};
