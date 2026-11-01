//! The event notifier port (docs/design/server.md, sections 4.5 and 8.1;
//! roadmap M2b): what wakes a replica's live event streams when a
//! tenant's stream gains an event, on this replica or another.
//!
//! **A wake-up, never an event.** A notification carries a tenant and a
//! sequence of its stream ([`Wakeup`]), nothing of the event: the outbox
//! stays the only source of what a stream sends ([`crate::live`]), which
//! reads it after its cursor when woken, and again every so often
//! whatever it heard (`LiveSettings::repoll`), so a notification lost
//! (a replica restarting, a `LISTEN` connection dropped, a notify that
//! failed) delays an event, never loses it. A spurious one costs one
//! read.
//!
//! **Per tenant, from the first subscriber.** A replica's wake-ups go
//! through one [`Hub`]: a channel per tenant, made by its first
//! [`Subscription`] and dropped with its last, so a stream is woken by its
//! own tenant's events only, and a tenant nobody streams costs nothing.
//! A wake-up of a tenant nobody here streams is dropped.
//!
//! | Backend | [`EventNotifier`] |
//! | --- | --- |
//! | memory | [`LocalNotifier`]: the process's [`Hub`], a broadcast in memory |
//! | Postgres | `NOTIFY` on a channel of the deployment's database, one `LISTEN` connection per replica feeding its [`Hub`] (`meta-whatsapp-server-store-postgres`) |
//!
//! The webhook pipeline notifies once its outbox insert committed, for a
//! row it routed to a tenant ([`crate::inbound`]); a failed notify is
//! logged, and the delivery still succeeds (the re-poll covers it).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use tokio::sync::watch;

use crate::model::TenantId;
use crate::store::StoreResult;

/// What a notification says: `tenant`'s stream holds an event at
/// `sequence`. A hint: the stream it wakes reads the outbox after its own
/// cursor, whatever the sequence says (a wake-up of a row the outbox's
/// check kept operator-only names a sequence of the operator's stream,
/// and wakes its tenant's streams for nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wakeup {
    /// The tenant whose stream gained an event.
    pub tenant: TenantId,
    /// The event's sequence in that stream.
    pub sequence: i64,
}

/// Wakes the live streams of every replica on a new event of a tenant's
/// (docs/design/server.md, section 8.1's ports table). Object-safe: the
/// service holds an `Arc<dyn EventNotifier>`, its backend's
/// ([`crate::backend::Backend::notifier`]).
///
/// Every implementation keeps three rules, which the conformance suite
/// checks (`conformance::notifier`, feature `conformance`):
///
/// - **every replica hears it**: a [`Self::notify`] wakes, within a
///   second or two, the subscriptions to that tenant on every replica of
///   the deployment, this one included;
/// - **only that tenant's**: a subscription to another tenant is not
///   woken by it;
/// - **best effort**: a notification may be lost (a connection drops, a
///   replica restarts) or arrive twice, and a stream must not rely on it
///   (it re-polls the outbox, [`crate::live`]). After a loss it knows of
///   (a listening connection re-established), a backend wakes every
///   subscription ([`Hub::wake_all`]).
#[async_trait]
pub trait EventNotifier: Send + Sync + 'static {
    /// Tell every replica that `wakeup.tenant`'s stream gained an event.
    ///
    /// # Errors
    ///
    /// The library's `StorageError`: the notification was not sent. The
    /// webhook pipeline logs it and goes on: the event is stored, and the
    /// streams' re-poll finds it.
    async fn notify(&self, wakeup: &Wakeup) -> StoreResult<()>;

    /// Wake-ups for `tenant`'s stream, from now on, on this replica.
    /// Ready when it returns: a notification sent after it (by any
    /// replica) wakes it.
    ///
    /// # Errors
    ///
    /// The library's `StorageError`: this replica cannot listen (Postgres:
    /// no `LISTEN` connection within its wait).
    async fn subscribe(&self, tenant: &TenantId) -> StoreResult<Subscription>;
}

/// What a wake-up sets in a tenant's channel: how many came, and the
/// highest sequence they named.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Signal {
    count: u64,
    sequence: i64,
}

/// One replica's wake-ups, a channel per tenant someone here streams:
/// what an [`EventNotifier`] feeds (directly, [`LocalNotifier`]; from its
/// database's notifications, the Postgres one), and what a
/// [`Subscription`] waits on. Cheap to clone: clones share the channels.
#[derive(Clone, Default)]
pub struct Hub {
    channels: Arc<Mutex<HashMap<TenantId, watch::Sender<Signal>>>>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("tenants", &self.tenants())
            .finish_non_exhaustive()
    }
}

impl Hub {
    /// No channel yet.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<TenantId, watch::Sender<Signal>>> {
        self.channels.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A subscription to `tenant`'s wake-ups, its channel made if it is
    /// the first.
    pub fn subscribe(&self, tenant: &TenantId) -> Subscription {
        let mut channels = self.lock();
        let receiver = if let Some(sender) = channels.get(tenant) {
            sender.subscribe()
        } else {
            let (sender, receiver) = watch::channel(Signal::default());
            channels.insert(tenant.clone(), sender);
            receiver
        };
        Subscription {
            hub: self.clone(),
            tenant: tenant.clone(),
            receiver: Some(receiver),
        }
    }

    /// Wake `wakeup.tenant`'s subscriptions; whether it has any here.
    pub fn wake(&self, wakeup: &Wakeup) -> bool {
        let channels = self.lock();
        let Some(sender) = channels.get(&wakeup.tenant) else {
            return false;
        };
        sender.send_modify(|signal| {
            signal.count = signal.count.wrapping_add(1);
            signal.sequence = signal.sequence.max(wakeup.sequence);
        });
        true
    }

    /// Wake every subscription: after notifications may have been lost
    /// (a backend's listening connection re-established).
    pub fn wake_all(&self) {
        for sender in self.lock().values() {
            sender.send_modify(|signal| signal.count = signal.count.wrapping_add(1));
        }
    }

    /// How many tenants have a channel here (a subscription at least).
    pub fn tenants(&self) -> usize {
        self.lock().len()
    }
}

/// Wake-ups for one tenant's stream on one replica ([`Hub::subscribe`],
/// [`EventNotifier::subscribe`]). Dropping the last of a tenant's drops its
/// channel.
pub struct Subscription {
    hub: Hub,
    tenant: TenantId,
    /// `None` only while dropping.
    receiver: Option<watch::Receiver<Signal>>,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("tenant", &self.tenant)
            .finish_non_exhaustive()
    }
}

impl Subscription {
    /// The tenant it hears.
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// Wait for a wake-up this subscription has not seen yet (one that
    /// came since the last call, or since it was made, returns at once);
    /// the highest sequence the wake-ups named. Several wake-ups between
    /// two calls are one. Cancel-safe: in a `select!`, a wake-up that
    /// lost the race is still there for the next call.
    pub async fn woken(&mut self) -> i64 {
        let Some(receiver) = self.receiver.as_mut() else {
            return std::future::pending().await;
        };
        // The hub keeps the sender while this receiver lives.
        if receiver.changed().await.is_err() {
            return std::future::pending().await;
        }
        receiver.borrow_and_update().sequence
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        drop(self.receiver.take());
        let mut channels = self.hub.lock();
        if channels
            .get(&self.tenant)
            .is_some_and(|sender| sender.receiver_count() == 0)
        {
            channels.remove(&self.tenant);
        }
    }
}

/// The notifier of one process: a broadcast in memory, through its
/// [`Hub`] (the memory backend's; any backend whose replicas are one
/// process). Cheap to clone: clones share the hub.
#[derive(Debug, Clone, Default)]
pub struct LocalNotifier {
    hub: Hub,
}

impl LocalNotifier {
    /// No subscription yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Its hub.
    pub fn hub(&self) -> &Hub {
        &self.hub
    }
}

#[async_trait]
impl EventNotifier for LocalNotifier {
    async fn notify(&self, wakeup: &Wakeup) -> StoreResult<()> {
        self.hub.wake(wakeup);
        Ok(())
    }

    async fn subscribe(&self, tenant: &TenantId) -> StoreResult<Subscription> {
        Ok(self.hub.subscribe(tenant))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a panic is the report
mod tests {
    use std::time::Duration;

    use super::*;

    fn tenant(id: &str) -> TenantId {
        TenantId::parse(id).unwrap()
    }

    async fn woken_within(subscription: &mut Subscription, wait: Duration) -> Option<i64> {
        tokio::time::timeout(wait, subscription.woken()).await.ok()
    }

    /// A wake-up reaches its tenant's subscriptions, every one, and no
    /// other tenant's; several between two waits are one, with the highest
    /// sequence. Decisive: the channel per tenant.
    #[tokio::test]
    async fn a_wakeup_reaches_its_tenants_subscriptions_only() {
        let hub = Hub::new();
        let mut a1 = hub.subscribe(&tenant("a"));
        let mut a2 = hub.subscribe(&tenant("a"));
        let mut b = hub.subscribe(&tenant("b"));
        assert_eq!(hub.tenants(), 2);
        for sequence in [3, 7, 5] {
            assert!(hub.wake(&Wakeup {
                tenant: tenant("a"),
                sequence,
            }));
        }
        let short = Duration::from_millis(50);
        assert_eq!(woken_within(&mut a1, short).await, Some(7));
        assert_eq!(woken_within(&mut a2, short).await, Some(7));
        assert_eq!(woken_within(&mut a1, short).await, None, "seen once");
        assert_eq!(woken_within(&mut b, short).await, None, "another tenant's");
        assert!(!hub.wake(&Wakeup {
            tenant: tenant("c"),
            sequence: 1,
        }));
        hub.wake_all();
        assert_eq!(woken_within(&mut b, short).await, Some(0));
        assert_eq!(woken_within(&mut a1, short).await, Some(7));
    }

    /// A tenant's channel goes with its last subscription: a hub holds no
    /// channel for a tenant nobody streams.
    #[tokio::test]
    async fn a_tenants_channel_goes_with_its_last_subscription() {
        let notifier = LocalNotifier::new();
        let first = notifier.subscribe(&tenant("a")).await.unwrap();
        let second = notifier.subscribe(&tenant("a")).await.unwrap();
        assert_eq!(notifier.hub().tenants(), 1);
        drop(first);
        assert_eq!(notifier.hub().tenants(), 1);
        drop(second);
        assert_eq!(notifier.hub().tenants(), 0);
        notifier
            .notify(&Wakeup {
                tenant: tenant("a"),
                sequence: 1,
            })
            .await
            .unwrap();
        // A new subscription does not see a wake-up from before it.
        let mut again = notifier.subscribe(&tenant("a")).await.unwrap();
        assert_eq!(
            woken_within(&mut again, Duration::from_millis(50)).await,
            None
        );
    }
}
