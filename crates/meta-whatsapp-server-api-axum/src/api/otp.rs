//! One-time passcodes (scope `otp`; docs/design/server.md, sections 4.2
//! and 4.7; roadmap M3c).
//!
//! | Route | Does |
//! | --- | --- |
//! | `POST /v1/otp/issue` | send a code to `phone` with the tenant's authentication template, from its sending number: `POST /{pn}/messages` (`templates/authentication-templates/copy-code-button-authentication-templates`, the send example), with the token of the number's WABA |
//! | `POST /v1/otp/verify` | check a typed code: asks Meta nothing |
//!
//! The rules are the core's ([`meta_whatsapp_server_core::otp`]): the
//! namespace is the calling tenant's, the sending number is the one the
//! tenant's OTP settings name (one of its own) or, with none, the default
//! OTP tenant's (decision D8), and the library's `OtpService` keeps the
//! codes, the attempts, the expiry, the cooldown and the issue limit. An
//! outcome is `200` with a status, as the library returns it as `Ok`: a
//! caller's generic error handling never swallows `invalid`. The code is in
//! no answer, log or error; a failed send's error keeps Meta's code and
//! trace id, never its text or `details`. An issue takes an
//! `Idempotency-Key`, under a fingerprint keyed with the OTP pepper (the
//! body names a phone number).

use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::otp::{Checked, IssueRequest, Issued, VerifyRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::{ApiJson, rfc3339};
use crate::auth::Caller;
use crate::error::{ApiError, ErrorBody, ErrorCode};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

// ─── Requests ────────────────────────────────────────────────────────────

/// `POST /v1/otp/issue`. No `Debug`: it holds a phone number.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = IssueOtp)]
pub struct IssueOtpBody {
    /// The recipient: E.164 with its `+` (`+16505551234`). A BSUID cannot
    /// receive an authentication template (Meta's `131062`), and a number
    /// without `+` would be read as local to the sending number's country.
    pub phone: String,
    /// The flow the code is for, 1 to 64 of `[A-Za-z0-9._:-]` (`login` when
    /// absent): a code sent for one purpose verifies for that purpose only.
    /// A constant of your code, never a value an end user chooses.
    pub purpose: Option<String>,
}

/// `POST /v1/otp/verify`. No `Debug`: it holds a phone number and a code.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = VerifyOtp)]
pub struct VerifyOtpBody {
    /// The recipient the code was sent to, as the issue named it.
    pub phone: String,
    /// The purpose the code was sent for (`login` when absent).
    pub purpose: Option<String>,
    /// The code as the user typed it, 1 to 15 characters, compared byte for
    /// byte: trim it first.
    pub code: String,
}

// ─── Answers ─────────────────────────────────────────────────────────────

/// What an issue did.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = OtpIssueStatus)]
pub enum OtpIssueStatusName {
    /// A code was sent: the recipient's previous code for the purpose no
    /// longer verifies.
    Sent,
    /// A code was sent to the recipient for the purpose less than the
    /// resend cooldown ago: nothing sent.
    CoolingDown,
    /// The recipient's issue limit for the purpose is used up: nothing
    /// sent.
    RateLimited,
}

/// An issue's outcome: `sent` with the challenge's id and expiry, or
/// `cooling_down` or `rate_limited` with the wait. Never the code.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = OtpIssued)]
pub struct OtpIssuedView {
    /// What happened.
    pub status: OtpIssueStatusName,
    /// `sent`: the challenge's random id (not a secret, not the code).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge_id: Option<String>,
    /// `sent`: when the code stops verifying (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(format = DateTime)]
    pub expires_at: Option<String>,
    /// `cooling_down`, `rate_limited`: seconds until a new code may be
    /// sent, rounded up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
}

impl From<Issued> for OtpIssuedView {
    fn from(issued: Issued) -> Self {
        match issued {
            Issued::Sent {
                challenge_id,
                expires_at,
            } => Self {
                status: OtpIssueStatusName::Sent,
                challenge_id: Some(challenge_id),
                expires_at: Some(rfc3339(expires_at)),
                retry_after_seconds: None,
            },
            Issued::CoolingDown {
                retry_after_seconds,
            } => Self {
                status: OtpIssueStatusName::CoolingDown,
                challenge_id: None,
                expires_at: None,
                retry_after_seconds: Some(retry_after_seconds),
            },
            Issued::RateLimited {
                retry_after_seconds,
            } => Self {
                status: OtpIssueStatusName::RateLimited,
                challenge_id: None,
                expires_at: None,
                retry_after_seconds: Some(retry_after_seconds),
            },
        }
    }
}

/// What a check found.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = OtpVerifyResult)]
pub enum OtpVerifyResultName {
    /// The code matched, and is used up.
    Verified,
    /// Wrong code; the attempt was counted (`attempts_left`).
    Invalid,
    /// The code expired: send a new one.
    Expired,
    /// The code's attempts are used up, even if this one was right: send a
    /// new one later.
    TooManyAttempts,
    /// No code is outstanding for the recipient and purpose for this
    /// tenant (never sent, used, long gone, or another tenant's).
    NotFound,
}

/// A check's outcome.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = OtpVerified)]
pub struct OtpVerifiedView {
    /// What the check found.
    pub result: OtpVerifyResultName,
    /// `invalid`: attempts left on this code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempts_left: Option<u32>,
}

impl From<Checked> for OtpVerifiedView {
    fn from(checked: Checked) -> Self {
        let (result, attempts_left) = match checked {
            Checked::Verified => (OtpVerifyResultName::Verified, None),
            Checked::Invalid { attempts_left } => {
                (OtpVerifyResultName::Invalid, Some(attempts_left))
            }
            Checked::Expired => (OtpVerifyResultName::Expired, None),
            Checked::TooManyAttempts => (OtpVerifyResultName::TooManyAttempts, None),
            Checked::NotFound => (OtpVerifyResultName::NotFound, None),
        };
        Self {
            result,
            attempts_left,
        }
    }
}

// ─── The routes ──────────────────────────────────────────────────────────

/// Send the code `request` asks for, for `caller`: `200` with the outcome
/// (the core's [`meta_whatsapp_server_core::otp::Otp::issue`]).
async fn issue(state: &Grant, caller: &Caller, request: IssueRequest) -> Result<Success, ApiError> {
    let issued = state
        .otp()
        .open(state.authz(), caller)?
        .observed_by(state.metrics())
        .issue(request)
        .await?;
    Success::json(StatusCode::OK, &OtpIssuedView::from(issued))
}

/// `POST /v1/otp/issue`: send a code with the tenant's authentication
/// template.
#[utoipa::path(
    post,
    path = "/v1/otp/issue",
    tag = "otp",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never send twice (a repeat gets the kept answer, with `Idempotent-Replayed: true`)"),
    ),
    request_body = IssueOtpBody,
    responses(
        (status = 200, description = "The outcome: `sent` (a code is on its way: `challenge_id`, `expires_at`), or `cooling_down` or `rate_limited` (nothing sent: `retry_after_seconds`; on the default OTP tenant's number, its issue limit per recipient is shared by every tenant and purpose sending from it). Never the code", body = OtpIssuedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 409, description = "`otp_not_configured` (no OTP settings for the tenant and no default OTP tenant that is active and has settings, or the sending number is no longer its holder's), the sending number's `number_not_connected` or `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`phone`: not E.164 with `+`, or a BSUID; `purpose`; `body`), `recipient_not_supported`, Meta's `template_*`, `idempotency_key_reused`: nothing was sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests` (the tenant's OTP issue budget, `Retry-After`) or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent` (when true, the code may be on its way and stays verifiable). Meta's text and `details` are never answered", body = ErrorBody),
        (status = 504, description = "`timeout`: the code may have been sent (`may_have_been_sent: true`)", body = ErrorBody),
    )
)]
pub(crate) async fn issue_otp(
    State(state): State<Grant>,
    caller: Caller,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    // The fingerprint is keyed with the pepper: no pepper, no OTP.
    let Some(deployment) = state.otp().deployment() else {
        return ApiError::new(ErrorCode::OTP_NOT_CONFIGURED).into_response();
    };
    let fingerprint = Fingerprint::issue_otp(deployment.keys(), &body);
    let request = match serde_json::from_value::<IssueOtpBody>(body) {
        Ok(body) => IssueRequest {
            phone: body.phone,
            purpose: body.purpose,
        },
        Err(_) => return ApiError::invalid("body").into_response(),
    };
    // Boxed: the library's OTP service makes the future large (clippy's
    // `large_futures`).
    state
        .idempotent(
            &caller,
            key,
            fingerprint,
            Box::pin(issue(&state, &caller, request)),
        )
        .await
}

/// `POST /v1/otp/verify`: check a typed code.
#[utoipa::path(
    post,
    path = "/v1/otp/verify",
    tag = "otp",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = VerifyOtpBody,
    responses(
        (status = 200, description = "What the check found: `verified`, `invalid` (with `attempts_left`), `expired`, `too_many_attempts` or `not_found`. Every check of an outstanding code counts as an attempt", body = OtpVerifiedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 409, description = "`otp_not_configured` (as on an issue)", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`phone`, `purpose`, `code`, `body`)", body = ErrorBody),
        (status = 429, description = "`too_many_requests` (the tenant's OTP check budget, `Retry-After`)", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`: retry", body = ErrorBody),
    )
)]
pub(crate) async fn verify_otp(
    State(state): State<Grant>,
    caller: Caller,
    ApiJson(body): ApiJson<VerifyOtpBody>,
) -> Result<meta_whatsapp_rs::webhooks::axum::Json<OtpVerifiedView>, ApiError> {
    let checked = state
        .otp()
        .open(state.authz(), &caller)?
        .observed_by(state.metrics())
        .verify(VerifyRequest {
            phone: body.phone,
            purpose: body.purpose,
            code: body.code,
        })
        .await?;
    Ok(meta_whatsapp_rs::webhooks::axum::Json(checked.into()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::macros::datetime;

    use super::*;

    /// Each outcome's answer, as the design writes it: never a field of
    /// another outcome.
    #[test]
    fn outcomes_answer_the_designs_fields() {
        let sent = OtpIssuedView::from(Issued::Sent {
            challenge_id: "c0ffee".to_owned(),
            expires_at: datetime!(2026-11-01 10:10:00 UTC),
        });
        assert_eq!(
            serde_json::to_value(&sent).unwrap(),
            json!({"status": "sent", "challenge_id": "c0ffee", "expires_at": "2026-11-01T10:10:00Z"})
        );
        let waiting = OtpIssuedView::from(Issued::CoolingDown {
            retry_after_seconds: 30,
        });
        assert_eq!(
            serde_json::to_value(&waiting).unwrap(),
            json!({"status": "cooling_down", "retry_after_seconds": 30})
        );
        let limited = OtpIssuedView::from(Issued::RateLimited {
            retry_after_seconds: 3600,
        });
        assert_eq!(
            serde_json::to_value(&limited).unwrap(),
            json!({"status": "rate_limited", "retry_after_seconds": 3600})
        );
        let checks = [
            (Checked::Verified, json!({"result": "verified"})),
            (
                Checked::Invalid { attempts_left: 4 },
                json!({"result": "invalid", "attempts_left": 4}),
            ),
            (Checked::Expired, json!({"result": "expired"})),
            (
                Checked::TooManyAttempts,
                json!({"result": "too_many_attempts"}),
            ),
            (Checked::NotFound, json!({"result": "not_found"})),
        ];
        for (checked, answer) in checks {
            assert_eq!(
                serde_json::to_value(OtpVerifiedView::from(checked)).unwrap(),
                answer
            );
            assert_eq!(answer["result"], checked.as_str());
        }
    }
}
