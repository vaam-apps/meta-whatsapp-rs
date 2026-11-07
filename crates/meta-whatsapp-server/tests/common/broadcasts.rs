//! What the broadcast tests share (roadmap M5k): the world they run in, the
//! wire to "Meta" that times and stalls sends, and the requests they make.
#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    clippy::unwrap_used,
    clippy::expect_used
)] // not every test binary uses every item

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use meta_whatsapp_rs::bot::Timer;
use meta_whatsapp_rs::core::clock::{Clock, ManualClock};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_rs::core::transport::{HttpRequest, HttpResponse, HttpTransport};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::broadcasts::BroadcastSettings;
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::{Call, Harness, Stores};

pub const TENANT: &str = "merchant-42";
pub const WABA: &str = "102290129340398";
pub const PN: &str = "106540352242922";
pub const PN2: &str = "106540352242923";
pub const TOKEN: &str = "EAAG-merchant-token";
pub const OTHER_TENANT: &str = "merchant-43";
pub const OTHER_WABA: &str = "102290129340399";
pub const OTHER_PN: &str = "106540352242924";
pub const BSUID: &str = "US.13491208655302741918";

// ─── The wire: Meta, timed, and able to stall ────────────────────────────

/// What a test does to the sends that reach "Meta".
#[derive(Debug)]
pub struct Controls {
    pub clock: Arc<dyn Clock>,
    /// The instant each send reached Meta, from every replica.
    pub times: Mutex<Vec<OffsetDateTime>>,
    /// How many sends reached the wire (held ones included).
    pub seen: AtomicUsize,
    /// The sends after this many wait for [`Self::open`].
    pub hold_after: AtomicUsize,
    pub open: tokio::sync::watch::Sender<bool>,
}

impl Controls {
    pub fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            times: Mutex::new(Vec::new()),
            seen: AtomicUsize::new(0),
            hold_after: AtomicUsize::new(usize::MAX),
            open: tokio::sync::watch::channel(false).0,
        })
    }

    pub fn times(&self) -> Vec<OffsetDateTime> {
        let mut times = self.times.lock().unwrap().clone();
        times.sort();
        times
    }

    pub fn hold_after(&self, sends: usize) {
        self.hold_after.store(sends, Ordering::SeqCst);
    }

    pub fn open(&self) {
        self.open.send_replace(true);
    }
}

#[derive(Debug)]
pub struct Wire {
    pub graph: ScriptedTransport,
    pub controls: Arc<Controls>,
}

#[async_trait::async_trait]
impl HttpTransport for Wire {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let n = self.controls.seen.fetch_add(1, Ordering::SeqCst) + 1;
        if n > self.controls.hold_after.load(Ordering::SeqCst) {
            let mut open = self.controls.open.subscribe();
            // Never opened: the replica "died" with this send in flight.
            let _ = open.wait_for(|open| *open).await;
        }
        self.controls
            .times
            .lock()
            .unwrap()
            .push(self.controls.clock.now());
        // Another replica's turn, as a real round trip gives it.
        tokio::task::yield_now().await;
        self.graph.send(request).await
    }
}

pub fn settings(clock: &ManualClock, broadcasts: BroadcastSettings) -> Settings {
    Settings {
        broadcasts: BroadcastSettings {
            timer: Some(Arc::new(clock.clone())),
            ..broadcasts
        },
        ..super::test_settings()
    }
}

pub fn rate(per_second: u32) -> BroadcastSettings {
    BroadcastSettings {
        rate_per_second: per_second,
        ..BroadcastSettings::default()
    }
}

/// A service on `stores`, its clock and pacer on `clock`, Meta behind
/// `controls`.
pub fn replica(
    stores: Stores,
    clock: &ManualClock,
    controls: &Arc<Controls>,
    broadcasts: BroadcastSettings,
) -> Harness {
    let controls = controls.clone();
    Harness::with_clock(
        stores,
        settings(clock, broadcasts),
        clock.clone(),
        move |graph| Arc::new(Wire { graph, controls }),
    )
}

/// The same data again, for a second replica.
pub fn shared(stores: &Stores) -> Stores {
    Stores {
        store: stores.store.clone(),
        kv: stores.kv.clone(),
        conversations: stores.conversations.clone(),
        events: stores.events.clone(),
        notifier: stores.notifier.clone(),
        webhooks: stores.webhooks.clone(),
    }
}

pub struct World {
    pub h: Harness,
    pub key: String,
    pub clock: ManualClock,
    pub controls: Arc<Controls>,
}

pub async fn world(broadcasts: BroadcastSettings) -> World {
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let controls = Controls::new(Arc::new(clock.clone()));
    let h = replica(Stores::memory(), &clock, &controls, broadcasts);
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN, PN2], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], "EAAG-other")
        .await;
    let key = h
        .tenant_key(TENANT, &[Scope::Broadcasts, Scope::Send])
        .await;
    World {
        h,
        key,
        clock,
        controls,
    }
}

/// Time as it passes under Tokio's paused clock: it moves only when every
/// task waits, so two replicas sharing it keep each send at the instant it
/// really started (a `ManualClock` jumps to the latest deadline whoever
/// asks, which bunches the instants of sends another replica delayed).
#[derive(Debug, Clone)]
pub struct Virtual {
    pub base: OffsetDateTime,
    pub start: tokio::time::Instant,
}

impl Virtual {
    pub fn new() -> Self {
        Self {
            base: OffsetDateTime::now_utc(),
            start: tokio::time::Instant::now(),
        }
    }
}

impl Clock for Virtual {
    fn now(&self) -> OffsetDateTime {
        self.base + self.start.elapsed()
    }
}

#[async_trait::async_trait]
impl Timer for Virtual {
    async fn sleep_until(&self, deadline: OffsetDateTime) {
        let wait = deadline - self.now();
        if wait.is_positive() {
            tokio::time::sleep(wait.unsigned_abs()).await;
        }
    }
}

// ─── Requests and answers ────────────────────────────────────────────────

pub fn template() -> Value {
    json!({
        "name": "spring_sale",
        "language": {"code": "en_US"},
        "components": [{"type": "body", "parameters": [{"type": "text", "text": "SPRING"}]}]
    })
}

pub fn phone(n: usize) -> String {
    format!("+1650555{n:04}")
}

pub fn people(n: usize) -> Vec<Value> {
    (0..n).map(|i| json!({"phone": phone(i)})).collect()
}

pub fn broadcast(people: Vec<Value>) -> Value {
    json!({"template": template(), "recipients": people})
}

pub fn create(key: &str, pn: &str, body: &Value) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{pn}/broadcasts"))
        .key(key)
        .json(body)
}

pub fn read(key: &str, pn: &str, id: &str) -> Call {
    Call::get(format!("/v1/numbers/{pn}/broadcasts/{id}")).key(key)
}

pub fn cancel(key: &str, pn: &str, id: &str) -> Call {
    Call::new(
        Method::POST,
        format!("/v1/numbers/{pn}/broadcasts/{id}/cancel"),
    )
    .key(key)
}

/// Meta's documented answer to a send (`messages/send-messages`).
pub fn accepted(n: usize) -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": phone(n), "wa_id": phone(n).trim_start_matches('+')}],
        "messages": [{"id": format!("wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBI{n}")}]
    })
}

pub fn script(h: &Harness, sends: usize) {
    for n in 0..sends {
        h.graph.push_json(200, accepted(n));
    }
}

pub async fn queued(w: &World, pn: &str, body: &Value) -> String {
    let reply = w.h.call(create(&w.key, pn, body)).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    reply.json()["id"].as_str().unwrap().to_owned()
}

pub async fn status(w: &World, id: &str) -> Value {
    let reply = w.h.call(read(&w.key, PN, id)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    reply.json()
}

pub async fn work(h: &Harness) {
    let workers = h.state.broadcast_workers().unwrap();
    workers[0].tick().await.unwrap();
}

/// No second of `times` holds more than `per_second` sends: the send
/// `per_second` places later is at least a second later.
pub fn within(times: &[OffsetDateTime], per_second: usize) {
    for window in times.windows(per_second + 1) {
        assert!(
            window[per_second] - window[0] >= time::Duration::SECOND,
            "{} sends within {}",
            per_second + 1,
            window[per_second] - window[0]
        );
    }
}

pub fn without_request_id(reply: &super::Reply) -> Value {
    let mut body = reply.json();
    body["error"]["request_id"] = Value::Null;
    body
}

pub fn bodies(h: &Harness) -> Vec<Value> {
    h.graph
        .requests()
        .iter()
        .map(|r| r.json().unwrap())
        .collect()
}

// ─── A store that gives other senders their turn, and loses races ────────

/// The bundle's `KvStore` with two faults a test switches on: every read
/// first gives the other tasks their turn (so concurrent callers really
/// interleave their read and their write), and every write to the shared
/// pacer's record loses the compare-and-swap, as under contention.
#[derive(Debug)]
pub struct Chaos {
    pub inner: Arc<dyn meta_whatsapp_rs::core::store::KvStore>,
    pub yield_reads: bool,
    pub lose_pacer: std::sync::atomic::AtomicBool,
}

impl Chaos {
    fn pacer(&self, key: &meta_whatsapp_rs::core::store::StoreKey) -> bool {
        key.namespace() == "wa.bot.pacer" && self.lose_pacer.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl meta_whatsapp_rs::core::store::KvStore for Chaos {
    async fn get(
        &self,
        key: &meta_whatsapp_rs::core::store::StoreKey,
    ) -> Result<
        Option<meta_whatsapp_rs::core::store::Versioned>,
        meta_whatsapp_rs::core::error::StorageError,
    > {
        if self.yield_reads {
            tokio::task::yield_now().await;
        }
        self.inner.get(key).await
    }
    async fn put(
        &self,
        key: &meta_whatsapp_rs::core::store::StoreKey,
        value: Vec<u8>,
        expiry: meta_whatsapp_rs::core::store::Expiry,
    ) -> Result<u64, meta_whatsapp_rs::core::error::StorageError> {
        self.inner.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &meta_whatsapp_rs::core::store::StoreKey,
        value: Vec<u8>,
        expiry: meta_whatsapp_rs::core::store::Expiry,
    ) -> Result<Option<u64>, meta_whatsapp_rs::core::error::StorageError> {
        if self.pacer(key) {
            return Ok(None);
        }
        self.inner.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &meta_whatsapp_rs::core::store::StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: meta_whatsapp_rs::core::store::Expiry,
    ) -> Result<Option<u64>, meta_whatsapp_rs::core::error::StorageError> {
        if self.pacer(key) {
            return Ok(None);
        }
        self.inner
            .compare_and_swap(key, expected, new, expiry)
            .await
    }
    async fn delete(
        &self,
        key: &meta_whatsapp_rs::core::store::StoreKey,
    ) -> Result<bool, meta_whatsapp_rs::core::error::StorageError> {
        self.inner.delete(key).await
    }
}

/// `stores` with its `KvStore` wrapped in a [`Chaos`].
pub fn chaotic(stores: Stores, yield_reads: bool) -> (Stores, Arc<Chaos>) {
    let chaos = Arc::new(Chaos {
        inner: stores.kv.clone(),
        yield_reads,
        lose_pacer: std::sync::atomic::AtomicBool::new(false),
    });
    (
        Stores {
            kv: chaos.clone(),
            ..stores
        },
        chaos,
    )
}
