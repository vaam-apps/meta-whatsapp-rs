//! `SignupSessions`: bind an Embedded Signup attempt to the tenant
//! (merchant) that started it.
//!
//! Embedded Signup itself carries no state parameter back to your server:
//! the page receives the code and the session info from Meta and forwards
//! them. Without a binding, any authenticated merchant could post a code
//! and have the result attributed to themselves or, worse, to someone
//! else. So: when a merchant starts the flow, [`SignupSessions::start`]
//! issues an opaque, random, short-lived [`SignupState`]; the page sends it
//! back with the code; [`SignupSessions::consume`] returns the tenant it
//! was issued to, exactly once. Compare that tenant with the one your own
//! authentication says is calling.
//!
//! Stored on any [`KvStore`] under namespace `wa.es.session`; the expiry is
//! the store's (`Expiry::After(ttl)`), and single use is a compare-and-swap
//! delete, so two concurrent callbacks with the same state cannot both win.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use wa_core::error::ValidationError;
use wa_core::store::{Expiry, JsonStore, KvStore};
use wa_core::{Error, Result};

/// `KvStore` namespace of signup sessions.
pub const SESSION_NAMESPACE: &str = "wa.es.session";

/// Random bytes per state: 128 bits.
const STATE_BYTES: usize = 16;
/// Upper bound accepted by [`SignupState::parse`].
const MAX_STATE_LEN: usize = 64;
/// Maximum tenant id length stored with a session.
const MAX_TENANT_LEN: usize = 256;

/// An opaque signup state: 128 random bits, base64url without padding
/// (22 characters).
///
/// It is a single-use capability for the attempt, so `Debug` does not
/// print it; read it with [`Self::as_str`] to hand it to the page.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SignupState(String);

impl SignupState {
    /// Validate a state received from the page: 1–64 base64url characters.
    /// Anything else is rejected before touching the store.
    pub fn parse(value: &str) -> Result<Self> {
        let ok = !value.is_empty()
            && value.len() <= MAX_STATE_LEN
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if ok {
            Ok(Self(value.to_owned()))
        } else {
            Err(ValidationError::new("state", "not a signup state").into())
        }
    }

    /// The state string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn generate() -> Result<Self> {
        let mut bytes = [0u8; STATE_BYTES];
        getrandom::fill(&mut bytes).map_err(|e| {
            Error::Other(anyhow::anyhow!(
                "the operating system's random number generator failed: {e}"
            ))
        })?;
        Ok(Self(URL_SAFE_NO_PAD.encode(bytes)))
    }
}

impl fmt::Debug for SignupState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SignupState([REDACTED])")
    }
}

#[derive(Serialize, Deserialize)]
struct SessionRecord {
    tenant: String,
}

/// Issues and redeems [`SignupState`]s. Cheap to clone.
#[derive(Debug, Clone)]
pub struct SignupSessions {
    store: JsonStore<SessionRecord>,
}

impl SignupSessions {
    /// Sessions on `kv`.
    pub fn new(kv: Arc<dyn KvStore>) -> Self {
        Self {
            store: JsonStore::new(kv, SESSION_NAMESPACE),
        }
    }

    /// Start an attempt for `tenant` (your merchant id), valid for `ttl`.
    ///
    /// Pick a `ttl` that covers a customer going through the whole flow
    /// (several screens, possibly an SMS code): minutes, not seconds.
    pub async fn start(&self, tenant: &str, ttl: Duration) -> Result<SignupState> {
        if tenant.is_empty() || tenant.len() > MAX_TENANT_LEN {
            return Err(ValidationError::new("tenant", "must be 1-256 bytes").into());
        }
        if ttl.is_zero() {
            return Err(ValidationError::new("ttl", "must be positive").into());
        }
        let record = SessionRecord {
            tenant: tenant.to_owned(),
        };
        // A collision among 2^128 values does not happen; the loop only
        // guarantees we never overwrite a live session if it did.
        for _ in 0..3 {
            let state = SignupState::generate()?;
            if self
                .store
                .put_if_absent(state.as_str(), &record, Expiry::After(ttl))
                .await?
                .is_some()
            {
                return Ok(state);
            }
        }
        Err(ValidationError::new("state", "could not allocate a unique signup state").into())
    }

    /// Redeem `state`: the tenant it was issued to, or `None` if it is
    /// unknown, expired, or already redeemed. Succeeds at most once per
    /// state, even under concurrency.
    pub async fn consume(&self, state: &SignupState) -> Result<Option<String>> {
        let Some((record, version, _)) = self.store.get(state.as_str()).await? else {
            return Ok(None);
        };
        let won = self
            .store
            .compare_and_swap(state.as_str(), version, None, Expiry::Keep)
            .await?
            .is_some();
        Ok(won.then_some(record.tenant))
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;
    use wa_adapters::store::MemoryKvStore;
    use wa_core::clock::ManualClock;
    use wa_core::store::StoreKey;

    use super::*;

    #[tokio::test]
    async fn start_then_consume_exactly_once() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let sessions = SignupSessions::new(Arc::clone(&kv));
        let state = sessions
            .start("merchant-42", Duration::from_secs(600))
            .await
            .unwrap();
        assert_eq!(state.as_str().len(), 22);
        assert_eq!(SignupState::parse(state.as_str()).unwrap(), state);
        assert_eq!(format!("{state:?}"), "SignupState([REDACTED])");
        assert!(
            kv.get(&StoreKey::new(SESSION_NAMESPACE, state.as_str()))
                .await
                .unwrap()
                .is_some()
        );

        assert_eq!(
            sessions.consume(&state).await.unwrap().as_deref(),
            Some("merchant-42")
        );
        assert_eq!(sessions.consume(&state).await.unwrap(), None, "single use");
    }

    #[tokio::test]
    async fn states_are_distinct_and_bound_to_their_tenant() {
        let sessions = SignupSessions::new(Arc::new(MemoryKvStore::new()));
        let a = sessions.start("A", Duration::from_secs(60)).await.unwrap();
        let b = sessions.start("B", Duration::from_secs(60)).await.unwrap();
        assert_ne!(a, b);
        assert_eq!(sessions.consume(&b).await.unwrap().as_deref(), Some("B"));
        assert_eq!(sessions.consume(&a).await.unwrap().as_deref(), Some("A"));
    }

    #[tokio::test]
    async fn expired_and_unknown_states_yield_nothing() {
        let clock = ManualClock::new(datetime!(2026-09-24 12:00 UTC));
        let sessions =
            SignupSessions::new(Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone()))));
        let state = sessions.start("m", Duration::from_secs(60)).await.unwrap();
        clock.advance(Duration::from_secs(61));
        assert_eq!(sessions.consume(&state).await.unwrap(), None);
        let unknown = SignupState::parse("AAAAAAAAAAAAAAAAAAAAAA").unwrap();
        assert_eq!(sessions.consume(&unknown).await.unwrap(), None);
    }

    /// A store where another redeemer always slips in between the read and
    /// the compare-and-swap.
    #[derive(Debug)]
    struct Racy(MemoryKvStore);

    #[async_trait::async_trait]
    impl KvStore for Racy {
        async fn get(
            &self,
            key: &StoreKey,
        ) -> Result<Option<wa_core::store::Versioned>, wa_core::error::StorageError> {
            self.0.get(key).await
        }
        async fn put(
            &self,
            key: &StoreKey,
            value: Vec<u8>,
            expiry: Expiry,
        ) -> Result<u64, wa_core::error::StorageError> {
            self.0.put(key, value, expiry).await
        }
        async fn put_if_absent(
            &self,
            key: &StoreKey,
            value: Vec<u8>,
            expiry: Expiry,
        ) -> Result<Option<u64>, wa_core::error::StorageError> {
            self.0.put_if_absent(key, value, expiry).await
        }
        async fn compare_and_swap(
            &self,
            key: &StoreKey,
            expected: u64,
            new: Option<Vec<u8>>,
            expiry: Expiry,
        ) -> Result<Option<u64>, wa_core::error::StorageError> {
            // The other redeemer wins first.
            self.0.delete(key).await?;
            self.0.compare_and_swap(key, expected, new, expiry).await
        }
        async fn delete(&self, key: &StoreKey) -> Result<bool, wa_core::error::StorageError> {
            self.0.delete(key).await
        }
    }

    #[tokio::test]
    async fn consume_reports_a_lost_compare_and_swap_as_nothing() {
        let sessions = SignupSessions::new(Arc::new(Racy(MemoryKvStore::new())));
        let state = sessions.start("m", Duration::from_secs(60)).await.unwrap();
        assert_eq!(sessions.consume(&state).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_lost_race_is_not_a_win() {
        // Simulate a concurrent redeemer: read the version, let the other
        // side consume, then try the compare-and-swap with the stale version.
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let sessions = SignupSessions::new(Arc::clone(&kv));
        let state = sessions.start("m", Duration::from_secs(60)).await.unwrap();
        let key = StoreKey::new(SESSION_NAMESPACE, state.as_str());
        let seen_version = kv.get(&key).await.unwrap().unwrap().version;
        assert!(sessions.consume(&state).await.unwrap().is_some());
        assert!(
            kv.compare_and_swap(&key, seen_version, None, Expiry::Keep)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn input_validation() {
        let sessions = SignupSessions::new(Arc::new(MemoryKvStore::new()));
        assert!(sessions.start("", Duration::from_secs(1)).await.is_err());
        assert!(sessions.start("m", Duration::ZERO).await.is_err());
        assert!(
            sessions
                .start(&"m".repeat(257), Duration::from_secs(1))
                .await
                .is_err()
        );
        for bad in ["", "a/b", "a b", "+==", &"a".repeat(65)] {
            assert!(SignupState::parse(bad).is_err(), "{bad:?}");
        }
    }
}
