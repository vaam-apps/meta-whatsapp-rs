//! The error model over HTTP (docs/design/server.md, section 5): the
//! service's errors are the core's [`ServiceError`]s (codes, statuses,
//! `retryable`, `may_have_been_sent`, Meta's `details`: see
//! [`meta_whatsapp_server_core::error`]); this module answers them.
//!
//! Every error answers the same body:
//!
//! ```json
//! {"error": {"code": "customer_service_window_closed",
//!   "message": "…", "retryable": false, "may_have_been_sent": false,
//!   "field": null, "step": null, "resumable": null,
//!   "graph": {"code": 131047, "subcode": null, "fbtrace_id": "…", "details": "…"},
//!   "request_id": "req_…"}}
//! ```
//!
//! - `code` is stable: [`CODES`] lists every one with its HTTP status and
//!   the service's sentence for it. Codes only grow within `v1`.
//! - A `401` carries `WWW-Authenticate: Bearer`; an error whose wait the
//!   service knows carries `Retry-After`.
//! - `request_id` is the request's `X-Request-Id`.

use std::fmt;

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderValue, StatusCode, header};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::error::{self as core_error, ServiceError};
pub use meta_whatsapp_server_core::error::{CODES, MAX_DETAILS_CHARS, kind_code};
use serde::Serialize;
use utoipa::ToSchema;

use crate::telemetry;

/// The HTTP status and sentence of `code`; `None` for a code not in
/// [`CODES`].
pub fn code_info(code: &str) -> Option<(StatusCode, &'static str)> {
    core_error::code_info(code)
        .and_then(|(status, message)| StatusCode::from_u16(status).ok().map(|s| (s, message)))
}

/// Meta's error, as the body carries it: the code and trace id, and, on
/// the routes that opt in, `error_data.details`. Never Meta's `message` or
/// titles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct GraphErrorInfo {
    /// Meta's error code, e.g. `131047`.
    pub code: i64,
    /// Meta's subcode, when it sent one (deprecated by Meta since v16).
    #[schema(required = true)]
    pub subcode: Option<i64>,
    /// Trace id for Meta's support.
    #[schema(required = true)]
    pub fbtrace_id: Option<String>,
    /// Meta's own text (`error_data.details`), on the routes that keep it:
    /// not the service's, and not a stable value to branch on.
    #[schema(required = true)]
    pub details: Option<String>,
}

/// An API error: a [`ServiceError`], answered over HTTP. Build one from a
/// code ([`ApiError::new`]) or from a library error
/// ([`ApiError::from_library`]); the core's services return
/// [`ServiceError`]s, which `?` turns into one.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiError(ServiceError);

impl fmt::Debug for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiError")
            .field("status", &self.0.status())
            .field("code", &self.0.code())
            .field("field", &self.0.field())
            .finish_non_exhaustive()
    }
}

/// The body of every error answer.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// The error.
    pub error: ErrorObject,
}

/// An error: its stable code, the service's sentence for it, whether to
/// retry, whether the request may have taken effect, and Meta's error when
/// Meta answered with one.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorObject {
    /// Stable error code (one of the `ErrorCode` values).
    #[schema(value_type = ErrorCode)]
    pub code: &'static str,
    /// The service's sentence for the code.
    pub message: &'static str,
    /// Whether repeating the same request later may succeed.
    pub retryable: bool,
    /// Whether the request may have taken effect (a message may have been
    /// sent): if `true`, reconcile before repeating it.
    pub may_have_been_sent: bool,
    /// The request field at fault, for `invalid_request`.
    #[schema(required = true)]
    pub field: Option<String>,
    /// The step a multi-step operation stopped at.
    #[schema(required = true)]
    pub step: Option<&'static str>,
    /// Whether the stopped operation can be resumed.
    #[schema(required = true)]
    pub resumable: Option<bool>,
    /// Meta's error, when Meta answered with one.
    #[schema(required = true)]
    pub graph: Option<GraphErrorInfo>,
    /// The request's id (`X-Request-Id`).
    #[schema(required = true)]
    pub request_id: Option<String>,
}

impl ApiError {
    /// An error with `code` from [`CODES`]: not retryable, nothing sent.
    /// An unknown code is a bug, answered as `internal`.
    pub fn new(code: &'static str) -> Self {
        Self(ServiceError::new(code))
    }

    /// `429 too_many_requests`, retryable, with `Retry-After`.
    pub fn too_many_requests(retry_after_secs: u64) -> Self {
        Self(ServiceError::too_many_requests(retry_after_secs))
    }

    /// `422 invalid_request` on `field`.
    pub fn invalid(field: impl Into<String>) -> Self {
        Self(ServiceError::invalid(field))
    }

    /// `404 not_found`.
    pub fn not_found() -> Self {
        Self(ServiceError::not_found())
    }

    /// `403 forbidden`.
    pub fn forbidden() -> Self {
        Self(ServiceError::forbidden())
    }

    /// `401 unauthenticated`.
    pub fn unauthenticated() -> Self {
        Self(ServiceError::unauthenticated())
    }

    /// `500 internal`.
    pub fn internal() -> Self {
        Self(ServiceError::internal())
    }

    /// Set `retryable`.
    #[must_use]
    pub fn retryable(self, retryable: bool) -> Self {
        Self(self.0.retryable(retryable))
    }

    /// Set `resumable`: whether repeating the operation that stopped at
    /// `step` finishes it.
    #[must_use]
    pub fn resumable(self, resumable: bool) -> Self {
        Self(self.0.resumable(resumable))
    }

    /// The same error, answered as `422 invalid_request` on `field` (Meta
    /// refused a value the request carried): `graph`, `retryable` and
    /// `may_have_been_sent` stay.
    #[must_use]
    pub fn as_invalid(self, field: impl Into<String>) -> Self {
        Self(self.0.as_invalid(field))
    }

    /// Set `may_have_been_sent`.
    #[must_use]
    pub fn with_may_have_been_sent(self, sent: bool) -> Self {
        Self(self.0.with_may_have_been_sent(sent))
    }

    /// The HTTP status.
    pub fn status(&self) -> StatusCode {
        StatusCode::from_u16(self.0.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// The code.
    pub fn code(&self) -> &'static str {
        self.0.code()
    }

    /// Whether the request may have taken effect.
    pub fn may_have_been_sent(&self) -> bool {
        self.0.may_have_been_sent()
    }

    /// Whether repeating the request later may succeed.
    pub fn is_retryable(&self) -> bool {
        self.0.is_retryable()
    }

    /// The field at fault, for `invalid_request`.
    pub fn field(&self) -> Option<&str> {
        self.0.field()
    }

    /// The error, as data.
    pub fn as_service_error(&self) -> &ServiceError {
        &self.0
    }

    /// The error, as data.
    pub fn into_service_error(self) -> ServiceError {
        self.0
    }

    /// The code and status of a library error, `retryable` and
    /// `may_have_been_sent` from the library, and Meta's code and trace id
    /// when Meta answered with a Graph error; never Meta's texts (for
    /// `details`, see [`Self::with_details`]). Logs the error (Meta's texts
    /// at `debug` only).
    pub fn from_library(error: &Error) -> Self {
        Self(ServiceError::from_library(error))
    }

    /// Add Meta's `error_data.details` (or a string `error_data`) to
    /// `graph`, for a route whose design keeps it (section 5.1: every route
    /// but OTP and signup), stripped of control and format characters and
    /// line separators, and cut at [`MAX_DETAILS_CHARS`]. A route opts in
    /// by calling this.
    #[must_use]
    pub fn with_details(self, error: &Error) -> Self {
        Self(self.0.with_details(error))
    }

    /// The JSON body, with the current request's id.
    pub fn body(&self) -> ErrorBody {
        error_body(&self.0)
    }
}

/// The JSON body of `error`, with the current request's id.
pub fn error_body(error: &ServiceError) -> ErrorBody {
    ErrorBody {
        error: ErrorObject {
            code: error.code(),
            message: error.message(),
            retryable: error.is_retryable(),
            may_have_been_sent: error.may_have_been_sent(),
            field: error.field().map(str::to_owned),
            step: error.step(),
            resumable: error.is_resumable(),
            graph: error.graph().map(|graph| GraphErrorInfo {
                code: graph.code,
                subcode: graph.subcode,
                fbtrace_id: graph.fbtrace_id.clone(),
                details: graph.details.clone(),
            }),
            request_id: telemetry::current_request_id(),
        },
    }
}

/// The JSON body of `error`, serialized: what an idempotency key keeps for
/// a failure that may have been sent
/// ([`meta_whatsapp_server_core::idempotency::RenderError`]).
pub fn error_body_bytes(error: &ServiceError) -> Vec<u8> {
    serde_json::to_vec(&error_body(error)).unwrap_or_default()
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let unauthenticated = status == StatusCode::UNAUTHORIZED;
        let mut response = (status, Json(self.body())).into_response();
        if unauthenticated {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if let Some(seconds) = self.0.retry_after() {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
            .extensions_mut()
            .insert(ErrorCodeTag(self.0.code()));
        response
    }
}

/// The error code of an error answer, for metrics.
#[derive(Debug, Clone, Copy)]
pub struct ErrorCodeTag(pub &'static str);

impl From<ServiceError> for ApiError {
    fn from(error: ServiceError) -> Self {
        Self(error)
    }
}

impl From<meta_whatsapp_rs::core::error::StorageError> for ApiError {
    fn from(error: meta_whatsapp_rs::core::error::StorageError) -> Self {
        Self(ServiceError::from(error))
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        Self(ServiceError::from(error))
    }
}

/// The `KnownErrorCode` schema: every code in [`CODES`], the ones this
/// version answers.
pub struct KnownErrorCode;

impl utoipa::PartialSchema for KnownErrorCode {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, Type};
        ObjectBuilder::new()
            .schema_type(Type::String)
            .enum_values(Some(CODES.iter().map(|(code, _, _)| *code)))
            .description(Some(
                "The error codes this version answers (a client can switch on them and let \
                 any other fall to its default).",
            ))
            .into()
    }
}

impl ToSchema for KnownErrorCode {}

/// The `ErrorCode` schema: a [`KnownErrorCode`], or any other string,
/// since codes grow within `v1`.
pub struct ErrorCode;

impl utoipa::PartialSchema for ErrorCode {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::Ref;
        use utoipa::openapi::schema::{AnyOfBuilder, ObjectBuilder, Type};
        AnyOfBuilder::new()
            .item(Ref::from_schema_name("KnownErrorCode"))
            .item(ObjectBuilder::new().schema_type(Type::String))
            .description(Some(
                "Stable error code: a KnownErrorCode today. Codes only grow within v1: handle \
                 an unknown one by its HTTP status class.",
            ))
            .into()
    }
}

impl ToSchema for ErrorCode {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_snake_case_with_a_known_status() {
        let mut seen = std::collections::HashSet::new();
        for (code, status, message) in CODES {
            assert!(seen.insert(*code), "{code} twice");
            assert!(
                code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{code}"
            );
            assert!(StatusCode::from_u16(*status).is_ok(), "{code}");
            assert!(!message.is_empty(), "{code}");
        }
    }

    /// Every code the source names (`ApiError::new("…")`, `.code() ==
    /// "…"`) is in [`CODES`]: a typo would otherwise answer `internal`, or
    /// never match.
    #[test]
    fn every_code_the_source_names_exists() {
        let sources = [
            include_str!("auth.rs"),
            include_str!("error.rs"),
            include_str!("idempotency.rs"),
            include_str!("api/mod.rs"),
            include_str!("api/admin.rs"),
            include_str!("api/numbers.rs"),
            include_str!("api/ops.rs"),
            include_str!("api/common.rs"),
            include_str!("api/webhooks.rs"),
            include_str!("api/messages.rs"),
            include_str!("api/media.rs"),
            include_str!("api/templates.rs"),
            include_str!("api/events.rs"),
            include_str!("events.rs"),
        ];
        let mut named = 0;
        for source in sources {
            let code = source.split("#[cfg(test)]").next().unwrap_or(source);
            for needle in ["ApiError::new(\"", ".code() == \"", "Self::new(\""] {
                for (i, _) in code.match_indices(needle) {
                    let rest = &code[i + needle.len()..];
                    let name = &rest[..rest.find('"').unwrap()];
                    named += 1;
                    assert!(
                        CODES.iter().any(|(c, _, _)| *c == name),
                        "`{name}` is not in CODES"
                    );
                }
            }
        }
        assert!(named >= 20, "{named}");
    }

    #[test]
    fn an_unknown_code_is_internal() {
        let error = ApiError::new("no_such_code");
        assert_eq!(error.code(), "internal");
        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
