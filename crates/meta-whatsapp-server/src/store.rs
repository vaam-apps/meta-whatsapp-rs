//! The service's backends: the core's storage ports
//! ([`meta_whatsapp_server_core::store`], [`meta_whatsapp_server_core::outbox`],
//! re-exported by `meta_whatsapp_server_http::store` and here) and the
//! backends this build composes, crates of their own since roadmap S7:
//! Postgres (`PgBackend`: `PgStore`, `PgEventStore`, …, the
//! `store-postgres` feature, `meta-whatsapp-server-store-postgres`) for
//! every deployment, and memory (`MemoryBackend`: `MemoryStore`,
//! `MemoryEventStore`, …, the `store-memory` feature,
//! `meta-whatsapp-server-store-memory`) for `WA_SERVER_ENV=development`
//! and tests.
//!
//! The library's records (the token vault, OTP challenges, signup
//! sessions, webhook dedup) stay in its `KvStore`; these are the tables
//! the design adds (`wa_server_*`, docs/design/server.md, section 2.2).
//! The event outbox has a port of its own: [`Outbox`], and its wake-ups
//! another: [`EventNotifier`] (`PgNotifier`, `LocalNotifier`; roadmap
//! M2b).

pub use meta_whatsapp_server_http::store::{
    Backend, BackendKind, EventNotifier, HOUSEKEEPING, HOUSEKEEPING_LEASE, IdempotencyRecords,
    Janitor, LeaderLock, LeaderTurn, LocalNotifier, Outbox, RecordStore, SchemaMigrator, Store,
    StoreResult, Turn, listing,
};
#[cfg(feature = "store-memory")]
pub use meta_whatsapp_server_store_memory::{MemoryBackend, MemoryLeaderLock, MemoryStore};
#[cfg(feature = "store-postgres")]
pub use meta_whatsapp_server_store_postgres::{
    EVENTS_CHANNEL, HOUSEKEEPING_LOCK, MIGRATION_LOCK, MIGRATIONS_TABLE, PgBackend, PgJanitor,
    PgLeaderLock, PgMigrator, PgNotifier, PgStore, lock_key, migrate, migrations,
};

/// The event outbox (`wa_server_events`, docs/design/server.md, section
/// 2.3): the core's port ([`Outbox`]) and its types, and the backends'
/// implementations this build has: `PgEventStore` and `MemoryEventStore`
/// (development and tests). Both keep the port's two contracts: **a
/// stream's inserts commit in sequence order**, so a reader that sees
/// sequence `n` of a tenant sees every event of that tenant before it, and
/// `next_after` never skips one that commits later; and **an insert checks
/// the routing again** (`RouteGuard`), atomically with it.
pub mod events {
    pub use meta_whatsapp_server_http::store::events::*;
    #[cfg(feature = "store-memory")]
    pub use meta_whatsapp_server_store_memory::MemoryEventStore;
    #[cfg(feature = "store-postgres")]
    pub use meta_whatsapp_server_store_postgres::PgEventStore;
}

#[cfg(feature = "store-memory")]
pub use events::MemoryEventStore;
#[cfg(feature = "store-postgres")]
pub use events::PgEventStore;
