//! `EmbeddedSignup::onboard`: from the code and session info to a verified,
//! stored, subscribed and (optionally) registered business. The step order
//! and why it is what it is are in the [module docs](super).

use std::future::{Future, Ready};
use std::pin::pin;

use futures::StreamExt;
use meta_whatsapp_core::error::{CreditError, ValidationError};
use meta_whatsapp_core::ids::{AllocationConfigId, AppId, BusinessId, PhoneNumberId, WabaId};
use meta_whatsapp_core::secret::AccessToken;
use meta_whatsapp_core::{Error, Result};
use time::OffsetDateTime;

use super::EmbeddedSignup;
use super::event::{EmbeddedSignupEvent, FinishKind, SessionInfo};
use super::partner::{CreditPlan, CreditSharing, assign_system_user, share_credit_line};
use super::token::{SignupCode, TokenDebug, WHATSAPP_BUSINESS_MANAGEMENT};
use super::vault::{StoredBusinessToken, TokenVault};
use crate::Client;
use crate::credit_lines::WabaCurrency;
use crate::phone_numbers::{DataLocalizationRegion, TwoStepPin};
use crate::waba::{CallbackOverride, PhoneNumbersQuery, Waba};

/// Stable step names used in [`meta_whatsapp_core::Error::Step`] by
/// [`EmbeddedSignup::onboard`], [`EmbeddedSignup::resume`] and
/// [`EmbeddedSignup::offboard`]. From `exchange_code` to `register_phone`
/// they are the ones `docs/architecture.md` lists for `onboard`, in order;
/// `approve` runs only in [`EmbeddedSignup::onboard_with_approval`], and
/// `assign_system_user` and `share_credit_line` only in
/// [Solution Partner mode](super#solution-partner-mode).
pub mod steps {
    /// `GET oauth/access_token`.
    pub const EXCHANGE_CODE: &str = "exchange_code";
    /// `GET debug_token`.
    pub const DEBUG_TOKEN: &str = "debug_token";
    /// Checking the browser's claims against Meta: the WABA among the
    /// token's grants, its owner business, the phone number among the WABA's
    /// numbers. In [`EmbeddedSignup::resume`](super::EmbeddedSignup::resume),
    /// checking the request against what onboarding verified.
    pub const VERIFY_ASSETS: &str = "verify_assets";
    /// Your approval of what `verify_assets` established, before anything
    /// is stored, subscribed or shared
    /// ([`EmbeddedSignup::onboard_with_approval`](super::EmbeddedSignup::onboard_with_approval),
    /// [`EmbeddedSignup::resume_with_approval`](super::EmbeddedSignup::resume_with_approval)).
    /// In Solution Partner mode it also records the approval in the credit
    /// ledger, and [`EmbeddedSignup::resume`](super::EmbeddedSignup::resume)
    /// fails here when the ledger has none.
    pub const APPROVE: &str = "approve";
    /// Writing the token to the vault.
    pub const STORE_TOKEN: &str = "store_token";
    /// `POST /{WABA_ID}/subscribed_apps`.
    pub const SUBSCRIBE_APP: &str = "subscribe_app";
    /// `POST /{WABA_ID}/assigned_users` with the partner's system user token:
    /// the system user on the customer's WABA, the prerequisite of
    /// [`CreditSharing::ShareAndAttach`](super::CreditSharing::ShareAndAttach).
    pub const ASSIGN_SYSTEM_USER: &str = "assign_system_user";
    /// Checking whether the partner's credit line already funds the WABA
    /// (or was revoked from its business), sharing it if not, and recording
    /// the allocation in the vault's credit ledger.
    pub const SHARE_CREDIT_LINE: &str = "share_credit_line";
    /// `POST /{PHONE_NUMBER_ID}/register`.
    pub const REGISTER_PHONE: &str = "register_phone";
    /// Reading the token back from the vault ([`EmbeddedSignup::resume`](super::EmbeddedSignup::resume)
    /// only).
    pub const LOAD_TOKEN: &str = "load_token";
    /// Revoking the partner's credit line ([`EmbeddedSignup::offboard`](super::EmbeddedSignup::offboard),
    /// Solution Partner mode).
    pub const REVOKE_CREDIT_LINE: &str = "revoke_credit_line";
    /// Deleting the token and its phone index ([`EmbeddedSignup::offboard`](super::EmbeddedSignup::offboard)).
    pub const DELETE_TOKEN: &str = "delete_token";
}

use steps::{
    APPROVE, ASSIGN_SYSTEM_USER, DEBUG_TOKEN, EXCHANGE_CODE, LOAD_TOKEN, REGISTER_PHONE,
    SHARE_CREDIT_LINE, STORE_TOKEN, SUBSCRIBE_APP, VERIFY_ASSETS,
};

/// Most phone numbers read from one WABA while verifying. Not a Meta limit:
/// it stops a pagination that never ends (cycling cursors) from looping for
/// ever; a WABA with more numbers fails closed.
const MAX_WABA_PHONE_NUMBERS: usize = 1_000;
/// Page size for that read (the edge's maximum).
const PHONE_PAGE_SIZE: u32 = 100;

/// What to onboard, and how.
///
/// `Debug` never shows the code or the PIN.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OnboardingRequest {
    /// The exchangeable code from `FB.login`.
    pub code: SignupCode,
    /// Asset ids from the `FINISH*` message event. Claims: the WABA and phone
    /// number are verified during onboarding; `business_id` is not used (the
    /// WABA's owner is read from Meta instead).
    pub session: SessionInfo,
    /// Which completion the event reported, when known.
    pub finish_kind: Option<FinishKind>,
    /// Register the phone number for Cloud API. Not for coexistence
    /// numbers, which are already registered.
    pub register: bool,
    /// Two-step verification PIN for `register`.
    pub pin: Option<TwoStepPin>,
    /// Local storage region to register with.
    pub data_localization_region: Option<DataLocalizationRegion>,
    /// Send this WABA's supported webhooks to another callback.
    pub subscribe_override: Option<CallbackOverride>,
    /// The customer's currency for the credit line (Solution Partner mode
    /// only; falls back to [`SolutionPartner::default_currency`](super::SolutionPartner::default_currency)).
    pub currency: Option<WabaCurrency>,
    /// Fund the customer's business with the credit line even though it was
    /// revoked from it (Solution Partner mode only; see
    /// [`Self::reshare_after_revocation`]).
    pub reshare_after_revocation: bool,
}

impl OnboardingRequest {
    /// Onboard the assets in `session` with `code`, without registering the
    /// number.
    pub fn new(code: SignupCode, session: SessionInfo) -> Self {
        Self {
            code,
            session,
            finish_kind: None,
            register: false,
            pin: None,
            data_localization_region: None,
            subscribe_override: None,
            currency: None,
            reshare_after_revocation: false,
        }
    }

    /// From the parsed message event. Fails unless the flow finished.
    pub fn from_event(code: SignupCode, event: &EmbeddedSignupEvent) -> Result<Self> {
        match event {
            EmbeddedSignupEvent::Finish { kind, session } => Ok(Self {
                finish_kind: Some(*kind),
                ..Self::new(code, session.clone())
            }),
            _ => Err(ValidationError::new(
                "event",
                "the Embedded Signup flow did not finish (cancelled, errored or unknown event)",
            )
            .into()),
        }
    }

    /// Register the phone number with `pin` (becomes its two-step
    /// verification PIN, or must match the existing one).
    #[must_use]
    pub fn register_with_pin(mut self, pin: TwoStepPin) -> Self {
        self.register = true;
        self.pin = Some(pin);
        self
    }

    /// Register with local storage in `region`.
    #[must_use]
    pub fn data_localization_region(mut self, region: DataLocalizationRegion) -> Self {
        self.data_localization_region = Some(region);
        self
    }

    /// Subscribe with a WABA-level callback override.
    #[must_use]
    pub fn subscribe_override(mut self, callback: CallbackOverride) -> Self {
        self.subscribe_override = Some(callback);
        self
    }

    /// Invoice this customer's WABA in `currency` (Solution Partner mode
    /// only: a Tech Provider onboarding naming one is refused). Take it from
    /// your billing records for the merchant, never from the browser: it
    /// sets the prices Meta charges you, and a credit line cannot be changed
    /// once attached. The first currency is sealed in the vault before the
    /// first share is posted; a later onboarding or `resume` of the WABA
    /// naming another is refused.
    #[must_use]
    pub fn currency(mut self, currency: WabaCurrency) -> Self {
        self.currency = Some(currency);
        self
    }

    /// Share the credit line with the customer's business **even though it
    /// was revoked from it** (Solution Partner mode only). Without this,
    /// `share_credit_line` refuses a business marked revoked by
    /// [`EmbeddedSignup::revoke_credit_line`](super::EmbeddedSignup::revoke_credit_line),
    /// or whose only records Meta reports `DELETED`
    /// ([`CreditError::Revoked`]), or with a record whose `request_status`
    /// Meta does not document ([`CreditError::StatusUnknown`]): whether to
    /// fund a merchant again after a revocation is your product decision,
    /// taken on one onboarding but **business-wide in effect**: a
    /// successful re-share clears the business's marker (unless a
    /// revocation touched it meanwhile; then the new share is revoked at
    /// once), so its other WABAs are no longer refused either. Never set
    /// it on every onboarding: gate it behind a decision of yours (the
    /// `meta-whatsapp-rs-embedded-signup` skill's example spends a one-time reconnect
    /// grant).
    #[must_use]
    pub fn reshare_after_revocation(mut self) -> Self {
        self.reshare_after_revocation = true;
        self
    }

    fn validate(&self) -> Result<(), ValidationError> {
        if self.register && self.pin.is_none() {
            return Err(ValidationError::new(
                "pin",
                "required to register the number",
            ));
        }
        if self.register && self.is_coexistence() {
            return Err(ValidationError::new(
                "register",
                "numbers onboarded with the WhatsApp Business app are already registered",
            ));
        }
        if let Some(region) = &self.data_localization_region {
            if !self.register {
                return Err(ValidationError::new(
                    "data_localization_region",
                    "only applies when registering the number",
                ));
            }
            region.validate()?;
        }
        if let Some(o) = &self.subscribe_override {
            o.validate()?;
        }
        Ok(())
    }

    fn is_coexistence(&self) -> bool {
        self.finish_kind == Some(FinishKind::WhatsappBusinessAppOnboarding)
    }
}

/// The result of a successful onboarding.
///
/// `Debug` never shows the token.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Onboarded {
    /// The verified WABA.
    pub waba_id: WabaId,
    /// The verified (or, when the session info named none, the WABA's most
    /// recently onboarded) phone number: the one registered. `None` when no
    /// number was claimed and none was needed.
    pub phone_number_id: Option<PhoneNumberId>,
    /// Every number of the WABA, as indexed in the vault for
    /// [`TokenVault::get_by_phone_number`]; `phone_number_id` first.
    pub phone_number_ids: Vec<PhoneNumberId>,
    /// The WABA's owner business portfolio, as Meta reports it
    /// (`owner_business_info`). The session info's `business_id` is never
    /// used: it comes from the browser.
    pub business_id: Option<BusinessId>,
    /// Which completion the flow reported, when known.
    pub finish_kind: Option<FinishKind>,
    /// The business token (also in the vault).
    pub token: AccessToken,
    /// When the token expires, if it does.
    pub token_expires_at: Option<OffsetDateTime>,
    /// The partner's credit line allocation funding the WABA (Solution
    /// Partner mode; also in the vault's credit ledger,
    /// [`TokenVault::credit`]). `None` for a Tech Provider.
    pub allocation_config_id: Option<AllocationConfigId>,
    /// Steps that ran, in order (see [`steps`]).
    pub steps_completed: Vec<&'static str>,
}

impl Onboarded {
    /// Whether this was a coexistence onboarding, which still needs the
    /// `smb_app_data` syncs within 24 hours.
    pub fn needs_coexistence_sync(&self) -> bool {
        self.finish_kind == Some(FinishKind::WhatsappBusinessAppOnboarding)
    }
}

/// What `verify_assets` established with Meta.
struct VerifiedAssets {
    waba_id: WabaId,
    business_id: Option<BusinessId>,
    phone_number_id: Option<PhoneNumberId>,
    phone_number_ids: Vec<PhoneNumberId>,
}

/// What `verify_assets` established with Meta, handed to the approval of
/// [`EmbeddedSignup::onboard_with_approval`] before anything is stored,
/// subscribed or shared. Every id here was checked with Meta; none comes
/// from the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VerifiedOnboarding {
    /// The WABA the token was granted.
    pub waba_id: WabaId,
    /// Its owner business portfolio (`owner_business_info`), when Meta
    /// reports one: the business a Solution Partner's credit line would be
    /// shared with.
    pub business_id: Option<BusinessId>,
    /// The number to register, when there is one.
    pub phone_number_id: Option<PhoneNumberId>,
    /// Every number of the WABA; `phone_number_id` first.
    pub phone_number_ids: Vec<PhoneNumberId>,
}

impl EmbeddedSignup {
    /// Onboard a business that completed Embedded Signup: exchange the code,
    /// inspect the token, verify the WABA, owner and phone number with Meta,
    /// store the token in `vault`, subscribe the app, and register the number
    /// if requested. **Tech Provider mode only**: in Solution Partner mode
    /// this refuses before any request ([`CreditError::ApprovalRequired`]);
    /// use [`Self::onboard_with_approval`].
    ///
    /// Input errors are reported before any request. Every later failure is
    /// an [`Error::Step`] naming the step; see the [module docs](super) for
    /// what each step does and which can be repeated. **Do not retry a
    /// failed `onboard`**: the code is spent. After `store_token` succeeded,
    /// use [`Self::resume`].
    ///
    /// This does not know which of *your* tenants is asking: redeem the
    /// [`SignupState`](super::SignupState) for the calling tenant first
    /// ([`SignupSessions::redeem`](super::SignupSessions::redeem)), then
    /// record [`Onboarded::waba_id`] against that tenant.
    pub async fn onboard(
        &self,
        request: &OnboardingRequest,
        vault: &TokenVault,
    ) -> Result<Onboarded> {
        self.onboard_inner(
            request,
            vault,
            None::<fn(VerifiedOnboarding) -> Ready<Result<()>>>,
        )
        .await
    }

    /// [`Self::onboard`], with your approval of the verified assets before
    /// anything is stored, subscribed or shared (step `approve`). **Required
    /// in Solution Partner mode.**
    ///
    /// `approve` runs right after `verify_assets`, with the WABA, owner
    /// business and numbers Meta confirmed. Refuse (return an error) when
    /// the WABA is already bound to another of your tenants, the business
    /// is one you will not onboard, and so on: `onboard` then fails with an
    /// [`Error::Step`] `approve` wrapping your error, and **nothing** is
    /// written to the vault, no app is subscribed, no system user added and
    /// no credit line shared. The code is spent all the same (verifying
    /// needs the token). An approval that runs after `onboard` instead would
    /// come too late: by then a Solution Partner's credit line is attached,
    /// and an attached line cannot be taken back from the WABA.
    ///
    /// meta-whatsapp-rs decides no policy here: which of your tenants may onboard a
    /// WABA is yours (`OPEN_QUESTIONS.md` #6). In Solution Partner mode the
    /// approval is recorded in the vault's credit ledger
    /// ([`StoredCredit::approved_at`](super::StoredCredit::approved_at)), and
    /// [`Self::resume`] shares a line only for a WABA approved so.
    ///
    /// Reserve the WABA for the tenant **atomically** (an insert that fails
    /// when the WABA is taken, e.g. `put_if_absent` or a unique key): two
    /// tenants onboarding the same WABA at once would both pass a lookup.
    ///
    /// ```no_run
    /// # async fn demo(es: meta_whatsapp_client::embedded_signup::EmbeddedSignup,
    /// #     request: meta_whatsapp_client::embedded_signup::OnboardingRequest,
    /// #     vault: meta_whatsapp_client::embedded_signup::TokenVault,
    /// #     reservations: std::sync::Arc<dyn meta_whatsapp_core::store::KvStore>) -> meta_whatsapp_core::Result<()> {
    /// use meta_whatsapp_core::error::ValidationError;
    /// use meta_whatsapp_core::store::{Expiry, StoreKey};
    ///
    /// let tenant = "merchant-42";
    /// let done = es
    ///     .onboard_with_approval(&request, &vault, |verified| async move {
    ///         // Your WABA -> tenant table, reserved in one atomic write.
    ///         let key = StoreKey::new("tenant.waba", verified.waba_id.as_str());
    ///         let mine = tenant.as_bytes().to_vec();
    ///         if reservations.put_if_absent(&key, mine.clone(), Expiry::Never).await?.is_none() {
    ///             let owner = reservations.get(&key).await?.map(|v| v.value);
    ///             if owner.as_deref() != Some(mine.as_slice()) {
    ///                 return Err(ValidationError::new(
    ///                     "waba_id",
    ///                     "already connected to another merchant",
    ///                 )
    ///                 .into());
    ///             }
    ///         }
    ///         Ok(())
    ///     })
    ///     .await?;
    /// # let _ = done; Ok(()) }
    /// ```
    pub async fn onboard_with_approval<F, Fut>(
        &self,
        request: &OnboardingRequest,
        vault: &TokenVault,
        approve: F,
    ) -> Result<Onboarded>
    where
        F: FnOnce(VerifiedOnboarding) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        self.onboard_inner(request, vault, Some(approve)).await
    }

    async fn onboard_inner<F, Fut>(
        &self,
        request: &OnboardingRequest,
        vault: &TokenVault,
        approve: Option<F>,
    ) -> Result<Onboarded>
    where
        F: FnOnce(VerifiedOnboarding) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        request.validate()?;
        let plan = self.credit_plan(request.currency.as_ref(), request.reshare_after_revocation)?;
        if plan.is_some() && approve.is_none() {
            return Err(CreditError::ApprovalRequired(
                "a Solution Partner shares its credit line, which cannot be taken back once attached: onboard with EmbeddedSignup::onboard_with_approval, whose approval decides which of your tenants may onboard the WABA".into(),
            )
            .into());
        }
        let mut done = Vec::with_capacity(9);

        let token = self
            .exchange_code(&request.code)
            .await
            .map_err(|e| e.in_step(EXCHANGE_CODE))?;
        done.push(EXCHANGE_CODE);

        let debug = self
            .debug_token(&token.access_token)
            .await
            .map_err(|e| e.in_step(DEBUG_TOKEN))?;
        done.push(DEBUG_TOKEN);

        let business = self.client.with_token(token.access_token.clone());
        let assets = verify_assets(
            &business,
            &debug,
            &self.app.app_id,
            &request.session,
            request.register || request.is_coexistence(),
        )
        .await
        .map_err(|e| e.in_step(VERIFY_ASSETS))?;
        done.push(VERIFY_ASSETS);

        // The token record stored below: an approval is recorded for it.
        let created_at = vault.now();
        if let Some(approve) = approve {
            let verified = VerifiedOnboarding {
                waba_id: assets.waba_id.clone(),
                business_id: assets.business_id.clone(),
                phone_number_id: assets.phone_number_id.clone(),
                phone_number_ids: assets.phone_number_ids.clone(),
            };
            self.approve(plan.as_ref(), vault, verified, approve, Some(created_at))
                .await
                .map_err(|e| e.in_step(APPROVE))?;
            done.push(APPROVE);
        }

        let expires_at = debug.expires_at_time().or_else(|| {
            token
                .expires_in
                .and_then(|s| i64::try_from(s).ok())
                .map(|s| vault.now() + time::Duration::seconds(s))
        });
        let mut stored =
            StoredBusinessToken::new(assets.waba_id.clone(), token.access_token.clone());
        stored.business_id.clone_from(&assets.business_id);
        stored.phone_number_ids.clone_from(&assets.phone_number_ids);
        stored.expires_at = expires_at;
        stored.created_at = Some(created_at);
        vault
            .store(&stored)
            .await
            .map_err(|e| e.in_step(STORE_TOKEN))?;
        done.push(STORE_TOKEN);

        let tail = Tail {
            es: self,
            business: &business,
            vault,
            request,
            plan: plan.as_ref(),
        };
        let allocation_config_id = tail
            .run(&stored, assets.phone_number_id.as_ref(), &mut done)
            .await?;

        Ok(Onboarded {
            waba_id: assets.waba_id,
            phone_number_id: assets.phone_number_id,
            phone_number_ids: assets.phone_number_ids,
            business_id: assets.business_id,
            finish_kind: request.finish_kind,
            token: token.access_token,
            token_expires_at: expires_at,
            allocation_config_id,
            steps_completed: done,
        })
    }

    /// Run the integrator's approval and, in Solution Partner mode, record
    /// it in the credit ledger for the token record created at
    /// `token_created_at`.
    async fn approve<F, Fut>(
        &self,
        plan: Option<&CreditPlan<'_>>,
        vault: &TokenVault,
        verified: VerifiedOnboarding,
        approve: F,
        token_created_at: Option<OffsetDateTime>,
    ) -> Result<()>
    where
        F: FnOnce(VerifiedOnboarding) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let waba_id = verified.waba_id.clone();
        approve(verified).await?;
        if plan.is_some() {
            vault.record_approval(&waba_id, token_created_at).await?;
        }
        Ok(())
    }

    /// Redo the repeatable tail of [`Self::onboard`] (subscribe, in
    /// Solution Partner mode the credit line steps, register) for a WABA
    /// whose token is already in `vault`, e.g. after `register_phone` failed
    /// on a wrong PIN. `request.code` is not used.
    ///
    /// In Solution Partner mode, `share_credit_line` first checks whether
    /// the credit line already funds the WABA (a share that failed with a
    /// timeout may have gone through) and posts nothing when it does. It
    /// runs only for a WABA whose onboarding you approved
    /// ([`Self::onboard_with_approval`] records it in the credit ledger);
    /// otherwise `resume` fails at step `approve` with
    /// [`CreditError::ApprovalRequired`] before any request: use
    /// [`Self::resume_with_approval`] (a token stored in Tech Provider mode,
    /// before the deployment became a Solution Partner, or by a revision
    /// that did not record approvals).
    ///
    /// The number registered is `request.session.phone_number_id` if it is
    /// one of the numbers verified and stored at onboarding, else the first
    /// stored one; a session naming another WABA or an unverified number is
    /// refused (`verify_assets`) before any request.
    ///
    /// This acts with the stored token of `waba_id`, which the caller names:
    /// check that the WABA belongs to the tenant asking (your own
    /// tenant → WABA mapping, e.g. from [`Onboarded::waba_id`]) before
    /// calling it, or one merchant could re-register another's number.
    pub async fn resume(
        &self,
        waba_id: &WabaId,
        request: &OnboardingRequest,
        vault: &TokenVault,
    ) -> Result<Onboarded> {
        self.resume_inner(
            waba_id,
            request,
            vault,
            None::<fn(VerifiedOnboarding) -> Ready<Result<()>>>,
        )
        .await
    }

    /// [`Self::resume`], with your approval first (step `approve`, after
    /// `verify_assets`, before any request): `approve` receives what
    /// onboarding verified and stored for the WABA (its owner business, its
    /// numbers), as in [`Self::onboard_with_approval`]. In Solution Partner
    /// mode the approval is recorded in the credit ledger, so later
    /// `resume` calls need no approval again.
    ///
    /// For a WABA whose token was stored without an approval in the ledger:
    /// onboarded in Tech Provider mode before the deployment became a
    /// Solution Partner, or by a revision that did not record approvals.
    pub async fn resume_with_approval<F, Fut>(
        &self,
        waba_id: &WabaId,
        request: &OnboardingRequest,
        vault: &TokenVault,
        approve: F,
    ) -> Result<Onboarded>
    where
        F: FnOnce(VerifiedOnboarding) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        self.resume_inner(waba_id, request, vault, Some(approve))
            .await
    }

    async fn resume_inner<F, Fut>(
        &self,
        waba_id: &WabaId,
        request: &OnboardingRequest,
        vault: &TokenVault,
        approve: Option<F>,
    ) -> Result<Onboarded>
    where
        F: FnOnce(VerifiedOnboarding) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        request.validate()?;
        let plan = self.credit_plan(request.currency.as_ref(), request.reshare_after_revocation)?;
        let stored = vault
            .get(waba_id)
            .await
            .map_err(|e| e.in_step(LOAD_TOKEN))?
            .ok_or_else(|| {
                Error::from(ValidationError::new(
                    "waba_id",
                    "no token is stored for this WABA",
                ))
                .in_step(LOAD_TOKEN)
            })?;
        let mut done = vec![LOAD_TOKEN];
        let phone_number_id = resume_phone(waba_id, &stored, &request.session)
            .map_err(|e| Error::from(e).in_step(VERIFY_ASSETS))?;
        done.push(VERIFY_ASSETS);
        match approve {
            Some(approve) => {
                let verified = VerifiedOnboarding {
                    waba_id: waba_id.clone(),
                    business_id: stored.business_id.clone(),
                    phone_number_id: phone_number_id.clone(),
                    phone_number_ids: stored.phone_number_ids.clone(),
                };
                self.approve(plan.as_ref(), vault, verified, approve, stored.created_at)
                    .await
                    .map_err(|e| e.in_step(APPROVE))?;
                done.push(APPROVE);
            }
            None if plan.is_some() => {
                let approved = vault
                    .credit(waba_id)
                    .await
                    .map_err(|e| e.in_step(APPROVE))?
                    .is_some_and(|c| c.approves(stored.created_at));
                if !approved {
                    return Err(Error::from(CreditError::ApprovalRequired(
                        "no approval of this WABA's stored token is recorded (a token stored in Tech Provider mode, stored again since the approval, or before approvals were recorded): resume with EmbeddedSignup::resume_with_approval".into(),
                    ))
                    .in_step(APPROVE));
                }
            }
            None => {}
        }
        let business = self.client.with_token(stored.token.clone());
        let tail = Tail {
            es: self,
            business: &business,
            vault,
            request,
            plan: plan.as_ref(),
        };
        let allocation_config_id = tail
            .run(&stored, phone_number_id.as_ref(), &mut done)
            .await?;
        Ok(Onboarded {
            waba_id: waba_id.clone(),
            phone_number_id,
            phone_number_ids: stored.phone_number_ids,
            business_id: stored.business_id,
            finish_kind: request.finish_kind,
            token: stored.token,
            token_expires_at: stored.expires_at,
            allocation_config_id,
            steps_completed: done,
        })
    }
}

/// The steps after `store_token`, shared by `onboard` and `resume`.
struct Tail<'a> {
    es: &'a EmbeddedSignup,
    /// Authenticated with the merchant's business token.
    business: &'a Client,
    vault: &'a TokenVault,
    request: &'a OnboardingRequest,
    /// `None` for a Tech Provider: no credit line step runs.
    plan: Option<&'a CreditPlan<'a>>,
}

impl Tail<'_> {
    /// `subscribe_app`; in Solution Partner mode `assign_system_user`
    /// (share-and-attach only) and `share_credit_line`; then
    /// `register_phone` if requested. The allocation funding the WABA, in
    /// Solution Partner mode.
    async fn run(
        &self,
        stored: &StoredBusinessToken,
        phone_number_id: Option<&PhoneNumberId>,
        done: &mut Vec<&'static str>,
    ) -> Result<Option<AllocationConfigId>> {
        let waba_id = stored.waba_id.clone();
        let business = self.business;
        let request = self.request;
        business
            .waba(waba_id.clone())
            .subscribe_app(request.subscribe_override.as_ref())
            .await
            .map_err(|e| e.in_step(SUBSCRIBE_APP))?;
        done.push(SUBSCRIBE_APP);

        let mut allocation = None;
        if let Some(plan) = self.plan {
            if plan.partner.method == CreditSharing::ShareAndAttach {
                assign_system_user(self.es, plan, &waba_id)
                    .await
                    .map_err(|e| e.in_step(ASSIGN_SYSTEM_USER))?;
                done.push(ASSIGN_SYSTEM_USER);
            }
            allocation = Some(
                share_credit_line(self.es, plan, business, self.vault, stored)
                    .await
                    .map_err(|e| e.in_step(SHARE_CREDIT_LINE))?,
            );
            done.push(SHARE_CREDIT_LINE);
        }

        register(business, phone_number_id, request, done).await?;
        Ok(allocation)
    }
}

/// `register_phone`, if requested.
async fn register(
    business: &Client,
    phone_number_id: Option<&PhoneNumberId>,
    request: &OnboardingRequest,
    done: &mut Vec<&'static str>,
) -> Result<()> {
    if request.register {
        let register = async {
            let pin = request
                .pin
                .as_ref()
                .ok_or_else(|| ValidationError::new("pin", "required to register the number"))?;
            let phone = phone_number_id.ok_or_else(|| {
                ValidationError::new(
                    "phone_number_id",
                    "the flow finished without a phone number to register",
                )
            })?;
            business
                .phone_number(phone.clone())
                .register(pin, request.data_localization_region.as_ref())
                .await
        };
        register
            .await
            .map_err(|e: Error| e.in_step(REGISTER_PHONE))?;
        done.push(REGISTER_PHONE);
    }
    Ok(())
}

/// Establish with Meta, using the business token, which assets this
/// onboarding is about. Nothing from the browser survives unchecked.
async fn verify_assets(
    business: &Client,
    debug: &TokenDebug,
    app_id: &AppId,
    session: &SessionInfo,
    need_phone: bool,
) -> Result<VerifiedAssets> {
    let waba_id = verify_grant(debug, session.primary_waba_id(), app_id)?;
    let waba = business.waba(waba_id.clone());
    // Privately decoded: the answer carries the business's name.
    let business_id = waba.owner_business().await?.and_then(|b| b.id);
    let mut numbers = waba_phone_numbers(&waba).await?;
    let phone_number_id = match &session.phone_number_id {
        Some(claimed) => {
            let Some(at) = numbers.iter().position(|n| n == claimed) else {
                return Err(ValidationError::new(
                    "phone_number_id",
                    "the phone number in the session info does not belong to the WABA",
                )
                .into());
            };
            Some(numbers.remove(at))
        }
        // Meta lists numbers most recently onboarded first.
        None if need_phone && !numbers.is_empty() => Some(numbers.remove(0)),
        None => None,
    };
    let phone_number_ids = phone_number_id.iter().cloned().chain(numbers).collect();
    Ok(VerifiedAssets {
        waba_id,
        business_id,
        phone_number_id,
        phone_number_ids,
    })
}

/// Check that `debug` describes a valid token of our app that manages
/// `claimed` (or pick the newest WABA it manages when nothing is claimed).
/// Fails closed on anything missing.
fn verify_grant(
    debug: &TokenDebug,
    claimed: Option<&WabaId>,
    app_id: &AppId,
) -> Result<WabaId, ValidationError> {
    if !debug.is_valid {
        return Err(ValidationError::new(
            "access_token",
            "Meta reports the exchanged token as invalid",
        ));
    }
    match &debug.app_id {
        Some(a) if a == app_id => {}
        Some(_) => {
            return Err(ValidationError::new(
                "app_id",
                "the token was issued to a different app",
            ));
        }
        None => {
            return Err(ValidationError::new(
                "app_id",
                "Meta did not say which app the token was issued to",
            ));
        }
    }
    let Some(targets) = debug.target_ids(WHATSAPP_BUSINESS_MANAGEMENT) else {
        return Err(ValidationError::new(
            "granular_scopes",
            "the token is not limited to specific WABAs, so the WABA cannot be verified",
        ));
    };
    match claimed {
        Some(waba) if targets.iter().any(|t| t == waba.as_str()) => Ok(waba.clone()),
        Some(_) => Err(ValidationError::new(
            "waba_id",
            "the WABA in the session info is not one this token was granted",
        )),
        // Meta lists the newest grant first (`solution-providers/manage-accounts`).
        None => targets
            .first()
            .map(|t| WabaId::new(t.as_str()))
            .ok_or_else(|| ValidationError::new("granular_scopes", "the token grants no WABA")),
    }
}

/// Every phone number id of the WABA, in Meta's order (most recently
/// onboarded first: `business-phone-numbers/phone-numbers`), read with the
/// business token. Reads at most [`MAX_WABA_PHONE_NUMBERS`]` + 1` items: the
/// counter below is the only bound, and it returns before polling again.
async fn waba_phone_numbers(waba: &Waba) -> Result<Vec<PhoneNumberId>> {
    let query = PhoneNumbersQuery::new()
        .fields(["id"])
        .limit(PHONE_PAGE_SIZE);
    let mut numbers = pin!(waba.phone_numbers_stream(&query));
    let mut read = 0usize;
    let mut ids: Vec<PhoneNumberId> = Vec::new();
    while let Some(number) = numbers.next().await {
        read += 1;
        if read > MAX_WABA_PHONE_NUMBERS {
            return Err(ValidationError::new(
                "phone_numbers",
                format!(
                    "the WABA lists more than {MAX_WABA_PHONE_NUMBERS} phone numbers; refusing to index a partial list"
                ),
            )
            .into());
        }
        let id = number?.id;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// The number [`EmbeddedSignup::resume`] registers, checked against what
/// onboarding verified and stored.
fn resume_phone(
    waba_id: &WabaId,
    stored: &StoredBusinessToken,
    session: &SessionInfo,
) -> Result<Option<PhoneNumberId>, ValidationError> {
    if session.primary_waba_id().is_some_and(|w| w != waba_id) {
        return Err(ValidationError::new(
            "waba_id",
            "the session info names a different WABA than the one being resumed",
        ));
    }
    match &session.phone_number_id {
        Some(p) if stored.phone_number_ids.contains(p) => Ok(Some(p.clone())),
        Some(_) => Err(ValidationError::new(
            "phone_number_id",
            "not one of the numbers verified for this WABA at onboarding",
        )),
        None => Ok(stored.phone_number_ids.first().cloned()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use http::Method;
    use meta_whatsapp_adapters::store::MemoryKvStore;
    use meta_whatsapp_core::ErrorKind;
    use meta_whatsapp_core::clock::ManualClock;
    use meta_whatsapp_core::secret::SecretBytes;
    use meta_whatsapp_core::store::{KvStore, StoreKey};
    use meta_whatsapp_core::testing::{RecordedBody, ScriptedTransport};
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use time::macros::datetime;

    use super::super::session::SignupSessions;
    use super::super::vault::tests::RecordingKv;
    use super::super::vault::{TOKEN_NAMESPACE, VaultKey, VaultKeys};
    use super::*;
    use crate::{AppCredentials, RetryPolicy};

    const APP_ID: &str = "236484624622562";
    const APP_SECRET: &str = "614fc2afde15eee07a26b2fe3eaee9b9";
    const CODE: &str = "AQBhlXsctMxJYbwbrpybxlo9tLPGy";
    const TOKEN: &str = "EAAAN6tcBzAUBOwtDtTfmZCJ9n3FHpSDcDTH86ekf89Xnn";
    const WABA: &str = "524126980791429";
    const PHONE: &str = "106540352242922";
    const BUSINESS: &str = "2729063490586005";
    const PIN: &str = "581063";
    const VERIFY_TOKEN: &str = "verify-me-secret";

    struct Harness {
        t: ScriptedTransport,
        es: EmbeddedSignup,
        kv: Arc<dyn KvStore>,
        vault: TokenVault,
    }

    fn harness_on(kv: Arc<dyn KvStore>, retry: RetryPolicy) -> Harness {
        let t = ScriptedTransport::new();
        let client = Client::builder()
            .transport(t.clone())
            .retry(retry)
            .build()
            .unwrap();
        let vault = TokenVault::new(
            Arc::clone(&kv),
            VaultKeys::new(VaultKey::new("k1", SecretBytes::new([42; 32])).unwrap()),
        )
        .unwrap()
        .with_clock(Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC))));
        Harness {
            es: client.embedded_signup(AppCredentials::new(APP_ID, APP_SECRET)),
            t,
            kv,
            vault,
        }
    }

    fn harness() -> Harness {
        harness_on(Arc::new(MemoryKvStore::new()), RetryPolicy::NONE)
    }

    fn finish_event(data: &serde_json::Value, event: &str) -> EmbeddedSignupEvent {
        EmbeddedSignupEvent::from_value(
            json!({"data": data, "type": "WA_EMBEDDED_SIGNUP", "event": event}),
        )
        .unwrap()
    }

    fn request_for(waba: &str, phone: &str) -> OnboardingRequest {
        let event = finish_event(
            &json!({"phone_number_id": phone, "waba_id": waba, "business_id": "CLAIMED_BY_THE_BROWSER"}),
            "FINISH",
        );
        OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &event)
            .unwrap()
            .register_with_pin(TwoStepPin::new(PIN).unwrap())
    }

    fn full_request() -> OnboardingRequest {
        request_for(WABA, PHONE)
    }

    fn token_response(token: &str) -> serde_json::Value {
        json!({"access_token": token, "token_type": "bearer"})
    }

    fn debug_response(wabas: &[&str]) -> serde_json::Value {
        json!({"data": {
          "app_id": APP_ID, "type": "SYSTEM_USER", "application": "Jaspers", "is_valid": true,
          "expires_at": 0, "data_access_expires_at": 0,
          "scopes": ["whatsapp_business_management", "whatsapp_business_messaging"],
          "granular_scopes": [
            {"scope": "whatsapp_business_management", "target_ids": wabas},
            {"scope": "whatsapp_business_messaging", "target_ids": wabas}
          ],
          "user_id": "1"
        }})
    }

    /// `solution-providers/share-and-revoke-credit-lines`, step 1.
    fn owner_response(waba: &str, business: &str) -> serde_json::Value {
        json!({"owner_business_info": {"name": "Wind & Wool", "id": business}, "id": waba})
    }

    fn phones_response(ids: &[&str]) -> serde_json::Value {
        json!({"data": ids.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()})
    }

    fn success() -> serde_json::Value {
        json!({"success": true})
    }

    fn graph_error(code: i64) -> serde_json::Value {
        json!({"error": {"message": format!("(#{code}) x"), "type": "OAuthException", "code": code, "fbtrace_id": "A"}})
    }

    /// Script a complete, successful onboarding of `waba` whose numbers are
    /// `phones` (Meta's order), with the register call.
    fn script_success(t: &ScriptedTransport, token: &str, waba: &str, phones: &[&str]) {
        t.push_json(200, token_response(token));
        t.push_json(200, debug_response(&[waba]));
        t.push_json(200, owner_response(waba, BUSINESS));
        t.push_json(200, phones_response(phones));
        t.push_json(200, success());
        t.push_json(200, success());
    }

    fn step(err: &Error) -> &'static str {
        match err {
            Error::Step { step, .. } => step,
            other => panic!("not a step error: {other}"),
        }
    }

    async fn raw(kv: &Arc<dyn KvStore>, key: &str) -> Option<Vec<u8>> {
        kv.get(&StoreKey::new(TOKEN_NAMESPACE, key))
            .await
            .unwrap()
            .map(|v| v.value)
    }

    #[tokio::test]
    async fn happy_path_runs_every_step_in_order() {
        let h = harness();
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, owner_response(WABA, BUSINESS));
        h.t.push_json(200, phones_response(&["999", PHONE]));
        h.t.push_json(200, success());
        h.t.push_json(200, success());

        let request = full_request().subscribe_override(CallbackOverride::new(
            "https://hooks.example.com/wa",
            VERIFY_TOKEN,
        ));
        let done = h.es.onboard(&request, &h.vault).await.unwrap();
        assert_eq!(
            done.steps_completed,
            vec![
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                STORE_TOKEN,
                SUBSCRIBE_APP,
                REGISTER_PHONE
            ]
        );
        assert_eq!(done.waba_id.as_str(), WABA);
        assert_eq!(
            done.phone_number_id.as_ref().map(PhoneNumberId::as_str),
            Some(PHONE)
        );
        assert_eq!(
            done.phone_number_ids,
            vec![PhoneNumberId::new(PHONE), PhoneNumberId::new("999")],
            "the onboarded number first, then the WABA's others"
        );
        assert_eq!(
            done.business_id.as_ref().map(BusinessId::as_str),
            Some(BUSINESS),
            "the owner Meta reports, not the browser's claim"
        );
        assert_eq!(done.token.expose_secret(), TOKEN);
        assert_eq!(done.token_expires_at, None);
        let debug_text = format!("{done:?} {request:?}");
        for secret in [TOKEN, CODE, PIN, VERIFY_TOKEN, APP_SECRET] {
            assert!(!debug_text.contains(secret), "{secret} in {debug_text}");
        }

        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 6);
        // 1. code exchange: no bearer.
        assert_eq!(reqs[0].path(), "/v25.0/oauth/access_token");
        assert_eq!(reqs[0].header("authorization"), None);
        assert_eq!(reqs[0].query("code").as_deref(), Some(CODE));
        // 2. debug_token with the app token.
        assert_eq!(reqs[1].path(), "/v25.0/debug_token");
        assert_eq!(reqs[1].query("input_token").as_deref(), Some(TOKEN));
        assert_eq!(
            reqs[1].bearer(),
            Some(format!("{APP_ID}|{APP_SECRET}").as_str())
        );
        // 3. the WABA's owner, with the business token.
        assert_eq!(reqs[2].method, Method::GET);
        assert_eq!(reqs[2].path(), format!("/v25.0/{WABA}"));
        assert_eq!(
            reqs[2].query("fields").as_deref(),
            Some("owner_business_info")
        );
        assert_eq!(reqs[2].bearer(), Some(TOKEN));
        // 4. the WABA's numbers, with the business token.
        assert_eq!(reqs[3].method, Method::GET);
        assert_eq!(reqs[3].path(), format!("/v25.0/{WABA}/phone_numbers"));
        assert_eq!(reqs[3].query("fields").as_deref(), Some("id"));
        assert_eq!(reqs[3].query("limit").as_deref(), Some("100"));
        assert_eq!(reqs[3].bearer(), Some(TOKEN));
        // 5. subscribe with the override.
        assert_eq!(reqs[4].method, Method::POST);
        assert_eq!(reqs[4].path(), format!("/v25.0/{WABA}/subscribed_apps"));
        assert_eq!(reqs[4].bearer(), Some(TOKEN));
        assert_eq!(
            reqs[4].json(),
            Some(
                json!({"override_callback_uri": "https://hooks.example.com/wa", "verify_token": VERIFY_TOKEN})
            )
        );
        // 6. register.
        assert_eq!(reqs[5].path(), format!("/v25.0/{PHONE}/register"));
        assert_eq!(reqs[5].bearer(), Some(TOKEN));
        assert_eq!(
            reqs[5].json(),
            Some(json!({"messaging_product": "whatsapp", "pin": PIN}))
        );
        assert_eq!(h.t.remaining(), 0);
        assert_stored_and_routed(&h, &[PHONE, "999"]).await;
    }

    /// Stored under `WABA`, encrypted, with the verified owner, and every
    /// one of `phones` routes to it.
    async fn assert_stored_and_routed(h: &Harness, phones: &[&str]) {
        let bytes = raw(&h.kv, &format!("waba/{WABA}")).await.unwrap();
        assert!(
            !bytes.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()),
            "token stored in the clear"
        );
        let stored = h.vault.get(&WabaId::new(WABA)).await.unwrap().unwrap();
        assert_eq!(stored.token.expose_secret(), TOKEN);
        assert_eq!(
            stored.business_id.as_ref().map(BusinessId::as_str),
            Some(BUSINESS)
        );
        for phone in phones {
            let by_phone = h
                .vault
                .get_by_phone_number(&PhoneNumberId::new(*phone))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(by_phone.waba_id.as_str(), WABA);
        }
    }

    #[tokio::test]
    async fn missing_ids_are_resolved_through_debug_token_and_the_waba() {
        let h = harness();
        h.t.push_json(200, json!({"access_token": TOKEN, "expires_in": 5_184_000}));
        h.t.push_json(200, debug_response(&["NEWEST", "OLDER"]));
        h.t.push_json(200, owner_response("NEWEST", BUSINESS));
        h.t.push_json(200, phones_response(&["P-NEWEST", "P-OLDER"]));
        h.t.push_json(200, success());
        h.t.push_json(200, success());
        let request =
            OnboardingRequest::new(SignupCode::new(CODE).unwrap(), SessionInfo::default())
                .register_with_pin(TwoStepPin::new("123456").unwrap());
        let done = h.es.onboard(&request, &h.vault).await.unwrap();
        assert_eq!(done.waba_id.as_str(), "NEWEST");
        assert_eq!(
            done.phone_number_id.as_ref().map(PhoneNumberId::as_str),
            Some("P-NEWEST")
        );
        assert_eq!(
            done.token_expires_at,
            Some(datetime!(2026-09-24 12:00 UTC) + time::Duration::days(60)),
            "expires_in counted from the vault clock"
        );
        let reqs = h.t.requests();
        assert_eq!(reqs[1].path(), "/v25.0/debug_token");
        assert_eq!(reqs[2].path(), "/v25.0/NEWEST");
        assert_eq!(reqs[3].path(), "/v25.0/NEWEST/phone_numbers");
        assert_eq!(reqs[5].path(), "/v25.0/P-NEWEST/register");
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn only_waba_flow_without_registration_indexes_but_registers_nothing() {
        let h = harness();
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, owner_response(WABA, BUSINESS));
        h.t.push_json(200, phones_response(&[PHONE]));
        h.t.push_json(200, success());
        let event = finish_event(&json!({"waba_id": WABA}), "FINISH_ONLY_WABA");
        let request =
            OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &event).unwrap();
        let done = h.es.onboard(&request, &h.vault).await.unwrap();
        assert_eq!(
            done.steps_completed,
            vec![
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                STORE_TOKEN,
                SUBSCRIBE_APP
            ]
        );
        assert_eq!(done.phone_number_id, None);
        assert_eq!(done.phone_number_ids, vec![PhoneNumberId::new(PHONE)]);
        assert_eq!(
            h.t.requests()[4].body,
            RecordedBody::Empty,
            "plain subscribe"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn re_onboarding_a_waba_keeps_every_number_routed() {
        // A merchant adds a second number to the same WABA through Embedded
        // Signup. The first number must keep routing to the WABA.
        let h = harness();
        script_success(&h.t, TOKEN, WABA, &[PHONE]);
        h.es.onboard(&full_request(), &h.vault).await.unwrap();
        script_success(&h.t, "EAAsecondToken", WABA, &["P2", PHONE]);
        let done =
            h.es.onboard(&request_for(WABA, "P2"), &h.vault)
                .await
                .unwrap();
        assert_eq!(
            done.phone_number_ids,
            vec![PhoneNumberId::new("P2"), PhoneNumberId::new(PHONE)]
        );
        for phone in [PHONE, "P2"] {
            let t = h
                .vault
                .get_by_phone_number(&PhoneNumberId::new(phone))
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("{phone} no longer routes"));
            assert_eq!(t.token.expose_secret(), "EAAsecondToken");
        }
        // And a number Meta no longer lists on the WABA is unlinked.
        script_success(&h.t, "EAAthirdToken", WABA, &["P2"]);
        h.es.onboard(&request_for(WABA, "P2"), &h.vault)
            .await
            .unwrap();
        assert!(
            h.vault
                .get_by_phone_number(&PhoneNumberId::new(PHONE))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn coexistence_resolves_the_number_and_never_registers() {
        let h = harness();
        let event = finish_event(
            &json!({"waba_id": WABA}),
            "FINISH_WHATSAPP_BUSINESS_APP_ONBOARDING",
        );
        let request =
            OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &event).unwrap();
        // Asking to register a coexistence number is refused up front.
        let refused = request
            .clone()
            .register_with_pin(TwoStepPin::new("123456").unwrap());
        assert!(matches!(
            h.es.onboard(&refused, &h.vault).await,
            Err(Error::Validation(_))
        ));
        assert!(h.t.requests().is_empty());

        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, owner_response(WABA, BUSINESS));
        h.t.push_json(200, phones_response(&[PHONE]));
        h.t.push_json(200, success());
        let done = h.es.onboard(&request, &h.vault).await.unwrap();
        assert!(done.needs_coexistence_sync());
        assert_eq!(
            done.phone_number_id.as_ref().map(PhoneNumberId::as_str),
            Some(PHONE)
        );
        assert_eq!(
            done.steps_completed,
            vec![
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                STORE_TOKEN,
                SUBSCRIBE_APP
            ]
        );
        assert!(
            h.t.requests()
                .iter()
                .all(|r| !r.path().ends_with("/register"))
        );
        assert!(
            h.vault
                .get_by_phone_number(&PhoneNumberId::new(PHONE))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn failure_at_exchange_code_stops_everything() {
        let h = harness();
        h.t.push_json(400, graph_error(100));
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), EXCHANGE_CODE);
        assert_eq!(err.kind(), ErrorKind::InvalidParameter);
        assert_eq!(h.t.requests().len(), 1);
        assert!(raw(&h.kv, &format!("waba/{WABA}")).await.is_none());
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn failure_at_debug_token_stops_before_storing() {
        let h = harness();
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(500, json!({"error": {"message": "x", "code": 2}}));
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), DEBUG_TOKEN);
        assert_eq!(err.kind(), ErrorKind::ServiceUnavailable);
        assert_eq!(h.t.requests().len(), 2);
        assert!(raw(&h.kv, &format!("waba/{WABA}")).await.is_none());
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_waba_the_token_was_not_granted_is_rejected() {
        let h = harness();
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&["SOMEONE_ELSES_OWN_WABA"]));
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), VERIFY_ASSETS);
        assert_eq!(h.t.requests().len(), 2, "nothing after the check");
        assert!(raw(&h.kv, &format!("waba/{WABA}")).await.is_none());
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn invalid_foreign_unnamed_or_ungranular_tokens_are_rejected() {
        let grants = json!([{"scope": "whatsapp_business_management", "target_ids": [WABA]}]);
        for debug in [
            json!({"data": {"is_valid": false, "app_id": APP_ID, "granular_scopes": grants}}),
            json!({"data": {"is_valid": true, "app_id": "OTHER_APP", "granular_scopes": grants}}),
            // No app_id at all: fail closed, never "not a different app".
            json!({"data": {"is_valid": true, "granular_scopes": grants}}),
            json!({"data": {"is_valid": true, "app_id": APP_ID, "granular_scopes": [{"scope": "whatsapp_business_management"}]}}),
            json!({"data": {"is_valid": true, "app_id": APP_ID, "granular_scopes": [{"scope": "whatsapp_business_management", "target_ids": []}]}}),
            json!({"data": {"is_valid": true, "app_id": APP_ID, "granular_scopes": [{"scope": "whatsapp_business_messaging", "target_ids": [WABA]}]}}),
        ] {
            for request in [
                OnboardingRequest::new(SignupCode::new(CODE).unwrap(), SessionInfo::default()),
                full_request(),
            ] {
                let h = harness();
                h.t.push_json(200, token_response(TOKEN));
                h.t.push_json(200, debug.clone());
                let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
                assert_eq!(step(&err), VERIFY_ASSETS, "{debug}");
                assert_eq!(h.t.requests().len(), 2, "{debug}");
                assert_eq!(h.t.remaining(), 0);
            }
        }
    }

    #[tokio::test]
    async fn a_phone_number_outside_the_waba_is_rejected() {
        let h = harness();
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, owner_response(WABA, BUSINESS));
        h.t.push_json(
            200,
            json!({"data": [{"id": "OTHER"}], "paging": {"cursors": {"after": "c"}, "next": "https://graph.facebook.com/x"}}),
        );
        h.t.push_json(200, phones_response(&["STILL_OTHER"]));
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), VERIFY_ASSETS);
        assert_eq!(h.t.requests().len(), 5, "all pages were searched");
        assert!(raw(&h.kv, &format!("waba/{WABA}")).await.is_none());
        assert!(
            raw(&h.kv, &format!("phone/{PHONE}")).await.is_none(),
            "no phone index poisoning"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_failed_owner_lookup_stops_before_storing() {
        let h = harness();
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(403, graph_error(200));
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), VERIFY_ASSETS);
        assert_eq!(err.kind(), ErrorKind::Permission);
        assert!(raw(&h.kv, &format!("waba/{WABA}")).await.is_none());
        assert_eq!(h.t.remaining(), 0);
    }

    /// The owner lookup answers with the business's name: a body that does
    /// not decode is reported without quoting it.
    #[tokio::test]
    async fn an_undecodable_owner_is_reported_without_its_name() {
        let h = harness();
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(
            200,
            json!({"owner_business_info": {"name": "Wind & Wool", "id": {"not": "a string"}}, "id": WABA}),
        );
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), VERIFY_ASSETS);
        assert!(
            matches!(&err, Error::Step { source, .. } if matches!(&**source, Error::Decode { .. })),
            "{err}"
        );
        assert!(!format!("{err} {err:?}").contains("Wind"), "{err:?}");
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn an_endless_number_list_fails_closed() {
        // Pages that never end (every page names a fresh cursor): the read
        // stops at the bound instead of looping or indexing a partial list.
        let h = harness();
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, owner_response(WABA, BUSINESS));
        let pages = MAX_WABA_PHONE_NUMBERS / PHONE_PAGE_SIZE as usize + 1;
        for page in 0..pages {
            // The claimed number is on the first page, so only the bound (not
            // a failed lookup) can make this fail.
            let ids: Vec<_> = (0..PHONE_PAGE_SIZE)
                .map(|i| match (page, i) {
                    (0, 0) => json!({"id": PHONE}),
                    _ => json!({"id": format!("P{page}-{i}")}),
                })
                .collect();
            h.t.push_json(
                200,
                json!({"data": ids, "paging": {"cursors": {"after": format!("c{page}")}, "next": "https://graph.facebook.com/x"}}),
            );
        }
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), VERIFY_ASSETS);
        assert_eq!(h.t.requests().len(), 3 + pages);
        assert!(raw(&h.kv, &format!("waba/{WABA}")).await.is_none());
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn failure_at_store_token_stops_before_subscribing() {
        #[derive(Debug)]
        struct BrokenKv;
        #[async_trait::async_trait]
        impl KvStore for BrokenKv {
            async fn get(
                &self,
                _: &StoreKey,
            ) -> Result<
                Option<meta_whatsapp_core::store::Versioned>,
                meta_whatsapp_core::error::StorageError,
            > {
                Ok(None)
            }
            async fn put(
                &self,
                _: &StoreKey,
                _: Vec<u8>,
                _: meta_whatsapp_core::store::Expiry,
            ) -> Result<u64, meta_whatsapp_core::error::StorageError> {
                Err(meta_whatsapp_core::error::StorageError::Backend(
                    anyhow::anyhow!("disk full"),
                ))
            }
            async fn put_if_absent(
                &self,
                _: &StoreKey,
                _: Vec<u8>,
                _: meta_whatsapp_core::store::Expiry,
            ) -> Result<Option<u64>, meta_whatsapp_core::error::StorageError> {
                Ok(None)
            }
            async fn compare_and_swap(
                &self,
                _: &StoreKey,
                _: u64,
                _: Option<Vec<u8>>,
                _: meta_whatsapp_core::store::Expiry,
            ) -> Result<Option<u64>, meta_whatsapp_core::error::StorageError> {
                Ok(None)
            }
            async fn delete(
                &self,
                _: &StoreKey,
            ) -> Result<bool, meta_whatsapp_core::error::StorageError> {
                Ok(false)
            }
        }
        let h = harness_on(Arc::new(BrokenKv), RetryPolicy::NONE);
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, owner_response(WABA, BUSINESS));
        h.t.push_json(200, phones_response(&[PHONE]));
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), STORE_TOKEN);
        assert!(matches!(
            err,
            Error::Step { ref source, .. } if matches!(**source, Error::Storage(_))
        ));
        assert_eq!(h.t.requests().len(), 4, "no subscribe, no register");
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn failure_at_subscribe_keeps_the_token_and_resume_finishes() {
        let h = harness();
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, owner_response(WABA, BUSINESS));
        h.t.push_json(200, phones_response(&[PHONE]));
        h.t.push_json(403, graph_error(200));
        let request = full_request();
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), SUBSCRIBE_APP);
        assert_eq!(err.kind(), ErrorKind::Permission);
        assert_eq!(h.t.requests().len(), 5, "register never ran");
        assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_some());
        assert_eq!(h.t.remaining(), 0);

        h.t.push_json(200, success());
        h.t.push_json(200, success());
        let done =
            h.es.resume(&WabaId::new(WABA), &request, &h.vault)
                .await
                .unwrap();
        assert_eq!(
            done.steps_completed,
            vec![LOAD_TOKEN, VERIFY_ASSETS, SUBSCRIBE_APP, REGISTER_PHONE]
        );
        assert_eq!(
            done.business_id.as_ref().map(BusinessId::as_str),
            Some(BUSINESS)
        );
        let reqs = h.t.requests();
        assert_eq!(reqs[5].path(), format!("/v25.0/{WABA}/subscribed_apps"));
        assert_eq!(reqs[6].path(), format!("/v25.0/{PHONE}/register"));
        assert_eq!(reqs[6].bearer(), Some(TOKEN), "stored token used");
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn failure_at_register_reports_the_step_and_resume_retries_it() {
        let h = harness();
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, owner_response(WABA, BUSINESS));
        h.t.push_json(200, phones_response(&[PHONE]));
        h.t.push_json(200, success());
        h.t.push_json(400, graph_error(133005));
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), REGISTER_PHONE);
        assert_eq!(err.kind(), ErrorKind::TwoStepVerification);
        assert!(!err.to_string().contains(PIN), "{err}");
        assert_eq!(h.t.remaining(), 0);

        // The merchant supplies their existing PIN; only the tail runs again.
        h.t.push_json(200, success());
        h.t.push_json(200, success());
        let fixed = full_request().register_with_pin(TwoStepPin::new("000111").unwrap());
        h.es.resume(&WabaId::new(WABA), &fixed, &h.vault)
            .await
            .unwrap();
        assert_eq!(
            h.t.last_request().unwrap().json(),
            Some(json!({"messaging_product": "whatsapp", "pin": "000111"}))
        );
        assert_eq!(h.t.remaining(), 0);

        // 133016: the number is locked for 72 hours; reported, not retried.
        h.t.push_json(200, success());
        h.t.push_json(400, graph_error(133016));
        let err =
            h.es.resume(&WabaId::new(WABA), &fixed, &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), REGISTER_PHONE);
        assert_eq!(err.kind(), ErrorKind::Registration);
        assert!(!err.is_retryable());
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn register_without_a_number_fails_at_register_after_storing() {
        let h = harness();
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, owner_response(WABA, BUSINESS));
        h.t.push_json(200, phones_response(&[]));
        h.t.push_json(200, success());
        let request = OnboardingRequest::new(
            SignupCode::new(CODE).unwrap(),
            SessionInfo {
                waba_id: Some(WabaId::new(WABA)),
                ..SessionInfo::default()
            },
        )
        .register_with_pin(TwoStepPin::new("123456").unwrap());
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), REGISTER_PHONE);
        assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_some());
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn resume_refuses_claims_onboarding_did_not_verify() {
        let h = harness();
        script_success(&h.t, TOKEN, WABA, &[PHONE]);
        h.es.onboard(&full_request(), &h.vault).await.unwrap();
        let sent = h.t.requests().len();

        for request in [
            // A number that is not one of this WABA's verified numbers.
            request_for(WABA, "SOMEONE_ELSES_NUMBER"),
            // A session naming another WABA.
            request_for("OTHER_WABA", PHONE),
        ] {
            let err =
                h.es.resume(&WabaId::new(WABA), &request, &h.vault)
                    .await
                    .unwrap_err();
            assert_eq!(step(&err), VERIFY_ASSETS);
        }
        let err =
            h.es.resume(&WabaId::new("UNKNOWN"), &full_request(), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), LOAD_TOKEN);
        assert_eq!(h.t.requests().len(), sent, "nothing was sent");
    }

    #[tokio::test]
    async fn input_is_validated_before_any_request() {
        let h = harness();
        let mut no_pin = full_request();
        no_pin.pin = None;
        let region_without_register =
            OnboardingRequest::new(SignupCode::new(CODE).unwrap(), SessionInfo::default())
                .data_localization_region(DataLocalizationRegion::De);
        let long_override =
            full_request().subscribe_override(CallbackOverride::new("u".repeat(201), "t"));
        for request in [no_pin, region_without_register, long_override] {
            assert!(matches!(
                h.es.onboard(&request, &h.vault).await,
                Err(Error::Validation(_))
            ));
        }
        let cancel = EmbeddedSignupEvent::from_value(
            json!({"data": {"current_step": "PERMISSIONS"}, "type": "WA_EMBEDDED_SIGNUP", "event": "CANCEL"}),
        )
        .unwrap();
        assert!(OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &cancel).is_err());
        assert!(h.t.requests().is_empty());
    }

    /// The multi-tenant property end to end: merchant A, signed in as tenant
    /// A, completes Embedded Signup with their own Meta assets but posts a
    /// session event naming merchant B's WABA and/or number. Nothing of B's
    /// may change.
    #[tokio::test]
    async fn a_tenant_cannot_take_over_another_tenants_waba_or_number() {
        const WABA_A: &str = "111111111111111";
        const WABA_B: &str = "222222222222222";
        const PHONE_A: &str = "333333333333333";
        const PHONE_B: &str = "444444444444444";
        const TOKEN_A: &str = "EAAtokenOfMerchantA";
        const TOKEN_B: &str = "EAAtokenOfMerchantB";
        let h = harness();
        let sessions = SignupSessions::new(Arc::clone(&h.kv));

        // B onboarded earlier.
        script_success(&h.t, TOKEN_B, WABA_B, &[PHONE_B]);
        h.es.onboard(&request_for(WABA_B, PHONE_B), &h.vault)
            .await
            .unwrap();
        let b_record = raw(&h.kv, &format!("waba/{WABA_B}")).await.unwrap();
        let b_index = raw(&h.kv, &format!("phone/{PHONE_B}")).await.unwrap();

        // A starts a session; B's login cannot redeem it, A's can.
        let state = sessions
            .start("tenant-A", Duration::from_mins(15))
            .await
            .unwrap();
        assert!(!sessions.redeem(&state, "tenant-B").await.unwrap());
        assert!(sessions.redeem(&state, "tenant-A").await.unwrap());

        // A's token (granted only A's WABA) with a claim of B's WABA + number.
        h.t.push_json(200, token_response(TOKEN_A));
        h.t.push_json(200, debug_response(&[WABA_A]));
        let err =
            h.es.onboard(&request_for(WABA_B, PHONE_B), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), VERIFY_ASSETS);

        // A's own WABA, but B's number: Meta does not list it on WABA_A.
        h.t.push_json(200, token_response(TOKEN_A));
        h.t.push_json(200, debug_response(&[WABA_A]));
        h.t.push_json(200, owner_response(WABA_A, "BUSINESS_A"));
        h.t.push_json(200, phones_response(&[PHONE_A]));
        let err =
            h.es.onboard(&request_for(WABA_A, PHONE_B), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), VERIFY_ASSETS);
        assert_eq!(h.t.remaining(), 0);

        // B's record and routing are byte-for-byte what they were.
        assert_eq!(
            raw(&h.kv, &format!("waba/{WABA_B}")).await.unwrap(),
            b_record
        );
        assert_eq!(
            raw(&h.kv, &format!("phone/{PHONE_B}")).await.unwrap(),
            b_index
        );
        let routed = h
            .vault
            .get_by_phone_number(&PhoneNumberId::new(PHONE_B))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(routed.waba_id.as_str(), WABA_B);
        assert_eq!(routed.token.expose_secret(), TOKEN_B);
        assert!(raw(&h.kv, &format!("waba/{WABA_A}")).await.is_none());
    }

    /// A `tracing` subscriber that renders every event and span field.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<String>>);

    struct Render<'a>(&'a mut String);

    impl tracing::field::Visit for Render<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{}={value:?} ", field.name());
        }
    }

    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            attrs.record(&mut Render(&mut self.0.lock().unwrap()));
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>) {
            values.record(&mut Render(&mut self.0.lock().unwrap()));
        }
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut out = self.0.lock().unwrap();
            out.push_str(event.metadata().target());
            out.push(' ');
            event.record(&mut Render(&mut out));
            out.push('\n');
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// Secrets never reach the store, the logs, or any `Debug`/`Display`,
    /// on the success path and on the failure paths that log (retries) or
    /// carry text (errors).
    #[tokio::test]
    async fn secrets_never_reach_the_store_logs_or_errors() {
        // tracing-core treats a single registered dispatcher as the only
        // one: a callsite another test thread registers first then gets its
        // interest from that thread's default (none, so "never", cached for
        // every thread), and the capture sees nothing (1 run in 5 to 10 of
        // the whole suite). With a second dispatcher registered, every
        // registration consults all of them, this capture included.
        let _second = tracing::Dispatch::new(Capture::default());
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.clone());
        let rec = Arc::new(RecordingKv::default());
        let h = harness_on(
            rec.clone(),
            RetryPolicy {
                max_retries: 2,
                base_delay: Duration::ZERO,
                max_delay: Duration::ZERO,
            },
        );
        let sessions = SignupSessions::new(rec.clone());
        let state = sessions
            .start("tenant-A", Duration::from_mins(15))
            .await
            .unwrap();
        assert!(sessions.redeem(&state, "tenant-A").await.unwrap());

        // Success, with a retried transient failure on the way (logged).
        h.t.push_json(200, token_response(TOKEN));
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, owner_response(WABA, BUSINESS));
        h.t.push_json(
            500,
            json!({"error": {"message": "x", "code": 2, "is_transient": true}}),
        );
        h.t.push_json(200, phones_response(&[PHONE]));
        h.t.push_json(200, success());
        h.t.push_json(200, success());
        let request = full_request()
            .subscribe_override(CallbackOverride::new("https://h.example/wa", VERIFY_TOKEN));
        let done = h.es.onboard(&request, &h.vault).await.unwrap();

        // Failures: a rejected code, a wrong PIN on resume.
        h.t.push_json(
            400,
            json!({"error": {"message": "This authorization code has been used.", "type": "OAuthException", "code": 100}}),
        );
        let e1 = h.es.onboard(&request, &h.vault).await.unwrap_err();
        h.t.push_json(200, success());
        h.t.push_json(400, graph_error(133005));
        let e2 =
            h.es.resume(&WabaId::new(WABA), &request, &h.vault)
                .await
                .unwrap_err();
        assert_eq!(h.t.remaining(), 0);

        let logs = capture.0.lock().unwrap().clone();
        assert!(
            logs.contains("graph request") && logs.contains("retrying graph request"),
            "capture saw nothing: vacuous check\n{logs}"
        );
        let text = format!("{logs}\n{done:?}\n{request:?}\n{e1}\n{e1:?}\n{e2}\n{e2:?}");
        for secret in [TOKEN, CODE, PIN, APP_SECRET, VERIFY_TOKEN, state.as_str()] {
            assert!(!text.contains(secret), "`{secret}` leaked:\n{text}");
        }
        for secret in [TOKEN, CODE, PIN, APP_SECRET, VERIFY_TOKEN] {
            rec.assert_never_wrote(secret);
        }
    }
}
