//! Housekeeping (docs/design/server.md, section 2.4), over a [`Backend`]:
//! every [`HOUSEKEEPING_INTERVAL`], one round ([`round`]) under one
//! [`LeaderLock`] turn, purging what expired on every port of the bundle
//! (the outbox past its retention, the inbox past the retention its store
//! was built with, the webhook deliveries settled more than 30 days ago,
//! the expired idempotency records, and what the backend's [`Janitor`]
//! sweeps).
//!
//! It reaches the database only through the bundle, so a backend an
//! integrator brings is kept as the in-tree ones are, with nothing to
//! wire: the service spawns [`run`] on the backend it serves.
//!
//! [`HOUSEKEEPING_INTERVAL`]: crate::events::HOUSEKEEPING_INTERVAL
//! [`LeaderLock`]: crate::store::LeaderLock
//! [`Janitor`]: crate::store::Janitor

use std::future::Future;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use time::OffsetDateTime;

use crate::backend::Backend;
use crate::events::purge_outbox;
use crate::store::{HOUSEKEEPING, HOUSEKEEPING_LEASE};

/// The shortest interval [`run`] keeps: a zero one is taken as this.
const SHORTEST_INTERVAL: Duration = Duration::from_millis(1);

/// Housekeeping on `backend`, a round every `every` (the first at once)
/// until `stop` completes, purging the outbox past `retention`. A round
/// already running when `stop` completes finishes first. A zero `every` is
/// taken as one millisecond.
pub async fn run(
    backend: Arc<dyn Backend>,
    retention: Duration,
    every: Duration,
    stop: impl Future<Output = ()>,
) {
    let mut ticks = tokio::time::interval(every.max(SHORTEST_INTERVAL));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut stop = std::pin::pin!(stop);
    loop {
        // `stop` first: once it completed, no other round starts.
        let stopped = std::future::poll_fn(|cx| {
            if stop.as_mut().poll(cx).is_ready() {
                return Poll::Ready(true);
            }
            ticks.poll_tick(cx).map(|_| false)
        })
        .await;
        if stopped {
            return;
        }
        round(backend.as_ref(), retention).await;
    }
}

/// One round of housekeeping on `backend`, under one turn of its
/// [`LeaderLock`](crate::store::LeaderLock) for [`HOUSEKEEPING`], leased for
/// [`HOUSEKEEPING_LEASE`]: purge its outbox past `retention`, then its
/// inbox past the retention its `ConversationStore` applies
/// (`ConversationStore::apply_retention`: the store's own setting, built
/// in with the adapter's `with_retention`; the service's
/// `WA_SERVER_INBOX_RETENTION`; nothing under `Retention::Keep`, the
/// default, decision D10), then its
/// expired idempotency records (already ignored: this bounds the table),
/// then what its [`Janitor`](crate::store::Janitor) sweeps (on Postgres,
/// the library's dead key/value rows: webhook dedup markers add one per
/// event; in memory, nothing). One replica at a time runs a round: one
/// that does not get the turn skips it. The purges take no lock of their
/// own and tolerate a round overlapping another (a lease that ended while
/// its round ran). A failure is logged and retried next round; `false`
/// when the round was skipped.
pub async fn round(backend: &dyn Backend, retention: Duration) -> bool {
    let turn = match backend
        .leader_lock()
        .try_exclusive(HOUSEKEEPING, HOUSEKEEPING_LEASE)
        .await
    {
        Ok(Some(turn)) => turn,
        // Another replica's round.
        Ok(None) => return false,
        Err(error) => {
            tracing::warn!(error = %error, "taking the housekeeping turn failed");
            return false;
        }
    };
    if let Err(error) = purge_outbox(backend.outbox().as_ref(), retention).await {
        tracing::warn!(error = %error, "purging the outbox failed");
    }
    match backend
        .conversations()
        .apply_retention(OffsetDateTime::now_utc())
        .await
    {
        Ok(purged) if purged.is_empty() => {}
        Ok(purged) => tracing::debug!(
            messages = purged.messages,
            conversations = purged.conversations,
            window_events = purged.window_events,
            thread_owners = purged.thread_owners,
            "inbox records past their retention purged"
        ),
        Err(error) => tracing::warn!(error = %error, "purging the inbox failed"),
    }
    // Webhook deliveries settled past their retention (roadmap M2c).
    let deliveries_before = OffsetDateTime::now_utc()
        - time::Duration::try_from(crate::webhooks_out::DELIVERY_RETENTION)
            .unwrap_or(time::Duration::MAX);
    match backend.webhooks().purge_deliveries(deliveries_before).await {
        Ok(0) => {}
        Ok(purged) => tracing::debug!(
            purged,
            "settled webhook deliveries past their retention purged"
        ),
        Err(error) => tracing::warn!(error = %error, "purging webhook deliveries failed"),
    }
    match backend.idempotency().purge_idempotency_keys().await {
        Ok(0) => {}
        Ok(purged) => tracing::debug!(purged, "expired idempotency records purged"),
        Err(error) => tracing::warn!(error = %error, "purging idempotency records failed"),
    }
    if let Err(error) = backend.janitor().purge_expired().await {
        tracing::warn!(error = %error, "purging expired key/value rows failed");
    }
    if let Err(error) = turn.release().await {
        tracing::warn!(error = %error, "ending the housekeeping turn failed");
    }
    true
}
