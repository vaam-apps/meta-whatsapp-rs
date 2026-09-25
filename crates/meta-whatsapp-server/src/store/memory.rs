//! [`MemoryStore`]: one process, emptied on restart. Only for
//! `WA_SERVER_ENV=development` and tests.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use time::{Duration, OffsetDateTime};

use super::{MemoryEventStore, Store, StoreResult, listing};
use crate::model::{
    ApiKeyRecord, BindOutcome, DeleteTenantOutcome, KeyOwner, KeyScope, Listing, NewApiKey,
    NumberBinding, NumberStatus, PageRequest, Tenant, TenantId, TenantStatus, WabaBinding,
};

/// In-memory [`Store`]. Its event outbox is [`MemoryStore::outbox`]: one
/// process's database, so that deleting a tenant reaches its events as it
/// does on Postgres.
#[derive(Debug, Default)]
pub struct MemoryStore {
    state: Mutex<State>,
    outbox: Arc<MemoryEventStore>,
}

#[derive(Debug, Default)]
struct State {
    tenants: BTreeMap<String, Tenant>,
    keys: BTreeMap<String, ApiKeyRecord>,
    wabas: BTreeMap<String, WabaBinding>,
    numbers: BTreeMap<String, NumberBinding>,
}

impl MemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// The event outbox of this store: deleting a tenant here turns its
    /// events into operator-only rows there.
    pub fn outbox(&self) -> Arc<MemoryEventStore> {
        self.outbox.clone()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn page<T: Clone>(
    map: &BTreeMap<String, T>,
    page: &PageRequest,
    keep: impl Fn(&T) -> bool,
) -> Vec<(String, T)> {
    map.iter()
        .filter(|(id, _)| {
            page.after
                .as_deref()
                .is_none_or(|after| id.as_str() > after)
        })
        .filter(|(_, item)| keep(item))
        .take(page.limit + 1)
        .map(|(id, item)| (id.clone(), item.clone()))
        .collect()
}

fn in_scope(key: &ApiKeyRecord, scope: &KeyScope) -> bool {
    match (scope, &key.owner) {
        (KeyScope::Tenant(tenant), KeyOwner::Tenant(owner)) => tenant == owner,
        (KeyScope::Platform, KeyOwner::Platform(_)) | (KeyScope::Admin, KeyOwner::Admin) => true,
        _ => false,
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn ping(&self) -> StoreResult<()> {
        Ok(())
    }

    async fn create_tenant(&self, id: &TenantId, name: &str) -> StoreResult<Option<Tenant>> {
        let mut state = self.lock();
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
        Ok(self.lock().tenants.get(id.as_str()).cloned())
    }

    async fn tenants(&self, request: &PageRequest) -> StoreResult<Listing<Tenant>> {
        let items = page(&self.lock().tenants, request, |_| true);
        Ok(listing(
            items.into_iter().map(|(_, t)| t).collect(),
            request.limit,
            |t: &Tenant| t.id.as_str().to_owned(),
        ))
    }

    async fn update_tenant(
        &self,
        id: &TenantId,
        name: Option<&str>,
        status: Option<TenantStatus>,
    ) -> StoreResult<Option<Tenant>> {
        let mut state = self.lock();
        let Some(tenant) = state.tenants.get_mut(id.as_str()) else {
            return Ok(None);
        };
        if let Some(name) = name {
            name.clone_into(&mut tenant.name);
        }
        if let Some(status) = status {
            tenant.status = status;
        }
        tenant.updated_at = OffsetDateTime::now_utc();
        Ok(Some(tenant.clone()))
    }

    async fn delete_tenant(&self, id: &TenantId) -> StoreResult<DeleteTenantOutcome> {
        let mut state = self.lock();
        if !state.tenants.contains_key(id.as_str()) {
            return Ok(DeleteTenantOutcome::NotFound);
        }
        if state.wabas.values().any(|w| &w.tenant_id == id) {
            return Ok(DeleteTenantOutcome::HasWabas);
        }
        state.tenants.remove(id.as_str());
        state
            .keys
            .retain(|_, key| !matches!(&key.owner, KeyOwner::Tenant(t) if t == id));
        // Under the store's lock, as Postgres does it in the deleting
        // transaction.
        self.outbox.forget_tenant(id);
        Ok(DeleteTenantOutcome::Deleted)
    }

    async fn insert_key(&self, key: &NewApiKey) -> StoreResult<Option<ApiKeyRecord>> {
        let mut state = self.lock();
        if state.keys.contains_key(&key.key_id) {
            return Ok(None);
        }
        if let KeyOwner::Tenant(tenant) = &key.owner
            && !state.tenants.contains_key(tenant.as_str())
        {
            return Err(meta_whatsapp_rs::core::error::StorageError::Backend(
                anyhow::anyhow!("no such tenant"),
            ));
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
        Ok(self.lock().keys.get(key_id).cloned())
    }

    async fn keys(
        &self,
        scope: &KeyScope,
        request: &PageRequest,
    ) -> StoreResult<Listing<ApiKeyRecord>> {
        let items = page(&self.lock().keys, request, |k| in_scope(k, scope));
        Ok(listing(
            items.into_iter().map(|(_, k)| k).collect(),
            request.limit,
            |k: &ApiKeyRecord| k.key_id.clone(),
        ))
    }

    async fn revoke_key(&self, scope: &KeyScope, key_id: &str) -> StoreResult<bool> {
        let mut state = self.lock();
        match state.keys.get_mut(key_id) {
            Some(key) if in_scope(key, scope) => {
                key.revoked_at.get_or_insert_with(OffsetDateTime::now_utc);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn touch_key(&self, key_id: &str) -> StoreResult<()> {
        let now = OffsetDateTime::now_utc();
        if let Some(key) = self.lock().keys.get_mut(key_id)
            && key
                .last_used_at
                .is_none_or(|at| at < now - Duration::minutes(1))
        {
            key.last_used_at = Some(now);
        }
        Ok(())
    }

    async fn bind_waba(
        &self,
        tenant: &TenantId,
        waba_id: &WabaId,
        numbers: &[PhoneNumberId],
    ) -> StoreResult<BindOutcome> {
        let mut state = self.lock();
        if state
            .wabas
            .get(waba_id.as_str())
            .is_some_and(|w| &w.tenant_id != tenant)
            || numbers.iter().any(|n| {
                state
                    .numbers
                    .get(n.as_str())
                    .is_some_and(|b| &b.tenant_id != tenant)
            })
        {
            return Ok(BindOutcome::OwnedByAnotherTenant);
        }
        let now = OffsetDateTime::now_utc();
        state
            .wabas
            .entry(waba_id.as_str().to_owned())
            .or_insert_with(|| WabaBinding {
                waba_id: waba_id.clone(),
                tenant_id: tenant.clone(),
                credit_allocation_id: None,
                attached_at: now,
            });
        state
            .numbers
            .retain(|pn, b| &b.waba_id != waba_id || numbers.iter().any(|n| n.as_str() == pn));
        for number in numbers {
            state.numbers.insert(
                number.as_str().to_owned(),
                NumberBinding {
                    phone_number_id: number.clone(),
                    waba_id: waba_id.clone(),
                    tenant_id: tenant.clone(),
                    status: NumberStatus::Connected,
                    updated_at: now,
                },
            );
        }
        Ok(BindOutcome::Bound)
    }

    async fn unbind_waba(&self, waba_id: &WabaId) -> StoreResult<bool> {
        let mut state = self.lock();
        let removed = state.wabas.remove(waba_id.as_str()).is_some();
        state.numbers.retain(|_, b| &b.waba_id != waba_id);
        Ok(removed)
    }

    async fn waba(&self, waba_id: &WabaId) -> StoreResult<Option<WabaBinding>> {
        Ok(self.lock().wabas.get(waba_id.as_str()).cloned())
    }

    async fn number(&self, phone_number_id: &PhoneNumberId) -> StoreResult<Option<NumberBinding>> {
        Ok(self.lock().numbers.get(phone_number_id.as_str()).cloned())
    }

    async fn all_wabas(&self, request: &PageRequest) -> StoreResult<Listing<WabaBinding>> {
        let items = page(&self.lock().wabas, request, |_| true);
        Ok(listing(
            items.into_iter().map(|(_, w)| w).collect(),
            request.limit,
            |w: &WabaBinding| w.waba_id.as_str().to_owned(),
        ))
    }

    async fn wabas(
        &self,
        tenant: &TenantId,
        request: &PageRequest,
    ) -> StoreResult<Listing<WabaBinding>> {
        let items = page(&self.lock().wabas, request, |w| &w.tenant_id == tenant);
        Ok(listing(
            items.into_iter().map(|(_, w)| w).collect(),
            request.limit,
            |w: &WabaBinding| w.waba_id.as_str().to_owned(),
        ))
    }

    async fn waba_numbers(&self, waba_id: &WabaId) -> StoreResult<Vec<NumberBinding>> {
        Ok(self
            .lock()
            .numbers
            .values()
            .filter(|n| &n.waba_id == waba_id)
            .cloned()
            .collect())
    }

    async fn numbers(
        &self,
        tenant: &TenantId,
        request: &PageRequest,
    ) -> StoreResult<Listing<NumberBinding>> {
        let items = page(&self.lock().numbers, request, |n| &n.tenant_id == tenant);
        Ok(listing(
            items.into_iter().map(|(_, n)| n).collect(),
            request.limit,
            |n: &NumberBinding| n.phone_number_id.as_str().to_owned(),
        ))
    }

    async fn set_waba_status(&self, waba_id: &WabaId, status: NumberStatus) -> StoreResult<()> {
        let now = OffsetDateTime::now_utc();
        for number in self.lock().numbers.values_mut() {
            if &number.waba_id == waba_id {
                number.status = status;
                number.updated_at = now;
            }
        }
        Ok(())
    }
}
