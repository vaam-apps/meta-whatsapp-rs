//! The Payments API's template parts: a payment request button's
//! [`PaymentRequestSetting`] (in a definition and in a send), the
//! [`PaymentRequest`] a send fills it with, and the [`TemplateOrderStatus`]
//! of an order status template. The order details button carries the
//! [`OrderDetailsParameters`](crate::payments::OrderDetailsParameters) of
//! an `order_details` message, checked by the same rules. Re-exported from
//! [`crate::payments`].
//!
//! Pages (relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`):
//! `payments/payments-in/orderdetailstemplate`,
//! `payments/payments-in/orderstatustemplate`,
//! `payments/payments-br/orderdetailstemplate`,
//! `payments/payments-br/payment-request-cta`.

use meta_whatsapp_core::error::ValidationError;
use serde::de::Error as _;
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Deserializer, Serialize};

use crate::messages::payments::{
    Amount, Currency, LAST_FOUR_DIGITS, OrderUpdate, PixFlowType, check_reference_id,
};
use crate::messages::validate;

type Check = Result<(), ValidationError>;

/// The card of a one-click payment request (`offsite_card_pay` in a
/// send): what the buyer sees, and your id of the stored card, which the
/// buyer's `payment_method` reply returns. Payment data, not secrets
/// (the [`crate::payments`] module docs): never put a card number or a PSP
/// token in `credential_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentRequestCard {
    /// The card's last four digits, shown for confirmation.
    pub last_four_digits: String,
    /// Your id of the stored card; optional on this page ("If provided,
    /// the payment confirmation webhook includes this value").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
}

impl PaymentRequestCard {
    /// The card ending in `last_four_digits`, without a credential id.
    pub fn new(last_four_digits: impl Into<String>) -> Self {
        Self {
            last_four_digits: last_four_digits.into(),
            credential_id: None,
        }
    }

    /// Set `credential_id`.
    #[must_use]
    pub fn credential_id(mut self, id: impl Into<String>) -> Self {
        self.credential_id = Some(id.into());
        self
    }

    fn validate(&self, field: &str) -> Check {
        let digits = &self.last_four_digits;
        if digits.len() != LAST_FOUR_DIGITS || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ValidationError::new(
                format!("{field}.last_four_digits"),
                "must be the card's last four digits",
            ));
        }
        if let Some(id) = &self.credential_id {
            validate::non_empty(&format!("{field}.credential_id"), id)?;
        }
        Ok(())
    }
}

/// The `payment_setting` of a payment request button
/// (`payments-br/payment-request-cta`): one payment method. In a
/// definition it carries a sample value (none for one-click); in a send,
/// the buyer's actual code, line, link or card. Serializes to
/// `{"type": T, T: {…}}`, or `{"type": "offsite_card_pay"}` alone in a
/// definition.
///
/// Not [`PaymentSetting`](crate::payments::PaymentSetting), the
/// `payment_settings` entry of an order: this page's Pix object has the
/// code alone (and `flow_type`), and its card's `credential_id` is
/// optional.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PaymentRequestSetting {
    /// `pix_dynamic_code`: a dynamic Pix code the buyer copies, or, with
    /// [`PixFlowType::AppSwitch`] ("Pix with redirect", onboarding by Meta
    /// required), opens in their bank app.
    PixDynamicCode {
        /// The code.
        code: String,
        /// The payment experience; `None` sends none (copy and paste).
        flow_type: Option<PixFlowType>,
    },
    /// `boleto`: a Boleto's digitable line, copied by the buyer.
    Boleto {
        /// The digitable line.
        digitable_line: String,
    },
    /// `payment_link`: a link the browser opens.
    PaymentLink {
        /// The link.
        uri: String,
    },
    /// `offsite_card_pay`: a one-click card payment. `None` in a
    /// definition ("set `type` to `offsite_card_pay` and omit this
    /// object"); the card in a send.
    OffsiteCardPay(Option<PaymentRequestCard>),
}

impl PaymentRequestSetting {
    /// A dynamic Pix code, copied and pasted.
    pub fn pix_dynamic_code(code: impl Into<String>) -> Self {
        Self::PixDynamicCode {
            code: code.into(),
            flow_type: None,
        }
    }

    /// A Boleto, by its digitable line.
    pub fn boleto(digitable_line: impl Into<String>) -> Self {
        Self::Boleto {
            digitable_line: digitable_line.into(),
        }
    }

    /// A payment link.
    pub fn payment_link(uri: impl Into<String>) -> Self {
        Self::PaymentLink { uri: uri.into() }
    }

    /// One-click payment in a definition: no sample value.
    pub fn offsite_card_pay() -> Self {
        Self::OffsiteCardPay(None)
    }

    /// One-click payment in a send: the card to confirm.
    pub fn card(card: PaymentRequestCard) -> Self {
        Self::OffsiteCardPay(Some(card))
    }

    /// The `type` value.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::PixDynamicCode { .. } => "pix_dynamic_code",
            Self::Boleto { .. } => "boleto",
            Self::PaymentLink { .. } => "payment_link",
            Self::OffsiteCardPay(_) => "offsite_card_pay",
        }
    }

    /// The label the button must have: the buttons table's "Must be `Copy
    /// Pix code` for `pix_dynamic_code`, `Copy Boleto code` for `boleto`,
    /// `Open payment link` for `payment_link`, or `Review payment` for
    /// `offsite_card_pay`".
    pub fn label(&self) -> &'static str {
        match self {
            Self::PixDynamicCode { .. } => "Copy Pix code",
            Self::Boleto { .. } => "Copy Boleto code",
            Self::PaymentLink { .. } => "Open payment link",
            Self::OffsiteCardPay(_) => "Review payment",
        }
    }

    /// `sending`: in a send (the card required), or in a definition (no
    /// card).
    pub(crate) fn validate(&self, field: &str, sending: bool) -> Check {
        match self {
            Self::PixDynamicCode { code, flow_type } => {
                validate::non_empty(&format!("{field}.pix_dynamic_code.code"), code)?;
                if let Some(PixFlowType::Other(s)) = flow_type {
                    validate::non_empty(&format!("{field}.pix_dynamic_code.flow_type"), s)?;
                }
                Ok(())
            }
            Self::Boleto { digitable_line } => {
                validate::non_empty(&format!("{field}.boleto.digitable_line"), digitable_line)
            }
            Self::PaymentLink { uri } => {
                validate::non_empty(&format!("{field}.payment_link.uri"), uri)
            }
            Self::OffsiteCardPay(card) => {
                let at = format!("{field}.offsite_card_pay");
                match (card, sending) {
                    (Some(card), true) => card.validate(&at),
                    (None, true) => Err(ValidationError::new(
                        at,
                        "is required when sending a one-click payment request",
                    )),
                    (None, false) => Ok(()),
                    (Some(_), false) => Err(ValidationError::new(
                        at,
                        "is sent with the message, not when creating the template",
                    )),
                }
            }
        }
    }
}

impl Serialize for PaymentRequestSetting {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Pix<'a> {
            code: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            flow_type: Option<&'a PixFlowType>,
        }
        #[derive(Serialize)]
        struct Boleto<'a> {
            digitable_line: &'a str,
        }
        #[derive(Serialize)]
        struct Link<'a> {
            uri: &'a str,
        }
        let kind = self.type_name();
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("type", kind)?;
        match self {
            Self::PixDynamicCode { code, flow_type } => map.serialize_entry(
                kind,
                &Pix {
                    code,
                    flow_type: flow_type.as_ref(),
                },
            )?,
            Self::Boleto { digitable_line } => {
                map.serialize_entry(kind, &Boleto { digitable_line })?;
            }
            Self::PaymentLink { uri } => map.serialize_entry(kind, &Link { uri })?,
            Self::OffsiteCardPay(Some(card)) => map.serialize_entry(kind, card)?,
            Self::OffsiteCardPay(None) => {}
        }
        map.end()
    }
}

/// The shape [`Serialize`] writes. An unknown `type` is refused, as is a
/// Pix code, Boleto or link without its object.
impl<'de> Deserialize<'de> for PaymentRequestSetting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Pix {
            code: String,
            #[serde(default)]
            flow_type: Option<PixFlowType>,
        }
        #[derive(Deserialize)]
        struct Boleto {
            digitable_line: String,
        }
        #[derive(Deserialize)]
        struct Link {
            uri: String,
        }
        #[derive(Deserialize)]
        struct Raw {
            #[serde(rename = "type")]
            kind: String,
            pix_dynamic_code: Option<Pix>,
            boleto: Option<Boleto>,
            payment_link: Option<Link>,
            offsite_card_pay: Option<PaymentRequestCard>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let setting = match raw.kind.as_str() {
            "pix_dynamic_code" => raw.pix_dynamic_code.map(|p| Self::PixDynamicCode {
                code: p.code,
                flow_type: p.flow_type,
            }),
            "boleto" => raw.boleto.map(|b| Self::Boleto {
                digitable_line: b.digitable_line,
            }),
            "payment_link" => raw.payment_link.map(|l| Self::PaymentLink { uri: l.uri }),
            "offsite_card_pay" => Some(Self::OffsiteCardPay(raw.offsite_card_pay)),
            _ => {
                return Err(D::Error::custom("a payment request type this client knows"));
            }
        };
        setting.ok_or_else(|| D::Error::custom("a payment setting without its object"))
    }
}

/// The `payment_request` a payment request button is sent with
/// (`payments-br/payment-request-cta`): the payment method and, for a
/// one-click payment (required) or a Pix code (optional, shown to the
/// buyer), the amount, in `BRL`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentRequest {
    /// The payment method.
    pub payment_setting: PaymentRequestSetting,
    /// `BRL`, with [`Self::total_amount`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<Currency>,
    /// What the buyer pays (`offset` 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_amount: Option<Amount>,
}

impl PaymentRequest {
    /// A request paid through `setting`, without an amount.
    pub fn new(setting: PaymentRequestSetting) -> Self {
        Self {
            payment_setting: setting,
            currency: None,
            total_amount: None,
        }
    }

    /// A one-click payment of `amount` (`BRL`) with `card`.
    pub fn card(card: PaymentRequestCard, amount: Amount) -> Self {
        Self::new(PaymentRequestSetting::card(card)).total_amount(amount)
    }

    /// Set the amount, and `currency` to `BRL` (the only value the page
    /// allows).
    #[must_use]
    pub fn total_amount(mut self, amount: Amount) -> Self {
        self.currency = Some(Currency::Brl);
        self.total_amount = Some(amount);
        self
    }

    /// The page's parameters table: the setting (a card required for
    /// one-click); `currency` "Must be `BRL`"; `currency` and
    /// `total_amount` required for `offsite_card_pay`, optional for
    /// `pix_dynamic_code`, "not used" for `boleto` and `payment_link`
    /// (refused there), and one without the other refused; the amount's
    /// `offset` 100 and its `value` positive.
    pub(crate) fn validate(&self, field: &str) -> Check {
        let setting = &self.payment_setting;
        setting.validate(&format!("{field}.payment_setting"), true)?;
        let currency_field = format!("{field}.currency");
        let total_field = format!("{field}.total_amount");
        if let Some(currency) = self.currency
            && currency != Currency::Brl
        {
            return Err(ValidationError::new(currency_field, "must be `BRL`"));
        }
        match setting {
            PaymentRequestSetting::Boleto { .. } | PaymentRequestSetting::PaymentLink { .. } => {
                for (name, set) in [
                    (&currency_field, self.currency.is_some()),
                    (&total_field, self.total_amount.is_some()),
                ] {
                    if set {
                        return Err(ValidationError::new(
                            name.clone(),
                            format!("is not used with `{}`", setting.type_name()),
                        ));
                    }
                }
            }
            PaymentRequestSetting::OffsiteCardPay(_) => {
                if self.currency.is_none() {
                    return Err(ValidationError::new(
                        currency_field,
                        "is required with `offsite_card_pay`",
                    ));
                }
                if self.total_amount.is_none() {
                    return Err(ValidationError::new(
                        total_field,
                        "is required with `offsite_card_pay`",
                    ));
                }
            }
            PaymentRequestSetting::PixDynamicCode { .. } => {
                // "used with `total_amount` to display the payment amount":
                // the two go together.
                if self.currency.is_some() != self.total_amount.is_some() {
                    let missing = if self.currency.is_none() {
                        currency_field
                    } else {
                        total_field
                    };
                    return Err(ValidationError::new(
                        missing,
                        "`currency` and `total_amount` go together",
                    ));
                }
            }
        }
        if let Some(amount) = self.total_amount {
            amount.validate(&total_field, Currency::Brl)?;
            if amount.value == 0 {
                return Err(ValidationError::new(
                    format!("{total_field}.value"),
                    "must be a positive integer",
                ));
            }
        }
        Ok(())
    }
}

/// The `order_status` parameter of an order status template
/// (`payments-in/orderstatustemplate`): the order's new status, outside
/// the customer service window. India documents it; Brazil documents no
/// order status template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateOrderStatus {
    /// The `reference_id` of the `order_details` message.
    pub reference_id: String,
    /// The order's new status.
    pub order: OrderUpdate,
}

impl TemplateOrderStatus {
    /// Move the order `reference_id` to `order`.
    pub fn new(reference_id: impl Into<String>, order: OrderUpdate) -> Self {
        Self {
            reference_id: reference_id.into(),
            order,
        }
    }

    /// India's rules for an `order_status` message's parameters: the
    /// `reference_id` (1 to 35 characters among letters, digits, `_`, `-`,
    /// `.`); a status other than `pending` (the example lists `processing`,
    /// `partially_shipped`, `shipped`, `completed` and `canceled`); the
    /// description 1 to 120 characters.
    pub(crate) fn validate(&self, field: &str) -> Check {
        check_reference_id(
            &format!("{field}.reference_id"),
            &self.reference_id,
            Currency::Inr.reference_id_max(),
        )?;
        self.order
            .validate(&format!("{field}.order"), Currency::Inr)
    }
}
