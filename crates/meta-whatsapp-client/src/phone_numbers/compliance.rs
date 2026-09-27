//! Business compliance information (India): the legal entity behind a
//! number, its grievance officer and its customer care contacts
//! (`reference/whatsapp-business-phone-number/business-compliance-information-api`).
//!
//! # Where the docs stop
//!
//! - The page has no request or response example; field names come from
//!   its schemas.
//! - Reading: the schema types `entity_type` as a free string ("e.g.,
//!   Partnership, Private Limited Company"), not the uppercase list the
//!   update takes, so [`BusinessComplianceInfo::entity_type`] stays a
//!   string.
//! - Emails must be well formed and phone numbers international, per the
//!   page; Meta checks those, not this client. The emails' length ("under
//!   128 characters") is checked here.

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::WabaId;
use serde::{Deserialize, Serialize};

use super::{PhoneNumber, fields_param};
use crate::templates::macros::string_enum;

/// Shortest `entity_name`, in characters.
pub const MIN_ENTITY_NAME_CHARS: usize = 2;
/// Longest `entity_name`, in characters.
pub const MAX_ENTITY_NAME_CHARS: usize = 128;
/// An email must be "under 128 characters" (the update's validation rules).
const EMAIL_CHARS_UNDER: usize = 128;

string_enum! {
    /// The legal form of the business (`entity_type` of an update).
    pub enum BusinessEntityType {
        /// A limited liability partnership.
        LimitedLiabilityPartnership => "LIMITED_LIABILITY_PARTNERSHIP",
        /// A sole proprietorship.
        SoleProprietorship => "SOLE_PROPRIETORSHIP",
        /// A partnership (may say whether it is registered).
        Partnership => "PARTNERSHIP",
        /// A public company.
        PublicCompany => "PUBLIC_COMPANY",
        /// A private company.
        PrivateCompany => "PRIVATE_COMPANY",
        /// Meta's `OTHER`: none of the above, described in
        /// `entity_type_custom` (required with it). Named `Custom` here
        /// because `Other(String)` is the catch-all for values this crate
        /// does not know.
        Custom => "OTHER",
    }
}

/// The grievance officer's contact details (`grievance_officer_details`).
/// An update needs `name` and `email`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct GrievanceOfficer {
    /// Full name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Email address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Mobile number, with the country code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mobile_number: Option<String>,
    /// Landline number, with the country code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landline_number: Option<String>,
}

impl GrievanceOfficer {
    /// An officer with the two required fields.
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            email: Some(email.into()),
            mobile_number: None,
            landline_number: None,
        }
    }

    /// Mobile number, with the country code.
    #[must_use]
    pub fn mobile_number(mut self, number: impl Into<String>) -> Self {
        self.mobile_number = Some(number.into());
        self
    }

    /// Landline number, with the country code.
    #[must_use]
    pub fn landline_number(mut self, number: impl Into<String>) -> Self {
        self.landline_number = Some(number.into());
        self
    }
}

/// Customer care contact details (`customer_care_details`). An update
/// needs `email`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CustomerCare {
    /// Email address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Mobile number, with the country code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mobile_number: Option<String>,
    /// Landline number, with the country code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landline_number: Option<String>,
}

impl CustomerCare {
    /// Customer care with the required email.
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: Some(email.into()),
            mobile_number: None,
            landline_number: None,
        }
    }

    /// Mobile number, with the country code.
    #[must_use]
    pub fn mobile_number(mut self, number: impl Into<String>) -> Self {
        self.mobile_number = Some(number.into());
        self
    }

    /// Landline number, with the country code.
    #[must_use]
    pub fn landline_number(mut self, number: impl Into<String>) -> Self {
        self.landline_number = Some(number.into());
        self
    }
}

/// One entry of `GET /{Phone-Number-ID}/business_compliance_info`
/// (`BusinessComplianceInfo`). Which fields are present depends on the
/// `fields` asked for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct BusinessComplianceInfo {
    /// The WABA the information belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_business_account_id: Option<WabaId>,
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// The legal name of the business.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_name: Option<String>,
    /// The legal form, as Meta returns it (a free string, see the module
    /// docs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_type: Option<String>,
    /// The legal form, when it is none of the listed ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_type_custom: Option<String>,
    /// Whether the business is registered with the authorities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_registered: Option<bool>,
    /// The grievance officer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grievance_officer_details: Option<GrievanceOfficer>,
    /// Customer care.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_care_details: Option<CustomerCare>,
}

/// Body of `POST /{Phone-Number-ID}/business_compliance_info`
/// (`BusinessComplianceInfoUpdateRequest`; `messaging_product` is added
/// when sent). `None` fields are left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ComplianceInfoUpdate {
    /// The legal name of the business, 2 to 128 characters.
    pub entity_name: String,
    /// The legal form.
    pub entity_type: BusinessEntityType,
    /// The legal form in words: required with
    /// [`BusinessEntityType::Custom`], refused with any other.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_type_custom: Option<String>,
    /// Whether the business is registered: only with
    /// [`BusinessEntityType::Custom`] or [`BusinessEntityType::Partnership`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_registered: Option<bool>,
    /// The grievance officer (name and email required).
    pub grievance_officer_details: GrievanceOfficer,
    /// Customer care (email required).
    pub customer_care_details: CustomerCare,
}

impl ComplianceInfoUpdate {
    /// An update with the required fields.
    pub fn new(
        entity_name: impl Into<String>,
        entity_type: BusinessEntityType,
        grievance_officer: GrievanceOfficer,
        customer_care: CustomerCare,
    ) -> Self {
        Self {
            entity_name: entity_name.into(),
            entity_type,
            entity_type_custom: None,
            is_registered: None,
            grievance_officer_details: grievance_officer,
            customer_care_details: customer_care,
        }
    }

    /// The legal form in words (with [`BusinessEntityType::Custom`]).
    #[must_use]
    pub fn entity_type_custom(mut self, description: impl Into<String>) -> Self {
        self.entity_type_custom = Some(description.into());
        self
    }

    /// Whether the business is registered (with
    /// [`BusinessEntityType::Custom`] or [`BusinessEntityType::Partnership`]).
    #[must_use]
    pub fn is_registered(mut self, registered: bool) -> Self {
        self.is_registered = Some(registered);
        self
    }

    /// Check the page's validation rules that need no lookup: the name's
    /// length (as sent, and not blank), `entity_type_custom` exactly with
    /// `OTHER`, `is_registered` only with `OTHER` or `PARTNERSHIP`, the
    /// required contact fields present, and the emails under 128
    /// characters. Whether an email or a phone number is well formed is
    /// left to Meta.
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        let name_len = self.entity_name.chars().count();
        if !(MIN_ENTITY_NAME_CHARS..=MAX_ENTITY_NAME_CHARS).contains(&name_len)
            || self.entity_name.trim().chars().count() < MIN_ENTITY_NAME_CHARS
        {
            return Err(ValidationError::new(
                "entity_name",
                "must be 2-128 characters",
            ));
        }
        let custom = self.entity_type == BusinessEntityType::Custom;
        match (&self.entity_type_custom, custom) {
            (None, true) => {
                return Err(ValidationError::new(
                    "entity_type_custom",
                    "is required when entity_type is OTHER",
                ));
            }
            (Some(text), true) if text.trim().is_empty() => {
                return Err(ValidationError::new(
                    "entity_type_custom",
                    "must not be empty",
                ));
            }
            (Some(_), false) => {
                return Err(ValidationError::new(
                    "entity_type_custom",
                    "is only allowed when entity_type is OTHER",
                ));
            }
            _ => {}
        }
        if self.is_registered.is_some()
            && !(custom || self.entity_type == BusinessEntityType::Partnership)
        {
            return Err(ValidationError::new(
                "is_registered",
                "is only allowed when entity_type is OTHER or PARTNERSHIP",
            ));
        }
        required(
            "grievance_officer_details.name",
            self.grievance_officer_details.name.as_deref(),
        )?;
        required(
            "grievance_officer_details.email",
            self.grievance_officer_details.email.as_deref(),
        )?;
        required(
            "customer_care_details.email",
            self.customer_care_details.email.as_deref(),
        )?;
        email_length(
            "grievance_officer_details.email",
            self.grievance_officer_details.email.as_deref(),
        )?;
        email_length(
            "customer_care_details.email",
            self.customer_care_details.email.as_deref(),
        )
    }
}

fn required(field: &str, value: Option<&str>) -> std::result::Result<(), ValidationError> {
    if value.is_some_and(|v| !v.trim().is_empty()) {
        Ok(())
    } else {
        Err(ValidationError::new(field, "is required"))
    }
}

fn email_length(field: &str, value: Option<&str>) -> std::result::Result<(), ValidationError> {
    if value.is_some_and(|v| v.chars().count() >= EMAIL_CHARS_UNDER) {
        Err(ValidationError::new(field, "must be under 128 characters"))
    } else {
        Ok(())
    }
}

#[derive(Serialize)]
struct UpdateBody<'a> {
    messaging_product: &'static str,
    #[serde(flatten)]
    update: &'a ComplianceInfoUpdate,
}

#[derive(Deserialize)]
struct ComplianceResponse {
    #[serde(default)]
    data: Vec<BusinessComplianceInfo>,
}

impl PhoneNumber {
    /// `GET /{Phone-Number-ID}/business_compliance_info` with `fields`
    /// (empty = Meta's defaults). Meta wraps the answer in a `data` list;
    /// the page documents no pagination.
    pub async fn business_compliance_info(
        &self,
        fields: &[&str],
    ) -> Result<Vec<BusinessComplianceInfo>> {
        let resp: ComplianceResponse = self
            .client
            .get_at(&[self.phone_number_id.as_str(), "business_compliance_info"])
            .query_opt("fields", fields_param(fields))
            .context("business compliance information")
            .send()
            .await?;
        Ok(resp.data)
    }

    /// `POST /{Phone-Number-ID}/business_compliance_info`: create or
    /// replace the number's compliance information. Checked first
    /// ([`ComplianceInfoUpdate::validate`]).
    ///
    /// Sets the information to the given values, so it is replayed on
    /// transient errors.
    pub async fn set_business_compliance_info(&self, update: &ComplianceInfoUpdate) -> Result<()> {
        update.validate()?;
        self.client
            .post_at(&[self.phone_number_id.as_str(), "business_compliance_info"])
            .json(&UpdateBody {
                messaging_product: "whatsapp",
                update,
            })
            // Setting a value: replaying it cannot duplicate an effect.
            .idempotent(true)
            .context("business compliance information response")
            .send_success()
            .await
    }
}

#[cfg(test)]
mod tests {
    use http::Method;
    use meta_whatsapp_core::testing::ScriptedTransport;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::{Client, RetryPolicy};

    const ID: &str = "106540352242922";

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn officer() -> GrievanceOfficer {
        GrievanceOfficer::new("Asha Rao", "grievance@luckyshrub.example")
    }

    fn care() -> CustomerCare {
        CustomerCare::new("care@luckyshrub.example")
    }

    #[tokio::test]
    async fn set_sends_every_field() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        let update = ComplianceInfoUpdate::new(
            "Lucky Shrub",
            BusinessEntityType::Custom,
            officer()
                .mobile_number("+919876543210")
                .landline_number("+912212345678"),
            care()
                .mobile_number("+919812345678")
                .landline_number("+912287654321"),
        )
        .entity_type_custom("Trust")
        .is_registered(true);
        client(&t)
            .phone_number(ID)
            .set_business_compliance_info(&update)
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.path(),
            "/v25.0/106540352242922/business_compliance_info"
        );
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.json(),
            Some(json!({
                "messaging_product": "whatsapp",
                "entity_name": "Lucky Shrub",
                "entity_type": "OTHER",
                "entity_type_custom": "Trust",
                "is_registered": true,
                "grievance_officer_details": {
                    "name": "Asha Rao",
                    "email": "grievance@luckyshrub.example",
                    "mobile_number": "+919876543210",
                    "landline_number": "+912212345678"
                },
                "customer_care_details": {
                    "email": "care@luckyshrub.example",
                    "mobile_number": "+919812345678",
                    "landline_number": "+912287654321"
                }
            }))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn set_with_only_the_required_fields() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        let update = ComplianceInfoUpdate::new(
            "Lucky Shrub Private Limited",
            BusinessEntityType::PrivateCompany,
            officer(),
            care(),
        );
        client(&t)
            .phone_number(ID)
            .set_business_compliance_info(&update)
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().json(),
            Some(json!({
                "messaging_product": "whatsapp",
                "entity_name": "Lucky Shrub Private Limited",
                "entity_type": "PRIVATE_COMPANY",
                "grievance_officer_details": {
                    "name": "Asha Rao",
                    "email": "grievance@luckyshrub.example"
                },
                "customer_care_details": {"email": "care@luckyshrub.example"}
            }))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn set_checks_the_documented_rules_first() {
        let t = ScriptedTransport::new();
        let pn = client(&t).phone_number(ID);
        let base = |ty| ComplianceInfoUpdate::new("Lucky Shrub", ty, officer(), care());
        let mut no_officer_name = base(BusinessEntityType::PublicCompany);
        no_officer_name.grievance_officer_details.name = None;
        let mut blank_officer_email = base(BusinessEntityType::PublicCompany);
        blank_officer_email.grievance_officer_details.email = Some(" ".into());
        let mut no_care_email = base(BusinessEntityType::PublicCompany);
        no_care_email.customer_care_details.email = None;
        let cases = [
            (
                ComplianceInfoUpdate::new(
                    "L",
                    BusinessEntityType::PublicCompany,
                    officer(),
                    care(),
                ),
                "entity_name",
            ),
            (
                ComplianceInfoUpdate::new(
                    "L".repeat(129),
                    BusinessEntityType::PublicCompany,
                    officer(),
                    care(),
                ),
                "entity_name",
            ),
            // The name is sent as given: 128 characters and a space are 129.
            (
                ComplianceInfoUpdate::new(
                    format!("{} ", "L".repeat(128)),
                    BusinessEntityType::PublicCompany,
                    officer(),
                    care(),
                ),
                "entity_name",
            ),
            // Long enough, but blank past one character.
            (
                ComplianceInfoUpdate::new(
                    " L ",
                    BusinessEntityType::PublicCompany,
                    officer(),
                    care(),
                ),
                "entity_name",
            ),
            (base(BusinessEntityType::Custom), "entity_type_custom"),
            (
                base(BusinessEntityType::Custom).entity_type_custom(" "),
                "entity_type_custom",
            ),
            (
                base(BusinessEntityType::SoleProprietorship).entity_type_custom("Trust"),
                "entity_type_custom",
            ),
            (
                base(BusinessEntityType::PrivateCompany).is_registered(true),
                "is_registered",
            ),
            (no_officer_name, "grievance_officer_details.name"),
            (blank_officer_email, "grievance_officer_details.email"),
            (no_care_email, "customer_care_details.email"),
        ];
        for (update, field) in cases {
            let err = pn.set_business_compliance_info(&update).await.unwrap_err();
            assert!(
                matches!(&err, meta_whatsapp_core::Error::Validation(v) if v.field == field),
                "{field}: {err:?}"
            );
        }
        assert!(t.requests().is_empty(), "nothing is sent for invalid input");
        assert!(
            base(BusinessEntityType::Partnership)
                .is_registered(false)
                .validate()
                .is_ok()
        );
        assert!(
            ComplianceInfoUpdate::new(
                "L".repeat(128),
                BusinessEntityType::PublicCompany,
                officer(),
                care()
            )
            .validate()
            .is_ok()
        );
    }

    /// "Email addresses must be valid format and under 128 characters":
    /// the length is checked here, 128 refused and 127 accepted.
    #[tokio::test]
    async fn set_checks_the_email_length_first() {
        let t = ScriptedTransport::new();
        let pn = client(&t).phone_number(ID);
        let email = |chars: usize| format!("{}@x.example", "a".repeat(chars - "@x.example".len()));
        assert_eq!(email(128).chars().count(), 128);
        let update = |officer_email: String, care_email: String| {
            ComplianceInfoUpdate::new(
                "Lucky Shrub",
                BusinessEntityType::PublicCompany,
                GrievanceOfficer::new("Asha Rao", officer_email),
                CustomerCare::new(care_email),
            )
        };
        for (update, field) in [
            (
                update(email(128), email(127)),
                "grievance_officer_details.email",
            ),
            (
                update(email(127), email(128)),
                "customer_care_details.email",
            ),
        ] {
            let err = pn.set_business_compliance_info(&update).await.unwrap_err();
            assert!(
                matches!(&err, meta_whatsapp_core::Error::Validation(v) if v.field == field),
                "{field}: {err:?}"
            );
        }
        assert!(t.requests().is_empty(), "nothing is sent for invalid input");
        assert!(update(email(127), email(127)).validate().is_ok());
    }

    /// What the rustdoc says: setting the information is replayed after a
    /// timeout (the same body), unlike a non-idempotent `POST`.
    #[tokio::test]
    async fn set_is_replayed_after_a_timeout() {
        let t = ScriptedTransport::new();
        t.push_error(|| meta_whatsapp_core::error::TransportError::Timeout);
        t.push_json(200, json!({"success": true}));
        let c = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy {
                max_retries: 1,
                base_delay: std::time::Duration::ZERO,
                max_delay: std::time::Duration::ZERO,
            })
            .build()
            .unwrap();
        c.phone_number(ID)
            .set_business_compliance_info(&ComplianceInfoUpdate::new(
                "Lucky Shrub Private Limited",
                BusinessEntityType::PrivateCompany,
                officer(),
                care(),
            ))
            .await
            .unwrap();
        let reqs = t.requests();
        assert_eq!(reqs.len(), 2, "setting a value is safe to replay");
        assert_eq!(reqs[0].json(), reqs[1].json());
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn get_passes_fields_and_parses_the_schema() {
        // The page has no response example: this is its schema's fields.
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{
                "whatsapp_business_account_id": "102290129340398",
                "messaging_product": "whatsapp",
                "entity_name": "Lucky Shrub",
                "entity_type": "Partnership",
                "is_registered": true,
                "grievance_officer_details": {
                    "name": "Asha Rao",
                    "email": "grievance@luckyshrub.example",
                    "mobile_number": "+919876543210"
                },
                "customer_care_details": {"email": "care@luckyshrub.example", "landline_number": "+912287654321"},
                "a_new_field": 1
            }]}),
        );
        let got = client(&t)
            .phone_number(ID)
            .business_compliance_info(&["entity_name", "entity_type"])
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(
            req.path(),
            "/v25.0/106540352242922/business_compliance_info"
        );
        assert_eq!(
            req.query("fields").as_deref(),
            Some("entity_name,entity_type")
        );
        assert_eq!(got.len(), 1);
        let info = &got[0];
        assert_eq!(
            info.whatsapp_business_account_id,
            Some(WabaId::new("102290129340398"))
        );
        assert_eq!(info.entity_type.as_deref(), Some("Partnership"));
        assert_eq!(info.is_registered, Some(true));
        assert_eq!(
            info.grievance_officer_details
                .as_ref()
                .and_then(|g| g.name.as_deref()),
            Some("Asha Rao")
        );
        assert_eq!(
            info.customer_care_details
                .as_ref()
                .and_then(|c| c.landline_number.as_deref()),
            Some("+912287654321")
        );
        assert_eq!(t.remaining(), 0);

        t.push_json(200, json!({"data": []}));
        assert!(
            client(&t)
                .phone_number(ID)
                .business_compliance_info(&[])
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(t.last_request().unwrap().query("fields"), None);
        assert_eq!(t.remaining(), 0);
    }

    #[test]
    fn entity_types_keep_unknown_values() {
        assert_eq!(
            serde_json::to_value(BusinessEntityType::Custom).unwrap(),
            json!("OTHER")
        );
        assert_eq!(
            serde_json::from_value::<BusinessEntityType>(json!("OTHER")).unwrap(),
            BusinessEntityType::Custom
        );
        assert_eq!(
            serde_json::from_value::<BusinessEntityType>(json!("TRUST")).unwrap(),
            BusinessEntityType::Other("TRUST".into())
        );
    }
}
