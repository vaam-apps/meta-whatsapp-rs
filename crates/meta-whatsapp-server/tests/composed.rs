//! Roadmap S4's decisive case: the service is composed from a `Backend`
//! alone, so one defined outside the server crate (here, memory's ports
//! in a struct of this test's) is served by `serve::serve_until` as the
//! in-tree ones are, with nothing of the server changed for it. Over real
//! listeners: the API reads and writes its records, the token vault is on
//! its key/value store, Meta's webhook goes
//! through its key/value store (dedup), its conversation store (the
//! inbox) and its outbox, housekeeping sweeps through its janitor, it is
//! migrated at start through its migrator, and closed at shutdown. The
//! other half, that a state cannot be built from loose ports, is the
//! compile-fail case `tests/visibility/fail/app_state_from_loose_ports.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use common::APP_SECRET;
use common::meta::{EXAMPLE_PN, EXAMPLE_WABA, example_text};
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::{AccessToken, AppSecret};
use meta_whatsapp_rs::core::store::{
    ConversationKey, ConversationStore, Expiry, KvStore, StoreKey, Versioned,
};
use meta_whatsapp_server::api::admin::mint;
use meta_whatsapp_server::config::{Config, Env, NotUnicode};
use meta_whatsapp_server::model::{KeyOwner, Scope, TenantId};
use meta_whatsapp_server::serve::{serve_until, serve_with};
use meta_whatsapp_server::store::events::EventQuery;
use meta_whatsapp_server::store::{
    Backend, BackendKind, IdempotencyRecords, Janitor, LeaderLock, MemoryBackend, Outbox,
    RecordStore, SchemaMigrator, StoreResult,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Counts what it is asked to do.
#[derive(Debug, Default)]
struct Counter(AtomicUsize);

impl Counter {
    fn bump(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Janitor for Counter {
    async fn purge_expired(&self) -> StoreResult<u64> {
        self.bump();
        Ok(0)
    }
}

#[async_trait]
impl SchemaMigrator for Counter {
    async fn migrate(&self) -> StoreResult<()> {
        self.bump();
        Ok(())
    }
}

/// A key/value store counting its writes.
#[derive(Debug)]
struct CountedKv {
    inner: Arc<dyn KvStore>,
    writes: Counter,
}

#[async_trait]
impl KvStore for CountedKv {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        self.inner.get(key).await
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.writes.bump();
        self.inner.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.writes.bump();
        self.inner.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.writes.bump();
        self.inner
            .compare_and_swap(key, expected, new, expiry)
            .await
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.writes.bump();
        self.inner.delete(key).await
    }
}

/// An integrator's backend: memory's ports, in a struct the server crate
/// has never seen, counting what the service asks of it.
struct Integrators {
    memory: MemoryBackend,
    kv: Arc<CountedKv>,
    janitor: Arc<Counter>,
    migrator: Arc<Counter>,
    closed: AtomicBool,
}

impl Integrators {
    fn new() -> Self {
        let memory = MemoryBackend::new();
        Self {
            kv: Arc::new(CountedKv {
                inner: memory.kv(),
                writes: Counter::default(),
            }),
            memory,
            janitor: Arc::new(Counter::default()),
            migrator: Arc::new(Counter::default()),
            closed: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl Backend for Integrators {
    fn kind(&self) -> BackendKind {
        BackendKind::Other("integrator's")
    }
    fn records(&self) -> Arc<dyn RecordStore> {
        self.memory.records()
    }
    fn idempotency(&self) -> Arc<dyn IdempotencyRecords> {
        self.memory.idempotency()
    }
    fn outbox(&self) -> Arc<dyn Outbox> {
        self.memory.outbox()
    }
    fn leader_lock(&self) -> Arc<dyn LeaderLock> {
        self.memory.leader_lock()
    }
    fn janitor(&self) -> Arc<dyn Janitor> {
        self.janitor.clone()
    }
    fn migrator(&self) -> Arc<dyn SchemaMigrator> {
        self.migrator.clone()
    }
    fn kv(&self) -> Arc<dyn KvStore> {
        self.kv.clone()
    }
    fn conversations(&self) -> Arc<dyn ConversationStore> {
        self.memory.conversations()
    }
    async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// The service's active vault key (`k1`), and the previous one (`k0`) a
/// token was stored under before it started: 32 bytes each, in base64.
const ACTIVE_VAULT_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
const PREVIOUS_VAULT_KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

/// The variables of a development configuration, listening on `public`
/// and `internal`, with the vault keys above. It names memory storage,
/// which `serve_until` does not read: the backend it is given is the
/// storage.
struct Vars(HashMap<&'static str, String>);

impl Vars {
    fn development(public: SocketAddr, internal: SocketAddr) -> Self {
        Self(HashMap::from([
            ("WA_SERVER_ENV", "development".to_owned()),
            ("WA_APP_SECRET", APP_SECRET.to_owned()),
            ("WA_VERIFY_TOKEN", common::VERIFY_TOKEN.to_owned()),
            ("WA_SERVER_PUBLIC_BIND", public.to_string()),
            ("WA_SERVER_INTERNAL_BIND", internal.to_string()),
            ("WA_VAULT_KEY", ACTIVE_VAULT_KEY.to_owned()),
            ("WA_VAULT_PREVIOUS_KEYS", format!("k0:{PREVIOUS_VAULT_KEY}")),
        ]))
    }
}

impl Env for Vars {
    fn var(&self, name: &str) -> Result<Option<String>, NotUnicode> {
        Ok(self.0.get(name).cloned())
    }
    fn read_file(&self, _: &str) -> std::io::Result<String> {
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    }
}

/// The ports of this file's tests, held by each for its whole run.
/// [`two_ports`] frees the two it finds for `serve_until`, and until it
/// binds them they are free for any bind on port 0: without this lock
/// the other test's `two_ports`, run at once, could be handed one, and
/// the test whose service lost the bind would talk to the other's (as
/// `tests/binary.rs` did on PR #47's CI). Held to the end, it also keeps
/// a stopped service's ports from being handed out before the test
/// checks they refuse.
static PORTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Two free local ports, and [`PORTS`]: keep it to the end of the test.
async fn two_ports() -> (SocketAddr, SocketAddr, tokio::sync::MutexGuard<'static, ()>) {
    let held = PORTS.lock().await;
    let a = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let b = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    (a.local_addr().unwrap(), b.local_addr().unwrap(), held)
}

/// The task running `serve_until`.
type Served = tokio::task::JoinHandle<anyhow::Result<()>>;

/// Fail the test with `served`'s error if it has stopped: what answers on
/// its ports is then not this test's service (one that could not bind
/// them, say).
async fn assert_serving(served: &mut Served) {
    if served.is_finished() {
        let ended = served.await;
        panic!("serve_until stopped before its test was done: {ended:?}");
    }
}

/// `method path` over plain HTTP/1.1: the status and the body, or `None`
/// when nothing listens.
async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Option<(u16, String)> {
    let mut stream = tokio::net::TcpStream::connect(addr).await.ok()?;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        write!(head, "{name}: {value}\r\n").unwrap();
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await.ok()?;
    stream.write_all(body).await.ok()?;
    let mut answer = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut answer))
        .await
        .ok()?
        .ok()?;
    let answer = String::from_utf8_lossy(&answer).into_owned();
    let status = answer.get(9..12)?.parse().ok()?;
    let body = answer
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())?;
    Some((status, body))
}

/// Wait until `addr` answers `GET /livez`, from `served`.
async fn live(addr: SocketAddr, served: &mut Served) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert_serving(served).await;
        if http(addr, "GET", "/livez", &[], b"").await.map(|(s, _)| s) == Some(200) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "never served");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_serving(served).await;
}

/// Roadmap S4, decisive: a `Backend` defined here, outside the server
/// crate, is served over real listeners with no change to the server: the
/// state, the webhook pipeline, housekeeping, the migrations and the
/// shutdown all go through it, and through nothing else. Decisive:
/// `serve_with`/`serve_until` taking `Arc<dyn Backend>` (a concrete
/// backend type does not compile here), and every store of the service
/// taken from the bundle (one reached past it, a memory store of the
/// server's own, say, fails the assertion on it).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn the_api_is_served_over_a_backend_defined_outside_the_server_crate() {
    let backend = Arc::new(Integrators::new());
    // Written before serving, through the bundle only: the service finds
    // them there.
    let records = backend.records();
    let tenant = TenantId::parse("merchant-a").unwrap();
    records.create_tenant(&tenant, "").await.unwrap().unwrap();
    records
        .bind_waba(
            &tenant,
            &WabaId::new(EXAMPLE_WABA).unwrap(),
            &[PhoneNumberId::new(EXAMPLE_PN).unwrap()],
        )
        .await
        .unwrap();
    let (admin, _) = mint(
        records.as_ref(),
        KeyOwner::Admin,
        Vec::new(),
        String::new(),
        None,
    )
    .await
    .unwrap();
    let (events_key, _) = mint(
        records.as_ref(),
        KeyOwner::Tenant(tenant.clone()),
        vec![Scope::Events],
        String::new(),
        None,
    )
    .await
    .unwrap();
    // The WABA's token, in a vault on the bundle's key/value store, under
    // the previous key.
    let key = |id: &str, base64: &str| VaultKey::from_base64(id, base64).unwrap();
    TokenVault::new(backend.kv(), VaultKeys::new(key("k0", PREVIOUS_VAULT_KEY)))
        .unwrap()
        .store(
            &StoredBusinessToken::new(
                WabaId::new(EXAMPLE_WABA).unwrap(),
                AccessToken::new("TOKEN-OF-A"),
            )
            .phone_number_ids(vec![PhoneNumberId::new(EXAMPLE_PN).unwrap()]),
        )
        .await
        .unwrap();
    // Meta's example, dated a second after the binding's.
    let delivery = example_text();

    let (public, internal, _held) = two_ports().await;
    let config = Config::from_env(&Vars::development(public, internal)).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let mut served = tokio::spawn(serve_until(config, backend.clone(), async move {
        let _ = stopped.await;
    }));
    live(internal, &mut served).await;
    assert_eq!(backend.migrator.count(), 1, "migrated through the bundle");

    // The API reads the bundle's records, and writes them.
    let admin_auth = format!("Bearer {}", admin.expose_key());
    let (status, listed) = http(
        internal,
        "GET",
        "/v1/admin/tenants",
        &[("authorization", &admin_auth)],
        b"",
    )
    .await
    .unwrap();
    assert_eq!(status, 200, "{listed}");
    assert!(listed.contains("merchant-a"), "{listed}");
    let (status, created) = http(
        internal,
        "POST",
        "/v1/admin/tenants",
        &[
            ("authorization", &admin_auth),
            ("content-type", "application/json"),
        ],
        br#"{"id":"merchant-b"}"#,
    )
    .await
    .unwrap();
    assert_eq!(status, 201, "{created}");
    assert!(
        records
            .tenant(&TenantId::parse("merchant-b").unwrap())
            .await
            .unwrap()
            .is_some(),
        "the API wrote another store's records"
    );

    // The vault is on the bundle's key/value store: rotating re-encrypts
    // the token stored there, which then opens with the active key alone.
    let (status, report) = http(
        internal,
        "POST",
        "/v1/admin/vault/rotate",
        &[("authorization", &admin_auth)],
        b"",
    )
    .await
    .unwrap();
    assert_eq!(status, 200, "{report}");
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(
        report,
        serde_json::json!({"wabas": 1, "rotated": 1, "failed": []}),
        "the vault is another store's"
    );
    let token = TokenVault::new(backend.kv(), VaultKeys::new(key("k1", ACTIVE_VAULT_KEY)))
        .unwrap()
        .get(&WabaId::new(EXAMPLE_WABA).unwrap())
        .await
        .unwrap()
        .expect("re-encrypted in the bundle's key/value store");
    assert_eq!(token.token.expose_secret(), "TOKEN-OF-A");

    // Meta's webhook: deduplicated in the bundle's key/value store, into
    // its inbox and its outbox.
    let signature = meta_whatsapp_rs::webhooks::sign(&AppSecret::new(APP_SECRET), &delivery);
    let kv_writes = backend.kv.writes.count();
    for _ in 0..2 {
        let (status, answer) = http(
            public,
            "POST",
            "/webhooks/meta",
            &[
                ("content-type", "application/json"),
                ("x-hub-signature-256", &signature),
            ],
            &delivery,
        )
        .await
        .unwrap();
        assert_eq!(status, 200, "{answer}");
    }
    assert!(
        backend.kv.writes.count() > kv_writes,
        "the dedup lease was not taken in the bundle's key/value store"
    );
    let inbox = backend
        .conversations()
        .messages(
            &ConversationKey::new(PhoneNumberId::new(EXAMPLE_PN).unwrap(), "16505551234"),
            None,
            10,
        )
        .await
        .unwrap();
    assert_eq!(inbox.len(), 1, "the inbox is another store's");
    let stored = backend
        .outbox()
        .page(&EventQuery {
            tenant: tenant.clone(),
            after: None,
            types: None,
            phone_number_id: None,
            limit: 10,
            max_bytes: meta_whatsapp_server::events::MAX_PAGE_DATA_BYTES,
        })
        .await
        .unwrap();
    assert_eq!(stored.events.len(), 1, "the outbox is another store's");
    let (status, polled) = http(
        internal,
        "GET",
        "/v1/events",
        &[(
            "authorization",
            &format!("Bearer {}", events_key.expose_key()),
        )],
        b"",
    )
    .await
    .unwrap();
    assert_eq!(status, 200, "{polled}");
    assert!(polled.contains("message_received"), "{polled}");

    // Housekeeping's first round runs at once, through the bundle's
    // janitor.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while backend.janitor.count() == 0 {
        assert!(tokio::time::Instant::now() < deadline, "never swept");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Closed once the listeners stopped.
    assert!(!backend.closed.load(Ordering::SeqCst));
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), served)
        .await
        .expect("stopped on the signal")
        .unwrap()
        .unwrap();
    assert!(backend.closed.load(Ordering::SeqCst), "never closed");
    assert!(http(internal, "GET", "/livez", &[], b"").await.is_none());
}

/// `WA_SERVER_MIGRATE=skip` is read from the configuration `serve_until`
/// is given: the backend is served unmigrated (an operator migrates it
/// first, `meta-whatsapp-server migrate`), and still closed at shutdown.
/// Decisive: migrating whatever the setting says.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_backend_is_not_migrated_when_the_configuration_skips_it() {
    let backend = Arc::new(Integrators::new());
    let (public, internal, _held) = two_ports().await;
    let mut vars = Vars::development(public, internal);
    vars.0.insert("WA_SERVER_MIGRATE", "skip".to_owned());
    let config = Config::from_env(&vars).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let mut served = tokio::spawn(serve_until(config, backend.clone(), async move {
        let _ = stopped.await;
    }));
    live(internal, &mut served).await;
    assert_eq!(backend.migrator.count(), 0, "migrated against the setting");
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), served)
        .await
        .expect("stopped on the signal")
        .unwrap()
        .unwrap();
    assert!(backend.closed.load(Ordering::SeqCst), "never closed");
}

/// `serve_with`, the same as `serve_until` on the process's signals,
/// takes an integrator's backend too: this compiles only while it takes
/// an `Arc<dyn Backend>` (it is not run: it would serve until `SIGTERM`).
#[allow(dead_code)]
fn serve_with_takes_a_backend_of_an_integrators(config: Config) {
    drop(serve_with(config, Arc::new(Integrators::new())));
}
