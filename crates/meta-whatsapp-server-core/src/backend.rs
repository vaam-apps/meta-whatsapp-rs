//! [`Backend`]: every storage port the service runs on, over one
//! database. The service composes one at start (memory in development,
//! Postgres everywhere else); an integrator swaps it for their own (a
//! MongoDB one, say) by implementing the ports and this bundle.
//!
//! One bundle rather than loose ports, because the ports share their
//! database's guarantees: deleting a tenant ([`RecordStore::delete_tenant`])
//! reaches its idempotency records and its event stream, and a Postgres
//! outbox insert re-reads the bindings under a lock. Two ports from two
//! backends would lose both.

use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};

use crate::outbox::Outbox;
use crate::store::{IdempotencyRecords, Janitor, LeaderLock, RecordStore, SchemaMigrator};

/// What a backend is, for what the service does differently on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// One process's memory, emptied on restart (`WA_SERVER_ENV=development`
    /// and tests): nothing outside the process reaches it, the service's
    /// own command line included.
    Memory,
    /// Postgres, shared by every replica.
    Postgres,
    /// Another database, shared by every replica: its name, for logs.
    Other(&'static str),
}

impl BackendKind {
    /// Its name, for logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Postgres => "postgres",
            Self::Other(name) => name,
        }
    }

    /// Whether the records live in this process only: lost on restart, and
    /// out of reach of anything else (the command line cannot mint a key
    /// in them, say).
    pub fn is_process_local(self) -> bool {
        matches!(self, Self::Memory)
    }
}

/// Every port the service stores through, over one database: its own
/// ([`RecordStore`], [`IdempotencyRecords`], [`Outbox`], [`LeaderLock`],
/// [`Janitor`], [`SchemaMigrator`]) and the library's ([`KvStore`]: the
/// token vault and webhook dedup; [`ConversationStore`]: the inbox).
///
/// **Every accessor returns a handle to the same data on every call**:
/// what one call's handle writes, the next call's handle reads. The
/// service asks more than once (the vault and webhook dedup each take a
/// [`Self::kv`], say). A database-backed store meets this by construction;
/// an in-process one must be built once and shared (an `Arc` the backend
/// keeps), never made anew per call, which would lose every write.
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// What it is.
    fn kind(&self) -> BackendKind;

    /// Tenants, keys and bindings.
    fn records(&self) -> Arc<dyn RecordStore>;

    /// `Idempotency-Key` records.
    fn idempotency(&self) -> Arc<dyn IdempotencyRecords>;

    /// The event outbox.
    fn outbox(&self) -> Arc<dyn Outbox>;

    /// Leader election for housekeeping.
    fn leader_lock(&self) -> Arc<dyn LeaderLock>;

    /// The expired rows nothing else deletes.
    fn janitor(&self) -> Arc<dyn Janitor>;

    /// The schema's migrations.
    fn migrator(&self) -> Arc<dyn SchemaMigrator>;

    /// The library's key/value store (the token vault's, webhook dedup's).
    fn kv(&self) -> Arc<dyn KvStore>;

    /// The library's conversation store (the inbox's).
    fn conversations(&self) -> Arc<dyn ConversationStore>;

    /// Release its connections, once the service stopped serving.
    async fn close(&self);
}
