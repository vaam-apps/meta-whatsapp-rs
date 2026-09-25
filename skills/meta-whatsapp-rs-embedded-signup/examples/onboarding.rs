//! Reference code for the `meta-whatsapp-rs-embedded-signup` skill: the backend of
//! Embedded Signup — start an attempt bound to the merchant, complete it
//! (local checks, redeem, onboard), and resume a failed tail.
//!
//! The full server, with authentication and a launch page, is
//! `crates/meta-whatsapp-rs/examples/embedded_signup.rs`. meta-whatsapp-rs compiles this file and
//! runs its tests in its own gate (`crates/meta-whatsapp-rs/tests/skills.rs`).

use std::sync::Arc;
use std::time::Duration;

use meta_whatsapp_rs::client::embedded_signup::{
    EmbeddedSignup, EmbeddedSignupEvent, FinishKind, LaunchOptions, Onboarded, OnboardingRequest,
    SessionInfo, SignupCode, SignupSessions, SignupState, TokenVault, VaultKey, VaultKeys, steps,
};
use meta_whatsapp_rs::client::phone_numbers::TwoStepPin;
use meta_whatsapp_rs::prelude::*;

/// Built once at startup, on a store every instance shares.
pub struct Signup {
    pub es: EmbeddedSignup,
    pub sessions: SignupSessions,
    pub vault: TokenVault,
    pub config_id: String, // Facebook Login for Business configuration id
}

/// Wire the pieces. The app secret stays on the server.
pub fn wire(
    kv: Arc<dyn KvStore>,
    app_id: &str,
    app_secret: &str,
    vault_key_b64: &str,
    config_id: &str,
) -> meta_whatsapp_rs::Result<Signup> {
    let client = meta_whatsapp_rs::client_builder()?.build()?; // no default token
    let es = client.embedded_signup(AppCredentials::new(app_id, app_secret));
    let vault = TokenVault::new(
        kv.clone(),
        VaultKeys::new(VaultKey::from_base64("2026-09", vault_key_b64)?),
    )?;
    let sessions = SignupSessions::new(kv);
    Ok(Signup {
        es,
        sessions,
        vault,
        config_id: config_id.to_owned(),
    })
}

/// "Connect WhatsApp", behind your authentication: bind an attempt to the
/// merchant and give the page what `FB.login` needs.
pub async fn start(signup: &Signup, merchant_id: &str) -> meta_whatsapp_rs::Result<serde_json::Value> {
    let state = signup
        .sessions
        .start(merchant_id, Duration::from_mins(15))
        .await?; // minutes: several screens, maybe an SMS
    let options = LaunchOptions::new(signup.config_id.as_str()).to_json()?; // .coexistence() for WhatsApp Business app users
    Ok(serde_json::json!({"state": state.as_str(), "options": options}))
}

/// The steps after `store_token`: the token is kept, `resume` redoes them
/// (the two credit steps run for a Solution Partner only).
pub const AFTER_STORE: [&str; 4] = [
    steps::SUBSCRIBE_APP,
    steps::ASSIGN_SYSTEM_USER,
    steps::SHARE_CREDIT_LINE,
    steps::REGISTER_PHONE,
];

/// How the callback ended.
#[derive(Debug)]
pub enum Completion {
    Connected(Box<Onboarded>),
    Cancelled,
    Stale,                         // expired, replayed, or another merchant's attempt
    Resumable(WabaId, Box<Error>), // token stored: fix the cause, then `resume`
    StartOver(Box<Error>),         // the code is spent: the merchant runs the popup again
}

/// The callback. `merchant_id` comes from YOUR session, never from the
/// body, the page or the URL.
pub async fn complete(
    signup: &Signup,
    merchant_id: &str,
    state: &str,
    code: String,
    event: &str,       // the raw WA_EMBEDDED_SIGNUP message event, as the page got it
    pin: Option<&str>, // the merchant's own 6-digit PIN, typed in your page
) -> meta_whatsapp_rs::Result<Completion> {
    // Local checks first: a malformed post must not burn the single-use state.
    let state = SignupState::parse(state)?;
    let code = SignupCode::new(code)?;
    let pin = pin.map(TwoStepPin::new).transpose()?;
    let event = EmbeddedSignupEvent::from_json(event)?;
    if !matches!(event, EmbeddedSignupEvent::Finish { .. }) {
        return Ok(Completion::Cancelled); // Cancel / Error / Unknown: nothing to onboard
    }
    let mut request = OnboardingRequest::from_event(code, &event)?;
    if let (Some(FinishKind::Finish), Some(pin)) = (event.finish_kind(), pin) {
        request = request.register_with_pin(pin); // never for coexistence numbers
    }
    if !signup.sessions.redeem(&state, merchant_id).await? {
        return Ok(Completion::Stale);
    }
    // Never retry `onboard`: its first step spends the code.
    match signup.es.onboard(&request, &signup.vault).await {
        Ok(done) => Ok(Completion::Connected(Box::new(done))), // save done.waba_id for merchant_id
        Err(e @ Error::Step { step, .. }) if AFTER_STORE.contains(&step) => {
            match request.session.primary_waba_id() {
                Some(waba) => Ok(Completion::Resumable(waba.clone(), Box::new(e))),
                None => Ok(Completion::StartOver(Box::new(e))),
            }
        }
        Err(e) => Ok(Completion::StartOver(Box::new(e))),
    }
}

/// Redo the steps after `store_token` with the stored token, e.g. after a
/// wrong PIN (`ErrorKind::TwoStepVerification`). Check first that
/// `waba_id` belongs to the calling merchant: `resume` acts with whatever
/// token is stored for the WABA it is given.
pub async fn resume_after_restart(
    signup: &Signup,
    waba_id: &WabaId,
    saved_session: SessionInfo, // persisted when onboarding failed (it is Serialize)
    corrected_pin: &str,
) -> meta_whatsapp_rs::Result<Onboarded> {
    // `resume` ignores the code, but a request cannot be built without one
    // (a code-less constructor is OPEN_QUESTIONS.md #10).
    let request = OnboardingRequest::new(SignupCode::new("unused")?, saved_session)
        .register_with_pin(TwoStepPin::new(corrected_pin)?);
    signup.es.resume(waba_id, &request, &signup.vault).await
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::adapters::store::MemoryKvStore;

    use super::*;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="; // test only

    fn signup() -> Signup {
        wire(
            Arc::new(MemoryKvStore::new()),
            "1234",
            "app-secret",
            KEY,
            "987654",
        )
        .unwrap()
    }

    const FINISH: &str = r#"{"type":"WA_EMBEDDED_SIGNUP","event":"FINISH",
        "data":{"phone_number_id":"106540352242922","waba_id":"102290129340398","business_id":"5"}}"#;

    #[tokio::test]
    async fn a_state_is_redeemed_once_and_only_by_its_merchant() {
        let signup = signup();
        let started = start(&signup, "merchant-a").await.unwrap();
        let state = SignupState::parse(started["state"].as_str().unwrap()).unwrap();
        assert_eq!(started["options"]["config_id"], "987654");
        assert!(!signup.sessions.redeem(&state, "merchant-b").await.unwrap()); // not burnt
        assert!(signup.sessions.redeem(&state, "merchant-a").await.unwrap());
        assert!(!signup.sessions.redeem(&state, "merchant-a").await.unwrap()); // single use
    }

    #[tokio::test]
    async fn local_checks_come_before_redeem() {
        let signup = signup();
        let started = start(&signup, "merchant-a").await.unwrap();
        let state = started["state"].as_str().unwrap();
        let bad_pin = complete(
            &signup,
            "merchant-a",
            state,
            "code".into(),
            FINISH,
            Some("12"),
        )
        .await;
        assert!(matches!(bad_pin, Err(Error::Validation(ref v)) if v.field == "pin"));
        // The attempt is still usable after the typo.
        let parsed = SignupState::parse(state).unwrap();
        assert!(signup.sessions.redeem(&parsed, "merchant-a").await.unwrap());
    }

    #[tokio::test]
    async fn a_cancelled_flow_onboards_nothing() {
        let signup = signup();
        let started = start(&signup, "merchant-a").await.unwrap();
        let cancel = r#"{"type":"WA_EMBEDDED_SIGNUP","event":"CANCEL","data":{"current_step":"PHONE_NUMBER_SETUP"}}"#;
        let state = started["state"].as_str().unwrap();
        let done = complete(&signup, "merchant-a", state, "code".into(), cancel, None)
            .await
            .unwrap();
        assert!(matches!(done, Completion::Cancelled));
    }

    #[test]
    fn numeric_ids_are_refused() {
        // A 64-bit id that went through a JavaScript number may be someone else's.
        let rounded =
            r#"{"type":"WA_EMBEDDED_SIGNUP","event":"FINISH","data":{"waba_id":102290129340398}}"#;
        assert!(EmbeddedSignupEvent::from_json(rounded).is_err());
        assert!(EmbeddedSignupEvent::from_json(FINISH).is_ok());
    }
}
