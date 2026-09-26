//! Authentication and the authorization order (docs/design/server.md,
//! section 3.3), as services an API adapter calls. Every tenant route, in
//! this order:
//!
//! 1. **Authenticate the key** before the body is read, else `401`
//!    ([`Authorizer::authenticate`]: the adapter calls it before it reads
//!    a body).
//! 2. **Resolve the tenant**: the tenant key's own, or the `WA-Tenant`
//!    header's within the platform key's allowed set, else `403
//!    forbidden`; a suspended tenant is `403 tenant_suspended`.
//! 3. **Check the scope**, else `403 forbidden` ([`Authorizer::tenant_caller`]
//!    runs steps 1 to 3). Then the tenant's rate limit for the route's
//!    class ([`crate::ratelimit`]), else `429 too_many_requests`: before
//!    ownership, so a limited request reads neither the bindings nor the
//!    vault.
//! 4. **Ownership**: the path's `{pn}` or `{waba_id}` must be bound to that
//!    tenant, else `404 not_found`, the answer for one that does not exist
//!    ([`Authorizer::owned_number`], [`Authorizer::owned_waba`]).
//! 5. **Only then read the vault**: no entry is `409
//!    number_not_connected`; an expired token, or a number marked after a
//!    `190`, is `409 reconnect_required`. Then `Client::with_token`.
//!
//! [`OwnedNumber`] and [`OwnedWaba`] are the only way to a token: [`Tokens`]
//! keeps the vault in a private field that only this module reads, and
//! only [`Authorizer`] makes them, after step 4 (or, for an admin, from an
//! [`AdminCaller`], which only [`Authorizer::admin_caller`] makes).

use std::sync::Arc;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{StoredBusinessToken, TokenVault};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::{Error, ErrorKind};
use time::OffsetDateTime;

use crate::error::ServiceError;
use crate::keys::PresentedKey;
use crate::model::{
    ApiKeyRecord, KeyOwner, MAX_PAGE_SIZE, NumberStatus, PageRequest, Scope, TenantId,
    TenantStatus, WabaBinding,
};
use crate::store::RecordStore;

/// The business token vault, readable only through the authorization
/// order.
pub struct Tokens {
    vault: TokenVault,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens").finish_non_exhaustive()
    }
}

impl Tokens {
    /// Wrap the vault.
    pub fn new(vault: TokenVault) -> Self {
        Self { vault }
    }

    /// Store a token whose phone numbers were listed by Meta with it
    /// (`TokenVault::store` trusts its input).
    pub async fn store(&self, token: &StoredBusinessToken) -> Result<(), Error> {
        self.vault.store(token).await
    }

    /// Delete a WABA's token and its phone index.
    pub async fn delete(&self, waba_id: &WabaId) -> Result<bool, Error> {
        self.vault.delete(waba_id).await
    }

    /// Re-encrypt every bound WABA's token (and its credit ledger) under
    /// the active vault key: see [`rotate_vault`].
    pub async fn rotate_all(&self, store: &dyn RecordStore) -> Result<VaultRotation, Error> {
        rotate_vault(store, &self.vault).await
    }
}

/// What a vault rotation did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VaultRotation {
    /// WABAs walked.
    pub wabas: usize,
    /// Records re-encrypted under the active key.
    pub rotated: usize,
    /// WABAs whose records could not be read or rewritten (a record under
    /// a key no longer configured, say): rotate again, or reconnect them,
    /// before dropping the old key.
    pub failed: Vec<String>,
}

/// Walk every bound WABA (the bindings: the vault cannot list its
/// records) and re-encrypt its token and credit ledger under the active
/// key with `TokenVault::rotate`. A record that fails is reported and the
/// walk goes on; only the service's own storage failing stops it.
pub async fn rotate_vault(
    store: &dyn RecordStore,
    vault: &TokenVault,
) -> Result<VaultRotation, Error> {
    let mut report = VaultRotation::default();
    let mut page = PageRequest {
        after: None,
        limit: MAX_PAGE_SIZE,
    };
    loop {
        let listing = store.all_wabas(&page).await?;
        for binding in &listing.items {
            report.wabas += 1;
            match vault.rotate(&binding.waba_id).await {
                Ok(true) => report.rotated += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        kind = error.kind().as_str(),
                        waba_id = binding.waba_id.as_str(),
                        "a vault record could not be rotated"
                    );
                    report.failed.push(binding.waba_id.as_str().to_owned());
                }
            }
        }
        let Some(after) = listing.next_after else {
            return Ok(report);
        };
        page.after = Some(after);
    }
}

/// A tenant request that passed steps 1 to 3. Only
/// [`Authorizer::tenant_caller`] makes one.
#[derive(Debug, Clone)]
pub struct Caller {
    key_id: String,
    tenant: TenantId,
}

impl Caller {
    /// The tenant it acts as.
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// The key's id.
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

/// An admin request that passed step 1 with an admin key. Only
/// [`Authorizer::admin_caller`] makes one.
#[derive(Debug, Clone)]
pub struct AdminCaller {
    key_id: String,
}

impl AdminCaller {
    /// The admin key's id.
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

/// The authorization order over the records, the vault and the tokenless
/// Graph client: who a request is ([`Caller`], [`AdminCaller`]), and what
/// it owns ([`OwnedNumber`], [`OwnedWaba`]: the only way to a stored
/// token).
pub struct Authorizer {
    records: Arc<dyn RecordStore>,
    tokens: Tokens,
    client: Client,
}

impl std::fmt::Debug for Authorizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authorizer").finish_non_exhaustive()
    }
}

impl Authorizer {
    /// The order over `records` and `vault`, calling Graph with `client`
    /// (built without a default token: each call runs with the tenant's
    /// token).
    pub fn new(records: Arc<dyn RecordStore>, vault: TokenVault, client: Client) -> Self {
        Self {
            records,
            tokens: Tokens::new(vault),
            client,
        }
    }

    /// The vault, for what writes to it (attaching a WABA, rotating its
    /// key, forgetting a WABA): reading a token is [`Self::owned_number`]'s
    /// and [`Self::owned_waba`]'s.
    pub fn tokens(&self) -> &Tokens {
        &self.tokens
    }

    /// The tokenless Graph client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Step 1: the key in `authorization` (the `Authorization` header's
    /// value: `Bearer wak_…`), if it exists, its secret matches (in
    /// constant time), and it is neither revoked nor expired. Records the
    /// key's public id on the current `tracing` span (`key_id`), and its
    /// use.
    pub async fn authenticate(
        &self,
        authorization: Option<&str>,
    ) -> Result<ApiKeyRecord, ServiceError> {
        let presented = authorization
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .and_then(|(_, key)| PresentedKey::parse(key.trim()))
            .ok_or_else(ServiceError::unauthenticated)?;
        let record = self.records.key(presented.key_id()).await?;
        let Some(record) = record else {
            // Same work as a known key, so the answer's timing says nothing.
            let _ = presented.matches(&[0; 32]);
            return Err(ServiceError::unauthenticated());
        };
        if !presented.matches(&record.secret_sha256) || !record.is_usable(OffsetDateTime::now_utc())
        {
            return Err(ServiceError::unauthenticated());
        }
        tracing::Span::current().record("key_id", record.key_id.as_str());
        if let Err(error) = self.records.touch_key(&record.key_id).await {
            tracing::debug!(error = %error, "could not record the key's last use");
        }
        Ok(record)
    }

    /// Step 2: the tenant a non-admin key acts as, from the key and the
    /// `WA-Tenant` header's raw value `named` (`None` when the request
    /// sent none).
    pub async fn resolve_tenant(
        &self,
        key: &ApiKeyRecord,
        named: Option<&[u8]>,
    ) -> Result<TenantId, ServiceError> {
        let named = match named {
            None => None,
            Some(value) => Some(
                std::str::from_utf8(value)
                    .ok()
                    .and_then(TenantId::parse)
                    .ok_or_else(ServiceError::forbidden)?,
            ),
        };
        let tenant = match (&key.owner, named) {
            // A tenant key acts as its tenant; naming another is refused.
            (KeyOwner::Tenant(own), None) => own.clone(),
            (KeyOwner::Tenant(own), Some(named)) if &named == own => named,
            // A platform key names a tenant within its set, checked before the
            // tenant is looked up: the answer says nothing of tenants outside
            // it.
            (KeyOwner::Platform(allowed), Some(named)) if allowed.allows(&named) => named,
            _ => return Err(ServiceError::forbidden()),
        };
        match self.records.tenant(&tenant).await? {
            None => Err(ServiceError::forbidden()),
            Some(t) if t.status == TenantStatus::Suspended => {
                Err(ServiceError::new("tenant_suspended"))
            }
            Some(_) => Ok(tenant),
        }
    }

    /// Steps 1 to 3 for a tenant route needing `scope`: the key in
    /// `authorization`, the tenant (the `WA-Tenant` header's raw value
    /// `named`, recorded on the current `tracing` span as `tenant`), the
    /// scope. The rate limit comes next, then ownership.
    pub async fn tenant_caller(
        &self,
        authorization: Option<&str>,
        named: Option<&[u8]>,
        scope: Scope,
    ) -> Result<Caller, ServiceError> {
        let key = self.authenticate(authorization).await?;
        let tenant = self.resolve_tenant(&key, named).await?;
        tracing::Span::current().record("tenant", tenant.as_str());
        if !key.scopes.contains(&scope) {
            return Err(ServiceError::forbidden());
        }
        Ok(Caller {
            key_id: key.key_id,
            tenant,
        })
    }

    /// Step 1 for `/v1/admin`, with an admin key, else `403 forbidden`.
    pub async fn admin_caller(
        &self,
        authorization: Option<&str>,
    ) -> Result<AdminCaller, ServiceError> {
        let key = self.authenticate(authorization).await?;
        if key.owner != KeyOwner::Admin {
            return Err(ServiceError::forbidden());
        }
        Ok(AdminCaller { key_id: key.key_id })
    }

    /// Steps 4 and 5 for the number `pn`: bound to `caller`'s tenant (else
    /// it does not exist), connected, with a usable token.
    pub async fn owned_number(
        &self,
        caller: &Caller,
        pn: PhoneNumberId,
    ) -> Result<OwnedNumber, ServiceError> {
        // Step 4: bound to this tenant, else it does not exist.
        let binding = self
            .records
            .number(&pn)
            .await?
            .filter(|binding| binding.tenant_id == caller.tenant)
            .ok_or_else(ServiceError::not_found)?;
        // Step 5.
        match binding.status {
            NumberStatus::Connected => {}
            NumberStatus::ReconnectRequired => {
                return Err(ServiceError::new("reconnect_required"));
            }
            NumberStatus::Disconnected => return Err(ServiceError::new("number_not_connected")),
        }
        let token = self.tokens.vault.get_by_phone_number(&pn).await?;
        let token = usable(token, &binding.waba_id)?;
        Ok(OwnedNumber {
            client: self.client.with_token(token.token),
            phone_number_id: pn,
            waba_id: binding.waba_id,
        })
    }

    /// Steps 4 and 5 for the WABA `waba_id`: bound to `caller`'s tenant
    /// (else it does not exist), with a usable token.
    pub async fn owned_waba(
        &self,
        caller: &Caller,
        waba_id: WabaId,
    ) -> Result<OwnedWaba, ServiceError> {
        // Step 4.
        let binding = self
            .records
            .waba(&waba_id)
            .await?
            .filter(|binding| binding.tenant_id == caller.tenant)
            .ok_or_else(ServiceError::not_found)?;
        // Step 5.
        self.open(binding.waba_id).await
    }

    /// A WABA for an admin operation (deleting its tenant, unbinding it):
    /// step 5 only, as the admin key may act on every tenant.
    pub async fn waba_for_admin(
        &self,
        _admin: &AdminCaller,
        binding: &WabaBinding,
    ) -> Result<OwnedWaba, ServiceError> {
        self.open(binding.waba_id.clone()).await
    }

    /// Step 5 for a WABA bound to a tenant: its token.
    async fn open(&self, waba_id: WabaId) -> Result<OwnedWaba, ServiceError> {
        let token = self.tokens.vault.get(&waba_id).await?;
        let token = usable(token, &waba_id)?;
        Ok(OwnedWaba {
            client: self.client.with_token(token.token),
            waba_id,
        })
    }

    /// The error of a failed Graph call made with `waba_id`'s stored
    /// token: a `190` (or `0`) marks the WABA's numbers
    /// `reconnect_required`.
    pub async fn graph_failed(&self, waba_id: &WabaId, error: &Error) -> ServiceError {
        if error.kind() == ErrorKind::Authentication
            && let Err(storage) = self
                .records
                .set_waba_status(waba_id, NumberStatus::ReconnectRequired)
                .await
        {
            tracing::warn!(error = %storage, "could not mark the numbers reconnect_required");
        }
        ServiceError::from_library(error)
    }
}

/// Step 5 for one WABA: the token, or why there is none to use.
fn usable(
    token: Option<StoredBusinessToken>,
    waba_id: &WabaId,
) -> Result<StoredBusinessToken, ServiceError> {
    let token = token.ok_or_else(|| ServiceError::new("number_not_connected"))?;
    if &token.waba_id != waba_id {
        tracing::warn!("the vault routes a bound number to another WABA");
        return Err(ServiceError::new("number_not_connected"));
    }
    if token.is_expired(OffsetDateTime::now_utc()) {
        return Err(ServiceError::new("reconnect_required"));
    }
    Ok(token)
}

/// A phone number the caller's tenant owns, with a client acting with its
/// WABA's token. Only [`Authorizer::owned_number`] makes one.
#[derive(Clone)]
pub struct OwnedNumber {
    phone_number_id: PhoneNumberId,
    waba_id: WabaId,
    client: Client,
}

impl std::fmt::Debug for OwnedNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedNumber")
            .field("phone_number_id", &self.phone_number_id)
            .field("waba_id", &self.waba_id)
            .finish_non_exhaustive()
    }
}

impl OwnedNumber {
    /// The number.
    pub fn phone_number_id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// Its WABA.
    pub fn waba_id(&self) -> &WabaId {
        &self.waba_id
    }

    /// A client acting with the WABA's token.
    pub fn client(&self) -> &Client {
        &self.client
    }
}

/// A WABA the caller's tenant owns (or an admin reaches), with a client
/// acting with its token. Only [`Authorizer`] makes one.
#[derive(Clone)]
pub struct OwnedWaba {
    waba_id: WabaId,
    client: Client,
}

impl std::fmt::Debug for OwnedWaba {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedWaba")
            .field("waba_id", &self.waba_id)
            .finish_non_exhaustive()
    }
}

impl OwnedWaba {
    /// The WABA.
    pub fn waba_id(&self) -> &WabaId {
        &self.waba_id
    }

    /// A client acting with its token.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Delete the WABA's token and its binding, and its numbers': the last
    /// step of a disconnection, once Meta unsubscribed the app.
    pub async fn forget(self, authz: &Authorizer) -> Result<(), ServiceError> {
        authz.tokens.delete(&self.waba_id).await?;
        authz.records.unbind_waba(&self.waba_id).await?;
        Ok(())
    }
}

/// Whether Meta refused a call on an object named by id (a media id, a
/// template id) as one it does not have, or not for this caller: an
/// invalid parameter (`100`, `33`), a permission error, a not-found, or an
/// unknown code, answered with a 4xx; a 4xx without a Graph error object;
/// or a JSON answer that is not that kind of object (another node the id
/// names). A token's own failure (`190`), throttling, Meta's failures
/// (5xx) and an answer that is not JSON are not refusals of the object.
pub fn object_refused(error: &Error) -> bool {
    if let Error::Decode { source, .. } = error {
        return source.classify() == serde_json::error::Category::Data;
    }
    if let Some(graph) = error.graph() {
        return graph.http_status.is_none_or(|status| status < 500)
            && matches!(
                graph.kind(),
                ErrorKind::InvalidParameter
                    | ErrorKind::Permission
                    | ErrorKind::NotFound
                    | ErrorKind::Unknown
            );
    }
    matches!(
        error,
        Error::Http {
            status: 400 | 403 | 404,
            ..
        }
    )
}

/// The error of a failed Graph call on an object named by id, given the
/// call's error (`api`, from [`Authorizer::graph_failed`]): Meta refusing
/// the object ([`object_refused`]) is `404 not_found`, without Meta's code
/// or text, the answer for an object that does not exist: another
/// tenant's looks like a missing one. Anything else keeps its code and
/// `details`.
pub fn object_failed(api: ServiceError, error: &Error) -> ServiceError {
    if object_refused(error) {
        tracing::debug!(
            code = api.code(),
            "Meta refused an object named by id: answered not_found"
        );
        return ServiceError::not_found();
    }
    api.with_details(error)
}
