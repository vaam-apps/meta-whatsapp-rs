//! The service's backends: the core's storage ports
//! ([`meta_whatsapp_server_core::store`], [`meta_whatsapp_server_core::outbox`],
//! re-exported here) implemented on Postgres ([`PgBackend`]: [`PgStore`],
//! [`PgEventStore`], …) for every deployment, and in memory
//! ([`MemoryBackend`]: [`MemoryStore`], [`MemoryEventStore`], …) for
//! `WA_SERVER_ENV=development` and tests.
//!
//! The library's records (the token vault, OTP challenges, signup
//! sessions, webhook dedup) stay in its `KvStore`; these are the tables
//! the design adds (`wa_server_*`, docs/design/server.md, section 2.2).
//! The event outbox has a port of its own: [`Outbox`].

pub mod events;
mod events_postgres;
mod memory;
mod postgres;

pub use events::{MemoryEventStore, PgEventStore};
pub use memory::{MemoryBackend, MemoryLeaderLock, MemoryStore};
pub use meta_whatsapp_server_core::backend::{Backend, BackendKind};
pub use meta_whatsapp_server_core::outbox::Outbox;
pub use meta_whatsapp_server_core::store::{
    HOUSEKEEPING, IdempotencyRecords, Janitor, LeaderLock, LeaderTurn, RecordStore, SchemaMigrator,
    StoreResult, Turn, listing,
};
pub use postgres::{
    HOUSEKEEPING_LOCK, MIGRATION_LOCK, MIGRATIONS_TABLE, PgBackend, PgJanitor, PgLeaderLock,
    PgMigrator, PgStore, lock_key, migrate, migrations,
};

/// The service's records on one database: its [`RecordStore`] and its
/// [`IdempotencyRecords`], one value implementing both (as [`MemoryStore`]
/// and [`PgStore`] do). [`crate::state::AppState::new`] takes one; a
/// [`Backend`] gives the two ports apart
/// ([`crate::state::AppState::from_backend`]).
pub trait Store: RecordStore + IdempotencyRecords {}

impl<T: RecordStore + IdempotencyRecords + ?Sized> Store for T {}
