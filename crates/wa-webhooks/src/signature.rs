//! `X-Hub-Signature-256` verification (`webhooks/create-webhook-endpoint`).
//!
//! Meta signs the **raw** POST body with HMAC-SHA256 under the app secret
//! and sends `sha256=<64 hex chars>`. Verify the bytes exactly as received,
//! before any parsing: re-serialized JSON would not match, and parsing
//! unauthenticated input is attack surface we do not need.
//!
//! Several secrets can be configured so an app secret can be rotated
//! without dropping deliveries signed with the old one.
//!
//! An empty key is the one thing this module must never accept: an
//! `AppSecret` read from an unset environment variable is `""`, and an
//! HMAC under the empty key is something anyone can compute, so a verifier
//! holding it would accept forged deliveries. [`SignatureVerifier::new`]
//! rejects blank secrets, and [`SignatureVerifier::verify`] independently
//! refuses to match under one (defence in depth: the check that matters
//! must not depend on every construction path remembering it).

use std::fmt;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::{Choice, ConstantTimeEq};
use wa_core::error::{ConfigError, WebhookError};
use wa_core::secret::AppSecret;

type HmacSha256 = Hmac<Sha256>;

/// The header Meta signs deliveries with, lower-case (`http` stores header
/// names lower-case). Pass its value to [`SignatureVerifier::verify`] or
/// `WebhookHandler::deliver`, whatever the HTTP framework.
pub const SIGNATURE_HEADER: &str = "x-hub-signature-256";

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
    /// Build a verifier. Needs at least one secret, and no blank one (empty
    /// or whitespace only, the usual result of an unset or mistyped
    /// environment variable): an HMAC under a key anyone can guess makes
    /// every signature forgeable.
    pub fn new(secrets: Vec<AppSecret>) -> wa_core::Result<Self> {
        if secrets.is_empty() {
            return Err(ConfigError::new("SignatureVerifier needs at least one app secret").into());
        }
        if secrets.iter().any(is_blank) {
            return Err(ConfigError::new("SignatureVerifier: app secret must not be blank").into());
        }
        Ok(Self { secrets })
    }

    /// Check `header` (the `X-Hub-Signature-256` value) against `body`.
    ///
    /// Accepts upper- or lower-case hex and surrounding whitespace. Every
    /// configured secret is tried and compared in constant time, so the
    /// timing reveals neither how much of the signature matched nor which
    /// secret did. A blank secret never matches.
    pub fn verify(&self, header: Option<&str>, body: &[u8]) -> Result<(), WebhookError> {
        let expected = parse_header(header)?;
        let matched = self.secrets.iter().fold(Choice::from(0), |acc, secret| {
            // `None` (blank secret) contributes "no match", never a key.
            acc | tag(secret, body).map_or(Choice::from(0), |tag| tag.as_slice().ct_eq(&expected))
        });
        if bool::from(matched) {
            Ok(())
        } else {
            Err(WebhookError::SignatureMismatch)
        }
    }
}

/// The 32-byte tag an `X-Hub-Signature-256` value carries: `sha256=` and
/// 64 hex characters (either case, surrounding whitespace ignored).
///
/// Needs neither a secret nor the body, so an HTTP layer can refuse a
/// missing or malformed header before it reads (and buffers) the body.
pub(crate) fn parse_header(header: Option<&str>) -> Result<[u8; 32], WebhookError> {
    let header = header.ok_or(WebhookError::MissingSignature)?;
    let hex = header
        .trim()
        .strip_prefix(PREFIX)
        .ok_or(WebhookError::MalformedSignature)?;
    let mut tag = [0u8; 32];
    if hex.len() != 64 || hex::decode_to_slice(hex, &mut tag).is_err() {
        return Err(WebhookError::MalformedSignature);
    }
    Ok(tag)
}

/// `sha256=<lowercase hex>` of `body` under `secret`: the header value Meta
/// would send. For tests, and for re-signing a body you forward.
///
/// Signing is not where a blank secret hurts (verifying under one is), so
/// this signs under whatever it is given; [`SignatureVerifier`] is what
/// refuses blank keys.
pub fn sign(secret: &AppSecret, body: &[u8]) -> String {
    // The error arm is unreachable (see `hmac_sha256`); if it were not, an
    // empty hex part is a header every verifier rejects as malformed.
    let hex = hmac_sha256(secret, body)
        .map(hex::encode)
        .unwrap_or_default();
    format!("{PREFIX}{hex}")
}

fn is_blank(secret: &AppSecret) -> bool {
    secret.expose_secret().trim().is_empty()
}

/// The tag to compare against, or `None` when there is no usable key; a
/// `None` never matches.
fn tag(secret: &AppSecret, body: &[u8]) -> Option<[u8; 32]> {
    if is_blank(secret) {
        return None;
    }
    hmac_sha256(secret, body)
}

/// HMAC accepts keys of any length, so `new_from_slice` cannot fail here;
/// it is still handled as "no tag" rather than with a fallback key. The
/// draft of this function fell back to an all-zero key, which an attacker
/// can sign with as easily as we can.
fn hmac_sha256(secret: &AppSecret, body: &[u8]) -> Option<[u8; 32]> {
    let mut mac = HmacSha256::new_from_slice(secret.expose_secret().as_bytes()).ok()?;
    mac.update(body);
    Some(mac.finalize().into_bytes().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wa_core::Error;

    #[test]
    fn empty_secret_list_and_blank_secrets_are_config_errors() {
        assert!(matches!(
            SignatureVerifier::new(vec![]),
            Err(Error::Config(_))
        ));
        for blank in ["", " ", "\n", "\t \r\n"] {
            assert!(
                matches!(
                    SignatureVerifier::new(vec![AppSecret::new("good"), AppSecret::new(blank)]),
                    Err(Error::Config(_))
                ),
                "{blank:?} accepted"
            );
        }
    }

    /// The forgery the constructor check exists to stop: whoever knows the
    /// key is empty signs with it. Built without `new` on purpose, so this
    /// fails if the verify-time guard is removed even though `new` would
    /// have refused the secret.
    #[test]
    fn a_blank_secret_never_verifies_even_if_it_got_in() {
        let body = br#"{"object":"whatsapp_business_account","entry":[]}"#;
        let mut mac = HmacSha256::new_from_slice(b"").unwrap();
        mac.update(body);
        let forged = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        for blank in ["", "  "] {
            let v = SignatureVerifier {
                secrets: vec![AppSecret::new(blank)],
            };
            let mut mac = HmacSha256::new_from_slice(blank.as_bytes()).unwrap();
            mac.update(body);
            let forged_blank = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
            for header in [&forged, &forged_blank] {
                assert!(
                    matches!(
                        v.verify(Some(header), body),
                        Err(WebhookError::SignatureMismatch)
                    ),
                    "{blank:?}"
                );
            }
        }
        // And a real secret next to a blank one still works for real
        // signatures only.
        let v = SignatureVerifier {
            secrets: vec![AppSecret::new(""), AppSecret::new("real")],
        };
        assert!(
            v.verify(Some(&sign(&AppSecret::new("real"), body)), body)
                .is_ok()
        );
        assert!(v.verify(Some(&forged), body).is_err());
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
