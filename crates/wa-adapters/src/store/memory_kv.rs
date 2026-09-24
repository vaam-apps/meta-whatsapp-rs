//! In-process [`KvStore`]. Honours the full contract (expiry, versions,
//! atomic CAS) within one process; state is lost on restart. Use it for
//! tests, development and single-instance deployments that can afford to
//! lose OTPs and dedup markers on restart.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use wa_core::clock::{Clock, SystemClock};
use wa_core::error::StorageError;
use wa_core::store::{Expiry, KvStore, StoreKey, Versioned};

#[derive(Debug, Clone)]
struct Entry {
    value: Vec<u8>,
    version: u64,
    expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Default)]
struct State {
    entries: HashMap<StoreKey, Entry>,
    /// Last version handed out per key, kept across deletes so versions are
    /// never reused for a key.
    versions: HashMap<StoreKey, u64>,
}

/// In-memory key/value store.
#[derive(Clone)]
pub struct MemoryKvStore {
    state: Arc<Mutex<State>>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for MemoryKvStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Counts only: a derived Debug prints every stored value (encrypted
        // tokens, OTP hashes) through tokio's Mutex Debug.
        let mut d = f.debug_struct("MemoryKvStore");
        match self.state.try_lock() {
            Ok(st) => d.field("entries", &st.entries.len()),
            Err(_) => d.field("entries", &format_args!("<locked>")),
        };
        d.field("clock", &self.clock).finish()
    }
}

impl Default for MemoryKvStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryKvStore {
    /// Empty store on the system clock.
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    /// Empty store on `clock` (tests pass a `ManualClock`).
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            clock,
        }
    }

    /// Drop expired entries. Reads already ignore them; call this
    /// periodically in long-running processes to bound memory.
    pub async fn purge_expired(&self) -> usize {
        let now = self.clock.now();
        let mut st = self.state.lock().await;
        let before = st.entries.len();
        st.entries
            .retain(|_, e| e.expires_at.is_none_or(|t| t > now));
        before - st.entries.len()
    }
}

fn resolve(
    expiry: Expiry,
    now: OffsetDateTime,
    existing: Option<&Entry>,
) -> Option<OffsetDateTime> {
    match expiry {
        Expiry::Never => None,
        // An expiry beyond the representable range is, in practice, never.
        Expiry::After(d) => time::Duration::try_from(d)
            .ok()
            .and_then(|d| now.checked_add(d)),
        Expiry::At(t) => Some(t),
        Expiry::Keep => existing.and_then(|e| e.expires_at),
    }
}

impl State {
    fn live(&self, key: &StoreKey, now: OffsetDateTime) -> Option<&Entry> {
        self.entries
            .get(key)
            .filter(|e| e.expires_at.is_none_or(|t| t > now))
    }

    fn next_version(&mut self, key: &StoreKey) -> u64 {
        let v = self.versions.entry(key.clone()).or_insert(0);
        *v += 1;
        *v
    }

    fn write(&mut self, key: &StoreKey, value: Vec<u8>, expires_at: Option<OffsetDateTime>) -> u64 {
        let version = self.next_version(key);
        self.entries.insert(
            key.clone(),
            Entry {
                value,
                version,
                expires_at,
            },
        );
        version
    }
}

#[async_trait]
impl KvStore for MemoryKvStore {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        let now = self.clock.now();
        let st = self.state.lock().await;
        Ok(st.live(key, now).map(|e| Versioned {
            value: e.value.clone(),
            version: e.version,
            expires_at: e.expires_at,
        }))
    }

    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        let now = self.clock.now();
        let mut st = self.state.lock().await;
        let expires_at = resolve(expiry, now, st.live(key, now));
        Ok(st.write(key, value, expires_at))
    }

    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        let now = self.clock.now();
        let mut st = self.state.lock().await;
        if st.live(key, now).is_some() {
            return Ok(None);
        }
        let expires_at = resolve(expiry, now, None);
        Ok(Some(st.write(key, value, expires_at)))
    }

    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        let now = self.clock.now();
        let mut st = self.state.lock().await;
        let Some(current) = st.live(key, now) else {
            return Ok(None);
        };
        if current.version != expected {
            return Ok(None);
        }
        if let Some(value) = new {
            let expires_at = resolve(expiry, now, Some(current));
            Ok(Some(st.write(key, value, expires_at)))
        } else {
            st.entries.remove(key);
            Ok(Some(0))
        }
    }

    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        let now = self.clock.now();
        let mut st = self.state.lock().await;
        let was_live = st.live(key, now).is_some();
        st.entries.remove(key);
        Ok(was_live)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::conformance;
    use wa_core::clock::ManualClock;

    #[tokio::test]
    async fn passes_conformance_suite() {
        let clock = ManualClock::new(time::macros::datetime!(2026-09-24 12:00 UTC));
        let store = MemoryKvStore::with_clock(Arc::new(clock.clone()));
        conformance::run(&store, &|d| clock.advance(d)).await;
    }

    #[tokio::test]
    async fn debug_shows_counts_not_values() {
        let store = MemoryKvStore::new();
        store
            .put(
                &StoreKey::new("wa.token", "waba-1"),
                b"EAAG-secret-token".to_vec(),
                Expiry::Never,
            )
            .await
            .unwrap();
        let rendered = format!("{store:?}");
        assert!(rendered.contains("entries: 1"), "{rendered}");
        assert!(!rendered.contains("69, 65, 65, 71"), "{rendered}"); // b"EAAG"
    }

    #[tokio::test]
    async fn huge_ttl_means_never_instead_of_panicking() {
        let store = MemoryKvStore::new();
        let k = StoreKey::new("t", "huge");
        store
            .put(&k, b"a".to_vec(), Expiry::After(std::time::Duration::MAX))
            .await
            .unwrap();
        assert_eq!(store.get(&k).await.unwrap().unwrap().expires_at, None);
    }

    #[tokio::test]
    async fn purge_drops_only_expired() {
        let clock = ManualClock::new(time::macros::datetime!(2026-09-24 12:00 UTC));
        let store = MemoryKvStore::with_clock(Arc::new(clock.clone()));
        let k1 = StoreKey::new("t", "1");
        let k2 = StoreKey::new("t", "2");
        store
            .put(
                &k1,
                b"a".to_vec(),
                Expiry::After(std::time::Duration::from_secs(1)),
            )
            .await
            .unwrap();
        store.put(&k2, b"b".to_vec(), Expiry::Never).await.unwrap();
        clock.advance(std::time::Duration::from_secs(2));
        assert_eq!(store.purge_expired().await, 1);
        assert!(store.get(&k2).await.unwrap().is_some());
    }
}
