//! Roadmap S5a's decisive case, in this crate alone: no HTTP layer and no
//! server crate. The core's admin services run over a memory backend
//! defined here (its records keep decision D4: `bind_waba` refuses a WABA
//! another tenant holds) and a vault on a memory key/value store, with a
//! Graph client on the library's `ScriptedTransport`: attaching a WABA
//! another tenant holds is refused with nothing stored, subscribed or
//! shared (`conformance::admin`, which the service's memory and Postgres
//! backends run too, in its `tests/store.rs` and `tests/live_postgres.rs`).
//! The rest of the admin services, and every HTTP answer, are the
//! service's `tests/admin.rs`.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_server_core::backend::{Backend, BackendKind};
use meta_whatsapp_server_core::conformance;
use meta_whatsapp_server_core::model::{
    ApiKeyRecord, BindOutcome, BindingEpoch, DeleteTenantOutcome, KeyScope, Listing, NewApiKey,
    NumberBinding, NumberStatus, PageRequest, Tenant, TenantId, TenantStatus, WabaBinding,
};
use meta_whatsapp_server_core::outbox::Outbox;
use meta_whatsapp_server_core::store::{
    IdempotencyRecords, Janitor, LeaderLock, RecordStore, SchemaMigrator, StoreResult, listing,
};
use time::OffsetDateTime;

#[derive(Default)]
struct State {
    tenants: BTreeMap<String, Tenant>,
    keys: BTreeMap<String, ApiKeyRecord>,
    wabas: BTreeMap<String, WabaBinding>,
    numbers: BTreeMap<String, NumberBinding>,
}

/// Records in memory, as far as the admin suite reaches them: tenants,
/// keys and bindings, with D4 kept by `bind_waba`.
#[derive(Default)]
struct Records(Mutex<State>);

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
        unimplemented!("not used by the admin suite")
    }
    async fn delete_tenant(&self, _: &TenantId) -> StoreResult<DeleteTenantOutcome> {
        unimplemented!("not used by the admin suite")
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
        unimplemented!("not used by the admin suite")
    }
    async fn revoke_key(&self, _: &KeyScope, _: &str) -> StoreResult<bool> {
        unimplemented!("not used by the admin suite")
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
        unimplemented!("not used by the admin suite")
    }
    async fn unbind_waba_if(&self, _: &BindingEpoch) -> StoreResult<bool> {
        unimplemented!("not used by the admin suite")
    }
    async fn waba(&self, waba_id: &WabaId) -> StoreResult<Option<WabaBinding>> {
        Ok(self.state().wabas.get(waba_id.as_str()).cloned())
    }
    async fn number(&self, pn: &PhoneNumberId) -> StoreResult<Option<NumberBinding>> {
        Ok(self.state().numbers.get(pn.as_str()).cloned())
    }
    async fn all_wabas(&self, _: &PageRequest) -> StoreResult<Listing<WabaBinding>> {
        unimplemented!("not used by the admin suite")
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
        unimplemented!("not used by the admin suite")
    }
    async fn set_waba_status_if(&self, _: &BindingEpoch, _: NumberStatus) -> StoreResult<bool> {
        unimplemented!("not used by the admin suite")
    }
}

/// A backend of the records above and a memory key/value store (the
/// vault's): the ports the admin suite does not reach are not there.
struct AdminOnly {
    records: Arc<Records>,
    kv: Arc<support::Kv>,
}

#[async_trait]
impl Backend for AdminOnly {
    fn kind(&self) -> BackendKind {
        BackendKind::Other("admin-only")
    }
    fn records(&self) -> Arc<dyn RecordStore> {
        self.records.clone()
    }
    fn idempotency(&self) -> Arc<dyn IdempotencyRecords> {
        unimplemented!("not used by the admin suite")
    }
    fn outbox(&self) -> Arc<dyn Outbox> {
        unimplemented!("not used by the admin suite")
    }
    fn leader_lock(&self) -> Arc<dyn LeaderLock> {
        unimplemented!("not used by the admin suite")
    }
    fn janitor(&self) -> Arc<dyn Janitor> {
        unimplemented!("not used by the admin suite")
    }
    fn migrator(&self) -> Arc<dyn SchemaMigrator> {
        unimplemented!("not used by the admin suite")
    }
    fn kv(&self) -> Arc<dyn KvStore> {
        self.kv.clone()
    }
    fn conversations(&self) -> Arc<dyn ConversationStore> {
        unimplemented!("not used by the admin suite")
    }
    async fn close(&self) {}
}

/// Roadmap S5a, decisive: called with no HTTP layer, attaching a WABA
/// another tenant holds is refused with nothing stored, subscribed or
/// shared, whether the holder is seen before Meta is asked or only by the
/// binding's own check.
#[tokio::test]
async fn attaching_a_waba_another_tenant_holds_stores_subscribes_and_shares_nothing() {
    let backend = AdminOnly {
        records: Arc::new(Records::default()),
        kv: Arc::new(support::Kv::default()),
    };
    conformance::admin::a_waba_another_tenant_holds_is_refused_with_nothing_stored(&backend).await;
}
