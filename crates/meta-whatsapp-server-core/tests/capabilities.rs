//! Capabilities are branded with the `Authorizer` that made them.
//!
//! The security review of `6309a5a` proved a bypass: `Authorizer::new` is
//! public, so any crate could build a throwaway `Authorizer` over records
//! of its own (a key it wrote, the victim's tenant as that key's owner),
//! have it make an `AdminCaller` or a `Caller`, and hand those to the
//! service's `Authorizer`, which accepted them and read, wrote, rotated
//! and deleted its vault's records. This is that proof as a test, from
//! outside the crate as the forger is: every capability the forger's
//! `Authorizer` (A) makes is refused by every method of the service's (B)
//! with `403 forbidden`, and B's vault and records are neither read nor
//! written. Decisive: `Issuer::is` (the `Arc::ptr_eq` of `authz.rs`), and
//! each method's own check.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::error::{GraphApiError, StorageError, TransportError};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::store::{Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_rs::core::transport::{HttpRequest, HttpResponse, HttpTransport};
use meta_whatsapp_rs::{Client, Error, ErrorKind};
use meta_whatsapp_server_core::ServiceError;
use meta_whatsapp_server_core::authz::{AdminCaller, Authorizer, Caller, OwnedNumber, OwnedWaba};
use meta_whatsapp_server_core::keys::MintedKey;
use meta_whatsapp_server_core::model::{
    ApiKeyRecord, BindOutcome, DeleteTenantOutcome, KeyOwner, KeyScope, Listing, NewApiKey,
    NumberBinding, NumberStatus, PageRequest, Scope, Tenant, TenantId, TenantStatus, WabaBinding,
};
use meta_whatsapp_server_core::store::{RecordStore, StoreResult};
use time::OffsetDateTime;

const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const VICTIM_TOKEN: &str = "EAAG-token-of-the-victim";
const FORGED_TOKEN: &str = "EAAG-token-the-forger-stores";

fn victim() -> TenantId {
    TenantId::parse("victim-b").unwrap()
}

fn waba() -> WabaId {
    WabaId::new(WABA.to_owned())
}

fn pn() -> PhoneNumberId {
    PhoneNumberId::new(PN.to_owned())
}

/// Every entry, by namespace and key: its value and its version.
type Entries = BTreeMap<(String, String), (Vec<u8>, u64)>;

/// A key/value store whose contents can be compared before and after.
#[derive(Debug, Default)]
struct Kv {
    entries: Mutex<Entries>,
    version: Mutex<u64>,
}

impl Kv {
    fn id(key: &StoreKey) -> (String, String) {
        (key.namespace().to_owned(), key.key().to_owned())
    }

    fn next(&self) -> u64 {
        let mut version = self.version.lock().unwrap();
        *version += 1;
        *version
    }

    fn snapshot(&self) -> Entries {
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
struct NoNet;

#[async_trait]
impl HttpTransport for NoNet {
    async fn send(&self, _: HttpRequest) -> Result<HttpResponse, TransportError> {
        Err(TransportError::Timeout)
    }
}

/// Records that answer fixtures and log every call, reads included.
struct Records {
    keys: Vec<ApiKeyRecord>,
    tenant: Tenant,
    number: NumberBinding,
    waba: WabaBinding,
    calls: Mutex<Vec<&'static str>>,
}

impl Records {
    fn new(keys: Vec<ApiKeyRecord>) -> Self {
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

    fn call(&self, name: &'static str) {
        self.calls.lock().unwrap().push(name);
    }

    fn take_calls(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

fn empty<T>() -> Listing<T> {
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
}

/// A key of `owner`'s, with every scope a tenant route needs here.
fn key(owner: KeyOwner) -> (String, ApiKeyRecord) {
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

fn tokenless_client() -> Client {
    Client::builder().transport(NoNet).build().unwrap()
}

/// One `Authorizer`, its records and its vault's store, and the bearer
/// values of its admin key and of a key of the victim tenant.
struct Side {
    authz: Authorizer,
    records: Arc<Records>,
    kv: Arc<Kv>,
    admin_bearer: String,
    tenant_bearer: String,
}

impl Side {
    /// An `Authorizer` whose vault holds `token` for the WABA and its
    /// number, stored with its own admin's capability.
    async fn new(vault_key_id: &str, token: &str) -> Self {
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
            .store_token(&admin, &stored(token))
            .await
            .unwrap();
        side.records.take_calls();
        side
    }

    async fn admin(&self) -> AdminCaller {
        self.authz
            .admin_caller(Some(&self.admin_bearer))
            .await
            .unwrap()
    }

    async fn caller(&self) -> Caller {
        self.authz
            .tenant_caller(Some(&self.tenant_bearer), None, Scope::Send)
            .await
            .unwrap()
    }

    async fn number(&self) -> OwnedNumber {
        let caller = self.caller().await;
        self.authz.owned_number(&caller, pn()).await.unwrap()
    }

    async fn waba(&self) -> OwnedWaba {
        let caller = self.caller().await;
        self.authz.owned_waba(&caller, waba()).await.unwrap()
    }
}

fn stored(token: &str) -> StoredBusinessToken {
    StoredBusinessToken::new(waba(), AccessToken::new(token)).phone_number_ids([pn()])
}

/// Meta refusing a token: `190`, which marks a WABA's numbers
/// `reconnect_required` when the capability is accepted.
fn refused_token() -> Error {
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
async fn service_and_forger() -> (Side, Side) {
    let service = Side::new("service", VICTIM_TOKEN).await;
    let forger = Side::new("forger", FORGED_TOKEN).await;
    (service, forger)
}

/// What B's vault and records hold, to compare after a refusal.
struct Before {
    kv: Entries,
}

impl Before {
    fn of(side: &Side) -> Self {
        side.records.take_calls();
        Self {
            kv: side.kv.snapshot(),
        }
    }

    /// B read nothing and wrote nothing, and still answers the victim's
    /// token to the victim's own capability.
    async fn unchanged(self, side: &Side, what: &str) {
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

fn assert_forbidden(error: &ServiceError, what: &str) {
    assert_eq!(error.code(), "forbidden", "{what}: {error:?}");
    assert_eq!(error.status(), 403, "{what}");
}

/// The control: each `Authorizer` accepts what it made, and the service
/// refuses the forger's key itself (it knows no such key).
#[tokio::test]
async fn each_authorizer_accepts_its_own_capabilities() {
    let (service, forger) = service_and_forger().await;
    for (side, token) in [(&service, VICTIM_TOKEN), (&forger, FORGED_TOKEN)] {
        let number = side.number().await;
        assert_eq!(number.client().token().unwrap().expose_secret(), token);
        let waba = side.waba().await;
        assert_eq!(waba.client().token().unwrap().expose_secret(), token);
        let admin = side.admin().await;
        let opened = side
            .authz
            .waba_for_admin(&admin, &side.records.waba)
            .await
            .unwrap();
        assert_eq!(opened.client().token().unwrap().expose_secret(), token);
        // Already under the active key: walked, nothing to re-encrypt.
        let rotation = side.authz.rotate_vault(&admin).await.unwrap();
        assert_eq!((rotation.wabas, rotation.rotated), (1, 0));
        assert!(rotation.failed.is_empty());
        side.records.take_calls();
        let marked = number.failed(&side.authz, &refused_token()).await;
        assert_eq!(marked.code(), "reconnect_required");
        assert_eq!(side.records.take_calls(), ["set_waba_status"]);
    }
    assert_eq!(
        service
            .authz
            .admin_caller(Some(&forger.admin_bearer))
            .await
            .unwrap_err()
            .code(),
        "unauthenticated"
    );
}

#[tokio::test]
async fn owned_number_refuses_another_authorizers_caller() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.caller().await;
    let before = Before::of(&service);
    let error = service
        .authz
        .owned_number(&foreign, pn())
        .await
        .unwrap_err();
    assert_forbidden(&error, "owned_number");
    before.unchanged(&service, "owned_number").await;
}

#[tokio::test]
async fn owned_waba_refuses_another_authorizers_caller() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.caller().await;
    let before = Before::of(&service);
    let error = service
        .authz
        .owned_waba(&foreign, waba())
        .await
        .unwrap_err();
    assert_forbidden(&error, "owned_waba");
    before.unchanged(&service, "owned_waba").await;
}

#[tokio::test]
async fn waba_for_admin_refuses_another_authorizers_admin() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.admin().await;
    let before = Before::of(&service);
    let error = service
        .authz
        .waba_for_admin(&foreign, &service.records.waba)
        .await
        .unwrap_err();
    assert_forbidden(&error, "waba_for_admin");
    before.unchanged(&service, "waba_for_admin").await;
}

#[tokio::test]
async fn store_token_refuses_another_authorizers_admin() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.admin().await;
    let before = Before::of(&service);
    let error = service
        .authz
        .store_token(&foreign, &stored(FORGED_TOKEN))
        .await
        .unwrap_err();
    assert_forbidden(&error, "store_token");
    before.unchanged(&service, "store_token").await;
}

#[tokio::test]
async fn rotate_vault_refuses_another_authorizers_admin() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.admin().await;
    let before = Before::of(&service);
    let error = service.authz.rotate_vault(&foreign).await.unwrap_err();
    assert_forbidden(&error, "rotate_vault");
    before.unchanged(&service, "rotate_vault").await;
}

#[tokio::test]
async fn forget_for_admin_refuses_another_authorizers_admin() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.admin().await;
    let before = Before::of(&service);
    let error = service
        .authz
        .forget_for_admin(&foreign, &waba())
        .await
        .unwrap_err();
    assert_forbidden(&error, "forget_for_admin");
    before.unchanged(&service, "forget_for_admin").await;
}

#[tokio::test]
async fn graph_failed_for_admin_refuses_another_authorizers_admin_and_marks_nothing() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.admin().await;
    let before = Before::of(&service);
    let error = service
        .authz
        .graph_failed_for_admin(&foreign, &waba(), &refused_token())
        .await;
    assert_forbidden(&error, "graph_failed_for_admin");
    before.unchanged(&service, "graph_failed_for_admin").await;
}

#[tokio::test]
async fn owned_number_failed_refuses_another_authorizer_and_marks_nothing() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.number().await;
    let before = Before::of(&service);
    let error = foreign.failed(&service.authz, &refused_token()).await;
    assert_forbidden(&error, "OwnedNumber::failed");
    before.unchanged(&service, "OwnedNumber::failed").await;
}

#[tokio::test]
async fn owned_waba_failed_refuses_another_authorizer_and_marks_nothing() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.waba().await;
    let before = Before::of(&service);
    let error = foreign.failed(&service.authz, &refused_token()).await;
    assert_forbidden(&error, "OwnedWaba::failed");
    before.unchanged(&service, "OwnedWaba::failed").await;
}

#[tokio::test]
async fn owned_waba_forget_refuses_another_authorizer_and_deletes_nothing() {
    let (service, forger) = service_and_forger().await;
    // An admin's opening too: the same type, from another method.
    let admin = forger.admin().await;
    let opened = forger
        .authz
        .waba_for_admin(&admin, &forger.records.waba)
        .await
        .unwrap();
    for foreign in [forger.waba().await, opened] {
        let before = Before::of(&service);
        let error = foreign.forget(&service.authz).await.unwrap_err();
        assert_forbidden(&error, "OwnedWaba::forget");
        before.unchanged(&service, "OwnedWaba::forget").await;
    }
    // The forger's own vault still works: it was never the target.
    assert_eq!(
        forger
            .number()
            .await
            .client()
            .token()
            .unwrap()
            .expose_secret(),
        FORGED_TOKEN
    );
}

/// A clone of a capability is the same capability: still its maker's.
#[tokio::test]
async fn a_clone_keeps_its_makers_brand() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.caller().await.clone();
    let error = service
        .authz
        .owned_number(&foreign, pn())
        .await
        .unwrap_err();
    assert_forbidden(&error, "a cloned Caller");
    let own = service.caller().await.clone();
    assert!(service.authz.owned_number(&own, pn()).await.is_ok());
}

/// L4: a Graph client built with a token is refused, not stripped: its
/// token would ride along on any call made with `Authorizer::client`
/// that sets none.
#[test]
fn a_client_carrying_a_token_is_refused() {
    let vault = TokenVault::new(
        Arc::new(Kv::default()),
        VaultKeys::new(VaultKey::generate("k").unwrap()),
    )
    .unwrap();
    let client = Client::builder()
        .transport(NoNet)
        .access_token(AccessToken::new("EAAG-default-token"))
        .build()
        .unwrap();
    let error = Authorizer::new(Arc::new(Records::new(Vec::new())), vault, client).unwrap_err();
    assert!(matches!(error, Error::Config(_)), "{error:?}");
    assert!(
        !error.to_string().contains("EAAG-default-token"),
        "the refusal never quotes the token: {error}"
    );
}

/// Every event logged while it is the thread's subscriber: its level and
/// its fields, as text.
#[derive(Clone, Default)]
struct Logged(Arc<Mutex<Vec<(tracing::Level, String)>>>);

impl tracing::Subscriber for Logged {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                let _ = write!(self.0, "{}={value:?} ", field.name());
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.0
            .lock()
            .unwrap()
            .push((*event.metadata().level(), fields.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// A refusal is logged at `warn` with the operation's name, and nothing
/// the forged capability or the vault holds.
#[tokio::test]
async fn a_refusal_is_logged_at_warn_without_secrets() {
    let (service, forger) = service_and_forger().await;
    let admin = forger.admin().await;
    let logged = Logged::default();
    let guard = tracing::subscriber::set_default(logged.clone());
    let error = service
        .authz
        .store_token(&admin, &stored(FORGED_TOKEN))
        .await
        .unwrap_err();
    drop(guard);
    assert_forbidden(&error, "store_token");
    let events = logged.0.lock().unwrap().clone();
    let refusals: Vec<_> = events
        .iter()
        .filter(|(_, fields)| fields.contains("another authorizer"))
        .collect();
    assert_eq!(refusals.len(), 1, "{events:?}");
    let (level, fields) = refusals[0];
    assert_eq!(*level, tracing::Level::WARN);
    assert!(fields.contains("operation=\"store_token\""), "{fields}");
    let secret = forger.admin_bearer.trim_start_matches("Bearer ");
    for (_, fields) in &events {
        for text in [VICTIM_TOKEN, FORGED_TOKEN, secret, admin.key_id()] {
            assert!(!fields.contains(text), "{fields} names {text}");
        }
    }
}
