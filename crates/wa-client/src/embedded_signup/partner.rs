//! Solution Partner onboarding: the credit line steps [`EmbeddedSignup`]
//! adds when a deployment is configured with [`SolutionPartner`], and
//! revocation and offboarding from what onboarding recorded.
//!
//! Docs: `embedded-signup/onboarding-customers-as-a-solution-partner`
//! (step order: subscribe the app, share the credit line, register the
//! number), `solution-providers/share-and-revoke-credit-lines`,
//! `solution-providers/manage-system-users`.

use std::fmt;

use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
use wa_core::error::{CreditError, ValidationError};
use wa_core::ids::{AllocationConfigId, BusinessId, CreditLineId, FundingId, SystemUserId, WabaId};
use wa_core::secret::AccessToken;
use wa_core::{Error, Result};

use super::EmbeddedSignup;
use super::ledger::{ClearedShare, StoredCredit};
use super::onboard::steps::{DELETE_TOKEN, REVOKE_CREDIT_LINE};
use super::vault::{StoredBusinessToken, TokenVault};
use crate::Client;
use crate::credit_lines::{
    CreditLines, CreditRevocation, WabaCurrency, WabaFunding, finish, is_shared, owned_by,
};
use crate::waba::WabaTask;

/// How the credit line is shared with each onboarded customer
/// (`solution-providers/share-and-revoke-credit-lines`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum CreditSharing {
    /// Meta's current method: add the partner's system user to the
    /// customer's WABA (`assign_system_user`), then one
    /// `whatsapp_credit_sharing_and_attach` call with the system user token.
    #[default]
    ShareAndAttach,
    /// Meta's "alternate method", being tested to replace the first:
    /// `whatsapp_credit_sharing` with the customer's verified business
    /// portfolio id (system user token), then `whatsapp_credit_attach`
    /// (the customer's business token). No system user step: Meta does not
    /// say whether this method needs it.
    ShareThenAttach,
}

/// Onboard as a **Solution Partner**: every customer onboarded through
/// Embedded Signup is funded by your credit line. Configured once per
/// deployment with [`EmbeddedSignup::solution_partner`]; without it,
/// onboarding is the Tech Provider flow (the customer adds a payment
/// method).
///
/// The system user token is private to this struct and never shown by
/// `Debug`.
#[derive(Clone)]
#[non_exhaustive]
pub struct SolutionPartner {
    /// Your system user access token: `business_management`, and an Admin
    /// or Financial Editor role on your business portfolio. Used for every
    /// credit line call except the attach of [`CreditSharing::ShareThenAttach`],
    /// and to add the system user to the customer's WABA.
    system_token: AccessToken,
    /// The id of the system user behind the system token, added to the
    /// customer's WABA before [`CreditSharing::ShareAndAttach`].
    pub system_user_id: SystemUserId,
    /// Your extended credit line
    /// ([`CreditLines::list`](crate::credit_lines::CreditLines::list)).
    pub credit_line_id: CreditLineId,
    /// How the line is shared.
    pub method: CreditSharing,
    /// The currency when an [`OnboardingRequest`](super::OnboardingRequest)
    /// names none. Take it from your billing records for the merchant,
    /// never from the browser: it sets the prices Meta charges you, and a
    /// line cannot change once attached.
    pub default_currency: Option<WabaCurrency>,
    /// Tasks granted to the system user on the customer's WABA
    /// (default `[MANAGE]`). Under a Multi-Partner Solution without
    /// `MESSAGING`, `MANAGE` is refused: pass granular tasks including
    /// `MANAGE_BILLING`, which credit sharing needs
    /// (`solution-providers/manage-system-users`).
    pub system_user_tasks: Vec<WabaTask>,
}

impl fmt::Debug for SolutionPartner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SolutionPartner")
            .field("system_token", &self.system_token)
            .field("system_user_id", &self.system_user_id)
            .field("credit_line_id", &self.credit_line_id)
            .field("method", &self.method)
            .field("default_currency", &self.default_currency)
            .field("system_user_tasks", &self.system_user_tasks)
            .finish()
    }
}

impl SolutionPartner {
    /// Share `credit_line_id` with [`CreditSharing::ShareAndAttach`],
    /// granting `system_user_id` `MANAGE` on each customer's WABA, with no
    /// default currency.
    pub fn new(
        system_token: AccessToken,
        system_user_id: impl Into<SystemUserId>,
        credit_line_id: impl Into<CreditLineId>,
    ) -> Self {
        Self {
            system_token,
            system_user_id: system_user_id.into(),
            credit_line_id: credit_line_id.into(),
            method: CreditSharing::default(),
            default_currency: None,
            system_user_tasks: vec![WabaTask::Manage],
        }
    }

    /// Share with `method`.
    #[must_use]
    pub fn method(mut self, method: CreditSharing) -> Self {
        self.method = method;
        self
    }

    /// The currency of customers whose onboarding request names none.
    #[must_use]
    pub fn default_currency(mut self, currency: WabaCurrency) -> Self {
        self.default_currency = Some(currency);
        self
    }

    /// Grant these tasks to the system user instead of `MANAGE`.
    #[must_use]
    pub fn system_user_tasks(mut self, tasks: impl IntoIterator<Item = WabaTask>) -> Self {
        self.system_user_tasks = tasks.into_iter().collect();
        self
    }

    fn validate(&self) -> Result<(), ValidationError> {
        if self.system_token.expose_secret().trim().is_empty() {
            return Err(ValidationError::new("system_token", "required"));
        }
        if self.credit_line_id.as_str().trim().is_empty() {
            return Err(ValidationError::new("credit_line_id", "required"));
        }
        if self.method == CreditSharing::ShareAndAttach {
            if self.system_user_id.as_str().trim().is_empty() {
                return Err(ValidationError::new(
                    "system_user_id",
                    "required: the system user is added to the customer's WABA before sharing",
                ));
            }
            if self.system_user_tasks.is_empty() {
                return Err(ValidationError::new(
                    "system_user_tasks",
                    "at least one task is required",
                ));
            }
        }
        if let Some(currency) = &self.default_currency {
            currency.validate()?;
        }
        Ok(())
    }
}

/// What the credit steps of one onboarding need, checked before any request.
pub(super) struct CreditPlan<'a> {
    pub(super) partner: &'a SolutionPartner,
    pub(super) currency: WabaCurrency,
    /// The request's explicit consent to fund a business whose line was
    /// revoked.
    pub(super) reshare_after_revocation: bool,
}

/// What [`EmbeddedSignup::offboard`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Offboarded {
    /// The WABA.
    pub waba_id: WabaId,
    /// The credit line revocation (Solution Partner mode), done before the
    /// token was deleted. `None` for a Tech Provider, and in Solution
    /// Partner mode when nothing names the WABA's business and the credit
    /// ledger shows nothing was ever shared with it.
    pub credit: Option<CreditRevocation>,
    /// Whether a stored token was deleted (`false` if it already was).
    pub token_deleted: bool,
}

/// What [`EmbeddedSignup::clear_pending_share`] found on Meta's side, and
/// whether it cleared the pending share.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
#[must_use = "a pending share is cleared only when this is `Cleared`"]
pub enum PendingShareClearance {
    /// Meta shows no record of your line that may be live for the WABA's
    /// owner business: the pending share is cleared, and this entry was
    /// appended to [`StoredCredit::cleared_shares`].
    Cleared(ClearedShare),
    /// **Nothing was cleared**: Meta shows your line funding the WABA, a
    /// record for its owner business that may be live, or a funding of the
    /// WABA that no record of your line explains and that the call did not
    /// acknowledge ([`SharesFound::unexplained_funding`]). A record that
    /// funds the WABA was recorded in the ledger, as `resume` records a
    /// share it finds.
    NotCleared(SharesFound),
}

/// What Meta showed [`EmbeddedSignup::clear_pending_share`] that stopped it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SharesFound {
    /// The WABA's owner business the records were looked up for.
    pub business_id: BusinessId,
    /// The record whose receiving credential is the WABA's
    /// `primary_funding_id`: your line funds the WABA. Now
    /// [`StoredCredit::allocation_config_id`]; the pending share stays until
    /// a revocation revokes that record or `resume` settles it.
    pub funding: Option<AllocationConfigId>,
    /// Active records naming the owner business (the recorded allocation
    /// included). One may fund another WABA of the business, or be the
    /// lost share not showing in the WABA's funding yet: Meta does not say
    /// which.
    pub active: Vec<AllocationConfigId>,
    /// Records whose `request_status` Meta does not document, with that
    /// value verbatim: they may be live. (`funding` is one of these or of
    /// `active`.)
    pub unknown_status: Vec<(AllocationConfigId, String)>,
    /// Records Meta's lookup for the owner business returned without
    /// naming a receiving business: they may be the lost share, or another
    /// customer's, and are not attributed (as a revocation leaves them to a
    /// person).
    pub unattributed: Vec<AllocationConfigId>,
    /// The WABA's `primary_funding_id` as read.
    pub primary_funding_id: Option<FundingId>,
}

impl SharesFound {
    /// The funding to acknowledge: the WABA's `primary_funding_id` when it
    /// alone stopped the clearance (no record of your line may be live, and
    /// none explains it). It may be the lost share itself, applied while
    /// Meta's lookup does not list it yet, which is why it is never cleared
    /// on its own; or the merchant's own payment method, or another
    /// partner's line. Once someone has seen in Meta Business Suite that
    /// what pays for the WABA is **not** your credit line, call
    /// [`EmbeddedSignup::clear_pending_share`] again with this id as
    /// `acknowledged_funding`.
    pub fn unexplained_funding(&self) -> Option<&FundingId> {
        let nothing_live = self.funding.is_none()
            && self.active.is_empty()
            && self.unknown_status.is_empty()
            && self.unattributed.is_empty();
        self.primary_funding_id.as_ref().filter(|_| nothing_live)
    }
}

impl EmbeddedSignup {
    /// Onboard as a Solution Partner with `partner` (one choice per
    /// deployment): [`Self::onboard_with_approval`] and [`Self::resume`]
    /// then add the credit line steps between `subscribe_app` and
    /// `register_phone`, following Meta's Solution Partner onboarding
    /// order, and plain [`Self::onboard`] is refused. See the
    /// [module docs](super#solution-partner-mode).
    #[must_use]
    pub fn solution_partner(mut self, partner: SolutionPartner) -> Self {
        self.partner = Some(partner);
        self
    }

    /// The Solution Partner configuration, if this deployment is one.
    pub fn partner(&self) -> Option<&SolutionPartner> {
        self.partner.as_ref()
    }

    /// Whether `error` (or the step error wrapping it) is onboarding's
    /// refusal to fund a business whose credit line was revoked
    /// ([`CreditError::Revoked`]; `posted` says whether a share raced the
    /// revocation and was revoked at once).
    pub fn is_credit_line_revoked(error: &Error) -> bool {
        matches!(error.credit(), Some(CreditError::Revoked { .. }))
    }

    /// Whether `error` (or the step error wrapping it) says another
    /// onboarding of the WABA holds the credit step
    /// ([`CreditError::Busy`]): resume later.
    pub fn is_credit_step_busy(error: &Error) -> bool {
        matches!(error.credit(), Some(CreditError::Busy { .. }))
    }

    /// The credit plan for `currency` (the request's), or `None` for a Tech
    /// Provider. Every refusal here happens before any request.
    pub(super) fn credit_plan(
        &self,
        currency: Option<&WabaCurrency>,
        reshare_after_revocation: bool,
    ) -> Result<Option<CreditPlan<'_>>, ValidationError> {
        let Some(partner) = &self.partner else {
            return match (currency, reshare_after_revocation) {
                (None, false) => Ok(None),
                (Some(_), _) => Err(ValidationError::new(
                    "currency",
                    "only used when onboarding as a Solution Partner (EmbeddedSignup::solution_partner)",
                )),
                (None, true) => Err(ValidationError::new(
                    "reshare_after_revocation",
                    "only used when onboarding as a Solution Partner (EmbeddedSignup::solution_partner)",
                )),
            };
        };
        partner.validate()?;
        let currency = currency
            .or(partner.default_currency.as_ref())
            .cloned()
            .ok_or_else(|| {
                ValidationError::new(
                    "currency",
                    "required to share the credit line: set OnboardingRequest::currency or SolutionPartner::default_currency",
                )
            })?;
        currency.validate()?;
        Ok(Some(CreditPlan {
            partner,
            currency,
            reshare_after_revocation,
        }))
    }

    /// The partner's client (system user token).
    fn system_client(&self, partner: &SolutionPartner) -> Client {
        self.client.with_token(partner.system_token.clone())
    }

    fn require_partner(&self) -> Result<&SolutionPartner> {
        let partner = self.partner.as_ref().ok_or_else(|| {
            ValidationError::new(
                "solution_partner",
                "this EmbeddedSignup is not configured as a Solution Partner",
            )
        })?;
        partner.validate()?;
        Ok(partner)
    }

    /// Revoke your credit line from the customer business that owns
    /// `waba_id`, and report what was revoked.
    ///
    /// Use it when the customer unshares the WABA or removes you as a
    /// partner (`account_update` `PARTNER_REMOVED`: messaging on the WABA is
    /// then blocked, `owner_business_info` can no longer be read, and Meta
    /// recommends revoking at once), or through [`Self::offboard`]. Revoking
    /// applies to **every** WABA of that business shared with you.
    ///
    /// Call it at once on **every** `PARTNER_REMOVED` of your solution,
    /// including a coexistence one with `disconnection_info` whose number
    /// may reconnect (the owner's decision, 2026-09-25): a merchant who
    /// reconnects onboards again, and funding them again takes
    /// [`OnboardingRequest::reshare_after_revocation`](super::OnboardingRequest::reshare_after_revocation).
    /// Nothing calls it for you.
    ///
    /// - **The business** is the one Meta reported at onboarding: from the
    ///   token record, else the credit ledger (which outlives the token),
    ///   else the one Meta's own record of the allocation in the ledger
    ///   names (`receiving_business`). Only when none of these exists is
    ///   `owner_business_id` used, and then only if your credit line has
    ///   records naming it: pass the `waba_info.owner_business_id` of a
    ///   `PARTNER_*` webhook **whose signature was checked**, never an id
    ///   from a browser or a request body. If a recorded business and
    ///   `owner_business_id` differ, nothing is revoked (a validation error
    ///   on `owner_business_id`): revoking another customer's line is worse
    ///   than an error.
    /// - **The business is marked revoked first** (a sealed marker in the
    ///   vault, replacing an unreadable one), before anything is looked up,
    ///   so neither `resume` nor a new onboarding funds it again without
    ///   [`OnboardingRequest::reshare_after_revocation`](super::OnboardingRequest::reshare_after_revocation),
    ///   even if a request below fails. A share posted while this runs sees
    ///   the marker after its post (also when its answer was lost) and
    ///   revokes what it may have made; when it cannot find that, it
    ///   reports it ([`CreditError::Reconcile`]) and keeps it pending. A
    ///   caller's business whose check failed is marked once this call's
    ///   own lookup finds records naming it. The business is also written
    ///   into the WABA's credit record when it names none.
    /// - Every active record naming the business is revoked, plus the
    ///   allocation recorded in the ledger, each confirmed `DELETED`; see
    ///   [`CreditLines::revoke_for_business`] for what is skipped. An
    ///   unreadable token or credit record, a failed lookup or a failed
    ///   marker write does not stop what the other sources can revoke.
    ///   Anything left undone is [`CreditError::RevocationIncomplete`], with
    ///   the report; a ledger write that failed is its `ledger`, and does
    ///   not make it unretryable. Safe to repeat.
    /// - **A share whose outcome is unknown** (the WABA's
    ///   [`StoredCredit::pending_share`]: an answer lost, or a share still
    ///   in flight) is settled only by a record revoked by this call: when
    ///   none is, the call is [`CreditError::RevocationIncomplete`] with
    ///   `share_pending` (retryable), never done, because that share may be
    ///   live and not listed by Meta yet. Call again; if it keeps revoking
    ///   nothing, check the WABA's funding in Meta Business Suite, and when
    ///   the share is not there, clear it with [`Self::clear_pending_share`].
    ///
    /// The token and the ledger are left in the vault; [`Self::offboard`]
    /// deletes the token after revoking.
    pub async fn revoke_credit_line(
        &self,
        waba_id: &WabaId,
        owner_business_id: Option<&BusinessId>,
        vault: &TokenVault,
    ) -> Result<CreditRevocation> {
        self.revoke_waba(waba_id, owner_business_id, vault)
            .await?
            .ok_or_else(|| {
                ValidationError::new(
                    "business_id",
                    "nothing recorded for this WABA and no owner business given: pass the owner_business_id of a signed PARTNER_* webhook (or from Meta Business Suite)",
                )
                .into()
            })
    }

    /// Revoke your credit line from `business_id` when no WABA is known:
    /// a signed `PARTNER_REMOVED` whose `waba_info` names the owner business
    /// but no WABA. As with the `owner_business_id` of
    /// [`Self::revoke_credit_line`], the id must come from a delivery whose
    /// signature was checked; the business is marked revoked only if your
    /// credit line has records naming it (otherwise nothing is written and
    /// an empty report is returned).
    pub async fn revoke_business_credit_line(
        &self,
        business_id: &BusinessId,
        vault: &TokenVault,
    ) -> Result<CreditRevocation> {
        let partner = self.require_partner()?;
        if business_id.as_str().trim().is_empty() {
            return Err(ValidationError::new("business_id", "required").into());
        }
        self.revoke_resolved(
            partner,
            vault,
            Revocation {
                business: Some(business_id.clone()),
                corroborated: false,
                known: Vec::new(),
                waba: None,
            },
        )
        .await
    }

    /// Offboard a merchant: in Solution Partner mode revoke the credit line
    /// first ([`Self::revoke_credit_line`], step `revoke_credit_line`), then
    /// delete the token and its phone index (step `delete_token`). A Tech
    /// Provider only deletes.
    ///
    /// For a merchant who disconnects in your CMS, and for
    /// `PARTNER_APP_UNINSTALLED` **of your own app** (compare its
    /// `waba_info.partner_app_id` with your app id first; pass its
    /// `waba_info.owner_business_id`). If revocation fails nothing is
    /// deleted, so the call can be repeated with everything it needs; it is
    /// safe to repeat after success too, and in any order with a
    /// `PARTNER_REMOVED` revocation: the credit ledger outlives the token.
    /// Stop the app's webhooks first if the token still works
    /// (`waba(..).unsubscribe_app()` with it).
    ///
    /// - When nothing names the WABA's business and the credit ledger shows
    ///   nothing was ever shared with it (a WABA onboarded before this
    ///   deployment became a Solution Partner, or whose share never ran),
    ///   there is nothing to revoke: the token is deleted and
    ///   [`Offboarded::credit`] is `None`.
    /// - When the ledger shows a share ([`StoredCredit::records_a_share`])
    ///   but revocation found no record of it, or the ledger cannot be
    ///   read, nothing is deleted: [`CreditError::Reconcile`]. Check the
    ///   line in Meta Business Suite, then delete the token yourself
    ///   ([`TokenVault::delete`]).
    /// - When the ledger shows a share whose outcome is unknown and the
    ///   revocation revoked nothing (old `DELETED` records say nothing
    ///   about it), nothing is deleted either:
    ///   [`CreditError::RevocationIncomplete`] with `share_pending`,
    ///   retryable (see [`Self::revoke_credit_line`] and
    ///   [`Self::clear_pending_share`]).
    /// - When the WABA's business is recorded (in its token record or the
    ///   ledger), it is **marked revoked even if nothing was ever shared with
    ///   it** (a WABA onboarded in Tech Provider mode, say): the marker is
    ///   written before anything is looked up, which is what stops a share
    ///   racing this call from surviving it. The cost: onboarding that
    ///   business in Solution Partner mode later takes
    ///   [`OnboardingRequest::reshare_after_revocation`](super::OnboardingRequest::reshare_after_revocation)
    ///   (the owner's decision, 2026-09-25).
    pub async fn offboard(
        &self,
        waba_id: &WabaId,
        owner_business_id: Option<&BusinessId>,
        vault: &TokenVault,
    ) -> Result<Offboarded> {
        let credit = match &self.partner {
            Some(_) => self
                .offboard_credit(waba_id, owner_business_id, vault)
                .await
                .map_err(|e| e.in_step(REVOKE_CREDIT_LINE))?,
            None => None,
        };
        let token_deleted = vault
            .delete(waba_id)
            .await
            .map_err(|e| e.in_step(DELETE_TOKEN))?;
        Ok(Offboarded {
            waba_id: waba_id.clone(),
            credit,
            token_deleted,
        })
    }

    async fn offboard_credit(
        &self,
        waba_id: &WabaId,
        owner_business_id: Option<&BusinessId>,
        vault: &TokenVault,
    ) -> Result<Option<CreditRevocation>> {
        // Read before revoking; an unreadable record cannot say nothing was
        // shared.
        let shared = match vault.credit(waba_id).await {
            Ok(credit) => credit.as_ref().is_some_and(StoredCredit::records_a_share),
            Err(e) => {
                tracing::warn!(waba_id = %waba_id, kind = ?e.kind(), "offboarding: credit record unreadable");
                true
            }
        };
        let report = self.revoke_waba(waba_id, owner_business_id, vault).await?;
        let found = report.as_ref().is_some_and(|r| r.all().next().is_some());
        if shared && !found {
            return Err(CreditError::Reconcile(
                "the credit ledger records a share for this WABA (or cannot be read), but revocation found no record of your line to revoke; the token is kept: check the line's records and the WABA's funding in Meta Business Suite, then delete the token with TokenVault::delete".into(),
            )
            .into());
        }
        Ok(report)
    }

    /// Clear the pending share of `waba_id` ([`StoredCredit::pending_share`]:
    /// a credit line post whose answer was lost) once Meta does not show
    /// it, recording who cleared it. An operator's call, after checking the
    /// WABA's funding in Meta Business Suite: nothing calls it for you.
    ///
    /// Until it is cleared, or a revocation revokes a record of that share,
    /// every [`Self::revoke_credit_line`] of the WABA is
    /// [`CreditError::RevocationIncomplete`] with `share_pending`,
    /// [`Self::offboard`] keeps the token, and onboarding the WABA is
    /// [`CreditError::Reconcile`] when something funds it. A post that never
    /// reached Meta is never listed, so without this call it stays pending.
    ///
    /// It posts nothing. Holding the WABA's credit lease (so no share runs
    /// meanwhile: [`CreditError::Busy`] while one does), it checks Meta
    /// first, as `share_credit_line` does before it posts: your line's
    /// records for the WABA's owner business
    /// (`owning_credit_allocation_configs`) and the allocation recorded in
    /// the ledger, each with its `request_status`, and the WABA's
    /// `primary_funding_id` (with the merchant's stored token, which Meta
    /// requires; none stored is a validation error on `waba_id`, and a
    /// token Meta refuses is its error: either way nothing is cleared).
    /// That read stays mandatory: it is what tells a lost share from
    /// nothing. After `PARTNER_REMOVED` the merchant's token may no longer
    /// read the WABA; the way out is a working token (a merchant who
    /// connects again stores one at the `store_token` step, even when the
    /// credit step then refuses the revoked business). Without one the flag
    /// stays: revocations of the WABA keep answering `share_pending`
    /// (retryable; stop retrying once Meta Business Suite shows no record
    /// of your line for the business). wa-rs has no call that edits the
    /// sealed record otherwise, and a hand edit of the store makes it
    /// unreadable (then treated as a share by `offboard`): ask the wa-rs
    /// maintainers rather than editing it.
    ///
    /// - An active record, one whose `request_status` Meta does not
    ///   document, or one the lookup returned naming no business, means the
    ///   share may be live: nothing is cleared, and
    ///   [`PendingShareClearance::NotCleared`] says what was found. A record
    ///   whose receiving credential is the WABA's `primary_funding_id` is
    ///   recorded as the WABA's allocation, as `resume` would record it (a
    ///   revocation then revokes it, which settles the pending share). Any
    ///   active record stops it, also one funding another WABA of the same
    ///   business: Meta does not say which WABA a record funds.
    /// - A `primary_funding_id` that no record explains stops it too
    ///   (`NotCleared`, [`SharesFound::unexplained_funding`]) unless
    ///   `acknowledged_funding` is exactly that id. It may be the lost
    ///   share itself, applied while Meta's lookup does not list it yet (a
    ///   share treats the same state as [`CreditError::Reconcile`]); the
    ///   library cannot tell it from the merchant's own payment method or
    ///   another partner's line. Pass `None` first; acknowledge the id the
    ///   refusal reports only once someone has seen in Meta Business Suite
    ///   that what pays for the WABA is not your credit line.
    /// - Otherwise the pending share is cleared and a [`ClearedShare`]
    ///   (`cleared_by`, the time, the pending share's time and the
    ///   `primary_funding_id` Meta showed, which is `Some` only when it was
    ///   acknowledged) is appended to [`StoredCredit::cleared_shares`],
    ///   sealed with the record: [`PendingShareClearance::Cleared`].
    ///   Revocation and offboarding then behave as if the share had never
    ///   been posted, and `resume` checks and shares again.
    ///
    /// Meta documents no delay after which a share that went through is
    /// listed: an operator should let time pass since the pending share's
    /// [`StoredCredit::pending_share`] before clearing it (the library
    /// imposes no minimum).
    ///
    /// **Operator-only.** Never route this call from a handler a merchant
    /// can reach, and take `cleared_by` from your authenticated staff
    /// session, never from the request. Prefer an opaque operator id to a
    /// name or an email: it is personal data kept, sealed, for as long as
    /// the WABA's credit record, which outlives the token (wa-rs never logs
    /// it, and [`ClearedShare`]'s `Debug` redacts it). It is trimmed, and
    /// refused when blank, longer than [`MAX_CLEARED_BY_CHARS`] characters,
    /// or containing a control, format (U+200B, U+FEFF, bidi controls, …)
    /// or line separator character.
    ///
    /// Refused before anything is sent: outside Solution Partner mode (or
    /// with an invalid [`SolutionPartner`]), with an invalid `cleared_by`,
    /// and when the ledger shows no pending share for the WABA (a
    /// validation error on `pending_share`; the lease taken meanwhile is
    /// released). A failed lookup, a record naming another business, a
    /// lease lost during the check, or a credit record written meanwhile (a
    /// share, a revocation, a key rotation) clear nothing: the error is
    /// returned, and the call can be repeated.
    pub async fn clear_pending_share(
        &self,
        waba_id: &WabaId,
        cleared_by: &str,
        acknowledged_funding: Option<&FundingId>,
        vault: &TokenVault,
    ) -> Result<PendingShareClearance> {
        let partner = self.require_partner()?;
        let cleared_by = cleared_by.trim();
        check_cleared_by(cleared_by)?;
        let mut lease = vault.lease_credit(waba_id).await?;
        let outcome = self
            .clear_leased(
                partner,
                waba_id,
                cleared_by,
                acknowledged_funding,
                vault,
                &mut lease,
            )
            .await;
        if let Err(e) = vault.release_credit(waba_id, lease).await {
            tracing::warn!(waba_id = %waba_id, kind = ?e.kind(), "credit lease not released");
        }
        outcome
    }

    async fn clear_leased(
        &self,
        partner: &SolutionPartner,
        waba_id: &WabaId,
        cleared_by: &str,
        acknowledged_funding: Option<&FundingId>,
        vault: &TokenVault,
        lease: &mut u64,
    ) -> Result<PendingShareClearance> {
        let nothing_pending = || -> Error {
            ValidationError::new(
                "pending_share",
                "the WABA's credit ledger shows no share without a recorded outcome; nothing to clear",
            )
            .into()
        };
        let Some((credit, version)) = vault.credit_versioned(waba_id).await? else {
            return Err(nothing_pending());
        };
        let Some(pending_since) = credit.pending_share else {
            return Err(nothing_pending());
        };
        let token = vault.get(waba_id).await?.ok_or_else(|| {
            ValidationError::new(
                "waba_id",
                "no business token is stored for this WABA: its primary_funding_id, which Meta serves to the merchant's token, cannot be checked; nothing cleared",
            )
        })?;
        let owner = clearance_owner(&credit, &token)?;

        // What Meta shows: the records naming the owner (and the recorded
        // allocation), and what funds the WABA.
        let system = self.system_client(partner).credit_lines();
        let found = records(
            &system,
            &partner.credit_line_id,
            &owner,
            credit.allocation_config_id.as_ref(),
        )
        .await?;
        let funding = self
            .client
            .with_token(token.token.clone())
            .credit_lines()
            .primary_funding(waba_id)
            .await?;

        // A record that may be live: never cleared.
        if !found.active.is_empty() || !found.unknown.is_empty() || !found.unattributed.is_empty() {
            let mut funds = None;
            let live = found
                .active
                .iter()
                .chain(found.unknown.iter().map(|(id, _)| id));
            for candidate in live {
                if is_shared(&system.receiving_credential(candidate).await?, &funding) {
                    funds = Some(candidate.clone());
                    break;
                }
            }
            if let Some(allocation) = &funds {
                renew_clearance(vault, waba_id, lease).await?;
                record_found(vault, waba_id, &owner, allocation).await?;
            }
            return Ok(PendingShareClearance::NotCleared(SharesFound {
                business_id: owner,
                funding: funds,
                active: found.active,
                unknown_status: found.unknown,
                unattributed: found.unattributed,
                primary_funding_id: funding.primary_funding_id,
            }));
        }

        // No record may be live, but something funds the WABA that none of
        // them explains: it may be the lost share itself, applied while
        // Meta's lookup does not list it yet (a share calls the same state
        // `Reconcile`). Cleared only when the operator acknowledged exactly
        // that funding as not the line.
        if funding
            .primary_funding_id
            .as_ref()
            .is_some_and(|shown| acknowledged_funding != Some(shown))
        {
            return Ok(PendingShareClearance::NotCleared(SharesFound {
                business_id: owner,
                funding: None,
                active: Vec::new(),
                unknown_status: Vec::new(),
                unattributed: Vec::new(),
                primary_funding_id: funding.primary_funding_id,
            }));
        }

        // Nothing may be live: clear, if no share took the lease meanwhile
        // (the compare-and-swap below also refuses a record written since).
        renew_clearance(vault, waba_id, lease).await?;
        let cleared = ClearedShare {
            pending_since,
            cleared_at: vault.now(),
            cleared_by: cleared_by.to_owned(),
            primary_funding_id: funding.primary_funding_id,
        };
        vault
            .clear_pending_share(&credit, version, cleared.clone())
            .await?;
        tracing::info!(waba_id = %waba_id, "pending credit line share cleared by an operator");
        Ok(PendingShareClearance::Cleared(cleared))
    }

    /// Resolve whose line to revoke for `waba_id` and revoke it. `None`
    /// when nothing addresses anything: no business anywhere, no allocation
    /// recorded, and every record readable.
    async fn revoke_waba(
        &self,
        waba_id: &WabaId,
        owner_business_id: Option<&BusinessId>,
        vault: &TokenVault,
    ) -> Result<Option<CreditRevocation>> {
        let partner = self.require_partner()?;
        // An unreadable record (a key dropped too early, tampering) must not
        // stop a revocation another source can still address: it is logged
        // and skipped, and only returned when nothing else is left.
        let mut unreadable = None;
        let token = vault.get(waba_id).await.unwrap_or_else(|e| {
            tracing::warn!(waba_id = %waba_id, kind = ?e.kind(), "revocation: token record unreadable");
            unreadable = Some(e);
            None
        });
        let credit = vault.credit(waba_id).await.unwrap_or_else(|e| {
            tracing::warn!(waba_id = %waba_id, kind = ?e.kind(), "revocation: credit record unreadable");
            unreadable.get_or_insert(e);
            None
        });
        let stored = token
            .and_then(|t| t.business_id)
            .or_else(|| credit.as_ref().and_then(|c| c.business_id.clone()));
        let contradicts = |recorded: &BusinessId| -> Result<()> {
            match owner_business_id {
                Some(hint) if hint != recorded => Err(ValidationError::new(
                    "owner_business_id",
                    "differs from the owner business recorded for this WABA; nothing revoked",
                )
                .into()),
                _ => Ok(()),
            }
        };
        let known: Vec<AllocationConfigId> = credit
            .and_then(|c| c.allocation_config_id)
            .into_iter()
            .collect();
        let (business, corroborated) = if let Some(stored) = stored {
            contradicts(&stored)?;
            (Some(stored), true)
        } else {
            // Meta's own record of the allocation we recorded names the
            // business it is shared with.
            let named = match known.first() {
                Some(id) => match self
                    .system_client(partner)
                    .credit_lines()
                    .allocation_status(id)
                    .await
                {
                    Ok(status) => status.receiving_business.and_then(|b| b.id),
                    Err(e) => {
                        tracing::warn!(waba_id = %waba_id, kind = ?e.kind(), "revocation: recorded allocation unreadable");
                        None
                    }
                },
                None => None,
            };
            match named {
                Some(named) => {
                    contradicts(&named)?;
                    (Some(named), true)
                }
                None => (owner_business_id.cloned(), false),
            }
        };
        if business.is_none() && known.is_empty() {
            return match unreadable {
                Some(e) => Err(e),
                None => Ok(None),
            };
        }
        self.revoke_resolved(
            partner,
            vault,
            Revocation {
                business,
                corroborated,
                known,
                waba: Some(waba_id),
            },
        )
        .await
        .map(Some)
    }

    /// Mark, then look up and revoke, then record what was revoked.
    async fn revoke_resolved(
        &self,
        partner: &SolutionPartner,
        vault: &TokenVault,
        mut r: Revocation<'_>,
    ) -> Result<CreditRevocation> {
        let system = self.system_client(partner).credit_lines();
        let line = &partner.credit_line_id;
        // 1. A business only the caller named is trusted, and marked, once
        // the line has records naming it.
        if let (Some(business), false) = (&r.business, r.corroborated) {
            match system.allocations_for(line, business).await {
                Ok(records) => {
                    if !owned_by(records, business).is_empty() {
                        r.corroborated = true;
                    } else if r.known.is_empty() {
                        return Ok(CreditRevocation::new(Some(business.clone())));
                    }
                }
                // Cannot tell yet: marked after the revocation's own lookup
                // if that one attributes a record to it (step 5).
                Err(e) => tracing::warn!(kind = ?e.kind(), "revocation: lookup failed"),
            }
        }
        // 2. Mark before looking anything up: a share posted meanwhile reads
        // the marker after its post, so either it sees the marker or the
        // lookup below sees its record.
        let mut ledger: Option<Error> = None;
        let mut marked = false;
        if r.corroborated
            && let Some(business) = &r.business
        {
            mark(vault, business, &[], r.waba, &mut ledger).await;
            marked = true;
        }
        // 3. The ledger again: an allocation a racing share recorded since
        // the first read, and a share posted whose outcome is unknown.
        let mut pending = None;
        if let Some(waba) = r.waba
            && let Ok(Some(credit)) = vault.credit(waba).await
        {
            if let Some(allocation) = credit.allocation_config_id
                && !r.known.contains(&allocation)
            {
                r.known.push(allocation);
            }
            pending = credit.pending_share;
        }
        // 4. Revoke.
        let revoked = system.revoke_all(line, r.business.as_ref(), &r.known).await;
        let mut out = revoked.outcome;
        // 5. The marker lists what is revoked, even after a partial run. A
        // business not marked yet (its check failed) is marked once the
        // revocation's own lookup attributed one of these records to it.
        if let Some(business) = &r.business {
            let ids: Vec<AllocationConfigId> = out.report.all().cloned().collect();
            if marked {
                if !ids.is_empty()
                    && let Err(e) = vault.mark_revoked(business, &ids).await
                {
                    tracing::warn!(kind = ?e.kind(), "revocation: marker not updated");
                    ledger.get_or_insert(e);
                }
            } else if ids.iter().any(|id| revoked.named.contains(id)) {
                mark(vault, business, &ids, r.waba, &mut ledger).await;
            }
        }
        // 6. A share posted whose outcome is unknown (its answer lost, or
        // still in flight) is settled only by a record revoked now: records
        // found already revoked say nothing about it.
        if let (Some(waba), Some(_)) = (r.waba, pending) {
            if out.report.revoked.is_empty() {
                out.share_pending = true;
            } else if !out.is_incomplete()
                && let Err(e) = settle_pending(vault, waba, None).await
            {
                ledger.get_or_insert(e);
            }
        }
        // Ledger failures do not make the revocation unretryable, and are
        // kept next to a failure on Meta's side.
        out.ledger = ledger.map(Box::new);
        finish(out)
    }
}

/// Mark `business` revoked (adding `ids`) and, for a WABA, record the
/// business in its credit record when it names none, so
/// [`TokenVault::rotate`] reaches the marker. Failures go to `ledger`.
async fn mark(
    vault: &TokenVault,
    business: &BusinessId,
    ids: &[AllocationConfigId],
    waba: Option<&WabaId>,
    ledger: &mut Option<Error>,
) {
    if let Err(e) = vault.mark_revoked(business, ids).await {
        tracing::warn!(kind = ?e.kind(), "revocation: marker not written; revoking anyway");
        ledger.get_or_insert(e);
    }
    if let Some(waba) = waba {
        match vault.credit(waba).await {
            // An unreadable record is left alone (and reported by the
            // caller's read); only a readable one is completed.
            Ok(_) => {
                if let Err(e) = vault.note_business(waba, business).await {
                    tracing::warn!(kind = ?e.kind(), "revocation: business not recorded");
                    ledger.get_or_insert(e);
                }
            }
            Err(e) => {
                tracing::warn!(kind = ?e.kind(), "revocation: credit record unreadable");
            }
        }
    }
}

/// Whose line a revocation revokes, and what it may write.
struct Revocation<'a> {
    business: Option<BusinessId>,
    /// Whether `business` comes from this deployment's records or from
    /// Meta's record of our allocation, not only from the caller.
    corroborated: bool,
    /// Allocations recorded in the ledger.
    known: Vec<AllocationConfigId>,
    /// The WABA whose ledger is read and completed, when there is one.
    waba: Option<&'a WabaId>,
}

fn revoked(business: &BusinessId, why: &str) -> Error {
    CreditError::Revoked {
        business_id: Some(business.clone()),
        reason: format!(
            "the credit line of business {business} was revoked ({why}); not sharing it again without OnboardingRequest::reshare_after_revocation"
        ),
        posted: false,
    }
    .into()
}

/// `assign_system_user`: add the partner's system user to the customer's
/// WABA with the configured tasks (system user token). Repeatable: it sets
/// the same grant again.
pub(super) async fn assign_system_user(
    es: &EmbeddedSignup,
    plan: &CreditPlan<'_>,
    waba_id: &WabaId,
) -> Result<()> {
    es.system_client(plan.partner)
        .waba(waba_id.clone())
        .assign_user(
            plan.partner.system_user_id.as_str(),
            &plan.partner.system_user_tasks,
        )
        .await
}

/// `share_credit_line`: make the partner's credit line fund `token`'s WABA,
/// **checking first** whether it already does, and record the allocation in
/// the vault's credit ledger. Holds the WABA's credit lease throughout,
/// renewed before each post.
pub(super) async fn share_credit_line(
    es: &EmbeddedSignup,
    plan: &CreditPlan<'_>,
    business: &Client,
    vault: &TokenVault,
    token: &StoredBusinessToken,
) -> Result<AllocationConfigId> {
    let mut lease = vault.lease_credit(&token.waba_id).await?;
    let shared = share_leased(es, plan, business, vault, token, &mut lease).await;
    if let Err(e) = vault.release_credit(&token.waba_id, lease).await {
        // It expires on its own; the kind only, as elsewhere in the vault.
        tracing::warn!(waba_id = %token.waba_id, kind = ?e.kind(), "credit lease not released");
    }
    shared
}

/// Extend the lease right before a post; stop, posting nothing more, when
/// it was lost. `posted`: whether this call already posted something.
async fn renew(vault: &TokenVault, waba: &WabaId, lease: &mut u64, posted: bool) -> Result<()> {
    let lost = |reason: &str| -> Error {
        CreditError::Busy {
            reason: reason.to_owned(),
            posted,
        }
        .into()
    };
    match vault.renew_credit(waba, *lease).await {
        Ok(Some(version)) => {
            *lease = version;
            Ok(())
        }
        Ok(None) => Err(lost(
            "this step's credit lease expired and another onboarding of the WABA may hold it; nothing more was posted, resume later",
        )),
        Err(e) if posted => {
            tracing::warn!(waba_id = %waba, kind = ?e.kind(), "credit lease not renewed");
            Err(lost(
                "this step's credit lease could not be renewed; nothing more was posted, resume later",
            ))
        }
        Err(e) => Err(e),
    }
}

/// What Meta knows of the line and the owner business: active, revoked and
/// unknown-status records, the stored allocation included.
struct Records {
    active: Vec<AllocationConfigId>,
    deleted: Vec<AllocationConfigId>,
    /// `request_status` values Meta does not document, verbatim.
    unknown: Vec<(AllocationConfigId, String)>,
    /// Records the lookup for the owner returned naming no receiving
    /// business (not checked further). A share ignores them, as it did
    /// before; an operator's clearance stops at them.
    unattributed: Vec<AllocationConfigId>,
}

async fn records(
    system: &CreditLines,
    line: &CreditLineId,
    owner: &BusinessId,
    stored: Option<&AllocationConfigId>,
) -> Result<Records> {
    let listed = system.allocations_for(line, owner).await?;
    let mut unattributed: Vec<AllocationConfigId> = Vec::new();
    for record in &listed {
        let names_none = record
            .receiving_business
            .as_ref()
            .and_then(|b| b.id.as_ref())
            .is_none_or(|b| b.as_str().trim().is_empty());
        if let (true, Some(id)) = (names_none, &record.id)
            && !unattributed.contains(id)
        {
            unattributed.push(id.clone());
        }
    }
    let mut ids = owned_by(listed, owner);
    if let Some(stored) = stored
        && !ids.contains(stored)
    {
        ids.insert(0, stored.clone());
    }
    // The stored allocation gets its own status check below.
    unattributed.retain(|id| !ids.contains(id));
    let mut records = Records {
        active: Vec::new(),
        deleted: Vec::new(),
        unknown: Vec::new(),
        unattributed,
    };
    for id in ids {
        // `owning_credit_allocation_configs` does not say whether a record
        // was revoked (nor whether Meta lists revoked ones): ask each.
        let status = system.allocation_status(&id).await?;
        let named = status
            .receiving_business
            .as_ref()
            .and_then(|b| b.id.as_ref());
        if let Some(named) = named
            && named != owner
        {
            return Err(ValidationError::new(
                "allocation_config_id",
                format!("{id} is shared with another business than the WABA's owner; not using it"),
            )
            .into());
        }
        match &status.request_status {
            _ if status.is_deleted() => records.deleted.push(id),
            None => records.active.push(id),
            Some(other) => records.unknown.push((id, other.as_str().to_owned())),
        }
    }
    Ok(records)
}

async fn share_leased(
    es: &EmbeddedSignup,
    plan: &CreditPlan<'_>,
    business: &Client,
    vault: &TokenVault,
    token: &StoredBusinessToken,
    lease: &mut u64,
) -> Result<AllocationConfigId> {
    let partner = plan.partner;
    let line = &partner.credit_line_id;
    let waba = &token.waba_id;
    let (mut credit, version) = match vault.credit_versioned(waba).await? {
        Some((credit, version)) => (credit, Some(version)),
        None => (StoredCredit::new(waba.clone()), None),
    };

    // 1. Whose line this is, and in which currency: local checks first.
    let owner = match (&token.business_id, &credit.business_id) {
        (Some(verified), Some(recorded)) if verified != recorded => {
            return Err(ValidationError::new(
                "business_id",
                "the WABA's owner differs from the business its credit line was shared with; not sharing",
            )
            .into());
        }
        (Some(owner), _) | (None, Some(owner)) => owner.clone(),
        // Meta's docs show `owner_business_info` on every WABA: without it
        // nothing can be checked (records, revocation marker) or revoked.
        (None, None) => {
            return Err(CreditError::OwnerUnknown(
                "Meta did not report the WABA's owner business (owner_business_info), which the line is checked, shared and revoked by; not sharing".into(),
            )
            .into());
        }
    };
    if let Some(sealed) = &credit.currency
        && sealed != &plan.currency
    {
        return Err(ValidationError::new(
            "waba_currency",
            format!(
                "this WABA's credit line was requested in {sealed}; a line cannot change once attached, so {} is refused",
                plan.currency
            ),
        )
        .into());
    }
    let marker = vault
        .revoked_versioned(&owner)
        .await?
        .map(|(_, version)| version);
    if marker.is_some() && !plan.reshare_after_revocation {
        // A share whose outcome is unknown may be live though the business
        // was revoked since: "revoked, nothing posted" would hide it.
        if let Some(at) = credit.pending_share {
            return Err(CreditError::Reconcile(format!(
                "a share posted at {at} has no recorded outcome, and the credit line of business {owner} was revoked since: that share may be live. Call revoke_credit_line again and check the WABA's funding in Meta Business Suite"
            ))
            .into());
        }
        return Err(revoked(&owner, "recorded by revoke_credit_line"));
    }

    // 2. What Meta has: the records naming the owner, and the stored one.
    let system = es.system_client(partner).credit_lines();
    let found = records(&system, line, &owner, credit.allocation_config_id.as_ref()).await?;
    if !plan.reshare_after_revocation {
        if let Some((id, status)) = found.unknown.first() {
            return Err(CreditError::StatusUnknown {
                allocation_config_id: id.clone(),
                status: status.clone(),
            }
            .into());
        }
        if found.active.is_empty() && !found.deleted.is_empty() {
            return Err(revoked(&owner, "Meta reports its records DELETED"));
        }
    }

    // 3, 4.
    let already = already_funded(&system, business, waba, &found, credit.pending_share).await?;

    // 5. Seal the owner, the currency and, before a post, the "posted,
    // outcome unknown" flag.
    credit.business_id = Some(owner.clone());
    credit.currency = Some(plan.currency.clone());
    if already.is_none() {
        credit.pending_share = Some(vault.now());
    }
    vault.put_credit(&credit, version).await?;

    // 6. If the line does not fund the WABA yet, share it.
    let share = Share {
        system: &system,
        business,
        vault,
        plan,
        waba,
        owner: &owner,
    };
    let mut posted = Posted::default();
    let allocation = match already {
        Some(id) => id,
        None => match post_share(&share, &found, lease, &mut posted).await {
            Ok(id) => id,
            Err(e) => return Err(after_failed_post(&share, marker, &posted, e).await),
        },
    };

    // 7, 8.
    record_share(&share, marker, posted.any, allocation).await
}

/// Steps 3 and 4 of a share: the record among `found` that already funds
/// the WABA, if any, and a refusal when a share posted earlier with no
/// recorded outcome (`pending`) could be what funds it.
async fn already_funded(
    system: &CreditLines,
    business: &Client,
    waba: &WabaId,
    found: &Records,
    pending: Option<time::OffsetDateTime>,
) -> Result<Option<AllocationConfigId>> {
    // 3. Does one of them already fund the WABA? (Opted in, a record of
    // unknown status is checked too.)
    let mut funding: Option<WabaFunding> = None;
    let mut already = None;
    let candidates = found
        .active
        .iter()
        .chain(found.unknown.iter().map(|(id, _)| id));
    for candidate in candidates {
        let allocation = system.receiving_credential(candidate).await?;
        if funding.is_none() {
            funding = Some(business.credit_lines().primary_funding(waba).await?);
        }
        if let Some(funding) = &funding
            && is_shared(&allocation, funding)
        {
            already = Some(candidate.clone());
            break;
        }
    }

    // 4. A share posted earlier whose outcome was never recorded, and
    // nothing found funds the WABA: if something else does, posting again
    // could fund it twice or fail half-way. When nothing does, it is posted
    // again: that assumes the lookup and `primary_funding_id` show a share
    // once Meta applied it, which Meta does not document (the two-call
    // method's share alone funds nothing, so for it only the lookup
    // tells).
    if already.is_none()
        && let Some(at) = pending
    {
        let funding = match funding {
            Some(funding) => funding,
            None => business.credit_lines().primary_funding(waba).await?,
        };
        if funding.primary_funding_id.is_some() {
            return Err(CreditError::Reconcile(format!(
                "a share posted at {at} was never recorded, no record of the line is found funding this WABA, and the WABA has a primary funding: check in Meta Business Suite which line funds it before sharing again"
            ))
            .into());
        }
    }

    Ok(already)
}

/// What the posts of one share, and what follows them, work on.
struct Share<'a> {
    /// The partner's credit lines (system user token).
    system: &'a CreditLines,
    /// The merchant's client (business token).
    business: &'a Client,
    vault: &'a TokenVault,
    plan: &'a CreditPlan<'a>,
    waba: &'a WabaId,
    /// The WABA's verified owner business.
    owner: &'a BusinessId,
}

/// How far [`post_share`] got.
#[derive(Default)]
struct Posted {
    /// A request that may change Meta's side went out.
    any: bool,
    /// The allocation the two-call method's share made, once it answered.
    shared: Option<AllocationConfigId>,
    /// Whether `shared` is recorded in the ledger.
    recorded: bool,
}

/// Post the share with the configured method, saying in `posted` how far it
/// got. A failure after the two-call method's share went out says so:
/// [`CreditError::AttachFailed`] when the attach provably reached nothing,
/// [`CreditError::Busy`] with `posted` when the lease was lost.
async fn post_share(
    share: &Share<'_>,
    found: &Records,
    lease: &mut u64,
    posted: &mut Posted,
) -> Result<AllocationConfigId> {
    let Share {
        system,
        business,
        vault,
        plan,
        waba,
        owner,
    } = share;
    let line = &plan.partner.credit_line_id;
    match plan.partner.method {
        CreditSharing::ShareAndAttach => {
            renew(vault, waba, lease, false).await?;
            posted.any = true;
            Ok(system
                .share_and_attach(line, waba, &plan.currency)
                .await?
                .allocation_config_id)
        }
        CreditSharing::ShareThenAttach => {
            if found.active.is_empty() {
                renew(vault, waba, lease, false).await?;
                posted.any = true;
                let id = system.share(line, owner).await?.allocation_config_id;
                posted.shared = Some(id.clone());
                // Recorded before the attach: a resume then checks it even
                // if the lookup does not list it.
                let recorded = vault
                    .update_credit(waba, |c| {
                        c.allocation_config_id = Some(id.clone());
                        true
                    })
                    .await;
                if !matches!(recorded, Ok(Some(_))) {
                    return Err(CreditError::Reconcile(format!(
                        "the line was shared with the owner business (allocation {id}) but the credit ledger could not record it; nothing was attached"
                    ))
                    .into());
                }
                posted.recorded = true;
            }
            renew(vault, waba, lease, posted.any).await?;
            posted.any = true;
            match business
                .credit_lines()
                .attach(line, waba, &plan.currency)
                .await
            {
                Ok(attached) => Ok(attached.allocation_config_id),
                // The share went out and is recorded: say so, whatever the
                // attach's own error says.
                Err(e) => match &posted.shared {
                    Some(id) if !e.may_have_been_sent() => Err(CreditError::AttachFailed {
                        allocation_config_id: id.clone(),
                        source: Box::new(e),
                    }
                    .into()),
                    _ => Err(e),
                },
            }
        }
    }
}

/// A post failed. When Meta provably did nothing, the outcome is known:
/// the pending flag is cleared and the error returned (a post that went
/// out before the failing one already made the error say so, in
/// [`post_share`]). Otherwise a share may be live: a revocation that ran
/// meanwhile is looked for, as after a successful post, and a lost answer
/// is [`CreditError::Reconcile`], never an invitation to retry at once.
async fn after_failed_post(
    share: &Share<'_>,
    marker: Option<u64>,
    posted: &Posted,
    e: Error,
) -> Error {
    if !e.may_have_been_sent() {
        clear_pending(share.vault, share.waba).await;
        return e;
    }
    if revoked_since(share.vault, share.owner, marker, false).await {
        return revoke_raced(share, posted.shared.as_ref(), posted.recorded).await;
    }
    // The share may be live: it stays pending whatever cleared the flag
    // while the post was out (an operator's clearance that took the lease
    // once it expired under a post slower than the lease, say).
    if let Err(k) = keep_pending(share.vault, share.waba).await {
        tracing::warn!(waba_id = %share.waba, kind = ?k.kind(), "pending share flag not kept");
    }
    if e.credit().is_some() {
        // Already says what was posted (Busy, Reconcile, AttachFailed).
        return e;
    }
    tracing::warn!(waba_id = %share.waba, kind = ?e.kind(), "credit line share: answer lost");
    CreditError::Reconcile(format!(
        "the answer to this step's credit line post was lost ({:?}): the share may have gone through. Do not retry at once: resume checks Meta's records first (the line's records for the owner business, and the WABA's primary funding) and posts again only when none funds the WABA, which assumes Meta lists a share as soon as it applied it (undocumented)",
        e.kind()
    ))
    .into()
}

/// Steps 7 and 8 of a share: record the allocation, merging with whatever
/// changed meanwhile (never "busy" once a share went out), then check
/// whether a revocation of the owner ran meanwhile. A revocation writes its
/// marker before it looks anything up, and this step reads it after
/// posting: one of the two sees the other.
async fn record_share(
    share: &Share<'_>,
    marker: Option<u64>,
    posted: bool,
    allocation: AllocationConfigId,
) -> Result<AllocationConfigId> {
    let Share {
        vault, waba, owner, ..
    } = share;
    let now = vault.now();
    let recorded = vault
        .update_credit(waba, |c| {
            if c.allocation_config_id.as_ref() != Some(&allocation) || c.shared_at.is_none() {
                c.shared_at = Some(now);
            }
            c.allocation_config_id = Some(allocation.clone());
            c.business_id.get_or_insert_with(|| (*owner).clone());
            c.pending_share = None;
            true
        })
        .await;
    if revoked_since(vault, owner, marker, true).await {
        return Err(if posted {
            let recorded = matches!(recorded, Ok(Some(_)));
            revoke_raced(share, Some(&allocation), recorded).await
        } else {
            revoked(owner, "a revocation ran while this step checked the line")
        });
    }
    match recorded {
        Ok(Some(_)) => Ok(allocation),
        Ok(None) | Err(_) if posted => {
            // Not recorded: the ledger must at least say a share may be live,
            // whatever cleared the flag meanwhile.
            if let Err(k) = keep_pending(vault, waba).await {
                tracing::warn!(waba_id = %waba, kind = ?k.kind(), "pending share flag not kept");
            }
            Err(CreditError::Reconcile(format!(
                "allocation {allocation} was shared with the WABA, but the credit ledger could not record it; resume checks the line before posting again"
            ))
            .into())
        }
        Ok(None) => Err(super::ledger::busy(
            "the WABA's credit record kept changing while this step ran",
        )),
        Err(e) => Err(e),
    }
}

/// Whether a revocation of `owner` ran since this step read its marker at
/// version `then` (`None`: there was none). A marker that cannot be read
/// counts as one.
///
/// `settle`: the share succeeded, so an opted-in re-share clears the marker
/// it read, by compare-and-swap on that version: a revocation that touched
/// it since makes the clear fail, and counts.
///
/// A marker that is gone counts as none: another opted-in re-share of the
/// business cleared it (a revocation never deletes one). A revocation that
/// ran before that clear, and whose lookup missed this share, is then not
/// seen here: the other re-share's opt-in funds the business again anyway.
async fn revoked_since(
    vault: &TokenVault,
    owner: &BusinessId,
    then: Option<u64>,
    settle: bool,
) -> bool {
    match vault.revoked_versioned(owner).await {
        Ok(None) => false,
        Ok(Some((_, now))) if then == Some(now) => {
            if !settle {
                return false;
            }
            match vault.clear_revoked(owner, now).await {
                Ok(cleared) => !cleared,
                Err(e) => {
                    // A leftover marker only makes onboarding stricter.
                    tracing::warn!(kind = ?e.kind(), "revocation marker not cleared");
                    false
                }
            }
        }
        Ok(Some(_)) => true,
        Err(e) => {
            tracing::warn!(kind = ?e.kind(), "revocation marker unreadable after the share");
            true
        }
    }
}

/// Best effort: a post Meta provably refused leaves no unknown outcome.
async fn clear_pending(vault: &TokenVault, waba: &WabaId) {
    let cleared = vault
        .update_credit(waba, |c| {
            if c.pending_share.is_none() {
                return false;
            }
            c.pending_share = None;
            true
        })
        .await;
    if !matches!(cleared, Ok(Some(_))) {
        tracing::warn!(waba_id = %waba, "pending share flag not cleared");
    }
}

/// A share whose outcome was unknown is settled (revoked): clear the flag,
/// and record `allocation`, what it made, when known.
async fn settle_pending(
    vault: &TokenVault,
    waba: &WabaId,
    allocation: Option<&AllocationConfigId>,
) -> Result<()> {
    vault
        .update_credit(waba, |c| {
            let known = allocation.is_none_or(|a| c.allocation_config_id.as_ref() == Some(a));
            if c.pending_share.is_none() && known {
                return false;
            }
            c.pending_share = None;
            if let Some(allocation) = allocation {
                c.allocation_config_id = Some(allocation.clone());
            }
            true
        })
        .await?
        .map(|_| ())
        .ok_or_else(|| super::ledger::busy("the WABA's credit record kept changing"))
}

/// A revocation of the owner ran while this step posted: revoke what the
/// post made at once, add it to the marker, and say what happened.
///
/// `made` is the allocation the post made, when known (its answer, or the
/// two-call method's share); `None` when the answer was lost, and then
/// every active record naming the owner is revoked, and only a record
/// revoked now can be that share. `recorded`: whether the ledger holds
/// `made`.
async fn revoke_raced(
    share: &Share<'_>,
    made: Option<&AllocationConfigId>,
    recorded: bool,
) -> Error {
    let Share {
        system,
        vault,
        plan,
        waba,
        owner,
        ..
    } = share;
    let line = &plan.partner.credit_line_id;
    let known: Vec<AllocationConfigId> = made.into_iter().cloned().collect();
    let out = system.revoke_all(line, Some(owner), &known).await.outcome;
    let ids: Vec<AllocationConfigId> = out.report.all().cloned().collect();
    if let Err(e) = vault.mark_revoked(owner, &ids).await {
        tracing::warn!(kind = ?e.kind(), "revocation marker not updated after a raced share");
    }
    let settled = match made {
        Some(made) => out.report.all().any(|id| id == made),
        None => !out.is_incomplete() && !out.report.revoked.is_empty(),
    };
    if settled {
        let what = match (made, out.report.revoked.as_slice()) {
            (Some(id), _) | (None, [id]) => Some(id),
            (None, _) => None,
        };
        if let Err(e) = settle_pending(vault, waba, what).await {
            tracing::warn!(kind = ?e.kind(), "raced share revoked; the ledger not updated");
        }
        let revoked_now = match made {
            Some(id) => format!("the new allocation {id} was revoked at once"),
            None => format!(
                "its answer was lost, and the records naming the business were revoked at once ({:?})",
                out.report.revoked
            ),
        };
        return CreditError::Revoked {
            business_id: Some((*owner).clone()),
            reason: format!(
                "a revocation of business {owner} ran while this step shared the credit line; {revoked_now}"
            ),
            posted: true,
        }
        .into();
    }
    tracing::warn!(kind = ?out.source.as_ref().map(|e| e.kind()), "raced share not revoked");
    match (made, recorded) {
        (Some(id), true) => CreditError::Revoked {
            business_id: Some((*owner).clone()),
            reason: format!(
                "a revocation of business {owner} ran while this step shared the credit line; the new allocation {id} could not be revoked: it is recorded in the credit ledger, call revoke_credit_line again"
            ),
            posted: true,
        }
        .into(),
        (Some(id), false) => CreditError::Reconcile(format!(
            "a revocation of business {owner} ran while this step shared the credit line; the new allocation {id} could be neither revoked nor recorded: revoke it in Meta Business Suite"
        ))
        .into(),
        (None, _) => {
            // Kept pending for the next revocation, even if a revocation
            // settled the flag while this post was in flight.
            if let Err(e) = keep_pending(vault, waba).await {
                tracing::warn!(kind = ?e.kind(), "pending share flag not kept");
            }
            CreditError::Reconcile(format!(
                "a revocation of business {owner} ran while this step shared the credit line, and the share's answer was lost; no record of it could be revoked (Meta's lookup may not list it yet): it may be live. Call revoke_credit_line again (the ledger keeps the share pending) and check the WABA's funding in Meta Business Suite"
            ))
            .into()
        }
    }
}

/// The longest `cleared_by` [`EmbeddedSignup::clear_pending_share`]
/// accepts, in characters (after trimming): an operator id, not a note.
pub const MAX_CLEARED_BY_CHARS: usize = 256;

/// `cleared_by`, trimmed: not blank, at most [`MAX_CLEARED_BY_CHARS`]
/// characters, and nothing invisible or line-breaking (control, format
/// such as U+200B or bidi controls, line and paragraph separators), so an
/// audit entry cannot look blank or like another operator.
fn check_cleared_by(cleared_by: &str) -> Result<(), ValidationError> {
    let bad = |reason: &str| Err(ValidationError::new("cleared_by", reason));
    if cleared_by.is_empty() {
        return bad(
            "required: the operator who checked the WABA's funding in Meta Business Suite, for the audit entry",
        );
    }
    if cleared_by.chars().count() > MAX_CLEARED_BY_CHARS {
        return bad("longer than 256 characters: pass an operator id");
    }
    let invisible = |c: char| {
        c.is_control()
            || matches!(
                c.general_category(),
                GeneralCategory::Format
                    | GeneralCategory::LineSeparator
                    | GeneralCategory::ParagraphSeparator
            )
    };
    if cleared_by.chars().any(invisible) {
        return bad(
            "must not contain control, format (U+200B, U+FEFF, bidi controls, …) or line separator characters",
        );
    }
    Ok(())
}

/// The owner business a clearance checks the line's records for: the one
/// the ledger recorded, which must agree with the token's.
fn clearance_owner(credit: &StoredCredit, token: &StoredBusinessToken) -> Result<BusinessId> {
    match (&credit.business_id, &token.business_id) {
        (Some(recorded), Some(verified)) if recorded != verified => Err(ValidationError::new(
            "business_id",
            "the WABA's owner differs from the business its credit line was shared with; nothing cleared",
        )
        .into()),
        (Some(owner), _) | (None, Some(owner)) => Ok(owner.clone()),
        (None, None) => Err(CreditError::OwnerUnknown(
            "no owner business is recorded for the WABA, so your line's records for it cannot be checked; nothing cleared".into(),
        )
        .into()),
    }
}

/// Extend a clearance's lease right before it writes the ledger; stop,
/// writing nothing, when it was lost (a share may hold it now).
async fn renew_clearance(vault: &TokenVault, waba: &WabaId, lease: &mut u64) -> Result<()> {
    match vault.renew_credit(waba, *lease).await? {
        Some(renewed) => {
            *lease = renewed;
            Ok(())
        }
        None => Err(super::ledger::busy(
            "this call's credit lease expired and another step of the WABA may hold it; nothing was cleared, call again",
        )),
    }
}

/// A record Meta shows funding the WABA, found while an operator's
/// clearance checked a pending share: recorded as the WABA's allocation,
/// as `record_share` records a share it finds, so a revocation revokes it.
/// The pending flag stays: nothing was cleared, and a revocation that
/// revokes this record settles it.
async fn record_found(
    vault: &TokenVault,
    waba: &WabaId,
    owner: &BusinessId,
    allocation: &AllocationConfigId,
) -> Result<()> {
    let now = vault.now();
    vault
        .update_credit(waba, |c| {
            let same = c.allocation_config_id.as_ref() == Some(allocation);
            if same && c.shared_at.is_some() && c.business_id.is_some() {
                return false;
            }
            if !same || c.shared_at.is_none() {
                c.shared_at = Some(now);
            }
            c.allocation_config_id = Some(allocation.clone());
            c.business_id.get_or_insert_with(|| owner.clone());
            true
        })
        .await?
        .map(|_| ())
        .ok_or_else(|| super::ledger::busy("the WABA's credit record kept changing"))
}

/// Set the pending share flag again if something cleared it.
async fn keep_pending(vault: &TokenVault, waba: &WabaId) -> Result<()> {
    let now = vault.now();
    vault
        .update_credit(waba, |c| {
            if c.pending_share.is_some() {
                return false;
            }
            c.pending_share = Some(now);
            true
        })
        .await?
        .map(|_| ())
        .ok_or_else(|| super::ledger::busy("the WABA's credit record kept changing"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http::Method;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use time::macros::datetime;
    use wa_adapters::store::MemoryKvStore;
    use wa_core::clock::ManualClock;
    use wa_core::error::TransportError;
    use wa_core::ids::PhoneNumberId;
    use wa_core::secret::SecretBytes;
    use wa_core::testing::{RecordedBody, RecordedRequest, ScriptedTransport};
    use wa_core::{Error, ErrorKind};

    use super::super::event::EmbeddedSignupEvent;
    use super::super::onboard::steps::{
        APPROVE, ASSIGN_SYSTEM_USER, DEBUG_TOKEN, EXCHANGE_CODE, LOAD_TOKEN, REGISTER_PHONE,
        SHARE_CREDIT_LINE, STORE_TOKEN, SUBSCRIBE_APP, VERIFY_ASSETS,
    };
    use super::super::onboard::{Onboarded, OnboardingRequest};
    use super::super::token::SignupCode;
    use super::super::vault::{VaultKey, VaultKeys};
    use super::*;
    use crate::phone_numbers::TwoStepPin;
    use crate::{AppCredentials, RetryPolicy};

    const APP_ID: &str = "236484624622562";
    const APP_SECRET: &str = "614fc2afde15eee07a26b2fe3eaee9b9";
    const CODE: &str = "AQBhlXsctMxJYbwbrpybxlo9tLPGy";
    const TOKEN: &str = "EAAAN6tcBzAUBOwtDtTfmZCJ9n3FHpSDcDTH86ekf89Xnn";
    const SYSTEM_TOKEN: &str = "EAAAN6tcBzAUBOZC82CW7iR2LiaZBwUHS4Y7FDtQ";
    const SYSTEM_USER: &str = "1972555232742222";
    const LINE: &str = "1972385232742146";
    const WABA: &str = "102290129340398";
    const PHONE: &str = "106540352242922";
    /// The WABA's owner as Meta reports it (`owner_business_info`).
    const BUSINESS: &str = "2729063490586005";
    /// What the browser's event claims; must never reach a credit call.
    const CLAIMED: &str = "CLAIMED_BY_THE_BROWSER";
    const ALLOCATION: &str = "58501441721238";
    const CREDENTIAL: &str = "7340125593810";
    const PIN: &str = "581063";

    /// The paths of every credit line call (anything but onboarding's own).
    const CREDIT_EDGES: [&str; 4] = [
        "/whatsapp_credit_sharing_and_attach",
        "/whatsapp_credit_sharing",
        "/whatsapp_credit_attach",
        "/owning_credit_allocation_configs",
    ];

    struct Harness {
        t: ScriptedTransport,
        es: EmbeddedSignup,
        vault: TokenVault,
        /// The vault's clock.
        clock: Arc<ManualClock>,
    }

    fn partner(method: CreditSharing) -> SolutionPartner {
        SolutionPartner::new(AccessToken::new(SYSTEM_TOKEN), SYSTEM_USER, LINE).method(method)
    }

    fn harness_with(partner: Option<SolutionPartner>) -> Harness {
        let t = ScriptedTransport::new();
        let client = Client::builder()
            .transport(t.clone())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let clock = Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC)));
        let vault = TokenVault::new(
            Arc::new(MemoryKvStore::new()),
            VaultKeys::new(VaultKey::new("k1", SecretBytes::new([42; 32])).unwrap()),
        )
        .unwrap()
        .with_clock(clock.clone());
        let es = client.embedded_signup(AppCredentials::new(APP_ID, APP_SECRET));
        let es = match partner {
            Some(p) => es.solution_partner(p),
            None => es,
        };
        Harness {
            t,
            es,
            vault,
            clock,
        }
    }

    fn harness(method: CreditSharing) -> Harness {
        harness_with(Some(partner(method)))
    }

    /// A hook run once, before the first request matching it is answered:
    /// another process acting between two requests of the one under test.
    type Hook = Box<dyn FnOnce() -> futures::future::BoxFuture<'static, ()> + Send>;
    type Hooks = Arc<std::sync::Mutex<Vec<(Method, &'static str, Hook)>>>;

    /// A transport that runs hooks, then answers from the shared script.
    #[derive(Clone)]
    struct Hooked {
        inner: ScriptedTransport,
        hooks: Hooks,
    }

    impl fmt::Debug for Hooked {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Hooked").finish_non_exhaustive()
        }
    }

    impl Hooked {
        fn before(
            &self,
            method: Method,
            path_suffix: &'static str,
            hook: impl FnOnce() -> futures::future::BoxFuture<'static, ()> + Send + 'static,
        ) {
            self.hooks
                .lock()
                .unwrap()
                .push((method, path_suffix, Box::new(hook)));
        }
    }

    #[async_trait::async_trait]
    impl wa_core::transport::HttpTransport for Hooked {
        async fn send(
            &self,
            request: wa_core::transport::HttpRequest,
        ) -> Result<wa_core::transport::HttpResponse, TransportError> {
            let hook = {
                let mut hooks = self.hooks.lock().unwrap();
                hooks
                    .iter()
                    .position(|(m, suffix, _)| {
                        *m == request.method && request.url.path().ends_with(suffix)
                    })
                    .map(|at| hooks.remove(at).2)
            };
            if let Some(hook) = hook {
                hook().await;
            }
            self.inner.send(request).await
        }
    }

    /// A store that can make the revocation marker change right before a
    /// re-share clears it, or refuse every write of a marker.
    #[derive(Debug, Default)]
    struct TestKv {
        inner: MemoryKvStore,
        touch_marker_before_clear: std::sync::atomic::AtomicBool,
        refuse_marker_writes: std::sync::atomic::AtomicBool,
        refuse_credit_writes: std::sync::atomic::AtomicBool,
        /// Refuse the next write of a credit record only.
        refuse_one_credit_write: std::sync::atomic::AtomicBool,
        refuse_lease_writes: std::sync::atomic::AtomicBool,
    }

    impl TestKv {
        fn refuses(
            &self,
            key: &wa_core::store::StoreKey,
        ) -> Result<(), wa_core::error::StorageError> {
            let on = |flag: &std::sync::atomic::AtomicBool| {
                flag.load(std::sync::atomic::Ordering::SeqCst)
            };
            let k = key.key();
            if (k.starts_with("revoked/") && on(&self.refuse_marker_writes))
                || (k.starts_with("credit/") && on(&self.refuse_credit_writes))
                || (k.starts_with("credit/")
                    && self
                        .refuse_one_credit_write
                        .swap(false, std::sync::atomic::Ordering::SeqCst))
                || (k.starts_with("credit-lease/") && on(&self.refuse_lease_writes))
            {
                return Err(wa_core::error::StorageError::Backend(anyhow::anyhow!(
                    "writes refused"
                )));
            }
            Ok(())
        }

        fn refuse(flag: &std::sync::atomic::AtomicBool) {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl wa_core::store::KvStore for TestKv {
        async fn get(
            &self,
            key: &wa_core::store::StoreKey,
        ) -> Result<Option<wa_core::store::Versioned>, wa_core::error::StorageError> {
            self.inner.get(key).await
        }
        async fn put(
            &self,
            key: &wa_core::store::StoreKey,
            value: Vec<u8>,
            expiry: wa_core::store::Expiry,
        ) -> Result<u64, wa_core::error::StorageError> {
            self.refuses(key)?;
            self.inner.put(key, value, expiry).await
        }
        async fn put_if_absent(
            &self,
            key: &wa_core::store::StoreKey,
            value: Vec<u8>,
            expiry: wa_core::store::Expiry,
        ) -> Result<Option<u64>, wa_core::error::StorageError> {
            self.refuses(key)?;
            self.inner.put_if_absent(key, value, expiry).await
        }
        async fn compare_and_swap(
            &self,
            key: &wa_core::store::StoreKey,
            expected: u64,
            new: Option<Vec<u8>>,
            expiry: wa_core::store::Expiry,
        ) -> Result<Option<u64>, wa_core::error::StorageError> {
            self.refuses(key)?;
            if new.is_none()
                && key.key().starts_with("revoked/")
                && self
                    .touch_marker_before_clear
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                && let Some(current) = self.inner.get(key).await?
            {
                // A revocation rewrites the marker between the re-share's
                // last read and its clear.
                self.inner
                    .put(key, current.value, wa_core::store::Expiry::Keep)
                    .await?;
            }
            self.inner
                .compare_and_swap(key, expected, new, expiry)
                .await
        }
        async fn delete(
            &self,
            key: &wa_core::store::StoreKey,
        ) -> Result<bool, wa_core::error::StorageError> {
            self.inner.delete(key).await
        }
    }

    /// A Solution Partner harness on `kv`, whose client runs `Hooked`
    /// hooks; `other` is a second deployment (another process) on the same
    /// script and store, without hooks.
    fn hooked_harness(method: CreditSharing, kv: Arc<TestKv>) -> (Harness, Hooked, Harness) {
        let t = ScriptedTransport::new();
        let hooked = Hooked {
            inner: t.clone(),
            hooks: Arc::default(),
        };
        let clock = Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC)));
        let vault = TokenVault::new(
            kv,
            VaultKeys::new(VaultKey::new("k1", SecretBytes::new([42; 32])).unwrap()),
        )
        .unwrap()
        .with_clock(clock.clone());
        let build = |transport: Hooked, plain: Option<ScriptedTransport>| {
            let builder = Client::builder().retry(RetryPolicy::NONE);
            let client = match plain {
                Some(plain) => builder.transport(plain),
                None => builder.transport(transport),
            }
            .build()
            .unwrap();
            client
                .embedded_signup(AppCredentials::new(APP_ID, APP_SECRET))
                .solution_partner(partner(method))
        };
        let h = Harness {
            t: t.clone(),
            es: build(hooked.clone(), None),
            vault: vault.clone(),
            clock: clock.clone(),
        };
        let other = Harness {
            t: t.clone(),
            es: build(hooked.clone(), Some(t)),
            vault,
            clock,
        };
        (h, hooked, other)
    }

    fn lease_key() -> wa_core::store::StoreKey {
        wa_core::store::StoreKey::new(
            super::super::vault::TOKEN_NAMESPACE,
            format!("credit-lease/{WABA}"),
        )
    }

    /// Another process revokes the line right before the share's POST is
    /// answered: its lookup finds nothing yet, and the ledger shows the
    /// share in flight, so it reports the revocation incomplete (call
    /// again), never done.
    async fn revoke_before_the_post(other: &Harness) {
        let err = other
            .es
            .revoke_credit_line(&waba(), None, &other.vault)
            .await
            .unwrap_err();
        let incomplete = err.credit().and_then(CreditError::revocation).unwrap();
        assert_eq!(
            incomplete.report.all().count(),
            0,
            "it looked before the share"
        );
        assert!(incomplete.share_pending, "{err}");
        assert!(err.is_retryable() && !err.may_have_been_sent(), "{err}");
    }

    fn deletes(reqs: &[RecordedRequest]) -> Vec<String> {
        reqs.iter()
            .filter(|r| r.method == Method::DELETE)
            .map(|r| r.path().to_owned())
            .collect()
    }

    impl Harness {
        /// Onboard with an approval that accepts (Solution Partner mode
        /// refuses plain `onboard`).
        async fn onboard(&self, request: &OnboardingRequest) -> Result<Onboarded> {
            self.es
                .onboard_with_approval(request, &self.vault, |_| async { Ok(()) })
                .await
        }
    }

    fn request() -> OnboardingRequest {
        let event = EmbeddedSignupEvent::from_value(json!({
            "data": {"phone_number_id": PHONE, "waba_id": WABA, "business_id": CLAIMED},
            "type": "WA_EMBEDDED_SIGNUP", "event": "FINISH"
        }))
        .unwrap();
        OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &event)
            .unwrap()
            .register_with_pin(TwoStepPin::new(PIN).unwrap())
    }

    fn waba() -> WabaId {
        WabaId::new(WABA)
    }

    fn success() -> serde_json::Value {
        json!({"success": true})
    }

    /// Up to and including `verify_assets`, as in the Tech Provider flow;
    /// `owner` is the `owner_business_info` Meta answers.
    fn script_until_verified(t: &ScriptedTransport, owner: serde_json::Value) {
        t.push_json(200, json!({"access_token": TOKEN, "token_type": "bearer"}));
        t.push_json(
            200,
            json!({"data": {
              "app_id": APP_ID, "type": "SYSTEM_USER", "is_valid": true,
              "granular_scopes": [
                {"scope": "whatsapp_business_management", "target_ids": [WABA]},
                {"scope": "whatsapp_business_messaging", "target_ids": [WABA]}
              ]
            }}),
        );
        t.push_json(200, owner);
        t.push_json(200, json!({"data": [{"id": PHONE}]}));
    }

    /// Up to and including `subscribe_app`.
    fn script_until_subscribe(t: &ScriptedTransport, owner: serde_json::Value) {
        script_until_verified(t, owner);
        t.push_json(200, success());
    }

    fn owner() -> serde_json::Value {
        json!({"owner_business_info": {"name": "Wind & Wool", "id": BUSINESS}, "id": WABA})
    }

    fn nothing_shared() -> serde_json::Value {
        json!({"data": []})
    }

    /// `share-and-revoke-credit-lines`, "Get the customer's credit sharing
    /// record": the page's single-object shape.
    fn shared_record(allocation: &str) -> serde_json::Value {
        json!({"id": allocation, "receiving_business": {"name": "Wind & Wool", "id": BUSINESS}})
    }

    /// `GET /{ALLOCATION}?fields=receiving_business,request_status` of a
    /// record not revoked (Meta documents only `DELETED`: no status here).
    fn active() -> serde_json::Value {
        json!({"receiving_business": {"name": "Wind & Wool", "id": BUSINESS}})
    }

    /// The same, revoked: the page's "Verify credit sharing was revoked"
    /// example.
    fn deleted() -> serde_json::Value {
        json!({"receiving_business": {"name": "Wind & Wool", "id": BUSINESS}, "request_status": "DELETED"})
    }

    fn receiving_credential(allocation: &str, credential: &str) -> serde_json::Value {
        json!({"receiving_credential": {"id": credential}, "id": allocation})
    }

    fn funding(credential: &str) -> serde_json::Value {
        json!({"primary_funding_id": credential, "id": WABA})
    }

    fn shared_and_attached() -> serde_json::Value {
        json!({"allocation_config_id": ALLOCATION, "waba_id": WABA})
    }

    fn step(err: &Error) -> &'static str {
        match err {
            Error::Step { step, .. } => step,
            other => panic!("not a step error: {other}"),
        }
    }

    fn assert_request(
        r: &RecordedRequest,
        method: &Method,
        path: &str,
        query: &[(&str, &str)],
        bearer: &str,
    ) {
        assert_eq!(&r.method, method, "{path}");
        assert_eq!(r.path(), path);
        let got: Vec<(String, String)> = r.url.query_pairs().into_owned().collect();
        let want: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        assert_eq!(got, want, "query of {path}");
        assert_eq!(r.bearer(), Some(bearer), "token of {path}");
    }

    fn posts_to(reqs: &[RecordedRequest], edge: &str) -> usize {
        reqs.iter()
            .filter(|r| r.method == Method::POST && r.path().ends_with(edge))
            .count()
    }

    fn credit_calls(reqs: &[RecordedRequest]) -> usize {
        reqs.iter()
            .filter(|r| {
                CREDIT_EDGES.iter().any(|e| r.path().ends_with(e))
                    || r.path().starts_with(&format!("/v25.0/{ALLOCATION}"))
            })
            .count()
    }

    fn assert_lookup(r: &RecordedRequest) {
        assert_request(
            r,
            &Method::GET,
            &format!("/v25.0/{LINE}/owning_credit_allocation_configs"),
            &[
                ("receiving_business_id", BUSINESS),
                ("fields", "id,receiving_business"),
            ],
            SYSTEM_TOKEN,
        );
    }

    fn assert_status(r: &RecordedRequest, allocation: &str) {
        assert_request(
            r,
            &Method::GET,
            &format!("/v25.0/{allocation}"),
            &[("fields", "receiving_business,request_status")],
            SYSTEM_TOKEN,
        );
    }

    /// Onboard with share-and-attach until the line is shared and the
    /// number registered.
    async fn onboarded(h: &Harness) {
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register
        h.onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap();
    }

    /// Revoke the line `onboarded` shared: lookup, status, DELETE, status.
    async fn revoked(h: &Harness) -> CreditRevocation {
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        h.es.revoke_credit_line(&waba(), None, &h.vault)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn share_and_attach_runs_every_step_in_meta_order_with_the_right_tokens() {
        let h = harness(CreditSharing::ShareAndAttach);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared()); // owning_credit_allocation_configs
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register

        let done = h
            .onboard(&request().currency(WabaCurrency::Eur))
            .await
            .unwrap();
        assert_eq!(
            done.steps_completed,
            [
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                APPROVE,
                STORE_TOKEN,
                SUBSCRIBE_APP,
                ASSIGN_SYSTEM_USER,
                SHARE_CREDIT_LINE,
                REGISTER_PHONE
            ]
        );
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );

        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 9);
        assert_eq!(reqs[0].path(), "/v25.0/oauth/access_token");
        assert_eq!(reqs[0].header("authorization"), None);
        assert_eq!(reqs[1].path(), "/v25.0/debug_token");
        assert_request(
            &reqs[2],
            &Method::GET,
            &format!("/v25.0/{WABA}"),
            &[("fields", "owner_business_info")],
            TOKEN,
        );
        assert_request(
            &reqs[4],
            &Method::POST,
            &format!("/v25.0/{WABA}/subscribed_apps"),
            &[],
            TOKEN,
        );
        assert_request(
            &reqs[5],
            &Method::POST,
            &format!("/v25.0/{WABA}/assigned_users"),
            &[("user", SYSTEM_USER), ("tasks", r#"["MANAGE"]"#)],
            SYSTEM_TOKEN,
        );
        assert_lookup(&reqs[6]);
        assert_request(
            &reqs[7],
            &Method::POST,
            &format!("/v25.0/{LINE}/whatsapp_credit_sharing_and_attach"),
            &[("waba_currency", "EUR"), ("waba_id", WABA)],
            SYSTEM_TOKEN,
        );
        assert_eq!(reqs[7].body, RecordedBody::Empty);
        assert_request(
            &reqs[8],
            &Method::POST,
            &format!("/v25.0/{PHONE}/register"),
            &[],
            TOKEN,
        );
        assert_eq!(
            reqs[8].json(),
            Some(json!({"messaging_product": "whatsapp", "pin": PIN}))
        );
        assert_eq!(h.t.remaining(), 0);

        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(
            credit.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        assert_eq!(credit.business_id, Some(BusinessId::new(BUSINESS)));
        assert_eq!(credit.currency, Some(WabaCurrency::Eur));
        assert_eq!(credit.shared_at, Some(datetime!(2026-09-24 12:00 UTC)));
        let stored = h.vault.get(&waba()).await.unwrap().unwrap();
        assert_eq!(stored.business_id, Some(BusinessId::new(BUSINESS)));
        assert_eq!(stored.token.expose_secret(), TOKEN);
        assert!(
            h.vault
                .get_by_phone_number(&PhoneNumberId::new(PHONE))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn share_then_attach_shares_with_the_verified_owner_and_attaches_as_the_merchant() {
        let h = harness_with(Some(
            partner(CreditSharing::ShareThenAttach).default_currency(WabaCurrency::Idr),
        ));
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, nothing_shared());
        h.t.push_json(
            200,
            json!({"success": true, "allocation_config_id": ALLOCATION}),
        );
        h.t.push_json(
            200,
            json!({"success": true, "waba_id": WABA, "allocation_config_id": ALLOCATION}),
        );
        h.t.push_json(200, success()); // register

        let done = h.onboard(&request()).await.unwrap();
        assert_eq!(
            done.steps_completed,
            [
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                APPROVE,
                STORE_TOKEN,
                SUBSCRIBE_APP,
                SHARE_CREDIT_LINE,
                REGISTER_PHONE
            ],
            "no system user step for the two-call method"
        );
        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 9);
        assert_request(
            &reqs[4],
            &Method::POST,
            &format!("/v25.0/{WABA}/subscribed_apps"),
            &[],
            TOKEN,
        );
        assert_lookup(&reqs[5]);
        assert_request(
            &reqs[6],
            &Method::POST,
            &format!("/v25.0/{LINE}/whatsapp_credit_sharing"),
            &[("receiving_business_id", BUSINESS)],
            SYSTEM_TOKEN,
        );
        assert_request(
            &reqs[7],
            &Method::POST,
            &format!("/v25.0/{LINE}/whatsapp_credit_attach"),
            &[("waba_currency", "IDR"), ("waba_id", WABA)],
            TOKEN,
        );
        assert_eq!(reqs[6].body, RecordedBody::Empty);
        assert_eq!(reqs[7].body, RecordedBody::Empty);
        assert_eq!(reqs[8].path(), format!("/v25.0/{PHONE}/register"));
        for r in &reqs {
            assert!(
                !r.url.as_str().contains(CLAIMED),
                "the browser's business id reached {}",
                r.url
            );
        }
        assert_eq!(h.t.remaining(), 0);
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(
            credit.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        assert_eq!(credit.currency, Some(WabaCurrency::Idr));
    }

    #[tokio::test]
    async fn missing_or_misplaced_settings_fail_before_any_request() {
        // Solution Partner mode, no currency anywhere: the code is not spent.
        let h = harness(CreditSharing::ShareAndAttach);
        h.t.push_json(200, json!({"access_token": TOKEN}));
        let err = h.onboard(&request()).await.unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "currency"),
            "{err}"
        );
        let err =
            h.es.resume(&waba(), &request(), &h.vault)
                .await
                .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "currency"),
            "{err}"
        );
        assert!(
            h.t.requests().is_empty(),
            "nothing sent, the code not exchanged"
        );
        assert_eq!(h.t.remaining(), 1);

        // A currency Meta does not list, unless chosen as `Other` on purpose.
        let err = h
            .onboard(&request().currency(WabaCurrency::Other("eur".into())))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "waba_currency"),
            "{err}"
        );

        // A Tech Provider request naming a currency, or a re-share, is a
        // caller mistake.
        let tp = harness_with(None);
        for (request, field) in [
            (request().currency(WabaCurrency::Usd), "currency"),
            (
                request().reshare_after_revocation(),
                "reshare_after_revocation",
            ),
        ] {
            let err = tp.es.onboard(&request, &tp.vault).await.unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == field),
                "{err}"
            );
        }

        // Incomplete partner settings.
        for (bad, field) in [
            (
                SolutionPartner::new(AccessToken::new(SYSTEM_TOKEN), " ", LINE),
                "system_user_id",
            ),
            (
                SolutionPartner::new(AccessToken::new(""), SYSTEM_USER, LINE),
                "system_token",
            ),
            (
                SolutionPartner::new(AccessToken::new(SYSTEM_TOKEN), SYSTEM_USER, ""),
                "credit_line_id",
            ),
            (
                partner(CreditSharing::ShareAndAttach).system_user_tasks([]),
                "system_user_tasks",
            ),
        ] {
            let h = harness_with(Some(bad));
            let err = h
                .onboard(&request().currency(WabaCurrency::Usd))
                .await
                .unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == field),
                "{err}"
            );
            assert!(h.t.requests().is_empty());
        }
        assert!(tp.t.requests().is_empty());
    }

    /// D4: the integrator's approval runs on what Meta verified, and a
    /// refusal leaves nothing behind: no token stored, no app subscribed, no
    /// system user added, no credit line shared.
    #[tokio::test]
    async fn a_refused_approval_stores_subscribes_and_shares_nothing() {
        let h = harness(CreditSharing::ShareAndAttach);
        script_until_verified(&h.t, owner());
        let seen = std::sync::Mutex::new(None);
        let err =
            h.es.onboard_with_approval(
                &request().currency(WabaCurrency::Usd),
                &h.vault,
                |verified| {
                    *seen.lock().unwrap() = Some(verified);
                    async { Err(ValidationError::new("waba_id", "bound to another tenant").into()) }
                },
            )
            .await
            .unwrap_err();
        assert_eq!(step(&err), APPROVE);
        assert_eq!(
            h.t.remaining(),
            0,
            "nothing after verify_assets was scripted"
        );
        assert_eq!(h.t.requests().len(), 4);
        assert!(h.vault.get(&waba()).await.unwrap().is_none(), "not stored");
        assert!(h.vault.credit(&waba()).await.unwrap().is_none());
        let verified = seen.lock().unwrap().take().unwrap();
        assert_eq!(verified.waba_id, waba());
        assert_eq!(
            verified.business_id,
            Some(BusinessId::new(BUSINESS)),
            "Meta's owner, not the browser's claim"
        );
        assert_eq!(verified.phone_number_id, Some(PhoneNumberId::new(PHONE)));

        // Approved: the flow goes on, and says it asked.
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success());
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success());
        let done =
            h.es.onboard_with_approval(
                &request().currency(WabaCurrency::Usd),
                &h.vault,
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        assert_eq!(done.steps_completed[3], APPROVE);
        assert_eq!(done.steps_completed[4], STORE_TOKEN);
        assert_eq!(
            h.vault.credit(&waba()).await.unwrap().unwrap().approved_at,
            Some(datetime!(2026-09-24 12:00 UTC)),
            "the approval is recorded for resume"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_refused_share_keeps_the_token_and_resume_finishes() {
        let h = harness(CreditSharing::ShareAndAttach);
        let request = request().currency(WabaCurrency::Usd);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(
            400,
            json!({"error": {"message": "(#100) Invalid parameter", "type": "OAuthException", "code": 100, "fbtrace_id": "A"}}),
        );
        let err = h.onboard(&request).await.unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert_eq!(err.kind(), ErrorKind::InvalidParameter);
        assert!(!err.may_have_been_sent());
        assert_eq!(h.t.requests().len(), 8, "register never ran");
        let stored = h.vault.get(&waba()).await.unwrap().unwrap();
        assert_eq!(stored.token.expose_secret(), TOKEN, "the token is kept");
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(credit.allocation_config_id, None);
        assert_eq!(
            credit.currency,
            Some(WabaCurrency::Usd),
            "sealed before the post"
        );
        assert_eq!(h.t.remaining(), 0);

        // Fixed on Meta's side: resume checks, finds nothing, shares.
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register
        let done = h.es.resume(&waba(), &request, &h.vault).await.unwrap();
        assert_eq!(
            done.steps_completed,
            [
                LOAD_TOKEN,
                VERIFY_ASSETS,
                SUBSCRIBE_APP,
                ASSIGN_SYSTEM_USER,
                SHARE_CREDIT_LINE,
                REGISTER_PHONE
            ]
        );
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        let reqs = h.t.requests();
        assert_lookup(&reqs[10]);
        assert_request(
            &reqs[11],
            &Method::POST,
            &format!("/v25.0/{LINE}/whatsapp_credit_sharing_and_attach"),
            &[("waba_currency", "USD"), ("waba_id", WABA)],
            SYSTEM_TOKEN,
        );
        assert_eq!(h.t.remaining(), 0);
        assert_eq!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
    }

    #[tokio::test]
    async fn resume_after_an_ambiguous_share_checks_first_and_posts_nothing_when_shared() {
        let h = harness(CreditSharing::ShareAndAttach);
        let request = request().currency(WabaCurrency::Usd);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_error(|| TransportError::Timeout); // did Meta act? unknown
        let err = h.onboard(&request).await.unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert!(err.may_have_been_sent());
        assert_eq!(h.t.remaining(), 0);
        let before = h.t.requests().len();

        // It had gone through: the record exists, is active, and funds the
        // WABA.
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, receiving_credential(ALLOCATION, CREDENTIAL));
        h.t.push_json(200, funding(CREDENTIAL));
        h.t.push_json(200, success()); // register
        let done = h.es.resume(&waba(), &request, &h.vault).await.unwrap();
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        let reqs = h.t.requests();
        let resumed = &reqs[before..];
        assert_eq!(resumed.len(), 7);
        assert_eq!(
            posts_to(resumed, "/whatsapp_credit_sharing_and_attach"),
            0,
            "shared already: never posted again"
        );
        assert_lookup(&resumed[2]);
        assert_status(&resumed[3], ALLOCATION);
        assert_request(
            &resumed[4],
            &Method::GET,
            &format!("/v25.0/{ALLOCATION}"),
            &[("fields", "receiving_credential")],
            SYSTEM_TOKEN,
        );
        assert_request(
            &resumed[5],
            &Method::GET,
            &format!("/v25.0/{WABA}"),
            &[("fields", "primary_funding_id")],
            TOKEN,
        );
        assert_eq!(resumed[6].path(), format!("/v25.0/{PHONE}/register"));
        assert_eq!(h.t.remaining(), 0);
        assert_eq!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION)),
            "found, then recorded"
        );
    }

    /// The allocation recorded in the vault is checked even when Meta's
    /// lookup does not list it: here the two-call method shared (recorded),
    /// the attach timed out but went through, and the lookup is empty.
    #[tokio::test]
    async fn the_stored_allocation_is_checked_when_the_lookup_misses_it() {
        let h = harness(CreditSharing::ShareThenAttach);
        let request = request().currency(WabaCurrency::Inr);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, nothing_shared());
        h.t.push_json(
            200,
            json!({"success": true, "allocation_config_id": ALLOCATION}),
        );
        h.t.push_error(|| TransportError::Timeout); // the attach
        let err = h.onboard(&request).await.unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert_eq!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION)),
            "the share intent is recorded before the attach"
        );
        let before = h.t.requests().len();

        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, nothing_shared()); // Meta's lookup misses it
        h.t.push_json(200, active()); // the stored one
        h.t.push_json(200, receiving_credential(ALLOCATION, CREDENTIAL));
        h.t.push_json(200, funding(CREDENTIAL));
        h.t.push_json(200, success()); // register
        h.es.resume(&waba(), &request, &h.vault).await.unwrap();
        let reqs = h.t.requests();
        let resumed = &reqs[before..];
        assert_status(&resumed[2], ALLOCATION);
        assert_eq!(posts_to(resumed, "/whatsapp_credit_sharing"), 0);
        assert_eq!(posts_to(resumed, "/whatsapp_credit_attach"), 0);
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_record_that_does_not_fund_the_waba_is_shared_again() {
        // The business has a record (another of its WABAs), but this WABA's
        // funding is something else: the one-call share runs.
        let h = harness(CreditSharing::ShareAndAttach);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(
            200,
            json!({"data": [
                {"id": "OTHER_BUSINESS_RECORD", "receiving_business": {"id": "SOMEONE_ELSE"}},
                {"id": "EARLIER", "receiving_business": {"id": BUSINESS}}
            ]}),
        );
        h.t.push_json(200, active());
        h.t.push_json(200, receiving_credential("EARLIER", "CRED_OF_ANOTHER_WABA"));
        h.t.push_json(200, json!({"id": WABA})); // no primary_funding_id yet
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register
        let done = h
            .onboard(&request().currency(WabaCurrency::Gbp))
            .await
            .unwrap();
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        let reqs = h.t.requests();
        assert_eq!(
            reqs.iter()
                .filter(|r| r.path() == "/v25.0/OTHER_BUSINESS_RECORD")
                .count(),
            0,
            "another business's record is never considered"
        );
        assert_status(&reqs[7], "EARLIER");
        assert_eq!(reqs[8].path(), "/v25.0/EARLIER");
        assert_eq!(
            reqs[10].path(),
            format!("/v25.0/{LINE}/whatsapp_credit_sharing_and_attach")
        );
        assert_eq!(reqs[10].query("waba_currency").as_deref(), Some("GBP"));
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn share_then_attach_resume_attaches_without_sharing_twice() {
        let h = harness(CreditSharing::ShareThenAttach);
        let request = request().currency(WabaCurrency::Inr);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, nothing_shared());
        h.t.push_json(
            200,
            json!({"success": true, "allocation_config_id": ALLOCATION}),
        );
        h.t.push_json(
            400,
            json!({"error": {"message": "(#100) Invalid currency", "type": "OAuthException", "code": 100}}),
        );
        let err = h.onboard(&request).await.unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        // The attach reached nothing, but the share went out: the answer
        // says so, and the ledger keeps the outcome pending.
        assert!(
            matches!(err.credit(), Some(CreditError::AttachFailed { allocation_config_id, .. }) if allocation_config_id.as_str() == ALLOCATION),
            "{err}"
        );
        assert!(err.may_have_been_sent(), "the share went out");
        assert!(!err.is_retryable(), "a 400: fix the request, then resume");
        assert_eq!(err.kind(), ErrorKind::InvalidParameter);
        assert!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .pending_share
                .is_some()
        );
        let before = h.t.requests().len();

        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(
            200,
            json!({"receiving_credential": {"id": CREDENTIAL}, "id": ALLOCATION}),
        );
        h.t.push_json(200, json!({"id": WABA}));
        h.t.push_json(
            200,
            json!({"success": true, "waba_id": WABA, "allocation_config_id": ALLOCATION}),
        );
        h.t.push_json(200, success()); // register
        h.es.resume(&waba(), &request, &h.vault).await.unwrap();
        let reqs = h.t.requests();
        let resumed = &reqs[before..];
        assert_eq!(
            posts_to(resumed, "/whatsapp_credit_sharing"),
            0,
            "shared once"
        );
        assert_request(
            &resumed[5],
            &Method::POST,
            &format!("/v25.0/{LINE}/whatsapp_credit_attach"),
            &[("waba_currency", "INR"), ("waba_id", WABA)],
            TOKEN,
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 1b: Meta reported no owner business, so nothing can be looked
    /// up, checked for a revocation, or revoked later: neither method
    /// shares, on the first attempt or on resume, and nothing is posted.
    #[tokio::test]
    async fn an_unknown_owner_business_is_never_shared_with() {
        for method in [
            CreditSharing::ShareAndAttach,
            CreditSharing::ShareThenAttach,
        ] {
            let h = harness(method);
            let request = request().currency(WabaCurrency::Usd);
            script_until_subscribe(&h.t, json!({"id": WABA}));
            if method == CreditSharing::ShareAndAttach {
                h.t.push_json(200, success()); // assigned_users
            }
            let err = h.onboard(&request).await.unwrap_err();
            assert_eq!(step(&err), SHARE_CREDIT_LINE);
            assert!(
                matches!(err.credit(), Some(CreditError::OwnerUnknown(_))),
                "{err}"
            );
            assert!(!err.may_have_been_sent() && !err.is_retryable());
            assert_eq!(credit_calls(&h.t.requests()), 0, "{method:?}");
            assert_eq!(h.t.remaining(), 0);

            h.t.push_json(200, success()); // subscribe
            if method == CreditSharing::ShareAndAttach {
                h.t.push_json(200, success()); // assigned_users
            }
            let err = h.es.resume(&waba(), &request, &h.vault).await.unwrap_err();
            assert!(
                matches!(err.credit(), Some(CreditError::OwnerUnknown(_))),
                "{err}"
            );
            assert_eq!(credit_calls(&h.t.requests()), 0, "{method:?}");
            assert_eq!(h.t.remaining(), 0);
        }

        // The probe of the review: an allocation recorded, no business
        // anywhere, Meta's record DELETED. Refused before any lookup.
        let h = harness(CreditSharing::ShareAndAttach);
        let mut credit = StoredCredit::new(waba());
        credit.allocation_config_id = Some(AllocationConfigId::new(ALLOCATION));
        h.vault.put_credit(&credit, None).await.unwrap();
        script_until_subscribe(&h.t, json!({"id": WABA}));
        h.t.push_json(200, success()); // assigned_users
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::OwnerUnknown(_))),
            "{err}"
        );
        assert_eq!(credit_calls(&h.t.requests()), 0);
        assert_eq!(h.t.remaining(), 0);
    }

    /// H1: once the line is revoked, neither `resume` nor a new onboarding
    /// funds the business again unless the request opts in.
    #[tokio::test]
    async fn a_revoked_business_is_not_funded_again_without_the_opt_in() {
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        let report = revoked(&h).await;
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        let marker = h
            .vault
            .revoked_business(&BusinessId::new(BUSINESS))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            marker.allocation_config_ids,
            [AllocationConfigId::new(ALLOCATION)]
        );
        let before = h.t.requests().len();

        // resume: refused, before any credit call.
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        let err =
            h.es.resume(&waba(), &request().currency(WabaCurrency::Usd), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert!(EmbeddedSignup::is_credit_line_revoked(&err), "{err}");
        assert_eq!(credit_calls(&h.t.requests()[before..]), 0);
        assert_eq!(h.t.remaining(), 0);

        // A new onboarding of the same business: refused the same way.
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(EmbeddedSignup::is_credit_line_revoked(&err), "{err}");
        assert_eq!(credit_calls(&h.t.requests()[before..]), 0);
        assert_eq!(h.t.remaining(), 0);
        let before = h.t.requests().len();

        // With the opt-in: the revoked record is seen as such, the line is
        // shared again, and the marker cleared.
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        h.t.push_json(
            200,
            json!({"allocation_config_id": "NEW_ALLOCATION", "waba_id": WABA}),
        );
        h.t.push_json(200, success()); // register
        let done =
            h.es.resume(
                &waba(),
                &request()
                    .currency(WabaCurrency::Usd)
                    .reshare_after_revocation(),
                &h.vault,
            )
            .await
            .unwrap();
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new("NEW_ALLOCATION"))
        );
        let resumed = &h.t.requests()[before..];
        assert_status(&resumed[3], ALLOCATION);
        assert_eq!(posts_to(resumed, "/whatsapp_credit_sharing_and_attach"), 1);
        assert_eq!(h.t.remaining(), 0);
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Revoked on Meta's side (Business Suite, another deployment): only
    /// `DELETED` records and no marker here. Still refused.
    #[tokio::test]
    async fn only_deleted_records_on_metas_side_count_as_revoked() {
        let h = harness(CreditSharing::ShareThenAttach);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(EmbeddedSignup::is_credit_line_revoked(&err), "{err}");
        let reqs = h.t.requests();
        assert_eq!(posts_to(&reqs, "/whatsapp_credit_sharing"), 0);
        assert_eq!(posts_to(&reqs, "/whatsapp_credit_attach"), 0);
        assert_eq!(h.t.remaining(), 0);

        // Opted in, the two-call method shares again rather than attaching
        // to the revoked record.
        let before = reqs.len();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        h.t.push_json(
            200,
            json!({"success": true, "allocation_config_id": "NEW_ALLOCATION"}),
        );
        h.t.push_json(
            200,
            json!({"success": true, "waba_id": WABA, "allocation_config_id": "NEW_ALLOCATION"}),
        );
        h.t.push_json(200, success()); // register
        h.es.resume(
            &waba(),
            &request()
                .currency(WabaCurrency::Usd)
                .reshare_after_revocation(),
            &h.vault,
        )
        .await
        .unwrap();
        let resumed = &h.t.requests()[before..];
        assert_eq!(posts_to(resumed, "/whatsapp_credit_sharing"), 1);
        assert_eq!(posts_to(resumed, "/whatsapp_credit_attach"), 1);
        assert_eq!(h.t.remaining(), 0);
    }

    /// L2: the first currency is sealed; another one is refused before any
    /// credit call.
    #[tokio::test]
    async fn another_currency_than_the_sealed_one_is_refused() {
        let h = harness(CreditSharing::ShareAndAttach);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_error(|| TransportError::Timeout);
        h.onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        let before = h.t.requests().len();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        let err =
            h.es.resume(&waba(), &request().currency(WabaCurrency::Eur), &h.vault)
                .await
                .unwrap_err();
        assert!(
            matches!(&err, Error::Step { source, .. } if matches!(&**source, Error::Validation(v) if v.field == "waba_currency")),
            "{err}"
        );
        assert_eq!(credit_calls(&h.t.requests()[before..]), 0);
        assert_eq!(h.t.remaining(), 0);
    }

    /// L3: while one onboarding holds the WABA's credit step, another does
    /// not post; the lease is released afterwards.
    #[tokio::test]
    async fn a_concurrent_credit_step_is_refused_and_the_lease_released() {
        let h = harness(CreditSharing::ShareAndAttach);
        let lease = h.vault.lease_credit(&waba()).await.unwrap();
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert!(EmbeddedSignup::is_credit_step_busy(&err), "{err}");
        assert!(err.is_retryable(), "resume later: {err}");
        assert!(!err.may_have_been_sent(), "nothing was posted");
        assert_eq!(credit_calls(&h.t.requests()), 0);
        assert_eq!(h.t.remaining(), 0);
        h.vault.release_credit(&waba(), lease).await.unwrap();

        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register
        h.es.resume(&waba(), &request().currency(WabaCurrency::Usd), &h.vault)
            .await
            .unwrap();
        // Released on success, and on failure: taken again at once.
        let again = h.vault.lease_credit(&waba()).await.unwrap();
        h.vault.release_credit(&waba(), again).await.unwrap();
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn revocation_uses_the_stored_owner_not_the_unreadable_waba() {
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        let before = h.t.requests().len();

        // PARTNER_REMOVED: `GET /{WABA}?fields=owner_business_info` would now
        // fail; revocation never asks.
        let report = revoked(&h).await;
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_eq!(report.business_id, Some(BusinessId::new(BUSINESS)));
        let reqs = h.t.requests();
        let revocation = &reqs[before..];
        assert_eq!(revocation.len(), 4);
        assert_lookup(&revocation[0]);
        assert_status(&revocation[1], ALLOCATION);
        assert_request(
            &revocation[2],
            &Method::DELETE,
            &format!("/v25.0/{ALLOCATION}"),
            &[],
            SYSTEM_TOKEN,
        );
        assert_status(&revocation[3], ALLOCATION);
        assert!(
            revocation
                .iter()
                .all(|r| r.path() != format!("/v25.0/{WABA}"))
        );
        assert_eq!(h.t.remaining(), 0);

        // Again: already revoked, nothing deleted twice.
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        let again =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap();
        assert_eq!(again.revoked, []);
        assert_eq!(again.already_revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_eq!(h.t.remaining(), 0);

        // Without a partner configuration, or anything to go on: refused.
        let tp = harness_with(None);
        assert!(matches!(
            tp.es.revoke_credit_line(&waba(), None, &h.vault).await,
            Err(Error::Validation(_))
        ));
        assert!(matches!(
            h.es.revoke_credit_line(&WabaId::new("UNKNOWN"), None, &h.vault)
                .await,
            Err(Error::Validation(v)) if v.field == "business_id"
        ));
        assert_eq!(h.t.requests().len(), before + 6);
    }

    /// H2: with no token and no ledger (never onboarded here, or wiped),
    /// the signed webhook's owner business is enough; a hint that
    /// contradicts what onboarding recorded revokes nothing.
    #[tokio::test]
    async fn revocation_from_the_webhooks_owner_and_never_from_a_contradicting_one() {
        let h = harness(CreditSharing::ShareAndAttach);
        h.t.push_json(200, shared_record(ALLOCATION)); // the hint has records
        h.t.push_json(200, shared_record(ALLOCATION)); // the lookup, after marking
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let report =
            h.es.revoke_credit_line(&waba(), Some(&BusinessId::new(BUSINESS)), &h.vault)
                .await
                .unwrap();
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_lookup(&h.t.requests()[0]);
        assert_lookup(&h.t.requests()[1]);
        assert_eq!(h.t.remaining(), 0);
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            h.vault.credit(&waba()).await.unwrap().unwrap().business_id,
            Some(BusinessId::new(BUSINESS)),
            "the business is recorded for the WABA, and rotated with it"
        );

        // Fix 9: a caller-supplied business the line has no record of is
        // neither marked nor recorded.
        let h = harness(CreditSharing::ShareAndAttach);
        h.t.push_json(200, nothing_shared());
        let report =
            h.es.revoke_credit_line(&waba(), Some(&BusinessId::new(BUSINESS)), &h.vault)
                .await
                .unwrap();
        assert_eq!(report.all().count(), 0);
        assert_eq!(h.t.requests().len(), 1, "one lookup, no DELETE");
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_none(),
            "an unsupported id must not block that business's onboarding"
        );
        assert!(h.vault.credit(&waba()).await.unwrap().is_none());
        assert_eq!(h.t.remaining(), 0);

        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        let before = h.t.requests().len();
        let err =
            h.es.revoke_credit_line(
                &waba(),
                Some(&BusinessId::new("2949482758682047")),
                &h.vault,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "owner_business_id"),
            "{err}"
        );
        assert_eq!(h.t.requests().len(), before, "nothing sent");
    }

    #[tokio::test]
    async fn revocation_falls_back_to_the_stored_allocation_when_the_lookup_finds_none() {
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        let before = h.t.requests().len();
        // The lookup answers records Meta did not attribute to the business,
        // and not the stored one.
        h.t.push_json(200, json!({"data": [{"id": "UNATTRIBUTED"}]}));
        h.t.push_json(200, active()); // the stored allocation names BUSINESS
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let err =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap_err();
        let incomplete = err.credit().and_then(CreditError::revocation).unwrap();
        assert_eq!(
            incomplete.unattributed,
            [AllocationConfigId::new("UNATTRIBUTED")],
            "reported, not revoked"
        );
        assert_eq!(
            incomplete.report.revoked,
            [AllocationConfigId::new(ALLOCATION)]
        );
        assert!(err.may_have_been_sent() && !err.is_retryable());
        let reqs = h.t.requests();
        let revocation = &reqs[before..];
        assert_request(
            &revocation[2],
            &Method::DELETE,
            &format!("/v25.0/{ALLOCATION}"),
            &[],
            SYSTEM_TOKEN,
        );
        assert!(
            revocation.iter().all(|r| r.path() != "/v25.0/UNATTRIBUTED"),
            "never revoked blindly"
        );
        assert_eq!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .unwrap()
                .allocation_config_ids,
            [AllocationConfigId::new(ALLOCATION)],
            "a partial revocation still lists what it revoked"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_stored_allocation_naming_another_business_is_never_revoked() {
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        let before = h.t.requests().len();
        h.t.push_json(200, nothing_shared());
        h.t.push_json(
            200,
            json!({"receiving_business": {"name": "Someone Else", "id": "SOMEONE_ELSE"}}),
        );
        let err =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap_err();
        let incomplete = err.credit().and_then(CreditError::revocation).unwrap();
        assert_eq!(incomplete.failed, [AllocationConfigId::new(ALLOCATION)]);
        assert!(
            matches!(incomplete.source.as_deref(), Some(Error::Validation(v)) if v.field == "allocation_config_id"),
            "{err}"
        );
        assert!(!err.may_have_been_sent(), "nothing deleted");
        assert!(
            h.t.requests()[before..]
                .iter()
                .all(|r| r.method != Method::DELETE)
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// H2: offboarding revokes first and deletes second, and the ledger
    /// outlives the token, so a `PARTNER_REMOVED` in either order ends with
    /// the line revoked.
    #[tokio::test]
    async fn offboarding_revokes_then_deletes_in_either_order() {
        // CMS disconnect / PARTNER_APP_UNINSTALLED first, then PARTNER_REMOVED.
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        let before = h.t.requests().len();
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let off = h.es.offboard(&waba(), None, &h.vault).await.unwrap();
        assert!(off.token_deleted);
        assert_eq!(
            off.credit.unwrap().revoked,
            [AllocationConfigId::new(ALLOCATION)]
        );
        assert!(h.vault.get(&waba()).await.unwrap().is_none());
        assert!(
            h.vault
                .get_by_phone_number(&PhoneNumberId::new(PHONE))
                .await
                .unwrap()
                .is_none()
        );
        assert!(h.vault.credit(&waba()).await.unwrap().is_some(), "kept");
        assert_eq!(
            h.t.requests()[before + 2].method,
            Method::DELETE,
            "revoked before the vault entry went"
        );
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        let removed =
            h.es.revoke_credit_line(&waba(), Some(&BusinessId::new(BUSINESS)), &h.vault)
                .await
                .unwrap();
        assert_eq!(
            removed.already_revoked,
            [AllocationConfigId::new(ALLOCATION)]
        );
        assert_eq!(h.t.remaining(), 0);

        // PARTNER_REMOVED first, then the uninstall.
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        h.es.revoke_credit_line(&waba(), Some(&BusinessId::new(BUSINESS)), &h.vault)
            .await
            .unwrap();
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        let off =
            h.es.offboard(&waba(), Some(&BusinessId::new(BUSINESS)), &h.vault)
                .await
                .unwrap();
        assert_eq!(
            off.credit.unwrap().already_revoked,
            [AllocationConfigId::new(ALLOCATION)]
        );
        assert!(off.token_deleted);
        assert_eq!(h.t.remaining(), 0);

        // A failed revocation deletes nothing: the call can be repeated.
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        h.t.push_json(
            500,
            json!({"error": {"message": "unknown", "type": "OAuthException", "code": 1}}),
        );
        let err = h.es.offboard(&waba(), None, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), REVOKE_CREDIT_LINE);
        assert!(h.vault.get(&waba()).await.unwrap().is_some());
        assert_eq!(h.t.remaining(), 0);

        // A Tech Provider only deletes.
        let tp = harness_with(None);
        tp.vault
            .store(&StoredBusinessToken::new(WABA, AccessToken::new(TOKEN)))
            .await
            .unwrap();
        let off = tp.es.offboard(&waba(), None, &tp.vault).await.unwrap();
        assert_eq!(off.credit, None);
        assert!(off.token_deleted);
        assert!(tp.t.requests().is_empty());
    }

    /// The marker is written before anything is sent: a revocation that
    /// fails half-way still keeps `resume` from funding the business, and
    /// the allocation recorded at onboarding is revoked even though the
    /// lookup failed.
    #[tokio::test]
    async fn a_failed_revocation_still_blocks_a_re_share() {
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        let before = h.t.requests().len();
        h.t.push_json(
            500,
            json!({"error": {"message": "unknown", "type": "OAuthException", "code": 1}}),
        );
        h.t.push_json(200, active()); // the recorded allocation
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let err =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ServiceUnavailable, "{err}");
        let revocation = &h.t.requests()[before..];
        assert_eq!(
            revocation
                .iter()
                .filter(|r| r.method == Method::DELETE)
                .count(),
            1,
            "the recorded allocation is revoked despite the failed lookup"
        );
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(h.t.remaining(), 0);

        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        let err =
            h.es.resume(&waba(), &request().currency(WabaCurrency::Usd), &h.vault)
                .await
                .unwrap_err();
        assert!(EmbeddedSignup::is_credit_line_revoked(&err), "{err}");
        assert_eq!(h.t.remaining(), 0);
    }

    /// An unreadable token record does not stop a revocation the credit
    /// ledger can still address.
    #[tokio::test]
    async fn revocation_survives_an_unreadable_token_record() {
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        // The same WABA's record, sealed under a key this vault does not have.
        let other = TokenVault::new(
            Arc::new(MemoryKvStore::new()),
            VaultKeys::new(VaultKey::new("gone", SecretBytes::new([9; 32])).unwrap()),
        )
        .unwrap();
        let foreign = StoredBusinessToken::new(WABA, AccessToken::new(TOKEN)).business_id("X");
        other.store(&foreign).await.unwrap();
        let bytes = other
            .kv()
            .get(&wa_core::store::StoreKey::new(
                super::super::vault::TOKEN_NAMESPACE,
                format!("waba/{WABA}"),
            ))
            .await
            .unwrap()
            .unwrap()
            .value;
        h.vault
            .kv()
            .put(
                &wa_core::store::StoreKey::new(
                    super::super::vault::TOKEN_NAMESPACE,
                    format!("waba/{WABA}"),
                ),
                bytes,
                wa_core::store::Expiry::Never,
            )
            .await
            .unwrap();
        assert!(h.vault.get(&waba()).await.is_err(), "vacuous otherwise");
        let report = revoked(&h).await;
        assert_eq!(report.business_id, Some(BusinessId::new(BUSINESS)));
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_eq!(h.t.remaining(), 0);

        // With nothing readable and nothing given, the read error is what
        // comes back.
        let h = harness(CreditSharing::ShareAndAttach);
        h.vault
            .kv()
            .put(
                &wa_core::store::StoreKey::new(
                    super::super::vault::TOKEN_NAMESPACE,
                    format!("waba/{WABA}"),
                ),
                b"{}".to_vec(),
                wa_core::store::Expiry::Never,
            )
            .await
            .unwrap();
        assert!(matches!(
            h.es.revoke_credit_line(&waba(), None, &h.vault).await,
            Err(Error::Storage(_))
        ));
        assert!(h.t.requests().is_empty());
    }

    /// An allocation recorded for the WABA that Meta says is shared with
    /// another business is never used to decide the WABA is funded.
    #[tokio::test]
    async fn a_recorded_allocation_naming_another_business_is_refused() {
        let h = harness(CreditSharing::ShareAndAttach);
        let mut credit = StoredCredit::new(waba());
        credit.allocation_config_id = Some(AllocationConfigId::new(ALLOCATION));
        h.vault.put_credit(&credit, None).await.unwrap();
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(
            200,
            json!({"receiving_business": {"name": "Someone Else", "id": "SOMEONE_ELSE"}}),
        );
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Step { source, .. } if matches!(&**source, Error::Validation(v) if v.field == "allocation_config_id")),
            "{err}"
        );
        let reqs = h.t.requests();
        assert_eq!(posts_to(&reqs, "/whatsapp_credit_sharing_and_attach"), 0);
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 2, the decisive test: a revocation runs between the share's
    /// marker check and its POST (another process: it finds nothing to
    /// revoke yet, and reports the share in flight). The share reads the
    /// marker after posting, revokes what it just shared, and reports the
    /// business revoked: the line ends revoked. B (security review of
    /// caadb55): the same when the POST's answer is lost.
    #[tokio::test]
    async fn a_revocation_racing_a_share_ends_with_the_line_revoked() {
        for answer_lost in [false, true] {
            a_revocation_racing_a_share(answer_lost).await;
        }
    }

    async fn a_revocation_racing_a_share(answer_lost: bool) {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared()); // the share's check: nothing yet
        h.t.push_json(200, nothing_shared()); // the revocation's lookup, before the POST
        if answer_lost {
            h.t.push_error(|| TransportError::Timeout); // the POST
        } else {
            h.t.push_json(200, shared_and_attached()); // the POST
        }
        h.t.push_json(200, shared_record(ALLOCATION)); // the share revokes itself
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        hooked.before(
            Method::POST,
            "/whatsapp_credit_sharing_and_attach",
            move || Box::pin(async move { revoke_before_the_post(&other).await }),
        );
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert!(EmbeddedSignup::is_credit_line_revoked(&err), "{err}");
        assert!(
            matches!(
                err.credit(),
                Some(CreditError::Revoked { posted: true, .. })
            ),
            "{err}"
        );
        assert!(err.may_have_been_sent(), "a share went out");
        let reqs = h.t.requests();
        assert_eq!(deletes(&reqs), [format!("/v25.0/{ALLOCATION}")]);
        assert!(reqs.iter().all(|r| !r.path().ends_with("/register")));
        let marker = h
            .vault
            .revoked_business(&BusinessId::new(BUSINESS))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            marker.allocation_config_ids,
            [AllocationConfigId::new(ALLOCATION)]
        );
        assert_eq!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION)),
            "recorded for any later revocation"
        );
        assert_eq!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .pending_share,
            None,
            "the share's outcome is known: revoked"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// A (audit of caadb55, `zz_audit_timeout_race.rs`): a revocation runs
    /// right before a share POST whose answer is then lost. The line ends
    /// revoked, or reported: never a live share with a "revoked, nothing
    /// posted" answer. The first case is the auditor's scenario as written,
    /// with what the fix then asks of Meta scripted after it.
    #[tokio::test]
    async fn a_timed_out_share_racing_a_revocation_ends_revoked_or_reported() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared()); // the share's check
        h.t.push_json(200, nothing_shared()); // the revocation's lookup, before the POST
        h.t.push_error(|| TransportError::Timeout); // the POST: applied by Meta, answer lost
        // The share sees the revocation's marker and revokes by business:
        // Meta lists the share now.
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        hooked.before(
            Method::POST,
            "/whatsapp_credit_sharing_and_attach",
            move || Box::pin(async move { revoke_before_the_post(&other).await }),
        );
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert!(
            matches!(
                err.credit(),
                Some(CreditError::Revoked { posted: true, .. })
            ),
            "{err}"
        );
        assert!(err.may_have_been_sent() && !err.is_retryable());
        let reqs = h.t.requests();
        assert_eq!(deletes(&reqs), [format!("/v25.0/{ALLOCATION}")]);
        assert!(reqs.iter().all(|r| !r.path().ends_with("/register")));
        let marker = h
            .vault
            .revoked_business(&BusinessId::new(BUSINESS))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            marker.allocation_config_ids,
            [AllocationConfigId::new(ALLOCATION)]
        );
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        let err =
            h.es.resume(&waba(), &request().currency(WabaCurrency::Usd), &h.vault)
                .await
                .unwrap_err();
        // True now: the share was revoked (the DELETE above was confirmed).
        assert!(EmbeddedSignup::is_credit_line_revoked(&err), "{err}");
        assert_eq!(h.t.remaining(), 0);
    }

    /// A, continued: Meta's lookup does not list the lost share yet, so
    /// nothing can be revoked: it is reported (`Reconcile`), stays pending,
    /// and every later answer says so until a revocation revokes it.
    #[tokio::test]
    async fn a_timed_out_share_meta_does_not_list_yet_is_reported() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared()); // the share's check
        h.t.push_json(200, nothing_shared()); // the revocation's lookup, before the POST
        h.t.push_error(|| TransportError::Timeout); // the POST
        h.t.push_json(200, nothing_shared()); // the share's own revocation
        hooked.before(
            Method::POST,
            "/whatsapp_credit_sharing_and_attach",
            move || Box::pin(async move { revoke_before_the_post(&other).await }),
        );
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        assert!(err.may_have_been_sent() && !err.is_retryable());
        assert!(deletes(&h.t.requests()).is_empty());
        assert!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .pending_share
                .is_some()
        );
        assert_eq!(h.t.remaining(), 0);

        // The auditor's resume: a share whose outcome is unknown, and the
        // business revoked since, is Reconcile, never "revoked, nothing
        // posted", and sends no credit call.
        let before = h.t.requests().len();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        let err =
            h.es.resume(&waba(), &request().currency(WabaCurrency::Usd), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        assert!(err.may_have_been_sent());
        assert_eq!(credit_calls(&h.t.requests()[before..]), 0);
        assert_eq!(h.t.remaining(), 0);

        // The PARTNER_REMOVED handler's revocation, while Meta still lists
        // nothing: incomplete, retryable, never done.
        h.t.push_json(200, nothing_shared());
        let err =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap_err();
        let incomplete = err.credit().and_then(CreditError::revocation).unwrap();
        assert!(incomplete.share_pending && err.is_retryable(), "{err}");
        // Called again once Meta lists it: revoked, and settled.
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let report =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap();
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_eq!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .pending_share,
            None
        );
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        let err =
            h.es.resume(&waba(), &request().currency(WabaCurrency::Usd), &h.vault)
                .await
                .unwrap_err();
        assert!(EmbeddedSignup::is_credit_line_revoked(&err), "{err}");
        assert!(!err.may_have_been_sent());
        assert_eq!(h.t.remaining(), 0);
    }

    /// Z01: a marker that cannot be read after the POST counts as a
    /// revocation: the new allocation is revoked, and the marker replaced.
    #[tokio::test]
    async fn an_unreadable_marker_after_the_post_revokes_the_share() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        hooked.before(
            Method::POST,
            "/whatsapp_credit_sharing_and_attach",
            move || {
                Box::pin(async move {
                    // A marker sealed under a key since dropped, say.
                    other
                        .vault
                        .kv()
                        .put(
                            &wa_core::store::StoreKey::new(
                                super::super::vault::TOKEN_NAMESPACE,
                                format!("revoked/{BUSINESS}"),
                            ),
                            b"{}".to_vec(),
                            wa_core::store::Expiry::Never,
                        )
                        .await
                        .unwrap();
                })
            },
        );
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err.credit(),
                Some(CreditError::Revoked { posted: true, .. })
            ),
            "{err}"
        );
        assert_eq!(deletes(&h.t.requests()), [format!("/v25.0/{ALLOCATION}")]);
        assert!(
            h.t.requests()
                .iter()
                .all(|r| !r.path().ends_with("/register"))
        );
        assert_eq!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .unwrap()
                .allocation_config_ids,
            [AllocationConfigId::new(ALLOCATION)],
            "replaced by a readable marker"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Z21: a revocation that runs while `resume` checks a line that already
    /// funds the WABA: nothing is posted, and the business is reported
    /// revoked (the revocation revoked that record), not funded.
    #[tokio::test]
    async fn a_revocation_during_the_check_is_reported_without_a_post() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        onboarded(&h).await;
        let before = h.t.requests().len();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, receiving_credential(ALLOCATION, CREDENTIAL));
        // The revocation, right before the WABA's funding is read.
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        h.t.push_json(200, funding(CREDENTIAL));
        hooked.before(Method::GET, "/102290129340398", move || {
            Box::pin(async move {
                let report = other
                    .es
                    .revoke_credit_line(&waba(), None, &other.vault)
                    .await
                    .unwrap();
                assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
            })
        });
        let err =
            h.es.resume(&waba(), &request().currency(WabaCurrency::Usd), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert!(
            matches!(
                err.credit(),
                Some(CreditError::Revoked { posted: false, .. })
            ),
            "{err}"
        );
        assert!(!err.may_have_been_sent());
        let resumed = &h.t.requests()[before..];
        assert_eq!(posts_to(resumed, "/whatsapp_credit_sharing_and_attach"), 0);
        assert!(resumed.iter().all(|r| !r.path().ends_with("/register")));
        assert_eq!(h.t.remaining(), 0);
    }

    /// Z13, Z34, Z14: the two-call method shared, then lost its lease (or
    /// could not renew it) before the attach: nothing is attached, and the
    /// answer says the share went out; `resume` later attaches it without
    /// sharing again.
    #[tokio::test]
    async fn a_lease_lost_between_the_two_calls_attaches_nothing() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareThenAttach, Arc::default());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, nothing_shared());
        h.t.push_json(
            200,
            json!({"success": true, "allocation_config_id": ALLOCATION}),
        );
        hooked.before(Method::POST, "/whatsapp_credit_sharing", move || {
            Box::pin(async move {
                // The lease expired during the share; another onboarding
                // took it.
                other.vault.kv().delete(&lease_key()).await.unwrap();
                other.vault.lease_credit(&waba()).await.unwrap();
            })
        });
        let request = request().currency(WabaCurrency::Usd);
        let err = h.onboard(&request).await.unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Busy { posted: true, .. })),
            "{err}"
        );
        assert!(err.may_have_been_sent() && err.is_retryable(), "{err}");
        assert_eq!(posts_to(&h.t.requests(), "/whatsapp_credit_sharing"), 1);
        assert_eq!(
            posts_to(&h.t.requests(), "/whatsapp_credit_attach"),
            0,
            "no attach without the lease"
        );
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(
            credit.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        assert!(
            credit.pending_share.is_some(),
            "the attach is still unknown"
        );
        assert_eq!(h.t.remaining(), 0);

        // Later: the other holder is gone; resume attaches the recorded
        // share without sharing again.
        h.vault.kv().delete(&lease_key()).await.unwrap();
        let before = h.t.requests().len();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, nothing_shared()); // the lookup misses it
        h.t.push_json(200, active()); // the recorded one
        h.t.push_json(200, receiving_credential(ALLOCATION, CREDENTIAL));
        h.t.push_json(200, json!({"id": WABA})); // not funded yet
        h.t.push_json(
            200,
            json!({"success": true, "waba_id": WABA, "allocation_config_id": ALLOCATION}),
        );
        h.t.push_json(200, success()); // register
        h.es.resume(&waba(), &request, &h.vault).await.unwrap();
        let resumed = &h.t.requests()[before..];
        assert_eq!(posts_to(resumed, "/whatsapp_credit_sharing"), 0);
        assert_eq!(posts_to(resumed, "/whatsapp_credit_attach"), 1);
        assert_eq!(h.t.remaining(), 0);

        // The renewal itself fails (the store is down) after the share.
        let kv = Arc::new(TestKv::default());
        let (h, hooked, _) = hooked_harness(CreditSharing::ShareThenAttach, kv.clone());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, nothing_shared());
        h.t.push_json(
            200,
            json!({"success": true, "allocation_config_id": ALLOCATION}),
        );
        hooked.before(Method::POST, "/whatsapp_credit_sharing", move || {
            Box::pin(async move { TestKv::refuse(&kv.refuse_lease_writes) })
        });
        let err = h.onboard(&request).await.unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Busy { posted: true, .. })),
            "a renewal failure after a post is typed: {err}"
        );
        assert!(err.may_have_been_sent());
        assert_eq!(posts_to(&h.t.requests(), "/whatsapp_credit_attach"), 0);
        assert_eq!(h.t.remaining(), 0);
    }

    /// Z26, Z28: a share the ledger cannot record is Reconcile, never a raw
    /// storage error, and the two-call method attaches nothing it could not
    /// record. A raced share that can be neither revoked nor recorded is
    /// Reconcile too.
    #[tokio::test]
    async fn a_share_the_ledger_cannot_record_is_reconciled() {
        // One call: shared, not recorded.
        let kv = Arc::new(TestKv::default());
        let (h, hooked, _) = hooked_harness(CreditSharing::ShareAndAttach, kv.clone());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, shared_and_attached());
        let refuse = kv.clone();
        hooked.before(
            Method::POST,
            "/whatsapp_credit_sharing_and_attach",
            move || Box::pin(async move { TestKv::refuse(&refuse.refuse_credit_writes) }),
        );
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        assert!(err.may_have_been_sent() && !err.is_retryable());
        assert!(
            h.t.requests()
                .iter()
                .all(|r| !r.path().ends_with("/register"))
        );
        assert_eq!(h.t.remaining(), 0);

        // Two calls: the share intent cannot be recorded, so no attach.
        let kv = Arc::new(TestKv::default());
        let (h, hooked, _) = hooked_harness(CreditSharing::ShareThenAttach, kv.clone());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, nothing_shared());
        h.t.push_json(
            200,
            json!({"success": true, "allocation_config_id": ALLOCATION}),
        );
        hooked.before(Method::POST, "/whatsapp_credit_sharing", move || {
            Box::pin(async move { TestKv::refuse(&kv.refuse_credit_writes) })
        });
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(m)) if m.contains("nothing was attached")),
            "{err}"
        );
        assert!(err.may_have_been_sent());
        assert_eq!(posts_to(&h.t.requests(), "/whatsapp_credit_attach"), 0);
        assert_eq!(h.t.remaining(), 0);

        // Raced, not recorded, and the revocation fails: Reconcile.
        let kv = Arc::new(TestKv::default());
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, kv.clone());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared()); // the share's check
        h.t.push_json(200, nothing_shared()); // the revocation's lookup
        h.t.push_json(200, shared_and_attached());
        let failure = json!({"error": {"message": "unknown", "type": "OAuthException", "code": 1}});
        for _ in 0..4 {
            h.t.push_json(500, failure.clone()); // lookup, status, DELETE, status
        }
        hooked.before(
            Method::POST,
            "/whatsapp_credit_sharing_and_attach",
            move || {
                Box::pin(async move {
                    revoke_before_the_post(&other).await;
                    TestKv::refuse(&kv.refuse_credit_writes);
                })
            },
        );
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(m)) if m.contains("neither revoked nor recorded")),
            "{err}"
        );
        assert!(err.may_have_been_sent() && !err.is_retryable());
        assert_eq!(h.t.remaining(), 0);
    }

    /// Z18: an allocation a racing share records after the revocation's
    /// first read, which Meta's lookup does not list yet, is still revoked.
    #[tokio::test]
    async fn a_revocation_revokes_an_allocation_recorded_meanwhile() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        h.t.push_json(200, shared_record("EARLIER")); // the hint has records
        h.t.push_json(200, nothing_shared()); // the lookup: not listed yet
        h.t.push_json(200, active()); // the recorded one
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        hooked.before(
            Method::GET,
            "/owning_credit_allocation_configs",
            move || {
                Box::pin(async move {
                    other
                        .vault
                        .update_credit(&waba(), |c| {
                            c.allocation_config_id = Some(AllocationConfigId::new(ALLOCATION));
                            true
                        })
                        .await
                        .unwrap();
                })
            },
        );
        let report =
            h.es.revoke_credit_line(&waba(), Some(&BusinessId::new(BUSINESS)), &h.vault)
                .await
                .unwrap();
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_eq!(deletes(&h.t.requests()), [format!("/v25.0/{ALLOCATION}")]);
        assert_eq!(h.t.remaining(), 0);
    }

    /// Z08: after a key rotation, reading the ledger re-seals it; the
    /// version then in hand is the new one, so an opted-in re-share clears
    /// its own marker instead of taking its re-seal for a revocation.
    #[tokio::test]
    async fn a_re_share_after_a_key_rotation_is_not_a_revocation() {
        let kv = Arc::new(TestKv::default());
        let (h, _, _) = hooked_harness(CreditSharing::ShareAndAttach, kv.clone());
        onboarded(&h).await;
        revoked(&h).await;
        let rotated = TokenVault::new(
            kv,
            VaultKeys::new(VaultKey::new("k2", SecretBytes::new([7; 32])).unwrap())
                .with_previous(VaultKey::new("k1", SecretBytes::new([42; 32])).unwrap()),
        )
        .unwrap()
        .with_clock(Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC))));
        let before = h.t.requests().len();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        h.t.push_json(
            200,
            json!({"allocation_config_id": "NEW_ALLOCATION", "waba_id": WABA}),
        );
        h.t.push_json(200, success()); // register
        let done =
            h.es.resume(
                &waba(),
                &request()
                    .currency(WabaCurrency::Usd)
                    .reshare_after_revocation(),
                &rotated,
            )
            .await
            .unwrap();
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new("NEW_ALLOCATION"))
        );
        assert!(
            deletes(&h.t.requests()[before..]).is_empty(),
            "no self-revocation"
        );
        assert!(
            rotated
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_none(),
            "the opt-in cleared the marker"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 2, opted in: the re-share read the marker, a revocation rewrote
    /// it before the POST. The new allocation is revoked, and the marker
    /// stays.
    #[tokio::test]
    async fn a_revocation_racing_an_opted_in_re_share_wins() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        onboarded(&h).await;
        revoked(&h).await;
        let before = h.t.requests().len();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION)); // the check
        h.t.push_json(200, deleted());
        h.t.push_json(200, shared_record(ALLOCATION)); // the revocation, before the POST
        h.t.push_json(200, deleted());
        h.t.push_json(
            200,
            json!({"allocation_config_id": "NEW_ALLOCATION", "waba_id": WABA}),
        );
        h.t.push_json(
            200,
            json!({"data": [
                {"id": ALLOCATION, "receiving_business": {"id": BUSINESS}},
                {"id": "NEW_ALLOCATION", "receiving_business": {"id": BUSINESS}}
            ]}),
        );
        h.t.push_json(200, deleted()); // ALLOCATION
        h.t.push_json(200, active()); // NEW_ALLOCATION
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        hooked.before(
            Method::POST,
            "/whatsapp_credit_sharing_and_attach",
            move || {
                Box::pin(async move {
                    // Only the earlier, revoked record exists yet, and the
                    // re-share is in flight: not reported complete.
                    let err = other
                        .es
                        .revoke_credit_line(&waba(), None, &other.vault)
                        .await
                        .unwrap_err();
                    let incomplete = err.credit().and_then(CreditError::revocation).unwrap();
                    assert!(incomplete.share_pending, "{err}");
                    assert_eq!(
                        incomplete.report.already_revoked,
                        [AllocationConfigId::new(ALLOCATION)]
                    );
                })
            },
        );
        let err =
            h.es.resume(
                &waba(),
                &request()
                    .currency(WabaCurrency::Usd)
                    .reshare_after_revocation(),
                &h.vault,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err.credit(),
                Some(CreditError::Revoked { posted: true, .. })
            ),
            "{err}"
        );
        assert_eq!(
            deletes(&h.t.requests()[before..]),
            ["/v25.0/NEW_ALLOCATION"]
        );
        let marker = h
            .vault
            .revoked_business(&BusinessId::new(BUSINESS))
            .await
            .unwrap()
            .expect("the marker stays");
        assert!(
            marker
                .allocation_config_ids
                .contains(&AllocationConfigId::new("NEW_ALLOCATION"))
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 2: the marker is cleared by compare-and-swap on the version the
    /// re-share read. A revocation that rewrites it between the re-share's
    /// last read and its clear makes the clear fail, and the new allocation
    /// is revoked.
    #[tokio::test]
    async fn a_marker_rewritten_before_the_clear_is_a_revocation() {
        let kv = Arc::new(TestKv::default());
        let (h, _, _) = hooked_harness(CreditSharing::ShareAndAttach, kv.clone());
        onboarded(&h).await;
        revoked(&h).await;
        let before = h.t.requests().len();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        h.t.push_json(
            200,
            json!({"allocation_config_id": "NEW_ALLOCATION", "waba_id": WABA}),
        );
        h.t.push_json(200, shared_record("NEW_ALLOCATION"));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        kv.touch_marker_before_clear
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let err =
            h.es.resume(
                &waba(),
                &request()
                    .currency(WabaCurrency::Usd)
                    .reshare_after_revocation(),
                &h.vault,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err.credit(),
                Some(CreditError::Revoked { posted: true, .. })
            ),
            "{err}"
        );
        assert_eq!(
            deletes(&h.t.requests()[before..]),
            ["/v25.0/NEW_ALLOCATION"]
        );
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// A12: the marker is cleared only after the re-share succeeded.
    #[tokio::test]
    async fn a_failed_re_share_keeps_the_marker() {
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        revoked(&h).await;
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        h.t.push_json(
            400,
            json!({"error": {"message": "(#100) Invalid parameter", "type": "OAuthException", "code": 100}}),
        );
        h.es.resume(
            &waba(),
            &request()
                .currency(WabaCurrency::Usd)
                .reshare_after_revocation(),
            &h.vault,
        )
        .await
        .unwrap_err();
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(h.t.remaining(), 0);

        // A re-share whose answer is lost: its outcome is unknown, so the
        // marker stays too.
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        h.t.push_error(|| TransportError::Timeout);
        let err =
            h.es.resume(
                &waba(),
                &request()
                    .currency(WabaCurrency::Usd)
                    .reshare_after_revocation(),
                &h.vault,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_some(),
            "kept"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// An opted-in re-share whose answer was lost: the next opted-in
    /// `resume` is not refused for the pending share (the opt-in is the
    /// integrator's decision to fund the business again); it checks Meta's
    /// records first, finds the lost share funding the WABA, and posts
    /// nothing.
    #[tokio::test]
    async fn an_opted_in_resume_checks_a_pending_share_first() {
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        revoked(&h).await;
        let reshare = request()
            .currency(WabaCurrency::Usd)
            .reshare_after_revocation();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        h.t.push_error(|| TransportError::Timeout);
        let err = h.es.resume(&waba(), &reshare, &h.vault).await.unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        let before = h.t.requests().len();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(
            200,
            json!({"data": [
                {"id": ALLOCATION, "receiving_business": {"id": BUSINESS}},
                {"id": "NEW_ALLOCATION", "receiving_business": {"id": BUSINESS}}
            ]}),
        );
        h.t.push_json(200, deleted()); // ALLOCATION
        h.t.push_json(200, active()); // NEW_ALLOCATION: the lost share
        h.t.push_json(200, receiving_credential("NEW_ALLOCATION", CREDENTIAL));
        h.t.push_json(200, funding(CREDENTIAL));
        h.t.push_json(200, success()); // register
        let done = h.es.resume(&waba(), &reshare, &h.vault).await.unwrap();
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new("NEW_ALLOCATION"))
        );
        assert_eq!(
            posts_to(
                &h.t.requests()[before..],
                "/whatsapp_credit_sharing_and_attach"
            ),
            0
        );
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(credit.pending_share, None);
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_none(),
            "the opt-in cleared the marker"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// A revocation that settled the pending flag while this share's POST
    /// was in flight (it revoked another record of the business), then the
    /// POST's answer is lost and nothing new can be revoked: the flag is
    /// set again, so the next revocation does not report the line done.
    #[tokio::test]
    async fn a_lost_share_is_kept_pending_even_if_a_revocation_settled_it() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared()); // the share's check
        // The revocation, before the POST: another WABA's record of the
        // business, revoked now.
        h.t.push_json(200, shared_record("OTHER_WABA_RECORD"));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        h.t.push_error(|| TransportError::Timeout); // the POST
        // The share's own revocation: only that record, already revoked.
        h.t.push_json(200, shared_record("OTHER_WABA_RECORD"));
        h.t.push_json(200, deleted());
        hooked.before(
            Method::POST,
            "/whatsapp_credit_sharing_and_attach",
            move || {
                Box::pin(async move {
                    let report = other
                        .es
                        .revoke_credit_line(&waba(), None, &other.vault)
                        .await
                        .unwrap();
                    assert_eq!(
                        report.revoked,
                        [AllocationConfigId::new("OTHER_WABA_RECORD")]
                    );
                    assert_eq!(
                        other
                            .vault
                            .credit(&waba())
                            .await
                            .unwrap()
                            .unwrap()
                            .pending_share,
                        None,
                        "vacuous otherwise: the revocation settled the flag"
                    );
                })
            },
        );
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        assert!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .pending_share
                .is_some(),
            "pending again"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 3: in Solution Partner mode a line is attached only to a WABA
    /// the integrator approved; a token stored without an approval (Tech
    /// Provider mode, an older revision) needs `resume_with_approval` once.
    #[tokio::test]
    async fn solution_partner_mode_shares_only_what_the_integrator_approved() {
        let h = harness(CreditSharing::ShareAndAttach);
        let request = request().currency(WabaCurrency::Usd);
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
        assert!(
            matches!(&err, Error::Credit(CreditError::ApprovalRequired(m)) if m.contains("onboard_with_approval")),
            "{err}"
        );
        assert!(!err.may_have_been_sent() && !err.is_retryable());
        assert!(h.t.requests().is_empty(), "the code is not spent");

        // A token stored in Tech Provider mode.
        h.vault
            .store(
                &StoredBusinessToken::new(WABA, AccessToken::new(TOKEN))
                    .business_id(BUSINESS)
                    .phone_number_ids([PHONE]),
            )
            .await
            .unwrap();
        let err = h.es.resume(&waba(), &request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), APPROVE);
        assert!(
            matches!(err.credit(), Some(CreditError::ApprovalRequired(m)) if m.contains("resume_with_approval")),
            "{err}"
        );
        let err =
            h.es.resume_with_approval(&waba(), &request, &h.vault, |_| async {
                Err(ValidationError::new("waba_id", "not this tenant's").into())
            })
            .await
            .unwrap_err();
        assert_eq!(step(&err), APPROVE);
        assert!(h.t.requests().is_empty(), "nothing sent before an approval");
        assert!(h.vault.credit(&waba()).await.unwrap().is_none());

        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register
        let seen = std::sync::Mutex::new(None);
        let done =
            h.es.resume_with_approval(&waba(), &request, &h.vault, |verified| {
                *seen.lock().unwrap() = Some(verified);
                async { Ok(()) }
            })
            .await
            .unwrap();
        assert_eq!(
            done.steps_completed,
            [
                LOAD_TOKEN,
                VERIFY_ASSETS,
                APPROVE,
                SUBSCRIBE_APP,
                ASSIGN_SYSTEM_USER,
                SHARE_CREDIT_LINE,
                REGISTER_PHONE
            ]
        );
        let verified = seen.lock().unwrap().take().unwrap();
        assert_eq!(verified.business_id, Some(BusinessId::new(BUSINESS)));
        assert_eq!(verified.phone_number_id, Some(PhoneNumberId::new(PHONE)));
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(credit.approved_at, Some(datetime!(2026-09-24 12:00 UTC)));
        assert_eq!(h.t.remaining(), 0);

        // Recorded: a plain resume goes on.
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, receiving_credential(ALLOCATION, CREDENTIAL));
        h.t.push_json(200, funding(CREDENTIAL));
        h.t.push_json(200, success()); // register
        h.es.resume(&waba(), &request, &h.vault).await.unwrap();
        assert_eq!(h.t.remaining(), 0);
    }

    /// Item 5: the approval onboarding records is for the token record it
    /// then stores, however long the integrator's check takes.
    #[tokio::test]
    async fn the_approval_is_for_the_token_onboarding_stores() {
        let h = harness(CreditSharing::ShareAndAttach);
        let clock = Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC)));
        let vault = TokenVault::new(
            Arc::new(MemoryKvStore::new()),
            VaultKeys::new(VaultKey::new("k1", SecretBytes::new([42; 32])).unwrap()),
        )
        .unwrap()
        .with_clock(clock.clone());
        let request = request().currency(WabaCurrency::Usd);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register
        h.es.onboard_with_approval(&request, &vault, |_| {
            clock.advance(std::time::Duration::from_secs(90));
            async { Ok(()) }
        })
        .await
        .unwrap();
        let stored = vault.get(&waba()).await.unwrap().unwrap();
        let credit = vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(credit.approved_token_created_at, stored.created_at);
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, receiving_credential(ALLOCATION, CREDENTIAL));
        h.t.push_json(200, funding(CREDENTIAL));
        h.t.push_json(200, success()); // register
        h.es.resume(&waba(), &request, &vault).await.unwrap();
        assert_eq!(h.t.remaining(), 0);
    }

    /// Item 5 (audit M2): an approval is bound to the token record it was
    /// given for. A credit record without one (here the one a revocation
    /// writes the business into, for a token stored in Tech Provider mode)
    /// is no approval, and neither is one given for an earlier token record.
    #[tokio::test]
    async fn an_approval_holds_only_for_the_token_it_was_given_for() {
        let h = harness(CreditSharing::ShareAndAttach);
        let request = request().currency(WabaCurrency::Usd);
        h.vault
            .store(
                &StoredBusinessToken::new(WABA, AccessToken::new(TOKEN))
                    .business_id(BUSINESS)
                    .phone_number_ids([PHONE]),
            )
            .await
            .unwrap();
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        h.es.revoke_credit_line(&waba(), None, &h.vault)
            .await
            .unwrap();
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(credit.business_id, Some(BusinessId::new(BUSINESS)));
        assert_eq!(credit.approved_at, None, "vacuous otherwise");
        let before = h.t.requests().len();
        let err = h.es.resume(&waba(), &request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), APPROVE);
        assert!(
            matches!(err.credit(), Some(CreditError::ApprovalRequired(_))),
            "{err}"
        );
        assert_eq!(h.t.requests().len(), before, "nothing sent");

        // Approved through onboarding, then the token record replaced (a
        // Tech Provider onboarding after an offboard, `TokenVault::store`).
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        let mut again = StoredBusinessToken::new(WABA, AccessToken::new(TOKEN))
            .business_id(BUSINESS)
            .phone_number_ids([PHONE]);
        again.created_at = Some(datetime!(2026-09-25 09:00 UTC));
        h.vault.store(&again).await.unwrap();
        let before = h.t.requests().len();
        let err = h.es.resume(&waba(), &request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), APPROVE, "{err}");
        assert_eq!(h.t.requests().len(), before, "nothing sent");
        // Approved again, for this token record: resume goes on.
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, receiving_credential(ALLOCATION, CREDENTIAL));
        h.t.push_json(200, funding(CREDENTIAL));
        h.t.push_json(200, success()); // register
        h.es.resume_with_approval(&waba(), &request, &h.vault, |_| async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .approved_token_created_at,
            Some(datetime!(2026-09-25 09:00 UTC))
        );
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, receiving_credential(ALLOCATION, CREDENTIAL));
        h.t.push_json(200, funding(CREDENTIAL));
        h.t.push_json(200, success()); // register
        h.es.resume(&waba(), &request, &h.vault).await.unwrap();
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 4: an unreadable marker is replaced and the revocation goes on;
    /// `offboard` then deletes the token.
    #[tokio::test]
    async fn an_unreadable_marker_does_not_stop_a_revocation() {
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        h.vault
            .kv()
            .put(
                &wa_core::store::StoreKey::new(
                    super::super::vault::TOKEN_NAMESPACE,
                    format!("revoked/{BUSINESS}"),
                ),
                b"{}".to_vec(),
                wa_core::store::Expiry::Never,
            )
            .await
            .unwrap();
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let off = h.es.offboard(&waba(), None, &h.vault).await.unwrap();
        assert_eq!(
            off.credit.unwrap().revoked,
            [AllocationConfigId::new(ALLOCATION)]
        );
        assert!(off.token_deleted);
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_some(),
            "replaced by a readable marker"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 4: a marker that cannot be written does not stop the DELETEs;
    /// its error comes back afterwards, with the report.
    #[tokio::test]
    async fn a_marker_write_failure_is_returned_after_revoking() {
        let kv = Arc::new(TestKv::default());
        let (h, _, _) = hooked_harness(CreditSharing::ShareAndAttach, kv.clone());
        onboarded(&h).await;
        kv.refuse_marker_writes
            .store(true, std::sync::atomic::Ordering::SeqCst);
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let err =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap_err();
        let incomplete = err.credit().and_then(CreditError::revocation).unwrap();
        assert_eq!(
            incomplete.report.revoked,
            [AllocationConfigId::new(ALLOCATION)]
        );
        assert!(incomplete.source.is_none(), "Meta's side is done: {err}");
        assert!(
            matches!(incomplete.ledger.as_deref(), Some(Error::Storage(_))),
            "{err}"
        );
        assert!(err.is_retryable(), "calling again writes the marker again");
        assert_eq!(err.kind(), ErrorKind::ServiceUnavailable);
        assert!(err.may_have_been_sent(), "the DELETE went out");
        assert_eq!(deletes(&h.t.requests()), [format!("/v25.0/{ALLOCATION}")]);
        assert_eq!(h.t.remaining(), 0);

        // A DELETE whose answer was lost, on a record Meta then reports
        // DELETED: found revoked, and this call may have done it.
        h.t.push_json(200, shared_record("A2"));
        h.t.push_json(200, active());
        h.t.push_error(|| TransportError::Timeout);
        h.t.push_json(200, deleted());
        h.t.push_json(200, deleted()); // the recorded allocation, revoked above
        let err =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap_err();
        let incomplete = err.credit().and_then(CreditError::revocation).unwrap();
        assert!(
            incomplete
                .report
                .already_revoked
                .contains(&AllocationConfigId::new("A2")),
            "{err}"
        );
        assert!(incomplete.deletes_sent, "{err}");
        assert!(err.may_have_been_sent());
        assert_eq!(h.t.remaining(), 0);

        // Meta's side fails too: both failures are kept.
        let failure = json!({"error": {"message": "unknown", "type": "OAuthException", "code": 1}});
        h.t.push_json(500, failure.clone()); // the lookup
        h.t.push_json(500, failure.clone()); // the recorded allocation's status
        h.t.push_json(500, failure.clone()); // its DELETE
        h.t.push_json(500, failure); // its status again
        let err =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap_err();
        let incomplete = err.credit().and_then(CreditError::revocation).unwrap();
        assert!(incomplete.source.is_some(), "{err}");
        assert!(
            matches!(incomplete.ledger.as_deref(), Some(Error::Storage(_))),
            "{err}"
        );
        assert!(err.is_retryable(), "{err}");
        assert_eq!(h.t.remaining(), 0);
    }

    /// Item 3: a caller's business whose check failed is not marked before
    /// the lookup, but is marked (and recorded for the WABA) once the
    /// revocation's own lookup attributes records to it.
    #[tokio::test]
    async fn a_business_whose_check_failed_is_marked_once_its_records_are_found() {
        let h = harness(CreditSharing::ShareAndAttach);
        let failure = json!({"error": {"message": "unknown", "type": "OAuthException", "code": 1}});
        h.t.push_json(500, failure); // the check of the caller's business
        h.t.push_json(200, shared_record(ALLOCATION)); // the lookup
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let report =
            h.es.revoke_credit_line(&waba(), Some(&BusinessId::new(BUSINESS)), &h.vault)
                .await
                .unwrap();
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_eq!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .expect("marked after the lookup")
                .allocation_config_ids,
            [AllocationConfigId::new(ALLOCATION)]
        );
        assert_eq!(
            h.vault.credit(&waba()).await.unwrap().unwrap().business_id,
            Some(BusinessId::new(BUSINESS)),
            "recorded, so rotate reaches the marker"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// N3: an unreadable credit record does not stop a revocation the
    /// token's business can address.
    #[tokio::test]
    async fn an_unreadable_credit_record_does_not_stop_a_revocation() {
        let h = harness(CreditSharing::ShareAndAttach);
        onboarded(&h).await;
        h.vault
            .kv()
            .put(
                &wa_core::store::StoreKey::new(
                    super::super::vault::TOKEN_NAMESPACE,
                    format!("credit/{WABA}"),
                ),
                b"{}".to_vec(),
                wa_core::store::Expiry::Never,
            )
            .await
            .unwrap();
        assert!(h.vault.credit(&waba()).await.is_err(), "vacuous otherwise");
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let report =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap();
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_eq!(h.t.remaining(), 0);

        // Offboarding cannot tell what the ledger recorded: it revokes, and
        // keeps the token when nothing was found.
        h.t.push_json(200, nothing_shared());
        let err = h.es.offboard(&waba(), None, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), REVOKE_CREDIT_LINE);
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        assert!(h.vault.get(&waba()).await.unwrap().is_some());
        assert_eq!(h.t.remaining(), 0);
    }

    /// A2/A3 and fix 1c: with an allocation recorded and no business, the
    /// business Meta's record of that allocation names is marked, written
    /// into the ledger, and revoked; when Meta names none, the allocation
    /// alone is revoked.
    #[tokio::test]
    async fn revocation_without_a_stored_owner_uses_the_stored_allocation() {
        let h = harness(CreditSharing::ShareAndAttach);
        let mut credit = StoredCredit::new(waba());
        credit.allocation_config_id = Some(AllocationConfigId::new(ALLOCATION));
        h.vault.put_credit(&credit, None).await.unwrap();
        h.t.push_json(200, active()); // Meta's record names BUSINESS
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let report =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap();
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_eq!(report.business_id, Some(BusinessId::new(BUSINESS)));
        let reqs = h.t.requests();
        assert_status(&reqs[0], ALLOCATION);
        assert_lookup(&reqs[1]);
        assert_eq!(deletes(&reqs), [format!("/v25.0/{ALLOCATION}")]);
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            h.vault.credit(&waba()).await.unwrap().unwrap().business_id,
            Some(BusinessId::new(BUSINESS))
        );
        assert_eq!(h.t.remaining(), 0);

        // Meta's record names no business: the allocation is revoked, and
        // nothing is marked.
        let h = harness(CreditSharing::ShareAndAttach);
        h.vault.put_credit(&credit, None).await.unwrap();
        h.t.push_json(200, json!({}));
        h.t.push_json(200, json!({}));
        h.t.push_json(200, success());
        h.t.push_json(200, json!({"request_status": "DELETED"}));
        let report =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap();
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_eq!(report.business_id, None);
        assert_eq!(h.t.remaining(), 0);

        // A hint that contradicts Meta's record revokes nothing.
        let h = harness(CreditSharing::ShareAndAttach);
        h.vault.put_credit(&credit, None).await.unwrap();
        h.t.push_json(200, active());
        let err =
            h.es.revoke_credit_line(&waba(), Some(&BusinessId::new("OTHER")), &h.vault)
                .await
                .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "owner_business_id"),
            "{err}"
        );
        assert_eq!(h.t.requests().len(), 1);

        // Nothing recorded at all, nothing given: refused, nothing sent.
        let h = harness(CreditSharing::ShareAndAttach);
        h.vault
            .store(&StoredBusinessToken::new("BARE", AccessToken::new(TOKEN)))
            .await
            .unwrap();
        assert!(matches!(
            h.es.revoke_credit_line(&WabaId::new("BARE"), None, &h.vault).await,
            Err(Error::Validation(v)) if v.field == "business_id"
        ));
        assert!(h.t.requests().is_empty());
    }

    /// Fix 9, on the only paths that reach the marker without the line's
    /// own records: a caller-supplied business with an allocation recorded
    /// (Meta's record of it names no business), and one whose lookup
    /// fails. Neither marks the business.
    #[tokio::test]
    async fn a_hint_the_line_has_no_record_of_is_never_marked() {
        let mut credit = StoredCredit::new(waba());
        credit.allocation_config_id = Some(AllocationConfigId::new(ALLOCATION));

        // Meta's record names no business, and the caller's hint has no
        // record of the line either: the allocation is revoked, the hint
        // is not marked.
        let h = harness(CreditSharing::ShareAndAttach);
        h.vault.put_credit(&credit, None).await.unwrap();
        h.t.push_json(200, json!({})); // the allocation names no business
        h.t.push_json(200, nothing_shared()); // the hint's records: none
        h.t.push_json(200, nothing_shared()); // the lookup
        h.t.push_json(200, json!({}));
        h.t.push_json(200, success());
        h.t.push_json(200, json!({"request_status": "DELETED"}));
        let report =
            h.es.revoke_credit_line(&waba(), Some(&BusinessId::new(BUSINESS)), &h.vault)
                .await
                .unwrap();
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_none(),
            "a hint the line has no record of is never marked"
        );
        assert_eq!(h.t.remaining(), 0);

        // The hint's lookup fails: it cannot be checked, so it is not
        // marked either, and the failure comes back.
        let h = harness(CreditSharing::ShareAndAttach);
        let failure = json!({"error": {"message": "unknown", "type": "OAuthException", "code": 1}});
        h.t.push_json(500, failure.clone());
        h.t.push_json(500, failure);
        let err =
            h.es.revoke_credit_line(&waba(), Some(&BusinessId::new(BUSINESS)), &h.vault)
                .await
                .unwrap_err();
        assert!(
            err.credit().and_then(CreditError::revocation).is_some(),
            "{err}"
        );
        assert!(err.is_retryable(), "{err}");
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 6: a record whose `request_status` Meta does not document is not
    /// taken as active: sharing next to it needs the opt-in, which then
    /// checks it like an active one.
    #[tokio::test]
    async fn an_undocumented_request_status_is_not_active() {
        let h = harness(CreditSharing::ShareAndAttach);
        let pending =
            json!({"receiving_business": {"id": BUSINESS}, "request_status": "PENDING_REVIEW"});
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, pending.clone());
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::StatusUnknown { status, .. }) if status == "PENDING_REVIEW"),
            "{err}"
        );
        assert_eq!(
            posts_to(&h.t.requests(), "/whatsapp_credit_sharing_and_attach"),
            0
        );
        assert_eq!(h.t.remaining(), 0);

        let before = h.t.requests().len();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, pending);
        h.t.push_json(200, receiving_credential(ALLOCATION, CREDENTIAL));
        h.t.push_json(200, funding("SOMETHING_ELSE"));
        h.t.push_json(
            200,
            json!({"allocation_config_id": "NEW_ALLOCATION", "waba_id": WABA}),
        );
        h.t.push_json(200, success()); // register
        let done =
            h.es.resume(
                &waba(),
                &request()
                    .currency(WabaCurrency::Usd)
                    .reshare_after_revocation(),
                &h.vault,
            )
            .await
            .unwrap();
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new("NEW_ALLOCATION"))
        );
        assert_eq!(
            posts_to(
                &h.t.requests()[before..],
                "/whatsapp_credit_sharing_and_attach"
            ),
            1
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 7: the lease is renewed right before the POST; one lost to a
    /// slow step posts nothing and is not released under its new holder.
    #[tokio::test]
    async fn a_lease_lost_during_the_check_posts_nothing() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        hooked.before(
            Method::GET,
            "/owning_credit_allocation_configs",
            move || {
                Box::pin(async move {
                    // The lease expired; another onboarding took it.
                    other.vault.kv().delete(&lease_key()).await.unwrap();
                    other.vault.lease_credit(&waba()).await.unwrap();
                })
            },
        );
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(EmbeddedSignup::is_credit_step_busy(&err), "{err}");
        assert!(err.is_retryable(), "resume later");
        assert!(!err.may_have_been_sent(), "nothing posted");
        assert_eq!(
            posts_to(&h.t.requests(), "/whatsapp_credit_sharing_and_attach"),
            0
        );
        assert!(
            h.vault.lease_credit(&waba()).await.is_err(),
            "the new holder keeps it"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 7: a record changed while the POST was in flight is merged,
    /// never reported busy after a share went out.
    #[tokio::test]
    async fn a_record_changed_during_the_post_is_merged() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register
        hooked.before(
            Method::POST,
            "/whatsapp_credit_sharing_and_attach",
            move || {
                Box::pin(async move {
                    other.vault.record_approval(&waba(), None).await.unwrap();
                })
            },
        );
        let done = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap();
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(
            credit.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        assert_eq!(credit.pending_share, None, "the outcome is recorded");
        assert!(credit.approved_at.is_some());
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 8: a share posted with no recorded outcome, which the lookup
    /// does not show, is not posted again while the WABA has a primary
    /// funding; offboarding keeps the token until it is reconciled.
    #[tokio::test]
    async fn a_share_whose_outcome_was_lost_is_reconciled_not_posted_again() {
        let h = harness(CreditSharing::ShareAndAttach);
        let request = request().currency(WabaCurrency::Usd);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_error(|| TransportError::Timeout);
        let err = h.onboard(&request).await.unwrap_err();
        // A lost answer is never an invitation to retry at once.
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        assert!(err.may_have_been_sent() && !err.is_retryable(), "{err}");
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(credit.pending_share, Some(datetime!(2026-09-24 12:00 UTC)));
        assert_eq!(credit.allocation_config_id, None);

        let before = h.t.requests().len();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared()); // Meta's lookup misses it
        h.t.push_json(200, funding(CREDENTIAL)); // yet something funds the WABA
        let err = h.es.resume(&waba(), &request, &h.vault).await.unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        assert!(err.may_have_been_sent());
        assert_eq!(
            posts_to(
                &h.t.requests()[before..],
                "/whatsapp_credit_sharing_and_attach"
            ),
            0
        );
        assert_eq!(h.t.remaining(), 0);

        // Nothing funds the WABA: taken as "the share did not go through",
        // and posted again. That relies on the lookup and the WABA's
        // `primary_funding_id` showing a share as soon as Meta applied it,
        // which Meta does not document (the guide says so).
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, json!({"id": WABA}));
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register
        h.es.resume(&waba(), &request, &h.vault).await.unwrap();
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(credit.pending_share, None);
        assert_eq!(
            credit.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 8, (c) and item 6: offboarding a WABA whose share has no recorded
    /// outcome. The revocation (which marks the business) revokes nothing,
    /// so the pending share is not settled: incomplete (call again), and
    /// the token is kept.
    #[tokio::test]
    async fn offboarding_a_share_whose_outcome_was_lost_needs_it_revoked() {
        let request = request().currency(WabaCurrency::Usd);
        let h = harness(CreditSharing::ShareAndAttach);
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_error(|| TransportError::Timeout);
        h.onboard(&request).await.unwrap_err();
        h.t.push_json(200, nothing_shared());
        let err = h.es.offboard(&waba(), None, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), REVOKE_CREDIT_LINE);
        let incomplete = err.credit().and_then(CreditError::revocation).unwrap();
        assert!(incomplete.share_pending, "{err}");
        assert!(err.is_retryable() && !err.may_have_been_sent(), "{err}");
        assert!(h.vault.get(&waba()).await.unwrap().is_some(), "token kept");
        assert_eq!(h.t.remaining(), 0);

        // Only an old DELETED record is found: that says nothing about the
        // pending share either (item 6), so the token is still kept.
        h.t.push_json(200, shared_record("OLD"));
        h.t.push_json(200, deleted());
        let err = h.es.offboard(&waba(), None, &h.vault).await.unwrap_err();
        let incomplete = err.credit().and_then(CreditError::revocation).unwrap();
        assert!(incomplete.share_pending, "{err}");
        assert_eq!(
            incomplete.report.already_revoked,
            [AllocationConfigId::new("OLD")]
        );
        assert!(h.vault.get(&waba()).await.unwrap().is_some(), "token kept");
        assert_eq!(h.t.remaining(), 0);

        // Meta lists the share now: it is revoked, which settles the flag,
        // and offboarding goes on; a repeat still succeeds.
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let off = h.es.offboard(&waba(), None, &h.vault).await.unwrap();
        assert_eq!(
            off.credit.unwrap().revoked,
            [AllocationConfigId::new(ALLOCATION)]
        );
        assert!(off.token_deleted);
        assert_eq!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .pending_share,
            None,
            "settled by the revocation"
        );
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, deleted());
        let again = h.es.offboard(&waba(), None, &h.vault).await.unwrap();
        assert_eq!(
            again.credit.unwrap().already_revoked,
            [AllocationConfigId::new(ALLOCATION)]
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Fix 10: nothing names the WABA's business and the ledger shows
    /// nothing shared: nothing to revoke, the token is deleted. A ledger
    /// that shows a share keeps the token.
    #[tokio::test]
    async fn offboarding_what_was_never_shared_deletes_the_token() {
        let h = harness(CreditSharing::ShareAndAttach);
        h.vault
            .store(&StoredBusinessToken::new(WABA, AccessToken::new(TOKEN)))
            .await
            .unwrap();
        let off = h.es.offboard(&waba(), None, &h.vault).await.unwrap();
        assert_eq!(off.credit, None);
        assert!(off.token_deleted);
        assert!(h.t.requests().is_empty());

        // An approval alone is not a share.
        h.vault
            .store(&StoredBusinessToken::new(WABA, AccessToken::new(TOKEN)))
            .await
            .unwrap();
        h.vault.record_approval(&waba(), None).await.unwrap();
        assert!(
            h.es.offboard(&waba(), None, &h.vault)
                .await
                .unwrap()
                .token_deleted
        );

        // A share posted whose outcome is unknown, no business to revoke by.
        h.vault
            .store(&StoredBusinessToken::new(WABA, AccessToken::new(TOKEN)))
            .await
            .unwrap();
        let mut credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        credit.pending_share = Some(datetime!(2026-09-24 12:00 UTC));
        let (_, version) = h.vault.credit_versioned(&waba()).await.unwrap().unwrap();
        h.vault.put_credit(&credit, Some(version)).await.unwrap();
        let err = h.es.offboard(&waba(), None, &h.vault).await.unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        assert!(h.vault.get(&waba()).await.unwrap().is_some());
        assert!(h.t.requests().is_empty());
    }

    /// Item 13: `PARTNER_REMOVED` naming the owner business and no WABA
    /// revokes by business, marking it only if the line has records for it.
    #[tokio::test]
    async fn revocation_by_business_alone() {
        let h = harness(CreditSharing::ShareAndAttach);
        h.t.push_json(200, shared_record(ALLOCATION)); // the business has records
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let report =
            h.es.revoke_business_credit_line(&BusinessId::new(BUSINESS), &h.vault)
                .await
                .unwrap();
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(h.t.remaining(), 0);

        let h = harness(CreditSharing::ShareAndAttach);
        h.t.push_json(200, nothing_shared());
        let report =
            h.es.revoke_business_credit_line(&BusinessId::new(BUSINESS), &h.vault)
                .await
                .unwrap();
        assert_eq!(report.all().count(), 0);
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            h.es.revoke_business_credit_line(&BusinessId::new(" "), &h.vault)
                .await
                .is_err()
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// The owner's decision (2026-09-25, closing the open question on
    /// marking an unfunded business): offboarding marks a recorded business
    /// revoked even when nothing was ever shared with it (the marker comes
    /// before any lookup, so no racing share survives), and onboarding that
    /// business in Solution Partner mode later needs the opt-in.
    #[tokio::test]
    async fn offboarding_marks_a_business_never_funded_and_a_re_onboarding_opts_in() {
        let h = harness(CreditSharing::ShareAndAttach);
        // Onboarded in Tech Provider mode: a token naming its business, no
        // share in the ledger.
        h.vault
            .store(&StoredBusinessToken::new(WABA, AccessToken::new(TOKEN)).business_id(BUSINESS))
            .await
            .unwrap();
        h.t.push_json(200, nothing_shared());
        let off = h.es.offboard(&waba(), None, &h.vault).await.unwrap();
        assert!(off.token_deleted);
        assert_eq!(off.credit.map(|r| r.all().count()), Some(0));
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_some(),
            "marked, though nothing was shared"
        );
        assert_eq!(h.t.remaining(), 0);

        let before = h.t.requests().len();
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert!(EmbeddedSignup::is_credit_line_revoked(&err), "{err}");
        assert!(!err.may_have_been_sent());
        assert_eq!(credit_calls(&h.t.requests()[before..]), 0);
        assert_eq!(h.t.remaining(), 0);
    }

    /// Onboard with share-and-attach until the share's POST times out: the
    /// ledger keeps a pending share.
    async fn lost_share(h: &Harness) {
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_error(|| TransportError::Timeout);
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(credit.pending_share, Some(datetime!(2026-09-24 12:00 UTC)));
        assert_eq!(h.t.remaining(), 0);
    }

    /// `GET /{WABA}?fields=primary_funding_id`, with the merchant's token.
    fn assert_funding_read(r: &RecordedRequest) {
        assert_request(
            r,
            &Method::GET,
            &format!("/v25.0/{WABA}"),
            &[("fields", "primary_funding_id")],
            TOKEN,
        );
    }

    fn credit_store_key() -> wa_core::store::StoreKey {
        wa_core::store::StoreKey::new(
            super::super::vault::TOKEN_NAMESPACE,
            format!("credit/{WABA}"),
        )
    }

    fn no_funding() -> serde_json::Value {
        json!({"id": WABA})
    }

    /// The owner's decision (2026-09-25, closing the open question on a
    /// share whose answer was lost): a lost share Meta never shows keeps
    /// every revocation incomplete until an operator clears it. The
    /// clearance checks Meta first (GETs only), clears, and seals who did
    /// it and when in the ledger; revocation and offboarding then finish as
    /// if no share had been posted.
    #[tokio::test]
    async fn an_operator_clears_a_lost_share_meta_does_not_show() {
        let h = harness(CreditSharing::ShareAndAttach);
        lost_share(&h).await;
        // Stuck: the revocation revokes nothing, and says so.
        h.t.push_json(200, nothing_shared());
        let err =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap_err();
        let incomplete = err.credit().and_then(CreditError::revocation).unwrap();
        assert!(incomplete.share_pending, "{err}");

        h.clock.advance(std::time::Duration::from_secs(3600));
        let before = h.t.requests().len();
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, no_funding());
        let out =
            h.es.clear_pending_share(&waba(), "  ops@wind-and-wool.example ", None, &h.vault)
                .await
                .unwrap();
        let entry = ClearedShare {
            pending_since: datetime!(2026-09-24 12:00 UTC),
            cleared_at: datetime!(2026-09-24 13:00 UTC),
            cleared_by: "ops@wind-and-wool.example".into(),
            primary_funding_id: None,
        };
        assert_eq!(out, PendingShareClearance::Cleared(entry.clone()));
        let checked = &h.t.requests()[before..];
        assert_eq!(checked.len(), 2, "{checked:?}");
        assert_lookup(&checked[0]);
        assert_funding_read(&checked[1]);
        assert_eq!(h.t.remaining(), 0);

        // The audit entry: sealed with the record, and read back.
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(credit.pending_share, None);
        assert_eq!(credit.cleared_shares, std::slice::from_ref(&entry));
        assert!(!credit.records_a_share(), "a cleared share is not a share");
        for shown in [format!("{out:?}"), format!("{credit:?}")] {
            assert!(!shown.contains("wind-and-wool"), "{shown}");
            assert!(shown.contains("cleared_by: \"<redacted>\""), "{shown}");
        }
        let sealed = h
            .vault
            .kv()
            .get(&credit_store_key())
            .await
            .unwrap()
            .unwrap()
            .value;
        assert!(
            !String::from_utf8_lossy(&sealed).contains("wind-and-wool"),
            "the operator is sealed, not in the clear"
        );
        let copy: Arc<dyn wa_core::store::KvStore> = Arc::new(MemoryKvStore::new());
        copy.put(&credit_store_key(), sealed, wa_core::store::Expiry::Never)
            .await
            .unwrap();
        let reader = TokenVault::new(
            Arc::clone(&copy),
            VaultKeys::new(VaultKey::new("k1", SecretBytes::new([42; 32])).unwrap()),
        )
        .unwrap();
        assert_eq!(
            reader
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .cleared_shares,
            std::slice::from_ref(&entry),
            "readable back with the vault key"
        );
        let stranger = TokenVault::new(
            copy,
            VaultKeys::new(VaultKey::new("k1", SecretBytes::new([7; 32])).unwrap()),
        )
        .unwrap();
        assert!(stranger.credit(&waba()).await.is_err(), "and only with it");

        // As if no share had been posted: the revocation is done, and
        // offboarding deletes the token.
        h.t.push_json(200, nothing_shared());
        let report =
            h.es.revoke_credit_line(&waba(), None, &h.vault)
                .await
                .unwrap();
        assert_eq!(report.all().count(), 0);
        h.t.push_json(200, nothing_shared());
        let off = h.es.offboard(&waba(), None, &h.vault).await.unwrap();
        assert!(off.token_deleted);
        assert_eq!(off.credit.map(|r| r.all().count()), Some(0));
        assert!(h.t.requests().iter().all(|r| r.method != Method::DELETE));
        assert_eq!(h.t.remaining(), 0);
        assert_eq!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .cleared_shares,
            [entry],
            "the trail outlives the token"
        );
    }

    /// After a clearance, `resume` shares again without the pending share's
    /// check of the WABA's funding: as if nothing had been posted. (The
    /// merchant's own card funds the WABA: the operator acknowledged it.)
    #[tokio::test]
    async fn after_a_clearance_resume_shares_as_if_nothing_had_been_posted() {
        let h = harness(CreditSharing::ShareAndAttach);
        lost_share(&h).await;
        let card = FundingId::new("THE_MERCHANTS_OWN_CARD");
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, funding("THE_MERCHANTS_OWN_CARD"));
        let PendingShareClearance::Cleared(entry) =
            h.es.clear_pending_share(&waba(), "ops", Some(&card), &h.vault)
                .await
                .unwrap()
        else {
            panic!("not cleared")
        };
        assert_eq!(
            entry.primary_funding_id,
            Some(card),
            "what Meta showed, acknowledged, is kept"
        );
        let before = h.t.requests().len();
        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, shared_and_attached());
        h.t.push_json(200, success()); // register
        let done =
            h.es.resume(&waba(), &request().currency(WabaCurrency::Usd), &h.vault)
                .await
                .unwrap();
        assert_eq!(
            done.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION))
        );
        let resumed = &h.t.requests()[before..];
        assert_lookup(&resumed[2]);
        assert_eq!(
            resumed[3].path(),
            format!("/v25.0/{LINE}/whatsapp_credit_sharing_and_attach")
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Meta shows the lost share funding the WABA: nothing is cleared, the
    /// record is recorded as the WABA's allocation (as `resume` would), and
    /// the revocation that revokes it settles the pending share.
    #[tokio::test]
    async fn a_lost_share_meta_shows_funding_the_waba_is_recorded_not_cleared() {
        let h = harness(CreditSharing::ShareAndAttach);
        lost_share(&h).await;
        let before = h.t.requests().len();
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, funding(CREDENTIAL));
        h.t.push_json(200, receiving_credential(ALLOCATION, CREDENTIAL));
        let out =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap();
        assert_eq!(
            out,
            PendingShareClearance::NotCleared(SharesFound {
                business_id: BusinessId::new(BUSINESS),
                funding: Some(AllocationConfigId::new(ALLOCATION)),
                active: vec![AllocationConfigId::new(ALLOCATION)],
                unknown_status: Vec::new(),
                unattributed: Vec::new(),
                primary_funding_id: Some(FundingId::new(CREDENTIAL)),
            })
        );
        let checked = &h.t.requests()[before..];
        assert_eq!(checked.len(), 4, "{checked:?}");
        assert_lookup(&checked[0]);
        assert_status(&checked[1], ALLOCATION);
        assert_funding_read(&checked[2]);
        assert_request(
            &checked[3],
            &Method::GET,
            &format!("/v25.0/{ALLOCATION}"),
            &[("fields", "receiving_credential")],
            SYSTEM_TOKEN,
        );
        assert_eq!(h.t.remaining(), 0);
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert!(credit.pending_share.is_some(), "not cleared");
        assert!(credit.cleared_shares.is_empty(), "no audit entry");
        assert_eq!(
            credit.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION)),
            "recorded"
        );
        assert!(credit.shared_at.is_some());

        let report = revoked(&h).await;
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_eq!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .pending_share,
            None,
            "settled by the revocation that revoked it"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Any record that may be live stops a clearance, whether or not it
    /// funds this WABA (Meta does not say which WABA it funds), and so does
    /// one whose status Meta does not document. Once none is live, a
    /// funding no record of the line explains does not, once acknowledged.
    #[tokio::test]
    async fn a_record_that_may_be_live_is_never_cleared() {
        let h = harness(CreditSharing::ShareAndAttach);
        lost_share(&h).await;
        let (_, version) = h.vault.credit_versioned(&waba()).await.unwrap().unwrap();
        h.t.push_json(200, shared_record("OTHER_WABA_RECORD"));
        h.t.push_json(200, active());
        h.t.push_json(200, no_funding());
        h.t.push_json(
            200,
            receiving_credential("OTHER_WABA_RECORD", "CRED_OF_ANOTHER_WABA"),
        );
        let out =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap();
        assert_eq!(
            out,
            PendingShareClearance::NotCleared(SharesFound {
                business_id: BusinessId::new(BUSINESS),
                funding: None,
                active: vec![AllocationConfigId::new("OTHER_WABA_RECORD")],
                unknown_status: Vec::new(),
                unattributed: Vec::new(),
                primary_funding_id: None,
            })
        );
        assert_eq!(
            h.vault.credit_versioned(&waba()).await.unwrap().unwrap().1,
            version,
            "the record is untouched"
        );

        h.t.push_json(200, shared_record("ODD"));
        h.t.push_json(
            200,
            json!({"receiving_business": {"id": BUSINESS}, "request_status": "PENDING_REVIEW"}),
        );
        h.t.push_json(200, funding("CRED_ELSEWHERE"));
        h.t.push_json(200, receiving_credential("ODD", "CRED_OF_ODD"));
        // Acknowledging the funding does not override a record that may be
        // live.
        let elsewhere = FundingId::new("CRED_ELSEWHERE");
        let out =
            h.es.clear_pending_share(&waba(), "ops", Some(&elsewhere), &h.vault)
                .await
                .unwrap();
        let PendingShareClearance::NotCleared(found) = &out else {
            panic!("cleared: {out:?}")
        };
        assert_eq!(
            found,
            &SharesFound {
                business_id: BusinessId::new(BUSINESS),
                funding: None,
                active: Vec::new(),
                unknown_status: vec![(AllocationConfigId::new("ODD"), "PENDING_REVIEW".to_owned())],
                unattributed: Vec::new(),
                primary_funding_id: Some(elsewhere.clone()),
            }
        );
        assert_eq!(
            found.unexplained_funding(),
            None,
            "a record explains nothing, but may be live"
        );
        assert_eq!(
            h.vault.credit_versioned(&waba()).await.unwrap().unwrap().1,
            version,
            "the record is untouched"
        );

        // Revoked since: nothing live. The WABA's funding is someone else's
        // (the operator checked, and acknowledges it); it is kept in the
        // audit entry.
        h.t.push_json(200, shared_record("OTHER_WABA_RECORD"));
        h.t.push_json(200, deleted());
        h.t.push_json(200, funding("CRED_ELSEWHERE"));
        let out =
            h.es.clear_pending_share(&waba(), "ops", Some(&elsewhere), &h.vault)
                .await
                .unwrap();
        let PendingShareClearance::Cleared(entry) = out else {
            panic!("not cleared: {out:?}")
        };
        assert_eq!(
            entry.primary_funding_id,
            Some(FundingId::new("CRED_ELSEWHERE"))
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Refused before anything is sent or written: not a Solution Partner,
    /// no operator named, nothing pending, no token to read the WABA's
    /// funding with. A failed check clears nothing.
    #[tokio::test]
    async fn a_clearance_is_refused_before_anything_is_sent() {
        let field = |err: &Error| match err {
            Error::Validation(v) => v.field.clone(),
            other => panic!("not a validation error: {other}"),
        };
        let tp = harness_with(None);
        let err = tp
            .es
            .clear_pending_share(&waba(), "ops", None, &tp.vault)
            .await
            .unwrap_err();
        assert_eq!(field(&err), "solution_partner");

        let h = harness(CreditSharing::ShareAndAttach);
        let err =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap_err();
        assert_eq!(field(&err), "pending_share", "no credit record");
        onboarded(&h).await;
        let sent = h.t.requests().len();
        let err =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap_err();
        assert_eq!(field(&err), "pending_share", "shared and recorded");
        assert!(!err.is_retryable() && !err.may_have_been_sent());
        assert_eq!(h.t.requests().len(), sent);

        let h = harness(CreditSharing::ShareAndAttach);
        lost_share(&h).await;
        let sent = h.t.requests().len();
        for blank in ["", "  ", "\u{200B}", "ops\u{202E}", "ops\nothers"] {
            let err =
                h.es.clear_pending_share(&waba(), blank, None, &h.vault)
                    .await
                    .unwrap_err();
            assert_eq!(field(&err), "cleared_by");
        }
        // A failed lookup clears nothing.
        h.t.push_json(
            500,
            json!({"error": {"message": "An unexpected error has occurred", "type": "OAuthException", "code": 2, "fbtrace_id": "A"}}),
        );
        let err =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap_err();
        assert!(err.graph().is_some(), "{err}");
        // No token: the WABA's funding cannot be read.
        h.vault.delete(&waba()).await.unwrap();
        let err =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap_err();
        assert_eq!(field(&err), "waba_id");
        assert_eq!(h.t.requests().len(), sent + 1, "the lookup only");
        assert_eq!(h.t.remaining(), 0);
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert!(credit.pending_share.is_some() && credit.cleared_shares.is_empty());
        // Every refusal released the lease.
        let lease = h.vault.lease_credit(&waba()).await.unwrap();
        h.vault.release_credit(&waba(), lease).await.unwrap();
    }

    /// The clearance holds the WABA's credit lease: while a share holds it
    /// the clearance is refused, and a share that starts during the check
    /// is refused, posting nothing.
    #[tokio::test]
    async fn a_clearance_holds_the_lease_so_no_share_races_it() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        lost_share(&h).await;
        let held = h.vault.lease_credit(&waba()).await.unwrap();
        let err =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap_err();
        assert!(EmbeddedSignup::is_credit_step_busy(&err), "{err}");
        assert!(err.is_retryable() && !err.may_have_been_sent());
        h.vault.release_credit(&waba(), held).await.unwrap();

        let before = h.t.requests().len();
        h.t.push_json(200, success()); // the racing resume's subscribe
        h.t.push_json(200, success()); // its assigned_users
        h.t.push_json(200, nothing_shared()); // the clearance's lookup
        h.t.push_json(200, no_funding());
        hooked.before(
            Method::GET,
            "/owning_credit_allocation_configs",
            move || {
                Box::pin(async move {
                    let err = other
                        .es
                        .resume(
                            &waba(),
                            &request().currency(WabaCurrency::Usd),
                            &other.vault,
                        )
                        .await
                        .unwrap_err();
                    assert_eq!(step(&err), SHARE_CREDIT_LINE);
                    assert!(EmbeddedSignup::is_credit_step_busy(&err), "{err}");
                    assert!(!err.may_have_been_sent(), "{err}");
                })
            },
        );
        let out =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap();
        assert!(matches!(out, PendingShareClearance::Cleared(_)), "{out:?}");
        assert_eq!(
            credit_calls(&h.t.requests()[before..]),
            1,
            "the clearance's lookup; the racing share posted nothing"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// A lease lost during the check, or a credit record written meanwhile
    /// (a revocation settling the share, say), clears nothing.
    #[tokio::test]
    async fn a_clearance_that_lost_its_lease_or_its_record_clears_nothing() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        lost_share(&h).await;
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, no_funding());
        let taker = other.vault.clone();
        hooked.before(Method::GET, "/102290129340398", move || {
            Box::pin(async move {
                // The lease expired; a share took it.
                taker.kv().delete(&lease_key()).await.unwrap();
                taker.lease_credit(&waba()).await.unwrap();
            })
        });
        let err =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap_err();
        assert!(EmbeddedSignup::is_credit_step_busy(&err), "{err}");
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert!(credit.pending_share.is_some() && credit.cleared_shares.is_empty());
        assert!(
            h.vault.lease_credit(&waba()).await.is_err(),
            "the new holder keeps it"
        );
        other.vault.kv().delete(&lease_key()).await.unwrap();

        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, no_funding());
        hooked.before(Method::GET, "/102290129340398", move || {
            Box::pin(async move {
                other.vault.record_approval(&waba(), None).await.unwrap();
            })
        });
        let err =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap_err();
        assert!(EmbeddedSignup::is_credit_step_busy(&err), "{err}");
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert!(credit.pending_share.is_some() && credit.cleared_shares.is_empty());
        assert_eq!(h.t.remaining(), 0);

        // A lease lost before a record found funding the WABA is written:
        // nothing is written (a share may hold the lease now).
        let (_, version) = h.vault.credit_versioned(&waba()).await.unwrap().unwrap();
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, funding(CREDENTIAL));
        h.t.push_json(200, receiving_credential(ALLOCATION, CREDENTIAL));
        let taker = h.vault.clone();
        hooked.before(Method::GET, "/102290129340398", move || {
            Box::pin(async move {
                taker.kv().delete(&lease_key()).await.unwrap();
                taker.lease_credit(&waba()).await.unwrap();
            })
        });
        let err =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap_err();
        assert!(EmbeddedSignup::is_credit_step_busy(&err), "{err}");
        let (credit, now) = h.vault.credit_versioned(&waba()).await.unwrap().unwrap();
        assert_eq!(now, version, "nothing written");
        assert_eq!(credit.allocation_config_id, None);
        assert_eq!(h.t.remaining(), 0);
    }

    /// A post slower than its lease: an operator's clearance takes the
    /// expired lease and clears the share it flagged (Meta does not list
    /// it yet), then the post's answer is lost. The share stays pending.
    #[tokio::test]
    async fn a_lost_answer_stays_pending_after_a_clearance_during_its_post() {
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, Arc::default());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared()); // the share's lookup
        h.t.push_json(200, nothing_shared()); // the clearance's lookup
        h.t.push_json(200, no_funding()); // and its funding read
        h.t.push_error(|| TransportError::Timeout); // the post's answer
        hooked.before(
            Method::POST,
            "/whatsapp_credit_sharing_and_attach",
            move || {
                Box::pin(async move {
                    other.vault.kv().delete(&lease_key()).await.unwrap();
                    let out = other
                        .es
                        .clear_pending_share(&waba(), "ops", None, &other.vault)
                        .await
                        .unwrap();
                    assert!(matches!(out, PendingShareClearance::Cleared(_)), "{out:?}");
                })
            },
        );
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(credit.cleared_shares.len(), 1, "the clearance ran");
        assert!(credit.pending_share.is_some(), "pending again");
        assert!(credit.records_a_share());
        assert_eq!(h.t.remaining(), 0);
    }

    /// The same race, but the post answers and the ledger fails to record
    /// its allocation: the share stays pending, so the ledger still says a
    /// share may be live.
    #[tokio::test]
    async fn an_unrecorded_share_stays_pending_after_a_clearance_during_its_post() {
        let kv = Arc::new(TestKv::default());
        let (h, hooked, other) = hooked_harness(CreditSharing::ShareAndAttach, kv.clone());
        script_until_subscribe(&h.t, owner());
        h.t.push_json(200, success()); // assigned_users
        h.t.push_json(200, nothing_shared()); // the share's lookup
        h.t.push_json(200, nothing_shared()); // the clearance's lookup
        h.t.push_json(200, no_funding()); // and its funding read
        h.t.push_json(200, shared_and_attached());
        hooked.before(
            Method::POST,
            "/whatsapp_credit_sharing_and_attach",
            move || {
                Box::pin(async move {
                    other.vault.kv().delete(&lease_key()).await.unwrap();
                    let out = other
                        .es
                        .clear_pending_share(&waba(), "ops", None, &other.vault)
                        .await
                        .unwrap();
                    assert!(matches!(out, PendingShareClearance::Cleared(_)), "{out:?}");
                    TestKv::refuse(&kv.refuse_one_credit_write);
                })
            },
        );
        let err = h
            .onboard(&request().currency(WabaCurrency::Usd))
            .await
            .unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::Reconcile(_))),
            "{err}"
        );
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(credit.allocation_config_id, None, "not recorded");
        assert!(credit.pending_share.is_some(), "pending again");
        assert_eq!(h.t.remaining(), 0);
    }

    /// The share went through and funds the WABA, but Meta's lookup does
    /// not list it yet: only `primary_funding_id` shows it, and nothing
    /// tells it from the merchant's own card. Never cleared on its own:
    /// only with the operator's acknowledgement of exactly that funding.
    #[tokio::test]
    async fn an_unexplained_funding_is_cleared_only_once_acknowledged() {
        let h = harness(CreditSharing::ShareAndAttach);
        lost_share(&h).await;
        let (_, version) = h.vault.credit_versioned(&waba()).await.unwrap().unwrap();
        let shown = FundingId::new("CRED_OF_THE_LOST_SHARE");
        let refused = SharesFound {
            business_id: BusinessId::new(BUSINESS),
            funding: None,
            active: Vec::new(),
            unknown_status: Vec::new(),
            unattributed: Vec::new(),
            primary_funding_id: Some(shown.clone()),
        };
        for acknowledged in [None, Some(FundingId::new("ANOTHER_FUNDING"))] {
            let before = h.t.requests().len();
            h.t.push_json(200, nothing_shared());
            h.t.push_json(200, funding("CRED_OF_THE_LOST_SHARE"));
            let out =
                h.es.clear_pending_share(&waba(), "ops", acknowledged.as_ref(), &h.vault)
                    .await
                    .unwrap();
            assert_eq!(out, PendingShareClearance::NotCleared(refused.clone()));
            let checked = &h.t.requests()[before..];
            assert_eq!(checked.len(), 2, "{checked:?}");
            assert_lookup(&checked[0]);
            assert_funding_read(&checked[1]);
            assert_eq!(
                h.vault.credit_versioned(&waba()).await.unwrap().unwrap().1,
                version,
                "{acknowledged:?}: the record is untouched"
            );
        }
        assert_eq!(refused.unexplained_funding(), Some(&shown));
        // The lease was released each time.
        let lease = h.vault.lease_credit(&waba()).await.unwrap();
        h.vault.release_credit(&waba(), lease).await.unwrap();

        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, funding("CRED_OF_THE_LOST_SHARE"));
        let out =
            h.es.clear_pending_share(&waba(), "ops", Some(&shown), &h.vault)
                .await
                .unwrap();
        let PendingShareClearance::Cleared(entry) = out else {
            panic!("not cleared: {out:?}")
        };
        assert_eq!(entry.primary_funding_id, Some(shown), "acknowledged");
        let credit = h.vault.credit(&waba()).await.unwrap().unwrap();
        assert_eq!(credit.pending_share, None);
        assert_eq!(credit.cleared_shares, [entry]);
        assert_eq!(h.t.remaining(), 0);

        // An acknowledgement of a funding Meta no longer shows is moot:
        // nothing funds the WABA, and that alone lets the clearance through.
        let h = harness(CreditSharing::ShareAndAttach);
        lost_share(&h).await;
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, no_funding());
        let stale = FundingId::new("CRED_SEEN_EARLIER");
        let out =
            h.es.clear_pending_share(&waba(), "ops", Some(&stale), &h.vault)
                .await
                .unwrap();
        let PendingShareClearance::Cleared(entry) = out else {
            panic!("not cleared: {out:?}")
        };
        assert_eq!(entry.primary_funding_id, None);
        assert_eq!(h.t.remaining(), 0);
    }

    /// A record the lookup returns naming no receiving business may be the
    /// lost share: it stops a clearance (a revocation leaves it to a
    /// person too), and no acknowledgement lets the clearance past it.
    #[tokio::test]
    async fn a_record_naming_no_business_stops_a_clearance() {
        let h = harness(CreditSharing::ShareAndAttach);
        lost_share(&h).await;
        let (_, version) = h.vault.credit_versioned(&waba()).await.unwrap().unwrap();
        for unnamed in [
            json!({"id": "UNNAMED"}),
            json!({"id": "UNNAMED", "receiving_business": {"id": " "}}),
        ] {
            h.t.push_json(200, json!({"data": [unnamed]}));
            h.t.push_json(200, no_funding());
            let out =
                h.es.clear_pending_share(&waba(), "ops", Some(&FundingId::new("X")), &h.vault)
                    .await
                    .unwrap();
            assert_eq!(
                out,
                PendingShareClearance::NotCleared(SharesFound {
                    business_id: BusinessId::new(BUSINESS),
                    funding: None,
                    active: Vec::new(),
                    unknown_status: Vec::new(),
                    unattributed: vec![AllocationConfigId::new("UNNAMED")],
                    primary_funding_id: None,
                })
            );
            assert_eq!(
                h.vault.credit_versioned(&waba()).await.unwrap().unwrap().1,
                version
            );
        }
        assert_eq!(h.t.remaining(), 0);

        // The recorded allocation, listed without its business, gets its
        // own status check instead: Meta reports it revoked, so nothing may
        // be live.
        h.vault
            .update_credit(&waba(), |c| {
                c.allocation_config_id = Some(AllocationConfigId::new("STORED"));
                true
            })
            .await
            .unwrap();
        h.t.push_json(200, json!({"data": [{"id": "STORED"}]}));
        h.t.push_json(200, deleted());
        h.t.push_json(200, no_funding());
        let out =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap();
        assert!(matches!(out, PendingShareClearance::Cleared(_)), "{out:?}");
        assert_status(&h.t.requests()[h.t.requests().len() - 2], "STORED");
        assert_eq!(h.t.remaining(), 0);
    }

    /// The owner business the clearance checks the line's records for: the
    /// recorded one, which must agree with the token's; none at all is
    /// `OwnerUnknown`. And an invalid partner configuration is refused.
    /// Each is refused before anything is sent, clearing nothing.
    #[tokio::test]
    async fn a_clearance_needs_an_owner_it_can_check() {
        let h = harness(CreditSharing::ShareAndAttach);
        lost_share(&h).await;
        let sent = h.t.requests().len();
        h.vault
            .store(
                &StoredBusinessToken::new(WABA, AccessToken::new(TOKEN))
                    .business_id("ANOTHER_OWNER"),
            )
            .await
            .unwrap();
        let err =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "business_id"),
            "{err}"
        );
        assert!(!err.is_retryable() && !err.may_have_been_sent());
        assert_eq!(h.t.requests().len(), sent, "nothing sent");

        let h = harness(CreditSharing::ShareAndAttach);
        h.vault
            .store(&StoredBusinessToken::new(WABA, AccessToken::new(TOKEN)))
            .await
            .unwrap();
        let at = datetime!(2026-09-24 12:00 UTC);
        h.vault
            .update_credit(&waba(), |c| {
                c.pending_share = Some(at);
                true
            })
            .await
            .unwrap();
        let err =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap_err();
        assert!(
            matches!(err.credit(), Some(CreditError::OwnerUnknown(_))),
            "{err}"
        );
        assert!(!err.is_retryable() && !err.may_have_been_sent());
        assert!(h.t.requests().is_empty(), "nothing sent");
        assert_eq!(
            h.vault
                .credit(&waba())
                .await
                .unwrap()
                .unwrap()
                .pending_share,
            Some(at)
        );

        let invalid = harness_with(Some(SolutionPartner::new(
            AccessToken::new(" "),
            SYSTEM_USER,
            LINE,
        )));
        let err = invalid
            .es
            .clear_pending_share(&waba(), "ops", None, &invalid.vault)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "system_token"),
            "{err}"
        );
        assert!(invalid.t.requests().is_empty());
    }

    /// A key rotation that re-seals the credit record while the clearance
    /// checks Meta changes its version: the clearance clears nothing.
    #[tokio::test]
    async fn a_key_rotation_during_a_clearance_clears_nothing() {
        let kv = Arc::new(TestKv::default());
        let (h, hooked, _) = hooked_harness(CreditSharing::ShareAndAttach, kv.clone());
        lost_share(&h).await;
        let rotated = TokenVault::new(
            kv,
            VaultKeys::new(VaultKey::new("k2", SecretBytes::new([43; 32])).unwrap())
                .with_previous(VaultKey::new("k1", SecretBytes::new([42; 32])).unwrap()),
        )
        .unwrap();
        let rotator = rotated.clone();
        h.t.push_json(200, nothing_shared());
        h.t.push_json(200, no_funding());
        hooked.before(Method::GET, "/102290129340398", move || {
            Box::pin(async move {
                assert!(rotator.rotate(&waba()).await.unwrap(), "rotated");
            })
        });
        let err =
            h.es.clear_pending_share(&waba(), "ops", None, &h.vault)
                .await
                .unwrap_err();
        assert!(EmbeddedSignup::is_credit_step_busy(&err), "{err}");
        assert!(err.is_retryable() && !err.may_have_been_sent());
        let credit = rotated.credit(&waba()).await.unwrap().unwrap();
        assert!(credit.pending_share.is_some() && credit.cleared_shares.is_empty());
        assert_eq!(h.t.remaining(), 0);
    }

    /// `cleared_by` is an operator id for an audit entry: not blank, not
    /// free text, nothing invisible or line-breaking.
    #[test]
    fn cleared_by_is_an_operator_id() {
        assert!(check_cleared_by("op_7f3a").is_ok());
        assert!(check_cleared_by("Aïcha Ndiaye").is_ok());
        assert!(check_cleared_by(&"x".repeat(MAX_CLEARED_BY_CHARS)).is_ok());
        assert!(
            check_cleared_by(&"é".repeat(MAX_CLEARED_BY_CHARS)).is_ok(),
            "characters, not bytes"
        );
        let long = "x".repeat(MAX_CLEARED_BY_CHARS + 1);
        for bad in [
            "",
            "\u{200B}",
            "\u{FEFF}ops",
            "ops\u{202E}",
            "ops\u{2066}x",
            "ops\nother",
            "ops\u{2028}x",
            "ops\u{7}",
            &long,
        ] {
            assert_eq!(
                check_cleared_by(bad).unwrap_err().field,
                "cleared_by",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn debug_never_shows_the_system_token() {
        let h = harness(CreditSharing::ShareAndAttach);
        let text = format!("{:?} {:?}", h.es, h.es.partner());
        assert!(text.contains(SYSTEM_USER), "vacuous: {text}");
        assert!(!text.contains(SYSTEM_TOKEN), "{text}");
    }
}
