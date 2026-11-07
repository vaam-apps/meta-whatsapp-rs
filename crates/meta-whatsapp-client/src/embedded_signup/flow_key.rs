//! A Flow private key in the token vault (roadmap M5d2, design D370):
//! the business RSA key a WABA's Flows data endpoint decrypts with
//! ([`FlowEndpointKey`]), stored encrypted beside the WABA's token, for
//! one **owner**.
//!
//! # Format
//!
//! Namespace `wa.token`, key `flow-key/<WABA_ID>` (the name is pending the
//! owner's confirmation before the first release): a sealed record
//! `{v, kid, nonce, ciphertext}`, the same shape and the same keys as the
//! Solution Partner credit ledger's (the module's `seal_blob`), so the
//! vault's rotation covers it ([`TokenVault::rotate`] re-seals it under
//! the active key, a read re-seals it when the vault rotates on read).
//! The associated data binds the ciphertext to the namespace, the store
//! key (so to the WABA) and the key id, each length-prefixed: a record
//! copied under another WABA's key does not open. The sealed plaintext is
//! `{"owner": …, "private_key_pem": …}`.
//!
//! # The owner
//!
//! The vault does not know tenants, so the caller names one: an opaque,
//! non-empty label (the service passes its tenant's id), sealed with the
//! key. [`TokenVault::flow_key`] gives the key **only to the owner that
//! stored it**: for another it is `None`, as if no key were stored. A
//! WABA that changes hands therefore never serves its previous holder's
//! key to the new one, whether or not anyone deleted it
//! ([`TokenVault::delete_flow_key`] removes the record whoever stored it:
//! the caller has already proved it holds the WABA).
//!
//! # What this does not protect against
//!
//! Anyone holding the vault key and the store (see [`TokenVault`]). The
//! key's text lives in [`SecretBytes`] while this module holds it; the
//! PEM the caller passes in is theirs to wipe. [`TokenVault::delete`]
//! leaves the record, as it leaves the credit ledger: a token's removal
//! is not a key's.

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::{CryptoError, ValidationError};
use meta_whatsapp_core::ids::WabaId;
use meta_whatsapp_core::secret::SecretBytes;
use meta_whatsapp_core::store::{Expiry, StoreKey};
use serde::{Deserialize, Serialize};

use super::vault::{SealedBlob, TOKEN_NAMESPACE, TokenVault, decode_json, encode_json};
use crate::flows::endpoint::FlowEndpointKey;

/// Longest owner label accepted.
pub const MAX_FLOW_KEY_OWNER_CHARS: usize = 128;

#[derive(Serialize)]
struct PlainRef<'a> {
    owner: &'a str,
    private_key_pem: &'a str,
}

#[derive(Deserialize)]
struct Plain {
    owner: String,
    private_key_pem: String,
}

fn flow_key_of(waba_id: &WabaId) -> StoreKey {
    StoreKey::new(TOKEN_NAMESPACE, format!("flow-key/{waba_id}"))
}

impl TokenVault {
    /// Seal `pem` (an unencrypted 2048-bit RSA private key, PKCS#8 or
    /// PKCS#1: what [`FlowEndpointKey::from_pem`] reads) as the Flow key of
    /// `waba_id` for `owner`, replacing any record there (a rotation).
    ///
    /// The key is parsed first, so one that cannot decrypt a Flow request
    /// is refused before anything is written, and the error never quotes
    /// it.
    ///
    /// # Errors
    ///
    /// `owner` empty or over [`MAX_FLOW_KEY_OWNER_CHARS`]: a
    /// [`ValidationError`]. `pem` not text, or not such a key:
    /// [`CryptoError::InvalidKey`]. The store failing.
    pub async fn store_flow_key(
        &self,
        waba_id: &WabaId,
        owner: &str,
        pem: &SecretBytes,
    ) -> Result<()> {
        if owner.is_empty() || owner.chars().count() > MAX_FLOW_KEY_OWNER_CHARS {
            return Err(ValidationError::new(
                "owner",
                format!("must be 1-{MAX_FLOW_KEY_OWNER_CHARS} characters"),
            )
            .into());
        }
        let text = std::str::from_utf8(pem.expose_secret())
            .map_err(|_| CryptoError::InvalidKey("a Flow private key is PEM text"))?;
        FlowEndpointKey::from_pem(text)?;
        let plain = serde_json::to_vec(&PlainRef {
            owner,
            private_key_pem: text,
        })
        .map(SecretBytes::new)
        .map_err(|_| CryptoError::Encrypt)?;
        let key = flow_key_of(waba_id);
        let bytes = encode_json(&key, &self.seal_blob(&key, plain.expose_secret())?)?;
        self.kv().put(&key, bytes, Expiry::Never).await?;
        Ok(())
    }

    /// The Flow key stored for `waba_id` by `owner`; `None` when there is
    /// none, or it was stored by another owner (see the module docs). A
    /// record under a previous vault key is re-sealed under the active one
    /// when the vault rotates on read (compare-and-swap; a concurrent
    /// write wins).
    ///
    /// # Errors
    ///
    /// The record cannot be authenticated (edited, copied from another
    /// WABA, sealed under a key no longer configured): [`CryptoError`].
    /// The store failing.
    pub async fn flow_key(&self, waba_id: &WabaId, owner: &str) -> Result<Option<FlowEndpointKey>> {
        let key = flow_key_of(waba_id);
        let Some(v) = self.kv().get(&key).await? else {
            return Ok(None);
        };
        let blob: SealedBlob = decode_json(&key, &v)?;
        let plain = SecretBytes::new(self.open_blob(&key, &blob)?);
        let Plain {
            owner: stored,
            private_key_pem,
        } = serde_json::from_slice(plain.expose_secret())
            .map_err(|_| CryptoError::Malformed("decrypted Flow key record"))?;
        let pem = SecretBytes::new(private_key_pem.into_bytes());
        if stored != owner {
            return Ok(None);
        }
        let text = std::str::from_utf8(pem.expose_secret())
            .map_err(|_| CryptoError::Malformed("decrypted Flow key record"))?;
        let parsed = FlowEndpointKey::from_pem(text)?;
        if self.rotates_on_read()
            && !self.is_current(&blob)
            && let Err(error) = self.reseal_flow_key(&key, &plain, v.version).await
        {
            tracing::warn!(
                kind = ?error.kind(),
                "Flow key: re-sealing under the active key failed; retried on next read"
            );
        }
        Ok(Some(parsed))
    }

    /// Whether a Flow key record is stored for `waba_id`, whoever stored
    /// it and readable or not: nothing is decrypted.
    ///
    /// # Errors
    ///
    /// The store failing.
    pub async fn holds_flow_key(&self, waba_id: &WabaId) -> Result<bool> {
        Ok(self.kv().get(&flow_key_of(waba_id)).await?.is_some())
    }

    /// Delete the Flow key record of `waba_id`, whoever stored it: whether
    /// there was one.
    ///
    /// # Errors
    ///
    /// The store failing.
    pub async fn delete_flow_key(&self, waba_id: &WabaId) -> Result<bool> {
        Ok(self.kv().delete(&flow_key_of(waba_id)).await?)
    }

    /// Re-seal the Flow key record of `waba_id` under the active key if it
    /// is under an older one: whether it was rewritten. Part of
    /// [`Self::rotate`].
    pub(super) async fn rotate_flow_key(&self, waba_id: &WabaId) -> Result<bool> {
        let key = flow_key_of(waba_id);
        let Some(v) = self.kv().get(&key).await? else {
            return Ok(false);
        };
        let blob: SealedBlob = decode_json(&key, &v)?;
        if self.is_current(&blob) {
            return Ok(false);
        }
        let plain = SecretBytes::new(self.open_blob(&key, &blob)?);
        Ok(self
            .reseal_flow_key(&key, &plain, v.version)
            .await?
            .is_some())
    }

    /// Seal `plain` under the active key in place of version `version`.
    async fn reseal_flow_key(
        &self,
        key: &StoreKey,
        plain: &SecretBytes,
        version: u64,
    ) -> Result<Option<u64>> {
        let bytes = encode_json(key, &self.seal_blob(key, plain.expose_secret())?)?;
        Ok(self
            .kv()
            .compare_and_swap(key, version, Some(bytes), Expiry::Keep)
            .await?)
    }
}

#[cfg(test)]
mod tests;
