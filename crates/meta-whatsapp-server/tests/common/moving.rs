//! A `Store` that moves a WABA at a chosen point of a request: a hook run
//! once, right after a chosen read has read what it returns (roadmap S2,
//! the security review's M1 and L1: `tests/moves.rs`). The move itself is
//! [`move_waba`], as the service does one.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::client::embedded_signup::{StoredBusinessToken, TokenVault};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_server::model::{
    ApiKeyRecord, BindOutcome, BindingEpoch, DeleteTenantOutcome, IdempotencyClaim, IdempotencyKey,
    KeyScope, Listing, NewApiKey, NumberBinding, NumberStatus, PageRequest, Tenant, TenantId,
    TenantStatus, WabaBinding,
};
use meta_whatsapp_server::store::{IdempotencyRecords, RecordStore, Store, StoreResult};

/// Work a hook does.
pub type Hook = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// The store under test, with hooks. Everything else is `inner`'s.
pub struct Moving {
    inner: Arc<dyn Store>,
    /// Run once the `n`-th `waba` read from now (1: the next) has read.
    after_waba: Mutex<Option<(usize, Hook)>>,
    /// Run once the next `wabas` listing has read.
    after_wabas: Mutex<Option<Hook>>,
}

impl Moving {
    pub fn new(inner: Arc<dyn Store>) -> Self {
        Self {
            inner,
            after_waba: Mutex::new(None),
            after_wabas: Mutex::new(None),
        }
    }

    /// Run `hook` once the `n`-th `waba` read from now has read.
    pub fn after_waba_read(&self, n: usize, hook: Hook) {
        *self.after_waba.lock().unwrap() = Some((n, hook));
    }

    /// Run `hook` once the next `wabas` listing has read.
    pub fn after_listing(&self, hook: Hook) {
        *self.after_wabas.lock().unwrap() = Some(hook);
    }

    /// Whether a hook is still waiting (it never ran: the test's count of
    /// reads is wrong).
    pub fn armed(&self) -> bool {
        self.after_waba.lock().unwrap().is_some() || self.after_wabas.lock().unwrap().is_some()
    }
}

/// Move `waba` (and `pn`) to `to`, as the service does: its token deleted
/// before its binding (every unbind), bound to `to`, then, with a `token`,
/// `to`'s token stored (an attach binds before it stores; `None`: an
/// attach between the two).
pub async fn move_waba(
    store: &dyn Store,
    vault: &TokenVault,
    waba: &WabaId,
    pn: &PhoneNumberId,
    to: &str,
    token: Option<&str>,
) {
    vault.delete(waba).await.unwrap();
    assert!(store.unbind_waba(waba).await.unwrap());
    let to = TenantId::parse(to).unwrap();
    let _ = store.create_tenant(&to, "").await.unwrap();
    assert_eq!(
        store
            .bind_waba(&to, waba, std::slice::from_ref(pn))
            .await
            .unwrap(),
        BindOutcome::Bound
    );
    if let Some(token) = token {
        vault
            .store(
                &StoredBusinessToken::new(waba.clone(), AccessToken::new(token))
                    .phone_number_ids([pn.clone()]),
            )
            .await
            .unwrap();
    }
}

#[async_trait]
impl RecordStore for Moving {
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
        self.inner.delete_tenant(id).await
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
        self.inner.bind_waba(tenant, waba_id, numbers).await
    }
    async fn unbind_waba(&self, waba_id: &WabaId) -> StoreResult<bool> {
        self.inner.unbind_waba(waba_id).await
    }
    async fn unbind_waba_if(&self, epoch: &BindingEpoch) -> StoreResult<bool> {
        self.inner.unbind_waba_if(epoch).await
    }
    async fn waba(&self, waba_id: &WabaId) -> StoreResult<Option<WabaBinding>> {
        let read = self.inner.waba(waba_id).await;
        let hook = {
            let mut armed = self.after_waba.lock().unwrap();
            match armed.take() {
                Some((1, hook)) => Some(hook),
                Some((n, hook)) => {
                    *armed = Some((n - 1, hook));
                    None
                }
                None => None,
            }
        };
        if let Some(hook) = hook {
            hook.await;
        }
        read
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
        let read = self.inner.wabas(tenant, page).await;
        let hook = self.after_wabas.lock().unwrap().take();
        if let Some(hook) = hook {
            hook.await;
        }
        read
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

#[async_trait]
impl IdempotencyRecords for Moving {
    async fn claim_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        fingerprint: &[u8; 32],
        claim: &str,
        lease: Duration,
        ttl: Duration,
    ) -> StoreResult<IdempotencyClaim> {
        self.inner
            .claim_idempotency_key(tenant, key, fingerprint, claim, lease, ttl)
            .await
    }
    async fn complete_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        claim: &str,
        status: u16,
        body: &[u8],
    ) -> StoreResult<bool> {
        self.inner
            .complete_idempotency_key(tenant, key, claim, status, body)
            .await
    }
    async fn release_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        claim: &str,
    ) -> StoreResult<bool> {
        self.inner.release_idempotency_key(tenant, key, claim).await
    }
    async fn purge_idempotency_keys(&self) -> StoreResult<u64> {
        self.inner.purge_idempotency_keys().await
    }
}
