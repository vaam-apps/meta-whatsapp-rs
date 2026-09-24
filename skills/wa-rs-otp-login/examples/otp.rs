//! Reference code for the `wa-rs-otp-login` skill: an authentication
//! template, and `OtpService` issuing and verifying one-time passcodes.
//!
//! wa-rs compiles this file and runs its tests in its own gate
//! (`crates/wa-rs/tests/skills.rs`).

use std::sync::Arc;

use wa_rs::client::authentication::{
    AuthenticationTemplate, IssueOutcome, OtpConfig, OtpPepper, OtpService, OtpTemplate,
    VerifyOutcome,
};
use wa_rs::client::templates::TemplateCreated;
use wa_rs::core::clock::{Clock, SystemClock};
use wa_rs::prelude::*;

/// Once per language: Meta's preset text and a copy-code button.
pub async fn create_login_template(
    client: &Client,
    waba_id: WabaId,
) -> wa_rs::Result<TemplateCreated> {
    let template = AuthenticationTemplate::copy_code("login_code", "en_US")
        .security_recommendation(true)
        .code_expiration_minutes(10); // keep equal to OtpConfig::ttl
    client.authentication(waba_id).create(&template).await
}

/// One service per sending number (and tenant), built at startup.
pub fn otp_service(
    client: Client, // the sending number's token
    phone_number_id: PhoneNumberId,
    kv: Arc<dyn KvStore>,   // shared: Postgres or Redis with several instances
    pepper: Vec<u8>,        // >= 32 random bytes from your secret manager, not the database
    tenant: Option<String>, // Some(tenant id) when one number serves several tenants
) -> wa_rs::Result<OtpService> {
    OtpService::new(
        client,
        phone_number_id,
        OtpTemplate::new("login_code", "en_US"), // must be APPROVED
        kv,
        Arc::new(SystemClock),
        OtpPepper::new(pepper)?,
        OtpConfig {
            namespace: tenant, // default config: 6 digits, 10 min, 5 attempts, 30 s, 5/hour
            ..OtpConfig::default()
        },
    )
}

/// What the login API answers.
#[derive(Debug, PartialEq, Eq)]
pub enum Login {
    CodeSent,
    Wait(std::time::Duration),
    NotAWhatsAppNumber(String),
    SignedIn,
    WrongCode { attempts_left: u32 },
    RequestANewCode,
}

/// Step 1: `user_input` must already be E.164 with the country code.
pub async fn request_code(otp: &OtpService, user_input: &str) -> wa_rs::Result<Login> {
    let user = Recipient::phone(user_input); // "+16505551234": never strip the `+`
    match otp.issue(&user, "login").await {
        Ok(IssueOutcome::Sent(_challenge)) => Ok(Login::CodeSent),
        Ok(
            IssueOutcome::CoolingDown { retry_after } | IssueOutcome::RateLimited { retry_after },
        ) => Ok(Login::Wait(retry_after)),
        // Not `+<digits>`, or a BSUID: refused before anything is stored or sent.
        Err(Error::Validation(v)) if v.field == "recipient" => {
            Ok(Login::NotAWhatsAppNumber(v.reason))
        }
        Err(e) => Err(e), // after a timeout or 5xx the code may have arrived: do not re-issue at once
    }
}

/// Step 2: every call with a live code counts as an attempt.
pub async fn check_code(otp: &OtpService, user_input: &str, typed: &str) -> wa_rs::Result<Login> {
    let user = Recipient::phone(user_input);
    Ok(match otp.verify(&user, "login", typed.trim()).await? {
        VerifyOutcome::Verified => Login::SignedIn, // consumed: single use
        VerifyOutcome::Invalid { attempts_left } => Login::WrongCode { attempts_left },
        VerifyOutcome::Expired | VerifyOutcome::NotFound | VerifyOutcome::TooManyAttempts => {
            Login::RequestANewCode
        }
    })
}

/// Tests pass their own clock (and the same one to the store).
pub fn with_clock(
    otp_client: Client,
    kv: Arc<dyn KvStore>,
    clock: Arc<dyn Clock>,
) -> wa_rs::Result<OtpService> {
    OtpService::new(
        otp_client,
        "106540352242922",
        OtpTemplate::new("login_code", "en_US"),
        kv,
        clock,
        OtpPepper::new(vec![7u8; 32])?,
        OtpConfig::default(),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use time::macros::datetime;
    use wa_rs::adapters::store::MemoryKvStore;
    use wa_rs::core::clock::ManualClock;
    use wa_rs::core::testing::ScriptedTransport;

    use super::*;

    const USER: &str = "+16505551234";

    fn scripted() -> (ScriptedTransport, Client) {
        let transport = ScriptedTransport::new();
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        (transport, client)
    }

    /// The code is the body parameter of the authentication template.
    fn sent_code(transport: &ScriptedTransport) -> String {
        let body = transport.last_request().unwrap().json().unwrap();
        assert_eq!(body["to"], USER);
        body["template"]["components"][0]["parameters"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn issue_then_verify_once() {
        let (transport, client) = scripted();
        transport.push_json(200, json!({"messages": [{"id": "wamid.OTP"}]}));
        let clock = Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC)));
        let kv = Arc::new(MemoryKvStore::with_clock(clock.clone()));
        let otp = with_clock(client, kv, clock.clone()).unwrap();

        assert_eq!(request_code(&otp, USER).await.unwrap(), Login::CodeSent);
        let code = sent_code(&transport);
        let wrong: String = code
            .chars()
            .map(|c| char::from_digit((c.to_digit(10).unwrap() + 1) % 10, 10).unwrap())
            .collect();
        assert_eq!(
            check_code(&otp, USER, &wrong).await.unwrap(),
            Login::WrongCode { attempts_left: 4 }
        );
        assert_eq!(
            check_code(&otp, USER, &format!(" {code} ")).await.unwrap(),
            Login::SignedIn
        );
        assert_eq!(
            check_code(&otp, USER, &code).await.unwrap(),
            Login::RequestANewCode
        ); // single use
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn codes_expire_and_resends_cool_down() {
        let (transport, client) = scripted();
        transport.push_json(200, json!({"messages": [{"id": "wamid.OTP"}]}));
        let clock = Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC)));
        let kv = Arc::new(MemoryKvStore::with_clock(clock.clone()));
        let otp = with_clock(client, kv, clock.clone()).unwrap();

        request_code(&otp, USER).await.unwrap();
        let code = sent_code(&transport);
        assert!(matches!(
            request_code(&otp, USER).await.unwrap(),
            Login::Wait(_)
        )); // nothing sent
        clock.advance(Duration::from_mins(11));
        assert_eq!(
            check_code(&otp, USER, &code).await.unwrap(),
            Login::RequestANewCode
        );
        assert_eq!(transport.requests().len(), 1);
    }

    #[tokio::test]
    async fn only_e164_phone_numbers_get_codes() {
        let (transport, client) = scripted();
        let kv = Arc::new(MemoryKvStore::new());
        let otp = with_clock(client, kv, Arc::new(SystemClock)).unwrap();
        for bad in ["16505551234", "US.13491208655302741918"] {
            let answer = request_code(&otp, bad).await.unwrap();
            assert!(
                matches!(answer, Login::NotAWhatsAppNumber(_)),
                "{bad}: {answer:?}"
            );
        }
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn a_blank_namespace_is_a_config_error() {
        let (_, client) = scripted();
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let built = otp_service(
            client,
            "106540352242922".into(),
            kv,
            vec![7; 32],
            Some(" ".into()),
        );
        assert!(matches!(built, Err(Error::Config(_))));
    }
}
