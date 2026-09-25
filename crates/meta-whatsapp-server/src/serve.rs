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
use meta_whatsapp_rs::adapters::store::{MemoryKvStore, PostgresKvStore};
use meta_whatsapp_rs::client::embedded_signup::{TokenVault, VaultKeys};
use meta_whatsapp_rs::core::store::KvStore;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::api;
use crate::config::{Config, DatabaseUrl, MigrateMode, Storage};
use crate::metrics::Metrics;
use crate::state::AppState;
use crate::store::{MemoryStore, PgStore, Store, migrate};

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
    /// The library's key/value store (the token vault's).
    pub kv: Arc<dyn KvStore>,
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
                pool: Some(pool),
            })
        }
        Storage::Memory => {
            tracing::warn!("memory storage: everything is lost on restart (development only)");
            Ok(Backends {
                store: Arc::new(MemoryStore::new()),
                kv: Arc::new(MemoryKvStore::new()),
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
    let backends = backends(&config).await?;
    let client = graph_client(&config)?;
    let Config {
        vault_keys,
        verify_token,
        public_bind,
        internal_bind,
        shutdown_grace,
        ..
    } = config;
    let vault = vault(backends.kv, vault_keys)?;
    let state = AppState::new(backends.store, vault, client, verify_token, Metrics::new());

    let public = TcpListener::bind(public_bind)
        .await
        .with_context(|| format!("cannot listen on WA_SERVER_PUBLIC_BIND {public_bind}"))?;
    let internal = TcpListener::bind(internal_bind)
        .await
        .with_context(|| format!("cannot listen on WA_SERVER_INTERNAL_BIND {internal_bind}"))?;
    tracing::info!(%public_bind, %internal_bind, "listening");

    let (stop, stopped) = watch::channel(false);
    let wait = |mut rx: watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|stop| *stop).await;
    };
    let public_server = axum_serve(public, api::public_router(&state), wait(stopped.clone()));
    let internal_server = axum_serve(internal, api::internal_router(&state), wait(stopped));
    let servers = tokio::spawn(async move { tokio::join!(public_server, internal_server) });

    shutdown_signal().await;
    tracing::info!("shutting down");
    state.begin_shutdown();
    let _ = stop.send(true);
    match tokio::time::timeout(shutdown_grace, servers).await {
        Ok(Ok((public, internal))) => {
            public.context("public listener")?;
            internal.context("internal listener")?;
        }
        Ok(Err(join)) => return Err(join).context("listener task"),
        Err(_) => tracing::warn!("open requests outlived WA_SERVER_SHUTDOWN_GRACE"),
    }
    if let Some(pool) = backends.pool {
        pool.close().await;
    }
    Ok(())
}

async fn axum_serve(
    listener: TcpListener,
    router: meta_whatsapp_rs::webhooks::axum::Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    meta_whatsapp_rs::webhooks::axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
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
