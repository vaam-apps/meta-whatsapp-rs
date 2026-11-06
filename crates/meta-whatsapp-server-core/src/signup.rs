//! Embedded Signup as services (docs/design/server.md, sections 3.4, 4.2
//! and 4.6; roadmap M3a), over the library's `EmbeddedSignup`, whose steps,
//! checks and credit line calls are all its own: a merchant connects their
//! WhatsApp number from the integrator's page, and the service ends up
//! holding their business token in the vault, the WABA and its numbers
//! bound to the tenant.
//!
//! ```text
//! POST /v1/signup/sessions ─► Signup::start: an attempt (the library's SignupSessions, wa.es.session,
//!                              15 minutes) bound to the caller's tenant; what FB.login needs
//! POST /v1/signup/complete ─► Signup::complete: every input checked (state, code, event, PIN), D4 on
//!                              the claimed WABA, then the attempt redeemed for the caller's tenant (never
//!                              burnt on another's), then EmbeddedSignup::onboard_with_approval, whose
//!                              approval (after Meta verified the WABA and its numbers, before anything
//!                              is stored, subscribed or shared) binds them to the tenant, D4 atomically
//! GET  /v1/signup/attempts ─► Signup::attempts: the tenant's kept attempts
//! POST /v1/signup/resume   ─► Signup::resume: the tenant's attempt, EmbeddedSignup::resume_with_approval
//!                              (the binding checked again before any credit call), from the stored token
//! ```
//!
//! **The mode is the deployment's** ([`SignupDeployment`]): a Tech
//! Provider's merchants pay Meta; a Solution Partner's credit line pays
//! (`SolutionPartner`, with the partner's system token, which every credit
//! call carries, never the merchant's). Without the Meta app's id and the
//! Facebook Login for Business configuration, every route answers `409
//! signup_not_configured`.
//!
//! **What never leaves**: the merchant's token lives in the vault only (no
//! answer, log, error, audit event or kept attempt holds it); the PIN is
//! typed per attempt and never stored (decision D6); the code and the
//! state are in no log. Meta's texts are in no log either
//! ([`ServiceError::from_library_withholding_texts`]) and in no answer:
//! an onboarding that stopped is `502 onboarding_failed` with its `step`,
//! whether it is `resumable`, and Meta's code and trace id.
//!
//! **A kept attempt** ([`NewSignupAttempt`], the backend's records) is an
//! onboarding that stopped after its token was stored: the WABA is bound,
//! the token stored, a later step failed (subscribing, the credit line, the
//! registration, a coexistence sync). It holds the session info (the
//! browser's ids) and the flow's completion, never the code or the PIN,
//! goes with the WABA's binding, and is the tenant's only: another
//! tenant's resume is `404 nothing_to_resume`.
//!
//! **Offboarding** ([`AccountUpdates`], [`Signups::offboard`]): in
//! Solution Partner mode the credit line is revoked first and the token
//! and bindings deleted second, by a tenant's disconnection, by
//! `PARTNER_APP_UNINSTALLED` of the service's app and by `ACCOUNT_DELETED`;
//! `PARTNER_REMOVED` revokes the line (decision D14). A webhook is
//! confirmed with Graph before it is acted on (design section 6), unless
//! the deployment says otherwise ([`AccountUpdateConfirmation`]).
//! Re-funding a business whose line was revoked is an explicit, audited
//! operator action ([`Signups::reshare_credit_line`]): neither a resume
//! nor a new signup does it.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::AppCredentials;
use meta_whatsapp_rs::client::embedded_signup::{
    EmbeddedSignup, EmbeddedSignupEvent, FinishKind, LaunchOptions, Onboarded, OnboardingRequest,
    SessionInfo, SignupCode, SignupSessions, SignupState, SolutionPartner, StoredBusinessToken,
    TokenVersion, VerifiedOnboarding, WabaOutcome, steps,
};
use meta_whatsapp_rs::client::phone_numbers::TwoStepPin;
use meta_whatsapp_rs::core::clock::Clock;
use meta_whatsapp_rs::core::config::ApiVersion;
use meta_whatsapp_rs::core::error::{StorageError, ValidationError};
use meta_whatsapp_rs::core::ids::{AppId, BusinessId, PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::store::{JsonStore, KvStore};
use meta_whatsapp_rs::webhooks::WebhookEvent;
use meta_whatsapp_rs::webhooks::fields::AccountUpdateEvent;
use time::OffsetDateTime;

mod hosted;
mod pre_verified;

pub use hosted::HostedCompletion;
pub use pre_verified::{PartnerPool, PreVerifiedAdd, PreVerifiedCode, PreVerifiedList};

use crate::audit::{Subject, audit, tenant_audit};
use crate::authz::{AdminCaller, Authorizer, Caller, OwnedWaba};
use crate::error::{ErrorCode, ServiceError};
use crate::inbound::AccountActions;
use crate::model::{
    BindOutcome, BindingEpoch, Listing, NewSignupAttempt, PageRequest, SignupAttempt, TenantId,
    VaultEntry, WabaBinding,
};
use crate::number_events::{DisconnectReason, NumberEvent};
use crate::observe::Observer;
use crate::request::typed_graph_id;
use crate::store::RecordStore;

/// How long an attempt waits for its completion: the design's 15 minutes
/// (section 4.2), several screens and maybe an SMS code.
pub const ATTEMPT_TTL: Duration = Duration::from_mins(15);

/// Whether an account update is confirmed with Graph before the service
/// acts on it (design section 6: the app secret signs every tenant's
/// deliveries, so whoever holds it could forge a `PARTNER_APP_UNINSTALLED`;
/// roadmap M3a, proposed decision D72).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccountUpdateConfirmation {
    /// Ask Meta first (the default): `debug_token` on the WABA's stored
    /// token. A token Meta still reports valid and still granted the WABA
    /// contradicts the webhook, which is then recorded and not acted on
    /// (logged at `warn`, target `audit`). A WABA with no stored token
    /// leaves nothing to confirm with, and nothing to delete: only its
    /// credit line is revoked then, from the vault's ledger, else the
    /// webhook's `owner_business_id`, on the signature alone; so is a
    /// `PARTNER_REMOVED` naming no WABA (decision D72: a revocation the
    /// operator's reshare undoes, never a deletion).
    #[default]
    Graph,
    /// Act on the signature alone (`WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION=signature`):
    /// for a deployment whose merchants' tokens Meta keeps valid after
    /// such an event, which Meta does not document either way. The app
    /// secret is then the only gate: whoever holds it can sign a delivery
    /// that deletes any merchant's token and revokes their line.
    Signature,
}

/// The deployment's Embedded Signup: the Meta app, the Facebook Login for
/// Business configuration, and the mode (a Solution Partner's credit line,
/// or none: a Tech Provider). `Debug` shows no secret.
#[derive(Clone)]
pub struct SignupDeployment {
    app: AppCredentials,
    config_id: String,
    partner: Option<SolutionPartner>,
    confirmation: AccountUpdateConfirmation,
    every_granted_waba: bool,
    hosted: Option<AccessToken>,
    pool: Option<PartnerPool>,
}

impl std::fmt::Debug for SignupDeployment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignupDeployment")
            .field("app_id", &self.app.app_id)
            .field("config_id", &self.config_id)
            .field("solution_partner", &self.partner.is_some())
            .field("confirmation", &self.confirmation)
            .field("every_granted_waba", &self.every_granted_waba)
            .field("hosted_signup", &self.hosted.is_some())
            .field("pre_verified_pool", &self.pool.is_some())
            .finish_non_exhaustive()
    }
}

impl SignupDeployment {
    /// A Tech Provider's signup for `app` (`WA_APP_ID`, `WA_APP_SECRET`),
    /// launched with the configuration `config_id` (`WA_ES_CONFIG_ID`).
    pub fn new(app: AppCredentials, config_id: impl Into<String>) -> Self {
        Self {
            app,
            config_id: config_id.into(),
            partner: None,
            confirmation: AccountUpdateConfirmation::default(),
            every_granted_waba: false,
            hosted: None,
            pool: None,
        }
    }

    /// Bind every WABA a signup grants to its tenant, not only the primary
    /// one (`WA_SIGNUP_EVERY_GRANTED_WABA`, roadmap M3f, decision D200).
    /// Off by default: the signup's primary WABA (the one its event
    /// claims, else the first it names) is the only one bound.
    #[must_use]
    pub fn every_granted_waba(mut self, enabled: bool) -> Self {
        self.every_granted_waba = enabled;
        self
    }

    /// Handle hosted Embedded Signup (`WA_HOSTED_SIGNUP`, roadmap M3f,
    /// decision D201): `PARTNER_ADDED` webhooks are remembered and a
    /// tenant claims one (`POST /v1/signup/hosted`), the business token
    /// fetched with `system_token`, the deployment's system user token.
    #[must_use]
    pub fn hosted_signup(mut self, system_token: AccessToken) -> Self {
        self.hosted = Some(system_token);
        self
    }

    /// Manage the pool of pre-verified numbers of `business_id`, the
    /// deployment's own business portfolio, with `token` (`business_management`;
    /// roadmap M3f, decision D204).
    #[must_use]
    pub fn pre_verified_pool(mut self, business_id: BusinessId, token: AccessToken) -> Self {
        self.pool = Some(PartnerPool { business_id, token });
        self
    }

    /// Whether every granted WABA is bound.
    pub fn binds_every_granted_waba(&self) -> bool {
        self.every_granted_waba
    }

    /// Whether hosted Embedded Signup is handled.
    pub fn handles_hosted_signup(&self) -> bool {
        self.hosted.is_some()
    }

    /// Whether the pool of pre-verified numbers is managed.
    pub fn manages_pre_verified_pool(&self) -> bool {
        self.pool.is_some()
    }

    /// Onboard as a Solution Partner: `partner`'s credit line funds every
    /// merchant (`WA_ONBOARDING_MODE=solution_partner`).
    #[must_use]
    pub fn solution_partner(mut self, partner: SolutionPartner) -> Self {
        self.partner = Some(partner);
        self
    }

    /// Confirm account updates as `confirmation` says.
    #[must_use]
    pub fn confirm_account_updates(mut self, confirmation: AccountUpdateConfirmation) -> Self {
        self.confirmation = confirmation;
        self
    }

    /// Whether the deployment onboards as a Solution Partner.
    pub fn is_solution_partner(&self) -> bool {
        self.partner.is_some()
    }

    /// The Meta app's id.
    pub fn app_id(&self) -> &AppId {
        &self.app.app_id
    }

    /// The library's onboarding for this deployment, calling Graph with
    /// `client` (tokenless: each call carries the token it needs).
    fn onboarding(&self, client: &Client) -> EmbeddedSignup {
        let es = client.embedded_signup(self.app.clone());
        match &self.partner {
            Some(partner) => es.solution_partner(partner.clone()),
            None => es,
        }
    }
}

/// The signup services: the attempts in the backend's key/value store (the
/// library's `SignupSessions`, namespace `wa.es.session`, unchanged), the
/// service's clock, and the deployment's [`SignupDeployment`], if any.
/// Built once by the HTTP crate's `AppState::from_backend`; opened for an
/// admitted caller ([`Self::open`]).
pub struct Signups {
    sessions: SignupSessions,
    clock: Arc<dyn Clock>,
    deployment: Option<SignupDeployment>,
    hosted: JsonStore<hosted::PendingHosted>,
    reservations: JsonStore<pre_verified::Reservation>,
}

impl std::fmt::Debug for Signups {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signups")
            .field("deployment", &self.deployment)
            .finish_non_exhaustive()
    }
}

/// What [`Signup::start`] hands the page: the attempt's state (for the
/// completion, in a body: never a URL), and what `FB.init` and `FB.login`
/// need. `Debug` hides the state.
pub struct Launch {
    /// The attempt: single use, bound to the caller's tenant.
    pub state: SignupState,
    /// When it lapses (the service's clock; the store's own expiry is what
    /// holds).
    pub expires_at: OffsetDateTime,
    /// The Meta app's id (`FB.init`'s `appId`): public.
    pub app_id: AppId,
    /// The Facebook Login for Business configuration id.
    pub config_id: String,
    /// The Graph API version (`FB.init`'s `version`).
    pub graph_api_version: ApiVersion,
    /// `FB.login`'s options (the library's `LaunchOptions`, as JSON).
    pub launch_options: serde_json::Value,
}

impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launch")
            .field("expires_at", &self.expires_at)
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

/// A completion, as the page posted it (through the integrator's backend).
/// No `Debug`: it holds the code and the PIN.
pub struct Completion {
    /// The attempt's state.
    pub state: String,
    /// The code `FB.login` returned (30 seconds, single use).
    pub code: String,
    /// The `WA_EMBEDDED_SIGNUP` message event, as received.
    pub event: serde_json::Value,
    /// The number's two-step verification PIN, typed by the merchant: it
    /// becomes the new number's PIN, or must match the one it has. Without
    /// it, the number is left unregistered.
    pub pin: Option<String>,
}

/// A resumption, as the request carried it. No `Debug`: it holds the PIN.
pub struct Resumption {
    /// The attempt's WABA.
    pub waba_id: String,
    /// A corrected PIN, when `register_phone` stopped it.
    pub pin: Option<String>,
}

/// What a completion or a resumption ended with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completed {
    /// The business is onboarded: the WABA and its numbers are the
    /// tenant's.
    Connected(Connected),
    /// The merchant left the flow (`CANCEL`): nothing was redeemed, the
    /// attempt may still complete.
    Cancelled {
        /// The screen they left, as Meta names it.
        current_step: Option<String>,
    },
}

/// An onboarded business. Never the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connected {
    /// The verified WABA, now the tenant's.
    pub waba_id: WabaId,
    /// Every number of the WABA, now the tenant's.
    pub phone_number_ids: Vec<PhoneNumberId>,
    /// The steps that ran, in order (the library's `steps`).
    pub steps_completed: Vec<&'static str>,
    /// Whether it was a coexistence onboarding (the WhatsApp Business app's
    /// number): the contacts and history syncs ran as its last steps.
    pub needs_coexistence_sync: bool,
    /// The partner's credit allocation funding the WABA (Solution Partner).
    pub credit_allocation_id: Option<String>,
    /// With the option of decision D200 on, a completion's WABAs, the
    /// primary one first, each with what became of it (roadmap M3f), one
    /// per granted WABA. Empty with it off (the answer's `waba_id` is the
    /// only WABA), and for a resumption or a reshare.
    pub wabas: Vec<WabaResult>,
    /// With the option off, the other WABAs the signup's event named and
    /// this completion did not bind (decision D200): bind them with a
    /// signup of their own, or turn the option on.
    pub not_bound_waba_ids: Vec<WabaId>,
}

/// What became of one WABA of a completion (roadmap M3f).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WabaResult {
    /// The WABA.
    pub waba_id: WabaId,
    /// Whether it is the signup's primary WABA: the one its event claims,
    /// else the first it names (the newest grant).
    pub primary: bool,
    /// What became of it.
    pub status: WabaStatus,
    /// `connected`: its numbers, now the tenant's.
    pub phone_number_ids: Vec<PhoneNumberId>,
    /// `failed`: the step it stopped at.
    pub failed_step: Option<String>,
    /// `failed`: whether `POST /v1/signup/resume` can finish it.
    pub resumable: bool,
    /// `refused`, `failed`: the error code (`waba_owned_by_another_tenant`,
    /// `onboarding_failed`, ...).
    pub code: Option<String>,
}

/// The state of one WABA after a completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WabaStatus {
    /// Onboarded and bound to the tenant.
    Connected,
    /// Not the primary WABA, and the vault already held a token for it:
    /// left as it was.
    Held,
    /// Another tenant holds it (decision D4): nothing was stored for it.
    Refused,
    /// Stopped at a step; see [`WabaResult::failed_step`].
    Failed,
}

impl From<Onboarded> for Connected {
    fn from(onboarded: Onboarded) -> Self {
        let needs_coexistence_sync = onboarded.needs_coexistence_sync();
        Self {
            waba_id: onboarded.waba_id,
            phone_number_ids: onboarded.phone_number_ids,
            steps_completed: onboarded.steps_completed,
            needs_coexistence_sync,
            credit_allocation_id: onboarded
                .allocation_config_id
                .map(|id| id.as_str().to_owned()),
            wabas: Vec::new(),
            not_bound_waba_ids: Vec::new(),
        }
    }
}

/// What an approval did, read once the library's call returned.
#[derive(Debug, Clone)]
enum Approval {
    /// The WABA and its numbers are bound to the tenant at `epoch`;
    /// `fresh` when no binding was there before (one to take back when the
    /// store fails).
    Bound { epoch: BindingEpoch, fresh: bool },
    /// Another tenant holds the WABA, or one of its numbers (decision D4).
    Owned,
    /// The tenant is gone.
    NoTenant,
}

/// A single-WABA completion's answer: as the option is off, the other WABAs
/// its event named, and nothing bound for them (decision D200).
fn single(completed: Completed, request: &OnboardingRequest, primary: &WabaId) -> Completed {
    let Completed::Connected(mut connected) = completed else {
        return completed;
    };
    connected.not_bound_waba_ids = request
        .session
        .waba_id
        .iter()
        .chain(request.session.waba_ids.iter())
        .filter(|id| *id != primary)
        .fold(Vec::new(), |mut seen: Vec<WabaId>, id| {
            if !seen.contains(id) {
                seen.push(id.clone());
            }
            seen
        });
    Completed::Connected(connected)
}

/// The approvals of a completion's WABAs, by WABA (several with the
/// option of decision D200 on).
#[derive(Default)]
struct Approvals(std::sync::Mutex<std::collections::HashMap<WabaId, Approval>>);

impl Approvals {
    fn set(&self, waba_id: &WabaId, approval: Approval) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(waba_id.clone(), approval);
    }

    fn get(&self, waba_id: &WabaId) -> Option<Approval> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(waba_id)
            .cloned()
    }
}

impl Signups {
    /// The services on `kv` (the backend's: where the vault is), reading
    /// "now" from `clock` (the service's), for `deployment` (`None`: every
    /// route answers `409 signup_not_configured`).
    pub fn new(
        kv: Arc<dyn KvStore>,
        clock: Arc<dyn Clock>,
        deployment: Option<SignupDeployment>,
    ) -> Self {
        Self {
            sessions: SignupSessions::new(Arc::clone(&kv)),
            clock,
            deployment,
            hosted: JsonStore::new(Arc::clone(&kv), hosted::HOSTED_NAMESPACE),
            reservations: JsonStore::new(kv, pre_verified::RESERVATION_NAMESPACE),
        }
    }

    /// The deployment's signup, when it has one.
    pub fn deployment(&self) -> Option<&SignupDeployment> {
        self.deployment.as_ref()
    }

    /// The services for `caller`, once `authz` admits it
    /// ([`Authorizer::admit`]).
    ///
    /// # Errors
    ///
    /// `caller` another [`Authorizer`] made: `403 forbidden`; no
    /// deployment: `409 signup_not_configured`. Nothing is read.
    pub fn open<'a>(
        &'a self,
        authz: &'a Authorizer,
        caller: &'a Caller,
    ) -> Result<Signup<'a>, ServiceError> {
        authz.admit(caller)?;
        let deployment = self
            .deployment
            .as_ref()
            .ok_or_else(|| ServiceError::new(ErrorCode::SIGNUP_NOT_CONFIGURED))?;
        Ok(Signup {
            signups: self,
            deployment,
            authz,
            caller,
            observer: &(),
        })
    }

    /// What the webhook pipeline does about an account update in this
    /// deployment ([`AccountUpdates`], over `authz`'s records and vault);
    /// `None` without a deployment (no merchant was onboarded through it).
    pub fn account_actions(&self, authz: Arc<Authorizer>) -> Option<Arc<dyn AccountActions>> {
        let deployment = self.deployment.clone()?;
        Some(Arc::new(AccountUpdates {
            authz,
            deployment,
            hosted: self.hosted.clone(),
            clock: Arc::clone(&self.clock),
        }))
    }

    /// Disconnect `owned` in Solution Partner mode (the tenant's `DELETE
    /// /v1/wabas/{waba_id}`, after the app was unsubscribed, and an
    /// admin's deletion of its tenant, decision D75): the credit line
    /// revoked first and the token deleted second (the library's
    /// `EmbeddedSignup::offboard_if_unchanged`), then the bindings, only
    /// the token and the binding `owned` was made from: the revocation
    /// takes several calls to Meta, and a WABA moved meanwhile keeps its
    /// new holder's token and binding. `Ok(false)` outside Solution Partner
    /// mode: nothing done, the caller forgets the WABA as a Tech Provider
    /// does. The callers audit it (`credit_line_revoked`).
    ///
    /// # Errors
    ///
    /// `owned` another [`Authorizer`] made: `403 forbidden`. The WABA moved
    /// since `owned` was made, before the revocation (`503`, nothing
    /// revoked or deleted) or during it (`503`, the token and the binding
    /// left, logged at `warn`); its token re-encrypted by a rotation
    /// meanwhile (`503`, a repeat reads it anew). A revocation that failed:
    /// its error (`step: revoke_credit_line`, with Meta's `details`), and
    /// nothing deleted.
    pub async fn offboard(
        &self,
        authz: &Authorizer,
        owned: &OwnedWaba,
        observer: &dyn Observer,
    ) -> Result<bool, ServiceError> {
        authz.admit_waba(owned)?;
        let Some(deployment) = self.deployment.as_ref().filter(|d| d.is_solution_partner()) else {
            return Ok(false);
        };
        if !owned.still_bound(authz).await? {
            return Err(ServiceError::from(StorageError::Busy));
        }
        let records = authz.records();
        let waba_id = owned.waba_id();
        // A rotation must reach the ledger once the binding goes.
        records
            .record_vault_entry(&VaultEntry::Waba(waba_id.clone()))
            .await?;
        let es = deployment.onboarding(authz.client());
        let offboarded = match es
            .offboard_if_unchanged(
                waba_id,
                None,
                authz.vault_for_signup(),
                Some(owned.token_version()),
            )
            .await
        {
            Ok(offboarded) => offboarded,
            Err(error) => {
                let api = ServiceError::from_library(&error).with_details(&error);
                if error.graph().is_some() {
                    observer.graph_error(api.code());
                }
                return Err(api);
            }
        };
        // The token `owned` read, then the binding it was made from: a
        // token stored or a binding made since is the new holder's.
        if !offboarded.token_deleted || !records.unbind_waba_if(owned.epoch()).await? {
            let moved = !owned.still_bound(authz).await.unwrap_or(false);
            tracing::warn!(
                waba_id = waba_id.as_str(),
                moved,
                token_deleted = offboarded.token_deleted,
                "a WABA's token or binding changed while its credit line was being revoked: \
                 what changed was left"
            );
            return Err(ServiceError::from(StorageError::Busy));
        }
        Ok(true)
    }

    /// Fund again the business of the WABA `waba_id`, whose credit line was
    /// revoked: an operator's explicit decision (decision D14: no resume
    /// and no new signup does it), audited (`credit_line_reshared`). The
    /// library's `EmbeddedSignup::resume_with_approval` with
    /// `reshare_after_revocation`, from the WABA's stored token: subscribe,
    /// the system user, the credit line checked and shared; nothing
    /// registered, no coexistence sync. Its approval checks the binding
    /// again before any credit call. A tenant's attempt kept for the WABA
    /// is finished by it, and dropped.
    ///
    /// # Errors
    ///
    /// `admin` another [`Authorizer`] made: `403 forbidden`; not a
    /// Solution Partner deployment: `409 signup_not_configured`; a WABA no
    /// tenant holds (or not an id): `404 not_found`; a step that stopped:
    /// `502 onboarding_failed` (`step`, `resumable`).
    pub async fn reshare_credit_line(
        &self,
        authz: &Authorizer,
        admin: &AdminCaller,
        waba_id: &str,
        observer: &dyn Observer,
    ) -> Result<Connected, ServiceError> {
        authz.admit_admin(admin)?;
        // A path's id that is no id names no WABA, before anything else.
        let waba_id: WabaId =
            typed_graph_id("waba_id", waba_id).map_err(|_| ServiceError::not_found())?;
        let deployment = self
            .deployment
            .as_ref()
            .filter(|d| d.is_solution_partner())
            .ok_or_else(|| ServiceError::new(ErrorCode::SIGNUP_NOT_CONFIGURED))?;
        let records = authz.records();
        let binding = records
            .waba(&waba_id)
            .await?
            .ok_or_else(ServiceError::not_found)?;
        let tenant = binding.tenant_id.clone();
        let mut session = SessionInfo::default();
        session.waba_id = Some(waba_id.clone());
        let request = OnboardingRequest::for_resume(session).reshare_after_revocation();
        let approval = OnceLock::new();
        let approval_set = &approval;
        let tenant_held = &tenant;
        // The share writes the WABA's ledger: remembered for a key rotation
        // first, as a partner's signup does (a WABA onboarded while the
        // deployment was a Tech Provider has no entry yet).
        let approve = move |verified: VerifiedOnboarding| async move {
            remember_ledger(records, &verified).await?;
            approve_held(records, tenant_held, verified, approval_set).await
        };
        let es = deployment.onboarding(authz.client());
        let resumed = es
            .resume_with_approval(&waba_id, &request, authz.vault_for_signup(), approve)
            .await;
        match resumed {
            Ok(onboarded) => {
                let connected = Connected::from(onboarded);
                record_allocation(records, &tenant, &connected).await?;
                records.delete_signup_attempt(&tenant, &waba_id).await?;
                audit(
                    "credit_line_reshared",
                    admin.key_id(),
                    &Subject {
                        tenant: Some(tenant.as_str()),
                        waba_id: Some(waba_id.as_str()),
                        ..Subject::default()
                    },
                );
                Ok(connected)
            }
            Err(_) if matches!(approval.get(), Some(Approval::Owned | Approval::NoTenant)) => {
                Err(ServiceError::not_found())
            }
            Err(error) => Err(answered(&error, observer).as_onboarding_failed(after_store(&error))),
        }
    }
}

/// The signup services for one admitted caller ([`Signups::open`]).
pub struct Signup<'a> {
    signups: &'a Signups,
    deployment: &'a SignupDeployment,
    authz: &'a Authorizer,
    caller: &'a Caller,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for Signup<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signup")
            .field("caller", self.caller)
            .finish_non_exhaustive()
    }
}

impl<'a> Signup<'a> {
    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    fn records(&self) -> &'a dyn RecordStore {
        self.authz.records()
    }

    fn tenant(&self) -> &'a TenantId {
        self.caller.tenant()
    }

    /// Start an attempt for the caller's tenant: a single-use state bound
    /// to it for [`ATTEMPT_TTL`] (the library's `SignupSessions::start`),
    /// and what the page needs to launch the flow; `coexistence` launches
    /// the WhatsApp Business app's onboarding (`featureType:
    /// whatsapp_business_app_onboarding`). Asks Meta nothing.
    ///
    /// # Errors
    ///
    /// The store's failure.
    pub async fn start(&self, coexistence: bool) -> Result<Launch, ServiceError> {
        self.start_with(coexistence, &[]).await
    }

    /// [`Self::start`], offering the pre-verified numbers `pre_verified`
    /// on the number screen (roadmap M3f, decision D203): each must be
    /// reserved for the caller's tenant by the operator, else `422
    /// invalid_request` on `pre_verified_phone_ids` (the same answer for
    /// an unreserved number and another tenant's).
    ///
    /// # Errors
    ///
    /// As [`Self::start`], and the ids above.
    pub async fn start_with(
        &self,
        coexistence: bool,
        pre_verified: &[String],
    ) -> Result<Launch, ServiceError> {
        let mut options = LaunchOptions::new(self.deployment.config_id.as_str());
        if coexistence {
            options = options.coexistence();
        }
        let options = self.offer_pre_verified(options, pre_verified).await?;
        let launch_options = options.to_json().map_err(|_| ServiceError::internal())?;
        let state = self
            .signups
            .sessions
            .start(self.tenant().as_str(), ATTEMPT_TTL)
            .await
            .map_err(|error| ServiceError::from_library_withholding_texts(&error))?;
        let expires_at = self.signups.clock.now()
            + time::Duration::try_from(ATTEMPT_TTL).unwrap_or(time::Duration::ZERO);
        Ok(Launch {
            state,
            expires_at,
            app_id: self.deployment.app.app_id.clone(),
            config_id: self.deployment.config_id.clone(),
            graph_api_version: self.authz.client().endpoint().version(),
            launch_options,
        })
    }

    /// Complete an attempt for the caller's tenant, in this order:
    ///
    /// 1. every input checked: the state, the code, the PIN (6 digits) and
    ///    the event, each `422 invalid_request` on its field, before
    ///    anything is read (a malformed post never burns the attempt); a
    ///    `CANCEL` event answers [`Completed::Cancelled`] and redeems
    ///    nothing;
    /// 2. decision D4 on the claimed WABA: another tenant's is `409
    ///    waba_owned_by_another_tenant`, the attempt kept;
    /// 3. the attempt redeemed for the caller's tenant (the library's
    ///    `SignupSessions::redeem`, which never burns another tenant's):
    ///    expired, used, or another's is `403 stale_attempt`;
    /// 4. the library's `onboard_with_approval` (never retried: the code is
    ///    spent), whose approval, once Meta verified the WABA and its
    ///    numbers and before anything is stored, subscribed or shared,
    ///    binds them to the tenant (D4 again, atomically: another tenant's
    ///    is `409 waba_owned_by_another_tenant`, nothing stored); in
    ///    Solution Partner mode it first remembers the WABA's ledger for a
    ///    key rotation;
    /// 5. once onboarded, the binding confirmed (moved meanwhile: the token
    ///    this stored taken back, `503`), a kept attempt of the WABA
    ///    dropped, the credit allocation recorded on the binding, audited
    ///    (`signup_completed`).
    ///
    /// A step that stops after the token was stored keeps the attempt
    /// (`502 onboarding_failed`, `resumable: true`); one that stops before
    /// is `resumable: false` (a binding this call made is taken back when
    /// the store itself failed).
    ///
    /// # Errors
    ///
    /// As the steps say; the store's failure.
    pub async fn complete(&self, completion: Completion) -> Result<Completed, ServiceError> {
        let Completion {
            state,
            code,
            event,
            pin,
        } = completion;
        let state = SignupState::parse(&state).map_err(|_| ServiceError::invalid("state"))?;
        let code = SignupCode::new(code).map_err(|_| ServiceError::invalid("code"))?;
        let pin = pin
            .map(TwoStepPin::new)
            .transpose()
            .map_err(|_| ServiceError::invalid("pin"))?;
        let event =
            EmbeddedSignupEvent::from_value(event).map_err(|_| ServiceError::invalid("event"))?;
        if let EmbeddedSignupEvent::Cancel(cancel) = &event {
            return Ok(Completed::Cancelled {
                current_step: cancel
                    .current_step
                    .as_ref()
                    .map(|step| step.as_str().to_owned()),
            });
        }
        let mut request = OnboardingRequest::from_event(code, &event)
            .map_err(|_| ServiceError::invalid("event"))?;
        // Only the Cloud API flow registers a number: a coexistence one is
        // registered already, and FINISH_ONLY_WABA has none.
        if let (Some(FinishKind::Finish), Some(pin)) = (event.finish_kind(), pin) {
            request = request.register_with_pin(pin);
        }
        let tenant = self.tenant();
        let every = self.deployment.every_granted_waba;
        if every {
            request = request.every_granted_waba(true);
        }
        // D4 on the claimed id, and with the option on on every WABA the
        // event names, before the attempt is redeemed: a WABA another
        // tenant holds refuses the whole signup, with nothing bound.
        let claimed: Vec<&WabaId> = if every {
            request
                .session
                .waba_id
                .iter()
                .chain(request.session.waba_ids.iter())
                .collect()
        } else {
            request.session.primary_waba_id().into_iter().collect()
        };
        for claimed in claimed {
            if self
                .records()
                .waba(claimed)
                .await?
                .is_some_and(|binding| &binding.tenant_id != tenant)
            {
                return Err(ServiceError::new(ErrorCode::WABA_OWNED_BY_ANOTHER_TENANT));
            }
        }
        let redeemed = self
            .signups
            .sessions
            .redeem(&state, tenant.as_str())
            .await
            .map_err(|error| ServiceError::from_library_withholding_texts(&error))?;
        if !redeemed {
            return Err(ServiceError::new(ErrorCode::STALE_ATTEMPT));
        }
        if every {
            return Box::pin(self.complete_many(&request)).await;
        }
        // The code is spent from here on: never retried.
        let approval = OnceLock::new();
        let partner = self.deployment.is_solution_partner();
        let records = self.records();
        let set_approval = |a: Approval| {
            let _ = approval.set(a);
        };
        let approve = |verified: VerifiedOnboarding| {
            approve_binding(records, tenant, verified, partner, &set_approval)
        };
        let es = self.deployment.onboarding(self.authz.client());
        let onboarded = es
            .onboard_with_approval(&request, self.authz.vault_for_signup(), approve)
            .await;
        match onboarded {
            Ok(onboarded) => {
                let primary = onboarded.waba_id.clone();
                let completed = self.connected(onboarded, "signup_completed").await?;
                Ok(single(completed, &request, &primary))
            }
            Err(error) => Err(self.stopped(&error, approval.get(), &request).await),
        }
    }

    /// A completion with every granted WABA bound (decision D200, the
    /// deployment's option): the library's `onboard_wabas_with_approval`,
    /// each WABA approved (bound, D4) on its own; one that fails does not
    /// stop the next. The call fails when no WABA connected (as a single
    /// WABA's failure does); otherwise each WABA's outcome is in the answer.
    async fn complete_many(&self, request: &OnboardingRequest) -> Result<Completed, ServiceError> {
        let tenant = self.tenant();
        let partner = self.deployment.is_solution_partner();
        let records = self.records();
        let approvals = Approvals::default();
        let approvals_ref = &approvals;
        let approve = move |verified: VerifiedOnboarding| async move {
            let waba_id = verified.waba_id.clone();
            let set = |a: Approval| approvals_ref.set(&waba_id, a);
            approve_binding(records, tenant, verified, partner, &set).await
        };
        let es = self.deployment.onboarding(self.authz.client());
        let many = match es
            .onboard_wabas_with_approval(request, self.authz.vault_for_signup(), approve)
            .await
        {
            Ok(many) => many,
            Err(error) => return Err(self.stopped(&error, None, request).await),
        };
        let mut results = Vec::with_capacity(many.wabas.len());
        let mut first: Option<Connected> = None;
        let mut primary_failure: Option<(Error, Option<Approval>)> = None;
        for entry in many.wabas {
            let waba_id = entry.waba_id.clone();
            let mut result = WabaResult {
                waba_id: waba_id.clone(),
                primary: entry.primary,
                status: WabaStatus::Failed,
                phone_number_ids: Vec::new(),
                failed_step: None,
                resumable: false,
                code: None,
            };
            match entry.outcome {
                WabaOutcome::Onboarded(onboarded) => {
                    match self.connected(*onboarded, "signup_completed").await {
                        Ok(Completed::Connected(connected)) => {
                            result.status = WabaStatus::Connected;
                            result
                                .phone_number_ids
                                .clone_from(&connected.phone_number_ids);
                            first.get_or_insert(connected);
                        }
                        Ok(Completed::Cancelled { .. }) => {}
                        Err(error) => result.code = Some(error.code().to_owned()),
                    }
                }
                WabaOutcome::Held => result.status = WabaStatus::Held,
                WabaOutcome::Failed { error, .. } => {
                    match approvals.get(&waba_id) {
                        Some(Approval::Owned) => {
                            result.status = WabaStatus::Refused;
                            result.code =
                                Some(ErrorCode::WABA_OWNED_BY_ANOTHER_TENANT.as_str().to_owned());
                        }
                        other => {
                            let api = answered(&error, self.observer);
                            result.failed_step = step_of(&error).map(str::to_owned);
                            result.code = Some(api.code().to_owned());
                            if after_store(&error)
                                && let Some(Approval::Bound { epoch, .. }) = other
                            {
                                let mut session = SessionInfo::default();
                                session.waba_id = Some(waba_id.clone());
                                let mut own = OnboardingRequest::for_resume(session);
                                own.finish_kind = None;
                                result.resumable = self
                                    .keep(&epoch.waba_id, &error, &own, "a signup attempt")
                                    .await;
                            }
                        }
                    }
                    if entry.primary {
                        // Kept for the call's answer, when nothing connected.
                        primary_failure = Some((error, approvals.get(&waba_id)));
                    }
                }
                // A variant a later library adds: reported as failed.
                other => {
                    tracing::warn!("a WABA outcome this service does not know: {other:?}");
                }
            }
            results.push(result);
        }
        let Some(mut connected) = first else {
            if let Some((error, approval)) = primary_failure {
                return Err(self.stopped(&error, approval.as_ref(), request).await);
            }
            return Err(ServiceError::from(StorageError::Busy));
        };
        connected.wabas = results;
        Ok(Completed::Connected(connected))
    }

    /// The caller's tenant's kept attempts, in byte order of their WABA
    /// ids.
    ///
    /// # Errors
    ///
    /// The store's failure.
    pub async fn attempts(
        &self,
        page: &PageRequest,
    ) -> Result<Listing<SignupAttempt>, ServiceError> {
        Ok(self.records().signup_attempts(self.tenant(), page).await?)
    }

    /// Resume the caller's tenant's attempt for `waba_id`: the library's
    /// `resume_with_approval` from the stored token (`load_token`), its
    /// session info checked against what onboarding stored
    /// (`verify_assets`), the binding checked again (the approval, before
    /// any credit call: a WABA no longer the tenant's is `404
    /// nothing_to_resume`), then the steps after `store_token`, with the
    /// corrected PIN when one is given (a Cloud API onboarding's). Done:
    /// the attempt dropped, audited (`signup_resumed`).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `waba_id` or `pin`, before anything is
    /// read; `404 nothing_to_resume`: no attempt of the tenant's for that
    /// WABA (another tenant's included), the WABA no longer the tenant's,
    /// or its token gone; `502 onboarding_failed` (`step`, `resumable`),
    /// the attempt kept at its new step; the store's failure.
    pub async fn resume(&self, resumption: Resumption) -> Result<Completed, ServiceError> {
        let Resumption { waba_id, pin } = resumption;
        let waba_id: WabaId = typed_graph_id("waba_id", &waba_id)?;
        let pin = pin
            .map(TwoStepPin::new)
            .transpose()
            .map_err(|_| ServiceError::invalid("pin"))?;
        let tenant = self.tenant();
        let records = self.records();
        let nothing = || ServiceError::new(ErrorCode::NOTHING_TO_RESUME);
        let attempt = records
            .signup_attempt(tenant, &waba_id)
            .await?
            .ok_or_else(nothing)?;
        records
            .waba(&waba_id)
            .await?
            .filter(|binding| &binding.tenant_id == tenant)
            .ok_or_else(nothing)?;
        let session: SessionInfo =
            serde_json::from_value(attempt.session.clone()).map_err(|_| {
                tracing::warn!("a kept signup attempt's session info does not read back");
                ServiceError::internal()
            })?;
        let finish_kind = attempt.finish_kind.as_deref().and_then(finish_kind);
        let mut request = OnboardingRequest::for_resume(session);
        request.finish_kind = finish_kind;
        if let (Some(FinishKind::Finish), Some(pin)) = (finish_kind, pin) {
            request = request.register_with_pin(pin);
        }
        let approval = OnceLock::new();
        let partner = self.deployment.is_solution_partner();
        let approval_set = &approval;
        let approve = move |verified: VerifiedOnboarding| async move {
            if partner {
                remember_ledger(records, &verified).await?;
            }
            approve_held(records, tenant, verified, approval_set).await
        };
        let es = self.deployment.onboarding(self.authz.client());
        let resumed = es
            .resume_with_approval(&waba_id, &request, self.authz.vault_for_signup(), approve)
            .await;
        match resumed {
            Ok(onboarded) => self.connected(onboarded, "signup_resumed").await,
            Err(_) if matches!(approval.get(), Some(Approval::Owned | Approval::NoTenant)) => {
                Err(nothing())
            }
            // No token to resume from (deleted since): nothing is left.
            Err(Error::Step {
                step: steps::LOAD_TOKEN,
                source,
            }) if matches!(*source, Error::Validation(_)) => {
                records.delete_signup_attempt(tenant, &waba_id).await?;
                Err(nothing())
            }
            Err(error) => {
                let api = answered(&error, self.observer);
                let resumable = after_store(&error)
                    && self
                        .keep(&waba_id, &error, &request, "a resumed signup attempt")
                        .await;
                Err(api.as_onboarding_failed(resumable))
            }
        }
    }

    /// An onboarding that went through: the binding confirmed, a kept
    /// attempt dropped, the allocation recorded, audited as `action`.
    async fn connected(
        &self,
        onboarded: Onboarded,
        action: &'static str,
    ) -> Result<Completed, ServiceError> {
        let tenant = self.tenant();
        let records = self.records();
        let waba_id = onboarded.waba_id.clone();
        // Bound, stored, confirmed (the port rule of `crate::store`): moved
        // meanwhile, this onboarding's own token is taken back.
        let held = records
            .waba(&waba_id)
            .await?
            .filter(|binding| &binding.tenant_id == tenant);
        let Some(held) = held else {
            let vault = self.authz.vault_for_signup();
            let mut taken_back = false;
            if let Some((stored, version)) = vault.get_versioned(&waba_id).await?
                && stored.token.expose_secret() == onboarded.token.expose_secret()
            {
                taken_back = vault.delete_if_unchanged(&waba_id, version).await?;
            }
            tracing::warn!(
                waba_id = waba_id.as_str(),
                taken_back,
                "a WABA was unbound, maybe bound to another tenant, while it was being \
                 onboarded: the token this onboarding stored was taken back"
            );
            return Err(ServiceError::from(StorageError::Busy));
        };
        let connected = Connected::from(onboarded);
        // Its numbers are connected, for this binding's generation
        // (roadmap M2d), before a kept attempt is dropped: not recorded
        // (the outbox failing), `503` with the attempt kept, and its
        // resume records them (once: a resume binds no new generation). A
        // first completion that kept no attempt has nothing to resume: the
        // events are lost with its `503` (design D81).
        self.authz
            .record_number_event(&held.epoch(), NumberEvent::Connected)
            .await?;
        records.delete_signup_attempt(tenant, &waba_id).await?;
        record_allocation(records, tenant, &connected).await?;
        tenant_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(tenant.as_str()),
                waba_id: Some(waba_id.as_str()),
                ..Subject::default()
            },
        );
        Ok(Completed::Connected(connected))
    }

    /// The answer to a completion that stopped (`error`), after
    /// `approval`: D4's refusal, else `502 onboarding_failed` with the
    /// step, the attempt kept when the token was stored.
    async fn stopped(
        &self,
        error: &Error,
        approval: Option<&Approval>,
        request: &OnboardingRequest,
    ) -> ServiceError {
        match approval {
            Some(Approval::Owned) => {
                return ServiceError::new(ErrorCode::WABA_OWNED_BY_ANOTHER_TENANT);
            }
            Some(Approval::NoTenant) => return ServiceError::forbidden(),
            _ => {}
        }
        let api = answered(error, self.observer);
        if step_of(error).is_none() {
            // Refused before any request (the library's checks).
            return api;
        }
        if after_store(error) {
            let Some(Approval::Bound { epoch, .. }) = approval else {
                return api.as_onboarding_failed(false);
            };
            let kept = self
                .keep(&epoch.waba_id, error, request, "a signup attempt")
                .await;
            return api.as_onboarding_failed(kept);
        }
        // The store itself failed after the approval bound a WABA no one
        // held: the binding is taken back, so it is nobody's without its
        // token. (A binding the tenant had already is left: it was
        // refreshed, not made.)
        if step_of(error) == Some(steps::STORE_TOKEN)
            && let Some(Approval::Bound { epoch, fresh: true }) = approval
        {
            match self.records().unbind_waba_if(epoch).await {
                Ok(_) => {}
                Err(storage) => tracing::warn!(
                    error = %storage,
                    "could not take back the binding of a WABA whose token was not stored"
                ),
            }
        }
        api.as_onboarding_failed(false)
    }

    /// Keep the attempt for `waba_id` at the step `error` stopped at:
    /// whether it was kept (the WABA still the tenant's).
    async fn keep(
        &self,
        waba_id: &WabaId,
        error: &Error,
        request: &OnboardingRequest,
        what: &'static str,
    ) -> bool {
        let attempt = NewSignupAttempt {
            tenant_id: self.tenant().clone(),
            waba_id: waba_id.clone(),
            failed_step: step_of(error).unwrap_or_default().to_owned(),
            session: serde_json::to_value(&request.session).unwrap_or_default(),
            finish_kind: request.finish_kind.map(|kind| kind.as_str().to_owned()),
        };
        match self.records().save_signup_attempt(&attempt).await {
            Ok(kept) => kept,
            Err(storage) => {
                tracing::warn!(error = %storage, "{what} could not be kept");
                false
            }
        }
    }
}

/// The approval of a completion: in Solution Partner mode the WABA's
/// ledger remembered for a key rotation first, then the WABA and its
/// numbers, as Meta verified them, bound to `tenant` (decision D4,
/// atomically), and what it did set in `approval`.
async fn approve_binding(
    records: &dyn RecordStore,
    tenant: &TenantId,
    verified: VerifiedOnboarding,
    partner: bool,
    set: &(dyn Fn(Approval) + Sync),
) -> Result<(), Error> {
    if partner {
        remember_ledger(records, &verified).await?;
    }
    let fresh = records.waba(&verified.waba_id).await?.is_none();
    match records
        .bind_waba(tenant, &verified.waba_id, &verified.phone_number_ids)
        .await?
    {
        BindOutcome::Bound => {}
        BindOutcome::OwnedByAnotherTenant => {
            set(Approval::Owned);
            return Err(ValidationError::new("waba_id", "bound to another tenant").into());
        }
        BindOutcome::NoSuchTenant => {
            set(Approval::NoTenant);
            return Err(ValidationError::new("tenant", "no such tenant").into());
        }
    }
    let binding = records
        .waba(&verified.waba_id)
        .await?
        .filter(|binding| &binding.tenant_id == tenant)
        .ok_or(StorageError::Busy)?;
    set(Approval::Bound {
        epoch: binding.epoch(),
        fresh,
    });
    Ok(())
}

/// The approval of a resumption: the WABA still `tenant`'s, read now,
/// before any credit call (decision D4).
async fn approve_held(
    records: &dyn RecordStore,
    tenant: &TenantId,
    verified: VerifiedOnboarding,
    approval: &OnceLock<Approval>,
) -> Result<(), Error> {
    match records.waba(&verified.waba_id).await? {
        Some(binding) if &binding.tenant_id == tenant => {
            let _ = approval.set(Approval::Bound {
                epoch: binding.epoch(),
                fresh: false,
            });
            Ok(())
        }
        _ => {
            let _ = approval.set(Approval::Owned);
            Err(ValidationError::new("waba_id", "no longer the tenant's").into())
        }
    }
}

/// Remember that the vault may keep `verified`'s credit ledger, and its
/// business's revocation marker, past the WABA's binding: what a key
/// rotation walks besides the bindings.
async fn remember_ledger(
    records: &dyn RecordStore,
    verified: &VerifiedOnboarding,
) -> Result<(), StorageError> {
    records
        .record_vault_entry(&VaultEntry::Waba(verified.waba_id.clone()))
        .await?;
    if let Some(business) = &verified.business_id {
        records
            .record_vault_entry(&VaultEntry::Business(business.clone()))
            .await?;
    }
    Ok(())
}

/// Record `connected`'s credit allocation on its binding, if it is still
/// `tenant`'s (the vault's ledger records it too).
async fn record_allocation(
    records: &dyn RecordStore,
    tenant: &TenantId,
    connected: &Connected,
) -> Result<(), StorageError> {
    let Some(allocation) = &connected.credit_allocation_id else {
        return Ok(());
    };
    if let Some(binding) = records
        .waba(&connected.waba_id)
        .await?
        .filter(|binding| &binding.tenant_id == tenant)
    {
        records
            .set_credit_allocation_if(&binding.epoch(), allocation)
            .await?;
    }
    Ok(())
}

/// The step a library error stopped at.
fn step_of(error: &Error) -> Option<&'static str> {
    match error {
        Error::Step { step, .. } => Some(step),
        _ => None,
    }
}

/// Whether `error` stopped a step after `store_token`: the token is in the
/// vault, and a resume can finish the job (subscribing, the system user,
/// the credit line, the registration, a coexistence sync).
fn after_store(error: &Error) -> bool {
    step_of(error).is_some_and(|step| {
        !matches!(
            step,
            steps::EXCHANGE_CODE
                | steps::FETCH_TOKEN
                | steps::DEBUG_TOKEN
                | steps::VERIFY_ASSETS
                | steps::APPROVE
                | steps::STORE_TOKEN
                | steps::LOAD_TOKEN
        )
    })
}

/// A library error, as the signup routes answer it: its code (Meta's text
/// in no log, `details` never), told to `observer` when Meta answered.
fn answered(error: &Error, observer: &dyn Observer) -> ServiceError {
    let api = ServiceError::from_library_withholding_texts(error);
    if error.graph().is_some() {
        observer.graph_error(api.code());
    }
    api
}

/// The completion an attempt kept (`FinishKind::as_str`).
fn finish_kind(stored: &str) -> Option<FinishKind> {
    [
        FinishKind::Finish,
        FinishKind::OnlyWaba,
        FinishKind::WhatsappBusinessAppOnboarding,
        FinishKind::OboMigration,
        FinishKind::GrantOnlyApiAccess,
    ]
    .into_iter()
    .find(|kind| kind.as_str() == stored)
}

/// What the service does about `account_update` webhooks in an Embedded
/// Signup deployment (roadmap M3a; design sections 3.4 and 6, decision
/// D14), the webhook pipeline's [`AccountActions`]:
///
/// - `PARTNER_APP_UNINSTALLED` of the service's own app
///   (`waba_info.partner_app_id` is `WA_APP_ID`; another partner's app is
///   not this deployment's) and `ACCOUNT_DELETED`: offboarded. In
///   Solution Partner mode the credit line is revoked first (from the
///   owner business the vault recorded, else the webhook's
///   `owner_business_id`), and the token deleted second (the library's
///   `EmbeddedSignup::offboard`), then the bindings; a failed revocation
///   deletes nothing. Before any of it, once confirmed, the bound numbers'
///   `number_disconnected` (`reason`: `partner_app_uninstalled`,
///   `account_deleted`; roadmap M2d, [`crate::number_events`]): not
///   recorded, nothing is revoked or deleted.
/// - `PARTNER_REMOVED`, in Solution Partner mode: the credit line revoked
///   at once (the library's `revoke_credit_line`, or
///   `revoke_business_credit_line` when it names no WABA); the token and
///   the bindings stay (the merchant's next calls answer
///   `reconnect_required`). A removal from a Multi-Partner Solution
///   (`solution_partner_business_ids` listed) is not acted on: nothing
///   tells whether this deployment's business is the partner removed.
///
/// Each is confirmed with Graph first ([`AccountUpdateConfirmation`]), and
/// audited (target `audit`, `origin` `webhook`). Every step is safe to
/// repeat in any order: the revocation is the library's (the ledger
/// outlives the token), a deleted token deletes nothing again.
pub struct AccountUpdates {
    authz: Arc<Authorizer>,
    deployment: SignupDeployment,
    hosted: JsonStore<hosted::PendingHosted>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for AccountUpdates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountUpdates")
            .field("deployment", &self.deployment)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl AccountActions for AccountUpdates {
    async fn on_event(&self, event: &WebhookEvent) -> Result<(), Error> {
        let WebhookEvent::AccountUpdated {
            waba_id, update, ..
        } = event
        else {
            return Ok(());
        };
        let info = update.waba_info.as_ref();
        let owner = info.and_then(|info| info.owner_business_id.as_ref());
        match &update.event {
            AccountUpdateEvent::PartnerAppUninstalled => {
                let ours = info.and_then(|info| info.partner_app_id.as_ref())
                    == Some(self.deployment.app_id());
                match waba_id {
                    Some(waba_id) if ours => {
                        self.offboard(waba_id, owner, DisconnectReason::PartnerAppUninstalled)
                            .await
                    }
                    _ => Ok(()),
                }
            }
            AccountUpdateEvent::PartnerAdded if self.deployment.hosted.is_some() => {
                match (waba_id, owner) {
                    (Some(waba_id), Some(owner)) => self.remember_arrival(waba_id, owner).await,
                    _ => Ok(()),
                }
            }
            AccountUpdateEvent::AccountDeleted => match waba_id {
                Some(waba_id) => {
                    self.offboard(waba_id, owner, DisconnectReason::AccountDeleted)
                        .await
                }
                None => Ok(()),
            },
            AccountUpdateEvent::PartnerRemoved if self.deployment.is_solution_partner() => {
                if info.is_some_and(|info| !info.solution_partner_business_ids.is_empty()) {
                    tracing::warn!(
                        target: "audit",
                        origin = "webhook",
                        waba_id = waba_id.as_ref().map(WabaId::as_str),
                        "a PARTNER_REMOVED of a Multi-Partner Solution was not acted on: \
                         revoke the line by hand if the partner removed is this deployment"
                    );
                    return Ok(());
                }
                match (waba_id, owner) {
                    (Some(waba_id), _) => self.revoke(waba_id, owner).await,
                    (None, Some(owner)) => self.revoke_business(owner).await,
                    (None, None) => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }
}

impl AccountUpdates {
    /// Remember a hosted signup's `PARTNER_ADDED` (decision D201) for a
    /// tenant to claim: only a WABA no tenant holds and the vault does not
    /// hold (Meta sends it for every WABA shared with the app, the code
    /// flow's too). Nothing is fetched or bound here; a redelivery leaves
    /// the first arrival as it is.
    async fn remember_arrival(&self, waba_id: &WabaId, owner: &BusinessId) -> Result<(), Error> {
        // A record the vault cannot read counts as held.
        if self.authz.records().waba(waba_id).await?.is_some()
            || !matches!(self.authz.vault_for_signup().get(waba_id).await, Ok(None))
        {
            return Ok(());
        }
        let pending = hosted::PendingHosted {
            owner_business_id: owner.as_str().to_owned(),
            received_at: self.clock.now().unix_timestamp(),
        };
        let stored = self
            .hosted
            .put_if_absent(
                waba_id.as_str(),
                &pending,
                meta_whatsapp_rs::core::store::Expiry::After(hosted::PENDING_TTL),
            )
            .await?;
        if stored.is_some() {
            tracing::info!(
                target: "audit",
                action = "hosted_signup_arrived",
                origin = "webhook",
                waba_id = waba_id.as_str(),
                "webhook change"
            );
        }
        Ok(())
    }

    /// Whether Graph confirms that `waba_id` left: its stored token
    /// (`stored`, as read) is no longer valid, or no longer granted it
    /// (`debug_token`). No token stored: nothing to confirm with, `true`.
    async fn confirmed(
        &self,
        es: &EmbeddedSignup,
        waba_id: &WabaId,
        stored: Option<&StoredBusinessToken>,
    ) -> Result<bool, Error> {
        if self.deployment.confirmation == AccountUpdateConfirmation::Signature {
            return Ok(true);
        }
        let Some(stored) = stored else {
            return Ok(true);
        };
        let debug = es.debug_token(&stored.token).await?;
        let still_granted = debug.is_valid && debug.waba_ids().contains(waba_id);
        if still_granted {
            tracing::warn!(
                target: "audit",
                origin = "webhook",
                waba_id = waba_id.as_str(),
                "an account update Meta's own answer contradicts was not acted on: the WABA's \
                 token is still valid and still granted it"
            );
        }
        Ok(!still_granted)
    }

    /// The WABA's binding and stored token, read together: the token read
    /// while the binding held (read again after it), as a capability is
    /// made. A WABA that moved between the reads is `StorageError::Busy`
    /// (retryable: Meta's redelivery reads them anew).
    async fn held(
        &self,
        waba_id: &WabaId,
    ) -> Result<
        (
            Option<WabaBinding>,
            Option<(StoredBusinessToken, TokenVersion)>,
        ),
        Error,
    > {
        let records = self.authz.records();
        let binding = records.waba(waba_id).await?;
        let token = self.authz.vault_for_signup().get_versioned(waba_id).await?;
        let again = records.waba(waba_id).await?;
        if binding.as_ref().map(WabaBinding::epoch) != again.as_ref().map(WabaBinding::epoch) {
            return Err(StorageError::Busy.into());
        }
        Ok((binding, token))
    }

    /// Offboard `waba_id` (see the type's docs): only the token and the
    /// binding read before the revocation's calls, so a WABA moved to
    /// another holder meanwhile loses nothing of the new holder's.
    async fn offboard(
        &self,
        waba_id: &WabaId,
        owner: Option<&BusinessId>,
        why: DisconnectReason,
    ) -> Result<(), Error> {
        let reason = why.as_str();
        let es = self.deployment.onboarding(self.authz.client());
        let (binding, token) = self.held(waba_id).await?;
        if !self
            .confirmed(&es, waba_id, token.as_ref().map(|(stored, _)| stored))
            .await?
        {
            return Ok(());
        }
        let records = self.authz.records();
        if self.deployment.is_solution_partner() {
            remember(records, waba_id, owner).await?;
        }
        // Its numbers' `number_disconnected` (roadmap M2d), once Graph
        // confirmed and before anything is revoked or deleted, as a
        // tenant's disconnect records them once Meta unsubscribed: while
        // the binding read holds (after the unbinding, the outbox would
        // keep the rows operator-only), and so that an outbox failing
        // leaves the merchant whole (`Busy`: Meta's redelivery records
        // them, each once; any other failure is the operator's) rather
        // than its token deleted and its binding kept.
        if let Some(binding) = &binding {
            self.authz
                .record_number_event(&binding.epoch(), NumberEvent::Disconnected(why))
                .await?;
        }
        let version = token.map(|(_, version)| version);
        let offboarded = es
            .offboard_if_unchanged(waba_id, owner, self.authz.vault_for_signup(), version)
            .await?;
        let still_held = || async {
            Ok::<_, Error>(
                records.waba(waba_id).await?.map(|now| now.epoch())
                    == binding.as_ref().map(WabaBinding::epoch),
            )
        };
        if version.is_some() && !offboarded.token_deleted {
            if still_held().await? {
                // Re-encrypted by a rotation, or stored again by the same
                // holder: Meta's redelivery reads it anew.
                return Err(StorageError::Busy.into());
            }
            tracing::warn!(
                target: "audit",
                origin = "webhook",
                reason,
                waba_id = waba_id.as_str(),
                credit_revoked = offboarded.credit.is_some(),
                "a WABA moved to another holder while it was being offboarded: the new \
                 holder's token and binding were left"
            );
            return Ok(());
        }
        let unbound = match &binding {
            Some(binding) => records.unbind_waba_if(&binding.epoch()).await?,
            None => false,
        };
        tracing::info!(
            target: "audit",
            action = "waba_offboarded",
            origin = "webhook",
            reason,
            tenant = binding.as_ref().map(|binding| binding.tenant_id.as_str()),
            waba_id = waba_id.as_str(),
            credit_revoked = offboarded.credit.is_some(),
            token_deleted = offboarded.token_deleted,
            unbound,
            "webhook change"
        );
        Ok(())
    }

    /// Revoke the credit line of `waba_id`'s business (`PARTNER_REMOVED`).
    async fn revoke(&self, waba_id: &WabaId, owner: Option<&BusinessId>) -> Result<(), Error> {
        let es = self.deployment.onboarding(self.authz.client());
        let stored = self.authz.vault_for_signup().get(waba_id).await?;
        if !self.confirmed(&es, waba_id, stored.as_ref()).await? {
            return Ok(());
        }
        let records = self.authz.records();
        remember(records, waba_id, owner).await?;
        match es
            .revoke_credit_line(waba_id, owner, self.authz.vault_for_signup())
            .await
        {
            Ok(_) => {}
            // Nothing recorded for the WABA and no owner business named:
            // there is nothing of this deployment's to revoke.
            Err(Error::Validation(refused)) if refused.field == "business_id" => return Ok(()),
            Err(error) => return Err(error),
        }
        tracing::info!(
            target: "audit",
            action = "credit_line_revoked",
            origin = "webhook",
            waba_id = waba_id.as_str(),
            "webhook change"
        );
        Ok(())
    }

    /// Revoke the credit line of `owner` (`PARTNER_REMOVED` naming no
    /// WABA): the library marks it only if the line has records for it.
    async fn revoke_business(&self, owner: &BusinessId) -> Result<(), Error> {
        let records = self.authz.records();
        records
            .record_vault_entry(&VaultEntry::Business(owner.clone()))
            .await?;
        let es = self.deployment.onboarding(self.authz.client());
        es.revoke_business_credit_line(owner, self.authz.vault_for_signup())
            .await?;
        tracing::info!(
            target: "audit",
            action = "credit_line_revoked",
            origin = "webhook",
            business_id = owner.as_str(),
            "webhook change"
        );
        Ok(())
    }
}

/// Remember the WABA's ledger and the owner's marker for a key rotation.
async fn remember(
    records: &dyn RecordStore,
    waba_id: &WabaId,
    owner: Option<&BusinessId>,
) -> Result<(), StorageError> {
    records
        .record_vault_entry(&VaultEntry::Waba(waba_id.clone()))
        .await?;
    if let Some(owner) = owner {
        records
            .record_vault_entry(&VaultEntry::Business(owner.clone()))
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The design's 15 minutes; each kept completion reads back; the steps
    /// a resume can finish are exactly those after `store_token`.
    /// Decisive: the TTL, a step moved across the line.
    #[test]
    fn the_attempts_numbers_and_steps_are_the_designs() {
        assert_eq!(ATTEMPT_TTL.as_secs(), 900);
        for kind in [
            FinishKind::Finish,
            FinishKind::OnlyWaba,
            FinishKind::WhatsappBusinessAppOnboarding,
            FinishKind::OboMigration,
            FinishKind::GrantOnlyApiAccess,
        ] {
            assert_eq!(finish_kind(kind.as_str()), Some(kind));
        }
        assert_eq!(finish_kind("CANCEL"), None);
        let at =
            |step: &'static str| after_store(&Error::Storage(StorageError::Busy).in_step(step));
        for step in [
            steps::SUBSCRIBE_APP,
            steps::ASSIGN_SYSTEM_USER,
            steps::SHARE_CREDIT_LINE,
            steps::REGISTER_PHONE,
            steps::SYNC_CONTACTS,
            steps::SYNC_HISTORY,
        ] {
            assert!(at(step), "{step}");
        }
        for step in [
            steps::EXCHANGE_CODE,
            steps::DEBUG_TOKEN,
            steps::VERIFY_ASSETS,
            steps::APPROVE,
            steps::STORE_TOKEN,
            steps::LOAD_TOKEN,
        ] {
            assert!(!at(step), "{step}");
        }
        assert!(!after_store(&Error::Storage(StorageError::Busy)));
    }

    /// A deployment's `Debug` shows neither the app secret nor the
    /// partner's system token.
    #[test]
    fn a_deployments_debug_shows_no_secret() {
        let app = AppCredentials::new(AppId::new("236484624622562").unwrap(), "s3cr3t-app-secret");
        let partner = SolutionPartner::new(
            meta_whatsapp_rs::core::secret::AccessToken::new("SYSTEM-TOKEN-SENTINEL"),
            "1972555232742222".to_owned(),
            "1972385232742146".to_owned(),
        );
        let deployment = SignupDeployment::new(app, "<CONFIGURATION_ID>").solution_partner(partner);
        let shown = format!("{deployment:?}");
        assert!(!shown.contains("s3cr3t-app-secret"), "{shown}");
        assert!(!shown.contains("SYSTEM-TOKEN-SENTINEL"), "{shown}");
        assert!(shown.contains("solution_partner: true"), "{shown}");
    }
}
