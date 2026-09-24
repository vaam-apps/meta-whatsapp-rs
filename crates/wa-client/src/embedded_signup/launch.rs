//! The options object passed to `FB.login` to launch Embedded Signup.
//!
//! Shape, from `embedded-signup/implementation` (launch method),
//! `embedded-signup/versions`, `embedded-signup/pre-filled-data`,
//! `embedded-signup/bypass-phone-addition`, `embedded-signup/app-only-install`
//! and `embedded-signup/onboarding-business-app-users`:
//!
//! ```json
//! {
//!   "config_id": "<CONFIGURATION_ID>",
//!   "response_type": "code",
//!   "override_default_response_type": true,
//!   "extras": {
//!     "setup": { "business": {…}, "preVerifiedPhone": {…}, "phone": {…},
//!                "whatsAppBusinessAccount": {…}, "solutionID": "…" },
//!     "featureType": "whatsapp_business_app_onboarding",
//!     "features": [{ "name": "app_only_install" }],
//!     "sessionInfoVersion": "3",
//!     "version": "v4-public-preview"
//!   }
//! }
//! ```
//!
//! For v4 (the current version) the products come from the Facebook Login
//! for Business configuration and `extras` needs nothing but the optional
//! pre-fill and `featureType`; `version`, `features` and
//! `sessionInfoVersion` exist for the older and preview versions. `setup`
//! is always emitted (possibly empty), matching the v4 implementation page.
//!
//! # Where Meta's pages disagree (decided here)
//!
//! Both are output-only (this crate never parses launch options back), so
//! each is one decision, pinned by a test against the page's example:
//!
//! - `setup.whatsAppBusinessAccount`: the syntax block of
//!   `embedded-signup/pre-filled-data` says `{ ids: '<WABA_ID>' }` (a string
//!   under `ids`), its worked example says `{ id: ['<WABA_ID>'] }` (an array
//!   under `id`). This follows the **example**, as the rest of the crate
//!   does when a page contradicts itself: examples are what Meta ran.
//! - `setup.business.id`: typed "Integer or null", but the existing-portfolio
//!   example passes a quoted string. Sent as a **string**, matching the
//!   example: the object is evaluated by JavaScript, and portfolio ids are
//!   64-bit values that a JS number may not hold exactly (a rounded id would
//!   name some other portfolio).
//!
//! Neither can be verified offline; a live Embedded Signup run with
//! pre-filled data is the test that would settle them.

use serde::Serialize;
use serde::ser::Serializer;
use wa_core::Result;
use wa_core::error::ValidationError;
use wa_core::ids::{BusinessId, PhoneNumberId, WabaId};

/// Maximum characters of the business portfolio name (`pre-filled-data`).
pub const MAX_BUSINESS_NAME_CHARS: usize = 100;
/// Maximum characters of the phone profile description (`pre-filled-data`).
pub const MAX_PHONE_DESCRIPTION_CHARS: usize = 512;

/// The object for `FB.login(callback, options)`. Build it, call
/// [`Self::to_json`], and hand the JSON to the page that launches the flow.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaunchOptions {
    config_id: String,
    response_type: &'static str,
    override_default_response_type: bool,
    extras: Extras,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
struct Extras {
    setup: Setup,
    #[serde(rename = "featureType", skip_serializing_if = "Option::is_none")]
    feature_type: Option<FeatureType>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    features: Vec<Feature>,
    #[serde(rename = "sessionInfoVersion", skip_serializing_if = "Option::is_none")]
    session_info_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<EsVersion>,
}

/// `extras.setup`: data injected into the flow's screens.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Setup {
    /// Business portfolio pre-fill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub business: Option<BusinessPrefill>,
    /// Pre-verified phone number ids to offer.
    #[serde(rename = "preVerifiedPhone", skip_serializing_if = "Option::is_none")]
    pub pre_verified_phone: Option<PreVerifiedPhone>,
    /// Phone number profile applied in the WhatsApp client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<PhoneProfilePrefill>,
    /// Existing WABA to attach a pre-verified number to.
    #[serde(
        rename = "whatsAppBusinessAccount",
        skip_serializing_if = "Option::is_none"
    )]
    pub whatsapp_business_account: Option<WabaPrefill>,
    /// Multi-Partner Solution id (`app-only-install`).
    #[serde(rename = "solutionID", skip_serializing_if = "Option::is_none")]
    pub solution_id: Option<String>,
}

/// `setup.business`. With `id` set to a portfolio the customer owns, the
/// flow uses that portfolio's data and ignores the rest; otherwise `name`,
/// `email`, `website` and `address.country` pre-fill the screen **only when
/// all four are set** (Meta shows the empty screen otherwise).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct BusinessPrefill {
    /// Existing portfolio id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<BusinessId>,
    /// Portfolio name (≤ 100 characters); also the WABA name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Business email.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Business website.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    /// Address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<AddressPrefill>,
    /// Business phone number (pre-fills the phone number addition screen).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<BusinessPhonePrefill>,
    /// Time zone as a UTC offset, e.g. `UTC-07:00`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

impl BusinessPrefill {
    /// Use an existing portfolio.
    pub fn existing(id: impl Into<BusinessId>) -> Self {
        Self {
            id: Some(id.into()),
            ..Self::default()
        }
    }

    /// A new portfolio from the four fields Meta needs to pre-fill it.
    pub fn new_portfolio(
        name: impl Into<String>,
        email: impl Into<String>,
        website: impl Into<String>,
        country: impl Into<String>,
    ) -> Self {
        Self {
            name: Some(name.into()),
            email: Some(email.into()),
            website: Some(website.into()),
            address: Some(AddressPrefill {
                country: Some(country.into()),
                ..AddressPrefill::default()
            }),
            ..Self::default()
        }
    }
}

/// `setup.business.address`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AddressPrefill {
    /// Street, line 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street_address1: Option<String>,
    /// Street, line 2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street_address2: Option<String>,
    /// City.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// State.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// ZIP / postal code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zip_postal: Option<String>,
    /// ISO 3166-1 alpha-2 country code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
}

/// `setup.business.phone`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct BusinessPhonePrefill {
    /// Country calling code, e.g. `1` (an integer in Meta's syntax).
    pub code: u32,
    /// Number without the country calling code.
    pub number: String,
}

impl BusinessPhonePrefill {
    /// Build the phone pre-fill.
    pub fn new(code: u32, number: impl Into<String>) -> Self {
        Self {
            code,
            number: number.into(),
        }
    }
}

/// `setup.preVerifiedPhone`. Only the first id is shown on the WABA
/// selection screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct PreVerifiedPhone {
    /// Pre-verified phone number ids.
    pub ids: Vec<PhoneNumberId>,
}

/// `setup.phone`: the number's WhatsApp profile. All three fields are
/// required by Meta when the object is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PhoneProfilePrefill {
    /// Profile display name.
    pub display_name: String,
    /// Profile category, a `vertical` value of the business profile API
    /// (e.g. `APPAREL`).
    pub category: String,
    /// Description, ≤ 512 characters.
    pub description: String,
}

impl PhoneProfilePrefill {
    /// Build the profile pre-fill.
    pub fn new(
        display_name: impl Into<String>,
        category: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            display_name: display_name.into(),
            category: category.into(),
            description: description.into(),
        }
    }
}

/// `setup.whatsAppBusinessAccount` (see the module docs for the shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct WabaPrefill {
    /// WABA ids, serialized under `id` as in Meta's example.
    #[serde(rename = "id")]
    pub ids: Vec<WabaId>,
}

/// `extras.featureType`: which custom flow to run.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FeatureType {
    /// Coexistence: onboard a number already used with the WhatsApp
    /// Business app (all versions up to v4).
    WhatsappBusinessAppOnboarding,
    /// Skip the phone number screens (v2 only; `FINISH_ONLY_WABA`).
    OnlyWabaSharing,
    /// Marketing Messages API onboarding (v2 only; v3+ uses `features`).
    MarketingMessagesLite,
    /// Any other documented value.
    Other(String),
}

impl FeatureType {
    /// The string Meta expects.
    pub fn as_str(&self) -> &str {
        match self {
            Self::WhatsappBusinessAppOnboarding => "whatsapp_business_app_onboarding",
            Self::OnlyWabaSharing => "only_waba_sharing",
            Self::MarketingMessagesLite => "marketing_messages_lite",
            Self::Other(s) => s,
        }
    }
}

impl Serialize for FeatureType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// `extras.features[].name` (v3 and the previews).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FeatureName {
    /// Only business tokens can access the onboarded assets
    /// (`FINISH_GRANT_ONLY_API_ACCESS`). Not combinable with coexistence.
    AppOnlyInstall,
    /// Marketing Messages API onboarding.
    MarketingMessagesLite,
    /// Any other documented value.
    Other(String),
}

impl FeatureName {
    /// The string Meta expects.
    pub fn as_str(&self) -> &str {
        match self {
            Self::AppOnlyInstall => "app_only_install",
            Self::MarketingMessagesLite => "marketing_messages_lite",
            Self::Other(s) => s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Feature {
    #[serde(serialize_with = "ser_feature_name")]
    name: FeatureName,
}

fn ser_feature_name<S: Serializer>(name: &FeatureName, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(name.as_str())
}

/// `extras.version`. Plain v4 has no value: the login configuration selects
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EsVersion {
    /// `v4-public-preview` (phone-number-first flow).
    V4PublicPreview,
    /// `v3-public-preview`.
    V3PublicPreview,
    /// `v2-public-preview`.
    V2PublicPreview,
    /// `v3`.
    V3,
    /// `v2` (deprecated on 2026-10-15).
    V2,
    /// Any other value.
    Other(String),
}

impl EsVersion {
    /// The string Meta expects.
    pub fn as_str(&self) -> &str {
        match self {
            Self::V4PublicPreview => "v4-public-preview",
            Self::V3PublicPreview => "v3-public-preview",
            Self::V2PublicPreview => "v2-public-preview",
            Self::V3 => "v3",
            Self::V2 => "v2",
            Self::Other(s) => s,
        }
    }
}

impl Serialize for EsVersion {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl LaunchOptions {
    /// Options for the Facebook Login for Business configuration
    /// `config_id`, with `response_type: "code"` and
    /// `override_default_response_type: true` as Embedded Signup requires.
    pub fn new(config_id: impl Into<String>) -> Self {
        Self {
            config_id: config_id.into(),
            response_type: "code",
            override_default_response_type: true,
            extras: Extras::default(),
        }
    }

    /// Run the coexistence flow (`featureType:
    /// whatsapp_business_app_onboarding`): the business keeps using the
    /// WhatsApp Business app on the same number.
    #[must_use]
    pub fn coexistence(self) -> Self {
        self.feature_type(FeatureType::WhatsappBusinessAppOnboarding)
    }

    /// Set `featureType`.
    #[must_use]
    pub fn feature_type(mut self, feature_type: FeatureType) -> Self {
        self.extras.feature_type = Some(feature_type);
        self
    }

    /// Add an entry to `features`.
    #[must_use]
    pub fn feature(mut self, name: FeatureName) -> Self {
        self.extras.features.push(Feature { name });
        self
    }

    /// Set `version` (only for v2/v3/previews).
    #[must_use]
    pub fn version(mut self, version: EsVersion) -> Self {
        self.extras.version = Some(version);
        self
    }

    /// Set `sessionInfoVersion` (v2 needs `"3"` to receive the session info
    /// message event; later versions send it anyway).
    #[must_use]
    pub fn session_info_version(mut self, version: impl Into<String>) -> Self {
        self.extras.session_info_version = Some(version.into());
        self
    }

    /// Pre-fill the business portfolio screen.
    #[must_use]
    pub fn business(mut self, business: BusinessPrefill) -> Self {
        self.extras.setup.business = Some(business);
        self
    }

    /// Offer pre-verified phone numbers (skips phone addition and
    /// verification).
    #[must_use]
    pub fn pre_verified_phone_ids(
        mut self,
        ids: impl IntoIterator<Item = impl Into<PhoneNumberId>>,
    ) -> Self {
        self.extras.setup.pre_verified_phone = Some(PreVerifiedPhone {
            ids: ids.into_iter().map(Into::into).collect(),
        });
        self
    }

    /// Set the phone number's WhatsApp profile.
    #[must_use]
    pub fn phone_profile(mut self, profile: PhoneProfilePrefill) -> Self {
        self.extras.setup.phone = Some(profile);
        self
    }

    /// Attach the pre-verified number to existing WABA(s).
    #[must_use]
    pub fn waba_ids(mut self, ids: impl IntoIterator<Item = impl Into<WabaId>>) -> Self {
        self.extras.setup.whatsapp_business_account = Some(WabaPrefill {
            ids: ids.into_iter().map(Into::into).collect(),
        });
        self
    }

    /// Multi-Partner Solution id.
    #[must_use]
    pub fn solution_id(mut self, solution_id: impl Into<String>) -> Self {
        self.extras.setup.solution_id = Some(solution_id.into());
        self
    }

    /// The `setup` object, for inspection.
    pub fn setup(&self) -> &Setup {
        &self.extras.setup
    }

    /// Check the documented rules.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.config_id.trim().is_empty() {
            return Err(ValidationError::new("config_id", "required"));
        }
        let coexistence =
            self.extras.feature_type == Some(FeatureType::WhatsappBusinessAppOnboarding);
        let app_only = self
            .extras
            .features
            .iter()
            .any(|f| f.name == FeatureName::AppOnlyInstall);
        if coexistence && app_only {
            return Err(ValidationError::new(
                "extras.features",
                "app_only_install cannot be used to onboard WhatsApp Business app users",
            ));
        }
        let setup = &self.extras.setup;
        if let Some(b) = &setup.business {
            if let Some(name) = &b.name {
                let n = name.chars().count();
                if n == 0 || n > MAX_BUSINESS_NAME_CHARS {
                    return Err(ValidationError::new(
                        "extras.setup.business.name",
                        format!("must be 1-{MAX_BUSINESS_NAME_CHARS} characters"),
                    ));
                }
            }
            if let Some(country) = b.address.as_ref().and_then(|a| a.country.as_deref())
                && !(country.len() == 2 && country.bytes().all(|c| c.is_ascii_uppercase()))
            {
                return Err(ValidationError::new(
                    "extras.setup.business.address.country",
                    "must be an ISO 3166-1 alpha-2 code, e.g. US",
                ));
            }
            if let Some(phone) = &b.phone
                && phone.number.trim().is_empty()
            {
                return Err(ValidationError::new(
                    "extras.setup.business.phone.number",
                    "required",
                ));
            }
        }
        if let Some(p) = &setup.pre_verified_phone
            && p.ids.is_empty()
        {
            return Err(ValidationError::new(
                "extras.setup.preVerifiedPhone.ids",
                "at least one id",
            ));
        }
        if let Some(w) = &setup.whatsapp_business_account
            && w.ids.is_empty()
        {
            return Err(ValidationError::new(
                "extras.setup.whatsAppBusinessAccount",
                "at least one WABA id",
            ));
        }
        if let Some(p) = &setup.phone {
            for (field, value) in [
                ("extras.setup.phone.displayName", &p.display_name),
                ("extras.setup.phone.category", &p.category),
                ("extras.setup.phone.description", &p.description),
            ] {
                if value.trim().is_empty() {
                    return Err(ValidationError::new(field, "required"));
                }
            }
            if p.description.chars().count() > MAX_PHONE_DESCRIPTION_CHARS {
                return Err(ValidationError::new(
                    "extras.setup.phone.description",
                    format!("at most {MAX_PHONE_DESCRIPTION_CHARS} characters"),
                ));
            }
        }
        Ok(())
    }

    /// Validate and render the JSON object for `FB.login`.
    pub fn to_json(&self) -> Result<serde_json::Value> {
        self.validate()?;
        serde_json::to_value(self)
            .map_err(|e| ValidationError::new("launch_options", e.to_string()).into())
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn default_matches_the_v4_implementation_snippet() {
        // embedded-signup/implementation, "Launch method and callback registration".
        let v = LaunchOptions::new("<CONFIGURATION_ID>").to_json().unwrap();
        assert_eq!(
            v,
            json!({
              "config_id": "<CONFIGURATION_ID>",
              "response_type": "code",
              "override_default_response_type": true,
              "extras": {"setup": {}}
            })
        );
    }

    #[test]
    fn coexistence_sets_the_feature_type() {
        let v = LaunchOptions::new("1").coexistence().to_json().unwrap();
        assert_eq!(
            v["extras"],
            json!({"setup": {}, "featureType": "whatsapp_business_app_onboarding"})
        );
    }

    #[test]
    fn new_portfolio_prefill_matches_docs_example() {
        // embedded-signup/pre-filled-data, "New business portfolio, pre-verified
        // number, and display profile".
        let mut business = BusinessPrefill::new_portfolio(
            "Wind & Wool",
            "support@windandwool.com",
            "https://windandwool.com/",
            "US",
        );
        business.address = Some(AddressPrefill {
            street_address1: Some("1 Hacker Way".into()),
            street_address2: Some("Suite 1".into()),
            city: Some("Menlo Park".into()),
            state: Some("California".into()),
            zip_postal: Some("94025".into()),
            country: Some("US".into()),
        });
        business.phone = Some(BusinessPhonePrefill::new(1, "6505559999"));
        business.timezone = Some("UTC-07:00".into());
        let v = LaunchOptions::new("31602279155865")
            .business(business)
            .pre_verified_phone_ids(["106540352242922"])
            .phone_profile(PhoneProfilePrefill::new(
                "Wind & Wool",
                "APPAREL",
                "Bespoke artisan apparel and lifestyle goods from upcoming designers.",
            ))
            .feature_type(FeatureType::Other(String::new()))
            .session_info_version("3")
            .to_json()
            .unwrap();
        assert_eq!(
            v,
            json!({
              "config_id": "31602279155865",
              "response_type": "code",
              "override_default_response_type": true,
              "extras": {
                "setup": {
                  "business": {
                    "name": "Wind & Wool",
                    "email": "support@windandwool.com",
                    "website": "https://windandwool.com/",
                    "address": {
                      "streetAddress1": "1 Hacker Way",
                      "streetAddress2": "Suite 1",
                      "city": "Menlo Park",
                      "state": "California",
                      "zipPostal": "94025",
                      "country": "US"
                    },
                    "phone": {"code": 1, "number": "6505559999"},
                    "timezone": "UTC-07:00"
                  },
                  "preVerifiedPhone": {"ids": ["106540352242922"]},
                  "phone": {
                    "displayName": "Wind & Wool",
                    "category": "APPAREL",
                    "description": "Bespoke artisan apparel and lifestyle goods from upcoming designers."
                  }
                },
                "featureType": "",
                "sessionInfoVersion": "3"
              }
            })
        );
    }

    #[test]
    fn existing_portfolio_and_waba_prefill() {
        // pre-filled-data, "Existing business portfolio …" and the
        // whatsAppBusinessAccount example.
        let v = LaunchOptions::new("31602279155865")
            .business(BusinessPrefill::existing("2729063490586005"))
            .pre_verified_phone_ids(["106540352242922"])
            .waba_ids(["432428883295692"])
            .to_json()
            .unwrap();
        assert_eq!(
            v["extras"]["setup"],
            json!({
              "business": {"id": "2729063490586005"},
              "preVerifiedPhone": {"ids": ["106540352242922"]},
              "whatsAppBusinessAccount": {"id": ["432428883295692"]}
            })
        );
    }

    #[test]
    fn bypass_and_app_only_install_shapes() {
        // bypass-phone-addition.
        let v = LaunchOptions::new("C")
            .feature_type(FeatureType::OnlyWabaSharing)
            .session_info_version("3")
            .to_json()
            .unwrap();
        assert_eq!(
            v["extras"],
            json!({"setup": {}, "featureType": "only_waba_sharing", "sessionInfoVersion": "3"})
        );
        // app-only-install, with a Multi-Partner Solution.
        let v = LaunchOptions::new("<CONFIG_ID>")
            .version(EsVersion::V3)
            .feature(FeatureName::AppOnlyInstall)
            .solution_id("<SOLUTION_ID>")
            .to_json()
            .unwrap();
        assert_eq!(
            v,
            json!({
              "config_id": "<CONFIG_ID>",
              "response_type": "code",
              "override_default_response_type": true,
              "extras": {
                "version": "v3",
                "features": [{"name": "app_only_install"}],
                "setup": {"solutionID": "<SOLUTION_ID>"}
              }
            })
        );
    }

    #[test]
    fn documented_rules_are_enforced() {
        let field = |o: LaunchOptions| match o.to_json().unwrap_err() {
            wa_core::Error::Validation(v) => v.field,
            other => panic!("{other}"),
        };
        assert_eq!(field(LaunchOptions::new(" ")), "config_id");
        assert_eq!(
            field(
                LaunchOptions::new("c")
                    .coexistence()
                    .feature(FeatureName::AppOnlyInstall)
            ),
            "extras.features"
        );
        assert_eq!(
            field(
                LaunchOptions::new("c").business(BusinessPrefill::new_portfolio(
                    "n".repeat(101),
                    "e",
                    "w",
                    "US"
                ))
            ),
            "extras.setup.business.name"
        );
        assert!(
            LaunchOptions::new("c")
                .business(BusinessPrefill::new_portfolio(
                    "n".repeat(100),
                    "e",
                    "w",
                    "US"
                ))
                .validate()
                .is_ok()
        );
        assert_eq!(
            field(
                LaunchOptions::new("c")
                    .business(BusinessPrefill::new_portfolio("n", "e", "w", "USA"))
            ),
            "extras.setup.business.address.country"
        );
        assert_eq!(
            field(
                LaunchOptions::new("c").phone_profile(PhoneProfilePrefill::new(
                    "n",
                    "APPAREL",
                    "d".repeat(513)
                ))
            ),
            "extras.setup.phone.description"
        );
        assert_eq!(
            field(LaunchOptions::new("c").phone_profile(PhoneProfilePrefill::new("n", "", "d"))),
            "extras.setup.phone.category"
        );
        assert_eq!(
            field(LaunchOptions::new("c").pre_verified_phone_ids(Vec::<String>::new())),
            "extras.setup.preVerifiedPhone.ids"
        );
    }
}
