//! Authentication and the authorization order (docs/design/server.md,
//! section 3.3). Every tenant route, in this order:
//!
//! 1. **Authenticate the key** before the body is read, else `401`
//!    ([`tenant_guard`], [`admin_guard`]: middleware, which runs before any
//!    extractor that reads the body).
//! 2. **Resolve the tenant**: the tenant key's own, or the `WA-Tenant`
//!    header's within the platform key's allowed set, else `403
//!    forbidden`; a suspended tenant is `403 tenant_suspended`.
//! 3. **Check the scope**, else `403 forbidden`. Then the tenant's rate
//!    limit for the route's class ([`crate::ratelimit`]), else `429
//!    too_many_requests`: before ownership, so a limited request reads
//!    neither the bindings nor the vault.
//! 4. **Ownership**: the path's `{pn}` or `{waba_id}` must be bound to that
//!    tenant, else `404 not_found`, the answer for one that does not exist
//!    ([`OwnedNumber`], [`OwnedWaba`]).
//! 5. **Only then read the vault**: no entry is `409
//!    number_not_connected`; an expired token, or a number marked after a
//!    `190`, is `409 reconnect_required`. Then `Client::with_token`.
//!
//! The extractors are the only way a tenant handler obtains a token:
//! [`Tokens`] keeps the vault in a private field that only this module
//! reads. Admin handlers reach a WABA's token through [`AdminCaller`],
//! which only [`admin_guard`] creates.

use std::collections::HashMap;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{StoredBusinessToken, TokenVault};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequestParts, Path, Request, State};
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderMap, HeaderName, header};
use meta_whatsapp_rs::webhooks::axum::middleware::Next;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_rs::{Error, ErrorKind};
use time::OffsetDateTime;

use crate::error::ApiError;
use crate::keys::PresentedKey;
use crate::model::{
    ApiKeyRecord, KeyOwner, MAX_PAGE_SIZE, NumberStatus, PageRequest, Scope, TenantId,
    TenantStatus, WabaBinding,
};
use crate::ratelimit::{RouteClass, retry_after_secs};
use crate::state::AppState;
use crate::store::Store;
use crate::telemetry;

/// `WA-Tenant`: the tenant a platform key acts as.
pub static WA_TENANT: HeaderName = HeaderName::from_static("wa-tenant");

/// The business token vault, readable only through the authorization
/// order.
pub struct Tokens {
    vault: TokenVault,
}

impl Tokens {
    /// Wrap the vault.
    pub(crate) fn new(vault: TokenVault) -> Self {
        Self { vault }
    }

    /// Store a token whose phone numbers were listed by Meta with it
    /// (`TokenVault::store` trusts its input).
    pub(crate) async fn store(&self, token: &StoredBusinessToken) -> Result<(), Error> {
        self.vault.store(token).await
    }

    /// Delete a WABA's token and its phone index.
    async fn delete(&self, waba_id: &WabaId) -> Result<bool, Error> {
        self.vault.delete(waba_id).await
    }

    /// Re-encrypt every bound WABA's token (and its credit ledger) under
    /// the active vault key: see [`rotate_vault`].
    pub(crate) async fn rotate_all(&self, store: &dyn Store) -> Result<VaultRotation, Error> {
        rotate_vault(store, &self.vault).await
    }
}

/// What a vault rotation did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
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

/// Walk every bound WABA (`wa_server_wabas`: the vault cannot list its
/// records) and re-encrypt its token and credit ledger under the active
/// key with `TokenVault::rotate`. A record that fails is reported and the
/// walk goes on; only the service's own storage failing stops it.
pub async fn rotate_vault(store: &dyn Store, vault: &TokenVault) -> Result<VaultRotation, Error> {
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

/// A tenant request that passed steps 1 to 3.
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
/// [`admin_guard`] creates one.
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

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        // A tenant route mounted without `tenant_guard` is a bug: refuse.
        std::future::ready(
            parts
                .extensions
                .get::<Caller>()
                .cloned()
                .ok_or_else(ApiError::internal),
        )
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AdminCaller {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        // An admin route mounted without `admin_guard` is a bug: refuse.
        std::future::ready(
            parts
                .extensions
                .get::<AdminCaller>()
                .cloned()
                .ok_or_else(ApiError::internal),
        )
    }
}

/// Step 1: the key in `Authorization: Bearer wak_…`, if it exists, its
/// secret matches (in constant time), and it is neither revoked nor
/// expired. Reads no body.
async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<ApiKeyRecord, ApiError> {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .and_then(|(_, key)| PresentedKey::parse(key.trim()))
        .ok_or_else(ApiError::unauthenticated)?;
    let record = state.store().key(presented.key_id()).await?;
    let Some(record) = record else {
        // Same work as a known key, so the answer's timing says nothing.
        let _ = presented.matches(&[0; 32]);
        return Err(ApiError::unauthenticated());
    };
    if !presented.matches(&record.secret_sha256) || !record.is_usable(OffsetDateTime::now_utc()) {
        return Err(ApiError::unauthenticated());
    }
    telemetry::record_key(&record.key_id);
    if let Err(error) = state.store().touch_key(&record.key_id).await {
        tracing::debug!(error = %error, "could not record the key's last use");
    }
    Ok(record)
}

/// Step 2: the tenant a non-admin key acts as, from the key and the
/// `WA-Tenant` header.
async fn resolve_tenant(
    state: &AppState,
    key: &ApiKeyRecord,
    headers: &HeaderMap,
) -> Result<TenantId, ApiError> {
    let named = match headers.get(&WA_TENANT) {
        None => None,
        Some(value) => Some(
            value
                .to_str()
                .ok()
                .and_then(TenantId::parse)
                .ok_or_else(ApiError::forbidden)?,
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
        _ => return Err(ApiError::forbidden()),
    };
    match state.store().tenant(&tenant).await? {
        None => Err(ApiError::forbidden()),
        Some(t) if t.status == TenantStatus::Suspended => Err(ApiError::new("tenant_suspended")),
        Some(_) => Ok(tenant),
    }
}

/// The scope a [`tenant_guard`] requires.
#[derive(Clone)]
pub struct Guard {
    /// The state.
    pub state: AppState,
    /// The scope every route behind the guard needs.
    pub scope: Scope,
}

/// Middleware for tenant routes: steps 1 to 3, then the [`Caller`] in the
/// request's extensions.
pub async fn tenant_guard(
    State(guard): State<Guard>,
    mut request: Request,
    next: Next,
) -> Response {
    let key = match authenticate(&guard.state, request.headers()).await {
        Ok(key) => key,
        Err(error) => return error.into_response(),
    };
    let tenant = match resolve_tenant(&guard.state, &key, request.headers()).await {
        Ok(tenant) => tenant,
        Err(error) => return error.into_response(),
    };
    telemetry::record_tenant(tenant.as_str());
    if !key.scopes.contains(&guard.scope) {
        return ApiError::forbidden().into_response();
    }
    // The tenant's budget for this class of route, per replica.
    let class = RouteClass::of(guard.scope, request.method());
    if let Err(wait) = guard.state.limiter().check(&tenant, class) {
        guard.state.metrics().rate_limited(class.as_str());
        return ApiError::too_many_requests(retry_after_secs(wait)).into_response();
    }
    request.extensions_mut().insert(Caller {
        key_id: key.key_id,
        tenant,
    });
    next.run(request).await
}

/// Middleware for `/v1/admin`: step 1 with an admin key, else `403
/// forbidden`; then the [`AdminCaller`] in the request's extensions.
pub async fn admin_guard(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let key = match authenticate(&state, request.headers()).await {
        Ok(key) => key,
        Err(error) => return error.into_response(),
    };
    if key.owner != KeyOwner::Admin {
        return ApiError::forbidden().into_response();
    }
    request
        .extensions_mut()
        .insert(AdminCaller { key_id: key.key_id });
    next.run(request).await
}

/// The path parameters of a request.
async fn path_params(
    parts: &mut Parts,
    state: &AppState,
) -> Result<HashMap<String, String>, ApiError> {
    Path::<HashMap<String, String>>::from_request_parts(parts, state)
        .await
        .map(|Path(params)| params)
        .map_err(|_| ApiError::not_found())
}

/// Step 5 for one WABA: the token, or why there is none to use.
fn usable(
    token: Option<StoredBusinessToken>,
    waba_id: &WabaId,
) -> Result<StoredBusinessToken, ApiError> {
    let token = token.ok_or_else(|| ApiError::new("number_not_connected"))?;
    if &token.waba_id != waba_id {
        tracing::warn!("the vault routes a bound number to another WABA");
        return Err(ApiError::new("number_not_connected"));
    }
    if token.is_expired(OffsetDateTime::now_utc()) {
        return Err(ApiError::new("reconnect_required"));
    }
    Ok(token)
}

/// A phone number the caller's tenant owns, with a client acting with its
/// WABA's token. Extracting it runs steps 4 and 5 for the path's `{pn}`.
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

    /// The API error for a failed Graph call made with this number's
    /// token; a `190` (or `0`) marks the WABA's numbers
    /// `reconnect_required`.
    pub async fn failed(&self, state: &AppState, error: &Error) -> ApiError {
        graph_failed(state, &self.waba_id, error).await
    }

    /// The API error for a failed Graph call on an object named by id (a
    /// media id): Meta refusing it (not this number's, or none at all) is
    /// `404 not_found`, like a missing one; anything else is
    /// [`Self::failed`] with Meta's `details`.
    pub async fn failed_on_object(&self, state: &AppState, error: &Error) -> ApiError {
        object_failed(state, &self.waba_id, error).await
    }
}

impl FromRequestParts<AppState> for OwnedNumber {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let caller = Caller::from_request_parts(parts, state).await?;
        let params = path_params(parts, state).await?;
        let pn = PhoneNumberId::new(params.get("pn").cloned().ok_or_else(ApiError::internal)?);
        // Step 4: bound to this tenant, else it does not exist.
        let binding = state
            .store()
            .number(&pn)
            .await?
            .filter(|binding| binding.tenant_id == caller.tenant)
            .ok_or_else(ApiError::not_found)?;
        // Step 5.
        match binding.status {
            NumberStatus::Connected => {}
            NumberStatus::ReconnectRequired => return Err(ApiError::new("reconnect_required")),
            NumberStatus::Disconnected => return Err(ApiError::new("number_not_connected")),
        }
        let token = state.tokens().vault.get_by_phone_number(&pn).await?;
        let token = usable(token, &binding.waba_id)?;
        Ok(Self {
            client: state.client().with_token(token.token),
            phone_number_id: pn,
            waba_id: binding.waba_id,
        })
    }
}

/// A WABA the caller's tenant owns, with a client acting with its token.
/// Extracting it runs steps 4 and 5 for the path's `{waba_id}`.
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

    /// See [`OwnedNumber::failed`].
    pub async fn failed(&self, state: &AppState, error: &Error) -> ApiError {
        graph_failed(state, &self.waba_id, error).await
    }

    /// See [`OwnedNumber::failed_on_object`] (a template id).
    pub async fn failed_on_object(&self, state: &AppState, error: &Error) -> ApiError {
        object_failed(state, &self.waba_id, error).await
    }

    /// Delete the WABA's token and its binding, and its numbers': the last
    /// step of a disconnection, once Meta unsubscribed the app.
    pub async fn forget(self, state: &AppState) -> Result<(), ApiError> {
        state.tokens().delete(&self.waba_id).await?;
        state.store().unbind_waba(&self.waba_id).await?;
        Ok(())
    }

    /// Step 5 for a WABA bound to a tenant: its token.
    async fn open(state: &AppState, waba_id: WabaId) -> Result<Self, ApiError> {
        let token = state.tokens().vault.get(&waba_id).await?;
        let token = usable(token, &waba_id)?;
        Ok(Self {
            client: state.client().with_token(token.token),
            waba_id,
        })
    }

    /// Unbind a WABA as the operator (decision D4's admin unbind): with
    /// its stored token, if one is usable, unsubscribe the app, at best
    /// (Meta refusing does not stop it); then delete the token, then the
    /// bindings, as a disconnection does. A token no tenant can reach
    /// serves nothing and widens what a database and vault key compromise
    /// exposes.
    pub async fn unbind_for_admin(
        state: &AppState,
        _admin: &AdminCaller,
        binding: &WabaBinding,
    ) -> Result<(), ApiError> {
        match Self::open(state, binding.waba_id.clone()).await {
            Ok(owned) => {
                let unsubscribed = owned
                    .client
                    .waba(owned.waba_id.clone())
                    .unsubscribe_app()
                    .await;
                if let Err(error) = unsubscribed {
                    let api = owned.failed(state, &error).await;
                    tracing::warn!(
                        code = api.code(),
                        "unbinding a WABA Meta did not unsubscribe the app from"
                    );
                }
                owned.forget(state).await
            }
            // Storage down: nothing can be deleted either.
            Err(error) if error.code() == "storage_unavailable" => Err(error),
            // No token, an expired one, or one that no longer decrypts.
            Err(unusable) => {
                tracing::info!(
                    code = unusable.code(),
                    "unbinding a WABA without a usable token: the app stays subscribed"
                );
                state.tokens().delete(&binding.waba_id).await?;
                state.store().unbind_waba(&binding.waba_id).await?;
                Ok(())
            }
        }
    }

    /// A WABA for an admin operation (deleting its tenant): step 5 only, as
    /// the admin key may act on every tenant.
    pub async fn for_admin(
        state: &AppState,
        _admin: &AdminCaller,
        binding: &WabaBinding,
    ) -> Result<Self, ApiError> {
        Self::open(state, binding.waba_id.clone()).await
    }
}

impl FromRequestParts<AppState> for OwnedWaba {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let caller = Caller::from_request_parts(parts, state).await?;
        let params = path_params(parts, state).await?;
        let waba_id = WabaId::new(
            params
                .get("waba_id")
                .cloned()
                .ok_or_else(ApiError::internal)?,
        );
        // Step 4.
        let binding = state
            .store()
            .waba(&waba_id)
            .await?
            .filter(|binding| binding.tenant_id == caller.tenant)
            .ok_or_else(ApiError::not_found)?;
        // Step 5.
        Self::open(state, binding.waba_id).await
    }
}

/// Whether Meta refused a call on an object named by id (a media id, a
/// template id) as one it does not have, or not for this caller: an
/// invalid parameter (`100`, `33`), a permission error, a not-found, or an
/// unknown code, answered with a 4xx; or a 4xx without a Graph error
/// object. A token's own failure (`190`), throttling and Meta's failures
/// (5xx) are not refusals of the object.
pub(crate) fn object_refused(error: &Error) -> bool {
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

/// [`graph_failed`] for a call on an object named by id: Meta refusing the
/// object ([`object_refused`]) is `404 not_found`, without Meta's code or
/// text, the answer for an object that does not exist: another tenant's
/// looks like a missing one. Anything else keeps its code and `details`.
async fn object_failed(state: &AppState, waba_id: &WabaId, error: &Error) -> ApiError {
    let api = graph_failed(state, waba_id, error).await;
    if object_refused(error) {
        tracing::debug!(
            code = api.code(),
            "Meta refused an object named by id: answered not_found"
        );
        return ApiError::not_found();
    }
    api.with_details(error)
}

/// The API error of a failed Graph call made with `waba_id`'s stored
/// token: a `190` (or `0`) marks the WABA's numbers `reconnect_required`.
pub(crate) async fn graph_failed(state: &AppState, waba_id: &WabaId, error: &Error) -> ApiError {
    if error.kind() == ErrorKind::Authentication
        && let Err(storage) = state
            .store()
            .set_waba_status(waba_id, NumberStatus::ReconnectRequired)
            .await
    {
        tracing::warn!(error = %storage, "could not mark the numbers reconnect_required");
    }
    let api = ApiError::from_library(error);
    if error.graph().is_some() {
        state.metrics().graph_error(api.code());
    }
    api
}

/// Shared by the routers: the guard for tenant routes needing `scope`.
pub fn guard(state: &AppState, scope: Scope) -> Guard {
    Guard {
        state: state.clone(),
        scope,
    }
}
