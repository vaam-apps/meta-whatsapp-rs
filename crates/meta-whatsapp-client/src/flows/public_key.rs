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
    // Exactly 2048 bits (the top bit of the first byte set: a 2047-bit
    // modulus in 256 bytes is another size) and odd (a product of two odd
    // primes).
    (modulus.len() == MODULUS_BYTES
        && modulus.first().is_some_and(|b| b & 0x80 != 0)
        && modulus.last().is_some_and(|b| b & 1 == 1))
    .then_some(())?;
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

    /// DER of an SPKI with the given parts, for the hostile cases below.
    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        match content.len() {
            n if n < 0x80 => out.push(u8::try_from(n).unwrap()),
            n if n < 0x100 => out.extend([0x81, u8::try_from(n).unwrap()]),
            n => out.extend([
                0x82,
                u8::try_from(n >> 8).unwrap(),
                u8::try_from(n & 0xff).unwrap(),
            ]),
        }
        out.extend_from_slice(content);
        out
    }

    struct Parts {
        oid: Vec<u8>,
        params: Vec<u8>,
        unused: u8,
        modulus: Vec<u8>,
        exponent: Vec<u8>,
    }

    impl Parts {
        fn good() -> Self {
            let mut modulus = vec![0x00, 0xc1];
            modulus.extend(std::iter::repeat_n(0x5a, 254));
            modulus.push(0x4b);
            Self {
                oid: RSA_ENCRYPTION_OID.to_vec(),
                params: vec![0x05, 0x00],
                unused: 0,
                modulus,
                exponent: vec![0x01, 0x00, 0x01],
            }
        }

        fn der(&self) -> Vec<u8> {
            let mut algorithm = tlv(0x06, &self.oid);
            algorithm.extend_from_slice(&self.params);
            let mut key = tlv(0x02, &self.modulus);
            key.extend(tlv(0x02, &self.exponent));
            let mut bits = vec![self.unused];
            bits.extend(tlv(0x30, &key));
            let mut spki = tlv(0x30, &algorithm);
            spki.extend(tlv(0x03, &bits));
            tlv(0x30, &spki)
        }

        fn parses(&self) -> bool {
            parse_spki(&self.der()).is_some()
        }
    }

    #[test]
    fn the_hand_built_good_key_parses_so_the_hostile_cases_fail_for_their_own_reason() {
        assert!(Parts::good().parses());
    }

    /// Decisive for the modulus rules: a 2047-bit modulus in 256 bytes (no
    /// leading zero, top bit clear), 2049 bits (a leading zero and a
    /// second one), an even modulus, zero, a negative one, a non-minimal
    /// leading zero.
    #[test]
    fn a_modulus_that_is_not_2048_bits_odd_and_positive_is_refused() {
        let mut p = Parts::good();
        p.modulus = std::iter::once(0x40)
            .chain(std::iter::repeat_n(0x5a, 254))
            .chain([0x4b])
            .collect();
        assert!(!p.parses(), "2047 bits");
        p.modulus = [0x00, 0x00, 0xc1]
            .into_iter()
            .chain(std::iter::repeat_n(0x5a, 254))
            .chain([0x4b])
            .collect();
        assert!(!p.parses(), "a leading zero too many");
        p.modulus = [0x01, 0xc1]
            .into_iter()
            .chain(std::iter::repeat_n(0x5a, 254))
            .chain([0x4b])
            .collect();
        assert!(!p.parses(), "2057 bits");
        p = Parts::good();
        *p.modulus.last_mut().unwrap() = 0x4a;
        assert!(!p.parses(), "even");
        for modulus in [vec![], vec![0x00], vec![0xc1; 256], vec![0xff; 257]] {
            p.modulus = modulus;
            assert!(!p.parses(), "empty, zero or negative");
        }
    }

    #[test]
    fn an_exponent_that_is_not_an_odd_word_of_at_least_three_is_refused() {
        for (exponent, ok) in [
            (vec![0x01, 0x00, 0x01], true),
            (vec![0x03], true),
            (vec![0x00], false),
            (vec![0x01], false),
            (vec![0x02], false),
            (vec![0x01, 0x00, 0x00], false),
            (vec![], false),
            (vec![0x80, 0x01], false),
            (vec![0x00, 0x00, 0x01, 0x01], false),
            (vec![0x7f; 9], false),
        ] {
            let mut p = Parts::good();
            p.exponent = exponent.clone();
            assert_eq!(p.parses(), ok, "{exponent:?}");
        }
    }

    #[test]
    fn the_algorithm_its_parameters_and_the_bit_string_are_exact() {
        let mut p = Parts::good();
        p.params = vec![];
        assert!(!p.parses(), "no NULL");
        p.params = vec![0x05, 0x01, 0x00];
        assert!(!p.parses(), "a non-empty NULL");
        p.params = vec![0x30, 0x00];
        assert!(!p.parses(), "an empty SEQUENCE for parameters");
        p.params = vec![0x05, 0x00, 0x05, 0x00];
        assert!(!p.parses(), "two parameters");
        p = Parts::good();
        p.oid.push(0);
        assert!(!p.parses(), "a longer OID");
        p = Parts::good();
        for unused in [1, 7, 8, 0xff] {
            p.unused = unused;
            assert!(!p.parses(), "{unused} unused bits");
        }
    }

    /// Every truncation and every one-byte corruption of a valid key
    /// either parses to a 2048-bit key or is refused: no panic. A prefix
    /// never parses.
    #[test]
    fn a_truncated_or_corrupted_key_never_panics_and_a_prefix_never_parses() {
        let der = Parts::good().der();
        for n in 0..der.len() {
            assert!(parse_spki(&der[..n]).is_none(), "prefix {n}");
        }
        for i in 0..der.len() {
            for flip in [0x01u8, 0x80, 0xff] {
                let mut bad = der.clone();
                bad[i] ^= flip;
                if let Some((modulus, exponent)) = parse_spki(&bad) {
                    assert_eq!(modulus.len(), 256);
                    assert!(modulus[0] & 0x80 != 0 && modulus[255] & 1 == 1);
                    assert!(exponent.len() <= 8 && exponent[exponent.len() - 1] & 1 == 1);
                }
            }
        }
    }

    /// The length encodings: indefinite, a long form of a short value, a
    /// leading zero, wider than three bytes (and past `usize` on 32-bit),
    /// longer than the data.
    #[test]
    fn hostile_lengths_are_refused() {
        assert!(element(&[0x30, 0x80, 0x00, 0x00]).is_none(), "indefinite");
        assert!(
            element(&[0x30, 0x81, 0x05, 1, 2, 3, 4, 5]).is_none(),
            "long form of 5"
        );
        let padded: Vec<u8> = [0x30, 0x82, 0x00, 0x80]
            .into_iter()
            .chain(std::iter::repeat_n(7, 128))
            .collect();
        assert!(element(&padded).is_none(), "leading zero, data complete");
        assert!(
            element(&[0x30, 0x84, 0xff, 0xff, 0xff, 0xff]).is_none(),
            "4 bytes"
        );
        assert!(element(&[0x30, 0x88, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).is_none());
        assert!(element(&[0x30, 0xff, 0xff]).is_none(), "127 length bytes");
        assert!(
            element(&[0x30, 0x82, 0xff, 0xff, 1]).is_none(),
            "longer than the data"
        );
        assert!(element(&[0x30, 0x81]).is_none(), "cut in the length");
        assert!(element(&[0x30]).is_none());
        assert!(element(&[]).is_none());
        assert_eq!(
            element(
                &[0x04, 0x81, 0x80]
                    .iter()
                    .copied()
                    .chain(std::iter::repeat_n(7, 128))
                    .collect::<Vec<_>>()
            )
            .map(|(t, c, r)| (t, c.len(), r.len())),
            Some((0x04, 128, 0))
        );
    }

    /// A SEQUENCE where the key's SEQUENCE belongs, nested one level too
    /// deep, or the integers outside it.
    #[test]
    fn nesting_confusion_is_refused() {
        let good = Parts::good();
        let mut key = tlv(0x02, &good.modulus);
        key.extend(tlv(0x02, &good.exponent));
        let wrapped = tlv(0x30, &tlv(0x30, &key));
        let mut algorithm = tlv(0x06, &good.oid);
        algorithm.extend_from_slice(&good.params);
        let mut bits = vec![0u8];
        bits.extend(wrapped);
        let mut spki = tlv(0x30, &algorithm);
        spki.extend(tlv(0x03, &bits));
        assert!(parse_spki(&tlv(0x30, &spki)).is_none(), "doubly nested");
        // Trailing bytes inside the key's SEQUENCE, a third integer.
        let mut three = key.clone();
        three.extend(tlv(0x02, &[1]));
        let mut bits = vec![0u8];
        bits.extend(tlv(0x30, &three));
        let mut spki = tlv(0x30, &algorithm);
        spki.extend(tlv(0x03, &bits));
        assert!(parse_spki(&tlv(0x30, &spki)).is_none(), "a third integer");
        // An OCTET STRING in the BIT STRING's place.
        let mut bits = vec![0u8];
        bits.extend(tlv(0x30, &key));
        let mut spki = tlv(0x30, &algorithm);
        spki.extend(tlv(0x04, &bits));
        assert!(parse_spki(&tlv(0x30, &spki)).is_none(), "an OCTET STRING");
    }
}
