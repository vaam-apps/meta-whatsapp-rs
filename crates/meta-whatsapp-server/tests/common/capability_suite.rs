//! Roadmap S2 (the core's security review, SR-L2, and the same race in
//! `failed`): a capability acts only on the WABA's binding and the vault
//! record it was made from. Run on memory (`tests/store.rs`) and on
//! Postgres (`tests/live_postgres.rs`), through the core's `Authorizer`
//! over the backend's records and a vault on its key/value store.

use std::sync::Arc;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::error::GraphApiError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::store::KvStore;
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_server::api::admin::mint;
use meta_whatsapp_server::auth::{Authorizer, Caller};
use meta_whatsapp_server::model::{
    ApiKeyRecord, BindOutcome, BindingEpoch, DeleteTenantOutcome, KeyOwner, KeyScope, Listing,
    NewApiKey, NumberBinding, NumberStatus, PageRequest, Scope, Tenant, TenantId, TenantStatus,
    WabaBinding,
};
use meta_whatsapp_server::store::{RecordStore, Store, StoreResult};

/// The tenant the capabilities are made for, and the one a WABA moves to.
const A: &str = "cap-a";
const B: &str = "cap-b";

struct Setup {
    store: Arc<dyn Store>,
    vault: TokenVault,
    authz: Authorizer,
    caller: Caller,
}

impl Setup {
    async fn new(store: Arc<dyn Store>, kv: Arc<dyn KvStore>) -> Self {
        for tenant in [A, B] {
            let _ = store
                .create_tenant(&TenantId::parse(tenant).unwrap(), "")
                .await
                .unwrap();
        }
        let vault =
            TokenVault::new(kv, VaultKeys::new(VaultKey::generate("cap").unwrap())).unwrap();
        let client = Client::builder()
            .transport(ScriptedTransport::new())
            .build()
            .unwrap();
        let records: Arc<dyn RecordStore> = store.clone();
        let authz = Authorizer::new(records, vault.clone(), client).unwrap();
        let (key, _) = mint(
            store.as_ref(),
            KeyOwner::Tenant(TenantId::parse(A).unwrap()),
            vec![Scope::Numbers],
            String::new(),
            None,
        )
        .await
        .unwrap();
        let caller = authz
            .tenant_caller(
                Some(&format!("Bearer {}", key.expose_key())),
                None,
                Scope::Numbers,
            )
            .await
            .unwrap();
        Self {
            store,
            vault,
            authz,
            caller,
        }
    }

    /// Bind `waba` and `pn` to `tenant`, as attaching does, and, with a
    /// `token`, store it (a new vault record).
    async fn attach(&self, tenant: &str, waba: &WabaId, pn: &PhoneNumberId, token: Option<&str>) {
        let bound = self
            .store
            .bind_waba(
                &TenantId::parse(tenant).unwrap(),
                waba,
                std::slice::from_ref(pn),
            )
            .await
            .unwrap();
        assert_eq!(bound, meta_whatsapp_server::model::BindOutcome::Bound);
        if let Some(token) = token {
            self.vault
                .store(
                    &StoredBusinessToken::new(waba.clone(), AccessToken::new(token))
                        .phone_number_ids([pn.clone()]),
                )
                .await
                .unwrap();
        }
    }

    /// The WABA unbound, then attached again to `tenant`, with a new token
    /// or none yet (an attach between its binding and its token).
    async fn attached_again(
        &self,
        tenant: &str,
        waba: &WabaId,
        pn: &PhoneNumberId,
        token: Option<&str>,
    ) {
        assert!(self.store.unbind_waba(waba).await.unwrap());
        self.attach(tenant, waba, pn, token).await;
    }

    async fn holder(&self, waba: &WabaId) -> Option<String> {
        self.store
            .waba(waba)
            .await
            .unwrap()
            .map(|binding| binding.tenant_id.as_str().to_owned())
    }

    async fn token(&self, waba: &WabaId) -> Option<String> {
        self.vault
            .get(waba)
            .await
            .unwrap()
            .map(|token| token.token.expose_secret().to_owned())
    }

    async fn status(&self, pn: &PhoneNumberId) -> Option<NumberStatus> {
        self.store
            .number(pn)
            .await
            .unwrap()
            .map(|number| number.status)
    }
}

/// Meta refusing a token: `190`.
fn refused_token() -> meta_whatsapp_rs::Error {
    let graph: GraphApiError = serde_json::from_value(serde_json::json!({
        "message": "Error validating access token: Session has expired",
        "type": "OAuthException",
        "code": 190,
        "fbtrace_id": "AbCdEf"
    }))
    .unwrap();
    meta_whatsapp_rs::Error::Api(Box::new(graph))
}

/// A WABA and a number of this case's own.
fn ids(case: &str) -> (WabaId, PhoneNumberId) {
    (
        WabaId::new(format!("cap-waba-{case}")),
        PhoneNumberId::new(format!("cap-pn-{case}")),
    )
}

/// SR-L2: an `OwnedWaba` made before its WABA was unbound and attached
/// again (to the same tenant, and to another) forgets neither the new
/// binding nor the new token; with the new token not stored yet (an
/// attach between its binding and its token), still not the new binding.
/// Refreshed for its own tenant with a new token (no unbind: the binding
/// keeps its epoch), it forgets neither either. Unmoved, it forgets both.
/// Decisive: the vault version in `forget`
/// (`TokenVault::delete_if_unchanged`, and `forget` stopping when it
/// deleted nothing) for the first and the third, the binding's epoch
/// (`RecordStore::unbind_waba_if`) for the second.
pub async fn forget_leaves_a_waba_attached_again(store: Arc<dyn Store>, kv: Arc<dyn KvStore>) {
    let s = Setup::new(store, kv).await;
    for target in [A, B] {
        for new_token in [Some("TOKEN-NEW"), None] {
            let case = format!("forget-{target}-{}", new_token.is_some());
            let (waba, pn) = ids(&case);
            s.attach(A, &waba, &pn, Some("TOKEN-OLD")).await;
            let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
            s.attached_again(target, &waba, &pn, new_token).await;
            assert!(
                !owned.forget(&s.authz).await.unwrap(),
                "{case}: forgot what moved"
            );
            assert_eq!(
                s.holder(&waba).await.as_deref(),
                Some(target),
                "{case}: the new binding went"
            );
            if let Some(token) = new_token {
                assert_eq!(
                    s.token(&waba).await.as_deref(),
                    Some(token),
                    "{case}: the new token went"
                );
            }
        }
    }
    // Attached again to its own tenant without an unbind (a new token, as
    // an operator's reconnect does): the binding keeps its epoch, the vault
    // record moved. Neither goes: the binding is not unbound on the old
    // token's word.
    let (waba, pn) = ids("forget-refreshed");
    s.attach(A, &waba, &pn, Some("TOKEN-OLD")).await;
    let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
    s.attach(A, &waba, &pn, Some("TOKEN-NEW")).await;
    assert!(
        !owned.forget(&s.authz).await.unwrap(),
        "forget-refreshed: forgot a binding refreshed with a new token"
    );
    assert_eq!(
        s.holder(&waba).await.as_deref(),
        Some(A),
        "forget-refreshed"
    );
    assert_eq!(
        s.token(&waba).await.as_deref(),
        Some("TOKEN-NEW"),
        "forget-refreshed"
    );
    // Unmoved: both go.
    let (waba, pn) = ids("forget-unmoved");
    s.attach(A, &waba, &pn, Some("TOKEN-OLD")).await;
    let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
    assert!(owned.forget(&s.authz).await.unwrap());
    assert_eq!(s.holder(&waba).await, None);
    assert_eq!(s.token(&waba).await, None);
}

/// The `failed` race: an `OwnedNumber` or an `OwnedWaba` made before its
/// WABA was unbound and attached again, to the same tenant and to
/// another, whose call Meta then answers `190`, leaves the new binding's
/// numbers `connected` in both cases; unmoved, it marks them
/// `reconnect_required`. Decisive: the binding's epoch in
/// `RecordStore::set_waba_status_if` (conditioning on the tenant alone
/// fails the same-tenant case, on nothing both).
pub async fn failed_leaves_a_waba_attached_again(store: Arc<dyn Store>, kv: Arc<dyn KvStore>) {
    let s = Setup::new(store, kv).await;
    for through_number in [true, false] {
        for target in [A, B] {
            let case = format!("failed-{through_number}-{target}");
            let (waba, pn) = ids(&case);
            s.attach(A, &waba, &pn, Some("TOKEN-OLD")).await;
            let failed = if through_number {
                let owned = s.authz.owned_number(&s.caller, pn.clone()).await.unwrap();
                s.attached_again(target, &waba, &pn, Some("TOKEN-NEW"))
                    .await;
                owned.failed(&s.authz, &refused_token()).await
            } else {
                let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
                s.attached_again(target, &waba, &pn, Some("TOKEN-NEW"))
                    .await;
                owned.failed(&s.authz, &refused_token()).await
            };
            assert_eq!(failed.code(), "reconnect_required", "{case}");
            assert_eq!(
                s.status(&pn).await,
                Some(NumberStatus::Connected),
                "{case}: the new binding's number was marked"
            );
        }
        // Unmoved: marked.
        let case = format!("failed-{through_number}-unmoved");
        let (waba, pn) = ids(&case);
        s.attach(A, &waba, &pn, Some("TOKEN-OLD")).await;
        let failed = if through_number {
            let owned = s.authz.owned_number(&s.caller, pn.clone()).await.unwrap();
            owned.failed(&s.authz, &refused_token()).await
        } else {
            let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
            owned.failed(&s.authz, &refused_token()).await
        };
        assert_eq!(failed.code(), "reconnect_required", "{case}");
        assert_eq!(
            s.status(&pn).await,
            Some(NumberStatus::ReconnectRequired),
            "{case}"
        );
    }
}

/// What a capability does, raced against its WABA being unbound and
/// attached again ([`racing_a_reattach`]).
#[derive(Debug, Clone, Copy)]
pub enum Act {
    /// `OwnedWaba::forget`.
    Forget,
    /// `OwnedNumber::failed` after Meta's `190`.
    NumberFailed,
    /// `OwnedWaba::failed` after Meta's `190`.
    WabaFailed,
}

/// A capability's conditioned write raced, on two tasks, against its WABA
/// being unbound and attached again (to the same tenant and to another,
/// round by round) with a new token: whichever lands first, the new
/// binding, its new token and its number's `connected` status survive.
/// `forget` first unbinds the old binding, which the attach then makes
/// again; the attach first leaves `forget` and `failed` nothing to act on.
/// Never the new binding's. Decisive (as far as the rounds meet): the
/// conditions of `delete_if_unchanged`, `unbind_waba_if` and
/// `set_waba_status_if`.
pub async fn racing_a_reattach(
    store: Arc<dyn Store>,
    kv: Arc<dyn KvStore>,
    act: Act,
    rounds: usize,
) {
    let s = Arc::new(Setup::new(store, kv).await);
    for round in 0..rounds {
        let target = if round % 2 == 0 { A } else { B };
        let case = format!("race-{act:?}-{round}-{}", super::unique());
        let (waba, pn) = ids(&case);
        s.attach(A, &waba, &pn, Some("TOKEN-OLD")).await;
        let acted = match act {
            Act::Forget => {
                let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
                let s = s.clone();
                tokio::spawn(async move {
                    super::race_suite::jitter(round, 0).await;
                    owned
                        .forget(&s.authz)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.code())
                })
            }
            Act::NumberFailed => {
                let owned = s.authz.owned_number(&s.caller, pn.clone()).await.unwrap();
                let s = s.clone();
                tokio::spawn(async move {
                    super::race_suite::jitter(round, 0).await;
                    let failed = owned.failed(&s.authz, &refused_token()).await;
                    assert_eq!(failed.code(), "reconnect_required");
                    Ok(())
                })
            }
            Act::WabaFailed => {
                let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
                let s = s.clone();
                tokio::spawn(async move {
                    super::race_suite::jitter(round, 0).await;
                    let failed = owned.failed(&s.authz, &refused_token()).await;
                    assert_eq!(failed.code(), "reconnect_required");
                    Ok(())
                })
            }
        };
        let attached = {
            let (s, waba, pn) = (s.clone(), waba.clone(), pn.clone());
            tokio::spawn(async move {
                super::race_suite::jitter(round, 1).await;
                // `forget` may have unbound it first.
                let _ = s.store.unbind_waba(&waba).await.unwrap();
                s.attach(target, &waba, &pn, Some("TOKEN-NEW")).await;
            })
        };
        let (acted, attached) = tokio::join!(acted, attached);
        attached.unwrap();
        acted
            .unwrap()
            .unwrap_or_else(|code| panic!("{case}: the capability failed: {code}"));
        assert_eq!(
            s.holder(&waba).await.as_deref(),
            Some(target),
            "{case}: the new binding went"
        );
        assert_eq!(
            s.token(&waba).await.as_deref(),
            Some("TOKEN-NEW"),
            "{case}: the new token went"
        );
        assert_eq!(
            s.status(&pn).await,
            Some(NumberStatus::Connected),
            "{case}: the new binding's number was marked"
        );
    }
}

type Hook = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// A `RecordStore` that runs a hook once, right after the next WABA
/// binding it reads (`waba`): the binding moves between a capability's
/// read of it and its read of the vault.
struct MovesAfterRead {
    inner: Arc<dyn Store>,
    hook: std::sync::Mutex<Option<Hook>>,
    /// Run after the next number read instead (`number`).
    after_number: std::sync::Mutex<Option<Hook>>,
}

impl MovesAfterRead {
    fn arm(&self, hook: Hook) {
        *self.hook.lock().unwrap() = Some(hook);
    }

    fn arm_after_number(&self, hook: Hook) {
        *self.after_number.lock().unwrap() = Some(hook);
    }
}

#[async_trait::async_trait]
impl RecordStore for MovesAfterRead {
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
        let hook = self.hook.lock().unwrap().take();
        if let Some(hook) = hook {
            hook.await;
        }
        read
    }
    async fn number(&self, phone_number_id: &PhoneNumberId) -> StoreResult<Option<NumberBinding>> {
        let read = self.inner.number(phone_number_id).await;
        let hook = self.after_number.lock().unwrap().take();
        if let Some(hook) = hook {
            hook.await;
        }
        read
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

/// SR-L2 when the capability is made: a WABA unbound and attached to
/// another tenant, with that tenant's token, between the capability's read
/// of the binding and its read of the vault, yields no capability (`503
/// storage_unavailable`, retryable), never the old holder's binding with
/// the new holder's token, which it would call Meta with (unsubscribing
/// the new holder's app, sending from its number) and which `forget` would
/// delete. For an `OwnedWaba` and an `OwnedNumber` (the move in the
/// middle of their making), and for an admin's `OwnedWaba` made from a
/// binding read earlier (a tenant deletion walking its WABAs, one moved
/// meanwhile). An `OwnedNumber` whose WABA moves between its number's read
/// and its WABA's is `404 not_found`: the WABA read is another tenant's.
/// The new holder keeps its binding, its token and its number's status.
/// Decisive: the binding read again after the vault, in
/// `Authorizer::open` and `Authorizer::owned_number`, and the tenant
/// filter on `owned_number`'s WABA read.
#[allow(clippy::too_many_lines)] // one scenario per capability, read top to bottom
pub async fn a_capability_made_while_its_waba_moves_is_refused(
    store: Arc<dyn Store>,
    kv: Arc<dyn KvStore>,
) {
    let s = Arc::new(Setup::new(store.clone(), kv).await);
    let moving = Arc::new(MovesAfterRead {
        inner: store.clone(),
        hook: std::sync::Mutex::new(None),
        after_number: std::sync::Mutex::new(None),
    });
    let records: Arc<dyn RecordStore> = moving.clone();
    let authz = Authorizer::new(
        records,
        s.vault.clone(),
        Client::builder()
            .transport(ScriptedTransport::new())
            .build()
            .unwrap(),
    )
    .unwrap();
    let bearer = |owner: KeyOwner, scopes: Vec<Scope>| {
        let store = store.clone();
        async move {
            let (key, _) = mint(store.as_ref(), owner, scopes, String::new(), None)
                .await
                .unwrap();
            format!("Bearer {}", key.expose_key())
        }
    };
    let tenant_key = bearer(
        KeyOwner::Tenant(TenantId::parse(A).unwrap()),
        vec![Scope::Numbers],
    )
    .await;
    let caller = authz
        .tenant_caller(Some(&tenant_key), None, Scope::Numbers)
        .await
        .unwrap();
    let admin_key = bearer(KeyOwner::Admin, vec![]).await;
    let admin = authz.admin_caller(Some(&admin_key)).await.unwrap();
    let move_to_b = |waba: &WabaId, pn: &PhoneNumberId| -> Hook {
        let (s, waba, pn) = (s.clone(), waba.clone(), pn.clone());
        Box::pin(async move { s.attached_again(B, &waba, &pn, Some("TOKEN-OF-B")).await })
    };
    for how in [
        "owned_waba",
        "owned_number",
        "owned_number_after_its_number",
        "waba_for_admin",
    ] {
        let case = format!("made-while-moving-{how}");
        let (waba, pn) = ids(&case);
        s.attach(A, &waba, &pn, Some("TOKEN-OF-A")).await;
        let made = match how {
            "owned_waba" => {
                moving.arm(move_to_b(&waba, &pn));
                authz.owned_waba(&caller, waba.clone()).await.map(|owned| {
                    let token = owned.client().token().map(|t| t.expose_secret().to_owned());
                    (Some(owned), token)
                })
            }
            "owned_number" => {
                moving.arm(move_to_b(&waba, &pn));
                authz.owned_number(&caller, pn.clone()).await.map(|owned| {
                    let token = owned.client().token().map(|t| t.expose_secret().to_owned());
                    (None, token)
                })
            }
            "owned_number_after_its_number" => {
                // Between the number's read and its WABA's: the WABA read
                // is the new holder's binding, whose own token follows.
                moving.arm_after_number(move_to_b(&waba, &pn));
                authz.owned_number(&caller, pn.clone()).await.map(|owned| {
                    let token = owned.client().token().map(|t| t.expose_secret().to_owned());
                    (None, token)
                })
            }
            _ => {
                // Read as a tenant deletion lists its WABAs, then moved.
                let listed = store.waba(&waba).await.unwrap().unwrap();
                move_to_b(&waba, &pn).await;
                authz.waba_for_admin(&admin, &listed).await.map(|owned| {
                    let token = owned.client().token().map(|t| t.expose_secret().to_owned());
                    (Some(owned), token)
                })
            }
        };
        // Moved after its WABA's read: `503`, repeat; before it, the WABA
        // read is already another tenant's: not this tenant's number.
        let refused = if how == "owned_number_after_its_number" {
            "not_found"
        } else {
            "storage_unavailable"
        };
        match made {
            Err(error) => assert_eq!(error.code(), refused, "{case}"),
            Ok((owned, token)) => {
                // What the capability would do with the new holder's token.
                if let Some(owned) = owned {
                    let _ = owned.forget(&authz).await;
                }
                panic!("{case}: made a capability of {A}'s binding with the token {token:?}");
            }
        }
        assert_eq!(s.holder(&waba).await.as_deref(), Some(B), "{case}");
        assert_eq!(
            s.token(&waba).await.as_deref(),
            Some("TOKEN-OF-B"),
            "{case}"
        );
        assert_eq!(s.status(&pn).await, Some(NumberStatus::Connected), "{case}");
    }
}

/// M1 (the security review of S2), before the new holder's token is
/// stored: the WABA moves as the service moves one (the old token deleted,
/// then the binding; bound to another tenant, whose attach has not stored
/// its token yet) between a capability's read of the binding and its read
/// of the vault, which then finds no token. That absence is the new
/// holder's state: no capability, `503 storage_unavailable` (retryable), as
/// for any move, never `409 number_not_connected` (on which the admin
/// unbind deletes the WABA's binding without a capability). For an
/// `OwnedWaba`, an `OwnedNumber` and an admin's `OwnedWaba` made from a
/// binding read before the move. The new holder keeps its binding and its
/// number's status. Decisive: `Authorizer::open` reading the binding again
/// before it answers for the vault's read.
pub async fn a_capability_made_before_the_new_token_is_stored_is_refused(
    store: Arc<dyn Store>,
    kv: Arc<dyn KvStore>,
) {
    let s = Arc::new(Setup::new(store.clone(), kv).await);
    let moving = Arc::new(MovesAfterRead {
        inner: store.clone(),
        hook: std::sync::Mutex::new(None),
        after_number: std::sync::Mutex::new(None),
    });
    let records: Arc<dyn RecordStore> = moving.clone();
    let authz = Authorizer::new(
        records,
        s.vault.clone(),
        Client::builder()
            .transport(ScriptedTransport::new())
            .build()
            .unwrap(),
    )
    .unwrap();
    let key = |owner: KeyOwner, scopes: Vec<Scope>| {
        let store = store.clone();
        async move {
            let (key, _) = mint(store.as_ref(), owner, scopes, String::new(), None)
                .await
                .unwrap();
            format!("Bearer {}", key.expose_key())
        }
    };
    let tenant_key = key(
        KeyOwner::Tenant(TenantId::parse(A).unwrap()),
        vec![Scope::Numbers],
    )
    .await;
    let caller = authz
        .tenant_caller(Some(&tenant_key), None, Scope::Numbers)
        .await
        .unwrap();
    let admin_key = key(KeyOwner::Admin, vec![]).await;
    let admin = authz.admin_caller(Some(&admin_key)).await.unwrap();
    // The old token first, then the binding (every unbind); bound to B,
    // no token yet.
    let move_to_b = |waba: &WabaId, pn: &PhoneNumberId| -> Hook {
        let (s, waba, pn) = (s.clone(), waba.clone(), pn.clone());
        Box::pin(async move {
            s.vault.delete(&waba).await.unwrap();
            s.attached_again(B, &waba, &pn, None).await;
        })
    };
    for how in ["owned_waba", "owned_number", "waba_for_admin"] {
        let case = format!("made-before-the-new-token-{how}");
        let (waba, pn) = ids(&case);
        s.attach(A, &waba, &pn, Some("TOKEN-OF-A")).await;
        let made = match how {
            "owned_waba" => {
                moving.arm(move_to_b(&waba, &pn));
                authz.owned_waba(&caller, waba.clone()).await.map(drop)
            }
            "owned_number" => {
                moving.arm(move_to_b(&waba, &pn));
                authz.owned_number(&caller, pn.clone()).await.map(drop)
            }
            _ => {
                let listed = store.waba(&waba).await.unwrap().unwrap();
                move_to_b(&waba, &pn).await;
                authz.waba_for_admin(&admin, &listed).await.map(drop)
            }
        };
        let refused = made.expect_err(&case);
        assert_eq!(refused.code(), "storage_unavailable", "{case}");
        assert_eq!(s.holder(&waba).await.as_deref(), Some(B), "{case}");
        assert_eq!(s.token(&waba).await, None, "{case}");
        assert_eq!(s.status(&pn).await, Some(NumberStatus::Connected), "{case}");
    }
}

/// Everything above.
pub async fn run(store: Arc<dyn Store>, kv: Arc<dyn KvStore>) {
    forget_leaves_a_waba_attached_again(store.clone(), kv.clone()).await;
    failed_leaves_a_waba_attached_again(store.clone(), kv.clone()).await;
    a_capability_made_before_the_new_token_is_stored_is_refused(store.clone(), kv.clone()).await;
    a_capability_made_while_its_waba_moves_is_refused(store, kv).await;
}
