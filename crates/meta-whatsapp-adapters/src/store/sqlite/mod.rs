//! SQLite adapters (feature `sqlite`, roadmap L8): [`SqliteKvStore`] and
//! [`SqliteConversationStore`] over a [`sqlx::SqlitePool`].
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use meta_whatsapp_adapters::store::sqlite::{self, SqliteConversationStore, SqliteKvStore};
//!
//! let pool = sqlite::open("inbox.db").await?; // created if missing, WAL
//! sqlite::migrate(&pool).await?; // idempotent; run it at startup
//! let kv = SqliteKvStore::new(pool.clone());
//! let inbox = SqliteConversationStore::new(pool);
//! # Ok(()) }
//! ```
//!
//! For one process on one machine: a CLI, a desktop or edge deployment, a
//! small single-instance service, tests. Several processes may share the
//! file (SQLite locks it), never over a network file system; several
//! instances of a service behind a load balancer want Postgres. The SQLite
//! library is compiled into the binary (sqlx's `sqlite-bundled`): nothing to
//! install.
//!
//! # Schema and migrations
//!
//! [`migrate`] runs the migrations embedded from
//! `crates/meta-whatsapp-adapters/migrations-sqlite` (templates, like the
//! Postgres ones: every table and index name starts with a [`TablePrefix`],
//! `wa_` by default, and so does the migration history,
//! `wa_sqlx_migrations`). The tables are `wa_kv` and `wa_kv_version` (the
//! key/value records and their version counter), `wa_messages`,
//! `wa_conversations`, `wa_window_events`, `wa_thread_owners`,
//! `wa_synced_contacts` and `wa_identity_links`, named and shaped like the
//! Postgres adapter's.
//!
//! # Concurrency
//!
//! Every write is one transaction begun with `BEGIN IMMEDIATE`: it takes
//! SQLite's write lock first, so writes run one at a time (across processes
//! sharing the file too) and each reads and decides under the lock. A
//! writer that finds the lock taken waits up to the connection's busy
//! timeout ([`open`] sets 30 seconds) and then fails with
//! `StorageError::Backend`. Reads run beside writes under WAL.
//!
//! # Whose clock?
//!
//! [`SqliteKvStore`] decides expiry with the application's clock (an
//! injectable [`Clock`](meta_whatsapp_core::clock::Clock), the system clock
//! by default), as `MemoryKvStore` does: SQLite has no
//! server. Processes sharing a file should agree on the time (NTP).
//!
//! # Types
//!
//! Times are `INTEGER` unix microseconds: they read back truncated to
//! microseconds, as on Postgres. Identifiers (message ids, contacts, phone
//! number ids) are `TEXT`, compared byte for byte (SQLite's `BINARY`
//! collation: the order of Rust's `str`); one holding U+0000 is refused
//! with `StorageError::Backend`, as on Postgres (Meta never assigns one).
//! Content keeps U+0000: a message's kind and text, the preview and a
//! synced contact's names are `BLOB` (their UTF-8 bytes: `kind_utf8`,
//! `text_utf8`, `last_text_utf8`, `full_name_utf8`, `first_name_utf8`,
//! `username_utf8`), payloads and errors JSON text (`payload_json`,
//! `error_json`), in which `serde_json` escapes U+0000. Key/value namespaces,
//! keys and values are `BLOB`: any bytes, U+0000 included.
//!
//! # Message ids
//!
//! A message is keyed by business number and id, and a unique index on the
//! id alone (`wa_messages_id_idx`) keeps today's rule: an id is stored once
//! per store, whatever the number (`OPEN_QUESTIONS.md` #33). Ids unique per
//! number would drop the index, with no change of key.

mod conversation;
mod kv;

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

/// The `sqlx` this adapter is built against — `SqlitePool` is part of its
/// API, so build pools with this re-export rather than pinning sqlx
/// yourself.
pub use sqlx;

use meta_whatsapp_core::error::StorageError;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, SqlSafeStr, SqlitePool};
use time::OffsetDateTime;

/// The table prefix, shared with the Postgres adapters.
pub use super::TablePrefix;
pub use conversation::SqliteConversationStore;
pub use kv::SqliteKvStore;

/// The placeholder the migration files use for the table prefix.
const PLACEHOLDER: &str = "{prefix}";

/// The migrations: `(version, description, template)`. sqlx records each
/// one's checksum: an edit to a released one makes `migrate` refuse every
/// database it ran on, so a change is a new migration.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "stores",
    include_str!("../../../migrations-sqlite/0001_stores.sql"),
)];

/// How long a write waits for another one's lock before it fails: what
/// [`open`] sets.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// Open (creating it if missing) the database file at `path`, in WAL mode,
/// with a [`BUSY_TIMEOUT`] of 30 seconds and a pool of up to 8 connections.
/// Run [`migrate`] on it before use. For other options, build a pool with
/// the re-exported [`sqlx`] (keep `BEGIN IMMEDIATE`'s busy timeout in mind:
/// [module docs](self#concurrency)).
pub async fn open(path: impl AsRef<Path>) -> Result<SqlitePool, StorageError> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(BUSY_TIMEOUT);
    SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .map_err(backend)
}

/// A private in-memory database (tests, demos): one connection that the
/// pool never closes, since the database lives as long as its connection.
/// Run [`migrate`] on it before use. Everything is lost when the pool is
/// dropped.
pub async fn memory() -> Result<SqlitePool, StorageError> {
    let options = SqliteConnectOptions::from_str("sqlite::memory:").map_err(backend)?;
    SqlitePoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(options)
        .await
        .map_err(backend)
}

/// Create or upgrade the tables with the default `wa_` prefix. Idempotent.
pub async fn migrate(pool: &SqlitePool) -> Result<(), StorageError> {
    migrate_with_prefix(pool, &TablePrefix::DEFAULT).await
}

/// Create or upgrade the tables for `prefix`. Each prefix keeps its own
/// migration history in `<prefix>sqlx_migrations`.
pub async fn migrate_with_prefix(
    pool: &SqlitePool,
    prefix: &TablePrefix,
) -> Result<(), StorageError> {
    let mut migrator = Migrator::with_migrations(migrations(prefix));
    migrator.dangerous_set_table_name(prefix.table("sqlx_migrations"));
    migrator.run(pool).await.map_err(backend)
}

/// The migrations for `prefix`, as [`migrate_with_prefix`] runs them (and
/// sqlx records their checksums).
fn migrations(prefix: &TablePrefix) -> Vec<Migration> {
    MIGRATIONS
        .iter()
        .map(|(version, description, sql)| {
            // Safe to splice: the prefix is validated (see `TablePrefix`)
            // and the rest is this crate's own SQL.
            let sql = AssertSqlSafe(sql.replace(PLACEHOLDER, prefix.as_str()));
            Migration::new(
                *version,
                (*description).into(),
                MigrationType::Simple,
                sql.into_sql_str(),
                false,
            )
        })
        .collect()
}

/// The statement every write transaction begins with: SQLite's write lock
/// first, so that what a write reads cannot change before it commits.
const BEGIN_WRITE: &str = "BEGIN IMMEDIATE";

/// Wrap a sqlx (or migration) failure as the opaque storage leaf.
fn backend(error: impl std::error::Error + Send + Sync + 'static) -> StorageError {
    StorageError::Backend(anyhow::Error::new(error))
}

/// An instant as stored: unix microseconds, floored. `OffsetDateTime` spans
/// years ±9999, well inside `i64` microseconds.
fn micros(t: OffsetDateTime) -> i64 {
    let us = t.unix_timestamp_nanos().div_euclid(1_000);
    i64::try_from(us).unwrap_or(if us < 0 { i64::MIN } else { i64::MAX })
}

/// A stored instant back. `what` names the column, never the row.
fn from_micros(us: i64, what: &str) -> Result<OffsetDateTime, StorageError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(us) * 1_000).map_err(|_| {
        StorageError::Corrupt {
            key: what.to_owned(),
            source: serde::de::Error::custom(format_args!("{us} is out of range")),
        }
    })
}

/// A `u64` count/limit as an SQLite `INTEGER`, saturating.
fn to_i64(n: impl TryInto<i64>) -> i64 {
    n.try_into().unwrap_or(i64::MAX)
}

/// A database file of a test's own, removed (with its WAL files) on drop.
#[cfg(test)]
pub(crate) mod tests_support {
    use std::path::{Path, PathBuf};

    pub(crate) struct TempDb(PathBuf);

    impl TempDb {
        pub(crate) fn new(name: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let t = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
            Self(std::env::temp_dir().join(format!(
                "meta-whatsapp-adapters-{name}-{}-{t:x}-{n}.db",
                std::process::id()
            )))
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut path = self.0.clone().into_os_string();
                path.push(suffix);
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    /// The checksum sqlx records for each migration under the default
    /// prefix (SHA-384 of its SQL as `migrate` runs it). An edit to a
    /// migration file, a comment included, makes `migrate` refuse a
    /// database it ran on, and fails here: add a migration instead. Pending
    /// the owner's confirmation before the first release
    /// (docs/architecture.md § "Stable identifiers").
    #[test]
    fn the_default_tables_and_migration_checksums_are_pinned() {
        let default = TablePrefix::DEFAULT;
        // `\x77` is `w`, as in the Postgres pin.
        assert_eq!(
            [
                "kv",
                "kv_version",
                "messages",
                "conversations",
                "window_events",
                "thread_owners",
                "synced_contacts",
                "identity_links",
                "sqlx_migrations"
            ]
            .map(|t| default.table(t)),
            [
                "\x77a_kv",
                "\x77a_kv_version",
                "\x77a_messages",
                "\x77a_conversations",
                "\x77a_window_events",
                "\x77a_thread_owners",
                "\x77a_synced_contacts",
                "\x77a_identity_links",
                "\x77a_sqlx_migrations"
            ]
        );
        let recorded: Vec<(i64, String)> = migrations(&default)
            .iter()
            .map(|m| {
                let hex = m.checksum.iter().fold(String::new(), |mut hex, b| {
                    write!(hex, "{b:02x}").unwrap();
                    hex
                });
                (m.version, hex)
            })
            .collect();
        assert_eq!(recorded, PINNED_CHECKSUMS.map(|(v, h)| (v, h.to_owned())));
    }

    /// `(version, SHA-384 hex)` of every migration under the default
    /// prefix. A new migration adds a line; an existing line never changes.
    const PINNED_CHECKSUMS: [(i64, &str); 1] = [(
        1,
        "113d5a7635aff2975b922d944a5013c94290f9e003c0916429c37d8a8c84d53ba5b3e562f45a6cb1a533e8e49c68368c",
    )];

    #[test]
    fn every_migration_is_prefixed() {
        for (version, _, sql) in MIGRATIONS {
            for stmt in [
                "CREATE TABLE ",
                "CREATE INDEX ",
                "CREATE UNIQUE INDEX ",
                " ON ",
                "INSERT INTO ",
            ] {
                for (i, _) in sql.match_indices(stmt) {
                    let rest = &sql[i + stmt.len()..];
                    assert!(
                        rest.starts_with(PLACEHOLDER),
                        "migration {version}: `{stmt}` not followed by the prefix placeholder"
                    );
                }
            }
        }
    }

    #[test]
    fn times_round_trip_at_microseconds() {
        for t in [
            time::macros::datetime!(2026-09-24 12:00:00.123456789 UTC),
            time::macros::datetime!(-9999-01-01 0:00 UTC),
            time::macros::datetime!(9999-12-31 23:59:59.999999999 UTC),
            time::macros::datetime!(1969-12-31 23:59:59.9999995 UTC),
        ] {
            let back = from_micros(micros(t), "t").unwrap();
            assert!(
                back <= t && t - back < time::Duration::microseconds(1),
                "{t}"
            );
        }
    }
}
