//! What every route shares.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::api::templates::TemplateCache;
use crate::auth::Tokens;
use crate::metrics::Metrics;
use crate::ratelimit::{RateLimiter, RateLimits, Slots};
use crate::store::Store;
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::DEFAULT_TIMEOUT;
use meta_whatsapp_rs::client::embedded_signup::TokenVault;
use meta_whatsapp_rs::core::config::ApiVersion;
use meta_whatsapp_rs::core::secret::VerifyToken;

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

struct Inner {
    store: Arc<dyn Store>,
    tokens: Tokens,
    client: Client,
    verify_token: VerifyToken,
    metrics: Metrics,
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
    /// State on `store` and `vault`, calling Graph with `client` (built
    /// without a default token: each call runs with the tenant's token),
    /// with the default [`Settings`].
    pub fn new(
        store: Arc<dyn Store>,
        vault: TokenVault,
        client: Client,
        verify_token: VerifyToken,
        metrics: Metrics,
    ) -> Self {
        Self::with_settings(
            store,
            vault,
            client,
            verify_token,
            metrics,
            Settings::default(),
        )
    }

    /// [`Self::new`] with `settings`.
    pub fn with_settings(
        store: Arc<dyn Store>,
        vault: TokenVault,
        client: Client,
        verify_token: VerifyToken,
        metrics: Metrics,
        settings: Settings,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                tokens: Tokens::new(vault),
                client,
                verify_token,
                metrics,
                shutting_down: AtomicBool::new(false),
                limiter: RateLimiter::new(settings.rate_limits),
                media_slots: Slots::new(settings.media_concurrency),
                stream_slots: Slots::new(settings.media_streams),
                templates: TemplateCache::new(settings.template_cache_ttl),
                settings,
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

        let vault = TokenVault::new(
            Arc::new(MemoryKvStore::new()),
            VaultKeys::new(VaultKey::generate("test").unwrap()),
        )
        .unwrap();
        let client = Client::builder()
            .transport(ScriptedTransport::new())
            .build()
            .unwrap();
        Self::new(
            Arc::new(crate::store::MemoryStore::new()),
            vault,
            client,
            VerifyToken::new("verify-token-for-unit-tests"),
            Metrics::new(),
        )
    }
}
