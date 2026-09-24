//! `X-Hub-Signature-256` verification (`webhooks/create-webhook-endpoint`).
//!
//! Meta signs the **raw** POST body with HMAC-SHA256 under the app secret
//! and sends `sha256=<64 hex chars>`. Verify the bytes exactly as received,
//! before any parsing: re-serialized JSON would not match, and parsing
//! unauthenticated input is attack surface we do not need.
//!
//! Several secrets can be configured so an app secret can be rotated
//! without dropping deliveries signed with the old one.

use std::fmt;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::{Choice, ConstantTimeEq};
use wa_core::error::{ConfigError, WebhookError};
use wa_core::secret::AppSecret;

type HmacSha256 = Hmac<Sha256>;

const PREFIX: &str = "sha256=";

/// Verifies `X-Hub-Signature-256` against one or more app secrets.
#[derive(Clone)]
pub struct SignatureVerifier {
    secrets: Vec<AppSecret>,
}

impl fmt::Debug for SignatureVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignatureVerifier")
            .field("secrets", &self.secrets.len())
            .finish()
    }
}

impl SignatureVerifier {
    /// Build a verifier. Needs at least one secret, and no empty one: an
    /// empty key would make every signature forgeable by anyone who knows it
    /// is empty.
    pub fn new(secrets: Vec<AppSecret>) -> wa_core::Result<Self> {
        if secrets.is_empty() {
            return Err(ConfigError::new("SignatureVerifier needs at least one app secret").into());
        }
        if secrets.iter().any(|s| s.expose_secret().is_empty()) {
            return Err(ConfigError::new("SignatureVerifier: app secret must not be empty").into());
        }
        Ok(Self { secrets })
    }

    /// Check `header` (the `X-Hub-Signature-256` value) against `body`.
    ///
    /// Accepts upper- or lower-case hex. Every configured secret is tried
    /// and compared in constant time, so the timing reveals neither how
    /// much of the signature matched nor which secret did.
    pub fn verify(&self, header: Option<&str>, body: &[u8]) -> Result<(), WebhookError> {
        let header = header.ok_or(WebhookError::MissingSignature)?;
        let hex = header
            .trim()
            .strip_prefix(PREFIX)
            .ok_or(WebhookError::MalformedSignature)?;
        let mut expected = [0u8; 32];
        if hex.len() != 64 || hex::decode_to_slice(hex, &mut expected).is_err() {
            return Err(WebhookError::MalformedSignature);
        }
        let matched = self.secrets.iter().fold(Choice::from(0), |acc, secret| {
            let tag = mac(secret, body).finalize().into_bytes();
            acc | tag.as_slice().ct_eq(&expected)
        });
        if bool::from(matched) {
            Ok(())
        } else {
            Err(WebhookError::SignatureMismatch)
        }
    }
}

/// `sha256=<lowercase hex>` of `body` under `secret`: the header value Meta
/// would send. For tests, and for re-signing a body you forward.
pub fn sign(secret: &AppSecret, body: &[u8]) -> String {
    let tag = mac(secret, body).finalize().into_bytes();
    format!("{PREFIX}{}", hex::encode(tag))
}

fn mac(secret: &AppSecret, body: &[u8]) -> HmacSha256 {
    let key = secret.expose_secret().as_bytes();
    // HMAC accepts keys of any length (`new_from_slice` only fails for MACs
    // with fixed key sizes), so the fallback arm is unreachable; it exists
    // to avoid an `expect` in library code. It keys with zeros, which cannot
    // verify anything Meta signed.
    let mut mac = HmacSha256::new_from_slice(key)
        .unwrap_or_else(|_| HmacSha256::new(&hmac::digest::Key::<HmacSha256>::default()));
    mac.update(body);
    mac
}

#[cfg(test)]
mod tests {
    use super::*;
    use wa_core::Error;

    #[test]
    fn empty_secret_list_and_empty_secret_are_config_errors() {
        assert!(matches!(
            SignatureVerifier::new(vec![]),
            Err(Error::Config(_))
        ));
        assert!(matches!(
            SignatureVerifier::new(vec![AppSecret::new("")]),
            Err(Error::Config(_))
        ));
    }

    #[test]
    fn debug_shows_only_the_count() {
        let v = SignatureVerifier::new(vec![AppSecret::new("s3cr3t")]).unwrap();
        assert_eq!(format!("{v:?}"), "SignatureVerifier { secrets: 1 }");
    }

    #[test]
    fn rfc4231_vector_matches() {
        // RFC 4231 test case 2: key "Jefe", data "what do ya want for nothing?".
        let s = sign(&AppSecret::new("Jefe"), b"what do ya want for nothing?");
        assert_eq!(
            s,
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
