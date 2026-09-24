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
    Unknown,
}

impl ErrorKind {
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
}
