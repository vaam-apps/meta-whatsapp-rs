//! The order parts only India's checkout button templates document
//! (`payments/payments-in/checkout-button-templates`): the shipping info a
//! template button's order offers the buyer, the address the buyer picks,
//! and the coupon a checkout endpoint applies. Limits are the page's
//! placeholder table's.

use meta_whatsapp_core::error::ValidationError;
use serde::{Deserialize, Serialize};

use super::{Amount, Currency};
use crate::messages::validate::{self, Check};

/// `<SHIPPING_INFO_NAME>`: "Maximum 256 characters".
const NAME_MAX: usize = 256;
/// `<SHIPPING_INFO_PHONE_NUMBER>`: "Maximum 12 characters".
const PHONE_NUMBER_MAX: usize = 12;
/// `<SHIPPING_INFO_ADDRESS>`: "Maximum 512 characters".
const ADDRESS_MAX: usize = 512;
/// `<SHIPPING_INFO_CITY>` and `<SHIPPING_INFO_STATE>`: "Maximum 100
/// characters".
const CITY_STATE_MAX: usize = 100;
/// `<SHIPPING_INFO_INDIA_PIN>`: "Maximum 6 characters".
const PIN_CODE_MAX: usize = 6;
/// `<SHIPPING_INFO_BUILDING_NAME>` and `<SHIPPING_INFO_LANDMARK_AREA>`:
/// "Maximum 128 characters".
const BUILDING_LANDMARK_MAX: usize = 128;
/// `<SHIPPING_INFO_HOUSE_NUMBER>` and `<SHIPPING_INFO_TOWER_NUMBER>`:
/// "Maximum 8 characters".
const HOUSE_TOWER_MAX: usize = 8;

/// `shipping_info` of a checkout button template's order: the addresses
/// offered to the buyer, who may add their own. Physical goods only ("
/// required for physical-goods type, else omit"), in INR.
///
/// The page's table also lists a floor number
/// (`<SHIPPING_INFO_FLOOR_NUMBER>`, at most 10 characters) that no example
/// shows, so its key is unknown: it is not modelled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShippingInfo {
    /// The country, `IN` in every example.
    pub country: String,
    /// The addresses you know the buyer by; empty when you know none ("set
    /// `addresses` to an empty array"). Always sent, even empty.
    #[serde(default)]
    pub addresses: Vec<ShippingAddress>,
    /// The address the buyer picked: in a checkout endpoint's answer to
    /// `apply_shipping` only (the request carries it as
    /// `input.selected_address`). Refused in a send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_address: Option<ShippingAddress>,
}

impl ShippingInfo {
    /// Ship in `country` (`IN`), to one of `addresses` or one the buyer
    /// adds.
    pub fn new(
        country: impl Into<String>,
        addresses: impl IntoIterator<Item = ShippingAddress>,
    ) -> Self {
        Self {
            country: country.into(),
            addresses: addresses.into_iter().collect(),
            selected_address: None,
        }
    }

    /// Ship in India (`IN`), to one of `addresses` or one the buyer adds.
    pub fn india(addresses: impl IntoIterator<Item = ShippingAddress>) -> Self {
        Self::new("IN", addresses)
    }

    /// Set the address the buyer picked (a checkout endpoint's answer to
    /// `apply_shipping`).
    #[must_use]
    pub fn selected_address(mut self, address: ShippingAddress) -> Self {
        self.selected_address = Some(address);
        self
    }

    /// `endpoint`: in a checkout endpoint's exchange, where the selected
    /// address is documented.
    pub(super) fn validate(&self, field: &str, endpoint: bool) -> Check {
        validate::non_empty(&format!("{field}.country"), &self.country)?;
        for (i, a) in self.addresses.iter().enumerate() {
            a.validate(&format!("{field}.addresses[{i}]"))?;
        }
        if let Some(a) = &self.selected_address {
            let at = format!("{field}.selected_address");
            if !endpoint {
                return Err(ValidationError::new(
                    at,
                    "is documented in a checkout endpoint's answer only, not in a send",
                ));
            }
            a.validate(&at)?;
        }
        Ok(())
    }
}

/// One shipping address: the six fields the page requires "if you know
/// the recipient's shipping information", and four optional ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShippingAddress {
    /// The recipient's full name: at most 256 characters.
    pub name: String,
    /// The recipient's WhatsApp phone number: at most 12 characters.
    pub phone_number: String,
    /// The address: at most 512 characters.
    pub address: String,
    /// The city's full name: at most 100 characters.
    pub city: String,
    /// The state's full name: at most 100 characters.
    pub state: String,
    /// The PIN code: at most 6 characters.
    pub in_pin_code: String,
    /// The house number: at most 8 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub house_number: Option<String>,
    /// The tower number: at most 8 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tower_number: Option<String>,
    /// The building's name: at most 128 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub building_name: Option<String>,
    /// The landmark or area: at most 128 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landmark_area: Option<String>,
}

impl ShippingAddress {
    /// An address with the six required fields.
    pub fn new(
        name: impl Into<String>,
        phone_number: impl Into<String>,
        address: impl Into<String>,
        city: impl Into<String>,
        state: impl Into<String>,
        in_pin_code: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            phone_number: phone_number.into(),
            address: address.into(),
            city: city.into(),
            state: state.into(),
            in_pin_code: in_pin_code.into(),
            house_number: None,
            tower_number: None,
            building_name: None,
            landmark_area: None,
        }
    }

    /// Set the house number.
    #[must_use]
    pub fn house_number(mut self, value: impl Into<String>) -> Self {
        self.house_number = Some(value.into());
        self
    }

    /// Set the tower number.
    #[must_use]
    pub fn tower_number(mut self, value: impl Into<String>) -> Self {
        self.tower_number = Some(value.into());
        self
    }

    /// Set the building's name.
    #[must_use]
    pub fn building_name(mut self, value: impl Into<String>) -> Self {
        self.building_name = Some(value.into());
        self
    }

    /// Set the landmark or area.
    #[must_use]
    pub fn landmark_area(mut self, value: impl Into<String>) -> Self {
        self.landmark_area = Some(value.into());
        self
    }

    fn validate(&self, field: &str) -> Check {
        let f = |name: &str| format!("{field}.{name}");
        validate::text(&f("name"), &self.name, NAME_MAX)?;
        validate::text(&f("phone_number"), &self.phone_number, PHONE_NUMBER_MAX)?;
        validate::text(&f("address"), &self.address, ADDRESS_MAX)?;
        validate::text(&f("city"), &self.city, CITY_STATE_MAX)?;
        validate::text(&f("state"), &self.state, CITY_STATE_MAX)?;
        validate::text(&f("in_pin_code"), &self.in_pin_code, PIN_CODE_MAX)?;
        for (name, value, max) in [
            ("house_number", &self.house_number, HOUSE_TOWER_MAX),
            ("tower_number", &self.tower_number, HOUSE_TOWER_MAX),
            ("building_name", &self.building_name, BUILDING_LANDMARK_MAX),
            ("landmark_area", &self.landmark_area, BUILDING_LANDMARK_MAX),
        ] {
            validate::opt_text(&f(name), value.as_deref(), max)?;
        }
        Ok(())
    }
}

/// `order_details.coupon`: the coupon a checkout endpoint applied
/// (`apply_coupon`), whose `discount` comes off the total on top of the
/// order's own discount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedCoupon {
    /// The code the buyer picked or typed (`input.coupon.code`).
    pub code: String,
    /// What it takes off the total.
    pub discount: Amount,
}

impl AppliedCoupon {
    /// `code`, taking `discount` off the total.
    pub fn new(code: impl Into<String>, discount: Amount) -> Self {
        Self {
            code: code.into(),
            discount,
        }
    }

    pub(super) fn validate(&self, field: &str, currency: Currency) -> Check {
        validate::non_empty(&format!("{field}.code"), &self.code)?;
        self.discount
            .validate(&format!("{field}.discount"), currency)
    }
}
