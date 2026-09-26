//! Authentication and the authorization order over HTTP
//! (docs/design/server.md, section 3.3). The order is the core's
//! ([`meta_whatsapp_server_core::authz`], whose module documents it: key,
//! tenant, scope, ownership, vault); this module runs it:
//!
//! 1. to 3. **The key, the tenant, the scope**, before the body is read:
//!    [`tenant_guard`] and [`admin_guard`] are middleware, which runs
//!    before any extractor that reads the body. Then the tenant's rate
//!    limit for the route's class ([`crate::ratelimit`]), else `429
//!    too_many_requests`: before ownership, so a limited request reads
//!    neither the bindings nor the vault.
//! 4. and 5. **Ownership, then the vault**, for the path's `{pn}` or
//!    `{waba_id}`: the [`OwnedNumber`] and [`OwnedWaba`] extractors.
//!
//! The extractors are the only way a tenant handler obtains a token: the
//! core's [`Authorizer`] keeps the vault in a private field, and only it
//! makes the owned number or WABA a client with a token comes from. Admin
//! handlers reach a WABA's token through [`AdminCaller`], which only
//! [`admin_guard`] (through [`Authorizer::admin_caller`]) creates.

use std::collections::HashMap;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::embedded_signup::TokenVault;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequestParts, Path, Request, State};
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderName, HeaderValue, header};
use meta_whatsapp_rs::webhooks::axum::middleware::Next;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::authz as core_authz;
pub use meta_whatsapp_server_core::authz::{AdminCaller, Authorizer, Caller, Tokens};

use crate::error::ApiError;
use crate::model::{Scope, WabaBinding};
use crate::ratelimit::{RouteClass, retry_after_secs};
use crate::state::AppState;
use crate::store::RecordStore;

/// `WA-Tenant`: the tenant a platform key acts as.
pub static WA_TENANT: HeaderName = HeaderName::from_static("wa-tenant");

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

impl From<core_authz::VaultRotation> for VaultRotation {
    fn from(report: core_authz::VaultRotation) -> Self {
        let core_authz::VaultRotation {
            wabas,
            rotated,
            failed,
        } = report;
        Self {
            wabas,
            rotated,
            failed,
        }
    }
}

/// Walk every bound WABA (`wa_server_wabas`: the vault cannot list its
/// records) and re-encrypt its token and credit ledger under the active
/// key with `TokenVault::rotate`. A record that fails is reported and the
/// walk goes on; only the service's own storage failing stops it. The
/// core's [`core_authz::rotate_vault`].
pub async fn rotate_vault(
    store: &dyn RecordStore,
    vault: &TokenVault,
) -> Result<VaultRotation, Error> {
    core_authz::rotate_vault(store, vault)
        .await
        .map(VaultRotation::from)
}

impl FromRequestParts<AppState> for Caller {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _: &AppState,
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

impl FromRequestParts<AppState> for AdminCaller {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _: &AppState,
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

/// The request's `Authorization` value, when it is text.
fn authorization(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
}

/// The scope a [`tenant_guard`] requires.
#[derive(Clone)]
pub struct Guard {
    /// The state.
    pub state: AppState,
    /// The scope every route behind the guard needs.
    pub scope: Scope,
}

/// Middleware for tenant routes: steps 1 to 3
/// ([`Authorizer::tenant_caller`]), then the tenant's rate limit, then the
/// [`Caller`] in the request's extensions.
pub async fn tenant_guard(
    State(guard): State<Guard>,
    mut request: Request,
    next: Next,
) -> Response {
    let named = request.headers().get(&WA_TENANT).map(HeaderValue::as_bytes);
    let caller = match guard
        .state
        .authz()
        .tenant_caller(authorization(&request), named, guard.scope)
        .await
    {
        Ok(caller) => caller,
        Err(error) => return ApiError::from(error).into_response(),
    };
    // The tenant's budget for this class of route, per replica.
    let class = RouteClass::of(guard.scope, request.method());
    if let Err(wait) = guard.state.limiter().check(caller.tenant(), class) {
        guard.state.metrics().rate_limited(class.as_str());
        return ApiError::too_many_requests(retry_after_secs(wait)).into_response();
    }
    request.extensions_mut().insert(caller);
    next.run(request).await
}

/// Middleware for `/v1/admin`: step 1 with an admin key, else `403
/// forbidden` ([`Authorizer::admin_caller`]); then the [`AdminCaller`] in
/// the request's extensions.
pub async fn admin_guard(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let admin = match state.authz().admin_caller(authorization(&request)).await {
        Ok(admin) => admin,
        Err(error) => return ApiError::from(error).into_response(),
    };
    request.extensions_mut().insert(admin);
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

/// A phone number the caller's tenant owns, with a client acting with its
/// WABA's token. Extracting it runs steps 4 and 5 for the path's `{pn}`
/// ([`Authorizer::owned_number`]).
#[derive(Clone)]
pub struct OwnedNumber(core_authz::OwnedNumber);

impl std::fmt::Debug for OwnedNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl OwnedNumber {
    /// The number.
    pub fn phone_number_id(&self) -> &PhoneNumberId {
        self.0.phone_number_id()
    }

    /// Its WABA.
    pub fn waba_id(&self) -> &WabaId {
        self.0.waba_id()
    }

    /// A client acting with the WABA's token.
    pub fn client(&self) -> &Client {
        self.0.client()
    }

    /// The API error for a failed Graph call made with this number's
    /// token; a `190` (or `0`) marks the WABA's numbers
    /// `reconnect_required`.
    pub async fn failed(&self, state: &AppState, error: &Error) -> ApiError {
        graph_failed(state, self.waba_id(), error).await
    }

    /// The API error for a failed Graph call on an object named by id (a
    /// media id): Meta refusing it (not this number's, or none at all) is
    /// `404 not_found`, like a missing one; anything else is
    /// [`Self::failed`] with Meta's `details`.
    pub async fn failed_on_object(&self, state: &AppState, error: &Error) -> ApiError {
        object_failed(state, self.waba_id(), error).await
    }
}

impl FromRequestParts<AppState> for OwnedNumber {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let caller = Caller::from_request_parts(parts, state).await?;
        let params = path_params(parts, state).await?;
        let pn = PhoneNumberId::new(params.get("pn").cloned().ok_or_else(ApiError::internal)?);
        Ok(Self(state.authz().owned_number(&caller, pn).await?))
    }
}

/// A WABA the caller's tenant owns, with a client acting with its token.
/// Extracting it runs steps 4 and 5 for the path's `{waba_id}`
/// ([`Authorizer::owned_waba`]).
#[derive(Clone)]
pub struct OwnedWaba(core_authz::OwnedWaba);

impl std::fmt::Debug for OwnedWaba {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl OwnedWaba {
    /// The WABA.
    pub fn waba_id(&self) -> &WabaId {
        self.0.waba_id()
    }

    /// A client acting with its token.
    pub fn client(&self) -> &Client {
        self.0.client()
    }

    /// See [`OwnedNumber::failed`].
    pub async fn failed(&self, state: &AppState, error: &Error) -> ApiError {
        graph_failed(state, self.waba_id(), error).await
    }

    /// See [`OwnedNumber::failed_on_object`] (a template id).
    pub async fn failed_on_object(&self, state: &AppState, error: &Error) -> ApiError {
        object_failed(state, self.waba_id(), error).await
    }

    /// Delete the WABA's token and its binding, and its numbers': the last
    /// step of a disconnection, once Meta unsubscribed the app.
    pub async fn forget(self, state: &AppState) -> Result<(), ApiError> {
        self.0.forget(state.authz()).await.map_err(ApiError::from)
    }

    /// Unbind a WABA as the operator (decision D4's admin unbind): with
    /// its stored token, if one is usable, unsubscribe the app, at best
    /// (Meta refusing does not stop it); then delete the token, then the
    /// bindings, as a disconnection does. A token no tenant can reach
    /// serves nothing and widens what a database and vault key compromise
    /// exposes.
    pub async fn unbind_for_admin(
        state: &AppState,
        admin: &AdminCaller,
        binding: &WabaBinding,
    ) -> Result<(), ApiError> {
        match Self::for_admin(state, admin, binding).await {
            Ok(owned) => {
                let unsubscribed = owned
                    .client()
                    .waba(owned.waba_id().clone())
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
    /// the admin key may act on every tenant
    /// ([`Authorizer::waba_for_admin`]).
    pub async fn for_admin(
        state: &AppState,
        admin: &AdminCaller,
        binding: &WabaBinding,
    ) -> Result<Self, ApiError> {
        Ok(Self(state.authz().waba_for_admin(admin, binding).await?))
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
        Ok(Self(state.authz().owned_waba(&caller, waba_id).await?))
    }
}

/// [`graph_failed`] for a call on an object named by id: Meta refusing the
/// object (the core's [`core_authz::object_refused`]) is `404 not_found`,
/// without Meta's code or text, the answer for an object that does not
/// exist: another tenant's looks like a missing one. Anything else keeps
/// its code and `details`.
async fn object_failed(state: &AppState, waba_id: &WabaId, error: &Error) -> ApiError {
    let api = graph_failed(state, waba_id, error).await;
    ApiError::from(core_authz::object_failed(api.into_service_error(), error))
}

/// The API error of a failed Graph call made with `waba_id`'s stored
/// token: a `190` (or `0`) marks the WABA's numbers `reconnect_required`
/// ([`Authorizer::graph_failed`]); a Graph error is counted.
pub(crate) async fn graph_failed(state: &AppState, waba_id: &WabaId, error: &Error) -> ApiError {
    let api = ApiError::from(state.authz().graph_failed(waba_id, error).await);
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
