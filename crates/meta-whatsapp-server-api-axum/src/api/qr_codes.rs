//! QR codes and short links (scope `qr_codes`; docs/design/server.md,
//! section 4.2; roadmap M5g): codes that open a chat with a prefilled
//! message.
//!
//! | Route | Graph calls, with the tenant's WABA token |
//! | --- | --- |
//! | `POST /v1/numbers/{pn}/qr-codes` | `POST /{pn}/message_qrdls` |
//! | `GET /v1/numbers/{pn}/qr-codes` | `GET /{pn}/message_qrdls` |
//! | `GET /v1/numbers/{pn}/qr-codes/{code}` | `GET /{pn}/message_qrdls?code={code}` (the number's own list: the code must be in it) |
//! | `PATCH /v1/numbers/{pn}/qr-codes/{code}` | the same lookup, then `POST /{pn}/message_qrdls` with `code` |
//! | `DELETE /v1/numbers/{pn}/qr-codes/{code}` | the same lookup, then `DELETE /{pn}/message_qrdls/{code}` |
//!
//! Pages: `qr-codes`,
//! `reference/whatsapp-business-phone-number/whatsapp-business-qr-code-management-api`
//! and `…/whatsapp-business-qr-code-api`. **A code is its number's, or it
//! does not exist**: another number's code is `404 not_found` and nothing
//! is read back, changed or deleted (the core's
//! [`meta_whatsapp_server_core::qr_codes`]). A creation takes an
//! `Idempotency-Key`; a deletion is audited.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::qr_codes::{self as domain, CodeQuery, QrCodes};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::{ApiJson, encode_cursor};
use super::listing;
use super::templates::ApiQuery;
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

/// A QR code and short link.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = QrCode)]
pub struct QrCodeView {
    /// Its 14-character id.
    pub code: String,
    /// The text prefilled in the user's composer.
    pub prefilled_message: String,
    /// The `https://wa.me/message/<code>` short link.
    pub deep_link_url: String,
    /// The image's download URL, when an image format was asked for.
    #[schema(required = true)]
    pub qr_image_url: Option<String>,
}

impl From<domain::Code> for QrCodeView {
    fn from(code: domain::Code) -> Self {
        let domain::Code {
            code,
            prefilled_message,
            deep_link_url,
            qr_image_url,
        } = code;
        Self {
            code,
            prefilled_message,
            deep_link_url,
            qr_image_url,
        }
    }
}

/// A page of a number's QR codes.
#[derive(Debug, Serialize, ToSchema)]
pub struct QrCodeList {
    /// The codes, newest first.
    pub data: Vec<QrCodeView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// A new QR code.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CreateQrCode)]
pub struct CreateQrCodeBody {
    /// The text prefilled in the user's composer, 1 to 140 characters.
    pub prefilled_message: String,
    /// Also render an image: `PNG`, or `SVG` (Meta recommends it for
    /// print). The answer's `qr_image_url` is its link.
    pub generate_qr_image: Option<String>,
}

/// What to change in a QR code.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = UpdateQrCode)]
pub struct UpdateQrCodeBody {
    /// The new prefilled message, 1 to 140 characters.
    pub prefilled_message: String,
}

/// `?limit=`, `?cursor=` and `?image_format=`, as the OpenAPI document
/// declares them.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct QrCodesQuery {
    /// Page size, 1 to 25 (Meta's maximum; Meta's default without).
    #[param(minimum = 1, maximum = 25)]
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
    /// Ask for each code's image, `PNG` or `SVG`.
    pub image_format: Option<String>,
}

/// `?image_format=`, as the OpenAPI document declares it.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct QrCodeQuery {
    /// Ask for the code's image, `PNG` or `SVG`.
    pub image_format: Option<String>,
}

fn qr_codes<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedNumber,
) -> Result<QrCodes<'a>, ApiError> {
    Ok(QrCodes::new(state.authz(), caller, owned.capability(state))?.observed_by(state.metrics()))
}

/// Create the code: `201` with Meta's answer, or its refusal.
async fn create(
    qr_codes: &QrCodes<'_>,
    new: &meta_whatsapp_rs::client::qr_codes::CreateQrCode,
) -> Result<Success, ApiError> {
    let code = qr_codes.create(new).await?;
    Success::json(StatusCode::CREATED, &QrCodeView::from(code))
}

/// `POST /v1/numbers/{pn}/qr-codes`: create a QR code and short link.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/qr-codes",
    tag = "qr_codes",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never create twice"),
    ),
    request_body = CreateQrCodeBody,
    responses(
        (status = 201, description = "Created", body = QrCodeView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `prefilled_message`, `generate_qr_image` or `body`; `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the code may have been created", body = ErrorBody),
    )
)]
pub(crate) async fn create_qr_code(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::create_qr_code(owned.phone_number_id(), &body);
    let new = serde_json::from_value::<CreateQrCodeBody>(body)
        .map_err(|_| ApiError::invalid("body"))
        .and_then(|body| {
            Ok(domain::new_qr_code(
                body.prefilled_message,
                body.generate_qr_image.as_deref(),
            )?)
        });
    let new = match new {
        Ok(new) => new,
        Err(error) => return error.into_response(),
    };
    let service = match qr_codes(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, create(&service, &new))
        .await
}

/// `GET /v1/numbers/{pn}/qr-codes`: a page of the number's codes, newest
/// first.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/qr-codes",
    tag = "qr_codes",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        QrCodesQuery,
    ),
    responses(
        (status = 200, description = "A page of codes", body = QrCodeList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit`, `cursor`, `image_format` or `query`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_qr_codes(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiQuery(query): ApiQuery<QrCodesQuery>,
) -> Result<Json<QrCodeList>, ApiError> {
    let after = listing::after(query.cursor.as_deref())?;
    let page = qr_codes(&state, &caller, &owned)?
        .list(CodeQuery {
            limit: query.limit,
            after,
            image_format: query.image_format,
        })
        .await?;
    Ok(Json(QrCodeList {
        data: page.codes.into_iter().map(QrCodeView::from).collect(),
        next_cursor: page.next_after.as_deref().map(encode_cursor),
    }))
}

/// `GET /v1/numbers/{pn}/qr-codes/{code}`: one of the number's codes.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/qr-codes/{code}",
    tag = "qr_codes",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("code" = String, Path, description = "The code's 14-character id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        QrCodeQuery,
    ),
    responses(
        (status = 200, description = "The code", body = QrCodeView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such code on it (another number's code looks like a missing one)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `code`, `image_format` or `query`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_qr_code(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    Path((_, code)): Path<(String, String)>,
    ApiQuery(query): ApiQuery<QrCodeQuery>,
) -> Result<Json<QrCodeView>, ApiError> {
    let code = qr_codes(&state, &caller, &owned)?
        .get(&code, query.image_format.as_deref())
        .await?;
    Ok(Json(QrCodeView::from(code)))
}

/// `PATCH /v1/numbers/{pn}/qr-codes/{code}`: change a code's prefilled
/// message.
#[utoipa::path(
    patch,
    path = "/v1/numbers/{pn}/qr-codes/{code}",
    tag = "qr_codes",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("code" = String, Path, description = "The code's 14-character id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = UpdateQrCodeBody,
    responses(
        (status = 200, description = "The code, changed", body = QrCodeView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such code on it; nothing changed", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `code`, `prefilled_message` or `body`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the code may have been changed", body = ErrorBody),
    )
)]
pub(crate) async fn update_qr_code(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    Path((_, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<UpdateQrCodeBody>,
) -> Result<Json<QrCodeView>, ApiError> {
    let code = qr_codes(&state, &caller, &owned)?
        .update(&code, body.prefilled_message)
        .await?;
    Ok(Json(QrCodeView::from(code)))
}

/// `DELETE /v1/numbers/{pn}/qr-codes/{code}`: delete a code (people who
/// scan it afterwards are told it expired).
#[utoipa::path(
    delete,
    path = "/v1/numbers/{pn}/qr-codes/{code}",
    tag = "qr_codes",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("code" = String, Path, description = "The code's 14-character id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such code on it; nothing deleted", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `code`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the code may have been deleted", body = ErrorBody),
    )
)]
pub(crate) async fn delete_qr_code(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    Path((_, code)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    qr_codes(&state, &caller, &owned)?.delete(&code).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Every field of the core's code reaches the answer. Decisive: each
    /// field of the conversion.
    #[test]
    fn every_field_of_the_cores_code_is_answered() {
        let code = domain::Code {
            code: "4O4YGZEG3RIVE1".to_owned(),
            prefilled_message: "Cyber Monday 1".to_owned(),
            deep_link_url: "https://wa.me/message/4O4YGZEG3RIVE1".to_owned(),
            qr_image_url: None,
        };
        assert_eq!(
            serde_json::to_value(QrCodeView::from(code)).unwrap(),
            json!({"code": "4O4YGZEG3RIVE1", "prefilled_message": "Cyber Monday 1",
                   "deep_link_url": "https://wa.me/message/4O4YGZEG3RIVE1",
                   "qr_image_url": null})
        );
    }
}
