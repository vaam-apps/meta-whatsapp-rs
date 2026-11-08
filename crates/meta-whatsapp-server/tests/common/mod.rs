//! Shared by the integration tests: the service on a memory store (or any
//! store), a vault whose reads are counted, and Graph scripted with
//! `ScriptedTransport`.
#![allow(dead_code)] // each test binary uses a different subset

pub mod bot;
pub mod broadcasts;
pub mod capture;
pub mod erasure;
pub mod failing_conversations;
pub mod flows;
pub mod inbox;
pub mod media;
pub mod meta;
pub mod moving;
pub mod otp;
pub mod registration;
pub mod scenarios;
pub mod signup;
pub mod sse;
pub mod token_expiry;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use http_body_util::BodyExt;
use meta_whatsapp_rs::adapters::store::{MemoryConversationStore, MemoryKvStore};
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TOKEN_NAMESPACE, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::clock::ManualClock;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::{AccessToken, AppSecret, VerifyToken};
use meta_whatsapp_rs::core::store::{ConversationStore, Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_rs::core::transport::HttpTransport;
use meta_whatsapp_rs::webhooks::axum::Router;
use meta_whatsapp_rs::webhooks::axum::body::Body;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderMap, Method, Request, StatusCode, header};
use meta_whatsapp_rs::{Client, RetryPolicy};
use meta_whatsapp_server::api::admin::mint;
use meta_whatsapp_server::api::{internal_router, public_router};
use meta_whatsapp_server::events::Inbound;
use meta_whatsapp_server::metrics::Metrics;
use meta_whatsapp_server::model::{AllowedTenants, KeyOwner, Scope, TenantId};
use meta_whatsapp_server::ratelimit::{Rate, RateLimits};
use meta_whatsapp_server::state::{AppState, Grant, Settings};
use meta_whatsapp_server::store::events::{EventPage, EventQuery, NewEvent};
use meta_whatsapp_server::store::{
    Backend, BackendKind, EventNotifier, IdempotencyRecords, Janitor, LeaderLock, LocalNotifier,
    MemoryBackend, MemoryStore, Outbox as EventStore, RecordStore, SchemaMigrator, Store,
};
use serde_json::Value;
use tower::ServiceExt;

/// The verify token of every test service.
pub const VERIFY_TOKEN: &str = "verify-token-for-tests";

/// The app secret Meta's test deliveries are signed with.
pub const APP_SECRET: &str = "app-secret-for-tests";

/// A second app secret, the previous one while rotating.
pub const PREVIOUS_APP_SECRET: &str = "previous-app-secret-for-tests";

/// The app secrets of every test service: [`APP_SECRET`], then
/// [`PREVIOUS_APP_SECRET`].
pub const DEFAULT_APP_SECRETS: &[&str] = &[APP_SECRET, PREVIOUS_APP_SECRET];

/// A `KvStore` that counts reads of the token vault's namespace, and
/// keeps every key written, and every value but a compare and swap's
/// deletion (roadmap M3d's sweep for a PIN).
#[derive(Debug)]
pub struct CountingKv {
    inner: Arc<dyn KvStore>,
    vault_reads: AtomicUsize,
    deletions: AtomicUsize,
    writes: Mutex<Vec<(StoreKey, Option<Vec<u8>>)>>,
    /// Namespace whose next present read is followed by the record's
    /// deletion (see [`Self::lose_next_read_in`]).
    lose_next: Mutex<Option<&'static str>>,
}

impl CountingKv {
    /// Count reads of `inner`.
    pub fn new(inner: Arc<dyn KvStore>) -> Self {
        Self {
            inner,
            vault_reads: AtomicUsize::new(0),
            deletions: AtomicUsize::new(0),
            writes: Mutex::new(Vec::new()),
            lose_next: Mutex::new(None),
        }
    }

    /// The next read of a present record in `namespace` returns it and
    /// then deletes it, as a concurrent claimant's winning compare and
    /// swap would between the read and the reader's own compare and swap.
    pub fn lose_next_read_in(&self, namespace: &'static str) {
        *self.lose_next.lock().unwrap() = Some(namespace);
    }

    /// Every key and value written so far (put, put if absent, compare and
    /// swap), in order; a compare and swap that deletes wrote no value.
    pub fn writes(&self) -> Vec<(StoreKey, Vec<u8>)> {
        self.writes
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(key, value)| Some((key.clone(), value.clone()?)))
            .collect()
    }

    /// Every key written so far (put, put if absent, compare and swap), in
    /// order.
    pub fn written(&self) -> Vec<StoreKey> {
        self.writes
            .lock()
            .unwrap()
            .iter()
            .map(|(key, _)| key.clone())
            .collect()
    }

    fn wrote(&self, key: &StoreKey, value: Option<&[u8]>) {
        self.writes
            .lock()
            .unwrap()
            .push((key.clone(), value.map(<[u8]>::to_vec)));
    }

    /// How many deletions reached the store so far: `delete` calls and
    /// compare and swaps that delete (roadmap M5f3: a refused purge makes
    /// none).
    pub fn deletions(&self) -> usize {
        self.deletions.load(Ordering::SeqCst)
    }

    /// Reads of the vault's namespace so far.
    pub fn vault_reads(&self) -> usize {
        self.vault_reads.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl KvStore for CountingKv {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        if key.namespace() == TOKEN_NAMESPACE {
            self.vault_reads.fetch_add(1, Ordering::SeqCst);
        }
        let read = self.inner.get(key).await?;
        if read.is_some() {
            let lost = {
                let mut armed = self.lose_next.lock().unwrap();
                let hit = *armed == Some(key.namespace());
                if hit {
                    *armed = None;
                }
                hit
            };
            if lost {
                self.inner.delete(key).await?;
            }
        }
        Ok(read)
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.wrote(key, Some(&value));
        self.inner.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.wrote(key, Some(&value));
        self.inner.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.wrote(key, new.as_deref());
        if new.is_none() {
            self.deletions.fetch_add(1, Ordering::SeqCst);
        }
        self.inner
            .compare_and_swap(key, expected, new, expiry)
            .await
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.deletions.fetch_add(1, Ordering::SeqCst);
        self.inner.delete(key).await
    }
}

/// What an insert into [`RecordingEvents`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// It goes to the store.
    Pass,
    /// It fails (a storage error).
    Fail,
    /// It never finishes: the request is cut there, as a crash would.
    Hang,
    /// It fails with the typed contention error (`StorageError::Busy`), as
    /// any backend reports a lock or write conflict it gave up on.
    Busy,
}

/// Work run just before the next insert reaches the store.
pub type BeforeInsert =
    Box<dyn FnOnce() -> futures::future::BoxFuture<'static, ()> + Send + Sync + 'static>;

/// An [`EventStore`] that records every insert the sink makes, and can
/// fail or hang the next ones, or run something just before one.
pub struct RecordingEvents {
    inner: Arc<dyn EventStore>,
    inserted: Mutex<Vec<(NewEvent, Option<i64>)>>,
    /// The next inserts' fates; then they pass.
    script: Mutex<std::collections::VecDeque<Fate>>,
    before: Mutex<Option<BeforeInsert>>,
}

impl RecordingEvents {
    pub fn new(inner: Arc<dyn EventStore>) -> Self {
        Self {
            inner,
            inserted: Mutex::new(Vec::new()),
            script: Mutex::new(std::collections::VecDeque::new()),
            before: Mutex::new(None),
        }
    }

    /// Fail the next `n` inserts (a storage error).
    pub fn fail_next(&self, n: usize) {
        self.script(&vec![true; n]);
    }

    /// The next inserts' fates, in order (`true`: fails); then they pass.
    pub fn script(&self, fates: &[bool]) {
        self.fates(
            &fates
                .iter()
                .map(|&fail| if fail { Fate::Fail } else { Fate::Pass })
                .collect::<Vec<_>>(),
        );
    }

    /// The next inserts' fates, in order; then they pass.
    pub fn fates(&self, fates: &[Fate]) {
        *self.script.lock().unwrap() = fates.iter().copied().collect();
    }

    /// Run `work` just before the next insert reaches the store (after the
    /// sink routed the event).
    pub fn before_next_insert(&self, work: BeforeInsert) {
        *self.before.lock().unwrap() = Some(work);
    }

    /// Every insert so far and its outcome (`None`: already stored).
    pub fn inserts(&self) -> Vec<(NewEvent, Option<i64>)> {
        self.inserted.lock().unwrap().clone()
    }

    /// The rows written (inserts that stored a row).
    pub fn rows(&self) -> Vec<NewEvent> {
        self.inserts()
            .into_iter()
            .filter(|(_, sequence)| sequence.is_some())
            .map(|(row, _)| row)
            .collect()
    }
}

#[async_trait]
impl EventStore for RecordingEvents {
    async fn insert(&self, event: &NewEvent) -> Result<Option<i64>, StorageError> {
        let fate = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Fate::Pass);
        match fate {
            Fate::Pass => {}
            Fate::Fail => {
                return Err(StorageError::Backend(anyhow::anyhow!(
                    "scripted outbox failure"
                )));
            }
            Fate::Hang => futures::future::pending::<()>().await,
            Fate::Busy => return Err(StorageError::Busy),
        }
        let before = self.before.lock().unwrap().take();
        if let Some(work) = before {
            work().await;
        }
        let outcome = self.inner.insert(event).await?;
        self.inserted.lock().unwrap().push((event.clone(), outcome));
        Ok(outcome)
    }

    async fn headers_after(
        &self,
        query: &meta_whatsapp_server_core::outbox::HeaderQuery,
    ) -> Result<meta_whatsapp_server_core::outbox::HeaderPage, StorageError> {
        self.inner.headers_after(query).await
    }

    async fn page(&self, query: &EventQuery) -> Result<EventPage, StorageError> {
        self.inner.page(query).await
    }

    async fn get(
        &self,
        tenant: &TenantId,
        id: &str,
    ) -> Result<Option<meta_whatsapp_server::store::events::StoredEvent>, StorageError> {
        self.inner.get(tenant, id).await
    }

    async fn purge(&self, older_than: std::time::Duration) -> Result<u64, StorageError> {
        self.inner.purge(older_than).await
    }

    async fn received_media(
        &self,
        media_id: &meta_whatsapp_rs::core::ids::MediaId,
    ) -> Result<Vec<meta_whatsapp_server_core::outbox::ReceivedMedia>, StorageError> {
        self.inner.received_media(media_id).await
    }

    async fn erase_contacts(
        &self,
        tenant: &TenantId,
        phone_number_id: &meta_whatsapp_rs::core::ids::PhoneNumberId,
        contacts: &[String],
    ) -> Result<u64, StorageError> {
        self.inner
            .erase_contacts(tenant, phone_number_id, contacts)
            .await
    }
}

/// The bundle a [`Harness`] serves (the service is composed from a
/// `Backend` alone): its [`Stores`], the vault's reads counted and the
/// sink's inserts recorded, and memory's leader lock, janitor and
/// migrator (the harness runs no housekeeping). A backend defined outside
/// the service's crate, as an integrator's would be.
pub struct HarnessBackend {
    store: Arc<dyn Store>,
    kv: Arc<CountingKv>,
    conversations: Arc<dyn ConversationStore>,
    outbox: Arc<RecordingEvents>,
    notifier: Arc<dyn EventNotifier>,
    webhooks: Arc<dyn meta_whatsapp_server::store::WebhookStore>,
    memory: MemoryBackend,
}

#[async_trait]
impl Backend for HarnessBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Other("test harness")
    }
    fn records(&self) -> Arc<dyn RecordStore> {
        self.store.clone()
    }
    fn idempotency(&self) -> Arc<dyn IdempotencyRecords> {
        self.store.clone()
    }
    fn outbox(&self) -> Arc<dyn EventStore> {
        self.outbox.clone()
    }
    fn notifier(&self) -> Arc<dyn EventNotifier> {
        self.notifier.clone()
    }
    fn webhooks(&self) -> Arc<dyn meta_whatsapp_server::store::WebhookStore> {
        self.webhooks.clone()
    }
    fn leader_lock(&self) -> Arc<dyn LeaderLock> {
        self.memory.leader_lock()
    }
    fn janitor(&self) -> Arc<dyn Janitor> {
        self.memory.janitor()
    }
    fn migrator(&self) -> Arc<dyn SchemaMigrator> {
        self.memory.migrator()
    }
    fn kv(&self) -> Arc<dyn KvStore> {
        self.kv.clone()
    }
    fn conversations(&self) -> Arc<dyn ConversationStore> {
        self.conversations.clone()
    }
    async fn close(&self) {}
}

/// The vault key of every test service, and of the harness's own handle
/// on its vault ([`Harness::vault`]): 32 bytes, in base64.
pub const VAULT_KEY: &str = "dGVzdC12YXVsdC1rZXktb2YtMzItYnl0ZXMtZXhhY3Q=";

/// [`VAULT_KEY`], under the id `test`.
pub fn vault_keys() -> VaultKeys {
    VaultKeys::new(VaultKey::from_base64("test", VAULT_KEY).unwrap())
}

/// The service under test.
pub struct Harness {
    pub state: AppState,
    /// The state's grant, which [`Self::internal`] is served with (roadmap
    /// S7): what the API adapter's handlers extract their capabilities on.
    pub grant: Grant,
    pub internal: Router,
    pub public: Router,
    pub graph: ScriptedTransport,
    pub kv: Arc<CountingKv>,
    pub vault: TokenVault,
    pub store: Arc<dyn Store>,
    pub conversations: Arc<dyn ConversationStore>,
    /// The outbox, recording what the sink writes.
    pub outbox: Arc<RecordingEvents>,
    /// The webhook pipeline's "now" (the replay window, the keyless dedup
    /// window): the time the harness was built until a test moves it.
    pub clock: ManualClock,
    /// The bundle the state was composed from: what a test's own
    /// webhooks-out dispatcher runs on (roadmap M2c).
    pub backend: Arc<dyn Backend>,
}

/// The stores a [`Harness`] runs on.
pub struct Stores {
    pub store: Arc<dyn Store>,
    pub kv: Arc<dyn KvStore>,
    pub conversations: Arc<dyn ConversationStore>,
    pub events: Arc<dyn EventStore>,
    /// The live streams' wake-ups: the process's broadcast, or Postgres'
    /// `NOTIFY` on the database (two harnesses on one database are two
    /// replicas).
    pub notifier: Arc<dyn EventNotifier>,
    /// Webhooks out's records (roadmap M2c), on the records' database.
    pub webhooks: Arc<dyn meta_whatsapp_server::store::WebhookStore>,
}

impl Stores {
    /// Everything in memory.
    pub fn memory() -> Self {
        let store = MemoryStore::new();
        Self {
            events: store.outbox(),
            webhooks: store.webhooks(),
            store: Arc::new(store),
            kv: Arc::new(MemoryKvStore::new()),
            conversations: Arc::new(MemoryConversationStore::new()),
            notifier: Arc::new(LocalNotifier::new()),
        }
    }

    /// Everything on Postgres, on `pool` (migrated).
    pub fn postgres(pool: &meta_whatsapp_rs::adapters::store::postgres::sqlx::PgPool) -> Self {
        use meta_whatsapp_rs::adapters::store::{PostgresConversationStore, PostgresKvStore};
        use meta_whatsapp_server::store::{PgEventStore, PgNotifier, PgStore, PgWebhookStore};
        Self {
            webhooks: Arc::new(PgWebhookStore::new(pool.clone())),
            notifier: Arc::new(PgNotifier::new(pool.clone())),
            store: Arc::new(PgStore::new(pool.clone())),
            kv: Arc::new(PostgresKvStore::new(pool.clone())),
            conversations: Arc::new(PostgresConversationStore::new(pool.clone())),
            events: Arc::new(PgEventStore::new(pool.clone())),
        }
    }
}

/// An answer.
#[derive(Debug)]
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub text: String,
}

impl Reply {
    /// The body as JSON.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text))
    }

    /// `error.code` of an error body.
    pub fn code(&self) -> String {
        self.json()["error"]["code"]
            .as_str()
            .unwrap_or_else(|| panic!("no error code: {}", self.text))
            .to_owned()
    }
}

/// A request to build.
pub struct Call {
    method: Method,
    path: String,
    key: Option<String>,
    tenant: Option<String>,
    body: Option<Body>,
    headers: Vec<(String, String)>,
}

impl Call {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            key: None,
            tenant: None,
            body: None,
            headers: Vec::new(),
        }
    }

    pub fn get(path: impl Into<String>) -> Self {
        Self::new(Method::GET, path)
    }

    pub fn key(mut self, key: &str) -> Self {
        self.key = Some(key.to_owned());
        self
    }

    pub fn tenant(mut self, tenant: &str) -> Self {
        self.tenant = Some(tenant.to_owned());
        self
    }

    pub fn json(mut self, body: &Value) -> Self {
        self.body = Some(Body::from(body.to_string()));
        self.headers
            .push(("content-type".to_owned(), "application/json".to_owned()));
        self
    }

    pub fn body(mut self, body: Body) -> Self {
        self.body = Some(body);
        self
    }

    /// A `multipart/form-data` body of `(name, file name, bytes)` parts.
    pub fn multipart(mut self, parts: &[(&str, Option<&str>, &[u8])]) -> Self {
        let (content_type, body) = multipart(parts);
        self.body = Some(Body::from(body));
        self.headers.push(("content-type".to_owned(), content_type));
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn build(self) -> Request<Body> {
        let mut builder = Request::builder().method(self.method).uri(self.path);
        if let Some(key) = self.key {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {key}"));
        }
        if let Some(tenant) = self.tenant {
            builder = builder.header("WA-Tenant", tenant);
        }
        for (name, value) in self.headers {
            builder = builder.header(name, value);
        }
        builder.body(self.body.unwrap_or_else(Body::empty)).unwrap()
    }
}

/// A `multipart/form-data` body of `(name, file name, bytes)` parts, and
/// its content type.
pub fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "wa-test-boundary-7MA4YWxkTrZu0gW";
    let mut body = Vec::new();
    for (name, filename, data) in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match filename {
            Some(filename) => body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n\
                     Content-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            ),
            None => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            ),
        }
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

/// Send `request` to `router`.
pub async fn send(router: &Router, request: Request<Body>) -> Reply {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let live = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    let bytes = if live {
        // A live event stream never ends: what it sent within a moment,
        // then it is dropped (the client went away).
        let mut body = response.into_body();
        let mut bytes = Vec::new();
        while let Ok(Some(Ok(frame))) =
            tokio::time::timeout(std::time::Duration::from_millis(100), body.frame()).await
        {
            if let Ok(data) = frame.into_data() {
                bytes.extend_from_slice(&data);
            }
        }
        bytes.into()
    } else {
        response.into_body().collect().await.unwrap().to_bytes()
    };
    Reply {
        status,
        headers,
        text: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// Rate limits no test reaches by accident: the tests of the limits set
/// their own.
pub fn unlimited() -> RateLimits {
    let rate = Rate {
        per_second: 1_000_000,
        burst: 1_000_000,
    };
    RateLimits {
        send: rate,
        read: rate,
        templates: rate,
        otp_issue: rate,
        otp_verify: rate,
    }
}

/// The OTP pepper of every test service (roadmap M3c): 32 bytes or more.
pub const OTP_PEPPER: &str = "otp-pepper-of-the-test-services-32-bytes-or-more";

/// The OTP settings of a test service: [`OTP_PEPPER`], and `default_tenant`
/// for the tenants without settings of their own.
pub fn otp_deployment(
    default_tenant: Option<&str>,
) -> meta_whatsapp_server_core::otp::OtpDeployment {
    meta_whatsapp_server_core::otp::OtpDeployment::new(
        meta_whatsapp_server_core::otp::OtpKeys::new(
            meta_whatsapp_rs::core::secret::SecretBytes::new(OTP_PEPPER.as_bytes().to_vec()),
        )
        .unwrap(),
        default_tenant.map(|id| TenantId::parse(id).unwrap()),
    )
}

/// The settings of a test service: the defaults, without rate limits, and
/// OTP set up with no default OTP tenant (a tenant without OTP settings is
/// `409 otp_not_configured`).
pub fn test_settings() -> Settings {
    Settings {
        rate_limits: unlimited(),
        otp: Some(otp_deployment(None)),
        erasure: Some(erasure_deployment()),
        // The Flows data endpoint (roadmap M5d), answered by an app that
        // sends the KAT's summary screen.
        flows: Some(flows::deployment(flows::Stub::summary())),
        // Roadmap M5c5: the tests that assert a page's example exactly
        // read the body the service builds, not the account it names
        // (on by default; `messaging_account.rs` and its live twin turn
        // it on and walk every route).
        send_messaging_account_id: false,
        ..Settings::default()
    }
}

/// The erasure key of every test service (roadmap M2f).
pub const ERASURE_KEY: &str = "erasure-key-of-the-tests-32-bytes-long";

/// The erasure settings of a test service: [`ERASURE_KEY`], the service's
/// log journal.
pub fn erasure_deployment() -> meta_whatsapp_server_core::erasure::ErasureDeployment {
    meta_whatsapp_server_core::erasure::ErasureDeployment::new(
        meta_whatsapp_server_core::erasure::ErasureKey::new(
            meta_whatsapp_rs::core::secret::SecretBytes::new(ERASURE_KEY.as_bytes().to_vec()),
        )
        .unwrap(),
    )
}

impl Harness {
    /// On a memory store.
    pub fn new() -> Self {
        Self::with_settings(test_settings())
    }

    /// On a memory store, with `settings`.
    pub fn with_settings(settings: Settings) -> Self {
        Self::build(Stores::memory(), settings, DEFAULT_APP_SECRETS, |graph| {
            Arc::new(graph)
        })
    }

    /// On `store` and `kv`, the inbox and the outbox in memory.
    pub fn on(store: Arc<dyn Store>, kv: Arc<dyn KvStore>) -> Self {
        Self::on_with(store, kv, test_settings())
    }

    /// On `store` and `kv`, with `settings`, the inbox and the outbox in
    /// memory.
    pub fn on_with(store: Arc<dyn Store>, kv: Arc<dyn KvStore>, settings: Settings) -> Self {
        let stores = Stores {
            store,
            kv,
            ..Stores::memory()
        };
        Self::build(stores, settings, DEFAULT_APP_SECRETS, |graph| {
            Arc::new(graph)
        })
    }

    /// On `stores`, with `settings` (roadmap M3c: a default OTP tenant on
    /// Postgres).
    pub fn with_stores_and_settings(stores: Stores, settings: Settings) -> Self {
        Self::build(stores, settings, DEFAULT_APP_SECRETS, |graph| {
            Arc::new(graph)
        })
    }

    /// On `stores`, verifying deliveries against [`APP_SECRET`] and
    /// [`PREVIOUS_APP_SECRET`].
    pub fn with(stores: Stores) -> Self {
        Self::with_app_secrets(stores, DEFAULT_APP_SECRETS)
    }

    /// On `stores`, verifying deliveries against `app_secrets` (the first
    /// one derives event ids).
    pub fn with_app_secrets(stores: Stores, app_secrets: &[&str]) -> Self {
        Self::build(stores, test_settings(), app_secrets, |graph| {
            Arc::new(graph)
        })
    }

    /// On a memory store, with `settings`, Meta reached through what
    /// `wrap` makes of the scripted transport (`graph` still records every
    /// request): a download body streamed in parts, say.
    pub fn with_transport(
        settings: Settings,
        wrap: impl FnOnce(ScriptedTransport) -> Arc<dyn HttpTransport>,
    ) -> Self {
        Self::build(Stores::memory(), settings, DEFAULT_APP_SECRETS, wrap)
    }

    /// On a memory store, with `settings`, Meta reached by a client that
    /// follows `retry` (every other harness never retries), to prove what
    /// the service does not let the library replay.
    #[allow(dead_code)] // not every test binary of the service uses it
    pub fn with_retry(settings: Settings, retry: RetryPolicy) -> Self {
        Self::build_with_retry(
            Stores::memory(),
            settings,
            DEFAULT_APP_SECRETS,
            |graph| Arc::new(graph),
            retry,
        )
    }

    fn build(
        stores: Stores,
        settings: Settings,
        app_secrets: &[&str],
        wrap: impl FnOnce(ScriptedTransport) -> Arc<dyn HttpTransport>,
    ) -> Self {
        Self::build_with_retry(stores, settings, app_secrets, wrap, RetryPolicy::NONE)
    }

    /// On `stores` with `settings`, the service's one clock being `clock`
    /// (roadmap M5k: two replicas sharing a fake clock, a broadcast's
    /// pacer on it), Meta reached through what `wrap` makes of the
    /// scripted transport.
    #[allow(dead_code)] // not every test binary of the service uses it
    pub fn with_clock(
        stores: Stores,
        settings: Settings,
        clock: ManualClock,
        wrap: impl FnOnce(ScriptedTransport) -> Arc<dyn HttpTransport>,
    ) -> Self {
        Self::build_full(
            stores,
            settings,
            DEFAULT_APP_SECRETS,
            wrap,
            RetryPolicy::NONE,
            clock,
            None,
        )
    }

    /// As [`Self::with_clock`], the service's clock being `state_clock`
    /// (a virtual clock over Tokio's paused time, say) and the harness's
    /// own `clock` a handle that moves nothing.
    #[allow(dead_code)] // not every test binary of the service uses it
    pub fn with_state_clock(
        stores: Stores,
        settings: Settings,
        state_clock: Arc<dyn meta_whatsapp_rs::core::clock::Clock>,
        wrap: impl FnOnce(ScriptedTransport) -> Arc<dyn HttpTransport>,
    ) -> Self {
        let clock = ManualClock::new(state_clock.now());
        Self::build_full(
            stores,
            settings,
            DEFAULT_APP_SECRETS,
            wrap,
            RetryPolicy::NONE,
            clock,
            Some(state_clock),
        )
    }

    fn build_with_retry(
        stores: Stores,
        settings: Settings,
        app_secrets: &[&str],
        wrap: impl FnOnce(ScriptedTransport) -> Arc<dyn HttpTransport>,
        retry: RetryPolicy,
    ) -> Self {
        let clock = ManualClock::new(time::OffsetDateTime::now_utc());
        Self::build_full(stores, settings, app_secrets, wrap, retry, clock, None)
    }

    fn build_full(
        stores: Stores,
        settings: Settings,
        app_secrets: &[&str],
        wrap: impl FnOnce(ScriptedTransport) -> Arc<dyn HttpTransport>,
        retry: RetryPolicy,
        clock: ManualClock,
        state_clock: Option<Arc<dyn meta_whatsapp_rs::core::clock::Clock>>,
    ) -> Self {
        let Stores {
            store,
            kv,
            conversations,
            events,
            notifier,
            webhooks,
        } = stores;
        let kv = Arc::new(CountingKv::new(kv));
        let outbox = Arc::new(RecordingEvents::new(events));
        // The test's own handle on the service's vault: the same store and
        // key.
        let vault = TokenVault::new(kv.clone(), vault_keys()).unwrap();
        let graph = ScriptedTransport::new();
        let client = Client::builder()
            .shared_transport(wrap(graph.clone()))
            .retry(retry)
            .build()
            .unwrap();
        let inbound = Inbound::new(
            app_secrets.iter().copied().map(AppSecret::new).collect(),
            VerifyToken::new(VERIFY_TOKEN),
        )
        .unwrap()
        .with_clock(state_clock.unwrap_or_else(|| Arc::new(clock.clone())));
        let backend: Arc<dyn Backend> = Arc::new(HarnessBackend {
            store: store.clone(),
            kv: kv.clone(),
            conversations: conversations.clone(),
            outbox: outbox.clone(),
            notifier,
            webhooks,
            memory: MemoryBackend::new(),
        });
        // The state and its grant, which the internal router is served
        // with (roadmap S7).
        let (state, grant) = AppState::from_backend(
            backend.clone(),
            vault_keys(),
            client,
            inbound,
            Metrics::new(),
            settings,
        )
        .unwrap();
        Self {
            internal: internal_router(&grant),
            public: public_router(&state),
            state,
            grant,
            graph,
            kv,
            vault,
            store,
            conversations,
            outbox,
            clock,
            backend,
        }
    }

    /// Deliver `body` to `POST /webhooks/meta`, signed with [`APP_SECRET`].
    pub async fn webhook(&self, body: &[u8]) -> Reply {
        send(&self.public, signed(body).build()).await
    }

    /// Call the internal listener.
    pub async fn call(&self, call: Call) -> Reply {
        send(&self.internal, call.build()).await
    }

    /// `key`, claimed and completed for `tenant` as a release before
    /// roadmap S6 stored it: under `legacy` (that release's fingerprint of
    /// the request, `Fingerprint::legacy_v1_*`), answered `status` and
    /// `body`.
    pub async fn claimed_before_s6(
        &self,
        tenant: &str,
        key: &str,
        legacy: &meta_whatsapp_server::idempotency::Fingerprint,
        status: u16,
        body: &[u8],
    ) {
        use meta_whatsapp_server::model::{IdempotencyClaim, IdempotencyKey, TenantId};
        let tenant = TenantId::parse(tenant).unwrap();
        let key = IdempotencyKey::parse(key).unwrap();
        let claimed = self
            .store
            .claim_idempotency_key(
                &tenant,
                &key,
                legacy.as_bytes(),
                "a-claim-before-s6",
                std::time::Duration::from_secs(60),
                std::time::Duration::from_secs(3600),
            )
            .await
            .unwrap();
        assert_eq!(claimed, IdempotencyClaim::Claimed);
        assert!(
            self.store
                .complete_idempotency_key(&tenant, &key, "a-claim-before-s6", status, body)
                .await
                .unwrap()
        );
    }

    /// Mint an admin key.
    pub async fn admin_key(&self) -> String {
        let (minted, _) = mint(
            self.store.as_ref(),
            KeyOwner::Admin,
            Vec::new(),
            String::new(),
            None,
        )
        .await
        .unwrap();
        minted.expose_key().to_owned()
    }

    /// Create a tenant.
    pub async fn tenant(&self, id: &str) -> TenantId {
        let id = TenantId::parse(id).unwrap();
        self.store.create_tenant(&id, "").await.unwrap().unwrap();
        id
    }

    /// Mint a key for `tenant` with `scopes`.
    pub async fn tenant_key(&self, tenant: &str, scopes: &[Scope]) -> String {
        let (minted, _) = mint(
            self.store.as_ref(),
            KeyOwner::Tenant(TenantId::parse(tenant).unwrap()),
            scopes.to_vec(),
            String::new(),
            None,
        )
        .await
        .unwrap();
        minted.expose_key().to_owned()
    }

    /// Mint a platform key.
    pub async fn platform_key(&self, allowed: AllowedTenants, scopes: &[Scope]) -> String {
        let (minted, _) = mint(
            self.store.as_ref(),
            KeyOwner::Platform(allowed),
            scopes.to_vec(),
            String::new(),
            None,
        )
        .await
        .unwrap();
        minted.expose_key().to_owned()
    }

    /// Bind `waba` and `numbers` to `tenant`, with `token` in the vault.
    pub async fn connect(&self, tenant: &str, waba: &str, numbers: &[&str], token: &str) {
        let tenant = TenantId::parse(tenant).unwrap();
        let waba = WabaId::new(waba).unwrap();
        let numbers: Vec<PhoneNumberId> = numbers
            .iter()
            .map(|n| PhoneNumberId::new(*n).unwrap())
            .collect();
        self.store
            .bind_waba(&tenant, &waba, &numbers)
            .await
            .unwrap();
        self.vault
            .store(
                &StoredBusinessToken::new(waba, AccessToken::new(token)).phone_number_ids(numbers),
            )
            .await
            .unwrap();
    }
}

/// A `POST /webhooks/meta` of `body`, signed as Meta signs it, with
/// [`APP_SECRET`].
pub fn signed(body: &[u8]) -> Call {
    let signature = meta_whatsapp_rs::webhooks::sign(&AppSecret::new(APP_SECRET), body);
    Call::new(Method::POST, "/webhooks/meta")
        .header("content-type", "application/json")
        .header("x-hub-signature-256", &signature)
        .body(Body::from(body.to_vec()))
}

/// Every scope.
pub const ALL_SCOPES: [Scope; 24] = Scope::ALL;

// ─── The committed document, as a table of calls ─────────────────────────

/// The committed OpenAPI document.
pub const SPEC: &str = include_str!("../../openapi/v1.json");

/// One operation of the committed document.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Operation {
    pub method: Method,
    pub template: String,
    /// It declares the API key (`security`).
    pub keyed: bool,
}

impl Operation {
    /// An admin route (admin key only).
    pub fn admin(&self) -> bool {
        self.template.starts_with("/v1/admin/")
    }

    /// `METHOD template`.
    pub fn label(&self) -> String {
        format!("{} {}", self.method, self.template)
    }
}

/// Every operation of the committed document, in document order. A route
/// served by the internal listener is in it (utoipa-axum's `routes!`
/// documents what it serves), so iterating this covers new routes.
pub fn spec_operations() -> Vec<Operation> {
    let spec: Value = serde_json::from_str(SPEC).unwrap();
    let mut operations = Vec::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            operations.push(Operation {
                method: Method::from_bytes(method.to_uppercase().as_bytes()).unwrap(),
                template: path.clone(),
                keyed: operation.get("security").is_some(),
            });
        }
    }
    operations
}

/// The values a sample call puts in a template's parameters.
#[derive(Debug, Clone)]
pub struct Sample {
    pub tenant: String,
    pub waba: String,
    pub pn: String,
    pub key_id: String,
}

impl Sample {
    /// `template` with these values; any other parameter is `placeholder`.
    pub fn fill(&self, template: &str) -> String {
        let mut out = String::new();
        let mut rest = template;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let end = rest[start..].find('}').unwrap() + start;
            out.push_str(match &rest[start + 1..end] {
                "pn" => &self.pn,
                "waba_id" => &self.waba,
                "id" if template.contains("/templates/") => SAMPLE_TEMPLATE_ID,
                "id" if template.contains("/partners/solutions/") => SAMPLE_SOLUTION_ID,
                "user_id" => SAMPLE_SYSTEM_USER_ID,
                "intent_id" => SAMPLE_SOLUTION_ID,
                "migration_id" => SAMPLE_MIGRATION_ID,
                "id" => &self.tenant,
                "key_id" => &self.key_id,
                "message_id" => SAMPLE_MESSAGE_ID,
                "contact" | "bsuid" => SAMPLE_CONTACT,
                "media_id" => SAMPLE_MEDIA_ID,
                "configuration_name" => SAMPLE_PAYMENT_CONFIGURATION,
                "reference_id" => SAMPLE_REFERENCE_ID,
                "group_id" => SAMPLE_TEMPLATE_GROUP_ID,
                "flow_id" => SAMPLE_FLOW_ID,
                "catalog_id" => SAMPLE_CATALOG_ID,
                "set_id" => SAMPLE_PRODUCT_SET_ID,
                "product_id" => SAMPLE_PRODUCT_ID,
                "template_id" => SAMPLE_TEMPLATE_ID,
                "sequence_id" => SAMPLE_SEQUENCE_ID,
                "business_id" => SAMPLE_BUSINESS_ID,
                "code" => SAMPLE_QR_CODE,
                "call_id" => SAMPLE_CALL_ID,
                "broadcast_id" => SAMPLE_BROADCAST_ID,
                "rule_id" => SAMPLE_RULE_ID,
                _ => "placeholder",
            });
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        out
    }
}

/// A bot rule's id (roadmap M5k, slice 2): the shape of one, never made.
pub const SAMPLE_RULE_ID: &str = "br_00000000000000000000000000000001";

/// A broadcast's id (roadmap M5k): the shape of one, never made.
pub const SAMPLE_BROADCAST_ID: &str = "bc_00000000000000000000000000000001";

/// A Multi-Partner Solution, and a migration intent (roadmap M5j).
pub const SAMPLE_SOLUTION_ID: &str = "795033096057724";

/// A system user (`reference/user/assigned-whatsapp-business-accounts-api`).
pub const SAMPLE_SYSTEM_USER_ID: &str = "1972555232742222";

/// A billing migration (`pricing/change-billing-currency`).
pub const SAMPLE_MIGRATION_ID: &str = "mig_01HZYK3ABCDEF4567890";

/// A received message's id (`messages/mark-message-as-read`).
pub const SAMPLE_MESSAGE_ID: &str = "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBJDQjZCMzlEQUE4OTJBMTE4RTUA";

/// A conversation's contact: a BSUID (`business-scoped-user-ids`).
pub const SAMPLE_CONTACT: &str = "US.13491208655302741918";

/// A media id (`business-phone-numbers/media`).
pub const SAMPLE_MEDIA_ID: &str = "1037543291543636";

/// A template id (`templates/template-management`).
pub const SAMPLE_TEMPLATE_ID: &str = "1407680676729941";

/// A template group's id (`templates/template-groups`).
pub const SAMPLE_TEMPLATE_GROUP_ID: &str = "9020555671393375";

/// A Flow's id (`flows/guides/flowsapi`: the page's ids are placeholders).
pub const SAMPLE_FLOW_ID: &str = "1234567890123456";
/// A welcome message sequence's id (`ctwa/welcome-message-sequences`).
pub const SAMPLE_SEQUENCE_ID: &str = "186473890";
/// A business portfolio's id (`marketing-messages/pricing/enroll-max-price`).
pub const SAMPLE_BUSINESS_ID: &str = "529216684107530";
/// A catalog's id (Meta's Catalog API).
pub const SAMPLE_CATALOG_ID: &str = "194836987003835";
/// A product set's id (Meta's Catalog API: the pages' ids are placeholders).
pub const SAMPLE_PRODUCT_SET_ID: &str = "5000000000000101";
/// A product's id (Meta's Catalog API: the pages' ids are placeholders).
pub const SAMPLE_PRODUCT_ID: &str = "5000000000000001";

/// A base64 public key of the shape `settings-api` asks for (an RSA
/// `SubjectPublicKeyInfo` header): not a private key.
pub const SAMPLE_CLIENT_KEY: &str = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8A";

/// An Official Business Account application with every field
/// (`official-business-accounts`; roadmap M5c3).
pub fn sample_oba_application() -> Value {
    serde_json::json!({
        "business_website_url": "https://www.luckyshrub.example",
        "primary_country_of_operation": "US",
        "primary_language": "en",
        "parent_business_or_brand": "Lucky Shrub Group",
        "supporting_links": [
            "https://news.example/1", "https://news.example/2", "https://news.example/3",
            "https://news.example/4", "https://news.example/5"
        ],
        "additional_supporting_information": "Founded in 1999."
    })
}

/// A compliance information declaration
/// (`business-compliance-information-api`; roadmap M5c3).
pub fn sample_compliance_info() -> Value {
    serde_json::json!({
        "entity_name": "Lucky Shrub Pvt Ltd",
        "entity_type": "PRIVATE_COMPANY",
        "grievance_officer_details": {
            "name": "A. Rao", "email": "grievance@luckyshrub.example",
            "mobile_number": "+919876543210"
        },
        "customer_care_details": {"email": "care@luckyshrub.example"}
    })
}

/// A QR code's id (`qr-codes`, "Create QR code").
pub const SAMPLE_QR_CODE: &str = "4O4YGZEG3RIVE1";

/// A call's id (`calling/user-initiated-calls`, the connect webhook).
pub const SAMPLE_CALL_ID: &str = "wacid.ABGGFjFVU2AfAgo6V-Hc5eCgK5Gh";

/// An SDP the sample calls send: a short WebRTC description (RFC 8866).
pub const SAMPLE_SDP: &str = "v=0\r\no=- 7602563789789945080 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=ice-pwd:SAMPLEICEPWDFORTHETESTS0\r\n";

/// A payment configuration's name (`payments/payments-in/onboarding-apis`).
pub const SAMPLE_PAYMENT_CONFIGURATION: &str = "test-payment-configuration";

/// An order's `reference_id` (`payments/payments-in/pg`).
pub const SAMPLE_REFERENCE_ID: &str = "BM345A-12";

/// The phone number every sample send goes to (`messages/text-messages`).
pub const SAMPLE_TO: &str = "+16505551234";

/// A template definition Meta documents (`templates/template-management`,
/// "Edit template components", as a creation).
pub fn sample_template_definition() -> Value {
    serde_json::json!({
        "name": "spring_sale",
        "language": "en_US",
        "category": "MARKETING",
        "components": [
            {"type": "HEADER", "format": "TEXT", "text": "Our {{1}} is on!",
             "example": {"header_text": ["Spring Sale"]}},
            {"type": "BODY",
             "text": "Shop now through {{1}} and use code {{2}} to get {{3}} off of all merchandise.",
             "example": {"body_text": [["the end of April", "25OFF", "25%"]]}},
            {"type": "FOOTER", "text": "Use the buttons below to manage your marketing subscriptions"},
            {"type": "BUTTONS", "buttons": [
                {"type": "QUICK_REPLY", "text": "Unsubscribe from Promos"},
                {"type": "QUICK_REPLY", "text": "Unsubscribe from All"}
            ]}
        ]
    })
}

/// A creation from the Template Library (`templates/template-library`,
/// "Example request", its button inputs as the array of objects the
/// reference types them; roadmap M5b).
pub fn sample_library_template() -> Value {
    serde_json::json!({
        "name": "my_delivery_update",
        "language": "en_US",
        "category": "UTILITY",
        "library_template_name": "delivery_update_1",
        "library_template_button_inputs": [{"type": "URL", "url": {
            "base_url": "https://www.example.com/{{1}}",
            "url_suffix_example": "https://www.example.com/order_update"}}]
    })
}

/// The query string an operation needs, so that a refusal is never the
/// query's fault.
pub fn sample_query(method: &Method, template: &str) -> Option<String> {
    // Roadmap M5g: analytics ranges are bounded by the look-back, so the
    // sample's are two days to one day before now.
    let range = || {
        let now = time::OffsetDateTime::now_utc();
        let at = |days: i64| {
            (now - time::Duration::days(days))
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap()
                .replace('+', "%2B")
        };
        format!("start={}&end={}", at(2), at(1))
    };
    match (method.as_str(), template) {
        ("DELETE", "/v1/wabas/{waba_id}/templates") => Some("name=order_confirmation".to_owned()),
        // Roadmap M5c1: a chunk names where it starts.
        ("POST", "/v1/numbers/{pn}/media-uploads/{upload_id}") => Some("file_offset=0".to_owned()),
        // templates/template-comparison, "Example request" (roadmap M5b).
        ("GET", "/v1/wabas/{waba_id}/templates/{id}/compare") => {
            Some("template_ids=1533406637136032&start=1674844791182&end=1674845395982".to_owned())
        }
        ("GET", "/v1/wabas/{waba_id}/analytics/messaging") => {
            Some(format!("{}&granularity=DAY", range()))
        }
        (
            "GET",
            "/v1/wabas/{waba_id}/analytics/conversations" | "/v1/wabas/{waba_id}/analytics/pricing",
        ) => Some(format!("{}&granularity=DAILY", range())),
        ("GET", "/v1/wabas/{waba_id}/analytics/templates") => {
            Some(format!("{}&template_ids={SAMPLE_TEMPLATE_ID}", range()))
        }
        // Roadmap M5g2: template group analytics, as the page's example.
        ("GET", "/v1/wabas/{waba_id}/analytics/template-groups") => Some(format!(
            "{}&group_ids={SAMPLE_TEMPLATE_GROUP_ID}&metric_types=sent,delivered,read",
            range()
        )),
        // Roadmap M5f: a group's analytics reach back 90 days at most, so
        // the sample's range is two days to one day before now.
        ("GET", "/v1/numbers/{pn}/groups/{group_id}/analytics") => {
            let now = time::OffsetDateTime::now_utc();
            let at = |days: i64| {
                (now - time::Duration::days(days))
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap()
            };
            Some(format!("start={}&end={}&metrics=SENT", at(2), at(1)))
        }
        // Roadmap M5h: a reach estimate names countries and a history.
        ("GET", "/v1/wabas/{waba_id}/marketing/reach-estimates") => {
            Some("countries=IN,US&date_interval=L7D".to_owned())
        }
        // Roadmap M5e: a user, by phone number (the `+` is encoded), and
        // the calls' ranges as the other analytics'.
        ("GET", "/v1/numbers/{pn}/call-permissions") => Some("phone=%2B16505551234".to_owned()),
        ("GET", "/v1/wabas/{waba_id}/analytics/calls") => {
            Some(format!("{}&granularity=DAILY", range()))
        }
        _ => None,
    }
}

/// A body the operation accepts, so that a refusal is never the body's
/// fault; `{}` for a body-taking operation this table does not know yet
/// (whatever it answers then shows up in the tests iterating the
/// document).
#[allow(clippy::too_many_lines)] // one arm per operation that takes a body
pub fn sample_body(method: &Method, template: &str) -> Option<Value> {
    Some(match (method.as_str(), template) {
        ("POST", "/v1/numbers/{pn}/messages") => serde_json::json!({
            "to": {"phone": SAMPLE_TO},
            "type": "text",
            "text": {"body": "Your order has shipped."}
        }),
        ("POST", "/v1/wabas/{waba_id}/templates") => sample_template_definition(),
        // A template: a reply that needs no open window, so it reaches Meta.
        ("POST", "/v1/numbers/{pn}/conversations/{contact}/messages") => serde_json::json!({
            "type": "template",
            "template": {"name": "order_confirmation", "language": {"code": "en_US"}}
        }),
        ("POST", "/v1/admin/tenants") => serde_json::json!({"id": "sample-new-tenant"}),
        ("PATCH", "/v1/admin/tenants/{id}") => serde_json::json!({"name": "Renamed"}),
        ("POST", "/v1/admin/tenants/{id}/keys") => serde_json::json!({"scopes": ["numbers"]}),
        ("POST", "/v1/admin/platform-keys") => {
            serde_json::json!({"tenants": "*", "scopes": ["numbers"]})
        }
        ("POST", "/v1/admin/tenants/{id}/wabas") => {
            serde_json::json!({"waba_id": "555000111", "token": "SYSTEM-TOKEN-FOR-SAMPLES"})
        }
        ("PATCH", "/v1/numbers/{pn}/profile") => serde_json::json!({"about": "Open 9 to 5"}),
        // display-names, "Update display name via API" (roadmap M5c2).
        ("POST", "/v1/numbers/{pn}/display-name") => {
            serde_json::json!({"new_display_name": "Lucky Shrub"})
        }
        // The Graph guide's file types (roadmap M5c1).
        ("POST", "/v1/numbers/{pn}/media-uploads") => serde_json::json!({
            "file_name": "offer.png", "file_length": 12, "file_type": "image/png"
        }),
        // Roadmap M3c: OTP, its settings and authentication templates.
        ("POST", "/v1/otp/issue") => serde_json::json!({"phone": SAMPLE_TO}),
        ("POST", "/v1/otp/verify") => serde_json::json!({"phone": SAMPLE_TO, "code": "123456"}),
        ("PUT", "/v1/admin/tenants/{id}/otp") => serde_json::json!({
            "phone_number_id": "106540352242922",
            "template": {"name": "login_code", "language": "en_US"}
        }),
        ("POST", "/v1/wabas/{waba_id}/templates/authentication") => serde_json::json!({
            "name": "authentication_code_copy_code_button",
            "languages": ["en_US", "es_ES", "fr"],
            "otp_type": "copy_code",
            "add_security_recommendation": true,
            "code_expiration_minutes": 10
        }),
        // business-phone-numbers/registration's and the request-code
        // reference's examples (roadmap M3d).
        ("POST", "/v1/numbers/{pn}/request-code") => {
            serde_json::json!({"code_method": "SMS", "language": "en_US"})
        }
        ("POST", "/v1/numbers/{pn}/verify-code") => serde_json::json!({"code": "123830"}),
        // A WABA's Flow private key (roadmap M5d2): the test key.
        ("PUT", "/v1/wabas/{waba_id}/flow-key") => {
            serde_json::json!({"private_key": flows::KEY_PEM})
        }
        ("POST", "/v1/numbers/{pn}/register" | "/v1/numbers/{pn}/two-step-pin") => {
            serde_json::json!({"pin": SAMPLE_PIN})
        }
        // payments-in/onboarding-apis, "Create a payment configuration".
        ("POST", "/v1/wabas/{waba_id}/payment-configurations") => serde_json::json!({
            "configuration_name": SAMPLE_PAYMENT_CONFIGURATION,
            "purpose_code": "00",
            "merchant_category_code": "0000",
            "provider_name": "razorpay",
            "redirect_url": "https://test-redirect-url.com"
        }),
        // payments-in/pg, "Step 8: Refunds".
        ("POST", "/v1/numbers/{pn}/payments/refunds") => serde_json::json!({
            "reference_id": SAMPLE_REFERENCE_ID,
            "speed": "normal",
            "payment_config_id": SAMPLE_PAYMENT_CONFIGURATION,
            "amount": {"currency": "INR", "value": "100", "offset": "100"}
        }),
        // Roadmap M5g2: analytics, "Disabling button click analytics".
        ("PUT", "/v1/wabas/{waba_id}/analytics/templates/{template_id}/click-tracking") => {
            serde_json::json!({"opted_out": true})
        }
        // Roadmap M5d: a Flow created from the page's example, and changed
        // (`flows/guides/flowsapi`).
        ("POST", "/v1/wabas/{waba_id}/flows") => {
            serde_json::json!({"name": "My first flow", "categories": ["OTHER"]})
        }
        ("PATCH", "/v1/wabas/{waba_id}/flows/{flow_id}") => {
            serde_json::json!({"name": "New flow name"})
        }
        // Roadmap M5b: each route's example from Meta's page.
        // templates/template-management, "Edit template category".
        ("PATCH", "/v1/wabas/{waba_id}/templates/{id}") => {
            serde_json::json!({"category": "MARKETING"})
        }
        // templates/template-archival, "Request syntax".
        (
            "POST",
            "/v1/wabas/{waba_id}/templates/archive" | "/v1/wabas/{waba_id}/templates/unarchive",
        ) => serde_json::json!({"hsm_ids": [SAMPLE_TEMPLATE_ID]}),
        // templates/template-library, "Example request", its button inputs
        // as the array of objects the reference types them.
        ("POST", "/v1/wabas/{waba_id}/templates/library") => sample_library_template(),
        // templates/template-migration: from the path's own WABA, which
        // every sample's tenant holds (a source the tenant does not hold is
        // `404` before anything is read).
        ("POST", "/v1/wabas/{waba_id}/templates/migrations") => {
            serde_json::json!({"source_waba_id": "102290129340398", "page_number": 0})
        }
        // templates/template-groups, the examples.
        ("POST", "/v1/wabas/{waba_id}/template-groups") => serde_json::json!({
            "name": "Black Friday 2024",
            "description": "US-based Black Friday sale 2024 templates.",
            "whatsapp_business_templates": [278_077_987_957_091_u64, 1_945_418_102_598_215_u64]
        }),
        ("PATCH", "/v1/wabas/{waba_id}/template-groups/{group_id}") => serde_json::json!({
            "add_templates": [844_006_344_365_270_u64, 1_188_657_612_184_382_u64],
            "remove_templates": [1_035_843_174_854_974_u64]
        }),
        // Roadmap M5c3: bodies each route accepts.
        ("PATCH", "/v1/numbers/{pn}/settings") => {
            serde_json::json!({"search_visibility": "HIDDEN"})
        }
        ("PUT", "/v1/numbers/{pn}/settings/payload-encryption") => {
            serde_json::json!({"client_encryption_key": SAMPLE_CLIENT_KEY})
        }
        ("PUT", "/v1/numbers/{pn}/username") => serde_json::json!({"username": "lucky_shrub"}),
        ("POST", "/v1/numbers/{pn}/official-business-account") => sample_oba_application(),
        ("PUT", "/v1/numbers/{pn}/compliance-info") => sample_compliance_info(),
        ("PUT", "/v1/numbers/{pn}/conversational-components") => serde_json::json!({
            "enable_welcome_message": true, "prompts": ["Book a table"],
            "commands": [{"command_name": "book", "command_description": "Book a table"}]
        }),
        // Roadmap M5g: the examples of the pages (`catalogs/set-commerce-settings`,
        // `qr-codes`, `block-users`).
        ("PATCH", "/v1/numbers/{pn}/commerce-settings") => {
            serde_json::json!({"is_cart_enabled": true})
        }
        ("POST", "/v1/numbers/{pn}/qr-codes") => {
            serde_json::json!({"prefilled_message": "Cyber Monday", "generate_qr_image": "SVG"})
        }
        ("PATCH", "/v1/numbers/{pn}/qr-codes/{code}") => {
            serde_json::json!({"prefilled_message": "Cyber Monday 2"})
        }
        (
            "POST",
            "/v1/numbers/{pn}/blocked-users"
            | "/v1/numbers/{pn}/blocked-users/unblock"
            | "/v1/numbers/{pn}/groups/{group_id}/participants/remove",
        ) => serde_json::json!({"users": [{"phone": SAMPLE_TO}]}),
        // Roadmap M5l: a thread action on a user
        // (`conversation-routing/thread-control`).
        (
            "POST",
            "/v1/numbers/{pn}/thread-control/pass"
            | "/v1/numbers/{pn}/thread-control/take"
            | "/v1/numbers/{pn}/thread-control/release",
        ) => serde_json::json!({"user": {"phone": SAMPLE_TO}}),
        // Roadmap M5k, slice 2: the bot's switch, and a rule.
        ("PUT", "/v1/numbers/{pn}/bot") => serde_json::json!({"enabled": false}),
        ("POST", "/v1/numbers/{pn}/bot/rules")
        | ("PUT", "/v1/numbers/{pn}/bot/rules/{rule_id}") => {
            serde_json::json!({
                "trigger": {"type": "command", "names": ["help"]},
                "action": {"type": "reply", "text": "hello"},
            })
        }
        // Roadmap M5k: a broadcast of a template to one person.
        ("POST", "/v1/numbers/{pn}/broadcasts") => serde_json::json!({
            "template": {"name": "spring_sale", "language": {"code": "en_US"}},
            "recipients": [{"phone": SAMPLE_TO}],
        }),
        // Roadmap M5f: a group's creation, a change and the participants
        // and join requests of one (`groups/reference`).
        ("POST", "/v1/numbers/{pn}/groups") => {
            serde_json::json!({"subject": "New Purchase Inquiry"})
        }
        ("PATCH", "/v1/numbers/{pn}/groups/{group_id}") => {
            serde_json::json!({"subject": "Watch Enthusiasts"})
        }
        (
            "POST",
            "/v1/numbers/{pn}/groups/{group_id}/join-requests/approve"
            | "/v1/numbers/{pn}/groups/{group_id}/join-requests/reject",
        ) => serde_json::json!({"join_request_ids": ["MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIw"]}),
        // Roadmap M5h: the examples of the pages
        // (`marketing-messages/send-marketing-messages`,
        // `marketing-messages/pricing`, `ctwa/welcome-message-sequences`,
        // `marketing-messages/pricing/enroll-max-price`).
        ("POST", "/v1/numbers/{pn}/marketing-messages") => serde_json::json!({
            "to": {"phone": SAMPLE_TO},
            "template": {"name": "seasonal_sale_promo", "language": {"code": "en_US"}}
        }),
        ("PUT", "/v1/wabas/{waba_id}/marketing/cloud-api-marketing") => {
            serde_json::json!({"disabled": true})
        }
        ("POST", "/v1/wabas/{waba_id}/marketing/template-duplicates") => serde_json::json!({
            "source_template_id": SAMPLE_TEMPLATE_ID,
            "name": "seasonal_sale_promo_cheaper",
            "optimization_spec": {"bid_amount": 5000}
        }),
        ("PUT", "/v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price") => {
            serde_json::json!({"bid_amount": 5000})
        }
        ("POST", "/v1/wabas/{waba_id}/welcome-sequences") => serde_json::json!({
            "name": "Driver sign-up",
            "welcome_message_sequence": {"text": "Welcome", "ice_breakers": [{"title": "Quick reply 1"}]}
        }),
        ("PATCH", "/v1/wabas/{waba_id}/welcome-sequences/{sequence_id}") => {
            serde_json::json!({"name": "Driver sign-up updated name"})
        }
        ("POST", "/v1/admin/businesses/{business_id}/max-price-agreement") => serde_json::json!({
            "signer_name": "Jane Doe",
            "signer_email": "jane.doe@example.com",
            "token": "EAAA-sample-business-token"
        }),
        // Roadmap M5h2: the agreement's state read carries the token.
        ("POST", "/v1/admin/businesses/{business_id}/max-price-agreement/state") => {
            serde_json::json!({"token": "EAAA-sample-business-token"})
        }
        // Roadmap M5e: the calling settings (`calling/call-settings`), a
        // call to a user, and the answers to a user's call.
        ("PATCH", "/v1/numbers/{pn}/call-settings") => {
            serde_json::json!({"callback_permission_status": "ENABLED"})
        }
        ("POST", "/v1/numbers/{pn}/calls") => {
            serde_json::json!({"to": {"phone": SAMPLE_TO}, "sdp_offer": SAMPLE_SDP})
        }
        (
            "POST",
            "/v1/numbers/{pn}/calls/{call_id}/pre-accept"
            | "/v1/numbers/{pn}/calls/{call_id}/accept",
        ) => serde_json::json!({"sdp_answer": SAMPLE_SDP}),
        // Roadmap M5j: the partner routes' bodies.
        ("POST", "/v1/admin/partners/business-verifications") => serde_json::json!({
            "client_business_id": "2729063490586005",
            "documents": [{"file_name": "registration.png",
                           "content_base64": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB"}]
        }),
        ("POST", "/v1/admin/partners/solutions") => serde_json::json!({
            "name": "Social OVD with Lucky Shrub", "partner_app_id": "21202248997039",
            "owner_permissions": ["MESSAGING"], "partner_permissions": ["MESSAGING"]
        }),
        ("POST", "/v1/wabas/{waba_id}/solution-migration-intents") => {
            serde_json::json!({"solution_id": SAMPLE_SOLUTION_ID})
        }
        ("POST", "/v1/wabas/{waba_id}/billing-migrations") => {
            serde_json::json!({"currency": "INR"})
        }
        ("POST" | "PATCH" | "PUT", _) => serde_json::json!({}),
        _ => return None,
    })
}

/// The PIN every sample registration sends
/// (`business-phone-numbers/registration`'s example).
pub const SAMPLE_PIN: &str = "212834";

/// The file every sample upload sends: a PNG signature.
pub const SAMPLE_PNG: &[u8] = b"\x89PNG\r\n\x1a\n-sample-image";

/// An Ogg page with an Opus identification header (`OggS` ... `OpusHead`)
/// and `filler` after it: what the service reads of a voicemail
/// announcement (roadmap M5e2) and Meta would refuse to decode.
pub fn ogg_opus(filler: &[u8]) -> Vec<u8> {
    let mut out = b"OggS\x00\x02".to_vec();
    out.extend([0u8; 20]);
    out.extend(b"\x01\x13OpusHead\x01\x01\x38\x01\x80\x3e\x00\x00\x00\x00\x00");
    out.extend(filler);
    out
}

/// A JPEG's headers (SOI, JFIF, a start-of-frame of `width` by `height`),
/// `filler` where the scan would be, and the end marker: what the service
/// reads of a group's picture (roadmap M5f2) and Meta would refuse to
/// decode.
pub fn jpeg(width: u16, height: u16, filler: &[u8]) -> Vec<u8> {
    let mut out = vec![
        0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0,
    ];
    out.extend([0xFF, 0xC0, 0, 17, 8]);
    out.extend(height.to_be_bytes());
    out.extend(width.to_be_bytes());
    out.extend([3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    out.extend(filler);
    out.extend([0xFF, 0xD9]);
    out
}

/// A sample call of `operation` with `key` (none for an unkeyed one).
pub fn sample_call(operation: &Operation, sample: &Sample, key: Option<&str>) -> Call {
    let mut path = sample.fill(&operation.template);
    if let Some(query) = sample_query(&operation.method, &operation.template) {
        path = format!("{path}?{query}");
    }
    let mut call = Call::new(operation.method.clone(), path);
    if let Some(key) = key {
        call = call.key(key);
    }
    if (operation.method.as_str(), operation.template.as_str())
        == ("POST", "/v1/numbers/{pn}/media")
    {
        return call.multipart(&[
            ("type", None, b"image/png"),
            ("file", Some("voucher.png"), SAMPLE_PNG),
        ]);
    }
    // A group's picture is a square JPEG of 192 pixels (roadmap M5f2).
    if (operation.method.as_str(), operation.template.as_str())
        == ("PUT", "/v1/numbers/{pn}/groups/{group_id}/picture")
    {
        return call.multipart(&[("file", Some("group.jpg"), &jpeg(192, 192, b"scan"))]);
    }
    // A voicemail announcement is Ogg with Opus (roadmap M5e2).
    if (operation.method.as_str(), operation.template.as_str())
        == (
            "POST",
            "/v1/numbers/{pn}/call-settings/voicemail-announcement",
        )
    {
        return call.multipart(&[("file", Some("announcement.ogg"), &ogg_opus(b"audio"))]);
    }
    // A deregistration confirms the path's number (roadmap M3d).
    if (operation.method.as_str(), operation.template.as_str())
        == ("POST", "/v1/numbers/{pn}/deregister")
    {
        return call.json(&serde_json::json!({"confirm": sample.pn}));
    }
    // An erasure confirms the path's number and names a contact (roadmap
    // M2f).
    if (operation.method.as_str(), operation.template.as_str())
        == ("POST", "/v1/numbers/{pn}/erasures")
    {
        return call.json(&serde_json::json!({"confirm": sample.pn, "contacts": [SAMPLE_CONTACT]}));
    }
    if let Some(body) = sample_body(&operation.method, &operation.template) {
        call = call.json(&body);
    }
    // A refund requires an `Idempotency-Key` (design D56): a fresh one per
    // call, so that no call replays another's answer.
    if (operation.method.as_str(), operation.template.as_str())
        == ("POST", "/v1/numbers/{pn}/payments/refunds")
    {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        call = call.header("idempotency-key", &format!("sample-refund-{n}"));
    }
    call
}

// ─── Live Postgres ───────────────────────────────────────────────────────

/// `META_WHATSAPP_RS_TEST_POSTGRES_URL`, or `None` (after saying so) when it
/// is unset.
///
/// # Panics
///
/// When it is unset and `META_WHATSAPP_RS_REQUIRE_LIVE=1`: a skipped live
/// test must not pass as a green one in `just test-live`.
pub fn postgres_url() -> Option<String> {
    const VAR: &str = "META_WHATSAPP_RS_TEST_POSTGRES_URL";
    match std::env::var(VAR) {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ if std::env::var("META_WHATSAPP_RS_REQUIRE_LIVE").is_ok_and(|v| v == "1") => {
            panic!(
                "{VAR} is not set, and META_WHATSAPP_RS_REQUIRE_LIVE=1 turns a skipped live test into a failure"
            )
        }
        _ => {
            eprintln!(
                "skipping: {VAR} is not set (META_WHATSAPP_RS_REQUIRE_LIVE=1 makes this a failure)"
            );
            None
        }
    }
}

/// A lowercase `[a-z0-9_]` token unique to this call.
pub fn unique() -> String {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    format!("{t:x}_{}_{n}", std::process::id())
}

/// A fresh schema on the test database, and a way to open pools on it
/// (each pool is another "replica"). Dropping it drops the schema, when the
/// test panics too, so a failed assertion leaves nothing on the shared test
/// server.
pub struct TestDb {
    pub url: String,
    pub schema: String,
}

impl TestDb {
    /// A new schema, or `None` when no database is configured.
    pub async fn new() -> Option<Self> {
        use meta_whatsapp_rs::adapters::store::postgres::sqlx;
        let url = postgres_url()?;
        // Before the `CREATE`: `Drop` says `IF EXISTS`, so a `CREATE` that
        // failed (or whose reply was lost) is covered too.
        let db = Self {
            url,
            schema: format!("wa_server_test_{}", unique()),
        };
        let admin = sqlx::PgPool::connect(&db.url)
            .await
            .expect("connect to META_WHATSAPP_RS_TEST_POSTGRES_URL");
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {}", db.schema)))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
        Some(db)
    }

    /// `DROP SCHEMA … CASCADE` on a connection of its own, in a runtime of
    /// its own: `Drop` cannot await, and the test's runtime may be the one
    /// unwinding (or, in `binary.rs`, not running at all).
    fn drop_schema(url: &str, schema: &str) -> Result<(), String> {
        use std::str::FromStr;

        use meta_whatsapp_rs::adapters::store::postgres::sqlx;
        use sqlx::{ConnectOptions, Connection};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            // A transaction a panicking test (or a service it started) left
            // open must not hang the drop.
            let mut conn = sqlx::postgres::PgConnectOptions::from_str(url)
                .map_err(|e| e.to_string())?
                .options([("lock_timeout", "10s")])
                .connect()
                .await
                .map_err(|e| e.to_string())?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE"
            )))
            .execute(&mut conn)
            .await
            .map_err(|e| e.to_string())?;
            conn.close().await.map_err(|e| e.to_string())
        })
    }

    /// A pool whose connections use the schema and are named `app` in
    /// `pg_stat_activity`, so a test can see its own sessions wait on a
    /// lock while other tests run on the same server.
    pub async fn named_pool(
        &self,
        size: u32,
        app: &str,
    ) -> meta_whatsapp_rs::adapters::store::postgres::sqlx::PgPool {
        use std::str::FromStr;

        use meta_whatsapp_rs::adapters::store::postgres::sqlx::postgres::{
            PgConnectOptions, PgPoolOptions,
        };
        let options = PgConnectOptions::from_str(&self.url)
            .unwrap()
            .application_name(app)
            .options([("search_path", self.schema.as_str())]);
        PgPoolOptions::new()
            .max_connections(size)
            .connect_with(options)
            .await
            .unwrap()
    }

    /// A pool whose connections use the schema.
    pub async fn pool(
        &self,
        size: u32,
    ) -> meta_whatsapp_rs::adapters::store::postgres::sqlx::PgPool {
        use std::str::FromStr;

        use meta_whatsapp_rs::adapters::store::postgres::sqlx::postgres::{
            PgConnectOptions, PgPoolOptions,
        };
        let options = PgConnectOptions::from_str(&self.url)
            .unwrap()
            .options([("search_path", self.schema.as_str())]);
        PgPoolOptions::new()
            .max_connections(size)
            .connect_with(options)
            .await
            .unwrap()
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let (url, schema) = (self.url.clone(), self.schema.clone());
        let result = std::thread::spawn(move || Self::drop_schema(&url, &schema))
            .join()
            .unwrap_or_else(|_| Err("the cleanup thread panicked".to_owned()));
        if let Err(e) = result {
            // A second panic while unwinding would abort the test binary and
            // hide the first one: report it instead.
            if std::thread::panicking() {
                eprintln!("cleanup: dropping schema {} failed: {e}", self.schema);
            } else {
                panic!("cleanup: dropping schema {} failed: {e}", self.schema);
            }
        }
    }
}
