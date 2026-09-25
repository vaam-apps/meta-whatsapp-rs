//! Storage ports.
//!
//! Two shapes, because the data has two shapes:
//!
//! - [`KvStore`] — small keyed records with expiry and optimistic
//!   concurrency: encrypted business tokens, OTP challenges, webhook dedup
//!   markers, Embedded Signup session state. Every typed store in the
//!   workspace (`TokenVault`, `OtpService`, `DedupGuard`, …) is built on it,
//!   so an adapter implements five methods once and gets all of them.
//! - [`ConversationStore`] — ordered message history per conversation, for
//!   in-app chat between merchants and their customers.
//!
//! Adapters: `meta_whatsapp_adapters::store::{MemoryKvStore, PostgresKvStore, RedisKvStore,
//! MemoryConversationStore, PostgresConversationStore}`.

mod conversation;

use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;

pub use conversation::{
    ConversationKey, ConversationStore, ConversationSummary, CustomerServiceWindow, DeliveryStatus,
    Direction, StoredMessage,
};

use crate::error::StorageError;

/// A namespaced key. Namespaces keep typed stores from colliding
/// (`wa.otp`, `wa.dedup`, `wa.token`, …).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StoreKey {
    namespace: Cow<'static, str>,
    key: String,
}

impl StoreKey {
    /// Build a key.
    pub fn new(namespace: impl Into<Cow<'static, str>>, key: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            key: key.into(),
        }
    }

    /// Namespace part.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Key part.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl fmt::Display for StoreKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.namespace, self.key)
    }
}

/// A stored value with its version and expiry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Versioned {
    /// Raw bytes.
    pub value: Vec<u8>,
    /// Monotonic per key; changes on every successful write. Pass it back to
    /// [`KvStore::compare_and_swap`].
    pub version: u64,
    /// When the record stops being visible, if ever.
    pub expires_at: Option<OffsetDateTime>,
}

/// When a written record expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expiry {
    /// Never.
    Never,
    /// This long after the write.
    After(Duration),
    /// At this instant.
    At(OffsetDateTime),
    /// Keep the existing record's expiry (or never, for a new record).
    Keep,
}

/// Key/value storage with expiry and optimistic concurrency.
///
/// Semantics every adapter must honour (the conformance suite in
/// `meta_whatsapp_adapters::store::conformance` checks them):
///
/// - An expired record is invisible: `get` returns `None`, `put_if_absent`
///   succeeds over it, `compare_and_swap` fails against it.
/// - `version` strictly increases on every successful write to a key, and
///   is never reused for that key even after delete + recreate.
/// - `put_if_absent` and `compare_and_swap` are atomic with respect to each
///   other and to `put`/`delete` on the same key, across processes for
///   shared backends. OTP attempt counting and webhook dedup rely on it.
/// - A value is any bytes (U+0000 and invalid UTF-8 included) and reads
///   back exactly. A key (namespace or key) holding U+0000 is either kept
///   exactly or refused with an error (the Postgres adapter refuses it),
///   never stored as another key.
#[async_trait]
pub trait KvStore: Send + Sync + fmt::Debug + 'static {
    /// Read a live record.
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError>;

    /// Create or replace. Returns the new version.
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError>;

    /// Create only if no live record exists. Returns the new version, or
    /// `None` if a live record was already there.
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError>;

    /// Replace (`Some`) or delete (`None`) only if the live record's version
    /// is `expected`. Returns the new version on success (`0` after a
    /// delete), `None` if the version did not match or the record is gone.
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError>;

    /// Delete. Returns whether a live record was removed.
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError>;
}

#[async_trait]
impl<T: KvStore + ?Sized> KvStore for Arc<T> {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        (**self).get(key).await
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        (**self).put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        (**self).put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        (**self).compare_and_swap(key, expected, new, expiry).await
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        (**self).delete(key).await
    }
}

/// A typed view over a [`KvStore`] namespace, storing `T` as JSON.
pub struct JsonStore<T> {
    kv: Arc<dyn KvStore>,
    namespace: Cow<'static, str>,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for JsonStore<T> {
    fn clone(&self) -> Self {
        Self {
            kv: Arc::clone(&self.kv),
            namespace: self.namespace.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T> fmt::Debug for JsonStore<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsonStore")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

impl<T: Serialize + DeserializeOwned> JsonStore<T> {
    /// View `namespace` of `kv` as a store of `T`.
    pub fn new(kv: Arc<dyn KvStore>, namespace: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kv,
            namespace: namespace.into(),
            _marker: PhantomData,
        }
    }

    fn key(&self, key: &str) -> StoreKey {
        StoreKey::new(self.namespace.clone(), key)
    }

    fn encode(value: &T, key: &StoreKey) -> Result<Vec<u8>, StorageError> {
        serde_json::to_vec(value).map_err(|source| StorageError::Corrupt {
            key: key.to_string(),
            source,
        })
    }

    /// Read `(value, version, expires_at)`.
    pub async fn get(
        &self,
        key: &str,
    ) -> Result<Option<(T, u64, Option<OffsetDateTime>)>, StorageError> {
        let k = self.key(key);
        let Some(v) = self.kv.get(&k).await? else {
            return Ok(None);
        };
        let value = serde_json::from_slice(&v.value).map_err(|source| StorageError::Corrupt {
            key: k.to_string(),
            source,
        })?;
        Ok(Some((value, v.version, v.expires_at)))
    }

    /// Create or replace.
    pub async fn put(&self, key: &str, value: &T, expiry: Expiry) -> Result<u64, StorageError> {
        let k = self.key(key);
        let bytes = Self::encode(value, &k)?;
        self.kv.put(&k, bytes, expiry).await
    }

    /// Create only if absent.
    pub async fn put_if_absent(
        &self,
        key: &str,
        value: &T,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        let k = self.key(key);
        let bytes = Self::encode(value, &k)?;
        self.kv.put_if_absent(&k, bytes, expiry).await
    }

    /// Replace if the version still matches.
    pub async fn compare_and_swap(
        &self,
        key: &str,
        expected: u64,
        new: Option<&T>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        let k = self.key(key);
        let bytes = new.map(|v| Self::encode(v, &k)).transpose()?;
        self.kv.compare_and_swap(&k, expected, bytes, expiry).await
    }

    /// Delete.
    pub async fn delete(&self, key: &str) -> Result<bool, StorageError> {
        self.kv.delete(&self.key(key)).await
    }
}
