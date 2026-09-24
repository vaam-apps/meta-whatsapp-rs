//! `TokenVault`: business tokens encrypted at rest on any [`KvStore`].
//!
//! # Format
//!
//! Namespace `wa.token`. Two kinds of keys:
//!
//! - `waba/<WABA_ID>` → a JSON record
//!   `{v, kid, nonce, ciphertext, waba_id, business_id?, phone_number_ids,
//!   created_at, expires_at?, allocation_config_id?}`. `nonce` and
//!   `ciphertext` are standard base64; timestamps are unix seconds.
//!   `allocation_config_id` (Solution Partner onboarding only: the credit
//!   line's allocation for this WABA) is omitted when unset, so a Tech
//!   Provider record is written exactly as before it existed.
//! - `phone/<PHONE_NUMBER_ID>` → `{"waba_id": …}`, the index behind
//!   [`TokenVault::get_by_phone_number`] (webhooks carry the phone number
//!   id; this finds the token to answer with). Keyed by Meta's phone number
//!   *id*, never by a phone number, so there is no E.164 normalisation to get
//!   wrong.
//!
//! # Cryptography, and why each choice
//!
//! - **AES-256-GCM**, a fresh random 96-bit nonce per write (`getrandom`).
//!   With random nonces the safe budget is about 2³² writes per key; token
//!   writes happen once per onboarding, so rotating keys yearly is far
//!   inside it. A failing OS random number generator is
//!   [`CryptoError::Rng`]: nothing is written rather than reusing a nonce.
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
//! - **The phone index is a hint, the record is the authority.**
//!   [`TokenVault::get_by_phone_number`] only returns a token whose
//!   authenticated record lists the number, so a stale or forged index entry
//!   yields `None`, never another WABA's token.
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
//! Anyone holding the vault key and the store. Key bytes live in
//! [`SecretBytes`] (zeroed on drop, never printed); so do the serialized
//! plaintexts this module builds. Copies made outside this module are not
//! covered: the token string inside [`AccessToken`] once handed out, and
//! the HTTP response body the token first arrived in. (The AES key schedule
//! is wiped too: the workspace builds `aes-gcm` with its `zeroize` feature.)
//!
//! # Who may write the index
//!
//! [`TokenVault::store`] trusts its input: every phone number id in the
//! record is routed to that WABA. [`EmbeddedSignup::onboard`](super::EmbeddedSignup::onboard)
//! only stores numbers Meta listed on the WABA with the business token. If
//! you call `store` yourself, do the same, or one merchant's customers can be
//! routed to another merchant.

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
use wa_core::ids::{AllocationConfigId, BusinessId, PhoneNumberId, WabaId};
use wa_core::secret::{AccessToken, SecretBytes};
use wa_core::store::{Expiry, KvStore, StoreKey, Versioned};
use wa_core::{Error, Result};

/// `KvStore` namespace of the vault.
pub const TOKEN_NAMESPACE: &str = "wa.token";

/// Domain separation for the associated data; bump with the record format.
const AAD_TAG: &[u8] = b"wa-rs/token-vault/v1";
const RECORD_VERSION: u8 = 1;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const MAX_KEY_ID_LEN: usize = 64;

/// A 256-bit AES key and the id recorded next to what it encrypts.
///
/// The bytes are held in [`SecretBytes`]: zeroed on drop, never printed
/// (`Debug` shows the id only).
pub struct VaultKey {
    id: String,
    key: SecretBytes,
}

impl VaultKey {
    /// Build a key from exactly 32 secret bytes. `id` is 1–64 characters of
    /// `A-Z a-z 0-9 - _ . :` (it is stored in every record and bound into
    /// the ciphertext).
    pub fn new(id: impl Into<String>, key: SecretBytes) -> Result<Self> {
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
        if key.len() != KEY_LEN {
            return Err(CryptoError::InvalidKey("vault key must be exactly 32 bytes").into());
        }
        Ok(Self { id, key })
    }

    /// Decode a key from standard base64 (e.g. from a secret manager or an
    /// environment variable). It must decode to exactly 32 bytes.
    pub fn from_base64(id: impl Into<String>, encoded: &str) -> Result<Self> {
        let bytes = B64
            .decode(encoded.trim())
            .map_err(|_| CryptoError::InvalidKey("vault key is not valid base64"))?;
        Self::new(id, SecretBytes::new(bytes))
    }

    /// A fresh random key from the operating system's CSPRNG.
    pub fn generate(id: impl Into<String>) -> Result<Self> {
        let mut key = vec![0u8; KEY_LEN];
        let filled = getrandom::fill(&mut key);
        // Wrap before checking so the buffer is zeroed on either path.
        let key = SecretBytes::new(key);
        filled.map_err(|_| CryptoError::Rng)?;
        Self::new(id, key)
    }

    /// The key id.
    pub fn id(&self) -> &str {
        &self.id
    }

    fn cipher(&self) -> Result<Aes256Gcm, CryptoError> {
        Aes256Gcm::new_from_slice(self.key.expose_secret())
            .map_err(|_| CryptoError::InvalidKey("vault key"))
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
    /// The customer's business portfolio (the WABA's owner), when known.
    pub business_id: Option<BusinessId>,
    /// Phone numbers indexed to this WABA. [`EmbeddedSignup::onboard`](super::EmbeddedSignup::onboard)
    /// puts the onboarded number first, then the WABA's other numbers.
    pub phone_number_ids: Vec<PhoneNumberId>,
    /// When the record was first written; set by [`TokenVault::store`] when
    /// `None`.
    pub created_at: Option<OffsetDateTime>,
    /// When the token expires, if it does.
    pub expires_at: Option<OffsetDateTime>,
    /// The partner's credit line allocation that funds this WABA, recorded
    /// by Solution Partner onboarding (`share_credit_line`). `None` for a
    /// Tech Provider.
    pub allocation_config_id: Option<AllocationConfigId>,
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
            allocation_config_id: None,
        }
    }

    /// Set the business portfolio.
    #[must_use]
    pub fn business_id(mut self, business_id: impl Into<BusinessId>) -> Self {
        self.business_id = Some(business_id.into());
        self
    }

    /// Set the phone numbers to index (see [`TokenVault`] on who may write
    /// the index).
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

    /// Set the credit line allocation that funds the WABA.
    #[must_use]
    pub fn allocation_config_id(mut self, id: impl Into<AllocationConfigId>) -> Self {
        self.allocation_config_id = Some(id.into());
        self
    }

    /// Whether the token has expired at `now`.
    pub fn is_expired(&self, now: OffsetDateTime) -> bool {
        self.expires_at.is_some_and(|t| t <= now)
    }
}

/// Encrypted business token storage, keyed by WABA, with a phone number
/// index. Cheap to clone.
///
/// - **At rest**: AES-256-GCM under the active [`VaultKey`], a fresh random
///   96-bit nonce per write, the key id recorded; the associated data binds
///   each ciphertext to the vault namespace, the WABA id it is stored under
///   and the key id, so a record copied or swapped between two tenants'
///   keys fails to decrypt ([`CryptoError::Decrypt`]) instead of handing one
///   tenant the other's token. The stored bytes never contain the token.
/// - **Rotation**: records under a previous key (kept with
///   [`VaultKeys::with_previous`]) still decrypt, and are re-encrypted under
///   the active key on read (see [`Self::rotate_on_read`]) or by
///   [`Self::rotate`].
/// - **Routing**: [`Self::get_by_phone_number`] returns a token only if the
///   WABA's *authenticated* record lists the number; the index itself is a
///   hint, so a stale or forged index entry yields `None`.
///
/// # Who may write the index
///
/// [`Self::store`] trusts its input: every phone number id in the record is
/// routed to that WABA. [`EmbeddedSignup::onboard`](super::EmbeddedSignup::onboard)
/// only stores numbers Meta listed on the WABA to the business token. If you
/// call `store` yourself, do the same, or one merchant's customers can be
/// routed to another merchant.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    allocation_config_id: Option<AllocationConfigId>,
}

/// The encrypted part, as written: borrows the token instead of copying it
/// into a `String` that would outlive the call un-zeroed.
#[derive(Serialize)]
struct SealedRef<'a> {
    access_token: &'a str,
    waba_id: &'a WabaId,
    business_id: Option<&'a BusinessId>,
    phone_number_ids: &'a [PhoneNumberId],
    #[serde(with = "time::serde::timestamp")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::timestamp::option")]
    expires_at: Option<OffsetDateTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allocation_config_id: Option<&'a AllocationConfigId>,
}

/// The encrypted part, as read back. The token moves straight into an
/// [`AccessToken`].
#[derive(Deserialize)]
#[cfg_attr(test, derive(Serialize))]
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
    #[serde(default)]
    allocation_config_id: Option<AllocationConfigId>,
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
    ///
    /// Trusts `token.phone_number_ids`: see [`TokenVault`] on who may write
    /// the index.
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
            // The kind only: a backend error's text is not ours to vouch for.
            tracing::warn!(
                waba_id = %waba_id,
                kind = ?e.kind(),
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
        let plaintext = serde_json::to_vec(&SealedRef {
            access_token: token.token.expose_secret(),
            waba_id: &token.waba_id,
            business_id: token.business_id.as_ref(),
            phone_number_ids: &token.phone_number_ids,
            created_at,
            expires_at: token.expires_at,
            allocation_config_id: token.allocation_config_id.as_ref(),
        })
        .map(SecretBytes::new)
        .map_err(|_| CryptoError::Encrypt)?;
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| CryptoError::Rng)?;
        let ciphertext = key
            .cipher()?
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: plaintext.expose_secret(),
                    aad: &aad(&token.waba_id, &key.id),
                },
            )
            .map_err(|_| CryptoError::Encrypt)?;
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
            allocation_config_id: token.allocation_config_id.clone(),
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
        let plaintext = key
            .cipher()?
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad(waba_id, &record.kid),
                },
            )
            .map(SecretBytes::new)
            .map_err(|_| CryptoError::Decrypt)?;
        let sealed = serde_json::from_slice::<Sealed>(plaintext.expose_secret())
            .map_err(|_| CryptoError::Malformed("decrypted record"))?;
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
            allocation_config_id: sealed.allocation_config_id,
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

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use pretty_assertions::assert_eq;
    use time::macros::datetime;
    use wa_adapters::store::MemoryKvStore;
    use wa_core::clock::ManualClock;

    use super::*;

    const TOKEN: &str = "EAAAN6tcBzAUBOwtDtTfmZCJ9n3FHpSDcDTH86ekf89XnnMZAtaitMUysPDE7LES3C";

    /// A `KvStore` that remembers every key and value ever written to it,
    /// so a test can scan the raw bytes for secrets.
    #[derive(Debug, Default)]
    pub(crate) struct RecordingKv {
        inner: MemoryKvStore,
        writes: Mutex<Vec<(String, Vec<u8>)>>,
    }

    impl RecordingKv {
        fn record(&self, key: &StoreKey, value: &[u8]) {
            self.writes
                .lock()
                .unwrap()
                .push((key.to_string(), value.to_vec()));
        }

        /// Panic if any key or value ever written contains `secret`, raw or
        /// base64-encoded (standard or URL-safe).
        pub(crate) fn assert_never_wrote(&self, secret: &str) {
            let needles = [
                secret.as_bytes().to_vec(),
                B64.encode(secret).into_bytes(),
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(secret)
                    .into_bytes(),
            ];
            let writes = self.writes.lock().unwrap();
            assert!(!writes.is_empty(), "nothing was written: vacuous check");
            for (key, value) in writes.iter() {
                for needle in &needles {
                    assert!(
                        !contains(key.as_bytes(), needle) && !contains(value, needle),
                        "secret written to the store under `{key}`"
                    );
                }
            }
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[async_trait::async_trait]
    impl KvStore for RecordingKv {
        async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
            self.inner.get(key).await
        }
        async fn put(
            &self,
            key: &StoreKey,
            value: Vec<u8>,
            expiry: Expiry,
        ) -> Result<u64, StorageError> {
            self.record(key, &value);
            self.inner.put(key, value, expiry).await
        }
        async fn put_if_absent(
            &self,
            key: &StoreKey,
            value: Vec<u8>,
            expiry: Expiry,
        ) -> Result<Option<u64>, StorageError> {
            self.record(key, &value);
            self.inner.put_if_absent(key, value, expiry).await
        }
        async fn compare_and_swap(
            &self,
            key: &StoreKey,
            expected: u64,
            new: Option<Vec<u8>>,
            expiry: Expiry,
        ) -> Result<Option<u64>, StorageError> {
            if let Some(v) = &new {
                self.record(key, v);
            }
            self.inner
                .compare_and_swap(key, expected, new, expiry)
                .await
        }
        async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
            self.inner.delete(key).await
        }
    }

    fn kv() -> Arc<dyn KvStore> {
        Arc::new(MemoryKvStore::new())
    }

    fn key(id: &str, byte: u8) -> VaultKey {
        VaultKey::new(id, SecretBytes::new([byte; 32])).unwrap()
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

    /// The Solution Partner allocation id is sealed like the rest, the
    /// clear copy is only for operators, and a record without one (a Tech
    /// Provider's, or any written before the field existed) is unchanged.
    #[tokio::test]
    async fn the_allocation_id_is_sealed_and_optional() {
        let kv = kv();
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        v.store(&sample("W1").allocation_config_id("58501441721238"))
            .await
            .unwrap();
        let got = v.get(&WabaId::new("W1")).await.unwrap().unwrap();
        assert_eq!(
            got.allocation_config_id,
            Some(AllocationConfigId::new("58501441721238"))
        );
        let mut rec = raw_json(&kv, "waba/W1").await;
        assert_eq!(rec["allocation_config_id"], "58501441721238");
        rec["allocation_config_id"] = "EDITED".into();
        put_json(&kv, "waba/W1", &rec).await;
        assert_eq!(
            v.get(&WabaId::new("W1"))
                .await
                .unwrap()
                .unwrap()
                .allocation_config_id,
            Some(AllocationConfigId::new("58501441721238")),
            "the authenticated copy wins"
        );

        v.store(&sample("W2")).await.unwrap();
        let rec = raw_json(&kv, "waba/W2").await;
        assert!(
            rec.get("allocation_config_id").is_none(),
            "no key when unset: {rec}"
        );
        assert_eq!(
            v.get(&WabaId::new("W2"))
                .await
                .unwrap()
                .unwrap()
                .allocation_config_id,
            None
        );

        // A sealed payload written before the field existed still opens.
        let k = key("k1", 7);
        let plaintext = serde_json::to_vec(&serde_json::json!({
            "access_token": TOKEN, "waba_id": "W3", "business_id": null,
            "phone_number_ids": [], "created_at": 1790251200, "expires_at": null
        }))
        .unwrap();
        let nonce = [6u8; NONCE_LEN];
        let ciphertext = k
            .cipher()
            .unwrap()
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad(&WabaId::new("W3"), "k1"),
                },
            )
            .unwrap();
        let record = serde_json::json!({
            "v": 1, "kid": "k1", "nonce": B64.encode(nonce), "ciphertext": B64.encode(ciphertext),
            "waba_id": "W3", "phone_number_ids": [], "created_at": 1790251200
        });
        put_json(&kv, "waba/W3", &record).await;
        let old = v.get(&WabaId::new("W3")).await.unwrap().unwrap();
        assert_eq!(old.token.expose_secret(), TOKEN);
        assert_eq!(old.allocation_config_id, None);
    }

    /// A record exactly as the vault wrote it before Solution Partner mode
    /// (b805dac: `sample("W1")` under `key("k1", 7)`, bytes captured from
    /// that build), so a change to the record format, the sealed payload or
    /// the AAD that would strand stored tokens fails here. The fixture above
    /// seals with today's code and cannot catch that.
    #[tokio::test]
    async fn a_record_written_before_solution_partner_mode_still_opens() {
        const B805DAC_RECORD: &str = r#"{"v":1,"kid":"k1","nonce":"yjeUqJLkV07hoZVu","ciphertext":"FZCl/j7gKKrv9admo8XlB0Jds7ooceKrFI4SIHOEec3+9m1GTwPglmmmh/UXCWBtVEvbGeBqnXZasSWTTCN+ocJrFXji1Hhhz5owLB5KfRc9+m1P/zxWCkiGVFVPZBorFt4K48bE5X8+vIIZdnEx8Odk6tpsCQFN4sJ8PAA1t5XaUbM9GOBev/Ju557PfGTT4MlNsz7EqA23me/ifd2TFX8YVitV+MGYh7d3iZWYkr1Ty2SK3rjA89KsA7oT56tox3MtbAnJe7OUha2wjGUrOZZIfNR95w0TfMkrUtTk+OUcP/+1QPR8JrFL7oa30y0=","waba_id":"W1","business_id":"2729063490586005","phone_number_ids":["106540352242922"],"created_at":1790251200,"expires_at":1795435200}"#;
        let kv = kv();
        kv.put(
            &StoreKey::new(TOKEN_NAMESPACE, "waba/W1"),
            B805DAC_RECORD.as_bytes().to_vec(),
            Expiry::Never,
        )
        .await
        .unwrap();
        put_json(
            &kv,
            "phone/106540352242922",
            &serde_json::json!({"waba_id": "W1"}),
        )
        .await;
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        let got = v.get(&WabaId::new("W1")).await.unwrap().unwrap();
        assert_eq!(got.token.expose_secret(), format!("{TOKEN}-W1"));
        assert_eq!(got.business_id, Some(BusinessId::new("2729063490586005")));
        assert_eq!(
            got.phone_number_ids,
            [PhoneNumberId::new("106540352242922")]
        );
        assert_eq!(got.created_at, Some(datetime!(2026-09-24 12:00 UTC)));
        assert_eq!(got.expires_at, Some(datetime!(2026-11-23 12:00 UTC)));
        let by_phone = v
            .get_by_phone_number(&PhoneNumberId::new("106540352242922"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_phone.token.expose_secret(), format!("{TOKEN}-W1"));
        // Not rewritten on read: the active key is the one it was sealed with.
        assert_eq!(
            raw(&kv, "waba/W1").await.unwrap().value,
            B805DAC_RECORD.as_bytes()
        );
    }

    #[tokio::test]
    async fn no_write_ever_contains_the_token() {
        // Every path that writes: store (record + index), re-store with a
        // dropped number (unlink), rotation on read, explicit rotation.
        let rec = Arc::new(RecordingKv::default());
        let kv: Arc<dyn KvStore> = rec.clone();
        let old = vault(&kv, VaultKeys::new(key("old", 1)));
        old.store(&sample("W1").phone_number_ids(["P1", "P2"]))
            .await
            .unwrap();
        old.store(&sample("W1").phone_number_ids(["P1"]))
            .await
            .unwrap();
        old.store(&sample("W2")).await.unwrap();
        let new = vault(
            &kv,
            VaultKeys::new(key("new", 2)).with_previous(key("old", 1)),
        );
        new.get(&WabaId::new("W1")).await.unwrap().unwrap();
        assert!(new.rotate(&WabaId::new("W2")).await.unwrap());
        assert_eq!(raw_json(&kv, "waba/W1").await["kid"], "new");
        for waba in ["W1", "W2"] {
            rec.assert_never_wrote(&format!("{TOKEN}-{waba}"));
        }
        // The common prefix alone, too: no partial leak.
        rec.assert_never_wrote(TOKEN);
    }

    #[tokio::test]
    async fn every_write_uses_a_fresh_nonce() {
        let kv = kv();
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        let mut nonces = HashSet::new();
        for waba in ["W1", "W1", "W1", "W2"] {
            v.store(&sample(waba)).await.unwrap();
            let rec = raw_json(&kv, &format!("waba/{waba}")).await;
            assert!(
                nonces.insert(rec["nonce"].as_str().unwrap().to_owned()),
                "nonce reused"
            );
        }
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

        let mut rec = original.clone();
        rec["v"] = 2.into();
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

        // The untouched record still opens.
        put_json(&kv, "waba/W1", &original).await;
        assert!(v.get(&WabaId::new("W1")).await.unwrap().is_some());
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
    async fn swapping_two_tenants_records_fails_to_decrypt_both_ways() {
        let kv = kv();
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        v.store(&sample("W1").phone_number_ids(["P1"]))
            .await
            .unwrap();
        v.store(&sample("W2").phone_number_ids(["P2"]))
            .await
            .unwrap();
        let a = raw_json(&kv, "waba/W1").await;
        let b = raw_json(&kv, "waba/W2").await;
        // An attacker with write access swaps the records, and even fixes up
        // the clear waba_id so the JSON looks consistent.
        let mut a_as_b = a.clone();
        a_as_b["waba_id"] = "W2".into();
        let mut b_as_a = b.clone();
        b_as_a["waba_id"] = "W1".into();
        put_json(&kv, "waba/W2", &a_as_b).await;
        put_json(&kv, "waba/W1", &b_as_a).await;
        for w in ["W1", "W2"] {
            assert!(
                matches!(
                    v.get(&WabaId::new(w)).await,
                    Err(Error::Crypto(CryptoError::Decrypt))
                ),
                "{w} must not open with the other tenant's record"
            );
        }
        // Routing by phone number fails closed too: P2's traffic never gets
        // W1's token.
        assert!(
            v.get_by_phone_number(&PhoneNumberId::new("P2"))
                .await
                .is_err()
        );
        // Swapping back restores both.
        put_json(&kv, "waba/W1", &a).await;
        put_json(&kv, "waba/W2", &b).await;
        for w in ["W1", "W2"] {
            let t = v.get(&WabaId::new(w)).await.unwrap().unwrap();
            assert_eq!(t.token.expose_secret(), format!("{TOKEN}-{w}"));
        }
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
            allocation_config_id: None,
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
        // And the old key alone no longer opens them.
        let only_old = vault(&kv, VaultKeys::new(key("old", 1)));
        assert!(only_old.get(&WabaId::new("W1")).await.is_err());
    }

    /// A store whose next read of one key lets a concurrent writer replace
    /// the record right after the read (the window a read-time rotation
    /// races against).
    #[derive(Debug)]
    struct Interleaved {
        inner: MemoryKvStore,
        key: StoreKey,
        concurrent_write: Mutex<Option<Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl KvStore for Interleaved {
        async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
            let read = self.inner.get(key).await;
            let pending = if key == &self.key {
                self.concurrent_write.lock().unwrap().take()
            } else {
                None
            };
            if let Some(bytes) = pending {
                self.inner.put(key, bytes, Expiry::Never).await?;
            }
            read
        }
        async fn put(
            &self,
            key: &StoreKey,
            value: Vec<u8>,
            expiry: Expiry,
        ) -> Result<u64, StorageError> {
            self.inner.put(key, value, expiry).await
        }
        async fn put_if_absent(
            &self,
            key: &StoreKey,
            value: Vec<u8>,
            expiry: Expiry,
        ) -> Result<Option<u64>, StorageError> {
            self.inner.put_if_absent(key, value, expiry).await
        }
        async fn compare_and_swap(
            &self,
            key: &StoreKey,
            expected: u64,
            new: Option<Vec<u8>>,
            expiry: Expiry,
        ) -> Result<Option<u64>, StorageError> {
            self.inner
                .compare_and_swap(key, expected, new, expiry)
                .await
        }
        async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
            self.inner.delete(key).await
        }
    }

    #[tokio::test]
    async fn a_rotation_never_overwrites_a_concurrent_store() {
        // The replacement a concurrent onboarding writes: a new token, under
        // the new key.
        let scratch = kv();
        vault(&scratch, VaultKeys::new(key("new", 2)))
            .store(&StoredBusinessToken::new(
                "W1",
                AccessToken::new("NEW_TOKEN"),
            ))
            .await
            .unwrap();
        let replacement = raw(&scratch, "waba/W1").await.unwrap().value;

        let wabakey = StoreKey::new(TOKEN_NAMESPACE, "waba/W1");
        let store = Arc::new(Interleaved {
            inner: MemoryKvStore::new(),
            key: wabakey.clone(),
            concurrent_write: Mutex::new(None),
        });
        let kv: Arc<dyn KvStore> = store.clone();
        vault(&kv, VaultKeys::new(key("old", 1)))
            .store(&sample("W1"))
            .await
            .unwrap();
        *store.concurrent_write.lock().unwrap() = Some(replacement);

        // This read sees the old record and tries to re-encrypt it; the
        // concurrent store lands in between.
        let rotating = vault(
            &kv,
            VaultKeys::new(key("new", 2)).with_previous(key("old", 1)),
        );
        let seen = rotating.get(&WabaId::new("W1")).await.unwrap().unwrap();
        assert_eq!(seen.token.expose_secret(), format!("{TOKEN}-W1"));
        // The concurrent write wins: the stale token is not resurrected.
        let now = rotating.get(&WabaId::new("W1")).await.unwrap().unwrap();
        assert_eq!(now.token.expose_secret(), "NEW_TOKEN");
        // Same for an explicit rotation.
        let reread = vault(&kv, VaultKeys::new(key("old", 1)));
        reread.store(&sample("W1")).await.unwrap();
        vault(&scratch, VaultKeys::new(key("new", 2)))
            .store(&StoredBusinessToken::new("W1", AccessToken::new("NEWER")))
            .await
            .unwrap();
        *store.concurrent_write.lock().unwrap() =
            Some(raw(&scratch, "waba/W1").await.unwrap().value);
        assert!(!rotating.rotate(&WabaId::new("W1")).await.unwrap());
        let now = rotating.get(&WabaId::new("W1")).await.unwrap().unwrap();
        assert_eq!(now.token.expose_secret(), "NEWER");
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
        for len in [0, 16, 31, 33, 64] {
            assert!(
                matches!(
                    VaultKey::from_base64("k", &B64.encode(vec![9u8; len])),
                    Err(Error::Crypto(CryptoError::InvalidKey(_)))
                ),
                "{len} bytes"
            );
            assert!(VaultKey::new("k", SecretBytes::new(vec![9u8; len])).is_err());
        }
        assert!(VaultKey::from_base64("k", "***").is_err());
        assert!(VaultKey::new("", SecretBytes::new([0; 32])).is_err());
        assert!(VaultKey::new("has space", SecretBytes::new([0; 32])).is_err());
        assert!(VaultKey::new("x".repeat(65), SecretBytes::new([0; 32])).is_err());
        let a = VaultKey::generate("a").unwrap();
        let b = VaultKey::generate("b").unwrap();
        assert_eq!(a.key.len(), 32);
        assert_ne!(a.key.expose_secret(), b.key.expose_secret());
        assert_ne!(a.key.expose_secret(), &[0u8; 32]);
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
        assert_ne!(aad(&WabaId::new("W1"), "k"), aad(&WabaId::new("W2"), "k"));
        assert_ne!(aad(&WabaId::new("W1"), "k1"), aad(&WabaId::new("W1"), "k2"));
    }
}
