//! WhatsApp OTP login: issue a one-time passcode through an authentication
//! template, then verify what the user types.
//!
//! [`OtpService`] keeps only keyed hashes of codes (and of phone numbers) in
//! a `KvStore` — the memory one here, Postgres or Redis when you run more
//! than one instance — rate-limits issuing per number, and counts verify
//! attempts atomically. Every [`IssueOutcome`] and [`VerifyOutcome`] branch
//! is handled below; map them to your login API's answers.
//!
//! The recipient must be strict E.164 **with** `+`: Meta reads a number
//! without it as local to the sending number's country, so the same digits
//! can reach someone else. A BSUID alone is refused (OTP buttons need a phone
//! number).
//!
//! Codes, cooldowns and issue limits are scoped to the sending phone number
//! id and `OtpConfig::namespace`. One tenant per number, as here, needs
//! nothing more; when one number sends codes for several merchants or
//! tenants, give each tenant's service its own namespace (the tenant id):
//! `OtpConfig { namespace: Some(tenant_id), ..OtpConfig::default() }`.
//! With the default `None` they share one scope, and a code sent for one
//! tenant verifies at another.
//!
//! | Variable | Required | What |
//! | --- | --- | --- |
//! | `WA_TOKEN` | yes | a system user access token with `whatsapp_business_messaging` |
//! | `WA_PHONE_NUMBER_ID` | yes | the business phone number id that sends the code |
//! | `WA_TO` | yes | the user's number, E.164 with `+`, e.g. `+16505551234` |
//! | `WA_OTP_TEMPLATE` | yes | an approved authentication template (copy code or one-tap) |
//! | `WA_OTP_LANGUAGE` | no | the language it was approved in (default `en_US`) |
//! | `WA_OTP_PEPPER` | yes | at least 32 random bytes (`openssl rand -base64 32`); keep it out of the database |
//!
//! ```text
//! WA_TOKEN=… WA_PHONE_NUMBER_ID=… WA_TO=+16505551234 WA_OTP_TEMPLATE=login_code \
//!   WA_OTP_PEPPER="$(openssl rand -base64 32)" cargo run -p wa-rs --example otp_login
//! ```

use std::sync::Arc;

use anyhow::Context as _;
use wa_rs::adapters::store::MemoryKvStore;
use wa_rs::client::authentication::{
    IssueOutcome, OtpConfig, OtpPepper, OtpService, OtpTemplate, VerifyOutcome,
};
use wa_rs::core::clock::SystemClock;
use wa_rs::prelude::*;

/// Separates independent flows for one number (`"login"`, `"reset_password"`).
const PURPOSE: &str = "login";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // RUST_LOG=info (or debug) shows what the library logs.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    // The default config: 6 digits, valid 10 minutes (keep it equal to the
    // template's code_expiration_minutes), 5 attempts, 30 s between codes,
    // at most 5 codes per number and hour.
    let otp = OtpService::new(
        wa_rs::client(env("WA_TOKEN")?)?,
        env("WA_PHONE_NUMBER_ID")?,
        OtpTemplate::new(env("WA_OTP_TEMPLATE")?, env_or("WA_OTP_LANGUAGE", "en_US")),
        Arc::new(MemoryKvStore::new()), // Postgres or Redis with several instances
        Arc::new(SystemClock),
        OtpPepper::new(env("WA_OTP_PEPPER")?)?, // >= 32 bytes, not stored with the codes
        OtpConfig::default(),
    )?;
    let user = Recipient::phone(env("WA_TO")?); // strict E.164, with `+`

    let issued = otp.issue(&user, PURPOSE).await; // Sent, CoolingDown or RateLimited
    match issued {
        Ok(IssueOutcome::Sent(challenge)) => {
            println!("code sent, valid until {}", challenge.expires_at);
        }
        Ok(IssueOutcome::CoolingDown { retry_after }) => {
            // A code went out less than 30 s ago: the user should wait for it.
            return Err(anyhow::anyhow!(
                "a code was just sent; retry in {retry_after:?}"
            ));
        }
        Ok(IssueOutcome::RateLimited { retry_after }) => {
            return Err(anyhow::anyhow!(
                "too many codes for this number; retry in {retry_after:?}"
            ));
        }
        // Not `+<digits>`, or a BSUID alone: refused before anything is
        // stored or sent. 131062 is Meta's answer to the same mistake.
        Err(Error::Validation(v)) if v.field == "recipient" => {
            return Err(anyhow::anyhow!(
                "not a number we can send a code to: {}",
                v.reason
            ));
        }
        Err(e) if e.kind() == ErrorKind::RecipientNotSupported => {
            return Err(anyhow::anyhow!("this recipient cannot receive codes"));
        }
        // An error after the request left (timeout, 5xx) keeps the code
        // verifiable: it may have been delivered. Do not issue again at once.
        Err(e) => return Err(e.into()),
    }

    // Asking again straight away is refused without sending anything.
    if let IssueOutcome::CoolingDown { retry_after } = otp.issue(&user, PURPOSE).await? {
        println!("(a second request now would wait {retry_after:?})");
    }

    loop {
        let code = read_line("code from WhatsApp: ").await?;
        let verified = otp.verify(&user, PURPOSE, code.trim()).await?; // counts as an attempt
        match verified {
            VerifyOutcome::Verified => {
                // Consumed: the same code never verifies again.
                println!("verified: sign the user in");
                return Ok(());
            }
            VerifyOutcome::Invalid { attempts_left } => {
                println!("wrong code, {attempts_left} attempts left");
            }
            VerifyOutcome::TooManyAttempts => {
                println!("too many wrong codes: this one is dead, request a new one");
                return Ok(());
            }
            VerifyOutcome::Expired => {
                println!("the code expired: request a new one");
                return Ok(());
            }
            VerifyOutcome::NotFound => {
                println!("no code outstanding for this number: request one");
                return Ok(());
            }
        }
    }
}

/// One line from stdin, without blocking the runtime.
async fn read_line(prompt: &'static str) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || {
        print!("{prompt}");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        anyhow::ensure!(!line.is_empty(), "stdin closed");
        Ok(line)
    })
    .await?
}

/// `std::env::var`, naming the variable when it is missing.
fn env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("set {name} (see the example's header)"))
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}
