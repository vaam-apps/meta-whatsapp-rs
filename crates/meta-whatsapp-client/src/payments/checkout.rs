//! The **checkout endpoint** of India's checkout button templates
//! (`payments/payments-in/checkout-button-templates`, beta, feature
//! `flows-endpoint`): your HTTPS endpoint that WhatsApp calls while the
//! buyer checks out, to list coupons, apply or remove one, and price
//! shipping to the address picked.
//!
//! WhatsApp encrypts the exchange "the same method that WhatsApp Flows
//! endpoint uses": the request body is an
//! [`EncryptedFlowRequest`], decrypted with your
//! [`FlowEndpointKey`] (the business key pair whose public half is
//! uploaded with
//! [`BusinessEncryption::set_public_key`](crate::flows::BusinessEncryption::set_public_key))
//! by [`FlowEndpointKey::decrypt_checkout_request`]; the answer is sealed
//! by the returned [`ResponseSealer`], under the request's AES key, as
//! `text/plain` ([`RESPONSE_CONTENT_TYPE`](crate::flows::endpoint::RESPONSE_CONTENT_TYPE)).
//! A request that does not decrypt is answered HTTP 421
//! ([`EndpointStatus::DecryptionFailed`](crate::flows::endpoint::EndpointStatus::DecryptionFailed)),
//! as the page says. Nothing here re-implements the crypto: it is the
//! Flows endpoint's.
//!
//! Link the endpoint to a payment gateway configuration first
//! ([`PaymentConfigurations::link_data_endpoint`](super::PaymentConfigurations::link_data_endpoint));
//! Meta enables the link per account on request.
//!
//! # The exchange
//!
//! Every request is a `data_exchange` with a [`CheckoutSubAction`] and the
//! order ([`OrderDetailsParameters`], without its payment settings) plus
//! the buyer's input ([`CheckoutInput`]: their phone number as `user_id`,
//! the coupon they picked, the address they submitted):
//!
//! | `sub_action` | You answer |
//! | --- | --- |
//! | `get_coupons` | the coupons for this order and buyer: [`CheckoutResponse::coupons`] |
//! | `apply_coupon` | the order repriced, the coupon attached ([`OrderDetailsParameters::coupon`]): [`CheckoutResponse::order`] |
//! | `remove_coupon` | the order repriced, the coupon removed ([`OrderDetailsParameters::without_coupon`]) |
//! | `apply_shipping` | the order repriced for the address, which the answer's `shipping_info.selected_address` echoes |
//!
//! [`CheckoutResponse::validate`] checks an answer before you seal it:
//! India's order rules, no payment settings, the total equal to
//! `subtotal + tax + shipping - discount - coupon discount` (the page's
//! examples), a coupon attached after `apply_coupon` and none after
//! `remove_coupon`.
//!
//! # Trust nothing in a request
//!
//! The page does not say whether checkout requests carry
//! `X-Hub-Signature-256`, as a Flow endpoint's do. Your public key is
//! public, so anyone can encrypt a well-formed request to you: if your
//! requests carry the header, verify it first
//! ([`verify_request_signature`](crate::flows::endpoint::verify_request_signature)),
//! and in every case price the answer from your own catalog and records,
//! never from the request's amounts, and check the coupon against the
//! order and the buyer yourself.
//!
//! # What the page leaves out
//!
//! Only the four sub-actions are documented: no health check, no error
//! notification and no error answer. A request with neither (a `ping`,
//! should one come) parses with [`CheckoutRequest::sub_action`] and
//! [`CheckoutRequest::data`] empty; answer it as a Flow endpoint's
//! ([`FlowResponse::health_check`](crate::flows::endpoint::FlowResponse::health_check)).
//! A request whose order does not parse typed (a currency other than
//! `INR`, a field missing) is a [`CryptoError::Decrypt`], as for a Flow
//! endpoint: nothing about it leaves the process.
//!
//! ```no_run
//! use meta_whatsapp_client::flows::endpoint::{
//!     EncryptedFlowRequest, EndpointStatus, FlowEndpointKey, FlowResponse,
//! };
//! use meta_whatsapp_client::payments::checkout::{
//!     CheckoutResponse, CheckoutSubAction, Coupon,
//! };
//! use meta_whatsapp_client::payments::{Amount, AppliedCoupon};
//!
//! /// `body`: the request body as received. Returns the status and body.
//! fn checkout(key: &FlowEndpointKey, body: &[u8]) -> Result<(u16, String), Box<dyn std::error::Error>> {
//!     let encrypted: EncryptedFlowRequest = serde_json::from_slice(body)?;
//!     let Ok((request, sealer)) = key.decrypt_checkout_request(&encrypted) else {
//!         return Ok((EndpointStatus::DecryptionFailed.code(), String::new()));
//!     };
//!     let (Some(sub_action), Some(data)) = (&request.sub_action, &request.data) else {
//!         return Ok((EndpointStatus::Ok.code(), sealer.seal(&FlowResponse::health_check())?));
//!     };
//!     // Reprice from your own records, not from the request's amounts.
//!     let order = data.order_details.clone();
//!     let response = match sub_action {
//!         CheckoutSubAction::GetCoupons => CheckoutResponse::coupons(
//!             &request.version,
//!             [Coupon::new("TRYNEW20", "try_new_20_id", "Save ₹20 on the order")],
//!         ),
//!         CheckoutSubAction::ApplyCoupon => {
//!             let code = data.input.coupon.as_ref().map(|c| c.code.clone()).unwrap_or_default();
//!             let order = order.coupon(AppliedCoupon::new(code, Amount::inr(2000)));
//!             CheckoutResponse::order(&request.version, sub_action.clone(), order)
//!         }
//!         CheckoutSubAction::RemoveCoupon => {
//!             CheckoutResponse::order(&request.version, sub_action.clone(), order.without_coupon())
//!         }
//!         _ => CheckoutResponse::order(&request.version, sub_action.clone(), order),
//!     };
//!     response.validate()?;
//!     Ok((EndpointStatus::Ok.code(), sealer.seal(&response)?))
//! }
//! ```

use meta_whatsapp_core::error::{CryptoError, ValidationError};
use meta_whatsapp_core::open_enum;
use serde::{Deserialize, Serialize};

use crate::flows::endpoint::{
    EncryptedFlowRequest, EndpointAction, FlowEndpointKey, ResponseSealer,
};
use crate::messages::payments::{OrderContext, OrderDetailsParameters, ShippingAddress};
use crate::messages::validate;

open_enum! {
    /// What the buyer did (`sub_action` of a checkout endpoint request,
    /// echoed in the answer).
    pub enum CheckoutSubAction {
        /// Tapped "Apply a savings offer": list the coupons.
        GetCoupons => "get_coupons",
        /// Picked or typed a coupon: reprice the order with it.
        ApplyCoupon => "apply_coupon",
        /// Removed the applied coupon: reprice the order without it.
        RemoveCoupon => "remove_coupon",
        /// Submitted a shipping address: check stock and delivery, price
        /// the shipping.
        ApplyShipping => "apply_shipping",
    }
}

/// A decrypted checkout endpoint request.
///
/// `Debug` shows the buyer's phone number and addresses: personal data,
/// not credentials (log it as such).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CheckoutRequest {
    /// The exchange's version, `1.0` in the page's examples; the answer
    /// echoes it.
    pub version: String,
    /// `data_exchange` in every documented request (the Flows endpoint's
    /// [`EndpointAction`]).
    pub action: EndpointAction,
    /// What the buyer did; `None` in a request that is none of the four
    /// (undocumented).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_action: Option<CheckoutSubAction>,
    /// The order and the buyer's input; `None` with `sub_action`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<CheckoutRequestData>,
}

/// `data` of a checkout endpoint request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CheckoutRequestData {
    /// The order as the buyer sees it, without its payment settings, with
    /// the coupon applied so far.
    pub order_details: OrderDetailsParameters,
    /// What the buyer gave.
    pub input: CheckoutInput,
}

/// `data.input` of a checkout endpoint request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CheckoutInput {
    /// The buyer's phone number, as the page calls it ("passes the user
    /// phone number"), digits with the country code (`919000090000`).
    pub user_id: String,
    /// `apply_coupon`: the coupon picked or typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupon: Option<SelectedCoupon>,
    /// `apply_shipping`: the address submitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_address: Option<ShippingAddress>,
}

/// `input.coupon`: the code the buyer picked or typed, not yet checked by
/// anyone (check it is yours, valid for this order and buyer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SelectedCoupon {
    /// The code (`WELCOME70`).
    pub code: String,
}

/// One coupon of a `get_coupons` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coupon {
    /// The code the buyer applies.
    pub code: String,
    /// Your id of the coupon.
    pub id: String,
    /// What it gives, shown to the buyer.
    pub description: String,
}

impl Coupon {
    /// The coupon `code`, your `id` for it, and its `description`.
    pub fn new(
        code: impl Into<String>,
        id: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            id: id.into(),
            description: description.into(),
        }
    }
}

/// The answer to a checkout endpoint request, before sealing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutResponse {
    /// The request's version.
    pub version: String,
    /// The request's sub-action.
    pub sub_action: CheckoutSubAction,
    /// The coupons, or the repriced order.
    pub data: CheckoutResponseData,
}

/// `data` of a checkout endpoint answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CheckoutResponseData {
    /// `get_coupons`: the coupons on offer, `{"coupons": […]}`.
    Coupons {
        /// The coupons.
        coupons: Vec<Coupon>,
    },
    /// `apply_coupon`, `remove_coupon`, `apply_shipping`: the order,
    /// repriced, `{"order_details": {…}}`.
    Order {
        /// The order, without payment settings.
        order_details: Box<OrderDetailsParameters>,
    },
}

impl CheckoutResponse {
    /// The answer to `get_coupons`: `coupons`, under the request's
    /// `version`.
    pub fn coupons(version: impl Into<String>, coupons: impl IntoIterator<Item = Coupon>) -> Self {
        Self {
            version: version.into(),
            sub_action: CheckoutSubAction::GetCoupons,
            data: CheckoutResponseData::Coupons {
                coupons: coupons.into_iter().collect(),
            },
        }
    }

    /// The answer to `sub_action` (`apply_coupon`, `remove_coupon`,
    /// `apply_shipping`): `order_details` repriced, under the request's
    /// `version`. Set its total last ([`OrderDetailsParameters::coupon`],
    /// [`without_coupon`](OrderDetailsParameters::without_coupon) and
    /// [`with_expected_total`](OrderDetailsParameters::with_expected_total)
    /// do).
    pub fn order(
        version: impl Into<String>,
        sub_action: CheckoutSubAction,
        order_details: OrderDetailsParameters,
    ) -> Self {
        Self {
            version: version.into(),
            sub_action,
            data: CheckoutResponseData::Order {
                order_details: Box::new(order_details),
            },
        }
    }

    /// Check the answer before sealing it: a version; the data its
    /// sub-action takes (coupons for `get_coupons`, the order for the
    /// other three; a sub-action this client does not know is refused, as
    /// it cannot tell what to answer); the order by India's rules (every
    /// check of [`OrderDetails::validate`](super::OrderDetails::validate)
    /// for INR, and its shipping info's limits), without payment settings,
    /// its total `subtotal + tax + shipping - discount - coupon discount`;
    /// a coupon attached after `apply_coupon` ("attach the coupon with the
    /// order"), none after `remove_coupon` ("remove the coupon attached
    /// with the order").
    ///
    /// # Errors
    ///
    /// A [`ValidationError`] naming the first value that breaks a rule,
    /// under `data`.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate::non_empty("version", &self.version)?;
        let order = match (&self.sub_action, &self.data) {
            (CheckoutSubAction::GetCoupons, CheckoutResponseData::Coupons { .. }) => return Ok(()),
            (
                CheckoutSubAction::ApplyCoupon
                | CheckoutSubAction::RemoveCoupon
                | CheckoutSubAction::ApplyShipping,
                CheckoutResponseData::Order { order_details },
            ) => order_details,
            (CheckoutSubAction::Other(_), _) => {
                return Err(ValidationError::new(
                    "sub_action",
                    "a sub-action this client knows the answer to",
                ));
            }
            (CheckoutSubAction::GetCoupons, _) => {
                return Err(ValidationError::new(
                    "data",
                    "a `get_coupons` answer carries `coupons`",
                ));
            }
            _ => {
                return Err(ValidationError::new(
                    "data",
                    "this sub-action's answer carries the repriced `order_details`",
                ));
            }
        };
        match (&self.sub_action, &order.coupon) {
            (CheckoutSubAction::ApplyCoupon, None) => {
                return Err(ValidationError::new(
                    "data.order_details.coupon",
                    "an `apply_coupon` answer attaches the coupon to the order",
                ));
            }
            (CheckoutSubAction::RemoveCoupon, Some(_)) => {
                return Err(ValidationError::new(
                    "data.order_details.coupon",
                    "a `remove_coupon` answer removes the coupon from the order",
                ));
            }
            _ => {}
        }
        order.validate_at("data.order_details", OrderContext::Checkout)
    }
}

impl FlowEndpointKey {
    /// Decrypt a checkout endpoint request
    /// ([`crate::payments::checkout`]) and return it with the
    /// [`ResponseSealer`] that encrypts your [`CheckoutResponse`] to it:
    /// the Flows endpoint's crypto ([`Self::decrypt_request`]), into the
    /// checkout's payload.
    ///
    /// Every failure is [`CryptoError::Decrypt`], a payload that does not
    /// parse as a [`CheckoutRequest`] included; answer it with
    /// [`EndpointStatus::DecryptionFailed`](crate::flows::endpoint::EndpointStatus::DecryptionFailed)
    /// (HTTP 421).
    pub fn decrypt_checkout_request(
        &self,
        request: &EncryptedFlowRequest,
    ) -> Result<(CheckoutRequest, ResponseSealer), CryptoError> {
        self.decrypt_as(request)
    }
}
