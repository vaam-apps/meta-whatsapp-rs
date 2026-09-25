//! Secret-bearing inputs of the phone number lifecycle.
//!
//! Both are held in [`SecretBytes`] (zeroed on drop). What they guarantee:
//! `Debug` never prints the value, there is no `Display`, no `Serialize` (a
//! PIN cannot end up in a JSON log by accident), and reading the value takes
//! an explicit `expose_secret()`. The request body that carries the value to
//! Meta is an ordinary buffer; that copy is outside this type's reach.

use std::fmt;

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::secret::SecretBytes;

/// A two-step verification PIN: exactly six ASCII digits.
///
/// Required by `register` and settable with `set_two_step_pin`. Meta
/// documents the PIN as "a 6-digit number"; anything else is rejected here,
/// before a request is made, because a wrong-format PIN still counts against
/// the 10-registrations-per-72-hours limit.
#[derive(Clone)]
pub struct TwoStepPin(SecretBytes);

impl TwoStepPin {
    /// Validate and wrap a PIN.
    pub fn new(pin: impl Into<String>) -> Result<Self> {
        let pin = SecretBytes::new(pin.into().into_bytes());
        let digits = pin.expose_secret();
        if digits.len() == 6 && digits.iter().all(u8::is_ascii_digit) {
            Ok(Self(pin))
        } else {
            Err(ValidationError::new("pin", "must be exactly 6 digits").into())
        }
    }

    /// Read the PIN. Keep the borrow short; never log it.
    pub fn expose_secret(&self) -> &str {
        ascii(&self.0)
    }
}

impl PartialEq for TwoStepPin {
    fn eq(&self, other: &Self) -> bool {
        self.0.expose_secret() == other.0.expose_secret()
    }
}

impl Eq for TwoStepPin {}

impl fmt::Debug for TwoStepPin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TwoStepPin([REDACTED])")
    }
}

/// A phone number verification code, as delivered by SMS or voice call.
///
/// Meta shows the SMS as `WhatsApp code 123-830` and asks for the code
/// "without the hyphen"; [`VerificationCode::new`] drops hyphens so the
/// string a user copies works as-is, then requires digits only.
#[derive(Clone)]
pub struct VerificationCode(SecretBytes);

impl VerificationCode {
    /// Normalize (drop `-`) and validate a code.
    pub fn new(code: impl Into<String>) -> Result<Self> {
        let raw = SecretBytes::new(code.into().into_bytes());
        let code = SecretBytes::new(
            raw.expose_secret()
                .iter()
                .copied()
                .filter(|b| *b != b'-')
                .collect::<Vec<u8>>(),
        );
        let digits = code.expose_secret();
        if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
            Ok(Self(code))
        } else {
            Err(ValidationError::new("code", "must be the numeric verification code").into())
        }
    }

    /// Read the code. Keep the borrow short; never log it.
    pub fn expose_secret(&self) -> &str {
        ascii(&self.0)
    }
}

impl PartialEq for VerificationCode {
    fn eq(&self, other: &Self) -> bool {
        self.0.expose_secret() == other.0.expose_secret()
    }
}

impl Eq for VerificationCode {}

impl fmt::Debug for VerificationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VerificationCode([REDACTED])")
    }
}

/// The text of a secret validated as ASCII digits on construction, so the
/// fallback is unreachable.
fn ascii(bytes: &SecretBytes) -> &str {
    std::str::from_utf8(bytes.expose_secret()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_must_be_six_ascii_digits() {
        assert!(TwoStepPin::new("123456").is_ok());
        for bad in ["12345", "1234567", "12345a", "", "１２３４５６", " 23456"] {
            let err = TwoStepPin::new(bad).unwrap_err();
            assert!(err.to_string().contains("`pin`"), "{bad:?}: {err}");
            assert!(!err.to_string().contains(bad) || bad.is_empty(), "{err}");
        }
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        let pin = TwoStepPin::new("581063").unwrap();
        assert_eq!(format!("{pin:?}"), "TwoStepPin([REDACTED])");
        assert_eq!(pin.expose_secret(), "581063");
        assert_eq!(pin, TwoStepPin::new("581063").unwrap());
        let code = VerificationCode::new("123-830").unwrap();
        assert_eq!(format!("{code:?}"), "VerificationCode([REDACTED])");
        assert_eq!(code.expose_secret(), "123830");
    }

    #[test]
    fn verification_code_is_numeric() {
        assert!(VerificationCode::new("").is_err());
        assert!(VerificationCode::new("-").is_err());
        assert!(VerificationCode::new("12a4").is_err());
    }
}
