//! Graph API error objects and their classification.
//!
//! Source: <https://developers.facebook.com/documentation/business-messaging/whatsapp/support/error-codes>.
//! Meta's guidance is to branch on `code` (and read `error_data.details`),
//! never on the HTTP status, the subcode (gone since v16) or the title text.
//! [`ErrorKind`] is that branching, done once.

use serde::{Deserialize, Serialize};

/// `{"error": {...}}` as returned by the Graph API.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GraphErrorEnvelope {
    /// The error object.
    pub error: GraphApiError,
}

/// The `error_data` object of a WhatsApp error, when it is an object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ErrorData {
    /// Always `whatsapp` for Cloud API errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// Most specific human-readable reason. Branch on `code`, show `details`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// A Graph API error, as found under `error` in a response body or in the
/// `errors` arrays of a webhook.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, thiserror::Error)]
#[error("Graph API error #{code}: {}{}", self.summary(), self.details().map(|d| format!(" ({d})")).unwrap_or_default())]
pub struct GraphApiError {
    /// Error code. The only field to branch on.
    pub code: i64,
    /// `"(#code) Title"`. For display only.
    #[serde(default)]
    pub message: String,
    /// Webhook-delivered errors carry a `title` next to (or instead of)
    /// `message`. For display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Graph error type, e.g. `OAuthException`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    /// Deprecated since v16; kept because older fixtures and some non-WhatsApp
    /// Graph endpoints (e.g. `/oauth/access_token`) still send it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_subcode: Option<i64>,
    /// Object with `details`; occasionally a bare string on non-WhatsApp
    /// endpoints, hence untyped. Read it through [`GraphApiError::details`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_data: Option<serde_json::Value>,
    /// User-facing title, when Meta provides one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_user_title: Option<String>,
    /// User-facing message, when Meta provides one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_user_msg: Option<String>,
    /// Graph's own hint that retrying may succeed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_transient: Option<bool>,
    /// Trace id for Meta Direct Support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fbtrace_id: Option<String>,
    /// Webhook errors carry a documentation link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// HTTP status of the response that carried this error. Not part of the
    /// body; set by the client. `None` for webhook-delivered errors.
    #[serde(skip)]
    pub http_status: Option<u16>,
}

impl GraphApiError {
    /// Minimal constructor, mostly for tests and fixtures.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            title: None,
            error_type: None,
            error_subcode: None,
            error_data: None,
            error_user_title: None,
            error_user_msg: None,
            is_transient: None,
            fbtrace_id: None,
            href: None,
            http_status: None,
        }
    }

    /// `message`, falling back to `title` when `message` is empty.
    pub fn summary(&self) -> &str {
        if self.message.is_empty() {
            self.title.as_deref().unwrap_or_default()
        } else {
            &self.message
        }
    }

    /// `error_data.details`, or `error_data` itself when it is a string.
    pub fn details(&self) -> Option<&str> {
        match self.error_data.as_ref()? {
            serde_json::Value::String(s) => Some(s),
            serde_json::Value::Object(map) => map.get("details")?.as_str(),
            _ => None,
        }
    }

    /// Classify [`Self::code`].
    pub fn kind(&self) -> ErrorKind {
        ErrorKind::from_code(self.code)
    }

    /// Whether repeating the same request later may succeed.
    pub fn is_retryable(&self) -> bool {
        self.is_transient == Some(true) || self.kind().is_retryable()
    }
}

/// What a Graph error code means for the caller.
///
/// `#[non_exhaustive]`: Meta adds codes; unknown ones map to
/// [`ErrorKind::Unknown`] rather than failing to parse.
///
/// Each kind has a stable name, [`ErrorKind::as_str`], and
/// [`ErrorKind::ALL`] lists every kind, for code that must decide something
/// for each one (an HTTP service mapping kinds to statuses, a metrics
/// label set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    // ── Access ────────────────────────────────────────────────────────────
    /// `0`, `190`: token expired, invalidated or missing. Get a new token.
    Authentication,
    /// `3`, `10`, `200`–`299`, `131005`: permission not granted or removed,
    /// or the system user lacks asset access to the WABA.
    Permission,

    // ── Throttling ────────────────────────────────────────────────────────
    /// `4` (app), `80007` (WABA), `130429` (throughput): back off and
    /// retry.
    RateLimited,
    /// `131048`: sending restricted after too many messages were flagged as
    /// spam. Retrying does not help; improve quality.
    SpamRateLimited,
    /// `131056`: too many messages to the *same* recipient in a short time.
    /// Retry later; other recipients are unaffected.
    PairRateLimited,
    /// `131064`: messaging limit reached due to template classification
    /// violations.
    ClassificationLimitReached,

    // ── Integrity / policy ────────────────────────────────────────────────
    /// `368`, `131031`: account restricted or disabled for a policy violation.
    AccountRestricted,
    /// `130497`: restricted from messaging users in this country.
    CountryRestricted,

    // ── Request shape ─────────────────────────────────────────────────────
    /// `100`, `131008`, `131009`, `131021`, `135000`, `33`, and the In-App
    /// Signup request errors `2494166`–`2494179`: missing, misspelled or
    /// invalid parameter.
    InvalidParameter,
    /// `131051`: message type not supported.
    UnsupportedMessageType,

    // ── Delivery to the user ──────────────────────────────────────────────
    /// `131047`: more than 24h since the user last messaged. Send a template.
    CustomerServiceWindowClosed,
    /// `131049`: not delivered "to maintain healthy ecosystem engagement"
    /// (per-user marketing limits). Do not retry for at least 24h.
    EcosystemEngagementLimit,
    /// `131050`: the user stopped marketing messages from this business.
    /// Never retry; record the opt-out.
    MarketingOptedOut,
    /// `131063`, `131055`, `134100`: marketing disabled on this API, or only
    /// marketing templates allowed on this API (MM API for WhatsApp).
    MarketingNotAllowed,
    /// `131026`: undeliverable (not on WhatsApp, old client, ToS pending…).
    Undeliverable,
    /// `130403`: this business blocked the user. Unblock first.
    BlockedByBusiness,
    /// `130472`: held back as part of a Meta experiment.
    ExperimentHoldout,
    /// `131062`: this message cannot go to a business-scoped user id (e.g. a
    /// marketing template with `bid_spec`, or an authentication template).
    /// Address the user by phone number instead.
    RecipientNotSupported,

    // ── Media ─────────────────────────────────────────────────────────────
    /// `131052`: media sent by the user could not be downloaded.
    MediaDownloadFailed,
    /// `131053`: media in the message could not be uploaded.
    MediaUploadFailed,

    // ── Templates ─────────────────────────────────────────────────────────
    /// `132000`, `132012`, `132018`: parameter count or format mismatch.
    TemplateParameterMismatch,
    /// `132001`: template missing in that language, or not approved.
    TemplateNotFound,
    /// `132005`: hydrated text too long.
    TemplateTextTooLong,
    /// `132007`: content violates policy.
    TemplatePolicyViolation,
    /// `132015`: paused for low quality.
    TemplatePaused,
    /// `132016`: paused too often; permanently disabled.
    TemplateDisabled,
    /// `134101`: template still syncing to the MM API (up to ~10 minutes).
    TemplateSyncing,
    /// `134102`: template unavailable on the MM API (eligibility).
    TemplateUnavailable,
    /// `2388019`: WABA template limit reached.
    TemplateLimitReached,
    /// `2388039`, `2388040`, `2388047`, `2388072`, `2388073`, `2388293`,
    /// `2388299`: template creation rejected for its content/format.
    TemplateRejected,

    // ── Flows ─────────────────────────────────────────────────────────────
    /// `132068` (blocked), `132069` (throttled).
    FlowUnavailable,

    // ── Phone number lifecycle ────────────────────────────────────────────
    /// `131045`, `133000`, `133006`, `133010`, `133015`, `131037`, `136024`:
    /// number not registered/verified (or already verified), deregistration
    /// pending, display name not approved. `133016`: too many
    /// (de)registration attempts — Meta locks the number for 72 h, so this is
    /// deliberately *not* [`ErrorKind::RateLimited`] (no automatic retry).
    Registration,
    /// `133005`, `133008`, `133009`: two-step verification PIN wrong, or
    /// guessed too often/fast.
    TwoStepVerification,
    /// `2593107`, `2593108`, `2593109`: coexistence contact/history sync not
    /// allowed (already done, outside the 24h window after onboarding, or
    /// history sharing turned off by the business).
    SyncNotAllowed,
    /// `1752041`: onboarding request already made by a partner.
    DuplicateOnboarding,

    // ── Resources ─────────────────────────────────────────────────────────
    /// `2494164`: the referenced object (e.g. an In-App Signup) does not
    /// exist.
    NotFound,
    /// `2494165`: the API is not enabled for this account (e.g. In-App
    /// Signup on this WABA).
    FeatureNotAvailable,

    // ── Billing ───────────────────────────────────────────────────────────
    /// `131042`, `134011`: payment method or payments ToS problem.
    Payment,

    // ── Meta side ─────────────────────────────────────────────────────────
    /// `1`, `2`, `131000`, `131016`, `131057`, `133004`, `2494100`: Meta is
    /// down, overloaded, or the account is in maintenance mode.
    ServiceUnavailable,

    /// Anything not listed above.
    ///
    /// Stays the last variant: [`ErrorKind::ALL`] is checked against its
    /// position, so a kind added before it and missing from the list fails
    /// the build.
    Unknown,
}

// `ALL` lists every kind exactly once, in declaration order: entry `i` has
// discriminant `i`, and there are as many entries as `Unknown`, the last
// variant, has predecessors plus one. A kind added before `Unknown` but not
// to `ALL` fails this at compile time; the unit test checks each position.
const _: () = assert!(ErrorKind::ALL.len() == ErrorKind::Unknown as usize + 1);

impl ErrorKind {
    /// Every kind exactly once, [`ErrorKind::Unknown`] last. Iterate it;
    /// do not rely on a kind's position in it.
    ///
    /// `ErrorKind` is `#[non_exhaustive]`, so this list grows when Meta
    /// documents a code worth its own kind: iterate it, rather than naming
    /// kinds, wherever each kind needs a decision (a test that every kind
    /// maps to an HTTP status, say), and a new kind fails that test instead
    /// of falling silently into a catch-all arm.
    pub const ALL: &'static [ErrorKind] = &[
        Self::Authentication,
        Self::Permission,
        Self::RateLimited,
        Self::SpamRateLimited,
        Self::PairRateLimited,
        Self::ClassificationLimitReached,
        Self::AccountRestricted,
        Self::CountryRestricted,
        Self::InvalidParameter,
        Self::UnsupportedMessageType,
        Self::CustomerServiceWindowClosed,
        Self::EcosystemEngagementLimit,
        Self::MarketingOptedOut,
        Self::MarketingNotAllowed,
        Self::Undeliverable,
        Self::BlockedByBusiness,
        Self::ExperimentHoldout,
        Self::RecipientNotSupported,
        Self::MediaDownloadFailed,
        Self::MediaUploadFailed,
        Self::TemplateParameterMismatch,
        Self::TemplateNotFound,
        Self::TemplateTextTooLong,
        Self::TemplatePolicyViolation,
        Self::TemplatePaused,
        Self::TemplateDisabled,
        Self::TemplateSyncing,
        Self::TemplateUnavailable,
        Self::TemplateLimitReached,
        Self::TemplateRejected,
        Self::FlowUnavailable,
        Self::Registration,
        Self::TwoStepVerification,
        Self::SyncNotAllowed,
        Self::DuplicateOnboarding,
        Self::NotFound,
        Self::FeatureNotAvailable,
        Self::Payment,
        Self::ServiceUnavailable,
        Self::Unknown,
    ];

    /// The kind's stable name: the variant's name in `snake_case`
    /// (`TemplateParameterMismatch` → `"template_parameter_mismatch"`).
    ///
    /// Stable: an error code callers may store, compare or show (an HTTP
    /// API's `error.code`, a metrics label). A name, once given, never
    /// changes, even if its variant is renamed; a new kind adds a new
    /// name. The unit test pins every one.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authentication => "authentication",
            Self::Permission => "permission",
            Self::RateLimited => "rate_limited",
            Self::SpamRateLimited => "spam_rate_limited",
            Self::PairRateLimited => "pair_rate_limited",
            Self::ClassificationLimitReached => "classification_limit_reached",
            Self::AccountRestricted => "account_restricted",
            Self::CountryRestricted => "country_restricted",
            Self::InvalidParameter => "invalid_parameter",
            Self::UnsupportedMessageType => "unsupported_message_type",
            Self::CustomerServiceWindowClosed => "customer_service_window_closed",
            Self::EcosystemEngagementLimit => "ecosystem_engagement_limit",
            Self::MarketingOptedOut => "marketing_opted_out",
            Self::MarketingNotAllowed => "marketing_not_allowed",
            Self::Undeliverable => "undeliverable",
            Self::BlockedByBusiness => "blocked_by_business",
            Self::ExperimentHoldout => "experiment_holdout",
            Self::RecipientNotSupported => "recipient_not_supported",
            Self::MediaDownloadFailed => "media_download_failed",
            Self::MediaUploadFailed => "media_upload_failed",
            Self::TemplateParameterMismatch => "template_parameter_mismatch",
            Self::TemplateNotFound => "template_not_found",
            Self::TemplateTextTooLong => "template_text_too_long",
            Self::TemplatePolicyViolation => "template_policy_violation",
            Self::TemplatePaused => "template_paused",
            Self::TemplateDisabled => "template_disabled",
            Self::TemplateSyncing => "template_syncing",
            Self::TemplateUnavailable => "template_unavailable",
            Self::TemplateLimitReached => "template_limit_reached",
            Self::TemplateRejected => "template_rejected",
            Self::FlowUnavailable => "flow_unavailable",
            Self::Registration => "registration",
            Self::TwoStepVerification => "two_step_verification",
            Self::SyncNotAllowed => "sync_not_allowed",
            Self::DuplicateOnboarding => "duplicate_onboarding",
            Self::NotFound => "not_found",
            Self::FeatureNotAvailable => "feature_not_available",
            Self::Payment => "payment",
            Self::ServiceUnavailable => "service_unavailable",
            Self::Unknown => "unknown",
        }
    }

    /// Classify a Graph error code.
    pub fn from_code(code: i64) -> Self {
        match code {
            0 | 190 => Self::Authentication,
            3 | 10 | 200..=299 | 131005 => Self::Permission,
            4 | 80007 | 130429 => Self::RateLimited,
            131048 => Self::SpamRateLimited,
            131056 => Self::PairRateLimited,
            131064 => Self::ClassificationLimitReached,
            368 | 131031 => Self::AccountRestricted,
            130497 => Self::CountryRestricted,
            // 2494166–2494179: In-App Signup request errors (unknown/missing
            // placeholder, ToS not/already accepted, ToS URL not allowed,
            // non-https website URL).
            33
            | 100
            | 131008
            | 131009
            | 131021
            | 135000
            | 2494166..=2494168
            | 2494176
            | 2494177
            | 2494179 => Self::InvalidParameter,
            131051 => Self::UnsupportedMessageType,
            131047 => Self::CustomerServiceWindowClosed,
            131049 => Self::EcosystemEngagementLimit,
            131050 => Self::MarketingOptedOut,
            131055 | 131063 | 134100 => Self::MarketingNotAllowed,
            131026 => Self::Undeliverable,
            130403 => Self::BlockedByBusiness,
            130472 => Self::ExperimentHoldout,
            131062 => Self::RecipientNotSupported,
            131052 => Self::MediaDownloadFailed,
            131053 => Self::MediaUploadFailed,
            132000 | 132012 | 132018 => Self::TemplateParameterMismatch,
            132001 => Self::TemplateNotFound,
            132005 => Self::TemplateTextTooLong,
            132007 => Self::TemplatePolicyViolation,
            132015 => Self::TemplatePaused,
            132016 => Self::TemplateDisabled,
            134101 => Self::TemplateSyncing,
            134102 => Self::TemplateUnavailable,
            2388019 => Self::TemplateLimitReached,
            2388039 | 2388040 | 2388047 | 2388072 | 2388073 | 2388293 | 2388299 => {
                Self::TemplateRejected
            }
            132068 | 132069 => Self::FlowUnavailable,
            131037 | 131045 | 133000 | 133006 | 133010 | 133015 | 133016 | 136024 => {
                Self::Registration
            }
            133005 | 133008 | 133009 => Self::TwoStepVerification,
            2593107..=2593109 => Self::SyncNotAllowed,
            1752041 => Self::DuplicateOnboarding,
            2494164 => Self::NotFound,
            2494165 => Self::FeatureNotAvailable,
            131042 | 134011 => Self::Payment,
            1 | 2 | 131000 | 131016 | 131057 | 133004 | 2494100 => Self::ServiceUnavailable,
            _ => Self::Unknown,
        }
    }

    /// Whether an automatic retry (with backoff) is reasonable.
    ///
    /// Deliberately `false` for [`Self::EcosystemEngagementLimit`] and
    /// [`Self::SpamRateLimited`]: Meta documents that retrying those makes
    /// things worse.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::RateLimited
                | Self::PairRateLimited
                | Self::ServiceUnavailable
                | Self::TemplateSyncing
        )
    }

    /// Whether the error proves the request was rejected *before* any side
    /// effect, so replaying even a non-idempotent send cannot duplicate it.
    pub fn is_rejected_before_processing(self) -> bool {
        matches!(self, Self::RateLimited | Self::PairRateLimited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_documented_example() {
        // Example response from the error-codes page.
        let body = r#"{
          "error": {
            "message": "(#130429) Rate limit hit",
            "type": "OAuthException",
            "code": 130429,
            "error_data": {
                "messaging_product": "whatsapp",
                "details": "Cloud API message throughput has been reached."
            },
            "error_subcode": 2494055,
            "fbtrace_id": "Az8or2yhqkZfEZ-_4Qn_Bam"
          }
        }"#;
        let env: GraphErrorEnvelope = serde_json::from_str(body).unwrap();
        let e = env.error;
        assert_eq!(e.code, 130429);
        assert_eq!(e.kind(), ErrorKind::RateLimited);
        assert!(e.is_retryable());
        assert_eq!(
            e.details(),
            Some("Cloud API message throughput has been reached.")
        );
        assert_eq!(e.error_type.as_deref(), Some("OAuthException"));
        assert!(e.to_string().contains("#130429"));
    }

    #[test]
    fn webhook_error_shape_with_title_parses() {
        // Webhook `errors[]` entries use `title` rather than `message`.
        let v = r#"{"code":131047,"title":"Re-engagement message","message":"Re-engagement message","error_data":{"details":"Message failed to send because more than 24 hours have passed since the customer last replied to this number."},"href":"https://developers.facebook.com/docs/whatsapp/cloud-api/support/error-codes/"}"#;
        let e: GraphApiError = serde_json::from_str(v).unwrap();
        assert_eq!(e.kind(), ErrorKind::CustomerServiceWindowClosed);
        assert!(!e.is_retryable());
    }

    #[test]
    fn string_error_data_is_readable() {
        let v = r#"{"code":100,"message":"bad","error_data":"plain string"}"#;
        let e: GraphApiError = serde_json::from_str(v).unwrap();
        assert_eq!(e.details(), Some("plain string"));
    }

    #[test]
    fn transient_flag_overrides_kind() {
        let mut e = GraphApiError::new(100, "x");
        assert!(!e.is_retryable());
        e.is_transient = Some(true);
        assert!(e.is_retryable());
    }

    #[test]
    fn classification_table_spot_checks() {
        use ErrorKind as K;
        for (code, kind) in [
            (0, K::Authentication),
            (190, K::Authentication),
            (250, K::Permission),
            (131005, K::Permission),
            (131048, K::SpamRateLimited),
            (131049, K::EcosystemEngagementLimit),
            (131050, K::MarketingOptedOut),
            (131062, K::RecipientNotSupported),
            (132001, K::TemplateNotFound),
            (132015, K::TemplatePaused),
            (133005, K::TwoStepVerification),
            (133016, K::Registration),
            (136024, K::Registration),
            (2593109, K::SyncNotAllowed),
            (2494164, K::NotFound),
            (2494165, K::FeatureNotAvailable),
            (2494177, K::InvalidParameter),
            (2494100, K::ServiceUnavailable),
            (999999999, K::Unknown),
        ] {
            assert_eq!(ErrorKind::from_code(code), kind, "code {code}");
        }
        assert!(!K::EcosystemEngagementLimit.is_retryable());
        assert!(!K::SpamRateLimited.is_retryable());
        assert!(K::PairRateLimited.is_rejected_before_processing());
        assert!(!K::Registration.is_retryable(), "133016 locks for 72h");
        assert!(!K::ServiceUnavailable.is_rejected_before_processing());
    }

    /// Every kind's name, as callers may have stored it. A row never
    /// changes; a new kind adds one (the test fails until it does).
    const PINNED_NAMES: &[(ErrorKind, &str)] = &[
        (ErrorKind::Authentication, "authentication"),
        (ErrorKind::Permission, "permission"),
        (ErrorKind::RateLimited, "rate_limited"),
        (ErrorKind::SpamRateLimited, "spam_rate_limited"),
        (ErrorKind::PairRateLimited, "pair_rate_limited"),
        (
            ErrorKind::ClassificationLimitReached,
            "classification_limit_reached",
        ),
        (ErrorKind::AccountRestricted, "account_restricted"),
        (ErrorKind::CountryRestricted, "country_restricted"),
        (ErrorKind::InvalidParameter, "invalid_parameter"),
        (
            ErrorKind::UnsupportedMessageType,
            "unsupported_message_type",
        ),
        (
            ErrorKind::CustomerServiceWindowClosed,
            "customer_service_window_closed",
        ),
        (
            ErrorKind::EcosystemEngagementLimit,
            "ecosystem_engagement_limit",
        ),
        (ErrorKind::MarketingOptedOut, "marketing_opted_out"),
        (ErrorKind::MarketingNotAllowed, "marketing_not_allowed"),
        (ErrorKind::Undeliverable, "undeliverable"),
        (ErrorKind::BlockedByBusiness, "blocked_by_business"),
        (ErrorKind::ExperimentHoldout, "experiment_holdout"),
        (ErrorKind::RecipientNotSupported, "recipient_not_supported"),
        (ErrorKind::MediaDownloadFailed, "media_download_failed"),
        (ErrorKind::MediaUploadFailed, "media_upload_failed"),
        (
            ErrorKind::TemplateParameterMismatch,
            "template_parameter_mismatch",
        ),
        (ErrorKind::TemplateNotFound, "template_not_found"),
        (ErrorKind::TemplateTextTooLong, "template_text_too_long"),
        (
            ErrorKind::TemplatePolicyViolation,
            "template_policy_violation",
        ),
        (ErrorKind::TemplatePaused, "template_paused"),
        (ErrorKind::TemplateDisabled, "template_disabled"),
        (ErrorKind::TemplateSyncing, "template_syncing"),
        (ErrorKind::TemplateUnavailable, "template_unavailable"),
        (ErrorKind::TemplateLimitReached, "template_limit_reached"),
        (ErrorKind::TemplateRejected, "template_rejected"),
        (ErrorKind::FlowUnavailable, "flow_unavailable"),
        (ErrorKind::Registration, "registration"),
        (ErrorKind::TwoStepVerification, "two_step_verification"),
        (ErrorKind::SyncNotAllowed, "sync_not_allowed"),
        (ErrorKind::DuplicateOnboarding, "duplicate_onboarding"),
        (ErrorKind::NotFound, "not_found"),
        (ErrorKind::FeatureNotAvailable, "feature_not_available"),
        (ErrorKind::Payment, "payment"),
        (ErrorKind::ServiceUnavailable, "service_unavailable"),
        (ErrorKind::Unknown, "unknown"),
    ];

    /// `ALL` holds every kind once, in declaration order, and every kind in
    /// it has its pinned, unique, `snake_case` name.
    #[test]
    fn every_kind_is_listed_once_with_its_pinned_name() {
        for (i, kind) in ErrorKind::ALL.iter().enumerate() {
            assert_eq!(*kind as usize, i, "ALL[{i}] is {kind:?}: out of order");
        }
        assert_eq!(ErrorKind::ALL.last(), Some(&ErrorKind::Unknown));
        let listed: Vec<(ErrorKind, &str)> =
            ErrorKind::ALL.iter().map(|k| (*k, k.as_str())).collect();
        assert_eq!(listed, PINNED_NAMES);

        let mut names = std::collections::HashSet::new();
        for kind in ErrorKind::ALL {
            let name = kind.as_str();
            assert!(names.insert(name), "{name} names two kinds");
            assert!(
                !name.is_empty()
                    && !name.starts_with('_')
                    && !name.ends_with('_')
                    && !name.contains("__")
                    && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{name} is not snake_case"
            );
        }
        // Every kind `from_code` returns is listed (a spot check that the
        // classification and the list agree).
        for code in [0, 3, 4, 100, 131047, 132001, 133005, 2494164, 1, 7] {
            assert!(
                ErrorKind::ALL.contains(&ErrorKind::from_code(code)),
                "{code}"
            );
        }
    }
}
