//! [`PgNotifier`]: the core's `EventNotifier` on Postgres (roadmap M2b;
//! docs/design/server.md, section 4.5): `NOTIFY` on the deployment's
//! channel ([`EVENTS_CHANNEL`]) and one `LISTEN` connection per replica
//! feeding the replica's hub, so an event recorded through any replica
//! wakes the live streams of every one.
//!
//! **A wake-up only.** A notification's payload is `<sequence> <tenant>`
//! (the tenant's sequence of the event, a space, the tenant's id: well
//! under Postgres's 8,000 bytes), nothing of the event; the streams read
//! the outbox. Postgres delivers a notification only to the sessions
//! listening when it commits, so one sent while a replica's listener is
//! reconnecting is lost to it: the listener wakes every stream of its
//! replica once it is listening again, and the streams' re-poll covers
//! the rest.
//!
//! **Its own connection.** The `LISTEN` session is a connection of its own,
//! outside the pool the API and the webhook pipeline share (a pool of one,
//! from the same connect options), opened with the first subscription and
//! closed with the backend ([`PgNotifier::close`]). A notification is sent
//! on the shared pool, after the outbox insert committed.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::postgres::{PgListener, PgPoolOptions};
use meta_whatsapp_rs::adapters::store::postgres::sqlx::{self, PgPool};
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_server_core::model::TenantId;
use meta_whatsapp_server_core::notify::{EventNotifier, Hub, Subscription, Wakeup};
use meta_whatsapp_server_core::store::StoreResult;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;

/// The target of this module's log lines.
const LOG_TARGET: &str = "meta_whatsapp_server::store::notify_postgres";

/// The channel every replica of a deployment notifies and listens on: one
/// per database. Replicas of two releases must use the same one during a
/// rolling deploy, so it is a stable identifier (docs/architecture.md,
/// § Stable identifiers; pending the owner's confirmation), as is the
/// payload's format, `<sequence> <tenant>`.
pub const EVENTS_CHANNEL: &str = "wa_server_events";

/// How long a subscription waits for this replica's `LISTEN` connection
/// before it is refused (`503 storage_unavailable`).
pub const LISTEN_WAIT: Duration = Duration::from_secs(5);

/// How long the listener waits before listening again after its
/// connection failed.
const RETRY: Duration = Duration::from_secs(1);

/// The event notifier on Postgres. Cheap to clone: clones share the hub
/// and the listening connection.
#[derive(Clone)]
pub struct PgNotifier {
    shared: Arc<Shared>,
}

struct Shared {
    pool: PgPool,
    channel: String,
    hub: Hub,
    listener: Mutex<Option<Listening>>,
}

/// The listening task, and whether it is listening now.
struct Listening {
    now: watch::Receiver<bool>,
    task: JoinHandle<()>,
    pool: PgPool,
}

impl std::fmt::Debug for PgNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgNotifier")
            .field("channel", &self.shared.channel)
            .field("hub", &self.shared.hub)
            .finish_non_exhaustive()
    }
}

impl PgNotifier {
    /// Notifications on `pool`'s database, on [`EVENTS_CHANNEL`].
    pub fn new(pool: PgPool) -> Self {
        Self::with_channel(pool, EVENTS_CHANNEL)
    }

    /// Notifications on `pool`'s database, on `channel` (a deployment
    /// sharing its database with another, or a test).
    pub fn with_channel(pool: PgPool, channel: &str) -> Self {
        Self {
            shared: Arc::new(Shared {
                pool,
                channel: channel.to_owned(),
                hub: Hub::new(),
                listener: Mutex::new(None),
            }),
        }
    }

    /// The channel.
    pub fn channel(&self) -> &str {
        &self.shared.channel
    }

    /// This replica's hub: the subscriptions its listener wakes.
    pub fn hub(&self) -> &Hub {
        &self.shared.hub
    }

    /// Stop listening and close the listening connection (the backend's
    /// `close`). A later subscription listens again.
    pub async fn close(&self) {
        let listening = self.shared.listener.lock().await.take();
        if let Some(listening) = listening {
            listening.task.abort();
            let _ = listening.task.await;
            listening.pool.close().await;
        }
    }

    /// Whether this replica is listening, the listener started if it is
    /// not running, waiting at most [`LISTEN_WAIT`].
    async fn listening(&self) -> StoreResult<()> {
        let mut listening = {
            let mut listener = self.shared.listener.lock().await;
            let running = listener
                .as_ref()
                .is_some_and(|listening| !listening.task.is_finished());
            if !running {
                *listener = Some(self.listen());
            }
            listener
                .as_ref()
                .map(|listening| listening.now.clone())
                .ok_or_else(|| StorageError::Backend(anyhow::anyhow!("no listener")))?
        };
        match tokio::time::timeout(LISTEN_WAIT, listening.wait_for(|now| *now)).await {
            Ok(Ok(_)) => Ok(()),
            _ => Err(StorageError::Backend(anyhow::anyhow!(
                "this replica is not listening for event notifications"
            ))),
        }
    }

    /// Start the listening task on a connection of its own.
    fn listen(&self) -> Listening {
        let options = (*self.shared.pool.connect_options()).clone();
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_lazy_with(options);
        let (tell, listening) = watch::channel(false);
        let task = tokio::spawn(listen(
            pool.clone(),
            self.shared.channel.clone(),
            self.shared.hub.clone(),
            tell,
        ));
        Listening {
            now: listening,
            task,
            pool,
        }
    }
}

/// The listener: every notification on `channel` to `hub`, and every
/// subscription woken after its connection came back.
async fn listen(pool: PgPool, channel: String, hub: Hub, tell: watch::Sender<bool>) {
    loop {
        let mut listener = match PgListener::connect_with(&pool).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    error = %error,
                    "could not connect to listen for event notifications"
                );
                tokio::time::sleep(RETRY).await;
                continue;
            }
        };
        if let Err(error) = listener.listen(&channel).await {
            tracing::warn!(
                target: LOG_TARGET,
                error = %error,
                "could not listen for event notifications"
            );
            tokio::time::sleep(RETRY).await;
            continue;
        }
        // Anything sent while it was not listening is lost to it: every
        // subscription reads once, before a subscriber hears it listens.
        hub.wake_all();
        tell.send_replace(true);
        loop {
            match listener.try_recv().await {
                Ok(Some(notification)) => {
                    if let Some(wakeup) = wakeup(notification.payload()) {
                        hub.wake(&wakeup);
                    } else {
                        tracing::debug!(
                            target: LOG_TARGET,
                            "an event notification of an unknown shape, ignored"
                        );
                    }
                }
                // The connection was lost. `PgListener` would listen again
                // on its next call, unseen: it is listened again here, so
                // that every stream reads once the new session listens
                // (above), not before, when what is committed meanwhile
                // would wait for the re-poll.
                Ok(None) => {
                    tell.send_replace(false);
                    tracing::warn!(
                        target: LOG_TARGET,
                        "the connection listening for event notifications was lost: listening again"
                    );
                    tokio::time::sleep(RETRY).await;
                    break;
                }
                Err(error) => {
                    tell.send_replace(false);
                    tracing::warn!(
                        target: LOG_TARGET,
                        error = %error,
                        "listening for event notifications failed: listening again"
                    );
                    tokio::time::sleep(RETRY).await;
                    break;
                }
            }
        }
    }
}

/// The payload of `wakeup`: `<sequence> <tenant>`.
pub fn payload(wakeup: &Wakeup) -> String {
    format!("{} {}", wakeup.sequence, wakeup.tenant.as_str())
}

/// The wake-up a payload says, if it is one ([`payload`]).
pub fn wakeup(payload: &str) -> Option<Wakeup> {
    let (sequence, tenant) = payload.split_once(' ')?;
    Some(Wakeup {
        sequence: sequence.parse().ok()?,
        tenant: TenantId::parse(tenant)?,
    })
}

#[async_trait]
impl EventNotifier for PgNotifier {
    async fn notify(&self, wakeup: &Wakeup) -> StoreResult<()> {
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(&self.shared.channel)
            .bind(payload(wakeup))
            .execute(&self.shared.pool)
            .await
            .map_err(|error| StorageError::Backend(anyhow::Error::new(error)))?;
        Ok(())
    }

    async fn subscribe(&self, tenant: &TenantId) -> StoreResult<Subscription> {
        // Subscribed first: a notification that arrives once the listener
        // is listening wakes it.
        let subscription = self.shared.hub.subscribe(tenant);
        self.listening().await?;
        Ok(subscription)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a panic is the report
mod tests {
    use super::*;

    /// The payload is `<sequence> <tenant>`, read back as it was written
    /// (a tenant id may hold `:` and `.`); anything else is no wake-up.
    /// Replicas of two releases read each other's: pinned.
    #[test]
    fn the_payload_is_the_sequence_and_the_tenant() {
        let wakeup = Wakeup {
            tenant: TenantId::parse("merchant-42:eu.1").unwrap(),
            sequence: 18342,
        };
        assert_eq!(payload(&wakeup), "18342 merchant-42:eu.1");
        assert_eq!(super::wakeup("18342 merchant-42:eu.1"), Some(wakeup));
        for bad in ["", "18342", "x merchant", "1 ", "1 two words"] {
            assert_eq!(super::wakeup(bad), None, "{bad:?}");
        }
        assert_eq!(EVENTS_CHANNEL, "wa_server_events");
    }
}
