//! `meta-whatsapp-server serve` and the pieces the other commands share:
//! the backend (opened by a [`BackendFactory`]), migrations, the vault and
//! the Graph client.
//!
//! **The service is composed from one [`Backend`] alone.** [`serve_with`]
//! takes the bundle and nothing else of storage: the state
//! ([`AppState::from_backend`]) builds the vault and the webhook pipeline
//! on it, housekeeping runs over it
//! ([`meta_whatsapp_server_core::housekeeping`]), and it is closed once
//! the listeners stopped. So a backend an integrator brings (their own
//! [`Backend`], passing `meta_whatsapp_server_core::conformance`) is
//! served without a change here: hand it to [`serve_with`], or run the
//! whole command line on it with a [`BackendFactory`] of theirs
//! ([`crate::cli::run_with`]). [`BuiltinBackends`] opens the in-tree
//! ones, Postgres and memory.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::PgPool;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::postgres::{
    PgConnectOptions, PgPoolOptions,
};
use meta_whatsapp_rs::client::embedded_signup::{TokenVault, VaultKeys};
use meta_whatsapp_rs::core::store::KvStore;
use meta_whatsapp_server_core::housekeeping;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::api::admin::mint;
use crate::config::{Config, DatabaseUrl, MigrateMode, Storage};
use crate::events::{HOUSEKEEPING_INTERVAL, Inbound};
use crate::metrics::Metrics;
use crate::model::KeyOwner;
use crate::state::AppState;
use crate::store::{Backend, MemoryBackend, PgBackend};
use crate::{api, listen};

/// Connections per replica.
const POOL_SIZE: u32 = 10;

// The webhook path takes at most MAX_DELIVERIES_RECORDING connections: API
// calls always find one.
const _: () = assert!(crate::events::MAX_DELIVERIES_RECORDING < POOL_SIZE as usize);

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

/// What opens the storage a configuration names, for every command that
/// stores (`serve`, `migrate`, `admin …`, `vault rotate`): the one place a
/// [`Backend`] is chosen. [`BuiltinBackends`] opens the in-tree ones; an
/// integrator with a backend of their own implements this and runs the
/// command line with [`crate::cli::run_with`].
#[async_trait]
pub trait BackendFactory: Send + Sync {
    /// The backend on `storage` (`DATABASE_URL`, or memory storage in
    /// development), connected, not migrated ([`open`] migrates). Its
    /// errors name settings, never their values (a URL may hold a
    /// password).
    async fn open(&self, storage: &Storage) -> anyhow::Result<Arc<dyn Backend>>;
}

/// The in-tree backends: [`PgBackend`] for a database URL, on a pool of
/// ten connections ([`connect`]), and [`MemoryBackend`] for memory
/// storage.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltinBackends;

#[async_trait]
impl BackendFactory for BuiltinBackends {
    async fn open(&self, storage: &Storage) -> anyhow::Result<Arc<dyn Backend>> {
        Ok(match storage {
            Storage::Postgres(url) => Arc::new(PgBackend::new(connect(url).await?)),
            Storage::Memory => Arc::new(MemoryBackend::new()),
        })
    }
}

/// `factory`'s backend on `storage`, migrated unless `migrate` is
/// [`MigrateMode::Skip`].
///
/// # Errors
///
/// The factory's, or the migrations failing.
pub async fn open(
    factory: &dyn BackendFactory,
    storage: &Storage,
    migrate: MigrateMode,
) -> anyhow::Result<Arc<dyn Backend>> {
    let backend = factory.open(storage).await?;
    if migrate == MigrateMode::Auto {
        backend
            .migrator()
            .migrate()
            .await
            .context("migrations failed")?;
    }
    Ok(backend)
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

/// Run the service until `SIGTERM` or `SIGINT`, on the backend `config`
/// names, opened by the in-tree [`BuiltinBackends`] ([`serve_from`]).
///
/// # Errors
///
/// As [`serve_from`].
pub async fn serve(config: Config) -> anyhow::Result<()> {
    serve_from(config, &BuiltinBackends).await
}

/// Run the service until `SIGTERM` or `SIGINT`, on the backend `factory`
/// opens for `config.storage`: [`serve_with`] once it is open (the
/// warnings about the configuration come first, as the backend may take
/// a while to answer).
///
/// # Errors
///
/// As [`serve_with`], or the backend failing to open.
pub async fn serve_from(config: Config, factory: &dyn BackendFactory) -> anyhow::Result<()> {
    warn_about(&config);
    let backend = factory.open(&config.storage).await?;
    run(config, backend, shutdown_signal).await
}

/// Run the service on `backend` until `SIGTERM` or `SIGINT`
/// ([`serve_until`]). `config.storage` is not read: `backend` is the
/// storage.
///
/// # Errors
///
/// As [`serve_until`].
pub async fn serve_with(config: Config, backend: Arc<dyn Backend>) -> anyhow::Result<()> {
    warn_about(&config);
    run(config, backend, shutdown_signal).await
}

/// Run the service on `backend`, and on nothing else of storage, until
/// `signal` completes: migrate it unless `WA_SERVER_MIGRATE=skip`, compose
/// the state from it ([`AppState::from_backend`]), mint a one-time admin
/// key when it is process-local (the command line cannot reach it), serve
/// both listeners, run housekeeping over it
/// ([`meta_whatsapp_server_core::housekeeping::run`]), and close it once
/// the listeners stopped ([`supervise`]). `config.storage` is not read.
///
/// # Errors
///
/// The migrations, the state's composition or a listener failing (see
/// [`supervise`]).
pub async fn serve_until(
    config: Config,
    backend: Arc<dyn Backend>,
    signal: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    warn_about(&config);
    run(config, backend, move || signal).await
}

/// The settings an operator must not miss in production.
fn warn_about(config: &Config) {
    if config.vault_key_is_throwaway {
        tracing::warn!("WA_VAULT_KEY is not set: throwaway vault key (development only)");
    }
    if let Some(host) = config.graph_endpoint_override() {
        tracing::warn!(
            host = %host,
            "WA_GRAPH_ENDPOINT is set: Graph calls, and the tokens they carry, go to this host, not graph.facebook.com"
        );
    }
}

/// [`serve_until`] once the configuration's warnings are out, stopping on
/// the future `signal` makes: made before the listeners are bound, so
/// that [`shutdown_signal`]'s handlers are in place once they answer.
async fn run<S: Future<Output = ()>>(
    config: Config,
    backend: Arc<dyn Backend>,
    signal: impl FnOnce() -> S,
) -> anyhow::Result<()> {
    if backend.kind().is_process_local() {
        tracing::warn!("memory storage: everything is lost on restart (development only)");
    }
    if config.migrate == MigrateMode::Auto {
        backend
            .migrator()
            .migrate()
            .await
            .context("migrations failed")?;
    }
    let client = graph_client(&config)?;
    let Config {
        vault_keys,
        verify_token,
        app_secrets,
        public_bind,
        internal_bind,
        shutdown_grace,
        outbox_retention,
        settings,
        ..
    } = config;
    let inbound = Inbound::new(app_secrets, verify_token)?;
    let state = AppState::from_backend(
        backend,
        vault_keys,
        client,
        inbound,
        Metrics::new(),
        settings,
    )?;
    let backend = state.backend().clone();
    if backend.kind().is_process_local() {
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

    // `SIGTERM` and `SIGINT` are caught from here: one that comes once the
    // listeners answer stops the service as any other does, rather than
    // ending the process before `supervise` waits for it.
    let signal = signal();
    let public = TcpListener::bind(public_bind)
        .await
        .with_context(|| format!("cannot listen on WA_SERVER_PUBLIC_BIND {public_bind}"))?;
    let internal = TcpListener::bind(internal_bind)
        .await
        .with_context(|| format!("cannot listen on WA_SERVER_INTERNAL_BIND {internal_bind}"))?;
    tracing::info!(%public_bind, %internal_bind, "listening");

    let (stop, stopped) = watch::channel(false);
    let housekeeping = tokio::spawn(housekeeping::run(
        backend.clone(),
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
        signal,
        shutdown_grace,
    )
    .await;
    housekeeping.abort();
    backend.close().await;
    served
}

/// Completes once `stop` says so: the shutdown future of a listener.
async fn stop_signal(mut stop: watch::Receiver<bool>) {
    let _ = stop.wait_for(|stop| *stop).await;
}

/// The second half of [`serve_until`]: serve until `signal`, or until either
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

/// Completes on `SIGTERM` or `SIGINT`, caught from this call on (not from
/// the future's first poll): one that comes before the future is awaited
/// is kept, and completes it at once. Without a `SIGINT` handler it
/// completes at once (as `tokio::signal::ctrl_c` failing did); without a
/// `SIGTERM` one it waits for `SIGINT` alone. Called in a runtime.
fn shutdown_signal() -> impl Future<Output = ()> + Send + 'static {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let interrupt = signal(SignalKind::interrupt());
        let terminate = signal(SignalKind::terminate());
        async move {
            let Ok(mut interrupt) = interrupt else {
                return;
            };
            match terminate {
                Ok(mut terminate) => {
                    tokio::select! {
                        _ = interrupt.recv() => {}
                        _ = terminate.recv() => {}
                    }
                }
                Err(_) => {
                    let _ = interrupt.recv().await;
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        async {
            let _ = tokio::signal::ctrl_c().await;
        }
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
        let _ports = crate::LOOPBACK_PORTS.lock().await;
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
        let _ports = crate::LOOPBACK_PORTS.lock().await;
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

    /// `SIGTERM` is caught from the moment [`shutdown_signal`] is called,
    /// before its future is first polled: `run` calls it before binding
    /// the listeners, so a `SIGTERM` that comes once they answer, before
    /// `supervise` awaits the signal, stops the service cleanly instead of
    /// ending the process (`signal 15`, as
    /// `binary::development_on_memory_prints_a_one_time_admin_key` saw on
    /// a loaded machine). Decisive: the handlers registered in the future
    /// (an `async fn`, as before): the `SIGTERM` below then ends this test
    /// process.
    #[cfg(unix)]
    #[tokio::test]
    async fn sigterm_is_caught_before_the_signal_is_awaited() {
        // `kill` is a child process: between its fork and its exec it holds
        // a copy of every socket open here, so a listener another test just
        // closed would still accept.
        let _ports = crate::LOOPBACK_PORTS.lock().await;
        let signal = shutdown_signal();
        let sent = std::process::Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .unwrap();
        assert!(sent.success());
        tokio::time::timeout(Duration::from_secs(10), signal)
            .await
            .expect("the SIGTERM sent before the signal was awaited was lost");
    }

    /// A development configuration's variables, listening on `public`
    /// and `internal`.
    struct Vars(std::collections::HashMap<&'static str, String>);

    impl crate::config::Env for Vars {
        fn var(&self, name: &str) -> Result<Option<String>, crate::config::NotUnicode> {
            Ok(self.0.get(name).cloned())
        }
        fn read_file(&self, _: &str) -> std::io::Result<String> {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        }
    }

    /// `run` makes its stop signal before it binds either listener, so
    /// that [`shutdown_signal`]'s handlers are in place before anything
    /// answers (see the test above). Decisive: making it after the
    /// listeners are bound (as awaiting `shutdown_signal()` in
    /// `supervise` did).
    #[tokio::test]
    async fn the_stop_signal_is_made_before_the_listeners_are_bound() {
        let _ports = crate::LOOPBACK_PORTS.lock().await;
        let free = || {
            std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
        };
        let (public, internal) = (free(), free());
        let config = Config::from_env(&Vars(std::collections::HashMap::from([
            ("WA_SERVER_ENV", "development".to_owned()),
            ("WA_APP_SECRET", "app-secret-for-unit-tests".to_owned()),
            ("WA_VERIFY_TOKEN", "verify-token-for-unit-tests".to_owned()),
            ("WA_SERVER_PUBLIC_BIND", public.to_string()),
            ("WA_SERVER_INTERNAL_BIND", internal.to_string()),
        ])))
        .unwrap();
        let unbound = Arc::new(std::sync::Mutex::new(None));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let made = unbound.clone();
        let served = tokio::spawn(run(config, Arc::new(MemoryBackend::new()), move || {
            let refused = |addr| std::net::TcpStream::connect(addr).is_err();
            *made.lock().unwrap() = Some(refused(public) && refused(internal));
            async move {
                let _ = stopped.await;
            }
        }));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::net::TcpStream::connect(internal).await.is_err() {
            assert!(tokio::time::Instant::now() < deadline, "never listened");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), served)
            .await
            .expect("stopped on the signal")
            .unwrap()
            .unwrap();
        assert_eq!(
            *unbound.lock().unwrap(),
            Some(true),
            "the stop signal was made once a listener was bound"
        );
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
