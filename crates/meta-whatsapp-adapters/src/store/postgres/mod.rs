//! Postgres adapters (feature `postgres`): [`PostgresKvStore`] and
//! [`PostgresConversationStore`] over a [`sqlx::PgPool`].
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use meta_whatsapp_adapters::store::postgres::{self, PostgresConversationStore, PostgresKvStore};
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
//! [`migrate`] runs the migrations embedded from `crates/meta-whatsapp-adapters/migrations`
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
//! # Content keeps U+0000
//!
//! Postgres `text` cannot hold the NUL character and `jsonb` refuses a
//! `\u0000` escape, so [`PostgresConversationStore`] keeps message content
//! in types that can (migration `0003`):
//!
//! | Content | Column | Type |
//! | --- | --- | --- |
//! | `kind`, `text` | `kind_utf8`, `text_utf8` | `BYTEA`, the UTF-8 bytes |
//! | `payload`, `error` | `payload_json`, `error_json` | `JSON`, the document's text as written |
//! | the summary's `last_text` | `last_text_utf8` | `BYTEA`, the UTF-8 bytes |
//!
//! Every string round-trips exactly, U+0000 included, in JSON strings and
//! object keys alike, and a NUL never reads back as U+FFFD (nor two keys
//! differing only by one as a single key). A `*_utf8` value that is not
//! UTF-8 (only a hand edit can make one) reads as `StorageError::Corrupt`
//! naming the column, never as a replacement character. What it costs, if
//! you query these tables yourself:
//!
//! - **Search and preview are bytes.** Decode `text_utf8` and
//!   `last_text_utf8` in your application (they are UTF-8).
//!   `convert_from(text_utf8, 'UTF8')` fails on a row holding a NUL, and
//!   that one row fails the whole statement; search in SQL on the bytes
//!   instead, e.g. `position(convert_to($1, 'UTF8') IN text_utf8) > 0`
//!   (exact and case-sensitive), or keep your own search index.
//! - **Payload fields are neither indexable nor safe to extract.** `json`
//!   has no equality operator and no b-tree or GIN operator class: `=`,
//!   `DISTINCT`, `GROUP BY` and `UNION` on `payload_json` or `error_json`
//!   fail on every row, and the `jsonb` operators and functions (`@>`,
//!   `?`, `jsonb_*`, `jsonb_path_*`) need a cast to `jsonb`. That cast,
//!   `->`, `->>`, `#>>` and the `json_*` functions that read keys or
//!   fields fail on a document holding `\u0000` *anywhere*, even under
//!   another key, and that one row fails the whole statement. Never put an
//!   expression index, a check constraint or a generated column on
//!   `payload_json` or `error_json`: the insert of such a message would
//!   fail, and the webhook batch with it. Read the payload in your
//!   application instead.
//! - **Names and types changed.** SQL that names `kind`, `text`,
//!   `payload`, `error` or `last_text` fails ("column does not exist"), and
//!   change-data-capture or ETL consumers of these tables see the new
//!   names, `bytea` and `json`.
//! - **Size**: `json` keeps the text (no binary form), usually a little
//!   smaller than `jsonb` for message payloads; each `->` re-parses it.
//!
//! Ordering never involves content: history pages by `(ts, id)`, the inbox by
//! `(last_message_at, contact)`, all `COLLATE "C"` or timestamps.
//!
//! **Identifiers keep `TEXT` and refuse U+0000**: a message id, contact (a
//! BSUID, `wa_id` or group id) or phone number id holding one, and a
//! [`StoreKey`](meta_whatsapp_core::store::StoreKey) namespace or key holding one, are
//! rejected with `StorageError::Backend`. Meta assigns those ids and never
//! with a NUL; `meta_whatsapp_rs::inbox::InboxSink` skips a history item that has one.
//! (The memory and Redis stores accept NUL there.) `PostgresKvStore` values
//! are `BYTEA`: any bytes, NUL included.
//!
//! # Upgrading to lossless content (migration `0003`)
//!
//! Migration 3 converts the content columns of existing rows in place and
//! renames them, in one transaction under an `ACCESS EXCLUSIVE` lock on the
//! messages and conversations tables: nothing sees it half done, and when
//! it fails it changes nothing. Existing rows keep their content byte for
//! byte: a NUL that an older revision stored as U+FFFD stays U+FFFD, the
//! original is gone. The key/value table is unchanged. In this order:
//!
//! 1. **Back up** the messages and conversations tables of every table
//!    prefix. The only way back is a restore: an older revision's
//!    [`migrate`] refuses the upgraded database (it does not know migration
//!    3). A restore loses what was recorded after the upgrade: webhooks the
//!    upgraded instances acknowledged are not delivered again, and replies
//!    sent meanwhile reached the customer but leave the history.
//! 2. **Stop every instance of the older revision that writes to these
//!    tables** (webhook receivers, anything calling `Inbox::send`).
//!    Stopping the webhook receivers pauses every consumer of those
//!    webhooks, not only the inbox (OTP delivery statuses,
//!    `PARTNER_REMOVED` revocations): Meta redelivers what failed with its
//!    own backoff, for up to 7 days, and that backoff decides how long the
//!    backlog takes once you are back. An older instance left running
//!    cannot corrupt anything, but it fails: every one of its inbox
//!    statements that touches content names a column that no longer
//!    exists, so its webhooks answer 500 (Meta redelivers them, to the
//!    upgraded instances), its inbox reads fail, and a reply it sends
//!    reaches the customer but is not recorded (it logs "message sent but
//!    not recorded").
//! 3. **Find and drop the objects of your own on the content columns**
//!    (`kind`, `text`, `payload` and `error` of `wa_messages`, `last_text`
//!    of `wa_conversations`). This query lists them, with every trigger on
//!    the two tables and every function whose body names them (for another
//!    prefix, replace `wa_messages` and `wa_conversations`); on the
//!    adapter's own tables it lists nothing:
//!
//!    ```sql
//!    SELECT pg_describe_object(d.classid, d.objid, d.objsubid) AS object,
//!           d.refobjid::regclass::text AS tbl, a.attname::text AS col
//!    FROM pg_depend d, pg_attribute a
//!    WHERE d.refclassid = 'pg_class'::regclass
//!      AND d.refobjid IN ('wa_messages'::regclass, 'wa_conversations'::regclass)
//!      AND a.attrelid = d.refobjid AND a.attnum = d.refobjsubid
//!      AND a.attname IN ('kind', 'text', 'payload', 'error', 'last_text')
//!      AND NOT (d.classid = 'pg_constraint'::regclass
//!               AND (SELECT contype FROM pg_constraint WHERE oid = d.objid) = 'n')
//!    UNION
//!    SELECT pg_describe_object('pg_trigger'::regclass, t.oid, 0), t.tgrelid::regclass::text, NULL
//!    FROM pg_trigger t
//!    WHERE t.tgrelid IN ('wa_messages'::regclass, 'wa_conversations'::regclass)
//!      AND NOT t.tgisinternal
//!    UNION
//!    SELECT 'function ' || p.oid::regprocedure::text, NULL, NULL
//!    FROM pg_proc p
//!    WHERE p.prosrc ~ '(wa_messages|wa_conversations)'
//!    ORDER BY 1
//!    ```
//!
//!    - **Anything on `payload` or `error`** (an expression or partial
//!      index, a check constraint, a view, a policy, a generated column):
//!      migration 3 refuses to run while one exists, names it, and changes
//!      nothing. An expression that reads a field would survive the
//!      conversion (no existing row holds a NUL) and then fail the insert
//!      of every payload holding one. None of them can be recreated on the
//!      `json` columns.
//!    - **On `kind`, `text` or `last_text`**: a view, rule, materialized
//!      view, policy or generated column, and a trigram (`gin_trgm_ops`,
//!      `gist_trgm_ops`), `text_pattern_ops`, full-text or `lower()` index
//!      make the migration fail, changing nothing. A plain b-tree or hash
//!      index is rebuilt on the bytes and kept (it can be created on a
//!      `*_utf8` column later too).
//!    - **Triggers and functions**: Postgres does not check their bodies,
//!      so the migration succeeds, and a trigger that names a content
//!      column (`NEW.text`, `NEW.payload`) then fails every insert: the
//!      inbox is down and every webhook batch answers 500. Rewrite them
//!      for the new names and types, or drop them.
//! 4. **Run [`migrate`] once, from a one-off job**, rather than from every
//!    instance at startup. The conversion rewrites both tables: 200,006
//!    messages (a 153 MB table) took 1 to 2 seconds on an otherwise idle
//!    Postgres 18 on a local solid-state disk, and reads and writes of both
//!    tables waited for it (the key/value table did not). The lock waits without
//!    limit behind any transaction open on those tables, and every later
//!    query on them queues behind the waiting lock; a role's or server's
//!    `statement_timeout` shorter than the rewrite cancels it (changing
//!    nothing), and a job restarted on failure then loops. Give the job's
//!    connection a `lock_timeout` (it then fails, changing nothing, instead
//!    of stalling the inbox) and no `statement_timeout`:
//!
//!    ```no_run
//!    # async fn job(url: &str) -> Result<(), Box<dyn std::error::Error>> {
//!    use std::str::FromStr;
//!    use meta_whatsapp_adapters::store::postgres::{self, sqlx};
//!
//!    let options = sqlx::postgres::PgConnectOptions::from_str(url)?
//!        .options([("lock_timeout", "10s"), ("statement_timeout", "0")]);
//!    let pool = sqlx::postgres::PgPoolOptions::new()
//!        .max_connections(1)
//!        .connect_with(options)
//!        .await?;
//!    postgres::migrate(&pool).await?; // once per table prefix
//!    # Ok(()) }
//!    ```
//!
//!    Keep free disk for about the size of `wa_messages` and its indexes
//!    (the rewrite writes a new copy before it drops the old one), plus the
//!    WAL it generates. Each [`TablePrefix`] has its own migration history:
//!    run [`migrate_with_prefix`] once for each.
//! 5. **Update SQL of your own** to the new names and types (see "Content
//!    keeps U+0000" above), and recreate what you dropped that still can
//!    be.
//! 6. **Start the new revision.** Its [`migrate`] at startup is then a
//!    no-op.

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
use meta_whatsapp_core::error::{ConfigError, StorageError};

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
            for stmt in [
                "CREATE TABLE ",
                "CREATE INDEX ",
                "CREATE SEQUENCE ",
                " ON ",
                "ALTER TABLE ",
                "LOCK TABLE ",
            ] {
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
