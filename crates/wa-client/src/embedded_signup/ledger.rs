//! The Solution Partner credit ledger: what onboarding shared with each
//! WABA, which WABAs the integrator approved, and which customer businesses
//! had the credit line revoked. Kept in the [`TokenVault`]'s store, sealed
//! with its keys, under keys of its own, so it outlives the tokens
//! ([`TokenVault::delete`] leaves it): revoking a line after the merchant
//! left needs the business id and the allocation.
//!
//! Keys (namespace `wa.token`):
//!
//! - `credit/<WABA_ID>`: a [`StoredCredit`], written with compare-and-swap
//!   by the approval of `onboard_with_approval` / `resume_with_approval`,
//!   by `share_credit_line`, by a revocation that learnt the owner
//!   business, and by an operator's
//!   [`EmbeddedSignup::clear_pending_share`](super::EmbeddedSignup::clear_pending_share).
//! - `revoked/<BUSINESS_ID>`: a [`RevokedBusiness`], written by
//!   [`EmbeddedSignup::revoke_credit_line`](super::EmbeddedSignup::revoke_credit_line)
//!   before it looks anything up on Meta's side; while it exists,
//!   onboarding refuses to share the line with that business again unless
//!   the request opts in
//!   ([`OnboardingRequest::reshare_after_revocation`](super::OnboardingRequest::reshare_after_revocation)).
//! - `credit-lease/<WABA_ID>`: a short lease held while one onboarding runs
//!   the credit step (or an operator clears a pending share), renewed
//!   before each post and before a clearance, so two concurrent onboardings
//!   of one WABA cannot both post a share, and no share runs while a
//!   pending one is cleared.
//!
//! Each record is `{v, kid, nonce, ciphertext}`: AES-256-GCM under the
//! vault's active key, the associated data binding it to its own store key,
//! so a record copied to another WABA's or business's key, or edited, fails
//! to open. Nothing in it is a secret. Sealing stops a record from being
//! forged or moved; it does **not** stop someone with write access to the
//! store from deleting a record (a revocation marker, and with it the
//! refusal to fund that business again) or from putting back an older
//! sealed copy of the same key (an earlier allocation, a marker since
//! cleared). Rollback protection is out of scope: keep write access to the
//! store as narrow as access to the vault key.
//!
//! Records under a previous key are re-sealed under the active key when
//! read, like tokens (unless [`TokenVault::rotate_on_read`] is off), and by
//! [`TokenVault::rotate`] (a WABA's credit record and its business's
//! marker) and [`TokenVault::rotate_business`] (a marker alone).

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use wa_core::error::{CreditError, CryptoError};
use wa_core::ids::{AllocationConfigId, BusinessId, FundingId, WabaId};
use wa_core::store::{Expiry, StoreKey, Versioned};
use wa_core::{Error, Result};

use super::vault::{SealedBlob, TOKEN_NAMESPACE, TokenVault, decode_json, encode_json};
use crate::credit_lines::WabaCurrency;

/// How long `share_credit_line` holds a WABA's lease after taking or
/// renewing it. Long enough for the requests between two renewals, short
/// enough that a crashed process does not block `resume` for long.
pub(super) const CREDIT_LEASE: Duration = Duration::from_secs(300);

/// How many times a ledger update retries a lost compare-and-swap before
/// giving up.
const ATTEMPTS: usize = 5;

/// What Solution Partner onboarding recorded for one WABA
/// ([`TokenVault::credit`]). Outlives the token.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StoredCredit {
    /// The WABA.
    pub waba_id: WabaId,
    /// Its owner business: as Meta reported it at onboarding, or as a
    /// revocation established it (what revocation looks the line up by).
    pub business_id: Option<BusinessId>,
    /// The allocation that funds the WABA, or the share intent recorded
    /// before the attach of the two-call method.
    pub allocation_config_id: Option<AllocationConfigId>,
    /// The currency the line was first requested in, sealed before the
    /// first share was posted: a line cannot change once attached, so a
    /// later onboarding or `resume` naming another is refused.
    pub currency: Option<WabaCurrency>,
    /// When the line was found or made to fund the WABA.
    pub shared_at: Option<OffsetDateTime>,
    /// When the integrator approved onboarding this WABA
    /// (`onboard_with_approval`, `resume_with_approval`). Solution Partner
    /// mode shares nothing for a WABA without it.
    pub approved_at: Option<OffsetDateTime>,
    /// The `created_at` of the token record the approval was given for
    /// ([`StoredBusinessToken::created_at`](super::StoredBusinessToken::created_at)):
    /// `resume` takes the approval only for that token record, so a token
    /// stored again for the WABA (a Tech Provider onboarding after an
    /// offboard, your own `TokenVault::store`) needs a new approval.
    pub approved_token_created_at: Option<OffsetDateTime>,
    /// Set just before a share is posted and cleared once its allocation is
    /// recorded: while set, a share may have gone through that the ledger
    /// does not know (a post that timed out, a process that died). When
    /// Meta never shows that share, an operator clears it with
    /// [`EmbeddedSignup::clear_pending_share`](super::EmbeddedSignup::clear_pending_share).
    pub pending_share: Option<OffsetDateTime>,
    /// Every pending share an operator cleared
    /// ([`EmbeddedSignup::clear_pending_share`](super::EmbeddedSignup::clear_pending_share)),
    /// oldest first: who, when, and what Meta showed then. Sealed with the
    /// rest of the record, and never counted as a share
    /// ([`Self::records_a_share`]).
    pub cleared_shares: Vec<ClearedShare>,
}

/// One pending share an operator cleared: the audit entry
/// [`EmbeddedSignup::clear_pending_share`](super::EmbeddedSignup::clear_pending_share)
/// appends to [`StoredCredit::cleared_shares`]. Its `Debug` redacts
/// [`Self::cleared_by`].
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ClearedShare {
    /// When the cleared share was flagged (the [`StoredCredit::pending_share`]
    /// it replaced).
    pub pending_since: OffsetDateTime,
    /// When it was cleared (the vault's clock).
    pub cleared_at: OffsetDateTime,
    /// Who cleared it, as the caller named them (trimmed). Personal data
    /// unless you pass an opaque operator id (recommended): it is kept,
    /// sealed, for as long as the WABA's credit record, which outlives the
    /// token and is never deleted by wa-rs; wa-rs never logs it, and
    /// `Debug` shows `<redacted>`.
    pub cleared_by: String,
    /// The WABA's `primary_funding_id` as Meta reported it when the share
    /// was cleared (`None`: nothing funded the WABA). When `Some`, no
    /// record of your line explained it and the caller acknowledged exactly
    /// this id as not your line (`acknowledged_funding`), or nothing would
    /// have been cleared.
    pub primary_funding_id: Option<FundingId>,
}

impl fmt::Debug for ClearedShare {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClearedShare")
            .field("pending_since", &self.pending_since)
            .field("cleared_at", &self.cleared_at)
            .field("cleared_by", &"<redacted>")
            .field("primary_funding_id", &self.primary_funding_id)
            .finish()
    }
}

impl StoredCredit {
    pub(super) fn new(waba_id: WabaId) -> Self {
        Self {
            waba_id,
            business_id: None,
            allocation_config_id: None,
            currency: None,
            shared_at: None,
            approved_at: None,
            approved_token_created_at: None,
            pending_share: None,
            cleared_shares: Vec::new(),
        }
    }

    /// Whether the integrator's approval is recorded for the token record
    /// created at `token_created_at` (see [`Self::approved_token_created_at`]).
    pub(super) fn approves(&self, token_created_at: Option<OffsetDateTime>) -> bool {
        self.approved_at.is_some()
            && self.approved_token_created_at.is_some()
            && self.approved_token_created_at == token_created_at
    }

    /// Whether this record shows that the credit line was, or may have
    /// been, shared with the WABA: an allocation, a share time, or a share
    /// posted whose outcome is unknown. (A currency alone does not: it is
    /// sealed before the first post.)
    pub fn records_a_share(&self) -> bool {
        self.allocation_config_id.is_some()
            || self.shared_at.is_some()
            || self.pending_share.is_some()
    }
}

/// A customer business whose credit line was revoked
/// ([`TokenVault::revoked_business`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RevokedBusiness {
    /// The business.
    pub business_id: BusinessId,
    /// When the revocation was first recorded (before it was sent).
    pub revoked_at: OffsetDateTime,
    /// The allocations revoked (or found already revoked) so far.
    pub allocation_config_ids: Vec<AllocationConfigId>,
}

#[derive(Serialize, Deserialize)]
struct CreditPlain {
    waba_id: WabaId,
    #[serde(default)]
    business_id: Option<BusinessId>,
    #[serde(default)]
    allocation_config_id: Option<AllocationConfigId>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default, with = "time::serde::timestamp::option")]
    shared_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::timestamp::option")]
    approved_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::timestamp::option")]
    approved_token_created_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::timestamp::option")]
    pending_share: Option<OffsetDateTime>,
    /// Absent from records written before the first clearance (and by
    /// revisions before it): an empty trail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cleared_shares: Vec<ClearedPlain>,
}

#[derive(Serialize, Deserialize)]
struct ClearedPlain {
    #[serde(with = "time::serde::timestamp")]
    pending_since: OffsetDateTime,
    #[serde(with = "time::serde::timestamp")]
    cleared_at: OffsetDateTime,
    cleared_by: String,
    #[serde(default)]
    primary_funding_id: Option<FundingId>,
}

impl CreditPlain {
    fn from_credit(credit: &StoredCredit) -> Self {
        Self {
            waba_id: credit.waba_id.clone(),
            business_id: credit.business_id.clone(),
            allocation_config_id: credit.allocation_config_id.clone(),
            currency: credit.currency.as_ref().map(|c| c.as_str().to_owned()),
            shared_at: credit.shared_at,
            approved_at: credit.approved_at,
            approved_token_created_at: credit.approved_token_created_at,
            pending_share: credit.pending_share,
            cleared_shares: credit
                .cleared_shares
                .iter()
                .map(|c| ClearedPlain {
                    pending_since: c.pending_since,
                    cleared_at: c.cleared_at,
                    cleared_by: c.cleared_by.clone(),
                    primary_funding_id: c.primary_funding_id.clone(),
                })
                .collect(),
        }
    }

    fn into_credit(self) -> StoredCredit {
        StoredCredit {
            waba_id: self.waba_id,
            business_id: self.business_id,
            allocation_config_id: self.allocation_config_id,
            currency: self.currency.map(currency_from),
            shared_at: self.shared_at,
            approved_at: self.approved_at,
            approved_token_created_at: self.approved_token_created_at,
            pending_share: self.pending_share,
            cleared_shares: self
                .cleared_shares
                .into_iter()
                .map(|c| ClearedShare {
                    pending_since: c.pending_since,
                    cleared_at: c.cleared_at,
                    cleared_by: c.cleared_by,
                    primary_funding_id: c.primary_funding_id,
                })
                .collect(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RevokedPlain {
    business_id: BusinessId,
    #[serde(with = "time::serde::timestamp")]
    revoked_at: OffsetDateTime,
    #[serde(default)]
    allocation_config_ids: Vec<AllocationConfigId>,
}

fn credit_key(waba_id: &WabaId) -> StoreKey {
    StoreKey::new(TOKEN_NAMESPACE, format!("credit/{waba_id}"))
}

fn revoked_key(business_id: &BusinessId) -> StoreKey {
    StoreKey::new(TOKEN_NAMESPACE, format!("revoked/{business_id}"))
}

fn lease_key(waba_id: &WabaId) -> StoreKey {
    StoreKey::new(TOKEN_NAMESPACE, format!("credit-lease/{waba_id}"))
}

/// [`CreditError::Busy`] for a step that posted nothing.
pub(super) fn busy(reason: &str) -> Error {
    CreditError::Busy {
        reason: reason.to_owned(),
        posted: false,
    }
    .into()
}

fn currency_from(code: String) -> WabaCurrency {
    code.parse().unwrap_or(WabaCurrency::Other(code))
}

impl TokenVault {
    /// Open a sealed record read from `key`.
    fn open_sealed<T: serde::de::DeserializeOwned>(
        &self,
        key: &StoreKey,
        v: &Versioned,
    ) -> Result<(T, SealedBlob, Vec<u8>)> {
        let blob: SealedBlob = decode_json(key, v)?;
        let plain = self.open_blob(key, &blob)?;
        let value = serde_json::from_slice(&plain)
            .map_err(|_| CryptoError::Malformed("decrypted credit ledger record"))?;
        Ok((value, blob, plain))
    }

    /// Read and open `key`; its version. A record under a previous key is
    /// re-sealed under the active one (compare-and-swap; a concurrent write
    /// wins) when the vault rotates on read, and the new version returned.
    async fn read_sealed<T: serde::de::DeserializeOwned>(
        &self,
        key: &StoreKey,
    ) -> Result<Option<(T, u64)>> {
        let Some(v) = self.kv().get(key).await? else {
            return Ok(None);
        };
        let (value, blob, plain) = self.open_sealed::<T>(key, &v)?;
        let mut version = v.version;
        if self.rotates_on_read() && !self.is_current(&blob) {
            match self.reseal_blob(key, &plain, version).await {
                Ok(Some(new)) => version = new,
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    kind = ?e.kind(),
                    "credit ledger: re-sealing under the active key failed; retried on next read"
                ),
            }
        }
        Ok(Some((value, version)))
    }

    /// Seal `plain` under the active key in place of version `version`.
    async fn reseal_blob(&self, key: &StoreKey, plain: &[u8], version: u64) -> Result<Option<u64>> {
        let bytes = encode_json(key, &self.seal_blob(key, plain)?)?;
        Ok(self
            .kv()
            .compare_and_swap(key, version, Some(bytes), Expiry::Keep)
            .await?)
    }

    /// Seal `value` for `key`: create it when `expected` is `None`, else
    /// replace version `expected`. `None` when another write got there
    /// first.
    async fn write_sealed<T: Serialize>(
        &self,
        key: &StoreKey,
        value: &T,
        expected: Option<u64>,
    ) -> Result<Option<u64>> {
        let plain = serde_json::to_vec(value)
            .map_err(|_| CryptoError::Malformed("credit ledger record"))?;
        let bytes = encode_json(key, &self.seal_blob(key, &plain)?)?;
        Ok(match expected {
            None => self.kv().put_if_absent(key, bytes, Expiry::Never).await?,
            Some(version) => {
                self.kv()
                    .compare_and_swap(key, version, Some(bytes), Expiry::Keep)
                    .await?
            }
        })
    }

    /// What Solution Partner onboarding recorded for `waba_id`, if anything.
    ///
    /// Fails with [`CryptoError`] when the record cannot be authenticated
    /// (edited, copied from another WABA, or sealed under a key no longer
    /// configured).
    pub async fn credit(&self, waba_id: &WabaId) -> Result<Option<StoredCredit>> {
        Ok(self.credit_versioned(waba_id).await?.map(|(c, _)| c))
    }

    pub(super) async fn credit_versioned(
        &self,
        waba_id: &WabaId,
    ) -> Result<Option<(StoredCredit, u64)>> {
        let key = credit_key(waba_id);
        let Some((plain, version)) = self.read_sealed::<CreditPlain>(&key).await? else {
            return Ok(None);
        };
        if &plain.waba_id != waba_id {
            return Err(CryptoError::Decrypt.into());
        }
        Ok(Some((plain.into_credit(), version)))
    }

    /// Write `credit`: create it (`expected` `None`) or replace version
    /// `expected`. Returns the new version; a concurrent write is
    /// [`CreditError::Busy`]. **Only before a share is posted**: after one,
    /// use [`Self::update_credit`], which merges instead.
    pub(super) async fn put_credit(
        &self,
        credit: &StoredCredit,
        expected: Option<u64>,
    ) -> Result<u64> {
        self.write_sealed(
            &credit_key(&credit.waba_id),
            &CreditPlain::from_credit(credit),
            expected,
        )
        .await?
        .ok_or_else(|| busy("the WABA's credit record changed while this step ran"))
    }

    /// Apply `change` to the credit record of `waba_id` (a new record when
    /// there is none) with compare-and-swap, re-reading and re-applying it
    /// when another write got there first. `change` returns `false` to
    /// leave the record as it is. The record and its version afterwards, or
    /// `None` when every attempt lost the race.
    pub(super) async fn update_credit(
        &self,
        waba_id: &WabaId,
        mut change: impl FnMut(&mut StoredCredit) -> bool,
    ) -> Result<Option<(StoredCredit, Option<u64>)>> {
        for _ in 0..ATTEMPTS {
            let (mut credit, version) = match self.credit_versioned(waba_id).await? {
                Some((credit, version)) => (credit, Some(version)),
                None => (StoredCredit::new(waba_id.clone()), None),
            };
            if !change(&mut credit) {
                return Ok(Some((credit, version)));
            }
            let written = self
                .write_sealed(
                    &credit_key(waba_id),
                    &CreditPlain::from_credit(&credit),
                    version,
                )
                .await?;
            if let Some(version) = written {
                return Ok(Some((credit, Some(version))));
            }
        }
        Ok(None)
    }

    /// Clear the pending share of `credit`, read at `version`, appending
    /// `cleared` to its audit trail. Compare-and-swap on that version: when
    /// anything wrote the record since (a share, a revocation settling it,
    /// another clearance), nothing is cleared and the call is
    /// [`CreditError::Busy`]: what Meta showed may no longer be what the
    /// record says.
    pub(super) async fn clear_pending_share(
        &self,
        credit: &StoredCredit,
        version: u64,
        cleared: ClearedShare,
    ) -> Result<StoredCredit> {
        let mut next = credit.clone();
        next.pending_share = None;
        next.cleared_shares.push(cleared);
        self.write_sealed(
            &credit_key(&next.waba_id),
            &CreditPlain::from_credit(&next),
            Some(version),
        )
        .await?
        .ok_or_else(|| {
            busy("the WABA's credit record changed while its pending share was checked; nothing was cleared, call again")
        })?;
        Ok(next)
    }

    /// Record the integrator's approval of onboarding `waba_id` with the
    /// token record created at `token_created_at`.
    pub(super) async fn record_approval(
        &self,
        waba_id: &WabaId,
        token_created_at: Option<OffsetDateTime>,
    ) -> Result<()> {
        let now = self.now();
        self.update_credit(waba_id, |c| {
            c.approved_at = Some(now);
            c.approved_token_created_at = token_created_at;
            true
        })
        .await?
        .map(|_| ())
        .ok_or_else(|| {
            busy("the WABA's credit record kept changing while the approval was recorded")
        })
    }

    /// Record `business_id` as the owner of `waba_id` when the credit record
    /// names none (creating the record if there is none). Leaves a record
    /// naming a business as it is.
    pub(super) async fn note_business(
        &self,
        waba_id: &WabaId,
        business_id: &BusinessId,
    ) -> Result<()> {
        self.update_credit(waba_id, |c| {
            if c.business_id.is_some() {
                return false;
            }
            c.business_id = Some(business_id.clone());
            true
        })
        .await?
        .map(|_| ())
        .ok_or_else(|| busy("the WABA's credit record kept changing"))
    }

    /// The revocation marker of `business_id`, if its credit line was
    /// revoked through [`EmbeddedSignup::revoke_credit_line`](super::EmbeddedSignup::revoke_credit_line)
    /// and not shared again since.
    pub async fn revoked_business(
        &self,
        business_id: &BusinessId,
    ) -> Result<Option<RevokedBusiness>> {
        Ok(self.revoked_versioned(business_id).await?.map(|(r, _)| r))
    }

    pub(super) async fn revoked_versioned(
        &self,
        business_id: &BusinessId,
    ) -> Result<Option<(RevokedBusiness, u64)>> {
        let key = revoked_key(business_id);
        let Some((plain, version)) = self.read_sealed::<RevokedPlain>(&key).await? else {
            return Ok(None);
        };
        if &plain.business_id != business_id {
            return Err(CryptoError::Decrypt.into());
        }
        Ok(Some((
            RevokedBusiness {
                business_id: plain.business_id,
                revoked_at: plain.revoked_at,
                allocation_config_ids: plain.allocation_config_ids,
            },
            version,
        )))
    }

    /// Record that `business_id`'s line is (being) revoked, adding `ids`.
    /// Keeps the first `revoked_at`. A marker that cannot be read (edited,
    /// sealed under a key no longer configured) is replaced: a marker only
    /// ever makes onboarding stricter, so writing a fresh one is safe.
    pub(super) async fn mark_revoked(
        &self,
        business_id: &BusinessId,
        ids: &[AllocationConfigId],
    ) -> Result<()> {
        let key = revoked_key(business_id);
        for _ in 0..ATTEMPTS {
            let fresh = || RevokedPlain {
                business_id: business_id.clone(),
                revoked_at: self.now(),
                allocation_config_ids: Vec::new(),
            };
            let (mut plain, version) = match self.kv().get(&key).await? {
                None => (fresh(), None),
                Some(v) => match self.open_sealed::<RevokedPlain>(&key, &v) {
                    Ok((plain, _, _)) if &plain.business_id == business_id => {
                        (plain, Some(v.version))
                    }
                    Ok(_) => (fresh(), Some(v.version)),
                    Err(e) => {
                        tracing::warn!(
                            kind = ?e.kind(),
                            "credit ledger: unreadable revocation marker replaced"
                        );
                        (fresh(), Some(v.version))
                    }
                },
            };
            for id in ids {
                if !plain.allocation_config_ids.contains(id) {
                    plain.allocation_config_ids.push(id.clone());
                }
            }
            if self.write_sealed(&key, &plain, version).await?.is_some() {
                return Ok(());
            }
        }
        Err(busy("the business's revocation marker kept changing"))
    }

    /// Forget `business_id`'s revocation marker (after an explicit
    /// re-share), only if it is still at `version`, the one the share read
    /// before posting. `false` when it changed or went: a revocation ran
    /// meanwhile.
    pub(super) async fn clear_revoked(
        &self,
        business_id: &BusinessId,
        version: u64,
    ) -> Result<bool> {
        Ok(self
            .kv()
            .compare_and_swap(&revoked_key(business_id), version, None, Expiry::Keep)
            .await?
            .is_some())
    }

    fn lease_stamp(&self, key: &StoreKey) -> Result<Vec<u8>> {
        encode_json(
            key,
            &serde_json::json!({ "at": self.now().unix_timestamp() }),
        )
    }

    /// Take `waba_id`'s credit lease for [`CREDIT_LEASE`]; its version, to
    /// renew and release it. [`CreditError::Busy`] while another holds it.
    pub(super) async fn lease_credit(&self, waba_id: &WabaId) -> Result<u64> {
        let key = lease_key(waba_id);
        self.kv()
            .put_if_absent(&key, self.lease_stamp(&key)?, Expiry::After(CREDIT_LEASE))
            .await?
            .ok_or_else(|| {
                busy("another onboarding of this WABA is sharing its credit line; resume later")
            })
    }

    /// Extend the lease at `version` for another [`CREDIT_LEASE`]; its new
    /// version. `None` when it expired or another holder has it now: the
    /// caller must not post.
    pub(super) async fn renew_credit(&self, waba_id: &WabaId, version: u64) -> Result<Option<u64>> {
        let key = lease_key(waba_id);
        Ok(self
            .kv()
            .compare_and_swap(
                &key,
                version,
                Some(self.lease_stamp(&key)?),
                Expiry::After(CREDIT_LEASE),
            )
            .await?)
    }

    /// Release a lease taken by [`Self::lease_credit`] (only that one: a
    /// lease that expired and was taken again is left to its new holder).
    pub(super) async fn release_credit(&self, waba_id: &WabaId, version: u64) -> Result<()> {
        self.kv()
            .compare_and_swap(&lease_key(waba_id), version, None, Expiry::Keep)
            .await?;
        Ok(())
    }

    /// Re-seal `key` under the active key if it is under an older one.
    async fn rotate_key(&self, key: &StoreKey) -> Result<bool> {
        let Some(v) = self.kv().get(key).await? else {
            return Ok(false);
        };
        let blob: SealedBlob = decode_json(key, &v)?;
        if self.is_current(&blob) {
            return Ok(false);
        }
        let plain = self.open_blob(key, &blob)?;
        Ok(self.reseal_blob(key, &plain, v.version).await?.is_some())
    }

    /// Re-seal the credit record of `waba_id`, and its business's
    /// revocation marker, under the active key. Whether anything was
    /// rewritten.
    pub(super) async fn rotate_ledger(&self, waba_id: &WabaId) -> Result<bool> {
        let key = credit_key(waba_id);
        let mut rewritten = self.rotate_key(&key).await?;
        let business = match self.kv().get(&key).await? {
            Some(v) => {
                let (plain, _, _) = self.open_sealed::<CreditPlain>(&key, &v)?;
                plain.business_id
            }
            None => None,
        };
        if let Some(business) = business {
            rewritten |= self.rotate_key(&revoked_key(&business)).await?;
        }
        Ok(rewritten)
    }

    /// Re-seal the revocation marker of `business_id` under the active key
    /// if it is under an older one; whether it was rewritten.
    ///
    /// [`Self::rotate`] already does this for the business a WABA's credit
    /// record names (every revocation writes its business there). Use this
    /// one for markers no credit record points to: a business revoked with
    /// [`EmbeddedSignup::revoke_business_credit_line`](super::EmbeddedSignup::revoke_business_credit_line),
    /// or before this revision recorded the business (take the ids from
    /// the `business_id` of your stored [`CreditRevocation`](crate::credit_lines::CreditRevocation)
    /// reports). The vault cannot list its records.
    pub async fn rotate_business(&self, business_id: &BusinessId) -> Result<bool> {
        self.rotate_key(&revoked_key(business_id)).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pretty_assertions::assert_eq;
    use time::macros::datetime;
    use wa_adapters::store::MemoryKvStore;
    use wa_core::clock::ManualClock;
    use wa_core::secret::SecretBytes;
    use wa_core::store::KvStore;

    use super::super::vault::{VaultKey, VaultKeys};
    use super::*;

    fn vault(kv: &Arc<dyn KvStore>, keys: VaultKeys) -> TokenVault {
        TokenVault::new(Arc::clone(kv), keys)
            .unwrap()
            .with_clock(Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC))))
    }

    fn key(id: &str, byte: u8) -> VaultKey {
        VaultKey::new(id, SecretBytes::new([byte; 32])).unwrap()
    }

    fn credit(waba: &str) -> StoredCredit {
        StoredCredit {
            business_id: Some(BusinessId::new("2729063490586005")),
            allocation_config_id: Some(AllocationConfigId::new("58501441721238")),
            currency: Some(WabaCurrency::Eur),
            shared_at: Some(datetime!(2026-09-24 12:00 UTC)),
            ..StoredCredit::new(WabaId::new(waba))
        }
    }

    async fn raw(kv: &Arc<dyn KvStore>, k: &str) -> Vec<u8> {
        kv.get(&StoreKey::new(TOKEN_NAMESPACE, k))
            .await
            .unwrap()
            .unwrap()
            .value
    }

    async fn put(kv: &Arc<dyn KvStore>, k: &str, v: Vec<u8>) {
        kv.put(&StoreKey::new(TOKEN_NAMESPACE, k), v, Expiry::Never)
            .await
            .unwrap();
    }

    /// Sealed, bound to its key, compare-and-swapped, and left in place
    /// when the token is deleted.
    #[tokio::test]
    async fn the_credit_record_is_sealed_bound_to_its_waba_and_outlives_the_token() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        let version = v.put_credit(&credit("W1"), None).await.unwrap();
        assert_eq!(
            v.credit(&WabaId::new("W1")).await.unwrap(),
            Some(credit("W1"))
        );
        let bytes = raw(&kv, "credit/W1").await;
        for clear in ["2729063490586005", "58501441721238", "currency", "waba_id"] {
            assert!(
                !String::from_utf8_lossy(&bytes).contains(clear),
                "{clear} in the clear"
            );
        }

        // A second writer with a stale version loses, typed.
        v.put_credit(&credit("W1"), Some(version)).await.unwrap();
        let err = v
            .put_credit(&credit("W1"), Some(version))
            .await
            .unwrap_err();
        assert!(EmbeddedSignupRefusal::busy(&err), "{err}");
        let err = v.put_credit(&credit("W1"), None).await.unwrap_err();
        assert!(EmbeddedSignupRefusal::busy(&err), "{err}");

        // Copied under another WABA's key: does not open.
        put(&kv, "credit/W2", bytes.clone()).await;
        assert!(matches!(
            v.credit(&WabaId::new("W2")).await,
            Err(Error::Crypto(CryptoError::Decrypt))
        ));
        // Edited: does not open.
        let mut blob: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        blob["ciphertext"] = "AAAA".into();
        put(&kv, "credit/W3", serde_json::to_vec(&blob).unwrap()).await;
        assert!(v.credit(&WabaId::new("W3")).await.is_err());

        // Deleting the token leaves the ledger.
        v.delete(&WabaId::new("W1")).await.unwrap();
        assert!(v.credit(&WabaId::new("W1")).await.unwrap().is_some());
    }

    /// The associated data binds a sealed record to its store key: opened
    /// under any other key it fails, whatever its plaintext says.
    #[test]
    fn a_sealed_record_opens_only_under_its_own_key() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        let here = StoreKey::new(TOKEN_NAMESPACE, "credit/W1");
        let blob = v.seal_blob(&here, b"{}").unwrap();
        assert_eq!(v.open_blob(&here, &blob).unwrap(), b"{}");
        for elsewhere in [
            StoreKey::new(TOKEN_NAMESPACE, "credit/W2"),
            StoreKey::new(TOKEN_NAMESPACE, "revoked/W1"),
            StoreKey::new("wa.other", "credit/W1"),
        ] {
            assert!(
                matches!(
                    v.open_blob(&elsewhere, &blob),
                    Err(Error::Crypto(CryptoError::Decrypt))
                ),
                "{elsewhere}"
            );
        }
    }

    #[tokio::test]
    async fn the_revocation_marker_merges_and_keeps_its_first_date() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let clock = Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC)));
        let v = TokenVault::new(Arc::clone(&kv), VaultKeys::new(key("k1", 7)))
            .unwrap()
            .with_clock(clock.clone());
        let business = BusinessId::new("2729063490586005");
        v.mark_revoked(&business, &[]).await.unwrap();
        clock.advance(std::time::Duration::from_secs(3600));
        v.mark_revoked(
            &business,
            &[AllocationConfigId::new("A1"), AllocationConfigId::new("A1")],
        )
        .await
        .unwrap();
        v.mark_revoked(&business, &[AllocationConfigId::new("A2")])
            .await
            .unwrap();
        let marker = v.revoked_business(&business).await.unwrap().unwrap();
        assert_eq!(marker.revoked_at, datetime!(2026-09-24 12:00 UTC));
        assert_eq!(
            marker.allocation_config_ids,
            [AllocationConfigId::new("A1"), AllocationConfigId::new("A2")]
        );
        // Moved to another business's key: does not open (a marker cannot
        // be forged by copying one).
        put(
            &kv,
            "revoked/OTHER",
            raw(&kv, "revoked/2729063490586005").await,
        )
        .await;
        assert!(v.revoked_business(&BusinessId::new("OTHER")).await.is_err());

        // Cleared only at the version the re-share read: a revocation that
        // touched the marker since keeps it.
        let (_, read) = v.revoked_versioned(&business).await.unwrap().unwrap();
        v.mark_revoked(&business, &[]).await.unwrap(); // a revocation meanwhile
        assert!(!v.clear_revoked(&business, read).await.unwrap());
        assert!(v.revoked_business(&business).await.unwrap().is_some());
        let (_, now) = v.revoked_versioned(&business).await.unwrap().unwrap();
        assert!(v.clear_revoked(&business, now).await.unwrap());
        assert!(v.revoked_business(&business).await.unwrap().is_none());
    }

    /// A marker only makes onboarding stricter: one that cannot be read is
    /// replaced, never a reason not to mark.
    #[tokio::test]
    async fn an_unreadable_marker_is_replaced() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        let business = BusinessId::new("2729063490586005");
        for garbage in [
            b"{}".to_vec(),
            b"not json".to_vec(),
            // Sealed under a key this vault does not have.
            {
                let other = vault(&kv, VaultKeys::new(key("gone", 9)));
                let k = revoked_key(&business);
                encode_json(&k, &other.seal_blob(&k, b"{}").unwrap()).unwrap()
            },
        ] {
            put(&kv, "revoked/2729063490586005", garbage).await;
            assert!(v.revoked_business(&business).await.is_err(), "vacuous");
            v.mark_revoked(&business, &[AllocationConfigId::new("A1")])
                .await
                .unwrap();
            let marker = v.revoked_business(&business).await.unwrap().unwrap();
            assert_eq!(
                marker.allocation_config_ids,
                [AllocationConfigId::new("A1")]
            );
        }
    }

    /// Approval, the business a revocation learnt and a post's allocation
    /// merge into whatever the record holds, instead of failing on a
    /// concurrent write.
    #[tokio::test]
    async fn updates_merge_into_the_current_record() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        let waba = WabaId::new("W1");
        v.record_approval(&waba, Some(datetime!(2026-09-24 11:00 UTC)))
            .await
            .unwrap();
        let first = v.credit(&waba).await.unwrap().unwrap();
        assert_eq!(first.approved_at, Some(datetime!(2026-09-24 12:00 UTC)));
        assert!(!first.records_a_share(), "an approval is not a share");
        v.note_business(&waba, &BusinessId::new("B1"))
            .await
            .unwrap();
        v.note_business(&waba, &BusinessId::new("B2"))
            .await
            .unwrap();
        let noted = v.credit(&waba).await.unwrap().unwrap();
        assert_eq!(
            noted.business_id,
            Some(BusinessId::new("B1")),
            "never replaced"
        );
        assert_eq!(noted.approved_at, first.approved_at, "kept");
        let (merged, _) = v
            .update_credit(&waba, |c| {
                c.allocation_config_id = Some(AllocationConfigId::new("A1"));
                true
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(merged.business_id, Some(BusinessId::new("B1")));
        assert!(merged.records_a_share());
        let mut pending = StoredCredit::new(WabaId::new("W2"));
        pending.pending_share = Some(datetime!(2026-09-24 12:00 UTC));
        assert!(pending.records_a_share(), "a post whose outcome is unknown");
        let mut sealed = StoredCredit::new(WabaId::new("W2"));
        sealed.currency = Some(WabaCurrency::Usd);
        assert!(
            !sealed.records_a_share(),
            "the currency is sealed before any post"
        );
    }

    /// An operator's clearance: only at the version read, appended to the
    /// trail, and a record written before the trail existed opens with an
    /// empty one.
    #[tokio::test]
    async fn a_clearance_is_compare_and_swapped_and_appended() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        // The format before the trail: no `cleared_shares` at all.
        let k = credit_key(&WabaId::new("W1"));
        let old = br#"{"waba_id":"W1","business_id":"B1","pending_share":1790251200}"#;
        put(
            &kv,
            "credit/W1",
            encode_json(&k, &v.seal_blob(&k, old).unwrap()).unwrap(),
        )
        .await;
        let (read, version) = v
            .credit_versioned(&WabaId::new("W1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.pending_share, Some(datetime!(2026-09-24 12:00 UTC)));
        assert!(read.cleared_shares.is_empty());

        let entry = |by: &str| ClearedShare {
            pending_since: datetime!(2026-09-24 12:00 UTC),
            cleared_at: datetime!(2026-09-24 13:00 UTC),
            cleared_by: by.to_owned(),
            primary_funding_id: Some(FundingId::new("F1")),
        };
        let cleared = v
            .clear_pending_share(&read, version, entry("first"))
            .await
            .unwrap();
        assert_eq!(cleared.pending_share, None);
        // A second clearance from the same stale read loses, typed.
        let err = v
            .clear_pending_share(&read, version, entry("stale"))
            .await
            .unwrap_err();
        assert!(EmbeddedSignupRefusal::busy(&err), "{err}");
        let stored = v.credit(&WabaId::new("W1")).await.unwrap().unwrap();
        assert_eq!(stored, cleared);
        assert_eq!(stored.cleared_shares, [entry("first")]);
        assert_eq!(stored.business_id, Some(BusinessId::new("B1")), "kept");

        // A later clearance appends.
        let mut again = stored.clone();
        again.pending_share = Some(datetime!(2026-09-25 08:00 UTC));
        let (_, version) = v
            .credit_versioned(&WabaId::new("W1"))
            .await
            .unwrap()
            .unwrap();
        v.put_credit(&again, Some(version)).await.unwrap();
        let (read, version) = v
            .credit_versioned(&WabaId::new("W1"))
            .await
            .unwrap()
            .unwrap();
        v.clear_pending_share(&read, version, entry("second"))
            .await
            .unwrap();
        assert_eq!(
            v.credit(&WabaId::new("W1"))
                .await
                .unwrap()
                .unwrap()
                .cleared_shares,
            [entry("first"), entry("second")]
        );
        assert!(
            !String::from_utf8_lossy(&raw(&kv, "credit/W1").await).contains("first"),
            "sealed"
        );
    }

    /// A5: a lease outlived by a slow step expires; the next holder gets
    /// it, and the first can neither renew (so it posts nothing more) nor
    /// release it.
    #[tokio::test]
    async fn a_lease_expires_and_the_late_holder_loses_it() {
        let clock = Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC)));
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::with_clock(clock.clone()));
        let v = TokenVault::new(Arc::clone(&kv), VaultKeys::new(key("k1", 7)))
            .unwrap()
            .with_clock(clock.clone());
        let waba = WabaId::new("W1");
        // Taken and never renewed: it expires on its own.
        let crashed = v.lease_credit(&waba).await.unwrap();
        clock.advance(CREDIT_LEASE + Duration::from_secs(1));
        let slow = v.lease_credit(&waba).await.unwrap();
        assert_eq!(v.renew_credit(&waba, crashed).await.unwrap(), None);
        clock.advance(CREDIT_LEASE.saturating_sub(Duration::from_secs(1)));
        let renewed = v.renew_credit(&waba, slow).await.unwrap().unwrap();
        clock.advance(CREDIT_LEASE.saturating_sub(Duration::from_secs(1)));
        assert!(v.lease_credit(&waba).await.is_err(), "renewal extended it");
        clock.advance(Duration::from_secs(2));
        let next = v.lease_credit(&waba).await.unwrap();
        assert_eq!(v.renew_credit(&waba, renewed).await.unwrap(), None);
        v.release_credit(&waba, renewed).await.unwrap();
        assert!(
            v.lease_credit(&waba).await.is_err(),
            "still the next holder's"
        );
        v.release_credit(&waba, next).await.unwrap();
        v.lease_credit(&waba).await.unwrap();
    }

    #[tokio::test]
    async fn the_lease_is_exclusive_and_released_only_by_its_holder() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let v = vault(&kv, VaultKeys::new(key("k1", 7)));
        let waba = WabaId::new("W1");
        let held = v.lease_credit(&waba).await.unwrap();
        let err = v.lease_credit(&waba).await.unwrap_err();
        assert!(EmbeddedSignupRefusal::busy(&err), "{err}");
        // A stale version does not release someone else's lease.
        v.release_credit(&waba, held + 1000).await.unwrap();
        assert!(v.lease_credit(&waba).await.is_err());
        v.release_credit(&waba, held).await.unwrap();
        let again = v.lease_credit(&waba).await.unwrap();
        v.release_credit(&waba, again).await.unwrap();
    }

    #[tokio::test]
    async fn rotation_reseals_the_ledger_under_the_active_key() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let old = vault(&kv, VaultKeys::new(key("old", 1)));
        old.put_credit(&credit("W1"), None).await.unwrap();
        old.mark_revoked(&BusinessId::new("2729063490586005"), &[])
            .await
            .unwrap();
        let new = vault(
            &kv,
            VaultKeys::new(key("new", 2)).with_previous(key("old", 1)),
        );
        assert!(new.rotate(&WabaId::new("W1")).await.unwrap());
        let only_new = vault(&kv, VaultKeys::new(key("new", 2)));
        assert_eq!(
            only_new.credit(&WabaId::new("W1")).await.unwrap(),
            Some(credit("W1"))
        );
        assert!(
            only_new
                .revoked_business(&BusinessId::new("2729063490586005"))
                .await
                .unwrap()
                .is_some()
        );
        assert!(!new.rotate(&WabaId::new("W1")).await.unwrap(), "done once");
    }

    /// Reading a ledger record re-seals it under the active key, as reading
    /// a token does; a replica that does not rotate on read leaves it.
    #[tokio::test]
    async fn ledger_reads_reseal_under_the_active_key() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let old = vault(&kv, VaultKeys::new(key("old", 1)));
        let business = BusinessId::new("2729063490586005");
        old.put_credit(&credit("W1"), None).await.unwrap();
        old.mark_revoked(&business, &[]).await.unwrap();
        let replica = vault(
            &kv,
            VaultKeys::new(key("new", 2)).with_previous(key("old", 1)),
        )
        .rotate_on_read(false);
        replica.credit(&WabaId::new("W1")).await.unwrap().unwrap();
        replica.revoked_business(&business).await.unwrap().unwrap();
        let only_new = vault(&kv, VaultKeys::new(key("new", 2)));
        assert!(
            only_new.credit(&WabaId::new("W1")).await.is_err(),
            "not rotated"
        );

        let new = vault(
            &kv,
            VaultKeys::new(key("new", 2)).with_previous(key("old", 1)),
        );
        new.credit(&WabaId::new("W1")).await.unwrap().unwrap();
        new.revoked_business(&business).await.unwrap().unwrap();
        assert_eq!(
            only_new.credit(&WabaId::new("W1")).await.unwrap(),
            Some(credit("W1"))
        );
        assert!(
            only_new
                .revoked_business(&business)
                .await
                .unwrap()
                .is_some()
        );
    }

    /// The ledger of an offboarded WABA (no token), a marker no credit
    /// record names, and a token whose credit record is corrupt all rotate.
    #[tokio::test]
    async fn rotation_reaches_every_ledger_record() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let old = vault(&kv, VaultKeys::new(key("old", 1)));
        old.put_credit(&credit("OFFBOARDED"), None).await.unwrap();
        let orphan = BusinessId::new("REVOKED_BY_BUSINESS_ID");
        old.mark_revoked(&orphan, &[]).await.unwrap();
        old.store(&super::super::vault::StoredBusinessToken::new(
            "CORRUPT",
            wa_core::secret::AccessToken::new("EAAB"),
        ))
        .await
        .unwrap();
        put(&kv, "credit/CORRUPT", b"{}".to_vec()).await;
        // And the other way round: a corrupt token, a readable ledger.
        old.put_credit(&credit("CORRUPT_TOKEN"), None)
            .await
            .unwrap();
        put(&kv, "waba/CORRUPT_TOKEN", b"{}".to_vec()).await;

        let new = vault(
            &kv,
            VaultKeys::new(key("new", 2)).with_previous(key("old", 1)),
        )
        .rotate_on_read(false);
        assert!(new.rotate(&WabaId::new("OFFBOARDED")).await.unwrap());
        assert!(new.rotate_business(&orphan).await.unwrap());
        assert!(!new.rotate_business(&orphan).await.unwrap(), "done once");
        assert!(
            new.rotate(&WabaId::new("CORRUPT")).await.is_err(),
            "the corrupt record is reported"
        );
        assert!(
            new.rotate(&WabaId::new("CORRUPT_TOKEN")).await.is_err(),
            "the corrupt token is reported"
        );

        let only_new = vault(&kv, VaultKeys::new(key("new", 2)));
        assert!(
            only_new
                .credit(&WabaId::new("OFFBOARDED"))
                .await
                .unwrap()
                .is_some()
        );
        assert!(only_new.revoked_business(&orphan).await.unwrap().is_some());
        assert!(
            only_new
                .get(&WabaId::new("CORRUPT"))
                .await
                .unwrap()
                .is_some(),
            "the token was rotated despite its corrupt credit record"
        );
        assert_eq!(
            only_new
                .credit(&WabaId::new("CORRUPT_TOKEN"))
                .await
                .unwrap(),
            Some(credit("CORRUPT_TOKEN")),
            "the ledger was rotated despite its corrupt token"
        );
    }

    /// The typed refusals, as callers test them.
    struct EmbeddedSignupRefusal;

    impl EmbeddedSignupRefusal {
        fn busy(err: &Error) -> bool {
            super::super::EmbeddedSignup::is_credit_step_busy(err)
        }
    }
}
