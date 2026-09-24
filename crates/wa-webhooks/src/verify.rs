//! Subscription verification: the `GET` Meta sends when you set or change
//! the callback URL or verify token (`webhooks/create-webhook-endpoint`).
//!
//! ```text
//! GET <callback>?hub.mode=subscribe&hub.challenge=1158201444&hub.verify_token=<yours>
//! ```
//!
//! Answer `200` with the challenge as the body when the token matches; any
//! other answer leaves the endpoint unverified.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use wa_core::error::WebhookError;
use wa_core::secret::VerifyToken;

/// The query string of a verification request. Every parameter is optional
/// here so that a malformed request is rejected by
/// [`verify_subscription`] (→ `403`) rather than by the query extractor.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VerificationQuery {
    /// `hub.mode`; must be `subscribe`.
    #[serde(rename = "hub.mode", default)]
    pub mode: Option<String>,
    /// `hub.verify_token`; compared in constant time, never logged.
    #[serde(rename = "hub.verify_token", default)]
    pub verify_token: Option<VerifyToken>,
    /// `hub.challenge`; echoed back on success.
    #[serde(rename = "hub.challenge", default)]
    pub challenge: Option<String>,
}

/// Check a verification request and return the challenge to echo.
///
/// Fails with [`WebhookError::InvalidVerificationRequest`] when `hub.mode`
/// is not `subscribe` or a parameter is missing, and with
/// [`WebhookError::VerifyTokenMismatch`] when the token differs.
pub fn verify_subscription(
    query: &VerificationQuery,
    expected: &VerifyToken,
) -> wa_core::Result<String> {
    if query.mode.as_deref() != Some("subscribe") {
        return Err(
            WebhookError::InvalidVerificationRequest("hub.mode must be `subscribe`").into(),
        );
    }
    let Some(token) = &query.verify_token else {
        return Err(WebhookError::InvalidVerificationRequest("missing hub.verify_token").into());
    };
    let Some(challenge) = &query.challenge else {
        return Err(WebhookError::InvalidVerificationRequest("missing hub.challenge").into());
    };
    if !tokens_match(token, expected) {
        return Err(WebhookError::VerifyTokenMismatch.into());
    }
    Ok(challenge.clone())
}

/// Compare digests rather than the tokens themselves: `ct_eq` on slices of
/// different lengths returns early, which would leak the token's length.
fn tokens_match(given: &VerifyToken, expected: &VerifyToken) -> bool {
    let a = Sha256::digest(given.expose_secret().as_bytes());
    let b = Sha256::digest(expected.expose_secret().as_bytes());
    a.as_slice().ct_eq(b.as_slice()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wa_core::Error;

    fn query(
        mode: Option<&str>,
        token: Option<&str>,
        challenge: Option<&str>,
    ) -> VerificationQuery {
        VerificationQuery {
            mode: mode.map(str::to_owned),
            verify_token: token.map(VerifyToken::new),
            challenge: challenge.map(str::to_owned),
        }
    }

    #[test]
    fn debug_never_shows_the_token() {
        let q = query(Some("subscribe"), Some("vibecoding"), Some("1"));
        assert!(!format!("{q:?}").contains("vibecoding"));
    }

    #[test]
    fn parameters_are_checked_before_the_token() {
        let token = VerifyToken::new("t");
        for q in [
            query(None, Some("t"), Some("1")),
            query(Some("unsubscribe"), Some("t"), Some("1")),
            query(Some("subscribe"), None, Some("1")),
            query(Some("subscribe"), Some("t"), None),
        ] {
            assert!(matches!(
                verify_subscription(&q, &token),
                Err(Error::Webhook(WebhookError::InvalidVerificationRequest(_)))
            ));
        }
    }

    #[test]
    fn token_prefix_does_not_match() {
        let q = query(Some("subscribe"), Some("vibe"), Some("1"));
        assert!(matches!(
            verify_subscription(&q, &VerifyToken::new("vibecoding")),
            Err(Error::Webhook(WebhookError::VerifyTokenMismatch))
        ));
    }
}
