//! Account, phone number and partner management fields.
//!
//! Doc paths: `webhooks/reference/account_alerts`,
//! `webhooks/reference/account_review_update`,
//! `webhooks/reference/account_update` (+ the `phone_number` and
//! `disconnection_info` variants in `embedded-signup/onboarding-business-app-users`
//! and `calling/call-settings`), `webhooks/reference/business_capability_update`,
//! `webhooks/reference/partner_solutions`,
//! `webhooks/reference/payment_configuration_update`,
//! `webhooks/reference/phone_number_name_update`,
//! `webhooks/reference/phone_number_quality_update`,
//! `webhooks/reference/security`, `business-scoped-user-ids`
//! (`business_username_updates`), `calling/call-settings`
//! (`account_settings_update`).
//!
//! `business_capability_update` documents `max_daily_conversations_per_business`
//! as an integer whose values are `TIER_250`…`TIER_UNLIMITED`, and its example
//! sends `2000`; [`MessagingLimit`] accepts both.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use wa_core::ids::{AppId, BusinessId, PhoneNumberId, WabaId};

use crate::open_enum::open_enum;

open_enum! {
    /// `account_alerts.entity_type`.
    pub enum AlertEntityType {
        /// A business portfolio.
        Business => "BUSINESS",
        /// A business phone number.
        PhoneNumber => "PHONE_NUMBER",
        /// A business phone number's business profile.
        CurrentStatusId => "CURRENT_STATUS_ID",
    }
}

open_enum! {
    /// `alert_info.alert_severity`.
    pub enum AlertSeverity {
        /// A rejection or denial.
        Critical => "CRITICAL",
        /// Informational; no action needed.
        Informational => "INFORMATIONAL",
        /// Action may be needed.
        Warning => "WARNING",
    }
}

open_enum! {
    /// `alert_info.alert_status`.
    pub enum AlertStatus {
        /// Active.
        Active => "ACTIVE",
        /// None.
        None => "NONE",
    }
}

open_enum! {
    /// `alert_info.alert_type`.
    pub enum AlertType {
        /// Not enough signal, identity verification rejected, or quality too low.
        IncreasedCapabilitiesEligibilityDeferred => "INCREASED_CAPABILITIES_ELIGIBILITY_DEFERRED",
        /// Limits cannot be increased due to past activity.
        IncreasedCapabilitiesEligibilityFailed => "INCREASED_CAPABILITIES_ELIGIBILITY_FAILED",
        /// More information needed before limits can be increased.
        IncreasedCapabilitiesEligibilityNeedMoreInfo => "INCREASED_CAPABILITIES_ELIGIBILITY_NEED_MORE_INFO",
        /// Official Business Account approved.
        ObaApproved => "OBA_APPROVED",
        /// Official Business Account denied.
        ObaRejected => "OBA_REJECTED",
        /// Business profile photo deleted.
        ProfilePictureLost => "PROFILE_PICTURE_LOST",
    }
}

/// `value` of an `account_alerts` change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountAlertsValue {
    /// What the alert is about.
    pub entity_type: AlertEntityType,
    /// Business portfolio id or business phone number id.
    pub entity_id: String,
    /// The alert.
    pub alert_info: AlertInfo,
}

/// `account_alerts.alert_info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertInfo {
    /// Severity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert_severity: Option<AlertSeverity>,
    /// Status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert_status: Option<AlertStatus>,
    /// Type.
    pub alert_type: AlertType,
    /// Human-readable description; may contain unfilled placeholders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert_description: Option<String>,
}

open_enum! {
    /// Review decision (`account_review_update.decision`,
    /// `phone_number_name_update.decision`).
    pub enum ReviewDecision {
        /// Approved.
        Approved => "APPROVED",
        /// Rejected.
        Rejected => "REJECTED",
        /// Still pending.
        Pending => "PENDING",
        /// Deferred.
        Deferred => "DEFERRED",
    }
}

/// `value` of an `account_review_update` change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountReviewUpdateValue {
    /// Outcome of the WABA policy review.
    pub decision: ReviewDecision,
}

open_enum! {
    /// `account_update.event`.
    pub enum AccountUpdateEvent {
        /// WABA deleted.
        AccountDeleted => "ACCOUNT_DELETED",
        /// WABA restricted; see [`AccountUpdateValue::restriction_info`].
        AccountRestriction => "ACCOUNT_RESTRICTION",
        /// WABA violated policies or terms; see [`AccountUpdateValue::violation_info`].
        AccountViolation => "ACCOUNT_VIOLATION",
        /// WABA onboarded to MM API for WhatsApp and shared its ad accounts.
        AdAccountLinked => "AD_ACCOUNT_LINKED",
        /// WABA eligible for authentication-international rates.
        AuthIntlPriceEligibilityUpdate => "AUTH_INTL_PRICE_ELIGIBILITY_UPDATE",
        /// Primary business location set.
        BusinessPrimaryLocationCountryUpdate => "BUSINESS_PRIMARY_LOCATION_COUNTRY_UPDATE",
        /// WABA disabled / reinstated; see [`AccountUpdateValue::ban_info`].
        DisabledUpdate => "DISABLED_UPDATE",
        /// MM API for WhatsApp terms accepted.
        MmLiteTermsSigned => "MM_LITE_TERMS_SIGNED",
        /// WABA shared with a Solution Partner.
        PartnerAdded => "PARTNER_ADDED",
        /// A business customer granted the app permissions.
        PartnerAppInstalled => "PARTNER_APP_INSTALLED",
        /// A business customer uninstalled the app.
        PartnerAppUninstalled => "PARTNER_APP_UNINSTALLED",
        /// A business customer who could not add a website finished
        /// Embedded Signup; verify their business
        /// (`embedded-signup/website-optional`).
        PartnerClientCertificationNeeded => "PARTNER_CLIENT_CERTIFICATION_NEEDED",
        /// Partner-led business verification status changed.
        PartnerClientCertificationStatusUpdate => "PARTNER_CLIENT_CERTIFICATION_STATUS_UPDATE",
        /// WABA unshared from a Solution Partner.
        PartnerRemoved => "PARTNER_REMOVED",
        /// Volume-based pricing tier changed.
        VolumeBasedPricingTierUpdate => "VOLUME_BASED_PRICING_TIER_UPDATE",
        /// WABA offboarded after a device change or re-registration.
        AccountOffboarded => "ACCOUNT_OFFBOARDED",
        /// WABA reconnected after offboarding.
        AccountReconnected => "ACCOUNT_RECONNECTED",
    }
}

/// `value` of an `account_update` change. Which optional part is present
/// depends on [`Self::event`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountUpdateValue {
    /// What happened.
    pub event: AccountUpdateEvent,
    /// Business phone number (coexistence and calling examples).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
    /// ISO 3166-1 alpha-2 country (`BUSINESS_PRIMARY_LOCATION_COUNTRY_UPDATE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// WABA and partner details (`AD_ACCOUNT_LINKED`, `MM_LITE_TERMS_SIGNED`, `PARTNER_*`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waba_info: Option<WabaInfo>,
    /// Violation (`ACCOUNT_VIOLATION`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub violation_info: Option<ViolationInfo>,
    /// Authentication-international eligibility (`AUTH_INTL_PRICE_ELIGIBILITY_UPDATE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_international_rate_eligibility: Option<AuthInternationalRateEligibility>,
    /// Ban state (`DISABLED_UPDATE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ban_info: Option<BanInfo>,
    /// Volume tier (`VOLUME_BASED_PRICING_TIER_UPDATE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_tier_info: Option<VolumeTierInfo>,
    /// Why a coexistence business was disconnected (`PARTNER_REMOVED`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disconnection_info: Option<DisconnectionInfo>,
    /// Partner-led business verification (`PARTNER_CLIENT_CERTIFICATION_STATUS_UPDATE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partner_client_certification_info: Option<PartnerClientCertificationInfo>,
    /// Whose business to verify (`PARTNER_CLIENT_CERTIFICATION_NEEDED`,
    /// `embedded-signup/website-optional`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partner_client_certification_needed_info: Option<PartnerClientCertificationNeededInfo>,
    /// Restrictions (`ACCOUNT_RESTRICTION`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restriction_info: Vec<RestrictionInfo>,
}

/// `account_update.waba_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WabaInfo {
    /// The WABA (the business customer's, for `PARTNER_*`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waba_id: Option<WabaId>,
    /// Owning business portfolio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_business_id: Option<BusinessId>,
    /// Ad account shared with the partner (`AD_ACCOUNT_LINKED`, as the
    /// `account_update` reference spells it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ad_account_linked: Option<String>,
    /// The same ad account as `marketing-messages/onboarding` spells it.
    /// A separate field rather than a serde alias: an alias would make a
    /// body carrying both spellings a duplicate-field error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ad_account_id: Option<String>,
    /// Partner app (`PARTNER_APP_INSTALLED`, `PARTNER_APP_UNINSTALLED`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partner_app_id: Option<AppId>,
    /// Multi-Partner Solution id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solution_id: Option<String>,
    /// Business portfolios of the solution's partners.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub solution_partner_business_ids: Vec<BusinessId>,
}

/// `account_update.violation_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViolationInfo {
    /// Violation type (`policy-enforcement-violations` lists them), e.g. `ADULT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub violation_type: Option<String>,
}

/// `account_update.auth_international_rate_eligibility`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthInternationalRateEligibility {
    /// Countries with their own start time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exception_countries: Vec<ExceptionCountry>,
    /// Start time for every other country.
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub start_time: Option<OffsetDateTime>,
}

/// `auth_international_rate_eligibility.exception_countries[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExceptionCountry {
    /// ISO 3166-1 alpha-2 country code.
    pub country_code: String,
    /// When rates start for that country.
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub start_time: Option<OffsetDateTime>,
}

open_enum! {
    /// `ban_info.waba_ban_state`.
    pub enum WabaBanState {
        /// Disabled.
        Disable => "DISABLE",
        /// Reinstated.
        Reinstate => "REINSTATE",
        /// Scheduled to be disabled.
        ScheduleForDisable => "SCHEDULE_FOR_DISABLE",
    }
}

/// `account_update.ban_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BanInfo {
    /// Ban state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waba_ban_state: Option<WabaBanState>,
    /// Ban date as Meta formats it (e.g. `April 17, 2025`), not a timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waba_ban_date: Option<String>,
}

/// `account_update.volume_tier_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeTierInfo {
    /// When the tier changed. Meta may send several webhooks for one switch;
    /// the one with the smallest time is authoritative (`pricing`).
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub tier_update_time: Option<OffsetDateTime>,
    /// Template category the tier applies to, e.g. `UTILITY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_category: Option<String>,
    /// Tier bounds, `min:max`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Month the tier applies to, `YYYY-MM`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_month: Option<String>,
    /// User country/region, e.g. `India`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

open_enum! {
    /// `disconnection_info.reason`.
    pub enum DisconnectionReason {
        /// Disconnected by enforcement or account deletion.
        AccountDisconnected => "ACCOUNT_DISCONNECTED",
        /// Number registered with the consumer WhatsApp app.
        BusinessDowngrade => "BUSINESS_DOWNGRADE",
        /// Number changed.
        ChangeNumber => "CHANGE_NUMBER",
        /// Companion device inactive ~30 days.
        CompanionInactivity => "COMPANION_INACTIVITY",
        /// Primary device inactive ~14 days.
        PrimaryInactivity => "PRIMARY_INACTIVITY",
        /// Re-registered on a new device.
        UserReRegistered => "USER_RE_REGISTERED",
    }
}

open_enum! {
    /// `disconnection_info.initiated_by`.
    pub enum DisconnectionInitiator {
        /// System-initiated.
        System => "SYSTEM",
        /// Client-initiated.
        User => "USER",
    }
}

/// `account_update.disconnection_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisconnectionInfo {
    /// Reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<DisconnectionReason>,
    /// Initiator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiated_by: Option<DisconnectionInitiator>,
}

open_enum! {
    /// `partner_client_certification_info.status`.
    pub enum CertificationStatus {
        /// Approved.
        Approved => "APPROVED",
        /// Discarded.
        Discarded => "DISCARDED",
        /// Rejected; see the rejection reasons.
        Failed => "FAILED",
        /// Pending review.
        Pending => "PENDING",
        /// Revoked.
        Revoked => "REVOKED",
    }
}

/// `account_update.partner_client_certification_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartnerClientCertificationInfo {
    /// The client's business portfolio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_business_id: Option<BusinessId>,
    /// Status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<CertificationStatus>,
    /// Rejection reasons, e.g. `LEGAL NAME NOT MATCHING`, or `NONE`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejection_reasons: Vec<String>,
}

/// `account_update.partner_client_certification_needed_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartnerClientCertificationNeededInfo {
    /// The business customer's portfolio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_business_id: Option<BusinessId>,
}

open_enum! {
    /// `restriction_info[].restriction_type`.
    pub enum RestrictionType {
        /// Cannot add phone numbers.
        RestrictedAddPhoneNumberAction => "RESTRICTED_ADD_PHONE_NUMBER_ACTION",
        /// Cannot make or receive calls.
        RestrictedBizInitiatedAndUserInitiatedCalling => "RESTRICTED_BIZ_INITIATED_AND_USER_INITIATED_CALLING",
        /// Cannot start conversations.
        RestrictedBizInitiatedMessaging => "RESTRICTED_BIZ_INITIATED_MESSAGING",
        /// Cannot place calls.
        RestrictedBusinessInitiatedCalling => "RESTRICTED_BUSINESS_INITIATED_CALLING",
        /// Cannot answer customer-initiated conversations.
        RestrictedCustomerInitiatedMessaging => "RESTRICTED_CUSTOMER_INITIATED_MESSAGING",
        /// Cannot send utility templates via Direct Send.
        RestrictedDirectSendUtilityTemplates => "RESTRICTED_DIRECT_SEND_UTILITY_TEMPLATES",
        /// Cannot receive calls.
        RestrictedUserInitiatedCalling => "RESTRICTED_USER_INITIATED_CALLING",
        /// Call button hidden for low pickup rates.
        RestrictedUserInitiatedCallingCallButtonHidden => "RESTRICTED_USER_INITIATED_CALLING_CALL_BUTTON_HIDDEN",
        /// Cannot create utility templates.
        RestrictedUtilityTemplates => "RESTRICTED_UTILITY_TEMPLATES",
    }
}

/// `account_update.restriction_info[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestrictionInfo {
    /// What is restricted.
    pub restriction_type: RestrictionType,
    /// When the restriction ends.
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub expiration: Option<OffsetDateTime>,
    /// Remediation steps, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

open_enum! {
    /// A messaging limit tier (`phone_number_quality_update`,
    /// `business_capability_update`).
    pub enum MessagingLimitTier {
        /// 50.
        Tier50 => "TIER_50",
        /// 250.
        Tier250 => "TIER_250",
        /// 2,000.
        Tier2K => "TIER_2K",
        /// 10,000.
        Tier10K => "TIER_10K",
        /// 100,000.
        Tier100K => "TIER_100K",
        /// Not used to send yet.
        TierNotSet => "TIER_NOT_SET",
        /// Unlimited / higher throughput.
        TierUnlimited => "TIER_UNLIMITED",
    }
}

/// A messaging limit sent as a tier name or as a number (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessagingLimit {
    /// A number, e.g. `2000`; `-1` means unlimited.
    Count(i64),
    /// A tier name, e.g. `TIER_2K`.
    Tier(MessagingLimitTier),
}

/// `value` of a `business_capability_update` change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessCapabilityUpdateValue {
    /// Deprecated (removed February 2026): portfolio messaging limit; `-1` = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_daily_conversation_per_phone: Option<i64>,
    /// Portfolio messaging limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_daily_conversations_per_business: Option<MessagingLimit>,
    /// Maximum phone numbers per business portfolio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_phone_numbers_per_business: Option<i64>,
    /// Maximum phone numbers per WABA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_phone_numbers_per_waba: Option<i64>,
}

open_enum! {
    /// `business_username_updates.status`.
    pub enum BusinessUsernameStatus {
        /// Visible to users.
        Approved => "approved",
        /// Deleted via the WhatsApp Business app.
        Deleted => "deleted",
        /// Reserved, not yet visible.
        Reserved => "reserved",
    }
}

/// `value` of a `business_username_updates` change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessUsernameUpdateValue {
    /// Business display phone number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_phone_number: Option<String>,
    /// The username; omitted when `status` is `deleted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// New status.
    pub status: BusinessUsernameStatus,
}

open_enum! {
    /// `partner_solutions.event`.
    pub enum PartnerSolutionEvent {
        /// Solution saved as draft or sent to a partner.
        SolutionCreated => "SOLUTION_CREATED",
        /// Solution updated.
        SolutionUpdated => "SOLUTION_UPDATED",
    }
}

open_enum! {
    /// `partner_solutions.solution_status`.
    pub enum PartnerSolutionStatus {
        /// Accepted by the partner; usable.
        Active => "ACTIVE",
        /// Deactivated.
        Deactivated => "DEACTIVATED",
        /// Drafted, not sent.
        Draft => "DRAFT",
        /// Sent, not answered.
        Initiated => "INITIATED",
        /// Deactivation requested.
        PendingDeactivation => "PENDING_DEACTIVATION",
        /// Rejected by the partner.
        Rejected => "REJECTED",
    }
}

/// `value` of a `partner_solutions` change. The entry id of this field is a
/// **business portfolio** id, not a WABA id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartnerSolutionsValue {
    /// What happened.
    pub event: PartnerSolutionEvent,
    /// Solution id.
    pub solution_id: String,
    /// Solution status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solution_status: Option<PartnerSolutionStatus>,
}

open_enum! {
    /// `payment_configuration_update.status`.
    pub enum PaymentConfigurationStatus {
        /// Tested and usable.
        Active => "Active",
        /// Disconnected from the gateway.
        NeedsConnecting => "Needs Connecting",
        /// Connected, not yet tested.
        NeedsTesting => "Needs Testing",
    }
}

/// `value` of a `payment_configuration_update` change (Payments API
/// India / Brazil).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentConfigurationUpdateValue {
    /// Configuration name used in order details messages.
    pub configuration_name: String,
    /// Gateway: `billdesk`, `payu`, `razorpay`, `zaakpay`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    /// Gateway merchant account id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_mid: Option<String>,
    /// Status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PaymentConfigurationStatus>,
    /// Created at.
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_timestamp: Option<OffsetDateTime>,
    /// Updated at.
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_timestamp: Option<OffsetDateTime>,
}

open_enum! {
    /// `phone_number_name_update.rejection_reason`.
    pub enum NameRejectionReason {
        /// Contains a person's name or employee identifier.
        NameEmployeeIssue => "NAME_EMPLOYEE_ISSUE",
        /// Contains an unrelated business's name.
        NameEndclientNotrelated => "NAME_ENDCLIENT_NOTRELATED",
        /// Unacceptable format.
        NameFormatUnacceptable => "NAME_FORMAT_UNACCEPTABLE",
        /// Contains a person's name or employee identifier.
        NameIndividualIssue => "NAME_INDIVIDUAL_ISSUE",
        /// Inconsistent with the business's branding.
        NameNotConsistent => "NAME_NOT_CONSISTENT",
        /// Unknown reason; contact support.
        Unknown => "UNKNOWN",
    }
}

/// `value` of a `phone_number_name_update` change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneNumberNameUpdateValue {
    /// Business display phone number.
    pub display_phone_number: String,
    /// Outcome of display name verification.
    pub decision: ReviewDecision,
    /// The display name that was reviewed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_verified_name: Option<String>,
    /// Why it was rejected; `null` when accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<NameRejectionReason>,
}

open_enum! {
    /// `phone_number_quality_update.event`.
    pub enum PhoneNumberQualityEvent {
        /// Still registering.
        Onboarding => "ONBOARDING",
        /// Throughput increased.
        ThroughputUpgrade => "THROUGHPUT_UPGRADE",
    }
}

/// `value` of a `phone_number_quality_update` change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneNumberQualityUpdateValue {
    /// Business display phone number.
    pub display_phone_number: String,
    /// What changed.
    pub event: PhoneNumberQualityEvent,
    /// Deprecated (removed February 2026): old limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_limit: Option<MessagingLimitTier>,
    /// Deprecated (removed February 2026): current limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_limit: Option<MessagingLimitTier>,
    /// Portfolio messaging limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_daily_conversations_per_business: Option<MessagingLimit>,
}

open_enum! {
    /// `security.event`.
    pub enum SecurityEvent {
        /// Two-step verification PIN changed or enabled in WhatsApp Manager.
        PinChanged => "PIN_CHANGED",
        /// Someone asked to turn off two-step verification.
        PinResetRequest => "PIN_RESET_REQUEST",
        /// Two-step verification turned off via the reset email.
        PinRequestSuccess => "PIN_REQUEST_SUCCESS",
    }
}

/// `value` of a `security` change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityValue {
    /// Business display phone number.
    pub display_phone_number: String,
    /// What happened.
    pub event: SecurityEvent,
    /// Meta Business Suite user who requested a PIN reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester: Option<String>,
}

/// `value` of an `account_settings_update` change (`calling/call-settings`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSettingsUpdateValue {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// When the settings changed.
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub timestamp: Option<OffsetDateTime>,
    /// Kind of change; the page names `PHONE_NUMBER_SETTINGS` as the only value.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub settings_type: Option<String>,
    /// The phone number settings that changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number_settings: Option<PhoneNumberSettings>,
}

/// `account_settings_update.phone_number_settings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneNumberSettings {
    /// The phone number whose settings changed.
    pub phone_number_id: PhoneNumberId,
    /// Calling settings, in the shape of the Get calling settings API. Kept
    /// untyped: that shape belongs to the calling settings endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calling: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messaging_limit_accepts_numbers_and_tiers() {
        let n: MessagingLimit = serde_json::from_str("2000").unwrap();
        let t: MessagingLimit = serde_json::from_str(r#""TIER_2K""#).unwrap();
        let o: MessagingLimit = serde_json::from_str(r#""TIER_1M""#).unwrap();
        assert_eq!(n, MessagingLimit::Count(2000));
        assert_eq!(t, MessagingLimit::Tier(MessagingLimitTier::Tier2K));
        assert_eq!(
            o,
            MessagingLimit::Tier(MessagingLimitTier::Other("TIER_1M".into()))
        );
    }
}
