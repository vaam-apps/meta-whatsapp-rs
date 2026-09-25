//! [`KvStore`] on Postgres.
//!
//! Every operation is **one statement**, so atomicity comes from Postgres
//! row locking rather than from a transaction we manage:
//!
//! - `put` / `put_if_absent` are `INSERT … ON CONFLICT DO UPDATE`; the
//!   conflict arm locks the existing row and (for `put_if_absent`) only
//!   overwrites it when it is a tombstone or expired.
//! - `compare_and_swap` is `UPDATE … WHERE version = $expected AND live`. Under
//!   READ COMMITTED a concurrent writer's update makes Postgres re-check the
//!   `WHERE` against the committed row, so of N racers on one version exactly
//!   one matches.
//!
//! Versions come from one table-wide sequence (see the migration): never
//! reused for any key, even after a delete or a purge. Inside the conflict
//! arm the version is drawn *after* the row lock, so successive versions of
//! a key strictly increase. A delete leaves a tombstone row (value `NULL`)
//! until [`PostgresKvStore::purge_expired`], so a writer that drew a version
//! before a delete still meets a row, takes the conflict arm and re-draws.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
use sqlx::postgres::PgRow;
use sqlx::{AssertSqlSafe, PgPool, Row};
use time::OffsetDateTime;

use super::{TablePrefix, backend};

/// How long a tombstone or expired row survives before
/// [`PostgresKvStore::purge_expired`] may remove it. It only has to outlast
/// one in-flight statement (the window between drawing a version and
/// inserting); ten minutes is orders of magnitude more than that.
const PURGE_GRACE: &str = "10 minutes";

/// `KvStore` over a Postgres pool. Cheap to clone (the pool is shared).
///
/// Expiry uses the **database server's** `now()`; see the
/// [module docs](super#the-database-servers-clock-is-the-clock).
#[derive(Clone)]
pub struct PostgresKvStore {
    pool: PgPool,
    prefix: TablePrefix,
    sql: Arc<Sql>,
}

impl fmt::Debug for PostgresKvStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The pool's options could reveal connection settings; the prefix
        // is all a log line needs.
        f.debug_struct("PostgresKvStore")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

/// The statements, rendered once per store with its prefix.
struct Sql {
    get: Arc<str>,
    put: Arc<str>,
    put_if_absent: Arc<str>,
    cas_set: Arc<str>,
    cas_delete: Arc<str>,
    delete: Arc<str>,
    purge: Arc<str>,
}

/// A row is live when it is not a tombstone and has not expired by the
/// server's clock.
const LIVE: &str = "value IS NOT NULL AND (expires_at IS NULL OR expires_at > now())";

/// The last instant `OffsetDateTime` represents at Postgres' precision.
const LAST_INSTANT: &str = "9999-12-31 23:59:59.999999+00";

/// Longest TTL sent to Postgres: 10 000 years in microseconds. Anything
/// longer already means "never" (it lands after [`LAST_INSTANT`]), and the
/// cap keeps `now() + ttl` inside Postgres' interval and timestamp ranges —
/// the planner may evaluate that sum while planning, before the `CASE` that
/// would have discarded it.
const MAX_AFTER_US: i64 = 10_000 * 366 * 24 * 3600 * 1_000_000;

impl Sql {
    fn new(prefix: &TablePrefix) -> Self {
        let kv = prefix.table("kv");
        let seq = prefix.table("kv_version_seq");
        // Expiry parameters, shared by the writes: `after` (microseconds,
        // BIGINT) and `at` (TIMESTAMPTZ); both NULL means "never". A TTL
        // that lands after year 9999 by the server's clock is also "never",
        // as in `MemoryKvStore`: `OffsetDateTime` could not read it back, so
        // storing it would make the key unreadable.
        let new_exp = |after: u8, at: u8| {
            let deadline = format!("now() + ${after}::bigint * interval '1 microsecond'");
            format!(
                "COALESCE(${at}::timestamptz, \
                   CASE WHEN {deadline} > timestamptz '{LAST_INSTANT}' THEN NULL ELSE {deadline} END)"
            )
        };
        Self {
            get: arc(format!(
                "SELECT value, version, expires_at FROM {kv} \
                 WHERE namespace = $1 AND key = $2 AND {LIVE}"
            )),
            // $1 ns, $2 key, $3 value, $4 after_us, $5 at, $6 keep
            put: arc(format!(
                "INSERT INTO {kv} AS kv (namespace, key, value, version, expires_at, updated_at) \
                 VALUES ($1, $2, $3, nextval('{seq}'), {exp}, now()) \
                 ON CONFLICT (namespace, key) DO UPDATE SET \
                   value = EXCLUDED.value, \
                   version = nextval('{seq}'), \
                   expires_at = CASE \
                     WHEN NOT $6 THEN EXCLUDED.expires_at \
                     WHEN kv.value IS NOT NULL AND (kv.expires_at IS NULL OR kv.expires_at > now()) \
                       THEN kv.expires_at \
                     ELSE NULL END, \
                   updated_at = now() \
                 RETURNING version",
                exp = new_exp(4, 5)
            )),
            // $1 ns, $2 key, $3 value, $4 after_us, $5 at
            put_if_absent: arc(format!(
                "INSERT INTO {kv} AS kv (namespace, key, value, version, expires_at, updated_at) \
                 VALUES ($1, $2, $3, nextval('{seq}'), {exp}, now()) \
                 ON CONFLICT (namespace, key) DO UPDATE SET \
                   value = EXCLUDED.value, \
                   version = nextval('{seq}'), \
                   expires_at = EXCLUDED.expires_at, \
                   updated_at = now() \
                 WHERE kv.value IS NULL OR (kv.expires_at IS NOT NULL AND kv.expires_at <= now()) \
                 RETURNING version",
                exp = new_exp(4, 5)
            )),
            // $1 ns, $2 key, $3 expected, $4 value, $5 after_us, $6 at, $7 keep
            cas_set: arc(format!(
                "UPDATE {kv} SET \
                   value = $4, \
                   version = nextval('{seq}'), \
                   expires_at = CASE WHEN $7 THEN expires_at ELSE {exp} END, \
                   updated_at = now() \
                 WHERE namespace = $1 AND key = $2 AND version = $3 AND {LIVE} \
                 RETURNING version",
                exp = new_exp(5, 6)
            )),
            // $1 ns, $2 key, $3 expected
            cas_delete: arc(format!(
                "UPDATE {kv} SET value = NULL, expires_at = NULL, updated_at = now() \
                 WHERE namespace = $1 AND key = $2 AND version = $3 AND {LIVE}"
            )),
            delete: arc(format!(
                "UPDATE {kv} SET value = NULL, expires_at = NULL, updated_at = now() \
                 WHERE namespace = $1 AND key = $2 AND {LIVE}"
            )),
            purge: arc(format!(
                "DELETE FROM {kv} \
                 WHERE (value IS NULL OR expires_at <= now()) \
                   AND updated_at < now() - interval '{PURGE_GRACE}'"
            )),
        }
    }
}

fn arc(sql: String) -> Arc<str> {
    Arc::from(sql)
}

/// An [`Expiry`] as the `(after_us, at, keep)` parameter triple.
struct ExpiryParams {
    after_us: Option<i64>,
    at: Option<OffsetDateTime>,
    keep: bool,
}

impl ExpiryParams {
    fn new(expiry: Expiry) -> Self {
        match expiry {
            Expiry::Never => Self {
                after_us: None,
                at: None,
                keep: false,
            },
            Expiry::After(d) => Self {
                after_us: Some(after_micros(d)),
                at: None,
                keep: false,
            },
            Expiry::At(t) => Self {
                after_us: None,
                at: Some(t),
                keep: false,
            },
            Expiry::Keep => Self {
                after_us: None,
                at: None,
                keep: true,
            },
        }
    }
}

/// A TTL in whole microseconds (Postgres' resolution), rounded **up** so a
/// sub-microsecond TTL is not dead on arrival, and capped at
/// [`MAX_AFTER_US`] (the SQL turns anything past year 9999 into "never").
fn after_micros(d: Duration) -> i64 {
    let micros = d.as_nanos().div_ceil(1_000);
    i64::try_from(micros).map_or(MAX_AFTER_US, |m| m.min(MAX_AFTER_US))
}

/// A version as stored (`BIGINT`) back to the port's `u64`.
fn version(row: &PgRow) -> Result<u64, StorageError> {
    let v: i64 = row.try_get("version").map_err(backend)?;
    u64::try_from(v)
        .map_err(|_| StorageError::Backend(anyhow::anyhow!("negative version {v} in the kv table")))
}

impl PostgresKvStore {
    /// Store on `pool` with the default `wa_` tables. Run
    /// [`migrate`](super::migrate) first.
    pub fn new(pool: PgPool) -> Self {
        Self::with_prefix(pool, TablePrefix::DEFAULT)
    }

    /// Store on `pool` using `prefix`'s tables. Run
    /// [`migrate_with_prefix`](super::migrate_with_prefix) with the same
    /// prefix first.
    pub fn with_prefix(pool: PgPool, prefix: TablePrefix) -> Self {
        let sql = Arc::new(Sql::new(&prefix));
        Self { pool, prefix, sql }
    }

    /// The table prefix in use.
    pub fn prefix(&self) -> &TablePrefix {
        &self.prefix
    }

    /// Delete rows that have been dead (deleted or expired) and untouched
    /// for more than ten minutes; returns how many were removed.
    ///
    /// Reads already ignore dead rows, so this is only about disk space:
    /// call it periodically (every few minutes to hourly) in long-running
    /// deployments — webhook dedup markers alone add a row per inbound
    /// message. Dead rows younger than the grace period are kept on purpose:
    /// they stop a write that was already in flight when its key died from
    /// re-creating the key with a *lower* version than the dead one. Versions
    /// are drawn from a table-wide sequence, so removing a row never allows a
    /// version to be reused.
    pub async fn purge_expired(&self) -> Result<u64, StorageError> {
        let done = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.purge)))
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected())
    }
}

#[async_trait]
impl KvStore for PostgresKvStore {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.get)))
            .bind(key.namespace())
            .bind(key.key())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.map(|row| {
            Ok(Versioned {
                value: row.try_get("value").map_err(backend)?,
                version: version(&row)?,
                expires_at: row.try_get("expires_at").map_err(backend)?,
            })
        })
        .transpose()
    }

    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        let exp = ExpiryParams::new(expiry);
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.put)))
            .bind(key.namespace())
            .bind(key.key())
            .bind(value)
            .bind(exp.after_us)
            .bind(exp.at)
            .bind(exp.keep)
            .fetch_one(&self.pool)
            .await
            .map_err(backend)?;
        version(&row)
    }

    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        // `Keep` on a new record means "never": its parameters are both NULL.
        let exp = ExpiryParams::new(expiry);
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.put_if_absent)))
            .bind(key.namespace())
            .bind(key.key())
            .bind(value)
            .bind(exp.after_us)
            .bind(exp.at)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(version).transpose()
    }

    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        // Versions are BIGINT; a larger `expected` can never match.
        let Ok(expected) = i64::try_from(expected) else {
            return Ok(None);
        };
        let Some(value) = new else {
            let done = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.cas_delete)))
                .bind(key.namespace())
                .bind(key.key())
                .bind(expected)
                .execute(&self.pool)
                .await
                .map_err(backend)?;
            return Ok((done.rows_affected() == 1).then_some(0));
        };
        let exp = ExpiryParams::new(expiry);
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.cas_set)))
            .bind(key.namespace())
            .bind(key.key())
            .bind(expected)
            .bind(value)
            .bind(exp.after_us)
            .bind(exp.at)
            .bind(exp.keep)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(version).transpose()
    }

    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        let done = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.delete)))
            .bind(key.namespace())
            .bind(key.key())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected() == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_rounds_up_to_microseconds() {
        assert_eq!(after_micros(Duration::from_nanos(1)), 1);
        assert_eq!(after_micros(Duration::from_micros(7)), 7);
        assert_eq!(after_micros(Duration::from_millis(1500)), 1_500_000);
        assert_eq!(after_micros(Duration::ZERO), 0);
    }

    #[test]
    fn absurd_ttl_is_capped_not_an_error_or_a_panic() {
        // The SQL maps anything past year 9999 to "never"; the conformance
        // suite checks that end to end.
        assert_eq!(after_micros(Duration::MAX), MAX_AFTER_US);
        assert_eq!(
            after_micros(Duration::from_hours(20_000 * 365 * 24)),
            MAX_AFTER_US
        );
        let ten_millennia = time::Duration::microseconds(MAX_AFTER_US);
        assert!(ten_millennia > time::Duration::days(10_000 * 365));
    }
}
