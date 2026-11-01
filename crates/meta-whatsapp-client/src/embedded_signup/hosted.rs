//! Hosted Embedded Signup (`embedded-signup/hosted-es`): Meta's own
//! onboarding page, with no JavaScript on yours. The page reports nothing
//! to your backend; Meta sends an `account_update` webhook with `event`
//! `PARTNER_ADDED` instead, whose `waba_info` names the customer's WABA
//! (`waba_id`) and business portfolio (`owner_business_id`). There is no
//! code to exchange: the business token is fetched from
//! `POST /{BUSINESS_PORTFOLIO_ID}/system_user_access_tokens` (the
//! customer's portfolio, not yours) with your system user token and its
//! `appsecret_proof` (step 5, through
//! [`Business::system_user_access_token`](crate::waba::Business::system_user_access_token)).
//! Then the onboarding guide of your partner type applies, "skip step 1":
//! the steps [`EmbeddedSignup::onboard`] runs after the code exchange.

use std::future::{Future, Ready};

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{BusinessId, WabaId};
use meta_whatsapp_core::secret::AccessToken;

use super::EmbeddedSignup;
use super::event::SessionInfo;
use super::onboard::{Granted, Onboarded, OnboardingRequest, VerifiedOnboarding, steps};
use super::vault::TokenVault;
use crate::waba::SystemUserTokenRequest;

/// A hosted Embedded Signup to onboard: what a `PARTNER_ADDED`
/// `account_update` names (`webhooks/reference/account_update`,
/// `waba_info.waba_id` and `waba_info.owner_business_id`), and how to ask
/// for the business token.
///
/// Built from a **signed** webhook (`meta-whatsapp-webhooks` verifies the
/// signature before it parses): the ids are Meta's, and onboarding checks
/// them again all the same (the token's grants, the WABA's owner).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostedSignup {
    /// The customer's WABA (`waba_info.waba_id`).
    pub waba_id: WabaId,
    /// The customer's business portfolio (`waba_info.owner_business_id`):
    /// the business whose token is fetched.
    pub owner_business_id: BusinessId,
    /// The `system_user_access_tokens` request: by default
    /// [`SystemUserTokenRequest::fetch_only`], what `hosted-es` sends.
    pub token_request: SystemUserTokenRequest,
}

impl HostedSignup {
    /// Onboard `waba_id` of `owner_business_id`, fetching the business's
    /// existing token (`fetch_only=true`, as `hosted-es` step 5 does).
    pub fn new(waba_id: impl Into<WabaId>, owner_business_id: impl Into<BusinessId>) -> Self {
        Self {
            waba_id: waba_id.into(),
            owner_business_id: owner_business_id.into(),
            token_request: SystemUserTokenRequest::fetch_only(),
        }
    }

    /// Ask for the token with `request` instead (e.g. naming a
    /// `system_user_id`). One without `fetch_only` **generates** a token,
    /// which `hosted-es` does not do: it is never replayed after a timeout
    /// or a transient error.
    #[must_use]
    pub fn token_request(mut self, request: SystemUserTokenRequest) -> Self {
        self.token_request = request;
        self
    }

    /// The session info of this signup: its WABA and owner business, no
    /// phone number. What [`OnboardingRequest::for_hosted`] starts from;
    /// save it to [`EmbeddedSignup::resume`] after a restart.
    pub fn session(&self) -> SessionInfo {
        SessionInfo {
            waba_id: Some(self.waba_id.clone()),
            business_id: Some(self.owner_business_id.clone()),
            ..SessionInfo::default()
        }
    }
}

impl EmbeddedSignup {
    /// Onboard a business that completed **hosted** Embedded Signup, from
    /// the `PARTNER_ADDED` webhook's ids: fetch its business token with
    /// `system_token` (your system user token) and the `appsecret_proof`
    /// of it under this app's secret (step `fetch_token`), then the steps
    /// of [`Self::onboard`] after the code exchange: inspect the token,
    /// verify the WABA (among the token's grants), its owner (Meta's must
    /// be `signup.owner_business_id`) and the number, store the token in
    /// `vault` (the same record the code flow writes), subscribe the app,
    /// and register the number if requested. **Tech Provider mode only**:
    /// a Solution Partner uses [`Self::onboard_hosted_with_approval`].
    ///
    /// `request` comes from [`OnboardingRequest::for_hosted`] (with
    /// [`OnboardingRequest::register_with_pin`] to register: the hosted
    /// page asks for no PIN, so the PIN is yours to choose and keep, as the
    /// onboarding guides' `<DESIRED_PIN>`). Refused before any request: a
    /// request with a code, one naming another WABA, one with
    /// [`OnboardingRequest::every_granted_waba`] on (Meta sends a
    /// `PARTNER_ADDED` per WABA: onboard each from its own), a blank system
    /// token or app secret, a token request that does not validate, and **a
    /// WABA `vault` already holds** (a [`ValidationError`] on `waba_id`).
    ///
    /// **`PARTNER_ADDED` is not only hosted signup's**: Meta sends it for
    /// every WABA shared with you, the code flow's onboardings and
    /// coexistence's included, and may deliver it again. Storing for a WABA
    /// already held would replace its token and record unasked (the code
    /// flow's `store_token` replaces, as re-onboarding must), so this
    /// refuses it: skip such a WABA (check [`TokenVault::get`] first, as
    /// the skill's example does), [`Self::resume`] it, or, to onboard it
    /// again on purpose, delete it from the vault first
    /// ([`TokenVault::delete`], or [`Self::offboard`]). The check is a read
    /// before the fetch, and `store_token` stores only where the vault
    /// still holds no record (`put_if_absent`): a code-flow or multi-WABA
    /// onboarding that stores the WABA after the read is never replaced,
    /// and this call then fails at `store_token`, nothing stored by it,
    /// after your approval ran (release what it reserved). The other way
    /// round, a code-flow onboarding still running when the webhook
    /// arrives can store after this one and replace it, as the code flow's
    /// re-onboarding must. Reserve the WABA for a tenant atomically in the
    /// approval to close that window, as [`Self::onboard_with_approval`]
    /// shows.
    ///
    /// **Nothing is spent before `store_token`**: the fetch is a read, so a
    /// failure up to `store_token` (an [`Error::Step`](meta_whatsapp_core::Error::Step)
    /// naming it) can be fixed and the call made again; a failure at
    /// `store_token` itself may have written the record before its phone
    /// index, and then the call again is refused as held: delete the WABA
    /// from the vault first. After it, the token is stored:
    /// [`Self::resume`] with the same request redoes only the steps after
    /// `store_token`, as for the code flow (calling this again is refused:
    /// the vault holds the WABA).
    ///
    /// The webhook names no tenant of yours: bind the WABA to one in
    /// [`Self::onboard_hosted_with_approval`].
    ///
    /// `debug_token` runs on the fetched token as on the exchanged one, so
    /// this assumes, as Meta's pages do not say otherwise, that hosted
    /// signup's business tokens carry `granular_scopes` naming the WABA:
    /// one that does not fails `verify_assets` (on `granular_scopes`)
    /// before anything is stored.
    pub async fn onboard_hosted(
        &self,
        signup: &HostedSignup,
        system_token: &AccessToken,
        request: &OnboardingRequest,
        vault: &TokenVault,
    ) -> Result<Onboarded> {
        self.onboard_hosted_inner(
            signup,
            system_token,
            request,
            vault,
            None::<fn(VerifiedOnboarding) -> Ready<Result<()>>>,
        )
        .await
    }

    /// [`Self::onboard_hosted`], with your approval of the verified assets
    /// before anything is stored, subscribed or shared (step `approve`), as
    /// in [`Self::onboard_with_approval`]. **Required in Solution Partner
    /// mode** (`hosted-es` requires a Solution Partner to have a line of
    /// credit; it is shared as in the code flow). The place to bind the
    /// WABA to one of your tenants: the webhook names none.
    pub async fn onboard_hosted_with_approval<F, Fut>(
        &self,
        signup: &HostedSignup,
        system_token: &AccessToken,
        request: &OnboardingRequest,
        vault: &TokenVault,
        approve: F,
    ) -> Result<Onboarded>
    where
        F: FnOnce(VerifiedOnboarding) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        self.onboard_hosted_inner(signup, system_token, request, vault, Some(approve))
            .await
    }

    async fn onboard_hosted_inner<F, Fut>(
        &self,
        signup: &HostedSignup,
        system_token: &AccessToken,
        request: &OnboardingRequest,
        vault: &TokenVault,
        approve: Option<F>,
    ) -> Result<Onboarded>
    where
        F: FnOnce(VerifiedOnboarding) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        request.validate()?;
        check_hosted(signup, system_token, request)?;
        if self.app.app_secret.expose_secret().trim().is_empty() {
            return Err(ValidationError::new(
                "app_secret",
                "required to sign the system user token (appsecret_proof)",
            )
            .into());
        }
        let plan = self.onboarding_plan(request, approve.is_some())?;
        // `PARTNER_ADDED` also comes for a WABA the code flow (or an earlier
        // hosted run, or a replayed webhook) onboarded: storing again would
        // replace its token and record unasked. One read, before any
        // request; replacing a held WABA is deleting it first.
        if vault.holds(&signup.waba_id).await? {
            return Err(ValidationError::new(
                "waba_id",
                "the vault already holds this WABA (onboarded by the code flow, an earlier hosted run or a redelivered PARTNER_ADDED): skip it, resume it with EmbeddedSignup::resume, or delete it from the vault first to onboard it again",
            )
            .into());
        }
        let mut done = Vec::with_capacity(10);

        // The proof is computed from the app secret, borrowed, and sent in
        // the form body; the token goes in the header, nothing in the URL.
        let token = self
            .client
            .with_token(system_token.clone())
            .business(signup.owner_business_id.clone())
            .system_user_access_token(&self.app.app_secret, &signup.token_request)
            .await
            .map_err(|e| e.in_step(steps::FETCH_TOKEN))?;
        done.push(steps::FETCH_TOKEN);

        // The webhook's WABA, whatever the request's session names (checked
        // equal above); its other claims (a phone number) are verified.
        let mut session = request.session.clone();
        session.waba_id = Some(signup.waba_id.clone());
        let granted = Granted {
            token: token.access_token,
            // `system_user_access_tokens` answers `{"access_token"}` alone.
            expires_in: None,
            session: &session,
            owner: Some(&signup.owner_business_id),
            // The `holds` read above is not a lock: a code-flow or
            // multi-WABA onboarding storing the WABA since is not replaced.
            replaces: false,
        };
        self.onboard_granted(granted, request, vault, plan, approve, done)
            .await
    }
}

/// What a hosted onboarding refuses before any request.
fn check_hosted(
    signup: &HostedSignup,
    system_token: &AccessToken,
    request: &OnboardingRequest,
) -> Result<(), ValidationError> {
    if request.code.is_some() {
        return Err(ValidationError::new(
            "code",
            "hosted Embedded Signup exchanges no code: build the request with OnboardingRequest::for_hosted",
        ));
    }
    // `PARTNER_ADDED` comes once per WABA shared with the app: each is
    // onboarded from its own webhook, and a WABA held is refused.
    request.refuse_every_granted_waba(
        "a PARTNER_ADDED webhook names one WABA, and Meta sends one for each WABA shared with your app: onboard each from its own webhook",
    )?;
    if request
        .session
        .primary_waba_id()
        .is_some_and(|w| w != &signup.waba_id)
    {
        return Err(ValidationError::new(
            "waba_id",
            "the request's session info names another WABA than the PARTNER_ADDED webhook",
        ));
    }
    if system_token.expose_secret().trim().is_empty() {
        return Err(ValidationError::new(
            "system_token",
            "your system user token is required to fetch the business token",
        ));
    }
    signup.token_request.validate()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use hmac::{Hmac, Mac};
    use http::Method;
    use meta_whatsapp_adapters::store::MemoryKvStore;
    use meta_whatsapp_core::clock::ManualClock;
    use meta_whatsapp_core::error::{CreditError, StorageError, TransportError};
    use meta_whatsapp_core::ids::{AppId, PhoneNumberId};
    use meta_whatsapp_core::secret::SecretBytes;
    use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
    use meta_whatsapp_core::testing::{RecordedBody, RecordedRequest, ScriptedTransport};
    use meta_whatsapp_core::{Error, ErrorKind};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use sha2::Sha256;
    use time::macros::datetime;

    use super::super::SolutionPartner;
    use super::super::event::EmbeddedSignupEvent;
    use super::super::onboard::tests::Capture;
    use super::super::token::SignupCode;
    use super::super::vault::tests::RecordingKv;
    use super::super::vault::{TOKEN_NAMESPACE, VaultKey, VaultKeys};
    use super::steps::{
        APPROVE, DEBUG_TOKEN, EXCHANGE_CODE, FETCH_TOKEN, LOAD_TOKEN, REGISTER_PHONE, STORE_TOKEN,
        SUBSCRIBE_APP, VERIFY_ASSETS,
    };
    use super::*;
    use crate::phone_numbers::TwoStepPin;
    use crate::{AppCredentials, Client, RetryPolicy};

    const APP_ID: &str = "236484624622562";
    const APP_SECRET: &str = "614fc2afde15eee07a26b2fe3eaee9b9";
    const WRONG_SECRET: &str = "a-wrong-app-secret";
    /// The partner's system user token: what the proof signs.
    const SYSTEM_TOKEN: &str = "EAAJB-PARTNER-SYSTEM-TOKEN";
    /// `echo -n EAAJB-PARTNER-SYSTEM-TOKEN | openssl dgst -sha256 -hmac
    /// 614fc2afde15eee07a26b2fe3eaee9b9` (`hosted-es`, step 4), computed
    /// outside this crate.
    const PROOF: &str = "dc5f466f5a81cf17e395e091a0a86c1abdf0a286f44fff9f8be1128b497e9723";
    /// The same under [`WRONG_SECRET`].
    const WRONG_PROOF: &str = "0e2a2979865a5c21fbc7b2651e8c2381c6355a75a5712cd6376f756afdb8a372";
    /// The customer's business token in the answer.
    const BUSINESS_TOKEN: &str = "EAAN-HOSTED-BUSINESS-TOKEN-SENTINEL";
    /// `webhooks/reference/account_update`, § Partner added.
    const WABA: &str = "980198427658004";
    const OWNER: &str = "2329417887457253";
    const PHONE: &str = "106540352242922";
    const OTHER_PHONE: &str = "106540352242999";
    const PIN: &str = "581063";
    const CODE: &str = "AQBhlXsctMxJYbwbrpybxlo9tLPGy";

    /// A store whose writes fail while `broken` is set, and whose phone
    /// index writes fail while `broken_index` is.
    #[derive(Debug, Default)]
    struct FlakyKv {
        inner: MemoryKvStore,
        broken: AtomicBool,
        broken_index: AtomicBool,
    }

    impl FlakyKv {
        fn check(&self, key: &StoreKey) -> Result<(), StorageError> {
            let index = key.to_string().contains("/phone/");
            if self.broken.load(Ordering::SeqCst)
                || (index && self.broken_index.load(Ordering::SeqCst))
            {
                Err(StorageError::Backend(anyhow::anyhow!("disk full")))
            } else {
                Ok(())
            }
        }
    }

    #[async_trait::async_trait]
    impl KvStore for FlakyKv {
        async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
            self.inner.get(key).await
        }
        async fn put(
            &self,
            key: &StoreKey,
            value: Vec<u8>,
            expiry: Expiry,
        ) -> Result<u64, StorageError> {
            self.check(key)?;
            self.inner.put(key, value, expiry).await
        }
        async fn put_if_absent(
            &self,
            key: &StoreKey,
            value: Vec<u8>,
            expiry: Expiry,
        ) -> Result<Option<u64>, StorageError> {
            self.check(key)?;
            self.inner.put_if_absent(key, value, expiry).await
        }
        async fn compare_and_swap(
            &self,
            key: &StoreKey,
            expected: u64,
            value: Option<Vec<u8>>,
            expiry: Expiry,
        ) -> Result<Option<u64>, StorageError> {
            self.check(key)?;
            self.inner
                .compare_and_swap(key, expected, value, expiry)
                .await
        }
        async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
            self.check(key)?;
            self.inner.delete(key).await
        }
    }

    struct Harness {
        t: ScriptedTransport,
        es: EmbeddedSignup,
        vault: TokenVault,
    }

    fn harness_with(kv: Arc<dyn KvStore>, secret: &str, retry: RetryPolicy) -> Harness {
        let t = ScriptedTransport::new();
        // A default token on purpose: the token request must send the
        // system token, the later steps the business token.
        let client = Client::builder()
            .transport(t.clone())
            .access_token("DEFAULT_CLIENT_TOKEN")
            .retry(retry)
            .build()
            .unwrap();
        let vault = TokenVault::new(
            kv,
            VaultKeys::new(VaultKey::new("k1", SecretBytes::new([42; 32])).unwrap()),
        )
        .unwrap()
        .with_clock(Arc::new(ManualClock::new(datetime!(2026-11-01 12:00 UTC))));
        Harness {
            es: client.embedded_signup(AppCredentials::new(AppId::new(APP_ID).unwrap(), secret)),
            t,
            vault,
        }
    }

    fn harness() -> Harness {
        harness_with(
            Arc::new(MemoryKvStore::new()),
            APP_SECRET,
            RetryPolicy::NONE,
        )
    }

    fn retrying() -> RetryPolicy {
        RetryPolicy {
            max_retries: 2,
            base_delay: std::time::Duration::ZERO,
            max_delay: std::time::Duration::ZERO,
        }
    }

    fn system_token() -> AccessToken {
        AccessToken::new(SYSTEM_TOKEN)
    }

    fn signup() -> HostedSignup {
        HostedSignup::new(WabaId::new(WABA).unwrap(), BusinessId::new(OWNER).unwrap())
    }

    fn request() -> OnboardingRequest {
        OnboardingRequest::for_hosted(&signup()).register_with_pin(TwoStepPin::new(PIN).unwrap())
    }

    /// The proof computed apart from the code under test, as `hosted-es`
    /// step 4 does it: HMAC-SHA256 of the system token keyed with the app
    /// secret, hex.
    fn hmac_hex(secret: &str, token: &str) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(token.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// The form body of `req`, in order.
    fn form(req: &RecordedRequest) -> Vec<(String, String)> {
        let RecordedBody::Bytes { content_type, data } = &req.body else {
            panic!("expected a buffered body, got {:?}", req.body);
        };
        assert_eq!(content_type, "application/x-www-form-urlencoded");
        url::form_urlencoded::parse(data)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    /// `hosted-es`, step 5: exactly the proof and `fetch_only=true`.
    fn fetch_form(proof: &str) -> Vec<(String, String)> {
        vec![
            ("appsecret_proof".to_owned(), proof.to_owned()),
            ("fetch_only".to_owned(), "true".to_owned()),
        ]
    }

    /// The token request: `hosted-es` step 5's syntax, on the customer's
    /// portfolio, the system token as bearer, nothing in the URL.
    fn assert_token_request(req: &RecordedRequest, proof: &str) {
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.path(),
            format!("/v25.0/{OWNER}/system_user_access_tokens")
        );
        assert_eq!(req.url.query(), None, "nothing in the URL");
        assert_eq!(req.bearer(), Some(SYSTEM_TOKEN));
        assert_eq!(form(req), fetch_form(proof));
    }

    /// `hosted-es`, step 5's response syntax.
    fn token_answer() -> Value {
        json!({"access_token": BUSINESS_TOKEN})
    }

    /// `debug_token` of a business token granted `waba` (the shape of
    /// `solution-providers/manage-accounts`; `expires_at` 0: no expiry).
    fn debug_answer(waba: &str) -> Value {
        json!({"data": {
          "app_id": APP_ID, "type": "SYSTEM_USER", "application": "Jaspers", "is_valid": true,
          "expires_at": 0, "data_access_expires_at": 0,
          "scopes": ["whatsapp_business_management", "whatsapp_business_messaging"],
          "granular_scopes": [
            {"scope": "whatsapp_business_management", "target_ids": [waba]},
            {"scope": "whatsapp_business_messaging", "target_ids": [waba]}
          ],
          "user_id": "1"
        }})
    }

    fn owner_answer(business: &str) -> Value {
        json!({"owner_business_info": {"name": "Wind & Wool", "id": business}, "id": WABA})
    }

    /// `hosted-es`, step 6's response syntax, with values: the most
    /// recently onboarded number first.
    fn phones_answer() -> Value {
        json!({"data": [
          {
            "verified_name": "Wind & Wool", "code_verification_status": "VERIFIED",
            "display_phone_number": "+1 555-010-0101", "quality_rating": "GREEN",
            "platform_type": "CLOUD_API", "throughput": {"level": "STANDARD"},
            "last_onboarded_time": "2026-08-17T19:05:47+0000",
            "webhook_configuration": {"application": "https://hooks.example.com/wa"},
            "id": PHONE
          },
          {"display_phone_number": "+1 555-010-0102", "id": OTHER_PHONE}
        ]})
    }

    fn success() -> Value {
        json!({"success": true})
    }

    fn graph_error(code: i64) -> Value {
        json!({"error": {"message": format!("(#{code}) x"), "type": "OAuthException", "code": code, "fbtrace_id": "A"}})
    }

    /// Every answer of a successful hosted onboarding, with registration.
    fn script_hosted(t: &ScriptedTransport) {
        t.push_json(200, token_answer());
        script_after_token(t);
    }

    /// The answers after the token: the same for both flows.
    fn script_after_token(t: &ScriptedTransport) {
        t.push_json(200, debug_answer(WABA));
        t.push_json(200, owner_answer(OWNER));
        t.push_json(200, phones_answer());
        t.push_json(200, success());
        t.push_json(200, success());
    }

    fn step(err: &Error) -> &'static str {
        match err {
            Error::Step { step, .. } => step,
            other => panic!("not a step error: {other}"),
        }
    }

    fn field(err: &Error) -> &str {
        match err {
            Error::Validation(v) => &v.field,
            Error::Step { source, .. } => field(source),
            other => panic!("not a validation error: {other:?}"),
        }
    }

    async fn stored(h: &Harness) -> Option<super::super::StoredBusinessToken> {
        h.vault.get(&WabaId::new(WABA).unwrap()).await.unwrap()
    }

    fn is_token_request(r: &RecordedRequest) -> bool {
        r.path().ends_with("/system_user_access_tokens")
    }

    /// The decisive test: a scripted `PARTNER_ADDED` (Meta's example ids)
    /// leads to a token request carrying the proof of the app secret, then
    /// to the code flow's steps with the business token.
    #[tokio::test]
    async fn a_partner_added_fetches_the_token_with_the_proof_and_onboards() {
        assert_eq!(hmac_hex(APP_SECRET, SYSTEM_TOKEN), PROOF, "fixed vector");
        let h = harness();
        script_hosted(&h.t);
        let done =
            h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                .await
                .unwrap();
        assert_eq!(
            done.steps_completed,
            vec![
                FETCH_TOKEN,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                STORE_TOKEN,
                SUBSCRIBE_APP,
                REGISTER_PHONE
            ]
        );
        assert_eq!(done.waba_id.as_str(), WABA);
        assert_eq!(
            done.business_id.as_ref().map(BusinessId::as_str),
            Some(OWNER)
        );
        assert_eq!(
            done.phone_number_id.as_ref().map(PhoneNumberId::as_str),
            Some(PHONE),
            "the most recently onboarded number (step 6)"
        );
        assert_eq!(done.token.expose_secret(), BUSINESS_TOKEN);
        assert_eq!(done.finish_kind, None);

        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 6);
        assert_token_request(&reqs[0], PROOF);
        assert_eq!(form(&reqs[0])[0].1, hmac_hex(APP_SECRET, SYSTEM_TOKEN));
        // debug_token: the business token, inspected with the app token.
        assert_eq!(reqs[1].method, Method::GET);
        assert_eq!(reqs[1].path(), "/v25.0/debug_token");
        assert_eq!(
            reqs[1].query("input_token").as_deref(),
            Some(BUSINESS_TOKEN)
        );
        assert_eq!(
            reqs[1].bearer(),
            Some(format!("{APP_ID}|{APP_SECRET}").as_str())
        );
        // The WABA's owner and numbers, with the business token.
        assert_eq!(reqs[2].path(), format!("/v25.0/{WABA}"));
        assert_eq!(
            reqs[2].query("fields").as_deref(),
            Some("owner_business_info")
        );
        assert_eq!(reqs[2].bearer(), Some(BUSINESS_TOKEN));
        assert_eq!(reqs[3].path(), format!("/v25.0/{WABA}/phone_numbers"));
        assert_eq!(reqs[3].bearer(), Some(BUSINESS_TOKEN));
        // Subscribe, then register with your PIN.
        assert_eq!(reqs[4].method, Method::POST);
        assert_eq!(reqs[4].path(), format!("/v25.0/{WABA}/subscribed_apps"));
        assert_eq!(reqs[4].body, RecordedBody::Empty);
        assert_eq!(reqs[4].bearer(), Some(BUSINESS_TOKEN));
        assert_eq!(reqs[5].path(), format!("/v25.0/{PHONE}/register"));
        assert_eq!(reqs[5].bearer(), Some(BUSINESS_TOKEN));
        assert_eq!(
            reqs[5].json(),
            Some(json!({"messaging_product": "whatsapp", "pin": PIN}))
        );
        assert_eq!(h.t.remaining(), 0);

        let stored = stored(&h).await.unwrap();
        assert_eq!(stored.token.expose_secret(), BUSINESS_TOKEN);
        assert_eq!(
            stored.phone_number_ids,
            vec![
                PhoneNumberId::new(PHONE).unwrap(),
                PhoneNumberId::new(OTHER_PHONE).unwrap()
            ]
        );
        for phone in [PHONE, OTHER_PHONE] {
            let routed = h
                .vault
                .get_by_phone_number(&PhoneNumberId::new(phone).unwrap())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(routed.waba_id.as_str(), WABA);
        }
    }

    /// The same flow under another app secret signs the system token
    /// otherwise: the decisive test's request assertion would fail.
    #[tokio::test]
    async fn a_wrong_app_secret_sends_another_proof() {
        assert_eq!(hmac_hex(WRONG_SECRET, SYSTEM_TOKEN), WRONG_PROOF);
        let h = harness_with(
            Arc::new(MemoryKvStore::new()),
            WRONG_SECRET,
            RetryPolicy::NONE,
        );
        // Meta refuses a proof that does not match the token.
        h.t.push_json(
            400,
            json!({"error": {"message": "Invalid appsecret_proof provided in the API argument", "type": "GraphMethodException", "code": 100, "fbtrace_id": "A"}}),
        );
        let err =
            h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), FETCH_TOKEN);
        let req = h.t.last_request().unwrap();
        assert_ne!(form(&req), fetch_form(PROOF), "not the app secret's proof");
        assert_token_request(&req, WRONG_PROOF);
        assert!(stored(&h).await.is_none(), "nothing stored");
        assert_eq!(h.t.requests().len(), 1);
        assert_eq!(h.t.remaining(), 0);
    }

    /// The writes of a store, the random parts of a token record (its
    /// nonce, and so its ciphertext) taken out.
    fn clear(writes: Vec<(String, Vec<u8>)>) -> Vec<(String, Value)> {
        writes
            .into_iter()
            .map(|(key, value)| {
                let mut json: Value = serde_json::from_slice(&value).unwrap();
                if let Some(record) = json.as_object_mut() {
                    for random in ["nonce", "ciphertext"] {
                        assert_eq!(
                            record.remove(random).is_some(),
                            key.contains("/waba/"),
                            "{key}: {random}"
                        );
                    }
                }
                (key, json)
            })
            .collect()
    }

    /// The requests after the token's: method, URL, bearer and body.
    fn tail(t: &ScriptedTransport) -> Vec<(Method, String, Option<String>, RecordedBody)> {
        t.requests()[1..]
            .iter()
            .map(|r| {
                (
                    r.method.clone(),
                    r.url.to_string(),
                    r.bearer().map(str::to_owned),
                    r.body.clone(),
                )
            })
            .collect()
    }

    /// The hosted flow writes the code flow's record: the same keys, in the
    /// same order, the same clear fields and the same sealed record (only
    /// the random nonce, and so the ciphertext, differ).
    #[tokio::test]
    async fn the_stored_record_is_the_code_flows() {
        let code_kv = Arc::new(RecordingKv::default());
        let code = harness_with(code_kv.clone(), APP_SECRET, RetryPolicy::NONE);
        code.t.push_json(
            200,
            json!({"access_token": BUSINESS_TOKEN, "token_type": "bearer"}),
        );
        script_after_token(&code.t);
        let event = EmbeddedSignupEvent::from_value(json!({
            "data": {"phone_number_id": PHONE, "waba_id": WABA, "business_id": OWNER},
            "type": "WA_EMBEDDED_SIGNUP", "event": "FINISH"
        }))
        .unwrap();
        let code_request = OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &event)
            .unwrap()
            .register_with_pin(TwoStepPin::new(PIN).unwrap());
        let by_code = code.es.onboard(&code_request, &code.vault).await.unwrap();
        assert_eq!(by_code.steps_completed[0], EXCHANGE_CODE);

        let hosted_kv = Arc::new(RecordingKv::default());
        let hosted = harness_with(hosted_kv.clone(), APP_SECRET, RetryPolicy::NONE);
        script_hosted(&hosted.t);
        let by_hosted = hosted
            .es
            .onboard_hosted(&signup(), &system_token(), &request(), &hosted.vault)
            .await
            .unwrap();
        assert_eq!(by_hosted.steps_completed[0], FETCH_TOKEN);

        // The writes, key by key.
        let code_writes = clear(code_kv.writes());
        let hosted_writes = clear(hosted_kv.writes());
        assert_eq!(
            code_writes
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>(),
            [
                format!("{TOKEN_NAMESPACE}/waba/{WABA}"),
                format!("{TOKEN_NAMESPACE}/phone/{PHONE}"),
                format!("{TOKEN_NAMESPACE}/phone/{OTHER_PHONE}"),
            ]
        );
        assert_eq!(hosted_writes, code_writes);
        assert_eq!(
            code_writes[0].1,
            json!({"v": 1, "kid": "k1", "waba_id": WABA, "business_id": OWNER,
                   "phone_number_ids": [PHONE, OTHER_PHONE], "created_at": 1_793_534_400,
                   "expires_at": null}),
            "the vault's format, `wa.token` v1"
        );

        // The sealed record, decrypted.
        let a = stored(&code).await.unwrap();
        let b = stored(&hosted).await.unwrap();
        assert_eq!(a.token.expose_secret(), b.token.expose_secret());
        assert_eq!(
            (
                &a.waba_id,
                &a.business_id,
                &a.phone_number_ids,
                a.created_at,
                a.expires_at
            ),
            (
                &b.waba_id,
                &b.business_id,
                &b.phone_number_ids,
                b.created_at,
                b.expires_at
            )
        );
        assert_eq!(
            (
                by_code.phone_number_ids,
                by_code.business_id,
                by_code.token_expires_at
            ),
            (
                by_hosted.phone_number_ids,
                by_hosted.business_id,
                by_hosted.token_expires_at
            )
        );
        // And the requests after the token are the code flow's.
        assert_eq!(tail(&hosted.t), tail(&code.t));
        assert_eq!(code.t.remaining() + hosted.t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_failure_before_store_token_spends_nothing_and_the_call_is_made_again() {
        type Script = fn(&ScriptedTransport);
        let cases: [(&str, Script); 3] = [
            (FETCH_TOKEN, |t| {
                t.push_json(403, graph_error(200));
            }),
            (DEBUG_TOKEN, |t| {
                t.push_json(200, token_answer());
                t.push_json(400, graph_error(190));
            }),
            (VERIFY_ASSETS, |t| {
                t.push_json(200, token_answer());
                t.push_json(200, debug_answer("111111111111111")); // another WABA
            }),
        ];
        for (failing, script) in cases {
            let h = harness();
            script(&h.t);
            let err =
                h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                    .await
                    .unwrap_err();
            assert_eq!(step(&err), failing);
            assert!(stored(&h).await.is_none(), "{failing}: nothing stored");
            assert_eq!(h.t.remaining(), 0);
            let before = h.t.requests().len();

            script_hosted(&h.t);
            let done =
                h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                    .await
                    .unwrap();
            assert_eq!(done.steps_completed[0], FETCH_TOKEN, "{failing}");
            assert_token_request(&h.t.requests()[before], PROOF);
            assert!(stored(&h).await.is_some());
            assert_eq!(h.t.remaining(), 0);
        }
    }

    #[tokio::test]
    async fn a_failure_at_store_token_is_fixed_and_the_call_made_again() {
        let kv = Arc::new(FlakyKv::default());
        kv.broken.store(true, Ordering::SeqCst);
        let h = harness_with(kv.clone(), APP_SECRET, RetryPolicy::NONE);
        h.t.push_json(200, token_answer());
        h.t.push_json(200, debug_answer(WABA));
        h.t.push_json(200, owner_answer(OWNER));
        h.t.push_json(200, phones_answer());
        let err =
            h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), STORE_TOKEN);
        assert_eq!(h.t.requests().len(), 4, "no subscribe, no register");
        assert_eq!(h.t.remaining(), 0);

        kv.broken.store(false, Ordering::SeqCst);
        script_hosted(&h.t);
        h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
            .await
            .unwrap();
        assert!(stored(&h).await.is_some());
        assert_eq!(h.t.remaining(), 0);
    }

    /// A `store_token` that failed after writing the record (its phone
    /// index did not follow) leaves the WABA held: the call again is
    /// refused, and once the record is deleted it runs and routes every
    /// number.
    #[tokio::test]
    async fn a_store_token_that_wrote_the_record_is_deleted_then_run_again() {
        let kv = Arc::new(FlakyKv::default());
        kv.broken_index.store(true, Ordering::SeqCst);
        let h = harness_with(kv.clone(), APP_SECRET, RetryPolicy::NONE);
        h.t.push_json(200, token_answer());
        h.t.push_json(200, debug_answer(WABA));
        h.t.push_json(200, owner_answer(OWNER));
        h.t.push_json(200, phones_answer());
        let err =
            h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), STORE_TOKEN);
        assert!(stored(&h).await.is_some(), "the record was written");
        let phone = PhoneNumberId::new(PHONE).unwrap();
        assert!(h.vault.get_by_phone_number(&phone).await.unwrap().is_none());
        kv.broken_index.store(false, Ordering::SeqCst);

        let before = h.t.requests().len();
        let err =
            h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(field(&err), "waba_id");
        assert_eq!(h.t.requests().len(), before, "refused before any request");

        assert!(h.vault.delete(&WabaId::new(WABA).unwrap()).await.unwrap());
        script_hosted(&h.t);
        h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
            .await
            .unwrap();
        let routed = h.vault.get_by_phone_number(&phone).await.unwrap().unwrap();
        assert_eq!(routed.waba_id.as_str(), WABA);
        assert_eq!(h.t.remaining(), 0);
    }

    /// After `store_token`, `resume` with the hosted request redoes only
    /// the tail with the stored token: no token request again.
    #[tokio::test]
    async fn a_failure_after_store_token_resumes_like_the_code_flow() {
        // subscribe_app fails.
        let h = harness();
        h.t.push_json(200, token_answer());
        h.t.push_json(200, debug_answer(WABA));
        h.t.push_json(200, owner_answer(OWNER));
        h.t.push_json(200, phones_answer());
        h.t.push_json(403, graph_error(200));
        let err =
            h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), SUBSCRIBE_APP);
        assert!(stored(&h).await.is_some(), "the token is kept");
        let before = h.t.requests().len();
        h.t.push_json(200, success());
        h.t.push_json(200, success());
        // After a restart: the request is rebuilt from the saved session.
        let saved: SessionInfo =
            serde_json::from_str(&serde_json::to_string(&signup().session()).unwrap()).unwrap();
        let resumed =
            OnboardingRequest::for_resume(saved).register_with_pin(TwoStepPin::new(PIN).unwrap());
        let done =
            h.es.resume(&WabaId::new(WABA).unwrap(), &resumed, &h.vault)
                .await
                .unwrap();
        assert_eq!(
            done.steps_completed,
            vec![LOAD_TOKEN, VERIFY_ASSETS, SUBSCRIBE_APP, REGISTER_PHONE]
        );
        let reqs = h.t.requests();
        assert!(!reqs[before..].iter().any(is_token_request));
        assert_eq!(
            reqs[before].path(),
            format!("/v25.0/{WABA}/subscribed_apps")
        );
        assert_eq!(reqs[before + 1].path(), format!("/v25.0/{PHONE}/register"));
        assert_eq!(reqs[before + 1].bearer(), Some(BUSINESS_TOKEN));
        assert_eq!(h.t.remaining(), 0);

        // register_phone fails on a PIN the number already has otherwise.
        let h = harness();
        h.t.push_json(200, token_answer());
        h.t.push_json(200, debug_answer(WABA));
        h.t.push_json(200, owner_answer(OWNER));
        h.t.push_json(200, phones_answer());
        h.t.push_json(200, success());
        h.t.push_json(400, graph_error(133005));
        let err =
            h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), REGISTER_PHONE);
        assert_eq!(err.kind(), ErrorKind::TwoStepVerification);
        let before = h.t.requests().len();
        h.t.push_json(200, success());
        h.t.push_json(200, success());
        let fixed = OnboardingRequest::for_hosted(&signup())
            .register_with_pin(TwoStepPin::new("000111").unwrap());
        h.es.resume(&WabaId::new(WABA).unwrap(), &fixed, &h.vault)
            .await
            .unwrap();
        assert!(!h.t.requests()[before..].iter().any(is_token_request));
        assert_eq!(
            h.t.last_request().unwrap().json(),
            Some(json!({"messaging_product": "whatsapp", "pin": "000111"}))
        );
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn an_approval_refusal_stores_nothing() {
        let h = harness();
        h.t.push_json(200, token_answer());
        h.t.push_json(200, debug_answer(WABA));
        h.t.push_json(200, owner_answer(OWNER));
        h.t.push_json(200, phones_answer());
        let seen = std::sync::Mutex::new(None);
        let err =
            h.es.onboard_hosted_with_approval(
                &signup(),
                &system_token(),
                &request(),
                &h.vault,
                |verified| {
                    *seen.lock().unwrap() = Some(verified);
                    std::future::ready(Err(ValidationError::new(
                        "waba_id",
                        "no tenant of ours claimed this business",
                    )
                    .into()))
                },
            )
            .await
            .unwrap_err();
        assert_eq!(step(&err), APPROVE);
        let verified = seen.lock().unwrap().take().unwrap();
        assert_eq!(verified.waba_id.as_str(), WABA);
        assert_eq!(
            verified.business_id.as_ref().map(BusinessId::as_str),
            Some(OWNER)
        );
        assert!(stored(&h).await.is_none());
        assert_eq!(h.t.requests().len(), 4, "nothing subscribed");
        assert_eq!(h.t.remaining(), 0);

        // Approved: the same steps, with `approve`.
        script_hosted(&h.t);
        let done = h
            .es
            .onboard_hosted_with_approval(&signup(), &system_token(), &request(), &h.vault, |_| {
                std::future::ready(Ok(()))
            })
            .await
            .unwrap();
        assert_eq!(
            done.steps_completed,
            vec![
                FETCH_TOKEN,
                DEBUG_TOKEN,
                VERIFY_ASSETS,
                APPROVE,
                STORE_TOKEN,
                SUBSCRIBE_APP,
                REGISTER_PHONE
            ]
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// Meta's owner of the WABA must be the business the token was fetched
    /// for.
    #[tokio::test]
    async fn another_owner_than_the_webhooks_is_refused() {
        let h = harness();
        h.t.push_json(200, token_answer());
        h.t.push_json(200, debug_answer(WABA));
        h.t.push_json(200, owner_answer("520744086200222"));
        let err =
            h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), VERIFY_ASSETS);
        assert_eq!(field(&err), "owner_business_id");
        assert!(stored(&h).await.is_none());
        assert_eq!(h.t.requests().len(), 3);
        assert_eq!(h.t.remaining(), 0);
    }

    /// `debug_token` runs on the fetched token as on the exchanged one: a
    /// token not limited to named WABAs (no `granular_scopes`) proves no
    /// grant of the webhook's WABA, and nothing is stored.
    #[tokio::test]
    async fn a_fetched_token_without_granular_scopes_is_refused() {
        let h = harness();
        h.t.push_json(200, token_answer());
        h.t.push_json(
            200,
            json!({"data": {
              "app_id": APP_ID, "type": "SYSTEM_USER", "is_valid": true, "expires_at": 0,
              "scopes": ["whatsapp_business_management", "whatsapp_business_messaging"]
            }}),
        );
        let err =
            h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), VERIFY_ASSETS);
        assert_eq!(field(&err), "granular_scopes");
        assert!(stored(&h).await.is_none());
        assert_eq!(
            h.t.requests().len(),
            2,
            "no asset read with an unproven grant"
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// The code flow checks no owner: the browser's `business_id` is a
    /// claim, never compared, and the owner stored is Meta's.
    #[tokio::test]
    async fn the_code_flow_compares_no_owner_with_the_browsers() {
        let h = harness();
        h.t.push_json(200, json!({"access_token": BUSINESS_TOKEN}));
        script_after_token(&h.t);
        let event = EmbeddedSignupEvent::from_value(json!({
            "data": {"phone_number_id": PHONE, "waba_id": WABA, "business_id": "520744086200222"},
            "type": "WA_EMBEDDED_SIGNUP", "event": "FINISH"
        }))
        .unwrap();
        let request = OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &event)
            .unwrap()
            .register_with_pin(TwoStepPin::new(PIN).unwrap());
        let done = h.es.onboard(&request, &h.vault).await.unwrap();
        assert_eq!(
            done.business_id.as_ref().map(BusinessId::as_str),
            Some(OWNER),
            "Meta's owner, not the browser's"
        );
        assert_eq!(
            stored(&h)
                .await
                .unwrap()
                .business_id
                .as_ref()
                .map(BusinessId::as_str),
            Some(OWNER)
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// The WABA onboarded is the webhook's, even when the request's session
    /// names none and the token's newest grant is another WABA.
    #[tokio::test]
    async fn the_webhooks_waba_is_onboarded_whatever_the_session_names() {
        let h = harness();
        h.t.push_json(200, token_answer());
        h.t.push_json(
            200,
            json!({"data": {
              "app_id": APP_ID, "is_valid": true, "expires_at": 0,
              "granular_scopes": [
                {"scope": "whatsapp_business_management", "target_ids": ["111111111111111", WABA]}
              ]
            }}),
        );
        h.t.push_json(200, owner_answer(OWNER));
        h.t.push_json(200, phones_answer());
        h.t.push_json(200, success());
        let request = OnboardingRequest::for_resume(SessionInfo::default());
        let done =
            h.es.onboard_hosted(&signup(), &system_token(), &request, &h.vault)
                .await
                .unwrap();
        assert_eq!(done.waba_id.as_str(), WABA);
        let reqs = h.t.requests();
        assert_eq!(reqs[2].path(), format!("/v25.0/{WABA}"));
        assert!(stored(&h).await.is_some());
        assert_eq!(h.t.remaining(), 0);
    }

    /// The `holds` read is not a lock: a code-flow (or multi-WABA)
    /// onboarding that stores the WABA after it (here, inside the
    /// approval) is never replaced. `store_token` stores only where the
    /// vault still holds none, so the hosted run fails there, nothing
    /// stored, subscribed or registered by it, the other record intact.
    #[tokio::test]
    async fn a_hosted_signup_never_replaces_a_record_stored_meanwhile() {
        let h = harness_with(
            Arc::new(MemoryKvStore::new()),
            APP_SECRET,
            RetryPolicy::NONE,
        );
        h.t.push_json(200, token_answer());
        h.t.push_json(200, debug_answer(WABA));
        h.t.push_json(200, owner_answer(OWNER));
        h.t.push_json(200, phones_answer());
        let vault = h.vault.clone();
        let err =
            h.es.onboard_hosted_with_approval(
                &signup(),
                &system_token(),
                &request(),
                &h.vault,
                |verified| async move {
                    let theirs = super::super::StoredBusinessToken::new(
                        verified.waba_id,
                        AccessToken::new("EAAN-CODE-FLOW-TOKEN"),
                    )
                    .phone_number_ids([PhoneNumberId::new(PHONE).unwrap()]);
                    vault.store(&theirs).await
                },
            )
            .await
            .unwrap_err();
        assert_eq!(step(&err), STORE_TOKEN, "{err}");
        assert_eq!(field(&err), "waba_id");
        assert_eq!(
            stored(&h).await.unwrap().token.expose_secret(),
            "EAAN-CODE-FLOW-TOKEN",
            "the record stored meanwhile is kept"
        );
        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 4, "not subscribed, not registered");
        assert!(reqs[3].path().ends_with("/phone_numbers"));
        assert_eq!(h.t.remaining(), 0);
    }

    /// `PARTNER_ADDED` also comes after a code-flow onboarding: the WABA the
    /// vault holds is refused before any request, its record untouched,
    /// even when this vault cannot read it; once deleted, it onboards.
    #[tokio::test]
    async fn a_waba_the_vault_holds_is_refused_before_any_request() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let h = harness_with(kv.clone(), APP_SECRET, RetryPolicy::NONE);
        // The code flow onboards the WABA first, with another token.
        h.t.push_json(200, json!({"access_token": "EAAN-CODE-FLOW-TOKEN"}));
        script_after_token(&h.t);
        let event = EmbeddedSignupEvent::from_value(json!({
            "data": {"phone_number_id": PHONE, "waba_id": WABA, "business_id": OWNER},
            "type": "WA_EMBEDDED_SIGNUP", "event": "FINISH"
        }))
        .unwrap();
        let code_request = OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &event)
            .unwrap()
            .register_with_pin(TwoStepPin::new(PIN).unwrap());
        h.es.onboard(&code_request, &h.vault).await.unwrap();
        let before = h.t.requests().len();
        let raw = |kv: Arc<dyn KvStore>| async move {
            let key = StoreKey::new(TOKEN_NAMESPACE, format!("waba/{WABA}"));
            kv.get(&key).await.unwrap().map(|v| v.value)
        };
        let record = raw(kv.clone()).await.unwrap();

        // Then the webhook, with and without an approval.
        let err =
            h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                .await
                .unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{err:?}");
        assert_eq!(field(&err), "waba_id");
        let err =
            h.es.onboard_hosted_with_approval(
                &signup(),
                &system_token(),
                &request(),
                &h.vault,
                |_| -> std::future::Ready<Result<()>> { panic!("no approval asked") },
            )
            .await
            .unwrap_err();
        assert_eq!(field(&err), "waba_id");
        // A vault under another key cannot read the record: held all the same.
        let other = TokenVault::new(
            kv.clone(),
            VaultKeys::new(VaultKey::new("k2", SecretBytes::new([7; 32])).unwrap()),
        )
        .unwrap();
        let err =
            h.es.onboard_hosted(&signup(), &system_token(), &request(), &other)
                .await
                .unwrap_err();
        assert_eq!(field(&err), "waba_id");
        assert_eq!(h.t.requests().len(), before, "no request");
        assert_eq!(
            raw(kv.clone()).await.unwrap(),
            record,
            "the record untouched"
        );
        assert_eq!(
            stored(&h).await.unwrap().token.expose_secret(),
            "EAAN-CODE-FLOW-TOKEN"
        );

        // Deleted on purpose: onboarded again, the hosted token stored.
        assert!(h.vault.delete(&WabaId::new(WABA).unwrap()).await.unwrap());
        script_hosted(&h.t);
        h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
            .await
            .unwrap();
        assert_token_request(&h.t.requests()[before], PROOF);
        assert_eq!(
            stored(&h).await.unwrap().token.expose_secret(),
            BUSINESS_TOKEN
        );
        assert_eq!(h.t.remaining(), 0);
    }

    /// `fetch_only` is a read: replayed on a timeout. A token request that
    /// generates one is not.
    #[tokio::test]
    async fn a_fetch_is_replayed_and_a_generation_is_not() {
        let h = harness_with(Arc::new(MemoryKvStore::new()), APP_SECRET, retrying());
        h.t.push_error(|| TransportError::Timeout);
        script_hosted(&h.t);
        h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
            .await
            .unwrap();
        let reqs = h.t.requests();
        assert_token_request(&reqs[0], PROOF);
        assert_token_request(&reqs[1], PROOF);
        assert!(!reqs[2..].iter().any(is_token_request));
        assert_eq!(h.t.remaining(), 0);

        let generate = signup()
            .token_request(SystemUserTokenRequest::new().system_user_id("1972555232742222"));
        let h = harness_with(Arc::new(MemoryKvStore::new()), APP_SECRET, retrying());
        h.t.push_error(|| TransportError::Timeout);
        let err =
            h.es.onboard_hosted(&generate, &system_token(), &request(), &h.vault)
                .await
                .unwrap_err();
        assert_eq!(step(&err), FETCH_TOKEN);
        assert!(matches!(
            err,
            Error::Step { ref source, .. } if matches!(**source, Error::Transport(TransportError::Timeout))
        ));
        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 1, "a generation is never replayed");
        assert_eq!(
            form(&reqs[0]),
            [
                ("appsecret_proof".to_owned(), PROOF.to_owned()),
                ("system_user_id".to_owned(), "1972555232742222".to_owned()),
            ]
        );
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn input_is_refused_before_any_request() {
        let h = harness();
        let with_code = OnboardingRequest::new(SignupCode::new(CODE).unwrap(), signup().session());
        let other_waba = OnboardingRequest::for_resume(
            HostedSignup::new(WabaId::new("111").unwrap(), BusinessId::new(OWNER).unwrap())
                .session(),
        );
        let bad_token_request =
            signup().token_request(SystemUserTokenRequest::new().system_user_id("12a"));
        let cases: [(&HostedSignup, AccessToken, &OnboardingRequest, &str); 5] = [
            (&signup(), system_token(), &with_code, "code"),
            (&signup(), system_token(), &other_waba, "waba_id"),
            (&signup(), AccessToken::new(" "), &request(), "system_token"),
            (
                &bad_token_request,
                system_token(),
                &request(),
                "system_user_id",
            ),
            (
                &signup(),
                system_token(),
                &OnboardingRequest::for_hosted(&signup())
                    .data_localization_region(crate::phone_numbers::DataLocalizationRegion::De),
                "data_localization_region",
            ),
        ];
        for (signup, token, request, expected) in cases {
            let err =
                h.es.onboard_hosted(signup, &token, request, &h.vault)
                    .await
                    .unwrap_err();
            assert!(matches!(err, Error::Validation(_)), "{expected}: {err:?}");
            assert_eq!(field(&err), expected);
        }
        let blank_secret = harness_with(Arc::new(MemoryKvStore::new()), " ", RetryPolicy::NONE);
        let err = blank_secret
            .es
            .onboard_hosted(&signup(), &system_token(), &request(), &blank_secret.vault)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Validation(_)),
            "refused before step fetch_token: {err:?}"
        );
        assert_eq!(field(&err), "app_secret");
        // A Solution Partner must approve.
        let p = harness();
        let partner = p.es.clone().solution_partner(
            SolutionPartner::new(system_token(), "1972555232742222", "58501441721238")
                .default_currency(crate::credit_lines::WabaCurrency::Usd),
        );
        let err = partner
            .onboard_hosted(&signup(), &system_token(), &request(), &p.vault)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Credit(CreditError::ApprovalRequired(_))),
            "{err:?}"
        );
        assert!(h.t.requests().is_empty());
        assert!(blank_secret.t.requests().is_empty());
        assert!(p.t.requests().is_empty());
    }

    /// No secret reaches `Debug`, an error, the logs or the store: the app
    /// secret, the system token, its proof, and the business token outside
    /// its sealed record.
    #[tokio::test]
    async fn secrets_never_reach_debug_errors_logs_or_the_store() {
        // A second dispatcher, as in `onboard`'s test: otherwise a callsite
        // another test thread registers first may never reach this capture.
        let _second = tracing::Dispatch::new(Capture::default());
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.clone());
        let kv = Arc::new(RecordingKv::default());
        let h = harness_with(kv.clone(), APP_SECRET, retrying());
        // The fetch retried once on a transient error (logged), then the rest.
        h.t.push_json(
            500,
            json!({"error": {"message": "x", "code": 2, "is_transient": true}}),
        );
        script_hosted(&h.t);
        let done =
            h.es.onboard_hosted(&signup(), &system_token(), &request(), &h.vault)
                .await
                .unwrap();
        assert_token_request(&h.t.requests()[1], PROOF);
        let mut shown = vec![
            format!("{done:?}"),
            format!("{:?}", signup()),
            format!("{:?}", request()),
            format!("{:?}", h.es),
        ];
        assert_eq!(h.t.remaining(), 0);
        // The vault holds the WABA now: the failures below need it gone. On
        // the same store, with no retry: each answer is one call's.
        assert!(h.vault.delete(&WabaId::new(WABA).unwrap()).await.unwrap());
        let f = harness_with(kv.clone(), APP_SECRET, RetryPolicy::NONE);
        // A non-2xx answer holding the token, and one that does not decode.
        f.t.push_json(502, json!({"access_token": BUSINESS_TOKEN}));
        f.t.push_json(200, json!({"access_token": [BUSINESS_TOKEN]}));
        for _ in 0..2 {
            let err =
                f.es.onboard_hosted(&signup(), &system_token(), &request(), &f.vault)
                    .await
                    .unwrap_err();
            assert_eq!(step(&err), FETCH_TOKEN);
            shown.push(format!("{err} {err:?}"));
        }
        assert_eq!(f.t.remaining(), 0);
        let logs = capture.0.lock().unwrap().clone();
        assert!(
            logs.contains("graph request") && logs.contains("retrying graph request"),
            "capture saw nothing: vacuous check\n{logs}"
        );
        shown.push(logs);
        for text in &shown {
            for secret in [APP_SECRET, SYSTEM_TOKEN, PROOF, BUSINESS_TOKEN, PIN] {
                assert!(!text.contains(secret), "{secret} in {text}");
            }
        }
        for secret in [APP_SECRET, SYSTEM_TOKEN, PROOF, BUSINESS_TOKEN, PIN] {
            kv.assert_never_wrote(secret);
        }
        assert_eq!(h.t.remaining(), 0);
    }
}
