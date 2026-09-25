//! Template lifecycle fields.
//!
//! Doc paths: `webhooks/reference/message_template_components_update`,
//! `webhooks/reference/message_template_quality_update`,
//! `webhooks/reference/message_template_status_update`,
//! `webhooks/reference/template_category_update`,
//! `direct-send/integrity-and-content-guidelines`
//! (`template_correct_category_detection`), `templates/template-pausing`.
//!
//! Template ids arrive as JSON numbers here; they are kept as
//! [`TemplateId`] strings like everywhere else in `meta-whatsapp-rs`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use meta_whatsapp_core::ids::TemplateId;

use crate::open_enum::open_enum;

open_enum! {
    /// Template category.
    pub enum TemplateCategory {
        /// Marketing.
        Marketing => "MARKETING",
        /// Utility.
        Utility => "UTILITY",
        /// Authentication.
        Authentication => "AUTHENTICATION",
    }
}

open_enum! {
    /// `message_template_components_update.message_template_buttons[].message_template_button_type`.
    pub enum TemplateButtonType {
        /// Catalog.
        Catalog => "CATALOG",
        /// Copy code.
        CopyCode => "COPY_CODE",
        /// Extension.
        Extension => "EXTENSION",
        /// Flow.
        Flow => "FLOW",
        /// Multi-product message.
        Mpm => "MPM",
        /// Order details.
        OrderDetails => "ORDER_DETAILS",
        /// One-time password.
        Otp => "OTP",
        /// Phone number.
        PhoneNumber => "PHONE_NUMBER",
        /// Postback.
        Postback => "POSTBACK",
        /// Reminder.
        Reminder => "REMINDER",
        /// Send location.
        SendLocation => "SEND_LOCATION",
        /// Single-product message.
        Spm => "SPM",
        /// Quick reply.
        QuickReply => "QUICK_REPLY",
        /// URL.
        Url => "URL",
        /// Voice call.
        VoiceCall => "VOICE_CALL",
    }
}

/// `value` of a `message_template_components_update` change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateComponentsUpdateValue {
    /// Template id.
    #[serde(deserialize_with = "crate::serde_ext::id::deserialize")]
    pub message_template_id: TemplateId,
    /// Template name.
    pub message_template_name: String,
    /// Language and locale, e.g. `en_US`.
    pub message_template_language: String,
    /// Body text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_element: Option<String>,
    /// Text header, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_title: Option<String>,
    /// Footer, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_footer: Option<String>,
    /// URL and phone number buttons.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_template_buttons: Vec<TemplateButton>,
}

/// `message_template_buttons[]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateButton {
    /// Button type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_button_type: Option<TemplateButtonType>,
    /// Label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_button_text: Option<String>,
    /// URL (URL buttons).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_button_url: Option<String>,
    /// Phone number (phone number buttons).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_button_phone_number: Option<String>,
}

open_enum! {
    /// Template quality score (`templates/template-quality`), as the
    /// `message_template_quality_update` webhook sends it.
    ///
    /// # The Graph API's type
    ///
    /// Templates and phone numbers read through the Graph API carry the
    /// same score as `meta_whatsapp_client::common::QualityRating`. The two types stay
    /// separate (the owner's decision, 2026-09-25: meta-whatsapp-webhooks and
    /// meta-whatsapp-client do not depend on each other); they correspond by wire
    /// value ([`Self::as_str`], and `as_str` on the client's type):
    ///
    /// | Wire | `TemplateQualityScore` | `QualityRating` |
    /// | --- | --- | --- |
    /// | `GREEN`, `YELLOW`, `RED` | `Green`, `Yellow`, `Red` | `Green`, `Yellow`, `Red` |
    /// | `UNKNOWN` (not rated yet) | `Unknown` | `Unknown` |
    /// | `NA` | `Other("NA")`: this webhook documents no `NA` | `NotApplicable` (in the phone number examples) |
    /// | anything else | `Other`, verbatim | `Other`, verbatim |
    ///
    /// **Case**: this type matches values exactly (`"green"` is
    /// `Other("green")`), the client's type case-insensitively (`"green"` is
    /// `Green`). Convert through the wire value, into the client's type:
    /// `score.as_str().parse::<QualityRating>()` (infallible) maps each
    /// value here to its variant there, `Other("NA")` and lower-case
    /// spellings included. The other way, [`From<&str>`](Self::from) of
    /// `QualityRating::as_str` turns `NotApplicable` into `Other("NA")`.
    pub enum TemplateQualityScore {
        /// High quality.
        Green => "GREEN",
        /// Medium quality.
        Yellow => "YELLOW",
        /// Low quality.
        Red => "RED",
        /// Pending.
        Unknown => "UNKNOWN",
    }
}

/// `value` of a `message_template_quality_update` change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateQualityUpdateValue {
    /// Previous score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_quality_score: Option<TemplateQualityScore>,
    /// New score.
    pub new_quality_score: TemplateQualityScore,
    /// Template id.
    #[serde(deserialize_with = "crate::serde_ext::id::deserialize")]
    pub message_template_id: TemplateId,
    /// Template name.
    pub message_template_name: String,
    /// Language and locale.
    pub message_template_language: String,
}

open_enum! {
    /// `message_template_status_update.event`.
    pub enum TemplateStatusEvent {
        /// Approved; can be sent.
        Approved => "APPROVED",
        /// Archived for inactivity; deleted after 28 days unless unarchived.
        Archived => "ARCHIVED",
        /// Unarchived.
        Unarchived => "UNARCHIVED",
        /// Deleted.
        Deleted => "DELETED",
        /// Disabled for user feedback.
        Disabled => "DISABLED",
        /// Negative feedback; at risk of being disabled.
        Flagged => "FLAGGED",
        /// In appeal.
        InAppeal => "IN_APPEAL",
        /// WABA at its template limit.
        LimitExceeded => "LIMIT_EXCEEDED",
        /// Locked; cannot be edited.
        Locked => "LOCKED",
        /// Paused (`templates/template-pausing`).
        Paused => "PAUSED",
        /// Under review.
        Pending => "PENDING",
        /// No longer flagged or disabled.
        Reinstated => "REINSTATED",
        /// Deleted via WhatsApp Manager.
        PendingDeletion => "PENDING_DELETION",
        /// Rejected.
        Rejected => "REJECTED",
    }
}

open_enum! {
    /// `message_template_status_update.reason`.
    pub enum TemplateRejectionReason {
        /// Policy violation.
        AbusiveContent => "ABUSIVE_CONTENT",
        /// Deprecated: authentication template for an unsupported region.
        CategoryNotAvailable => "CATEGORY_NOT_AVAILABLE",
        /// Content does not match the category.
        IncorrectCategory => "INCORRECT_CATEGORY",
        /// Invalid format; see `rejection_info`.
        InvalidFormat => "INVALID_FORMAT",
        /// No reason (e.g. paused, approved).
        None => "NONE",
        /// Policy violation.
        Promotional => "PROMOTIONAL",
        /// Policy violation.
        Scam => "SCAM",
        /// Content does not match the category.
        TagContentMismatch => "TAG_CONTENT_MISMATCH",
    }
}

open_enum! {
    /// `other_info.title`: pause and lock events.
    pub enum TemplatePauseTitle {
        /// First pause.
        FirstPause => "FIRST_PAUSE",
        /// Second pause.
        SecondPause => "SECOND_PAUSE",
        /// Paused for rate limiting.
        RateLimitingPause => "RATE_LIMITING_PAUSE",
        /// Unpaused.
        Unpause => "UNPAUSE",
        /// Disabled.
        Disabled => "DISABLED",
    }
}

/// `value` of a `message_template_status_update` change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateStatusUpdateValue {
    /// New status.
    pub event: TemplateStatusEvent,
    /// Template id.
    #[serde(deserialize_with = "crate::serde_ext::id::deserialize")]
    pub message_template_id: TemplateId,
    /// Template name.
    pub message_template_name: String,
    /// Language and locale.
    pub message_template_language: String,
    /// Reason; `null` when scheduled for deletion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<TemplateRejectionReason>,
    /// Category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_category: Option<TemplateCategory>,
    /// Disable details (disabled templates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_info: Option<TemplateDisableInfo>,
    /// Pause / lock details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_info: Option<TemplateOtherInfo>,
    /// Rejection details (`INVALID_FORMAT`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_info: Option<TemplateRejectionInfo>,
}

/// `message_template_status_update.disable_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateDisableInfo {
    /// When the template was disabled.
    #[serde(
        default,
        with = "meta_whatsapp_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_date: Option<OffsetDateTime>,
}

/// `message_template_status_update.other_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateOtherInfo {
    /// Pause / unpause kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<TemplatePauseTitle>,
    /// Why the template was locked or unlocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `message_template_status_update.rejection_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateRejectionInfo {
    /// What is wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// How to fix it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<String>,
}

/// `value` of a `template_category_update` change.
///
/// Two shapes: an impending recategorization (`correct_category` +
/// `category_update_timestamp`, where `new_category` is the *current*
/// category) and a completed one (`previous_category` + `new_category`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateCategoryUpdateValue {
    /// Template id.
    #[serde(deserialize_with = "crate::serde_ext::id::deserialize")]
    pub message_template_id: TemplateId,
    /// Template name.
    pub message_template_name: String,
    /// Language and locale.
    pub message_template_language: String,
    /// Previous category (completed changes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_category: Option<TemplateCategory>,
    /// New category; the current one for impending changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_category: Option<TemplateCategory>,
    /// Category it will be moved to in 24 hours (impending changes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correct_category: Option<TemplateCategory>,
    /// When the impending change takes effect.
    #[serde(
        default,
        with = "meta_whatsapp_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub category_update_timestamp: Option<OffsetDateTime>,
}

impl TemplateCategoryUpdateValue {
    /// Whether this announces a future change rather than a completed one.
    pub fn is_impending(&self) -> bool {
        self.correct_category.is_some()
    }
}

/// `value` of a `template_correct_category_detection` change: Direct Send
/// content detected in the wrong category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateCorrectCategoryDetectionValue {
    /// Flagged template id.
    #[serde(deserialize_with = "crate::serde_ext::id::deserialize")]
    pub message_template_id: TemplateId,
    /// Template name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_name: Option<String>,
    /// Language and locale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_language: Option<String>,
    /// Category it was sent as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<TemplateCategory>,
    /// Category Meta detected (`MARKETING` or `AUTHENTICATION`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correct_category: Option<TemplateCategory>,
}
