//! Authentication and the authorization order over HTTP
//! (docs/design/server.md, section 3.3). The order is the core's
//! ([`meta_whatsapp_server_core::authz`], whose module documents it: key,
//! tenant, scope, ownership, vault); this module runs it:
//!
//! 1. to 3. **The key, the tenant, the scope**, before the body is read:
//!    [`tenant_guard`] and [`admin_guard`] are middleware, which runs
//!    before any extractor that reads the body. Then the tenant's rate
//!    limit, charged by the request's operation ([`Operations`]: its
//!    method and route template to the core's [`Operation`], whose class
//!    the core decides, [`crate::ratelimit`]), else `429
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
//!
//! A capability is only its maker's word, and the core's
//! [`Authorizer::new`] is public: anyone can build one and have it make a
//! [`Caller`] naming any tenant. So the [`Caller`] and [`AdminCaller`]
//! extractors admit what they take from the request's extensions
//! ([`Authorizer::admit`], [`Authorizer::admit_admin`]): one this
//! service's [`Authorizer`] did not make is `403 forbidden`. And the
//! handlers, which act on [`Caller::tenant`] without asking the
//! [`Authorizer`] again, are private to their crates (this one's
//! `api::media`, the API adapter's `api::*`), as is `idempotency::run`,
//! which runs over the service's own idempotency records: another crate
//! reaches them only through the routers, behind these guards. What an
//! API adapter's handlers need beyond the state (the records, the
//! authorization order, the core's capabilities, the idempotency engine)
//! they reach through the state's [`Grant`], which only the state's
//! composer is given (roadmap S7).

use std::collections::HashMap;
use std::sync::Arc;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::embedded_signup::TokenVault;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::webhooks::axum::extract::{FromRef, FromRequestParts, Path, Request, State};
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderName, HeaderValue, header};
use meta_whatsapp_rs::webhooks::axum::middleware::Next;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::ServiceError;
use meta_whatsapp_server_core::admin::Admin;
use meta_whatsapp_server_core::authz as core_authz;
pub use meta_whatsapp_server_core::authz::{AdminCaller, Authorizer, Caller, Tokens};
pub use meta_whatsapp_server_core::operation::Operation;

use crate::error::ApiError;
use crate::model::{Scope, WabaBinding};
use crate::state::{AppState, Grant};
use crate::store::RecordStore;

/// The target of this module's log lines: the module they were logged
/// from before roadmap S7 moved it out of `meta-whatsapp-server`.
const LOG_TARGET: &str = "meta_whatsapp_server::auth";

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
    /// Records the vault keeps past a binding walked besides: Solution
    /// Partner mode's credit ledgers and revocation markers, which no
    /// binding names once their WABA left (roadmap M3a).
    pub records: usize,
    /// Those that could not be read or rewritten, as `waba/<id>` (a
    /// credit ledger) or `business/<id>` (a revocation marker). Drop the
    /// old key only once both this and `failed` are empty.
    pub failed_records: Vec<String>,
}

impl From<core_authz::VaultRotation> for VaultRotation {
    fn from(report: core_authz::VaultRotation) -> Self {
        let core_authz::VaultRotation {
            wabas,
            rotated,
            failed,
            records,
            failed_records,
        } = report;
        Self {
            wabas,
            rotated,
            failed,
            records,
            failed_records,
        }
    }
}

/// Walk every bound WABA (`wa_server_wabas`: the vault cannot list its
/// records) and re-encrypt its token and credit ledger under the active
/// key with `TokenVault::rotate`, then every record kept past a binding
/// (`wa_server_vault_records`: credit ledgers, revocation markers; roadmap
/// M3a). A record that fails is reported and the walk goes on; only the
/// service's own storage failing stops it. The core's
/// [`core_authz::rotate_vault`].
pub async fn rotate_vault(
    store: &dyn RecordStore,
    vault: &TokenVault,
) -> Result<VaultRotation, Error> {
    core_authz::rotate_vault(store, vault)
        .await
        .map(VaultRotation::from)
}

/// The tenant route's [`Caller`], which [`tenant_guard`] put in the
/// request's extensions, once `state`'s [`Authorizer`] admits it
/// ([`Authorizer::admit`]): one another [`Authorizer`] made (put there by
/// anything but this service's guard) is `403 forbidden`, as handlers act
/// on [`Caller::tenant`] without asking again.
fn admitted_caller(parts: &Parts, state: &AppState) -> Result<Caller, ApiError> {
    // A tenant route mounted without `tenant_guard` is a bug: refuse.
    let caller = parts
        .extensions
        .get::<Caller>()
        .cloned()
        .ok_or_else(ApiError::internal)?;
    state.authz().admit(&caller)?;
    Ok(caller)
}

/// The admin route's [`AdminCaller`], which [`admin_guard`] put in the
/// request's extensions, once `state`'s [`Authorizer`] admits it
/// ([`Authorizer::admit_admin`]): one another [`Authorizer`] made is `403
/// forbidden`.
fn admitted_admin(parts: &Parts, state: &AppState) -> Result<AdminCaller, ApiError> {
    // An admin route mounted without `admin_guard` is a bug: refuse.
    let admin = parts
        .extensions
        .get::<AdminCaller>()
        .cloned()
        .ok_or_else(ApiError::internal)?;
    state.authz().admit_admin(&admin)?;
    Ok(admin)
}

/// The tenant route's [`Caller`] on a route served with the state: see
/// the one on a route served with its [`Grant`].
impl FromRequestParts<AppState> for Caller {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(admitted_caller(parts, state))
    }
}

/// The tenant route's [`Caller`], which [`tenant_guard`] put in the
/// request's extensions, once the state's [`Authorizer`] admits it
/// ([`Authorizer::admit`]): one another [`Authorizer`] made (put there by
/// anything but this service's guard) is `403 forbidden`, as handlers act
/// on [`Caller::tenant`] without asking again. Here on a route served with
/// the state's [`Grant`] (an API adapter's, the internal listener's).
impl FromRequestParts<Grant> for Caller {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        grant: &Grant,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(admitted_caller(parts, grant.state()))
    }
}

/// The admin route's [`AdminCaller`] on a route served with the state:
/// see the one on a route served with its [`Grant`].
impl FromRequestParts<AppState> for AdminCaller {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(admitted_admin(parts, state))
    }
}

/// The admin route's [`AdminCaller`], which [`admin_guard`] put in the
/// request's extensions, once the state's [`Authorizer`] admits it
/// ([`Authorizer::admit_admin`]): one another [`Authorizer`] made is `403
/// forbidden`. Here on a route served with the state's [`Grant`].
impl FromRequestParts<Grant> for AdminCaller {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        grant: &Grant,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(admitted_admin(parts, grant.state()))
    }
}

/// The request's `Authorization` value, when it is text.
fn authorization(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
}

/// The tenant operations a [`tenant_guard`] charges: each operation of
/// the OpenAPI document that is a core [`Operation`] (its `operationId`
/// is the operation's id), by its method and route template (the API
/// adapter's `tenant_operations`, `meta-whatsapp-server-api-axum`). A
/// request is the operation of its
/// method and the template its path matches as axum routes it
/// ([`Self::of`]: a `{name}` matches any one segment, an empty one
/// included); `HEAD`, which axum answers on every `GET` route, is that
/// `GET`'s. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Operations(Arc<Vec<(String, String, Operation)>>);

impl Operations {
    /// The operations `(method, route template, operation)`, the method in
    /// capitals (`GET`).
    pub fn new(routes: impl IntoIterator<Item = (String, String, Operation)>) -> Self {
        Self(Arc::new(routes.into_iter().collect()))
    }

    /// The operation a request with `method` on `path` calls, if any: the
    /// template `path` is routed to, segment by segment, where a `{name}`
    /// matches any one segment, **an empty one included**, as axum's
    /// router does (`/v1/numbers//profile` is `get_profile`'s, whose
    /// extractor then refuses the empty number). A stricter match than the
    /// router's would leave a routed request with no operation: `500`,
    /// and charged nothing.
    ///
    /// Of two templates a path matches, the one with a literal segment
    /// where the other has a parameter, at the first segment where they
    /// differ, is the path's, as axum's router decides
    /// (`/v1/events/stream` before `/v1/events/{id}`):
    /// [`crate::telemetry::most_specific`].
    pub fn of(&self, method: &str, path: &str) -> Option<Operation> {
        let method = if method == "HEAD" { "GET" } else { method };
        crate::telemetry::most_specific(
            self.0
                .iter()
                .filter(|(m, template, _)| m == method && routed(template, path)),
            |(_, template, _)| template,
        )
        .map(|(_, _, operation)| *operation)
    }

    /// Every `(method, route template, operation)`.
    pub fn routes(&self) -> &[(String, String, Operation)] {
        &self.0
    }
}

/// Whether axum routes `path` to `template`: the same segments, a
/// `{name}` matching any one (empty or not, as the path carries it,
/// percent-encoded or not), anything else itself. (The metrics' route
/// label, `crate::telemetry::template_matches`, leaves an empty segment
/// `unmatched`, as it did before roadmap S6.)
fn routed(template: &str, path: &str) -> bool {
    let mut parts = template.split('/');
    let mut segments = path.split('/');
    loop {
        match (parts.next(), segments.next()) {
            (None, None) => return true,
            (Some(t), Some(s)) if (t.starts_with('{') && t.ends_with('}')) || t == s => {}
            _ => return false,
        }
    }
}

/// What a [`tenant_guard`] requires and charges.
#[derive(Clone)]
pub struct Guard {
    /// The state.
    pub state: AppState,
    /// The scope every route behind the guard needs.
    pub scope: Scope,
    /// The operations behind it, which its rate limit charges.
    pub operations: Operations,
}

/// Middleware for tenant routes: steps 1 to 3
/// ([`Authorizer::tenant_caller`]), then the tenant's rate limit for the
/// request's operation ([`Operations::of`], charged with the core's
/// `RateLimiter::charge`: the class is the operation's, never the
/// method's), then the [`Caller`] in the request's extensions. A route
/// behind the guard that is no tenant operation is a bug: `500 internal`,
/// after the key (an unauthenticated request learns nothing).
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
    // The tenant's budget for this operation's class, per replica.
    let Some(operation) = guard
        .operations
        .of(request.method().as_str(), request.uri().path())
    else {
        tracing::error!(target: LOG_TARGET, "a tenant route is no tenant operation");
        return ApiError::internal().into_response();
    };
    if let Err(limited) = guard.state.limiter().charge(caller.tenant(), operation) {
        guard.state.metrics().rate_limited(limited.class.as_str());
        return ApiError::from(limited.error()).into_response();
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
async fn path_params<S: Send + Sync>(
    parts: &mut Parts,
    state: &S,
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

    /// The core's capability, for the core's services
    /// (`meta_whatsapp_server_core::messages::Messages`), to the holder of
    /// a [`Grant`] (an API adapter's handlers), as the handlers that hand
    /// it on are its own.
    ///
    /// The grant is not checked to be the state's this number was opened
    /// on: anyone may compose a state of their own, and so hold a grant.
    /// That opens nothing. The capability carries the [`Authorizer`] that
    /// made it, and every core service taking it admits it with the one it
    /// is given ([`Authorizer::admit_number`]): another state's grant
    /// reaches no record of this one's, and its token's client is
    /// [`Self::client`] already.
    pub fn capability(&self, _granted: &Grant) -> &core_authz::OwnedNumber {
        &self.0
    }

    /// [`Self::capability`], in this crate (its media handlers).
    pub(crate) fn core(&self) -> &core_authz::OwnedNumber {
        &self.0
    }

    /// The API error for a failed Graph call made with this number's
    /// token; a `190` (or `0`) marks the WABA's numbers
    /// `reconnect_required` ([`core_authz::OwnedNumber::failed`]).
    pub async fn failed(&self, state: &AppState, error: &Error) -> ApiError {
        counted(state, self.0.failed(state.authz(), error).await, error)
    }

    /// The API error for a failed Graph call on an object named by id (a
    /// media id): Meta refusing it (not this number's, or none at all) is
    /// `404 not_found`, like a missing one; anything else is
    /// [`Self::failed`] with Meta's `details`.
    pub async fn failed_on_object(&self, state: &AppState, error: &Error) -> ApiError {
        object_failed(self.failed(state, error).await, error)
    }
}

impl<S> FromRequestParts<S> for OwnedNumber
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        let state = AppState::from_ref(state);
        let caller = admitted_caller(parts, &state)?;
        let params = path_params(parts, &state).await?;
        let pn = params.get("pn").cloned().ok_or_else(ApiError::internal)?;
        // Only a numeric id can be bound (step 4): anything else does not exist.
        let pn = PhoneNumberId::new(pn).map_err(|_| ApiError::not_found())?;
        Ok(Self(state.authz().owned_number(&caller, pn).await?))
    }
}

/// A group the caller's tenant's number holds (roadmap M5f, the design's
/// M5.2), with the number and a client acting with its WABA's token.
/// Extracting it runs steps 4 and 5 for the path's `{pn}` and `{group_id}`
/// **in two halves, the group between them**: step 4 for the number (no
/// token), the group among the number's records
/// ([`GroupRecords::owned`](meta_whatsapp_server_core::groups::GroupRecords::owned)),
/// and only then step 5, the vault ([`Authorizer::owned_number`]). A group
/// the number does not hold is `404 not_found` with the vault unread.
#[derive(Clone)]
pub struct OwnedGroup {
    number: OwnedNumber,
    group: meta_whatsapp_server_core::groups::OwnedGroup,
}

impl std::fmt::Debug for OwnedGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.group.fmt(f)
    }
}

impl OwnedGroup {
    /// The number the group is held by.
    pub fn number(&self) -> &OwnedNumber {
        &self.number
    }

    /// The core's capability, for the core's group services
    /// (`meta_whatsapp_server_core::groups::Groups`), to the holder of a
    /// [`Grant`], as [`OwnedNumber::capability`]: the services refuse one
    /// made for another number or tenant.
    pub fn capability(&self, _granted: &Grant) -> &meta_whatsapp_server_core::groups::OwnedGroup {
        &self.group
    }
}

impl<S> FromRequestParts<S> for OwnedGroup
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        let state = AppState::from_ref(state);
        let caller = admitted_caller(parts, &state)?;
        let params = path_params(parts, &state).await?;
        let pn = params.get("pn").cloned().ok_or_else(ApiError::internal)?;
        let group = params
            .get("group_id")
            .cloned()
            .ok_or_else(ApiError::internal)?;
        // Only a numeric id can be bound (step 4): anything else does not exist.
        let pn = PhoneNumberId::new(pn).map_err(|_| ApiError::not_found())?;
        // Step 4, then the group, with the vault unread.
        let owned = state
            .group_records()
            .owned(state.authz(), &caller, &pn, &group)
            .await?;
        // Step 5.
        let number = state.authz().owned_number(&caller, pn).await?;
        Ok(Self {
            number: OwnedNumber(number),
            group: owned,
        })
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

    /// The core's capability, for the core's services
    /// (`meta_whatsapp_server_core::templates::Templates`,
    /// `meta_whatsapp_server_core::numbers::Waba`), to the holder of a
    /// [`Grant`], as [`OwnedNumber::capability`] (whose grant is not
    /// checked either: the core's services admit the capability with
    /// their own [`Authorizer`], [`Authorizer::admit_waba`]).
    pub fn capability(&self, _granted: &Grant) -> &core_authz::OwnedWaba {
        &self.0
    }

    /// See [`OwnedNumber::failed`].
    pub async fn failed(&self, state: &AppState, error: &Error) -> ApiError {
        counted(state, self.0.failed(state.authz(), error).await, error)
    }

    /// See [`OwnedNumber::failed_on_object`] (a template id).
    pub async fn failed_on_object(&self, state: &AppState, error: &Error) -> ApiError {
        object_failed(self.failed(state, error).await, error)
    }

    /// Delete the WABA's token and its binding, and its numbers': the last
    /// step of a disconnection, once Meta unsubscribed the app. Only the
    /// token and the binding this was made from
    /// ([`core_authz::OwnedWaba::forget`]): `false` when either moved
    /// since (the WABA attached again, a rotation), nothing deleted.
    pub async fn forget(self, state: &AppState) -> Result<bool, ApiError> {
        self.0.forget(state.authz()).await.map_err(ApiError::from)
    }

    /// [`Self::forget`], answering `503 storage_unavailable` (retryable)
    /// when the WABA's token or binding moved since this was made: a
    /// repeat of the request acts on what is there now
    /// ([`core_authz::OwnedWaba::forget_or_busy`]).
    pub async fn forget_or_busy(self, state: &AppState) -> Result<(), ApiError> {
        self.0
            .forget_or_busy(state.authz())
            .await
            .map_err(ApiError::from)
    }

    /// Whether the WABA is still bound as it was when this was made, read
    /// now ([`core_authz::OwnedWaba::still_bound`]): asked just before
    /// unsubscribing the app with its token, which, after the WABA moved
    /// to another tenant, would remove that tenant's subscription.
    pub async fn still_bound(&self, state: &AppState) -> Result<bool, ApiError> {
        self.0
            .still_bound(state.authz())
            .await
            .map_err(ApiError::from)
    }

    /// Unbind a WABA as the operator (decision D4's admin unbind): with
    /// its stored token, if one is usable, unsubscribe the app, at best
    /// (Meta refusing does not stop it); then delete the token, then the
    /// bindings, as a disconnection does. A token no tenant can reach
    /// serves nothing and widens what a database and vault key compromise
    /// exposes. The core's [`Admin::unbind`]: an `admin` another
    /// [`Authorizer`] made is `403 forbidden`, and nothing is unbound.
    pub async fn unbind_for_admin(
        state: &AppState,
        admin: &AdminCaller,
        binding: &WabaBinding,
    ) -> Result<(), ApiError> {
        Admin::new(state.authz(), admin)?
            .observed_by(state.metrics())
            .unbind(binding)
            .await
            .map_err(ApiError::from)
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

impl<S> FromRequestParts<S> for OwnedWaba
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        let state = AppState::from_ref(state);
        let caller = admitted_caller(parts, &state)?;
        let params = path_params(parts, &state).await?;
        let waba_id = params
            .get("waba_id")
            .cloned()
            .ok_or_else(ApiError::internal)?;
        // Only a numeric id can be bound (step 4): anything else does not exist.
        let waba_id = WabaId::new(waba_id).map_err(|_| ApiError::not_found())?;
        Ok(Self(state.authz().owned_waba(&caller, waba_id).await?))
    }
}

/// `api`, the error of a failed Graph call on an object named by id
/// ([`OwnedNumber::failed`], [`OwnedWaba::failed`]): Meta refusing the
/// object (the core's [`core_authz::object_refused`]) is `404 not_found`,
/// without Meta's code or text, the answer for an object that does not
/// exist: another tenant's looks like a missing one. Anything else keeps
/// its code and `details`.
fn object_failed(api: ApiError, error: &Error) -> ApiError {
    ApiError::from(core_authz::object_failed(api.into_service_error(), error))
}

/// `api`, the error of a failed Graph call, over HTTP; a Graph error is
/// counted.
fn counted(state: &AppState, api: ServiceError, error: &Error) -> ApiError {
    let api = ApiError::from(api);
    if error.graph().is_some() {
        state.metrics().graph_error(api.code());
    }
    api
}

/// Shared by the routers: the guard for tenant routes needing `scope`,
/// charging `operations`.
pub fn guard(state: &AppState, scope: Scope, operations: &Operations) -> Guard {
    Guard {
        state: state.clone(),
        scope,
        operations: operations.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Of two templates a path matches, the one with a literal segment
    /// where the other has a parameter is the path's, whatever their
    /// order in the table, as axum routes it (roadmap M2b: the stream is
    /// not an event of id `stream`). Decisive: `most_specific` in
    /// `Operations::of` (the first match wins in one of the two orders).
    #[test]
    fn a_literal_segment_beats_a_parameter_in_either_order() {
        let routes = [
            (
                "GET".to_owned(),
                "/v1/events/{id}".to_owned(),
                Operation::GetEvent,
            ),
            (
                "GET".to_owned(),
                "/v1/events/stream".to_owned(),
                Operation::StreamEvents,
            ),
        ];
        for reversed in [false, true] {
            let mut routes = routes.to_vec();
            if reversed {
                routes.reverse();
            }
            let operations = Operations::new(routes);
            assert_eq!(
                operations.of("GET", "/v1/events/stream"),
                Some(Operation::StreamEvents),
                "reversed: {reversed}"
            );
            assert_eq!(
                operations.of("HEAD", "/v1/events/stream"),
                Some(Operation::StreamEvents),
                "reversed: {reversed}"
            );
            assert_eq!(
                operations.of("GET", "/v1/events/evt_4042233518184f0e9822bebdbba028a8"),
                Some(Operation::GetEvent),
                "reversed: {reversed}"
            );
        }
    }
}
