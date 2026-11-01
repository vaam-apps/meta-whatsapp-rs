//! What every route shares: [`AppState`], composed from one
//! [`Backend`] alone ([`AppState::from_backend`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::auth::Authorizer;
use crate::events::{Events, Inbound};
use crate::metrics::Metrics;
use crate::ratelimit::{RateLimiter, RateLimits, Slots};
use crate::store::{Backend, IdempotencyRecords, Outbox, RecordStore};
use meta_whatsapp_rs::client::DEFAULT_TIMEOUT;
use meta_whatsapp_rs::client::embedded_signup::{TokenVault, VaultKeys};
use meta_whatsapp_rs::core::config::ApiVersion;
use meta_whatsapp_rs::core::secret::VerifyToken;
use meta_whatsapp_rs::webhooks::axum::extract::FromRef;
use meta_whatsapp_rs::{Client, Error};
use meta_whatsapp_server_core::templates::TemplateCache;

/// Default `WA_SERVER_IDEMPOTENCY_TTL`: how long an idempotency key's
/// record is kept (docs/design/server.md, section 5.4).
pub const DEFAULT_IDEMPOTENCY_TTL: Duration = Duration::from_hours(24);

/// How long a claimed idempotency key stays `in_progress` before its
/// outcome counts as unknown: twice the Graph timeout (section 5.4), the
/// library's default one, which the service keeps.
pub const IDEMPOTENCY_LEASE: Duration = DEFAULT_TIMEOUT.saturating_mul(2);

/// Default `WA_SERVER_MEDIA_MAX_BYTES` (section 7.2): the largest upload,
/// and the largest streamed download.
pub const DEFAULT_MEDIA_MAX_BYTES: u64 = 100 * 1024 * 1024;

/// Default `WA_SERVER_MEDIA_CONCURRENCY`: media transfers held in memory
/// at once on a replica (uploads, unstreamed downloads). The design asks
/// for "bounded media concurrency" (section 6) without a number: a
/// conservative one, as each may hold up to `WA_SERVER_MEDIA_MAX_BYTES`.
pub const DEFAULT_MEDIA_CONCURRENCY: usize = 4;

/// Default `WA_SERVER_MEDIA_STREAMS`: streamed downloads (`?stream=true`)
/// a replica forwards at once. Each holds a connection to Meta and one to
/// the caller rather than memory; the design states no number.
pub const DEFAULT_MEDIA_STREAMS: usize = 16;

/// How long a WABA's template list is cached (section 4.2: Meta allows 200
/// management calls an hour per WABA).
pub const TEMPLATE_CACHE_TTL: Duration = Duration::from_secs(60);

/// What the operator tunes (sections 5.4, 6 and 7.2), and the design's
/// fixed values, which tests shorten.
#[derive(Debug, Clone)]
pub struct Settings {
    /// `WA_SERVER_RATE_*`.
    pub rate_limits: RateLimits,
    /// `WA_SERVER_IDEMPOTENCY_TTL`.
    pub idempotency_ttl: Duration,
    /// [`IDEMPOTENCY_LEASE`].
    pub idempotency_lease: Duration,
    /// `WA_SERVER_MEDIA_MAX_BYTES`.
    pub media_max_bytes: u64,
    /// `WA_SERVER_MEDIA_CONCURRENCY`.
    pub media_concurrency: usize,
    /// `WA_SERVER_MEDIA_STREAMS`.
    pub media_streams: usize,
    /// [`TEMPLATE_CACHE_TTL`].
    pub template_cache_ttl: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            rate_limits: RateLimits::default(),
            idempotency_ttl: DEFAULT_IDEMPOTENCY_TTL,
            idempotency_lease: IDEMPOTENCY_LEASE,
            media_max_bytes: DEFAULT_MEDIA_MAX_BYTES,
            media_concurrency: DEFAULT_MEDIA_CONCURRENCY,
            media_streams: DEFAULT_MEDIA_STREAMS,
            template_cache_ttl: TEMPLATE_CACHE_TTL,
        }
    }
}

/// Shared state. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

/// What the state holds: the ports it took from its backend, never the
/// backend itself (its composer keeps that: `serve` closes it once the
/// listeners stopped). Before roadmap S7 it held the backend too, behind a
/// crate-private accessor only `serve` used.
struct Inner {
    store: Arc<dyn RecordStore>,
    idempotency: Arc<dyn IdempotencyRecords>,
    authz: Authorizer,
    metrics: Metrics,
    events: Events,
    shutting_down: AtomicBool,
    settings: Settings,
    limiter: RateLimiter,
    media_slots: Slots,
    stream_slots: Slots,
    templates: TemplateCache,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

impl AppState {
    /// The service on `backend`, and on nothing else: its records and
    /// idempotency records, the token vault on its `KvStore` under
    /// `vault_keys`, and the webhook pipeline `inbound` describes, built
    /// here on its `KvStore` (dedup leases), `ConversationStore` (the
    /// inbox), `Outbox` and records (routing). Graph is called with
    /// `client` (built without a default token: each call runs with the
    /// tenant's), and "now" read from `inbound`'s clock, the service's
    /// one. A backend is the unit of swapping (docs/design/server.md,
    /// section 8.2): its ports share one database's guarantees, so there
    /// is no constructor taking ports one by one, and a backend an
    /// integrator brings is served as the in-tree ones are.
    ///
    /// # Errors
    ///
    /// `vault_keys` holds two keys with one id (the library's
    /// `TokenVault::new` refuses them), or `client` carries a token (the
    /// core's `Authorizer::new` refuses it).
    ///
    /// Besides the state, its [`Grant`]: what an API adapter's routes are
    /// served with (the records, the authorization order), handed to the
    /// one composing the state, who holds the backend already, and made
    /// nowhere else: code given only the state never reaches them.
    // The bundle is handed over whole (roadmap S4, pinned by
    // `tests/visibility/fail/app_state_from_loose_ports.rs`); the state
    // keeps the ports it takes from it, its composer the bundle.
    #[allow(clippy::needless_pass_by_value)]
    pub fn from_backend(
        backend: Arc<dyn Backend>,
        vault_keys: VaultKeys,
        client: Client,
        inbound: Inbound,
        metrics: Metrics,
        settings: Settings,
    ) -> Result<(Self, Grant), Error> {
        let store = backend.records();
        let vault = TokenVault::new(backend.kv(), vault_keys)?;
        // One clock for the whole service: the webhook pipeline's.
        let authz = Authorizer::new(store.clone(), vault, client)?.with_clock(inbound.clock());
        let events = Events::new(inbound, backend.as_ref(), metrics.clone());
        let state = Self {
            inner: Arc::new(Inner {
                idempotency: backend.idempotency(),
                authz,
                store,
                metrics,
                events,
                shutting_down: AtomicBool::new(false),
                limiter: RateLimiter::new(settings.rate_limits),
                media_slots: Slots::new(settings.media_concurrency),
                stream_slots: Slots::new(settings.media_streams),
                templates: TemplateCache::new(settings.template_cache_ttl),
                settings,
            }),
        };
        let grant = Grant(state.clone());
        Ok((state, grant))
    }

    /// The service's records. Crate-private: the records decide who owns
    /// what, and only the service's own handlers write them.
    pub(crate) fn store(&self) -> &dyn RecordStore {
        self.inner.store.as_ref()
    }

    /// The idempotency keys' records.
    pub(crate) fn idempotency_records(&self) -> &Arc<dyn IdempotencyRecords> {
        &self.inner.idempotency
    }

    /// The authorization order (see [`crate::auth`]): the only way to a
    /// stored token, and to write the vault (with an admin's
    /// [`crate::auth::AdminCaller`], or an owned WABA). Crate-private, as
    /// the vault and the tokenless client were before it.
    pub(crate) fn authz(&self) -> &Authorizer {
        &self.inner.authz
    }

    /// The tokenless Graph client.
    pub(crate) fn client(&self) -> &Client {
        self.inner.authz.client()
    }

    /// Meta's webhook verify token.
    pub(crate) fn verify_token(&self) -> &VerifyToken {
        self.inner.events.verify_token()
    }

    /// The metrics.
    pub fn metrics(&self) -> &Metrics {
        &self.inner.metrics
    }

    /// Meta's webhook pipeline and the event outbox.
    pub(crate) fn events(&self) -> &Events {
        &self.inner.events
    }

    /// The settings.
    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    /// The rate limiter.
    pub(crate) fn limiter(&self) -> &RateLimiter {
        &self.inner.limiter
    }

    /// Slots for media transfers held in memory (uploads, unstreamed
    /// downloads): `WA_SERVER_MEDIA_CONCURRENCY` of them, half of them at
    /// most for one tenant.
    pub fn media_slots(&self) -> &Slots {
        &self.inner.media_slots
    }

    /// Slots for streamed downloads: `WA_SERVER_MEDIA_STREAMS` of them,
    /// half of them at most for one tenant.
    pub fn stream_slots(&self) -> &Slots {
        &self.inner.stream_slots
    }

    /// The template lists cached per WABA.
    pub(crate) fn template_cache(&self) -> &TemplateCache {
        &self.inner.templates
    }

    /// The Graph API version calls use.
    pub fn graph_api_version(&self) -> ApiVersion {
        self.client().endpoint().version()
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

/// What an API adapter's routes are served with: the state, and what the
/// state keeps from its other holders (its records, its authorization
/// order, its outbox, the template cache, the idempotency engine over its
/// records, the core's capabilities behind an owned number or WABA).
///
/// Made only by [`AppState::from_backend`], for the one composing the
/// state, who holds the backend (and so the records) already: a handler, a
/// router or a crate given only an [`AppState`] cannot make one (a UI case
/// pins it, `tests/visibility/fail/grant_from_state.rs`). The service
/// itself hands it to its API adapter's router
/// (`meta-whatsapp-server-api-axum`'s `internal_router`), whose handlers
/// take it as their state ([`crate::api::internal_router_with`]); this
/// crate's own handlers, behind the same router, take the [`AppState`] it
/// derefs to. Before roadmap S7 split the crates these were the state's
/// crate-private methods, and they still are on [`AppState`]. Cheap to
/// clone.
#[derive(Clone)]
pub struct Grant(AppState);

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant").finish_non_exhaustive()
    }
}

impl std::ops::Deref for Grant {
    type Target = AppState;

    fn deref(&self) -> &AppState {
        &self.0
    }
}

/// A route served with a [`Grant`] extracts the [`AppState`] it carries
/// (`State<AppState>`): never the other way round.
impl FromRef<Grant> for AppState {
    fn from_ref(grant: &Grant) -> Self {
        grant.0.clone()
    }
}

impl Grant {
    /// The state.
    pub fn state(&self) -> &AppState {
        &self.0
    }

    /// The service's records: who owns what.
    pub fn store(&self) -> &dyn RecordStore {
        self.0.store()
    }

    /// The authorization order, and through it the vault (see
    /// [`crate::auth`]).
    pub fn authz(&self) -> &Authorizer {
        self.0.authz()
    }

    /// The event outbox, which `/v1/events` pages.
    pub fn outbox(&self) -> &dyn Outbox {
        self.0.events().outbox()
    }

    /// The template lists cached per WABA.
    pub fn template_cache(&self) -> &TemplateCache {
        self.0.template_cache()
    }
}

#[cfg(test)]
impl AppState {
    /// A state on a memory backend, with a Graph client scripted to answer
    /// nothing: for unit tests that never call Meta.
    #[allow(clippy::unwrap_used)] // test helper: a panic is the report
    pub(crate) fn for_tests() -> Self {
        use meta_whatsapp_rs::client::embedded_signup::VaultKey;
        use meta_whatsapp_rs::core::testing::ScriptedTransport;

        let client = Client::builder()
            .transport(ScriptedTransport::new())
            .build()
            .unwrap();
        let inbound = Inbound::new(
            vec![meta_whatsapp_rs::core::secret::AppSecret::new(
                "app-secret-for-unit-tests",
            )],
            VerifyToken::new("verify-token-for-unit-tests"),
        )
        .unwrap();
        Self::from_backend(
            Arc::new(meta_whatsapp_server_store_memory::MemoryBackend::new()),
            VaultKeys::new(VaultKey::generate("test").unwrap()),
            client,
            inbound,
            Metrics::new(),
            Settings::default(),
        )
        .unwrap()
        .0
    }
}
