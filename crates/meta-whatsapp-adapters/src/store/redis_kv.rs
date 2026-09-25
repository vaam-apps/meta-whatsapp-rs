//! [`KvStore`] on Redis (feature `redis`).
//!
//! # Layout
//!
//! A record is a hash `{prefix}{<ns-len>:<ns>}:<key>` with fields `v`
//! (value), `ver` (version) and, when it expires, `exp` (unix milliseconds,
//! also set as the key's `PEXPIREAT`, so Redis itself makes it disappear).
//! The namespace is length-prefixed, so no namespace/key pair can spell
//! another's key.
//!
//! # Versions
//!
//! Versions come from one counter per **namespace**,
//! `{prefix}{<ns-len>:<ns>}#version`, which is never deleted and never
//! expires. The contract needs versions that are never reused for a key,
//! even after delete + recreate; a per-namespace counter gives that while
//! letting records expire and vanish completely. (A per-key counter would
//! have to outlive its record forever: one orphaned Redis key per webhook
//! dedup marker ever written.)
//!
//! # Atomicity
//!
//! Every conditional or multi-step write (`put`, `put_if_absent`,
//! `compare_and_swap`) is one Lua script, executed atomically by Redis.
//! `get` (`HMGET`) and `delete` (`DEL`) are single commands, atomic on their
//! own.
//!
//! What an operator needs to know (eviction policy: `noeviction` only;
//! clock, Cluster, TLS) is on [`RedisKvStore`]'s own docs: this module is
//! private, so its docs are not rendered.

use std::fmt;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
use redis::Script;
use redis::aio::{ConnectionLike, ConnectionManager};
use time::OffsetDateTime;

/// 9999-12-31T23:59:59.999Z in unix milliseconds: the latest instant
/// `OffsetDateTime` represents, so every stored `exp` reads back.
const MAX_EXP_MS: i64 = 253_402_300_799_999;

/// The write script. One script for every write so they share the expiry
/// logic; `ARGV[1]` picks the operation.
///
/// Numbers handed back to Redis go through `string.format('%d')`: Lua 5.1
/// numbers are doubles, and Redis' own number-to-string conversion may use
/// exponent notation for large values.
static WRITE: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r"
-- KEYS[1] record hash, KEYS[2] namespace version counter
-- ARGV[1] op: put | put_if_absent | cas | cas_delete
-- ARGV[2] expected version (cas, cas_delete)
-- ARGV[3] value
-- ARGV[4] expiry mode: never | after | at | keep
-- ARGV[5] expiry milliseconds (after: TTL, at: unix ms)
local op = ARGV[1]
local record = KEYS[1]

if op == 'put_if_absent' then
  if redis.call('EXISTS', record) == 1 then return false end
elseif op == 'cas' or op == 'cas_delete' then
  local current = redis.call('HGET', record, 'ver')
  if (not current) or current ~= ARGV[2] then return false end
  if op == 'cas_delete' then
    redis.call('DEL', record)
    return 0
  end
end

local t = redis.call('TIME')
local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
local mode = ARGV[4]
local exp = nil
if mode == 'after' then
  exp = now + tonumber(ARGV[5])
  -- Past year 9999 (what `OffsetDateTime` can read back) means never, as
  -- in MemoryKvStore. The TTL is capped client-side, so this sum stays
  -- exact in a double.
  if exp > 253402300799999 then exp = nil end
elseif mode == 'at' then
  exp = tonumber(ARGV[5])
elseif mode == 'keep' then
  -- The live record's deadline; none (never) for a new record.
  local current = redis.call('HGET', record, 'exp')
  if current then exp = tonumber(current) end
end
if exp and exp > 253402300799999 then
  -- Only a corrupt `exp` field can get here (`at` is at most year 9999).
  return redis.error_reply('meta-whatsapp-adapters: expiry beyond year 9999')
end

local version = redis.call('INCR', KEYS[2])
redis.call('DEL', record)
if exp and exp <= now then
  -- Written and already expired: invisible, but the version is spent.
  return version
end
local ver = string.format('%d', version)
if exp then
  local e = string.format('%d', exp)
  redis.call('HSET', record, 'v', ARGV[3], 'ver', ver, 'exp', e)
  redis.call('PEXPIREAT', record, e)
else
  redis.call('HSET', record, 'v', ARGV[3], 'ver', ver)
end
return version
",
    )
});

/// `KvStore` over a Redis connection. Cheap to clone.
///
/// `C` is any async redis connection that can be cloned and shared:
/// [`ConnectionManager`] (the default: reconnects on its own) or
/// `redis::aio::MultiplexedConnection`.
///
/// ```no_run
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// use meta_whatsapp_adapters::store::RedisKvStore;
///
/// let client = redis::Client::open("redis://127.0.0.1:6379")?;
/// let kv = RedisKvStore::new(client.get_connection_manager().await?);
/// # Ok(()) }
/// ```
///
/// # Operating Redis for it
///
/// - **Eviction policy: `noeviction`.** The policy is per instance, so use
///   a Redis instance of its own rather than one shared with a cache that
///   needs eviction. Under memory pressure Redis evicts keys *silently*, and
///   every other policy evicts something this store relies on:
///   - `volatile-*` evicts keys with a TTL, which here are the ones that
///     enforce limits: OTP issue logs and challenges (an evicted issue log
///     resets the per-number hourly issue limit, an evicted challenge the
///     resend cooldown, so an attacker gets more codes sent and more
///     guesses), webhook dedup claims and markers (an evicted marker lets
///     Meta's retry be delivered twice), and Embedded Signup sessions.
///   - `allkeys-*` evicts those and keys without a TTL too: token vault
///     records (the merchant must onboard again) and a namespace's version
///     counter (`{prefix}{<len>:<ns>}#version`, kept without a TTL so that
///     records can expire and vanish without leaving a key behind). A
///     recreated counter restarts at 1 and hands old versions out again,
///     which breaks compare-and-swap.
///
///   With `noeviction`, a full Redis refuses writes with an error instead,
///   which surfaces as `StorageError::Backend`: size `maxmemory` for the
///   dedup markers (one per webhook event, kept 7 days) and alert on it.
/// - **Clock.** Expiry uses the **Redis server's** clock (`TIME` inside the
///   script, key TTLs) at millisecond resolution: `expires_at` comes back
///   truncated to milliseconds. An `Expiry::After` that would land after
///   year 9999 is stored without expiry ("never"), as `MemoryKvStore` does.
/// - **Cluster.** The `{…}` around the namespace is a hash tag: a record and
///   its namespace counter share a slot, which the Lua script requires. One
///   namespace therefore lives on one shard. Keep an empty `{}` out of the
///   [prefix](Self::with_prefix): Redis ignores an empty tag and would hash
///   the whole key, splitting a record from its counter.
///
/// # TLS (`rediss://`): bring your own connection
///
/// This crate enables no TLS feature of `redis`, on purpose. redis-rs builds
/// its rustls configuration with `rustls::ClientConfig::builder()`, which
/// uses the **process-wide default** crypto provider. When rustls is built
/// with both of its providers — `aws-lc-rs` (which `reqwest` and `sqlx` use
/// here) and `ring` (which many other crates enable) — and the application
/// installed none, that call **panics** on the first `rediss://` connection.
/// Whether both are linked depends on the whole dependency graph of the
/// final binary (in this workspace, the repository's own `xtask` pulls
/// `ring` in through `ureq`), so a library cannot promise it will not
/// happen.
///
/// `RedisKvStore` accepts any async connection, so TLS stays with the
/// application, which picks the provider explicitly:
///
/// ```toml
/// # Your application's Cargo.toml (same `redis` major version as meta-whatsapp-adapters)
/// redis = { version = "1", features = ["tokio-rustls-comp", "connection-manager"] }
/// rustls = "0.23"
/// ```
///
/// ```ignore
/// // Once at startup, before any TLS connection is made.
/// let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
/// let client = redis::Client::open("rediss://:password@redis.example.com:6380")?;
/// let kv = meta_whatsapp_adapters::store::RedisKvStore::new(client.get_connection_manager().await?);
/// ```
#[derive(Clone)]
pub struct RedisKvStore<C = ConnectionManager> {
    conn: C,
    prefix: Arc<str>,
}

impl<C> fmt::Debug for RedisKvStore<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // No connection details: they can carry credentials.
        f.debug_struct("RedisKvStore")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl<C> RedisKvStore<C>
where
    C: ConnectionLike + Clone + Send + Sync + 'static,
{
    /// Store on `conn` with the default `wa:` key prefix.
    pub fn new(conn: C) -> Self {
        Self {
            conn,
            // Stable: predates the rename to meta-whatsapp-rs, never change
            // it (docs/architecture.md § "Stable identifiers").
            prefix: Arc::from("wa:"),
        }
    }

    /// Use `prefix` for every key instead of `wa:` (e.g. `tenant1:wa:` to
    /// share a Redis between deployments). Any string works, an empty one
    /// included; on Redis Cluster it must not contain `{}` (see
    /// [Operating Redis](Self#operating-redis-for-it)).
    #[must_use]
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Arc::from(prefix.into());
        self
    }

    /// The key prefix in use.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// `{prefix}{<len>:<ns>}`, the part a record and its counter share.
    fn tag(&self, key: &StoreKey) -> String {
        let ns = key.namespace();
        format!("{}{{{}:{ns}}}", self.prefix, ns.len())
    }

    fn record_key(&self, key: &StoreKey) -> String {
        format!("{}:{}", self.tag(key), key.key())
    }

    fn counter_key(&self, key: &StoreKey) -> String {
        format!("{}#version", self.tag(key))
    }

    async fn write(
        &self,
        key: &StoreKey,
        op: &str,
        expected: Option<u64>,
        value: &[u8],
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        let (mode, ms) = expiry_args(expiry);
        let mut invocation = WRITE.prepare_invoke();
        invocation
            .key(self.record_key(key))
            .key(self.counter_key(key))
            .arg(op)
            .arg(expected.map(|v| v.to_string()).unwrap_or_default())
            .arg(value)
            .arg(mode)
            .arg(ms);
        let mut conn = self.conn.clone();
        invocation.invoke_async(&mut conn).await.map_err(backend)
    }
}

/// `(mode, milliseconds)` for the script.
fn expiry_args(expiry: Expiry) -> (&'static str, i64) {
    match expiry {
        Expiry::Never => ("never", 0),
        Expiry::Keep => ("keep", 0),
        Expiry::After(d) => ("after", ttl_millis(d)),
        // Floor: the record never outlives the requested instant.
        Expiry::At(t) => ("at", unix_millis(t)),
    }
}

/// A TTL in milliseconds (Redis' resolution), rounded **up** so a
/// sub-millisecond TTL is not dead on arrival, and capped at
/// [`MAX_EXP_MS`]: any longer TTL lands after year 9999, which the script
/// turns into "never", and the cap keeps its arithmetic exact.
fn ttl_millis(d: Duration) -> i64 {
    let ms = d.as_nanos().div_ceil(1_000_000);
    i64::try_from(ms).map_or(MAX_EXP_MS, |ms| ms.min(MAX_EXP_MS))
}

fn unix_millis(t: OffsetDateTime) -> i64 {
    let ms = t.unix_timestamp_nanos().div_euclid(1_000_000);
    // OffsetDateTime spans years ±9999, well inside i64 milliseconds.
    i64::try_from(ms).unwrap_or(if ms < 0 { i64::MIN } else { i64::MAX })
}

fn backend(error: redis::RedisError) -> StorageError {
    StorageError::Backend(anyhow::Error::new(error))
}

fn corrupt(key: &StoreKey, what: &str) -> StorageError {
    StorageError::Backend(anyhow::anyhow!(
        "redis record for `{key}` is corrupt: {what}"
    ))
}

#[async_trait]
impl<C> KvStore for RedisKvStore<C>
where
    C: ConnectionLike + Clone + Send + Sync + 'static,
{
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        let mut conn = self.conn.clone();
        let (value, version, exp): (Option<Vec<u8>>, Option<String>, Option<String>) =
            redis::cmd("HMGET")
                .arg(self.record_key(key))
                .arg("v")
                .arg("ver")
                .arg("exp")
                .query_async(&mut conn)
                .await
                .map_err(backend)?;
        let (value, version) = match (value, version) {
            (None, None) => return Ok(None),
            (Some(value), Some(version)) => (value, version),
            _ => return Err(corrupt(key, "value without version or vice versa")),
        };
        let version = version
            .parse::<u64>()
            .map_err(|_| corrupt(key, "version is not a number"))?;
        let expires_at = exp
            .map(|ms| {
                ms.parse::<i64>()
                    .ok()
                    .and_then(|ms| {
                        OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()
                    })
                    .ok_or_else(|| corrupt(key, "expiry is not a timestamp"))
            })
            .transpose()?;
        Ok(Some(Versioned {
            value,
            version,
            expires_at,
        }))
    }

    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.write(key, "put", None, &value, expiry)
            .await?
            .ok_or_else(|| corrupt(key, "put returned no version"))
    }

    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.write(key, "put_if_absent", None, &value, expiry).await
    }

    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        match new {
            Some(value) => self.write(key, "cas", Some(expected), &value, expiry).await,
            None => {
                self.write(key, "cas_delete", Some(expected), &[], Expiry::Never)
                    .await
            }
        }
    }

    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        let mut conn = self.conn.clone();
        let removed: u64 = redis::cmd("DEL")
            .arg(self.record_key(key))
            .query_async(&mut conn)
            .await
            .map_err(backend)?;
        Ok(removed > 0)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use time::macros::datetime;

    /// A connection that records every command it is sent and answers each
    /// with an empty `HMGET` reply (every field nil), so a `get` finds
    /// nothing.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<Vec<String>>>>);

    impl ConnectionLike for Recorder {
        fn req_packed_command<'a>(
            &'a mut self,
            cmd: &'a redis::Cmd,
        ) -> redis::RedisFuture<'a, redis::Value> {
            let args = cmd
                .args_iter()
                .map(|arg| match arg {
                    redis::Arg::Simple(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                    _ => "<cursor>".to_owned(),
                })
                .collect();
            self.0.lock().unwrap().push(args);
            Box::pin(async { Ok(redis::Value::Array(vec![redis::Value::Nil; 3])) })
        }

        fn req_packed_commands<'a>(
            &'a mut self,
            _: &'a redis::Pipeline,
            _: usize,
            _: usize,
        ) -> redis::RedisFuture<'a, Vec<redis::Value>> {
            Box::pin(async { Err((redis::ErrorKind::Io, "no pipelines here").into()) })
        }

        fn get_db(&self) -> i64 {
            0
        }
    }

    /// The Redis key a record written before the rename to meta-whatsapp-rs
    /// lives under: the default `wa:` prefix, then `{<len>:<namespace>}:`
    /// and the key. A change to either strands every stored record (tokens,
    /// OTP codes, dedup markers), and fails here.
    #[tokio::test]
    async fn the_default_prefix_and_key_layout_are_pinned() {
        let conn = Recorder::default();
        let kv = RedisKvStore::new(conn.clone());
        assert_eq!(kv.prefix(), "wa:");
        let got = kv.get(&StoreKey::new("wa.token", "waba/W1")).await.unwrap();
        assert!(got.is_none());
        assert_eq!(
            *conn.0.lock().unwrap(),
            [["HMGET", "wa:{8:wa.token}:waba/W1", "v", "ver", "exp"]]
        );
    }

    #[test]
    fn ttl_rounds_up_to_milliseconds() {
        assert_eq!(ttl_millis(Duration::from_nanos(1)), 1);
        assert_eq!(ttl_millis(Duration::from_millis(1500)), 1500);
        assert_eq!(ttl_millis(Duration::ZERO), 0);
        // Capped, not an error: the script maps it to "never".
        assert_eq!(ttl_millis(Duration::MAX), MAX_EXP_MS);
    }

    #[test]
    fn instants_floor_to_milliseconds() {
        let t = datetime!(2026-09-24 12:00:00.000_999_999 UTC);
        assert_eq!(unix_millis(t), 1_790_251_200_000);
        let before_epoch = datetime!(1969-12-31 23:59:59.999_5 UTC);
        assert_eq!(unix_millis(before_epoch), -1);
        assert_eq!(
            OffsetDateTime::from_unix_timestamp_nanos(i128::from(MAX_EXP_MS) * 1_000_000).unwrap(),
            datetime!(9999-12-31 23:59:59.999 UTC)
        );
    }
}
