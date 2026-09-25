//! The error model (docs/design/server.md, section 5).
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
//! - A Graph failure's code is its [`ErrorKind::as_str`] (library change
//!   L1), except `Authentication` on a merchant's token, which is
//!   `reconnect_required`. [`kind_code`] is the mapping; its test iterates
//!   [`ErrorKind::ALL`], so a kind a new library release adds fails the
//!   test instead of falling through unmapped.
//! - `message` is always the service's sentence for the code: never Meta's
//!   `message`, `title` or user-facing texts, and never an input value.
//! - `graph.details` is Meta's text (`error_data.details`), not the
//!   service's: section 5.1 of the design keeps it, except on the OTP and
//!   signup routes (a template error could quote a parameter: the code).
//!   So it is **opt-in per route** ([`ApiError::with_details`]; a new route
//!   answers without it), stripped of control and format characters and
//!   line separators, cut at [`MAX_DETAILS_CHARS`], and logged at `debug`
//!   only.
//! - `retryable` and `may_have_been_sent` are the library's
//!   [`Error::is_retryable`] and [`Error::may_have_been_sent`]: resend only
//!   when `may_have_been_sent` is `false`.

use std::fmt;

use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderValue, StatusCode, header};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_rs::{Error, ErrorKind, GraphApiError};
use serde::Serialize;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
use utoipa::ToSchema;

use crate::telemetry;

/// `(code, HTTP status, the service's sentence)` for every error code the
/// API answers: the table of docs/design/server.md, section 5.2, which
/// `tests/errors.rs` reads to compare.
pub const CODES: &[(&str, u16, &str)] = &[
    // 401, 405
    (
        "unauthenticated",
        401,
        "A valid API key is required: send Authorization: Bearer wak_….",
    ),
    (
        "method_not_allowed",
        405,
        "This path does not take this method: see the Allow header.",
    ),
    // 422: fix the request; nothing was sent.
    (
        "invalid_request",
        422,
        "The request is invalid: see `field`.",
    ),
    (
        "invalid_parameter",
        422,
        "Meta rejected a parameter of the request.",
    ),
    (
        "unsupported_message_type",
        422,
        "This message type is not supported.",
    ),
    (
        "recipient_not_supported",
        422,
        "This message cannot be sent to that recipient: address them by phone number.",
    ),
    (
        "undeliverable",
        422,
        "The message cannot be delivered to this recipient.",
    ),
    (
        "template_parameter_mismatch",
        422,
        "The parameters do not match the approved template.",
    ),
    (
        "template_not_found",
        422,
        "The template does not exist in that language, or is not approved.",
    ),
    (
        "template_text_too_long",
        422,
        "The template's text is too long once its parameters are filled in.",
    ),
    (
        "template_policy_violation",
        422,
        "The content violates WhatsApp's policies.",
    ),
    (
        "template_rejected",
        422,
        "Meta rejected the template's content or format.",
    ),
    (
        "idempotency_key_reused",
        422,
        "This Idempotency-Key was used with another request.",
    ),
    // 409: a state must change first.
    (
        "customer_service_window_closed",
        409,
        "More than 24 hours since the customer's last message: send a template.",
    ),
    (
        "marketing_opted_out",
        409,
        "The customer stopped marketing messages from this business.",
    ),
    (
        "blocked_by_business",
        409,
        "This business blocked the customer: unblock them first.",
    ),
    (
        "experiment_holdout",
        409,
        "Meta held the message back as part of an experiment.",
    ),
    (
        "template_paused",
        409,
        "The template is paused for low quality.",
    ),
    ("template_disabled", 409, "The template is disabled."),
    (
        "template_syncing",
        409,
        "The template is still syncing: try again in a few minutes.",
    ),
    (
        "template_unavailable",
        409,
        "The template is not available for this API.",
    ),
    (
        "template_limit_reached",
        409,
        "The WhatsApp Business Account has reached its template limit.",
    ),
    ("flow_unavailable", 409, "The Flow is blocked or throttled."),
    (
        "registration",
        409,
        "The phone number is not registered or verified.",
    ),
    (
        "two_step_verification",
        409,
        "The two-step verification PIN is wrong, or was guessed too often.",
    ),
    (
        "sync_not_allowed",
        409,
        "The coexistence sync is no longer allowed.",
    ),
    (
        "duplicate_onboarding",
        409,
        "A partner already requested this onboarding.",
    ),
    (
        "number_not_connected",
        409,
        "The phone number has no stored token: connect it first.",
    ),
    (
        "reconnect_required",
        409,
        "The number's token is no longer valid: connect it again.",
    ),
    (
        "waba_owned_by_another_tenant",
        409,
        "This WhatsApp Business Account, or one of its numbers, belongs to another tenant.",
    ),
    ("tenant_exists", 409, "A tenant with this id exists."),
    (
        "idempotency_in_progress",
        409,
        "A request with this Idempotency-Key is still running.",
    ),
    (
        "outcome_unknown",
        409,
        "The outcome of the request with this Idempotency-Key is unknown: reconcile before retrying.",
    ),
    // 403: not allowed, by Meta or the service.
    (
        "permission",
        403,
        "The token lacks a permission, or access to this asset.",
    ),
    (
        "account_restricted",
        403,
        "The account is restricted for a policy violation.",
    ),
    (
        "country_restricted",
        403,
        "The account cannot message users in that country.",
    ),
    ("payment", 403, "The account has a payment problem."),
    (
        "feature_not_available",
        403,
        "This feature is not enabled for the account.",
    ),
    (
        "marketing_not_allowed",
        403,
        "Marketing messages are not allowed here.",
    ),
    ("forbidden", 403, "This key may not do this."),
    ("tenant_suspended", 403, "The tenant is suspended."),
    (
        "stale_attempt",
        403,
        "The signup attempt expired, was used, or belongs to another tenant.",
    ),
    // 429
    (
        "rate_limited",
        429,
        "Too many requests: back off and retry.",
    ),
    (
        "pair_rate_limited",
        429,
        "Too many messages to this recipient: slow down for them.",
    ),
    (
        "spam_rate_limited",
        429,
        "Sending is restricted after messages were reported as spam: do not retry.",
    ),
    (
        "ecosystem_engagement_limit",
        429,
        "Not delivered, to keep engagement healthy: do not retry for 24 hours.",
    ),
    (
        "classification_limit_reached",
        429,
        "The messaging limit was reached after template classification violations.",
    ),
    (
        "too_many_requests",
        429,
        "Too many requests: back off and retry.",
    ),
    ("too_many_streams", 429, "Too many open event streams."),
    // 404, 410, 413
    ("not_found", 404, "Not found."),
    (
        "nothing_to_resume",
        404,
        "There is no signup attempt to resume.",
    ),
    (
        "cursor_expired",
        410,
        "The cursor is past the retention period.",
    ),
    ("payload_too_large", 413, "The request body is too large."),
    ("media_too_large", 413, "The media is larger than allowed."),
    // 502: Meta or the network failed.
    (
        "service_unavailable",
        502,
        "Meta is unavailable or overloaded: try again later.",
    ),
    (
        "unknown",
        502,
        "Meta answered with an error the service does not know.",
    ),
    (
        "upstream",
        502,
        "Meta answered with something that is not a Graph API answer.",
    ),
    (
        "integrity",
        502,
        "The downloaded media failed its integrity check.",
    ),
    (
        "media_download_failed",
        502,
        "The media could not be downloaded.",
    ),
    (
        "media_upload_failed",
        502,
        "The media could not be uploaded.",
    ),
    ("onboarding_failed", 502, "Onboarding stopped at `step`."),
    // 504
    (
        "timeout",
        504,
        "Meta did not answer in time: the request may have taken effect.",
    ),
    // 503, 500
    (
        "storage_unavailable",
        503,
        "The service's storage is unavailable: try again later.",
    ),
    (
        "shutting_down",
        503,
        "The service is shutting down: retry on another replica.",
    ),
    ("internal", 500, "Internal error."),
];

/// The HTTP status and sentence of `code`; `None` for a code not in
/// [`CODES`].
pub fn code_info(code: &str) -> Option<(StatusCode, &'static str)> {
    CODES
        .iter()
        .find(|(c, _, _)| *c == code)
        .and_then(|(_, status, message)| StatusCode::from_u16(*status).ok().map(|s| (s, *message)))
}

/// Longest `graph.details` answered, in characters.
pub const MAX_DETAILS_CHARS: usize = 512;

/// Meta's details as answered: at most [`MAX_DETAILS_CHARS`] characters,
/// without the characters that could forge or disguise a line of a
/// caller's log: control characters (Cc: a newline starts a new line),
/// format characters (Cf: a right-to-left override, U+202E, reverses what
/// follows; zero-width ones hide text) and the line and paragraph
/// separators (U+2028, U+2029: a new line to some log viewers).
fn bounded_details(details: &str) -> String {
    details
        .chars()
        .filter(|&c| !forges_log_lines(c))
        .take(MAX_DETAILS_CHARS)
        .collect()
}

/// A character [`bounded_details`] drops.
fn forges_log_lines(c: char) -> bool {
    c.is_control()
        || matches!(
            c.general_category(),
            GeneralCategory::Format
                | GeneralCategory::LineSeparator
                | GeneralCategory::ParagraphSeparator
        )
}

/// The API code of a Graph error kind: [`ErrorKind::as_str`], except
/// `Authentication` (codes 0 and 190 on a merchant's token), which is
/// `reconnect_required`.
pub fn kind_code(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Authentication => "reconnect_required",
        other => other.as_str(),
    }
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

/// An API error. Build one from a code ([`ApiError::new`]) or from a
/// library error ([`ApiError::from_library`]).
#[derive(Clone, PartialEq, Eq)]
pub struct ApiError(Box<Details>);

#[derive(Clone, PartialEq, Eq)]
struct Details {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retryable: bool,
    may_have_been_sent: bool,
    field: Option<String>,
    step: Option<&'static str>,
    resumable: Option<bool>,
    graph: Option<GraphErrorInfo>,
}

impl fmt::Debug for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiError")
            .field("status", &self.0.status.as_u16())
            .field("code", &self.0.code)
            .field("field", &self.0.field)
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
        let (status, message, code) = match code_info(code) {
            Some((status, message)) => (status, message, code),
            None => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal error.",
                "internal",
            ),
        };
        Self(Box::new(Details {
            status,
            code,
            message,
            retryable: false,
            may_have_been_sent: false,
            field: None,
            step: None,
            resumable: None,
            graph: None,
        }))
    }

    /// `422 invalid_request` on `field`.
    pub fn invalid(field: impl Into<String>) -> Self {
        let mut error = Self::new("invalid_request");
        error.0.field = Some(field.into());
        error
    }

    /// `404 not_found`.
    pub fn not_found() -> Self {
        Self::new("not_found")
    }

    /// `403 forbidden`.
    pub fn forbidden() -> Self {
        Self::new("forbidden")
    }

    /// `401 unauthenticated`.
    pub fn unauthenticated() -> Self {
        Self::new("unauthenticated")
    }

    /// `500 internal`.
    pub fn internal() -> Self {
        Self::new("internal")
    }

    /// Set `retryable`.
    #[must_use]
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.0.retryable = retryable;
        self
    }

    /// Set `resumable`: whether repeating the operation that stopped at
    /// `step` finishes it.
    #[must_use]
    pub fn resumable(mut self, resumable: bool) -> Self {
        self.0.resumable = Some(resumable);
        self
    }

    /// The same error, answered as `422 invalid_request` on `field` (Meta
    /// refused a value the request carried): `graph`, `retryable` and
    /// `may_have_been_sent` stay.
    #[must_use]
    pub fn as_invalid(mut self, field: impl Into<String>) -> Self {
        let invalid = Self::invalid(field);
        self.0.status = invalid.0.status;
        self.0.code = invalid.0.code;
        self.0.message = invalid.0.message;
        self.0.field = invalid.0.field;
        self
    }

    /// Set `may_have_been_sent`.
    #[must_use]
    pub fn with_may_have_been_sent(mut self, sent: bool) -> Self {
        self.0.may_have_been_sent = sent;
        self
    }

    /// The HTTP status.
    pub fn status(&self) -> StatusCode {
        self.0.status
    }

    /// The code.
    pub fn code(&self) -> &'static str {
        self.0.code
    }

    /// Whether the request may have taken effect.
    pub fn may_have_been_sent(&self) -> bool {
        self.0.may_have_been_sent
    }

    /// The code and status of a library error, `retryable` and
    /// `may_have_been_sent` from the library, and Meta's code and trace id
    /// when Meta answered with a Graph error; never Meta's texts (for
    /// `details`, see [`Self::with_details`]). Logs the error (Meta's texts
    /// at `debug` only).
    pub fn from_library(error: &Error) -> Self {
        let mut api = Self::classify(error);
        api.0.retryable = error.is_retryable();
        api.0.may_have_been_sent = error.may_have_been_sent();
        if let Some(graph) = error.graph() {
            api.0.graph = Some(GraphErrorInfo {
                code: graph.code,
                subcode: graph.error_subcode,
                fbtrace_id: graph.fbtrace_id.clone(),
                details: None,
            });
        }
        log(error, &api);
        api
    }

    /// Add Meta's `error_data.details` (or a string `error_data`) to
    /// `graph`, for a route whose design keeps it (section 5.1: every route
    /// but OTP and signup), stripped of control and format characters and
    /// line separators, and cut at [`MAX_DETAILS_CHARS`]. A route opts in
    /// by calling this.
    #[must_use]
    pub fn with_details(mut self, error: &Error) -> Self {
        if let (Some(info), Some(graph)) = (self.0.graph.as_mut(), error.graph()) {
            info.details = graph.details().map(bounded_details);
        }
        self
    }

    fn classify(error: &Error) -> Self {
        match error {
            Error::Api(graph) => Self::new(kind_code(graph.kind())),
            // A non-Graph answer, or a 2xx of the wrong shape.
            Error::Http { .. } | Error::Decode { .. } => Self::new("upstream"),
            Error::Transport(TransportError::Timeout) => Self::new("timeout"),
            Error::Transport(TransportError::Integrity(_)) => Self::new("integrity"),
            Error::Transport(_) => Self::new("service_unavailable"),
            Error::Validation(v) if v.is_customer_service_window_closed() => {
                Self::new("customer_service_window_closed")
            }
            // The library's reason may quote the value: only the field.
            Error::Validation(v) => Self::invalid(v.field.clone()),
            Error::Storage(_) => Self::new("storage_unavailable"),
            Error::Step { step, source } => {
                let mut inner = Self::classify(source);
                inner.0.step = Some(step);
                inner
            }
            // Configuration, an undecryptable vault record, a sink, a
            // credit line step (routes of later milestones), anything else.
            _ => Self::internal(),
        }
    }

    /// The JSON body, with the current request's id.
    pub fn body(&self) -> ErrorBody {
        let d = &self.0;
        ErrorBody {
            error: ErrorObject {
                code: d.code,
                message: d.message,
                retryable: d.retryable,
                may_have_been_sent: d.may_have_been_sent,
                field: d.field.clone(),
                step: d.step,
                resumable: d.resumable,
                graph: d.graph.clone(),
                request_id: telemetry::current_request_id(),
            },
        }
    }
}

/// Log a library error: its kind, Meta's code and trace id at `warn`, and
/// Meta's message at `debug` only (docs/design/server.md, section 6).
fn log(error: &Error, api: &ApiError) {
    let graph: Option<&GraphApiError> = error.graph();
    tracing::warn!(
        code = api.0.code,
        status = api.0.status.as_u16(),
        kind = error.kind().as_str(),
        graph_code = graph.map(|g| g.code),
        fbtrace_id = graph.and_then(|g| g.fbtrace_id.as_deref()),
        may_have_been_sent = api.0.may_have_been_sent,
        "request failed"
    );
    if let Some(graph) = graph {
        tracing::debug!(
            graph_message = %graph.summary(),
            graph_details = graph.details(),
            "Meta's error texts"
        );
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.0.status;
        let unauthenticated = status == StatusCode::UNAUTHORIZED;
        let mut response = (status, Json(self.body())).into_response();
        if unauthenticated {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response.extensions_mut().insert(ErrorCodeTag(self.0.code));
        response
    }
}

/// The error code of an error answer, for metrics.
#[derive(Debug, Clone, Copy)]
pub struct ErrorCodeTag(pub &'static str);

impl From<meta_whatsapp_rs::core::error::StorageError> for ApiError {
    fn from(error: meta_whatsapp_rs::core::error::StorageError) -> Self {
        Self::from_library(&Error::Storage(error))
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        Self::from_library(&error)
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

    /// `details` is opt-in, bounded to 512 characters and free of control
    /// characters.
    #[test]
    fn details_are_opt_in_and_bounded() {
        let error = |data: serde_json::Value| {
            let graph: GraphApiError = serde_json::from_value(serde_json::json!({
                "message": "(#131047) Re-engagement message",
                "type": "OAuthException",
                "code": 131047,
                "error_data": data,
            }))
            .unwrap();
            Error::Api(Box::new(graph))
        };
        let object = error(serde_json::json!({"details": "Message failed\r\nto send"}));
        let plain = ApiError::from_library(&object);
        assert_eq!(
            plain.0.graph.as_ref().unwrap().details,
            None,
            "off by default"
        );
        let kept = ApiError::from_library(&object).with_details(&object);
        assert_eq!(
            kept.0.graph.as_ref().unwrap().details.as_deref(),
            Some("Message failedto send")
        );
        let string = error(serde_json::json!("plain string"));
        let kept = ApiError::from_library(&string).with_details(&string);
        assert_eq!(
            kept.0.graph.unwrap().details.as_deref(),
            Some("plain string")
        );
        // 512 characters, the design's bound (section 5.1), whatever the
        // constant says.
        assert_eq!(MAX_DETAILS_CHARS, 512);
        let long = error(serde_json::json!({"details": "é".repeat(600)}));
        let kept = ApiError::from_library(&long).with_details(&long);
        assert_eq!(kept.0.graph.unwrap().details.unwrap(), "é".repeat(512));
    }

    /// Characters that forge or disguise a log line go too: format
    /// characters (a right-to-left override, zero-width ones, a byte order
    /// mark, tags) and the line and paragraph separators; letters, symbols
    /// and spaces of any script stay. Decisive: the general categories
    /// checked beside `is_control`.
    #[test]
    fn details_lose_format_characters_and_line_separators() {
        let graph: GraphApiError = serde_json::from_value(serde_json::json!({
            "message": "(#100) Invalid parameter",
            "type": "OAuthException",
            "code": 100,
            "error_data": {"details": "Param\u{202E}txt.exe\u{202C} ok\u{2028}level=ERROR\u{2029}x\u{200B}y\u{2066}z\u{2069}\u{FEFF}\u{E0041}\u{00AD} café 日本 👍\u{00A0}end"},
        }))
        .unwrap();
        let error = Error::Api(Box::new(graph));
        let kept = ApiError::from_library(&error).with_details(&error);
        assert_eq!(
            kept.0.graph.unwrap().details.as_deref(),
            Some("Paramtxt.exe oklevel=ERRORxyz café 日本 👍\u{00A0}end")
        );
        for c in [
            '\u{202E}', '\u{200F}', '\u{2028}', '\u{2029}', '\u{FEFF}', '\n', '\u{85}',
        ] {
            assert!(forges_log_lines(c), "{:04X}", u32::from(c));
        }
        for c in ['a', 'é', '日', '👍', ' ', '\u{00A0}', '\u{3000}'] {
            assert!(!forges_log_lines(c), "{:04X}", u32::from(c));
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
            include_str!("api/mod.rs"),
            include_str!("api/admin.rs"),
            include_str!("api/numbers.rs"),
            include_str!("api/ops.rs"),
            include_str!("api/common.rs"),
            include_str!("api/webhooks.rs"),
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
