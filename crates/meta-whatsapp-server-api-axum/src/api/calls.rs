//! Calling (scope `calls`; docs/design/server.md, section 4.2; roadmap
//! M5e): a number's calling settings, a user's call permission, the call a
//! business places and the actions on a call.
//!
//! | Route | Graph call, with the tenant's WABA token |
//! | --- | --- |
//! | `GET /v1/numbers/{pn}/call-settings` | `GET /{pn}/settings` (the `calling` object; the SIP password is never asked for) |
//! | `PATCH /v1/numbers/{pn}/call-settings` | `POST /{pn}/settings` with `{"calling": …}` |
//! | `GET /v1/numbers/{pn}/call-permissions` | `GET /{pn}/call_permissions?user_wa_id=…` or `?recipient=…` |
//! | `POST /v1/numbers/{pn}/calls` | `POST /{pn}/calls`, `action: connect` (a business-initiated call) |
//! | `POST /v1/numbers/{pn}/calls/{call_id}/pre-accept`, `…/accept`, `…/reject`, `…/terminate` | `POST /{pn}/calls`, `action` and `call_id` |
//!
//! Call analytics are the WABA's: `GET /v1/wabas/{waba_id}/analytics/calls`
//! (the analytics routes, scope `analytics`).
//!
//! **A call is the number's, or it does not exist** (the design's M5.2,
//! D210): the [`OwnedCall`] extractor looks the call up in the service's
//! records of the number's calls *before the tenant's token is opened*, so
//! another tenant's call, another number's, an unrecorded or expired one and
//! an id no call has are all `404 not_found`, with the vault unread and
//! Meta not asked (the core's [`meta_whatsapp_server_core::calls`] says how
//! the records are kept). Placing a call takes an `Idempotency-Key`.
//!
//! An SDP offer or answer and a SIP password are secrets: the first two are
//! in the request that carries them and in no log, error or audit event;
//! the password is never asked of Meta and never answered. Meta's texts
//! are answered nowhere here: its code and the service's kind only.

use std::collections::BTreeMap;

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::calls::{
    self as domain, AcceptRequest, Announcement, Callee, Calls, ConnectRequest,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::{ApiJson, rfc3339};
use super::templates::ApiQuery;
use crate::auth::{Caller, OwnedCall, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

// ─── The calling settings ────────────────────────────────────────────────

/// A media id Meta writes as a number and the service as a string.
fn string_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Text(String),
        Number(serde_json::Number),
    }
    Ok(Option::<Raw>::deserialize(d)?.map(|raw| match raw {
        Raw::Text(text) => text,
        Raw::Number(number) => number.to_string(),
    }))
}

/// Where call icons show.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallIcons)]
pub struct IconsDoc {
    /// Two-letter country codes: the call icon shows only to users of
    /// these countries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restrict_to_user_countries: Option<Vec<String>>,
}

/// One day's window of the call hours.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallWeeklyHours)]
pub struct WeeklyDoc {
    /// `MONDAY` to `SUNDAY`.
    pub day_of_week: String,
    /// `HHMM` (`0900`), 24-hour; `HH:MM` is taken too.
    pub open_time: String,
    /// `HHMM`, after `open_time`.
    pub close_time: String,
}

/// A holiday's window.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallHolidayHours)]
pub struct HolidayDoc {
    /// `YYYY-MM-DD`.
    pub date: String,
    /// `HHMM`.
    pub start_time: String,
    /// `HHMM`, after `start_time`.
    pub end_time: String,
}

/// The hours calls are taken. Replaced as a whole by a change.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallHours)]
pub struct HoursDoc {
    /// `ENABLED` or `DISABLED`.
    pub status: String,
    /// An IANA timezone id (`America/Manaus`).
    pub timezone_id: String,
    /// At least one window; at most two a day.
    #[serde(default)]
    pub weekly_operating_hours: Vec<WeeklyDoc>,
    /// At most 20 holiday windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holiday_schedule: Option<Vec<HolidayDoc>>,
}

/// A SIP server. Its password is not here: the service never asks Meta for
/// it and never answers it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallSipServer)]
pub struct SipServerDoc {
    /// The server's host name.
    pub hostname: String,
    /// Its port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Parameters of the request URI's user part, at most 128 characters
    /// each, key and value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub request_uri_user_params: BTreeMap<String, String>,
}

/// SIP signalling.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallSip)]
pub struct SipDoc {
    /// `ENABLED` or `DISABLED`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// `ENABLED` or `DISABLED`: whether call events are also delivered as
    /// webhooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_delivery: Option<String>,
    /// At most one server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub servers: Option<Vec<SipServerDoc>>,
}

/// Audio settings.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallAudio)]
pub struct AudioDoc {
    /// `PCMA` and/or `PCMU`, beyond Opus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_codecs: Option<Vec<String>>,
}

/// The default voicemail announcement.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallVoicemailAnnouncement)]
pub struct VoicemailDefaultDoc {
    /// The media id of an audio file uploaded **on this number**: any other
    /// is `404 not_found`, nothing changed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "string_or_number"
    )]
    pub announcement_media_id: Option<String>,
    /// Seconds before the `TIMEOUT` trigger, 0 to 30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u8>,
}

/// Voicemail audio.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallVoicemailAudio)]
pub struct VoicemailAudioDoc {
    /// The default announcement.
    pub default: VoicemailDefaultDoc,
}

/// Voicemail.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallVoicemail)]
pub struct VoicemailDoc {
    /// `ENABLED` or `DISABLED`.
    pub status: String,
    /// `REJECT` and/or `TIMEOUT`; at least one when enabled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<String>,
    /// The announcement; required when enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<VoicemailAudioDoc>,
}

/// A number's calling settings, as Meta's `calling` object has them: what
/// a change sets (at least one field) and what the read answers.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallingSettings)]
pub struct SettingsDoc {
    /// `ENABLED` or `DISABLED`: whether the number takes calls at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// `DEFAULT` or `DISABLE_ALL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_icon_visibility: Option<String>,
    /// Where the call icon shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_icons: Option<IconsDoc>,
    /// The hours calls are taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_hours: Option<HoursDoc>,
    /// `ENABLED` or `DISABLED`: whether users are offered to grant a call
    /// permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_permission_status: Option<String>,
    /// SIP signalling. A call signalled over SIP is not answered through
    /// these routes; its events reach the service only with
    /// `webhook_delivery` enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sip: Option<SipDoc>,
    /// Audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioDoc>,
    /// Voicemail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voicemail: Option<VoicemailDoc>,
}

/// A restriction Meta put on the number's calling.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CallRestriction)]
pub struct RestrictionView {
    /// `RESTRICTED_BUSINESS_INITIATED_CALLING` or
    /// `RESTRICTED_USER_INITIATED_CALLING`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Why, in Meta's words.
    #[schema(required = true)]
    pub reason: Option<String>,
    /// When it lifts (RFC 3339).
    #[schema(required = true, format = DateTime)]
    pub expiration: Option<String>,
}

/// A number's calling settings, with Meta's restrictions.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CallingSettingsRead)]
pub struct SettingsView {
    /// The settings.
    #[serde(flatten)]
    pub settings: SettingsDoc,
    /// The restrictions Meta reports; empty when none.
    pub restrictions: Vec<RestrictionView>,
}

// ─── The requests ────────────────────────────────────────────────────────

/// A user: a phone number and/or a BSUID. With both, Meta uses the phone
/// number.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallParticipant)]
pub struct CalleeBody {
    /// A phone number in E.164 **with** its `+` (`+16505551234`).
    pub phone: Option<String>,
    /// A business-scoped user id (`US.13491208655302741918`) or a parent
    /// one (`US.ENT.11815799212886844830`).
    pub user_id: Option<String>,
}

impl From<CalleeBody> for Callee {
    fn from(body: CalleeBody) -> Self {
        Self {
            phone: body.phone,
            user_id: body.user_id,
        }
    }
}

/// The `recording` or `transcription` object of a call: **the caller's
/// choice, per call; the service never sets one**. When enabled, Meta
/// plays an announcement of the purpose to both participants first, and a
/// participant who does not consent ends the call.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CallAnnouncement)]
pub struct AnnouncementBody {
    /// `ENABLED` or `DISABLED`.
    pub status: String,
    /// Why, at most 250 characters; required when enabled.
    pub purpose: Option<String>,
    /// The announcement's locale (`en_US`); required when enabled.
    pub announcement_language: Option<String>,
}

impl From<AnnouncementBody> for Announcement {
    fn from(body: AnnouncementBody) -> Self {
        Self {
            status: body.status,
            purpose: body.purpose,
            announcement_language: body.announcement_language,
        }
    }
}

/// A call to place. No `Debug`: it holds the SDP offer.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ConnectCallRequest)]
pub struct ConnectBody {
    /// The user to call; they must have granted the number a call
    /// permission.
    pub to: CalleeBody,
    /// Your SDP offer (RFC 8866), up to 32 KiB, passed to Meta unparsed.
    pub sdp_offer: String,
    /// Up to 512 characters echoed in the call's webhooks.
    pub biz_opaque_callback_data: Option<String>,
    /// Record the call (off unless set).
    pub recording: Option<AnnouncementBody>,
    /// Transcribe the call (off unless set).
    pub transcription: Option<AnnouncementBody>,
}

/// Your answer to a user's call. No `Debug`: it holds the SDP answer.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = PreAcceptCallRequest)]
pub struct PreAcceptBody {
    /// Your SDP answer (RFC 8866), up to 32 KiB, passed to Meta unparsed.
    pub sdp_answer: String,
}

/// Your answer to a user's call, and what to add. No `Debug`: it holds the
/// SDP answer.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = AcceptCallRequest)]
pub struct AcceptBody {
    /// Your SDP answer, the one of the pre-accept if there was one.
    pub sdp_answer: String,
    /// Up to 512 characters echoed in the call's webhooks.
    pub biz_opaque_callback_data: Option<String>,
    /// Record the call (off unless set).
    pub recording: Option<AnnouncementBody>,
    /// Transcribe the call (off unless set).
    pub transcription: Option<AnnouncementBody>,
}

/// Whose call permission to read: a phone number and/or a BSUID.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct PermissionsQuery {
    /// A phone number in E.164 with its `+`; URL-encode the `+`.
    pub phone: Option<String>,
    /// A BSUID or a parent BSUID. With both parameters Meta uses the phone
    /// number.
    pub user_id: Option<String>,
}

// ─── The answers ─────────────────────────────────────────────────────────

/// A limit on an action.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CallPermissionLimit)]
pub struct LimitView {
    /// ISO 8601 duration (`PT24H`).
    pub time_period: String,
    /// Allowed within the period.
    pub max_allowed: u32,
    /// Used within the period.
    pub current_usage: u32,
    /// When the limit resets (RFC 3339); present once it is reached.
    #[schema(required = true, format = DateTime)]
    pub limit_expiration_time: Option<String>,
}

/// What the business may do with a user's call permission.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CallPermissionAction)]
pub struct ActionView {
    /// `start_call` or `send_call_permission_request`.
    pub action_name: String,
    /// Whether it is allowed right now, every limit considered.
    pub can_perform_action: bool,
    /// The limits on it.
    pub limits: Vec<LimitView>,
}

/// A user's call permission for the number.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CallPermissions)]
pub struct PermissionsView {
    /// `no_permission`, `temporary`, `permanent`, `granted`, `pending`,
    /// `denied` or `expired`, as Meta writes it.
    pub status: String,
    /// When a temporary permission expires (RFC 3339).
    #[schema(required = true, format = DateTime)]
    pub expiration_time: Option<String>,
    /// What the business may do now.
    pub actions: Vec<ActionView>,
}

/// The call that was placed.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CallPlaced)]
pub struct StartedView {
    /// Its id (`wacid.…`): what the actions and the `calls` events name.
    pub call_id: String,
}

// ─── Helpers ─────────────────────────────────────────────────────────────

fn calls<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    number: &'a OwnedNumber,
) -> Result<Calls<'a>, ApiError> {
    Ok(Calls::new(
        state.authz(),
        caller,
        number.capability(state),
        state.call_records(),
    )?
    .observed_by(state.metrics()))
}

/// The settings' change from a body: the document's strict type, written
/// back as Meta's object and read by the core.
fn change(body: &SettingsDoc) -> Result<domain::SettingsChange, ApiError> {
    let object = serde_json::to_value(body).map_err(|_| ApiError::invalid("body"))?;
    Ok(domain::settings_change(object)?)
}

/// The document's settings from the core's, through Meta's object.
fn settings_doc(settings: &domain::Settings) -> Result<SettingsDoc, ApiError> {
    let object = serde_json::to_value(&settings.calling).map_err(|_| ApiError::internal())?;
    serde_json::from_value(object).map_err(|_| {
        ApiError::from(meta_whatsapp_server_core::error::ServiceError::new(
            meta_whatsapp_server_core::error::ErrorCode::UPSTREAM,
        ))
    })
}

// ─── Routes ──────────────────────────────────────────────────────────────

/// `GET /v1/numbers/{pn}/call-settings`: the number's calling settings.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/call-settings",
    tag = "calls",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The settings; a SIP server's password is never in it", body = SettingsView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_call_settings(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<SettingsView>, ApiError> {
    let settings = calls(&state, &caller, &owned)?.settings().await?;
    let restrictions = settings
        .restrictions()
        .into_iter()
        .map(|r| RestrictionView {
            kind: r.kind,
            reason: r.reason,
            expiration: r.expiration.map(rfc3339),
        })
        .collect();
    Ok(Json(SettingsView {
        settings: settings_doc(&settings)?,
        restrictions,
    }))
}

/// `PATCH /v1/numbers/{pn}/call-settings`: change the number's calling
/// settings; only the fields set change, `call_hours` as a whole.
#[utoipa::path(
    patch,
    path = "/v1/numbers/{pn}/call-settings",
    tag = "calls",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = SettingsDoc,
    responses(
        (status = 204, description = "Changed; users' apps may take up to 7 days to show it. Audited by section name"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or a voicemail announcement that is not this number's media (only the lookup was sent)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `update` (nothing to change), `body`, or the field at fault (`call_hours.status`, `sip.servers`, `voicemail.triggers`, …)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the settings may have been changed", body = ErrorBody),
    )
)]
pub(crate) async fn update_call_settings(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiJson(body): ApiJson<SettingsDoc>,
) -> Result<StatusCode, ApiError> {
    let change = change(&body)?;
    calls(&state, &caller, &owned)?
        .update_settings(&change)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /v1/numbers/{pn}/call-permissions`: whether the number may call a
/// user, and what it may do next.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/call-permissions",
    tag = "calls",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        PermissionsQuery,
    ),
    responses(
        (status = 200, description = "The user's permission", body = PermissionsView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `phone`, `user_id`, `user` (neither given) or `query`: nothing asked of Meta", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_call_permissions(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiQuery(query): ApiQuery<PermissionsQuery>,
) -> Result<Json<PermissionsView>, ApiError> {
    let user = domain::callee(
        Callee {
            phone: query.phone,
            user_id: query.user_id,
        },
        "",
        "user",
    )?;
    let permissions = calls(&state, &caller, &owned)?.permissions(&user).await?;
    Ok(Json(PermissionsView {
        status: permissions.status,
        expiration_time: permissions.expiration_time.map(rfc3339),
        actions: permissions
            .actions
            .into_iter()
            .map(|a| ActionView {
                action_name: a.action_name,
                can_perform_action: a.can_perform_action,
                limits: a
                    .limits
                    .into_iter()
                    .map(|l| LimitView {
                        time_period: l.time_period,
                        max_allowed: l.max_allowed,
                        current_usage: l.current_usage,
                        limit_expiration_time: l.limit_expiration_time.map(rfc3339),
                    })
                    .collect(),
            })
            .collect(),
    }))
}

/// Place the call: `201` with its id.
async fn connect(service: &Calls<'_>, request: &ConnectRequest) -> Result<Success, ApiError> {
    let started = service.connect(request).await?;
    Success::json(
        StatusCode::CREATED,
        &StartedView {
            call_id: started.call_id,
        },
    )
}

/// `POST /v1/numbers/{pn}/calls`: place a call to a user, with your SDP
/// offer.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/calls",
    tag = "calls",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never ring the user twice"),
    ),
    request_body = ConnectBody,
    responses(
        (status = 201, description = "Placed: the user's phone rings. The call's events arrive as `call_updated` and `call_status_updated` events, and the call is the number's here from now on", body = StartedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`, or Meta's refusal", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `to`, `sdp_offer`, `biz_opaque_callback_data`, `recording`, `transcription` or `body`; `idempotency_key_reused`: nothing sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed (code 138006 is a missing call permission: read it with the permissions route)", body = ErrorBody),
        (status = 504, description = "`timeout`: the call may have been placed", body = ErrorBody),
    )
)]
pub(crate) async fn connect_call(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::connect_call(owned.phone_number_id(), &body);
    let Ok(ConnectBody {
        to,
        sdp_offer,
        biz_opaque_callback_data,
        recording,
        transcription,
    }) = serde_json::from_value::<ConnectBody>(body)
    else {
        return ApiError::invalid("body").into_response();
    };
    let request = ConnectRequest {
        to: to.into(),
        sdp_offer,
        biz_opaque_callback_data,
        recording: recording.map(Announcement::from),
        transcription: transcription.map(Announcement::from),
    };
    let service = match calls(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, connect(&service, &request))
        .await
}

/// `POST /v1/numbers/{pn}/calls/{call_id}/pre-accept`: answer a user's
/// call so that media can connect before it is accepted.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/calls/{call_id}/pre-accept",
    tag = "calls",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("call_id" = String, Path, description = "Call id, as the `calls` event gave it"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = PreAcceptBody,
    responses(
        (status = 204, description = "Pre-accepted. Audited"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such call among the number's (the call's last event is older than 24 hours, or none arrived); nothing sent", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `sdp_answer` or `body`: nothing sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: not repeated; read the call's events", body = ErrorBody),
    )
)]
pub(crate) async fn pre_accept_call(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedCall,
    ApiJson(body): ApiJson<PreAcceptBody>,
) -> Result<StatusCode, ApiError> {
    calls(&state, &caller, owned.number())?
        .pre_accept(owned.capability(&state), &body.sdp_answer)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/numbers/{pn}/calls/{call_id}/accept`: accept a user's call.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/calls/{call_id}/accept",
    tag = "calls",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("call_id" = String, Path, description = "Call id, as the `calls` event gave it"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = AcceptBody,
    responses(
        (status = 204, description = "Accepted: start sending media. Audited, with whether recording or transcription was enabled"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such call among the number's; nothing sent", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `sdp_answer`, `biz_opaque_callback_data`, `recording`, `transcription` or `body`: nothing sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: not repeated; read the call's events", body = ErrorBody),
    )
)]
pub(crate) async fn accept_call(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedCall,
    ApiJson(body): ApiJson<AcceptBody>,
) -> Result<StatusCode, ApiError> {
    let request = AcceptRequest {
        sdp_answer: body.sdp_answer,
        biz_opaque_callback_data: body.biz_opaque_callback_data,
        recording: body.recording.map(Announcement::from),
        transcription: body.transcription.map(Announcement::from),
    };
    calls(&state, &caller, owned.number())?
        .accept(owned.capability(&state), &request)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/numbers/{pn}/calls/{call_id}/reject`: reject a user's call.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/calls/{call_id}/reject",
    tag = "calls",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("call_id" = String, Path, description = "Call id, as the `calls` event gave it"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Rejected. Audited"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such call among the number's; nothing sent", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: not repeated; read the call's events", body = ErrorBody),
    )
)]
pub(crate) async fn reject_call(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedCall,
) -> Result<StatusCode, ApiError> {
    calls(&state, &caller, owned.number())?
        .reject(owned.capability(&state))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/numbers/{pn}/calls/{call_id}/terminate`: end a call, in either
/// direction.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/calls/{call_id}/terminate",
    tag = "calls",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("call_id" = String, Path, description = "Call id, as the `calls` event gave it or the placing answered"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Ended. Audited; do it also after the media ended, for an accurate price"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such call among the number's; nothing sent", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: not repeated; read the call's events", body = ErrorBody),
    )
)]
pub(crate) async fn terminate_call(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedCall,
) -> Result<StatusCode, ApiError> {
    calls(&state, &caller, owned.number())?
        .terminate(owned.capability(&state))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
