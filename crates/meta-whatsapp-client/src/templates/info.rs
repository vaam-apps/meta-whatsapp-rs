//! Response and query types of the template management endpoints.

use std::collections::BTreeMap;

use meta_whatsapp_core::ids::TemplateId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::definition::{Button, SupportedApp, TemplateComponent};
use super::macros::string_enum;
use super::types::{
    DisplayFormat, ParameterFormat, QualityRating, RejectionReason, TemplateCategory,
    TemplateSource, TemplateStatus, TemplateSubCategory,
};

/// A template as returned by `GET /{waba}/message_templates` and
/// `GET /{template_id}` (`MessageTemplate` in
/// `reference/whatsapp-business-account/message-template-api`).
///
/// Only `id` is always present: Meta returns just the fields asked for with
/// `fields=…` (plus `id`), so everything else is optional rather than
/// defaulted — an absent status is not an empty one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateInfo {
    /// Template id.
    pub id: TemplateId,
    /// Name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TemplateStatus>,
    /// Category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<TemplateCategory>,
    /// Language code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Components. Authentication templates' OTP buttons come back as `URL`
    /// buttons.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "null_as_empty"
    )]
    pub components: Vec<TemplateComponent>,
    /// Placeholder style.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_format: Option<ParameterFormat>,
    /// Sub-category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_category: Option<TemplateSubCategory>,
    /// Quality rating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_score: Option<QualityScore>,
    /// Why the template was rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_reason: Option<RejectionReason>,
    /// Category before an automatic category update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_category: Option<TemplateCategory>,
    /// Category the template will be moved to on the next automatic update.
    /// Meta sends an empty string or `null` when the template is unaffected
    /// (`templates/template-categorization`); both parse as `None`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "empty_as_none"
    )]
    pub correct_category: Option<TemplateCategory>,
    /// Message time-to-live in seconds; cleared (`null`) by an automatic
    /// category update (`templates/time-to-live`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_send_ttl_seconds: Option<i64>,
    /// Set when the template was created from the Template Library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_template_name: Option<String>,
    /// How the template was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<TemplateSource>,
    /// Health status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_status: Option<HealthStatus>,
    /// Display format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_format: Option<DisplayFormat>,
    /// CTA URL link tracking opt-out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cta_url_link_tracking_opted_out: Option<bool>,
    /// Primary-device-only delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_primary_device_delivery_only: Option<bool>,
    /// SMS fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_sms_fallback_enabled: Option<bool>,
    /// Last update, UNIX seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated_time: Option<i64>,
    /// Click-to-WhatsApp ad account id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ad_account_id: Option<String>,
    /// Ad set id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ad_adset_id: Option<String>,
    /// Campaign id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ad_campaign_id: Option<String>,
    /// Ad id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ad_id: Option<String>,
    /// Marketing bid specification, as returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid_spec: Option<Value>,
    /// Marketing creative degrees-of-freedom specification, as returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degrees_of_freedom_spec: Option<Value>,
}

/// Parse a `null` list as empty instead of failing the whole response. Not
/// seen in the docs' examples; defensive, like every response type here.
fn null_as_empty<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<Vec<T>>::deserialize(deserializer).map(Option::unwrap_or_default)
}

fn empty_as_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: std::str::FromStr<Err = std::convert::Infallible>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw
        .filter(|s| !s.trim().is_empty())
        .and_then(|s| s.parse().ok()))
}

/// `quality_score` (`templates/template-quality`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityScore {
    /// Rating (optional in the reference schema, present in every example).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<QualityRating>,
    /// When it was last updated, UNIX seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<i64>,
    /// Reason for the rating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Reasons for the rating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasons: Option<Vec<String>>,
}

/// `health_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthStatus {
    /// Whether messages can be sent (a string in the reference schema).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_send_message: Option<String>,
}

/// Response of a template creation (`CreateTemplateResponse`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateCreated {
    /// New template id.
    pub id: TemplateId,
    /// Initial status (often `PENDING`; library templates come back
    /// `APPROVED`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TemplateStatus>,
    /// Category actually assigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<TemplateCategory>,
}

/// Filters of `GET /{waba}/message_templates`.
///
/// Documented parameters only: `fields`, `limit`, `after`, `before`
/// (reference), `status` (`templates/template-management`), `name`
/// (`templates/template-library`, `direct-send/faq`), `source` and
/// `correct_category` (`direct-send/view-generated-templates`,
/// `direct-send/faq`). Filters by category, language or content are not
/// documented and therefore not offered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemplateListQuery {
    /// Fields to return (`id` always comes back). Empty: Meta's defaults.
    pub fields: Vec<String>,
    /// Page size (minimum 1).
    pub limit: Option<u32>,
    /// Only templates with this name.
    pub name: Option<String>,
    /// Only templates with this status.
    pub status: Option<TemplateStatus>,
    /// Only templates from this source.
    pub source: Option<TemplateSource>,
    /// Only templates whose `correct_category` is this.
    pub correct_category: Option<TemplateCategory>,
    /// Cursor from a previous page's `paging.cursors.after`
    /// ([`Page::next_cursor`](meta_whatsapp_core::paging::Page::next_cursor)); for
    /// `list` only, `list_stream` refuses it (it manages its own).
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`; for `list`
    /// only, like `after`.
    pub before: Option<String>,
}

impl TemplateListQuery {
    /// No filters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for these fields.
    #[must_use]
    pub fn fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.fields = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Page size.
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Filter by name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Filter by status.
    #[must_use]
    pub fn status(mut self, status: TemplateStatus) -> Self {
        self.status = Some(status);
        self
    }
}

/// Filters of `GET /message_template_library` (`templates/template-library`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryQuery {
    /// Substring of the content, name, header, body or footer.
    pub search: Option<String>,
    /// Topic, e.g. `ORDER_MANAGEMENT`, `PAYMENTS`, `ACCOUNT_UPDATE`,
    /// `CUSTOMER_FEEDBACK`.
    pub topic: Option<String>,
    /// Use case, e.g. `SHIPMENT_CONFIRMATION`, `DELIVERY_FAILED`.
    pub usecase: Option<String>,
    /// Industry: `E_COMMERCE` or `FINANCIAL_SERVICES`.
    pub industry: Option<String>,
    /// Language code.
    pub language: Option<String>,
    /// Exact library template name.
    pub name: Option<String>,
}

/// A Template Library entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryTemplate {
    /// Library template id.
    pub id: String,
    /// Library template name (pass it as `library_template_name`).
    pub name: String,
    /// Language code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<TemplateCategory>,
    /// Topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Use case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usecase: Option<String>,
    /// Industries.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "null_as_empty"
    )]
    pub industry: Vec<String>,
    /// Header text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// Body text with positional placeholders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Example body values.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "null_as_empty"
    )]
    pub body_params: Vec<String>,
    /// Types of the body values (`TEXT`, `AMOUNT`, `DATE`, …), checked by
    /// Meta at send time.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "null_as_empty"
    )]
    pub body_param_types: Vec<String>,
    /// Buttons.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "null_as_empty"
    )]
    pub buttons: Vec<Button>,
}

/// Create a template from the Template Library
/// (`POST /{waba}/message_templates` with `library_template_name`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryTemplateRequest {
    /// Your template's name (same rules as any template name).
    pub name: String,
    /// Language code.
    pub language: String,
    /// Category. The page says it must be `UTILITY`, yet also describes
    /// authentication library templates; not checked locally.
    pub category: TemplateCategory,
    /// Exact library template name.
    pub library_template_name: String,
    /// Website / phone inputs; required for library templates with buttons.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub library_template_button_inputs: Vec<LibraryButtonInput>,
    /// Optional add-ons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_template_body_inputs: Option<LibraryBodyInputs>,
}

string_enum! {
    /// `type` of a library button input.
    pub enum LibraryButtonType {
        /// Quick reply.
        QuickReply => "QUICK_REPLY",
        /// URL.
        Url => "URL",
        /// Phone number.
        PhoneNumber => "PHONE_NUMBER",
        /// OTP.
        Otp => "OTP",
        /// Multi-product.
        Mpm => "MPM",
        /// Catalog.
        Catalog => "CATALOG",
        /// Flow.
        Flow => "FLOW",
        /// Voice call.
        VoiceCall => "VOICE_CALL",
        /// App.
        App => "APP",
    }
}

/// One `library_template_button_inputs` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryButtonInput {
    /// Button type.
    #[serde(rename = "type")]
    pub kind: LibraryButtonType,
    /// Phone number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
    /// URL with an optional `{{1}}` suffix placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<LibraryUrlInput>,
    /// Zero-tap terms accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zero_tap_terms_accepted: Option<bool>,
    /// OTP type (`COPY_CODE`, `ONE_TAP`, `ZERO_TAP`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otp_type: Option<String>,
    /// Apps for one-tap / zero-tap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_apps: Option<Vec<SupportedApp>>,
}

impl LibraryButtonInput {
    /// A URL input.
    pub fn url(base_url: impl Into<String>, url_suffix_example: Option<String>) -> Self {
        Self {
            kind: LibraryButtonType::Url,
            phone_number: None,
            url: Some(LibraryUrlInput {
                base_url: base_url.into(),
                url_suffix_example,
            }),
            zero_tap_terms_accepted: None,
            otp_type: None,
            supported_apps: None,
        }
    }

    /// A phone-number input.
    pub fn phone_number(phone_number: impl Into<String>) -> Self {
        Self {
            kind: LibraryButtonType::PhoneNumber,
            phone_number: Some(phone_number.into()),
            url: None,
            zero_tap_terms_accepted: None,
            otp_type: None,
            supported_apps: None,
        }
    }
}

/// `url` of a library button input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryUrlInput {
    /// Base URL, e.g. `https://www.example.com/{{1}}`.
    pub base_url: String,
    /// Example of the full URL with the suffix filled in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_suffix_example: Option<String>,
}

/// `library_template_body_inputs`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryBodyInputs {
    /// Mention contacting the business by phone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_contact_number: Option<bool>,
    /// Add a learn-more link (not widely available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_learn_more_link: Option<bool>,
    /// Add "do not share this code".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_security_recommendation: Option<bool>,
    /// Add a package-tracking link (not widely available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_track_package_link: Option<bool>,
    /// Code expiry note, in minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_expiration_minutes: Option<u32>,
}

/// Options of `POST /{destination_waba}/migrate_message_templates`
/// (`templates/template-migration`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationOptions {
    /// Zero-based batch of 500 templates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_number: Option<u32>,
    /// Batch size override, at most 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// Only these templates (e.g. retrying failures), at most 500.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub template_ids: Vec<TemplateId>,
}

/// Result of a migration batch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationResult {
    /// Ids of templates recreated in the destination.
    #[serde(default)]
    pub migrated_templates: Vec<TemplateId>,
    /// Source template id → failure reason.
    #[serde(default)]
    pub failed_templates: BTreeMap<TemplateId, String>,
}

string_enum! {
    /// Metric of a template comparison.
    pub enum ComparisonMetricKind {
        /// Templates ordered by block rate (blocks / sends), lowest first.
        BlockRate => "BLOCK_RATE",
        /// Number of sends per template.
        MessageSends => "MESSAGE_SENDS",
        /// Top block reason per template.
        TopBlockReason => "TOP_BLOCK_REASON",
    }
}

/// One metric of `GET /{template_id}/compare` (`templates/template-comparison`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonMetric {
    /// The metric.
    pub metric: ComparisonMetricKind,
    /// Value shape: `RELATIVE`, `NUMBER_VALUES` or `STRING_VALUES`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// `BLOCK_RATE`: template ids by increasing block rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_by_relative_metric: Option<Vec<TemplateId>>,
    /// `MESSAGE_SENDS`: sends per template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_values: Option<Vec<ComparisonValue<i64>>>,
    /// `TOP_BLOCK_REASON`: reason per template (`NO_LONGER_NEEDED`, `SPAM`,
    /// `UNKNOWN_BLOCK_REASON`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub string_values: Option<Vec<ComparisonValue<String>>>,
}

/// `{key: template id, value}` of a comparison metric.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonValue<T> {
    /// Template id.
    pub key: TemplateId,
    /// Value.
    pub value: T,
}
