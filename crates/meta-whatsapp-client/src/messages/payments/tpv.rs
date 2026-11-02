//! Third party validation (TPV, alpha: `payments/payments-in/pg`, "Third
//! party validation with Razorpay and `PayU` payment gateways"): the
//! merchant names the bank accounts an order must be paid from, encrypted
//! with a public key the payment gateway provides, in a payment setting of
//! the gateway's own type: `{"type": "razorpay", "razorpay":
//! {"encrypted_payment_gateway_data": …}}`.
//!
//! The encryption is the gateway's (the page says to work with the gateway
//! to get its public key) and is not done here: the caller passes the
//! ciphertext. The page shows the plaintext's shape for each gateway
//! (Razorpay: `bank_account` with `account_number`, `name`, `ifsc`;
//! `PayU`: `beneficiaryDetail` with `beneficiaryAccountNumber` and
//! `ifscCode`, several separated by `|`), "something like": it is not
//! modelled either.

use std::fmt;

use meta_whatsapp_core::secret::SecretBytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::messages::validate::{self, Check};

/// The gateway a third party validation setting is for: the two the page
/// names. A closed set: the setting's `type` is the gateway's name, and a
/// gateway the page does not name has no documented setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TpvGateway {
    /// `razorpay`.
    Razorpay,
    /// `payu`.
    Payu,
}

impl TpvGateway {
    /// The setting's `type`, and the key of its object.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Razorpay => "razorpay",
            Self::Payu => "payu",
        }
    }
}

/// `encrypted_payment_gateway_data`: the buyer's bank accounts, encrypted
/// with the gateway's public key.
///
/// Ciphertext of the buyer's bank details: `Debug` never shows it, and
/// reading it takes [`Self::expose_secret`]. `Serialize` writes it as
/// given (it is what the send carries).
#[derive(Clone)]
pub struct EncryptedPaymentGatewayData(SecretBytes);

impl EncryptedPaymentGatewayData {
    /// Wrap the ciphertext, as the gateway's encryption produced it.
    pub fn new(ciphertext: impl Into<String>) -> Self {
        Self(SecretBytes::new(ciphertext.into().into_bytes()))
    }

    /// Read the ciphertext. Keep the borrow short; never log it.
    pub fn expose_secret(&self) -> &str {
        // Built from a `String`: always UTF-8, so the fallback is
        // unreachable.
        std::str::from_utf8(self.0.expose_secret()).unwrap_or_default()
    }
}

impl PartialEq for EncryptedPaymentGatewayData {
    fn eq(&self, other: &Self) -> bool {
        self.0.expose_secret() == other.0.expose_secret()
    }
}

impl Eq for EncryptedPaymentGatewayData {}

impl fmt::Debug for EncryptedPaymentGatewayData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncryptedPaymentGatewayData([REDACTED])")
    }
}

impl Serialize for EncryptedPaymentGatewayData {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.expose_secret())
    }
}

impl<'de> Deserialize<'de> for EncryptedPaymentGatewayData {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

/// A third party validation payment setting (INR): which accounts the
/// buyer pays from, for `gateway`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThirdPartyValidation {
    /// Razorpay or `PayU`.
    pub gateway: TpvGateway,
    /// The encrypted accounts.
    pub encrypted_payment_gateway_data: EncryptedPaymentGatewayData,
}

impl ThirdPartyValidation {
    /// `data` for `gateway`.
    pub fn new(gateway: TpvGateway, data: EncryptedPaymentGatewayData) -> Self {
        Self {
            gateway,
            encrypted_payment_gateway_data: data,
        }
    }

    /// The setting's object, `{"encrypted_payment_gateway_data": …}`.
    pub(super) fn body(&self) -> TpvBody<'_> {
        TpvBody {
            encrypted_payment_gateway_data: &self.encrypted_payment_gateway_data,
        }
    }

    pub(super) fn validate(&self, field: &str) -> Check {
        validate::non_empty(
            &format!("{field}.encrypted_payment_gateway_data"),
            self.encrypted_payment_gateway_data.expose_secret(),
        )
    }
}

/// The wire object of a [`ThirdPartyValidation`].
#[derive(Serialize)]
pub(super) struct TpvBody<'a> {
    encrypted_payment_gateway_data: &'a EncryptedPaymentGatewayData,
}

/// What a [`TpvBody`] parses from.
#[derive(Deserialize)]
pub(super) struct TpvRaw {
    pub(super) encrypted_payment_gateway_data: EncryptedPaymentGatewayData,
}
