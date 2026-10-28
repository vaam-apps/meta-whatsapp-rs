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
//!
//! Since roadmap S5c a number's details, its profile's read and change,
//! and the disconnection are the core's
//! ([`meta_whatsapp_server_core::numbers`]: the `Numbers` service on an
//! owned number, the `Waba` service on an owned WABA). This module keeps
//! the routes, the request and answer types with their OpenAPI schema,
//! converts them to and from the core's (every field named on both
//! sides), and lists the tenant's bindings for the admitted caller.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_server_core::numbers::{self as domain, Numbers, Waba};
pub use meta_whatsapp_server_core::numbers::{NUMBER_FIELDS, PROFILE_FIELDS};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::common::{ApiJson, PageParams, PageQuery, next_cursor, rfc3339};
use crate::auth::{Caller, OwnedNumber, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::model::{NumberBinding, NumberStatus, WabaBinding};
use crate::state::AppState;

/// A WABA of the tenant.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Waba)]
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
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// A number's connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = NumberStatus)]
pub enum NumberStatusName {
    /// Usable.
    Connected,
    /// Meta rejected the WABA's token (`190`): attach or onboard it again.
    ReconnectRequired,
    /// Offboarded; history is kept.
    Disconnected,
}

impl From<NumberStatus> for NumberStatusName {
    fn from(status: NumberStatus) -> Self {
        match status {
            NumberStatus::Connected => Self::Connected,
            NumberStatus::ReconnectRequired => Self::ReconnectRequired,
            NumberStatus::Disconnected => Self::Disconnected,
        }
    }
}

/// A number of the tenant and its connection status.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Number)]
pub struct NumberView {
    /// Phone number id.
    pub phone_number_id: String,
    /// Its WABA.
    pub waba_id: String,
    /// Its connection status.
    pub status: NumberStatusName,
    /// When the status last changed (RFC 3339).
    #[schema(format = DateTime)]
    pub updated_at: String,
}

impl From<NumberBinding> for NumberView {
    fn from(n: NumberBinding) -> Self {
        Self {
            phone_number_id: n.phone_number_id.into_inner(),
            waba_id: n.waba_id.into_inner(),
            status: n.status.into(),
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
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// A number's throughput.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Throughput)]
pub struct ThroughputView {
    /// Throughput level, as Meta reports it.
    #[schema(required = true)]
    pub level: Option<String>,
}

/// A number's live details, from Meta.
#[derive(Debug, Serialize, ToSchema)]
pub struct NumberDetails {
    /// Phone number id.
    pub phone_number_id: String,
    /// Its WABA.
    pub waba_id: String,
    /// The service's connection status (`connected`: the details come
    /// from Meta).
    pub status: NumberStatusName,
    /// The number as WhatsApp displays it.
    #[schema(required = true)]
    pub display_phone_number: Option<String>,
    /// Approved display name.
    #[schema(required = true)]
    pub verified_name: Option<String>,
    /// Quality rating, as Meta reports it (`GREEN`, `YELLOW`, `RED`, …).
    #[schema(required = true)]
    pub quality_rating: Option<String>,
    /// Display name review status, as Meta reports it.
    #[schema(required = true)]
    pub name_status: Option<String>,
    /// Throughput.
    #[schema(required = true)]
    pub throughput: Option<ThroughputView>,
}

/// A business profile.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Profile)]
pub struct ProfileView {
    /// "About" text.
    #[schema(required = true)]
    pub about: Option<String>,
    /// Address.
    #[schema(required = true)]
    pub address: Option<String>,
    /// Description.
    #[schema(required = true)]
    pub description: Option<String>,
    /// Contact email.
    #[schema(required = true)]
    pub email: Option<String>,
    /// Websites.
    pub websites: Vec<String>,
    /// Industry, as Meta names it (`RETAIL`, `OTHER`, …).
    #[schema(required = true)]
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

// ─── To and from the core's types ────────────────────────────────────────

impl From<domain::NumberDetails> for NumberDetails {
    fn from(details: domain::NumberDetails) -> Self {
        let domain::NumberDetails {
            phone_number_id,
            waba_id,
            status,
            display_phone_number,
            verified_name,
            quality_rating,
            name_status,
            throughput,
        } = details;
        Self {
            phone_number_id: phone_number_id.into_inner(),
            waba_id: waba_id.into_inner(),
            status: status.into(),
            display_phone_number,
            verified_name,
            quality_rating,
            name_status,
            throughput: throughput.map(|domain::Throughput { level }| ThroughputView { level }),
        }
    }
}

impl From<domain::BusinessProfile> for ProfileView {
    fn from(profile: domain::BusinessProfile) -> Self {
        let domain::BusinessProfile {
            about,
            address,
            description,
            email,
            websites,
            vertical,
        } = profile;
        Self {
            about,
            address,
            description,
            email,
            websites,
            vertical,
        }
    }
}

impl From<ProfilePatch> for domain::ProfileChange {
    fn from(patch: ProfilePatch) -> Self {
        let ProfilePatch {
            about,
            address,
            description,
            email,
            websites,
            vertical,
        } = patch;
        Self {
            about,
            address,
            description,
            email,
            websites,
            vertical,
        }
    }
}

/// The core's number operations on `owned`, counted in `state`'s metrics
/// (`owned` is the state's own: the extractor made it with
/// `state.authz()`, which admits it).
fn numbers<'a>(state: &'a AppState, owned: &'a OwnedNumber) -> Result<Numbers<'a>, ApiError> {
    Ok(Numbers::new(state.authz(), owned.capability())?.observed_by(state.metrics()))
}

/// `GET /v1/wabas`: the tenant's WABAs.
#[utoipa::path(
    get,
    path = "/v1/wabas",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        PageParams,
    ),
    responses(
        (status = 200, description = "The tenant's WABAs", body = WabaList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit` or `cursor`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`)", body = ErrorBody),
    )
)]
pub(crate) async fn list_wabas(
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
        PageParams,
    ),
    responses(
        (status = 200, description = "The tenant's numbers", body = NumberList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit` or `cursor`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`)", body = ErrorBody),
    )
)]
pub(crate) async fn list_numbers(
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
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_number(
    State(state): State<AppState>,
    owned: OwnedNumber,
) -> Result<Json<NumberDetails>, ApiError> {
    let details = numbers(&state, &owned)?.details().await?;
    Ok(Json(NumberDetails::from(details)))
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
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_profile(
    State(state): State<AppState>,
    owned: OwnedNumber,
) -> Result<Json<ProfileView>, ApiError> {
    let profile = numbers(&state, &owned)?.profile().await?;
    Ok(Json(ProfileView::from(profile)))
}

/// `PATCH /v1/numbers/{pn}/profile`: change the given fields, answer the
/// whole profile. (Operation `update_profile`, like `update_tenant`.)
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
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn update_profile(
    State(state): State<AppState>,
    owned: OwnedNumber,
    ApiJson(patch): ApiJson<ProfilePatch>,
) -> Result<Json<ProfileView>, ApiError> {
    let profile = numbers(&state, &owned)?
        .update_profile(patch.into())
        .await?;
    Ok(Json(ProfileView::from(profile)))
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
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling (nothing deleted)", body = ErrorBody),
        (status = 502, description = "Unsubscribing failed: nothing deleted", body = ErrorBody),
        (status = 504, description = "`timeout`: nothing deleted", body = ErrorBody),
    )
)]
pub(crate) async fn disconnect_waba(
    State(state): State<AppState>,
    owned: OwnedWaba,
) -> Result<StatusCode, ApiError> {
    // The core's `Waba::disconnect`: a WABA moved since the capability was
    // made is `503`, retryable, and nothing is unsubscribed or deleted.
    Waba::new(state.authz(), owned.capability())?
        .observed_by(state.metrics())
        .disconnect()
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
    use serde_json::json;

    use super::*;

    /// Every field of the core's answers reaches the service's, and every
    /// field of a patch reaches the core (roadmap S5c: the operations are
    /// the core's, `meta_whatsapp_server_core::numbers`). Decisive: each
    /// field of the conversions.
    #[test]
    fn every_field_crosses_to_and_from_the_core() {
        let details = domain::NumberDetails {
            phone_number_id: PhoneNumberId::new("106540352242922").unwrap(),
            waba_id: WabaId::new("102290129340398").unwrap(),
            status: NumberStatus::Connected,
            display_phone_number: Some("+1 631-555-1111".to_owned()),
            verified_name: Some("John's Cake Shop".to_owned()),
            quality_rating: Some("GREEN".to_owned()),
            name_status: Some("APPROVED".to_owned()),
            throughput: Some(domain::Throughput {
                level: Some("STANDARD".to_owned()),
            }),
        };
        assert_eq!(
            serde_json::to_value(NumberDetails::from(details)).unwrap(),
            json!({
                "phone_number_id": "106540352242922",
                "waba_id": "102290129340398",
                "status": "connected",
                "display_phone_number": "+1 631-555-1111",
                "verified_name": "John's Cake Shop",
                "quality_rating": "GREEN",
                "name_status": "APPROVED",
                "throughput": {"level": "STANDARD"}
            })
        );
        let profile = domain::BusinessProfile {
            about: Some("Succulent specialists!".to_owned()),
            address: Some("1 Hacker Way, Menlo Park, CA 94025".to_owned()),
            description: Some("At Lucky Shrub, we specialize in providing a...".to_owned()),
            email: Some("lucky@luckyshrub.com".to_owned()),
            websites: vec!["https://www.luckyshrub.com/".to_owned()],
            vertical: Some("RETAIL".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(ProfileView::from(profile)).unwrap(),
            json!({
                "about": "Succulent specialists!",
                "address": "1 Hacker Way, Menlo Park, CA 94025",
                "description": "At Lucky Shrub, we specialize in providing a...",
                "email": "lucky@luckyshrub.com",
                "websites": ["https://www.luckyshrub.com/"],
                "vertical": "RETAIL"
            })
        );
        let patch: ProfilePatch = serde_json::from_value(json!({
            "about": "a", "address": "b", "description": "c", "email": "d@e.f",
            "websites": ["https://g.example/"], "vertical": "RETAIL"
        }))
        .unwrap();
        assert_eq!(
            domain::ProfileChange::from(patch),
            domain::ProfileChange {
                about: Some("a".to_owned()),
                address: Some("b".to_owned()),
                description: Some("c".to_owned()),
                email: Some("d@e.f".to_owned()),
                websites: Some(vec!["https://g.example/".to_owned()]),
                vertical: Some("RETAIL".to_owned()),
            }
        );
    }
}
