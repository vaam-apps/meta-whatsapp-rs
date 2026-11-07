//! Re-sealing the signing secrets (roadmap M2i; design D110): a walk over
//! every endpoint, enabled or not, that seals its secret and its previous
//! secret again under the active data key, so that a previous key can
//! leave `WA_SERVER_DATA_PREVIOUS_KEYS`.
//!
//! The walk is the one `vault rotate` is, for the data key:
//!
//! - **Batch by batch**: [`WebhookStore::all_endpoints`] a page at a time
//!   ([`MAX_PAGE_SIZE`]), one row's secrets at a time.
//! - **Each row written only if it is still the one read**
//!   ([`WebhookStore::reseal_secrets`], a compare-and-set on both sealed
//!   values): a rotation (or another walk) landing between the read and
//!   the write wins, and the row is counted as [`ResealReport::changed`], not
//!   retried; a repeat walk reads it anew.
//! - **Idempotent**: a secret already under the active key is read, not
//!   rewritten, so a second run re-seals nothing.
//! - **No secret leaves**: a plaintext secret lives inside
//!   [`DataKeys::reseal`] only; the report holds counts and endpoint ids.

use super::seal::{DataKeys, Resealed};
use super::store::{SealedSecrets, WebhookStore};
use super::{Endpoint, EndpointId};
use crate::model::{MAX_PAGE_SIZE, PageRequest};
use crate::store::StoreResult;

/// What a walk did. Counts, and the ids of endpoints it could not read:
/// never a secret.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResealReport {
    /// Endpoints walked.
    pub endpoints: u64,
    /// Endpoints whose secrets were sealed again and written.
    pub resealed: u64,
    /// Endpoints whose secrets were all under the active key already.
    pub current: u64,
    /// Endpoints left alone because their row changed between the read
    /// and the write (a rotation, mostly): run the walk again.
    pub changed: u64,
    /// Endpoints with a secret that no listed key opens (for that
    /// endpoint), or that is altered; what could be re-sealed of such an
    /// endpoint was. Keep the previous keys: one of them may open it once
    /// listed.
    pub failed: Vec<EndpointId>,
}

impl ResealReport {
    /// Whether every secret now opens under the active key alone, once
    /// no run is `changed` or `failed`: the previous keys can go.
    pub fn complete(&self) -> bool {
        self.changed == 0 && self.failed.is_empty()
    }
}

/// What one endpoint's secrets come to under `keys`: the replacement to
/// write (`None`: nothing to), and whether a secret was unreadable.
fn plan(keys: &DataKeys, endpoint: &Endpoint) -> (Option<SealedSecrets>, bool) {
    let mut unreadable = false;
    let mut changes = false;
    let mut reseal = |sealed: &super::SealedSecret| match keys.reseal(&endpoint.id, sealed) {
        Resealed::Current => sealed.clone(),
        Resealed::Sealed(new) => {
            changes = true;
            new
        }
        Resealed::Unreadable => {
            unreadable = true;
            sealed.clone()
        }
    };
    let secret = reseal(&endpoint.secret);
    let previous = endpoint.previous_secret.as_ref().map(&mut reseal);
    (
        changes.then_some(SealedSecrets { secret, previous }),
        unreadable,
    )
}

/// Re-seal `batch`, endpoints as they were read, under `keys`, adding to
/// `report`. Each row is written only if it is still `batch`'s version of
/// it. A storage failure stops the walk.
///
/// # Errors
///
/// The store failed.
pub async fn reseal_batch(
    store: &dyn WebhookStore,
    keys: &DataKeys,
    batch: &[Endpoint],
    report: &mut ResealReport,
) -> StoreResult<()> {
    for endpoint in batch {
        report.endpoints += 1;
        let (replacement, unreadable) = plan(keys, endpoint);
        if unreadable {
            tracing::warn!(
                endpoint_id = endpoint.id.as_str(),
                "a webhook secret is not opened by any listed data key"
            );
            report.failed.push(endpoint.id.clone());
        }
        let Some(replacement) = replacement else {
            if !unreadable {
                report.current += 1;
            }
            continue;
        };
        let expected = SealedSecrets {
            secret: endpoint.secret.clone(),
            previous: endpoint.previous_secret.clone(),
        };
        if store
            .reseal_secrets(&endpoint.id, &expected, &replacement)
            .await?
        {
            report.resealed += 1;
        } else {
            report.changed += 1;
        }
    }
    Ok(())
}

/// Walk every endpoint and re-seal its secrets under `keys`' active key.
///
/// # Errors
///
/// The store failed (the endpoints done stay done; run it again).
pub async fn reseal_secrets(
    store: &dyn WebhookStore,
    keys: &DataKeys,
) -> StoreResult<ResealReport> {
    let mut report = ResealReport::default();
    let mut page = PageRequest {
        after: None,
        limit: MAX_PAGE_SIZE,
    };
    loop {
        let listing = store.all_endpoints(&page).await?;
        reseal_batch(store, keys, &listing.items, &mut report).await?;
        let Some(after) = listing.next_after else {
            return Ok(report);
        };
        page.after = Some(after);
    }
}
