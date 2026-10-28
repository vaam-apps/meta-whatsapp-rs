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
//! | [`admin`] | the admin operations as services: tenants, keys, platform keys, attaching a WABA (decision D4), reading and unbinding a binding, the vault's rotation |
//! | [`messages`] | the message operations as services: the send request (free of any API toolkit), the recipient and E.164 rules, sending (the accepted message) and marking read |
//! | [`numbers`] | the number operations as services: a number's live details, its business profile read and changed, disconnecting a WABA |
//! | [`templates`] | the template operations as services: the list (cached per WABA, [`templates::TemplateCache`]), the lookup of an id in a WABA, creation (the definition checked first), deletion |
//! | [`request`] | what the requests share: a Meta object read into the library's type, no key dropped; a Meta object id checked before it reaches a Graph path |
//! | [`keys`] | the API key format, digests, constant-time comparison |
//! | [`error`] | the error model as data: codes, statuses as numbers, `retryable`, `may_have_been_sent`, Meta's `details` |
//! | [`authz`] | the authorization order: key, tenant, scope, ownership, vault |
//! | [`audit`], [`observe`] | the audit events (target `audit`); what an operation reports to the adapter's metrics |
//! | [`events`] | routing Meta's webhook events to tenants, their outbox keys and ids, polling |
//! | [`idempotency`] | `Idempotency-Key`: claim, settle, replay; running an operation under a key (`run`: a status, a body, and whether it replayed) |
//! | [`ratelimit`] | per-tenant token buckets by route class, transfer slots |
//! | [`store`], [`outbox`], [`backend`] | the ports, and the bundle a backend gives |
//! | [`housekeeping`] | the periodic purges, over a [`backend::Backend`], one replica at a time |
//! | `conformance` (feature `conformance`) | the ports' contracts as executable suites, run over a [`backend::Backend`]: every backend's tests run them |
//!
//! # Ports
//!
//! | Port | Methods | Implemented by the service for |
//! | --- | --- | --- |
//! | [`store::RecordStore`] | tenants, keys, bindings (`ping` … `set_waba_status`) | memory, Postgres |
//! | [`store::IdempotencyRecords`] | `claim_idempotency_key`, `complete_…`, `release_…`, `purge_…` | memory, Postgres |
//! | [`outbox::Outbox`] | `insert`, `page`, `purge` | memory, Postgres |
//! | [`store::LeaderLock`] | `try_exclusive(name, lease)`: a lease | memory (the process), Postgres (an advisory lock, the session ended past the lease) |
//! | [`store::Janitor`] | `purge_expired` | memory (nothing to do), Postgres (the library's key/value rows) |
//! | [`store::SchemaMigrator`] | `migrate` | memory (nothing to do), Postgres |
//! | [`backend::Backend`] | all of the above, and the library's `KvStore` and `ConversationStore`, over one database | memory, Postgres |
//!
//! No port names a type of a database driver, an HTTP framework or an
//! API toolkit: failures are the library's `StorageError`, times are
//! `time`'s, and an HTTP method is its name. What every backend
//! guarantees, whatever its database (the routing checked again with each
//! outbox insert, the referential rules, one clock, typed contention,
//! purges without locks, byte order), is [`store`]'s module
//! documentation; the suites of feature `conformance`
//! (`meta_whatsapp_server_core::conformance::run`) check it, over a
//! backend's bundle.

pub mod admin;
pub mod audit;
pub mod authz;
pub mod backend;
#[cfg(feature = "conformance")]
pub mod conformance;
pub mod error;
pub mod events;
pub mod housekeeping;
pub mod idempotency;
pub mod keys;
pub mod messages;
pub mod model;
pub mod numbers;
pub mod observe;
pub mod outbox;
pub mod ratelimit;
pub mod request;
pub mod store;
pub mod templates;

pub use error::ServiceError;
