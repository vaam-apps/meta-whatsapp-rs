//! A `Store` that moves a WABA at a chosen point of a request: a hook run
//! once, right after a chosen read (or binding) has done its work, and a
//! `KvStore` that does the same right after a chosen write (roadmap S2,
//! the security review's M1 and L1, and the attach's confirmation:
//! `tests/moves.rs`, `tests/live_postgres.rs`). The move itself is
//! [`move_waba`], as the service does one.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::client::embedded_signup::{StoredBusinessToken, TOKEN_NAMESPACE, TokenVault};
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::store::{Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{
    ApiKeyRecord, BindOutcome, BindingEpoch, DeleteTenantOutcome, IdempotencyClaim, IdempotencyKey,
    KeyScope, Listing, NewApiKey, NumberBinding, NumberStatus, PageRequest, Tenant, TenantId,
    TenantStatus, WabaBinding,
};
use meta_whatsapp_server::store::{IdempotencyRecords, RecordStore, Store, StoreResult};
use serde_json::json;

use super::{Call, Harness};

/// Work a hook does.
pub type Hook = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// The store under test, with hooks. Everything else is `inner`'s.
pub struct Moving {
    inner: Arc<dyn Store>,
    /// Run once the `n`-th `waba` read from now (1: the next) has read.
    after_waba: Mutex<Option<(usize, Hook)>>,
    /// Run once the next `wabas` listing has read.
    after_wabas: Mutex<Option<Hook>>,
    /// Run once the next `bind_waba` has bound.
    after_bind: Mutex<Option<Hook>>,
}

impl Moving {
    pub fn new(inner: Arc<dyn Store>) -> Self {
        Self {
            inner,
            after_waba: Mutex::new(None),
            after_wabas: Mutex::new(None),
            after_bind: Mutex::new(None),
        }
    }

    /// Run `hook` once the next `bind_waba` has bound.
    pub fn after_binding(&self, hook: Hook) {
        *self.after_bind.lock().unwrap() = Some(hook);
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
        self.after_waba.lock().unwrap().is_some()
            || self.after_wabas.lock().unwrap().is_some()
            || self.after_bind.lock().unwrap().is_some()
    }
}

/// A `KvStore` that runs a hook once, right after a `put` of a chosen key
/// has written (the vault's store of a WABA's record: `waba/{id}` in
/// [`TOKEN_NAMESPACE`]). Everything else is `inner`'s.
pub struct AfterPut {
    inner: Arc<dyn KvStore>,
    hook: Mutex<Option<(StoreKey, Hook)>>,
}

impl std::fmt::Debug for AfterPut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AfterPut").finish_non_exhaustive()
    }
}

impl AfterPut {
    pub fn new(inner: Arc<dyn KvStore>) -> Self {
        Self {
            inner,
            hook: Mutex::new(None),
        }
    }

    /// Run `hook` once the next `put` of the vault record of `waba` has
    /// written.
    pub fn after_storing(&self, waba: &str, hook: Hook) {
        let key = StoreKey::new(TOKEN_NAMESPACE, format!("waba/{waba}"));
        *self.hook.lock().unwrap() = Some((key, hook));
    }

    /// Whether the hook is still waiting.
    pub fn armed(&self) -> bool {
        self.hook.lock().unwrap().is_some()
    }
}

#[async_trait]
impl KvStore for AfterPut {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        self.inner.get(key).await
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        let written = self.inner.put(key, value, expiry).await;
        let hook = {
            let mut armed = self.hook.lock().unwrap();
            match armed.take() {
                Some((at, hook)) if &at == key => Some(hook),
                other => {
                    *armed = other;
                    None
                }
            }
        };
        if let Some(hook) = hook {
            hook.await;
        }
        written
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

/// Move `waba` (and `pn`) to `to`, as the service does: its token deleted
/// before its binding (every unbind), bound to `to`, then, with a `token`,
/// `to`'s token stored (an attach binds, then stores; `None`: an attach
/// between the two).
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
        let bound = self.inner.bind_waba(tenant, waba_id, numbers).await;
        let hook = self.after_bind.lock().unwrap().take();
        if let Some(hook) = hook {
            hook.await;
        }
        bound
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

/// The attach's confirmation (roadmap S2, ruled with its security
/// review's remediation): an attach binds, stores, then reads its binding
/// again, and, when the WABA moved to another tenant meanwhile, takes back
/// exactly its own write and answers `503 storage_unavailable`
/// (retryable), with the app never subscribed with its token. Each case
/// on `store` and `kv` through a harness of its own, with a WABA of its
/// own; merchant-b's move is [`move_waba`], with its token:
///
/// - unmoved: `201`, merchant-a's token stored, the app subscribed;
/// - right after the attach's binding: the attach's read of its binding
///   finds merchant-b's, and nothing is stored: merchant-b's token stays;
/// - right after the attach's store: merchant-b's unbind deleted it and
///   merchant-b stored its own; the confirmation finds merchant-b's
///   binding and takes back the attach's version only: merchant-b's token
///   stays;
/// - between the attach's read of its binding and its store (the window
///   the order narrows and does not close: `server_core::store`): the
///   attach overwrites merchant-b's token, and the confirmation takes its
///   own back, so merchant-b's binding is left with no token, never with
///   merchant-a's.
///
/// Decisive: the binding read again after the store in
/// `Authorizer::store_token` (without it, the last two cases answer
/// `201` and subscribe the app with merchant-a's token, and the last one
/// leaves merchant-a's token under merchant-b's binding); the tenant check
/// on the read before the store (the second case); the version the vault's
/// `store_versioned` hands out (a stale one takes back nothing: the last
/// case).
#[allow(clippy::too_many_lines)] // one scenario per window, read top to bottom
pub async fn an_attach_whose_waba_moves_takes_back_its_token(
    store: Arc<dyn Store>,
    kv: Arc<dyn KvStore>,
) {
    for (case, waba, pn) in [
        ("unmoved", "102290129340501", "1972385232742501"),
        ("after_its_binding", "102290129340502", "1972385232742502"),
        ("after_its_store", "102290129340503", "1972385232742503"),
        (
            "between_its_read_and_its_store",
            "102290129340504",
            "1972385232742504",
        ),
    ] {
        let moving = Arc::new(Moving::new(store.clone()));
        let puts = Arc::new(AfterPut::new(kv.clone()));
        let h = Harness::on(moving.clone(), puts.clone());
        let admin = h.admin_key().await;
        for tenant in ["merchant-a", "merchant-b"] {
            let _ = store
                .create_tenant(&TenantId::parse(tenant).unwrap(), "")
                .await
                .unwrap();
        }
        // Meta lists the WABA's numbers, then subscribes the app.
        h.graph.push_json(
            200,
            json!({"data": [{"id": pn, "display_phone_number": "+1 631-555-1111",
                             "verified_name": "John's Cake Shop", "quality_rating": "GREEN"}]}),
        );
        h.graph.push_json(200, json!({"success": true}));
        let move_to_b: Hook = {
            let (store, vault) = (h.store.clone(), h.vault.clone());
            Box::pin(async move {
                move_waba(
                    store.as_ref(),
                    &vault,
                    &WabaId::new(waba),
                    &PhoneNumberId::new(pn),
                    "merchant-b",
                    Some("TOKEN-OF-B"),
                )
                .await;
            })
        };
        match case {
            "after_its_binding" => moving.after_binding(move_to_b),
            "after_its_store" => puts.after_storing(waba, move_to_b),
            // The attach's reads of the binding: D4's before Meta is
            // asked, then the one before the store.
            "between_its_read_and_its_store" => moving.after_waba_read(2, move_to_b),
            _ => {}
        }
        let reply = h
            .call(
                Call::new(Method::POST, "/v1/admin/tenants/merchant-a/wabas")
                    .key(&admin)
                    .json(&json!({"waba_id": waba, "token": "TOKEN-OF-A"})),
            )
            .await;
        assert!(
            !moving.armed() && !puts.armed(),
            "{case}: the move never ran: {}",
            reply.text
        );
        let holder = h
            .store
            .waba(&WabaId::new(waba))
            .await
            .unwrap()
            .map(|b| b.tenant_id.as_str().to_owned());
        let stored =
            |token: Option<StoredBusinessToken>| token.map(|t| t.token.expose_secret().to_owned());
        let token = stored(h.vault.get(&WabaId::new(waba)).await.unwrap());
        let by_number = stored(
            h.vault
                .get_by_phone_number(&PhoneNumberId::new(pn))
                .await
                .unwrap(),
        );
        let requests = h.graph.requests();
        if case == "unmoved" {
            assert_eq!(reply.status, StatusCode::CREATED, "{case}: {}", reply.text);
            assert_eq!(holder.as_deref(), Some("merchant-a"), "{case}");
            assert_eq!(token.as_deref(), Some("TOKEN-OF-A"), "{case}");
            assert_eq!(by_number.as_deref(), Some("TOKEN-OF-A"), "{case}");
            assert_eq!(requests.len(), 2, "{case}: {requests:?}");
            assert_eq!(h.graph.remaining(), 0, "{case}");
            continue;
        }
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
            "{case}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["retryable"], true, "{case}");
        assert_eq!(holder.as_deref(), Some("merchant-b"), "{case}");
        let left = (case != "between_its_read_and_its_store").then_some("TOKEN-OF-B");
        assert_eq!(token.as_deref(), left, "{case}: merchant-b's WABA's token");
        assert_eq!(by_number.as_deref(), left, "{case}: its number's");
        // Only the listing: the app was never subscribed with
        // merchant-a's token on merchant-b's WABA.
        assert_eq!(requests.len(), 1, "{case}: {requests:?}");
        assert_eq!(requests[0].method, Method::GET, "{case}");
        assert_eq!(h.graph.remaining(), 1, "{case}");
    }
}
