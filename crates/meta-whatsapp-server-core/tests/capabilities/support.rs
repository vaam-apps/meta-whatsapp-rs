//! Shared by `capabilities.rs` and `capabilities_logs.rs`: the service and
//! the forger (one `Authorizer` each, with its records and its vault's
//! store), and the checks both make.
#![allow(dead_code, unused_imports)] // each binary uses a different subset

pub use std::collections::BTreeMap;
pub use std::sync::{Arc, Mutex};

pub use async_trait::async_trait;
pub use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
pub use meta_whatsapp_rs::core::error::{GraphApiError, StorageError, TransportError};
pub use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
pub use meta_whatsapp_rs::core::secret::AccessToken;
pub use meta_whatsapp_rs::core::store::{Expiry, KvStore, StoreKey, Versioned};
pub use meta_whatsapp_rs::core::transport::{HttpRequest, HttpResponse, HttpTransport};
pub use meta_whatsapp_rs::{Client, Error, ErrorKind};
pub use meta_whatsapp_server_core::ServiceError;
pub use meta_whatsapp_server_core::authz::{
    AdminCaller, Authorizer, Caller, OwnedNumber, OwnedWaba,
};
pub use meta_whatsapp_server_core::keys::MintedKey;
pub use meta_whatsapp_server_core::model::{
    ApiKeyRecord, BindOutcome, BindingEpoch, DeleteTenantOutcome, KeyOwner, KeyScope, Listing,
    NewApiKey, NumberBinding, NumberStatus, PageRequest, Scope, Tenant, TenantId, TenantStatus,
    WabaBinding,
};
pub use meta_whatsapp_server_core::store::{RecordStore, StoreResult};
pub use time::OffsetDateTime;

pub const WABA: &str = "102290129340398";
pub const PN: &str = "106540352242922";
pub const VICTIM_TOKEN: &str = "EAAG-token-of-the-victim";
pub const FORGED_TOKEN: &str = "EAAG-token-the-forger-stores";

pub fn victim() -> TenantId {
    TenantId::parse("victim-b").unwrap()
}

pub fn waba() -> WabaId {
    WabaId::new(WABA.to_owned())
}

pub fn pn() -> PhoneNumberId {
    PhoneNumberId::new(PN.to_owned())
}

/// Every entry, by namespace and key: its value and its version.
pub type Entries = BTreeMap<(String, String), (Vec<u8>, u64)>;

/// A key/value store whose contents can be compared before and after.
#[derive(Debug, Default)]
pub struct Kv {
    pub entries: Mutex<Entries>,
    pub version: Mutex<u64>,
}

impl Kv {
    pub fn id(key: &StoreKey) -> (String, String) {
        (key.namespace().to_owned(), key.key().to_owned())
    }

    pub fn next(&self) -> u64 {
        let mut version = self.version.lock().unwrap();
        *version += 1;
        *version
    }

    pub fn snapshot(&self) -> Entries {
        self.entries.lock().unwrap().clone()
    }
}

#[async_trait]
impl KvStore for Kv {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .get(&Self::id(key))
            .map(|(value, version)| Versioned {
                value: value.clone(),
                version: *version,
                expires_at: None,
            }))
    }

    async fn put(&self, key: &StoreKey, value: Vec<u8>, _: Expiry) -> Result<u64, StorageError> {
        let version = self.next();
        self.entries
            .lock()
            .unwrap()
            .insert(Self::id(key), (value, version));
        Ok(version)
    }

    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        _: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        let version = self.next();
        let mut entries = self.entries.lock().unwrap();
        if entries.contains_key(&Self::id(key)) {
            return Ok(None);
        }
        entries.insert(Self::id(key), (value, version));
        Ok(Some(version))
    }

    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        _: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        let version = self.next();
        let mut entries = self.entries.lock().unwrap();
        match entries.get(&Self::id(key)) {
            Some((_, current)) if *current == expected => {}
            _ => return Ok(None),
        }
        if let Some(value) = new {
            entries.insert(Self::id(key), (value, version));
            Ok(Some(version))
        } else {
            entries.remove(&Self::id(key));
            Ok(Some(0))
        }
    }

    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .remove(&Self::id(key))
            .is_some())
    }
}

/// No network: nothing here calls Meta.
#[derive(Debug)]
pub struct NoNet;

#[async_trait]
impl HttpTransport for NoNet {
    async fn send(&self, _: HttpRequest) -> Result<HttpResponse, TransportError> {
        Err(TransportError::Timeout)
    }
}

/// Records that answer fixtures and log every call, reads included.
pub struct Records {
    pub keys: Vec<ApiKeyRecord>,
    pub tenant: Tenant,
    pub number: NumberBinding,
    pub waba: WabaBinding,
    pub calls: Mutex<Vec<&'static str>>,
}

impl Records {
    pub fn new(keys: Vec<ApiKeyRecord>) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            keys,
            tenant: Tenant {
                id: victim(),
                name: "victim".to_owned(),
                status: TenantStatus::Active,
                created_at: now,
                updated_at: now,
            },
            number: NumberBinding {
                phone_number_id: pn(),
                waba_id: waba(),
                tenant_id: victim(),
                status: NumberStatus::Connected,
                updated_at: now,
            },
            waba: WabaBinding {
                waba_id: waba(),
                tenant_id: victim(),
                credit_allocation_id: None,
                attached_at: now,
            },
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn call(&self, name: &'static str) {
        self.calls.lock().unwrap().push(name);
    }

    pub fn take_calls(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

pub fn empty<T>() -> Listing<T> {
    Listing {
        items: Vec::new(),
        next_after: None,
    }
}

#[async_trait]
impl RecordStore for Records {
    async fn ping(&self) -> StoreResult<()> {
        self.call("ping");
        Ok(())
    }
    async fn create_tenant(&self, _: &TenantId, _: &str) -> StoreResult<Option<Tenant>> {
        self.call("create_tenant");
        Ok(None)
    }
    async fn tenant(&self, id: &TenantId) -> StoreResult<Option<Tenant>> {
        self.call("tenant");
        Ok((id == &self.tenant.id).then(|| self.tenant.clone()))
    }
    async fn tenants(&self, _: &PageRequest) -> StoreResult<Listing<Tenant>> {
        self.call("tenants");
        Ok(empty())
    }
    async fn update_tenant(
        &self,
        _: &TenantId,
        _: Option<&str>,
        _: Option<TenantStatus>,
    ) -> StoreResult<Option<Tenant>> {
        self.call("update_tenant");
        Ok(None)
    }
    async fn delete_tenant(&self, _: &TenantId) -> StoreResult<DeleteTenantOutcome> {
        self.call("delete_tenant");
        Ok(DeleteTenantOutcome::NotFound)
    }
    async fn insert_key(&self, _: &NewApiKey) -> StoreResult<Option<ApiKeyRecord>> {
        self.call("insert_key");
        Ok(None)
    }
    async fn key(&self, key_id: &str) -> StoreResult<Option<ApiKeyRecord>> {
        self.call("key");
        Ok(self.keys.iter().find(|k| k.key_id == key_id).cloned())
    }
    async fn keys(&self, _: &KeyScope, _: &PageRequest) -> StoreResult<Listing<ApiKeyRecord>> {
        self.call("keys");
        Ok(empty())
    }
    async fn revoke_key(&self, _: &KeyScope, _: &str) -> StoreResult<bool> {
        self.call("revoke_key");
        Ok(false)
    }
    async fn touch_key(&self, _: &str) -> StoreResult<()> {
        self.call("touch_key");
        Ok(())
    }
    async fn bind_waba(
        &self,
        _: &TenantId,
        _: &WabaId,
        _: &[PhoneNumberId],
    ) -> StoreResult<BindOutcome> {
        self.call("bind_waba");
        Ok(BindOutcome::OwnedByAnotherTenant)
    }
    async fn unbind_waba(&self, _: &WabaId) -> StoreResult<bool> {
        self.call("unbind_waba");
        Ok(true)
    }
    async fn waba(&self, waba_id: &WabaId) -> StoreResult<Option<WabaBinding>> {
        self.call("waba");
        Ok((waba_id == &self.waba.waba_id).then(|| self.waba.clone()))
    }
    async fn number(&self, pn: &PhoneNumberId) -> StoreResult<Option<NumberBinding>> {
        self.call("number");
        Ok((pn == &self.number.phone_number_id).then(|| self.number.clone()))
    }
    async fn all_wabas(&self, _: &PageRequest) -> StoreResult<Listing<WabaBinding>> {
        self.call("all_wabas");
        Ok(Listing {
            items: vec![self.waba.clone()],
            next_after: None,
        })
    }
    async fn wabas(&self, _: &TenantId, _: &PageRequest) -> StoreResult<Listing<WabaBinding>> {
        self.call("wabas");
        Ok(empty())
    }
    async fn waba_numbers(&self, _: &WabaId) -> StoreResult<Vec<NumberBinding>> {
        self.call("waba_numbers");
        Ok(Vec::new())
    }
    async fn numbers(&self, _: &TenantId, _: &PageRequest) -> StoreResult<Listing<NumberBinding>> {
        self.call("numbers");
        Ok(empty())
    }
    async fn set_waba_status(&self, _: &WabaId, _: NumberStatus) -> StoreResult<()> {
        self.call("set_waba_status");
        Ok(())
    }
    async fn unbind_waba_if(&self, _: &BindingEpoch) -> StoreResult<bool> {
        self.call("unbind_waba_if");
        Ok(true)
    }
    async fn set_waba_status_if(&self, _: &BindingEpoch, _: NumberStatus) -> StoreResult<bool> {
        self.call("set_waba_status_if");
        Ok(true)
    }
}

/// A key of `owner`'s, with every scope a tenant route needs here.
pub fn key(owner: KeyOwner) -> (String, ApiKeyRecord) {
    let minted = MintedKey::generate().unwrap();
    let record = ApiKeyRecord {
        key_id: minted.key_id().to_owned(),
        secret_sha256: minted.digest(),
        owner,
        scopes: vec![Scope::Send],
        name: "k".to_owned(),
        created_at: OffsetDateTime::now_utc(),
        expires_at: None,
        revoked_at: None,
        last_used_at: None,
    };
    (format!("Bearer {}", minted.expose_key()), record)
}

pub fn tokenless_client() -> Client {
    Client::builder().transport(NoNet).build().unwrap()
}

/// One `Authorizer`, its records and its vault's store, and the bearer
/// values of its admin key and of a key of the victim tenant.
pub struct Side {
    pub authz: Authorizer,
    pub records: Arc<Records>,
    pub kv: Arc<Kv>,
    pub admin_bearer: String,
    pub tenant_bearer: String,
}

impl Side {
    /// An `Authorizer` whose vault holds `token` for the WABA and its
    /// number, stored with its own admin's capability.
    pub async fn new(vault_key_id: &str, token: &str) -> Self {
        let (admin_bearer, admin) = key(KeyOwner::Admin);
        let (tenant_bearer, tenant) = key(KeyOwner::Tenant(victim()));
        let records = Arc::new(Records::new(vec![admin, tenant]));
        let kv = Arc::new(Kv::default());
        let vault = TokenVault::new(
            kv.clone(),
            VaultKeys::new(VaultKey::generate(vault_key_id).unwrap()),
        )
        .unwrap();
        let authz = Authorizer::new(records.clone(), vault, tokenless_client()).unwrap();
        let side = Self {
            authz,
            records,
            kv,
            admin_bearer,
            tenant_bearer,
        };
        let admin = side.admin().await;
        side.authz
            .store_token(&admin, &victim(), &stored(token))
            .await
            .unwrap();
        side.records.take_calls();
        side
    }

    pub async fn admin(&self) -> AdminCaller {
        self.authz
            .admin_caller(Some(&self.admin_bearer))
            .await
            .unwrap()
    }

    pub async fn caller(&self) -> Caller {
        self.authz
            .tenant_caller(Some(&self.tenant_bearer), None, Scope::Send)
            .await
            .unwrap()
    }

    pub async fn number(&self) -> OwnedNumber {
        let caller = self.caller().await;
        self.authz.owned_number(&caller, pn()).await.unwrap()
    }

    pub async fn waba(&self) -> OwnedWaba {
        let caller = self.caller().await;
        self.authz.owned_waba(&caller, waba()).await.unwrap()
    }
}

pub fn stored(token: &str) -> StoredBusinessToken {
    StoredBusinessToken::new(waba(), AccessToken::new(token)).phone_number_ids([pn()])
}

/// Meta refusing a token: `190`, which marks a WABA's numbers
/// `reconnect_required` when the capability is accepted.
pub fn refused_token() -> Error {
    let graph: GraphApiError = serde_json::from_value(serde_json::json!({
        "message": "Error validating access token: Session has expired",
        "type": "OAuthException",
        "code": 190,
        "fbtrace_id": "AbCdEf"
    }))
    .unwrap();
    let error = Error::Api(Box::new(graph));
    assert_eq!(error.kind(), ErrorKind::Authentication);
    error
}

/// The service (B, holding the victim's token) and the forger (A, a
/// throwaway `Authorizer` over records of its own).
pub async fn service_and_forger() -> (Side, Side) {
    let service = Side::new("service", VICTIM_TOKEN).await;
    let forger = Side::new("forger", FORGED_TOKEN).await;
    (service, forger)
}

/// What B's vault and records hold, to compare after a refusal.
pub struct Before {
    pub kv: Entries,
}

impl Before {
    pub fn of(side: &Side) -> Self {
        side.records.take_calls();
        Self {
            kv: side.kv.snapshot(),
        }
    }

    /// B read nothing and wrote nothing, and still answers the victim's
    /// token to the victim's own capability.
    pub async fn unchanged(self, side: &Side, what: &str) {
        assert_eq!(
            side.records.take_calls(),
            Vec::<&str>::new(),
            "{what}: the service's records were reached"
        );
        assert!(
            side.kv.snapshot() == self.kv,
            "{what}: the service's vault changed"
        );
        let own = side.number().await;
        assert_eq!(
            own.client().token().unwrap().expose_secret(),
            VICTIM_TOKEN,
            "{what}: the service's own capability"
        );
    }
}

pub fn assert_forbidden(error: &ServiceError, what: &str) {
    assert_eq!(error.code(), "forbidden", "{what}: {error:?}");
    assert_eq!(error.status(), 403, "{what}");
}
