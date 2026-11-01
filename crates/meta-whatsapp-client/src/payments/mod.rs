//! Payments API (India and Brazil): the `order_details` and
//! `order_status` messages and their parts (defined with the other
//! interactive messages, in [`crate::messages`], and re-exported here),
//! and, in India, the payment lookup, refunds, and the payment
//! configurations a WABA collects payments with.
//!
//! Docs: `payments/payments-in/overview`,
//! `payments/payments-in/upi-intent/dynamic-vpa`,
//! `payments/payments-in/upi-intent`, `payments/payments-in/payment-links`,
//! `payments/payments-in/pg`, `payments/payments-in/onboarding-apis`;
//! `payments/payments-br/overview`, `payments/payments-br/orders`,
//! `payments/payments-br/offsite-pix`, `payments/payments-br/payment-links`,
//! `payments/payments-br/boleto`, `payments/payments-br/one-click-payments`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown).
//!
//! | Accessor | Meta paths |
//! | --- | --- |
//! | [`Client::messages`] with an [`OrderDetails`] or [`OrderStatus`] | `POST /{PHONE_NUMBER_ID}/messages` |
//! | [`Client::payments`] | `GET /{PHONE_NUMBER_ID}/payments/{PAYMENT_CONFIGURATION}/{REFERENCE_ID}`, `POST /{PHONE_NUMBER_ID}/payments_refund` |
//! | [`Client::payment_configurations`] | `GET /{WABA_ID}/payment_configurations`, `GET /{WABA_ID}/payment_configuration/{CONFIGURATION_NAME}`, `POST /{WABA_ID}/payment_configuration`, `POST /{WABA_ID}/generate_payment_configuration_oauth_link`, `DELETE /{WABA_ID}/payment_configuration` |
//!
//! Every call authenticates with the token of the [`Client`] it was built
//! from: the business's (a merchant's, with [`Client::with_token`]).
//!
//! # One currency, one set of rules
//!
//! An order's [`Currency`] selects what is checked: India's rules for
//! `INR` ([`OrderDetailsParameters::inr`], [`OrderStatus::new`]),
//! Brazil's for `BRL` ([`OrderDetailsParameters::brl`],
//! [`OrderDetailsParameters::brl_simplified`], [`OrderStatus::brl`]).
//!
//! | | India (`INR`) | Brazil (`BRL`) |
//! | --- | --- | --- |
//! | `reference_id` | at most 35 characters | at most 60 characters |
//! | `payment_type` | `upi`, with a payment link or a configuration | `br`, always |
//! | `payment_settings` | required (or `payment_configuration`) | optional |
//! | settings | `upi_intent_link`, `payment_link`, `payment_gateway` | `pix_dynamic_code`, `payment_link` (`uri` alone), `boleto`, `offsite_card_pay` |
//! | `order` | required | optional ("simplified": the total alone, no header) |
//! | `order_details` header | an image, by media id or link | an image by link (Brazil's image object has `link` alone) |
//! | items | origin and importer without a catalog; custom images | a `retailer_id` each; the subtotal checked |
//! | `order_status` | `order` required, not `pending`; no `payment` | `order` and `payment` optional; `pending` allowed |
//! | `offset` | 100 | 100 |
//!
//! A field one currency's pages document and the other's do not is
//! refused, rather than sent where no page says Meta reads it.
//!
//! # Integration modes
//!
//! - **UPI intent** ([`PaymentSetting::upi_intent_link`]) and **payment
//!   links** ([`PaymentSetting::payment_link`], limited access): your
//!   payment gateway reports the payment to you; then send an
//!   [`OrderStatus`].
//! - **Payment gateway** ([`PaymentGateway`]: `BillDesk`, Razorpay, `PayU`,
//!   Zaakpay): checkout runs through a payment configuration linked to the
//!   WABA ([`PaymentConfigurations`]); Meta reports the payment in a status
//!   webhook of type `payment`, and [`Payments::lookup`] reads it. Meta
//!   asks not to rely on the webhook alone: look the payment up.
//! - Enhanced payment links (a template with a payment link URL button)
//!   need nothing of this module.
//! - **Brazil**: a dynamic Pix code ([`PixDynamicCode`], copied by the
//!   buyer or, with [`PixFlowType::AppSwitch`], opened in their bank
//!   app), a payment link, a Boleto ([`PaymentSetting::boleto`]) or a
//!   one-click card payment ([`OffsiteCardPay`], not publicly available).
//!   Your bank or PSP confirms the payment to you; then send an
//!   [`OrderStatus`] with a [`PaymentUpdate`] (`captured`). WhatsApp does
//!   not reconcile payments: match them by `reference_id`. Only for Pix
//!   with redirect does WhatsApp mark the order paid by itself.
//!
//! The payment template buttons and the payment webhooks (India's status
//! of type `payment`, Brazil's `payment_method` reply) are not typed yet
//! (roadmap P1c).
//!
//! # Where the pages disagree
//!
//! The client follows the examples (Meta's examples are the closest thing
//! to a contract it publishes), and says so here:
//!
//! - `payment_settings` is described as an object and shown as an array:
//!   it is sent as an array.
//! - `importer_address` is described as a string and shown as an object
//!   (`address_line1`, `address_line2`, `city`, `zone_code`,
//!   `postal_code`, `country_code`): it is sent as that object,
//!   [`ImporterAddress`].
//! - The order status table spells `partially-shipped`; the examples, the
//!   `order_status` parameters table of `payments-in/pg` and the Brazil
//!   page spell `partially_shipped`, which is what
//!   [`OrderStatusValue::PartiallyShipped`] sends.
//! - Three pages warn that "the `parameters` value is a stringified JSON
//!   object" right above examples that send it as an object: it is sent
//!   as an object.
//! - The examples' header images carry a `provider` object that no table
//!   describes; it is not modelled.
//! - Brazil's `subtotal` row says its `offset` and `value` are strings;
//!   the amount tables say integers, and every example sends integers:
//!   they are sent as integers.
//! - Brazil's tables call `payment_settings` optional on every page,
//!   while each page's text says the message carries them: they are
//!   optional (every example sends one).
//! - Brazil's `order_status` table lists a `header`, "Optional object",
//!   with no fields and no example: it is not modelled.
//!
//! # What is checked locally
//!
//! Every limit the pages state as a rule ("required", "must", "cannot
//! exceed", "only supported value"), before any request:
//! [`OrderDetails::validate`] and [`OrderStatus::validate`] list them.
//! Three rules are not checked: in India, `subtotal` equal to the sum of
//! the items' prices (every India example that has a `sale_amount` breaks
//! it, and the pages disagree on whether `sale_amount` counts; Brazil's
//! page says the sale price counts, its examples keep the rule, and it is
//! checked there, [`Order::items_sum`]), the expiration's 300-second
//! minimum (it is relative to the time Meta receives the message), and
//! beneficiaries for physical goods, which only shipped goods need (the
//! client cannot tell them from goods collected). Brazil's order status
//! transitions, like India's, are Meta's to refuse (error `2046`); a Pix
//! code's validity is not read from the code, by WhatsApp or here.
//!
//! # Payment data, not secrets
//!
//! A Pix code, its key and merchant name, a Boleto's digitable line and
//! a card's last four digits are shown to (or copied by) the buyer, and
//! `Debug` shows them; a `CPF` key is a person's taxpayer number, which
//! the buyer sees too. An [`OffsiteCardPay::credential_id`] is your id of
//! a stored card, not the card: Meta returns it in the buyer's reply. Do
//! not put a card number or a PSP token there.
//!
//! # Retries
//!
//! A message, a refund, a configuration created or an OAuth link
//! regenerated is never replayed after a timeout or a server error
//! ([`crate::RetryPolicy`]): each would happen twice. Only a refusal that
//! provably did nothing (throttling, an HTTP 429) is sent again. Reads and
//! the deletion are replayed.
//!
//! # No legal act here
//!
//! Sending is refused with error `134011`
//! ([`ErrorKind::Payment`](meta_whatsapp_core::ErrorKind::Payment)) until
//! the WABA has accepted the WhatsApp Payments terms of service, through a
//! link in the error's details that the business opens; the onboarding
//! calls ([`PaymentConfigurations`]) are refused with `131005`, "Access
//! denied" ([`ErrorKind::Permission`](meta_whatsapp_core::ErrorKind::Permission)),
//! and the same link. No call of this library accepts them.
//!
//! ```no_run
//! # async fn demo(client: meta_whatsapp_client::Client) -> meta_whatsapp_core::Result<()> {
//! use meta_whatsapp_client::messages::OutboundMessage;
//! use meta_whatsapp_client::payments::{
//!     Amount, DescribedAmount, GatewayProvider, GoodsType, Order, OrderDetails,
//!     OrderDetailsParameters, OrderItem, PaymentGateway, RazorpayFields,
//! };
//! use meta_whatsapp_core::ids::{CatalogId, PhoneNumberId};
//! use meta_whatsapp_core::recipient::Recipient;
//!
//! let number = PhoneNumberId::new("106540352242922")?;
//! let order = Order::new(
//!     [OrderItem::new("Bread", Amount::inr(10000), 2).retailer_id("SKU-1")],
//!     Amount::inr(20000),
//!     DescribedAmount::new(Amount::inr(1000)),
//! )
//! .catalog_id(CatalogId::new("1234567890"));
//! let gateway = PaymentGateway::new(
//!     GatewayProvider::Razorpay(RazorpayFields::default()),
//!     "shop-razorpay",
//! );
//! let details = OrderDetails::new(
//!     "Your order is ready",
//!     OrderDetailsParameters::inr("BM345A-12", GoodsType::Physical, gateway, order),
//! );
//! client
//!     .messages(number.clone())
//!     .send(&OutboundMessage::new(Recipient::phone("+919999999999"), details))
//!     .await?;
//! // Later, after the `payment` status webhook:
//! let payments = client.payments(number).lookup("shop-razorpay", "BM345A-12").await?;
//! # let _ = payments; Ok(()) }
//! ```

use std::fmt;

use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_core::open_enum;
use meta_whatsapp_core::secret::RedactedPath;
use meta_whatsapp_core::{Error, Result};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub use crate::common::SecretUrl;
pub use crate::messages::payments::{
    Amount, Beneficiary, BilldeskFields, Currency, DescribedAmount, Discount, GatewayProvider,
    GoodsType, ImporterAddress, ItemImage, OffsiteCardPay, Order, OrderDetails,
    OrderDetailsParameters, OrderExpiration, OrderItem, OrderStatus, OrderStatusValue, OrderUpdate,
    PaymentGateway, PaymentLink, PaymentOption, PaymentSetting, PaymentType, PaymentUpdate,
    PaymentUpdateStatus, PayuFields, PixDynamicCode, PixFlowType, PixKeyType, RazorpayFields,
    UpiApp, ZaakpayFields,
};

use crate::Client;
use crate::messages::payments::CONFIGURATION_NAME_MAX;
use crate::messages::validate;

#[cfg(test)]
mod tests;

/// Body snippet of a payment response that does not decode: the fields a
/// merchant passed on (notes, UDFs) can name the buyer.
const PAYMENT_WITHHELD: &[u8] = b"(withheld: the payment may carry the merchant's notes)";
/// Body snippet of an OAuth link response that does not decode, or of a
/// refusal that is not Meta's error envelope: the link is a credential.
const OAUTH_WITHHELD_TEXT: &str = "(withheld: the response carries an OAuth link)";
/// [`OAUTH_WITHHELD_TEXT`] as bytes, the decode error's placeholder.
const OAUTH_WITHHELD: &[u8] = OAUTH_WITHHELD_TEXT.as_bytes();

/// A refusal of an OAuth link's endpoint that is not Meta's error
/// envelope (a proxy's page, an answer cut short) keeps its status, never
/// its body: the crate's own rule (`safe_snippet`) withholds only bodies
/// naming `paging` or `access_token`, and this body may hold the link.
/// Meta's envelope ([`Error::Api`]) carries Meta's message, not the body,
/// and is kept.
fn withhold_http_body(error: Error) -> Error {
    match error {
        Error::Http { status, .. } => Error::Http {
            status,
            body_snippet: OAUTH_WITHHELD_TEXT.to_owned(),
        },
        other => other,
    }
}

// ─── Payments: lookup and refunds ────────────────────────────────────────

/// Entry point, see [`Client::payments`].
#[derive(Debug, Clone)]
pub struct Payments {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`Payments`] API (lookup, refunds) for the number that sent the
    /// `order_details` message.
    pub fn payments(&self, phone_number_id: impl Into<PhoneNumberId>) -> Payments {
        Payments {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }

    /// [`PaymentConfigurations`] API for `waba_id`.
    pub fn payment_configurations(&self, waba_id: impl Into<WabaId>) -> PaymentConfigurations {
        PaymentConfigurations {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

open_enum! {
    #![case_insensitive]
    /// A payment's status ([`Payment::status`]).
    pub enum PaymentStatus {
        /// Not captured: no attempt yet (no transactions), or only failed
        /// ones.
        Pending => "pending",
        /// Captured: one transaction succeeded.
        Captured => "captured",
    }
}

open_enum! {
    #![case_insensitive]
    /// A transaction's or a refund's status.
    pub enum TransactionStatus {
        /// `pending`.
        Pending => "pending",
        /// `success` (at most one transaction of a payment).
        Success => "success",
        /// `failed`.
        Failed => "failed",
        /// `completed`: a refund's status in the refund's answer.
        Completed => "completed",
    }
}

open_enum! {
    #![case_insensitive]
    /// The gateway of a transaction (`transaction.type`).
    pub enum GatewayType {
        /// `BillDesk`.
        Billdesk => "billdesk",
        /// Razorpay.
        Razorpay => "razorpay",
        /// `PayU`.
        Payu => "payu",
        /// Zaakpay.
        Zaakpay => "zaakpay",
    }
}

open_enum! {
    #![case_insensitive]
    /// How the buyer paid (`method.type`).
    pub enum PaymentMethodType {
        /// `upi`.
        Upi => "upi",
        /// `card`.
        Card => "card",
        /// `wallet`.
        Wallet => "wallet",
        /// `netbanking`.
        Netbanking => "netbanking",
    }
}

open_enum! {
    #![case_insensitive]
    /// How fast a refund goes (`speed`, `speed_processed`). The gateway
    /// decides: the speed processed may differ from the one asked.
    pub enum RefundSpeed {
        /// `instant`.
        Instant => "instant",
        /// `normal`.
        Normal => "normal",
    }
}

/// A payment, from [`Payments::lookup`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Payment {
    /// The order's `reference_id`.
    pub reference_id: String,
    /// Captured or not.
    pub status: PaymentStatus,
    /// `INR`.
    pub currency: String,
    /// The amount to pay.
    pub amount: Amount,
    /// The payment attempts; empty before the first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transactions: Vec<Transaction>,
    /// `BillDesk`'s fields, as sent in the order.
    #[serde(flatten)]
    pub billdesk: BilldeskFields,
    /// Razorpay's fields, as sent in the order.
    #[serde(flatten)]
    pub razorpay: RazorpayFields,
    /// `PayU`'s fields, as sent in the order.
    #[serde(flatten)]
    pub payu: PayuFields,
    /// Zaakpay's fields, as sent in the order.
    #[serde(flatten)]
    pub zaakpay: ZaakpayFields,
}

/// One payment attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Transaction {
    /// The gateway's order id.
    pub id: String,
    /// The gateway's payment id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pg_transaction_id: Option<String>,
    /// The gateway.
    #[serde(rename = "type")]
    pub gateway: GatewayType,
    /// The attempt's status.
    pub status: TransactionStatus,
    /// Created at.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub created_timestamp: OffsetDateTime,
    /// Last updated at.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub updated_timestamp: OffsetDateTime,
    /// The amount paid (the examples show it; the table does not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<Amount>,
    /// The order's amount (the examples show it; the table does not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_amount: Option<Amount>,
    /// The currency (the examples show it; the table does not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// How the buyer paid; may be absent for a failed attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<PaymentMethod>,
    /// Why it failed, as the gateway says; not always given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TransactionError>,
    /// Refunds of this transaction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refunds: Vec<PaymentRefund>,
}

/// `transaction.method`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PaymentMethod {
    /// UPI, card, wallet or net banking.
    #[serde(rename = "type")]
    pub kind: PaymentMethodType,
}

/// `transaction.error`: the gateway's failure code and reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TransactionError {
    /// The gateway's code (`BAD_REQUEST_ERROR`).
    pub code: String,
    /// The gateway's reason (`incorrect_pin`).
    pub reason: String,
}

/// A refund of a transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PaymentRefund {
    /// The refund's id.
    pub id: String,
    /// How much.
    pub amount: Amount,
    /// The speed it went at.
    pub speed_processed: RefundSpeed,
    /// Its status.
    pub status: TransactionStatus,
    /// Created at.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub created_timestamp: OffsetDateTime,
    /// Last updated at.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub updated_timestamp: OffsetDateTime,
}

#[derive(Deserialize)]
struct PaymentsEnvelope {
    #[serde(default)]
    payments: Vec<Payment>,
}

/// A refund to ask for, with [`Payments::refund`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RefundRequest {
    /// The order's `reference_id`.
    pub reference_id: String,
    /// The payment configuration the order was paid through
    /// (`payment_config_id`).
    pub payment_config_id: String,
    /// How much to refund.
    pub amount: Amount,
    /// Its currency.
    pub currency: Currency,
    /// The speed asked; the gateway decides.
    pub speed: Option<RefundSpeed>,
}

impl RefundRequest {
    /// Refund `amount` (INR) of the order `reference_id`, paid through
    /// `payment_config_id`.
    pub fn inr(
        reference_id: impl Into<String>,
        payment_config_id: impl Into<String>,
        amount: Amount,
    ) -> Self {
        Self {
            reference_id: reference_id.into(),
            payment_config_id: payment_config_id.into(),
            amount,
            currency: Currency::Inr,
            speed: None,
        }
    }

    /// Ask for `speed`.
    #[must_use]
    pub fn speed(mut self, speed: RefundSpeed) -> Self {
        self.speed = Some(speed);
        self
    }

    /// The order and configuration are named, and the amount is positive,
    /// with its currency's `offset` (100 for INR).
    ///
    /// # Errors
    ///
    /// A [`ValidationError`] on `reference_id`, `payment_config_id` or
    /// `amount`.
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        validate::non_empty("reference_id", &self.reference_id)?;
        validate::non_empty("payment_config_id", &self.payment_config_id)?;
        self.amount.validate("amount", self.currency)?;
        if self.amount.value == 0 {
            return Err(ValidationError::new(
                "amount.value",
                "must be a positive integer",
            ));
        }
        Ok(())
    }
}

impl Serialize for RefundRequest {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        // payments-in/pg, step 8: the refund's `value` and `offset` are
        // strings, unlike everywhere else.
        #[derive(Serialize)]
        struct Money<'a> {
            currency: Currency,
            value: &'a str,
            offset: &'a str,
        }
        let value = self.amount.value.to_string();
        let offset = self.amount.offset.to_string();
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("reference_id", &self.reference_id)?;
        if let Some(speed) = &self.speed {
            map.serialize_entry("speed", speed)?;
        }
        map.serialize_entry("payment_config_id", &self.payment_config_id)?;
        map.serialize_entry(
            "amount",
            &Money {
                currency: self.currency,
                value: &value,
                offset: &offset,
            },
        )?;
        map.end()
    }
}

/// The answer to [`Payments::refund`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RefundCreated {
    /// The refund's id.
    pub id: String,
    /// `pending`, `failed` or `completed`.
    pub status: TransactionStatus,
    /// The speed the gateway chose.
    pub speed_processed: RefundSpeed,
}

impl Payments {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// `GET /{PHONE_NUMBER_ID}/payments/{PAYMENT_CONFIGURATION}/{REFERENCE_ID}`:
    /// the payment of an order sent through a payment gateway, with its
    /// attempts and refunds. Both names are those of the `order_details`
    /// message; each is one path segment.
    ///
    /// Meta asks to read the status here rather than trust the webhook
    /// alone. Meta's example of a lookup error is not Graph's error
    /// envelope (`{"errors": [{"code", "title", "details"}]}`): it reaches
    /// the caller as [`Error::Http`].
    pub async fn lookup(
        &self,
        payment_configuration: &str,
        reference_id: &str,
    ) -> Result<Vec<Payment>> {
        validate::non_empty("payment_configuration", payment_configuration)?;
        validate::non_empty("reference_id", reference_id)?;
        let envelope: PaymentsEnvelope = self
            .client
            .get_at(&[
                self.phone_number_id.as_str(),
                "payments",
                payment_configuration,
                reference_id,
            ])
            .context("payment lookup response")
            .send_withheld(PAYMENT_WITHHELD)
            .await?;
        Ok(envelope.payments)
    }

    /// `POST /{PHONE_NUMBER_ID}/payments_refund`: refund an order paid
    /// through a payment gateway, in full or in part. Checked first
    /// ([`RefundRequest::validate`]): the amount is positive, but not
    /// compared with what was paid, which the client does not know (look
    /// the payment up, with its earlier refunds, first). **Never replayed**
    /// on a timeout or a server error: a replay could refund twice; look
    /// the payment up to see whether it went through.
    pub async fn refund(&self, request: &RefundRequest) -> Result<RefundCreated> {
        request.validate()?;
        self.client
            .post_at(&[self.phone_number_id.as_str(), "payments_refund"])
            .json(request)
            .idempotent(false)
            .context("payment refund response")
            .send()
            .await
    }
}

// ─── Payment configurations ──────────────────────────────────────────────

/// Entry point, see [`Client::payment_configurations`].
#[derive(Debug, Clone)]
pub struct PaymentConfigurations {
    client: Client,
    waba_id: WabaId,
}

open_enum! {
    #![normalize = status_spelling]
    #![case_insensitive]
    /// A payment configuration's status. The reference tables write
    /// `Needs_Connecting` and `Needs_Testing`, the webhook example `Needs
    /// Testing`: both spellings parse, in any case, and serialize as the
    /// tables'.
    pub enum PaymentConfigurationStatus {
        /// Tested and usable.
        Active => "Active",
        /// To link to the gateway (see
        /// [`PaymentConfigurations::regenerate_oauth_link`]).
        NeedsConnecting => "Needs_Connecting",
        /// Linked, not yet tested.
        NeedsTesting => "Needs_Testing",
    }
}

/// What [`PaymentConfigurationStatus`] compares: spaces read as
/// underscores.
fn status_spelling(status: &str) -> String {
    status.replace(' ', "_")
}

open_enum! {
    #![case_insensitive]
    /// A payment configuration's provider (`provider_name`). Parsed in
    /// any case: Meta's example answers `RazorPay`.
    pub enum PaymentProvider {
        /// `upi_vpa`: UPI to the merchant's VPA, no gateway.
        UpiVpa => "upi_vpa",
        /// Razorpay.
        Razorpay => "razorpay",
        /// `PayU`.
        Payu => "payu",
        /// Zaakpay.
        Zaakpay => "zaakpay",
    }
}

/// A code with its description: a configuration's merchant category code
/// or purpose code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CodeDescription {
    /// The code (`0000`).
    pub code: String,
    /// What it means.
    pub description: String,
}

/// A payment configuration linked to a WABA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PaymentConfiguration {
    /// Its name: what an order's [`PaymentGateway::configuration_name`]
    /// gives.
    pub configuration_name: String,
    /// Merchant category code. The tables mark it required in answers but
    /// optional when creating, so it is optional here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merchant_category_code: Option<CodeDescription>,
    /// Purpose code (optional, as `merchant_category_code`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose_code: Option<CodeDescription>,
    /// Its status.
    pub status: PaymentConfigurationStatus,
    /// The gateway's merchant id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_mid: Option<String>,
    /// The gateway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<PaymentProvider>,
    /// The merchant's UPI handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merchant_vpa: Option<String>,
    /// Created at.
    #[serde(
        default,
        with = "meta_whatsapp_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_timestamp: Option<OffsetDateTime>,
    /// Last updated at.
    #[serde(
        default,
        with = "meta_whatsapp_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_timestamp: Option<OffsetDateTime>,
}

/// A payment configuration to create, with
/// [`PaymentConfigurations::create`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct CreatePaymentConfiguration {
    /// Its name, for orders: 1 to 60 characters (the longest an order's
    /// `configuration_name` takes).
    pub configuration_name: String,
    /// The provider.
    pub provider_name: PaymentProvider,
    /// Merchant category code (`0000`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merchant_category_code: Option<String>,
    /// Purpose code (`00`). The table calls it an object; both examples
    /// send a string, as here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose_code: Option<String>,
    /// The merchant's UPI handle (a `upi_vpa` configuration).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merchant_vpa: Option<String>,
    /// Where the merchant lands after linking the gateway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_url: Option<String>,
}

impl CreatePaymentConfiguration {
    /// A configuration paid through `provider` (a gateway: the answer
    /// carries the OAuth link to link the merchant's account).
    pub fn new(configuration_name: impl Into<String>, provider_name: PaymentProvider) -> Self {
        Self {
            configuration_name: configuration_name.into(),
            provider_name,
            merchant_category_code: None,
            purpose_code: None,
            merchant_vpa: None,
            redirect_url: None,
        }
    }

    /// A `upi_vpa` configuration paying `merchant_vpa`.
    pub fn upi_vpa(configuration_name: impl Into<String>, merchant_vpa: impl Into<String>) -> Self {
        let mut c = Self::new(configuration_name, PaymentProvider::UpiVpa);
        c.merchant_vpa = Some(merchant_vpa.into());
        c
    }

    /// Set `merchant_category_code`.
    #[must_use]
    pub fn merchant_category_code(mut self, code: impl Into<String>) -> Self {
        self.merchant_category_code = Some(code.into());
        self
    }

    /// Set `purpose_code`.
    #[must_use]
    pub fn purpose_code(mut self, code: impl Into<String>) -> Self {
        self.purpose_code = Some(code.into());
        self
    }

    /// Set `redirect_url`.
    #[must_use]
    pub fn redirect_url(mut self, url: impl Into<String>) -> Self {
        self.redirect_url = Some(url.into());
        self
    }

    /// The name has 1 to 60 characters; the optional fields, when set, are
    /// not empty.
    ///
    /// # Errors
    ///
    /// A [`ValidationError`] naming the field.
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        validate::text(
            "configuration_name",
            &self.configuration_name,
            CONFIGURATION_NAME_MAX,
        )?;
        if let PaymentProvider::Other(name) = &self.provider_name {
            validate::non_empty("provider_name", name)?;
        }
        for (field, value) in [
            ("merchant_category_code", &self.merchant_category_code),
            ("purpose_code", &self.purpose_code),
            ("merchant_vpa", &self.merchant_vpa),
            ("redirect_url", &self.redirect_url),
        ] {
            validate::opt_text(field, value.as_deref(), usize::MAX)?;
        }
        Ok(())
    }
}

/// An OAuth link that links a payment configuration to the merchant's
/// gateway account: from [`PaymentConfigurations::create`] (a gateway
/// configuration) and [`PaymentConfigurations::regenerate_oauth_link`].
///
/// The link is a [`SecretUrl`]: it links whatever account opens it, so it
/// goes to the merchant only. No page says where its credential sits, so
/// this type's `Debug` hides the link's path too, not only its query
/// ([`RedactedPath`]: scheme, host and port shown); the field's own
/// `Debug` ([`SecretUrl`]'s) shows the path. A refusal that is not Meta's
/// error envelope reaches the caller without its body.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PaymentOauthLink {
    /// The link (absent for a `upi_vpa` configuration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_url: Option<SecretUrl>,
    /// When the link expires.
    #[serde(
        default,
        with = "meta_whatsapp_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub expiration: Option<OffsetDateTime>,
}

impl fmt::Debug for PaymentOauthLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PaymentOauthLink")
            .field(
                "oauth_url",
                &self
                    .oauth_url
                    .as_ref()
                    .map(|u| RedactedPath(u.expose_secret())),
            )
            .field("expiration", &self.expiration)
            .finish()
    }
}

#[derive(Deserialize)]
struct Created {
    #[serde(flatten)]
    link: PaymentOauthLink,
    success: bool,
}

#[derive(Deserialize)]
struct DataList<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

#[derive(Deserialize)]
struct ConfigurationsEntry {
    #[serde(default)]
    payment_configurations: Vec<PaymentConfiguration>,
}

#[derive(Serialize)]
struct NameBody<'a> {
    configuration_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    redirect_url: Option<&'a str>,
}

fn check_name(configuration_name: &str) -> Result<()> {
    validate::text(
        "configuration_name",
        configuration_name,
        CONFIGURATION_NAME_MAX,
    )?;
    Ok(())
}

impl PaymentConfigurations {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// `GET /{WABA_ID}/payment_configurations`: every payment
    /// configuration of the WABA. The page documents no pagination.
    pub async fn list(&self) -> Result<Vec<PaymentConfiguration>> {
        let list: DataList<ConfigurationsEntry> = self
            .client
            .get_at(&[self.waba_id.as_str(), "payment_configurations"])
            .context("payment configurations")
            .send()
            .await?;
        Ok(list
            .data
            .into_iter()
            .flat_map(|e| e.payment_configurations)
            .collect())
    }

    /// `GET /{WABA_ID}/payment_configuration/{CONFIGURATION_NAME}`: one
    /// configuration (`None` when Meta's `data` list is empty). The name
    /// is one path segment.
    pub async fn get(&self, configuration_name: &str) -> Result<Option<PaymentConfiguration>> {
        validate::non_empty("configuration_name", configuration_name)?;
        let list: DataList<PaymentConfiguration> = self
            .client
            .get_at(&[
                self.waba_id.as_str(),
                "payment_configuration",
                configuration_name,
            ])
            .context("payment configuration")
            .send()
            .await?;
        Ok(list.data.into_iter().next())
    }

    /// `POST /{WABA_ID}/payment_configuration`: create a configuration.
    /// For a gateway, the answer's OAuth link is what the merchant opens to
    /// link their account; a `upi_vpa` configuration has none.
    ///
    /// Checked first ([`CreatePaymentConfiguration::validate`]). An answer
    /// of `"success": false` is an [`Error::Http`], and so is a refusal
    /// that is not Meta's error envelope, its body withheld (it may hold
    /// the link). **Never replayed** on a timeout or a server error.
    pub async fn create(&self, request: &CreatePaymentConfiguration) -> Result<PaymentOauthLink> {
        request.validate()?;
        let created: Created = self
            .client
            .post_at(&[self.waba_id.as_str(), "payment_configuration"])
            .json(request)
            .idempotent(false)
            .context("payment configuration creation response")
            .send_withheld(OAUTH_WITHHELD)
            .await
            .map_err(withhold_http_body)?;
        if !created.success {
            return Err(Error::Http {
                status: 200,
                body_snippet: "payment configuration creation answered `success: false`".into(),
            });
        }
        Ok(created.link)
    }

    /// `POST /{WABA_ID}/generate_payment_configuration_oauth_link`: a new
    /// OAuth link for a gateway configuration, which lands the merchant on
    /// `redirect_url` once linked. **Never replayed** on a timeout or a
    /// server error (a replay would issue another link). A refusal that is
    /// not Meta's error envelope is an [`Error::Http`] without its body.
    pub async fn regenerate_oauth_link(
        &self,
        configuration_name: &str,
        redirect_url: Option<&str>,
    ) -> Result<PaymentOauthLink> {
        check_name(configuration_name)?;
        validate::opt_text("redirect_url", redirect_url, usize::MAX)?;
        self.client
            .post_at(&[
                self.waba_id.as_str(),
                "generate_payment_configuration_oauth_link",
            ])
            .json(&NameBody {
                configuration_name,
                redirect_url,
            })
            .idempotent(false)
            .context("payment configuration OAuth link response")
            .send_withheld(OAUTH_WITHHELD)
            .await
            .map_err(withhold_http_body)
    }

    /// `DELETE /{WABA_ID}/payment_configuration`, the name in the JSON
    /// body: remove a configuration. Send no more orders naming it first:
    /// their buyers could not pay. Replayed on transient errors: a second
    /// deletion removes nothing more, but when the first one went through
    /// unanswered, the replay can answer an error (the configuration is
    /// gone); [`Self::get`] tells.
    pub async fn delete(&self, configuration_name: &str) -> Result<()> {
        check_name(configuration_name)?;
        self.client
            .delete_at(&[self.waba_id.as_str(), "payment_configuration"])
            .json(&NameBody {
                configuration_name,
                redirect_url: None,
            })
            .context("payment configuration removal response")
            .send_success()
            .await
    }
}
