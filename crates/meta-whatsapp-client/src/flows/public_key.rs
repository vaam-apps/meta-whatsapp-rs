//! A business public key, checked before it goes anywhere: a 2048-bit RSA
//! public key in PEM (`flows/guides/whatsapp-business-encryption`: "a
//! 2048-bit RSA public key"; the key `openssl rsa -pubout` writes, a
//! `SubjectPublicKeyInfo`, label `PUBLIC KEY`).
//!
//! [`BusinessPublicKey`](super::BusinessPublicKey) is Meta's answer;
//! [`Rsa2048PublicKey`] is the key before it is sent. Parsing needs no
//! crypto library (a small DER reader), so it is there without the
//! `flows-endpoint` feature. It lets a caller refuse a wrong key locally,
//! and compare the key it holds with the private key it serves
//! (`FlowEndpointKey::public_key_pem` parses to an equal value).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use meta_whatsapp_core::error::ValidationError;
use sha2::{Digest, Sha256};

/// Largest PEM text [`Rsa2048PublicKey::from_pem`] reads: a 2048-bit key
/// is about 450 bytes of PEM.
pub const MAX_PUBLIC_KEY_PEM_BYTES: usize = 4096;

const FIELD: &str = "business_public_key";
/// `rsaEncryption`, 1.2.840.113549.1.1.1.
const RSA_ENCRYPTION_OID: [u8; 9] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const MODULUS_BYTES: usize = 256;

/// A 2048-bit RSA public key, parsed. Two values are equal when their
/// modulus and exponent are. Public data: `Debug` shows the key's SHA-256
/// only, to keep logs short.
#[derive(Clone, PartialEq, Eq)]
pub struct Rsa2048PublicKey {
    modulus: Vec<u8>,
    exponent: Vec<u8>,
    der: Vec<u8>,
}

impl std::fmt::Debug for Rsa2048PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rsa2048PublicKey")
            .field("sha256", &self.sha256())
            .finish()
    }
}

fn invalid(message: &str) -> ValidationError {
    ValidationError::new(FIELD, message.to_owned())
}

/// `(tag, content, rest)` of the DER element at the start of `input`.
fn element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        // Definite lengths of up to 3 bytes: a key is under 64 KiB.
        if count == 0 || count > 3 || rest.len() < count {
            return None;
        }
        let (digits, rest) = rest.split_at(count);
        let mut len = 0usize;
        for &d in digits {
            len = (len << 8) | usize::from(d);
        }
        // Not minimal: a long form for a value the short form holds.
        if len < 0x80 || digits.first() == Some(&0) {
            return None;
        }
        (len, rest)
    };
    let content = rest.get(..len)?;
    Some((tag, content, rest.get(len..)?))
}

/// The content of the `tag` element at the start of `input`, and the rest.
fn expect(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (found, content, rest) = element(input)?;
    (found == tag).then_some((content, rest))
}

/// A positive INTEGER's magnitude, without its sign byte.
fn magnitude(content: &[u8]) -> Option<&[u8]> {
    match content {
        // A leading zero is there when, and only when, the next byte's high
        // bit is set (a minimal positive INTEGER).
        [0, rest @ ..] => rest.first().is_some_and(|b| b & 0x80 != 0).then_some(rest),
        [first, ..] if first & 0x80 == 0 => Some(content),
        // Empty, or negative.
        _ => None,
    }
}

fn parse_spki(der: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let (spki, rest) = expect(der, 0x30)?;
    rest.is_empty().then_some(())?;
    let (algorithm, rest) = expect(spki, 0x30)?;
    let (oid, params) = expect(algorithm, 0x06)?;
    (oid == RSA_ENCRYPTION_OID).then_some(())?;
    (params == [0x05, 0x00]).then_some(())?;
    let (bits, rest) = expect(rest, 0x03)?;
    rest.is_empty().then_some(())?;
    let (&unused, key) = bits.split_first()?;
    (unused == 0).then_some(())?;
    let (sequence, rest) = expect(key, 0x30)?;
    rest.is_empty().then_some(())?;
    let (modulus, rest) = expect(sequence, 0x02)?;
    let (exponent, rest) = expect(rest, 0x02)?;
    rest.is_empty().then_some(())?;
    let modulus = magnitude(modulus)?;
    let exponent = magnitude(exponent)?;
    (modulus.len() == MODULUS_BYTES).then_some(())?;
    // A public exponent is odd, at least 3, and fits a word.
    (exponent.len() <= 8 && exponent.last().is_some_and(|b| b & 1 == 1) && exponent != [1])
        .then_some(())?;
    Some((modulus.to_vec(), exponent.to_vec()))
}

impl Rsa2048PublicKey {
    /// Parse `pem`: one `-----BEGIN PUBLIC KEY-----` block holding a
    /// 2048-bit RSA `SubjectPublicKeyInfo`, at most
    /// [`MAX_PUBLIC_KEY_PEM_BYTES`], nothing else in the text.
    ///
    /// # Errors
    ///
    /// A [`ValidationError`] on `business_public_key`, never quoting the
    /// text: a private key, a PKCS#1 `RSA PUBLIC KEY`, another algorithm
    /// or key size, a malformed block.
    pub fn from_pem(pem: &str) -> Result<Self, ValidationError> {
        if pem.len() > MAX_PUBLIC_KEY_PEM_BYTES {
            return Err(invalid("is too long for a public key"));
        }
        let text = pem.trim();
        if text.contains("PRIVATE KEY") {
            return Err(invalid(
                "is a private key; give only the public key (`openssl rsa -in private.pem -pubout`)",
            ));
        }
        let body = text
            .strip_prefix("-----BEGIN PUBLIC KEY-----")
            .and_then(|rest| rest.strip_suffix("-----END PUBLIC KEY-----"))
            .ok_or_else(|| invalid("must be one PEM block, -----BEGIN PUBLIC KEY-----"))?;
        if body.contains("-----") || body.contains(':') {
            return Err(invalid("must be one PEM block without headers"));
        }
        let encoded: Vec<u8> = body.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        let der = STANDARD
            .decode(&encoded)
            .map_err(|_| invalid("is not valid base64"))?;
        let (modulus, exponent) = parse_spki(&der)
            .ok_or_else(|| invalid("must be a 2048-bit RSA public key (SubjectPublicKeyInfo)"))?;
        Ok(Self {
            modulus,
            exponent,
            der,
        })
    }

    /// The key as PEM, in the form `FlowEndpointKey::public_key_pem`
    /// writes (64-character lines, a final newline): equal keys give equal
    /// text, whatever the line length they were read from.
    #[must_use]
    pub fn to_pem(&self) -> String {
        let b64 = STANDARD.encode(&self.der);
        let mut out = String::from("-----BEGIN PUBLIC KEY-----\n");
        for line in b64.as_bytes().chunks(64) {
            out.push_str(&String::from_utf8_lossy(line));
            out.push('\n');
        }
        out.push_str("-----END PUBLIC KEY-----\n");
        out
    }

    /// The SHA-256 of [`Self::to_pem`], lowercase hex: a short name for the
    /// key.
    #[must_use]
    pub fn sha256(&self) -> String {
        hex::encode(Sha256::digest(self.to_pem().as_bytes()))
    }

    /// The big-endian modulus, 256 bytes.
    #[must_use]
    pub fn modulus(&self) -> &[u8] {
        &self.modulus
    }

    /// The big-endian public exponent.
    #[must_use]
    pub fn exponent(&self) -> &[u8] {
        &self.exponent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEM: &str = include_str!("testdata/rsa2048_public.pem");

    fn body() -> String {
        PEM.lines().filter(|l| !l.starts_with("-----")).collect()
    }

    #[test]
    fn a_2048_bit_key_parses_and_round_trips_in_canonical_form() {
        let key = Rsa2048PublicKey::from_pem(PEM).unwrap();
        assert_eq!(key.modulus().len(), 256);
        assert_eq!(key.exponent(), [1, 0, 1]);
        assert_eq!(key.to_pem().trim_end(), PEM.trim_end());
        assert!(key.to_pem().ends_with("-----END PUBLIC KEY-----\n"));
        // Another line length and surrounding whitespace: the same key.
        let rewrapped = format!(
            "\n  -----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----  \n\n",
            body()
        );
        assert_eq!(Rsa2048PublicKey::from_pem(&rewrapped).unwrap(), key);
        assert_eq!(key.sha256().len(), 64);
        assert!(!format!("{key:?}").contains("MIIBIjAN"));
    }

    #[test]
    fn what_is_not_a_2048_bit_rsa_public_key_is_refused() {
        let body = body();
        let der = STANDARD.decode(&body).unwrap();
        let pem = |label: &str, der: &[u8]| {
            format!(
                "-----BEGIN {label}-----\n{}\n-----END {label}-----",
                STANDARD.encode(der)
            )
        };
        let mut wrong_oid = der.clone();
        wrong_oid[16] ^= 1;
        let mut trailing = der.clone();
        trailing.push(0);
        let cases: Vec<(&str, String)> = vec![
            ("empty", String::new()),
            ("private", pem("PRIVATE KEY", &der)),
            ("pkcs1 label", pem("RSA PUBLIC KEY", &der)),
            ("certificate label", pem("CERTIFICATE", &der)),
            ("truncated", pem("PUBLIC KEY", &der[..der.len() - 10])),
            ("another oid", pem("PUBLIC KEY", &wrong_oid)),
            ("trailing byte", pem("PUBLIC KEY", &trailing)),
            (
                "not base64",
                "-----BEGIN PUBLIC KEY-----\n!!!!\n-----END PUBLIC KEY-----".into(),
            ),
            (
                "a header",
                format!(
                    "-----BEGIN PUBLIC KEY-----\nProc-Type: 4,ENCRYPTED\n{body}\n-----END PUBLIC KEY-----"
                ),
            ),
            ("two blocks", format!("{PEM}{PEM}")),
            (
                "a key and a private key",
                format!("{PEM}\n-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----"),
            ),
            ("too long", "A".repeat(MAX_PUBLIC_KEY_PEM_BYTES + 1)),
            (
                "placeholder",
                "-----BEGIN PUBLIC KEY-----\nAAA\nBBB\n-----END PUBLIC KEY-----".into(),
            ),
        ];
        for (name, text) in cases {
            let err = Rsa2048PublicKey::from_pem(&text).unwrap_err();
            assert_eq!(err.field, FIELD, "{name}");
            assert!(
                !err.to_string().contains(&body[..20]),
                "{name}: quotes the key"
            );
        }
    }

    #[test]
    fn a_key_of_another_size_is_refused() {
        // A 1024-bit key's shape: a modulus of 128 bytes.
        let mut der = vec![0x30, 0x81, 0x9f, 0x30, 0x0d, 0x06, 0x09];
        der.extend_from_slice(&RSA_ENCRYPTION_OID);
        der.extend_from_slice(&[
            0x05, 0x00, 0x03, 0x81, 0x8d, 0x00, 0x30, 0x81, 0x89, 0x02, 0x81, 0x81, 0x00,
        ]);
        der.extend(std::iter::repeat_n(0xab, 128));
        der.extend_from_slice(&[0x02, 0x03, 0x01, 0x00, 0x01]);
        let pem = format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
            STANDARD.encode(&der)
        );
        assert!(Rsa2048PublicKey::from_pem(&pem).is_err());
    }
}
