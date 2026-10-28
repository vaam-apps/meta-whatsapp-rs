//! A memory backend of the core's own tests, with no server crate: the
//! ports the admin, messages, templates and numbers suites
//! (`conformance::admin`, `conformance::messages`,
//! `conformance::templates`, `conformance::numbers`) reach. Its records
//! keep decision D4 (`bind_waba` refuses a WABA another tenant holds) and
//! condition an unbind and a status change on the binding's epoch, its
//! idempotency records lease and expire keys on the process's monotonic
//! clock, and its vault's key/value store is `support::Kv`. The ports no
//! suite reaches are not there.
#![allow(dead_code)] // each test binary uses a different subset

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_server_core::backend::{Backend, BackendKind};
use meta_whatsapp_server_core::model::{
    ApiKeyRecord, BindOutcome, BindingEpoch, DeleteTenantOutcome, IdempotencyClaim, IdempotencyKey,
    IdempotencyRecord, IdempotencyState, KeyScope, Listing, NewApiKey, NumberBinding, NumberStatus,
    PageRequest, Tenant, TenantId, TenantStatus, WabaBinding,
};
use meta_whatsapp_server_core::outbox::Outbox;
use meta_whatsapp_server_core::store::{
    IdempotencyRecords, Janitor, LeaderLock, RecordStore, SchemaMigrator, StoreResult, listing,
};
use time::OffsetDateTime;

use crate::support;

#[derive(Default)]
pub struct State {
    tenants: BTreeMap<String, Tenant>,
    keys: BTreeMap<String, ApiKeyRecord>,
    wabas: BTreeMap<String, WabaBinding>,
    numbers: BTreeMap<String, NumberBinding>,
}

/// Records in memory, as far as the suites this backend runs reach them:
/// tenants, keys and bindings, with D4 kept by `bind_waba`.
#[derive(Default)]
pub struct Records(Mutex<State>);

impl Records {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }
}

#[async_trait]
impl RecordStore for Records {
    async fn ping(&self) -> StoreResult<()> {
        Ok(())
    }
    async fn create_tenant(&self, id: &TenantId, name: &str) -> StoreResult<Option<Tenant>> {
        let mut state = self.state();
        if state.tenants.contains_key(id.as_str()) {
            return Ok(None);
        }
        let now = OffsetDateTime::now_utc();
        let tenant = Tenant {
            id: id.clone(),
            name: name.to_owned(),
            status: TenantStatus::Active,
            created_at: now,
            updated_at: now,
        };
        state.tenants.insert(id.as_str().to_owned(), tenant.clone());
        Ok(Some(tenant))
    }
    async fn tenant(&self, id: &TenantId) -> StoreResult<Option<Tenant>> {
        Ok(self.state().tenants.get(id.as_str()).cloned())
    }
    async fn tenants(&self, page: &PageRequest) -> StoreResult<Listing<Tenant>> {
        let items = self.state().tenants.values().cloned().collect();
        Ok(listing(items, page.limit, |t| t.id.as_str().to_owned()))
    }
    async fn update_tenant(
        &self,
        _: &TenantId,
        _: Option<&str>,
        _: Option<TenantStatus>,
    ) -> StoreResult<Option<Tenant>> {
        unimplemented!("not used by the suites this backend runs")
    }
    async fn delete_tenant(&self, _: &TenantId) -> StoreResult<DeleteTenantOutcome> {
        unimplemented!("not used by the suites this backend runs")
    }
    async fn insert_key(&self, key: &NewApiKey) -> StoreResult<Option<ApiKeyRecord>> {
        let mut state = self.state();
        if state.keys.contains_key(&key.key_id) {
            return Ok(None);
        }
        let record = ApiKeyRecord {
            key_id: key.key_id.clone(),
            secret_sha256: key.secret_sha256,
            owner: key.owner.clone(),
            scopes: key.scopes.clone(),
            name: key.name.clone(),
            created_at: OffsetDateTime::now_utc(),
            expires_at: key.expires_at,
            revoked_at: None,
            last_used_at: None,
        };
        state.keys.insert(key.key_id.clone(), record.clone());
        Ok(Some(record))
    }
    async fn key(&self, key_id: &str) -> StoreResult<Option<ApiKeyRecord>> {
        Ok(self.state().keys.get(key_id).cloned())
    }
    async fn keys(&self, _: &KeyScope, _: &PageRequest) -> StoreResult<Listing<ApiKeyRecord>> {
        unimplemented!("not used by the suites this backend runs")
    }
    async fn revoke_key(&self, _: &KeyScope, _: &str) -> StoreResult<bool> {
        unimplemented!("not used by the suites this backend runs")
    }
    async fn touch_key(&self, _: &str) -> StoreResult<()> {
        Ok(())
    }
    async fn bind_waba(
        &self,
        tenant: &TenantId,
        waba_id: &WabaId,
        numbers: &[PhoneNumberId],
    ) -> StoreResult<BindOutcome> {
        let mut state = self.state();
        if !state.tenants.contains_key(tenant.as_str()) {
            return Ok(BindOutcome::NoSuchTenant);
        }
        // D4, atomically with the binding: the WABA, or any of its numbers,
        // held by another tenant.
        let held_elsewhere = state
            .wabas
            .get(waba_id.as_str())
            .is_some_and(|w| &w.tenant_id != tenant)
            || numbers.iter().any(|pn| {
                state
                    .numbers
                    .get(pn.as_str())
                    .is_some_and(|n| &n.tenant_id != tenant)
            });
        if held_elsewhere {
            return Ok(BindOutcome::OwnedByAnotherTenant);
        }
        let now = OffsetDateTime::now_utc();
        let attached_at = state
            .wabas
            .get(waba_id.as_str())
            .map_or(now, |w| w.attached_at);
        state.wabas.insert(
            waba_id.as_str().to_owned(),
            WabaBinding {
                waba_id: waba_id.clone(),
                tenant_id: tenant.clone(),
                credit_allocation_id: None,
                attached_at,
            },
        );
        for pn in numbers {
            state.numbers.insert(
                pn.as_str().to_owned(),
                NumberBinding {
                    phone_number_id: pn.clone(),
                    waba_id: waba_id.clone(),
                    tenant_id: tenant.clone(),
                    status: NumberStatus::Connected,
                    updated_at: now,
                },
            );
        }
        Ok(BindOutcome::Bound)
    }
    async fn unbind_waba(&self, _: &WabaId) -> StoreResult<bool> {
        unimplemented!("not used by the suites this backend runs")
    }
    async fn unbind_waba_if(&self, epoch: &BindingEpoch) -> StoreResult<bool> {
        // The numbers suite's disconnection: the binding and its numbers,
        // only while the binding is the one `epoch` read.
        let mut state = self.state();
        let held = state
            .wabas
            .get(epoch.waba_id.as_str())
            .is_some_and(|w| w.epoch() == *epoch);
        if !held {
            return Ok(false);
        }
        state.wabas.remove(epoch.waba_id.as_str());
        state.numbers.retain(|_, n| n.waba_id != epoch.waba_id);
        Ok(true)
    }
    async fn waba(&self, waba_id: &WabaId) -> StoreResult<Option<WabaBinding>> {
        Ok(self.state().wabas.get(waba_id.as_str()).cloned())
    }
    async fn number(&self, pn: &PhoneNumberId) -> StoreResult<Option<NumberBinding>> {
        Ok(self.state().numbers.get(pn.as_str()).cloned())
    }
    async fn all_wabas(&self, _: &PageRequest) -> StoreResult<Listing<WabaBinding>> {
        unimplemented!("not used by the suites this backend runs")
    }
    async fn wabas(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<WabaBinding>> {
        let items = self
            .state()
            .wabas
            .values()
            .filter(|w| &w.tenant_id == tenant)
            .cloned()
            .collect();
        Ok(listing(items, page.limit, |w| {
            w.waba_id.as_str().to_owned()
        }))
    }
    async fn waba_numbers(&self, waba_id: &WabaId) -> StoreResult<Vec<NumberBinding>> {
        Ok(self
            .state()
            .numbers
            .values()
            .filter(|n| &n.waba_id == waba_id)
            .cloned()
            .collect())
    }
    async fn numbers(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<NumberBinding>> {
        let items = self
            .state()
            .numbers
            .values()
            .filter(|n| &n.tenant_id == tenant)
            .cloned()
            .collect();
        Ok(listing(items, page.limit, |n| {
            n.phone_number_id.as_str().to_owned()
        }))
    }
    async fn set_waba_status(&self, _: &WabaId, _: NumberStatus) -> StoreResult<()> {
        unimplemented!("not used by the suites this backend runs")
    }
    async fn set_waba_status_if(
        &self,
        epoch: &BindingEpoch,
        status: NumberStatus,
    ) -> StoreResult<bool> {
        // A `190` marking the numbers of the binding a capability was made
        // from (the numbers and templates suites), only while it holds.
        let mut state = self.state();
        let held = state
            .wabas
            .get(epoch.waba_id.as_str())
            .is_some_and(|w| w.epoch() == *epoch);
        if !held {
            return Ok(false);
        }
        let now = OffsetDateTime::now_utc();
        for number in state.numbers.values_mut() {
            if number.waba_id == epoch.waba_id {
                number.status = status;
                number.updated_at = now;
            }
        }
        Ok(true)
    }
}

/// A key's record: the request's fingerprint, the claim holding it, its
/// state (`None` while in progress) and when its lease and its record end.
struct Kept {
    fingerprint: [u8; 32],
    claim: String,
    answer: Option<(u16, Vec<u8>)>,
    lease_until: Instant,
    expires_at: Instant,
}

/// Idempotency records in memory, by tenant and key: one lock, so a claim
/// is atomic.
#[derive(Default)]
pub struct Idempotency(Mutex<BTreeMap<(String, String), Kept>>);

impl Idempotency {
    fn id(tenant: &TenantId, key: &IdempotencyKey) -> (String, String) {
        (tenant.as_str().to_owned(), key.as_str().to_owned())
    }
}

#[async_trait]
impl IdempotencyRecords for Idempotency {
    async fn claim_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        fingerprint: &[u8; 32],
        claim: &str,
        lease: Duration,
        ttl: Duration,
    ) -> StoreResult<IdempotencyClaim> {
        let now = Instant::now();
        let mut kept = self.0.lock().unwrap();
        if let Some(record) = kept.get(&Self::id(tenant, key))
            && record.expires_at > now
        {
            let state = match &record.answer {
                Some((status, body)) => IdempotencyState::Completed {
                    status: *status,
                    body: body.clone(),
                },
                None => IdempotencyState::InProgress {
                    lease_expired: record.lease_until <= now,
                },
            };
            return Ok(IdempotencyClaim::Existing(IdempotencyRecord {
                fingerprint: record.fingerprint,
                state,
            }));
        }
        kept.insert(
            Self::id(tenant, key),
            Kept {
                fingerprint: *fingerprint,
                claim: claim.to_owned(),
                answer: None,
                lease_until: now + lease,
                expires_at: now + ttl,
            },
        );
        Ok(IdempotencyClaim::Claimed)
    }

    async fn complete_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        claim: &str,
        status: u16,
        body: &[u8],
    ) -> StoreResult<bool> {
        let mut kept = self.0.lock().unwrap();
        match kept.get_mut(&Self::id(tenant, key)) {
            Some(record) if record.claim == claim && record.answer.is_none() => {
                record.answer = Some((status, body.to_vec()));
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn release_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        claim: &str,
    ) -> StoreResult<bool> {
        let mut kept = self.0.lock().unwrap();
        let id = Self::id(tenant, key);
        match kept.get(&id) {
            Some(record) if record.claim == claim && record.answer.is_none() => {
                kept.remove(&id);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn purge_idempotency_keys(&self) -> StoreResult<u64> {
        let now = Instant::now();
        let mut kept = self.0.lock().unwrap();
        let before = kept.len();
        kept.retain(|_, record| record.expires_at > now);
        Ok((before - kept.len()) as u64)
    }
}

/// A backend of the records and idempotency records above and a memory
/// key/value store (the vault's).
#[derive(Default)]
pub struct Memory {
    pub records: Arc<Records>,
    pub idempotency: Arc<Idempotency>,
    pub kv: Arc<support::Kv>,
}

#[async_trait]
impl Backend for Memory {
    fn kind(&self) -> BackendKind {
        BackendKind::Other("core-tests-memory")
    }
    fn records(&self) -> Arc<dyn RecordStore> {
        self.records.clone()
    }
    fn idempotency(&self) -> Arc<dyn IdempotencyRecords> {
        self.idempotency.clone()
    }
    fn outbox(&self) -> Arc<dyn Outbox> {
        unimplemented!("not used by the suites this backend runs")
    }
    fn leader_lock(&self) -> Arc<dyn LeaderLock> {
        unimplemented!("not used by the suites this backend runs")
    }
    fn janitor(&self) -> Arc<dyn Janitor> {
        unimplemented!("not used by the suites this backend runs")
    }
    fn migrator(&self) -> Arc<dyn SchemaMigrator> {
        unimplemented!("not used by the suites this backend runs")
    }
    fn kv(&self) -> Arc<dyn KvStore> {
        self.kv.clone()
    }
    fn conversations(&self) -> Arc<dyn ConversationStore> {
        unimplemented!("not used by the suites this backend runs")
    }
    async fn close(&self) {}
}
