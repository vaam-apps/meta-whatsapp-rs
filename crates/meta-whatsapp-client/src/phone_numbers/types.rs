//! Response types for business phone numbers.
//!
//! Every enum here is open (`meta_whatsapp_core::open_enum!`): Meta adds
//! values (messaging limit tiers are being reworked, v4 Embedded Signup
//! added statuses), and a new value must never fail a whole `GET`, so it
//! is kept verbatim in the enum's `Other(String)` and serializes back
//! unchanged. `QualityRating::Unknown` is Meta's documented `UNKNOWN`, a
//! value like any other (roadmap L20a).

use meta_whatsapp_core::ids::PhoneNumberId;
use meta_whatsapp_core::open_enum;
use serde::{Deserialize, Serialize};

use super::official::OfficialBusinessAccount;
use crate::common::HealthStatus;

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
    /// Messaging health (`fields=health_status`, `support/health-status`);
    /// also read by [`PhoneNumber::health_status`](super::PhoneNumber::health_status).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_status: Option<HealthStatus>,
    /// Official Business Account status
    /// (`fields=official_business_account`); also read by
    /// [`PhoneNumber::official_business_account`](super::PhoneNumber::official_business_account).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_business_account: Option<OfficialBusinessAccount>,
}

/// Quality rating of a business phone number; the same type template
/// quality scores use (see [`crate::common`]). A value Meta adds later is
/// kept in `QualityRating::Other`.
pub use crate::common::QualityRating;

open_enum! {
    /// Whether the number's ownership was verified with a code.
    pub enum CodeVerificationStatus {
        /// Verified.
        Verified => "VERIFIED",
        /// `NOT_VERIFIED` (the phone-number-management reference).
        NotVerified => "NOT_VERIFIED",
        /// `UNVERIFIED` (the phone number reference).
        Unverified => "UNVERIFIED",
        /// The verification expired (pre-verified numbers revert after 90 days).
        Expired => "EXPIRED",
    }
}

open_enum! {
    /// Display name review status (`name_status`, `new_name_status`).
    pub enum NameStatus {
        /// Approved.
        Approved => "APPROVED",
        /// Usable without review.
        AvailableWithoutReview => "AVAILABLE_WITHOUT_REVIEW",
        /// Rejected.
        Declined => "DECLINED",
        /// Certificate expired; cannot be used to register.
        Expired => "EXPIRED",
        /// Under review.
        PendingReview => "PENDING_REVIEW",
        /// No certificate; cannot be used to register.
        None => "NONE",
    }
}

open_enum! {
    /// Connection status of a business phone number.
    pub enum PhoneNumberStatus {
        /// Banned.
        Banned => "BANNED",
        /// Connected: can send and receive.
        Connected => "CONNECTED",
        /// Deleted.
        Deleted => "DELETED",
        /// Disconnected.
        Disconnected => "DISCONNECTED",
        /// Flagged for low quality.
        Flagged => "FLAGGED",
        /// Migrated.
        Migrated => "MIGRATED",
        /// Pending.
        Pending => "PENDING",
        /// Rate limited.
        RateLimited => "RATE_LIMITED",
        /// Restricted.
        Restricted => "RESTRICTED",
    }
}

open_enum! {
    /// Hosting platform (`platform_type`, `host_platform`).
    pub enum PlatformType {
        /// Registered for Cloud API.
        CloudApi => "CLOUD_API",
        /// On-Premises API (retired).
        OnPremise => "ON_PREMISE",
        /// Not registered.
        NotApplicable => "NOT_APPLICABLE",
    }
}

open_enum! {
    /// Messaging limit tier.
    pub enum MessagingLimitTier {
        /// 50 users per 24h.
        Tier50 => "TIER_50",
        /// 250 users per 24h.
        Tier250 => "TIER_250",
        /// 1,000 users per 24h.
        Tier1K => "TIER_1K",
        /// 10,000 users per 24h.
        Tier10K => "TIER_10K",
        /// 100,000 users per 24h.
        Tier100K => "TIER_100K",
        /// Unlimited.
        TierUnlimited => "TIER_UNLIMITED",
    }
}

open_enum! {
    /// Account mode of a number.
    pub enum AccountMode {
        /// Unverified trial experience.
        Sandbox => "SANDBOX",
        /// Live.
        Live => "LIVE",
    }
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

open_enum! {
    #![case_insensitive]
    /// Whether the number shows up when WhatsApp users search for businesses
    /// (`search_visibility` of `POST /{Phone-Number-ID}`,
    /// `WAAPIBusinessGlobalSearchStateStatus`).
    pub enum SearchVisibility {
        /// Shown in search results.
        Visible => "VISIBLE",
        /// Hidden from search results.
        Hidden => "HIDDEN",
    }
}

open_enum! {
    #![case_insensitive]
    /// `connection_status` of `POST /{Phone-Number-ID}`
    /// (`WhatsAppBusinessAccountToNumberStatus`, "Connection status between
    /// WhatsApp Business Account and phone number"). The reference lists
    /// the values and says nothing of which ones a business may set, or
    /// what setting one does. Reading a number's status is
    /// [`PhoneNumberInfo::status`], a different, older list.
    pub enum ConnectionStatus {
        /// `CONNECTED`.
        Connected => "CONNECTED",
        /// `DISCONNECTED`.
        Disconnected => "DISCONNECTED",
        /// `PENDING`.
        Pending => "PENDING",
        /// `FLAGGED`.
        Flagged => "FLAGGED",
        /// `RESTRICTED`.
        Restricted => "RESTRICTED",
        /// `RATE_LIMITED`.
        RateLimited => "RATE_LIMITED",
        /// `MESSAGING_LIMIT_TIER_0`.
        MessagingLimitTier0 => "MESSAGING_LIMIT_TIER_0",
        /// `MESSAGING_LIMIT_TIER_1`.
        MessagingLimitTier1 => "MESSAGING_LIMIT_TIER_1",
        /// `MESSAGING_LIMIT_TIER_2`.
        MessagingLimitTier2 => "MESSAGING_LIMIT_TIER_2",
        /// `MESSAGING_LIMIT_TIER_3`.
        MessagingLimitTier3 => "MESSAGING_LIMIT_TIER_3",
        /// `MESSAGING_LIMIT_TIER_4`.
        MessagingLimitTier4 => "MESSAGING_LIMIT_TIER_4",
    }
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
            Some(CodeVerificationStatus::Other("SOMETHING_NEW".into()))
        );
        assert_eq!(p.quality_rating, Some(QualityRating::Unknown));
        assert_eq!(
            p.messaging_limit_tier,
            Some(MessagingLimitTier::Other("TIER_2K".into()))
        );
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
    fn connection_status_writes_every_documented_value() {
        // reference/whatsapp-business-phone-number/whatsapp-business-account-phone-number-api,
        // `WhatsAppBusinessAccountToNumberStatus`.
        let all = [
            (ConnectionStatus::Connected, "CONNECTED"),
            (ConnectionStatus::Disconnected, "DISCONNECTED"),
            (ConnectionStatus::Pending, "PENDING"),
            (ConnectionStatus::Flagged, "FLAGGED"),
            (ConnectionStatus::Restricted, "RESTRICTED"),
            (ConnectionStatus::RateLimited, "RATE_LIMITED"),
            (
                ConnectionStatus::MessagingLimitTier0,
                "MESSAGING_LIMIT_TIER_0",
            ),
            (
                ConnectionStatus::MessagingLimitTier1,
                "MESSAGING_LIMIT_TIER_1",
            ),
            (
                ConnectionStatus::MessagingLimitTier2,
                "MESSAGING_LIMIT_TIER_2",
            ),
            (
                ConnectionStatus::MessagingLimitTier3,
                "MESSAGING_LIMIT_TIER_3",
            ),
            (
                ConnectionStatus::MessagingLimitTier4,
                "MESSAGING_LIMIT_TIER_4",
            ),
        ];
        for (status, wire) in all {
            assert_eq!(serde_json::to_value(&status).unwrap(), wire);
            let back: ConnectionStatus = serde_json::from_value(wire.into()).unwrap();
            assert_eq!(back, status);
        }
        let other: ConnectionStatus = serde_json::from_str("\"MESSAGING_LIMIT_TIER_5\"").unwrap();
        assert_eq!(
            other,
            ConnectionStatus::Other("MESSAGING_LIMIT_TIER_5".into())
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
