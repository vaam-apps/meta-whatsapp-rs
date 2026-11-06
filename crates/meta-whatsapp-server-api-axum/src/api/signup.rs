//! Embedded Signup (scope `signup`; docs/design/server.md, sections 3.4,
//! 4.2 and 4.6; roadmap M3a), and the operator's re-funding of a revoked
//! business (admin key).
//!
//! | Route | Does |
//! | --- | --- |
//! | `POST /v1/signup/sessions` | an attempt bound to the calling tenant (15 minutes), and what the page's `FB.init` and `FB.login` need; asks Meta nothing |
//! | `POST /v1/signup/complete` | the attempt redeemed for the calling tenant, then the library's onboarding: `GET oauth/access_token` (the code), `GET debug_token`, `GET /{waba_id}?fields=owner_business_info`, `GET /{waba_id}/phone_numbers`, the token stored, `POST /{waba_id}/subscribed_apps`, a Solution Partner's `POST /{waba_id}/assigned_users` and credit line (`GET …/owning_credit_allocation_configs`, `POST /{extended_credit_line_id}/whatsapp_credit_sharing_and_attach`, with the partner's system token), `POST /{pn}/register` (with the merchant's PIN), a coexistence number's `POST /{pn}/smb_app_data` twice |
//! | `GET /v1/signup/attempts` | the tenant's attempts that stopped after their token was stored |
//! | `POST /v1/signup/resume` | an attempt's steps after the stored token, again (a corrected PIN) |
//! | `POST /v1/admin/wabas/{waba_id}/credit-line/reshare` | the operator funds again a business whose credit line was revoked: the same steps, from the stored token, sharing the line again (`reshare_after_revocation`) |
//!
//! The rules are the core's ([`meta_whatsapp_server_core::signup`]): the
//! tenant is the credential's, never the body's; every input is checked
//! before the attempt is redeemed; decision D4 before the redemption and
//! again, atomically, in the library's approval, before anything is
//! stored, subscribed or shared. The state travels in bodies only (URLs
//! reach access logs). The token is in no answer; the PIN and the code are
//! in no answer, log or error (a body of the wrong shape is `422` on
//! `body`, never quoting it); Meta's texts and `details` are never
//! answered. The completion's body is at most 16 KiB.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_server_core::signup::{
    Completed, Completion, Connected, HostedCompletion, Launch, Resumption, WabaResult, WabaStatus,
};
use serde::{Deserialize, Serialize};

use serde_json::Value;
use utoipa::ToSchema;

use super::common::{ApiJson, PageParams, PageQuery, next_cursor, rfc3339};
use crate::auth::{AdminCaller, Caller};
use crate::error::{ApiError, ErrorBody};
use crate::model::SignupAttempt;
use crate::state::Grant;

/// The largest body a signup route reads: the design's 16 KiB for the
/// completion (section 4.1), whose event comes from the browser.
pub const MAX_SIGNUP_BODY_BYTES: usize = 16 * 1024;

// ─── Requests ────────────────────────────────────────────────────────────

/// `POST /v1/signup/sessions`.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = StartSignup)]
pub struct StartSignupBody {
    /// Launch the onboarding of a WhatsApp Business app number
    /// (coexistence: `featureType: whatsapp_business_app_onboarding`).
    /// Default `false`: the Cloud API flow.
    #[serde(default)]
    pub coexistence: bool,
    /// Offer these pre-verified numbers on the number screen, so that the
    /// merchant types no one-time code (roadmap M3f): the ids the
    /// operator reserved for this tenant
    /// (`PUT /v1/admin/pre-verified-numbers/{id}/reservation`); any other
    /// id is `422` on `pre_verified_phone_ids`. At most 20.
    #[serde(default)]
    pub pre_verified_phone_ids: Vec<String>,
}

/// `POST /v1/signup/complete`: what the page collected, relayed by the
/// integrator's backend. No `Debug`: it holds the code and the PIN.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CompleteSignup)]
pub struct CompleteSignupBody {
    /// The attempt's state, from `POST /v1/signup/sessions`.
    pub state: String,
    /// The code `FB.login`'s callback received (`authResponse.code`): 30
    /// seconds, single use.
    pub code: String,
    /// The `WA_EMBEDDED_SIGNUP` message event the page received, as it
    /// arrived (`FINISH`, `FINISH_ONLY_WABA`,
    /// `FINISH_WHATSAPP_BUSINESS_APP_ONBOARDING`, or `CANCEL`).
    #[schema(value_type = HashMap<String, Value>)]
    pub event: Value,
    /// The number's two-step verification PIN, 6 digits, typed by the
    /// merchant: it becomes a new number's PIN, or must match the one it
    /// has. Never stored. Without it, the number is left unregistered
    /// (a coexistence number is registered already: ignored).
    pub pin: Option<String>,
}

/// `POST /v1/signup/hosted`. No `Debug`: it holds the PIN.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CompleteHostedSignup)]
pub struct CompleteHostedBody {
    /// The WABA Meta's `PARTNER_ADDED` named (`waba_info.waba_id`).
    pub waba_id: String,
    /// Its business portfolio, as the webhook named it
    /// (`waba_info.owner_business_id`).
    pub owner_business_id: String,
    /// The number's two-step verification PIN, 6 digits, chosen by the
    /// tenant (Meta's hosted page asks for none): it becomes the number's
    /// PIN. Never stored. Without it, the number is left unregistered.
    pub pin: Option<String>,
}

/// `POST /v1/signup/resume`. No `Debug`: it holds the PIN.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ResumeSignup)]
pub struct ResumeSignupBody {
    /// The attempt's WABA (`GET /v1/signup/attempts`).
    pub waba_id: String,
    /// A corrected PIN, when the registration stopped it
    /// (`register_phone`): 6 digits, never stored.
    pub pin: Option<String>,
}

// ─── Answers ─────────────────────────────────────────────────────────────

/// What `FB.init` and `FB.login` need, and the attempt's state. Never a
/// secret.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = SignupSession)]
pub struct SignupSessionView {
    /// The attempt, bound to the calling tenant: post it back with the
    /// completion, in the body. Single use.
    pub state: String,
    /// When the attempt lapses (RFC 3339): 15 minutes.
    #[schema(format = DateTime)]
    pub expires_at: String,
    /// The Meta app's id, `FB.init`'s `appId`.
    pub app_id: String,
    /// The Facebook Login for Business configuration, `FB.login`'s
    /// `config_id` (also in `launch_options`).
    pub config_id: String,
    /// `FB.init`'s `version`, e.g. `v25.0`.
    pub graph_api_version: String,
    /// `FB.login`'s options, as they are: Embedded Signup v4.
    #[schema(value_type = HashMap<String, Value>)]
    pub launch_options: Value,
}

impl From<Launch> for SignupSessionView {
    fn from(launch: Launch) -> Self {
        Self {
            state: launch.state.as_str().to_owned(),
            expires_at: rfc3339(launch.expires_at),
            app_id: launch.app_id.as_str().to_owned(),
            config_id: launch.config_id,
            graph_api_version: launch.graph_api_version.to_string(),
            launch_options: launch.launch_options,
        }
    }
}

/// How a completion or a resumption ended.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = SignupStatus)]
pub enum SignupStatusName {
    /// The business is onboarded: the WABA and its numbers are the
    /// tenant's.
    Connected,
    /// The merchant left the flow: nothing was redeemed, the attempt may
    /// still complete.
    Cancelled,
}

/// A completion's or a resumption's outcome. Never the token.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = SignupResult)]
pub struct SignupResultView {
    /// `connected` or `cancelled`.
    pub status: SignupStatusName,
    /// `connected`: the verified WABA, now the tenant's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waba_id: Option<String>,
    /// `connected`: every number of the WABA, now the tenant's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone_number_ids: Option<Vec<String>>,
    /// `connected`: the steps that ran, in order (`exchange_code`,
    /// `debug_token`, `verify_assets`, `approve`, `store_token`,
    /// `subscribe_app`, `assign_system_user`, `share_credit_line`,
    /// `register_phone`, `sync_contacts`, `sync_history`; a resumption
    /// starts with `load_token`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps_completed: Option<Vec<String>>,
    /// `connected`: whether it was a coexistence onboarding (the contacts
    /// and history syncs ran as its last steps, once each).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_coexistence_sync: Option<bool>,
    /// `connected`, Solution Partner: the credit allocation funding the
    /// WABA.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_allocation_id: Option<String>,
    /// `connected`, when the deployment binds every granted WABA
    /// (`WA_SIGNUP_EVERY_GRANTED_WABA`): each WABA of the signup and what
    /// became of it, the primary one first. Absent with the option off:
    /// `waba_id` is then the only WABA bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wabas: Option<Vec<SignupWabaView>>,
    /// `connected` after a completion, with the option off: the other
    /// WABAs the event named, which this completion did not bind (the
    /// primary one, `waba_id`, is the only one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_bound_waba_ids: Option<Vec<String>>,
    /// `cancelled`: the screen the merchant left, as Meta names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_step: Option<String>,
}

/// How one WABA of a completion ended.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = SignupWabaStatus)]
pub enum SignupWabaStatusName {
    /// Onboarded and bound to the tenant.
    Connected,
    /// Not the primary WABA, and a token for it was already stored: left
    /// as it was.
    Held,
    /// Another tenant holds it (`409 waba_owned_by_another_tenant`'s
    /// case): nothing was stored for it.
    Refused,
    /// Stopped at `failed_step`.
    Failed,
}

/// One WABA of a completion (roadmap M3f).
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = SignupWaba)]
pub struct SignupWabaView {
    /// The WABA.
    pub waba_id: String,
    /// Whether it is the signup's primary WABA: the one its event claims,
    /// else the first it names.
    pub primary: bool,
    /// What became of it.
    pub status: SignupWabaStatusName,
    /// `connected`: its numbers, now the tenant's.
    pub phone_number_ids: Vec<String>,
    /// `failed`: the step it stopped at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
    /// `failed`: whether `POST /v1/signup/resume` with this `waba_id`
    /// finishes it.
    pub resumable: bool,
    /// `refused`, `failed`: the error code, e.g.
    /// `waba_owned_by_another_tenant`, `onboarding_failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl From<WabaResult> for SignupWabaView {
    fn from(result: WabaResult) -> Self {
        Self {
            waba_id: result.waba_id.as_str().to_owned(),
            primary: result.primary,
            status: match result.status {
                WabaStatus::Connected => SignupWabaStatusName::Connected,
                WabaStatus::Held => SignupWabaStatusName::Held,
                WabaStatus::Refused => SignupWabaStatusName::Refused,
                WabaStatus::Failed => SignupWabaStatusName::Failed,
            },
            phone_number_ids: result
                .phone_number_ids
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            failed_step: result.failed_step,
            resumable: result.resumable,
            code: result.code,
        }
    }
}

impl From<Connected> for SignupResultView {
    fn from(connected: Connected) -> Self {
        Self {
            status: SignupStatusName::Connected,
            waba_id: Some(connected.waba_id.as_str().to_owned()),
            phone_number_ids: Some(
                connected
                    .phone_number_ids
                    .iter()
                    .map(|id| id.as_str().to_owned())
                    .collect(),
            ),
            steps_completed: Some(
                connected
                    .steps_completed
                    .iter()
                    .map(|step| (*step).to_owned())
                    .collect(),
            ),
            needs_coexistence_sync: Some(connected.needs_coexistence_sync),
            credit_allocation_id: connected.credit_allocation_id,
            wabas: (!connected.wabas.is_empty())
                .then(|| connected.wabas.into_iter().map(Into::into).collect()),
            not_bound_waba_ids: (!connected.not_bound_waba_ids.is_empty()).then(|| {
                connected
                    .not_bound_waba_ids
                    .iter()
                    .map(|id| id.as_str().to_owned())
                    .collect()
            }),
            current_step: None,
        }
    }
}

impl From<Completed> for SignupResultView {
    fn from(completed: Completed) -> Self {
        match completed {
            Completed::Connected(connected) => connected.into(),
            Completed::Cancelled { current_step } => Self {
                status: SignupStatusName::Cancelled,
                waba_id: None,
                phone_number_ids: None,
                steps_completed: None,
                needs_coexistence_sync: None,
                credit_allocation_id: None,
                wabas: None,
                not_bound_waba_ids: None,
                current_step,
            },
        }
    }
}

/// An attempt that stopped after its token was stored.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = SignupAttempt)]
pub struct SignupAttemptView {
    /// The attempt's WABA: resume it with this id.
    pub waba_id: String,
    /// The step it stopped at (`subscribe_app`, `assign_system_user`,
    /// `share_credit_line`, `register_phone`, `sync_contacts`,
    /// `sync_history`).
    pub failed_step: String,
    /// When it last stopped (RFC 3339).
    #[schema(format = DateTime)]
    pub updated_at: String,
    /// When it first stopped (RFC 3339).
    #[schema(format = DateTime)]
    pub created_at: String,
}

impl From<SignupAttempt> for SignupAttemptView {
    fn from(attempt: SignupAttempt) -> Self {
        Self {
            waba_id: attempt.waba_id.as_str().to_owned(),
            failed_step: attempt.failed_step,
            updated_at: rfc3339(attempt.updated_at),
            created_at: rfc3339(attempt.created_at),
        }
    }
}

/// A page of the tenant's attempts.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = SignupAttemptPage)]
pub struct SignupAttemptPageView {
    /// The attempts, in byte order of their WABA ids.
    pub data: Vec<SignupAttemptView>,
    /// The next page's cursor, when there is one.
    pub next_cursor: Option<String>,
}

// ─── The routes ──────────────────────────────────────────────────────────

/// `POST /v1/signup/sessions`: start an attempt for the calling tenant.
#[utoipa::path(
    post,
    path = "/v1/signup/sessions",
    tag = "signup",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as: the attempt is bound to it"),
    ),
    request_body = StartSignupBody,
    responses(
        (status = 200, description = "The attempt (`state`, 15 minutes, single use) and what `FB.init` and `FB.login` need. Asks Meta nothing", body = SignupSessionView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`: no `WA_APP_ID` or `WA_ES_CONFIG_ID`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`body`, `pre_verified_phone_ids`: an id the operator did not reserve for this tenant, or more than 20)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`)", body = ErrorBody),
    )
)]
pub(crate) async fn start_signup(
    State(state): State<Grant>,
    caller: Caller,
    ApiJson(body): ApiJson<StartSignupBody>,
) -> Result<Json<SignupSessionView>, ApiError> {
    let launch = state
        .signups()
        .open(state.authz(), &caller)?
        .observed_by(state.metrics())
        .start_with(body.coexistence, &body.pre_verified_phone_ids)
        .await?;
    Ok(Json(launch.into()))
}

/// `POST /v1/signup/complete`: redeem the attempt for the calling tenant,
/// then onboard.
#[utoipa::path(
    post,
    path = "/v1/signup/complete",
    tag = "signup",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as: the attempt must be its own"),
    ),
    request_body = CompleteSignupBody,
    responses(
        (status = 200, description = "`connected` (the WABA and its numbers bound to the tenant, the token stored in the vault, never answered) or `cancelled` (a `CANCEL` event: nothing redeemed)", body = SignupResultView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`stale_attempt`: the attempt expired, was used, or is another tenant's (not burnt by another tenant); `forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 409, description = "`waba_owned_by_another_tenant` (decision D4: the claimed WABA before the attempt is redeemed, the verified one before anything is stored, subscribed or shared), `signup_not_configured`", body = ErrorBody),
        (status = 413, description = "`payload_too_large`: over 16 KiB", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`state`, `code`, `pin`: 6 digits, `event`, `body`), before the attempt is redeemed: it survives", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`)", body = ErrorBody),
        (status = 502, description = "`onboarding_failed`: stopped at `step`; `resumable: true` when the token was stored and the attempt kept (`POST /v1/signup/resume`), `false` when it was not (start again). Meta's code and trace id in `graph`, never its text", body = ErrorBody),
        (status = 503, description = "`storage_unavailable` (the WABA moved while it was onboarded: its token taken back)", body = ErrorBody),
    )
)]
pub(crate) async fn complete_signup(
    State(state): State<Grant>,
    caller: Caller,
    ApiJson(body): ApiJson<CompleteSignupBody>,
) -> Result<Json<SignupResultView>, ApiError> {
    let CompleteSignupBody {
        state: attempt,
        code,
        event,
        pin,
    } = body;
    // Boxed: the library's onboarding makes the future large (clippy's
    // `large_futures`).
    let completed = Box::pin(
        state
            .signups()
            .open(state.authz(), &caller)?
            .observed_by(state.metrics())
            .complete(Completion {
                state: attempt,
                code,
                event,
                pin,
            }),
    )
    .await?;
    Ok(Json(completed.into()))
}

/// `POST /v1/signup/hosted`: claim a hosted Embedded Signup the service
/// saw arrive (`PARTNER_ADDED`) and onboard it for the calling tenant.
#[utoipa::path(
    post,
    path = "/v1/signup/hosted",
    tag = "signup",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as: the WABA becomes its own"),
    ),
    request_body = CompleteHostedBody,
    responses(
        (status = 200, description = "`connected`: the WABA and its numbers bound to the tenant, the token fetched with the deployment's system token and stored in the vault, never answered", body = SignupResultView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no `PARTNER_ADDED` for that WABA and business is waiting (none arrived, it lapsed after 7 days, another call claimed it, or the business differs)", body = ErrorBody),
        (status = 409, description = "`waba_owned_by_another_tenant` (decision D4, before anything is claimed or read from Meta), `signup_not_configured` (hosted signup is off: `WA_HOSTED_SIGNUP`)", body = ErrorBody),
        (status = 413, description = "`payload_too_large`: over 16 KiB", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`waba_id`, `owner_business_id`, `pin`: 6 digits, `body`), before the arrival is claimed; also a WABA the vault already holds (`waba_id`)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`)", body = ErrorBody),
        (status = 502, description = "`onboarding_failed`: stopped at `step`; `resumable: true` when the token was stored (`POST /v1/signup/resume`), `false` when it was not (the arrival is put back: claim it again)", body = ErrorBody),
        (status = 503, description = "`storage_unavailable` (the WABA moved while it was onboarded: its token taken back)", body = ErrorBody),
    )
)]
pub(crate) async fn complete_hosted_signup(
    State(state): State<Grant>,
    caller: Caller,
    ApiJson(body): ApiJson<CompleteHostedBody>,
) -> Result<Json<SignupResultView>, ApiError> {
    let CompleteHostedBody {
        waba_id,
        owner_business_id,
        pin,
    } = body;
    let completed = Box::pin(
        state
            .signups()
            .open(state.authz(), &caller)?
            .observed_by(state.metrics())
            .complete_hosted(HostedCompletion {
                waba_id,
                owner_business_id,
                pin,
            }),
    )
    .await?;
    Ok(Json(completed.into()))
}

/// `GET /v1/signup/attempts`: the calling tenant's attempts that stopped
/// after their token was stored.
#[utoipa::path(
    get,
    path = "/v1/signup/attempts",
    tag = "signup",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        PageParams,
    ),
    responses(
        (status = 200, description = "The tenant's attempts, in byte order of their WABA ids", body = SignupAttemptPageView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`limit`, `cursor`)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`)", body = ErrorBody),
    )
)]
pub(crate) async fn list_signup_attempts(
    State(state): State<Grant>,
    caller: Caller,
    PageQuery(page): PageQuery,
) -> Result<Json<SignupAttemptPageView>, ApiError> {
    let listing = state
        .signups()
        .open(state.authz(), &caller)?
        .attempts(&page)
        .await?;
    let next_cursor = next_cursor(&listing);
    Ok(Json(SignupAttemptPageView {
        data: listing.items.into_iter().map(Into::into).collect(),
        next_cursor,
    }))
}

/// `POST /v1/signup/resume`: rerun an attempt's steps after its stored
/// token.
#[utoipa::path(
    post,
    path = "/v1/signup/resume",
    tag = "signup",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as: the attempt must be its own"),
    ),
    request_body = ResumeSignupBody,
    responses(
        (status = 200, description = "`connected`, as a completion answers it (`steps_completed` starts with `load_token`)", body = SignupResultView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`nothing_to_resume`: no attempt of this tenant's for that WABA (another tenant's included), the WABA no longer its own, or its token gone", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`waba_id`, `pin`: 6 digits, `body`), before anything is read: the attempt survives", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`)", body = ErrorBody),
        (status = 502, description = "`onboarding_failed`: stopped at `step` again; `resumable` as on a completion, the attempt kept at its new step", body = ErrorBody),
    )
)]
pub(crate) async fn resume_signup(
    State(state): State<Grant>,
    caller: Caller,
    ApiJson(body): ApiJson<ResumeSignupBody>,
) -> Result<Json<SignupResultView>, ApiError> {
    let ResumeSignupBody { waba_id, pin } = body;
    let resumed = Box::pin(
        state
            .signups()
            .open(state.authz(), &caller)?
            .observed_by(state.metrics())
            .resume(Resumption { waba_id, pin }),
    )
    .await?;
    Ok(Json(resumed.into()))
}

/// `POST /v1/admin/wabas/{waba_id}/credit-line/reshare`: fund again the
/// business of a WABA whose credit line was revoked (Solution Partner).
/// The operator's explicit, audited decision: no resume and no new signup
/// re-funds a revoked business (decision D14).
#[utoipa::path(
    post,
    path = "/v1/admin/wabas/{waba_id}/credit-line/reshare",
    tag = "admin",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id: one a tenant holds"),
    ),
    responses(
        (status = 200, description = "Funded again: the WABA's steps after its stored token ran, sharing the credit line again (`credit_allocation_id`); audited `credit_line_reshared`", body = SignupResultView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 404, description = "`not_found`: no tenant holds the WABA (or not an id)", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`: not a Solution Partner deployment", body = ErrorBody),
        (status = 502, description = "`onboarding_failed`: stopped at `step` (`resumable`: repeat it)", body = ErrorBody),
    )
)]
pub(crate) async fn reshare_credit_line(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(waba_id): Path<String>,
) -> Result<Json<SignupResultView>, ApiError> {
    let connected = Box::pin(state.signups().reshare_credit_line(
        state.authz(),
        &admin,
        &waba_id,
        state.metrics(),
    ))
    .await?;
    Ok(Json(connected.into()))
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
    use serde_json::json;

    use super::*;

    /// Each outcome answers the design's fields (section 4.2), never one
    /// of the other outcome's, and never a token.
    #[test]
    fn outcomes_answer_the_designs_fields() {
        let connected = SignupResultView::from(Completed::Connected(Connected {
            waba_id: WabaId::new("524126980791429").unwrap(),
            phone_number_ids: vec![PhoneNumberId::new("106540352242922").unwrap()],
            steps_completed: vec!["exchange_code", "store_token"],
            needs_coexistence_sync: false,
            credit_allocation_id: None,
            wabas: Vec::new(),
            not_bound_waba_ids: Vec::new(),
        }));
        assert_eq!(
            serde_json::to_value(&connected).unwrap(),
            json!({
                "status": "connected",
                "waba_id": "524126980791429",
                "phone_number_ids": ["106540352242922"],
                "steps_completed": ["exchange_code", "store_token"],
                "needs_coexistence_sync": false
            })
        );
        // M3f: the WABAs of a several-WABA completion, and the ones not bound.
        let many = SignupResultView::from(Completed::Connected(Connected {
            waba_id: WabaId::new("524126980791429").unwrap(),
            phone_number_ids: Vec::new(),
            steps_completed: Vec::new(),
            needs_coexistence_sync: false,
            credit_allocation_id: None,
            wabas: vec![WabaResult {
                waba_id: WabaId::new("524126980791430").unwrap(),
                primary: false,
                status: WabaStatus::Refused,
                phone_number_ids: Vec::new(),
                failed_step: None,
                resumable: false,
                code: Some("waba_owned_by_another_tenant".to_owned()),
            }],
            not_bound_waba_ids: vec![WabaId::new("524126980791431").unwrap()],
        }));
        let many = serde_json::to_value(&many).unwrap();
        assert_eq!(
            many["wabas"],
            json!([{
                "waba_id": "524126980791430", "primary": false, "status": "refused",
                "phone_number_ids": [], "resumable": false,
                "code": "waba_owned_by_another_tenant"
            }])
        );
        assert_eq!(many["not_bound_waba_ids"], json!(["524126980791431"]));
        let cancelled = SignupResultView::from(Completed::Cancelled {
            current_step: Some("PHONE_NUMBER_SETUP".to_owned()),
        });
        assert_eq!(
            serde_json::to_value(&cancelled).unwrap(),
            json!({"status": "cancelled", "current_step": "PHONE_NUMBER_SETUP"})
        );
        assert_eq!(MAX_SIGNUP_BODY_BYTES, 16 * 1024, "the design's 16 KiB");
    }
}
