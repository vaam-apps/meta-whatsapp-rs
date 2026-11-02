//! A number's registration and its two-step PIN (scope `numbers`;
//! docs/design/server.md, section 4.2, decision D6; roadmap M3d).
//!
//! | Route | Graph call, with the tenant's WABA token | `Idempotency-Key` |
//! | --- | --- | --- |
//! | `POST /v1/numbers/{pn}/request-code` | `POST /{pn}/request_code` | yes |
//! | `POST /v1/numbers/{pn}/verify-code` | `POST /{pn}/verify_code` | no |
//! | `POST /v1/numbers/{pn}/register` | `POST /{pn}/register` | yes, never over the PIN |
//! | `POST /v1/numbers/{pn}/deregister` | `POST /{pn}/deregister` | yes |
//! | `POST /v1/numbers/{pn}/two-step-pin` | `POST /{pn}` `{pin}` | no |
//!
//! The operations are the core's
//! ([`meta_whatsapp_server_core::registration`]: the `Registration`
//! service on an owned number, the attempts counted per number). This
//! module keeps the routes and the request and answer types with their
//! OpenAPI schema. The bodies that carry a PIN or a code have no `Debug`
//! (a `{:?}` cannot print them) and are read by [`ApiJson`], whose
//! refusal never quotes the body; the PIN and the code become the
//! library's secret types before anything else is done with them
//! ([`meta_whatsapp_server_core::registration::pin`],
//! [`meta_whatsapp_server_core::registration::verification_code`]). The
//! answers name the number and what was done, nothing of the request.

use meta_whatsapp_rs::client::phone_numbers::{CodeMethod, DataLocalizationRegion};
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::registration::{self as domain, CodeRequest, Registration};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::common::ApiJson;
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

/// How a verification code is delivered (Meta's `code_method`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[schema(as = CodeMethod)]
pub enum CodeMethodName {
    /// A text message.
    #[serde(rename = "SMS")]
    Sms,
    /// A voice call.
    #[serde(rename = "VOICE")]
    Voice,
}

impl CodeMethodName {
    fn library(self) -> CodeMethod {
        match self {
            Self::Sms => CodeMethod::Sms,
            Self::Voice => CodeMethod::Voice,
        }
    }

    /// Meta's name, as the fingerprint takes it.
    fn as_str(self) -> &'static str {
        match self {
            Self::Sms => "SMS",
            Self::Voice => "VOICE",
        }
    }
}

/// `POST /v1/numbers/{pn}/request-code`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeRequestBody {
    /// `SMS` or `VOICE`.
    pub code_method: CodeMethodName,
    /// The language of the code's message, e.g. `en_US` (not blank).
    pub language: String,
}

/// `POST /v1/numbers/{pn}/verify-code`. No `Debug`: it holds the code.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeCheckBody {
    /// The code the number received (`123-830` or `123830`: hyphens are
    /// dropped). Never answered, stored or logged.
    #[schema(write_only, format = Password)]
    pub code: String,
}

/// `POST /v1/numbers/{pn}/register`. No `Debug`: it holds the PIN.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistrationBody {
    /// The number's two-step verification PIN, six digits, typed by the
    /// merchant for this attempt: the current one, or the one to set when
    /// the number has none. Never answered, stored or logged (decision D6).
    #[schema(write_only, format = Password, pattern = "^[0-9]{6}$")]
    pub pin: String,
    /// Turns local storage on, in this country (a two-letter ISO 3166
    /// code Meta supports, e.g. `CH`). Changing it later takes a
    /// deregistration.
    pub data_localization_region: Option<String>,
}

/// `POST /v1/numbers/{pn}/deregister`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeregistrationBody {
    /// The path's phone number id, again: deregistering stops the number
    /// sending and receiving through the Cloud API until it is registered
    /// again.
    pub confirm: String,
}

/// `POST /v1/numbers/{pn}/two-step-pin`. No `Debug`: it holds the PIN.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TwoStepPinBody {
    /// The new two-step verification PIN, six digits. Never answered,
    /// stored or logged (decision D6).
    #[schema(write_only, format = Password, pattern = "^[0-9]{6}$")]
    pub pin: String,
}

/// What a registration route did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = RegistrationStatus)]
pub enum RegistrationStatusName {
    /// Meta is sending a verification code.
    CodeRequested,
    /// Meta accepted the verification code.
    CodeVerified,
    /// The number is registered for the Cloud API.
    Registered,
    /// The number is deregistered.
    Deregistered,
    /// The two-step verification PIN is set.
    PinSet,
}

/// A registration route's answer: the number and what was done, nothing
/// of the request.
#[derive(Debug, Serialize, ToSchema)]
pub struct RegistrationAnswer {
    /// Phone number id.
    pub phone_number_id: String,
    /// What was done.
    pub status: RegistrationStatusName,
}

/// The core's registration operations on `owned` for `caller`, counted in
/// `state`'s metrics and attempts (`owned` is the state's own: the
/// extractor made it with `state.authz()`, which admits it).
fn registration<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedNumber,
) -> Result<Registration<'a>, ApiError> {
    Ok(Registration::new(
        state.authz(),
        caller,
        owned.capability(state),
        state.registration_attempts(),
    )?
    .observed_by(state.metrics()))
}

fn answer(owned: &OwnedNumber, status: RegistrationStatusName) -> RegistrationAnswer {
    RegistrationAnswer {
        phone_number_id: owned.phone_number_id().as_str().to_owned(),
        status,
    }
}

/// `POST /v1/numbers/{pn}/request-code`: have Meta send the number a
/// verification code, by SMS or voice call.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/request-code",
    tag = "registration",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never ask for a code twice (a repeat gets the kept answer, with `Idempotent-Replayed: true`)"),
    ),
    request_body = CodeRequestBody,
    responses(
        (status = 200, description = "Meta is sending the code", body = RegistrationAnswer),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`, or Meta's `registration` (the number is verified already)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `language` (blank) or `body`, `idempotency_key_reused`, or Meta's `invalid_parameter`: nothing was asked", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget, or the number's verification attempts (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: a code may have been sent", body = ErrorBody),
    )
)]
pub(crate) async fn request_verification_code(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<CodeRequestBody>,
) -> Response {
    let CodeRequestBody {
        code_method,
        language,
    } = body;
    let fingerprint = Fingerprint::request_verification_code(
        owned.phone_number_id(),
        code_method.as_str(),
        &language,
    );
    let request = CodeRequest {
        method: code_method.library(),
        language,
    };
    let operation = async {
        registration(&state, &caller, &owned)?
            .request_code(&request)
            .await?;
        Success::json(
            StatusCode::OK,
            &answer(&owned, RegistrationStatusName::CodeRequested),
        )
    };
    state.idempotent(&caller, key, fingerprint, operation).await
}

/// `POST /v1/numbers/{pn}/verify-code`: submit the code the number
/// received.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/verify-code",
    tag = "registration",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = CodeCheckBody,
    responses(
        (status = 200, description = "Meta accepted the code", body = RegistrationAnswer),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's `registration`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `code` (digits and hyphens) or `body` (never quoting it), or Meta's `invalid_parameter` (a wrong code)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget, or the number's verification attempts (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn verify_code(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiJson(body): ApiJson<CodeCheckBody>,
) -> Result<Json<RegistrationAnswer>, ApiError> {
    let code = domain::verification_code(body.code)?;
    registration(&state, &caller, &owned)?
        .verify_code(&code)
        .await?;
    Ok(Json(answer(&owned, RegistrationStatusName::CodeVerified)))
}

/// `POST /v1/numbers/{pn}/register`: register the (verified) number for
/// the Cloud API with its two-step verification PIN.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/register",
    tag = "registration",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key never registers twice (a repeat gets the kept answer, with `Idempotent-Replayed: true`). It covers the number and `data_localization_region`, never the PIN: a repeat with another PIN is the same request, and a wrong PIN Meta refused frees the key"),
    ),
    request_body = RegistrationBody,
    responses(
        (status = 200, description = "Registered", body = RegistrationAnswer),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "Meta's `two_step_verification` (a wrong PIN, or too many), `registration` (not verified, or Meta's 72-hour limit), `number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `pin` (six digits), `data_localization_region` or `body` (never quoting it), `idempotency_key_reused`, or Meta's `invalid_parameter`: nothing was asked", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget, or the number's registration attempts (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the number may have been registered", body = ErrorBody),
    )
)]
pub(crate) async fn register_number(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<RegistrationBody>,
) -> Response {
    let RegistrationBody {
        pin,
        data_localization_region,
    } = body;
    // The local checks, before the key is claimed; the PIN first becomes
    // the library's secret type.
    let pin = match domain::pin(pin) {
        Ok(pin) => pin,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let region = match data_localization_region.as_deref().map(domain::region) {
        None => None,
        Some(Ok(region)) => Some(region),
        Some(Err(error)) => return ApiError::from(error).into_response(),
    };
    // Never over the PIN (decision D6): the record holds nothing of it.
    let fingerprint = Fingerprint::register_number(
        owned.phone_number_id(),
        region.as_ref().map(DataLocalizationRegion::as_str),
    );
    let operation = async {
        registration(&state, &caller, &owned)?
            .register(&pin, region.as_ref())
            .await?;
        Success::json(
            StatusCode::OK,
            &answer(&owned, RegistrationStatusName::Registered),
        )
    };
    state.idempotent(&caller, key, fingerprint, operation).await
}

/// `POST /v1/numbers/{pn}/deregister`: deregister the number. It can no
/// longer send or receive through the Cloud API until registered again
/// (with its PIN); Meta keeps its history and its local storage setting,
/// and refuses a number also on the WhatsApp Business app. The number
/// stays bound to the tenant. Audited (`number_deregistered`).
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/deregister",
    tag = "registration",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key never deregisters twice (a repeat gets the kept answer, with `Idempotent-Replayed: true`)"),
    ),
    request_body = DeregistrationBody,
    responses(
        (status = 200, description = "Deregistered", body = RegistrationAnswer),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "Meta's `registration` (Meta's 72-hour limit), `number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `confirm` (not the path's number) or `body`, `idempotency_key_reused`, or Meta's `invalid_parameter`: nothing was asked", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget, or the number's registration attempts (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the number may have been deregistered", body = ErrorBody),
    )
)]
pub(crate) async fn deregister_number(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<DeregistrationBody>,
) -> Response {
    // The confirmation first, before the key is claimed (the core checks
    // it again).
    if let Err(error) = domain::confirmed(owned.phone_number_id(), &body.confirm) {
        return ApiError::from(error).into_response();
    }
    let fingerprint = Fingerprint::deregister_number(owned.phone_number_id());
    let operation = async {
        registration(&state, &caller, &owned)?
            .deregister(&body.confirm)
            .await?;
        Success::json(
            StatusCode::OK,
            &answer(&owned, RegistrationStatusName::Deregistered),
        )
    };
    state.idempotent(&caller, key, fingerprint, operation).await
}

/// `POST /v1/numbers/{pn}/two-step-pin`: set the number's two-step
/// verification PIN (there is no call to turn it off). Audited
/// (`two_step_pin_set`).
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/two-step-pin",
    tag = "registration",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = TwoStepPinBody,
    responses(
        (status = 200, description = "The PIN is set", body = RegistrationAnswer),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's `registration`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `pin` (six digits) or `body` (never quoting it), or Meta's `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn set_two_step_pin(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiJson(body): ApiJson<TwoStepPinBody>,
) -> Result<Json<RegistrationAnswer>, ApiError> {
    let pin = domain::pin(body.pin)?;
    registration(&state, &caller, &owned)?
        .set_two_step_pin(&pin)
        .await?;
    Ok(Json(answer(&owned, RegistrationStatusName::PinSet)))
}
