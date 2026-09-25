//! `/v1/admin` (admin key only; docs/design/server.md, sections 3 and 4.2):
//! tenants, their keys, platform keys, attaching the platform's own WABAs
//! and unbinding a WABA (decision D4).

use std::collections::BTreeSet;

use futures::{StreamExt, TryStreamExt};
use meta_whatsapp_rs::ErrorKind;
use meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken;
use meta_whatsapp_rs::client::waba::PhoneNumbersQuery;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use super::common::{ApiJson, PageQuery, json, next_cursor, parse_rfc3339, rfc3339};
use crate::auth::{AdminCaller, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::keys::MintedKey;
use crate::model::{
    AllowedTenants, ApiKeyRecord, BindOutcome, DeleteTenantOutcome, KeyOwner, KeyScope,
    MAX_NAME_CHARS, NewApiKey, PageRequest, Scope, Tenant, TenantId, TenantStatus,
};
use crate::state::AppState;
use crate::store::Store;
use crate::telemetry::{self, Subject};

/// Log an operator's change (see [`crate::telemetry::audit`]).
fn audit(action: &'static str, admin: &AdminCaller, subject: Subject<'_>) {
    telemetry::audit(action, admin.key_id(), &subject);
}

fn tenant_subject(tenant: &TenantId) -> Subject<'_> {
    Subject {
        tenant: Some(tenant.as_str()),
        ..Subject::default()
    }
}

fn key_subject(key_id: &str) -> Subject<'_> {
    Subject {
        key_id: Some(key_id),
        ..Subject::default()
    }
}

// ─── Tenants ─────────────────────────────────────────────────────────────

/// A tenant.
#[derive(Debug, Serialize, ToSchema)]
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

fn tenant_id(field: &'static str, id: &str) -> Result<TenantId, ApiError> {
    TenantId::parse(id).ok_or_else(|| ApiError::invalid(field))
}

fn name(value: Option<String>) -> Result<Option<String>, ApiError> {
    match value {
        Some(name)
            if name.chars().count() > MAX_NAME_CHARS || name.chars().any(char::is_control) =>
        {
            Err(ApiError::invalid("name"))
        }
        other => Ok(other),
    }
}

/// A path's tenant id: an invalid one names no tenant.
fn path_tenant(id: &str) -> Result<TenantId, ApiError> {
    TenantId::parse(id).ok_or_else(ApiError::not_found)
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
        (status = 422, description = "`invalid_request` on `id` (malformed, or taken), `name` or `body`", body = ErrorBody),
    )
)]
pub async fn create_tenant(
    State(state): State<AppState>,
    admin: AdminCaller,
    ApiJson(body): ApiJson<CreateTenant>,
) -> Result<(StatusCode, Json<TenantView>), ApiError> {
    let id = tenant_id("id", &body.id)?;
    let name = name(body.name)?.unwrap_or_default();
    match state.store().create_tenant(&id, &name).await? {
        Some(tenant) => {
            audit("tenant_created", &admin, tenant_subject(&tenant.id));
            Ok(json(StatusCode::CREATED, TenantView::from(tenant)))
        }
        // Taken: another tenant's id (the design names no code for it).
        None => Err(ApiError::invalid("id")),
    }
}

/// List tenants.
#[utoipa::path(
    get,
    path = "/v1/admin/tenants",
    tag = "admin",
    security(("api_key" = [])),
    params(
        ("limit" = Option<u32>, Query, description = "Page size, 1 to 100 (default 50)"),
        ("cursor" = Option<String>, Query, description = "`next_cursor` of the previous page"),
    ),
    responses(
        (status = 200, description = "Tenants, in id order", body = TenantList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit` or `cursor`", body = ErrorBody),
    )
)]
pub async fn list_tenants(
    State(state): State<AppState>,
    _admin: AdminCaller,
    PageQuery(page): PageQuery,
) -> Result<Json<TenantList>, ApiError> {
    let listing = state.store().tenants(&page).await?;
    let next_cursor = next_cursor(&listing);
    Ok(Json(TenantList {
        data: listing.items.into_iter().map(TenantView::from).collect(),
        next_cursor,
    }))
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
pub async fn get_tenant(
    State(state): State<AppState>,
    _admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<Json<TenantView>, ApiError> {
    let id = path_tenant(&id)?;
    let tenant = state
        .store()
        .tenant(&id)
        .await?
        .ok_or_else(ApiError::not_found)?;
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
pub async fn update_tenant(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateTenant>,
) -> Result<Json<TenantView>, ApiError> {
    let id = path_tenant(&id)?;
    let name = name(body.name)?;
    let status = body.status.map(|s| match s {
        TenantStatusName::Active => TenantStatus::Active,
        TenantStatusName::Suspended => TenantStatus::Suspended,
    });
    let tenant = state
        .store()
        .update_tenant(&id, name.as_deref(), status)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let action = match status {
        Some(TenantStatus::Suspended) => "tenant_suspended",
        Some(TenantStatus::Active) => "tenant_activated",
        None => "tenant_renamed",
    };
    audit(action, &admin, tenant_subject(&id));
    Ok(Json(tenant.into()))
}

/// Delete a tenant: disconnect every WABA it has (as
/// `DELETE /v1/wabas/{waba_id}` does), then delete it and its keys. Stops
/// at the first WABA that cannot be disconnected; the tenant stays.
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
pub async fn delete_tenant(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = path_tenant(&id)?;
    // A WABA attached while this runs is disconnected by the next round.
    for _ in 0..5 {
        if state.store().tenant(&id).await?.is_none() {
            return Err(ApiError::not_found());
        }
        let wabas = state
            .store()
            .wabas(
                &id,
                &PageRequest {
                    after: None,
                    limit: crate::model::MAX_PAGE_SIZE,
                },
            )
            .await?;
        for binding in &wabas.items {
            let owned = OwnedWaba::for_admin(&state, &admin, binding).await?;
            let unsubscribed = owned
                .client()
                .waba(owned.waba_id().clone())
                .unsubscribe_app()
                .await;
            if let Err(error) = unsubscribed {
                return Err(owned.failed(&state, &error).await.with_details(&error));
            }
            owned.forget(&state).await?;
            let waba_id = binding.waba_id.as_str();
            audit(
                "waba_disconnected",
                &admin,
                Subject {
                    tenant: Some(id.as_str()),
                    waba_id: Some(waba_id),
                    ..Subject::default()
                },
            );
        }
        match state.store().delete_tenant(&id).await? {
            DeleteTenantOutcome::Deleted => {
                audit("tenant_deleted", &admin, tenant_subject(&id));
                return Ok(StatusCode::NO_CONTENT);
            }
            DeleteTenantOutcome::NotFound => return Err(ApiError::not_found()),
            DeleteTenantOutcome::HasWabas => {}
        }
    }
    tracing::warn!("tenant deletion kept finding WABAs; giving up");
    Err(ApiError::internal().retryable(true))
}

// ─── Keys ────────────────────────────────────────────────────────────────

/// A scope of a tenant or platform key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum TenantsSpec {
    /// `"*"`: any tenant.
    All(String),
    /// These tenant ids.
    Only(Vec<String>),
}

/// An API key, as listed (never its secret).
#[derive(Debug, Serialize, ToSchema)]
pub struct KeyView {
    /// Public id (the part of the key between `wak_` and the secret).
    pub key_id: String,
    /// `tenant`, `platform` or `admin`.
    pub kind: String,
    /// A tenant key's tenant.
    pub tenant_id: Option<String>,
    /// A platform key's tenants.
    pub tenants: Option<TenantsSpec>,
    /// Scopes on tenant routes.
    pub scopes: Vec<ScopeName>,
    /// A label for operators.
    pub name: String,
    /// RFC 3339.
    #[schema(format = DateTime)]
    pub created_at: String,
    /// When it stops working (RFC 3339).
    #[schema(format = DateTime)]
    pub expires_at: Option<String>,
    /// When it was revoked (RFC 3339).
    #[schema(format = DateTime)]
    pub revoked_at: Option<String>,
    /// When it was last used, to the minute (RFC 3339).
    #[schema(format = DateTime)]
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
            kind: k.owner.kind().as_str().to_owned(),
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

/// Why a key could not be minted.
#[derive(Debug, thiserror::Error)]
pub enum MintError {
    /// The operating system's random number generator failed.
    #[error("the random number generator failed")]
    Random,
    /// Three drawn key ids were all taken.
    #[error("no free key id after three draws")]
    Collision,
    /// The store failed.
    #[error(transparent)]
    Storage(#[from] meta_whatsapp_rs::core::error::StorageError),
}

impl From<MintError> for ApiError {
    fn from(error: MintError) -> Self {
        match error {
            MintError::Random | MintError::Collision => ApiError::internal(),
            MintError::Storage(storage) => storage.into(),
        }
    }
}

/// Mint and store a key for `owner`. The admin API and the CLI's
/// bootstrap both use it. Returns the key (to show once) and its record.
pub async fn mint(
    store: &dyn Store,
    owner: KeyOwner,
    scopes: Vec<Scope>,
    name: String,
    expires_at: Option<OffsetDateTime>,
) -> Result<(MintedKey, ApiKeyRecord), MintError> {
    // 96 random bits: a collision is not expected, but never fatal.
    for _ in 0..3 {
        let minted = MintedKey::generate().map_err(|_| MintError::Random)?;
        let new = NewApiKey {
            key_id: minted.key_id().to_owned(),
            secret_sha256: minted.digest(),
            owner: owner.clone(),
            scopes: scopes.clone(),
            name: name.clone(),
            expires_at,
        };
        if let Some(record) = store.insert_key(&new).await? {
            return Ok((minted, record));
        }
    }
    Err(MintError::Collision)
}

fn scopes(names: Vec<ScopeName>) -> Result<Vec<Scope>, ApiError> {
    let set: BTreeSet<Scope> = names.into_iter().map(Scope::from).collect();
    if set.is_empty() {
        return Err(ApiError::invalid("scopes"));
    }
    Ok(set.into_iter().collect())
}

fn expiry(value: Option<String>) -> Result<Option<OffsetDateTime>, ApiError> {
    let Some(value) = value else { return Ok(None) };
    let at = parse_rfc3339("expires_at", &value)?;
    if at <= OffsetDateTime::now_utc() {
        return Err(ApiError::invalid("expires_at"));
    }
    Ok(Some(at))
}

fn minted_view(minted: &MintedKey, record: ApiKeyRecord) -> MintedKeyView {
    MintedKeyView {
        key: minted.expose_key().to_owned(),
        api_key: record.into(),
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
pub async fn mint_tenant_key(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<MintTenantKey>,
) -> Result<(StatusCode, Json<MintedKeyView>), ApiError> {
    let id = path_tenant(&id)?;
    let scopes = scopes(body.scopes)?;
    let name = name(body.name)?.unwrap_or_default();
    let expires_at = expiry(body.expires_at)?;
    if state.store().tenant(&id).await?.is_none() {
        return Err(ApiError::not_found());
    }
    let (minted, record) = mint(
        state.store(),
        KeyOwner::Tenant(id.clone()),
        scopes,
        name,
        expires_at,
    )
    .await?;
    audit(
        "tenant_key_minted",
        &admin,
        Subject {
            tenant: Some(id.as_str()),
            key_id: Some(minted.key_id()),
            ..Subject::default()
        },
    );
    Ok(json(StatusCode::CREATED, minted_view(&minted, record)))
}

/// List a tenant's keys, revoked ones included.
#[utoipa::path(
    get,
    path = "/v1/admin/tenants/{id}/keys",
    tag = "admin",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "Tenant id"),
        ("limit" = Option<u32>, Query, description = "Page size, 1 to 100 (default 50)"),
        ("cursor" = Option<String>, Query, description = "`next_cursor` of the previous page"),
    ),
    responses(
        (status = 200, description = "The keys", body = KeyList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 404, description = "`not_found`: no such tenant", body = ErrorBody),
    )
)]
pub async fn list_tenant_keys(
    State(state): State<AppState>,
    _admin: AdminCaller,
    Path(id): Path<String>,
    PageQuery(page): PageQuery,
) -> Result<Json<KeyList>, ApiError> {
    let id = path_tenant(&id)?;
    if state.store().tenant(&id).await?.is_none() {
        return Err(ApiError::not_found());
    }
    key_list(state.store(), &KeyScope::Tenant(id), &page).await
}

async fn key_list(
    store: &dyn Store,
    scope: &KeyScope,
    page: &PageRequest,
) -> Result<Json<KeyList>, ApiError> {
    let listing = store.keys(scope, page).await?;
    let next_cursor = next_cursor(&listing);
    Ok(Json(KeyList {
        data: listing.items.into_iter().map(KeyView::from).collect(),
        next_cursor,
    }))
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
pub async fn revoke_tenant_key(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path((id, key_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let id = path_tenant(&id)?;
    if state
        .store()
        .revoke_key(&KeyScope::Tenant(id.clone()), &key_id)
        .await?
    {
        audit(
            "tenant_key_revoked",
            &admin,
            Subject {
                tenant: Some(id.as_str()),
                key_id: Some(&key_id),
                ..Subject::default()
            },
        );
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found())
    }
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
pub async fn mint_platform_key(
    State(state): State<AppState>,
    admin: AdminCaller,
    ApiJson(body): ApiJson<MintPlatformKey>,
) -> Result<(StatusCode, Json<MintedKeyView>), ApiError> {
    let allowed = allowed_tenants(body.tenants)?;
    let scopes = scopes(body.scopes)?;
    let name = name(body.name)?.unwrap_or_default();
    let expires_at = expiry(body.expires_at)?;
    let (minted, record) = mint(
        state.store(),
        KeyOwner::Platform(allowed),
        scopes,
        name,
        expires_at,
    )
    .await?;
    audit("platform_key_minted", &admin, key_subject(minted.key_id()));
    Ok(json(StatusCode::CREATED, minted_view(&minted, record)))
}

/// Validate a platform key's tenants: `"*"`, or a non-empty list of tenant
/// ids (which need not exist yet).
pub fn allowed_tenants(spec: TenantsSpec) -> Result<AllowedTenants, ApiError> {
    match spec {
        TenantsSpec::All(star) if star == "*" => Ok(AllowedTenants::All),
        TenantsSpec::All(_) => Err(ApiError::invalid("tenants")),
        TenantsSpec::Only(list) => {
            let set = list
                .iter()
                .map(|t| TenantId::parse(t).ok_or_else(|| ApiError::invalid("tenants")))
                .collect::<Result<BTreeSet<_>, _>>()?;
            if set.is_empty() {
                return Err(ApiError::invalid("tenants"));
            }
            Ok(AllowedTenants::Only(set.into_iter().collect()))
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
        ("limit" = Option<u32>, Query, description = "Page size, 1 to 100 (default 50)"),
        ("cursor" = Option<String>, Query, description = "`next_cursor` of the previous page"),
    ),
    responses(
        (status = 200, description = "The keys", body = KeyList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
    )
)]
pub async fn list_platform_keys(
    State(state): State<AppState>,
    _admin: AdminCaller,
    PageQuery(page): PageQuery,
) -> Result<Json<KeyList>, ApiError> {
    key_list(state.store(), &KeyScope::Platform, &page).await
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
pub async fn revoke_platform_key(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(key_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if state
        .store()
        .revoke_key(&KeyScope::Platform, &key_id)
        .await?
    {
        audit("platform_key_revoked", &admin, key_subject(&key_id));
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found())
    }
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

/// Longest WABA id accepted in a body.
const MAX_ID_LEN: usize = 64;

/// Most numbers attach binds for one WABA (Meta allows far fewer): past
/// it the listing is refused as `upstream` rather than followed for ever.
pub const MAX_WABA_NUMBERS: usize = 1000;

/// A Meta id from a body: digits only, as Meta writes them (D4 compares
/// the exact string, so `0102…` and `102…` must not both pass).
fn graph_id(field: &'static str, id: &str) -> Result<String, ApiError> {
    let valid = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id.bytes().all(|b| b.is_ascii_digit())
        && !id.starts_with('0');
    if valid {
        Ok(id.to_owned())
    } else {
        Err(ApiError::invalid(field))
    }
}

/// Attach one of the platform's own WABAs to a tenant: list its numbers
/// from Meta with the given token (so no id is bound on the caller's word),
/// bind the WABA and those numbers to the tenant (refused when another
/// tenant has it, decision D4), store the token in the vault, then
/// subscribe the app to the WABA's webhooks with it.
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
        (status = 403, description = "Not an admin key, or Meta refused the token access to the WABA (nothing bound or stored), or to subscribe the app (the WABA stays attached: repeat the call)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such tenant", body = ErrorBody),
        (status = 409, description = "`waba_owned_by_another_tenant`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `waba_id` (digits), `token` (blank, or not a valid token for Meta) or `body`; Meta's `invalid_parameter`", body = ErrorBody),
        (status = 502, description = "Meta failed, or listed more than 1,000 numbers (`upstream`)", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub async fn attach_waba(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<AttachWaba>,
) -> Result<(StatusCode, Json<AttachedWaba>), ApiError> {
    let tenant = path_tenant(&id)?;
    let waba_id = WabaId::new(graph_id("waba_id", &body.waba_id)?);
    if body.token.trim().is_empty() {
        return Err(ApiError::invalid("token"));
    }
    let token = AccessToken::new(body.token);
    if state.store().tenant(&tenant).await?.is_none() {
        return Err(ApiError::not_found());
    }
    // D4 on the claimed id, before Meta is asked anything.
    if state
        .store()
        .waba(&waba_id)
        .await?
        .is_some_and(|w| w.tenant_id != tenant)
    {
        return Err(ApiError::new("waba_owned_by_another_tenant"));
    }
    let waba = state
        .client()
        .with_token(token.clone())
        .waba(waba_id.clone());
    let meta_failed = |error: &meta_whatsapp_rs::Error| {
        let api = ApiError::from_library(error).with_details(error);
        state.metrics().graph_error(api.code());
        // The token is the request's: Meta refusing it is the caller's
        // input to fix, not a stored token to reconnect.
        if error.kind() == ErrorKind::Authentication {
            api.as_invalid("token")
        } else {
            api
        }
    };
    // Every number Meta lists on the WABA, with the given token; a listing
    // that never ends (a cursor per page, from a stub or a fault) stops.
    let numbers: Vec<PhoneNumberId> = waba
        .phone_numbers_stream(&PhoneNumbersQuery::new())
        .take(MAX_WABA_NUMBERS + 1)
        .map_ok(|n| n.id)
        .try_collect()
        .await
        .map_err(|error| meta_failed(&error))?;
    if numbers.len() > MAX_WABA_NUMBERS {
        tracing::warn!(
            max = MAX_WABA_NUMBERS,
            "Meta listed more numbers on a WABA than attach binds"
        );
        return Err(ApiError::new("upstream"));
    }
    // D4 again, atomically with the binding. Binding before storing: a
    // store first would overwrite the owner's token before D4 refused.
    match state.store().bind_waba(&tenant, &waba_id, &numbers).await? {
        BindOutcome::Bound => {}
        BindOutcome::OwnedByAnotherTenant => {
            return Err(ApiError::new("waba_owned_by_another_tenant"));
        }
    }
    let record = StoredBusinessToken::new(waba_id.clone(), token).phone_number_ids(numbers.clone());
    state.tokens().store(&record).await?;
    audit(
        "waba_attached",
        &admin,
        Subject {
            tenant: Some(tenant.as_str()),
            waba_id: Some(waba_id.as_str()),
            ..Subject::default()
        },
    );
    // Subscribe the app to the WABA's webhooks, as onboarding does after
    // storing the token: a WABA the app is not subscribed to delivers no
    // webhook, and disconnecting unsubscribes it (docs/design/server.md,
    // section 3.4), so attaching again must subscribe again. Safe to
    // repeat: after a refusal the WABA stays attached, and repeating the
    // attach finishes it.
    waba.subscribe_app(None)
        .await
        .map_err(|error| meta_failed(&error))?;
    Ok(json(
        StatusCode::CREATED,
        AttachedWaba {
            waba_id: waba_id.into_inner(),
            tenant_id: tenant.as_str().to_owned(),
            phone_number_ids: numbers.into_iter().map(PhoneNumberId::into_inner).collect(),
        },
    ))
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
pub async fn unbind_waba(
    State(state): State<AppState>,
    admin: AdminCaller,
    Path(waba_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let binding = state
        .store()
        .waba(&WabaId::new(waba_id))
        .await?
        .ok_or_else(ApiError::not_found)?;
    OwnedWaba::unbind_for_admin(&state, &admin, &binding).await?;
    audit(
        "waba_unbound",
        &admin,
        Subject {
            tenant: Some(binding.tenant_id.as_str()),
            waba_id: Some(binding.waba_id.as_str()),
            ..Subject::default()
        },
    );
    Ok(StatusCode::NO_CONTENT)
}
