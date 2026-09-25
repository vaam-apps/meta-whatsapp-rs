//! The Solution Partner credit line node of the error tree.
//!
//! A credit line is money: the partner pays Meta for every message sent on
//! a line it shared, and an attached line cannot be taken back from a WABA.
//! So the credit steps (`wa_client::embedded_signup`, `wa_client::credit_lines`)
//! stop in ways a [`ValidationError`](super::ValidationError) cannot
//! describe: some are worth retrying (another onboarding holds the step),
//! some follow a request that did reach Meta (a share that raced a
//! revocation, a revocation that deleted some records and not others), and
//! a revocation that stopped part-way has a report the caller needs. Each
//! [`CreditError`] decides [`CreditError::is_retryable`] and
//! [`CreditError::may_have_been_sent`] explicitly.

use super::{Error, ErrorKind};
use crate::ids::{AllocationConfigId, BusinessId};

/// What a credit line revocation did: the records it deleted and the ones
/// it found already deleted. Returned by `CreditLines::revoke_for_business`
/// and `EmbeddedSignup::revoke_credit_line` (wa-client), and carried by
/// [`RevocationIncomplete`] when a revocation stops part-way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CreditRevocation {
    /// The customer business whose records were looked up, when known.
    pub business_id: Option<BusinessId>,
    /// Records this call deleted, each confirmed `DELETED` afterwards.
    pub revoked: Vec<AllocationConfigId>,
    /// Records already `DELETED` (by an earlier call, in Meta Business
    /// Suite, …), and records whose `DELETE` failed but that Meta then
    /// reported `DELETED`: nothing more to do for them.
    pub already_revoked: Vec<AllocationConfigId>,
}

impl CreditRevocation {
    /// An empty report for `business_id`.
    pub fn new(business_id: Option<BusinessId>) -> Self {
        Self {
            business_id,
            ..Self::default()
        }
    }

    /// Every record that is now revoked, by this call or before it.
    pub fn all(&self) -> impl Iterator<Item = &AllocationConfigId> {
        self.revoked.iter().chain(&self.already_revoked)
    }
}

/// A credit line revocation that did not finish: what it did, what it could
/// not do, and why. Call the revocation again to finish (what is already
/// revoked is skipped) when [`Self::is_retryable`]; otherwise check the
/// records named here in Meta Business Suite.
#[derive(Debug, thiserror::Error)]
#[error(
    "credit line revocation incomplete: revoked {revoked:?}, already revoked {already:?}, \
     failed {failed:?}, not confirmed yet {unconfirmed:?}, naming no business {unattributed:?}",
    revoked = .report.revoked,
    already = .report.already_revoked,
    failed = .failed,
    unconfirmed = .unconfirmed,
    unattributed = .unattributed
)]
#[non_exhaustive]
pub struct RevocationIncomplete {
    /// What was revoked, or found revoked, before it stopped.
    pub report: CreditRevocation,
    /// Records that are still active: their `DELETE` failed (and Meta does
    /// not report them `DELETED`), or they were not deleted because Meta
    /// says they are shared with another business than the one revoked.
    pub failed: Vec<AllocationConfigId>,
    /// Records Meta accepted a `DELETE` for but does not report `DELETED`
    /// yet. Call again.
    pub unconfirmed: Vec<AllocationConfigId>,
    /// Records the lookup returned that name no receiving business. Not
    /// revoked, because they could be another customer's: check them in
    /// Meta Business Suite.
    pub unattributed: Vec<AllocationConfigId>,
    /// Whether a `DELETE` this call sent may have taken effect (one
    /// succeeded, or one failed without proving Meta did nothing).
    pub deletes_sent: bool,
    /// The first failure behind it (a lookup, a `DELETE`, a record naming
    /// another business, the vault's ledger), if any. `None` when the only
    /// problems are [`Self::unconfirmed`] or [`Self::unattributed`] records.
    #[source]
    pub source: Option<Box<Error>>,
}

impl RevocationIncomplete {
    /// An incomplete revocation with `report` and nothing else recorded yet.
    pub fn new(report: CreditRevocation) -> Self {
        Self {
            report,
            failed: Vec::new(),
            unconfirmed: Vec::new(),
            unattributed: Vec::new(),
            deletes_sent: false,
            source: None,
        }
    }

    /// Calling the revocation again may finish it: no record names no
    /// business (those need a person), and the underlying failure, if any,
    /// is itself retryable. A record Meta has not confirmed `DELETED` yet
    /// is retryable.
    pub fn is_retryable(&self) -> bool {
        self.unattributed.is_empty() && self.source.as_ref().is_none_or(|e| e.is_retryable())
    }

    /// Whether this call changed, or may have changed, something on Meta's
    /// side: a `DELETE` went out, or the underlying failure may have been
    /// sent.
    pub fn may_have_been_sent(&self) -> bool {
        self.deletes_sent || self.source.as_ref().is_some_and(|e| e.may_have_been_sent())
    }
}

/// Why a Solution Partner credit line step stopped without Meta refusing
/// it. Reach it through [`Error::credit`] (which looks through
/// [`Error::Step`]). Each variant says whether repeating the call can help
/// ([`Self::is_retryable`]) and whether this call may have changed
/// something on Meta's side ([`Self::may_have_been_sent`]).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CreditError {
    /// The customer business had the credit line revoked: marked by a
    /// revocation, or only `DELETED` records on Meta's side. The line is
    /// not shared again unless the onboarding request opts in
    /// (`OnboardingRequest::reshare_after_revocation`). Not retryable.
    ///
    /// `posted` is `true` when this call's share went out and a revocation
    /// raced it: the new allocation was revoked at once (or, if that failed,
    /// is recorded for the next revocation), so reconcile before relying
    /// on the WABA's funding.
    #[error("{reason}")]
    Revoked {
        /// The business, when known.
        business_id: Option<BusinessId>,
        /// What revoked it.
        reason: String,
        /// Whether this call posted a share before noticing.
        posted: bool,
    },
    /// Another onboarding of the WABA holds the credit step (its lease), or
    /// changed its ledger record meanwhile: retry later (retryable).
    ///
    /// `posted` is `false` unless this call had already posted the first of
    /// the two-call method's requests (`whatsapp_credit_sharing`, whose
    /// allocation is then recorded) when it lost the lease before the
    /// attach: a later `resume` checks that allocation and attaches it
    /// without sharing again. Any other failure after a post is
    /// [`Self::Reconcile`] or [`Self::Revoked`], never this.
    #[error("credit line step busy: {reason}")]
    Busy {
        /// What was busy.
        reason: String,
        /// Whether this call posted a share before stopping.
        posted: bool,
    },
    /// Meta did not report the WABA's owner business
    /// (`owner_business_info`), so the line cannot be checked, shared or
    /// revoked for it. Nothing was posted. Not retryable.
    #[error("credit line not shared: {0}")]
    OwnerUnknown(String),
    /// A record of the line has a `request_status` Meta does not document
    /// (only `DELETED` is). It might be revoked, pending, or something new:
    /// nothing is shared unless the request opts in
    /// (`reshare_after_revocation`). Nothing was posted. Not retryable.
    #[error(
        "credit line not shared: allocation {allocation_config_id} has request_status `{status}`, which Meta does not document"
    )]
    StatusUnknown {
        /// The record.
        allocation_config_id: AllocationConfigId,
        /// Its status, verbatim.
        status: String,
    },
    /// Solution Partner mode shares a line only after the integrator
    /// approved the onboarding (`onboard_with_approval`,
    /// `resume_with_approval`). A programming error: nothing was sent. Not
    /// retryable.
    #[error("approval required: {0}")]
    ApprovalRequired(String),
    /// The ledger shows a share whose outcome is unknown (a post that timed
    /// out, a revocation that found nothing to revoke) and Meta's records
    /// cannot settle it: check the WABA's funding and the line's records in
    /// Meta Business Suite before anything else. `may_have_been_sent` is
    /// `true`: a share this deployment posted may be live. Not retryable.
    #[error("reconcile first: {0}")]
    Reconcile(String),
    /// A revocation stopped part-way; the report says what it did.
    #[error(transparent)]
    RevocationIncomplete(Box<RevocationIncomplete>),
}

impl CreditError {
    /// Classification, as [`Error::kind`] reports it: `ServiceUnavailable`
    /// for [`Self::Busy`] (try later), a revocation's underlying failure
    /// (`ServiceUnavailable` when only records Meta has not confirmed yet
    /// remain, `Unknown` when only unattributed ones do), and
    /// `InvalidParameter` for the refusals the request or the integrator
    /// must change.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Busy { .. } => ErrorKind::ServiceUnavailable,
            Self::RevocationIncomplete(r) => match &r.source {
                Some(source) => source.kind(),
                None if !r.unconfirmed.is_empty() => ErrorKind::ServiceUnavailable,
                None => ErrorKind::Unknown,
            },
            Self::Revoked { .. }
            | Self::OwnerUnknown(_)
            | Self::StatusUnknown { .. }
            | Self::ApprovalRequired(_)
            | Self::Reconcile(_) => ErrorKind::InvalidParameter,
        }
    }

    /// Whether the same call may succeed later: [`Self::Busy`], and a
    /// [`RevocationIncomplete`] that [says so](RevocationIncomplete::is_retryable).
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Busy { .. } => true,
            Self::RevocationIncomplete(r) => r.is_retryable(),
            Self::Revoked { .. }
            | Self::OwnerUnknown(_)
            | Self::StatusUnknown { .. }
            | Self::ApprovalRequired(_)
            | Self::Reconcile(_) => false,
        }
    }

    /// Whether something on Meta's side may have changed (see each
    /// variant): a raced share ([`Self::Revoked`] with `posted`), a share
    /// to reconcile, a revocation's `DELETE`s.
    pub fn may_have_been_sent(&self) -> bool {
        match self {
            Self::Revoked { posted, .. } | Self::Busy { posted, .. } => *posted,
            Self::Reconcile(_) => true,
            Self::RevocationIncomplete(r) => r.may_have_been_sent(),
            Self::OwnerUnknown(_) | Self::StatusUnknown { .. } | Self::ApprovalRequired(_) => false,
        }
    }

    /// The incomplete revocation, if this is one.
    pub fn revocation(&self) -> Option<&RevocationIncomplete> {
        match self {
            Self::RevocationIncomplete(r) => Some(r),
            _ => None,
        }
    }
}

impl From<RevocationIncomplete> for CreditError {
    fn from(r: RevocationIncomplete) -> Self {
        Self::RevocationIncomplete(Box::new(r))
    }
}

impl From<RevocationIncomplete> for Error {
    fn from(r: RevocationIncomplete) -> Self {
        Self::Credit(r.into())
    }
}
