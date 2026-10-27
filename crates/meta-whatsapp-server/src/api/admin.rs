//! `/v1/admin` (admin key only; docs/design/server.md, sections 3 and 4.2):
//! tenants, their keys, platform keys, attaching the platform's own WABAs
//! and unbinding a WABA (decision D4).

//!
//! Each handler is an adapter over the core's services
//! ([`meta_whatsapp_server_core::admin::Admin`], roadmap S5a): it reads
//! the request, calls the operation, and answers its value or its error.
//! The validation, the order of D4's checks, the audit events and what
//! the metrics are told are the core's.

use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_server_core::admin::{self as core_admin, Admin, KeyRequest, TenantsInput};
pub use meta_whatsapp_server_core::admin::{MAX_WABA_NUMBERS, MintError, mint};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::common::{ApiJson, PageParams, PageQuery, json, next_cursor, rfc3339};
use crate::auth::{AdminCaller, VaultRotation};
use crate::error::{ApiError, ErrorBody};
use crate::model::{
    AllowedTenants, ApiKeyRecord, KeyKind, KeyOwner, Listing, Scope, Tenant, TenantStatus,
};
use crate::state::AppState;

/// The core's admin operations for `admin`, on `state`'s records and
/// vault, telling its metrics of failed Graph calls.
fn operations<'a>(state: &'a AppState, admin: &'a AdminCaller) -> Result<Admin<'a>, ApiError> {
    Ok(Admin::new(state.authz(), admin)?.observed_by(state.metrics()))
}

// ─── Tenants ─────────────────────────────────────────────────────────────

/// A tenant.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Tenant)]
pub struct TenantView {
    /// The integrator's id: 1 to 64 characters of `[A-Za-z0-9._:-]`.
    pub id: String,
    /// A label for operators.
    pub name: String,
    /// `active` or `suspended`.
    pub status: TenantStatusName,
    /// RFC 3339.
    #[schema(format = DateTime)]
    pub created_at: String,
    /// RFC 3339.
    #[schema(format = DateTime)]
    pub updated_at: String,
}

impl From<Tenant> for TenantView {
    fn from(t: Tenant) -> Self {
        Self {
            id: t.id.as_str().to_owned(),
            name: t.name,
            status: match t.status {
                TenantStatus::Active => TenantStatusName::Active,
                TenantStatus::Suspended => TenantStatusName::Suspended,
            },
            created_at: rfc3339(t.created_at),
            updated_at: rfc3339(t.updated_at),
        }
    }
}

/// A tenant's status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = TenantStatus)]
pub enum TenantStatusName {
    /// Its keys work.
    Active,
    /// Its keys, and platform keys naming it, get `403 tenant_suspended`.
    Suspended,
}

/// A page of tenants.
#[derive(Debug, Serialize, ToSchema)]
pub struct TenantList {
    /// The tenants, in id order.
    pub data: Vec<TenantView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// `POST /v1/admin/tenants`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTenant {
    /// The integrator's id (e.g. the CMS merchant id): 1 to 64 characters
    /// of `[A-Za-z0-9._:-]`. Immutable.
    pub id: String,
    /// A label for operators, at most 256 characters.
    pub name: Option<String>,
}

/// `PATCH /v1/admin/tenants/{id}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateTenant {
    /// A new label, at most 256 characters.
    pub name: Option<String>,
    /// `suspended` to suspend the tenant, `active` to lift it.
    pub status: Option<TenantStatusName>,
}

impl From<TenantStatusName> for TenantStatus {
    fn from(status: TenantStatusName) -> Self {
        match status {
            TenantStatusName::Active => TenantStatus::Active,
            TenantStatusName::Suspended => TenantStatus::Suspended,
        }
    }
}

/// A page of tenants, as answered.
fn tenant_list(listing: Listing<Tenant>) -> TenantList {
    let next_cursor = next_cursor(&listing);
    TenantList {
        data: listing.items.into_iter().map(TenantView::from).collect(),
        next_cursor,
    }
}

/// Create a tenant.
#[utoipa::path(
    post,
    path = "/v1/admin/tenants",
    tag = "admin",
    security(("api_key" = [])),
    request_body = CreateTenant,
    responses(
        (status = 201, description = "Created", body = TenantView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 409, description = "`tenant_exists`: the id is taken", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `id` (malformed), `name` or `body`", body = ErrorBody),
    )
)]
pub(crate) async fn create_tenant(
    State(state): State<AppState>,
    admin: AdminCaller,
    ApiJson(body): ApiJson<CreateTenant>,
) -> Result<(StatusCode, Json<TenantView>), ApiError> {
    let tenant = operations(&state, &admin)?
        .create_tenant(&body.id, body.name)
        .await?;
    Ok(json(StatusCode::CREATED, TenantView::from(tenant)))
}

/// List tenants.
#[utoipa::path(
    get,
    path = "/v1/admin/tenants",
    tag = "admin",
    security(("api_key" = [])),
    params(
        PageParams,
    ),
    responses(
        (status = 200, description = "Tenants, in id order", body = TenantList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit` or `cursor`", body = ErrorBody),
    )
)]
pub(crate) async fn list_tenants(
    State(state): State<AppState>,
    admin: AdminCaller,
    PageQuery(page): PageQuery,
) -> Result<Json<TenantList>, ApiError> {
    let listing = operations(&state, &admin)?.tenants(&page).await?;
    Ok(Json(tenant_list(listing)))
}

/// One tenant.
#[utoipa::path(
    get,
    path = "/v1/admin/tenants/{id}",
    tag = "admin",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "Tenant id")),
    responses(
        (status = 200, description = "The tenant", body = TenantView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn get_tenant(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<Json<TenantView>, ApiError> {
    let tenant = operations(&state, &admin)?.tenant(&id).await?;
    Ok(Json(tenant.into()))
}

/// Rename, suspend or reactivate a tenant.
#[utoipa::path(
    patch,
    path = "/v1/admin/tenants/{id}",
    tag = "admin",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "Tenant id")),
    request_body = UpdateTenant,
    responses(
        (status = 200, description = "The tenant after the change", body = TenantView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `name` or `body`", body = ErrorBody),
    )
)]
pub(crate) async fn update_tenant(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateTenant>,
) -> Result<Json<TenantView>, ApiError> {
    let tenant = operations(&state, &admin)?
        .update_tenant(&id, body.name, body.status.map(TenantStatus::from))
        .await?;
    Ok(Json(tenant.into()))
}

/// Delete a tenant: disconnect every WABA it has (as
/// `DELETE /v1/wabas/{waba_id}` does), then delete it and its keys. Stops
/// at the first WABA that cannot be disconnected; the tenant stays. Its
/// events go with it, and every platform key listing it stops allowing
/// it, for good: no route edits a key's tenants, so a tenant created
/// later with the same id polls none of its events and needs a new
/// platform key (a `*` key allows it at once).
#[utoipa::path(
    delete,
    path = "/v1/admin/tenants/{id}",
    tag = "admin",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "Tenant id")),
    responses(
        (status = 204, description = "Deleted, with its keys; every WABA disconnected"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key, or Meta refused to unsubscribe a WABA", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "A WABA has no usable token (`number_not_connected`, `reconnect_required`): unbind it first", body = ErrorBody),
        (status = 502, description = "Unsubscribing a WABA failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn delete_tenant(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    operations(&state, &admin)?.delete_tenant(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Keys ────────────────────────────────────────────────────────────────

/// A scope of a tenant or platform key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = Scope)]
pub enum ScopeName {
    /// Messages.
    Send,
    /// Media.
    Media,
    /// Templates.
    Templates,
    /// The inbox.
    Inbox,
    /// Events.
    Events,
    /// Webhook endpoints.
    Webhooks,
    /// Embedded Signup.
    Signup,
    /// OTP.
    Otp,
    /// WABAs, numbers, profiles.
    Numbers,
}

impl From<ScopeName> for Scope {
    fn from(s: ScopeName) -> Self {
        match s {
            ScopeName::Send => Scope::Send,
            ScopeName::Media => Scope::Media,
            ScopeName::Templates => Scope::Templates,
            ScopeName::Inbox => Scope::Inbox,
            ScopeName::Events => Scope::Events,
            ScopeName::Webhooks => Scope::Webhooks,
            ScopeName::Signup => Scope::Signup,
            ScopeName::Otp => Scope::Otp,
            ScopeName::Numbers => Scope::Numbers,
        }
    }
}

impl From<Scope> for ScopeName {
    fn from(s: Scope) -> Self {
        match s {
            Scope::Send => ScopeName::Send,
            Scope::Media => ScopeName::Media,
            Scope::Templates => ScopeName::Templates,
            Scope::Inbox => ScopeName::Inbox,
            Scope::Events => ScopeName::Events,
            Scope::Webhooks => ScopeName::Webhooks,
            Scope::Signup => ScopeName::Signup,
            Scope::Otp => ScopeName::Otp,
            Scope::Numbers => ScopeName::Numbers,
        }
    }
}

/// The tenants a platform key may name: `"*"` for any, or a list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TenantsSpec {
    /// `"*"`: any tenant.
    All(String),
    /// These tenant ids.
    Only(Vec<String>),
}

impl utoipa::PartialSchema for TenantsSpec {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, OneOfBuilder, Type};
        OneOfBuilder::new()
            .item(
                ObjectBuilder::new()
                    .schema_type(Type::String)
                    .enum_values(Some(["*"]))
                    .description(Some("Any tenant.")),
            )
            .item(
                ArrayBuilder::new()
                    .items(ObjectBuilder::new().schema_type(Type::String))
                    .min_items(Some(1))
                    .description(Some("These tenant ids.")),
            )
            .description(Some(
                "The tenants a platform key may name with WA-Tenant: \"*\" for any, or a \
                 non-empty list of tenant ids.",
            ))
            .into()
    }
}

impl ToSchema for TenantsSpec {}

/// Whose key it is.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = KeyKind)]
pub enum KeyKindName {
    /// A tenant key: its tenant.
    Tenant,
    /// A platform key: the tenant `WA-Tenant` names, within its set.
    Platform,
    /// An admin key: `/v1/admin` only.
    Admin,
}

impl From<KeyKind> for KeyKindName {
    fn from(kind: KeyKind) -> Self {
        match kind {
            KeyKind::Tenant => Self::Tenant,
            KeyKind::Platform => Self::Platform,
            KeyKind::Admin => Self::Admin,
        }
    }
}

/// An API key, as listed (never its secret).
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Key)]
pub struct KeyView {
    /// Public id (the part of the key between `wak_` and the secret).
    pub key_id: String,
    /// Whose key it is.
    pub kind: KeyKindName,
    /// A tenant key's tenant.
    #[schema(required = true)]
    pub tenant_id: Option<String>,
    /// A platform key's tenants.
    #[schema(required = true)]
    pub tenants: Option<TenantsSpec>,
    /// Scopes on tenant routes.
    pub scopes: Vec<ScopeName>,
    /// A label for operators.
    pub name: String,
    /// RFC 3339.
    #[schema(format = DateTime)]
    pub created_at: String,
    /// When it stops working (RFC 3339).
    #[schema(format = DateTime, required = true)]
    pub expires_at: Option<String>,
    /// When it was revoked (RFC 3339).
    #[schema(format = DateTime, required = true)]
    pub revoked_at: Option<String>,
    /// When it was last used, to the minute (RFC 3339).
    #[schema(format = DateTime, required = true)]
    pub last_used_at: Option<String>,
}

impl From<ApiKeyRecord> for KeyView {
    fn from(k: ApiKeyRecord) -> Self {
        let (tenant_id, tenants) = match &k.owner {
            KeyOwner::Tenant(t) => (Some(t.as_str().to_owned()), None),
            KeyOwner::Platform(AllowedTenants::All) => (None, Some(TenantsSpec::All("*".into()))),
            KeyOwner::Platform(AllowedTenants::Only(list)) => (
                None,
                Some(TenantsSpec::Only(
                    list.iter().map(|t| t.as_str().to_owned()).collect(),
                )),
            ),
            KeyOwner::Admin => (None, None),
        };
        Self {
            key_id: k.key_id,
            kind: k.owner.kind().into(),
            tenant_id,
            tenants,
            scopes: k.scopes.into_iter().map(ScopeName::from).collect(),
            name: k.name,
            created_at: rfc3339(k.created_at),
            expires_at: k.expires_at.map(rfc3339),
            revoked_at: k.revoked_at.map(rfc3339),
            last_used_at: k.last_used_at.map(rfc3339),
        }
    }
}

/// A key just minted: the whole key, shown this once, and its record.
#[derive(Serialize, ToSchema)]
#[schema(as = MintedKey)]
pub struct MintedKeyView {
    /// The key, `wak_<key_id>_<secret>`: send it as
    /// `Authorization: Bearer <key>`. Shown once; only its digest is kept.
    pub key: String,
    /// The key's record.
    pub api_key: KeyView,
}

/// A page of keys.
#[derive(Debug, Serialize, ToSchema)]
pub struct KeyList {
    /// The keys, in id order, revoked ones included.
    pub data: Vec<KeyView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// `POST /v1/admin/tenants/{id}/keys`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MintTenantKey {
    /// At least one scope.
    pub scopes: Vec<ScopeName>,
    /// A label for operators, at most 256 characters.
    pub name: Option<String>,
    /// When it stops working (RFC 3339, in the future).
    #[schema(format = DateTime)]
    pub expires_at: Option<String>,
}

/// `POST /v1/admin/platform-keys`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MintPlatformKey {
    /// The tenants it may name with `WA-Tenant`: `"*"` or a non-empty list.
    pub tenants: TenantsSpec,
    /// At least one scope.
    pub scopes: Vec<ScopeName>,
    /// A label for operators, at most 256 characters.
    pub name: Option<String>,
    /// When it stops working (RFC 3339, in the future).
    #[schema(format = DateTime)]
    pub expires_at: Option<String>,
}

impl From<MintError> for ApiError {
    fn from(error: MintError) -> Self {
        meta_whatsapp_server_core::ServiceError::from(error).into()
    }
}

/// A key request's fields, for the core.
fn key_request(
    scopes: Vec<ScopeName>,
    name: Option<String>,
    expires_at: Option<String>,
) -> KeyRequest {
    KeyRequest {
        scopes: scopes.into_iter().map(Scope::from).collect(),
        name,
        expires_at,
    }
}

/// A page of keys, as answered.
fn key_list(listing: Listing<ApiKeyRecord>) -> KeyList {
    let next_cursor = next_cursor(&listing);
    KeyList {
        data: listing.items.into_iter().map(KeyView::from).collect(),
        next_cursor,
    }
}

fn minted_view(minted: core_admin::Minted) -> MintedKeyView {
    MintedKeyView {
        key: minted.key.expose_key().to_owned(),
        api_key: minted.record.into(),
    }
}

/// Mint a key for a tenant.
#[utoipa::path(
    post,
    path = "/v1/admin/tenants/{id}/keys",
    tag = "admin",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "Tenant id")),
    request_body = MintTenantKey,
    responses(
        (status = 201, description = "The key, shown once", body = MintedKeyView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 404, description = "`not_found`: no such tenant", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `scopes`, `name`, `expires_at` or `body`", body = ErrorBody),
    )
)]
pub(crate) async fn mint_tenant_key(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<MintTenantKey>,
) -> Result<(StatusCode, Json<MintedKeyView>), ApiError> {
    let request = key_request(body.scopes, body.name, body.expires_at);
    let minted = operations(&state, &admin)?
        .mint_tenant_key(&id, request)
        .await?;
    Ok(json(StatusCode::CREATED, minted_view(minted)))
}

/// List a tenant's keys, revoked ones included.
#[utoipa::path(
    get,
    path = "/v1/admin/tenants/{id}/keys",
    tag = "admin",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "Tenant id"),
        PageParams,
    ),
    responses(
        (status = 200, description = "The keys", body = KeyList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 404, description = "`not_found`: no such tenant", body = ErrorBody),
    )
)]
pub(crate) async fn list_tenant_keys(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<String>,
    PageQuery(page): PageQuery,
) -> Result<Json<KeyList>, ApiError> {
    let listing = operations(&state, &admin)?.tenant_keys(&id, &page).await?;
    Ok(Json(key_list(listing)))
}

/// Revoke a tenant's key. Effective at once, on every replica (keys are
/// not cached).
#[utoipa::path(
    delete,
    path = "/v1/admin/tenants/{id}/keys/{key_id}",
    tag = "admin",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "Tenant id"),
        ("key_id" = String, Path, description = "Key id"),
    ),
    responses(
        (status = 204, description = "Revoked (or already revoked)"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 404, description = "`not_found`: no such key for this tenant", body = ErrorBody),
    )
)]
pub(crate) async fn revoke_tenant_key(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path((id, key_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    operations(&state, &admin)?
        .revoke_tenant_key(&id, &key_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Mint a platform key.
#[utoipa::path(
    post,
    path = "/v1/admin/platform-keys",
    tag = "admin",
    security(("api_key" = [])),
    request_body = MintPlatformKey,
    responses(
        (status = 201, description = "The key, shown once", body = MintedKeyView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `tenants`, `scopes`, `name`, `expires_at` or `body`", body = ErrorBody),
    )
)]
pub(crate) async fn mint_platform_key(
    State(state): State<AppState>,
    admin: AdminCaller,
    ApiJson(body): ApiJson<MintPlatformKey>,
) -> Result<(StatusCode, Json<MintedKeyView>), ApiError> {
    let request = key_request(body.scopes, body.name, body.expires_at);
    let minted = operations(&state, &admin)?
        .mint_platform_key(body.tenants.into(), request)
        .await?;
    Ok(json(StatusCode::CREATED, minted_view(minted)))
}

impl From<TenantsSpec> for TenantsInput {
    fn from(spec: TenantsSpec) -> Self {
        match spec {
            TenantsSpec::All(text) => Self::All(text),
            TenantsSpec::Only(list) => Self::Only(list),
        }
    }
}

/// List platform keys, revoked ones included.
#[utoipa::path(
    get,
    path = "/v1/admin/platform-keys",
    tag = "admin",
    security(("api_key" = [])),
    params(
        PageParams,
    ),
    responses(
        (status = 200, description = "The keys", body = KeyList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
    )
)]
pub(crate) async fn list_platform_keys(
    State(state): State<AppState>,
    admin: AdminCaller,
    PageQuery(page): PageQuery,
) -> Result<Json<KeyList>, ApiError> {
    let listing = operations(&state, &admin)?.platform_keys(&page).await?;
    Ok(Json(key_list(listing)))
}

/// Revoke a platform key.
#[utoipa::path(
    delete,
    path = "/v1/admin/platform-keys/{key_id}",
    tag = "admin",
    security(("api_key" = [])),
    params(("key_id" = String, Path, description = "Key id")),
    responses(
        (status = 204, description = "Revoked (or already revoked)"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn revoke_platform_key(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(key_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    operations(&state, &admin)?
        .revoke_platform_key(&key_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── WABAs ───────────────────────────────────────────────────────────────

/// `POST /v1/admin/tenants/{id}/wabas`: the platform's own WABA and a
/// system user token with access to it.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachWaba {
    /// WhatsApp Business Account id.
    pub waba_id: String,
    /// A system user access token with access to the WABA. Stored
    /// encrypted; never answered or logged.
    #[schema(write_only, format = Password)]
    pub token: String,
}

/// An attached WABA.
#[derive(Debug, Serialize, ToSchema)]
pub struct AttachedWaba {
    /// The WABA.
    pub waba_id: String,
    /// Its tenant.
    pub tenant_id: String,
    /// Every number Meta listed on it, now bound to the tenant.
    pub phone_number_ids: Vec<String>,
}

/// Attach one of the platform's own WABAs to a tenant: list its numbers
/// from Meta with the given token (so no id is bound on the caller's word),
/// bind the WABA and those numbers to the tenant (refused when another
/// tenant has it, decision D4), store the token in the vault, then
/// subscribe the app to the WABA's webhooks with it.
///
/// Subscribing the app is the last step: its failure carries
/// `step: subscribe_app` and `resumable: true`, as the WABA stays
/// attached and repeating the call finishes it (with a new token after a
/// `409 reconnect_required`: Meta refused the stored one).
#[utoipa::path(
    post,
    path = "/v1/admin/tenants/{id}/wabas",
    tag = "admin",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "Tenant id")),
    request_body = AttachWaba,
    responses(
        (status = 201, description = "Attached: its numbers bound, the token stored, the app subscribed to its webhooks", body = AttachedWaba),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key, or Meta refused the token access to the WABA (nothing bound or stored), or to subscribe the app (`step` `subscribe_app`: the WABA stays attached; repeat the call)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such tenant", body = ErrorBody),
        (status = 409, description = "`waba_owned_by_another_tenant`; `reconnect_required` with `step` `subscribe_app`: Meta refused the token once it was stored (the WABA stays attached, its numbers `reconnect_required`: repeat the call with a valid token)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `waba_id` (digits), `token` (blank, or refused by Meta when listing the numbers: nothing bound or stored) or `body`; Meta's `invalid_parameter`", body = ErrorBody),
        (status = 502, description = "Meta failed, or listed more than 1,000 numbers (`upstream`)", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn attach_waba(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<AttachWaba>,
) -> Result<(StatusCode, Json<AttachedWaba>), ApiError> {
    let attached = operations(&state, &admin)?
        .attach_waba(&id, &body.waba_id, AccessToken::new(body.token))
        .await?;
    Ok(json(
        StatusCode::CREATED,
        AttachedWaba {
            waba_id: attached.waba_id.into_inner(),
            tenant_id: attached.tenant_id.as_str().to_owned(),
            phone_number_ids: attached
                .phone_number_ids
                .into_iter()
                .map(PhoneNumberId::into_inner)
                .collect(),
        },
    ))
}

/// A bound WABA, as the operator sees it.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = WabaBinding)]
pub struct WabaBindingView {
    /// The WABA.
    pub waba_id: String,
    /// The tenant it is bound to.
    pub tenant_id: String,
    /// When it was bound (RFC 3339).
    #[schema(format = DateTime)]
    pub attached_at: String,
    /// Its numbers and their connection status.
    pub numbers: Vec<crate::api::numbers::NumberView>,
}

/// Which tenant holds a WABA, and its numbers: what an operator checks
/// before an unbind (decision D4).
#[utoipa::path(
    get,
    path = "/v1/admin/wabas/{waba_id}",
    tag = "admin",
    security(("api_key" = [])),
    params(("waba_id" = String, Path, description = "WhatsApp Business Account id")),
    responses(
        (status = 200, description = "The binding", body = WabaBindingView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 404, description = "`not_found`: not bound", body = ErrorBody),
    )
)]
pub(crate) async fn get_waba(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(waba_id): Path<String>,
) -> Result<Json<WabaBindingView>, ApiError> {
    let core_admin::HeldWaba { binding, numbers } =
        operations(&state, &admin)?.waba(&waba_id).await?;
    Ok(Json(WabaBindingView {
        waba_id: binding.waba_id.into_inner(),
        tenant_id: binding.tenant_id.as_str().to_owned(),
        attached_at: rfc3339(binding.attached_at),
        numbers: numbers.into_iter().map(Into::into).collect(),
    }))
}

/// Unbind a WABA (decision D4: the admin unbind that lets another tenant
/// connect it), also the way to free a WABA whose token no longer works:
/// with its stored token, if usable, unsubscribe the app, at best; then
/// delete the token and the bindings of the WABA and its numbers.
#[utoipa::path(
    delete,
    path = "/v1/admin/wabas/{waba_id}/binding",
    tag = "admin",
    security(("api_key" = [])),
    params(("waba_id" = String, Path, description = "WhatsApp Business Account id")),
    responses(
        (status = 204, description = "Unbound, its token deleted (the app unsubscribed when the token allowed it)"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 404, description = "`not_found`: not bound", body = ErrorBody),
    )
)]
pub(crate) async fn unbind_waba(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(waba_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    operations(&state, &admin)?.unbind_waba(&waba_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Vault ───────────────────────────────────────────────────────────────

/// Re-encrypt every bound WABA's token (and its credit ledger) under the
/// active vault key (`WA_VAULT_KEY`), walking the service's bindings (the
/// vault cannot list its records). The old key can go once `failed` is
/// empty. Also `meta-whatsapp-server vault rotate`.
///
/// The walk is a request like any other, cut at the 55 s request
/// deadline: one too large to finish in time answers `504 timeout`, part
/// of the records rotated. It is idempotent: repeat it (records already
/// under the active key are read, not rewritten), or run
/// `meta-whatsapp-server vault rotate`, which has no deadline.
#[utoipa::path(
    post,
    path = "/v1/admin/vault/rotate",
    tag = "admin",
    security(("api_key" = [])),
    responses(
        (status = 200, description = "Walked every WABA: how many records were re-encrypted, and which failed", body = VaultRotation),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 504, description = "`timeout`: the walk outlived the 55 s request deadline, part of the records rotated; repeat it, or run `meta-whatsapp-server vault rotate`, which has no deadline", body = ErrorBody),
    )
)]
pub(crate) async fn rotate_vault(
    State(state): State<AppState>,
    admin: AdminCaller,
) -> Result<Json<VaultRotation>, ApiError> {
    let report = operations(&state, &admin)?.rotate_vault().await?;
    Ok(Json(report.into()))
}
