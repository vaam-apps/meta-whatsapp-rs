//! Signing secrets at rest (design §4.4 and §6): sealed under
//! `WA_SERVER_DATA_KEY` with AES-256-GCM, a random 96-bit nonce each
//! time, and bound to their endpoint and key id by the associated data,
//! so a sealed secret copied onto another endpoint's row does not open.
//! Signing needs the secret itself, so it is encrypted, not hashed.
//!
//! The stored form ([`SealedSecret`]) is `v1.<key id>.<base64 of nonce
//! and ciphertext>`; the associated data `wa-server/webhook-secret/v1`,
//! a zero byte, the endpoint's id, a zero byte and the key id. Both are
//! stable identifiers, pending the owner's confirmation
//! (docs/architecture.md, § Stable identifiers). A previous key
//! (`WA_SERVER_DATA_PREVIOUS_KEYS`) still opens what it sealed; a new
//! seal always uses the active key.

use std::fmt;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use meta_whatsapp_rs::core::secret::SecretBytes;

use super::signature::EndpointSecret;
use super::{EndpointId, SealedSecret};

/// The associated data's tag.
const AAD_TAG: &[u8] = b"wa-server/webhook-secret/v1";

/// The stored form's version.
const VERSION: &str = "v1";

/// A nonce's bytes.
const NONCE_LEN: usize = 12;

/// The longest key id.
const MAX_KEY_ID_LEN: usize = 32;

/// Why a data key was refused. Never holds the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidDataKey {
    /// Not 32 bytes (in base64).
    #[error("a data key is the base64 of 32 bytes")]
    Length,
    /// An id that is not 1 to 32 of `[A-Za-z0-9_-]`.
    #[error("a data key id is 1 to 32 of [A-Za-z0-9_-]")]
    Id,
    /// Two keys with one id.
    #[error("each data key needs its own id")]
    DuplicateId,
    /// No randomness for a throwaway key.
    #[error("no randomness for a data key")]
    Rng,
}

/// One data key and its id. `Debug` shows the id only.
#[derive(Clone)]
pub struct DataKey {
    id: String,
    key: SecretBytes,
}

impl fmt::Debug for DataKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DataKey")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl DataKey {
    /// `key` (32 bytes) under `id`.
    ///
    /// # Errors
    ///
    /// [`InvalidDataKey::Length`], [`InvalidDataKey::Id`].
    pub fn new(id: impl Into<String>, key: SecretBytes) -> Result<Self, InvalidDataKey> {
        let id = id.into();
        let valid_id = !id.is_empty()
            && id.len() <= MAX_KEY_ID_LEN
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !valid_id {
            return Err(InvalidDataKey::Id);
        }
        if key.len() != 32 {
            return Err(InvalidDataKey::Length);
        }
        Ok(Self { id, key })
    }

    /// The base64 of 32 bytes under `id`.
    ///
    /// # Errors
    ///
    /// As [`Self::new`]; not base64 is [`InvalidDataKey::Length`].
    pub fn from_base64(id: impl Into<String>, encoded: &str) -> Result<Self, InvalidDataKey> {
        let key = B64
            .decode(encoded.trim())
            .map_err(|_| InvalidDataKey::Length)?;
        Self::new(id, SecretBytes::new(key))
    }

    /// A random key under `id`: a throwaway one, for memory storage in
    /// development (what it seals dies with the process).
    ///
    /// # Errors
    ///
    /// As [`Self::new`], and [`InvalidDataKey::Rng`].
    pub fn generate(id: impl Into<String>) -> Result<Self, InvalidDataKey> {
        let mut key = vec![0u8; 32];
        getrandom::fill(&mut key).map_err(|_| InvalidDataKey::Rng)?;
        Self::new(id, SecretBytes::new(key))
    }

    /// Its id.
    pub fn id(&self) -> &str {
        &self.id
    }

    fn cipher(&self) -> Option<Aes256Gcm> {
        Aes256Gcm::new_from_slice(self.key.expose_secret()).ok()
    }
}

/// What re-sealing one stored secret found ([`DataKeys::reseal`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resealed {
    /// Already sealed under the active key; nothing to do.
    Current,
    /// Sealed under a previous key: the same secret, sealed under the
    /// active one.
    Sealed(SealedSecret),
    /// Not opened by any listed key, for this endpoint, or altered (or no
    /// randomness to seal with): left as it is.
    Unreadable,
}

/// The active data key and the previous ones. `Debug` shows their ids.
#[derive(Debug, Clone)]
pub struct DataKeys {
    active: DataKey,
    previous: Vec<DataKey>,
}

/// The associated data of `endpoint`'s secret sealed under `kid`.
fn aad(endpoint: &EndpointId, kid: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(AAD_TAG.len() + 2 + 36 + kid.len());
    out.extend_from_slice(AAD_TAG);
    out.push(0);
    out.extend_from_slice(endpoint.as_str().as_bytes());
    out.push(0);
    out.extend_from_slice(kid.as_bytes());
    out
}

impl DataKeys {
    /// `active` alone.
    pub fn new(active: DataKey) -> Self {
        Self {
            active,
            previous: Vec::new(),
        }
    }

    /// With `key`, which opens what it sealed and seals nothing.
    ///
    /// # Errors
    ///
    /// [`InvalidDataKey::DuplicateId`]: its id is already one of these.
    pub fn with_previous(mut self, key: DataKey) -> Result<Self, InvalidDataKey> {
        if self.find(&key.id).is_some() {
            return Err(InvalidDataKey::DuplicateId);
        }
        self.previous.push(key);
        Ok(self)
    }

    /// The active key's id.
    pub fn active_id(&self) -> &str {
        &self.active.id
    }

    fn find(&self, kid: &str) -> Option<&DataKey> {
        std::iter::once(&self.active)
            .chain(&self.previous)
            .find(|key| key.id == kid)
    }

    /// `secret`, sealed for `endpoint` under the active key; `None`
    /// without randomness.
    pub fn seal(&self, endpoint: &EndpointId, secret: &EndpointSecret) -> Option<SealedSecret> {
        let key = &self.active;
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).ok()?;
        let ciphertext = key
            .cipher()?
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: secret.key(),
                    aad: &aad(endpoint, &key.id),
                },
            )
            .ok()?;
        let mut sealed = nonce.to_vec();
        sealed.extend_from_slice(&ciphertext);
        Some(SealedSecret::from_stored(format!(
            "{VERSION}.{}.{}",
            key.id,
            B64.encode(sealed)
        )))
    }

    /// `sealed`, which `endpoint`'s secret is stored as, sealed anew under
    /// the active key when another key sealed it (roadmap M2i). The
    /// plaintext lives in this call only (zeroized on drop).
    pub fn reseal(&self, endpoint: &EndpointId, sealed: &SealedSecret) -> Resealed {
        let Some(secret) = self.open(endpoint, sealed) else {
            return Resealed::Unreadable;
        };
        let current = sealed
            .as_stored()
            .split('.')
            .nth(1)
            .is_some_and(|kid| kid == self.active.id);
        if current {
            return Resealed::Current;
        }
        self.seal(endpoint, &secret)
            .map_or(Resealed::Unreadable, Resealed::Sealed)
    }

    /// `endpoint`'s secret, opened; `None` when it was sealed under a key
    /// not listed, for another endpoint, or was altered.
    pub fn open(&self, endpoint: &EndpointId, sealed: &SealedSecret) -> Option<EndpointSecret> {
        let mut parts = sealed.as_stored().splitn(3, '.');
        let (Some(VERSION), Some(kid), Some(encoded)) = (parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        let key = self.find(kid)?;
        let bytes = B64.decode(encoded).ok()?;
        if bytes.len() <= NONCE_LEN {
            return None;
        }
        let (nonce, ciphertext) = bytes.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().ok()?;
        let plain = key
            .cipher()?
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: ciphertext,
                    aad: &aad(endpoint, kid),
                },
            )
            .ok()
            .map(SecretBytes::new)?;
        // The key as it is, zeroized on drop: no base64 copy of it.
        EndpointSecret::from_key(plain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> DataKeys {
        DataKeys::new(DataKey::from_base64("k1", &B64.encode([1u8; 32])).unwrap())
    }

    /// A sealed secret opens for its endpoint, under its key or after the
    /// key became a previous one; never for another endpoint, under an
    /// unlisted key, or altered. It holds nothing of the secret in the
    /// clear, and two seals of one secret differ (the nonce). Decisive:
    /// the endpoint in the associated data.
    #[test]
    fn a_sealed_secret_opens_for_its_endpoint_only() {
        let endpoint = EndpointId::generate().unwrap();
        let other = EndpointId::generate().unwrap();
        let secret = EndpointSecret::generate().unwrap();
        let sealed = keys().seal(&endpoint, &secret).unwrap();
        assert!(sealed.as_stored().starts_with("v1.k1."));
        assert!(!sealed.as_stored().contains(&secret.expose()[6..]));
        assert_ne!(keys().seal(&endpoint, &secret).unwrap(), sealed);
        assert_eq!(
            keys().open(&endpoint, &sealed).map(|s| s.expose()),
            Some(secret.expose())
        );
        assert!(keys().open(&other, &sealed).is_none(), "another endpoint");
        let rotated = DataKeys::new(DataKey::generate("k2").unwrap())
            .with_previous(DataKey::from_base64("k1", &B64.encode([1u8; 32])).unwrap())
            .unwrap();
        assert_eq!(
            rotated.open(&endpoint, &sealed).map(|s| s.expose()),
            Some(secret.expose()),
            "a previous key opens what it sealed"
        );
        assert!(
            rotated
                .seal(&endpoint, &secret)
                .unwrap()
                .as_stored()
                .starts_with("v1.k2.")
        );
        let unlisted = DataKeys::new(DataKey::generate("k1").unwrap());
        assert!(
            unlisted.open(&endpoint, &sealed).is_none(),
            "another key, same id"
        );
        let mut altered = sealed.as_stored().to_owned();
        altered.push('A');
        assert!(
            keys()
                .open(&endpoint, &SealedSecret::from_stored(altered))
                .is_none()
        );
        let relabelled = sealed.as_stored().replacen("v1.k1.", "v1.k2.", 1);
        assert!(
            rotated
                .open(&endpoint, &SealedSecret::from_stored(relabelled))
                .is_none()
        );
    }

    /// The stored form and the associated data, pinned apart from
    /// `seal`: a secret sealed by hand with a fixed nonce, the documented
    /// layout (`v1.<kid>.<base64 of nonce and ciphertext>`) and associated
    /// data (`wa-server/webhook-secret/v1`, NUL, the endpoint's id, NUL,
    /// the key id), opens; one sealed under another tag does not. A change
    /// of either strands every stored secret (a stable identifier,
    /// docs/architecture.md). `\x77` spells the tag's `w`.
    #[test]
    fn the_sealed_format_is_pinned() {
        assert_eq!(AAD_TAG, b"\x77a-server/webhook-secret/v1");
        let endpoint = EndpointId::parse("whe_00112233445566778899aabbccddeeff").unwrap();
        let key = [1u8; 32];
        let secret = [9u8; 32];
        let by_hand = |tag: &[u8]| {
            let mut aad = tag.to_vec();
            aad.push(0);
            aad.extend_from_slice(b"whe_00112233445566778899aabbccddeeff");
            aad.push(0);
            aad.extend_from_slice(b"k1");
            let nonce = [7u8; 12];
            let ct = Aes256Gcm::new_from_slice(&key)
                .unwrap()
                .encrypt(
                    &Nonce::from(nonce),
                    Payload {
                        msg: &secret,
                        aad: &aad,
                    },
                )
                .unwrap();
            let mut blob = nonce.to_vec();
            blob.extend_from_slice(&ct);
            SealedSecret::from_stored(format!("v1.k1.{}", B64.encode(blob)))
        };
        let opened = keys().open(&endpoint, &by_hand(AAD_TAG)).unwrap();
        assert_eq!(opened.key(), &secret);
        assert!(
            keys()
                .open(&endpoint, &by_hand(b"wa-rs/token-vault/v1"))
                .is_none()
        );
    }

    /// The key id is bound too, not only named: one key's bytes listed
    /// under two ids, a blob relabelled from one to the other does not
    /// open (the key it names would decrypt it, the associated data
    /// refuses it). Decisive: the key id in the associated data.
    #[test]
    fn a_sealed_secret_is_bound_to_its_key_id() {
        let endpoint = EndpointId::generate().unwrap();
        let secret = EndpointSecret::generate().unwrap();
        let bytes = B64.encode([3u8; 32]);
        let sealed = DataKeys::new(DataKey::from_base64("k1", &bytes).unwrap())
            .seal(&endpoint, &secret)
            .unwrap();
        let both = DataKeys::new(DataKey::from_base64("k2", &bytes).unwrap())
            .with_previous(DataKey::from_base64("k1", &bytes).unwrap())
            .unwrap();
        assert_eq!(
            both.open(&endpoint, &sealed).map(|s| s.expose()),
            Some(secret.expose())
        );
        let relabelled = sealed.as_stored().replacen("v1.k1.", "v1.k2.", 1);
        assert!(
            both.open(&endpoint, &SealedSecret::from_stored(relabelled))
                .is_none(),
            "the same key under another id"
        );
        let other_version = sealed.as_stored().replacen("v1.", "v2.", 1);
        assert!(
            both.open(&endpoint, &SealedSecret::from_stored(other_version))
                .is_none()
        );
    }

    /// Keys are 32 bytes under an id of their own; `Debug` shows ids
    /// only.
    #[test]
    fn data_keys_are_checked() {
        assert_eq!(
            DataKey::from_base64("k1", &B64.encode([1u8; 31])).unwrap_err(),
            InvalidDataKey::Length
        );
        assert_eq!(
            DataKey::from_base64("k.1", &B64.encode([1u8; 32])).unwrap_err(),
            InvalidDataKey::Id
        );
        assert_eq!(
            keys()
                .with_previous(DataKey::generate("k1").unwrap())
                .unwrap_err(),
            InvalidDataKey::DuplicateId
        );
        let debug = format!("{:?}", keys());
        assert!(debug.contains("k1"));
        assert!(!debug.contains(&B64.encode([1u8; 32])));
    }
}
