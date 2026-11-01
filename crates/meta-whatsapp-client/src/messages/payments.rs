//! Payments API interactive messages: `order_details` (ask for a payment)
//! and `order_status` (tell the buyer where the order stands). Re-exported
//! from [`crate::payments`], where the payment lookup, refunds and the
//! payment configurations live.
//!
//! Pages (India): `payments/payments-in/upi-intent/dynamic-vpa` (UPI
//! intent links), `payments/payments-in/payment-links`,
//! `payments/payments-in/pg` (payment gateways) and
//! `payments/payments-in/upi-intent` (the older payment configuration
//! method, `payment_configuration` instead of `payment_settings`).
//!
//! What the pages disagree on, and what is checked locally: the
//! [`crate::payments`] module docs.

use std::collections::BTreeMap;

use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::CatalogId;
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};

use super::interactive::{FOOTER_MAX, TextObject};
use super::validate::{self, Check};
use crate::common::MediaSource;
use crate::templates::macros::string_enum;

/// Body text limit of `order_details` and `order_status` ("Maximum length
/// is 1024 characters" on every payments page).
const BODY_MAX: usize = 1024;
/// `reference_id` limit in India ("should not exceed 35 characters").
const REFERENCE_ID_MAX_INR: usize = 35;
/// `configuration_name` and `payment_configuration` limit ("must not
/// exceed 60 characters", `payments-in/pg`, `payments-in/upi-intent`).
pub(crate) const CONFIGURATION_NAME_MAX: usize = 60;
/// Item name limit ("Cannot exceed 60 characters").
const ITEM_NAME_MAX: usize = 60;
/// Tax, shipping and discount `description`, `discount_program_name`
/// ("Max character limit is 60 characters").
const LINE_DESCRIPTION_MAX: usize = 60;
/// Expiration and order status `description` ("Max character limit is 120
/// characters").
const STATUS_DESCRIPTION_MAX: usize = 120;
/// Items with a custom `image` ("will limit the items array to a maximum
/// of 10 items").
const ITEMS_WITH_IMAGE_MAX: usize = 10;

const PARAMS: &str = "interactive.action.parameters";

// ─── Money ───────────────────────────────────────────────────────────────

/// An amount of money: `value` is the amount multiplied by `offset`
/// (₹12.34 is `value` 1234, `offset` 100). Every payments page writes
/// amounts this way, and requires `offset` 100 for INR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Amount {
    /// The amount times `offset`.
    pub value: u64,
    /// The divisor of `value`: `100` for INR.
    pub offset: u32,
}

impl Amount {
    /// `value` divided by `offset`.
    pub fn new(value: u64, offset: u32) -> Self {
        Self { value, offset }
    }

    /// An INR amount in paise (`offset` 100): `Amount::inr(1234)` is ₹12.34.
    pub fn inr(paise: u64) -> Self {
        Self::new(paise, 100)
    }

    pub(crate) fn validate(self, field: &str, currency: Currency) -> Check {
        let offset = currency.offset();
        if self.offset != offset {
            return Err(ValidationError::new(
                format!("{field}.offset"),
                format!(
                    "is {}; it must be {offset} for {}",
                    self.offset,
                    currency.as_str()
                ),
            ));
        }
        Ok(())
    }
}

/// An amount with an optional `description`: an order's `tax` and
/// `shipping`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DescribedAmount {
    /// The amount.
    #[serde(flatten)]
    pub amount: Amount,
    /// Shown with the line; at most 60 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl DescribedAmount {
    /// `amount` without a description.
    pub fn new(amount: Amount) -> Self {
        Self {
            amount,
            description: None,
        }
    }

    /// Set the description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    fn validate(&self, field: &str, currency: Currency) -> Check {
        self.amount.validate(field, currency)?;
        validate::opt_text(
            &format!("{field}.description"),
            self.description.as_deref(),
            LINE_DESCRIPTION_MAX,
        )
    }
}

/// An order's `discount`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Discount {
    /// The amount taken off.
    #[serde(flatten)]
    pub amount: Amount,
    /// Shown with the line; at most 60 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The incentive program, for an incentivized order (which must name
    /// it); at most 60 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discount_program_name: Option<String>,
}

impl Discount {
    /// `amount` off, without a description.
    pub fn new(amount: Amount) -> Self {
        Self {
            amount,
            description: None,
            discount_program_name: None,
        }
    }

    /// Set the description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set `discount_program_name`.
    #[must_use]
    pub fn program_name(mut self, name: impl Into<String>) -> Self {
        self.discount_program_name = Some(name.into());
        self
    }

    fn validate(&self, field: &str, currency: Currency) -> Check {
        self.amount.validate(field, currency)?;
        validate::opt_text(
            &format!("{field}.description"),
            self.description.as_deref(),
            LINE_DESCRIPTION_MAX,
        )?;
        validate::opt_text(
            &format!("{field}.discount_program_name"),
            self.discount_program_name.as_deref(),
            LINE_DESCRIPTION_MAX,
        )
    }
}

/// The currency of an order. A closed set: the client checks the rules of
/// each currency it knows (only `INR` today, the India pages' "the only
/// supported value"), and cannot send another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Currency {
    /// `INR`, Indian rupee: amounts in paise (`offset` 100).
    Inr,
}

impl Currency {
    /// The ISO 4217 code sent.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inr => "INR",
        }
    }

    /// The `offset` Meta requires for this currency.
    pub fn offset(self) -> u32 {
        match self {
            Self::Inr => 100,
        }
    }
}

impl Serialize for Currency {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

string_enum! {
    /// What the order pays for (`parameters.type`).
    pub enum GoodsType {
        /// `digital-goods`.
        Digital => "digital-goods",
        /// `physical-goods`: shipped goods need
        /// [`OrderDetailsParameters::beneficiaries`].
        Physical => "physical-goods",
    }
}

string_enum! {
    /// `parameters.payment_type`. Required (`upi`) by the India payment
    /// links page and the older payment configuration method; the dynamic
    /// UPI intent and payment gateway pages do not send it.
    pub enum PaymentType {
        /// `upi`.
        Upi => "upi",
    }
}

// ─── Payment settings ────────────────────────────────────────────────────

/// One entry of `parameters.payment_settings`: how the buyer pays.
/// Serializes to `{"type": T, T: {…}}`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PaymentSetting {
    /// `upi_intent_link`: the UPI intent your payment gateway generated
    /// (`payments-in/upi-intent/dynamic-vpa`).
    UpiIntentLink {
        /// The `upi://pay?…` link. The page lists the attributes Meta
        /// supports in it: `pa`, `pn`, `mc`, `purpose` and `tr`.
        link: String,
    },
    /// `payment_link`: a link your payment gateway generated
    /// (`payments-in/payment-links`; limited access). Needs
    /// [`PaymentType::Upi`].
    PaymentLink(PaymentLink),
    /// `payment_gateway`: checkout through a payment configuration linked
    /// to the WABA (`payments-in/pg`).
    PaymentGateway(PaymentGateway),
}

impl PaymentSetting {
    /// A UPI intent link.
    pub fn upi_intent_link(link: impl Into<String>) -> Self {
        Self::UpiIntentLink { link: link.into() }
    }

    /// A payment link, without success or cancel URLs.
    pub fn payment_link(uri: impl Into<String>) -> Self {
        Self::PaymentLink(PaymentLink::new(uri))
    }

    fn type_name(&self) -> &'static str {
        match self {
            Self::UpiIntentLink { .. } => "upi_intent_link",
            Self::PaymentLink(_) => "payment_link",
            Self::PaymentGateway(_) => "payment_gateway",
        }
    }

    fn validate(&self, field: &str) -> Check {
        match self {
            Self::UpiIntentLink { link } => {
                validate::non_empty(&format!("{field}.upi_intent_link.link"), link)
            }
            Self::PaymentLink(l) => l.validate(&format!("{field}.payment_link")),
            Self::PaymentGateway(g) => g.validate(&format!("{field}.payment_gateway")),
        }
    }
}

impl Serialize for PaymentSetting {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Link<'a> {
            link: &'a str,
        }
        let kind = self.type_name();
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("type", kind)?;
        match self {
            Self::UpiIntentLink { link } => map.serialize_entry(kind, &Link { link })?,
            Self::PaymentLink(l) => map.serialize_entry(kind, l)?,
            Self::PaymentGateway(g) => map.serialize_entry(kind, g)?,
        }
        map.end()
    }
}

impl From<PaymentLink> for PaymentSetting {
    fn from(l: PaymentLink) -> Self {
        Self::PaymentLink(l)
    }
}

impl From<PaymentGateway> for PaymentSetting {
    fn from(g: PaymentGateway) -> Self {
        Self::PaymentGateway(g)
    }
}

/// The `payment_link` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PaymentLink {
    /// The link; its domain must be enabled by Meta.
    pub uri: String,
    /// Reaching it ends the flow as a success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success_url: Option<String>,
    /// Reaching it ends the flow as a failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_url: Option<String>,
}

impl PaymentLink {
    /// A link without success or cancel URLs.
    pub fn new(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            success_url: None,
            cancel_url: None,
        }
    }

    /// Set `success_url`.
    #[must_use]
    pub fn success_url(mut self, url: impl Into<String>) -> Self {
        self.success_url = Some(url.into());
        self
    }

    /// Set `cancel_url`.
    #[must_use]
    pub fn cancel_url(mut self, url: impl Into<String>) -> Self {
        self.cancel_url = Some(url.into());
        self
    }

    fn validate(&self, field: &str) -> Check {
        validate::non_empty(&format!("{field}.uri"), &self.uri)?;
        validate::opt_text(
            &format!("{field}.success_url"),
            self.success_url.as_deref(),
            usize::MAX,
        )?;
        validate::opt_text(
            &format!("{field}.cancel_url"),
            self.cancel_url.as_deref(),
            usize::MAX,
        )
    }
}

string_enum! {
    /// A UPI app to show first (`preferred_payment_methods[].method`;
    /// `payments-in/pg`, "Merchant preferred UPI payment method").
    pub enum UpiApp {
        /// Google Pay.
        GooglePay => "gpay",
        /// `PhonePe`.
        PhonePe => "phonepe",
        /// Paytm.
        Paytm => "paytm",
        /// BHIM.
        Bhim => "bhim",
        /// Amazon Pay.
        AmazonPay => "amazonpay",
        /// CRED.
        Cred => "cred",
        /// `MobiKwik`.
        Mobikwik => "mobikwik",
    }
}

string_enum! {
    /// A checkout option to offer (`enabled_payment_options`;
    /// `payments-in/pg`, "Restrict available payment options").
    pub enum PaymentOption {
        /// `upi`: UPI apps only. UPI is limited to ₹5,00,000 a
        /// transaction; above it, offer `web` only.
        Upi => "upi",
        /// `web`: the gateway's web checkout.
        Web => "web",
    }
}

/// The `payment_gateway` object: checkout through a payment configuration
/// (`payments-in/pg`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentGateway {
    /// The gateway (`type`) and its own fields.
    pub provider: GatewayProvider,
    /// The payment configuration's name, as linked in WhatsApp Manager or
    /// created with
    /// [`PaymentConfigurations::create`](crate::payments::PaymentConfigurations::create);
    /// at most 60 characters. A wrong name leaves the buyer unable to pay.
    pub configuration_name: String,
    /// The UPI app shown first (`preferred_payment_methods`, at most one).
    pub preferred_upi_app: Option<UpiApp>,
    /// The checkout options offered (`enabled_payment_options`); empty
    /// sends none.
    pub enabled_payment_options: Vec<PaymentOption>,
}

impl PaymentGateway {
    /// Checkout through `provider` with the configuration
    /// `configuration_name`.
    pub fn new(provider: GatewayProvider, configuration_name: impl Into<String>) -> Self {
        Self {
            provider,
            configuration_name: configuration_name.into(),
            preferred_upi_app: None,
            enabled_payment_options: Vec::new(),
        }
    }

    /// Show `app` first among the UPI apps.
    #[must_use]
    pub fn preferred_upi_app(mut self, app: UpiApp) -> Self {
        self.preferred_upi_app = Some(app);
        self
    }

    /// Offer only `options`.
    #[must_use]
    pub fn enabled_payment_options(
        mut self,
        options: impl IntoIterator<Item = PaymentOption>,
    ) -> Self {
        self.enabled_payment_options = options.into_iter().collect();
        self
    }

    fn validate(&self, field: &str) -> Check {
        validate::text(
            &format!("{field}.configuration_name"),
            &self.configuration_name,
            CONFIGURATION_NAME_MAX,
        )?;
        self.provider.validate(field)
    }
}

impl Serialize for PaymentGateway {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Method<'a> {
            method: &'a UpiApp,
        }
        let mut map = serializer.serialize_map(None)?;
        let name = self.provider.as_str();
        map.serialize_entry("type", name)?;
        map.serialize_entry("configuration_name", &self.configuration_name)?;
        match &self.provider {
            GatewayProvider::Billdesk(f) if !f.is_empty() => map.serialize_entry(name, f)?,
            GatewayProvider::Razorpay(f) if !f.is_empty() => map.serialize_entry(name, f)?,
            GatewayProvider::Payu(f) if !f.is_empty() => map.serialize_entry(name, f)?,
            GatewayProvider::Zaakpay(f) if !f.is_empty() => map.serialize_entry(name, f)?,
            _ => {}
        }
        if let Some(app) = &self.preferred_upi_app {
            map.serialize_entry("preferred_payment_methods", &[Method { method: app }])?;
        }
        if !self.enabled_payment_options.is_empty() {
            map.serialize_entry("enabled_payment_options", &self.enabled_payment_options)?;
        }
        map.end()
    }
}

/// The payment gateway (`payment_gateway.type`) and the fields it takes,
/// which Meta passes on to the gateway's order and returns with the
/// payment. Fields left `None` (all of them, by default) are not sent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GatewayProvider {
    /// `billdesk`.
    Billdesk(BilldeskFields),
    /// `razorpay`.
    Razorpay(RazorpayFields),
    /// `payu`.
    Payu(PayuFields),
    /// `zaakpay`.
    Zaakpay(ZaakpayFields),
    /// A gateway this version does not know, sent as `type` with no
    /// gateway fields.
    Other(String),
}

impl GatewayProvider {
    /// The `type` value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Billdesk(_) => "billdesk",
            Self::Razorpay(_) => "razorpay",
            Self::Payu(_) => "payu",
            Self::Zaakpay(_) => "zaakpay",
            Self::Other(s) => s,
        }
    }

    fn validate(&self, field: &str) -> Check {
        match self {
            Self::Billdesk(f) => f.validate(&format!("{field}.billdesk")),
            Self::Razorpay(f) => f.validate(&format!("{field}.razorpay")),
            Self::Payu(f) => f.validate(&format!("{field}.payu")),
            Self::Zaakpay(f) => f.validate(&format!("{field}.zaakpay")),
            Self::Other(s) => validate::non_empty(&format!("{field}.type"), s),
        }
    }
}

/// Check each optional field: when set, `1..=max` characters.
fn check_fields(field: &str, fields: &[(&str, Option<&str>)], max: usize) -> Check {
    for (name, value) in fields {
        validate::opt_text(&format!("{field}.{name}"), *value, max)?;
    }
    Ok(())
}

/// `BillDesk`'s `additional_info1` to `additional_info7`, at most 120
/// characters each.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BilldeskFields {
    /// `additional_info1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_info1: Option<String>,
    /// `additional_info2`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_info2: Option<String>,
    /// `additional_info3`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_info3: Option<String>,
    /// `additional_info4`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_info4: Option<String>,
    /// `additional_info5`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_info5: Option<String>,
    /// `additional_info6`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_info6: Option<String>,
    /// `additional_info7`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_info7: Option<String>,
}

impl BilldeskFields {
    fn as_list(&self) -> [(&'static str, Option<&str>); 7] {
        [
            ("additional_info1", self.additional_info1.as_deref()),
            ("additional_info2", self.additional_info2.as_deref()),
            ("additional_info3", self.additional_info3.as_deref()),
            ("additional_info4", self.additional_info4.as_deref()),
            ("additional_info5", self.additional_info5.as_deref()),
            ("additional_info6", self.additional_info6.as_deref()),
            ("additional_info7", self.additional_info7.as_deref()),
        ]
    }

    fn is_empty(&self) -> bool {
        self.as_list().iter().all(|(_, v)| v.is_none())
    }

    fn validate(&self, field: &str) -> Check {
        // payments-in/pg: "Each extra field has a maximum character limit
        // of 120."
        check_fields(field, &self.as_list(), 120)
    }
}

/// Razorpay's `receipt` and `notes`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RazorpayFields {
    /// Your receipt number for the order: 1 to 40 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
    /// Key-value pairs: at most 15 keys, each value at most 256
    /// characters.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, String>,
}

impl RazorpayFields {
    fn is_empty(&self) -> bool {
        self.receipt.is_none() && self.notes.is_empty()
    }

    fn validate(&self, field: &str) -> Check {
        // payments-in/pg: receipt "Maximum length of 40 characters
        // supported with minimum length greater than 0"; notes "maximum 15
        // keys and each value limits to 256 characters".
        validate::opt_text(&format!("{field}.receipt"), self.receipt.as_deref(), 40)?;
        validate::count(&format!("{field}.notes"), self.notes.len(), 0, 15)?;
        for (key, value) in &self.notes {
            validate::max_chars(&format!("{field}.notes.{key}"), value, 256)?;
        }
        Ok(())
    }
}

/// `PayU`'s `udf1` to `udf4`, at most 255 characters each.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PayuFields {
    /// `udf1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udf1: Option<String>,
    /// `udf2`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udf2: Option<String>,
    /// `udf3`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udf3: Option<String>,
    /// `udf4`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udf4: Option<String>,
}

impl PayuFields {
    fn as_list(&self) -> [(&'static str, Option<&str>); 4] {
        [
            ("udf1", self.udf1.as_deref()),
            ("udf2", self.udf2.as_deref()),
            ("udf3", self.udf3.as_deref()),
            ("udf4", self.udf4.as_deref()),
        ]
    }

    fn is_empty(&self) -> bool {
        self.as_list().iter().all(|(_, v)| v.is_none())
    }

    fn validate(&self, field: &str) -> Check {
        // payments-in/pg: "Each UDF field has a maximum character limit of
        // 255."
        check_fields(field, &self.as_list(), 255)
    }
}

/// Zaakpay's `extra1` and `extra2`, at most 180 characters each.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ZaakpayFields {
    /// `extra1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra1: Option<String>,
    /// `extra2`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra2: Option<String>,
}

impl ZaakpayFields {
    fn as_list(&self) -> [(&'static str, Option<&str>); 2] {
        [
            ("extra1", self.extra1.as_deref()),
            ("extra2", self.extra2.as_deref()),
        ]
    }

    fn is_empty(&self) -> bool {
        self.as_list().iter().all(|(_, v)| v.is_none())
    }

    fn validate(&self, field: &str) -> Check {
        // payments-in/pg: "Each extra field has a maximum character limit
        // of 180."
        check_fields(field, &self.as_list(), 180)
    }
}

// ─── The order ───────────────────────────────────────────────────────────

/// A recipient of shipped physical goods (`beneficiaries[]`). Not shown to
/// the buyer; Meta needs it for legal and compliance reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Beneficiary {
    /// Person or business receiving the goods: at most 200 characters.
    pub name: String,
    /// Door or tower number, street…: at most 100 characters.
    pub address_line1: String,
    /// Landmark, area…: at most 100 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address_line2: Option<String>,
    /// City.
    pub city: String,
    /// State.
    pub state: String,
    /// Must be `India`.
    pub country: String,
    /// The 6-digit PIN code.
    pub postal_code: String,
}

impl Beneficiary {
    /// A beneficiary in India.
    pub fn new(
        name: impl Into<String>,
        address_line1: impl Into<String>,
        city: impl Into<String>,
        state: impl Into<String>,
        postal_code: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            address_line1: address_line1.into(),
            address_line2: None,
            city: city.into(),
            state: state.into(),
            country: "India".into(),
            postal_code: postal_code.into(),
        }
    }

    /// Set `address_line2`.
    #[must_use]
    pub fn address_line2(mut self, line: impl Into<String>) -> Self {
        self.address_line2 = Some(line.into());
        self
    }

    fn validate(&self, field: &str) -> Check {
        validate::text(&format!("{field}.name"), &self.name, 200)?;
        validate::text(&format!("{field}.address_line1"), &self.address_line1, 100)?;
        validate::opt_text(
            &format!("{field}.address_line2"),
            self.address_line2.as_deref(),
            100,
        )?;
        validate::non_empty(&format!("{field}.city"), &self.city)?;
        validate::non_empty(&format!("{field}.state"), &self.state)?;
        if self.country != "India" {
            return Err(ValidationError::new(
                format!("{field}.country"),
                "must be `India`",
            ));
        }
        if self.postal_code.len() != 6 || !self.postal_code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ValidationError::new(
                format!("{field}.postal_code"),
                "must be a 6-digit PIN code",
            ));
        }
        Ok(())
    }
}

/// An item's `importer_address`, as the examples send it (the tables call
/// it a string; see the [module docs](crate::payments#where-the-pages-disagree)). No
/// page says which of its fields are required: the template example
/// leaves out `address_line2` and `zone_code`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImporterAddress {
    /// `address_line1`.
    pub address_line1: String,
    /// `address_line2`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address_line2: Option<String>,
    /// `city`.
    pub city: String,
    /// The state's code (`DL` in the examples).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone_code: Option<String>,
    /// `postal_code`.
    pub postal_code: String,
    /// The country's ISO code (`IN` in the examples).
    pub country_code: String,
}

impl ImporterAddress {
    /// An address without `address_line2` or `zone_code`.
    pub fn new(
        address_line1: impl Into<String>,
        city: impl Into<String>,
        postal_code: impl Into<String>,
        country_code: impl Into<String>,
    ) -> Self {
        Self {
            address_line1: address_line1.into(),
            address_line2: None,
            city: city.into(),
            zone_code: None,
            postal_code: postal_code.into(),
            country_code: country_code.into(),
        }
    }

    /// Set `address_line2`.
    #[must_use]
    pub fn address_line2(mut self, line: impl Into<String>) -> Self {
        self.address_line2 = Some(line.into());
        self
    }

    /// Set `zone_code`.
    #[must_use]
    pub fn zone_code(mut self, code: impl Into<String>) -> Self {
        self.zone_code = Some(code.into());
        self
    }
}

/// An item's custom `image`: a JPEG or PNG link (8-bit, RGB or RGBA).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemImage {
    /// The image's link.
    pub link: String,
}

/// One of `order.items`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderItem {
    /// The item's content id in your catalog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retailer_id: Option<String>,
    /// Shown to the buyer: at most 60 characters.
    pub name: String,
    /// A custom image. An order with one has at most 10 items, and no
    /// `catalog_id`; the item has no `retailer_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<ItemImage>,
    /// The price of one.
    pub amount: Amount,
    /// The discounted price of one, below `amount`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sale_amount: Option<Amount>,
    /// How many.
    pub quantity: u32,
    /// Required without a `catalog_id` (India).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_of_origin: Option<String>,
    /// Required without a `catalog_id` (India).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importer_name: Option<String>,
    /// Required without a `catalog_id` (India).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importer_address: Option<ImporterAddress>,
}

impl OrderItem {
    /// `quantity` of `name` at `amount` each.
    pub fn new(name: impl Into<String>, amount: Amount, quantity: u32) -> Self {
        Self {
            retailer_id: None,
            name: name.into(),
            image: None,
            amount,
            sale_amount: None,
            quantity,
            country_of_origin: None,
            importer_name: None,
            importer_address: None,
        }
    }

    /// Set the catalog `retailer_id`.
    #[must_use]
    pub fn retailer_id(mut self, id: impl Into<String>) -> Self {
        self.retailer_id = Some(id.into());
        self
    }

    /// Set a custom image.
    #[must_use]
    pub fn image_link(mut self, link: impl Into<String>) -> Self {
        self.image = Some(ItemImage { link: link.into() });
        self
    }

    /// Set the discounted price.
    #[must_use]
    pub fn sale_amount(mut self, amount: Amount) -> Self {
        self.sale_amount = Some(amount);
        self
    }

    /// Set the country of origin, importer and importer address, which an
    /// order without a `catalog_id` needs (India).
    #[must_use]
    pub fn import(
        mut self,
        country_of_origin: impl Into<String>,
        importer_name: impl Into<String>,
        importer_address: ImporterAddress,
    ) -> Self {
        self.country_of_origin = Some(country_of_origin.into());
        self.importer_name = Some(importer_name.into());
        self.importer_address = Some(importer_address);
        self
    }

    fn validate(&self, field: &str, currency: Currency, has_catalog: bool) -> Check {
        validate::text(&format!("{field}.name"), &self.name, ITEM_NAME_MAX)?;
        if let Some(id) = &self.retailer_id {
            validate::non_empty(&format!("{field}.retailer_id"), id)?;
        }
        if let Some(image) = &self.image {
            validate::non_empty(&format!("{field}.image.link"), &image.link)?;
            if self.retailer_id.is_some() {
                return Err(ValidationError::new(
                    format!("{field}.image"),
                    "an item with a custom image cannot have a `retailer_id`",
                ));
            }
        }
        self.amount.validate(&format!("{field}.amount"), currency)?;
        if let Some(sale) = self.sale_amount {
            sale.validate(&format!("{field}.sale_amount"), currency)?;
        }
        if !has_catalog {
            // dynamic-vpa, payment-links, pg: "Required if `catalog_id` is
            // not present."
            let country = format!("{field}.country_of_origin");
            validate::non_empty(&country, self.country_of_origin.as_deref().unwrap_or(""))?;
            let importer = format!("{field}.importer_name");
            validate::non_empty(&importer, self.importer_name.as_deref().unwrap_or(""))?;
            if self.importer_address.is_none() {
                return Err(ValidationError::new(
                    format!("{field}.importer_address"),
                    "is required when the order has no `catalog_id`",
                ));
            }
        }
        Ok(())
    }
}

/// `order.expiration`: after it the buyer cannot pay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderExpiration {
    /// When, in unix seconds (UTC), sent as a string. At least 300 seconds
    /// ahead (not checked locally: it is relative to Meta's clock).
    #[serde(serialize_with = "u64_as_string")]
    pub timestamp: u64,
    /// Shown to the buyer: at most 120 characters.
    pub description: String,
}

impl OrderExpiration {
    /// Expire at `timestamp` (unix seconds), explained by `description`.
    pub fn new(timestamp: u64, description: impl Into<String>) -> Self {
        Self {
            timestamp,
            description: description.into(),
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `serialize_with` signature
fn u64_as_string<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}

/// `parameters.order`: what is paid for. Its `status` is always `pending`
/// in an `order_details` message, and is sent as such.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    /// At least one item.
    pub items: Vec<OrderItem>,
    /// Sum of the items' prices times their quantities (not checked
    /// locally, see the [module docs](crate::payments#what-is-checked-locally)).
    pub subtotal: Amount,
    /// Required, even when zero.
    pub tax: DescribedAmount,
    /// Shipping cost.
    pub shipping: Option<DescribedAmount>,
    /// Discount.
    pub discount: Option<Discount>,
    /// The catalog the items' `retailer_id`s are from. Without it, every
    /// item needs [`OrderItem::import`] (India).
    pub catalog_id: Option<CatalogId>,
    /// When the order expires.
    pub expiration: Option<OrderExpiration>,
    /// `type: quick_pay`: show only the "Pay Now" button, not "Review and
    /// Pay".
    pub quick_pay: bool,
}

impl Order {
    /// An order of `items`.
    pub fn new(
        items: impl IntoIterator<Item = OrderItem>,
        subtotal: Amount,
        tax: DescribedAmount,
    ) -> Self {
        Self {
            items: items.into_iter().collect(),
            subtotal,
            tax,
            shipping: None,
            discount: None,
            catalog_id: None,
            expiration: None,
            quick_pay: false,
        }
    }

    /// Set the shipping cost.
    #[must_use]
    pub fn shipping(mut self, shipping: DescribedAmount) -> Self {
        self.shipping = Some(shipping);
        self
    }

    /// Set the discount.
    #[must_use]
    pub fn discount(mut self, discount: Discount) -> Self {
        self.discount = Some(discount);
        self
    }

    /// Set the catalog.
    #[must_use]
    pub fn catalog_id(mut self, id: impl Into<CatalogId>) -> Self {
        self.catalog_id = Some(id.into());
        self
    }

    /// Set the expiration.
    #[must_use]
    pub fn expiration(mut self, expiration: OrderExpiration) -> Self {
        self.expiration = Some(expiration);
        self
    }

    /// Send `type: quick_pay`.
    #[must_use]
    pub fn quick_pay(mut self) -> Self {
        self.quick_pay = true;
        self
    }

    /// `subtotal + tax + shipping - discount`, or `None` below zero.
    pub fn total(&self) -> Option<u64> {
        let plus = u128::from(self.subtotal.value)
            + u128::from(self.tax.amount.value)
            + self
                .shipping
                .as_ref()
                .map_or(0, |s| u128::from(s.amount.value));
        let minus = self
            .discount
            .as_ref()
            .map_or(0, |d| u128::from(d.amount.value));
        plus.checked_sub(minus).and_then(|t| u64::try_from(t).ok())
    }

    fn validate(&self, field: &str, currency: Currency) -> Check {
        let items = format!("{field}.items");
        validate::count(&items, self.items.len(), 1, usize::MAX)?;
        let has_catalog = self.catalog_id.is_some();
        if let Some(id) = &self.catalog_id {
            validate::non_empty(&format!("{field}.catalog_id"), id.as_str())?;
        }
        if self.items.iter().any(|i| i.image.is_some()) {
            // "Using this image field will limit the items array to a
            // maximum of 10 items and this cannot be used with
            // `retailer_id` or `catalog_id`."
            validate::count(&items, self.items.len(), 1, ITEMS_WITH_IMAGE_MAX)?;
            if has_catalog {
                return Err(ValidationError::new(
                    format!("{field}.catalog_id"),
                    "an order with custom item images cannot have a `catalog_id`",
                ));
            }
        }
        for (i, item) in self.items.iter().enumerate() {
            item.validate(&format!("{items}[{i}]"), currency, has_catalog)?;
        }
        self.subtotal
            .validate(&format!("{field}.subtotal"), currency)?;
        self.tax.validate(&format!("{field}.tax"), currency)?;
        if let Some(s) = &self.shipping {
            s.validate(&format!("{field}.shipping"), currency)?;
        }
        if let Some(d) = &self.discount {
            d.validate(&format!("{field}.discount"), currency)?;
        }
        if let Some(e) = &self.expiration {
            validate::text(
                &format!("{field}.expiration.description"),
                &e.description,
                STATUS_DESCRIPTION_MAX,
            )?;
        }
        Ok(())
    }
}

impl Serialize for Order {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("status", "pending")?;
        if self.quick_pay {
            map.serialize_entry("type", "quick_pay")?;
        }
        if let Some(id) = &self.catalog_id {
            map.serialize_entry("catalog_id", id)?;
        }
        if let Some(e) = &self.expiration {
            map.serialize_entry("expiration", e)?;
        }
        map.serialize_entry("items", &self.items)?;
        map.serialize_entry("subtotal", &self.subtotal)?;
        map.serialize_entry("tax", &self.tax)?;
        if let Some(s) = &self.shipping {
            map.serialize_entry("shipping", s)?;
        }
        if let Some(d) = &self.discount {
            map.serialize_entry("discount", d)?;
        }
        map.end()
    }
}

// ─── order_details ───────────────────────────────────────────────────────

/// `action.parameters` of an `order_details` message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderDetailsParameters {
    /// Your unique id for this message's order: payment statuses, the
    /// lookup and `order_status` messages name it. Case-sensitive, English
    /// letters, digits, `_`, `-` and `.`; at most 35 characters in India.
    /// Unique per `order_details` message: add a sequence number to send
    /// another one for the same order (`BM345A-12`).
    pub reference_id: String,
    /// What is paid for.
    #[serde(rename = "type")]
    pub goods_type: GoodsType,
    /// Required (`upi`) with a payment link or a `payment_configuration`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_type: Option<PaymentType>,
    /// The older payment configuration method (`payments-in/upi-intent`):
    /// a configuration's name instead of `payment_settings`; at most 60
    /// characters. Meta asks to move to `payment_settings`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_configuration: Option<String>,
    /// How the buyer pays; required unless `payment_configuration` is set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub payment_settings: Vec<PaymentSetting>,
    /// Who receives shipped physical goods.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub beneficiaries: Vec<Beneficiary>,
    /// The order's currency.
    pub currency: Currency,
    /// What the buyer pays: `subtotal + tax + shipping - discount`, above
    /// zero.
    pub total_amount: Amount,
    /// The order; required in India.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Order>,
}

impl OrderDetailsParameters {
    /// An INR order paid through `payment_setting`. Its `total_amount` is
    /// computed from `order` ([`Order::total`]), so set the order's
    /// shipping and discount first. Add more settings with
    /// [`Self::payment_setting`].
    pub fn inr(
        reference_id: impl Into<String>,
        goods_type: GoodsType,
        payment_setting: impl Into<PaymentSetting>,
        order: Order,
    ) -> Self {
        let total = order.total().unwrap_or(0);
        Self {
            reference_id: reference_id.into(),
            goods_type,
            payment_type: None,
            payment_configuration: None,
            payment_settings: vec![payment_setting.into()],
            beneficiaries: Vec::new(),
            currency: Currency::Inr,
            total_amount: Amount::inr(total),
            order: Some(order),
        }
    }

    /// Add a payment setting.
    #[must_use]
    pub fn payment_setting(mut self, setting: impl Into<PaymentSetting>) -> Self {
        self.payment_settings.push(setting.into());
        self
    }

    /// Set `payment_type`.
    #[must_use]
    pub fn payment_type(mut self, payment_type: PaymentType) -> Self {
        self.payment_type = Some(payment_type);
        self
    }

    /// Add a beneficiary.
    #[must_use]
    pub fn beneficiary(mut self, beneficiary: Beneficiary) -> Self {
        self.beneficiaries.push(beneficiary);
        self
    }

    fn validate(&self) -> Check {
        let currency = self.currency;
        check_reference_id(&self.reference_id, REFERENCE_ID_MAX_INR)?;
        if let Some(name) = &self.payment_configuration {
            validate::text(
                &format!("{PARAMS}.payment_configuration"),
                name,
                CONFIGURATION_NAME_MAX,
            )?;
        }
        if self.payment_settings.is_empty() && self.payment_configuration.is_none() {
            return Err(ValidationError::new(
                format!("{PARAMS}.payment_settings"),
                "is required (or the older `payment_configuration`)",
            ));
        }
        for (i, s) in self.payment_settings.iter().enumerate() {
            s.validate(&format!("{PARAMS}.payment_settings[{i}]"))?;
        }
        let needs_upi = self.payment_configuration.is_some()
            || self
                .payment_settings
                .iter()
                .any(|s| matches!(s, PaymentSetting::PaymentLink(_)));
        if needs_upi && self.payment_type != Some(PaymentType::Upi) {
            // payments-in/payment-links and payments-in/upi-intent:
            // `payment_type` "Required. Must be `upi`".
            return Err(ValidationError::new(
                format!("{PARAMS}.payment_type"),
                "must be `upi` with a payment link or a `payment_configuration`",
            ));
        }
        for (i, b) in self.beneficiaries.iter().enumerate() {
            b.validate(&format!("{PARAMS}.beneficiaries[{i}]"))?;
        }
        let total = format!("{PARAMS}.total_amount");
        self.total_amount.validate(&total, currency)?;
        if self.total_amount.value == 0 {
            return Err(ValidationError::new(
                format!("{total}.value"),
                "must be a positive integer",
            ));
        }
        let Some(order) = &self.order else {
            return Err(ValidationError::new(
                format!("{PARAMS}.order"),
                "is required for INR",
            ));
        };
        order.validate(&format!("{PARAMS}.order"), currency)?;
        // Every page: "`total_amount.value` must be equal to
        // `order.subtotal.value` + `order.tax.value` +
        // `order.shipping.value` - `order.discount.value`."
        if order.total() != Some(self.total_amount.value) {
            return Err(ValidationError::new(
                format!("{total}.value"),
                format!(
                    "is {}; it must equal subtotal + tax + shipping - discount ({})",
                    self.total_amount.value,
                    order
                        .total()
                        .map_or_else(|| "negative".to_owned(), |t| t.to_string()),
                ),
            ));
        }
        Ok(())
    }
}

/// `reference_id`: "cannot be an empty string and can only contain
/// English letters, numbers, underscores, dashes, or dots".
fn check_reference_id(id: &str, max: usize) -> Check {
    let field = format!("{PARAMS}.reference_id");
    validate::text(&field, id, max)?;
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err(ValidationError::new(
            field,
            "may only contain English letters, digits, `_`, `-` and `.`",
        ));
    }
    Ok(())
}

/// An `order_details` message: a bill the buyer pays in WhatsApp
/// (Payments API). Send it as `OutboundMessage::new(to, details)`.
///
/// ```
/// # fn main() -> Result<(), meta_whatsapp_core::error::ValidationError> {
/// use meta_whatsapp_client::payments::{
///     Amount, DescribedAmount, GoodsType, Order, OrderDetails, OrderDetailsParameters,
///     OrderItem, PaymentSetting,
/// };
/// use meta_whatsapp_core::ids::CatalogId;
///
/// let order = Order::new(
///     [OrderItem::new("Bread", Amount::inr(10000), 2).retailer_id("SKU-1")],
///     Amount::inr(20000),
///     DescribedAmount::new(Amount::inr(1000)),
/// )
/// .catalog_id(CatalogId::new("1234567890"));
/// let details = OrderDetails::new(
///     "Your order is ready",
///     OrderDetailsParameters::inr(
///         "BM345A-12",
///         GoodsType::Physical,
///         PaymentSetting::upi_intent_link("upi://pay?pa=shop@psp&pn=Shop&mc=5399&purpose=00&tr=877376394"),
///         order,
///     ),
/// );
/// assert_eq!(details.parameters.total_amount, Amount::inr(21000));
/// details.validate()?;
/// # Ok(()) }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderDetails {
    /// An image header; without one, Meta shows the first product's image.
    pub header: Option<MediaSource>,
    /// Body text: at most 1024 characters.
    pub body: String,
    /// Footer text: at most 60 characters.
    pub footer: Option<String>,
    /// `action.parameters` (the action's `name`, `review_and_pay`, is
    /// added when serializing).
    pub parameters: OrderDetailsParameters,
}

impl OrderDetails {
    /// `body` with `parameters`, no header or footer.
    pub fn new(body: impl Into<String>, parameters: OrderDetailsParameters) -> Self {
        Self {
            header: None,
            body: body.into(),
            footer: None,
            parameters,
        }
    }

    /// Set the image header.
    #[must_use]
    pub fn header_image(mut self, image: impl Into<MediaSource>) -> Self {
        self.header = Some(image.into());
        self
    }

    /// Set the footer.
    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    /// Check what the pages require, without sending: the body (1 to 1024
    /// characters) and footer (1 to 60); the `reference_id` (1 to 35
    /// characters among letters, digits, `_`, `-`, `.`); a payment setting
    /// or a `payment_configuration` (1 to 60), and `payment_type` `upi`
    /// with a payment link or a configuration; each setting's link, URI or
    /// configuration name (1 to 60) and gateway fields (`BillDesk` 120,
    /// Razorpay receipt 40 and 15 notes of 256, `PayU` 255, Zaakpay 180);
    /// beneficiaries (name 200, address lines 100, city, state, `India`, a
    /// 6-digit PIN code); every amount's `offset` (100 for INR); a
    /// positive total equal to `subtotal + tax + shipping - discount`; the
    /// order (required in India), with at least one item, names of 1 to 60
    /// characters, an item's origin and importer when there is no
    /// `catalog_id`, custom images on at most 10 items, without
    /// `retailer_id` or `catalog_id`, and descriptions (60, expiration
    /// 120).
    ///
    /// [`Messages::send`](super::Messages::send) calls it first.
    ///
    /// # Errors
    ///
    /// A [`ValidationError`] naming the JSON path of the first value that
    /// breaks a rule.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if let Some(h) = &self.header {
            h.validate("interactive.header.image")?;
        }
        validate::text("interactive.body.text", &self.body, BODY_MAX)?;
        validate::opt_text(
            "interactive.footer.text",
            self.footer.as_deref(),
            FOOTER_MAX,
        )?;
        self.parameters.validate()
    }
}

impl Serialize for OrderDetails {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct ImageHeader<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            image: &'a MediaSource,
        }
        #[derive(Serialize)]
        struct Action<'a> {
            name: &'static str,
            parameters: &'a OrderDetailsParameters,
        }
        let mut map = serializer.serialize_map(None)?;
        if let Some(image) = &self.header {
            map.serialize_entry(
                "header",
                &ImageHeader {
                    kind: "image",
                    image,
                },
            )?;
        }
        map.serialize_entry("body", &TextObject(&self.body))?;
        if let Some(f) = &self.footer {
            map.serialize_entry("footer", &TextObject(f))?;
        }
        map.serialize_entry(
            "action",
            &Action {
                name: "review_and_pay",
                parameters: &self.parameters,
            },
        )?;
        map.end()
    }
}

// ─── order_status ────────────────────────────────────────────────────────

string_enum! {
    /// An order's status in an `order_status` message. `canceled` and
    /// `completed` are final; `pending` can move to any other;
    /// `processing`, `shipped` and `partially_shipped` move among
    /// themselves or to a final one. Another move is refused with error
    /// `2046` in an error webhook, and canceling a paid order with `2047`.
    pub enum OrderStatusValue {
        /// `pending`: not paid yet.
        Pending => "pending",
        /// `processing`: paid, being fulfilled.
        Processing => "processing",
        /// `partially_shipped`: part of it has shipped.
        PartiallyShipped => "partially_shipped",
        /// `shipped`: all of it has shipped.
        Shipped => "shipped",
        /// `completed`: nothing more to expect (final).
        Completed => "completed",
        /// `canceled`: the buyer can no longer pay (final); refused once
        /// paid.
        Canceled => "canceled",
    }
}

/// `parameters.order` of an `order_status` message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderUpdate {
    /// The new status.
    pub status: OrderStatusValue,
    /// Shown on the order details page (a cancellation's reason, …): at
    /// most 120 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl OrderUpdate {
    /// Move the order to `status`.
    pub fn new(status: OrderStatusValue) -> Self {
        Self {
            status,
            description: None,
        }
    }

    /// Set the description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// An `order_status` message: the new status of an order an
/// `order_details` message created, shown as its own bubble and on the
/// order details page. Meta asks for one after every payment update.
///
/// ```
/// use meta_whatsapp_client::payments::{OrderStatus, OrderStatusValue, OrderUpdate};
///
/// let status = OrderStatus::new(
///     "Your order has shipped",
///     "BM345A-12",
///     OrderUpdate::new(OrderStatusValue::Shipped),
/// );
/// assert!(status.validate().is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderStatus {
    /// Body text: at most 1024 characters.
    pub body: String,
    /// Footer text: at most 60 characters.
    pub footer: Option<String>,
    /// The `reference_id` of the `order_details` message.
    pub reference_id: String,
    /// The new status; required in India.
    pub order: Option<OrderUpdate>,
}

impl OrderStatus {
    /// Move the order `reference_id` to `order`.
    pub fn new(
        body: impl Into<String>,
        reference_id: impl Into<String>,
        order: OrderUpdate,
    ) -> Self {
        Self {
            body: body.into(),
            footer: None,
            reference_id: reference_id.into(),
            order: Some(order),
        }
    }

    /// Set the footer.
    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    /// Check what the pages require, without sending: the body (1 to 1024
    /// characters) and footer (1 to 60), a `reference_id`, and the order's
    /// update (required in India) with a description of 1 to 120
    /// characters.
    ///
    /// # Errors
    ///
    /// A [`ValidationError`] naming the JSON path of the first value that
    /// breaks a rule.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate::text("interactive.body.text", &self.body, BODY_MAX)?;
        validate::opt_text(
            "interactive.footer.text",
            self.footer.as_deref(),
            FOOTER_MAX,
        )?;
        validate::non_empty(&format!("{PARAMS}.reference_id"), &self.reference_id)?;
        let Some(order) = &self.order else {
            return Err(ValidationError::new(
                format!("{PARAMS}.order"),
                "is required",
            ));
        };
        validate::opt_text(
            &format!("{PARAMS}.order.description"),
            order.description.as_deref(),
            STATUS_DESCRIPTION_MAX,
        )
    }
}

impl Serialize for OrderStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Parameters<'a> {
            reference_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            order: Option<&'a OrderUpdate>,
        }
        #[derive(Serialize)]
        struct Action<'a> {
            name: &'static str,
            parameters: Parameters<'a>,
        }
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("body", &TextObject(&self.body))?;
        if let Some(f) = &self.footer {
            map.serialize_entry("footer", &TextObject(f))?;
        }
        map.serialize_entry(
            "action",
            &Action {
                name: "review_order",
                parameters: Parameters {
                    reference_id: &self.reference_id,
                    order: self.order.as_ref(),
                },
            },
        )?;
        map.end()
    }
}
