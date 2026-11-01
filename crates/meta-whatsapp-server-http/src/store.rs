//! The service's storage ports ([`meta_whatsapp_server_core::store`],
//! [`meta_whatsapp_server_core::outbox`], re-exported here). The backends
//! that implement them are crates of their own since roadmap S7:
//! `meta-whatsapp-server-store-postgres` (`PgBackend`, every deployment)
//! and `meta-whatsapp-server-store-memory` (`MemoryBackend`,
//! `WA_SERVER_ENV=development` and tests), which the binary,
//! `meta-whatsapp-server`, re-exports beside these in its own `store`.
//!
//! The library's records (the token vault, OTP challenges, signup
//! sessions, webhook dedup) stay in its `KvStore`; these are the tables
//! the design adds (`wa_server_*`, docs/design/server.md, section 2.2).
//! The event outbox has a port of its own: [`Outbox`].

pub use meta_whatsapp_server_core::backend::{Backend, BackendKind};
pub use meta_whatsapp_server_core::outbox::Outbox;
pub use meta_whatsapp_server_core::store::{
    HOUSEKEEPING, HOUSEKEEPING_LEASE, IdempotencyRecords, Janitor, LeaderLock, LeaderTurn,
    RecordStore, SchemaMigrator, StoreResult, Turn, listing,
};

/// The event outbox's types (`wa_server_events`, docs/design/server.md,
/// section 2.3): the core's port ([`Outbox`]) and what it carries.
pub mod events {
    pub use meta_whatsapp_server_core::outbox::{
        AfterMove, DatedBounds, DedupWindow, EventPage, EventQuery, GuardedBinding, NewEvent,
        Outbox, RouteGuard, StoredEvent,
    };
}

/// The service's records on one database: its [`RecordStore`] and its
/// [`IdempotencyRecords`], one value implementing both (as the memory and
/// Postgres backends' stores do), for code that holds the two together.
/// The service itself is composed from a [`Backend`] alone, which gives the
/// two ports apart ([`crate::state::AppState::from_backend`]).
pub trait Store: RecordStore + IdempotencyRecords {}

impl<T: RecordStore + IdempotencyRecords + ?Sized> Store for T {}
