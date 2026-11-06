//! A number's display name (scope `numbers`; docs/design/server.md,
//! section 4.2; roadmap M5c2): the request for a new one, which Meta
//! reviews, and the status of the name and of the request.
//!
//! | Route | Graph call, with the tenant's WABA token | `Idempotency-Key` |
//! | --- | --- | --- |
//! | `POST /v1/numbers/{pn}/display-name` | `POST /{pn}?new_display_name=…` | yes |
//! | `GET /v1/numbers/{pn}/display-name` | `GET /{pn}?fields=verified_name,name_status,new_display_name,new_name_status` | no |
//!
//! Meta's page (`display-names`) says what a request does and what is left
//! to do: the name changes only once Meta approves it (the event
//! `phone_number_name_updated`) **and** the number is registered again
//! (`POST /v1/numbers/{pn}/register`, with its PIN). The service's core
//! ([`meta_whatsapp_server_core::display_names`]) says how it is bounded
//! and audited; this module keeps the routes, the request and answer
//! types with their OpenAPI schema.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::display_names::{DisplayNameStatus, DisplayNames};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::ApiJson;
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

/// A new display name to ask Meta to review.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DisplayNameRequest {
    /// The name, trimmed: 1 to 256 characters, no control, bidi override or zero-width character (the
    /// service's own bound: Meta's pages state none).
    pub new_display_name: String,
}

/// What was asked: Meta reviews the name, and answers by the
/// `phone_number_name_updated` event.
#[derive(Debug, Serialize, ToSchema)]
pub struct DisplayNameRequested {
    /// The name sent to Meta, as trimmed.
    pub new_display_name: String,
}

/// A number's display name and the review of the one requested, as Meta
/// names each value (`null` when Meta answers none).
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = DisplayName)]
pub struct DisplayNameView {
    /// The display name WhatsApp holds. It does not say the name is
    /// approved: `name_status` does.
    #[schema(required = true)]
    pub verified_name: Option<String>,
    /// `APPROVED`, `AVAILABLE_WITHOUT_REVIEW`, `DECLINED`, `EXPIRED`,
    /// `PENDING_REVIEW` or `NONE`.
    #[schema(required = true)]
    pub name_status: Option<String>,
    /// The display name last requested.
    #[schema(required = true)]
    pub new_display_name: Option<String>,
    /// The review of `new_display_name`, with the same values.
    #[schema(required = true)]
    pub new_name_status: Option<String>,
}

impl From<DisplayNameStatus> for DisplayNameView {
    fn from(status: DisplayNameStatus) -> Self {
        let DisplayNameStatus {
            verified_name,
            name_status,
            new_display_name,
            new_name_status,
        } = status;
        Self {
            verified_name,
            name_status,
            new_display_name,
            new_name_status,
        }
    }
}

fn display_names<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedNumber,
) -> Result<DisplayNames<'a>, ApiError> {
    Ok(
        DisplayNames::new(state.authz(), caller, owned.capability(state))?
            .observed_by(state.metrics()),
    )
}

/// `POST /v1/numbers/{pn}/display-name`: ask Meta to review a new display
/// name. The name changes only once approved and the number registered
/// again.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/display-name",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never ask for the review twice (a repeat gets the kept answer, with `Idempotent-Replayed: true`)"),
    ),
    request_body = DisplayNameRequest,
    responses(
        (status = 202, description = "Meta accepted the request and reviews the name (`GET …/display-name` for `new_name_status`; the event `phone_number_name_updated` for the decision). Once approved, register the number again (`POST /v1/numbers/{pn}/register`) within the 14 days Meta allows, or the name is reviewed again. Meta allows 10 changes per 30 days", body = DisplayNameRequested),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `new_display_name` (1 to 256 characters, no control, bidi override or zero-width character) or `body`, `idempotency_key_reused`, or Meta's `invalid_parameter`: nothing was asked", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the request may have reached Meta (`GET …/display-name` says)", body = ErrorBody),
    )
)]
pub(crate) async fn request_display_name_change(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::request_display_name_change(owned.phone_number_id(), &body);
    let Ok(DisplayNameRequest { new_display_name }) =
        serde_json::from_value::<DisplayNameRequest>(body)
    else {
        return ApiError::invalid("body").into_response();
    };
    let service = match display_names(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    let operation = async {
        let asked = service.request(&new_display_name).await?;
        Success::json(
            StatusCode::ACCEPTED,
            &DisplayNameRequested {
                new_display_name: asked,
            },
        )
    };
    state.idempotent(&caller, key, fingerprint, operation).await
}

/// `GET /v1/numbers/{pn}/display-name`: the number's display name and the
/// review of a requested one.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/display-name",
    tag = "numbers",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The display name and the review of the requested one", body = DisplayNameView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_display_name(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<DisplayNameView>, ApiError> {
    let status = display_names(&state, &caller, &owned)?.status().await?;
    Ok(Json(DisplayNameView::from(status)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The body takes the name and nothing else; the answer is the status's
    /// four values, each present (`null` when Meta answers none).
    #[test]
    fn the_request_and_the_answer_have_the_documented_shape() {
        let ok: DisplayNameRequest =
            serde_json::from_value(json!({"new_display_name": "Lucky Shrub"})).unwrap();
        assert_eq!(ok.new_display_name, "Lucky Shrub");
        assert!(
            serde_json::from_value::<DisplayNameRequest>(
                json!({"new_display_name": "x", "pin": "123456"})
            )
            .is_err()
        );
        // display-names, "Update display name via API", the status example.
        let status = DisplayNameStatus {
            new_display_name: Some("New Lucky Shrub".to_owned()),
            new_name_status: Some("PENDING_REVIEW".to_owned()),
            ..DisplayNameStatus::default()
        };
        assert_eq!(
            serde_json::to_value(DisplayNameView::from(status)).unwrap(),
            json!({"verified_name": null, "name_status": null,
                   "new_display_name": "New Lucky Shrub",
                   "new_name_status": "PENDING_REVIEW"})
        );
    }
}
