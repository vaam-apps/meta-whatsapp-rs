//! Shared by the integration tests: the service on a memory store (or any
//! store), a vault whose reads are counted, and Graph scripted with
//! `ScriptedTransport`.
#![allow(dead_code)] // each test binary uses a different subset

pub mod backend_suite;
pub mod capture;
pub mod events_suite;
pub mod meta;
pub mod scenarios;
pub mod store_suite;

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
use meta_whatsapp_server::state::{AppState, Settings};
use meta_whatsapp_server::store::events::{EventPage, EventQuery, NewEvent};
use meta_whatsapp_server::store::{MemoryStore, Outbox as EventStore, Store};
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

/// A `KvStore` that counts reads of the token vault's namespace.
#[derive(Debug)]
pub struct CountingKv {
    inner: Arc<dyn KvStore>,
    vault_reads: AtomicUsize,
}

impl CountingKv {
    /// Count reads of `inner`.
    pub fn new(inner: Arc<dyn KvStore>) -> Self {
        Self {
            inner,
            vault_reads: AtomicUsize::new(0),
        }
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
        self.inner.get(key).await
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.inner.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.inner.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.inner
            .compare_and_swap(key, expected, new, expiry)
            .await
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
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
        }
        let before = self.before.lock().unwrap().take();
        if let Some(work) = before {
            work().await;
        }
        let outcome = self.inner.insert(event).await?;
        self.inserted.lock().unwrap().push((event.clone(), outcome));
        Ok(outcome)
    }

    async fn page(&self, query: &EventQuery) -> Result<EventPage, StorageError> {
        self.inner.page(query).await
    }

    async fn purge(&self, older_than: std::time::Duration) -> Result<Option<u64>, StorageError> {
        self.inner.purge(older_than).await
    }
}

/// The service under test.
pub struct Harness {
    pub state: AppState,
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
}

/// The stores a [`Harness`] runs on.
pub struct Stores {
    pub store: Arc<dyn Store>,
    pub kv: Arc<dyn KvStore>,
    pub conversations: Arc<dyn ConversationStore>,
    pub events: Arc<dyn EventStore>,
}

impl Stores {
    /// Everything in memory.
    pub fn memory() -> Self {
        let store = MemoryStore::new();
        Self {
            events: store.outbox(),
            store: Arc::new(store),
            kv: Arc::new(MemoryKvStore::new()),
            conversations: Arc::new(MemoryConversationStore::new()),
        }
    }

    /// Everything on Postgres, on `pool` (migrated).
    pub fn postgres(pool: &meta_whatsapp_rs::adapters::store::postgres::sqlx::PgPool) -> Self {
        use meta_whatsapp_rs::adapters::store::{PostgresConversationStore, PostgresKvStore};
        use meta_whatsapp_server::store::{PgEventStore, PgStore};
        Self {
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
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
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
    }
}

/// The settings of a test service: the defaults, without rate limits.
pub fn test_settings() -> Settings {
    Settings {
        rate_limits: unlimited(),
        ..Settings::default()
    }
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

    fn build(
        stores: Stores,
        settings: Settings,
        app_secrets: &[&str],
        wrap: impl FnOnce(ScriptedTransport) -> Arc<dyn HttpTransport>,
    ) -> Self {
        let Stores {
            store,
            kv,
            conversations,
            events,
        } = stores;
        let kv = Arc::new(CountingKv::new(kv));
        let outbox = Arc::new(RecordingEvents::new(events));
        let vault = TokenVault::new(
            kv.clone(),
            VaultKeys::new(VaultKey::generate("test").unwrap()),
        )
        .unwrap();
        let graph = ScriptedTransport::new();
        let client = Client::builder()
            .shared_transport(wrap(graph.clone()))
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let clock = ManualClock::new(time::OffsetDateTime::now_utc());
        let inbound = Inbound::new(
            app_secrets.iter().copied().map(AppSecret::new).collect(),
            kv.clone(),
            conversations.clone(),
            outbox.clone(),
        )
        .unwrap()
        .with_clock(Arc::new(clock.clone()));
        let state = AppState::with_settings(
            store.clone(),
            vault.clone(),
            client,
            VerifyToken::new(VERIFY_TOKEN),
            Metrics::new(),
            inbound,
            settings,
        )
        .unwrap();
        Self {
            internal: internal_router(&state),
            public: public_router(&state),
            state,
            graph,
            kv,
            vault,
            store,
            conversations,
            outbox,
            clock,
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
        let waba = WabaId::new(waba);
        let numbers: Vec<PhoneNumberId> = numbers.iter().map(|n| PhoneNumberId::new(*n)).collect();
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
pub const ALL_SCOPES: [Scope; 9] = Scope::ALL;

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
                "id" => &self.tenant,
                "key_id" => &self.key_id,
                "message_id" => SAMPLE_MESSAGE_ID,
                "media_id" => SAMPLE_MEDIA_ID,
                _ => "placeholder",
            });
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        out
    }
}

/// A received message's id (`messages/mark-message-as-read`).
pub const SAMPLE_MESSAGE_ID: &str = "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBJDQjZCMzlEQUE4OTJBMTE4RTUA";

/// A media id (`business-phone-numbers/media`).
pub const SAMPLE_MEDIA_ID: &str = "1037543291543636";

/// A template id (`templates/template-management`).
pub const SAMPLE_TEMPLATE_ID: &str = "1407680676729941";

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

/// The query string an operation needs, so that a refusal is never the
/// query's fault.
pub fn sample_query(method: &Method, template: &str) -> Option<&'static str> {
    match (method.as_str(), template) {
        ("DELETE", "/v1/wabas/{waba_id}/templates") => Some("name=order_confirmation"),
        _ => None,
    }
}

/// A body the operation accepts, so that a refusal is never the body's
/// fault; `{}` for a body-taking operation this table does not know yet
/// (whatever it answers then shows up in the tests iterating the
/// document).
pub fn sample_body(method: &Method, template: &str) -> Option<Value> {
    Some(match (method.as_str(), template) {
        ("POST", "/v1/numbers/{pn}/messages") => serde_json::json!({
            "to": {"phone": SAMPLE_TO},
            "type": "text",
            "text": {"body": "Your order has shipped."}
        }),
        ("POST", "/v1/wabas/{waba_id}/templates") => sample_template_definition(),
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
        ("POST" | "PATCH" | "PUT", _) => serde_json::json!({}),
        _ => return None,
    })
}

/// The file every sample upload sends: a PNG signature.
pub const SAMPLE_PNG: &[u8] = b"\x89PNG\r\n\x1a\n-sample-image";

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
    if let Some(body) = sample_body(&operation.method, &operation.template) {
        call = call.json(&body);
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
