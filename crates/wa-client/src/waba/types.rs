//! Request and response types of the WABA and business endpoints.

use std::fmt;

use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use wa_core::error::ValidationError;
use wa_core::ids::{BusinessId, WabaId};
use wa_core::secret::VerifyToken;

/// Maximum length of a callback override URL (`webhooks/override`).
pub const MAX_CALLBACK_URI_CHARS: usize = 200;

/// An alternate webhook callback for a WABA or a phone number
/// (`webhooks/override`). Meta verifies the URL with `verify_token` the same
/// way it verifies the app's callback.
///
/// `Debug` shows the URL but never the token.
#[derive(Clone)]
pub struct CallbackOverride {
    /// Alternate callback URL, at most 200 characters.
    pub override_callback_uri: String,
    /// Token Meta echoes during the verification request.
    pub verify_token: VerifyToken,
}

impl CallbackOverride {
    /// Build an override.
    pub fn new(uri: impl Into<String>, verify_token: impl Into<VerifyToken>) -> Self {
        Self {
            override_callback_uri: uri.into(),
            verify_token: verify_token.into(),
        }
    }

    /// Check the documented limit (1–200 characters). The token has no
    /// documented maximum.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let n = self.override_callback_uri.chars().count();
        if n == 0 || n > MAX_CALLBACK_URI_CHARS {
            return Err(ValidationError::new(
                "override_callback_uri",
                format!("must be 1-{MAX_CALLBACK_URI_CHARS} characters"),
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for CallbackOverride {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallbackOverride")
            .field("override_callback_uri", &self.override_callback_uri)
            .field("verify_token", &self.verify_token)
            .finish()
    }
}

/// A WhatsApp Business Account, from `GET /{WABA_ID}` or the business
/// `client_`/`owned_whatsapp_business_accounts` edges. Which fields are
/// present depends on the `fields` requested.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct WabaInfo {
    /// WABA id.
    pub id: WabaId,
    /// Name (visible in WhatsApp Manager only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Time zone id (Meta returns it as a string; numbers are accepted).
    #[serde(
        default,
        deserialize_with = "lenient_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub timezone_id: Option<String>,
    /// Template namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_namespace: Option<String>,
    /// Policy review status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_review_status: Option<AccountReviewStatus>,
    /// Business verification status as Meta reports it. Kept as a string:
    /// the reference lists uppercase values while `whatsapp-business-accounts`
    /// shows `verified` in lowercase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business_verification_status: Option<String>,
    /// Country.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Billing currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Ownership type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership_type: Option<OwnershipType>,
    /// Primary business location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_business_location: Option<String>,
    /// Account status, e.g. `ACTIVE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Business the WABA operates on behalf of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of_business_info: Option<BusinessRef>,
    /// Purchase order number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purchase_order_number: Option<String>,
    /// Whether insights are enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_enabled_for_insights: Option<bool>,
}

/// `{id, name}` of a business.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct BusinessRef {
    /// Business id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<BusinessId>,
    /// Business name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// WABA review status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum AccountReviewStatus {
    /// Approved.
    Approved,
    /// Deferred.
    Deferred,
    /// Pending.
    Pending,
    /// Rejected.
    Rejected,
    /// Restricted.
    Restricted,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

/// WABA ownership type (values from both the WABA and business references).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum OwnershipType {
    /// Owned by a client business.
    ClientOwned,
    /// On behalf of (deprecated OBO model).
    OnBehalfOf,
    /// Owned by the business itself: `SELF` (WABA reference) or
    /// `SELF_OWNED` (business reference).
    #[serde(rename = "SELF", alias = "SELF_OWNED")]
    SelfOwned,
    /// Owned by an agency.
    AgencyOwned,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

/// Body of `POST /{WABA_ID}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct WabaUpdate {
    /// New name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New time zone id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone_id: Option<String>,
}

impl WabaUpdate {
    /// Empty update.
    pub fn new() -> Self {
        Self::default()
    }

    /// Rename the WABA.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Change the time zone.
    #[must_use]
    pub fn timezone_id(mut self, timezone_id: impl Into<String>) -> Self {
        self.timezone_id = Some(timezone_id.into());
        self
    }
}

/// One app subscribed to a WABA's webhooks.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SubscribedApp {
    /// The app.
    pub whatsapp_business_api_data: SubscribedAppData,
    /// The WABA-level callback override, when one is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_callback_uri: Option<String>,
}

/// `whatsapp_business_api_data` of a subscribed app.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SubscribedAppData {
    /// App id.
    pub id: String,
    /// App name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Link to the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// A user (person or system user) assigned to a WABA.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct AssignedUser {
    /// User id.
    pub id: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Granted tasks (returned by the `manage-system-users` example).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<WabaTask>,
    /// Business the user belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business: Option<BusinessRef>,
    /// Kind of user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_type: Option<AssignedUserType>,
}

/// Kind of assigned user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum AssignedUserType {
    /// Business user.
    BusinessUser,
    /// System user.
    SystemUser,
    /// Personal user.
    PersonalUser,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

/// A permission task on a WABA.
///
/// The union of the `assigned-users-management-api` reference and the
/// `manage-system-users` guide (which adds `VIEW_INSIGHTS`, `MANAGE_USERS`,
/// `MANAGE_BILLING`); [`Self::Other`] keeps anything newer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WabaTask {
    /// Admin access.
    Manage,
    /// Developer access.
    Develop,
    /// Manage templates.
    ManageTemplates,
    /// Manage phone numbers.
    ManagePhone,
    /// View cost.
    ViewCost,
    /// Manage extensions.
    ManageExtensions,
    /// View phone assets.
    ViewPhoneAssets,
    /// Manage phone assets.
    ManagePhoneAssets,
    /// View templates.
    ViewTemplates,
    /// Send and receive messages.
    Messaging,
    /// View insights.
    ViewInsights,
    /// Manage users.
    ManageUsers,
    /// Manage billing (needed to share a line of credit).
    ManageBilling,
    /// Any other task string.
    Other(String),
}

impl WabaTask {
    /// The string Meta uses.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Manage => "MANAGE",
            Self::Develop => "DEVELOP",
            Self::ManageTemplates => "MANAGE_TEMPLATES",
            Self::ManagePhone => "MANAGE_PHONE",
            Self::ViewCost => "VIEW_COST",
            Self::ManageExtensions => "MANAGE_EXTENSIONS",
            Self::ViewPhoneAssets => "VIEW_PHONE_ASSETS",
            Self::ManagePhoneAssets => "MANAGE_PHONE_ASSETS",
            Self::ViewTemplates => "VIEW_TEMPLATES",
            Self::Messaging => "MESSAGING",
            Self::ViewInsights => "VIEW_INSIGHTS",
            Self::ManageUsers => "MANAGE_USERS",
            Self::ManageBilling => "MANAGE_BILLING",
            Self::Other(s) => s,
        }
    }

    fn from_str_lossless(s: &str) -> Self {
        match s {
            "MANAGE" => Self::Manage,
            "DEVELOP" => Self::Develop,
            "MANAGE_TEMPLATES" => Self::ManageTemplates,
            "MANAGE_PHONE" => Self::ManagePhone,
            "VIEW_COST" => Self::ViewCost,
            "MANAGE_EXTENSIONS" => Self::ManageExtensions,
            "VIEW_PHONE_ASSETS" => Self::ViewPhoneAssets,
            "MANAGE_PHONE_ASSETS" => Self::ManagePhoneAssets,
            "VIEW_TEMPLATES" => Self::ViewTemplates,
            "MESSAGING" => Self::Messaging,
            "VIEW_INSIGHTS" => Self::ViewInsights,
            "MANAGE_USERS" => Self::ManageUsers,
            "MANAGE_BILLING" => Self::ManageBilling,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl Serialize for WabaTask {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WabaTask {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(|s| Self::from_str_lossless(&s))
    }
}

/// One `filtering` condition (`[{"field", "operator", "value"}]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Filter {
    /// Field, e.g. `account_mode` or `creation_time`.
    pub field: String,
    /// Operator, e.g. `EQUAL`, `GREATER_THAN`, `LESS_THAN`.
    pub operator: String,
    /// Value, as a string (Meta's examples quote timestamps too).
    pub value: String,
}

impl Filter {
    /// Arbitrary condition.
    pub fn new(
        field: impl Into<String>,
        operator: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            field: field.into(),
            operator: operator.into(),
            value: value.into(),
        }
    }

    /// `account_mode EQUAL <mode>` (`SANDBOX` or `LIVE`) for phone numbers.
    pub fn account_mode(mode: impl Into<String>) -> Self {
        Self::new("account_mode", "EQUAL", mode)
    }

    /// `creation_time GREATER_THAN <unix>` for WABA lists.
    pub fn created_after(unix_seconds: i64) -> Self {
        Self::new("creation_time", "GREATER_THAN", unix_seconds.to_string())
    }

    /// `creation_time LESS_THAN <unix>` for WABA lists.
    pub fn created_before(unix_seconds: i64) -> Self {
        Self::new("creation_time", "LESS_THAN", unix_seconds.to_string())
    }
}

/// Body of `POST /{WABA_ID}/phone_numbers` (create a number on a WABA,
/// step 1 of `solution-providers/registering-phone-numbers`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct NewPhoneNumber {
    /// Country calling code, e.g. `1`.
    pub cc: String,
    /// The number, with or without the country calling code.
    pub phone_number: String,
    /// Display name.
    pub verified_name: String,
    /// On-Premises → Cloud API migration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migrate_phone_number: Option<bool>,
    /// Pre-verified phone number id to use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preverified_id: Option<String>,
}

impl NewPhoneNumber {
    /// The three fields the guide marks required.
    pub fn new(
        cc: impl Into<String>,
        phone_number: impl Into<String>,
        verified_name: impl Into<String>,
    ) -> Self {
        Self {
            cc: cc.into(),
            phone_number: phone_number.into(),
            verified_name: verified_name.into(),
            migrate_phone_number: None,
            preverified_id: None,
        }
    }

    /// Use a pre-verified number.
    #[must_use]
    pub fn preverified_id(mut self, id: impl Into<String>) -> Self {
        self.preverified_id = Some(id.into());
        self
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        for (field, value) in [
            ("cc", &self.cc),
            ("phone_number", &self.phone_number),
            ("verified_name", &self.verified_name),
        ] {
            if value.trim().is_empty() {
                return Err(ValidationError::new(field, "required"));
            }
        }
        Ok(())
    }
}

/// Business portfolio, from `GET /{BUSINESS_ID}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct BusinessInfo {
    /// Business id.
    pub id: BusinessId,
    /// Name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Time zone id (an integer in the reference; kept as a string).
    #[serde(
        default,
        deserialize_with = "lenient_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub timezone_id: Option<String>,
}

/// A messaging customer base (In-App Signup subscribers land in one).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct MessagingCustomerBase {
    /// Id.
    pub id: String,
    /// Name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Sort order for WABA lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WabaSort {
    /// Oldest first.
    CreationTimeAscending,
    /// Newest first.
    CreationTimeDescending,
}

/// Accepts a JSON string or number (Meta is inconsistent about ids such as
/// `timezone_id`) and keeps it as a string.
pub(crate) fn lenient_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    match Option::<serde_json::Value>::deserialize(d)? {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        Some(serde_json::Value::Number(n)) => Ok(Some(n.to_string())),
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected a string or number, got {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn callback_override_debug_hides_the_token() {
        let o = CallbackOverride::new("https://x.example/hook", "s3cret-verify");
        let dbg = format!("{o:?}");
        assert!(dbg.contains("https://x.example/hook"));
        assert!(!dbg.contains("s3cret-verify"), "{dbg}");
    }

    #[test]
    fn callback_override_length_limit() {
        let ok = CallbackOverride::new("u".repeat(200), "t");
        assert!(ok.validate().is_ok());
        assert!(
            CallbackOverride::new("u".repeat(201), "t")
                .validate()
                .is_err()
        );
        assert!(CallbackOverride::new("", "t").validate().is_err());
    }

    #[test]
    fn waba_info_parses_guide_example() {
        // whatsapp-business-accounts, "Get WABA data via API".
        let v = r#"{
          "name": "Lucky Shrub",
          "status": "ACTIVE",
          "currency": "USD",
          "country": "US",
          "business_verification_status": "verified",
          "id": "102290129340398"
        }"#;
        let w: WabaInfo = serde_json::from_str(v).unwrap();
        assert_eq!(w.id.as_str(), "102290129340398");
        assert_eq!(w.status.as_deref(), Some("ACTIVE"));
        assert_eq!(w.business_verification_status.as_deref(), Some("verified"));
    }

    #[test]
    fn waba_info_accepts_numeric_timezone_and_unknown_enums() {
        let v = r#"{"id": "1", "timezone_id": 5, "account_review_status": "NEWLY_INVENTED", "ownership_type": "CLIENT_OWNED"}"#;
        let w: WabaInfo = serde_json::from_str(v).unwrap();
        assert_eq!(w.timezone_id.as_deref(), Some("5"));
        assert_eq!(w.account_review_status, Some(AccountReviewStatus::Unknown));
        assert_eq!(w.ownership_type, Some(OwnershipType::ClientOwned));
    }

    #[test]
    fn tasks_round_trip_including_unknown() {
        let t: Vec<WabaTask> =
            serde_json::from_str(r#"["MANAGE","MANAGE_BILLING","FUTURE_TASK"]"#).unwrap();
        assert_eq!(
            t,
            vec![
                WabaTask::Manage,
                WabaTask::ManageBilling,
                WabaTask::Other("FUTURE_TASK".into())
            ]
        );
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            r#"["MANAGE","MANAGE_BILLING","FUTURE_TASK"]"#
        );
    }
}
