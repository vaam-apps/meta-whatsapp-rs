//! Payments API webhooks, both in the `messages` field: India's status of
//! `type` `payment` ([`StatusPayment`], on a
//! [`Status`](super::Status)) and Brazil's `payment_method` reply
//! ([`PaymentMethodReply`], an
//! [`InteractiveReply`](super::InteractiveReply)).
//!
//! Doc paths (relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`):
//! `payments/payments-in/pg` ("Receive webhook about transaction status")
//! and `payments/payments-br/one-click-payments` ("Receive the webhook
//! notification"); `payments/payments-br/payment-request-cta` (a payment
//! request's `credential_id` comes back in the reply).
//!
//! # Lenient on purpose
//!
//! A webhook that fails its typed parse falls back to `Unknown` as a whole
//! change, so only the identifiers are required here: the order's
//! `reference_id`, a transaction's and a refund's `id`, and the reply's
//! `payment_method`. Every other property the tables call required is an
//! `Option`; an amount, a method and an error need all their fields, and
//! one that lacks any (or holds another JSON type) is `None`, as if
//! absent. `refunds` may be `null`, `notes` `[]`.
//!
//! Neither object can fail the change it arrives in: before this crate
//! typed them, a payment's status parsed as a status and the reply as a
//! message, whatever they held, and they still do. A `payment` that does
//! not parse typed leaves [`Status::payment`](super::Status::payment)
//! `None`; a `payment_method` reply that does not parse typed is an
//! [`InteractiveReply::Unknown`](super::InteractiveReply::Unknown).
//!
//! # Where the pages disagree
//!
//! - `pg`'s table calls a transaction's `created_timestamp` and
//!   `updated_timestamp` integers, and its example prints placeholder
//!   strings: both forms parse (unix seconds).
//! - The webhook carries one `transaction`, with the order's `refunds`
//!   next to it; the payment lookup answers a `transactions` array, with
//!   each transaction's `refunds` inside. Each shape is the one its page
//!   documents.
//! - One-click's table calls the order message's `credential_id`
//!   required; the payment request page makes it optional, and says the
//!   reply carries it when it was sent: it is optional here.
//!
//! # Payment data, not secrets
//!
//! A card's last four digits and a `credential_id` (the merchant's own id
//! of a stored card, as `meta_whatsapp_client::payments::OffsiteCardPay`
//! says) are echoed back to the merchant: `Debug` shows them, as the
//! client's does.

use std::collections::BTreeMap;

use meta_whatsapp_core::open_enum;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// An amount of money: `value` is the amount multiplied by `offset`
/// (₹210.00 is `value` 21000, `offset` 100).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaymentAmount {
    /// The amount times `offset`.
    pub value: u64,
    /// The divisor of `value` (`100`).
    pub offset: u32,
}

/// `statuses[].payment`: the payment a status of `type` `payment` is
/// about (`payments/payments-in/pg`).
///
/// The gateway's own fields come back as they were sent in the
/// `order_details` message: `receipt` and `notes` for Razorpay, `udf1` to
/// `udf4` for `PayU`, `additional_info1` to `additional_info7` for
/// `BillDesk`, `extra1` and `extra2` for Zaakpay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusPayment {
    /// The `reference_id` of the `order_details` message.
    pub reference_id: String,
    /// What the buyer paid.
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::lenient_option::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub amount: Option<PaymentAmount>,
    /// The currency: "always INR".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// The transaction attempt this status reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<PaymentTransaction>,
    /// The order's refunds (`null` is none).
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::vec_or_null::deserialize",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub refunds: Vec<PaymentRefund>,
    /// Razorpay: the `receipt` sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
    /// Razorpay: the `notes` sent, key-value pairs (a value that is not a
    /// string is kept as its JSON text).
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::string_map::deserialize",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub notes: BTreeMap<String, String>,
    /// `PayU`: `udf1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udf1: Option<String>,
    /// `PayU`: `udf2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udf2: Option<String>,
    /// `PayU`: `udf3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udf3: Option<String>,
    /// `PayU`: `udf4`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udf4: Option<String>,
    /// `BillDesk`: `additional_info1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_info1: Option<String>,
    /// `BillDesk`: `additional_info2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_info2: Option<String>,
    /// `BillDesk`: `additional_info3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_info3: Option<String>,
    /// `BillDesk`: `additional_info4`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_info4: Option<String>,
    /// `BillDesk`: `additional_info5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_info5: Option<String>,
    /// `BillDesk`: `additional_info6`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_info6: Option<String>,
    /// `BillDesk`: `additional_info7`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_info7: Option<String>,
    /// Zaakpay: `extra1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra1: Option<String>,
    /// Zaakpay: `extra2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra2: Option<String>,
}

open_enum! {
    /// `transaction.type`: the payment gateway.
    pub enum PaymentGatewayType {
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
    /// `transaction.status`.
    pub enum PaymentTransactionStatus {
        /// Not settled yet.
        Pending => "pending",
        /// Paid.
        Success => "success",
        /// Failed; see [`PaymentTransaction::error`].
        Failed => "failed",
    }
}

open_enum! {
    /// `transaction.method.type`: how the buyer paid.
    pub enum PaymentMethodType {
        /// UPI.
        Upi => "upi",
        /// A card.
        Card => "card",
        /// A wallet.
        Wallet => "wallet",
        /// Net banking.
        Netbanking => "netbanking",
    }
}

/// `payment.transaction`: one attempt to pay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentTransaction {
    /// The payment gateway's order id.
    pub id: String,
    /// The payment gateway's payment id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pg_transaction_id: Option<String>,
    /// The gateway.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub gateway: Option<PaymentGatewayType>,
    /// The attempt's status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PaymentTransactionStatus>,
    /// When the attempt was created.
    #[serde(
        default,
        with = "meta_whatsapp_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_timestamp: Option<OffsetDateTime>,
    /// When it last changed.
    #[serde(
        default,
        with = "meta_whatsapp_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_timestamp: Option<OffsetDateTime>,
    /// How the buyer paid; may be absent on a failed attempt.
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::lenient_option::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub method: Option<TransactionMethod>,
    /// Why it failed, as the gateway says; not on every attempt.
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::lenient_option::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub error: Option<TransactionError>,
}

/// `transaction.method`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionMethod {
    /// The kind of payment method.
    #[serde(rename = "type")]
    pub method_type: PaymentMethodType,
}

/// `transaction.error`: the gateway's reason for a failure, passed on by
/// Meta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionError {
    /// The gateway's error code.
    pub code: String,
    /// The gateway's reason, in plain text.
    pub reason: String,
}

open_enum! {
    /// `refunds[].speed_processed`.
    pub enum RefundSpeed {
        /// Instant.
        Instant => "instant",
        /// Normal.
        Normal => "normal",
    }
}

open_enum! {
    /// `refunds[].status`.
    pub enum RefundStatus {
        /// Not settled yet.
        Pending => "pending",
        /// Refunded.
        Success => "success",
        /// Failed.
        Failed => "failed",
    }
}

/// `payment.refunds[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentRefund {
    /// The refund's id.
    pub id: String,
    /// How much was refunded.
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::lenient_option::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub amount: Option<PaymentAmount>,
    /// How fast it was processed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_processed: Option<RefundSpeed>,
    /// Its status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RefundStatus>,
    /// When it was created.
    #[serde(
        default,
        with = "meta_whatsapp_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_timestamp: Option<OffsetDateTime>,
    /// When it last changed.
    #[serde(
        default,
        with = "meta_whatsapp_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_timestamp: Option<OffsetDateTime>,
}

open_enum! {
    /// `payment_method.payment_method`: how the buyer chose to pay.
    pub enum PaymentMethodKind {
        /// One-click payment with a card the merchant stores
        /// (`offsite_card_pay`).
        OffsiteCardPay => "offsite_card_pay",
    }
}

/// `interactive.payment_method`: the buyer tapped "Send payment" on a
/// one-click payment (`payments/payments-br/one-click-payments`). It is
/// their confirmation, not a payment: charge the card through your PSP,
/// then send an `order_status` message with the payment's status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentMethodReply {
    /// The payment method chosen.
    pub payment_method: PaymentMethodKind,
    /// When the buyer confirmed.
    #[serde(
        default,
        with = "meta_whatsapp_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub payment_timestamp: Option<OffsetDateTime>,
    /// The `reference_id` of the order.
    pub reference_id: String,
    /// The card's last four digits, as sent in the order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_four_digits: Option<String>,
    /// The `credential_id` sent in the order: your id of the card to
    /// charge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_values_and_missing_optionals_still_parse() {
        let p: StatusPayment = serde_json::from_value(json!({
            "reference_id": "BM345A-12",
            "transaction": {"id": "order_1", "type": "cashfree", "status": "refunded",
                            "method": {"type": "emi"}}
        }))
        .unwrap();
        let t = p.transaction.unwrap();
        assert_eq!(
            t.gateway,
            Some(PaymentGatewayType::Other("cashfree".into()))
        );
        assert_eq!(
            t.status,
            Some(PaymentTransactionStatus::Other("refunded".into()))
        );
        assert_eq!(
            t.method.unwrap().method_type,
            PaymentMethodType::Other("emi".into())
        );
        let r: PaymentMethodReply =
            serde_json::from_value(json!({"payment_method": "pix_redirect", "reference_id": "r"}))
                .unwrap();
        assert_eq!(
            r.payment_method,
            PaymentMethodKind::Other("pix_redirect".into())
        );
        assert_eq!(r.credential_id, None);
    }
}
