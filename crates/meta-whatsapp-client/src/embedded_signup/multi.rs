//! Several WABAs in one signup (`OPEN_QUESTIONS.md` #5, roadmap L11b):
//! [`EmbeddedSignup::onboard_wabas`] onboards the claimed WABA as
//! [`EmbeddedSignup::onboard`] does and, with
//! [`OnboardingRequest::every_granted_waba`] on, every other WABA the
//! business token was granted, each on its own.
//!
//! What Meta documents: a multi-WABA flow's session info carries
//! `waba_ids`, every WABA of the customer's (`embedded-signup/implementation`,
//! § Session logging), and `debug_token` on the business token lists, in
//! `granular_scopes`, the ids of every WABA that granted the app a
//! permission, the most recently onboarded first
//! (`solution-providers/manage-accounts`). The browser's list is a claim;
//! the token's grants are Meta's, so they decide what is onboarded, and a
//! claim they do not back refuses the signup.
//!
//! A token's grants can outlast one signup, and Meta's example token is a
//! user's, whose grants may span businesses: so a grant other than the
//! primary WABA is onboarded only when Meta reports the primary WABA's
//! owner as its owner too (`owner_business_info`, the business the signup
//! is about), and only where the vault holds no record for it, checked by
//! a read before any request and again, atomically, when it is stored.

use std::future::{Future, Ready};

use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{AppId, BusinessId, WabaId};
use meta_whatsapp_core::{Error, Result};

use super::EmbeddedSignup;
use super::event::SessionInfo;
use super::onboard::{
    Granted, GrantedWaba, Onboarded, OnboardingRequest, VerifiedOnboarding, WabaRole,
    required_code, steps, verify_grant,
};
use super::partner::CreditPlan;
use super::token::{TokenDebug, WHATSAPP_BUSINESS_MANAGEMENT};
use super::vault::TokenVault;

/// What [`EmbeddedSignup::onboard_wabas`] did for each WABA of a signup.
///
/// The call succeeds once the steps the WABAs share have (`exchange_code`,
/// `debug_token`, the check of the session's claims against the token's
/// grants); from there each WABA has its own [`WabaOutcome`], and **a
/// WABA that failed does not fail the call**: check [`Self::is_complete`]
/// or [`Self::failures`], and resume each WABA whose token was stored
/// ([`WabaOutcome::is_resumable`]).
///
/// `Debug` never shows a token.
#[derive(Debug)]
#[non_exhaustive]
#[must_use = "a signup can onboard some WABAs and fail on others: check is_complete() or failures()"]
pub struct WabasOnboarded {
    /// One entry per WABA: the primary one first (the claimed WABA, else
    /// the newest grant, what [`EmbeddedSignup::onboard`] onboards), then,
    /// with [`OnboardingRequest::every_granted_waba`] on, the token's other
    /// grants in Meta's order (the most recently onboarded first).
    pub wabas: Vec<WabaOnboarding>,
}

impl WabasOnboarded {
    /// Whether no WABA failed: each was onboarded, or left as the vault
    /// holds it ([`WabaOutcome::Held`]).
    pub fn is_complete(&self) -> bool {
        self.failures().next().is_none()
    }

    /// The WABAs that failed, in order.
    pub fn failures(&self) -> impl Iterator<Item = &WabaOnboarding> {
        self.wabas
            .iter()
            .filter(|w| matches!(w.outcome, WabaOutcome::Failed { .. }))
    }

    /// The WABAs onboarded by this call, in order.
    pub fn onboarded(&self) -> impl Iterator<Item = &Onboarded> {
        self.wabas.iter().filter_map(|w| w.outcome.onboarded())
    }

    /// The primary WABA's entry: the claimed one, else the newest grant.
    pub fn primary(&self) -> Option<&WabaOnboarding> {
        self.wabas.iter().find(|w| w.primary)
    }

    /// The entry of `waba_id`, when the signup had it.
    pub fn get(&self, waba_id: &WabaId) -> Option<&WabaOnboarding> {
        self.wabas.iter().find(|w| &w.waba_id == waba_id)
    }
}

/// One WABA of a signup, and what became of it.
#[derive(Debug)]
#[non_exhaustive]
pub struct WabaOnboarding {
    /// The WABA, among the business token's grants.
    pub waba_id: WabaId,
    /// Whether this is the primary WABA (the claimed one, else the newest
    /// grant): onboarded with the request as it is. The others are
    /// onboarded with no number registered and no coexistence sync.
    pub primary: bool,
    /// What became of it.
    pub outcome: WabaOutcome,
}

/// What became of one WABA of a signup.
#[derive(Debug)]
#[non_exhaustive]
pub enum WabaOutcome {
    /// Onboarded: verified, approved, stored, subscribed and the rest of
    /// [`Onboarded::steps_completed`].
    Onboarded(Box<Onboarded>),
    /// Not the primary WABA, and the vault already holds a token for it
    /// (an earlier onboarding, perhaps another tenant's): left as it is,
    /// with no request, so as not to replace its token and record unasked.
    /// Re-onboard it by claiming it in a signup, or delete it from the
    /// vault first. A record stored for it after this check (another
    /// signup, a hosted `PARTNER_ADDED`, racing this one) is not replaced
    /// either: the WABA then fails at `store_token`, after your approval
    /// ran, so release what it reserved.
    Held,
    /// Failed at a step: `error` is an [`Error::Step`] naming it. When
    /// `steps_completed` includes `store_token` the token is stored and
    /// [`EmbeddedSignup::resume`] redoes the rest
    /// ([`Self::is_resumable`]); before it nothing was stored for this
    /// WABA, and the code is spent.
    Failed {
        /// Why, as an [`Error::Step`].
        error: Error,
        /// The steps that ran for this WABA before the failure, the shared
        /// ones first.
        steps_completed: Vec<&'static str>,
    },
}

impl WabaOutcome {
    /// The onboarding, when this WABA was onboarded.
    pub fn onboarded(&self) -> Option<&Onboarded> {
        match self {
            Self::Onboarded(done) => Some(done),
            _ => None,
        }
    }

    /// The error, when this WABA failed.
    pub fn error(&self) -> Option<&Error> {
        match self {
            Self::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

    /// Whether this WABA failed after its token was stored: resume it with
    /// [`EmbeddedSignup::resume`] (a request from
    /// [`OnboardingRequest::for_resume`] naming it).
    pub fn is_resumable(&self) -> bool {
        matches!(self, Self::Failed { steps_completed, .. } if steps_completed.contains(&steps::STORE_TOKEN))
    }
}

impl EmbeddedSignup {
    /// Onboard the WABAs of a signup, each on its own: the primary one (the
    /// claimed WABA, else the newest grant) as [`Self::onboard`] does and,
    /// with [`OnboardingRequest::every_granted_waba`] on, every other WABA
    /// the business token was granted. **Tech Provider mode only**: in
    /// Solution Partner mode this refuses before any request
    /// ([`CreditError::ApprovalRequired`](meta_whatsapp_core::error::CreditError::ApprovalRequired));
    /// use [`Self::onboard_wabas_with_approval`].
    ///
    /// The code is exchanged and the token inspected once
    /// (`exchange_code`, `debug_token`); with the option on, every WABA the
    /// session info names (`waba_id`, `waba_ids`) must be among the token's
    /// grants, or the call fails at `verify_assets` with nothing stored.
    /// Input errors, and a failure in these shared steps, fail the call (an
    /// [`Error::Step`] naming the step). From there, WABA by WABA, in
    /// [`WabasOnboarded::wabas`]' order, each runs `verify_assets` (its
    /// owner, its numbers), `store_token` (its own vault record and phone
    /// index), `subscribe_app`, and the rest of the tail; **a WABA that
    /// fails is reported in its [`WabaOutcome`] and the next one runs**.
    ///
    /// The primary WABA is onboarded with `request` as it is (its number
    /// registered with the PIN, a coexistence onboarding's sync). The
    /// others get no registration and no sync: the PIN, the region and the
    /// finish are the claimed number's, and both are once-only or
    /// rate-limited (resume such a WABA with a request of its own to
    /// register or sync it). Another WABA the vault already holds is left
    /// as it is ([`WabaOutcome::Held`]; one stored by another onboarding
    /// while this one runs fails at `store_token`, its record kept), the
    /// primary one is replaced, as `onboard` replaces it.
    ///
    /// **The others must be the primary WABA's business's**: each fails
    /// at `verify_assets`, before its numbers are read, approved or
    /// stored, when Meta reports another owner for it, or none, than the
    /// owner it reported for the primary WABA; and every one fails there,
    /// with no request, when that owner is unknown (the primary WABA
    /// failed before it was verified, or Meta reported no owner for it).
    ///
    /// With the option off, the result has one entry, the primary WABA's,
    /// whose failure is reported in it (not as an error, unlike
    /// `onboard`). Record each [`Onboarded::waba_id`] against the tenant
    /// that redeemed the signup, as for `onboard`.
    pub async fn onboard_wabas(
        &self,
        request: &OnboardingRequest,
        vault: &TokenVault,
    ) -> Result<WabasOnboarded> {
        self.onboard_wabas_inner(
            request,
            vault,
            None::<fn(VerifiedOnboarding) -> Ready<Result<()>>>,
        )
        .await
    }

    /// [`Self::onboard_wabas`], with your approval of **each** WABA before
    /// anything is stored, subscribed or shared for it (step `approve`, as
    /// in [`Self::onboard_with_approval`]). **Required in Solution Partner
    /// mode.**
    ///
    /// `approve` runs once per WABA, in order, right after that WABA's
    /// `verify_assets`, with what Meta confirmed for it. A refusal fails
    /// that WABA at `approve` (nothing written for it, no subscription, no
    /// system user, no credit line) and the next WABA runs. In Solution
    /// Partner mode each approval is recorded in that WABA's credit ledger
    /// for the token record it stores, so [`Self::resume`] of any of them
    /// shares a line only for a WABA approved so, and each WABA's credit
    /// line is checked, shared and recorded on its own. Reserve each WABA
    /// for the tenant atomically, as [`Self::onboard_with_approval`] shows.
    pub async fn onboard_wabas_with_approval<F, Fut>(
        &self,
        request: &OnboardingRequest,
        vault: &TokenVault,
        approve: F,
    ) -> Result<WabasOnboarded>
    where
        F: FnMut(VerifiedOnboarding) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        self.onboard_wabas_inner(request, vault, Some(approve))
            .await
    }

    async fn onboard_wabas_inner<F, Fut>(
        &self,
        request: &OnboardingRequest,
        vault: &TokenVault,
        mut approve: Option<F>,
    ) -> Result<WabasOnboarded>
    where
        F: FnMut(VerifiedOnboarding) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        request.validate()?;
        let code = required_code(request)?;
        let plan = self.onboarding_plan(request, approve.is_some())?;
        let mut shared = Vec::with_capacity(2);

        let token = self
            .exchange_code(code)
            .await
            .map_err(|e| e.in_step(steps::EXCHANGE_CODE))?;
        shared.push(steps::EXCHANGE_CODE);
        let granted = Granted {
            token: token.access_token,
            expires_in: token.expires_in,
            session: &request.session,
            owner: None,
            replaces: true,
        };
        let debug = self
            .debug_token(&granted.token)
            .await
            .map_err(|e| e.in_step(steps::DEBUG_TOKEN))?;
        shared.push(steps::DEBUG_TOKEN);
        let wabas = signup_wabas(
            &debug,
            &request.session,
            &self.app.app_id,
            request.every_granted_waba,
        )
        .map_err(|e| Error::from(e).in_step(steps::VERIFY_ASSETS))?;

        // The business the signup is about: the owner Meta reports for the
        // primary WABA, once verified. Every other grant must be its.
        let mut signup_owner: Option<BusinessId> = None;
        let mut out = Vec::with_capacity(wabas.len());
        for (at, waba_id) in wabas.into_iter().enumerate() {
            let primary = at == 0;
            let mut done = shared.clone();
            let onboarded = if primary {
                let waba = GrantedWaba {
                    granted: &granted,
                    debug: &debug,
                    waba_id: waba_id.clone(),
                    role: WabaRole::Primary(&mut signup_owner),
                };
                self.onboard_waba(
                    waba,
                    request,
                    vault,
                    plan.as_ref(),
                    approve.as_mut(),
                    &mut done,
                )
                .await
            } else {
                let signup = Signup {
                    granted: &granted,
                    debug: &debug,
                    request,
                    vault,
                    plan: plan.as_ref(),
                };
                let Some(onboarded) = self
                    .onboard_other(
                        &signup,
                        &waba_id,
                        signup_owner.as_ref(),
                        approve.as_mut(),
                        &mut done,
                    )
                    .await
                else {
                    out.push(WabaOnboarding {
                        waba_id,
                        primary,
                        outcome: WabaOutcome::Held,
                    });
                    continue;
                };
                onboarded
            };
            let outcome = match onboarded {
                Ok(done) => WabaOutcome::Onboarded(Box::new(done)),
                Err(error) => WabaOutcome::Failed {
                    error,
                    steps_completed: done,
                },
            };
            out.push(WabaOnboarding {
                waba_id,
                primary,
                outcome,
            });
        }
        Ok(WabasOnboarded { wabas: out })
    }

    /// Another grant of a signup: `None` when the vault holds it (left as
    /// it is, nothing requested), else its onboarding, with no number
    /// registered and no coexistence sync, its owner required to be
    /// `signup_owner` (the primary WABA's, as Meta reported it).
    async fn onboard_other<F, Fut>(
        &self,
        signup: &Signup<'_>,
        waba_id: &WabaId,
        signup_owner: Option<&BusinessId>,
        approve: Option<&mut F>,
        done: &mut Vec<&'static str>,
    ) -> Option<Result<Onboarded>>
    where
        F: FnMut(VerifiedOnboarding) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        // A token held for this WABA is not replaced by a signup that did
        // not claim it: this read skips it before any request, and
        // `store_token` stores only where the vault still holds none (one
        // stored meanwhile fails the WABA there). A read that fails fails
        // the WABA closed.
        match signup.vault.holds(waba_id).await {
            Ok(true) => return None,
            Ok(false) => {}
            Err(e) => return Some(Err(e.in_step(steps::VERIFY_ASSETS))),
        }
        let Some(owner) = signup_owner else {
            return Some(Err(Error::from(ValidationError::new(
                "owner_business_id",
                "the business of the signup is unknown (the claimed WABA was not verified, or Meta reported no owner for it), so this grant cannot be shown to be the same business's: not onboarded, nothing requested or stored for it",
            ))
            .in_step(steps::VERIFY_ASSETS)));
        };
        let other = signup.request.for_other_waba(waba_id);
        let granted = Granted {
            token: signup.granted.token.clone(),
            expires_in: signup.granted.expires_in,
            session: &other.session,
            owner: None,
            replaces: false,
        };
        let waba = GrantedWaba {
            granted: &granted,
            debug: signup.debug,
            waba_id: waba_id.clone(),
            role: WabaRole::Other(owner),
        };
        Some(
            self.onboard_waba(waba, &other, signup.vault, signup.plan, approve, done)
                .await,
        )
    }
}

/// What the WABAs of one signup share: the token, its inspection, the
/// request, the vault and the credit plan.
struct Signup<'a> {
    granted: &'a Granted<'a>,
    debug: &'a TokenDebug,
    request: &'a OnboardingRequest,
    vault: &'a TokenVault,
    plan: Option<&'a CreditPlan<'a>>,
}

/// The WABAs a signup onboards, the primary one first: the claimed WABA
/// (else the newest grant), checked as `onboard` checks it, then, when
/// `every` is set, the token's other grants in Meta's order, once every
/// WABA the session info names is found among them.
fn signup_wabas(
    debug: &TokenDebug,
    session: &SessionInfo,
    app_id: &AppId,
    every: bool,
) -> Result<Vec<WabaId>, ValidationError> {
    let primary = verify_grant(debug, session.primary_waba_id(), app_id)?;
    let mut wabas = vec![primary];
    if !every {
        return Ok(wabas);
    }
    // `verify_grant` refused a token without them.
    let granted = debug
        .target_ids(WHATSAPP_BUSINESS_MANAGEMENT)
        .unwrap_or_default();
    let unbacked = session
        .waba_id
        .iter()
        .chain(&session.waba_ids)
        .any(|claimed| !granted.iter().any(|g| g == claimed.as_str()));
    if unbacked {
        return Err(ValidationError::new(
            "waba_ids",
            "a WABA in the session info is not one this token was granted",
        ));
    }
    for id in granted {
        let waba = WabaId::new_unchecked(id.as_str());
        if !wabas.contains(&waba) {
            wabas.push(waba);
        }
    }
    Ok(wabas)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use http::Method;
    use meta_whatsapp_adapters::store::MemoryKvStore;
    use meta_whatsapp_core::clock::ManualClock;
    use meta_whatsapp_core::error::{CreditError, StorageError};
    use meta_whatsapp_core::ids::{AllocationConfigId, BusinessId, PhoneNumberId};
    use meta_whatsapp_core::secret::{AccessToken, SecretBytes};
    use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
    use meta_whatsapp_core::testing::{RecordedBody, RecordedRequest, ScriptedTransport};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use time::macros::datetime;

    use super::super::event::EmbeddedSignupEvent;
    use super::super::hosted::HostedSignup;
    use super::super::partner::{CreditSharing, SolutionPartner};
    use super::super::token::SignupCode;
    use super::super::vault::{StoredBusinessToken, VaultKey, VaultKeys};
    use super::steps::{
        APPROVE, ASSIGN_SYSTEM_USER, DEBUG_TOKEN, EXCHANGE_CODE, LOAD_TOKEN, REGISTER_PHONE,
        SHARE_CREDIT_LINE, STORE_TOKEN, SUBSCRIBE_APP, SYNC_CONTACTS, SYNC_HISTORY, VERIFY_ASSETS,
    };
    use super::*;
    use crate::credit_lines::WabaCurrency;
    use crate::phone_numbers::TwoStepPin;
    use crate::{AppCredentials, Client, RetryPolicy};

    const APP_ID: &str = "236484624622562";
    const APP_SECRET: &str = "614fc2afde15eee07a26b2fe3eaee9b9";
    const CODE: &str = "AQBhlXsctMxJYbwbrpybxlo9tLPGy";
    const TOKEN: &str = "EAAAN6tcBzAUBOwtDtTfmZCJ9n3FHpSDcDTH86ekf89Xnn";
    /// The claimed WABA (`embedded-signup/implementation`'s example id)
    /// and its number.
    const WABA_A: &str = "524126980791429";
    const PHONE_A: &str = "106540352242922";
    /// A second WABA the token grants, onboarded more recently: Meta lists
    /// it first (`solution-providers/manage-accounts`).
    const WABA_B: &str = "102289599326934";
    const PHONE_B: &str = "106540352242999";
    const BUSINESS: &str = "2729063490586005";
    const PIN: &str = "581063";
    const SYSTEM_TOKEN: &str = "EAAAN6tcBzAUBOZC82CW7iR2LiaZBwUHS4Y7FDtQ";
    const SYSTEM_USER: &str = "1972555232742222";
    const LINE: &str = "1972385232742146";
    const ALLOCATION_A: &str = "58501441721238";
    const ALLOCATION_B: &str = "58501441721239";
    const CREDENTIAL_A: &str = "7340125593810";

    struct Harness {
        t: ScriptedTransport,
        es: EmbeddedSignup,
        vault: TokenVault,
    }

    fn harness_with(partner: Option<SolutionPartner>) -> Harness {
        harness_on(Arc::new(MemoryKvStore::new()), partner)
    }

    fn harness_on(kv: Arc<dyn KvStore>, partner: Option<SolutionPartner>) -> Harness {
        let t = ScriptedTransport::new();
        let client = Client::builder()
            .transport(t.clone())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let vault = TokenVault::new(
            kv,
            VaultKeys::new(VaultKey::new("k1", SecretBytes::new([42; 32])).unwrap()),
        )
        .unwrap()
        .with_clock(Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC))));
        let es =
            client.embedded_signup(AppCredentials::new(AppId::new(APP_ID).unwrap(), APP_SECRET));
        let es = match partner {
            Some(p) => es.solution_partner(p),
            None => es,
        };
        Harness { t, es, vault }
    }

    fn harness() -> Harness {
        harness_with(None)
    }

    fn partner_harness() -> Harness {
        harness_with(Some(
            SolutionPartner::new(AccessToken::new(SYSTEM_TOKEN), SYSTEM_USER, LINE)
                .method(CreditSharing::ShareAndAttach),
        ))
    }

    fn waba(id: &str) -> WabaId {
        WabaId::new(id).unwrap()
    }

    fn phone(id: &str) -> PhoneNumberId {
        PhoneNumberId::new(id).unwrap()
    }

    /// The `FINISH` event of a multi-WABA flow
    /// (`embedded-signup/implementation`): the claimed WABA, its number,
    /// and `waba_ids`.
    fn event(waba_ids: &[&str]) -> EmbeddedSignupEvent {
        EmbeddedSignupEvent::from_value(json!({
            "data": {
                "phone_number_id": PHONE_A,
                "waba_id": WABA_A,
                "business_id": BUSINESS,
                "waba_ids": waba_ids
            },
            "type": "WA_EMBEDDED_SIGNUP",
            "event": "FINISH"
        }))
        .unwrap()
    }

    fn request(every: bool) -> OnboardingRequest {
        OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &event(&[WABA_A, WABA_B]))
            .unwrap()
            .register_with_pin(TwoStepPin::new(PIN).unwrap())
            .every_granted_waba(every)
    }

    /// `solution-providers/manage-accounts`' example answer, with this
    /// app's id and the two WABAs (the newest first).
    fn debug_response(wabas: &[&str]) -> Value {
        json!({"data": {
            "app_id": APP_ID,
            "application": "JaspersMarket",
            "data_access_expires_at": 1672092840,
            "expires_at": 0,
            "granular_scopes": [
                {"scope": "whatsapp_business_management", "target_ids": wabas},
                {"scope": "whatsapp_business_messaging", "target_ids": wabas}
            ],
            "is_valid": true,
            "scopes": ["whatsapp_business_management", "whatsapp_business_messaging", "public_profile"],
            "type": "SYSTEM_USER",
            "user_id": "10222270944537964"
        }})
    }

    fn owner_response(waba: &str) -> Value {
        json!({"owner_business_info": {"name": "Wind & Wool", "id": BUSINESS}, "id": waba})
    }

    fn phones_response(ids: &[&str]) -> Value {
        json!({"data": ids.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()})
    }

    fn success() -> Value {
        json!({"success": true})
    }

    fn graph_error(code: i64) -> Value {
        json!({"error": {"message": format!("(#{code}) x"), "type": "OAuthException", "code": code, "fbtrace_id": "A"}})
    }

    /// The code exchange and `debug_token`, granting `wabas`.
    fn script_shared(t: &ScriptedTransport, wabas: &[&str]) {
        t.push_json(200, json!({"access_token": TOKEN, "token_type": "bearer"}));
        t.push_json(200, debug_response(wabas));
    }

    /// A WABA's `verify_assets`: its owner, then its numbers.
    fn script_verified(t: &ScriptedTransport, waba: &str, phones: &[&str]) {
        t.push_json(200, owner_response(waba));
        t.push_json(200, phones_response(phones));
    }

    fn step(err: &Error) -> &'static str {
        match err {
            Error::Step { step, .. } => step,
            other => panic!("not a step error: {other}"),
        }
    }

    fn assert_request(r: &RecordedRequest, method: &Method, path: &str, bearer: &str) {
        assert_eq!(&r.method, method, "{path}");
        assert_eq!(r.path(), path);
        assert_eq!(r.bearer(), Some(bearer), "token of {path}");
    }

    fn assert_owner_lookup(r: &RecordedRequest, waba: &str) {
        assert_request(r, &Method::GET, &format!("/v25.0/{waba}"), TOKEN);
        assert_eq!(
            r.query("fields").as_deref(),
            Some("owner_business_info"),
            "{waba}"
        );
    }

    fn assert_numbers(r: &RecordedRequest, waba: &str) {
        assert_request(
            r,
            &Method::GET,
            &format!("/v25.0/{waba}/phone_numbers"),
            TOKEN,
        );
        assert_eq!(r.query("fields").as_deref(), Some("id"));
        assert_eq!(r.query("limit").as_deref(), Some("100"));
    }

    fn assert_subscribe(r: &RecordedRequest, waba: &str) {
        assert_request(
            r,
            &Method::POST,
            &format!("/v25.0/{waba}/subscribed_apps"),
            TOKEN,
        );
        assert_eq!(r.body, RecordedBody::Empty);
    }

    fn assert_register(r: &RecordedRequest, phone: &str) {
        assert_request(r, &Method::POST, &format!("/v25.0/{phone}/register"), TOKEN);
        assert_eq!(
            r.json(),
            Some(json!({"messaging_product": "whatsapp", "pin": PIN}))
        );
    }

    fn touches(reqs: &[RecordedRequest], id: &str) -> bool {
        reqs.iter().any(|r| r.path().contains(id))
    }

    fn onboarded<'a>(result: &'a WabasOnboarded, id: &str) -> &'a Onboarded {
        result
            .get(&waba(id))
            .and_then(|w| w.outcome.onboarded())
            .unwrap_or_else(|| panic!("{id} not onboarded: {result:?}"))
    }

    async fn stored(vault: &TokenVault, id: &str) -> Option<StoredBusinessToken> {
        vault.get(&waba(id)).await.unwrap()
    }

    /// Decisive (L11b), the option off: a signup whose token grants two
    /// WABAs onboards only the claimed one, through `onboard` and
    /// `onboard_wabas` alike; the other is neither read nor stored.
    #[tokio::test]
    async fn with_the_option_off_only_the_claimed_waba_is_onboarded() {
        for through_wabas in [false, true] {
            let h = harness();
            script_shared(&h.t, &[WABA_B, WABA_A]);
            script_verified(&h.t, WABA_A, &[PHONE_A]);
            h.t.push_json(200, success()); // subscribe A
            h.t.push_json(200, success()); // register A
            let request = request(false);
            let steps = if through_wabas {
                let result = h.es.onboard_wabas(&request, &h.vault).await.unwrap();
                assert_eq!(result.wabas.len(), 1, "{result:?}");
                assert!(result.is_complete());
                let primary = result.primary().unwrap();
                assert_eq!(primary.waba_id, waba(WABA_A));
                assert!(primary.primary);
                onboarded(&result, WABA_A).steps_completed.clone()
            } else {
                h.es.onboard(&request, &h.vault)
                    .await
                    .unwrap()
                    .steps_completed
            };
            assert_eq!(
                steps,
                [
                    EXCHANGE_CODE,
                    DEBUG_TOKEN,
                    VERIFY_ASSETS,
                    STORE_TOKEN,
                    SUBSCRIBE_APP,
                    REGISTER_PHONE
                ]
            );
            let reqs = h.t.requests();
            assert_eq!(reqs.len(), 6);
            assert_owner_lookup(&reqs[2], WABA_A);
            assert_numbers(&reqs[3], WABA_A);
            assert_subscribe(&reqs[4], WABA_A);
            assert_register(&reqs[5], PHONE_A);
            assert!(!touches(&reqs, WABA_B), "the other grant is not read");
            assert!(stored(&h.vault, WABA_A).await.is_some());
            assert!(stored(&h.vault, WABA_B).await.is_none());
            assert_eq!(h.t.remaining(), 0);
        }
    }

    /// Decisive (L11b), the option on: both WABAs onboarded, the claimed
    /// one first with its number registered, the other with its own record,
    /// phone index and subscription, and no registration.
    #[tokio::test]
    async fn with_the_option_on_every_granted_waba_is_onboarded() {
        let h = harness();
        script_shared(&h.t, &[WABA_B, WABA_A]);
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(200, success()); // subscribe A
        h.t.push_json(200, success()); // register A
        script_verified(&h.t, WABA_B, &[PHONE_B]);
        h.t.push_json(200, success()); // subscribe B
        let result = h.es.onboard_wabas(&request(true), &h.vault).await.unwrap();
        assert!(result.is_complete(), "{result:?}");
        let order: Vec<_> = result
            .wabas
            .iter()
            .map(|w| (w.waba_id.as_str(), w.primary))
            .collect();
        assert_eq!(order, [(WABA_A, true), (WABA_B, false)]);

        let a = onboarded(&result, WABA_A);
        assert_eq!(a.phone_number_id, Some(phone(PHONE_A)));
        assert_eq!(a.finish_kind, Some(super::super::FinishKind::Finish));
        assert_eq!(
            a.steps_completed,
            [
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                STORE_TOKEN,
                SUBSCRIBE_APP,
                REGISTER_PHONE
            ]
        );
        let b = onboarded(&result, WABA_B);
        assert_eq!(b.phone_number_id, None, "nothing claimed for it");
        assert_eq!(b.phone_number_ids, [phone(PHONE_B)]);
        assert_eq!(b.business_id, Some(BusinessId::new(BUSINESS).unwrap()));
        assert_eq!(b.finish_kind, None);
        assert_eq!(
            b.steps_completed,
            [
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                STORE_TOKEN,
                SUBSCRIBE_APP
            ]
        );
        assert_eq!(result.onboarded().count(), 2);

        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 9, "one code exchange, one debug_token");
        assert_eq!(reqs[0].path(), "/v25.0/oauth/access_token");
        assert_eq!(reqs[0].query("code").as_deref(), Some(CODE));
        assert_eq!(reqs[1].path(), "/v25.0/debug_token");
        assert_eq!(reqs[1].query("input_token").as_deref(), Some(TOKEN));
        assert_owner_lookup(&reqs[2], WABA_A);
        assert_numbers(&reqs[3], WABA_A);
        assert_subscribe(&reqs[4], WABA_A);
        assert_register(&reqs[5], PHONE_A);
        assert_owner_lookup(&reqs[6], WABA_B);
        assert_numbers(&reqs[7], WABA_B);
        assert_subscribe(&reqs[8], WABA_B);
        assert!(
            !reqs.iter().any(|r| r.path().contains(PHONE_B)),
            "the other WABA's number is not registered"
        );
        for (id, number) in [(WABA_A, PHONE_A), (WABA_B, PHONE_B)] {
            let record = stored(&h.vault, id).await.unwrap();
            assert_eq!(record.token.expose_secret(), TOKEN);
            assert_eq!(record.phone_number_ids, [phone(number)]);
            let routed = h
                .vault
                .get_by_phone_number(&phone(number))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(routed.waba_id, waba(id));
        }
        let debug_text = format!("{result:?}");
        assert!(!debug_text.contains(TOKEN), "{debug_text}");
        assert_eq!(h.t.remaining(), 0);
    }

    /// Decisive (L11b): a failure on the second WABA leaves the first
    /// stored and resumable, and resuming the second works.
    #[tokio::test]
    async fn a_failure_on_the_second_waba_leaves_the_first_and_both_resume() {
        let h = harness();
        script_shared(&h.t, &[WABA_B, WABA_A]);
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(200, success()); // subscribe A
        h.t.push_json(200, success()); // register A
        script_verified(&h.t, WABA_B, &[PHONE_B]);
        h.t.push_json(403, graph_error(200)); // subscribe B
        let result = h.es.onboard_wabas(&request(true), &h.vault).await.unwrap();
        assert!(!result.is_complete());
        let failures: Vec<_> = result.failures().map(|w| w.waba_id.as_str()).collect();
        assert_eq!(failures, [WABA_B]);
        onboarded(&result, WABA_A);
        let b = &result.get(&waba(WABA_B)).unwrap().outcome;
        let WabaOutcome::Failed {
            error,
            steps_completed,
        } = b
        else {
            panic!("{b:?}")
        };
        assert_eq!(step(error), SUBSCRIBE_APP);
        assert_eq!(error.kind(), meta_whatsapp_core::ErrorKind::Permission);
        assert_eq!(
            steps_completed,
            &[EXCHANGE_CODE, DEBUG_TOKEN, VERIFY_ASSETS, STORE_TOKEN]
        );
        assert!(b.is_resumable());
        let debug_text = format!("{result:?}");
        assert!(!debug_text.contains(TOKEN), "{debug_text}");
        assert!(stored(&h.vault, WABA_A).await.is_some());
        assert!(stored(&h.vault, WABA_B).await.is_some());
        assert_eq!(h.t.remaining(), 0);

        // The second resumes on its own: its stored token, its number.
        h.t.push_json(200, success());
        let session_b = SessionInfo {
            waba_id: Some(waba(WABA_B)),
            ..SessionInfo::default()
        };
        let resumed =
            h.es.resume(
                &waba(WABA_B),
                &OnboardingRequest::for_resume(session_b),
                &h.vault,
            )
            .await
            .unwrap();
        assert_eq!(
            resumed.steps_completed,
            [LOAD_TOKEN, VERIFY_ASSETS, SUBSCRIBE_APP]
        );
        assert_eq!(resumed.phone_number_id, Some(phone(PHONE_B)));
        assert_subscribe(&h.t.last_request().unwrap(), WABA_B);
        assert_eq!(h.t.remaining(), 0);

        // And so does the first, with the signup's own request.
        h.t.push_json(200, success());
        h.t.push_json(200, success());
        let resumed =
            h.es.resume(&waba(WABA_A), &request(true), &h.vault)
                .await
                .unwrap();
        assert_eq!(
            resumed.steps_completed,
            [LOAD_TOKEN, VERIFY_ASSETS, SUBSCRIBE_APP, REGISTER_PHONE]
        );
        let reqs = h.t.requests();
        assert_subscribe(&reqs[reqs.len() - 2], WABA_A);
        assert_register(&reqs[reqs.len() - 1], PHONE_A);
        assert_eq!(h.t.remaining(), 0);
    }

    /// Isolation the other way: the claimed WABA failing after it was
    /// verified (here at `subscribe_app`) does not stop the other one.
    #[tokio::test]
    async fn a_failure_on_the_claimed_waba_does_not_stop_the_others() {
        let h = harness();
        script_shared(&h.t, &[WABA_B, WABA_A]);
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(403, graph_error(200)); // subscribe A
        script_verified(&h.t, WABA_B, &[PHONE_B]);
        h.t.push_json(200, success()); // subscribe B
        let result = h.es.onboard_wabas(&request(true), &h.vault).await.unwrap();
        let a = &result.primary().unwrap().outcome;
        assert_eq!(step(a.error().unwrap()), SUBSCRIBE_APP);
        assert!(a.is_resumable(), "its token was stored");
        assert!(stored(&h.vault, WABA_A).await.is_some());
        onboarded(&result, WABA_B);
        assert!(stored(&h.vault, WABA_B).await.is_some());
        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 8);
        assert_subscribe(&reqs[4], WABA_A);
        assert_owner_lookup(&reqs[5], WABA_B);
        assert_subscribe(&reqs[7], WABA_B);
        assert_eq!(h.t.remaining(), 0);
    }

    /// The business a signup is about is the owner Meta reports for its
    /// claimed WABA. When that is unknown (the claimed WABA failed its
    /// verification, or Meta reported no owner for it), no other grant
    /// can be shown to be that business's: each fails closed at
    /// `verify_assets`, with no request made and nothing stored for it.
    #[tokio::test]
    async fn the_others_fail_closed_when_the_signup_business_is_unknown() {
        for owner_of_a in [None, Some(json!({"id": WABA_A}))] {
            let h = harness();
            script_shared(&h.t, &[WABA_B, WABA_A]);
            match &owner_of_a {
                // The owner lookup fails: A is not verified.
                None => {
                    h.t.push_json(403, graph_error(200));
                }
                // Meta reports no owner: A is onboarded, as by `onboard`.
                Some(answer) => {
                    h.t.push_json(200, answer.clone());
                    h.t.push_json(200, phones_response(&[PHONE_A]));
                    h.t.push_json(200, success()); // subscribe A
                    h.t.push_json(200, success()); // register A
                }
            }
            let result = h.es.onboard_wabas(&request(true), &h.vault).await.unwrap();
            let a = &result.primary().unwrap().outcome;
            assert_eq!(a.onboarded().is_some(), owner_of_a.is_some(), "{a:?}");
            let b = &result.get(&waba(WABA_B)).unwrap().outcome;
            let WabaOutcome::Failed {
                error,
                steps_completed,
            } = b
            else {
                panic!("{b:?}")
            };
            assert_eq!(step(error), VERIFY_ASSETS);
            assert!(
                matches!(error, Error::Step { source, .. } if matches!(&**source, Error::Validation(v) if v.field == "owner_business_id")),
                "{error}"
            );
            assert_eq!(steps_completed, &[EXCHANGE_CODE, DEBUG_TOKEN]);
            assert!(!b.is_resumable());
            assert!(!touches(&h.t.requests(), WABA_B), "nothing asked for B");
            assert!(stored(&h.vault, WABA_B).await.is_none());
            assert_eq!(h.t.remaining(), 0);
        }
    }

    /// A grant Meta reports another owner for than the claimed WABA's (or
    /// none) is not the signup's business's: it fails at `verify_assets`,
    /// before its numbers are read, nothing stored, approved or subscribed
    /// for it, while the claimed WABA is onboarded.
    #[tokio::test]
    async fn a_grant_of_another_business_is_not_onboarded() {
        let other_business = json!({"owner_business_info": {"name": "Other", "id": "9990001112223334"}, "id": WABA_B});
        for owner_of_b in [other_business, json!({"id": WABA_B})] {
            let h = harness();
            script_shared(&h.t, &[WABA_B, WABA_A]);
            script_verified(&h.t, WABA_A, &[PHONE_A]);
            h.t.push_json(200, success()); // subscribe A
            h.t.push_json(200, success()); // register A
            h.t.push_json(200, owner_of_b.clone());
            let approved = Mutex::new(Vec::new());
            let result =
                h.es.onboard_wabas_with_approval(&request(true), &h.vault, |verified| {
                    approved.lock().unwrap().push(verified.waba_id);
                    async { Ok(()) }
                })
                .await
                .unwrap();
            onboarded(&result, WABA_A);
            let b = &result.get(&waba(WABA_B)).unwrap().outcome;
            let error = b.error().unwrap_or_else(|| panic!("{b:?}"));
            assert_eq!(step(error), VERIFY_ASSETS, "{owner_of_b}");
            assert!(
                matches!(error, Error::Step { source, .. } if matches!(&**source, Error::Validation(v) if v.field == "owner_business_id")),
                "{error}"
            );
            assert_eq!(
                approved.into_inner().unwrap(),
                [waba(WABA_A)],
                "B not approved"
            );
            let reqs = h.t.requests();
            assert_eq!(reqs.len(), 7, "B's numbers not read");
            assert_owner_lookup(&reqs[6], WABA_B);
            assert!(stored(&h.vault, WABA_B).await.is_none());
            assert!(
                h.vault
                    .get_by_phone_number(&phone(PHONE_B))
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(h.t.remaining(), 0);
        }
    }

    /// The `Held` read is not a lock: a record another onboarding stores
    /// for a grant after it (here, inside the approval) is not replaced.
    /// `store_token` stores only where the vault still holds none, and the
    /// grant fails there, nothing stored or subscribed for it.
    #[tokio::test]
    async fn a_grant_stored_meanwhile_is_not_replaced() {
        let h = harness();
        script_shared(&h.t, &[WABA_B, WABA_A]);
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(200, success()); // subscribe A
        h.t.push_json(200, success()); // register A
        script_verified(&h.t, WABA_B, &[PHONE_B]);
        let vault = h.vault.clone();
        let result =
            h.es.onboard_wabas_with_approval(&request(true), &h.vault, |verified| {
                let vault = vault.clone();
                async move {
                    if verified.waba_id.as_str() == WABA_B {
                        // Another signup (or a hosted `PARTNER_ADDED`) of B.
                        let theirs =
                            StoredBusinessToken::new(waba(WABA_B), AccessToken::new("EAAtheirs"))
                                .phone_number_ids([phone(PHONE_B)]);
                        vault.store(&theirs).await?;
                    }
                    Ok(())
                }
            })
            .await
            .unwrap();
        onboarded(&result, WABA_A);
        let b = &result.get(&waba(WABA_B)).unwrap().outcome;
        let WabaOutcome::Failed {
            error,
            steps_completed,
        } = b
        else {
            panic!("{b:?}")
        };
        assert_eq!(step(error), STORE_TOKEN);
        assert_eq!(
            steps_completed,
            &[EXCHANGE_CODE, DEBUG_TOKEN, VERIFY_ASSETS, APPROVE]
        );
        assert!(!b.is_resumable(), "this signup stored nothing for it");
        assert_eq!(
            stored(&h.vault, WABA_B)
                .await
                .unwrap()
                .token
                .expose_secret(),
            "EAAtheirs"
        );
        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 8, "B not subscribed");
        assert_numbers(&reqs[7], WABA_B);
        assert_eq!(h.t.remaining(), 0);
    }

    /// A store whose reads of one key fail.
    #[derive(Debug)]
    struct UnreadableKv {
        inner: MemoryKvStore,
        key: String,
    }

    #[async_trait::async_trait]
    impl KvStore for UnreadableKv {
        async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
            if key.key() == self.key {
                return Err(StorageError::Backend(anyhow::anyhow!("unreachable")));
            }
            self.inner.get(key).await
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
            value: Option<Vec<u8>>,
            expiry: Expiry,
        ) -> Result<Option<u64>, StorageError> {
            self.inner
                .compare_and_swap(key, expected, value, expiry)
                .await
        }
        async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
            self.inner.delete(key).await
        }
    }

    /// Whether the vault holds another grant is read before anything is
    /// asked for it: a read that fails fails that grant closed (it may be
    /// held), not as absent.
    #[tokio::test]
    async fn a_grant_whose_record_cannot_be_read_fails_closed() {
        let h = harness_on(
            Arc::new(UnreadableKv {
                inner: MemoryKvStore::new(),
                key: format!("waba/{WABA_B}"),
            }),
            None,
        );
        script_shared(&h.t, &[WABA_B, WABA_A]);
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(200, success()); // subscribe A
        h.t.push_json(200, success()); // register A
        let result = h.es.onboard_wabas(&request(true), &h.vault).await.unwrap();
        onboarded(&result, WABA_A);
        let b = &result.get(&waba(WABA_B)).unwrap().outcome;
        assert_eq!(step(b.error().unwrap()), VERIFY_ASSETS);
        assert!(
            matches!(b.error().unwrap(), Error::Step { source, .. } if matches!(&**source, Error::Storage(_))),
            "{b:?}"
        );
        assert!(!touches(&h.t.requests(), WABA_B));
        assert_eq!(h.t.remaining(), 0);
    }

    /// With nothing claimed, the primary WABA is the newest grant (what
    /// `onboard` picks), then the others in Meta's order; each record
    /// carries the code exchange's expiry.
    #[tokio::test]
    async fn with_nothing_claimed_the_newest_grant_is_primary() {
        let h = harness();
        h.t.push_json(
            200,
            json!({"access_token": TOKEN, "token_type": "bearer", "expires_in": 3600}),
        );
        h.t.push_json(200, debug_response(&[WABA_B, WABA_A]));
        script_verified(&h.t, WABA_B, &[PHONE_B]);
        h.t.push_json(200, success()); // subscribe B
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(200, success()); // subscribe A
        let request =
            OnboardingRequest::new(SignupCode::new(CODE).unwrap(), SessionInfo::default())
                .every_granted_waba(true);
        let result = h.es.onboard_wabas(&request, &h.vault).await.unwrap();
        assert!(result.is_complete(), "{result:?}");
        let order: Vec<_> = result
            .wabas
            .iter()
            .map(|w| (w.waba_id.as_str(), w.primary))
            .collect();
        assert_eq!(order, [(WABA_B, true), (WABA_A, false)]);
        assert_eq!(result.primary().unwrap().waba_id, waba(WABA_B));
        let expires = datetime!(2026-09-24 13:00 UTC);
        for id in [WABA_A, WABA_B] {
            assert_eq!(
                onboarded(&result, id).token_expires_at,
                Some(expires),
                "{id}"
            );
            assert_eq!(
                stored(&h.vault, id).await.unwrap().expires_at,
                Some(expires)
            );
        }
        let reqs = h.t.requests();
        assert_owner_lookup(&reqs[2], WABA_B);
        assert_owner_lookup(&reqs[5], WABA_A);
        assert_eq!(h.t.remaining(), 0);
    }

    /// The other grants keep the request's subscription override: their
    /// webhooks go where the claimed WABA's go.
    #[tokio::test]
    async fn the_others_keep_the_subscription_override() {
        let h = harness();
        script_shared(&h.t, &[WABA_B, WABA_A]);
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(200, success()); // subscribe A
        h.t.push_json(200, success()); // register A
        script_verified(&h.t, WABA_B, &[PHONE_B]);
        h.t.push_json(200, success()); // subscribe B
        let request = request(true).subscribe_override(crate::waba::CallbackOverride::new(
            "https://hooks.example.com/wa",
            "verify-me-secret",
        ));
        let result = h.es.onboard_wabas(&request, &h.vault).await.unwrap();
        assert!(result.is_complete(), "{result:?}");
        let reqs = h.t.requests();
        let overridden = Some(
            json!({"override_callback_uri": "https://hooks.example.com/wa", "verify_token": "verify-me-secret"}),
        );
        for (at, id) in [(4, WABA_A), (8, WABA_B)] {
            assert_request(
                &reqs[at],
                &Method::POST,
                &format!("/v25.0/{id}/subscribed_apps"),
                TOKEN,
            );
            assert_eq!(reqs[at].json(), overridden, "{id}");
        }
        assert_eq!(h.t.remaining(), 0);
    }

    /// Another grant the vault already holds is left as it is: no request
    /// for it, its record unchanged.
    #[tokio::test]
    async fn another_granted_waba_the_vault_holds_is_left_as_it_is() {
        let h = harness();
        let held = StoredBusinessToken::new(waba(WABA_B), AccessToken::new("EAAheldToken"))
            .phone_number_ids([phone(PHONE_B)]);
        h.vault.store(&held).await.unwrap();
        script_shared(&h.t, &[WABA_B, WABA_A]);
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(200, success());
        h.t.push_json(200, success());
        let result = h.es.onboard_wabas(&request(true), &h.vault).await.unwrap();
        assert!(result.is_complete());
        assert!(matches!(
            result.get(&waba(WABA_B)).unwrap().outcome,
            WabaOutcome::Held
        ));
        assert!(!touches(&h.t.requests(), WABA_B));
        assert_eq!(
            stored(&h.vault, WABA_B)
                .await
                .unwrap()
                .token
                .expose_secret(),
            "EAAheldToken"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// The browser's `waba_ids` are claims: one the token was not granted
    /// refuses the whole signup before anything is read or stored.
    #[tokio::test]
    async fn a_claimed_waba_the_token_was_not_granted_refuses_the_signup() {
        let h = harness();
        script_shared(&h.t, &[WABA_B, WABA_A]);
        let request = OnboardingRequest::from_event(
            SignupCode::new(CODE).unwrap(),
            &event(&[WABA_A, "999999999999999"]),
        )
        .unwrap()
        .every_granted_waba(true);
        let err = h.es.onboard_wabas(&request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), VERIFY_ASSETS);
        assert!(
            matches!(&err, Error::Step { source, .. } if matches!(&**source, Error::Validation(v) if v.field == "waba_ids")),
            "{err}"
        );
        assert_eq!(h.t.requests().len(), 2);
        assert!(stored(&h.vault, WABA_A).await.is_none());
        assert!(stored(&h.vault, WABA_B).await.is_none());
        assert_eq!(h.t.remaining(), 0);

        // With the option off the extra claim is not read, as before.
        script_shared(&h.t, &[WABA_B, WABA_A]);
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(200, success());
        let result =
            h.es.onboard_wabas(&request.every_granted_waba(false), &h.vault)
                .await
                .unwrap();
        assert!(result.is_complete());
        assert_eq!(result.wabas.len(), 1);
        assert_eq!(h.t.remaining(), 0);
    }

    /// The calls that onboard one WABA refuse the option before any
    /// request, rather than drop the other WABAs.
    #[tokio::test]
    async fn one_waba_calls_refuse_the_option_before_any_request() {
        let h = harness();
        for err in [
            h.es.onboard(&request(true), &h.vault).await.unwrap_err(),
            h.es.onboard_with_approval(&request(true), &h.vault, |_| async { Ok(()) })
                .await
                .unwrap_err(),
            h.es.onboard_hosted(
                &HostedSignup::new(waba(WABA_A), BusinessId::new(BUSINESS).unwrap()),
                &AccessToken::new(SYSTEM_TOKEN),
                &OnboardingRequest::for_resume(SessionInfo::default()).every_granted_waba(true),
                &h.vault,
            )
            .await
            .unwrap_err(),
        ] {
            assert!(
                matches!(&err, Error::Validation(v) if v.field == "every_granted_waba"),
                "{err}"
            );
        }
        assert!(h.t.requests().is_empty());
    }

    /// Coexistence: the claimed number syncs; the other WABA's number
    /// neither syncs nor registers.
    #[tokio::test]
    async fn only_the_claimed_number_of_a_coexistence_signup_syncs() {
        let h = harness();
        let event = EmbeddedSignupEvent::from_value(json!({
            "data": {"waba_id": WABA_A},
            "type": "WA_EMBEDDED_SIGNUP",
            "event": "FINISH_WHATSAPP_BUSINESS_APP_ONBOARDING"
        }))
        .unwrap();
        let request = OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &event)
            .unwrap()
            .every_granted_waba(true);
        script_shared(&h.t, &[WABA_B, WABA_A]);
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(200, success()); // subscribe A
        h.t.push_json(
            200,
            json!({"messaging_product": "whatsapp", "request_id": "R-CONTACTS"}),
        );
        h.t.push_json(
            200,
            json!({"messaging_product": "whatsapp", "request_id": "R-HISTORY"}),
        );
        script_verified(&h.t, WABA_B, &[PHONE_B]);
        h.t.push_json(200, success()); // subscribe B
        let result = h.es.onboard_wabas(&request, &h.vault).await.unwrap();
        assert!(result.is_complete(), "{result:?}");
        let a = onboarded(&result, WABA_A);
        assert_eq!(
            a.steps_completed,
            [
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                STORE_TOKEN,
                SUBSCRIBE_APP,
                SYNC_CONTACTS,
                SYNC_HISTORY
            ]
        );
        assert!(a.coexistence_sync.is_some());
        let b = onboarded(&result, WABA_B);
        assert_eq!(b.coexistence_sync, None);
        assert!(!b.needs_coexistence_sync());
        let syncs: Vec<_> =
            h.t.requests()
                .iter()
                .filter(|r| r.path().ends_with("/smb_app_data"))
                .map(|r| r.path().to_owned())
                .collect();
        assert_eq!(
            syncs,
            [
                format!("/v25.0/{PHONE_A}/smb_app_data"),
                format!("/v25.0/{PHONE_A}/smb_app_data")
            ]
        );
        assert_eq!(h.t.remaining(), 0);
    }

    // Solution Partner mode.

    fn assert_assigned(r: &RecordedRequest, waba: &str) {
        assert_request(
            r,
            &Method::POST,
            &format!("/v25.0/{waba}/assigned_users"),
            SYSTEM_TOKEN,
        );
        assert_eq!(r.query("user").as_deref(), Some(SYSTEM_USER));
        assert_eq!(r.query("tasks").as_deref(), Some(r#"["MANAGE"]"#));
    }

    fn assert_lookup(r: &RecordedRequest) {
        assert_request(
            r,
            &Method::GET,
            &format!("/v25.0/{LINE}/owning_credit_allocation_configs"),
            SYSTEM_TOKEN,
        );
        assert_eq!(r.query("receiving_business_id").as_deref(), Some(BUSINESS));
        assert_eq!(r.query("fields").as_deref(), Some("id,receiving_business"));
    }

    fn assert_share(r: &RecordedRequest, waba: &str) {
        assert_request(
            r,
            &Method::POST,
            &format!("/v25.0/{LINE}/whatsapp_credit_sharing_and_attach"),
            SYSTEM_TOKEN,
        );
        assert_eq!(r.query("waba_currency").as_deref(), Some("USD"));
        assert_eq!(r.query("waba_id").as_deref(), Some(waba));
        assert_eq!(r.body, RecordedBody::Empty);
    }

    fn partner_request(every: bool) -> OnboardingRequest {
        request(every).currency(WabaCurrency::Usd)
    }

    /// A Solution Partner approves each WABA, and each approval is
    /// recorded for that WABA's own token record; each WABA gets its own
    /// share (the first WABA's allocation, found by the business-wide
    /// lookup, does not fund the second).
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one scenario, read top to bottom
    async fn a_solution_partner_approves_records_and_funds_each_waba() {
        let h = partner_harness();
        let err =
            h.es.onboard_wabas(&partner_request(true), &h.vault)
                .await
                .unwrap_err();
        assert!(
            matches!(&err, Error::Credit(CreditError::ApprovalRequired(_))),
            "{err}"
        );
        assert!(h.t.requests().is_empty(), "the code is not spent");

        script_shared(&h.t, &[WABA_B, WABA_A]);
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(200, success()); // subscribe A
        h.t.push_json(200, success()); // assigned_users A
        h.t.push_json(200, json!({"data": []})); // nothing shared yet
        h.t.push_json(
            200,
            json!({"allocation_config_id": ALLOCATION_A, "waba_id": WABA_A}),
        );
        h.t.push_json(200, success()); // register A
        script_verified(&h.t, WABA_B, &[PHONE_B]);
        h.t.push_json(200, success()); // subscribe B
        h.t.push_json(200, success()); // assigned_users B
        // The business's records now include A's allocation, active...
        h.t.push_json(
            200,
            json!({"data": [{"id": ALLOCATION_A, "receiving_business": {"name": "Wind & Wool", "id": BUSINESS}}]}),
        );
        h.t.push_json(
            200,
            json!({"receiving_business": {"name": "Wind & Wool", "id": BUSINESS}}),
        );
        // ...whose receiving credential is not B's funding.
        h.t.push_json(
            200,
            json!({"receiving_credential": {"id": CREDENTIAL_A}, "id": ALLOCATION_A}),
        );
        h.t.push_json(200, json!({"id": WABA_B}));
        h.t.push_json(
            200,
            json!({"allocation_config_id": ALLOCATION_B, "waba_id": WABA_B}),
        );
        let seen = Mutex::new(Vec::new());
        let result =
            h.es.onboard_wabas_with_approval(&partner_request(true), &h.vault, |verified| {
                seen.lock().unwrap().push(verified);
                async { Ok(()) }
            })
            .await
            .unwrap();
        assert!(result.is_complete(), "{result:?}");
        let seen = seen.into_inner().unwrap();
        let approved: Vec<_> = seen.iter().map(|v| v.waba_id.as_str()).collect();
        assert_eq!(approved, [WABA_A, WABA_B], "one approval per WABA");
        assert_eq!(seen[1].phone_number_ids, [phone(PHONE_B)]);
        assert_eq!(
            seen[1].business_id,
            Some(BusinessId::new(BUSINESS).unwrap())
        );

        let a = onboarded(&result, WABA_A);
        assert_eq!(
            a.steps_completed,
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
            a.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION_A))
        );
        let b = onboarded(&result, WABA_B);
        assert_eq!(
            b.steps_completed,
            [
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                APPROVE,
                STORE_TOKEN,
                SUBSCRIBE_APP,
                ASSIGN_SYSTEM_USER,
                SHARE_CREDIT_LINE
            ]
        );
        assert_eq!(
            b.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION_B))
        );

        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 18);
        assert_subscribe(&reqs[4], WABA_A);
        assert_assigned(&reqs[5], WABA_A);
        assert_lookup(&reqs[6]);
        assert_share(&reqs[7], WABA_A);
        assert_register(&reqs[8], PHONE_A);
        assert_owner_lookup(&reqs[9], WABA_B);
        assert_numbers(&reqs[10], WABA_B);
        assert_subscribe(&reqs[11], WABA_B);
        assert_assigned(&reqs[12], WABA_B);
        assert_lookup(&reqs[13]);
        assert_request(
            &reqs[14],
            &Method::GET,
            &format!("/v25.0/{ALLOCATION_A}"),
            SYSTEM_TOKEN,
        );
        assert_eq!(
            reqs[14].query("fields").as_deref(),
            Some("receiving_business,request_status")
        );
        assert_request(
            &reqs[15],
            &Method::GET,
            &format!("/v25.0/{ALLOCATION_A}"),
            SYSTEM_TOKEN,
        );
        assert_eq!(
            reqs[15].query("fields").as_deref(),
            Some("receiving_credential")
        );
        assert_request(&reqs[16], &Method::GET, &format!("/v25.0/{WABA_B}"), TOKEN);
        assert_eq!(
            reqs[16].query("fields").as_deref(),
            Some("primary_funding_id")
        );
        assert_share(&reqs[17], WABA_B);
        assert_eq!(h.t.remaining(), 0);

        // Each approval is recorded for its own WABA's token record, which
        // is what lets a plain `resume` of either share.
        for (id, allocation) in [(WABA_A, ALLOCATION_A), (WABA_B, ALLOCATION_B)] {
            let record = stored(&h.vault, id).await.unwrap();
            let credit = h.vault.credit(&waba(id)).await.unwrap().unwrap();
            assert_eq!(credit.approved_at, Some(datetime!(2026-09-24 12:00 UTC)));
            assert_eq!(credit.approved_token_created_at, record.created_at, "{id}");
            assert_eq!(
                credit.allocation_config_id,
                Some(AllocationConfigId::new(allocation))
            );
        }

        // A plain `resume` of the second WABA needs no approval again: its
        // own is recorded. Its line already funds it, so nothing is posted.
        let business = || json!({"name": "Wind & Wool", "id": BUSINESS});
        h.t.push_json(200, success()); // subscribe B
        h.t.push_json(200, success()); // assigned_users B
        h.t.push_json(
            200,
            json!({"data": [
                {"id": ALLOCATION_A, "receiving_business": business()},
                {"id": ALLOCATION_B, "receiving_business": business()}
            ]}),
        );
        h.t.push_json(200, json!({"receiving_business": business()}));
        h.t.push_json(200, json!({"receiving_business": business()}));
        h.t.push_json(
            200,
            json!({"receiving_credential": {"id": CREDENTIAL_A}, "id": ALLOCATION_A}),
        );
        h.t.push_json(
            200,
            json!({"primary_funding_id": "7340125593811", "id": WABA_B}),
        );
        h.t.push_json(
            200,
            json!({"receiving_credential": {"id": "7340125593811"}, "id": ALLOCATION_B}),
        );
        let session_b = SessionInfo {
            waba_id: Some(waba(WABA_B)),
            ..SessionInfo::default()
        };
        let resumed =
            h.es.resume(
                &waba(WABA_B),
                &OnboardingRequest::for_resume(session_b).currency(WabaCurrency::Usd),
                &h.vault,
            )
            .await
            .unwrap();
        assert_eq!(
            resumed.steps_completed,
            [
                LOAD_TOKEN,
                VERIFY_ASSETS,
                SUBSCRIBE_APP,
                ASSIGN_SYSTEM_USER,
                SHARE_CREDIT_LINE
            ]
        );
        assert_eq!(
            resumed.allocation_config_id,
            Some(AllocationConfigId::new(ALLOCATION_B))
        );
        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 26);
        assert!(
            !reqs[18..]
                .iter()
                .any(|r| r.path().ends_with("/whatsapp_credit_sharing_and_attach")),
            "nothing shared again"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// A refused approval stops that WABA only: nothing stored,
    /// subscribed or shared for it, no approval recorded, and it cannot be
    /// resumed; the other WABA is onboarded.
    #[tokio::test]
    async fn a_refused_approval_skips_that_waba_only() {
        let h = partner_harness();
        script_shared(&h.t, &[WABA_B, WABA_A]);
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(200, success()); // subscribe A
        h.t.push_json(200, success()); // assigned_users A
        h.t.push_json(200, json!({"data": []}));
        h.t.push_json(
            200,
            json!({"allocation_config_id": ALLOCATION_A, "waba_id": WABA_A}),
        );
        h.t.push_json(200, success()); // register A
        script_verified(&h.t, WABA_B, &[PHONE_B]);
        let result =
            h.es.onboard_wabas_with_approval(&partner_request(true), &h.vault, |verified| {
                let refuse = verified.waba_id.as_str() == WABA_B;
                async move {
                    if refuse {
                        Err(ValidationError::new("waba_id", "another tenant's").into())
                    } else {
                        Ok(())
                    }
                }
            })
            .await
            .unwrap();
        onboarded(&result, WABA_A);
        let b = &result.get(&waba(WABA_B)).unwrap().outcome;
        assert_eq!(step(b.error().unwrap()), APPROVE);
        assert!(!b.is_resumable());
        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 11, "nothing after B's verification");
        assert_numbers(&reqs[10], WABA_B);
        assert!(stored(&h.vault, WABA_B).await.is_none());
        assert!(h.vault.credit(&waba(WABA_B)).await.unwrap().is_none());
        assert!(stored(&h.vault, WABA_A).await.is_some());
        let err =
            h.es.resume(
                &waba(WABA_B),
                &OnboardingRequest::for_resume(SessionInfo::default()).currency(WabaCurrency::Usd),
                &h.vault,
            )
            .await
            .unwrap_err();
        assert_eq!(step(&err), LOAD_TOKEN);
        assert_eq!(h.t.remaining(), 0);
    }

    /// A revoked business's line is shared again for none of its WABAs
    /// unless the request opts in: the other grants keep the request's
    /// credit settings, `reshare_after_revocation` off included.
    #[tokio::test]
    async fn a_revoked_business_is_funded_again_for_no_waba() {
        let h = partner_harness();
        h.vault
            .mark_revoked(&BusinessId::new(BUSINESS).unwrap(), &[])
            .await
            .unwrap();
        script_shared(&h.t, &[WABA_B, WABA_A]);
        script_verified(&h.t, WABA_A, &[PHONE_A]);
        h.t.push_json(200, success()); // subscribe A
        h.t.push_json(200, success()); // assigned_users A
        script_verified(&h.t, WABA_B, &[PHONE_B]);
        h.t.push_json(200, success()); // subscribe B
        h.t.push_json(200, success()); // assigned_users B
        let result = h
            .es
            .onboard_wabas_with_approval(&partner_request(true), &h.vault, |_| async { Ok(()) })
            .await
            .unwrap();
        for id in [WABA_A, WABA_B] {
            let outcome = &result.get(&waba(id)).unwrap().outcome;
            let error = outcome.error().unwrap_or_else(|| panic!("{outcome:?}"));
            assert_eq!(step(error), SHARE_CREDIT_LINE, "{id}");
            assert!(
                matches!(error, Error::Step { source, .. } if matches!(&**source, Error::Credit(CreditError::Revoked { posted: false, .. }))),
                "{id}: {error}"
            );
            assert!(outcome.is_resumable());
        }
        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 10, "no lookup, no share");
        assert_assigned(&reqs[9], WABA_B);
        assert_eq!(h.t.remaining(), 0);
    }
}
