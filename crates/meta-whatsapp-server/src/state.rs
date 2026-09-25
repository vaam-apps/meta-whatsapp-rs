//! What every route shares.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::TokenVault;
use meta_whatsapp_rs::core::config::ApiVersion;
use meta_whatsapp_rs::core::secret::VerifyToken;

use crate::auth::Tokens;
use crate::events::{Events, Inbound};
use crate::metrics::Metrics;
use crate::store::Store;

/// Shared state. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    store: Arc<dyn Store>,
    tokens: Tokens,
    client: Client,
    verify_token: VerifyToken,
    metrics: Metrics,
    events: Events,
    shutting_down: AtomicBool,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

impl AppState {
    /// State on `store` and `vault`, calling Graph with `client` (built
    /// without a default token: each call runs with the tenant's token),
    /// receiving Meta's webhooks into `inbound`'s stores.
    pub fn new(
        store: Arc<dyn Store>,
        vault: TokenVault,
        client: Client,
        verify_token: VerifyToken,
        metrics: Metrics,
        inbound: Inbound,
    ) -> Self {
        let events = Events::new(
            inbound,
            store.clone(),
            verify_token.clone(),
            metrics.clone(),
        );
        Self {
            inner: Arc::new(Inner {
                store,
                tokens: Tokens::new(vault),
                client,
                verify_token,
                metrics,
                events,
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    /// The service's records.
    pub fn store(&self) -> &dyn Store {
        self.inner.store.as_ref()
    }

    /// The token vault, behind the authorization order (see
    /// [`crate::auth`]).
    pub(crate) fn tokens(&self) -> &Tokens {
        &self.inner.tokens
    }

    /// The tokenless Graph client.
    pub(crate) fn client(&self) -> &Client {
        &self.inner.client
    }

    /// Meta's webhook verify token.
    pub(crate) fn verify_token(&self) -> &VerifyToken {
        &self.inner.verify_token
    }

    /// The metrics.
    pub fn metrics(&self) -> &Metrics {
        &self.inner.metrics
    }

    /// Meta's webhook pipeline and the event outbox.
    pub(crate) fn events(&self) -> &Events {
        &self.inner.events
    }

    /// The Graph API version calls use.
    pub fn graph_api_version(&self) -> ApiVersion {
        self.inner.client.endpoint().version()
    }

    /// Mark the service as shutting down: `/readyz` fails from now on.
    pub fn begin_shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
    }

    /// Whether [`Self::begin_shutdown`] was called.
    pub fn is_shutting_down(&self) -> bool {
        self.inner.shutting_down.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
impl AppState {
    /// A state on memory stores, with a Graph client scripted to answer
    /// nothing: for unit tests that never call Meta.
    #[allow(clippy::unwrap_used)] // test helper: a panic is the report
    pub(crate) fn for_tests() -> Self {
        use meta_whatsapp_rs::adapters::store::MemoryKvStore;
        use meta_whatsapp_rs::client::embedded_signup::{VaultKey, VaultKeys};
        use meta_whatsapp_rs::core::testing::ScriptedTransport;

        let kv = Arc::new(MemoryKvStore::new());
        let vault = TokenVault::new(
            kv.clone(),
            VaultKeys::new(VaultKey::generate("test").unwrap()),
        )
        .unwrap();
        let client = Client::builder()
            .transport(ScriptedTransport::new())
            .build()
            .unwrap();
        let inbound = Inbound::in_memory(
            meta_whatsapp_rs::core::secret::AppSecret::new("app-secret-for-unit-tests"),
            kv,
        )
        .unwrap();
        Self::new(
            Arc::new(crate::store::MemoryStore::new()),
            vault,
            client,
            VerifyToken::new("verify-token-for-unit-tests"),
            Metrics::new(),
            inbound,
        )
    }
}
