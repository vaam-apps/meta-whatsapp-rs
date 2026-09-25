//! Reference code for the `meta-whatsapp-rs-production` skill: secrets loaded once and
//! checked at startup, logs without customer data, one tuned client per
//! process, and the numbers worth a metric.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use std::time::Duration;

use meta_whatsapp_rs::core::config::ApiVersion;
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::webhooks::DeliveryReport;

/// Everything secret, read once from your secret manager (the environment
/// here). A missing or blank value stops the process at boot, not at the
/// first webhook.
pub struct Settings {
    pub system_user_token: AccessToken,
    pub app_secrets: Vec<AppSecret>, // current first; the previous one while rotating
    pub verify_token: VerifyToken,
    pub vault_key_b64: String, // not in the database that holds the vault
    pub otp_pepper: Vec<u8>,   // not in the database that holds the OTP challenges
}

fn required(name: &str) -> anyhow::Result<String> {
    let value = std::env::var(name).map_err(|_| anyhow::anyhow!("{name} is not set"))?;
    anyhow::ensure!(!value.trim().is_empty(), "{name} is blank");
    Ok(value)
}

impl Settings {
    pub fn load() -> anyhow::Result<Self> {
        let mut app_secrets = vec![AppSecret::new(required("WA_APP_SECRET")?)];
        if let Ok(previous) = required("WA_APP_SECRET_PREVIOUS") {
            app_secrets.push(AppSecret::new(previous));
        }
        Ok(Self {
            system_user_token: AccessToken::new(required("WA_SYSTEM_USER_TOKEN")?),
            app_secrets,
            verify_token: VerifyToken::new(required("WA_VERIFY_TOKEN")?),
            vault_key_b64: required("WA_VAULT_KEY")?,
            otp_pepper: required("WA_OTP_PEPPER")?.into_bytes(),
        })
    }
}

/// meta-whatsapp-rs logs through `tracing`: sizes, digests, field names, error kinds,
/// never payload values or secrets. `RUST_LOG=info,meta_whatsapp_client=debug`.
pub fn init_logs() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
}

/// One client per process, cloned everywhere; `with_token` per merchant.
pub fn client(settings: &Settings) -> meta_whatsapp_rs::Result<Client> {
    meta_whatsapp_rs::client_builder()?
        .access_token(settings.system_user_token.clone())
        .api_version(ApiVersion::new(25, 0)) // moves only when you change it
        .timeout(Duration::from_secs(15))
        .retry(RetryPolicy {
            max_retries: 2,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(4),
        })
        .build()
}

/// Worth a metric: webhook outcomes, event kinds, failed sends by kind.
pub fn record_webhook(result: &meta_whatsapp_rs::Result<DeliveryReport>, events: &[WebhookEvent]) {
    match result {
        Ok(report) => tracing::info!(
            delivered = report.delivered,
            duplicates = report.duplicates,
            unparsed = report.unparsed, // alert when > 0
            "webhook"
        ),
        Err(e) => tracing::warn!(kind = ?e.kind(), "webhook refused"), // a run of 503s: sinks outlast the lease
    }
    for event in events {
        tracing::debug!(kind = event.kind(), "event"); // `Unknown` rising: Meta shipped a field
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_shows_secrets() {
        let token = AccessToken::new("EAAB-very-secret");
        let secret = AppSecret::new("app-secret-value");
        let shown = format!("{token:?} {secret:?}");
        assert!(
            !shown.contains("very-secret") && !shown.contains("app-secret-value"),
            "{shown}"
        );
    }

    #[test]
    fn the_client_is_pinned() {
        let settings = Settings {
            system_user_token: AccessToken::new("T"),
            app_secrets: vec![AppSecret::new("s")],
            verify_token: VerifyToken::new("v"),
            vault_key_b64: String::new(),
            otp_pepper: Vec::new(),
        };
        let client = client(&settings).unwrap();
        assert_eq!(client.endpoint().version(), ApiVersion::new(25, 0));
        assert!(format!("{client:?}").contains("has_token: true"));
        assert!(!format!("{client:?}").contains("\"T\""));
    }
}
