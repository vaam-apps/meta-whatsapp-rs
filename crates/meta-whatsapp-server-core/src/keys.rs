//! API keys: `wak_<key id>_<secret>` (docs/design/server.md, section 3.2).
//!
//! - The key id is 12 random bytes and the secret 32, each written in
//!   base62 at a fixed length (17 and 43 characters), so a key is always
//!   `wak_` + 17 + `_` + 43 characters. The `wak_` prefix lets secret
//!   scanners find leaked keys.
//! - Only the key id and the SHA-256 digest of the secret are stored. A
//!   random 256-bit secret needs no slow hash: nobody can guess it, and a
//!   digest is enough to keep a database dump from being a key list.
//! - A presented key is compared with the stored digest in constant time
//!   ([`matches`](fn@matches)).
//! - The key is shown once, when minted; [`MintedKey`] has no `Debug`
//!   that shows it.

use std::fmt;

use meta_whatsapp_rs::core::error::CryptoError;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Every key starts with this.
pub const PREFIX: &str = "wak_";

/// Random bytes in a key id.
const KEY_ID_BYTES: usize = 12;
/// Random bytes in a secret.
const SECRET_BYTES: usize = 32;
/// Base62 characters of a key id: ⌈96 / log2(62)⌉.
pub const KEY_ID_CHARS: usize = 17;
/// Base62 characters of a secret: ⌈256 / log2(62)⌉.
pub const SECRET_CHARS: usize = 43;

const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// `bytes` as a big-endian number in base62, left-padded with `0` to
/// `width` characters.
fn base62(bytes: &[u8], width: usize) -> String {
    let mut number = bytes.to_vec();
    let mut digits = Vec::with_capacity(width);
    while number.iter().any(|b| *b != 0) {
        let mut remainder = 0u32;
        for byte in &mut number {
            let value = (remainder << 8) | u32::from(*byte);
            // value < 62 * 256: the quotient fits in a byte.
            *byte = u8::try_from(value / 62).unwrap_or(u8::MAX);
            remainder = value % 62;
        }
        digits.push(ALPHABET[remainder as usize]);
    }
    while digits.len() < width {
        digits.push(b'0');
    }
    digits.reverse();
    digits.into_iter().map(char::from).collect()
}

/// A freshly minted key: the whole key, shown to the caller once, and what
/// is stored. `Debug` shows the key id only.
pub struct MintedKey {
    key_id: String,
    key: String,
    digest: [u8; 32],
}

impl fmt::Debug for MintedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MintedKey")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

impl MintedKey {
    /// Draw a new key from the operating system's random number generator;
    /// [`CryptoError::Rng`] when it fails.
    pub fn generate() -> Result<Self, CryptoError> {
        let mut id = [0u8; KEY_ID_BYTES];
        let mut secret = [0u8; SECRET_BYTES];
        getrandom::fill(&mut id).map_err(|_| CryptoError::Rng)?;
        getrandom::fill(&mut secret).map_err(|_| CryptoError::Rng)?;
        let key_id = base62(&id, KEY_ID_CHARS);
        let secret = base62(&secret, SECRET_CHARS);
        let digest = digest(&secret);
        let key = format!("{PREFIX}{key_id}_{secret}");
        Ok(Self {
            key_id,
            key,
            digest,
        })
    }

    /// The public key id.
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// The whole key, to show once.
    pub fn expose_key(&self) -> &str {
        &self.key
    }

    /// SHA-256 of the secret, to store.
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

/// A key as presented in `Authorization: Bearer …`, split into its parts.
/// `Debug` shows the key id only.
pub struct PresentedKey<'a> {
    key_id: &'a str,
    secret: &'a str,
}

impl fmt::Debug for PresentedKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresentedKey")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

impl<'a> PresentedKey<'a> {
    /// Split `key`; `None` unless it is exactly `wak_` + 17 base62 + `_` +
    /// 43 base62 characters.
    pub fn parse(key: &'a str) -> Option<Self> {
        let rest = key.strip_prefix(PREFIX)?;
        let (key_id, secret) = rest.split_once('_')?;
        let base62 =
            |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_alphanumeric());
        (base62(key_id, KEY_ID_CHARS) && base62(secret, SECRET_CHARS))
            .then_some(Self { key_id, secret })
    }

    /// The key id, to look the stored digest up by.
    pub fn key_id(&self) -> &'a str {
        self.key_id
    }

    /// Whether the secret's digest equals `stored`, compared in constant
    /// time.
    pub fn matches(&self, stored: &[u8; 32]) -> bool {
        matches(stored, self.secret)
    }
}

/// SHA-256 of a secret.
pub fn digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

/// Whether `secret`'s digest equals `stored`, in constant time: both are 32
/// bytes, so the comparison takes as long whatever they hold.
pub fn matches(stored: &[u8; 32], secret: &str) -> bool {
    bool::from(digest(secret).ct_eq(stored))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base62_is_fixed_width_big_endian() {
        assert_eq!(base62(&[0; 4], 3), "000");
        assert_eq!(base62(&[0, 61], 2), "0z");
        assert_eq!(base62(&[0, 62], 2), "10");
        assert_eq!(base62(&[1, 0], 3), "048"); // 256 = 4 * 62 + 8
        assert_eq!(
            base62(&[0xff; SECRET_BYTES], SECRET_CHARS).len(),
            SECRET_CHARS
        );
        assert_eq!(
            base62(&[0xff; KEY_ID_BYTES], KEY_ID_CHARS).len(),
            KEY_ID_CHARS
        );
    }

    /// Every integrator holds keys of this layout (docs/architecture.md,
    /// "Stable identifiers"): pinned in literals, not through the
    /// constants, so a changed width fails here instead of refusing every
    /// key already issued. The `w` is spelled `\x77` so that a
    /// search-and-replace of the prefix cannot rewrite the pin with it.
    /// Decisive: `PREFIX`, `KEY_ID_CHARS`, `SECRET_CHARS`.
    #[test]
    fn the_key_layout_is_pinned() {
        assert_eq!(PREFIX, "\x77ak_");
        let issued = format!("\x77ak_{}_{}", "0".repeat(17), "z".repeat(43));
        let presented = PresentedKey::parse(&issued).unwrap();
        assert_eq!(presented.key_id(), "0".repeat(17));
        for wrong in [
            format!("\x77ak_{}_{}", "0".repeat(16), "z".repeat(43)),
            format!("\x77ak_{}_{}", "0".repeat(18), "z".repeat(43)),
            format!("\x77ak_{}_{}", "0".repeat(17), "z".repeat(42)),
            format!("\x77ak_{}_{}", "0".repeat(17), "z".repeat(44)),
        ] {
            assert!(PresentedKey::parse(&wrong).is_none(), "{wrong}");
        }
        let minted = MintedKey::generate().unwrap();
        assert_eq!(minted.expose_key().len(), 4 + 17 + 1 + 43);
        assert_eq!(minted.key_id().len(), 17);
    }

    #[test]
    fn a_minted_key_parses_and_matches_its_digest_only() {
        let minted = MintedKey::generate().unwrap();
        let key = minted.expose_key();
        assert!(key.starts_with("wak_"));
        assert_eq!(key.len(), 4 + KEY_ID_CHARS + 1 + SECRET_CHARS);
        let presented = PresentedKey::parse(key).unwrap();
        assert_eq!(presented.key_id(), minted.key_id());
        assert!(presented.matches(&minted.digest()));
        let other = MintedKey::generate().unwrap();
        assert!(!presented.matches(&other.digest()));
        assert_ne!(minted.key_id(), other.key_id());
        // Debug never shows the secret.
        let secret = key.rsplit('_').next().unwrap();
        assert!(!format!("{minted:?}").contains(secret));
        assert!(!format!("{presented:?}").contains(secret));
    }

    #[test]
    fn malformed_keys_do_not_parse() {
        let minted = MintedKey::generate().unwrap();
        let key = minted.expose_key();
        let (id, secret) = key[4..].split_once('_').unwrap();
        for bad in [
            String::new(),
            key[4..].to_owned(),
            format!("wak_{id}{secret}"),
            format!("wak_{id}_{}", &secret[1..]),
            format!("wak_{id}_{secret}x"),
            format!("wak_{}_{secret}", &id[1..]),
            format!("wak_{id}_{}-", &secret[1..]),
            format!("WAK_{id}_{secret}"),
            format!("wak_{id}_{secret} "),
        ] {
            assert!(PresentedKey::parse(&bad).is_none(), "{bad:?}");
        }
    }

    /// The whole digest is compared: one differing bit, anywhere, refuses.
    /// (A changed secret changes every byte of its digest, so only a
    /// crafted digest catches a comparison of a prefix.)
    #[test]
    fn every_byte_of_the_digest_is_compared() {
        let minted = MintedKey::generate().unwrap();
        let secret = minted.expose_key().rsplit('_').next().unwrap().to_owned();
        assert!(matches(&minted.digest(), &secret));
        for byte in 0..32 {
            for bit in [0x01, 0x80] {
                let mut stored = minted.digest();
                stored[byte] ^= bit;
                assert!(!matches(&stored, &secret), "byte {byte}, bit {bit:#x}");
            }
        }
    }

    #[test]
    fn one_changed_character_does_not_match() {
        let minted = MintedKey::generate().unwrap();
        let key = minted.expose_key().to_owned();
        let last = key.chars().last().unwrap();
        let flipped = format!(
            "{}{}",
            &key[..key.len() - 1],
            if last == 'a' { 'b' } else { 'a' }
        );
        let presented = PresentedKey::parse(&flipped).unwrap();
        assert!(!presented.matches(&minted.digest()));
    }
}
