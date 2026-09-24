//! `EmbeddedSignup::onboard`: from the code and session info to a verified,
//! stored, subscribed and (optionally) registered business. The step order
//! and why it is what it is are in the [module docs](super).

use std::pin::pin;

use futures::StreamExt;
use time::OffsetDateTime;
use wa_core::error::ValidationError;
use wa_core::ids::{AppId, BusinessId, PhoneNumberId, WabaId};
use wa_core::secret::AccessToken;
use wa_core::{Error, Result};

use super::EmbeddedSignup;
use super::event::{EmbeddedSignupEvent, FinishKind, SessionInfo};
use super::token::{SignupCode, TokenDebug, WHATSAPP_BUSINESS_MANAGEMENT};
use super::vault::{StoredBusinessToken, TokenVault};
use crate::Client;
use crate::phone_numbers::{DataLocalizationRegion, TwoStepPin};
use crate::waba::{CallbackOverride, PhoneNumbersQuery};

/// Stable step names used in [`wa_core::Error::Step`] by
/// [`EmbeddedSignup::onboard`] and [`EmbeddedSignup::resume`].
pub mod steps {
    /// `GET oauth/access_token`.
    pub const EXCHANGE_CODE: &str = "exchange_code";
    /// `GET debug_token`, and checking the WABA against its grants.
    pub const DEBUG_TOKEN: &str = "debug_token";
    /// Checking (or finding) the phone number among the WABA's numbers.
    pub const RESOLVE_PHONE_NUMBER: &str = "resolve_phone_number";
    /// Writing the token to the vault.
    pub const STORE_TOKEN: &str = "store_token";
    /// Reading the token back from the vault ([`EmbeddedSignup::resume`](super::EmbeddedSignup::resume)).
    pub const LOAD_TOKEN: &str = "load_token";
    /// `POST /{WABA_ID}/subscribed_apps`.
    pub const SUBSCRIBE_APP: &str = "subscribe_app";
    /// `POST /{PHONE_NUMBER_ID}/register`.
    pub const REGISTER_PHONE: &str = "register_phone";
}

use steps::{
    DEBUG_TOKEN, EXCHANGE_CODE, LOAD_TOKEN, REGISTER_PHONE, RESOLVE_PHONE_NUMBER, STORE_TOKEN,
    SUBSCRIBE_APP,
};

/// What to onboard, and how.
///
/// `Debug` never shows the code or the PIN.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OnboardingRequest {
    /// The exchangeable code from `FB.login`.
    pub code: SignupCode,
    /// Asset ids from the `FINISH*` message event (claims, verified during
    /// onboarding).
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

    fn validate(&self) -> Result<(), ValidationError> {
        if self.register && self.pin.is_none() {
            return Err(ValidationError::new(
                "pin",
                "required to register the number",
            ));
        }
        if self.register && self.finish_kind == Some(FinishKind::WhatsappBusinessAppOnboarding) {
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
    /// The verified (or resolved) phone number, if any.
    pub phone_number_id: Option<PhoneNumberId>,
    /// The business portfolio, as reported by the session info.
    pub business_id: Option<BusinessId>,
    /// Which completion the flow reported, when known.
    pub finish_kind: Option<FinishKind>,
    /// The business token (also in the vault).
    pub token: AccessToken,
    /// When the token expires, if it does.
    pub token_expires_at: Option<OffsetDateTime>,
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

impl EmbeddedSignup {
    /// Onboard a business that completed Embedded Signup: exchange the code,
    /// verify the WABA and phone number, store the token in `vault`,
    /// subscribe the app, and register the number if requested.
    ///
    /// Input errors are reported before any request. Every later failure is
    /// an [`Error::Step`] naming the step; see the [module docs](super) for
    /// what each step does and which can be repeated.
    pub async fn onboard(
        &self,
        request: &OnboardingRequest,
        vault: &TokenVault,
    ) -> Result<Onboarded> {
        request.validate()?;
        let mut done = Vec::with_capacity(6);

        let token = self
            .exchange_code(&request.code)
            .await
            .map_err(|e| e.in_step(EXCHANGE_CODE))?;
        done.push(EXCHANGE_CODE);

        let debug = self
            .debug_token(&token.access_token)
            .await
            .map_err(|e| e.in_step(DEBUG_TOKEN))?;
        let waba_id = verify_grant(&debug, request.session.primary_waba_id(), &self.app.app_id)
            .map_err(|e| Error::from(e).in_step(DEBUG_TOKEN))?;
        done.push(DEBUG_TOKEN);

        let business = self.client.with_token(token.access_token.clone());
        let claimed_phone = request.session.phone_number_id.as_ref();
        let phone_number_id =
            if claimed_phone.is_some() || request.register || request.is_coexistence() {
                let phone = resolve_phone_number(&business, &waba_id, claimed_phone)
                    .await
                    .map_err(|e| e.in_step(RESOLVE_PHONE_NUMBER))?;
                done.push(RESOLVE_PHONE_NUMBER);
                phone
            } else {
                None
            };

        let expires_at = debug.expires_at_time().or_else(|| {
            token
                .expires_in
                .and_then(|s| i64::try_from(s).ok())
                .map(|s| vault.now() + time::Duration::seconds(s))
        });
        let mut stored = StoredBusinessToken::new(waba_id.clone(), token.access_token.clone());
        stored.business_id.clone_from(&request.session.business_id);
        stored.phone_number_ids = phone_number_id.iter().cloned().collect();
        stored.expires_at = expires_at;
        vault
            .store(&stored)
            .await
            .map_err(|e| e.in_step(STORE_TOKEN))?;
        done.push(STORE_TOKEN);

        setup(
            &business,
            &waba_id,
            phone_number_id.as_ref(),
            request,
            &mut done,
        )
        .await?;

        Ok(Onboarded {
            waba_id,
            phone_number_id,
            business_id: request.session.business_id.clone(),
            finish_kind: request.finish_kind,
            token: token.access_token,
            token_expires_at: expires_at,
            steps_completed: done,
        })
    }

    /// Redo the repeatable tail of [`Self::onboard`] (subscribe, register)
    /// for a WABA whose token is already in `vault`, e.g. after
    /// `register_phone` failed on a wrong PIN. `request.code` is not used;
    /// the phone number is the one stored with the token (verified during
    /// onboarding), not the one in `request.session`.
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
        request.validate()?;
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
        let phone_number_id = stored.phone_number_ids.first().cloned();
        let business = self.client.with_token(stored.token.clone());
        setup(
            &business,
            waba_id,
            phone_number_id.as_ref(),
            request,
            &mut done,
        )
        .await?;
        Ok(Onboarded {
            waba_id: waba_id.clone(),
            phone_number_id,
            business_id: stored.business_id,
            finish_kind: request.finish_kind,
            token: stored.token,
            token_expires_at: stored.expires_at,
            steps_completed: done,
        })
    }
}

/// `subscribe_app`, then `register_phone` if requested.
async fn setup(
    business: &Client,
    waba_id: &WabaId,
    phone_number_id: Option<&PhoneNumberId>,
    request: &OnboardingRequest,
    done: &mut Vec<&'static str>,
) -> Result<()> {
    business
        .waba(waba_id.clone())
        .subscribe_app(request.subscribe_override.as_ref())
        .await
        .map_err(|e| e.in_step(SUBSCRIBE_APP))?;
    done.push(SUBSCRIBE_APP);

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

/// Check that `debug` describes a valid token of our app that manages
/// `claimed` (or pick the newest WABA it manages when nothing is claimed).
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
    if debug.app_id.as_ref().is_some_and(|a| a != app_id) {
        return Err(ValidationError::new(
            "app_id",
            "the token was issued to a different app",
        ));
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
        None => targets
            .first()
            .map(|t| WabaId::new(t.as_str()))
            .ok_or_else(|| ValidationError::new("granular_scopes", "the token grants no WABA")),
    }
}

/// Verify `claimed` is one of the WABA's numbers, or (nothing claimed) take
/// the most recently onboarded one — Meta lists them newest first.
async fn resolve_phone_number(
    business: &Client,
    waba_id: &WabaId,
    claimed: Option<&PhoneNumberId>,
) -> Result<Option<PhoneNumberId>> {
    let waba = business.waba(waba_id.clone());
    let query = PhoneNumbersQuery::new().fields(["id"]);
    let Some(claimed) = claimed else {
        let page = waba.phone_numbers(&query).await?;
        return Ok(page.data.into_iter().next().map(|p| p.id));
    };
    let mut numbers = pin!(waba.phone_numbers_stream(&query));
    while let Some(number) = numbers.next().await {
        if &number?.id == claimed {
            return Ok(Some(claimed.clone()));
        }
    }
    Err(ValidationError::new(
        "phone_number_id",
        "the phone number in the session info does not belong to the WABA",
    )
    .into())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http::Method;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use time::macros::datetime;
    use wa_adapters::store::MemoryKvStore;
    use wa_core::ErrorKind;
    use wa_core::clock::ManualClock;
    use wa_core::store::{KvStore, StoreKey};
    use wa_core::testing::{RecordedBody, ScriptedTransport};

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

    struct Harness {
        t: ScriptedTransport,
        es: EmbeddedSignup,
        kv: Arc<dyn KvStore>,
        vault: TokenVault,
    }

    fn harness() -> Harness {
        let t = ScriptedTransport::new();
        let client = Client::builder()
            .transport(t.clone())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let vault = TokenVault::new(
            Arc::clone(&kv),
            VaultKeys::new(VaultKey::new("k1", [42; 32]).unwrap()),
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

    fn finish_event(data: &serde_json::Value, event: &str) -> EmbeddedSignupEvent {
        EmbeddedSignupEvent::from_value(
            json!({"data": data, "type": "WA_EMBEDDED_SIGNUP", "event": event}),
        )
        .unwrap()
    }

    fn full_request() -> OnboardingRequest {
        let event = finish_event(
            &json!({"phone_number_id": PHONE, "waba_id": WABA, "business_id": BUSINESS}),
            "FINISH",
        );
        OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &event)
            .unwrap()
            .register_with_pin(TwoStepPin::new("581063").unwrap())
    }

    fn token_response() -> serde_json::Value {
        json!({"access_token": TOKEN, "token_type": "bearer"})
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

    fn phones_response(ids: &[&str]) -> serde_json::Value {
        json!({"data": ids.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()})
    }

    fn success() -> serde_json::Value {
        json!({"success": true})
    }

    fn graph_error(code: i64) -> serde_json::Value {
        json!({"error": {"message": format!("(#{code}) x"), "type": "OAuthException", "code": code, "fbtrace_id": "A"}})
    }

    fn step(err: &Error) -> &'static str {
        match err {
            Error::Step { step, .. } => step,
            other => panic!("not a step error: {other}"),
        }
    }

    async fn stored_bytes(kv: &Arc<dyn KvStore>, waba: &str) -> Option<Vec<u8>> {
        kv.get(&StoreKey::new(TOKEN_NAMESPACE, format!("waba/{waba}")))
            .await
            .unwrap()
            .map(|v| v.value)
    }

    #[tokio::test]
    async fn happy_path_runs_every_step_in_order() {
        let h = harness();
        h.t.push_json(200, token_response());
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, phones_response(&["999", PHONE]));
        h.t.push_json(200, success());
        h.t.push_json(200, success());

        let request = full_request().subscribe_override(CallbackOverride::new(
            "https://hooks.example.com/wa",
            "verify-me",
        ));
        let done = h.es.onboard(&request, &h.vault).await.unwrap();
        assert_eq!(
            done.steps_completed,
            vec![
                EXCHANGE_CODE,
                DEBUG_TOKEN,
                RESOLVE_PHONE_NUMBER,
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
            done.business_id.as_ref().map(BusinessId::as_str),
            Some(BUSINESS)
        );
        assert_eq!(done.token.expose_secret(), TOKEN);
        assert_eq!(done.token_expires_at, None);
        let debug_text = format!("{done:?} {request:?}");
        for secret in [TOKEN, CODE, "581063", "verify-me", APP_SECRET] {
            assert!(!debug_text.contains(secret), "{secret} in {debug_text}");
        }

        let reqs = h.t.requests();
        assert_eq!(reqs.len(), 5);
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
        // 3. the WABA's numbers, with the business token.
        assert_eq!(reqs[2].method, Method::GET);
        assert_eq!(reqs[2].path(), format!("/v25.0/{WABA}/phone_numbers"));
        assert_eq!(reqs[2].bearer(), Some(TOKEN));
        // 5. subscribe with the override.
        assert_eq!(reqs[3].method, Method::POST);
        assert_eq!(reqs[3].path(), format!("/v25.0/{WABA}/subscribed_apps"));
        assert_eq!(reqs[3].bearer(), Some(TOKEN));
        assert_eq!(
            reqs[3].json(),
            Some(
                json!({"override_callback_uri": "https://hooks.example.com/wa", "verify_token": "verify-me"})
            )
        );
        // 6. register.
        assert_eq!(reqs[4].path(), format!("/v25.0/{PHONE}/register"));
        assert_eq!(reqs[4].bearer(), Some(TOKEN));
        assert_eq!(
            reqs[4].json(),
            Some(json!({"messaging_product": "whatsapp", "pin": "581063"}))
        );
        assert_eq!(h.t.remaining(), 0);

        // 4. stored, encrypted, and indexed by phone number.
        let bytes = stored_bytes(&h.kv, WABA).await.unwrap();
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
        let by_phone = h
            .vault
            .get_by_phone_number(&PhoneNumberId::new(PHONE))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_phone.waba_id.as_str(), WABA);
    }

    #[tokio::test]
    async fn missing_ids_are_resolved_through_debug_token_and_the_waba() {
        let h = harness();
        h.t.push_json(200, json!({"access_token": TOKEN, "expires_in": 5_184_000}));
        h.t.push_json(200, debug_response(&["NEWEST", "OLDER"]));
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
        assert_eq!(reqs[2].path(), "/v25.0/NEWEST/phone_numbers");
        assert_eq!(reqs[2].query("fields").as_deref(), Some("id"));
        assert_eq!(reqs[4].path(), "/v25.0/P-NEWEST/register");
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn only_waba_flow_without_registration_skips_phone_steps() {
        let h = harness();
        h.t.push_json(200, token_response());
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, success());
        let event = finish_event(&json!({"waba_id": WABA}), "FINISH_ONLY_WABA");
        let request =
            OnboardingRequest::from_event(SignupCode::new(CODE).unwrap(), &event).unwrap();
        let done = h.es.onboard(&request, &h.vault).await.unwrap();
        assert_eq!(
            done.steps_completed,
            vec![EXCHANGE_CODE, DEBUG_TOKEN, STORE_TOKEN, SUBSCRIBE_APP]
        );
        assert_eq!(done.phone_number_id, None);
        assert_eq!(
            h.t.requests()[2].body,
            RecordedBody::Empty,
            "plain subscribe"
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

        h.t.push_json(200, token_response());
        h.t.push_json(200, debug_response(&[WABA]));
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
                RESOLVE_PHONE_NUMBER,
                STORE_TOKEN,
                SUBSCRIBE_APP
            ]
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
        assert!(stored_bytes(&h.kv, WABA).await.is_none());
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn failure_at_debug_token_stops_before_storing() {
        let h = harness();
        h.t.push_json(200, token_response());
        h.t.push_json(500, json!({"error": {"message": "x", "code": 2}}));
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), DEBUG_TOKEN);
        assert_eq!(err.kind(), ErrorKind::ServiceUnavailable);
        assert_eq!(h.t.requests().len(), 2);
        assert!(stored_bytes(&h.kv, WABA).await.is_none());
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_waba_the_token_was_not_granted_is_rejected() {
        let h = harness();
        h.t.push_json(200, token_response());
        h.t.push_json(200, debug_response(&["SOMEONE_ELSES_OWN_WABA"]));
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), DEBUG_TOKEN);
        assert_eq!(h.t.requests().len(), 2, "nothing after the check");
        assert!(stored_bytes(&h.kv, WABA).await.is_none());
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn invalid_foreign_or_ungranular_tokens_are_rejected() {
        for debug in [
            json!({"data": {"is_valid": false, "app_id": APP_ID, "granular_scopes": [{"scope": "whatsapp_business_management", "target_ids": [WABA]}]}}),
            json!({"data": {"is_valid": true, "app_id": "OTHER_APP", "granular_scopes": [{"scope": "whatsapp_business_management", "target_ids": [WABA]}]}}),
            json!({"data": {"is_valid": true, "app_id": APP_ID, "granular_scopes": [{"scope": "whatsapp_business_management"}]}}),
            json!({"data": {"is_valid": true, "app_id": APP_ID, "granular_scopes": [{"scope": "whatsapp_business_management", "target_ids": []}]}}),
        ] {
            let h = harness();
            h.t.push_json(200, token_response());
            h.t.push_json(200, debug);
            let request =
                OnboardingRequest::new(SignupCode::new(CODE).unwrap(), SessionInfo::default());
            let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
            assert_eq!(step(&err), DEBUG_TOKEN);
            assert_eq!(h.t.remaining(), 0);
        }
    }

    #[tokio::test]
    async fn a_phone_number_outside_the_waba_is_rejected() {
        let h = harness();
        h.t.push_json(200, token_response());
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(
            200,
            json!({"data": [{"id": "OTHER"}], "paging": {"cursors": {"after": "c"}, "next": "https://graph.facebook.com/x"}}),
        );
        h.t.push_json(200, phones_response(&["STILL_OTHER"]));
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), RESOLVE_PHONE_NUMBER);
        assert_eq!(h.t.requests().len(), 4, "all pages were searched");
        assert!(stored_bytes(&h.kv, WABA).await.is_none());
        assert!(
            h.kv.get(&StoreKey::new(TOKEN_NAMESPACE, format!("phone/{PHONE}")))
                .await
                .unwrap()
                .is_none(),
            "no phone index poisoning"
        );
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
            ) -> Result<Option<wa_core::store::Versioned>, wa_core::error::StorageError>
            {
                Ok(None)
            }
            async fn put(
                &self,
                _: &StoreKey,
                _: Vec<u8>,
                _: wa_core::store::Expiry,
            ) -> Result<u64, wa_core::error::StorageError> {
                Err(wa_core::error::StorageError::Backend(anyhow::anyhow!(
                    "disk full"
                )))
            }
            async fn put_if_absent(
                &self,
                _: &StoreKey,
                _: Vec<u8>,
                _: wa_core::store::Expiry,
            ) -> Result<Option<u64>, wa_core::error::StorageError> {
                Ok(None)
            }
            async fn compare_and_swap(
                &self,
                _: &StoreKey,
                _: u64,
                _: Option<Vec<u8>>,
                _: wa_core::store::Expiry,
            ) -> Result<Option<u64>, wa_core::error::StorageError> {
                Ok(None)
            }
            async fn delete(&self, _: &StoreKey) -> Result<bool, wa_core::error::StorageError> {
                Ok(false)
            }
        }
        let h = harness();
        let vault = TokenVault::new(
            Arc::new(BrokenKv),
            VaultKeys::new(VaultKey::new("k1", [1; 32]).unwrap()),
        )
        .unwrap();
        h.t.push_json(200, token_response());
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, phones_response(&[PHONE]));
        let err = h.es.onboard(&full_request(), &vault).await.unwrap_err();
        assert_eq!(step(&err), STORE_TOKEN);
        assert!(matches!(
            err,
            Error::Step { ref source, .. } if matches!(**source, Error::Storage(_))
        ));
        assert_eq!(h.t.requests().len(), 3, "no subscribe, no register");
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn failure_at_subscribe_keeps_the_token_and_resume_finishes() {
        let h = harness();
        h.t.push_json(200, token_response());
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, phones_response(&[PHONE]));
        h.t.push_json(403, graph_error(200));
        let request = full_request();
        let err = h.es.onboard(&request, &h.vault).await.unwrap_err();
        assert_eq!(step(&err), SUBSCRIBE_APP);
        assert_eq!(err.kind(), ErrorKind::Permission);
        assert_eq!(h.t.requests().len(), 4, "register never ran");
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
            vec![LOAD_TOKEN, SUBSCRIBE_APP, REGISTER_PHONE]
        );
        let reqs = h.t.requests();
        assert_eq!(reqs[4].path(), format!("/v25.0/{WABA}/subscribed_apps"));
        assert_eq!(reqs[5].path(), format!("/v25.0/{PHONE}/register"));
        assert_eq!(reqs[5].bearer(), Some(TOKEN), "stored token used");
        assert_eq!(h.t.remaining(), 0);
    }

    #[tokio::test]
    async fn failure_at_register_reports_the_step_and_resume_retries_it() {
        let h = harness();
        h.t.push_json(200, token_response());
        h.t.push_json(200, debug_response(&[WABA]));
        h.t.push_json(200, phones_response(&[PHONE]));
        h.t.push_json(200, success());
        h.t.push_json(400, graph_error(133005));
        let err = h.es.onboard(&full_request(), &h.vault).await.unwrap_err();
        assert_eq!(step(&err), REGISTER_PHONE);
        assert_eq!(err.kind(), ErrorKind::TwoStepVerification);
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
    }

    #[tokio::test]
    async fn register_without_a_number_fails_at_register_after_storing() {
        let h = harness();
        h.t.push_json(200, token_response());
        h.t.push_json(200, debug_response(&[WABA]));
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
        assert!(
            h.es.resume(&WabaId::new("unknown"), &full_request(), &h.vault)
                .await
                .is_err()
        );
        assert!(h.t.requests().is_empty());
    }
}
