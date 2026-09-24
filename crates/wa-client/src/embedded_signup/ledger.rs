//! The Solution Partner credit ledger: what onboarding shared with each
//! WABA, and which customer businesses had the credit line revoked. Kept in
//! the [`TokenVault`]'s store, sealed with its keys, under keys of its own,
//! so it outlives the tokens ([`TokenVault::delete`] leaves it): revoking a
//! line after the merchant left needs the business id and the allocation.
//!
//! Keys (namespace `wa.token`):
//!
//! - `credit/<WABA_ID>`: a [`StoredCredit`], written by `share_credit_line`
//!   with compare-and-swap.
//! - `revoked/<BUSINESS_ID>`: a [`RevokedBusiness`], written by
//!   [`EmbeddedSignup::revoke_credit_line`](super::EmbeddedSignup::revoke_credit_line)
//!   before it revokes anything; while it exists, onboarding refuses to
//!   share the line with that business again unless the request opts in
//!   ([`OnboardingRequest::reshare_after_revocation`](super::OnboardingRequest::reshare_after_revocation)).
//! - `credit-lease/<WABA_ID>`: a short lease held while one onboarding runs
//!   the credit step, so two concurrent onboardings of one WABA cannot both
//!   post a share.
//!
//! Each record is `{v, kid, nonce, ciphertext}`: AES-256-GCM under the
//! vault's active key, the associated data binding it to its own store key,
//! so a record copied to another WABA's or business's key (or edited) fails
//! to open. Nothing in it is a secret; sealing makes it tamper-evident: a
//! revocation marker cannot be forged away, nor a stored allocation
//! redirected to another customer's.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use wa_core::error::{CryptoError, ValidationError};
use wa_core::ids::{AllocationConfigId, BusinessId, WabaId};
use wa_core::store::{Expiry, StoreKey};
use wa_core::{Error, Result};

use super::vault::{SealedBlob, TOKEN_NAMESPACE, TokenVault, decode_json, encode_json};
use crate::credit_lines::WabaCurrency;

/// How long `share_credit_line` holds a WABA's lease. Long enough for its
/// requests, short enough that a crashed process does not block `resume`
/// for long.
pub(super) const CREDIT_LEASE: Duration = Duration::from_secs(300);

/// How many times a revocation marker update retries a lost
/// compare-and-swap before giving up.
const MARK_ATTEMPTS: usize = 5;

/// `ValidationError::field` of the refusals the Solution Partner credit
/// steps make, for callers to branch on (also through
/// [`EmbeddedSignup::is_credit_line_revoked`](super::EmbeddedSignup::is_credit_line_revoked)
/// and [`EmbeddedSignup::is_credit_step_busy`](super::EmbeddedSignup::is_credit_step_busy)).
pub mod refusals {
    /// The customer business had the credit line revoked (by
    /// `revoke_credit_line`, or on Meta's side: only `DELETED` records and
    /// no active one). Onboarding does not share it again unless the request
    /// says so with `OnboardingRequest::reshare_after_revocation`.
    pub const CREDIT_LINE_REVOKED: &str = "credit_line_revoked";
    /// Another onboarding of the same WABA holds the credit step, or changed
    /// its credit record meanwhile. Nothing was posted; `resume` later.
    pub const CREDIT_STEP_BUSY: &str = "credit_line_busy";
}

/// What Solution Partner onboarding shared with one WABA
/// ([`TokenVault::credit`]). Outlives the token.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StoredCredit {
    /// The WABA.
    pub waba_id: WabaId,
    /// Its owner business as Meta reported it at onboarding (what
    /// revocation looks the line up by).
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
}

impl StoredCredit {
    pub(super) fn new(waba_id: WabaId) -> Self {
        Self {
            waba_id,
            business_id: None,
            allocation_config_id: None,
            currency: None,
            shared_at: None,
        }
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

pub(super) fn busy(reason: &str) -> Error {
    ValidationError::new(refusals::CREDIT_STEP_BUSY, reason).into()
}

fn currency_from(code: String) -> WabaCurrency {
    code.parse().unwrap_or(WabaCurrency::Other(code))
}

impl TokenVault {
    async fn read_sealed<T: serde::de::DeserializeOwned>(
        &self,
        key: &StoreKey,
    ) -> Result<Option<(T, SealedBlob, u64)>> {
        let Some(v) = self.kv().get(key).await? else {
            return Ok(None);
        };
        let blob: SealedBlob = decode_json(key, &v)?;
        let plain = self.open_blob(key, &blob)?;
        let value = serde_json::from_slice(&plain)
            .map_err(|_| CryptoError::Malformed("decrypted credit ledger record"))?;
        Ok(Some((value, blob, v.version)))
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
        let Some((plain, _, version)) = self.read_sealed::<CreditPlain>(&key).await? else {
            return Ok(None);
        };
        if &plain.waba_id != waba_id {
            return Err(CryptoError::Decrypt.into());
        }
        Ok(Some((
            StoredCredit {
                waba_id: plain.waba_id,
                business_id: plain.business_id,
                allocation_config_id: plain.allocation_config_id,
                currency: plain.currency.map(currency_from),
                shared_at: plain.shared_at,
            },
            version,
        )))
    }

    /// Write `credit`: create it (`expected` `None`) or replace version
    /// `expected`. Returns the new version; a concurrent write is
    /// [`refusals::CREDIT_STEP_BUSY`].
    pub(super) async fn put_credit(
        &self,
        credit: &StoredCredit,
        expected: Option<u64>,
    ) -> Result<u64> {
        let plain = CreditPlain {
            waba_id: credit.waba_id.clone(),
            business_id: credit.business_id.clone(),
            allocation_config_id: credit.allocation_config_id.clone(),
            currency: credit.currency.as_ref().map(|c| c.as_str().to_owned()),
            shared_at: credit.shared_at,
        };
        self.write_sealed(&credit_key(&credit.waba_id), &plain, expected)
            .await?
            .ok_or_else(|| busy("the WABA's credit record changed while this step ran"))
    }

    /// The revocation marker of `business_id`, if its credit line was
    /// revoked through [`EmbeddedSignup::revoke_credit_line`](super::EmbeddedSignup::revoke_credit_line)
    /// and not shared again since.
    pub async fn revoked_business(
        &self,
        business_id: &BusinessId,
    ) -> Result<Option<RevokedBusiness>> {
        let key = revoked_key(business_id);
        let Some((plain, _, _)) = self.read_sealed::<RevokedPlain>(&key).await? else {
            return Ok(None);
        };
        if &plain.business_id != business_id {
            return Err(CryptoError::Decrypt.into());
        }
        Ok(Some(RevokedBusiness {
            business_id: plain.business_id,
            revoked_at: plain.revoked_at,
            allocation_config_ids: plain.allocation_config_ids,
        }))
    }

    /// Record that `business_id`'s line is (being) revoked, adding `ids`.
    /// Keeps the first `revoked_at`.
    pub(super) async fn mark_revoked(
        &self,
        business_id: &BusinessId,
        ids: &[AllocationConfigId],
    ) -> Result<()> {
        let key = revoked_key(business_id);
        for _ in 0..MARK_ATTEMPTS {
            let current = self.read_sealed::<RevokedPlain>(&key).await?;
            let (mut plain, version) = match current {
                Some((plain, _, version)) => (plain, Some(version)),
                None => (
                    RevokedPlain {
                        business_id: business_id.clone(),
                        revoked_at: self.now(),
                        allocation_config_ids: Vec::new(),
                    },
                    None,
                ),
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
    /// re-share). Returns whether one was removed.
    pub(super) async fn clear_revoked(&self, business_id: &BusinessId) -> Result<bool> {
        Ok(self.kv().delete(&revoked_key(business_id)).await?)
    }

    /// Take `waba_id`'s credit lease for [`CREDIT_LEASE`]; its version, to
    /// release it. [`refusals::CREDIT_STEP_BUSY`] while another holds it.
    pub(super) async fn lease_credit(&self, waba_id: &WabaId) -> Result<u64> {
        let key = lease_key(waba_id);
        let stamp = serde_json::json!({ "at": self.now().unix_timestamp() });
        self.kv()
            .put_if_absent(
                &key,
                encode_json(&key, &stamp)?,
                Expiry::After(CREDIT_LEASE),
            )
            .await?
            .ok_or_else(|| {
                busy("another onboarding of this WABA is sharing its credit line; resume later")
            })
    }

    /// Release a lease taken by [`Self::lease_credit`] (only that one: a
    /// lease that expired and was taken again is left to its new holder).
    pub(super) async fn release_credit(&self, waba_id: &WabaId, version: u64) -> Result<()> {
        self.kv()
            .compare_and_swap(&lease_key(waba_id), version, None, Expiry::Keep)
            .await?;
        Ok(())
    }

    /// Re-seal the credit record of `waba_id`, and its business's
    /// revocation marker, under the active key. Whether anything was
    /// rewritten.
    pub(super) async fn rotate_ledger(&self, waba_id: &WabaId) -> Result<bool> {
        let key = credit_key(waba_id);
        let mut rewritten = false;
        let mut business = None;
        if let Some((plain, blob, version)) = self.read_sealed::<CreditPlain>(&key).await? {
            business.clone_from(&plain.business_id);
            if !self.is_current(&blob) {
                rewritten |= self
                    .write_sealed(&key, &plain, Some(version))
                    .await?
                    .is_some();
            }
        }
        if let Some(business) = business {
            let key = revoked_key(&business);
            if let Some((plain, blob, version)) = self.read_sealed::<RevokedPlain>(&key).await?
                && !self.is_current(&blob)
            {
                rewritten |= self
                    .write_sealed(&key, &plain, Some(version))
                    .await?
                    .is_some();
            }
        }
        Ok(rewritten)
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
        assert!(v.clear_revoked(&business).await.unwrap());
        assert!(v.revoked_business(&business).await.unwrap().is_none());
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

    /// The typed refusals, as callers test them.
    struct EmbeddedSignupRefusal;

    impl EmbeddedSignupRefusal {
        fn busy(err: &Error) -> bool {
            super::super::EmbeddedSignup::is_credit_step_busy(err)
        }
    }
}
