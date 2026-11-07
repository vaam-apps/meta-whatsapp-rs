//! A number's settings (scope `numbers`; docs/design/server.md, section
//! 4.2; roadmap M5c3): its settings, payload encryption, business
//! username, Official Business Account, compliance information (India),
//! health status, conversational components, and, under scope `erasure`,
//! its contact book.
//!
//! | Route | Graph call, with the tenant's WABA token | `Idempotency-Key` |
//! | --- | --- | --- |
//! | `GET /v1/numbers/{pn}/settings` | `GET /{pn}/settings` | no |
//! | `PATCH /v1/numbers/{pn}/settings` | one `POST /{pn}/settings` or `POST /{pn}` | no (sets a value) |
//! | `PUT`, `DELETE /v1/numbers/{pn}/settings/payload-encryption` | `POST /{pn}/settings` `{"payload_encryption": …}` | no |
//! | `GET`, `PUT`, `DELETE /v1/numbers/{pn}/username` | `GET`, `POST`, `DELETE /{pn}/username` | no |
//! | `GET /v1/numbers/{pn}/username/suggestions` | `GET /{pn}/username_suggestions` | no |
//! | `GET /v1/numbers/{pn}/official-business-account` | `GET /{pn}?fields=official_business_account` | no |
//! | `POST /v1/numbers/{pn}/official-business-account` | `POST /{pn}/official_business_account` | yes; audited |
//! | `GET`, `PUT /v1/numbers/{pn}/compliance-info` | `GET`, `POST /{pn}/business_compliance_info` | no; the `PUT` a legal act |
//! | `GET /v1/numbers/{pn}/health-status` | `GET /{pn}?fields=health_status` | no |
//! | `GET`, `PUT`, `DELETE /v1/numbers/{pn}/webhook-override` | `GET /{pn}?fields=webhook_configuration`, `POST /{pn}` `{"webhook_configuration": …}` | no; the `PUT` off unless the deployment enables it |
//! | `GET`, `PUT /v1/numbers/{pn}/conversational-components` | `GET /{pn}?fields=conversational_automation`, `POST /{pn}/conversational_automation` | no |
//! | `DELETE /v1/numbers/{pn}/contact-book/{bsuid}` | `DELETE /{pn}/contact_book` | no; audited, scope `erasure` |
//!
//! The service's core ([`meta_whatsapp_server_core::number_settings`])
//! says how each is bounded, gated and audited; this module keeps the
//! routes, the request and answer types with their OpenAPI schema. A key
//! is never in an answer, a log or a `Debug`.

use meta_whatsapp_rs::client::common::{HealthEntity, HealthStatus};
use meta_whatsapp_rs::client::phone_numbers::{
    BotCommand, BusinessComplianceInfo, BusinessEntityType, ComplianceInfoUpdate,
    ConversationalAutomation, ConversationalAutomationConfig, CustomerCare, GrievanceOfficer,
    ObaApplication,
};
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequestParts, Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::number_settings::{
    ClientKey, NumberSettings, ObaView, SettingChange, SettingsView,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::ApiJson;
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody, ErrorCode};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

fn service<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedNumber,
) -> Result<NumberSettings<'a>, ApiError> {
    Ok(
        NumberSettings::new(state.authz(), caller, owned.capability(state))?
            .observed_by(state.metrics())
            .with_legal_acts(state.settings().legal_acts)
            .with_webhook_override(
                state.settings().number_webhook_override,
                state.settings().webhooks_out.as_ref(),
            ),
    )
}

/// A value of one of Meta's enums, as its wire string.
fn wire<T: Serialize>(value: &T) -> Option<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
}

// ─── Settings ────────────────────────────────────────────────────────────

/// A number's local storage.
#[derive(Debug, Serialize, ToSchema)]
pub struct LocalStorageView {
    /// Meta's status (`IN_COUNTRY_STORAGE_ENABLED`, `IN_COUNTRY_STORAGE_DISABLED`, …).
    #[schema(required = true)]
    pub status: Option<String>,
    /// The region, when enabled.
    #[schema(required = true)]
    pub data_localization_region: Option<String>,
}

/// A number's payload encryption: never a key.
#[derive(Debug, Serialize, ToSchema)]
pub struct PayloadEncryptionView {
    /// `enabled` or `disabled`.
    #[schema(required = true)]
    pub status: Option<String>,
    /// The fingerprint of the business's key, as Meta computes it.
    #[schema(required = true)]
    pub client_encryption_key_fingerprint: Option<String>,
}

/// A number's settings, as Meta answers them, **without key material**:
/// Meta's `cloud_encryption_key` is not answered (the service's own
/// choice: payload encryption keys are secrets).
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = NumberSettings)]
pub struct NumberSettingsView {
    /// Local storage; `null` when Meta answers none.
    #[schema(required = true)]
    pub local_storage: Option<LocalStorageView>,
    /// Payload encryption; `null` when Meta answers none.
    #[schema(required = true)]
    pub payload_encryption: Option<PayloadEncryptionView>,
}

impl From<SettingsView> for NumberSettingsView {
    fn from(view: SettingsView) -> Self {
        Self {
            local_storage: view.local_storage.map(|s| LocalStorageView {
                status: s.status,
                data_localization_region: s.data_localization_region,
            }),
            payload_encryption: view.payload_encryption.map(|p| PayloadEncryptionView {
                status: p.status,
                client_encryption_key_fingerprint: p.client_encryption_key_fingerprint,
            }),
        }
    }
}

/// Local storage on or off.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalStorageBody {
    /// `true` turns local storage on (the number must not be registered:
    /// Meta's page `local-storage`), `false` off.
    pub enabled: bool,
    /// The region, two uppercase letters: required with `enabled: true`,
    /// refused with `false`.
    pub data_localization_region: Option<String>,
}

/// One setting to change: exactly one of the fields (Meta sets one value
/// per call, so a change either went through or did not).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    /// The identity change check: when on, webhooks carry the user's
    /// `identity_key_hash`.
    pub identity_key_check: Option<bool>,
    /// Local storage.
    pub local_storage: Option<LocalStorageBody>,
    /// `VISIBLE` or `HIDDEN` in WhatsApp's business search.
    pub search_visibility: Option<String>,
    /// Whether security notifications are shown.
    pub security_notifications: Option<bool>,
    /// Whether users are told when the business changes its number.
    pub notify_user_change_number: Option<bool>,
}

impl SettingsPatch {
    fn into_change(self) -> Result<SettingChange, ApiError> {
        let given = usize::from(self.identity_key_check.is_some())
            + usize::from(self.local_storage.is_some())
            + usize::from(self.search_visibility.is_some())
            + usize::from(self.security_notifications.is_some())
            + usize::from(self.notify_user_change_number.is_some());
        if given != 1 {
            return Err(ApiError::invalid("body"));
        }
        if let Some(on) = self.identity_key_check {
            return Ok(SettingChange::IdentityKeyCheck(on));
        }
        if let Some(storage) = self.local_storage {
            return match (storage.enabled, storage.data_localization_region) {
                (true, Some(region)) => Ok(SettingChange::LocalStorage(Some(region))),
                (false, None) => Ok(SettingChange::LocalStorage(None)),
                _ => Err(ApiError::invalid("local_storage")),
            };
        }
        if let Some(visibility) = self.search_visibility {
            return match visibility.as_str() {
                "VISIBLE" => Ok(SettingChange::SearchVisible(true)),
                "HIDDEN" => Ok(SettingChange::SearchVisible(false)),
                _ => Err(ApiError::invalid("search_visibility")),
            };
        }
        if let Some(show) = self.security_notifications {
            return Ok(SettingChange::SecurityNotifications(show));
        }
        self.notify_user_change_number
            .map(SettingChange::NumberChangeNotifications)
            .ok_or_else(|| ApiError::invalid("body"))
    }
}

/// Which setting was changed.
#[derive(Debug, Serialize, ToSchema)]
pub struct SettingChanged {
    /// The field of the request that was applied.
    pub setting: String,
}

/// The business's public key for payload encryption. Handled as a secret:
/// its `Debug` shows a length, and it is in no log and no answer.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PayloadEncryptionBody {
    /// The business's public key, base64 (the standard alphabet; padding
    /// optional). A private key is refused (`422 invalid_request` on this
    /// field, which never quotes it).
    pub client_encryption_key: String,
}

impl std::fmt::Debug for PayloadEncryptionBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PayloadEncryptionBody {{ client_encryption_key: <{} bytes> }}",
            self.client_encryption_key.len()
        )
    }
}

/// `GET /v1/numbers/{pn}/settings`: the number's settings, without key
/// material.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/settings",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The settings (local storage, payload encryption's status and fingerprint; never a key)", body = NumberSettingsView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_number_settings(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<NumberSettingsView>, ApiError> {
    let settings = service(&state, &caller, &owned)?.settings().await?;
    Ok(Json(NumberSettingsView::from(settings)))
}

/// `PATCH /v1/numbers/{pn}/settings`: change one setting.
#[utoipa::path(
    patch,
    path = "/v1/numbers/{pn}/settings",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = SettingsPatch,
    responses(
        (status = 200, description = "The setting was applied", body = SettingChanged),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `body` (not exactly one setting), `local_storage` or `search_visibility`, or Meta's `invalid_parameter`: nothing was changed", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the setting may have been applied (`GET …/settings`)", body = ErrorBody),
    )
)]
pub(crate) async fn update_number_settings(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiJson(patch): ApiJson<SettingsPatch>,
) -> Result<Json<SettingChanged>, ApiError> {
    let change = patch.into_change()?;
    service(&state, &caller, &owned)?.change(&change).await?;
    Ok(Json(SettingChanged {
        setting: change.name().to_owned(),
    }))
}

/// `PUT /v1/numbers/{pn}/settings/payload-encryption`: turn payload
/// encryption on with the business's public key.
#[utoipa::path(
    put,
    path = "/v1/numbers/{pn}/settings/payload-encryption",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = PayloadEncryptionBody,
    responses(
        (status = 204, description = "Payload encryption is on (`GET …/settings` for its status and the key's fingerprint)"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `client_encryption_key` (not base64, or a private key; the value is never quoted) or `body`: nothing was sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`; Meta's texts are withheld", body = ErrorBody),
        (status = 504, description = "`timeout`: read the settings before asking again (not replayed)", body = ErrorBody),
    )
)]
pub(crate) async fn enable_payload_encryption(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiJson(body): ApiJson<PayloadEncryptionBody>,
) -> Result<StatusCode, ApiError> {
    service(&state, &caller, &owned)?
        .enable_payload_encryption(ClientKey::new(body.client_encryption_key))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/numbers/{pn}/settings/payload-encryption`: turn payload
/// encryption off.
#[utoipa::path(
    delete,
    path = "/v1/numbers/{pn}/settings/payload-encryption",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Payload encryption is off"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`; Meta's texts are withheld", body = ErrorBody),
        (status = 504, description = "`timeout`: read the settings (not replayed)", body = ErrorBody),
    )
)]
pub(crate) async fn disable_payload_encryption(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<StatusCode, ApiError> {
    service(&state, &caller, &owned)?
        .disable_payload_encryption()
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Username ────────────────────────────────────────────────────────────

/// A number's business username.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BusinessUsername)]
pub struct UsernameView {
    /// The username; `null` when the number has none.
    #[schema(required = true)]
    pub username: Option<String>,
    /// `approved` or `reserved`.
    #[schema(required = true)]
    pub status: Option<String>,
}

/// A username to adopt or change to.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsernameBody {
    /// 3 to 35 characters; English letters, digits, `.` and `_`; at least
    /// one letter; no `.` first, last or doubled; not starting with `www`
    /// (Meta's page `business-scoped-user-ids`).
    pub username: String,
    /// When the username is in use on another number of the same business
    /// portfolio: `true` moves it here, `false` (the default) fails with
    /// Meta's `147005`.
    pub force_transfer: Option<bool>,
}

/// The usernames WhatsApp reserved for the business portfolio.
#[derive(Debug, Serialize, ToSchema)]
pub struct UsernameSuggestions {
    /// The suggestions, in Meta's order.
    pub data: Vec<String>,
}

/// `GET /v1/numbers/{pn}/username`: the number's business username.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/username",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The username and its status", body = UsernameView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_username(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<UsernameView>, ApiError> {
    let view = service(&state, &caller, &owned)?.username().await?;
    Ok(Json(UsernameView {
        username: view.username,
        status: view.status,
    }))
}

/// `GET /v1/numbers/{pn}/username/suggestions`: the usernames WhatsApp
/// reserved for the business portfolio.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/username/suggestions",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The suggestions", body = UsernameSuggestions),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_username_suggestions(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<UsernameSuggestions>, ApiError> {
    let data = service(&state, &caller, &owned)?
        .username_suggestions()
        .await?;
    Ok(Json(UsernameSuggestions { data }))
}

/// `PUT /v1/numbers/{pn}/username`: adopt a business username, or change
/// the current one.
#[utoipa::path(
    put,
    path = "/v1/numbers/{pn}/username",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = UsernameBody,
    responses(
        (status = 200, description = "The username was set; its status (`approved` or `reserved`)", body = UsernameView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `username` or `body`, or Meta's `invalid_parameter` (`147005`: in use on another number): nothing was set", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the username may have been set (`GET …/username`)", body = ErrorBody),
    )
)]
pub(crate) async fn set_username(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiJson(body): ApiJson<UsernameBody>,
) -> Result<Json<UsernameView>, ApiError> {
    let status = service(&state, &caller, &owned)?
        .set_username(&body.username, body.force_transfer.unwrap_or(false))
        .await?;
    Ok(Json(UsernameView {
        username: Some(body.username),
        status,
    }))
}

/// `DELETE /v1/numbers/{pn}/username`: remove the number's business
/// username.
#[utoipa::path(
    delete,
    path = "/v1/numbers/{pn}/username",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "The username was removed"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed (also when the number had no username to delete)", body = ErrorBody),
        (status = 504, description = "`timeout`: read the username", body = ErrorBody),
    )
)]
pub(crate) async fn delete_username(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<StatusCode, ApiError> {
    service(&state, &caller, &owned)?.delete_username().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Official Business Account ───────────────────────────────────────────

/// A number's Official Business Account status.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = OfficialBusinessAccount)]
pub struct ObaStatusView {
    /// Where the application stands (`NOT_STARTED`, `PENDING`,
    /// `UNDER_REVIEW`, `APPROVED`, `REJECTED`, `EXPIRED`, `CANCELLED`, …).
    #[schema(required = true)]
    pub oba_status: Option<String>,
    /// Meta's explanation of the status.
    #[schema(required = true)]
    pub status_message: Option<String>,
}

impl From<ObaView> for ObaStatusView {
    fn from(view: ObaView) -> Self {
        Self {
            oba_status: view.oba_status,
            status_message: view.status_message,
        }
    }
}

/// An application for Official Business Account status.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ObaApplicationBody {
    /// The business's official website (`http` or `https`, no credentials).
    pub business_website_url: String,
    /// The country the business mainly operates in.
    pub primary_country_of_operation: String,
    /// The business's main language.
    pub primary_language: Option<String>,
    /// The parent business or brand.
    pub parent_business_or_brand: Option<String>,
    /// Links showing the business is notable: 5 to 10 of them (Meta's
    /// page), each `http` or `https`.
    pub supporting_links: Option<Vec<String>>,
    /// Anything else in support of the application.
    pub additional_supporting_information: Option<String>,
}

impl ObaApplicationBody {
    fn into_application(self) -> ObaApplication {
        let mut application =
            ObaApplication::new(self.business_website_url, self.primary_country_of_operation);
        if let Some(language) = self.primary_language {
            application = application.primary_language(language);
        }
        if let Some(parent) = self.parent_business_or_brand {
            application = application.parent_business_or_brand(parent);
        }
        if let Some(links) = self.supporting_links {
            application = application.supporting_links(links);
        }
        if let Some(info) = self.additional_supporting_information {
            application = application.additional_supporting_information(info);
        }
        application
    }
}

/// What Meta answered an application.
#[derive(Debug, Serialize, ToSchema)]
pub struct ObaApplicationAnswer {
    /// Meta's description of the result.
    #[schema(required = true)]
    pub message: Option<String>,
    /// The status after the request.
    #[schema(required = true)]
    pub updated_status: Option<ObaStatusView>,
    /// An id to follow the application with.
    #[schema(required = true)]
    pub tracking_id: Option<String>,
}

/// `GET /v1/numbers/{pn}/official-business-account`: the number's
/// Official Business Account status.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/official-business-account",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The status", body = ObaStatusView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_official_business_account(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<ObaStatusView>, ApiError> {
    let view = service(&state, &caller, &owned)?
        .official_business_account()
        .await?;
    Ok(Json(ObaStatusView::from(view)))
}

/// `POST /v1/numbers/{pn}/official-business-account`: apply for Official
/// Business Account status. Not a legal act (an application for Meta's
/// review): audited, and it reaches Meta whatever `WA_SERVER_LEGAL_ACTS` says.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/official-business-account",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never apply twice (a repeat gets the kept answer, with `Idempotent-Replayed: true`)"),
    ),
    request_body = ObaApplicationBody,
    responses(
        (status = 202, description = "Meta accepted the application for review (`GET …/official-business-account` for where it stands). Meta refuses a new one within 30 days of a rejection", body = ObaApplicationAnswer),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `business_website_url`, `primary_country_of_operation`, `primary_language`, `parent_business_or_brand`, `supporting_links` (5 to 10, `http` or `https`), `additional_supporting_information` or `body`, `idempotency_key_reused`, or Meta's `invalid_parameter`: nothing was asked", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the application may have reached Meta (`GET …/official-business-account` says; not replayed)", body = ErrorBody),
    )
)]
pub(crate) async fn request_official_business_account(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint =
        Fingerprint::request_official_business_account(owned.phone_number_id(), &body);
    let Ok(body) = serde_json::from_value::<ObaApplicationBody>(body) else {
        return ApiError::invalid("body").into_response();
    };
    let service = match service(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    let operation = async {
        let answer = service
            .request_official_business_account(&body.into_application())
            .await?;
        Success::json(
            StatusCode::ACCEPTED,
            &ObaApplicationAnswer {
                message: answer.message,
                updated_status: answer.updated_status.map(ObaStatusView::from),
                tracking_id: answer.tracking_id,
            },
        )
    };
    state.idempotent(&caller, key, fingerprint, operation).await
}

// ─── Compliance information ──────────────────────────────────────────────

/// The grievance officer's contact details.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GrievanceOfficerBody {
    /// Full name (required on a `PUT`).
    pub name: Option<String>,
    /// Email (required on a `PUT`, under 128 characters).
    pub email: Option<String>,
    /// International mobile number.
    pub mobile_number: Option<String>,
    /// International landline number.
    pub landline_number: Option<String>,
}

/// Customer care contact details.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CustomerCareBody {
    /// Email (required on a `PUT`, under 128 characters).
    pub email: Option<String>,
    /// International mobile number.
    pub mobile_number: Option<String>,
    /// International landline number.
    pub landline_number: Option<String>,
}

/// A number's business compliance information (India), as Meta holds it.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ComplianceInfo)]
pub struct ComplianceInfoView {
    /// The legal name of the business.
    #[schema(required = true)]
    pub entity_name: Option<String>,
    /// The legal form, as Meta returns it.
    #[schema(required = true)]
    pub entity_type: Option<String>,
    /// The legal form in words, when it is none of the listed ones.
    #[schema(required = true)]
    pub entity_type_custom: Option<String>,
    /// Whether the business is registered with the authorities.
    #[schema(required = true)]
    pub is_registered: Option<bool>,
    /// The grievance officer.
    #[schema(required = true)]
    pub grievance_officer_details: Option<GrievanceOfficerBody>,
    /// Customer care.
    #[schema(required = true)]
    pub customer_care_details: Option<CustomerCareBody>,
}

impl From<Option<BusinessComplianceInfo>> for ComplianceInfoView {
    fn from(info: Option<BusinessComplianceInfo>) -> Self {
        let Some(info) = info else {
            return Self {
                entity_name: None,
                entity_type: None,
                entity_type_custom: None,
                is_registered: None,
                grievance_officer_details: None,
                customer_care_details: None,
            };
        };
        Self {
            entity_name: info.entity_name,
            entity_type: info.entity_type,
            entity_type_custom: info.entity_type_custom,
            is_registered: info.is_registered,
            grievance_officer_details: info.grievance_officer_details.map(|g| {
                GrievanceOfficerBody {
                    name: g.name,
                    email: g.email,
                    mobile_number: g.mobile_number,
                    landline_number: g.landline_number,
                }
            }),
            customer_care_details: info.customer_care_details.map(|c| CustomerCareBody {
                email: c.email,
                mobile_number: c.mobile_number,
                landline_number: c.landline_number,
            }),
        }
    }
}

/// A business compliance information declaration.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ComplianceInfoBody {
    /// The legal name of the business, 2 to 128 characters.
    pub entity_name: String,
    /// The legal form: `LIMITED_LIABILITY_PARTNERSHIP`,
    /// `SOLE_PROPRIETORSHIP`, `PARTNERSHIP`, `PUBLIC_COMPANY`,
    /// `PRIVATE_COMPANY` or `OTHER`.
    pub entity_type: String,
    /// The legal form in words: required with `OTHER`, refused with any
    /// other.
    pub entity_type_custom: Option<String>,
    /// Whether the business is registered: only with `OTHER` or
    /// `PARTNERSHIP`.
    pub is_registered: Option<bool>,
    /// The grievance officer (`name` and `email` required).
    pub grievance_officer_details: GrievanceOfficerBody,
    /// Customer care (`email` required).
    pub customer_care_details: CustomerCareBody,
}

impl ComplianceInfoBody {
    fn into_update(self) -> Result<ComplianceInfoUpdate, ApiError> {
        let entity_type = BusinessEntityType::from(self.entity_type.as_str());
        if matches!(entity_type, BusinessEntityType::Other(_)) {
            return Err(ApiError::invalid("entity_type"));
        }
        let officer = self.grievance_officer_details;
        let mut grievance = GrievanceOfficer::new(
            officer.name.unwrap_or_default(),
            officer.email.unwrap_or_default(),
        );
        grievance.mobile_number = officer.mobile_number;
        grievance.landline_number = officer.landline_number;
        let care = self.customer_care_details;
        let mut customer_care = CustomerCare::new(care.email.unwrap_or_default());
        customer_care.mobile_number = care.mobile_number;
        customer_care.landline_number = care.landline_number;
        let mut update =
            ComplianceInfoUpdate::new(self.entity_name, entity_type, grievance, customer_care);
        if let Some(custom) = self.entity_type_custom {
            update = update.entity_type_custom(custom);
        }
        if let Some(registered) = self.is_registered {
            update = update.is_registered(registered);
        }
        Ok(update)
    }
}

/// `GET /v1/numbers/{pn}/compliance-info`: the number's business
/// compliance information (India).
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/compliance-info",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "What Meta holds (every field `null` when it holds none)", body = ComplianceInfoView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_compliance_info(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<ComplianceInfoView>, ApiError> {
    let info = service(&state, &caller, &owned)?.compliance_info().await?;
    Ok(Json(ComplianceInfoView::from(info)))
}

/// `PUT /v1/numbers/{pn}/compliance-info`: declare the number's business
/// compliance information (India), replacing what Meta holds. **A legal
/// act**: off until the deployment sets `WA_SERVER_LEGAL_ACTS=on`;
/// audited, never what it says.
#[utoipa::path(
    put,
    path = "/v1/numbers/{pn}/compliance-info",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = ComplianceInfoBody,
    responses(
        (status = 204, description = "Meta holds the declaration"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`feature_not_available` (the deployment has not enabled the legal acts: nothing was asked of Meta), `forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on the field (`entity_name`, `entity_type`, `entity_type_custom`, `is_registered`, `grievance_officer_details.*`, `customer_care_details.*`) or `body`: nothing was sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: read the information", body = ErrorBody),
    )
)]
pub(crate) async fn set_compliance_info(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiJson(body): ApiJson<ComplianceInfoBody>,
) -> Result<StatusCode, ApiError> {
    let service = service(&state, &caller, &owned)?;
    let update = body.into_update()?;
    service.set_compliance_info(&update).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Health status ───────────────────────────────────────────────────────

/// A question to one node of the health status, answered.
#[derive(Debug, Serialize, ToSchema)]
pub struct HealthErrorView {
    /// Meta's error code.
    #[schema(required = true)]
    pub error_code: Option<i64>,
    /// What is wrong.
    #[schema(required = true)]
    pub error_description: Option<String>,
    /// What to do about it.
    #[schema(required = true)]
    pub possible_solution: Option<String>,
}

/// One node of a health status.
#[derive(Debug, Serialize, ToSchema)]
pub struct HealthEntityView {
    /// `PHONE_NUMBER`, `MESSAGE_TEMPLATE`, `WABA`, `BUSINESS` or `APP`.
    pub entity_type: String,
    /// The node's id.
    pub id: String,
    /// `AVAILABLE`, `LIMITED` or `BLOCKED`.
    #[schema(required = true)]
    pub can_send_message: Option<String>,
    /// The same, for receiving a call over SIP.
    #[schema(required = true)]
    pub can_receive_call_sip: Option<String>,
    /// Why a node is `LIMITED`.
    pub additional_info: Vec<String>,
    /// Why a node is `BLOCKED`, with a possible solution.
    pub errors: Vec<HealthErrorView>,
}

/// A number's health status: whether messages can be sent through it,
/// with the status of the number, its WABA, its business portfolio and
/// the app (Meta's page `support/health-status`).
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = NumberHealthStatus)]
pub struct HealthStatusView {
    /// The overall status: `BLOCKED` if any node is blocked, else
    /// `LIMITED` if any is limited, else `AVAILABLE`.
    #[schema(required = true)]
    pub can_send_message: Option<String>,
    /// One entry per node involved.
    pub entities: Vec<HealthEntityView>,
}

impl From<&HealthEntity> for HealthEntityView {
    fn from(entity: &HealthEntity) -> Self {
        Self {
            entity_type: wire(&entity.entity_type).unwrap_or_default(),
            id: entity.id.clone(),
            can_send_message: entity.can_send_message.as_ref().and_then(wire),
            can_receive_call_sip: entity.can_receive_call_sip.as_ref().and_then(wire),
            additional_info: entity.additional_info.clone(),
            errors: entity
                .errors
                .iter()
                .map(|e| HealthErrorView {
                    error_code: e.error_code,
                    error_description: e.error_description.clone(),
                    possible_solution: e.possible_solution.clone(),
                })
                .collect(),
        }
    }
}

impl From<HealthStatus> for HealthStatusView {
    fn from(status: HealthStatus) -> Self {
        Self {
            can_send_message: status.can_send_message.as_ref().and_then(wire),
            entities: status.entities.iter().map(HealthEntityView::from).collect(),
        }
    }
}

/// `GET /v1/numbers/{pn}/health-status`: whether messages can be sent
/// through the number.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/health-status",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The health status", body = HealthStatusView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_health_status(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<HealthStatusView>, ApiError> {
    let status = service(&state, &caller, &owned)?.health_status().await?;
    Ok(Json(HealthStatusView::from(status)))
}

/// A WABA's health status: whether messages can be sent through it, with
/// the nodes the tenant owns or already holds (its WABA, its numbers, its
/// templates, the business portfolio and the app). Another tenant's
/// entity ids are never in it (roadmap M5c6, design D440).
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = WabaHealthStatus)]
pub struct WabaHealthStatusView {
    /// The overall status Meta computed: `BLOCKED` if any node is blocked,
    /// else `LIMITED` if any is limited, else `AVAILABLE`.
    #[schema(required = true)]
    pub can_send_message: Option<String>,
    /// The nodes the tenant may see.
    pub entities: Vec<HealthEntityView>,
}

impl From<HealthStatus> for WabaHealthStatusView {
    fn from(status: HealthStatus) -> Self {
        let HealthStatusView {
            can_send_message,
            entities,
        } = HealthStatusView::from(status);
        Self {
            can_send_message,
            entities,
        }
    }
}

// ─── Webhook override ────────────────────────────────────────────────────

/// A number's webhook override, as the tenant set it. The WABA's override
/// and the app's callback URL are never answered.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = WebhookOverride)]
pub struct WebhookOverrideView {
    /// The URL Meta delivers this number's supported webhooks to instead
    /// of the service; `null` when the number has none.
    #[schema(required = true)]
    pub url: Option<String>,
}

/// An override to set.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = SetWebhookOverride)]
pub struct WebhookOverrideBody {
    /// Where Meta delivers the number's supported webhooks: `https`, 1 to
    /// 200 characters (Meta's page), a destination the deployment's webhook
    /// allow-list admits, as a webhook endpoint's is.
    pub url: String,
    /// The token Meta echoes when it verifies the URL (the same handshake
    /// as the app's callback). A secret: never answered, logged or audited.
    #[schema(value_type = String, format = Password)]
    pub verify_token: meta_whatsapp_rs::core::secret::VerifyToken,
}

/// The deployment's gate on setting a webhook override as an extractor,
/// placed **before** the number's: with `WA_SERVER_NUMBER_WEBHOOK_OVERRIDE`
/// off the set is `403 feature_not_available` before the number is looked
/// up and before the tenant's token is opened from the vault. The core
/// gates (and audits the refusal) again.
pub(crate) struct WebhookOverrideOn;

impl FromRequestParts<Grant> for WebhookOverrideOn {
    type Rejection = ApiError;

    fn from_request_parts(
        _: &mut Parts,
        state: &Grant,
    ) -> impl Future<Output = Result<Self, ApiError>> + Send {
        std::future::ready(if state.settings().number_webhook_override {
            Ok(Self)
        } else {
            Err(ApiError::new(ErrorCode::FEATURE_NOT_AVAILABLE))
        })
    }
}

/// `GET /v1/numbers/{pn}/webhook-override`: the URL the number's webhooks
/// go to instead of the service, if any.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/webhook-override",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The number's own override, `null` when it has none", body = WebhookOverrideView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_webhook_override(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<WebhookOverrideView>, ApiError> {
    let url = service(&state, &caller, &owned)?.webhook_override().await?;
    Ok(Json(WebhookOverrideView { url }))
}

/// `PUT /v1/numbers/{pn}/webhook-override`: make Meta deliver the number's
/// supported webhooks (`messages`, `calls`, `history`, …) to the tenant's
/// URL instead of the service. Off unless the deployment enables it.
#[utoipa::path(
    put,
    path = "/v1/numbers/{pn}/webhook-override",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = WebhookOverrideBody,
    responses(
        (status = 204, description = "The override is set. Audited as `webhook_override_set` (who and which number, never the URL or the token)"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, `feature_not_available` (the deployment has not enabled overrides, `WA_SERVER_NUMBER_WEBHOOK_OVERRIDE`: nothing was asked of Meta), or Meta's refusal (its texts are withheld)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`webhooks_not_configured` (no allow-list to check the URL against), `number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `url` (not `https`, over 200 characters, credentials in it, not listed, an address it resolves to not allowed, or unresolvable: as a webhook endpoint's) or `verify_token` (empty) or `body`: nothing was sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed (for one, it could not verify the URL): see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the override may have been set (`GET …/webhook-override`)", body = ErrorBody),
    )
)]
pub(crate) async fn set_webhook_override(
    State(state): State<Grant>,
    caller: Caller,
    _enabled: WebhookOverrideOn,
    owned: OwnedNumber,
    ApiJson(body): ApiJson<WebhookOverrideBody>,
) -> Result<StatusCode, ApiError> {
    service(&state, &caller, &owned)?
        .set_webhook_override(&body.url, body.verify_token)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/numbers/{pn}/webhook-override`: remove the number's own
/// override, so Meta delivers its webhooks to the service again. Always
/// allowed to the number's owner, whatever the deployment's setting.
#[utoipa::path(
    delete,
    path = "/v1/numbers/{pn}/webhook-override",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "The override is cleared (also when there was none). Audited as `webhook_override_cleared`"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: read the override", body = ErrorBody),
    )
)]
pub(crate) async fn clear_webhook_override(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<StatusCode, ApiError> {
    service(&state, &caller, &owned)?
        .clear_webhook_override()
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Conversational components ───────────────────────────────────────────

/// A slash command.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandBody {
    /// The name, 1 to 32 characters, without the leading `/`; unique.
    pub command_name: String,
    /// The hint shown next to it, at most 256 characters.
    pub command_description: String,
}

/// A number's conversational components.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ConversationalComponents)]
pub struct ComponentsView {
    /// Whether the welcome message is on (`null` when Meta does not say).
    #[schema(required = true)]
    pub enable_welcome_message: Option<bool>,
    /// Ice breakers.
    pub prompts: Vec<String>,
    /// Slash commands.
    pub commands: Vec<CommandBody>,
}

impl From<ConversationalAutomation> for ComponentsView {
    fn from(automation: ConversationalAutomation) -> Self {
        Self {
            enable_welcome_message: automation.enable_welcome_message,
            prompts: automation.prompts,
            commands: automation
                .commands
                .into_iter()
                .map(|c| CommandBody {
                    command_name: c.command_name,
                    command_description: c.command_description,
                })
                .collect(),
        }
    }
}

/// Conversational components to set; an absent field stays as it is.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentsBody {
    /// Turn the welcome message on or off.
    pub enable_welcome_message: Option<bool>,
    /// Ice breakers: at most 4, of 1 to 80 characters (Meta's page
    /// `conversational-components`).
    pub prompts: Option<Vec<String>>,
    /// Slash commands: at most 30.
    pub commands: Option<Vec<CommandBody>>,
}

impl ComponentsBody {
    fn into_config(self) -> ConversationalAutomationConfig {
        let mut config = ConversationalAutomationConfig::new();
        if let Some(on) = self.enable_welcome_message {
            config = config.enable_welcome_message(on);
        }
        if let Some(prompts) = self.prompts {
            config = config.prompts(prompts);
        }
        if let Some(commands) = self.commands {
            config = config.commands(
                commands
                    .into_iter()
                    .map(|c| BotCommand::new(c.command_name, c.command_description)),
            );
        }
        config
    }
}

/// `GET /v1/numbers/{pn}/conversational-components`: the welcome message
/// flag, ice breakers and commands.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/conversational-components",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The components (empty when none are configured)", body = ComponentsView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_conversational_components(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<ComponentsView>, ApiError> {
    let components = service(&state, &caller, &owned)?
        .conversational_components()
        .await?;
    Ok(Json(ComponentsView::from(components)))
}

/// `PUT /v1/numbers/{pn}/conversational-components`: set the welcome
/// message flag, ice breakers and/or commands.
#[utoipa::path(
    put,
    path = "/v1/numbers/{pn}/conversational-components",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = ComponentsBody,
    responses(
        (status = 204, description = "The components were set"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `prompts`, `commands` (a field names the entry: `prompts[0]`, `commands[1].command_name`) or `body`: nothing was sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: read the components", body = ErrorBody),
    )
)]
pub(crate) async fn set_conversational_components(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiJson(body): ApiJson<ComponentsBody>,
) -> Result<StatusCode, ApiError> {
    service(&state, &caller, &owned)?
        .set_conversational_components(&body.into_config())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Contact book ────────────────────────────────────────────────────────

/// Whether an entry existed.
#[derive(Debug, Serialize, ToSchema)]
pub struct ContactBookErased {
    /// `true` if Meta held an entry for the BSUID and deleted it; `false`
    /// if it held none (deleting again is harmless).
    pub deleted: bool,
}

/// `DELETE /v1/numbers/{pn}/contact-book/{bsuid}`: **erase** one user's
/// entry from the business portfolio's contact book, at Meta. Irreversible.
/// A data-subject erasure the tenant performs explicitly, so scope
/// `erasure`; audited, never the BSUID, and not gated by
/// `WA_SERVER_LEGAL_ACTS`.
#[utoipa::path(
    delete,
    path = "/v1/numbers/{pn}/contact-book/{bsuid}",
    tag = "erasure",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("bsuid" = String, Path, description = "The user's business-scoped user id, such as `US.13491208655302741918` (a phone number or a parent BSUID is refused)"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "Meta answered; whether an entry existed", body = ContactBookErased),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden` (no scope `erasure`), `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `bsuid` (never quoted): nothing was asked", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the entry may have been erased", body = ErrorBody),
    )
)]
pub(crate) async fn delete_contact_book_entry(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    Path((_, bsuid)): Path<(String, String)>,
) -> Result<Json<ContactBookErased>, ApiError> {
    let deleted = service(&state, &caller, &owned)?
        .delete_contact_book_entry(&bsuid)
        .await?;
    Ok(Json(ContactBookErased { deleted }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn patch(value: Value) -> Result<SettingChange, ApiError> {
        serde_json::from_value::<SettingsPatch>(value)
            .map_err(|_| ApiError::invalid("body"))?
            .into_change()
    }

    /// A change names exactly one setting, and each reaches its variant.
    /// Decisive: each arm of `SettingsPatch::into_change`.
    #[test]
    fn a_patch_names_one_setting() {
        assert_eq!(
            patch(json!({"identity_key_check": true})).unwrap(),
            SettingChange::IdentityKeyCheck(true)
        );
        assert_eq!(
            patch(json!({"local_storage": {"enabled": true, "data_localization_region": "DE"}}))
                .unwrap(),
            SettingChange::LocalStorage(Some("DE".into()))
        );
        assert_eq!(
            patch(json!({"local_storage": {"enabled": false}})).unwrap(),
            SettingChange::LocalStorage(None)
        );
        assert_eq!(
            patch(json!({"search_visibility": "HIDDEN"})).unwrap(),
            SettingChange::SearchVisible(false)
        );
        assert_eq!(
            patch(json!({"security_notifications": false})).unwrap(),
            SettingChange::SecurityNotifications(false)
        );
        assert_eq!(
            patch(json!({"notify_user_change_number": true})).unwrap(),
            SettingChange::NumberChangeNotifications(true)
        );
        for (bad, field) in [
            (json!({}), "body"),
            (
                json!({"identity_key_check": true, "search_visibility": "HIDDEN"}),
                "body",
            ),
            (json!({"unknown": 1}), "body"),
            (json!({"search_visibility": "visible"}), "search_visibility"),
            (json!({"local_storage": {"enabled": true}}), "local_storage"),
            (
                json!({"local_storage": {"enabled": false, "data_localization_region": "DE"}}),
                "local_storage",
            ),
        ] {
            let error = patch(bad.clone()).unwrap_err();
            assert_eq!(error.field(), Some(field), "{bad}");
        }
    }

    /// The key body's `Debug` shows a length, never the key.
    #[test]
    fn the_key_body_hides_the_key() {
        let body: PayloadEncryptionBody =
            serde_json::from_value(json!({"client_encryption_key": "MIIBIjANBgkqhkiG9w0B"}))
                .unwrap();
        let shown = format!("{body:?}");
        assert!(!shown.contains("MIIB"), "{shown}");
        assert!(shown.contains("<20 bytes>"), "{shown}");
        assert!(
            serde_json::from_value::<PayloadEncryptionBody>(
                json!({"client_encryption_key": "a", "status": "disabled"})
            )
            .is_err()
        );
    }

    /// An entity type Meta does not list is refused; the listed ones pass.
    #[test]
    fn a_declaration_names_a_listed_entity_type() {
        let body = |entity_type: &str| -> ComplianceInfoBody {
            serde_json::from_value(json!({
                "entity_name": "Lucky Shrub Pvt Ltd", "entity_type": entity_type,
                "grievance_officer_details": {"name": "A. Rao", "email": "g@luckyshrub.example"},
                "customer_care_details": {"email": "c@luckyshrub.example"}
            }))
            .unwrap()
        };
        assert!(body("PRIVATE_COMPANY").into_update().is_ok());
        assert_eq!(
            body("COOPERATIVE").into_update().unwrap_err().field(),
            Some("entity_type")
        );
    }
}
