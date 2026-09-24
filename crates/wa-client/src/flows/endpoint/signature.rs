//! `X-Hub-Signature-256` on Flow endpoint requests
//! (`implementingyourflowendpoint`, "Request signature validation").
//!
//! Meta signs every endpoint request with HMAC-SHA256 of the raw body under
//! the secret of the app connected to the Flow. Verify **before** decrypting
//! or parsing: the RSA key only proves the payload was encrypted to you, and
//! anyone can do that with your public key.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use wa_core::error::WebhookError;
use wa_core::secret::AppSecret;

/// Name of the header carrying the signature (`sha256=<hex>`), lowercase as
/// HTTP/2 transmits it.
pub const SIGNATURE_HEADER: &str = "x-hub-signature-256";

/// Check `X-Hub-Signature-256` against the **raw** request `body`.
///
/// `app_secrets` may hold several secrets so an app secret can be rotated
/// without downtime, as Meta recommends: accept old and new for a while,
/// then drop the old one. The comparison is constant-time per secret.
/// Answer a failure with
/// [`EndpointStatus::SignatureMismatch`](super::EndpointStatus::SignatureMismatch).
///
/// An empty secret is skipped, never used as an HMAC key: HMAC under the
/// empty key is something anyone can compute, so an app secret read from an
/// unset environment variable would otherwise accept forged requests
/// silently. With only empty secrets every request fails, which is the
/// failure you want to notice.
///
/// # Errors
///
/// [`WebhookError::MissingSignature`] when `signature_header` is `None`,
/// [`WebhookError::MalformedSignature`] when it is not `sha256=` followed by
/// 64 hex digits, [`WebhookError::SignatureMismatch`] when no non-empty
/// secret (or an empty list) produces it.
pub fn verify_request_signature(
    body: &[u8],
    signature_header: Option<&str>,
    app_secrets: &[AppSecret],
) -> Result<(), WebhookError> {
    let header = signature_header.ok_or(WebhookError::MissingSignature)?;
    let hex_digest = header
        .trim()
        .strip_prefix("sha256=")
        .ok_or(WebhookError::MalformedSignature)?;
    let mut expected = [0u8; 32];
    hex::decode_to_slice(hex_digest, &mut expected)
        .map_err(|_| WebhookError::MalformedSignature)?;

    for secret in app_secrets {
        let key = secret.expose_secret().as_bytes();
        if key.is_empty() {
            continue;
        }
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key) else {
            continue;
        };
        mac.update(body);
        if mac.verify_slice(&expected).is_ok() {
            return Ok(());
        }
    }
    Err(WebhookError::SignatureMismatch)
}
