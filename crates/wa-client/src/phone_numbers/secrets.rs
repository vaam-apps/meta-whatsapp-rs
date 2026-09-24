//! Secret-bearing inputs of the phone number lifecycle.
//!
//! `wa_core::secret` has no type for a two-step verification PIN or a
//! registration code yet, and this crate may not add a dependency on
//! `secrecy`, so these are small local newtypes. What they guarantee is the
//! part that matters for us: `Debug` never prints the value, there is no
//! `Display`, no `Serialize` (a PIN cannot end up in a JSON log by accident),
//! and reading the value takes an explicit `expose_secret()`. Moving them to
//! `wa_core::secret` (on `SecretString`, which zeroizes) is a pending core
//! change request.

use std::fmt;

use wa_core::Result;
use wa_core::error::ValidationError;

/// A two-step verification PIN: exactly six ASCII digits.
///
/// Required by `register` and settable with `set_two_step_pin`. Meta
/// documents the PIN as "a 6-digit number"; anything else is rejected here,
/// before a request is made, because a wrong-format PIN still counts against
/// the 10-registrations-per-72-hours limit.
#[derive(Clone, PartialEq, Eq)]
pub struct TwoStepPin(String);

impl TwoStepPin {
    /// Validate and wrap a PIN.
    pub fn new(pin: impl Into<String>) -> Result<Self> {
        let pin = pin.into();
        if pin.len() == 6 && pin.bytes().all(|b| b.is_ascii_digit()) {
            Ok(Self(pin))
        } else {
            Err(ValidationError::new("pin", "must be exactly 6 digits").into())
        }
    }

    /// Read the PIN. Keep the borrow short; never log it.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

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
#[derive(Clone, PartialEq, Eq)]
pub struct VerificationCode(String);

impl VerificationCode {
    /// Normalize (drop `-`) and validate a code.
    pub fn new(code: impl Into<String>) -> Result<Self> {
        let code: String = code.into().chars().filter(|c| *c != '-').collect();
        if !code.is_empty() && code.bytes().all(|b| b.is_ascii_digit()) {
            Ok(Self(code))
        } else {
            Err(ValidationError::new("code", "must be the numeric verification code").into())
        }
    }

    /// Read the code. Keep the borrow short; never log it.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for VerificationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VerificationCode([REDACTED])")
    }
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
        }
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        let pin = TwoStepPin::new("581063").unwrap();
        assert_eq!(format!("{pin:?}"), "TwoStepPin([REDACTED])");
        assert_eq!(pin.expose_secret(), "581063");
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
