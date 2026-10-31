//! The error model as data (docs/design/server.md, section 5): every
//! failure the service answers is a [`ServiceError`], a stable
//! [`ErrorCode`] with its status as a number, and the flags a caller acts
//! on. An API adapter renders it: the HTTP one
//! (`meta_whatsapp_server::error`) as the `ErrorBody` JSON, with
//! `WWW-Authenticate` and `Retry-After` headers.
//!
//! - The code is stable: [`CODES`] lists every one with its status and the
//!   service's sentence for it. Codes only grow within `v1`.
//! - A Graph failure's code is its [`ErrorKind::as_str`] (library change
//!   L1), except `Authentication` on a merchant's token, which is
//!   `reconnect_required`. [`kind_code`] is the mapping; the service's test
//!   iterates [`ErrorKind::ALL`], so a kind a new library release adds
//!   fails the test instead of falling through unmapped.
//! - The message is always the service's sentence for the code: never
//!   Meta's `message`, `title` or user-facing texts, and never an input
//!   value.
//! - `graph.details` is Meta's text (`error_data.details`), not the
//!   service's: section 5.1 of the design keeps it, except on the OTP and
//!   signup routes (a template error could quote a parameter: the code).
//!   So it is **opt-in per route** ([`ServiceError::with_details`]; a new
//!   route answers without it), stripped of control and format characters
//!   and line separators, cut at [`MAX_DETAILS_CHARS`], and logged at
//!   `debug` only.
//! - `retryable` and `may_have_been_sent` are the library's
//!   [`Error::is_retryable`] and [`Error::may_have_been_sent`]: resend only
//!   when `may_have_been_sent` is `false`.

use std::fmt;

use meta_whatsapp_rs::core::error::{StorageError, TransportError};
use meta_whatsapp_rs::{Error, ErrorKind, GraphApiError};
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

/// The code table: each entry is a constant of [`ErrorCode`] (its name the
/// code in capitals), a row of [`CODES`] and an element of
/// [`ErrorCode::ALL`], in one place, so a code the source names is one
/// the table has: a misspelled one does not compile. Kept equal to the
/// table of docs/design/server.md, section 5.2, code for code and status
/// for status, by `the_codes_are_the_designs` (here, and the service's
/// `tests/errors.rs`), and each constant's name to its code by
/// `each_constant_is_named_after_its_code`.
macro_rules! codes {
    ($($name:ident = ($code:literal, $status:literal, $message:literal),)*) => {
        impl ErrorCode {
            $(
                #[doc = concat!("`", $status, " ", $code, "`: ", $message)]
                pub const $name: Self = Self {
                    code: $code,
                    status: $status,
                    message: $message,
                };
            )*

            /// Every code, in the order of [`CODES`].
            pub const ALL: &'static [Self] = &[$(Self::$name),*];
        }

        /// `(code, HTTP status, the service's sentence)` for every error
        /// code the API answers ([`ErrorCode::ALL`], as tuples): the table
        /// of docs/design/server.md, section 5.2, which the tests read to
        /// compare.
        pub const CODES: &[(&str, u16, &str)] = &[$(($code, $status, $message)),*];

        /// Each constant's name, for `each_constant_is_named_after_its_code`.
        #[cfg(test)]
        const NAMES: &[&str] = &[$(stringify!($name)),*];
    };
}

codes! {
    // 401, 405
    UNAUTHENTICATED = ("unauthenticated", 401, "A valid API key is required: send Authorization: Bearer wak_…."),
    METHOD_NOT_ALLOWED = ("method_not_allowed", 405, "This path does not take this method: see the Allow header."),
    // 422: fix the request; nothing was sent.
    INVALID_REQUEST = ("invalid_request", 422, "The request is invalid: see `field`."),
    INVALID_PARAMETER = ("invalid_parameter", 422, "Meta rejected a parameter of the request."),
    UNSUPPORTED_MESSAGE_TYPE = ("unsupported_message_type", 422, "This message type is not supported."),
    RECIPIENT_NOT_SUPPORTED = ("recipient_not_supported", 422, "This message cannot be sent to that recipient: address them by phone number."),
    UNDELIVERABLE = ("undeliverable", 422, "The message cannot be delivered to this recipient."),
    TEMPLATE_PARAMETER_MISMATCH = ("template_parameter_mismatch", 422, "The parameters do not match the approved template."),
    TEMPLATE_NOT_FOUND = ("template_not_found", 422, "The template does not exist in that language, or is not approved."),
    TEMPLATE_TEXT_TOO_LONG = ("template_text_too_long", 422, "The template's text is too long once its parameters are filled in."),
    TEMPLATE_POLICY_VIOLATION = ("template_policy_violation", 422, "The content violates WhatsApp's policies."),
    TEMPLATE_REJECTED = ("template_rejected", 422, "Meta rejected the template's content or format."),
    SAMPLE_TEMPLATE_NOT_ALLOWED = ("sample_template_not_allowed", 422, "Meta sends its sample template only from its public test numbers: send one of your own templates."),
    IDEMPOTENCY_KEY_REUSED = ("idempotency_key_reused", 422, "This Idempotency-Key was used with another request."),
    // 409: a state must change first.
    CUSTOMER_SERVICE_WINDOW_CLOSED = ("customer_service_window_closed", 409, "More than 24 hours since the customer's last message: send a template."),
    THREAD_OWNED_ELSEWHERE = ("thread_owned_elsewhere", 409, "Another app owns this conversation's thread: send a template, or wait for the thread."),
    MARKETING_OPTED_OUT = ("marketing_opted_out", 409, "The customer stopped marketing messages from this business."),
    BLOCKED_BY_BUSINESS = ("blocked_by_business", 409, "This business blocked the customer: unblock them first."),
    EXPERIMENT_HOLDOUT = ("experiment_holdout", 409, "Meta held the message back as part of an experiment."),
    TEMPLATE_PAUSED = ("template_paused", 409, "The template is paused for low quality."),
    TEMPLATE_DISABLED = ("template_disabled", 409, "The template is disabled."),
    TEMPLATE_SYNCING = ("template_syncing", 409, "The template is still syncing: try again in a few minutes."),
    TEMPLATE_UNAVAILABLE = ("template_unavailable", 409, "The template is not available for this API."),
    TEMPLATE_LIMIT_REACHED = ("template_limit_reached", 409, "The WhatsApp Business Account has reached its template limit."),
    FLOW_UNAVAILABLE = ("flow_unavailable", 409, "The Flow is blocked or throttled."),
    REGISTRATION = ("registration", 409, "The phone number is not registered or verified."),
    TWO_STEP_VERIFICATION = ("two_step_verification", 409, "The two-step verification PIN is wrong, or was guessed too often."),
    SYNC_NOT_ALLOWED = ("sync_not_allowed", 409, "The coexistence sync is no longer allowed."),
    DUPLICATE_ONBOARDING = ("duplicate_onboarding", 409, "A partner already requested this onboarding."),
    NUMBER_NOT_CONNECTED = ("number_not_connected", 409, "The phone number has no stored token: connect it first."),
    RECONNECT_REQUIRED = ("reconnect_required", 409, "The number's token is no longer valid: connect it again."),
    WABA_OWNED_BY_ANOTHER_TENANT = ("waba_owned_by_another_tenant", 409, "This WhatsApp Business Account, or one of its numbers, belongs to another tenant."),
    TENANT_EXISTS = ("tenant_exists", 409, "A tenant with this id exists."),
    IDEMPOTENCY_IN_PROGRESS = ("idempotency_in_progress", 409, "A request with this Idempotency-Key is still running."),
    OUTCOME_UNKNOWN = ("outcome_unknown", 409, "The outcome of the request with this Idempotency-Key is unknown: reconcile before retrying."),
    // 403: not allowed, by Meta or the service.
    PERMISSION = ("permission", 403, "The token lacks a permission, or access to this asset."),
    ACCOUNT_RESTRICTED = ("account_restricted", 403, "The account is restricted for a policy violation."),
    COUNTRY_RESTRICTED = ("country_restricted", 403, "The account cannot message users in that country."),
    PAYMENT = ("payment", 403, "The account has a payment problem."),
    FEATURE_NOT_AVAILABLE = ("feature_not_available", 403, "This feature is not enabled for the account."),
    MARKETING_NOT_ALLOWED = ("marketing_not_allowed", 403, "Marketing messages are not allowed here."),
    FORBIDDEN = ("forbidden", 403, "This key may not do this."),
    TENANT_SUSPENDED = ("tenant_suspended", 403, "The tenant is suspended."),
    STALE_ATTEMPT = ("stale_attempt", 403, "The signup attempt expired, was used, or belongs to another tenant."),
    // 429
    RATE_LIMITED = ("rate_limited", 429, "Too many requests: back off and retry."),
    PAIR_RATE_LIMITED = ("pair_rate_limited", 429, "Too many messages to this recipient: slow down for them."),
    SPAM_RATE_LIMITED = ("spam_rate_limited", 429, "Sending is restricted after messages were reported as spam: do not retry."),
    ECOSYSTEM_ENGAGEMENT_LIMIT = ("ecosystem_engagement_limit", 429, "Not delivered, to keep engagement healthy: do not retry for 24 hours."),
    CLASSIFICATION_LIMIT_REACHED = ("classification_limit_reached", 429, "The messaging limit was reached after template classification violations."),
    TOO_MANY_REQUESTS = ("too_many_requests", 429, "Too many requests: back off and retry."),
    TOO_MANY_STREAMS = ("too_many_streams", 429, "Too many open event streams."),
    // 404, 410, 413
    NOT_FOUND = ("not_found", 404, "Not found."),
    NOTHING_TO_RESUME = ("nothing_to_resume", 404, "There is no signup attempt to resume."),
    CURSOR_EXPIRED = ("cursor_expired", 410, "The cursor is past the retention period."),
    PAYLOAD_TOO_LARGE = ("payload_too_large", 413, "The request body is too large."),
    MEDIA_TOO_LARGE = ("media_too_large", 413, "The media is larger than allowed."),
    // 502: Meta or the network failed.
    SERVICE_UNAVAILABLE = ("service_unavailable", 502, "Meta is unavailable or overloaded: try again later."),
    UNKNOWN = ("unknown", 502, "Meta answered with an error the service does not know."),
    UPSTREAM = ("upstream", 502, "Meta answered with something that is not a Graph API answer."),
    INTEGRITY = ("integrity", 502, "The downloaded media failed its integrity check."),
    MEDIA_DOWNLOAD_FAILED = ("media_download_failed", 502, "The media could not be downloaded."),
    MEDIA_UPLOAD_FAILED = ("media_upload_failed", 502, "The media could not be uploaded."),
    ONBOARDING_FAILED = ("onboarding_failed", 502, "Onboarding stopped at `step`."),
    // 504
    TIMEOUT = ("timeout", 504, "Meta did not answer in time: the request may have taken effect."),
    // 503, 500
    STORAGE_UNAVAILABLE = ("storage_unavailable", 503, "The service's storage is unavailable: try again later."),
    SHUTTING_DOWN = ("shutting_down", 503, "The service is shutting down: retry on another replica."),
    INTERNAL = ("internal", 500, "Internal error."),
}

/// The status and sentence of `code`; `None` for a code not in [`CODES`].
pub fn code_info(code: &str) -> Option<(u16, &'static str)> {
    ErrorCode::of(code).map(|code| (code.status(), code.message()))
}

/// One of [`CODES`]: a stable error code, with the HTTP status it is
/// answered with and the service's sentence for it. Only the table makes
/// one (a constant per code, e.g. [`ErrorCode::NOT_FOUND`]), so a code a
/// caller receives is always documented, and a code the source names is
/// checked by the compiler:
///
/// ```
/// use meta_whatsapp_server_core::error::{ErrorCode, ServiceError};
///
/// let error = ServiceError::new(ErrorCode::CUSTOMER_SERVICE_WINDOW_CLOSED);
/// assert_eq!((error.code(), error.status()), ("customer_service_window_closed", 409));
/// ```
///
/// A misspelled code is no constant (`E0599`):
///
/// ```compile_fail,E0599
/// use meta_whatsapp_server_core::error::{ErrorCode, ServiceError};
///
/// let error = ServiceError::new(ErrorCode::CUSTOMER_SERVICE_WINDOW_CLSOED);
/// ```
///
/// and a string is no code (`E0308`), whatever it spells:
///
/// ```compile_fail,E0308
/// use meta_whatsapp_server_core::error::ServiceError;
///
/// let error = ServiceError::new("customer_service_window_closed");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorCode {
    code: &'static str,
    status: u16,
    message: &'static str,
}

impl ErrorCode {
    /// The entry of [`CODES`] named `code` (one read at run time: a kept
    /// answer's, a library kind's name). A code the source names is a
    /// constant instead.
    pub fn of(code: &str) -> Option<Self> {
        Self::ALL.iter().find(|known| known.code == code).copied()
    }

    /// The code, e.g. `customer_service_window_closed`.
    pub fn as_str(self) -> &'static str {
        self.code
    }

    /// The HTTP status it is answered with, e.g. `409`.
    pub fn status(self) -> u16 {
        self.status
    }

    /// The service's sentence for it.
    pub fn message(self) -> &'static str {
        self.message
    }
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

/// Meta's error, as an error answer carries it: the code and trace id,
/// and, on the routes that opt in, `error_data.details`. Never Meta's
/// `message` or titles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphErrorInfo {
    /// Meta's error code, e.g. `131047`.
    pub code: i64,
    /// Meta's subcode, when it sent one (deprecated by Meta since v16).
    pub subcode: Option<i64>,
    /// Trace id for Meta's support.
    pub fbtrace_id: Option<String>,
    /// Meta's own text (`error_data.details`), on the routes that keep it:
    /// not the service's, and not a stable value to branch on.
    pub details: Option<String>,
}

/// A failure the service answers. Build one from a code
/// ([`ServiceError::new`]) or from a library error
/// ([`ServiceError::from_library`]).
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceError(Box<Details>);

#[derive(Clone, PartialEq, Eq)]
struct Details {
    code: ErrorCode,
    retryable: bool,
    may_have_been_sent: bool,
    field: Option<String>,
    step: Option<&'static str>,
    resumable: Option<bool>,
    graph: Option<GraphErrorInfo>,
    /// `Retry-After`, in seconds, when the service knows it.
    retry_after: Option<u64>,
}

impl fmt::Debug for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceError")
            .field("status", &self.0.code.status())
            .field("code", &self.0.code.as_str())
            .field("field", &self.0.field)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The code and the service's sentence: never an input value.
        write!(f, "{}: {}", self.0.code.as_str(), self.0.code.message())
    }
}

impl std::error::Error for ServiceError {}

impl ServiceError {
    /// An error with `code` (a constant of [`ErrorCode`], so one of
    /// [`CODES`]): not retryable, nothing sent.
    pub fn new(code: ErrorCode) -> Self {
        Self(Box::new(Details {
            code,
            retryable: false,
            may_have_been_sent: false,
            field: None,
            step: None,
            resumable: None,
            graph: None,
            retry_after: None,
        }))
    }

    /// `429 too_many_requests`, retryable, with `Retry-After`.
    pub fn too_many_requests(retry_after_secs: u64) -> Self {
        let mut error = Self::new(ErrorCode::TOO_MANY_REQUESTS).retryable(true);
        error.0.retry_after = Some(retry_after_secs);
        error
    }

    /// `422 invalid_request` on `field`.
    pub fn invalid(field: impl Into<String>) -> Self {
        let mut error = Self::new(ErrorCode::INVALID_REQUEST);
        error.0.field = Some(field.into());
        error
    }

    /// `404 not_found`.
    pub fn not_found() -> Self {
        Self::new(ErrorCode::NOT_FOUND)
    }

    /// `403 forbidden`.
    pub fn forbidden() -> Self {
        Self::new(ErrorCode::FORBIDDEN)
    }

    /// `401 unauthenticated`.
    pub fn unauthenticated() -> Self {
        Self::new(ErrorCode::UNAUTHENTICATED)
    }

    /// `500 internal`.
    pub fn internal() -> Self {
        Self::new(ErrorCode::INTERNAL)
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
        self.0.code = invalid.0.code;
        self.0.field = invalid.0.field;
        self
    }

    /// Set `may_have_been_sent`.
    #[must_use]
    pub fn with_may_have_been_sent(mut self, sent: bool) -> Self {
        self.0.may_have_been_sent = sent;
        self
    }

    /// The code, with its status and sentence.
    pub fn error_code(&self) -> ErrorCode {
        self.0.code
    }

    /// The HTTP status, e.g. `409`.
    pub fn status(&self) -> u16 {
        self.0.code.status()
    }

    /// The code.
    pub fn code(&self) -> &'static str {
        self.0.code.as_str()
    }

    /// The service's sentence for the code.
    pub fn message(&self) -> &'static str {
        self.0.code.message()
    }

    /// Whether the request may have taken effect.
    pub fn may_have_been_sent(&self) -> bool {
        self.0.may_have_been_sent
    }

    /// Whether repeating the request later may succeed.
    pub fn is_retryable(&self) -> bool {
        self.0.retryable
    }

    /// The field at fault, for `invalid_request`.
    pub fn field(&self) -> Option<&str> {
        self.0.field.as_deref()
    }

    /// The step a multi-step operation stopped at.
    pub fn step(&self) -> Option<&'static str> {
        self.0.step
    }

    /// Whether the operation that stopped at [`Self::step`] can be resumed.
    pub fn is_resumable(&self) -> Option<bool> {
        self.0.resumable
    }

    /// Meta's error, when Meta answered with one.
    pub fn graph(&self) -> Option<&GraphErrorInfo> {
        self.0.graph.as_ref()
    }

    /// How long to wait before retrying, in seconds, when the service
    /// knows it (`Retry-After`).
    pub fn retry_after(&self) -> Option<u64> {
        self.0.retry_after
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
            // Every kind's code is in the table (the service's
            // `every_error_kind_maps_to_a_documented_code_and_status`
            // iterates `ErrorKind::ALL`); a kind a library release adds
            // without one would be a bug, answered as `internal`.
            Error::Api(graph) => {
                Self::new(ErrorCode::of(kind_code(graph.kind())).unwrap_or(ErrorCode::INTERNAL))
            }
            // A non-Graph answer, or a 2xx of the wrong shape.
            Error::Http { .. } | Error::Decode { .. } => Self::new(ErrorCode::UPSTREAM),
            Error::Transport(TransportError::Timeout) => Self::new(ErrorCode::TIMEOUT),
            Error::Transport(TransportError::Integrity(_)) => Self::new(ErrorCode::INTEGRITY),
            Error::Transport(_) => Self::new(ErrorCode::SERVICE_UNAVAILABLE),
            Error::Validation(v) if v.is_customer_service_window_closed() => {
                Self::new(ErrorCode::CUSTOMER_SERVICE_WINDOW_CLOSED)
            }
            Error::Validation(v) if v.is_thread_owned_elsewhere() => {
                Self::new(ErrorCode::THREAD_OWNED_ELSEWHERE)
            }
            // The library's reason may quote the value: only the field.
            Error::Validation(v) => Self::invalid(v.field.clone()),
            Error::Storage(_) => Self::new(ErrorCode::STORAGE_UNAVAILABLE),
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
}

/// Log a library error: its kind, Meta's code and trace id at `warn`, and
/// Meta's message at `debug` only (docs/design/server.md, section 6).
fn log(error: &Error, api: &ServiceError) {
    let graph: Option<&GraphApiError> = error.graph();
    tracing::warn!(
        code = api.0.code.as_str(),
        status = api.0.code.status(),
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

impl From<StorageError> for ServiceError {
    fn from(error: StorageError) -> Self {
        Self::from_library(&Error::Storage(error))
    }
}

impl From<Error> for ServiceError {
    fn from(error: Error) -> Self {
        Self::from_library(&error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let plain = ServiceError::from_library(&object);
        assert_eq!(
            plain.0.graph.as_ref().unwrap().details,
            None,
            "off by default"
        );
        let kept = ServiceError::from_library(&object).with_details(&object);
        assert_eq!(
            kept.0.graph.as_ref().unwrap().details.as_deref(),
            Some("Message failedto send")
        );
        let string = error(serde_json::json!("plain string"));
        let kept = ServiceError::from_library(&string).with_details(&string);
        assert_eq!(
            kept.0.graph.unwrap().details.as_deref(),
            Some("plain string")
        );
        // 512 characters, the design's bound (section 5.1), whatever the
        // constant says.
        assert_eq!(MAX_DETAILS_CHARS, 512);
        let long = error(serde_json::json!({"details": "é".repeat(600)}));
        let kept = ServiceError::from_library(&long).with_details(&long);
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
        let kept = ServiceError::from_library(&error).with_details(&error);
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

    /// A code read at run time (a kept answer's, a library kind's name) is
    /// the table's entry, or none. (A code the source names is a
    /// constant, which the compiler checks: the source scans of roadmap S1
    /// to S5e went with S6.)
    #[test]
    fn a_code_read_at_run_time_is_the_tables() {
        assert_eq!(ErrorCode::of("internal"), Some(ErrorCode::INTERNAL));
        assert_eq!(ErrorCode::of("no_such_code"), None);
        assert_eq!(
            code_info("cursor_expired"),
            Some((410, "The cursor is past the retention period."))
        );
        assert_eq!(code_info("no_such_code"), None);
        let error = ServiceError::new(ErrorCode::CURSOR_EXPIRED);
        assert_eq!(error.error_code(), ErrorCode::CURSOR_EXPIRED);
        assert_eq!((error.code(), error.status()), ("cursor_expired", 410));
    }

    /// The constants, [`ErrorCode::ALL`] and [`CODES`] are one table: each
    /// constant named after its code in capitals (`NOT_FOUND` is
    /// `not_found`, never another code), each code once, in snake case,
    /// with an error status and a sentence. Decisive: a constant whose
    /// name and code differ, a code twice.
    #[test]
    fn each_constant_is_named_after_its_code() {
        assert_eq!(NAMES.len(), ErrorCode::ALL.len());
        assert_eq!(CODES.len(), ErrorCode::ALL.len());
        let mut seen = std::collections::HashSet::new();
        for ((name, code), row) in NAMES.iter().zip(ErrorCode::ALL).zip(CODES) {
            assert_eq!(name.to_ascii_lowercase(), code.as_str(), "{name}");
            assert_eq!(*row, (code.as_str(), code.status(), code.message()));
            assert!(seen.insert(code.as_str()), "{name} twice");
            assert!(
                code.as_str()
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{name}"
            );
            assert!((400..600).contains(&code.status()), "{name}");
            assert!(!code.message().is_empty(), "{name}");
        }
    }

    /// docs/design/server.md, section 5.2, read from the document itself:
    /// every code of its table with the status of its row. Parenthesized
    /// remarks are not codes; a row of several statuses (`404, 410, 413`)
    /// gives each its `;`-separated group of codes. (The service's
    /// `tests/errors.rs` reads it the same way, for its own tests.)
    fn design() -> std::collections::HashMap<String, u16> {
        let doc = include_str!("../../../docs/design/server.md");
        let section = doc
            .split("### 5.2 Codes and statuses")
            .nth(1)
            .and_then(|rest| rest.split("\n### ").next())
            .expect("section 5.2 of docs/design/server.md");
        let mut map = std::collections::HashMap::new();
        let rows = section
            .lines()
            .filter(|l| l.starts_with("| ") && !l.starts_with("| HTTP") && !l.starts_with("| ---"));
        for row in rows {
            let cells: Vec<&str> = row.split('|').map(str::trim).collect();
            let statuses: Vec<u16> = cells[1]
                .split(',')
                .map(|s| s.trim().parse().unwrap())
                .collect();
            let mut codes = String::new();
            let mut depth = 0;
            for c in cells[2].chars() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ if depth == 0 => codes.push(c),
                    _ => {}
                }
            }
            let groups: Vec<&str> = codes.split(';').collect();
            assert!(
                statuses.len() == 1 || statuses.len() == groups.len(),
                "5.2: {row}"
            );
            for (i, group) in groups.iter().enumerate() {
                let status = statuses[if statuses.len() == 1 { 0 } else { i }];
                for code in group.split('`').skip(1).step_by(2) {
                    assert!(
                        map.insert(code.to_owned(), status).is_none(),
                        "{code} twice in 5.2"
                    );
                }
            }
        }
        assert!(map.len() >= 60, "{} codes read from 5.2", map.len());
        map
    }

    /// The constants are the design's table, code for code and status for
    /// status: a constant the table lacks, or a code of the table with no
    /// constant, fails. Decisive: an entry added to or dropped from
    /// `codes!`.
    #[test]
    fn the_codes_are_the_designs() {
        let design = design();
        for code in ErrorCode::ALL {
            assert_eq!(
                design.get(code.as_str()),
                Some(&code.status()),
                "`{}` is not the design's",
                code.as_str()
            );
        }
        for (code, status) in &design {
            assert_eq!(
                ErrorCode::of(code).map(ErrorCode::status),
                Some(*status),
                "`{code}` of the design has no constant"
            );
        }
    }

    /// `as_invalid` keeps what Meta said and whether it may have been sent,
    /// and answers `422 invalid_request` on the field.
    #[test]
    fn as_invalid_keeps_graph_and_the_flags() {
        let graph: GraphApiError = serde_json::from_value(serde_json::json!({
            "message": "(#100) Invalid parameter",
            "type": "OAuthException",
            "code": 100,
        }))
        .unwrap();
        let error = ServiceError::from_library(&Error::Api(Box::new(graph)))
            .with_may_have_been_sent(true)
            .retryable(true)
            .as_invalid("to");
        assert_eq!((error.code(), error.status()), ("invalid_request", 422));
        assert_eq!(error.field(), Some("to"));
        assert_eq!(error.graph().map(|g| g.code), Some(100));
        assert!(error.may_have_been_sent() && error.is_retryable());
    }
}
