//! Solution Partner onboarding: the credit line steps [`EmbeddedSignup`]
//! adds when a deployment is configured with [`SolutionPartner`], and
//! revocation and offboarding from what onboarding recorded.
//!
//! Docs: `embedded-signup/onboarding-customers-as-a-solution-partner`
//! (step order: subscribe the app, share the credit line, register the
//! number), `solution-providers/share-and-revoke-credit-lines`,
//! `solution-providers/manage-system-users`.

use std::fmt;

use wa_core::error::ValidationError;
use wa_core::ids::{AllocationConfigId, BusinessId, CreditLineId, SystemUserId, WabaId};
use wa_core::secret::AccessToken;
use wa_core::{Error, Result};

use super::EmbeddedSignup;
use super::ledger::{StoredCredit, refusals};
use super::onboard::steps::{DELETE_TOKEN, REVOKE_CREDIT_LINE};
use super::vault::{StoredBusinessToken, TokenVault};
use crate::Client;
use crate::credit_lines::{CreditLines, CreditRevocation, WabaCurrency, is_shared, owned_by};
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
    /// token was deleted. `None` for a Tech Provider.
    pub credit: Option<CreditRevocation>,
    /// Whether a stored token was deleted (`false` if it already was).
    pub token_deleted: bool,
}

impl EmbeddedSignup {
    /// Onboard as a Solution Partner with `partner` (one choice per
    /// deployment): [`Self::onboard`] and [`Self::resume`] then add the
    /// credit line steps between `subscribe_app` and `register_phone`,
    /// following Meta's Solution Partner onboarding order. See the
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
    /// ([`refusals::CREDIT_LINE_REVOKED`]).
    pub fn is_credit_line_revoked(error: &Error) -> bool {
        refusal(error) == Some(refusals::CREDIT_LINE_REVOKED)
    }

    /// Whether `error` (or the step error wrapping it) says another
    /// onboarding of the WABA holds the credit step
    /// ([`refusals::CREDIT_STEP_BUSY`]): nothing was posted, resume later.
    pub fn is_credit_step_busy(error: &Error) -> bool {
        refusal(error) == Some(refusals::CREDIT_STEP_BUSY)
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
    /// - **The business** is the one Meta reported at onboarding: from the
    ///   token record, else the credit ledger (which outlives the token).
    ///   When neither exists, `owner_business_id` is used: pass the
    ///   `waba_info.owner_business_id` of a **signature-checked**
    ///   `PARTNER_*` webhook. If both exist and differ, nothing is revoked
    ///   (a validation error on `owner_business_id`): revoking another
    ///   customer's line is worse than an error.
    /// - **The business is marked revoked first** (a sealed marker in the
    ///   vault), so neither `resume` nor a new onboarding funds it again
    ///   without [`OnboardingRequest::reshare_after_revocation`](super::OnboardingRequest::reshare_after_revocation),
    ///   even if a request below fails.
    /// - Every active record naming the business is revoked, plus the
    ///   allocation stored at onboarding, each confirmed `DELETED`; see
    ///   [`CreditLines::revoke_for_business`] for what is skipped and how
    ///   failures are reported. Safe to repeat.
    ///
    /// The token and the ledger are left in the vault; [`Self::offboard`]
    /// deletes the token after revoking.
    pub async fn revoke_credit_line(
        &self,
        waba_id: &WabaId,
        owner_business_id: Option<&BusinessId>,
        vault: &TokenVault,
    ) -> Result<CreditRevocation> {
        let partner = self.require_partner()?;
        let token = vault.get(waba_id).await?;
        let credit = vault.credit(waba_id).await?;
        let stored_business = token
            .and_then(|t| t.business_id)
            .or_else(|| credit.as_ref().and_then(|c| c.business_id.clone()));
        let business = match (stored_business, owner_business_id) {
            (Some(stored), Some(hint)) if &stored != hint => {
                return Err(ValidationError::new(
                    "owner_business_id",
                    "differs from the owner business recorded at onboarding; nothing revoked",
                )
                .into());
            }
            (Some(stored), _) => Some(stored),
            (None, hint) => hint.cloned(),
        };
        let known = credit.and_then(|c| c.allocation_config_id);
        if business.is_none() && known.is_none() {
            return Err(ValidationError::new(
                "business_id",
                "nothing recorded for this WABA and no owner business given: cannot tell which line to revoke",
            )
            .into());
        }
        if let Some(business) = &business {
            vault.mark_revoked(business, &[]).await?;
        }
        let report = self
            .system_client(partner)
            .credit_lines()
            .revoke_all(&partner.credit_line_id, business.as_ref(), known.as_ref())
            .await?;
        if let Some(business) = &business {
            let ids: Vec<AllocationConfigId> = report.all().cloned().collect();
            vault.mark_revoked(business, &ids).await?;
        }
        Ok(report)
    }

    /// Offboard a merchant: in Solution Partner mode revoke the credit line
    /// first ([`Self::revoke_credit_line`], step `revoke_credit_line`), then
    /// delete the token and its phone index (step `delete_token`). A Tech
    /// Provider only deletes.
    ///
    /// For a merchant who disconnects in your CMS, and for
    /// `PARTNER_APP_UNINSTALLED` (pass its `waba_info.owner_business_id`).
    /// If revocation fails nothing is deleted, so the call can be repeated
    /// with everything it needs; it is safe to repeat after success too,
    /// and in any order with a `PARTNER_REMOVED` revocation: the credit
    /// ledger outlives the token. Stop the app's webhooks first if the token
    /// still works (`waba(..).unsubscribe_app()` with it).
    pub async fn offboard(
        &self,
        waba_id: &WabaId,
        owner_business_id: Option<&BusinessId>,
        vault: &TokenVault,
    ) -> Result<Offboarded> {
        let credit = match &self.partner {
            Some(_) => Some(
                self.revoke_credit_line(waba_id, owner_business_id, vault)
                    .await
                    .map_err(|e| e.in_step(REVOKE_CREDIT_LINE))?,
            ),
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
}

fn refusal(error: &Error) -> Option<&str> {
    match error {
        Error::Validation(v) => Some(v.field.as_str()),
        Error::Step { source, .. } => refusal(source),
        _ => None,
    }
}

fn revoked(business: &BusinessId, why: &str) -> Error {
    ValidationError::new(
        refusals::CREDIT_LINE_REVOKED,
        format!(
            "the credit line of business {business} was revoked ({why}); not sharing it again without OnboardingRequest::reshare_after_revocation"
        ),
    )
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
/// the vault's credit ledger. Holds the WABA's credit lease throughout.
pub(super) async fn share_credit_line(
    es: &EmbeddedSignup,
    plan: &CreditPlan<'_>,
    business: &Client,
    vault: &TokenVault,
    token: &StoredBusinessToken,
    resuming: bool,
) -> Result<AllocationConfigId> {
    let lease = vault.lease_credit(&token.waba_id).await?;
    let shared = share_leased(es, plan, business, vault, token, resuming).await;
    if let Err(e) = vault.release_credit(&token.waba_id, lease).await {
        // It expires on its own; the kind only, as elsewhere in the vault.
        tracing::warn!(waba_id = %token.waba_id, kind = ?e.kind(), "credit lease not released");
    }
    shared
}

/// What Meta knows of the line and the owner business: active and revoked
/// records, the stored allocation included.
struct Records {
    active: Vec<AllocationConfigId>,
    deleted: Vec<AllocationConfigId>,
}

async fn records(
    system: &CreditLines,
    line: &CreditLineId,
    owner: Option<&BusinessId>,
    stored: Option<&AllocationConfigId>,
) -> Result<Records> {
    let mut ids = match owner {
        Some(owner) => owned_by(system.allocations_for(line, owner).await?, owner),
        None => Vec::new(),
    };
    if let Some(stored) = stored
        && !ids.contains(stored)
    {
        ids.insert(0, stored.clone());
    }
    let mut records = Records {
        active: Vec::new(),
        deleted: Vec::new(),
    };
    for id in ids {
        // `owning_credit_allocation_configs` does not say whether a record
        // was revoked (nor whether Meta lists revoked ones): ask each.
        let status = system.allocation_status(&id).await?;
        let named = status
            .receiving_business
            .as_ref()
            .and_then(|b| b.id.as_ref());
        if let (Some(owner), Some(named)) = (owner, named)
            && named != owner
        {
            return Err(ValidationError::new(
                "allocation_config_id",
                format!("{id} is shared with another business than the WABA's owner; not using it"),
            )
            .into());
        }
        if status.is_deleted() {
            records.deleted.push(id);
        } else {
            records.active.push(id);
        }
    }
    Ok(records)
}

#[allow(clippy::too_many_lines)] // one check per documented failure mode, in order
async fn share_leased(
    es: &EmbeddedSignup,
    plan: &CreditPlan<'_>,
    business: &Client,
    vault: &TokenVault,
    token: &StoredBusinessToken,
    resuming: bool,
) -> Result<AllocationConfigId> {
    let partner = plan.partner;
    let line = &partner.credit_line_id;
    let waba = &token.waba_id;
    let (mut credit, mut version) = match vault.credit_versioned(waba).await? {
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
        (Some(verified), _) => Some(verified.clone()),
        (None, recorded) => recorded.clone(),
    };
    if partner.method == CreditSharing::ShareThenAttach && owner.is_none() {
        return Err(ValidationError::new(
            "business_id",
            "Meta did not report the WABA's owner business, which whatsapp_credit_sharing needs",
        )
        .into());
    }
    if resuming && owner.is_none() && credit.allocation_config_id.is_none() {
        return Err(ValidationError::new(
            "business_id",
            "cannot check whether the credit line already funds this WABA (no owner business or allocation stored); refusing to share it again blindly",
        )
        .into());
    }
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
    let marker = match &owner {
        Some(owner) => vault.revoked_business(owner).await?,
        None => None,
    };
    if let (Some(owner), Some(_), false) = (&owner, &marker, plan.reshare_after_revocation) {
        return Err(revoked(owner, "recorded by revoke_credit_line"));
    }

    // 2. What Meta has: the records naming the owner, and the stored one.
    let system = es.system_client(partner).credit_lines();
    let found = records(
        &system,
        line,
        owner.as_ref(),
        credit.allocation_config_id.as_ref(),
    )
    .await?;
    if let (Some(owner), true, false) = (
        &owner,
        found.active.is_empty() && !found.deleted.is_empty(),
        plan.reshare_after_revocation,
    ) {
        return Err(revoked(owner, "Meta reports its records DELETED"));
    }

    // 3. Does one of the active ones already fund the WABA?
    let mut funding = None;
    let mut already = None;
    for candidate in &found.active {
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

    // 4. Seal the owner and the currency before anything is posted, so a
    // share that times out is resumed in the same currency.
    credit.business_id.clone_from(&owner);
    credit.currency = Some(plan.currency.clone());
    version = Some(vault.put_credit(&credit, version).await?);

    // 5. If the line does not fund the WABA yet, share it.
    let allocation = match already {
        Some(id) => id,
        None => match partner.method {
            CreditSharing::ShareAndAttach => {
                system
                    .share_and_attach(line, waba, &plan.currency)
                    .await?
                    .allocation_config_id
            }
            CreditSharing::ShareThenAttach => {
                if found.active.is_empty()
                    && let Some(owner) = &owner
                {
                    // Recorded before the attach: a resume then checks it
                    // even if the lookup does not list it.
                    credit.allocation_config_id =
                        Some(system.share(line, owner).await?.allocation_config_id);
                    version = Some(vault.put_credit(&credit, version).await?);
                }
                business
                    .credit_lines()
                    .attach(line, waba, &plan.currency)
                    .await?
                    .allocation_config_id
            }
        },
    };

    // 6. Remember it, for resume and revocation.
    if credit.allocation_config_id.as_ref() != Some(&allocation) || credit.shared_at.is_none() {
        credit.shared_at = Some(vault.now());
    }
    credit.allocation_config_id = Some(allocation.clone());
    vault.put_credit(&credit, version).await?;
    if marker.is_some()
        && let Some(owner) = &owner
    {
        vault.clear_revoked(owner).await?;
    }
    Ok(allocation)
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
    use super::super::onboard::OnboardingRequest;
    use super::super::onboard::steps::{
        APPROVE, ASSIGN_SYSTEM_USER, DEBUG_TOKEN, EXCHANGE_CODE, LOAD_TOKEN, REGISTER_PHONE,
        SHARE_CREDIT_LINE, STORE_TOKEN, SUBSCRIBE_APP, VERIFY_ASSETS,
    };
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
        let vault = TokenVault::new(
            Arc::new(MemoryKvStore::new()),
            VaultKeys::new(VaultKey::new("k1", SecretBytes::new([42; 32])).unwrap()),
        )
        .unwrap()
        .with_clock(Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC))));
        let es = client.embedded_signup(AppCredentials::new(APP_ID, APP_SECRET));
        let es = match partner {
            Some(p) => es.solution_partner(p),
            None => es,
        };
        Harness { t, es, vault }
    }

    fn harness(method: CreditSharing) -> Harness {
        harness_with(Some(partner(method)))
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
        h.es.onboard(&request().currency(WabaCurrency::Usd), &h.vault)
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

        let done =
            h.es.onboard(&request().currency(WabaCurrency::Eur), &h.vault)
                .await
                .unwrap();
        assert_eq!(
            done.steps_completed,
            [
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
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

        let done = h.es.onboard(&request(), &h.vault).await.unwrap();
        assert_eq!(
            done.steps_completed,
            [
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
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
        let err = h.es.onboard(&request(), &h.vault).await.unwrap_err();
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
        let err =
            h.es.onboard(
                &request().currency(WabaCurrency::Other("eur".into())),
                &h.vault,
            )
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
            let err =
                h.es.onboard(&request().currency(WabaCurrency::Usd), &h.vault)
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
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
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
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
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
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
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
        let done =
            h.es.onboard(&request().currency(WabaCurrency::Gbp), &h.vault)
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
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
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

    #[tokio::test]
    async fn a_resume_that_cannot_check_refuses_to_share_blindly() {
        // Meta reported no owner business, so nothing can be looked up.
        let h = harness(CreditSharing::ShareAndAttach);
        let request = request().currency(WabaCurrency::Usd);
        script_until_subscribe(&h.t, json!({"id": WABA}));
        h.t.push_json(200, success()); // assigned_users
        h.t.push_error(|| TransportError::Timeout);
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        let reqs = h.t.requests();
        assert_eq!(
            reqs[6].path(),
            format!("/v25.0/{LINE}/whatsapp_credit_sharing_and_attach"),
            "a first attempt without an owner shares directly"
        );
        let before = reqs.len();

        h.t.push_json(200, success()); // subscribe
        h.t.push_json(200, success()); // assigned_users
        let err = h.es.resume(&waba(), &request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert!(matches!(
            &err,
            Error::Step { source, .. } if matches!(&**source, Error::Validation(v) if v.field == "business_id")
        ));
        assert_eq!(h.t.requests().len(), before + 2, "no share posted");
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
        let err =
            h.es.onboard(&request().currency(WabaCurrency::Usd), &h.vault)
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
        let err =
            h.es.onboard(&request().currency(WabaCurrency::Usd), &h.vault)
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
        h.es.onboard(&request().currency(WabaCurrency::Usd), &h.vault)
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
        let err =
            h.es.onboard(&request().currency(WabaCurrency::Usd), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), SHARE_CREDIT_LINE);
        assert!(EmbeddedSignup::is_credit_step_busy(&err), "{err}");
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
        h.t.push_json(200, shared_record(ALLOCATION));
        h.t.push_json(200, active());
        h.t.push_json(200, success());
        h.t.push_json(200, deleted());
        let report =
            h.es.revoke_credit_line(&waba(), Some(&BusinessId::new(BUSINESS)), &h.vault)
                .await
                .unwrap();
        assert_eq!(report.revoked, [AllocationConfigId::new(ALLOCATION)]);
        assert_lookup(&h.t.requests()[0]);
        assert_eq!(h.t.remaining(), 0);
        assert!(
            h.vault
                .revoked_business(&BusinessId::new(BUSINESS))
                .await
                .unwrap()
                .is_some()
        );

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
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "receiving_business" && v.reason.contains("UNATTRIBUTED")),
            "the unattributed record is reported, not revoked: {err}"
        );
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
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "allocation_config_id"),
            "{err}"
        );
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

    #[test]
    fn debug_never_shows_the_system_token() {
        let h = harness(CreditSharing::ShareAndAttach);
        let text = format!("{:?} {:?}", h.es, h.es.partner());
        assert!(text.contains(SYSTEM_USER), "vacuous: {text}");
        assert!(!text.contains(SYSTEM_TOKEN), "{text}");
    }
}
