//! `meta-whatsapp-server-core`: the meta-whatsapp-rs service's domain,
//! without an HTTP framework or a database driver (docs/design/server.md;
//! docs/architecture.md, § Service).
//!
//! The service (`meta-whatsapp-server`) is this crate plus adapters on
//! each side: an API adapter (today the axum one, with the OpenAPI
//! document) that turns requests into calls here and [`ServiceError`]s
//! into answers, and a backend (memory, Postgres) that implements the
//! [ports](#ports). Either side can be swapped without touching the rest.
//!
//! | Module | What |
//! | --- | --- |
//! | [`model`] | tenants, API keys, bindings, idempotency records: the validated domain types |
//! | [`keys`] | the API key format, digests, constant-time comparison |
//! | [`error`] | the error model as data: codes, statuses as numbers, `retryable`, `may_have_been_sent`, Meta's `details` |
//! | [`authz`] | the authorization order: key, tenant, scope, ownership, vault |
//! | [`events`] | routing Meta's webhook events to tenants, their outbox keys and ids, polling |
//! | [`idempotency`] | `Idempotency-Key`: claim, settle, replay |
//! | [`ratelimit`] | per-tenant token buckets by route class, transfer slots |
//! | [`store`], [`outbox`], [`backend`] | the ports, and the bundle a backend gives |
//!
//! # Ports
//!
//! | Port | Methods | Implemented by the service for |
//! | --- | --- | --- |
//! | [`store::RecordStore`] | tenants, keys, bindings (`ping` … `set_waba_status`) | memory, Postgres |
//! | [`store::IdempotencyRecords`] | `claim_idempotency_key`, `complete_…`, `release_…`, `purge_…` | memory, Postgres |
//! | [`outbox::Outbox`] | `insert`, `page`, `purge` | memory, Postgres |
//! | [`store::LeaderLock`] | `try_exclusive(name)` | memory (the process), Postgres (an advisory lock) |
//! | [`store::Janitor`] | `purge_expired` | memory (nothing to do), Postgres (the library's key/value rows) |
//! | [`store::SchemaMigrator`] | `migrate` | memory (nothing to do), Postgres |
//! | [`backend::Backend`] | all of the above, and the library's `KvStore` and `ConversationStore`, over one database | memory, Postgres |
//!
//! No port names a type of a database driver, an HTTP framework or an
//! API toolkit: failures are the library's `StorageError`, times are
//! `time`'s, and an HTTP method is its name.

pub mod authz;
pub mod backend;
pub mod error;
pub mod events;
pub mod idempotency;
pub mod keys;
pub mod model;
pub mod outbox;
pub mod ratelimit;
pub mod store;

pub use error::ServiceError;
