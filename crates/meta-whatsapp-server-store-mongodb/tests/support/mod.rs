//! What this crate's live tests share: the test server's URL (a skip, or
//! a failure under `META_WHATSAPP_RS_REQUIRE_LIVE=1`), a `wa_test_*`
//! database per backend, dropped when the test ends (a panicking one
//! included), and the bundle the core's conformance suites run over.
//!
//! **The bundle** ([`SpikeBackend`]): this crate's [`MongoOutbox`] for the
//! outbox, the memory backend's ports for the rest (the records are roadmap
//! S18). The outbox checks its inserts against the bindings it stores
//! itself ([`MongoOutbox::record_binding`]), so the records here are the
//! memory store's, wrapped ([`MirroredRecords`]): every binding they change
//! is written to the outbox's database before the call returns, and a
//! deleted tenant's stream purged there, under one lock, so the bindings
//! the outbox reads are the ones the records answer with whenever no
//! binding call is in flight. That is how the cross-port invariants
//! (`conformance::invariants`) get their binding.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)] // test support

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_server_core::backend::{Backend, BackendKind};
use meta_whatsapp_server_core::model::{
    ApiKeyRecord, BindOutcome, BindingEpoch, DeleteTenantOutcome, KeyScope, Listing, NewApiKey,
    NumberBinding, NumberStatus, PageRequest, Tenant, TenantId, TenantStatus, WabaBinding,
};
use meta_whatsapp_server_core::outbox::Outbox;
use meta_whatsapp_server_core::store::{
    IdempotencyRecords, Janitor, LeaderLock, RecordStore, SchemaMigrator, StoreResult,
};
use meta_whatsapp_server_store_memory::MemoryBackend;
use meta_whatsapp_server_store_mongodb::MongoOutbox;
use mongodb::{Client, Database};

/// `META_WHATSAPP_RS_TEST_MONGODB_URL`, or `None` (after saying so) when it
/// is unset.
///
/// # Panics
///
/// When it is unset and `META_WHATSAPP_RS_REQUIRE_LIVE=1`: a skipped live
/// test must not pass as a green one in `just test-live`.
pub fn mongodb_url() -> Option<String> {
    const VAR: &str = "META_WHATSAPP_RS_TEST_MONGODB_URL";
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

/// Drops a test database when it goes out of scope, a panicking test's
/// unwinding included: on a thread of its own, with a client and a
/// current-thread runtime of its own (`Drop` cannot await, and the test's
/// runtime may be the one unwinding). As meta-whatsapp-adapters'
/// `tests/live_mongodb.rs` does; `just test-live-clean` drops what a killed
/// run left.
struct DropDatabase {
    url: String,
    name: String,
}

impl DropDatabase {
    fn run(url: &str, name: &str) -> Result<(), String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            let client = Client::with_uri_str(url).await.map_err(|e| e.to_string())?;
            client
                .database(name)
                .drop()
                .await
                .map_err(|e| e.to_string())?;
            client.shutdown().await;
            Ok(())
        })
    }
}

impl Drop for DropDatabase {
    fn drop(&mut self) {
        let (url, name) = (self.url.clone(), self.name.clone());
        let result = std::thread::spawn(move || Self::run(&url, &name))
            .join()
            .unwrap_or_else(|_| Err("the cleanup thread panicked".to_owned()));
        if let Err(e) = result {
            // A second panic while unwinding would abort the test binary and
            // hide the first one: report it instead.
            if std::thread::panicking() {
                eprintln!("cleanup: dropping database {} failed: {e}", self.name);
            } else {
                panic!("cleanup: dropping database {} failed: {e}", self.name);
            }
        }
    }
}

/// The test server, and every database this test made on it, each dropped
/// with it.
pub struct Live {
    pub url: String,
    pub client: Client,
    databases: Mutex<Vec<DropDatabase>>,
}

impl Live {
    /// The test server, or `None` when no URL is set (and the run does not
    /// require live services).
    pub async fn new() -> Option<Self> {
        let url = mongodb_url()?;
        let client = Client::with_uri_str(&url)
            .await
            .expect("valid META_WHATSAPP_RS_TEST_MONGODB_URL");
        Some(Self {
            url,
            client,
            databases: Mutex::new(Vec::new()),
        })
    }

    /// A new, empty `wa_test_*` database, dropped with `self`.
    pub fn database(&self) -> Database {
        let name = format!("wa_test_{}", unique());
        // Before anything is written: a database that exists only in part
        // is dropped too.
        self.databases.lock().unwrap().push(DropDatabase {
            url: self.url.clone(),
            name: name.clone(),
        });
        self.client.database(&name)
    }

    /// The outbox on a new database, migrated.
    pub async fn outbox(&self) -> MongoOutbox {
        let outbox = MongoOutbox::new(self.database());
        outbox.migrate().await.unwrap();
        outbox
    }

    /// A bundle with this crate's outbox on a new database (and the memory
    /// backend's other ports).
    pub async fn backend(&self) -> SpikeBackend {
        SpikeBackend::new(self.outbox().await)
    }
}

/// The memory backend's records, each binding they change written to the
/// outbox's database before the call returns (the crate's docs: the
/// bindings the outbox checks are a projection, for this spike).
pub struct MirroredRecords {
    inner: Arc<dyn RecordStore>,
    outbox: MongoOutbox,
    /// Held across a binding change and its write to MongoDB, so that two
    /// changes reach MongoDB in the order the records made them.
    changing: tokio::sync::Mutex<()>,
}

impl MirroredRecords {
    /// Write the binding `waba_id` has in the records now, or its absence.
    async fn mirror(&self, waba_id: &WabaId) -> StoreResult<()> {
        match self.inner.waba(waba_id).await? {
            Some(binding) => {
                let numbers: Vec<PhoneNumberId> = self
                    .inner
                    .waba_numbers(waba_id)
                    .await?
                    .into_iter()
                    .map(|n| n.phone_number_id)
                    .collect();
                self.outbox.record_binding(&binding, &numbers).await
            }
            None => self.outbox.forget_binding(waba_id).await,
        }
    }
}

#[async_trait]
impl RecordStore for MirroredRecords {
    async fn ping(&self) -> StoreResult<()> {
        self.inner.ping().await
    }
    async fn create_tenant(&self, id: &TenantId, name: &str) -> StoreResult<Option<Tenant>> {
        self.inner.create_tenant(id, name).await
    }
    async fn tenant(&self, id: &TenantId) -> StoreResult<Option<Tenant>> {
        self.inner.tenant(id).await
    }
    async fn tenants(&self, page: &PageRequest) -> StoreResult<Listing<Tenant>> {
        self.inner.tenants(page).await
    }
    async fn update_tenant(
        &self,
        id: &TenantId,
        name: Option<&str>,
        status: Option<TenantStatus>,
    ) -> StoreResult<Option<Tenant>> {
        self.inner.update_tenant(id, name, status).await
    }
    async fn delete_tenant(&self, id: &TenantId) -> StoreResult<DeleteTenantOutcome> {
        let _changing = self.changing.lock().await;
        let outcome = self.inner.delete_tenant(id).await?;
        if outcome == DeleteTenantOutcome::Deleted {
            self.outbox.forget_tenant(id).await?;
        }
        Ok(outcome)
    }
    async fn insert_key(&self, key: &NewApiKey) -> StoreResult<Option<ApiKeyRecord>> {
        self.inner.insert_key(key).await
    }
    async fn key(&self, key_id: &str) -> StoreResult<Option<ApiKeyRecord>> {
        self.inner.key(key_id).await
    }
    async fn keys(
        &self,
        scope: &KeyScope,
        page: &PageRequest,
    ) -> StoreResult<Listing<ApiKeyRecord>> {
        self.inner.keys(scope, page).await
    }
    async fn revoke_key(&self, scope: &KeyScope, key_id: &str) -> StoreResult<bool> {
        self.inner.revoke_key(scope, key_id).await
    }
    async fn touch_key(&self, key_id: &str) -> StoreResult<()> {
        self.inner.touch_key(key_id).await
    }
    async fn bind_waba(
        &self,
        tenant: &TenantId,
        waba_id: &WabaId,
        numbers: &[PhoneNumberId],
    ) -> StoreResult<BindOutcome> {
        let _changing = self.changing.lock().await;
        // A number the binding takes from another of the tenant's WABAs
        // leaves that WABA's document too.
        let mut touched = vec![waba_id.clone()];
        for number in numbers {
            if let Some(bound) = self.inner.number(number).await?
                && !touched.contains(&bound.waba_id)
            {
                touched.push(bound.waba_id);
            }
        }
        let outcome = self.inner.bind_waba(tenant, waba_id, numbers).await?;
        if outcome == BindOutcome::Bound {
            for waba in &touched {
                self.mirror(waba).await?;
            }
        }
        Ok(outcome)
    }
    async fn unbind_waba(&self, waba_id: &WabaId) -> StoreResult<bool> {
        let _changing = self.changing.lock().await;
        let unbound = self.inner.unbind_waba(waba_id).await?;
        if unbound {
            self.outbox.forget_binding(waba_id).await?;
        }
        Ok(unbound)
    }
    async fn unbind_waba_if(&self, epoch: &BindingEpoch) -> StoreResult<bool> {
        let _changing = self.changing.lock().await;
        let unbound = self.inner.unbind_waba_if(epoch).await?;
        if unbound {
            self.outbox.forget_binding(&epoch.waba_id).await?;
        }
        Ok(unbound)
    }
    async fn waba(&self, waba_id: &WabaId) -> StoreResult<Option<WabaBinding>> {
        self.inner.waba(waba_id).await
    }
    async fn number(&self, phone_number_id: &PhoneNumberId) -> StoreResult<Option<NumberBinding>> {
        self.inner.number(phone_number_id).await
    }
    async fn all_wabas(&self, page: &PageRequest) -> StoreResult<Listing<WabaBinding>> {
        self.inner.all_wabas(page).await
    }
    async fn wabas(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<WabaBinding>> {
        self.inner.wabas(tenant, page).await
    }
    async fn waba_numbers(&self, waba_id: &WabaId) -> StoreResult<Vec<NumberBinding>> {
        self.inner.waba_numbers(waba_id).await
    }
    async fn numbers(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<NumberBinding>> {
        self.inner.numbers(tenant, page).await
    }
    async fn set_waba_status(&self, waba_id: &WabaId, status: NumberStatus) -> StoreResult<()> {
        self.inner.set_waba_status(waba_id, status).await
    }
    async fn set_waba_status_if(
        &self,
        epoch: &BindingEpoch,
        status: NumberStatus,
    ) -> StoreResult<bool> {
        self.inner.set_waba_status_if(epoch, status).await
    }
}

/// [`MongoOutbox`] in a bundle: the memory backend's other ports, its
/// records [`MirroredRecords`].
pub struct SpikeBackend {
    memory: MemoryBackend,
    records: Arc<MirroredRecords>,
    outbox: Arc<MongoOutbox>,
}

impl SpikeBackend {
    /// The bundle around `outbox` (migrated), on a new memory backend.
    pub fn new(outbox: MongoOutbox) -> Self {
        let memory = MemoryBackend::new();
        let records = Arc::new(MirroredRecords {
            inner: memory.records(),
            outbox: outbox.clone(),
            changing: tokio::sync::Mutex::new(()),
        });
        Self {
            memory,
            records,
            outbox: Arc::new(outbox),
        }
    }

    /// Its outbox, as itself.
    pub fn mongo_outbox(&self) -> &MongoOutbox {
        &self.outbox
    }
}

#[async_trait]
impl Backend for SpikeBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Other("mongodb outbox spike")
    }
    fn records(&self) -> Arc<dyn RecordStore> {
        self.records.clone()
    }
    fn idempotency(&self) -> Arc<dyn IdempotencyRecords> {
        self.memory.idempotency()
    }
    fn outbox(&self) -> Arc<dyn Outbox> {
        self.outbox.clone()
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
        self.memory.kv()
    }
    fn conversations(&self) -> Arc<dyn ConversationStore> {
        self.memory.conversations()
    }
    async fn close(&self) {
        self.memory.close().await;
    }
}
