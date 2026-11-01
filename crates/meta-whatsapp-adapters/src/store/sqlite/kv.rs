//! [`KvStore`] on SQLite.
//!
//! Every conditional or multi-step write is one `BEGIN IMMEDIATE`
//! transaction: it holds SQLite's write lock from its first statement, so
//! the record it reads is the one it replaces, across connections and
//! processes. Versions come from one counter row (`wa_kv_version`), drawn
//! under that lock: they strictly increase and are never reused, even after
//! a delete, so a deleted record's row can go at once. Expiry is decided
//! with the store's [`Clock`].

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_core::clock::{Clock, SystemClock};
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
use sqlx::sqlite::SqliteRow;
use sqlx::{AssertSqlSafe, Row, Sqlite, SqlitePool, Transaction};
use time::OffsetDateTime;

use super::{BEGIN_WRITE, TablePrefix, backend, from_micros, micros};

/// `KvStore` over an SQLite pool. Cheap to clone (the pool is shared).
///
/// Expiry uses the store's [`Clock`] (the system clock unless
/// [`with_clock`](Self::with_clock) sets one): see the
/// [module docs](super#whose-clock). Deadlines are kept in microseconds.
#[derive(Clone)]
pub struct SqliteKvStore {
    pool: SqlitePool,
    prefix: TablePrefix,
    sql: Arc<Sql>,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for SqliteKvStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The pool's options name the database file; the prefix is all a
        // log line needs.
        f.debug_struct("SqliteKvStore")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

/// The statements, rendered once per store with its prefix.
struct Sql {
    get: Arc<str>,
    live: Arc<str>,
    next_version: Arc<str>,
    write: Arc<str>,
    remove: Arc<str>,
    delete: Arc<str>,
    purge: Arc<str>,
}

impl Sql {
    fn new(prefix: &TablePrefix) -> Self {
        let kv = prefix.table("kv");
        let version = prefix.table("kv_version");
        let arc = |s: String| -> Arc<str> { Arc::from(s) };
        // A record is live when it has not expired at `now` (`?3`).
        let live = "(expires_at IS NULL OR expires_at > ?3)";
        Self {
            get: arc(format!(
                "SELECT value, version, expires_at FROM {kv} \
                 WHERE namespace = ?1 AND key = ?2 AND {live}"
            )),
            live: arc(format!(
                "SELECT version, expires_at FROM {kv} \
                 WHERE namespace = ?1 AND key = ?2 AND {live}"
            )),
            next_version: arc(format!(
                "UPDATE {version} SET version = version + 1 WHERE id = 1 RETURNING version"
            )),
            write: arc(format!(
                "INSERT INTO {kv} (namespace, key, value, version, expires_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT (namespace, key) DO UPDATE SET \
                   value = excluded.value, version = excluded.version, \
                   expires_at = excluded.expires_at"
            )),
            remove: arc(format!(
                "DELETE FROM {kv} WHERE namespace = ?1 AND key = ?2"
            )),
            // Whether the record removed was live.
            delete: arc(format!(
                "DELETE FROM {kv} WHERE namespace = ?1 AND key = ?2 RETURNING {live}"
            )),
            purge: arc(format!(
                "DELETE FROM {kv} WHERE expires_at IS NOT NULL AND expires_at <= ?1"
            )),
        }
    }
}

/// The live record's `(version, expires_at)`, as read in a write.
type Live = Option<(u64, Option<OffsetDateTime>)>;

impl SqliteKvStore {
    /// Store on `pool` with the default `wa_` tables and the system clock.
    /// Run [`migrate`](super::migrate) first.
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_prefix(pool, TablePrefix::DEFAULT)
    }

    /// Store on `pool` using `prefix`'s tables. Run
    /// [`migrate_with_prefix`](super::migrate_with_prefix) with the same
    /// prefix first.
    pub fn with_prefix(pool: SqlitePool, prefix: TablePrefix) -> Self {
        let sql = Arc::new(Sql::new(&prefix));
        Self {
            pool,
            prefix,
            sql,
            clock: Arc::new(SystemClock),
        }
    }

    /// This store deciding expiry with `clock` (tests pass a
    /// `ManualClock`).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The table prefix in use.
    pub fn prefix(&self) -> &TablePrefix {
        &self.prefix
    }

    /// Delete the records expired by the store's clock; returns how many
    /// were removed. Reads already ignore them: this is only about disk
    /// space, so call it periodically in long-running deployments (webhook
    /// dedup markers alone add a record per inbound message).
    pub async fn purge_expired(&self) -> Result<u64, StorageError> {
        let done = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.purge)))
            .bind(micros(self.clock.now()))
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected())
    }

    async fn begin(&self) -> Result<Transaction<'static, Sqlite>, StorageError> {
        self.pool.begin_with(BEGIN_WRITE).await.map_err(backend)
    }

    /// The live record of `key` at `now`, read under the write lock.
    async fn live(
        &self,
        tx: &mut Transaction<'static, Sqlite>,
        key: &StoreKey,
        now: OffsetDateTime,
    ) -> Result<Live, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.live)))
            .bind(key.namespace().as_bytes())
            .bind(key.key().as_bytes())
            .bind(micros(now))
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
        row.map(|row| {
            let version = version(&row)?;
            let expires_at: Option<i64> = row.try_get("expires_at").map_err(backend)?;
            let expires_at = expires_at
                .map(|us| from_micros(us, "kv expires_at"))
                .transpose()?;
            Ok((version, expires_at))
        })
        .transpose()
    }

    /// Write `value` under `key` with a new version and `expires_at`, in
    /// `tx`, and commit. Returns the version.
    async fn write(
        &self,
        mut tx: Transaction<'static, Sqlite>,
        key: &StoreKey,
        value: Vec<u8>,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<u64, StorageError> {
        let drawn: i64 = sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.next_version)))
            .fetch_one(&mut *tx)
            .await
            .map_err(backend)?;
        let version = u64::try_from(drawn).map_err(|_| {
            StorageError::Backend(anyhow::anyhow!("negative version {drawn} in the kv table"))
        })?;
        sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.write)))
            .bind(key.namespace().as_bytes())
            .bind(key.key().as_bytes())
            .bind(value)
            .bind(drawn)
            .bind(expires_at.map(micros))
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        Ok(version)
    }
}

/// When a record written at `now` with `expiry` expires; `live` is the
/// deadline of the live record it replaces (`None`: no live record, or one
/// that never expires), for [`Expiry::Keep`]. A TTL
/// past the last representable instant means never, as in `MemoryKvStore`.
fn resolve(
    expiry: Expiry,
    now: OffsetDateTime,
    live: Option<OffsetDateTime>,
) -> Option<OffsetDateTime> {
    match expiry {
        Expiry::Never => None,
        Expiry::After(d) => time::Duration::try_from(d)
            .ok()
            .and_then(|d| now.checked_add(d)),
        Expiry::At(t) => Some(t),
        Expiry::Keep => live,
    }
}

fn version(row: &SqliteRow) -> Result<u64, StorageError> {
    let v: i64 = row.try_get("version").map_err(backend)?;
    u64::try_from(v)
        .map_err(|_| StorageError::Backend(anyhow::anyhow!("negative version {v} in the kv table")))
}

#[async_trait]
impl KvStore for SqliteKvStore {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.get)))
            .bind(key.namespace().as_bytes())
            .bind(key.key().as_bytes())
            .bind(micros(self.clock.now()))
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let expires_at: Option<i64> = row.try_get("expires_at").map_err(backend)?;
        Ok(Some(Versioned {
            value: row.try_get("value").map_err(backend)?,
            version: version(&row)?,
            expires_at: expires_at
                .map(|us| from_micros(us, "kv expires_at"))
                .transpose()?,
        }))
    }

    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        let now = self.clock.now();
        let mut tx = self.begin().await?;
        let live = self.live(&mut tx, key, now).await?;
        let expires_at = resolve(expiry, now, live.and_then(|(_, exp)| exp));
        self.write(tx, key, value, expires_at).await
    }

    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        let now = self.clock.now();
        let mut tx = self.begin().await?;
        if self.live(&mut tx, key, now).await?.is_some() {
            return Ok(None);
        }
        let expires_at = resolve(expiry, now, None);
        self.write(tx, key, value, expires_at).await.map(Some)
    }

    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        let now = self.clock.now();
        let mut tx = self.begin().await?;
        let Some((version, live_expiry)) = self.live(&mut tx, key, now).await? else {
            return Ok(None);
        };
        if version != expected {
            return Ok(None);
        }
        if let Some(value) = new {
            let expires_at = resolve(expiry, now, live_expiry);
            return self.write(tx, key, value, expires_at).await.map(Some);
        }
        sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.remove)))
            .bind(key.namespace().as_bytes())
            .bind(key.key().as_bytes())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        Ok(Some(0))
    }

    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        // One statement: atomic on its own.
        let was_live: Option<bool> =
            sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.delete)))
                .bind(key.namespace().as_bytes())
                .bind(key.key().as_bytes())
                .bind(micros(self.clock.now()))
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        Ok(was_live.unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::conformance;
    use crate::store::sqlite::{memory, migrate};
    use meta_whatsapp_core::clock::ManualClock;

    async fn store() -> SqliteKvStore {
        let pool = memory().await.unwrap();
        migrate(&pool).await.unwrap();
        SqliteKvStore::new(pool)
    }

    #[tokio::test]
    async fn passes_kv_conformance_suite_on_a_manual_clock() {
        let clock = Arc::new(ManualClock::new(
            time::macros::datetime!(2026-09-24 12:00 UTC),
        ));
        let store = store().await.with_clock(clock.clone());
        conformance::run(&store, &|d| clock.advance(d)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn passes_kv_conformance_suite_on_real_time() {
        conformance::run_with_real_time(&store().await, std::time::Duration::from_millis(200))
            .await;
    }

    /// A file shared by a pool of several connections: the writes take the
    /// write lock in turn, and the suite's concurrency cases still hold.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn passes_kv_conformance_suite_on_a_shared_file() {
        let file = crate::store::sqlite::tests_support::TempDb::new("kv");
        let pool = crate::store::sqlite::open(file.path()).await.unwrap();
        migrate(&pool).await.unwrap();
        migrate(&pool).await.unwrap(); // idempotent
        conformance::run_with_real_time(
            &SqliteKvStore::new(pool),
            std::time::Duration::from_millis(200),
        )
        .await;
    }

    #[tokio::test]
    async fn purge_expired_removes_only_dead_records() {
        let clock = Arc::new(ManualClock::new(
            time::macros::datetime!(2026-09-24 12:00 UTC),
        ));
        let store = store().await.with_clock(clock.clone());
        let short = StoreKey::new("wa.test", "short");
        let long = StoreKey::new("wa.test", "long");
        store
            .put(
                &short,
                b"a".to_vec(),
                Expiry::After(std::time::Duration::from_secs(1)),
            )
            .await
            .unwrap();
        store
            .put(&long, b"b".to_vec(), Expiry::Never)
            .await
            .unwrap();
        assert_eq!(store.purge_expired().await.unwrap(), 0);
        clock.advance(std::time::Duration::from_secs(1));
        assert_eq!(store.purge_expired().await.unwrap(), 1);
        assert!(store.get(&long).await.unwrap().is_some());
    }

    /// A record is gone at its deadline exactly, as in `MemoryKvStore`
    /// (and as `purge_expired` deletes it): not readable, free for
    /// `put_if_absent`, not swappable.
    #[tokio::test]
    async fn a_record_expires_at_its_deadline_exactly() {
        let clock = Arc::new(ManualClock::new(
            time::macros::datetime!(2026-09-24 12:00 UTC),
        ));
        let store = store().await.with_clock(clock.clone());
        let key = StoreKey::new("wa.test", "deadline");
        let ttl = Expiry::After(std::time::Duration::from_secs(1));
        let v = store.put(&key, b"a".to_vec(), ttl).await.unwrap();
        clock.advance(std::time::Duration::from_micros(999_999));
        assert!(
            store.get(&key).await.unwrap().is_some(),
            "a microsecond before"
        );
        clock.advance(std::time::Duration::from_micros(1));
        assert_eq!(store.get(&key).await.unwrap(), None, "at the deadline");
        assert_eq!(
            store
                .compare_and_swap(&key, v, Some(b"b".to_vec()), Expiry::Keep)
                .await
                .unwrap(),
            None
        );
        assert!(!store.delete(&key).await.unwrap(), "nothing live to delete");
        let v = store.put(&key, b"a".to_vec(), ttl).await.unwrap();
        clock.advance(std::time::Duration::from_secs(1));
        assert!(
            store
                .put_if_absent(&key, b"c".to_vec(), Expiry::Never)
                .await
                .unwrap()
                .is_some_and(|w| w > v)
        );
    }

    #[tokio::test]
    async fn debug_names_the_prefix_only() {
        assert_eq!(
            format!("{:?}", store().await),
            r#"SqliteKvStore { prefix: TablePrefix("wa_"), .. }"#
        );
    }
}
