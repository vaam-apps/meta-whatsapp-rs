//! Response types for business phone numbers.
//!
//! Every enum here has a catch-all (`Unknown`): Meta adds values (messaging
//! limit tiers are being reworked, v4 Embedded Signup added statuses), and a
//! new value must never fail a whole `GET`.

use serde::{Deserialize, Serialize};
use wa_core::ids::PhoneNumberId;

/// A WhatsApp Business phone number, as returned by `GET /{PHONE_NUMBER_ID}`
/// and by the WABA's `phone_numbers` edge.
///
/// Every field but `id` is optional: which ones come back depends on the
/// `fields` you ask for (Meta's default is `id`, `display_phone_number`,
/// `verified_name`, `quality_rating`), and several are beta.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct PhoneNumberInfo {
    /// Phone number id.
    pub id: PhoneNumberId,
    /// The number as WhatsApp displays it, e.g. `+1 631-555-5555` or
    /// `15555555555` (Meta's examples use both shapes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_phone_number: Option<String>,
    /// Approved display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_name: Option<String>,
    /// Quality rating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_rating: Option<QualityRating>,
    /// Whether ownership of the number has been verified by code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_verification_status: Option<CodeVerificationStatus>,
    /// Display name review status (beta field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_status: Option<NameStatus>,
    /// Display name requested with `request_display_name_change`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_display_name: Option<String>,
    /// Review status of `new_display_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_name_status: Option<NameStatus>,
    /// Connection status; must be `CONNECTED` to send and receive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PhoneNumberStatus>,
    /// Where the number is hosted (`CLOUD_API` once registered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_type: Option<PlatformType>,
    /// Same values as `platform_type`, on the WABA `phone_numbers` edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_platform: Option<PlatformType>,
    /// Throughput level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput: Option<Throughput>,
    /// Messaging limit tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_limit_tier: Option<MessagingLimitTier>,
    /// Official Business Account (blue check) status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_official_business_account: Option<bool>,
    /// `true` when the number is also in use with the WhatsApp Business app
    /// (coexistence). With `platform_type == CLOUD_API` it means both work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_on_biz_app: Option<bool>,
    /// `SANDBOX` (unverified trial) or `LIVE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_mode: Option<AccountMode>,
    /// ISO 3166-1 alpha-2 country of the number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
    /// Country dialing code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country_dial_code: Option<String>,
    /// Combined business/name certification status, as Meta reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_cert_status: Option<String>,
    /// When Embedded Signup last onboarded the number. Kept as the raw
    /// string: `solution-providers/manage-phone-numbers` shows
    /// `2023-08-22T19:05:53+0000` while `embedded-signup/hosted-es` calls it a
    /// unix timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_onboarded_time: Option<String>,
    /// Where webhooks for this number go (see `webhooks/override`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_configuration: Option<WebhookConfiguration>,
}

/// Quality rating of a business phone number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum QualityRating {
    /// High quality.
    Green,
    /// Medium quality.
    Yellow,
    /// Low quality.
    Red,
    /// Not determined yet (new numbers).
    #[serde(rename = "NA")]
    NotApplicable,
    /// `UNKNOWN`, or a value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

/// Whether the number's ownership was verified with a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum CodeVerificationStatus {
    /// Verified.
    Verified,
    /// `NOT_VERIFIED` (the phone-number-management reference).
    NotVerified,
    /// `UNVERIFIED` (the phone number reference).
    Unverified,
    /// The verification expired (pre-verified numbers revert after 90 days).
    Expired,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

/// Display name review status (`name_status`, `new_name_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum NameStatus {
    /// Approved.
    Approved,
    /// Usable without review.
    AvailableWithoutReview,
    /// Rejected.
    Declined,
    /// Certificate expired; cannot be used to register.
    Expired,
    /// Under review.
    PendingReview,
    /// No certificate; cannot be used to register.
    None,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

/// Connection status of a business phone number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum PhoneNumberStatus {
    /// Banned.
    Banned,
    /// Connected: can send and receive.
    Connected,
    /// Deleted.
    Deleted,
    /// Disconnected.
    Disconnected,
    /// Flagged for low quality.
    Flagged,
    /// Migrated.
    Migrated,
    /// Pending.
    Pending,
    /// Rate limited.
    RateLimited,
    /// Restricted.
    Restricted,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

/// Hosting platform (`platform_type`, `host_platform`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum PlatformType {
    /// Registered for Cloud API.
    CloudApi,
    /// On-Premises API (retired).
    OnPremise,
    /// Not registered.
    NotApplicable,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

/// Messaging limit tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[non_exhaustive]
pub enum MessagingLimitTier {
    /// 50 users per 24h.
    #[serde(rename = "TIER_50")]
    Tier50,
    /// 250 users per 24h.
    #[serde(rename = "TIER_250")]
    Tier250,
    /// 1,000 users per 24h.
    #[serde(rename = "TIER_1K")]
    Tier1K,
    /// 10,000 users per 24h.
    #[serde(rename = "TIER_10K")]
    Tier10K,
    /// 100,000 users per 24h.
    #[serde(rename = "TIER_100K")]
    Tier100K,
    /// Unlimited.
    #[serde(rename = "TIER_UNLIMITED")]
    TierUnlimited,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

/// Account mode of a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum AccountMode {
    /// Unverified trial experience.
    Sandbox,
    /// Live.
    Live,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

/// `throughput` object.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct Throughput {
    /// Throughput level, as Meta reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

/// `webhook_configuration` object: the callback URL that applies at each
/// level. Only the levels that have one are present.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct WebhookConfiguration {
    /// Override set on this phone number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
    /// Override set on the number's WABA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_business_account: Option<String>,
    /// The app's callback URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<String>,
}

/// `{"id": "..."}` returned when a phone number is created on a WABA.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CreatedPhoneNumber {
    /// The new (unverified) phone number id.
    pub id: PhoneNumberId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_single_number_example() {
        // business-phone-numbers/phone-numbers, "Get a single phone number".
        let v = r#"{
          "code_verification_status" : "VERIFIED",
          "display_phone_number" : "15555555555",
          "id" : "105954558954427",
          "quality_rating" : "GREEN",
          "verified_name" : "Support Number"
        }"#;
        let p: PhoneNumberInfo = serde_json::from_str(v).unwrap();
        assert_eq!(p.id.as_str(), "105954558954427");
        assert_eq!(
            p.code_verification_status,
            Some(CodeVerificationStatus::Verified)
        );
        assert_eq!(p.quality_rating, Some(QualityRating::Green));
        assert_eq!(p.verified_name.as_deref(), Some("Support Number"));
    }

    #[test]
    fn parses_coexistence_status_example() {
        // embedded-signup/onboarding-business-app-users, "Check onboarding status".
        let v = r#"{"is_on_biz_app": true, "platform_type": "CLOUD_API", "id": "106540352242922"}"#;
        let p: PhoneNumberInfo = serde_json::from_str(v).unwrap();
        assert_eq!(p.is_on_biz_app, Some(true));
        assert_eq!(p.platform_type, Some(PlatformType::CloudApi));
    }

    #[test]
    fn parses_hosted_es_listing_shape_and_unknown_values() {
        let v = r#"{
          "verified_name": "Jasper's Market",
          "code_verification_status": "SOMETHING_NEW",
          "display_phone_number": "+1 631-555-5555",
          "quality_rating": "UNKNOWN",
          "platform_type": "CLOUD_API",
          "throughput": {"level": "STANDARD"},
          "last_onboarded_time": "2023-08-22T19:05:53+0000",
          "webhook_configuration": {"application": "https://example.com/webhook"},
          "messaging_limit_tier": "TIER_2K",
          "status": "CONNECTED",
          "name_status": "AVAILABLE_WITHOUT_REVIEW",
          "brand_new_field": {"x": 1},
          "id": "1906385232743451"
        }"#;
        let p: PhoneNumberInfo = serde_json::from_str(v).unwrap();
        assert_eq!(
            p.code_verification_status,
            Some(CodeVerificationStatus::Unknown)
        );
        assert_eq!(p.quality_rating, Some(QualityRating::Unknown));
        assert_eq!(p.messaging_limit_tier, Some(MessagingLimitTier::Unknown));
        assert_eq!(p.status, Some(PhoneNumberStatus::Connected));
        assert_eq!(p.name_status, Some(NameStatus::AvailableWithoutReview));
        assert_eq!(
            p.throughput.and_then(|t| t.level).as_deref(),
            Some("STANDARD")
        );
        assert_eq!(
            p.webhook_configuration
                .and_then(|w| w.application)
                .as_deref(),
            Some("https://example.com/webhook")
        );
    }

    #[test]
    fn quality_na_is_not_applicable() {
        let q: QualityRating = serde_json::from_str("\"NA\"").unwrap();
        assert_eq!(q, QualityRating::NotApplicable);
        let t: MessagingLimitTier = serde_json::from_str("\"TIER_1K\"").unwrap();
        assert_eq!(t, MessagingLimitTier::Tier1K);
    }
}
