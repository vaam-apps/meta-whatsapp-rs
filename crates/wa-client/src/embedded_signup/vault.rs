//! `TokenVault`: business tokens encrypted at rest on any [`KvStore`].
//!
//! # Format
//!
//! Namespace `wa.token`. Two kinds of keys:
//!
//! - `waba/<WABA_ID>` → a JSON record
//!   `{v, kid, nonce, ciphertext, waba_id, business_id?, phone_number_ids,
//!   created_at, expires_at?}`. `nonce` and `ciphertext` are standard
//!   base64; timestamps are unix seconds.
//! - `phone/<PHONE_NUMBER_ID>` → `{"waba_id": …}`, the index behind
//!   [`TokenVault::get_by_phone_number`] (webhooks carry the phone number
//!   id; this finds the token to answer with).
//!
//! # Cryptography, and why each choice
//!
//! - **AES-256-GCM**, a fresh random 96-bit nonce per write (`getrandom`).
//!   With random nonces the safe budget is about 2³² writes per key; token
//!   writes happen once per onboarding, so rotating keys yearly is far
//!   inside it.
//! - **Associated data** binds the ciphertext to `wa.token`, the WABA id it
//!   is stored under, and the key id, each length-prefixed (so no two
//!   different triples encode to the same bytes). Copying tenant A's record
//!   over tenant B's key therefore fails to decrypt instead of handing B's
//!   traffic A's token. The WABA id used is the one being *looked up*, never
//!   the one written in the record.
//! - **The sealed plaintext repeats the metadata** (WABA, business, phone
//!   numbers, times). The clear copies in the record are for operators and
//!   index cleanup only; everything returned by `get` comes from the
//!   authenticated copy, so editing the clear JSON changes nothing.
//! - **Key rotation**: records remember their key id. Reads decrypt with
//!   whichever configured key matches; writes always use the active key. By
//!   default a read of a record under an old key re-encrypts it under the
//!   active key (compare-and-swap, so a concurrent write wins); [`TokenVault::rotate`]
//!   does the same on demand. Once every record is rotated, drop the old key.
//! - Errors carry no detail that could serve as an oracle
//!   ([`CryptoError::Decrypt`] for any authentication failure).
//!
//! # What this does not protect against
//!
//! Anyone holding the vault key and the store. Key material lives in memory
//! for the life of the vault; [`VaultKey`] is zeroed on drop on a
//! best-effort basis (no `zeroize` dependency yet, see the crate's pending
//! change requests), and never printed by `Debug`.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use wa_core::clock::{Clock, SystemClock};
use wa_core::error::{CryptoError, StorageError, ValidationError};
use wa_core::ids::{BusinessId, PhoneNumberId, WabaId};
use wa_core::secret::AccessToken;
use wa_core::store::{Expiry, KvStore, StoreKey, Versioned};
use wa_core::{Error, Result};

/// `KvStore` namespace of the vault.
pub const TOKEN_NAMESPACE: &str = "wa.token";

/// Domain separation for the associated data; bump with the record format.
const AAD_TAG: &[u8] = b"wa-rs/token-vault/v1";
const RECORD_VERSION: u8 = 1;
const NONCE_LEN: usize = 12;
const MAX_KEY_ID_LEN: usize = 64;

/// A 256-bit AES key and the id recorded next to what it encrypts.
///
/// `Debug` prints the id only. The key bytes are overwritten on drop (best
/// effort: without `zeroize`, copies the compiler makes are not tracked).
pub struct VaultKey {
    id: String,
    key: [u8; 32],
}

impl VaultKey {
    /// Build a key. `id` is 1–64 characters of `A-Z a-z 0-9 - _ . :`
    /// (it is stored in every record and bound into the ciphertext).
    ///
    /// `key` is taken by value; wipe your own copy if you had one.
    pub fn new(id: impl Into<String>, key: [u8; 32]) -> Result<Self> {
        let id = id.into();
        let valid_id = !id.is_empty()
            && id.len() <= MAX_KEY_ID_LEN
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
        if !valid_id {
            return Err(ValidationError::new(
                "key_id",
                "must be 1-64 characters of A-Z a-z 0-9 - _ . :",
            )
            .into());
        }
        Ok(Self { id, key })
    }

    /// Decode a key from standard base64 (e.g. from a secret manager or an
    /// environment variable). It must decode to exactly 32 bytes.
    pub fn from_base64(id: impl Into<String>, encoded: &str) -> Result<Self> {
        let mut bytes = B64
            .decode(encoded.trim())
            .map_err(|_| CryptoError::InvalidKey("vault key is not valid base64"))?;
        let key = <[u8; 32]>::try_from(bytes.as_slice())
            .map_err(|_| CryptoError::InvalidKey("vault key must decode to exactly 32 bytes"));
        wipe(&mut bytes);
        Self::new(id, key?)
    }

    /// A fresh random key from the operating system's CSPRNG.
    pub fn generate(id: impl Into<String>) -> Result<Self> {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key)
            .map_err(|_| CryptoError::InvalidKey("the OS random number generator failed"))?;
        Self::new(id, key)
    }

    /// The key id.
    pub fn id(&self) -> &str {
        &self.id
    }

    fn cipher(&self) -> Result<Aes256Gcm, CryptoError> {
        Aes256Gcm::new_from_slice(&self.key).map_err(|_| CryptoError::InvalidKey("vault key"))
    }
}

impl Drop for VaultKey {
    fn drop(&mut self) {
        wipe(&mut self.key);
    }
}

impl fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultKey")
            .field("id", &self.id)
            .field("key", &"[REDACTED]")
            .finish()
    }
}

/// The active key (encrypts every write) and previous keys (still decrypt
/// older records).
#[derive(Debug)]
pub struct VaultKeys {
    active: VaultKey,
    previous: Vec<VaultKey>,
}

impl VaultKeys {
    /// Only an active key.
    pub fn new(active: VaultKey) -> Self {
        Self {
            active,
            previous: Vec::new(),
        }
    }

    /// Keep an older key for decryption during a rotation.
    #[must_use]
    pub fn with_previous(mut self, key: VaultKey) -> Self {
        self.previous.push(key);
        self
    }

    /// Id of the key new writes use.
    pub fn active_id(&self) -> &str {
        self.active.id()
    }

    fn find(&self, id: &str) -> Option<&VaultKey> {
        std::iter::once(&self.active)
            .chain(&self.previous)
            .find(|k| k.id == id)
    }

    fn validate(&self) -> Result<()> {
        let mut seen = HashSet::new();
        for key in std::iter::once(&self.active).chain(&self.previous) {
            if !seen.insert(key.id.as_str()) {
                return Err(ValidationError::new(
                    "vault_keys",
                    format!("key id `{}` is used twice", key.id),
                )
                .into());
            }
        }
        Ok(())
    }
}

/// A business token and the assets it serves, as stored in the vault.
///
/// `Debug` never shows the token.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StoredBusinessToken {
    /// The WABA this token is stored under.
    pub waba_id: WabaId,
    /// The business token.
    pub token: AccessToken,
    /// The customer's business portfolio, when known.
    pub business_id: Option<BusinessId>,
    /// Phone numbers indexed to this WABA.
    pub phone_number_ids: Vec<PhoneNumberId>,
    /// When the record was first written; set by [`TokenVault::store`] when
    /// `None`.
    pub created_at: Option<OffsetDateTime>,
    /// When the token expires, if it does.
    pub expires_at: Option<OffsetDateTime>,
}

impl StoredBusinessToken {
    /// A token for `waba_id` with no other metadata.
    pub fn new(waba_id: impl Into<WabaId>, token: AccessToken) -> Self {
        Self {
            waba_id: waba_id.into(),
            token,
            business_id: None,
            phone_number_ids: Vec::new(),
            created_at: None,
            expires_at: None,
        }
    }

    /// Set the business portfolio.
    #[must_use]
    pub fn business_id(mut self, business_id: impl Into<BusinessId>) -> Self {
        self.business_id = Some(business_id.into());
        self
    }

    /// Set the phone numbers to index.
    #[must_use]
    pub fn phone_number_ids(
        mut self,
        ids: impl IntoIterator<Item = impl Into<PhoneNumberId>>,
    ) -> Self {
        self.phone_number_ids = ids.into_iter().map(Into::into).collect();
        self
    }

    /// Set the expiry.
    #[must_use]
    pub fn expires_at(mut self, at: OffsetDateTime) -> Self {
        self.expires_at = Some(at);
        self
    }

    /// Whether the token has expired at `now`.
    pub fn is_expired(&self, now: OffsetDateTime) -> bool {
        self.expires_at.is_some_and(|t| t <= now)
    }
}

/// Encrypted business token storage, keyed by WABA, with a phone number
/// index. Cheap to clone.
#[derive(Clone)]
pub struct TokenVault {
    kv: Arc<dyn KvStore>,
    keys: Arc<VaultKeys>,
    clock: Arc<dyn Clock>,
    rotate_on_read: bool,
}

impl fmt::Debug for TokenVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenVault")
            .field("namespace", &TOKEN_NAMESPACE)
            .field("keys", &self.keys)
            .field("rotate_on_read", &self.rotate_on_read)
            .finish_non_exhaustive()
    }
}

/// The clear part of a stored record.
#[derive(Serialize, Deserialize)]
struct Record {
    v: u8,
    kid: String,
    nonce: String,
    ciphertext: String,
    waba_id: WabaId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    business_id: Option<BusinessId>,
    #[serde(default)]
    phone_number_ids: Vec<PhoneNumberId>,
    #[serde(with = "time::serde::timestamp")]
    created_at: OffsetDateTime,
    #[serde(default, with = "time::serde::timestamp::option")]
    expires_at: Option<OffsetDateTime>,
}

/// The encrypted part. Holds the token in the clear: never `Debug`, wiped
/// after use.
#[derive(Serialize, Deserialize)]
struct Sealed {
    access_token: String,
    waba_id: WabaId,
    #[serde(default)]
    business_id: Option<BusinessId>,
    #[serde(default)]
    phone_number_ids: Vec<PhoneNumberId>,
    #[serde(with = "time::serde::timestamp")]
    created_at: OffsetDateTime,
    #[serde(default, with = "time::serde::timestamp::option")]
    expires_at: Option<OffsetDateTime>,
}

#[derive(Serialize, Deserialize)]
struct PhoneIndex {
    waba_id: WabaId,
}

impl TokenVault {
    /// A vault on `kv` with `keys`, on the system clock, re-encrypting old
    /// records on read. Fails if two keys share an id.
    pub fn new(kv: Arc<dyn KvStore>, keys: VaultKeys) -> Result<Self> {
        keys.validate()?;
        Ok(Self {
            kv,
            keys: Arc::new(keys),
            clock: Arc::new(SystemClock),
            rotate_on_read: true,
        })
    }

    /// Use `clock` for `created_at` (tests pass a `ManualClock`).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Whether [`Self::get`] re-encrypts records found under a previous key
    /// (default `true`). Turn it off for read-only replicas and rotate with
    /// [`Self::rotate`] instead.
    #[must_use]
    pub fn rotate_on_read(mut self, enabled: bool) -> Self {
        self.rotate_on_read = enabled;
        self
    }

    /// Current time on the vault's clock.
    pub(crate) fn now(&self) -> OffsetDateTime {
        self.clock.now()
    }

    /// Encrypt and store `token` under its WABA (replacing any previous
    /// token), and point each of its phone numbers at that WABA. Phone
    /// numbers the previous record listed and this one does not are
    /// unlinked. Safe to repeat.
    pub async fn store(&self, token: &StoredBusinessToken) -> Result<()> {
        if token.waba_id.as_str().is_empty() {
            return Err(ValidationError::new("waba_id", "required").into());
        }
        if token.token.expose_secret().is_empty() {
            return Err(ValidationError::new("token", "required").into());
        }
        let key = waba_key(&token.waba_id);
        // Best effort: only used to unlink phone numbers that were dropped.
        let previous = self.read_record(&key).await.ok().flatten();
        let created_at = token.created_at.unwrap_or_else(|| self.clock.now());
        let record = self.seal(token, created_at)?;
        self.kv
            .put(&key, encode(&key, &record)?, Expiry::Never)
            .await?;
        for phone in &token.phone_number_ids {
            let pkey = phone_key(phone);
            let index = PhoneIndex {
                waba_id: token.waba_id.clone(),
            };
            self.kv
                .put(&pkey, encode(&pkey, &index)?, Expiry::Never)
                .await?;
        }
        if let Some((prev, _)) = previous {
            for phone in prev
                .phone_number_ids
                .iter()
                .filter(|p| !token.phone_number_ids.contains(p))
            {
                self.unlink_phone(phone, &token.waba_id).await?;
            }
        }
        Ok(())
    }

    /// The token stored for `waba_id`, decrypted.
    ///
    /// Fails with [`CryptoError`] when the record cannot be authenticated
    /// (wrong key, tampering, a record copied from another WABA) or was
    /// written with a key id not in [`VaultKeys`].
    pub async fn get(&self, waba_id: &WabaId) -> Result<Option<StoredBusinessToken>> {
        let key = waba_key(waba_id);
        let Some((record, version)) = self.read_record(&key).await? else {
            return Ok(None);
        };
        let token = self.open(waba_id, &record)?;
        if self.rotate_on_read
            && record.kid != self.keys.active.id
            && let Err(e) = self.reseal(&key, &token, version).await
        {
            tracing::warn!(
                waba_id = %waba_id,
                error = %e,
                "token vault: re-encrypting under the active key failed; retried on next read"
            );
        }
        Ok(Some(token))
    }

    /// The token for the WABA `phone_number_id` belongs to.
    ///
    /// `None` when the number is not indexed, or when the index is stale
    /// (the WABA's authenticated record no longer lists the number).
    pub async fn get_by_phone_number(
        &self,
        phone_number_id: &PhoneNumberId,
    ) -> Result<Option<StoredBusinessToken>> {
        let pkey = phone_key(phone_number_id);
        let Some(v) = self.kv.get(&pkey).await? else {
            return Ok(None);
        };
        let index: PhoneIndex = decode(&pkey, &v)?;
        let Some(token) = self.get(&index.waba_id).await? else {
            return Ok(None);
        };
        Ok(token
            .phone_number_ids
            .contains(phone_number_id)
            .then_some(token))
    }

    /// Delete the token of `waba_id` and the phone index entries that point
    /// to it. Returns whether a record was removed.
    pub async fn delete(&self, waba_id: &WabaId) -> Result<bool> {
        let key = waba_key(waba_id);
        let record = self.read_record(&key).await.ok().flatten();
        let removed = self.kv.delete(&key).await?;
        if let Some((record, _)) = record {
            for phone in &record.phone_number_ids {
                self.unlink_phone(phone, waba_id).await?;
            }
        }
        Ok(removed)
    }

    /// Re-encrypt the record of `waba_id` under the active key if it is
    /// under an older one. Returns whether it was rewritten (`false` also
    /// when a concurrent write got there first, or there is no record).
    pub async fn rotate(&self, waba_id: &WabaId) -> Result<bool> {
        let key = waba_key(waba_id);
        let Some((record, version)) = self.read_record(&key).await? else {
            return Ok(false);
        };
        if record.kid == self.keys.active.id {
            return Ok(false);
        }
        let token = self.open(waba_id, &record)?;
        self.reseal(&key, &token, version).await
    }

    async fn reseal(
        &self,
        key: &StoreKey,
        token: &StoredBusinessToken,
        version: u64,
    ) -> Result<bool> {
        let created_at = token.created_at.unwrap_or_else(|| self.clock.now());
        let record = self.seal(token, created_at)?;
        let swapped = self
            .kv
            .compare_and_swap(key, version, Some(encode(key, &record)?), Expiry::Keep)
            .await?;
        Ok(swapped.is_some())
    }

    async fn read_record(&self, key: &StoreKey) -> Result<Option<(Record, u64)>> {
        let Some(v) = self.kv.get(key).await? else {
            return Ok(None);
        };
        let record: Record = decode(key, &v)?;
        Ok(Some((record, v.version)))
    }

    /// Remove `phone`'s index entry if (and only if) it still points to
    /// `waba_id`; compare-and-swap so a concurrent re-link is not undone.
    async fn unlink_phone(&self, phone: &PhoneNumberId, waba_id: &WabaId) -> Result<()> {
        let pkey = phone_key(phone);
        let Some(v) = self.kv.get(&pkey).await? else {
            return Ok(());
        };
        let points_here =
            serde_json::from_slice::<PhoneIndex>(&v.value).is_ok_and(|i| &i.waba_id == waba_id);
        if points_here {
            self.kv
                .compare_and_swap(&pkey, v.version, None, Expiry::Keep)
                .await?;
        }
        Ok(())
    }

    fn seal(&self, token: &StoredBusinessToken, created_at: OffsetDateTime) -> Result<Record> {
        let key = &self.keys.active;
        let sealed = Sealed {
            access_token: token.token.expose_secret().to_owned(),
            waba_id: token.waba_id.clone(),
            business_id: token.business_id.clone(),
            phone_number_ids: token.phone_number_ids.clone(),
            created_at,
            expires_at: token.expires_at,
        };
        let plaintext = serde_json::to_vec(&sealed).map_err(|_| CryptoError::Encrypt);
        let mut access_token = sealed.access_token;
        wipe_string(&mut access_token);
        let mut plaintext = plaintext?;
        let mut nonce = [0u8; NONCE_LEN];
        let encrypted = getrandom::fill(&mut nonce)
            .map_err(|_| CryptoError::Encrypt)
            .and_then(|()| key.cipher())
            .and_then(|cipher| {
                cipher
                    .encrypt(
                        &Nonce::from(nonce),
                        Payload {
                            msg: &plaintext,
                            aad: &aad(&token.waba_id, &key.id),
                        },
                    )
                    .map_err(|_| CryptoError::Encrypt)
            });
        wipe(&mut plaintext);
        let ciphertext = encrypted?;
        Ok(Record {
            v: RECORD_VERSION,
            kid: key.id.clone(),
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(ciphertext),
            waba_id: token.waba_id.clone(),
            business_id: token.business_id.clone(),
            phone_number_ids: token.phone_number_ids.clone(),
            created_at,
            expires_at: token.expires_at,
        })
    }

    fn open(&self, waba_id: &WabaId, record: &Record) -> Result<StoredBusinessToken> {
        if record.v != RECORD_VERSION {
            return Err(CryptoError::Malformed("unsupported token vault record version").into());
        }
        let key = self.keys.find(&record.kid).ok_or(CryptoError::InvalidKey(
            "the record was encrypted with a key id that is not configured",
        ))?;
        let nonce: [u8; NONCE_LEN] = B64
            .decode(&record.nonce)
            .ok()
            .and_then(|n| <[u8; NONCE_LEN]>::try_from(n.as_slice()).ok())
            .ok_or(CryptoError::Malformed("nonce"))?;
        let ciphertext = B64
            .decode(&record.ciphertext)
            .map_err(|_| CryptoError::Malformed("ciphertext"))?;
        let mut plaintext = key
            .cipher()?
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad(waba_id, &record.kid),
                },
            )
            .map_err(|_| CryptoError::Decrypt)?;
        let sealed = serde_json::from_slice::<Sealed>(&plaintext);
        wipe(&mut plaintext);
        let sealed = sealed.map_err(|_| CryptoError::Malformed("decrypted record"))?;
        if &sealed.waba_id != waba_id {
            // Unreachable while the AAD binds the WABA id; kept as a second
            // line of defence should the AAD format ever change.
            return Err(CryptoError::Decrypt.into());
        }
        Ok(StoredBusinessToken {
            waba_id: sealed.waba_id,
            token: AccessToken::new(sealed.access_token),
            business_id: sealed.business_id,
            phone_number_ids: sealed.phone_number_ids,
            created_at: Some(sealed.created_at),
            expires_at: sealed.expires_at,
        })
    }
}

fn waba_key(waba_id: &WabaId) -> StoreKey {
    StoreKey::new(TOKEN_NAMESPACE, format!("waba/{waba_id}"))
}

fn phone_key(phone_number_id: &PhoneNumberId) -> StoreKey {
    StoreKey::new(TOKEN_NAMESPACE, format!("phone/{phone_number_id}"))
}

/// `tag || len(ns) ns || len(waba) waba || len(kid) kid`, lengths as u64 BE.
fn aad(waba_id: &WabaId, kid: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(AAD_TAG.len() + 64);
    out.extend_from_slice(AAD_TAG);
    for part in [TOKEN_NAMESPACE, waba_id.as_str(), kid] {
        let len = u64::try_from(part.len()).unwrap_or(u64::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(part.as_bytes());
    }
    out
}

fn encode<T: Serialize>(key: &StoreKey, value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|source| {
        Error::from(StorageError::Corrupt {
            key: key.to_string(),
            source,
        })
    })
}

fn decode<T: serde::de::DeserializeOwned>(key: &StoreKey, v: &Versioned) -> Result<T> {
    serde_json::from_slice(&v.value).map_err(|source| {
        Error::from(StorageError::Corrupt {
            key: key.to_string(),
            source,
        })
    })
}

/// Overwrite a buffer that held secret material. `black_box` keeps the
/// compiler from treating the writes as dead stores.
fn wipe(buf: &mut [u8]) {
    buf.fill(0);
    std::hint::black_box(&*buf);
}

fn wipe_string(s: &mut String) {
    let mut bytes = std::mem::take(s).into_bytes();
    wipe(&mut bytes);
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use time::macros::datetime;
    use wa_adapters::store::MemoryKvStore;
    use wa_core::clock::ManualClock;

    use super::*;

    const TOKEN: &str = "EAAAN6tcBzAUBOwtDtTfmZCJ9n3FHpSDcDTH86ekf89XnnMZAtaitMUysPDE7LES3C";

    fn kv() -> Arc<dyn KvStore> {
        Arc::new(MemoryKvStore::new())
    }

    fn key(id: &str, byte: u8) -> VaultKey {
        VaultKey::new(id, [byte; 32]).unwrap()
    }

    fn vault(kv: &Arc<dyn KvStore>, keys: VaultKeys) -> TokenVault {
        TokenVault::new(Arc::clone(kv), keys)
            .unwrap()
            .with_clock(Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC))))
    }

    fn sample(waba: &str) -> StoredBusinessToken {
        StoredBusinessToken::new(waba, AccessToken::new(format!("{TOKEN}-{waba}")))
            .business_id("2729063490586005")
            .phone_number_ids(["106540352242922"])
            .expires_at(datetime!(2026-11-23 12:00 UTC))
    }

    async fn raw(kv: &Arc<dyn KvStore>, k: &str) -> Option<Versioned> {
        kv.get(&StoreKey::new(TOKEN_NAMESPACE, k)).await.unwrap()
    }

    async fn raw_json(kv: &Arc<dyn KvStore>, k: &str) -> serde_json::Value {
        serde_json::from_slice(&raw(kv, k).await.unwrap().value).unwrap()
    }

    async fn put_json(kv: &Arc<dyn KvStore>, k: &str, v: &serde_json::Value) {
        kv.put(
            &StoreKey::new(TOKEN_NAMESPACE, k),
            serde_json::to_vec(v).unwrap(),
            Expiry::Never,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn round_trip_encrypts_at_rest() {
        let kv = kv();
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        v.store(&sample("W1")).await.unwrap();

        let got = v.get(&WabaId::new("W1")).await.unwrap().unwrap();
        assert_eq!(got.token.expose_secret(), format!("{TOKEN}-W1"));
        assert_eq!(got.business_id, Some(BusinessId::new("2729063490586005")));
        assert_eq!(
            got.phone_number_ids,
            vec![PhoneNumberId::new("106540352242922")]
        );
        assert_eq!(got.created_at, Some(datetime!(2026-09-24 12:00 UTC)));
        assert_eq!(got.expires_at, Some(datetime!(2026-11-23 12:00 UTC)));

        let bytes = raw(&kv, "waba/W1").await.unwrap().value;
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains(TOKEN), "token stored in the clear: {text}");
        let rec: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(rec["kid"], "k1");
        assert_eq!(rec["v"], 1);
        assert_eq!(rec["waba_id"], "W1");
        assert_eq!(rec["business_id"], "2729063490586005");
        assert_eq!(
            rec["phone_number_ids"],
            serde_json::json!(["106540352242922"])
        );
        assert_eq!(
            rec["created_at"],
            datetime!(2026-09-24 12:00 UTC).unix_timestamp()
        );
        assert_eq!(
            B64.decode(rec["nonce"].as_str().unwrap()).unwrap().len(),
            12
        );
        // Debug output never carries the token.
        let dbg = format!("{got:?} {v:?}");
        assert!(!dbg.contains(TOKEN), "{dbg}");
        assert_eq!(
            v.get(&WabaId::new("nope"))
                .await
                .unwrap()
                .map(|t| t.waba_id),
            None
        );
    }

    #[tokio::test]
    async fn every_write_uses_a_fresh_nonce() {
        let kv = kv();
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        v.store(&sample("W1")).await.unwrap();
        let a = raw_json(&kv, "waba/W1").await;
        v.store(&sample("W1")).await.unwrap();
        let b = raw_json(&kv, "waba/W1").await;
        assert_ne!(a["nonce"], b["nonce"]);
        assert_ne!(a["ciphertext"], b["ciphertext"]);
    }

    #[tokio::test]
    async fn wrong_key_fails_and_unknown_key_id_is_reported() {
        let kv = kv();
        vault(&kv, VaultKeys::new(key("k1", 7)))
            .store(&sample("W1"))
            .await
            .unwrap();
        let wrong = vault(&kv, VaultKeys::new(key("k1", 8)));
        assert!(matches!(
            wrong.get(&WabaId::new("W1")).await,
            Err(Error::Crypto(CryptoError::Decrypt))
        ));
        let other = vault(&kv, VaultKeys::new(key("k2", 7)));
        assert!(matches!(
            other.get(&WabaId::new("W1")).await,
            Err(Error::Crypto(CryptoError::InvalidKey(_)))
        ));
    }

    #[tokio::test]
    async fn tampering_is_detected() {
        let kv = kv();
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        v.store(&sample("W1")).await.unwrap();
        let original = raw_json(&kv, "waba/W1").await;

        let mut ct = B64
            .decode(original["ciphertext"].as_str().unwrap())
            .unwrap();
        ct[3] ^= 0x01;
        let mut rec = original.clone();
        rec["ciphertext"] = B64.encode(&ct).into();
        put_json(&kv, "waba/W1", &rec).await;
        assert!(matches!(
            v.get(&WabaId::new("W1")).await,
            Err(Error::Crypto(CryptoError::Decrypt))
        ));

        let mut nonce = B64.decode(original["nonce"].as_str().unwrap()).unwrap();
        nonce[0] ^= 0x80;
        let mut rec = original.clone();
        rec["nonce"] = B64.encode(&nonce).into();
        put_json(&kv, "waba/W1", &rec).await;
        assert!(matches!(
            v.get(&WabaId::new("W1")).await,
            Err(Error::Crypto(CryptoError::Decrypt))
        ));

        // Relabelling the key id is caught by the associated data.
        let v2 = vault(
            &kv,
            VaultKeys::new(key("k2", 7)).with_previous(key("k1", 7)),
        );
        let mut rec = original.clone();
        rec["kid"] = "k2".into();
        put_json(&kv, "waba/W1", &rec).await;
        assert!(matches!(
            v2.get(&WabaId::new("W1")).await,
            Err(Error::Crypto(CryptoError::Decrypt))
        ));

        let mut rec = original.clone();
        rec["nonce"] = "not base64!".into();
        put_json(&kv, "waba/W1", &rec).await;
        assert!(matches!(
            v.get(&WabaId::new("W1")).await,
            Err(Error::Crypto(CryptoError::Malformed(_)))
        ));

        put_json(&kv, "waba/W1", &serde_json::json!({"oops": true})).await;
        assert!(matches!(
            v.get(&WabaId::new("W1")).await,
            Err(Error::Storage(StorageError::Corrupt { .. }))
        ));
    }

    #[tokio::test]
    async fn clear_metadata_edits_do_not_change_what_is_returned() {
        let kv = kv();
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        v.store(&sample("W1")).await.unwrap();
        let mut rec = raw_json(&kv, "waba/W1").await;
        rec["phone_number_ids"] = serde_json::json!(["999"]);
        rec["business_id"] = "evil".into();
        put_json(&kv, "waba/W1", &rec).await;
        let got = v.get(&WabaId::new("W1")).await.unwrap().unwrap();
        assert_eq!(
            got.phone_number_ids,
            vec![PhoneNumberId::new("106540352242922")]
        );
        assert_eq!(got.business_id, Some(BusinessId::new("2729063490586005")));
    }

    #[tokio::test]
    async fn a_record_copied_to_another_waba_does_not_decrypt() {
        let kv = kv();
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        v.store(&sample("W1")).await.unwrap();
        v.store(&sample("W2")).await.unwrap();
        // Attacker with write access to the store copies A's record over B's,
        // and even fixes up the clear waba_id.
        let mut stolen = raw_json(&kv, "waba/W1").await;
        stolen["waba_id"] = "W2".into();
        put_json(&kv, "waba/W2", &stolen).await;
        assert!(matches!(
            v.get(&WabaId::new("W2")).await,
            Err(Error::Crypto(CryptoError::Decrypt))
        ));
        // W1 is untouched.
        assert!(v.get(&WabaId::new("W1")).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn associated_data_alone_binds_the_waba() {
        // A record whose sealed payload says W2 but whose associated data was
        // computed for W1: only the AAD check can reject it.
        let kv = kv();
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        let k = key("k1", 7);
        let sealed = Sealed {
            access_token: TOKEN.to_owned(),
            waba_id: WabaId::new("W2"),
            business_id: None,
            phone_number_ids: Vec::new(),
            created_at: datetime!(2026-09-24 12:00 UTC),
            expires_at: None,
        };
        let plaintext = serde_json::to_vec(&sealed).unwrap();
        let nonce = [5u8; NONCE_LEN];
        let ciphertext = k
            .cipher()
            .unwrap()
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad(&WabaId::new("W1"), "k1"),
                },
            )
            .unwrap();
        let record = serde_json::json!({
            "v": 1, "kid": "k1", "nonce": B64.encode(nonce), "ciphertext": B64.encode(ciphertext),
            "waba_id": "W2", "phone_number_ids": [], "created_at": 1790251200
        });
        put_json(&kv, "waba/W2", &record).await;
        assert!(matches!(
            v.get(&WabaId::new("W2")).await,
            Err(Error::Crypto(CryptoError::Decrypt))
        ));
        // Same record under the WABA the AAD names: the sealed id check
        // rejects it instead.
        put_json(&kv, "waba/W1", &record).await;
        assert!(matches!(
            v.get(&WabaId::new("W1")).await,
            Err(Error::Crypto(CryptoError::Decrypt))
        ));
    }

    #[tokio::test]
    async fn rotation_on_read_and_explicit() {
        let kv = kv();
        vault(&kv, VaultKeys::new(key("old", 1)))
            .store(&sample("W1"))
            .await
            .unwrap();
        vault(&kv, VaultKeys::new(key("old", 1)))
            .store(&sample("W2"))
            .await
            .unwrap();

        // Explicit rotation, with read-time rotation off.
        let manual = vault(
            &kv,
            VaultKeys::new(key("new", 2)).with_previous(key("old", 1)),
        )
        .rotate_on_read(false);
        let got = manual.get(&WabaId::new("W1")).await.unwrap().unwrap();
        assert_eq!(got.token.expose_secret(), format!("{TOKEN}-W1"));
        assert_eq!(raw_json(&kv, "waba/W1").await["kid"], "old", "no rewrite");
        assert!(manual.rotate(&WabaId::new("W1")).await.unwrap());
        assert_eq!(raw_json(&kv, "waba/W1").await["kid"], "new");
        assert!(
            !manual.rotate(&WabaId::new("W1")).await.unwrap(),
            "already current"
        );
        assert!(!manual.rotate(&WabaId::new("missing")).await.unwrap());

        // Read-time rotation.
        let auto = vault(
            &kv,
            VaultKeys::new(key("new", 2)).with_previous(key("old", 1)),
        );
        auto.get(&WabaId::new("W2")).await.unwrap().unwrap();
        assert_eq!(raw_json(&kv, "waba/W2").await["kid"], "new");

        // The old key can now be dropped.
        let only_new = vault(&kv, VaultKeys::new(key("new", 2)));
        for w in ["W1", "W2"] {
            let t = only_new.get(&WabaId::new(w)).await.unwrap().unwrap();
            assert_eq!(t.token.expose_secret(), format!("{TOKEN}-{w}"));
            assert_eq!(
                t.created_at,
                Some(datetime!(2026-09-24 12:00 UTC)),
                "created_at kept"
            );
        }
    }

    #[tokio::test]
    async fn phone_index_follows_the_records() {
        let kv = kv();
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        let p1 = PhoneNumberId::new("P1");
        let p2 = PhoneNumberId::new("P2");
        v.store(&sample("W1").phone_number_ids(["P1", "P2"]))
            .await
            .unwrap();
        assert_eq!(
            v.get_by_phone_number(&p2).await.unwrap().unwrap().waba_id,
            WabaId::new("W1")
        );

        // P2 moves out of W1: its index entry goes away.
        v.store(&sample("W1").phone_number_ids(["P1"]))
            .await
            .unwrap();
        assert!(v.get_by_phone_number(&p2).await.unwrap().is_none());
        assert!(raw(&kv, "phone/P2").await.is_none());

        // P1 moves to W2: W1's later cleanup must not remove W2's link.
        v.store(&sample("W2").phone_number_ids(["P1"]))
            .await
            .unwrap();
        assert_eq!(
            v.get_by_phone_number(&p1).await.unwrap().unwrap().waba_id,
            WabaId::new("W2")
        );
        assert!(v.delete(&WabaId::new("W1")).await.unwrap());
        assert_eq!(
            v.get_by_phone_number(&p1).await.unwrap().unwrap().waba_id,
            WabaId::new("W2")
        );

        // A forged index entry pointing at a WABA that does not list the
        // number yields nothing.
        put_json(&kv, "phone/P9", &serde_json::json!({"waba_id": "W2"})).await;
        assert!(
            v.get_by_phone_number(&PhoneNumberId::new("P9"))
                .await
                .unwrap()
                .is_none()
        );

        assert!(v.delete(&WabaId::new("W2")).await.unwrap());
        assert!(v.get_by_phone_number(&p1).await.unwrap().is_none());
        assert!(raw(&kv, "phone/P1").await.is_none());
        assert!(!v.delete(&WabaId::new("W2")).await.unwrap());
    }

    #[tokio::test]
    async fn store_rejects_empty_input() {
        let kv = kv();
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        assert!(
            v.store(&StoredBusinessToken::new("W", AccessToken::new("")))
                .await
                .is_err()
        );
        assert!(
            v.store(&StoredBusinessToken::new("", AccessToken::new("t")))
                .await
                .is_err()
        );
    }

    #[test]
    fn keys_validate_and_never_print() {
        let k = VaultKey::from_base64("2026-09", &B64.encode([9u8; 32])).unwrap();
        assert_eq!(k.id(), "2026-09");
        assert_eq!(
            format!("{k:?}"),
            r#"VaultKey { id: "2026-09", key: "[REDACTED]" }"#
        );
        assert!(matches!(
            VaultKey::from_base64("k", &B64.encode([9u8; 31])),
            Err(Error::Crypto(CryptoError::InvalidKey(_)))
        ));
        assert!(VaultKey::from_base64("k", "***").is_err());
        assert!(VaultKey::new("", [0; 32]).is_err());
        assert!(VaultKey::new("has space", [0; 32]).is_err());
        assert!(VaultKey::new("x".repeat(65), [0; 32]).is_err());
        let a = VaultKey::generate("a").unwrap();
        let b = VaultKey::generate("b").unwrap();
        assert_ne!(a.key, b.key);
        assert!(
            TokenVault::new(
                kv(),
                VaultKeys::new(key("same", 1)).with_previous(key("same", 2))
            )
            .is_err()
        );
    }

    #[test]
    fn associated_data_is_unambiguous() {
        assert_ne!(aad(&WabaId::new("a"), "bc"), aad(&WabaId::new("ab"), "c"));
    }
}
