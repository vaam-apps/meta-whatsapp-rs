//! Media a user uploaded through a `PhotoPicker` or `DocumentPicker` and
//! that reached your endpoint in a `data_exchange` request
//! (`flows/guides/media_upload`, "Handling media").
//!
//! WhatsApp stores each file on its CDN (up to 20 days) encrypted with
//! AES-256-CBC (PKCS#7) and authenticated with HMAC-SHA256; the request
//! carries the keys. You download `cdn_url` yourself — it is a plain
//! download, not a Graph call — and hand the bytes to [`decrypt_media`].
//!
//! Meta also warns that uploaded files may be malicious: treat the
//! decrypted bytes as untrusted input.

use std::fmt;

use aws_lc_rs::cipher::{AES_256, DecryptionContext, PaddedBlockDecryptingKey, UnboundCipherKey};
use aws_lc_rs::iv::FixedLength;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use wa_core::error::CryptoError;

use super::BASE64;
use super::pem::Scrubbed;

/// Bytes of the HMAC-SHA256 kept at the end of the CDN file.
const HMAC_LEN: usize = 10;

/// One uploaded file, as it appears in the request `data` (an array of these
/// under the picker component's name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowMedia {
    /// Id of the upload; use it as the key of a per-file `error-message`.
    pub media_id: String,
    /// Where to download the encrypted file.
    pub cdn_url: String,
    /// Original file name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    /// Keys and hashes to decrypt and check the file.
    pub encryption_metadata: MediaEncryptionMetadata,
}

/// `encryption_metadata` of an uploaded file. All values are base64.
/// `Debug` hides the two keys.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaEncryptionMetadata {
    /// SHA-256 of the downloaded (encrypted) file.
    pub encrypted_hash: String,
    /// AES-CBC IV, also the first HMAC input.
    pub iv: String,
    /// AES-256 key. Secret.
    pub encryption_key: String,
    /// HMAC-SHA256 key. Secret.
    pub hmac_key: String,
    /// SHA-256 of the decrypted file.
    pub plaintext_hash: String,
}

impl fmt::Debug for MediaEncryptionMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaEncryptionMetadata")
            .field("encrypted_hash", &self.encrypted_hash)
            .field("iv", &self.iv)
            .field("encryption_key", &"[REDACTED]")
            .field("hmac_key", &"[REDACTED]")
            .field("plaintext_hash", &self.plaintext_hash)
            .finish()
    }
}

impl FlowMedia {
    /// [`decrypt_media`] with this file's metadata.
    pub fn decrypt(&self, cdn_file: &[u8]) -> Result<Vec<u8>, CryptoError> {
        decrypt_media(cdn_file, &self.encryption_metadata)
    }
}

/// Verify and decrypt a downloaded CDN file, following Meta's steps:
/// SHA-256 of the file must equal `encrypted_hash`; the file is
/// `ciphertext || hmac10`, where `hmac10` is the first 10 bytes of
/// HMAC-SHA256(`hmac_key`, `iv || ciphertext`); the ciphertext is
/// AES-256-CBC with PKCS#7 padding; SHA-256 of the result must equal
/// `plaintext_hash`.
///
/// The HMAC is checked (in constant time) before any decryption, so a
/// tampered file never reaches the padding check. Every failure is the
/// single [`CryptoError::Decrypt`].
///
/// Meta's text lists the HMAC inputs as "`hmac_key`, initialization vector
/// and ciphertext" without spelling out the byte order; `iv || ciphertext`
/// is used, in that order.
pub fn decrypt_media(
    cdn_file: &[u8],
    metadata: &MediaEncryptionMetadata,
) -> Result<Vec<u8>, CryptoError> {
    open(cdn_file, metadata).ok_or(CryptoError::Decrypt)
}

fn open(cdn_file: &[u8], metadata: &MediaEncryptionMetadata) -> Option<Vec<u8>> {
    let encrypted_hash = BASE64.decode(&metadata.encrypted_hash).ok()?;
    let plaintext_hash = BASE64.decode(&metadata.plaintext_hash).ok()?;
    let iv: [u8; 16] = BASE64.decode(&metadata.iv).ok()?.try_into().ok()?;
    let encryption_key = Scrubbed(BASE64.decode(&metadata.encryption_key).ok()?);
    let hmac_key = Scrubbed(BASE64.decode(&metadata.hmac_key).ok()?);

    if !bool::from(Sha256::digest(cdn_file).as_slice().ct_eq(&encrypted_hash)) {
        return None;
    }
    let ciphertext_len = cdn_file.len().checked_sub(HMAC_LEN)?;
    let ciphertext = cdn_file.get(..ciphertext_len)?;
    let hmac10 = cdn_file.get(ciphertext_len..)?;

    let mut mac = Hmac::<Sha256>::new_from_slice(&hmac_key.0).ok()?;
    mac.update(&iv);
    mac.update(ciphertext);
    let tag = mac.finalize().into_bytes();
    if !bool::from(tag.get(..HMAC_LEN)?.ct_eq(hmac10)) {
        return None;
    }

    let key = UnboundCipherKey::new(&AES_256, &encryption_key.0).ok()?;
    let cipher = PaddedBlockDecryptingKey::cbc_pkcs7(key).ok()?;
    let mut buffer = ciphertext.to_vec();
    let plaintext_len = cipher
        .decrypt(&mut buffer, DecryptionContext::Iv128(FixedLength::from(iv)))
        .ok()?
        .len();
    buffer.truncate(plaintext_len);

    if !bool::from(Sha256::digest(&buffer).as_slice().ct_eq(&plaintext_hash)) {
        return None;
    }
    Some(buffer)
}
