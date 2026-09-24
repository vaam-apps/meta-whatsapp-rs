//! Deliver each of Meta's retries once, without ever swallowing one.
//!
//! Meta redelivers a webhook until it gets a `200`, for up to 7 days, and
//! sends the same notification to every app subscribed to the WABA
//! (`webhooks/overview`, § Webhook delivery failure). [`DedupGuard`] keeps a
//! marker per [`WebhookEvent::dedup_key`] in a [`KvStore`], in two phases:
//!
//! 1. **Claim**: [`KvStore::put_if_absent`] a `pending` marker that expires
//!    after a short *lease* ([`DEFAULT_CLAIM_LEASE`]). The port guarantees
//!    this is atomic across processes, so of two replicas racing on the same
//!    retry exactly one gets the claim.
//! 2. **Complete**: once the sink accepted the event,
//!    [`KvStore::compare_and_swap`] the marker to `done`, which lives for
//!    the dedup TTL ([`DEFAULT_DEDUP_TTL`]). On a sink error the claim is
//!    released instead (again by compare-and-swap, so only our own claim).
//!
//! Why not one marker written at claim time: a claim that outlives an
//! undelivered event swallows it. The handler future can be dropped between
//! claim and sink (the client disconnected, a `tower` timeout fired, the
//! process was redeployed or crashed), and then no code runs to release the
//! claim; every one of Meta's retries for the next 7 days would be
//! acknowledged as a duplicate and the event (an order, a customer's
//! message) would be lost. With a lease the worst case is a delay: the
//! `pending` marker expires and the next retry delivers the event.
//!
//! A retry that finds a live `pending` marker is not acknowledged: another
//! request is delivering that event right now and may still fail, so the
//! handler answers non-`200` ([`ClaimInFlight`]) and Meta tries again later,
//! by which time the marker is `done` (a duplicate, `200`), released, or
//! expired (delivered then).
//!
//! Semantics: at-least-once, deduplicated. An event is delivered twice only
//! if a sink call outlasts the lease while a retry arrives, or if marking it
//! `done` fails after the sink accepted it.
//!
//! Replay: Meta's signature has no timestamp or nonce, so a captured,
//! validly signed body can be replayed by whoever captured it. Within the
//! TTL the markers turn that into duplicates for every keyed event; after
//! the TTL, and for keyless events ([`WebhookEvent::ErrorReported`],
//! [`WebhookEvent::Unparsed`]), a replay is delivered again. Capturing a
//! body needs TLS interception or access to wherever bodies and headers
//! are logged, so do not log the raw request; Meta's mutual-TLS option
//! (`webhooks/overview`, § Mutual TLS) also closes the "anyone who can
//! reach the endpoint" part of the threat.
//!
//! Store keys are the SHA-256 of the dedup key ([`store_key`]): dedup keys
//! can contain a group participant's phone number, which has no business
//! sitting in plain text in a shared cache's keyspace for a week.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use wa_core::store::{Expiry, KvStore, StoreKey};

use crate::event::WebhookEvent;

/// Namespace of the dedup markers in the [`KvStore`].
pub const DEDUP_NAMESPACE: &str = "wa.webhook.dedup";

/// Default lifetime of a `done` marker: Meta's 7-day retry window plus an
/// hour of slack for clock skew and the last retry's own latency.
pub const DEFAULT_DEDUP_TTL: Duration = Duration::from_hours(7 * 24 + 1);

/// Default lifetime of a `pending` claim. Must outlast one sink call (sinks
/// should be fast; see `wa_core::sink`), and bounds how long an event
/// claimed by a request that then died waits before a retry may deliver it.
pub const DEFAULT_CLAIM_LEASE: Duration = Duration::from_secs(60);

const PENDING: &[u8] = b"pending";
const DONE: &[u8] = b"done";

/// The [`KvStore`] key of the marker for `dedup_key` (an event's
/// [`WebhookEvent::dedup_key`]): its SHA-256, hex, in [`DEDUP_NAMESPACE`].
/// For operators who need to inspect or delete one marker by hand.
pub fn store_key(dedup_key: &str) -> StoreKey {
    StoreKey::new(
        DEDUP_NAMESPACE,
        hex::encode(Sha256::digest(dedup_key.as_bytes())),
    )
}

/// Remembers which events were already delivered. Cheap to clone.
#[derive(Debug, Clone)]
pub struct DedupGuard {
    kv: Arc<dyn KvStore>,
    ttl: Duration,
    lease: Duration,
}

/// Outcome of [`DedupGuard::claim`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Claim {
    /// Not seen before, or an earlier claim's lease expired: deliver the
    /// event, then [`DedupGuard::complete`] (or [`DedupGuard::release`] if
    /// delivering failed).
    Acquired(ClaimTicket),
    /// The event has no dedup key; deliver it, there is nothing to complete.
    Untracked,
    /// Delivered before, within the TTL: skip it and acknowledge.
    Duplicate,
    /// Another request holds a live claim on it: do not acknowledge, so
    /// Meta retries later.
    InFlight,
}

/// Proof of an acquired claim: which marker, at which version. Completing
/// or releasing is a compare-and-swap on that version, so a request whose
/// lease expired can never overwrite or delete somebody else's claim.
#[derive(Clone, PartialEq, Eq)]
pub struct ClaimTicket {
    key: StoreKey,
    version: u64,
}

impl fmt::Debug for ClaimTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimTicket")
            .field("key", &self.key.key())
            .field("version", &self.version)
            .finish()
    }
}

/// Returned (inside [`wa_core::Error::Other`]) by
/// [`crate::WebhookHandler::deliver`] when an event of the delivery is being
/// delivered by another request right now. Answer non-`200` (the axum
/// router answers `503`) so Meta retries; see the [module docs](self).
///
/// Carries no event data: dedup keys can contain phone numbers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "a webhook event in this delivery is being delivered by another request; answer non-200 so Meta retries"
)]
pub struct ClaimInFlight;

impl DedupGuard {
    /// Guard on `kv`, with [`DEFAULT_DEDUP_TTL`] and [`DEFAULT_CLAIM_LEASE`].
    pub fn new(kv: Arc<dyn KvStore>) -> Self {
        Self {
            kv,
            ttl: DEFAULT_DEDUP_TTL,
            lease: DEFAULT_CLAIM_LEASE,
        }
    }

    /// Change how long a delivered event is remembered. Shorter than 7 days
    /// lets late retries through as new deliveries.
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Change how long a claim stays `pending` without being completed.
    /// Longer than your slowest sink call, shorter than you are willing to
    /// wait for an event whose request died mid-delivery.
    #[must_use]
    pub fn with_lease(mut self, lease: Duration) -> Self {
        self.lease = lease;
        self
    }

    /// How long a delivered event is remembered.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// How long an uncompleted claim lives.
    pub fn lease(&self) -> Duration {
        self.lease
    }

    /// Claim `event` for delivery. See [`Claim`] for what to do next.
    pub async fn claim(&self, event: &WebhookEvent) -> wa_core::Result<Claim> {
        match event.dedup_key() {
            Some(key) => self.claim_key(&key).await,
            None => Ok(Claim::Untracked),
        }
    }

    /// Mark a claimed event delivered: a later claim of it is a
    /// [`Claim::Duplicate`] for the TTL. Returns `false` when the lease had
    /// expired and the marker could not be taken back (the event may then
    /// be delivered again by a concurrent request).
    pub async fn complete(&self, ticket: &ClaimTicket) -> wa_core::Result<bool> {
        let swapped = self
            .kv
            .compare_and_swap(
                &ticket.key,
                ticket.version,
                Some(DONE.to_vec()),
                Expiry::After(self.ttl),
            )
            .await?;
        if swapped.is_some() {
            return Ok(true);
        }
        // Our lease expired. If nobody re-claimed the event, still record
        // it as done so later retries are duplicates; if somebody did, it
        // is theirs now.
        let recorded = self
            .kv
            .put_if_absent(&ticket.key, DONE.to_vec(), Expiry::After(self.ttl))
            .await?;
        Ok(recorded.is_some())
    }

    /// Give a claim back after delivering failed, so Meta's redelivery is
    /// not treated as a duplicate. Only removes the marker if it is still
    /// this claim. Returns whether it removed one.
    pub async fn release(&self, ticket: &ClaimTicket) -> wa_core::Result<bool> {
        let removed = self
            .kv
            .compare_and_swap(&ticket.key, ticket.version, None, Expiry::Keep)
            .await?;
        Ok(removed.is_some())
    }

    pub(crate) async fn claim_key(&self, dedup_key: &str) -> wa_core::Result<Claim> {
        let key = store_key(dedup_key);
        // Two rounds: the marker can expire between our failed
        // `put_if_absent` and the `get` that asks what it was.
        for _ in 0..2 {
            if let Some(version) = self
                .kv
                .put_if_absent(&key, PENDING.to_vec(), Expiry::After(self.lease))
                .await?
            {
                return Ok(Claim::Acquired(ClaimTicket { key, version }));
            }
            match self.kv.get(&key).await? {
                Some(marker) if marker.value == PENDING => return Ok(Claim::InFlight),
                // `done`, or anything else a future version might write.
                Some(_) => return Ok(Claim::Duplicate),
                None => {}
            }
        }
        // Still flapping: somebody is claiming and releasing it right now.
        Ok(Claim::InFlight)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_keys_hide_the_dedup_key() {
        let k = store_key("wamid.X:read:16505551234");
        assert_eq!(k.namespace(), DEDUP_NAMESPACE);
        assert_eq!(k.key().len(), 64);
        assert!(!k.key().contains("16505551234"));
        assert_ne!(k, store_key("wamid.X:read:16505551235"));
    }
}
