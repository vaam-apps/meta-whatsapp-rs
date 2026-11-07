//! Pre-verified numbers (admin key only; docs/design/server.md, section
//! 4.6; roadmap M3f, decisions D203 to D205): the deployment's own pool of
//! numbers verified ahead of time, which a merchant picks in Embedded
//! Signup without typing a one-time code.
//!
//! | Route | Graph call, with the pool's token |
//! | --- | --- |
//! | `POST /v1/admin/pre-verified-numbers` | `POST /{business}/add_phone_numbers` |
//! | `GET /v1/admin/pre-verified-numbers` | `GET /{business}/preverified_numbers` |
//! | `GET /v1/admin/pre-verified-numbers/{id}` | `GET /{id}` |
//! | `POST /v1/admin/pre-verified-numbers/{id}/code` | `POST /{id}/request_code` |
//! | `POST /v1/admin/pre-verified-numbers/{id}/verify` | `POST /{id}/verify_code` |
//! | `DELETE /v1/admin/pre-verified-numbers/{id}` | `DELETE /{id}` (and its reservation) |
//! | `PUT`, `DELETE /v1/admin/pre-verified-numbers/{id}/reservation` | none: reserve the number for a tenant's launch, or release it |
//!
//! The pool is the deployment's, so these are the operator's. A tenant
//! reaches a number only through `POST /v1/signup/sessions`
//! (`pre_verified_phone_ids`), which offers the ids reserved for it. The
//! verification code is in no answer, log or audit event.

use meta_whatsapp_rs::client::phone_numbers::{CodeMethod, CodeVerificationStatus};
use meta_whatsapp_rs::client::waba::PreVerifiedNumber;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::signup::{PreVerifiedAdd, PreVerifiedCode, PreVerifiedList};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::ApiJson;
use super::templates::ApiQuery;
use crate::auth::AdminCaller;
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::model::TenantId;
use crate::state::Grant;

/// The longest cursor accepted.
const MAX_CURSOR_CHARS: usize = 512;

/// `POST /v1/admin/pre-verified-numbers`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = AddPreVerifiedNumber)]
pub struct AddPreVerifiedBody {
    /// The number to verify: E.164, or formatted with spaces, hyphens and
    /// parentheses (Meta normalizes it).
    pub phone_number: String,
}

/// How the one-time code is delivered.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[schema(as = PreVerifiedCodeMethod)]
pub enum CodeMethodName {
    /// A text message.
    Sms,
    /// A voice call.
    Voice,
}

/// `POST /v1/admin/pre-verified-numbers/{id}/code`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = RequestPreVerifiedCode)]
pub struct RequestCodeBody {
    /// `SMS` or `VOICE`.
    pub method: CodeMethodName,
    /// The language of the message, e.g. `en_US`.
    pub language: String,
}

/// `POST /v1/admin/pre-verified-numbers/{id}/verify`. No `Debug`: it
/// holds the code.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = VerifyPreVerifiedCode)]
pub struct VerifyCodeBody {
    /// The code the number received (digits; hyphens are dropped).
    pub code: String,
}

/// `PUT /v1/admin/pre-verified-numbers/{id}/reservation`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ReservePreVerifiedNumber)]
pub struct ReserveBody {
    /// The tenant whose signup may offer the number.
    pub tenant_id: String,
}

/// A number of the pool.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = PreVerifiedNumber)]
pub struct PreVerifiedNumberView {
    /// Its id: what `pre_verified_phone_ids` takes.
    pub id: String,
    /// The number, E.164.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
    /// `VERIFIED`, `NOT_VERIFIED` or `EXPIRED`, as Meta names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_verification_status: Option<String>,
    /// When it was last verified, Unix seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_verification_time: Option<i64>,
    /// When the verification expires, Unix seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_expiry_time: Option<i64>,
}

impl From<PreVerifiedNumber> for PreVerifiedNumberView {
    fn from(number: PreVerifiedNumber) -> Self {
        Self {
            id: number.id.as_str().to_owned(),
            phone_number: number.phone_number,
            code_verification_status: number
                .code_verification_status
                .map(|status| status.as_str().to_owned()),
            code_verification_time: number.code_verification_time,
            verification_expiry_time: number.verification_expiry_time,
        }
    }
}

/// A number just added to the pool.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CreatedPreVerifiedNumber)]
pub struct CreatedView {
    /// Its id: request its code next.
    pub id: String,
}

/// A page of the pool.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = PreVerifiedNumberPage)]
pub struct PreVerifiedPageView {
    /// The numbers, in order of creation.
    pub data: Vec<PreVerifiedNumberView>,
    /// The next page's cursor, when there is one.
    pub next_cursor: Option<String>,
}

/// `?phone_number=`, `?status=`, `?limit=` and `?cursor=`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct PreVerifiedQuery {
    /// Only the number written this way.
    pub phone_number: Option<String>,
    /// Only the numbers in this state: `VERIFIED`, `NOT_VERIFIED` or
    /// `EXPIRED`.
    pub status: Option<String>,
    /// Page size, at least 1.
    #[param(minimum = 1)]
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// The addition as the operation a key keeps: the new number's id.
async fn add_number(
    state: &Grant,
    admin: &AdminCaller,
    body: AddPreVerifiedBody,
) -> Result<Success, ApiError> {
    let created = state
        .signups()
        .pre_verified_add(
            state.authz(),
            admin,
            PreVerifiedAdd {
                phone_number: body.phone_number,
            },
            state.metrics(),
        )
        .await?;
    Success::json(
        StatusCode::CREATED,
        &CreatedView {
            id: created.id.as_str().to_owned(),
        },
    )
}

/// `POST /v1/admin/pre-verified-numbers`: add a number to the pool. It
/// takes an `Idempotency-Key`, scoped to the admin key (roadmap M5j3), so
/// that a retry after a `504` does not add the number twice.
#[utoipa::path(
    post,
    path = "/v1/admin/pre-verified-numbers",
    tag = "pre_verified",
    security(("api_key" = [])),
    params(
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the admin key: the same key and body never add twice"),
    ),
    request_body = AddPreVerifiedBody,
    responses(
        (status = 201, description = "The number is in the pool (not yet verified): request its code", body = CreatedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`: no pool (`WA_PARTNER_BUSINESS_ID`); `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`phone_number`, `body`, `Idempotency-Key`); `idempotency_key_reused`", body = ErrorBody),
        (status = 502, description = "`graph_error` and its kin: Meta refused", body = ErrorBody),
        (status = 504, description = "`timeout`: list the pool before adding again, or repeat the request with the same `Idempotency-Key` for this answer", body = ErrorBody),
    )
)]
pub(crate) async fn add_pre_verified_number(
    State(state): State<Grant>,
    admin: AdminCaller,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::add_pre_verified_number(&body);
    let Ok(body) = serde_json::from_value::<AddPreVerifiedBody>(body) else {
        return ApiError::invalid("body").into_response();
    };
    state
        .idempotent_admin(&admin, key, fingerprint, add_number(&state, &admin, body))
        .await
}

/// `GET /v1/admin/pre-verified-numbers`: one page of the pool.
#[utoipa::path(
    get,
    path = "/v1/admin/pre-verified-numbers",
    tag = "pre_verified",
    security(("api_key" = [])),
    params(PreVerifiedQuery),
    responses(
        (status = 200, description = "One page of the pool", body = PreVerifiedPageView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`query`, `status`, `limit`, `cursor`)", body = ErrorBody),
        (status = 502, description = "Meta refused", body = ErrorBody),
    )
)]
pub(crate) async fn list_pre_verified_numbers(
    State(state): State<Grant>,
    admin: AdminCaller,
    ApiQuery(query): ApiQuery<PreVerifiedQuery>,
) -> Result<Json<PreVerifiedPageView>, ApiError> {
    if query.limit == Some(0) {
        return Err(ApiError::invalid("limit"));
    }
    if query
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.is_empty() || cursor.len() > MAX_CURSOR_CHARS)
    {
        return Err(ApiError::invalid("cursor"));
    }
    // Meta's reference lists three states; `UNVERIFIED` and others it does
    // not list are refused here, with the field.
    let status = query
        .status
        .map(|status| match status.as_str() {
            "VERIFIED" => Ok(CodeVerificationStatus::Verified),
            "NOT_VERIFIED" => Ok(CodeVerificationStatus::NotVerified),
            "EXPIRED" => Ok(CodeVerificationStatus::Expired),
            _ => Err(ApiError::invalid("status")),
        })
        .transpose()?;
    let page = state
        .signups()
        .pre_verified_list(
            state.authz(),
            &admin,
            PreVerifiedList {
                phone_number: query.phone_number,
                status,
                limit: query.limit,
                after: query.cursor,
            },
            state.metrics(),
        )
        .await?;
    let next_cursor = page.next_cursor().map(str::to_owned);
    Ok(Json(PreVerifiedPageView {
        data: page.data.into_iter().map(Into::into).collect(),
        next_cursor,
    }))
}

/// `GET /v1/admin/pre-verified-numbers/{id}`: one number.
#[utoipa::path(
    get,
    path = "/v1/admin/pre-verified-numbers/{id}",
    tag = "pre_verified",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "The pre-verified number's id (digits)")),
    responses(
        (status = 200, description = "The number, its state and when its verification expires", body = PreVerifiedNumberView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`id`)", body = ErrorBody),
        (status = 502, description = "Meta refused", body = ErrorBody),
    )
)]
pub(crate) async fn get_pre_verified_number(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<Json<PreVerifiedNumberView>, ApiError> {
    let number = state
        .signups()
        .pre_verified_get(state.authz(), &admin, &id, state.metrics())
        .await?;
    Ok(Json(number.into()))
}

/// The code request as the operation a key keeps: no body.
async fn request_code(
    state: &Grant,
    admin: &AdminCaller,
    id: &str,
    body: RequestCodeBody,
) -> Result<Success, ApiError> {
    let method = match body.method {
        CodeMethodName::Sms => CodeMethod::Sms,
        CodeMethodName::Voice => CodeMethod::Voice,
    };
    state
        .signups()
        .pre_verified_request_code(
            state.authz(),
            admin,
            id,
            PreVerifiedCode {
                method,
                language: body.language,
            },
            state.metrics(),
        )
        .await?;
    Ok(Success::no_content())
}

/// `POST /v1/admin/pre-verified-numbers/{id}/code`: have Meta send the
/// one-time code (not replayed after a timeout: each request sends a
/// message). It takes an `Idempotency-Key`, scoped to the admin key
/// (roadmap M5j3), so that a client's retry after a `504` is not a second
/// message to the number.
#[utoipa::path(
    post,
    path = "/v1/admin/pre-verified-numbers/{id}/code",
    tag = "pre_verified",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The pre-verified number's id (digits)"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the admin key: the same key, number and body never send a second code"),
    ),
    request_body = RequestCodeBody,
    responses(
        (status = 204, description = "The code was requested"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`; `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`id`, `language`, `body`, `Idempotency-Key`); `idempotency_key_reused`", body = ErrorBody),
        (status = 502, description = "Meta refused", body = ErrorBody),
        (status = 504, description = "`timeout`: the code may have been sent; repeat the request with the same `Idempotency-Key` for this answer", body = ErrorBody),
    )
)]
pub(crate) async fn request_pre_verified_code(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::request_pre_verified_code(&id, &body);
    let Ok(body) = serde_json::from_value::<RequestCodeBody>(body) else {
        return ApiError::invalid("body").into_response();
    };
    state
        .idempotent_admin(
            &admin,
            key,
            fingerprint,
            request_code(&state, &admin, &id, body),
        )
        .await
}

/// `POST /v1/admin/pre-verified-numbers/{id}/verify`: check the code.
#[utoipa::path(
    post,
    path = "/v1/admin/pre-verified-numbers/{id}/verify",
    tag = "pre_verified",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "The pre-verified number's id (digits)")),
    request_body = VerifyCodeBody,
    responses(
        (status = 204, description = "The code matched: the number is verified, to be claimed within 90 days"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`id`, `code`, `body`; the code is never quoted)", body = ErrorBody),
        (status = 502, description = "Meta refused (a wrong code included)", body = ErrorBody),
    )
)]
pub(crate) async fn verify_pre_verified_code(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<VerifyCodeBody>,
) -> Result<StatusCode, ApiError> {
    state
        .signups()
        .pre_verified_verify_code(state.authz(), &admin, &id, body.code, state.metrics())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/admin/pre-verified-numbers/{id}`: remove the number from
/// the pool, and its reservation. An explicit operator action, audited.
#[utoipa::path(
    delete,
    path = "/v1/admin/pre-verified-numbers/{id}",
    tag = "pre_verified",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "The pre-verified number's id (digits)")),
    responses(
        (status = 204, description = "Deleted; audited `pre_verified_number_deleted`"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`id`)", body = ErrorBody),
        (status = 502, description = "Meta refused", body = ErrorBody),
    )
)]
pub(crate) async fn delete_pre_verified_number(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .signups()
        .pre_verified_delete(state.authz(), &admin, &id, state.metrics())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /v1/admin/pre-verified-numbers/{id}/reservation`: reserve the
/// number for a tenant: only that tenant's signup may offer it.
#[utoipa::path(
    put,
    path = "/v1/admin/pre-verified-numbers/{id}/reservation",
    tag = "pre_verified",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "The pre-verified number's id (digits)")),
    request_body = ReserveBody,
    responses(
        (status = 204, description = "Reserved (replacing another tenant's); audited `pre_verified_number_reserved`. Asks Meta nothing"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 404, description = "`not_found`: no such tenant", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`id`, `tenant_id`, `body`)", body = ErrorBody),
    )
)]
pub(crate) async fn reserve_pre_verified_number(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ReserveBody>,
) -> Result<StatusCode, ApiError> {
    let tenant = TenantId::parse(&body.tenant_id).ok_or_else(|| ApiError::invalid("tenant_id"))?;
    state
        .signups()
        .pre_verified_reserve(state.authz(), &admin, &id, &tenant)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/admin/pre-verified-numbers/{id}/reservation`: release it.
#[utoipa::path(
    delete,
    path = "/v1/admin/pre-verified-numbers/{id}/reservation",
    tag = "pre_verified",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "The pre-verified number's id (digits)")),
    responses(
        (status = 204, description = "Released (also when there was none); audited `pre_verified_number_released`"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "Not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`id`)", body = ErrorBody),
    )
)]
pub(crate) async fn release_pre_verified_number(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .signups()
        .pre_verified_release(state.authz(), &admin, &id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
