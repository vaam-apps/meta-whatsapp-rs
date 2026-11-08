//! Numbers and profile (scope `numbers`; docs/design/server.md, section
//! 4.2).
//!
//! | Route | Graph calls, with the tenant's WABA token |
//! | --- | --- |
//! | `GET /v1/wabas`, `GET /v1/numbers` | none: the service's bindings |
//! | `GET /v1/numbers/{pn}` | `GET /{pn}?fields=display_phone_number,verified_name,quality_rating,name_status,throughput,messaging_limit_tier` |
//! | `GET /v1/numbers/{pn}/profile` | `GET /{pn}/whatsapp_business_profile?fields=about,address,description,email,profile_picture_url,websites,vertical` |
//! | `PATCH /v1/numbers/{pn}/profile` | `POST /{pn}/whatsapp_business_profile` (a picture's `profile_picture_handle` read from the number's own completed upload, roadmap M5c2), then the `GET` above |
//! | `DELETE /v1/wabas/{waba_id}` | `DELETE /{waba_id}/subscribed_apps`; in Solution Partner mode, the credit line revoked (the library's `EmbeddedSignup::offboard`, with the partner's system token); then the token and bindings are deleted |
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
use meta_whatsapp_server_core::uploads::{ProfilePicture, Uploads};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::common::{ApiJson, PageParams, PageQuery, next_cursor, rfc3339};
use crate::auth::{Caller, OwnedNumber, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::model::{NumberBinding, NumberStatus, WabaBinding};
use crate::state::Grant;

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
    /// Messaging limit tier, as Meta reports it (`TIER_250`, `TIER_1K`,
    /// `TIER_10K`, `TIER_100K`, `TIER_UNLIMITED`, …; roadmap M5c3): how
    /// many people the number may start conversations with in 24 hours.
    #[schema(required = true)]
    pub messaging_limit_tier: Option<String>,
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
    /// The profile picture's URL, as Meta answers it (roadmap M5c2).
    #[schema(required = true)]
    pub profile_picture_url: Option<String>,
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
    /// The picture: the `upload_id` of one of this number's completed
    /// resumable uploads (`POST /v1/numbers/{pn}/media-uploads`), a JPEG
    /// or PNG. The service sends the upload's handle as
    /// `profile_picture_handle`; a handle itself is never taken (it cannot
    /// be checked against the tenant and number that obtained it), and an
    /// upload of another tenant or number is `404`, with nothing asked of
    /// Meta (roadmap M5c2).
    pub profile_picture_upload_id: Option<String>,
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
            messaging_limit_tier,
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
            messaging_limit_tier,
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
            profile_picture_url,
            websites,
            vertical,
        } = profile;
        Self {
            about,
            address,
            description,
            email,
            profile_picture_url,
            websites,
            vertical,
        }
    }
}

impl ProfilePatch {
    /// The core's change, with the picture's handle read from the upload
    /// the patch names (`picture`: [`Uploads::profile_picture`]'s answer).
    fn into_change(self, picture: Option<ProfilePicture>) -> domain::ProfileChange {
        let Self {
            about,
            address,
            description,
            email,
            websites,
            vertical,
            profile_picture_upload_id: _,
        } = self;
        domain::ProfileChange {
            about,
            address,
            description,
            email,
            websites,
            vertical,
            picture,
        }
    }
}

/// The core's number operations on `owned`, counted in `state`'s metrics
/// (`owned` is the state's own: the extractor made it with
/// `state.authz()`, which admits it).
fn numbers<'a>(state: &'a Grant, owned: &'a OwnedNumber) -> Result<Numbers<'a>, ApiError> {
    Ok(Numbers::new(state.authz(), owned.capability(state))?.observed_by(state.metrics()))
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
    State(state): State<Grant>,
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
    State(state): State<Grant>,
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
    State(state): State<Grant>,
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
    State(state): State<Grant>,
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
        (status = 404, description = "`not_found`: no such number for this tenant, or no such upload of this number (another tenant's, another number's, expired): nothing asked of Meta", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`address` over 256 characters, a vertical that cannot be set, a website that is not an `http` or `https` URL without credentials, an email without the shape `name@host`, a text with a control, bidi override or zero-width character, `profile_picture_upload_id` that is no id, not an image or not complete), or Meta's `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn update_profile(
    State(state): State<Grant>,
    owned: OwnedNumber,
    ApiJson(patch): ApiJson<ProfilePatch>,
) -> Result<Json<ProfileView>, ApiError> {
    // The picture's handle is the number's own completed upload's, read
    // before anything is sent to Meta: another tenant's or another number's
    // upload is `404`, as an unknown one is (roadmap M5c2).
    let picture = match &patch.profile_picture_upload_id {
        Some(id) => Some(
            Uploads::new(
                state.authz(),
                owned.capability(&state),
                state.upload_sessions(),
                state.settings().app_id.as_ref(),
                state.media_slots(),
            )?
            .profile_picture(id)
            .await?,
        ),
        None => None,
    };
    let profile = numbers(&state, &owned)?
        .update_profile(patch.into_change(picture))
        .await?;
    Ok(Json(ProfileView::from(profile)))
}

/// `GET /v1/wabas/{waba_id}/health-status`: whether messages can be sent
/// through the tenant's WABA (roadmap M5c6, design D440).
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/health-status",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The health status: the overall `can_send_message` and the nodes the tenant owns or already holds (its WABA, its numbers, its templates, the business portfolio, the app). Another tenant's entity ids are not in it", body = super::number_settings::WabaHealthStatusView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant (another tenant's included: nothing was asked of Meta)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_waba_health_status(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
) -> Result<Json<super::number_settings::WabaHealthStatusView>, ApiError> {
    let status = Waba::new(state.authz(), owned.capability(&state))?
        .observed_by(state.metrics())
        .health_status(&caller)
        .await?;
    Ok(Json(status.into()))
}

/// `DELETE /v1/wabas/{waba_id}`: disconnect. Unsubscribes the app from the
/// WABA with its token; only then deletes the token and the bindings. When
/// Meta refuses or fails, nothing is deleted.
///
/// In Solution Partner mode the partner's credit line is revoked between
/// the two (roadmap M3a), with the partner's system token: a revocation
/// that fails deletes nothing (its error carries `step:
/// revoke_credit_line`), and the call can be repeated.
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
        (status = 502, description = "Unsubscribing failed, or, in Solution Partner mode, revoking the credit line (`step: revoke_credit_line`): nothing deleted", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`: the WABA's token or binding changed meanwhile (moved to another tenant: theirs left alone); retryable", body = ErrorBody),
        (status = 504, description = "`timeout`: nothing deleted", body = ErrorBody),
    )
)]
pub(crate) async fn disconnect_waba(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
) -> Result<StatusCode, ApiError> {
    // The core's `Waba::disconnect`: a WABA moved since the capability was
    // made is `503`, retryable, and nothing is unsubscribed or deleted.
    // In Solution Partner mode the credit line is revoked between the two
    // (roadmap M3a), audited for the caller: a failed revocation deletes
    // nothing.
    Waba::new(state.authz(), owned.capability(&state))?
        .observed_by(state.metrics())
        .offboarded_by(state.signups(), &caller)
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
            messaging_limit_tier: Some("TIER_1K".to_owned()),
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
                "throughput": {"level": "STANDARD"},
                "messaging_limit_tier": "TIER_1K"
            })
        );
        let profile = domain::BusinessProfile {
            about: Some("Succulent specialists!".to_owned()),
            address: Some("1 Hacker Way, Menlo Park, CA 94025".to_owned()),
            description: Some("At Lucky Shrub, we specialize in providing a...".to_owned()),
            email: Some("lucky@luckyshrub.com".to_owned()),
            profile_picture_url: Some("https://pps.whatsapp.net/v/t61.24...".to_owned()),
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
                "profile_picture_url": "https://pps.whatsapp.net/v/t61.24...",
                "websites": ["https://www.luckyshrub.com/"],
                "vertical": "RETAIL"
            })
        );
        let patch: ProfilePatch = serde_json::from_value(json!({
            "about": "a", "address": "b", "description": "c", "email": "d@e.f",
            "websites": ["https://g.example/"], "vertical": "RETAIL",
            "profile_picture_upload_id": "up_0123456789abcdef0123456789abcdef"
        }))
        .unwrap();
        assert_eq!(
            patch.into_change(None),
            domain::ProfileChange {
                about: Some("a".to_owned()),
                address: Some("b".to_owned()),
                description: Some("c".to_owned()),
                email: Some("d@e.f".to_owned()),
                websites: Some(vec!["https://g.example/".to_owned()]),
                vertical: Some("RETAIL".to_owned()),
                picture: None,
            }
        );
    }
}
