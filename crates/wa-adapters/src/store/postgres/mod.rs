//! Postgres adapters (feature `postgres`): [`PostgresKvStore`] and
//! [`PostgresConversationStore`] over a [`sqlx::PgPool`].
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use wa_adapters::store::postgres::{self, PostgresConversationStore, PostgresKvStore};
//!
//! let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL")?).await?;
//! postgres::migrate(&pool).await?; // idempotent; run it at startup
//! let kv = PostgresKvStore::new(pool.clone());
//! let inbox = PostgresConversationStore::new(pool);
//! # Ok(()) }
//! ```
//!
//! # Schema and migrations
//!
//! [`migrate`] runs the migrations embedded from `crates/wa-adapters/migrations`
//! (with `sqlx::migrate!`, so the SQL ships inside the binary). Every table,
//! index and sequence name starts with a [`TablePrefix`] (`wa_` by default),
//! and so does the migration bookkeeping table (`wa_sqlx_migrations`): this
//! crate's migration history never mixes with your application's own
//! `_sqlx_migrations`, and two prefixes can live side by side in one schema.
//! Tables land in the first schema of the connection's `search_path`.
//!
//! All queries are runtime queries (no `query!` macros): building never needs
//! a database.
//!
//! # The database server's clock is the clock
//!
//! Expiry is evaluated with Postgres `now()`, never with the application's
//! clock: `Expiry::After(d)` means `d` after the server's `now()`, and a
//! record is invisible once the server's `now()` reaches `expires_at`.
//! This keeps every application instance consistent with the others, but
//! `Expiry::At(t)` is compared against the server's time — keep the
//! database host on NTP. `now()` is the start of the (single-statement)
//! transaction, and Postgres keeps microseconds, so `expires_at` and message
//! timestamps come back truncated to microseconds. An `Expiry::After` that
//! would land after year 9999 by that clock is stored as "never", as
//! `MemoryKvStore` does (`OffsetDateTime` could not read such a deadline
//! back).
//!
//! # Limitation: no U+0000
//!
//! Postgres `text` and `jsonb` cannot hold the NUL character. A message
//! whose id, contact, kind, text, payload or error contains U+0000, or a
//! `StoreKey` that does, is rejected with `StorageError::Backend` — on every
//! retry, so a webhook carrying one would be redelivered by Meta until it
//! gives up. The memory and Redis stores accept it. If your pipeline can see
//! NUL characters, strip or replace them before `append`.

mod conversation;
mod kv;

/// The `sqlx` this adapter is built against — `PgPool` is part of its API,
/// so build pools with this re-export rather than pinning sqlx yourself.
pub use sqlx;

use std::borrow::Cow;
use std::fmt;

use sqlx::PgPool;
use sqlx::migrate::{Migration, Migrator};
use sqlx::{AssertSqlSafe, SqlSafeStr};
use wa_core::error::{ConfigError, StorageError};

pub use conversation::PostgresConversationStore;
pub use kv::PostgresKvStore;

/// The migrations, embedded at compile time. They are templates: each
/// statement names its objects with a placeholder that [`migrate_with_prefix`]
/// replaces with the prefix before running them.
static MIGRATIONS: Migrator = sqlx::migrate!("./migrations");

/// The placeholder the migration files use for the table prefix.
const PLACEHOLDER: &str = "{prefix}";

/// Longest accepted prefix. Postgres truncates identifiers at 63 bytes; the
/// longest name we derive (`…messages_conversation_idx`) adds 25. A test
/// checks every identifier in the migrations against this.
const MAX_PREFIX_LEN: usize = 38;

/// A validated prefix for every table, index and sequence the Postgres
/// adapters create (`wa_` by default).
///
/// Validation (`[a-z_][a-z0-9_]*`, at most 38 bytes) is what makes it safe to
/// splice into SQL text: identifiers cannot be bound as parameters.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct TablePrefix(Cow<'static, str>);

impl TablePrefix {
    /// `wa_`.
    pub const DEFAULT: Self = Self(Cow::Borrowed("wa_"));

    /// Validate a prefix. Lower-case ASCII letters, digits and `_`, not
    /// starting with a digit, 1 to 38 bytes. End it with `_` for readable
    /// table names (`tenant1_kv`).
    pub fn new(prefix: impl Into<String>) -> Result<Self, ConfigError> {
        let prefix = prefix.into();
        let valid_start = prefix
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b == b'_');
        let valid_rest = prefix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !valid_start || !valid_rest || prefix.len() > MAX_PREFIX_LEN {
            return Err(ConfigError::new(format!(
                "invalid Postgres table prefix `{prefix}`: expected [a-z_][a-z0-9_]*, at most {MAX_PREFIX_LEN} bytes"
            )));
        }
        Ok(Self(Cow::Owned(prefix)))
    }

    /// The prefix.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `prefix` + `name`, for SQL text. Safe because the prefix is validated
    /// and `name` is always a literal in this crate.
    fn table(&self, name: &str) -> String {
        format!("{}{name}", self.0)
    }
}

impl Default for TablePrefix {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Debug for TablePrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TablePrefix({:?})", self.0)
    }
}

impl fmt::Display for TablePrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Create or upgrade the tables with the default `wa_` prefix. Idempotent and
/// safe to run from several instances at once (sqlx takes an advisory lock).
pub async fn migrate(pool: &PgPool) -> Result<(), StorageError> {
    migrate_with_prefix(pool, &TablePrefix::DEFAULT).await
}

/// Create or upgrade the tables for `prefix`. Each prefix keeps its own
/// migration history in `<prefix>sqlx_migrations`.
pub async fn migrate_with_prefix(pool: &PgPool, prefix: &TablePrefix) -> Result<(), StorageError> {
    let migrations = MIGRATIONS
        .iter()
        .map(|m| {
            // Safe to splice: the prefix is validated (see `TablePrefix`)
            // and the rest is this crate's own SQL.
            let sql = AssertSqlSafe(m.sql.as_str().replace(PLACEHOLDER, prefix.as_str()));
            Migration::new(
                m.version,
                m.description.clone(),
                m.migration_type,
                sql.into_sql_str(),
                m.no_tx,
            )
        })
        .collect();
    let mut migrator = Migrator::with_migrations(migrations);
    migrator.dangerous_set_table_name(prefix.table("sqlx_migrations"));
    migrator.run(pool).await.map_err(backend)
}

/// Wrap a sqlx (or migration) failure as the opaque storage leaf.
fn backend(error: impl std::error::Error + Send + Sync + 'static) -> StorageError {
    StorageError::Backend(anyhow::Error::new(error))
}

/// A `u64` count/limit as a Postgres `BIGINT`, saturating.
fn to_i64(n: impl TryInto<i64>) -> i64 {
    n.try_into().unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_validation() {
        assert_eq!(TablePrefix::default().as_str(), "wa_");
        assert_eq!(TablePrefix::new("tenant_1_").unwrap().as_str(), "tenant_1_");
        assert_eq!(TablePrefix::new("_x").unwrap().as_str(), "_x");
        for bad in [
            "",
            "1abc",
            "Wa_",
            "wa-",
            "wa_; drop table x; --",
            "wa\"",
            "wä_",
            &"a".repeat(MAX_PREFIX_LEN + 1),
        ] {
            assert!(TablePrefix::new(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(TablePrefix::new("a".repeat(MAX_PREFIX_LEN)).is_ok());
    }

    #[test]
    fn longest_derived_identifier_fits_postgres_limit() {
        let prefix = TablePrefix::new("a".repeat(MAX_PREFIX_LEN)).unwrap();
        for m in MIGRATIONS.iter() {
            let sql = m.sql.as_str().replace(PLACEHOLDER, prefix.as_str());
            for word in sql.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                if word.starts_with(prefix.as_str()) {
                    assert!(word.len() <= 63, "`{word}` exceeds 63 bytes");
                }
            }
        }
        assert!(prefix.table("sqlx_migrations").len() <= 63);
    }

    #[test]
    fn every_migration_is_prefixed() {
        for m in MIGRATIONS.iter() {
            let sql = m.sql.as_str();
            assert!(
                sql.contains(PLACEHOLDER),
                "migration {} uses the prefix",
                m.version
            );
            for stmt in ["CREATE TABLE ", "CREATE INDEX ", "CREATE SEQUENCE ", " ON "] {
                for (i, _) in sql.match_indices(stmt) {
                    let rest = &sql[i + stmt.len()..];
                    assert!(
                        rest.starts_with(PLACEHOLDER),
                        "migration {}: `{stmt}` not followed by the prefix placeholder",
                        m.version
                    );
                }
            }
        }
    }
}
