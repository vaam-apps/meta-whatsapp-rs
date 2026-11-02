//! `meta-whatsapp-server-store-postgres`: the meta-whatsapp-rs service's
//! Postgres backend (docs/design/server.md, section 8.2), through sqlx: the
//! one every deployment runs.
//!
//! [`PgBackend`] is a `meta_whatsapp_server_core::backend::Backend` on one
//! pool: the core's ports ([`PgStore`] for the records and the idempotency
//! records, [`PgEventStore`] for the event outbox, [`PgNotifier`] for its
//! notifications (`NOTIFY` on [`EVENTS_CHANNEL`], one `LISTEN` connection
//! per replica: roadmap M2b), [`PgWebhookStore`] for webhooks out
//! (roadmap M2c, migration 9), [`PgLeaderLock`]
//! (advisory locks), [`PgJanitor`] (the library's expired key/value rows),
//! [`PgMigrator`]) with the library's `PostgresKvStore` and
//! `PostgresConversationStore`. Its migrations are this crate's
//! `migrations/`, recorded in [`MIGRATIONS_TABLE`] under
//! [`MIGRATION_LOCK`] ([`migrate`]): stable identifiers
//! (docs/architecture.md, § Stable identifiers), unchanged by the move out
//! of `meta-whatsapp-server` (roadmap S7; the checksums are pinned by this
//! crate's unit tests). It passes the core's conformance suite
//! (`meta_whatsapp_server_core::conformance::run`, live:
//! `tests/live_postgres.rs`). The binary, `meta-whatsapp-server`, compiles it
//! in with its `store-postgres` feature.
//!
//! It depends on the facade (sqlx through
//! `meta_whatsapp_rs::adapters::store::postgres::sqlx`) and on the
//! service's core, never on the service's HTTP crates.

mod events;
mod notify;
mod postgres;
mod webhooks;

pub use events::{ERASE_SQL, PgEventStore};
pub use notify::{EVENTS_CHANNEL, LISTEN_WAIT, PgNotifier};
pub use postgres::{
    HOUSEKEEPING_LOCK, MIGRATION_LOCK, MIGRATIONS_TABLE, PgBackend, PgJanitor, PgLeaderLock,
    PgMigrator, PgStore, lock_key, migrate, migrations,
};
pub use webhooks::PgWebhookStore;
