//! Hosted Embedded Signup (roadmap M3f, decisions D201 and D202; Meta's
//! `embedded-signup/hosted-es`): the merchant onboards on Meta's own page,
//! which returns nothing to the integrator. Meta sends an `account_update`
//! `PARTNER_ADDED` naming the WABA and its business portfolio, and no
//! tenant.
//!
//! ```text
//! PARTNER_ADDED (signed) ─► AccountUpdates: remembered, 7 days (wa.es.hosted), nothing fetched, nothing bound
//! POST /v1/admin/signup/hosted ─► Signups::complete_hosted: the operator names the tenant, the WABA and its
//!                           business; D4; the remembered arrival claimed (one claimant wins); the library's
//!                           onboard_hosted_with_approval (token fetched with the system token and the
//!                           appsecret_proof; verify, bind to the tenant in the approval, store, subscribe,
//!                           [credit line], register with the PIN)
//! ```
//!
//! **Who may claim: the operator.** Meta's page carries no state, so the
//! webhook cannot be tied to the tenant that sent the merchant, and the
//! WABA and business ids are not secrets (the merchant sees them in
//! WhatsApp Manager). A tenant route naming both would let tenant A claim
//! an arrival for tenant B's merchant and have that merchant's business
//! token stored under A: a cross-tenant takeover. So the claim is an
//! admin-key action (decision D202, changed 2026-11-06): the operator, who
//! knows which tenant sent which merchant, names the tenant; no tenant key
//! can claim. A WABA another tenant holds is refused
//! (`409 waba_owned_by_another_tenant`, D4) before anything is read from
//! Meta.

use meta_whatsapp_rs::client::embedded_signup::{FinishKind, HostedSignup, OnboardingRequest};
use meta_whatsapp_rs::client::phone_numbers::TwoStepPin;
use meta_whatsapp_rs::core::ids::{BusinessId, WabaId};
use meta_whatsapp_rs::core::store::Expiry;
use serde::{Deserialize, Serialize};

use super::{
    Approval, Completed, ErrorCode, OnceLock, ServiceError, Signup, Signups, VerifiedOnboarding,
    after_store, approve_binding, single,
};
use crate::authz::{AdminCaller, Authorizer};
use crate::model::{TenantId, TenantStatus};
use crate::observe::Observer;

/// The key/value namespace of the remembered `PARTNER_ADDED` arrivals
/// (decision D201; a new stored name, pending the owner's confirmation
/// before the first release).
pub(super) const HOSTED_NAMESPACE: &str = "wa.es.hosted";

/// How long an arrival waits to be claimed.
pub(super) const PENDING_TTL: std::time::Duration = std::time::Duration::from_hours(7 * 24);

/// A `PARTNER_ADDED` that was received and not yet claimed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PendingHosted {
    pub owner_business_id: String,
    pub received_at: i64,
}

/// A hosted completion, as the operator's request carried it. No `Debug`:
/// it holds the PIN.
pub struct HostedCompletion {
    /// The tenant the WABA becomes: named by the operator.
    pub tenant_id: String,
    /// The WABA the `PARTNER_ADDED` named.
    pub waba_id: String,
    /// Its `owner_business_id`, as the webhook named it.
    pub owner_business_id: String,
    /// The number's two-step verification PIN, chosen by the operator for
    /// the tenant (the hosted page asks for none): it becomes the number's
    /// PIN. Without it the number is left unregistered. Never stored.
    pub pin: Option<String>,
}

impl Signups {
    /// Claim a remembered `PARTNER_ADDED` for the tenant the operator
    /// names and onboard it, in this order: `admin` admitted (`403`); a
    /// deployment (`409 signup_not_configured`); the inputs (`422
    /// invalid_request`); hosted signup on (else `409
    /// signup_not_configured`); the tenant (none: `404 not_found`;
    /// suspended: `403 tenant_suspended`); D4 on the WABA (`409
    /// waba_owned_by_another_tenant`, nothing read or claimed); the
    /// arrival (none, or another owner business: `404 not_found`, the
    /// same answer as no WABA); its claim, atomic (the loser is `404`);
    /// the library's `onboard_hosted_with_approval`, whose approval binds
    /// the WABA and its numbers to the tenant. An arrival whose onboarding
    /// stopped before its token was stored is put back, to claim again;
    /// one that stopped after is an attempt the tenant resumes
    /// (`POST /v1/signup/resume`). Audited as the operator's
    /// (`hosted_signup_completed`).
    ///
    /// # Errors
    ///
    /// As the steps say; the store's failure.
    pub async fn complete_hosted(
        &self,
        authz: &Authorizer,
        admin: &AdminCaller,
        completion: HostedCompletion,
        observer: &dyn Observer,
    ) -> Result<Completed, ServiceError> {
        authz.admit_admin(admin)?;
        let deployment = self
            .deployment
            .as_ref()
            .ok_or_else(|| ServiceError::new(ErrorCode::SIGNUP_NOT_CONFIGURED))?;
        let tenant = TenantId::parse(&completion.tenant_id)
            .ok_or_else(|| ServiceError::invalid("tenant_id"))?;
        // The inputs first, as every route does; then the setting.
        let claim = Claim::parse(completion)?;
        if deployment.hosted.is_none() {
            return Err(ServiceError::new(ErrorCode::SIGNUP_NOT_CONFIGURED));
        }
        match authz.records().tenant(&tenant).await? {
            None => return Err(ServiceError::not_found()),
            Some(found) if found.status == TenantStatus::Suspended => {
                return Err(ServiceError::new(ErrorCode::TENANT_SUSPENDED));
            }
            Some(_) => {}
        }
        Signup {
            signups: self,
            deployment,
            authz,
            tenant: &tenant,
            key_id: admin.key_id(),
            by_admin: true,
            observer,
        }
        .claim_hosted(claim)
        .await
    }
}

/// A hosted completion whose inputs were checked.
struct Claim {
    waba_id: WabaId,
    owner: BusinessId,
    pin: Option<TwoStepPin>,
}

impl Claim {
    fn parse(completion: HostedCompletion) -> Result<Self, ServiceError> {
        let waba_id: WabaId = crate::request::typed_graph_id("waba_id", &completion.waba_id)?;
        let owner: BusinessId =
            crate::request::typed_graph_id("owner_business_id", &completion.owner_business_id)?;
        let pin = completion
            .pin
            .map(TwoStepPin::new)
            .transpose()
            .map_err(|_| ServiceError::invalid("pin"))?;
        Ok(Self {
            waba_id,
            owner,
            pin,
        })
    }
}

impl Signup<'_> {
    async fn claim_hosted(&self, claim: Claim) -> Result<Completed, ServiceError> {
        let Claim {
            waba_id,
            owner,
            pin,
        } = claim;
        let system_token = self
            .deployment
            .hosted
            .as_ref()
            .ok_or_else(|| ServiceError::new(ErrorCode::SIGNUP_NOT_CONFIGURED))?;
        let tenant = self.tenant();
        let records = self.records();
        if records
            .waba(&waba_id)
            .await?
            .is_some_and(|binding| &binding.tenant_id != tenant)
        {
            return Err(ServiceError::new(ErrorCode::WABA_OWNED_BY_ANOTHER_TENANT));
        }
        let key = waba_id.as_str();
        let hosted = &self.signups.hosted;
        let Some((pending, version, expires)) = hosted
            .get(key)
            .await?
            .filter(|(pending, ..)| pending.owner_business_id == owner.as_str())
        else {
            return Err(ServiceError::not_found());
        };
        if hosted
            .compare_and_swap(key, version, None, Expiry::Keep)
            .await?
            .is_none()
        {
            return Err(ServiceError::not_found());
        }
        let put_back = || async {
            let expiry = expires.map_or(Expiry::After(PENDING_TTL), Expiry::At);
            if let Err(error) = hosted.put_if_absent(key, &pending, expiry).await {
                tracing::warn!(
                    error = %error,
                    "a claimed PARTNER_ADDED could not be put back after its onboarding stopped"
                );
            }
        };

        let signup = HostedSignup::new(waba_id.clone(), owner);
        let mut request = OnboardingRequest::for_hosted(&signup);
        // A hosted onboarding is Cloud API's: a resume registers with a PIN.
        request.finish_kind = Some(FinishKind::Finish);
        if let Some(pin) = pin {
            request = request.register_with_pin(pin);
        }
        let approval = OnceLock::new();
        let partner = self.deployment.is_solution_partner();
        let set_approval = |a: Approval| {
            let _ = approval.set(a);
        };
        let approve = |verified: VerifiedOnboarding| {
            approve_binding(records, tenant, verified, partner, &set_approval)
        };
        let es = self.deployment.onboarding(self.authz.client());
        let onboarded = Box::pin(es.onboard_hosted_with_approval(
            &signup,
            system_token,
            &request,
            self.authz.vault_for_signup(),
            approve,
        ))
        .await;
        match onboarded {
            Ok(onboarded) => {
                let primary = onboarded.waba_id.clone();
                match self.connected(onboarded, "hosted_signup_completed").await {
                    Ok(completed) => Ok(single(completed, &request, &primary)),
                    Err(error) => {
                        put_back().await;
                        Err(error)
                    }
                }
            }
            Err(error) => {
                if !after_store(&error) {
                    put_back().await;
                }
                Err(self.stopped(&error, approval.get(), &request).await)
            }
        }
    }
}
