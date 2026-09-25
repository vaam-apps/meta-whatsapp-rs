//! Shared by the integration tests: the service on a memory store (or any
//! store), a vault whose reads are counted, and Graph scripted with
//! `ScriptedTransport`.
#![allow(dead_code)] // each test binary uses a different subset

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
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::{AccessToken, AppSecret, VerifyToken};
use meta_whatsapp_rs::core::store::{ConversationStore, Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_rs::webhooks::axum::Router;
use meta_whatsapp_rs::webhooks::axum::body::Body;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderMap, Method, Request, StatusCode, header};
use meta_whatsapp_rs::{Client, RetryPolicy};
use meta_whatsapp_server::api::admin::mint;
use meta_whatsapp_server::api::{internal_router, public_router};
use meta_whatsapp_server::events::Inbound;
use meta_whatsapp_server::metrics::Metrics;
use meta_whatsapp_server::model::{AllowedTenants, KeyOwner, Scope, TenantId};
use meta_whatsapp_server::state::AppState;
use meta_whatsapp_server::store::events::{EventPage, EventQuery, NewEvent};
use meta_whatsapp_server::store::{EventStore, MemoryStore, Store};
use serde_json::Value;
use tower::ServiceExt;

/// The verify token of every test service.
pub const VERIFY_TOKEN: &str = "verify-token-for-tests";

/// The app secret Meta's test deliveries are signed with.
pub const APP_SECRET: &str = "app-secret-for-tests";

/// A second app secret, the previous one while rotating.
pub const PREVIOUS_APP_SECRET: &str = "previous-app-secret-for-tests";

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

impl Harness {
    /// On a memory store.
    pub fn new() -> Self {
        Self::with(Stores::memory())
    }

    /// On `store` and `kv`, the inbox and the outbox in memory.
    pub fn on(store: Arc<dyn Store>, kv: Arc<dyn KvStore>) -> Self {
        Self::with(Stores {
            store,
            kv,
            ..Stores::memory()
        })
    }

    /// On `stores`.
    pub fn with(stores: Stores) -> Self {
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
            .transport(graph.clone())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let inbound = Inbound::new(
            vec![
                AppSecret::new(APP_SECRET),
                AppSecret::new(PREVIOUS_APP_SECRET),
            ],
            kv.clone(),
            conversations.clone(),
            outbox.clone(),
        )
        .unwrap();
        let state = AppState::new(
            store.clone(),
            vault.clone(),
            client,
            VerifyToken::new(VERIFY_TOKEN),
            Metrics::new(),
            inbound,
        );
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
                "id" => &self.tenant,
                "key_id" => &self.key_id,
                _ => "placeholder",
            });
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        out
    }
}

/// A body the operation accepts, so that a refusal is never the body's
/// fault; `{}` for a body-taking operation this table does not know yet
/// (whatever it answers then shows up in the tests iterating the
/// document).
pub fn sample_body(method: &Method, template: &str) -> Option<Value> {
    Some(match (method.as_str(), template) {
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

/// A sample call of `operation` with `key` (none for an unkeyed one).
pub fn sample_call(operation: &Operation, sample: &Sample, key: Option<&str>) -> Call {
    let mut call = Call::new(operation.method.clone(), sample.fill(&operation.template));
    if let Some(key) = key {
        call = call.key(key);
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
/// (each pool is another "replica").
pub struct TestDb {
    pub url: String,
    pub schema: String,
}

impl TestDb {
    /// A new schema, or `None` when no database is configured.
    pub async fn new() -> Option<Self> {
        use meta_whatsapp_rs::adapters::store::postgres::sqlx;
        let url = postgres_url()?;
        let schema = format!("wa_server_test_{}", unique());
        let admin = sqlx::PgPool::connect(&url)
            .await
            .expect("connect to META_WHATSAPP_RS_TEST_POSTGRES_URL");
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
        Some(Self { url, schema })
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
