//! `meta-whatsapp-server serve` and the pieces the other commands share:
//! storage, migrations, the vault and the Graph client.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::PgPool;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::postgres::{
    PgConnectOptions, PgPoolOptions,
};
use meta_whatsapp_rs::adapters::store::{
    MemoryConversationStore, MemoryKvStore, PostgresConversationStore, PostgresKvStore,
};
use meta_whatsapp_rs::client::embedded_signup::{TokenVault, VaultKeys};
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::api::admin::mint;
use crate::config::{Config, DatabaseUrl, MigrateMode, Storage};
use crate::events::{HOUSEKEEPING_INTERVAL, Inbound, purge_outbox};
use crate::metrics::Metrics;
use crate::model::KeyOwner;
use crate::state::AppState;
use crate::store::{
    EventStore, MemoryEventStore, MemoryStore, PgEventStore, PgStore, Store, migrate,
};
use crate::{api, listen};

/// Connections per replica.
const POOL_SIZE: u32 = 10;

/// A pool on `url`, with at least two connections (migrations hold one for
/// their lock).
pub async fn connect(url: &DatabaseUrl) -> anyhow::Result<PgPool> {
    // The error could quote the URL: say which variable, not what it held.
    let options = PgConnectOptions::from_str(url.expose_secret())
        .map_err(|_| anyhow::anyhow!("DATABASE_URL is not a valid Postgres URL"))?;
    PgPoolOptions::new()
        .max_connections(POOL_SIZE)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(options)
        .await
        .map_err(|e| anyhow::anyhow!("cannot connect to DATABASE_URL: {}", redacted(&e)))
}

/// A sqlx error's category, never its text (which may quote the URL).
fn redacted(error: &meta_whatsapp_rs::adapters::store::postgres::sqlx::Error) -> &'static str {
    use meta_whatsapp_rs::adapters::store::postgres::sqlx::Error as E;
    match error {
        E::Io(_) => "connection failed",
        E::Tls(_) => "TLS failed",
        E::PoolTimedOut => "timed out",
        E::Database(_) => "the server refused the connection",
        _ => "error",
    }
}

/// The records, the library's key/value store and, with Postgres, the
/// pool, for `config`'s storage.
pub struct Backends {
    /// The service's records.
    pub store: Arc<dyn Store>,
    /// The library's key/value store (the token vault's, webhook dedup's).
    pub kv: Arc<dyn KvStore>,
    /// The library's conversation store (the inbox's).
    pub conversations: Arc<dyn ConversationStore>,
    /// The event outbox.
    pub events: Arc<dyn EventStore>,
    /// The pool, with Postgres.
    pub pool: Option<PgPool>,
}

/// Open the storage `config` names, migrating it unless
/// `WA_SERVER_MIGRATE=skip`.
pub async fn backends(config: &Config) -> anyhow::Result<Backends> {
    match &config.storage {
        Storage::Postgres(url) => {
            let pool = connect(url).await?;
            if config.migrate == MigrateMode::Auto {
                migrate(&pool).await.context("migrations failed")?;
            }
            Ok(Backends {
                store: Arc::new(PgStore::new(pool.clone())),
                kv: Arc::new(PostgresKvStore::new(pool.clone())),
                conversations: Arc::new(PostgresConversationStore::new(pool.clone())),
                events: Arc::new(PgEventStore::new(pool.clone())),
                pool: Some(pool),
            })
        }
        Storage::Memory => {
            tracing::warn!("memory storage: everything is lost on restart (development only)");
            Ok(Backends {
                store: Arc::new(MemoryStore::new()),
                kv: Arc::new(MemoryKvStore::new()),
                conversations: Arc::new(MemoryConversationStore::new()),
                events: Arc::new(MemoryEventStore::new()),
                pool: None,
            })
        }
    }
}

/// The Graph client: production transport, no default token, the
/// configured endpoint and version.
pub fn graph_client(config: &Config) -> anyhow::Result<Client> {
    Ok(meta_whatsapp_rs::client_builder()?
        .endpoint(config.graph_endpoint.clone())
        .build()?)
}

/// The vault on `kv` with the configured keys.
pub fn vault(kv: Arc<dyn KvStore>, keys: VaultKeys) -> anyhow::Result<TokenVault> {
    Ok(TokenVault::new(kv, keys)?)
}

/// Run the service until `SIGTERM` or `SIGINT`.
pub async fn serve(config: Config) -> anyhow::Result<()> {
    if config.vault_key_is_throwaway {
        tracing::warn!("WA_VAULT_KEY is not set: throwaway vault key (development only)");
    }
    if let Some(host) = config.graph_endpoint_override() {
        tracing::warn!(
            host = %host,
            "WA_GRAPH_ENDPOINT is set: Graph calls, and the tokens they carry, go to this host, not graph.facebook.com"
        );
    }
    let backends = backends(&config).await?;
    let client = graph_client(&config)?;
    let Config {
        vault_keys,
        verify_token,
        app_secrets,
        public_bind,
        internal_bind,
        shutdown_grace,
        outbox_retention,
        ..
    } = config;
    let vault = vault(backends.kv.clone(), vault_keys)?;
    let memory = backends.pool.is_none();
    let inbound = Inbound::new(
        app_secrets,
        backends.kv.clone(),
        backends.conversations.clone(),
        backends.events.clone(),
    )?;
    let state = AppState::new(
        backends.store,
        vault,
        client,
        verify_token,
        Metrics::new(),
        inbound,
    );
    if memory {
        // Memory storage exists in development only (the configuration
        // refuses it elsewhere), and the CLI cannot reach it: the first
        // admin key is minted here and written once to standard error,
        // never through the logs.
        let (minted, _) = mint(
            state.store(),
            KeyOwner::Admin,
            Vec::new(),
            "development".to_owned(),
            None,
        )
        .await
        .context("minting the development admin key")?;
        eprintln!(
            "meta-whatsapp-server: development, memory storage: a one-time admin key for this \
             process, shown once: {}",
            minted.expose_key()
        );
    }

    let public = TcpListener::bind(public_bind)
        .await
        .with_context(|| format!("cannot listen on WA_SERVER_PUBLIC_BIND {public_bind}"))?;
    let internal = TcpListener::bind(internal_bind)
        .await
        .with_context(|| format!("cannot listen on WA_SERVER_INTERNAL_BIND {internal_bind}"))?;
    tracing::info!(%public_bind, %internal_bind, "listening");

    let (stop, stopped) = watch::channel(false);
    let housekeeping = tokio::spawn(housekeeping(
        backends.events.clone(),
        backends.pool.clone().map(PostgresKvStore::new),
        outbox_retention,
        HOUSEKEEPING_INTERVAL,
        stop_signal(stopped.clone()),
    ));
    let public_task = tokio::spawn(listen::serve(
        public,
        api::public_router(&state),
        listen::PUBLIC_LIMITS,
        stop_signal(stopped.clone()),
    ));
    let internal_task = tokio::spawn(listen::serve(
        internal,
        api::internal_router(&state),
        listen::INTERNAL_LIMITS,
        stop_signal(stopped),
    ));
    let served = supervise(
        &state,
        &stop,
        public_task,
        internal_task,
        shutdown_signal(),
        shutdown_grace,
    )
    .await;
    housekeeping.abort();
    if let Some(pool) = backends.pool {
        pool.close().await;
    }
    served
}

/// Housekeeping, every `every` until `stop`: purge the outbox past
/// `retention` and, on Postgres, the library's dead key/value rows (webhook
/// dedup markers add one per event). Each round runs on one replica at a
/// time (the outbox purge's advisory lock; a replica that does not get it
/// skips the round). A failure is logged and retried next round.
pub async fn housekeeping(
    events: Arc<dyn EventStore>,
    kv: Option<PostgresKvStore>,
    retention: Duration,
    every: Duration,
    stop: impl Future<Output = ()>,
) {
    let mut ticks = tokio::time::interval(every);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut stop = std::pin::pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => return,
            _ = ticks.tick() => {}
        }
        match purge_outbox(events.as_ref(), retention).await {
            Ok(Some(_)) => {
                if let Some(kv) = &kv
                    && let Err(error) = kv.purge_expired().await
                {
                    tracing::warn!(error = %error, "purging expired key/value rows failed");
                }
            }
            // Another replica holds the lock this round.
            Ok(None) => {}
            Err(error) => tracing::warn!(error = %error, "purging the outbox failed"),
        }
    }
}

/// Completes once `stop` says so: the shutdown future of a listener.
async fn stop_signal(mut stop: watch::Receiver<bool>) {
    let _ = stop.wait_for(|stop| *stop).await;
}

/// The second half of [`serve`]: serve until `signal`, or until either
/// listener stops on its own (its task failing or panicking), which stops
/// the process too rather than leaving it up half served (the orchestrator
/// then restarts it). Then fail `/readyz`, tell the listeners through
/// `stop` to stop accepting, and give the ones still running `grace` to
/// finish their requests.
///
/// # Errors
///
/// A listener that stopped on its own, or ended with an error after `stop`.
pub async fn supervise(
    state: &AppState,
    stop: &watch::Sender<bool>,
    mut public_task: ListenerTask,
    mut internal_task: ListenerTask,
    signal: impl Future<Output = ()>,
    grace: Duration,
) -> anyhow::Result<()> {
    let outcome = first_to_stop(&mut public_task, &mut internal_task, signal).await;
    match &outcome {
        Stopped::Signal => tracing::info!("shutting down"),
        Stopped::Listener { error, .. } => {
            tracing::error!(error = %error, "a listener stopped: shutting down");
        }
    }
    state.begin_shutdown();
    let _ = stop.send(true);
    // The listeners still running get the grace period; one that stopped
    // on its own was already awaited.
    let running = [("public", public_task), ("internal", internal_task)]
        .into_iter()
        .filter(|(name, _)| outcome.ended() != Some(*name))
        .map(|(name, task)| async move { (name, task.await) });
    let drained = tokio::time::timeout(grace, futures::future::join_all(running)).await;
    if let Stopped::Listener { error, .. } = outcome {
        return Err(error);
    }
    let Ok(ended) = drained else {
        tracing::warn!("open requests outlived WA_SERVER_SHUTDOWN_GRACE");
        return Ok(());
    };
    for (name, result) in ended {
        listener_result(name, result)?;
    }
    Ok(())
}

/// A listener task's end.
pub type ListenerTask = JoinHandle<std::io::Result<()>>;

/// Why [`first_to_stop`] returned.
#[derive(Debug)]
pub enum Stopped {
    /// `SIGTERM` or `SIGINT`.
    Signal,
    /// A listener stopped before any signal.
    Listener {
        /// `public` or `internal`.
        name: &'static str,
        /// Why.
        error: anyhow::Error,
    },
}

impl Stopped {
    /// The listener that stopped on its own, if one did.
    pub fn ended(&self) -> Option<&'static str> {
        match self {
            Self::Signal => None,
            Self::Listener { name, .. } => Some(name),
        }
    }
}

/// `Ok` for a listener that ended cleanly after the stop signal, else the
/// error, naming the listener.
fn listener_result(
    name: &'static str,
    ended: Result<std::io::Result<()>, tokio::task::JoinError>,
) -> anyhow::Result<()> {
    match ended {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(anyhow::Error::new(error).context(format!("{name} listener"))),
        Err(join) => Err(anyhow::Error::new(join).context(format!("{name} listener task"))),
    }
}

/// Wait for `signal`, or for either listener task to end on its own,
/// whichever comes first. A listener never ends before the stop signal
/// unless it failed, so its end, with or without an error, is
/// [`Stopped::Listener`].
pub async fn first_to_stop(
    public: &mut ListenerTask,
    internal: &mut ListenerTask,
    signal: impl Future<Output = ()>,
) -> Stopped {
    let ended = |name: &'static str, ended| Stopped::Listener {
        name,
        error: match listener_result(name, ended) {
            Ok(()) => anyhow::anyhow!("the {name} listener stopped"),
            Err(error) => error,
        },
    };
    tokio::select! {
        () = signal => Stopped::Signal,
        result = public => ended("public", result),
        result = internal => ended("internal", result),
    }
}

/// `SIGTERM` or `SIGINT`.
async fn shutdown_signal() {
    let interrupt = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = interrupt => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = interrupt.await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = interrupt.await;
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use super::*;

    fn never() -> ListenerTask {
        tokio::spawn(pending())
    }

    /// Either listener ending before the signal stops the process, with or
    /// without an error, and names it. Decisive: supervising the two
    /// listeners together (a `join!` of both) instead of each.
    #[tokio::test]
    async fn a_listener_that_stops_stops_the_process() {
        let failed = || tokio::spawn(async { Err(std::io::Error::other("accept failed")) });
        let ended = || tokio::spawn(async { Ok(()) });
        let panicked = || -> ListenerTask { tokio::spawn(async { panic!("the accept loop") }) };
        for (which, mut public, mut internal) in [
            ("public", failed(), never()),
            ("internal", never(), failed()),
            ("public", ended(), never()),
            ("internal", never(), ended()),
            ("internal", never(), panicked()),
        ] {
            let stopped = tokio::time::timeout(
                Duration::from_secs(5),
                first_to_stop(&mut public, &mut internal, pending()),
            )
            .await
            .unwrap_or_else(|_| panic!("the {which} listener stopped, the process did not"));
            assert_eq!(stopped.ended(), Some(which), "{stopped:?}");
            let Stopped::Listener { error, .. } = stopped else {
                unreachable!()
            };
            assert!(format!("{error:#}").contains(which), "{error:#}");
            public.abort();
            internal.abort();
        }
    }

    /// A listener on a free local port, stopping when `stop` says so, and
    /// its address.
    async fn listening(
        state: &AppState,
        stop: &watch::Sender<bool>,
    ) -> (ListenerTask, std::net::SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(listen::serve(
            listener,
            api::public_router(state),
            listen::PUBLIC_LIMITS,
            stop_signal(stop.subscribe()),
        ));
        (task, addr)
    }

    /// `serve`'s supervision: a listener that stops on its own (an error
    /// or a panic in its accept loop) stops the process with an error
    /// naming it, after failing `/readyz` and stopping the other listener
    /// (which gets its grace period and then accepts nothing). Decisive:
    /// `supervise` watching the listeners, not only the signal (security
    /// review A3).
    #[tokio::test]
    async fn serve_stops_when_either_listener_stops() {
        for (which, panics) in [("internal", false), ("public", true), ("internal", true)] {
            let state = AppState::for_tests();
            let (stop, _) = watch::channel(false);
            let (healthy, addr) = listening(&state, &stop).await;
            let broken: ListenerTask = if panics {
                tokio::spawn(async { panic!("the accept loop") })
            } else {
                tokio::spawn(async { Err(std::io::Error::other("accept failed")) })
            };
            let (public, internal) = if which == "public" {
                (broken, healthy)
            } else {
                (healthy, broken)
            };
            let served = tokio::time::timeout(
                Duration::from_secs(10),
                supervise(
                    &state,
                    &stop,
                    public,
                    internal,
                    pending(),
                    Duration::from_secs(5),
                ),
            )
            .await
            .unwrap_or_else(|_| panic!("the {which} listener stopped, serve did not"));
            let error = served.expect_err("a listener that stops is an error");
            assert!(format!("{error:#}").contains(which), "{error:#}");
            assert!(state.is_shutting_down(), "/readyz fails");
            assert!(*stop.borrow(), "the other listener was told to stop");
            assert!(
                tokio::net::TcpStream::connect(addr).await.is_err(),
                "the other listener still accepts"
            );
        }
    }

    /// On the signal, both listeners stop accepting and `serve` returns
    /// `Ok` once they drained.
    #[tokio::test]
    async fn serve_stops_both_listeners_on_the_signal() {
        let state = AppState::for_tests();
        let (stop, _) = watch::channel(false);
        let (public, public_addr) = listening(&state, &stop).await;
        let (internal, internal_addr) = listening(&state, &stop).await;
        tokio::time::timeout(
            Duration::from_secs(10),
            supervise(
                &state,
                &stop,
                public,
                internal,
                async {},
                Duration::from_secs(5),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(state.is_shutting_down());
        for addr in [public_addr, internal_addr] {
            assert!(tokio::net::TcpStream::connect(addr).await.is_err());
        }
    }

    /// Housekeeping purges the outbox past retention, round after round,
    /// and stops with the service. Decisive: the purge in the loop.
    #[tokio::test]
    async fn housekeeping_purges_past_retention_until_stopped() {
        use crate::store::events::{EventQuery, NewEvent};
        let events: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());
        let row = |id: &str| NewEvent {
            id: id.to_owned(),
            dedup_key: None,
            tenant: crate::model::TenantId::parse("tenant-a"),
            phone_number_id: None,
            waba_id: None,
            event_type: "message_received".to_owned(),
            data: "{}".to_owned(),
        };
        let first = events.insert(&row("evt_1")).await.unwrap().unwrap();
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(housekeeping(
            events.clone(),
            None,
            Duration::from_millis(1),
            Duration::from_millis(20),
            stop_signal(stopped),
        ));
        let query = EventQuery {
            tenant: crate::model::TenantId::parse("tenant-a").unwrap(),
            after: None,
            types: None,
            phone_number_id: None,
            limit: 10,
        };
        let purged_through = |events: Arc<dyn EventStore>, query: EventQuery| async move {
            events.page(&query).await.unwrap().purged_through
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while purged_through(events.clone(), query.clone()).await < first {
            assert!(tokio::time::Instant::now() < deadline, "never purged");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // A later event goes in a later round.
        let second = events.insert(&row("evt_2")).await.unwrap().unwrap();
        while purged_through(events.clone(), query.clone()).await < second {
            assert!(tokio::time::Instant::now() < deadline, "no second round");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("stopped with the service")
            .unwrap();
    }

    /// The signal stops serving while both listeners run.
    #[tokio::test]
    async fn the_signal_stops_serving() {
        let (mut public, mut internal) = (never(), never());
        let stopped = tokio::time::timeout(
            Duration::from_secs(5),
            first_to_stop(&mut public, &mut internal, async {}),
        )
        .await
        .unwrap();
        assert!(matches!(stopped, Stopped::Signal), "{stopped:?}");
        assert!(!public.is_finished() && !internal.is_finished());
        public.abort();
        internal.abort();
    }
}
