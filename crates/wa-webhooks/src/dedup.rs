//! At-most-once delivery of Meta's retries.
//!
//! Meta redelivers a webhook until it gets a `200`, for up to 7 days, and
//! sends the same notification to every app subscribed to the WABA
//! (`webhooks/overview`, § Webhook delivery failure). [`DedupGuard`] records
//! each event's [`WebhookEvent::dedup_key`] with
//! [`KvStore::put_if_absent`], which the port guarantees atomic across
//! processes for shared backends, so two replicas racing on the same retry
//! deliver it once.

use std::sync::Arc;
use std::time::Duration;

use wa_core::store::{Expiry, KvStore, StoreKey};

use crate::event::WebhookEvent;

/// Namespace of the dedup markers in the [`KvStore`].
pub const DEDUP_NAMESPACE: &str = "wa.webhook.dedup";

/// Default marker lifetime: Meta's 7-day retry window plus an hour of
/// slack for clock skew and the last retry's own latency.
pub const DEFAULT_DEDUP_TTL: Duration = Duration::from_hours(7 * 24 + 1);

/// Remembers which events were already delivered. Cheap to clone.
#[derive(Debug, Clone)]
pub struct DedupGuard {
    kv: Arc<dyn KvStore>,
    ttl: Duration,
}

impl DedupGuard {
    /// Guard on `kv`, with [`DEFAULT_DEDUP_TTL`].
    pub fn new(kv: Arc<dyn KvStore>) -> Self {
        Self {
            kv,
            ttl: DEFAULT_DEDUP_TTL,
        }
    }

    /// Change the marker lifetime. Shorter than 7 days lets late retries
    /// through as duplicates.
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// The marker lifetime.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Claim `event`: `true` if it was not seen before (deliver it), `false`
    /// if it is a duplicate. Events without a key are always `true`.
    pub async fn claim(&self, event: &WebhookEvent) -> wa_core::Result<bool> {
        match event.dedup_key() {
            Some(key) => self.claim_key(&key).await,
            None => Ok(true),
        }
    }

    /// Undo a claim, so a redelivery of `event` is not treated as a
    /// duplicate. Call it when delivering a claimed event failed. Returns
    /// whether a marker was removed.
    pub async fn release(&self, event: &WebhookEvent) -> wa_core::Result<bool> {
        match event.dedup_key() {
            Some(key) => self.release_key(&key).await,
            None => Ok(false),
        }
    }

    pub(crate) async fn claim_key(&self, key: &str) -> wa_core::Result<bool> {
        let created = self
            .kv
            .put_if_absent(&store_key(key), b"1".to_vec(), Expiry::After(self.ttl))
            .await?;
        Ok(created.is_some())
    }

    pub(crate) async fn release_key(&self, key: &str) -> wa_core::Result<bool> {
        Ok(self.kv.delete(&store_key(key)).await?)
    }
}

fn store_key(key: &str) -> StoreKey {
    StoreKey::new(DEDUP_NAMESPACE, key)
}
