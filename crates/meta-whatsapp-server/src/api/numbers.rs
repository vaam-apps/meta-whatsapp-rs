//! Numbers and profile (scope `numbers`; docs/design/server.md, section
//! 4.2).
//!
//! | Route | Graph calls, with the tenant's WABA token |
//! | --- | --- |
//! | `GET /v1/wabas`, `GET /v1/numbers` | none: the service's bindings |
//! | `GET /v1/numbers/{pn}` | `GET /{pn}?fields=display_phone_number,verified_name,quality_rating,name_status,throughput` |
//! | `GET /v1/numbers/{pn}/profile` | `GET /{pn}/whatsapp_business_profile?fields=about,address,description,email,websites,vertical` |
//! | `PATCH /v1/numbers/{pn}/profile` | `POST /{pn}/whatsapp_business_profile`, then the `GET` above |
//! | `DELETE /v1/wabas/{waba_id}` | `DELETE /{waba_id}/subscribed_apps`; then the token and bindings are deleted |

use meta_whatsapp_rs::client::business_profile::{Profile, ProfileField, ProfileUpdate, Vertical};
use meta_whatsapp_rs::client::phone_numbers::PhoneNumberInfo;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::common::{ApiJson, PageQuery, next_cursor, rfc3339};
use crate::auth::{Caller, OwnedNumber, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::model::{NumberBinding, WabaBinding};
use crate::state::AppState;

/// The fields `GET /v1/numbers/{pn}` asks Meta for.
pub const NUMBER_FIELDS: [&str; 5] = [
    "display_phone_number",
    "verified_name",
    "quality_rating",
    "name_status",
    "throughput",
];

/// The profile fields the service reads and writes.
pub const PROFILE_FIELDS: [ProfileField; 6] = [
    ProfileField::About,
    ProfileField::Address,
    ProfileField::Description,
    ProfileField::Email,
    ProfileField::Websites,
    ProfileField::Vertical,
];

/// A WABA of the tenant.
#[derive(Debug, Serialize, ToSchema)]
pub struct WabaView {
    /// WhatsApp Business Account id.
    pub waba_id: String,
    /// When it was bound to the tenant (RFC 3339).
    #[schema(format = DateTime)]
    pub attached_at: String,
}

impl From<WabaBinding> for WabaView {
    fn from(w: WabaBinding) -> Self {
        Self {
            waba_id: w.waba_id.into_inner(),
            attached_at: rfc3339(w.attached_at),
        }
    }
}

/// A page of WABAs.
#[derive(Debug, Serialize, ToSchema)]
pub struct WabaList {
    /// The WABAs, in id order.
    pub data: Vec<WabaView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    pub next_cursor: Option<String>,
}

/// A number of the tenant and its connection status.
#[derive(Debug, Serialize, ToSchema)]
pub struct NumberView {
    /// Phone number id.
    pub phone_number_id: String,
    /// Its WABA.
    pub waba_id: String,
    /// `connected`, `reconnect_required` or `disconnected`.
    #[schema(example = "connected")]
    pub status: String,
    /// When the status last changed (RFC 3339).
    #[schema(format = DateTime)]
    pub updated_at: String,
}

impl From<NumberBinding> for NumberView {
    fn from(n: NumberBinding) -> Self {
        Self {
            phone_number_id: n.phone_number_id.into_inner(),
            waba_id: n.waba_id.into_inner(),
            status: n.status.as_str().to_owned(),
            updated_at: rfc3339(n.updated_at),
        }
    }
}

/// A page of numbers.
#[derive(Debug, Serialize, ToSchema)]
pub struct NumberList {
    /// The numbers, in id order.
    pub data: Vec<NumberView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    pub next_cursor: Option<String>,
}

/// A number's throughput.
#[derive(Debug, Serialize, ToSchema)]
pub struct ThroughputView {
    /// Throughput level, as Meta reports it.
    pub level: Option<String>,
}

/// A number's live details, from Meta.
#[derive(Debug, Serialize, ToSchema)]
pub struct NumberDetails {
    /// Phone number id.
    pub phone_number_id: String,
    /// Its WABA.
    pub waba_id: String,
    /// The service's connection status.
    pub status: String,
    /// The number as WhatsApp displays it.
    pub display_phone_number: Option<String>,
    /// Approved display name.
    pub verified_name: Option<String>,
    /// Quality rating, as Meta reports it (`GREEN`, `YELLOW`, `RED`, …).
    pub quality_rating: Option<String>,
    /// Display name review status, as Meta reports it.
    pub name_status: Option<String>,
    /// Throughput.
    pub throughput: Option<ThroughputView>,
}

/// A business profile.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileView {
    /// "About" text.
    pub about: Option<String>,
    /// Address.
    pub address: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Contact email.
    pub email: Option<String>,
    /// Websites.
    pub websites: Vec<String>,
    /// Industry, as Meta names it (`RETAIL`, `OTHER`, …).
    pub vertical: Option<String>,
}

/// Fields to change; absent ones stay as they are.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfilePatch {
    /// "About" text.
    pub about: Option<String>,
    /// Address (at most 256 characters).
    pub address: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Contact email.
    pub email: Option<String>,
    /// Websites.
    pub websites: Option<Vec<String>>,
    /// Industry, as Meta names it (`UNDEFINED` and `NOT_A_BIZ` cannot be
    /// set).
    pub vertical: Option<String>,
}

/// A value of Meta's enum, as its wire string.
fn wire<T: Serialize>(value: Option<&T>) -> Option<String> {
    value
        .and_then(|v| serde_json::to_value(v).ok())
        .and_then(|v| v.as_str().map(str::to_owned))
}

fn profile_view(profile: Profile) -> ProfileView {
    ProfileView {
        vertical: wire(profile.vertical.as_ref()),
        about: profile.about,
        address: profile.address,
        description: profile.description,
        email: profile.email,
        websites: profile.websites,
    }
}

fn details(owned: &OwnedNumber, info: PhoneNumberInfo) -> NumberDetails {
    NumberDetails {
        phone_number_id: owned.phone_number_id().as_str().to_owned(),
        waba_id: owned.waba_id().as_str().to_owned(),
        status: "connected".to_owned(),
        quality_rating: wire(info.quality_rating.as_ref()),
        name_status: wire(info.name_status.as_ref()),
        throughput: info.throughput.map(|t| ThroughputView { level: t.level }),
        display_phone_number: info.display_phone_number,
        verified_name: info.verified_name,
    }
}

/// `GET /v1/wabas`: the tenant's WABAs.
#[utoipa::path(
    get,
    path = "/v1/wabas",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("limit" = Option<u32>, Query, description = "Page size, 1 to 100 (default 50)"),
        ("cursor" = Option<String>, Query, description = "`next_cursor` of the previous page"),
    ),
    responses(
        (status = 200, description = "The tenant's WABAs", body = WabaList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit` or `cursor`", body = ErrorBody),
    )
)]
pub async fn list_wabas(
    State(state): State<AppState>,
    caller: Caller,
    PageQuery(page): PageQuery,
) -> Result<Json<WabaList>, ApiError> {
    let listing = state.store().wabas(caller.tenant(), &page).await?;
    let next_cursor = next_cursor(&listing);
    Ok(Json(WabaList {
        data: listing.items.into_iter().map(WabaView::from).collect(),
        next_cursor,
    }))
}

/// `GET /v1/numbers`: the tenant's numbers with their connection status.
#[utoipa::path(
    get,
    path = "/v1/numbers",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("limit" = Option<u32>, Query, description = "Page size, 1 to 100 (default 50)"),
        ("cursor" = Option<String>, Query, description = "`next_cursor` of the previous page"),
    ),
    responses(
        (status = 200, description = "The tenant's numbers", body = NumberList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit` or `cursor`", body = ErrorBody),
    )
)]
pub async fn list_numbers(
    State(state): State<AppState>,
    caller: Caller,
    PageQuery(page): PageQuery,
) -> Result<Json<NumberList>, ApiError> {
    let listing = state.store().numbers(caller.tenant(), &page).await?;
    let next_cursor = next_cursor(&listing);
    Ok(Json(NumberList {
        data: listing.items.into_iter().map(NumberView::from).collect(),
        next_cursor,
    }))
}

/// `GET /v1/numbers/{pn}`: the number's live details from Meta.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "Live details", body = NumberDetails),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub async fn get_number(
    State(state): State<AppState>,
    owned: OwnedNumber,
) -> Result<Json<NumberDetails>, ApiError> {
    let info = owned
        .client()
        .phone_number(owned.phone_number_id().clone())
        .get(&NUMBER_FIELDS)
        .await;
    match info {
        Ok(info) => Ok(Json(details(&owned, info))),
        Err(error) => Err(owned.failed(&state, &error).await.with_details(&error)),
    }
}

async fn read_profile(state: &AppState, owned: &OwnedNumber) -> Result<ProfileView, ApiError> {
    let profile = owned
        .client()
        .business_profile(owned.phone_number_id().clone())
        .get(&PROFILE_FIELDS)
        .await;
    match profile {
        Ok(profile) => Ok(profile_view(profile)),
        Err(error) => Err(owned.failed(state, &error).await.with_details(&error)),
    }
}

/// `GET /v1/numbers/{pn}/profile`: the business profile.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/profile",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The profile", body = ProfileView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub async fn get_profile(
    State(state): State<AppState>,
    owned: OwnedNumber,
) -> Result<Json<ProfileView>, ApiError> {
    read_profile(&state, &owned).await.map(Json)
}

/// `PATCH /v1/numbers/{pn}/profile`: change the given fields, answer the
/// whole profile.
#[utoipa::path(
    patch,
    path = "/v1/numbers/{pn}/profile",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = ProfilePatch,
    responses(
        (status = 200, description = "The profile after the change", body = ProfileView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`address` over 256 characters, a vertical that cannot be set), or Meta's `invalid_parameter`", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub async fn patch_profile(
    State(state): State<AppState>,
    owned: OwnedNumber,
    ApiJson(patch): ApiJson<ProfilePatch>,
) -> Result<Json<ProfileView>, ApiError> {
    let vertical = patch
        .vertical
        .map(|v| serde_json::from_value::<Vertical>(serde_json::Value::String(v)))
        .transpose()
        .map_err(|_| ApiError::invalid("vertical"))?;
    let update = ProfileUpdate {
        about: patch.about,
        address: patch.address,
        description: patch.description,
        email: patch.email,
        websites: patch.websites,
        vertical,
        ..ProfileUpdate::default()
    };
    // Nothing to change: no write, just the profile.
    if update != ProfileUpdate::default() {
        let updated = owned
            .client()
            .business_profile(owned.phone_number_id().clone())
            .update(&update)
            .await;
        if let Err(error) = updated {
            return Err(owned.failed(&state, &error).await.with_details(&error));
        }
    }
    read_profile(&state, &owned).await.map(Json)
}

/// `DELETE /v1/wabas/{waba_id}`: disconnect. Unsubscribes the app from the
/// WABA with its token; only then deletes the token and the bindings. When
/// Meta refuses or fails, nothing is deleted.
#[utoipa::path(
    delete,
    path = "/v1/wabas/{waba_id}",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Disconnected: token and bindings deleted"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (nothing deleted)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required` (nothing deleted)", body = ErrorBody),
        (status = 502, description = "Unsubscribing failed: nothing deleted", body = ErrorBody),
        (status = 504, description = "`timeout`: nothing deleted", body = ErrorBody),
    )
)]
pub async fn disconnect_waba(
    State(state): State<AppState>,
    owned: OwnedWaba,
) -> Result<StatusCode, ApiError> {
    let unsubscribed = owned
        .client()
        .waba(owned.waba_id().clone())
        .unsubscribe_app()
        .await;
    if let Err(error) = unsubscribed {
        return Err(owned.failed(&state, &error).await.with_details(&error));
    }
    owned.forget(&state).await?;
    Ok(StatusCode::NO_CONTENT)
}
