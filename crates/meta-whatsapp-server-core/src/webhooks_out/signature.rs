//! [Standard Webhooks](https://www.standardwebhooks.com/) signatures
//! (design §4.4), the symmetric scheme:
//!
//! - a secret is `whsec_` and the base64 of its key, 24 to 64 bytes
//!   ([`EndpointSecret`]; the service makes 32);
//! - an attempt carries `webhook-id` (the event's id, the same on every
//!   attempt: receivers deduplicate on it), `webhook-timestamp` (the
//!   attempt's Unix second, so receivers refuse a replay over 5 minutes
//!   off) and `webhook-signature`;
//! - a signature is `v1,` and the base64 of HMAC-SHA256 under the key of
//!   `{id}.{timestamp}.{body}`, the body's exact bytes; during a rotation
//!   the header lists one per secret, separated by spaces, and a receiver
//!   accepts the attempt when any one verifies.
//!
//! [`verify`] is a receiver's check, in Rust: what the tests and the
//! guide's receivers run.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, Mac};
use meta_whatsapp_rs::core::secret::SecretBytes;
use sha2::Sha256;

/// `webhook-id`.
pub const WEBHOOK_ID: &str = "webhook-id";
/// `webhook-timestamp`.
pub const WEBHOOK_TIMESTAMP: &str = "webhook-timestamp";
/// `webhook-signature`.
pub const WEBHOOK_SIGNATURE: &str = "webhook-signature";

/// The secrets' prefix.
pub const SECRET_PREFIX: &str = "whsec_";

/// The bytes of a secret the service makes.
pub const SECRET_BYTES: usize = 32;

/// The shortest and longest keys a secret may have (the spec's 24 to 64
/// bytes).
const KEY_BYTES: std::ops::RangeInclusive<usize> = 24..=64;

/// How far a receiver's clock and an attempt's timestamp may differ
/// ([`verify`]: the spec's, and the libraries', 5 minutes).
pub const TOLERANCE_SECS: i64 = 5 * 60;

/// A signing secret: its key, zeroized on drop. `Debug` shows nothing;
/// [`Self::expose`] is its `whsec_…` form, for the one answer that shows
/// it (a creation, a rotation).
#[derive(Clone)]
pub struct EndpointSecret(SecretBytes);

impl fmt::Debug for EndpointSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EndpointSecret([REDACTED])")
    }
}

impl EndpointSecret {
    /// A new random secret of [`SECRET_BYTES`]; `None` without
    /// randomness.
    pub fn generate() -> Option<Self> {
        let mut key = vec![0u8; SECRET_BYTES];
        getrandom::fill(&mut key).ok()?;
        Some(Self(SecretBytes::new(key)))
    }

    /// The secret written `whsec_<base64>` (or its base64 alone, as the
    /// libraries take it): `None` unless its key is 24 to 64 bytes.
    pub fn parse(text: &str) -> Option<Self> {
        let encoded = text.strip_prefix(SECRET_PREFIX).unwrap_or(text);
        let key = B64.decode(encoded).ok()?;
        KEY_BYTES
            .contains(&key.len())
            .then(|| Self(SecretBytes::new(key)))
    }

    /// The key's bytes.
    pub fn key(&self) -> &[u8] {
        self.0.expose_secret()
    }

    /// `whsec_<base64>`: shown once, to the endpoint's owner.
    pub fn expose(&self) -> String {
        format!("{SECRET_PREFIX}{}", B64.encode(self.key()))
    }

    /// `v1,<base64 HMAC-SHA256(key, "{id}.{timestamp}.{body}")>`.
    pub fn sign(&self, id: &str, timestamp: i64, body: &[u8]) -> String {
        format!("v1,{}", B64.encode(self.mac(id, timestamp, body)))
    }

    fn mac(&self, id: &str, timestamp: i64, body: &[u8]) -> Vec<u8> {
        // HMAC takes a key of any length.
        let Ok(mut mac) = <Hmac<Sha256> as Mac>::new_from_slice(self.key()) else {
            return Vec::new();
        };
        mac.update(id.as_bytes());
        mac.update(b".");
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        mac.finalize().into_bytes().to_vec()
    }
}

/// The `webhook-signature` of an attempt signed by each of `secrets`, in
/// order, separated by spaces.
pub fn signature_header(
    secrets: &[EndpointSecret],
    id: &str,
    timestamp: i64,
    body: &[u8],
) -> String {
    secrets
        .iter()
        .map(|secret| secret.sign(id, timestamp, body))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Why [`verify`] refused an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The timestamp is not an integer.
    #[error("the timestamp is not a number")]
    BadTimestamp,
    /// The timestamp is over [`TOLERANCE_SECS`] from now.
    #[error("the timestamp is too far from now")]
    Stale,
    /// No `v1` signature of the header matches.
    #[error("no signature matches")]
    NoMatch,
}

/// A receiver's check of an attempt: `timestamp` within
/// [`TOLERANCE_SECS`] of `now` (a Unix second), and one of the header's
/// `v1` signatures (`signatures`, space-separated) that of `secret` over
/// `id`, `timestamp` and `body`, compared in constant time. Entries of
/// another scheme (`v1a,…`) or malformed ones are skipped, as the
/// libraries do.
///
/// # Errors
///
/// [`VerifyError`].
pub fn verify(
    secret: &EndpointSecret,
    id: &str,
    timestamp: &str,
    signatures: &str,
    body: &[u8],
    now: i64,
) -> Result<(), VerifyError> {
    use subtle::ConstantTimeEq as _;
    let at: i64 = timestamp.parse().map_err(|_| VerifyError::BadTimestamp)?;
    if (now - at).abs() > TOLERANCE_SECS {
        return Err(VerifyError::Stale);
    }
    let expected = secret.mac(id, at, body);
    let matches = signatures
        .split(' ')
        .filter_map(|entry| entry.strip_prefix("v1,"))
        .filter_map(|encoded| B64.decode(encoded).ok())
        .any(|candidate| bool::from(candidate.ct_eq(&expected)));
    if matches {
        Ok(())
    } else {
        Err(VerifyError::NoMatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Standard Webhooks libraries' own known-answer vectors
    /// (`libraries/python/tests/test_webhooks.py` of
    /// `standard-webhooks/standard-webhooks`: `test_sign_function`, and
    /// `test_sign_function_binary_payload`, whose expected value is the Go
    /// library's): a secret, a message id, a timestamp and a body, and the
    /// signature each library computes. Decisive: the `{id}.{ts}.{body}`
    /// layout, the key read from the base64 after `whsec_`, the `v1,`
    /// prefix and standard base64.
    #[test]
    fn the_standard_webhooks_test_vectors() {
        let secret = EndpointSecret::parse("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap();
        assert_eq!(
            secret.sign(
                "msg_p5jXN8AQM9LWM0D4loKWxJek",
                1_614_265_330,
                br#"{"test": 2432232314}"#
            ),
            "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE="
        );
        let binary: Vec<u8> = (0..=255).collect();
        assert_eq!(
            secret.sign("msg_p5jXN8AQM9LWM0D4loKWxJek", 1_614_265_330, &binary),
            "v1,sEcZ042G8Crqgxbybb3y0eDxQR3TcfYsk11I1XULhSI="
        );
        // The libraries take the secret without its prefix too.
        let bare = EndpointSecret::parse("MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap();
        assert_eq!(bare.key(), secret.key());
        assert_eq!(secret.expose(), "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw");
    }

    /// A receiver's check, as the libraries' tests make it: the vector
    /// verifies within the tolerance; a changed byte of the signature
    /// (the libraries' `…1OA=`), of the body or of the id, a timestamp
    /// 5 minutes and a second off either way, and a header of other
    /// schemes or malformed entries do not; one valid signature among
    /// several does (a rotation's header, either order).
    #[test]
    fn a_receiver_verifies_as_the_libraries_do() {
        let secret = EndpointSecret::parse("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap();
        let id = "msg_p5jXN8AQM9LWM0D4loKWxJek";
        let body = br#"{"test": 2432232314}"#;
        let ts = 1_614_265_330;
        let good = "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=";
        let check = |sig: &str, body: &[u8], id: &str, now: i64| {
            verify(&secret, id, &ts.to_string(), sig, body, now)
        };
        assert_eq!(check(good, body, id, ts), Ok(()));
        assert_eq!(check(good, body, id, ts + TOLERANCE_SECS), Ok(()));
        assert_eq!(
            check(
                "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OA=",
                body,
                id,
                ts
            ),
            Err(VerifyError::NoMatch)
        );
        assert_eq!(
            check(good, br#"{"test": 2432232315}"#, id, ts),
            Err(VerifyError::NoMatch)
        );
        assert_eq!(
            check(good, body, "msg_other", ts),
            Err(VerifyError::NoMatch)
        );
        assert_eq!(
            check(good, body, id, ts + TOLERANCE_SECS + 1),
            Err(VerifyError::Stale)
        );
        assert_eq!(
            check(good, body, id, ts - TOLERANCE_SECS - 1),
            Err(VerifyError::Stale)
        );
        let other = EndpointSecret::generate().unwrap();
        let theirs = other.sign(id, ts, body);
        for header in [format!("{theirs} {good}"), format!("{good} {theirs}")] {
            assert_eq!(check(&header, body, id, ts), Ok(()), "{header}");
        }
        for header in [
            "v1a,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=",
            "v1,not-base64! v1,a,b",
            "",
        ] {
            assert_eq!(
                check(header, body, id, ts),
                Err(VerifyError::NoMatch),
                "{header}"
            );
        }
        assert_eq!(
            verify(&secret, id, "x", good, body, ts),
            Err(VerifyError::BadTimestamp)
        );
    }

    /// Two secrets sign one attempt in order, space-separated: what a
    /// rotation's 24 hours send, each verifying with its own secret.
    #[test]
    fn a_rotation_sends_both_signatures() {
        let new = EndpointSecret::generate().unwrap();
        let old = EndpointSecret::generate().unwrap();
        let header = signature_header(&[new.clone(), old.clone()], "evt_1", 1_700_000_000, b"{}");
        let parts: Vec<&str> = header.split(' ').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], new.sign("evt_1", 1_700_000_000, b"{}"));
        assert_eq!(parts[1], old.sign("evt_1", 1_700_000_000, b"{}"));
        for secret in [&new, &old] {
            assert_eq!(
                verify(secret, "evt_1", "1700000000", &header, b"{}", 1_700_000_000),
                Ok(())
            );
        }
    }

    /// A made secret is `whsec_` and the base64 of 32 bytes, read back to
    /// the same key; one of 23 or 65 bytes, or not base64, is no secret.
    /// Its `Debug` shows nothing of it.
    #[test]
    fn secrets_are_whsec_and_32_random_bytes() {
        let secret = EndpointSecret::generate().unwrap();
        let shown = secret.expose();
        assert!(shown.starts_with("whsec_"));
        assert_eq!(EndpointSecret::parse(&shown).unwrap().key(), secret.key());
        assert_eq!(secret.key().len(), 32);
        assert_ne!(EndpointSecret::generate().unwrap().key(), secret.key());
        assert!(!format!("{secret:?}").contains(&shown[6..]));
        for bad in [
            format!("whsec_{}", B64.encode([7u8; 23])),
            format!("whsec_{}", B64.encode([7u8; 65])),
            "whsec_!!".to_owned(),
        ] {
            assert!(EndpointSecret::parse(&bad).is_none(), "{bad}");
        }
        assert!(EndpointSecret::parse(&format!("whsec_{}", B64.encode([7u8; 24]))).is_some());
        assert!(EndpointSecret::parse(&format!("whsec_{}", B64.encode([7u8; 64]))).is_some());
    }
}
