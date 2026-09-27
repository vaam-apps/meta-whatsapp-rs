//! Reference code for the `meta-whatsapp-rs-production` skill: secrets loaded once and
//! checked at startup, logs without customer data, one tuned client per
//! process, the numbers worth a metric, and the contact book step of an
//! erasure.
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

/// Erasure step 4, Meta's contact book: each BSUID among `identities`,
/// the ones step 1 collected (`Inbox::identities` on each number). Not
/// collected again here: step 2's `erase_all` removed the contacts and
/// links that connect them, so the store no longer knows them. With the
/// merchant's client (`with_token`) on one of their numbers (the book is
/// the portfolio's). The identities also hold contact keys, phone numbers
/// and parent BSUIDs, which the call refuses before any request: skip
/// them, never abort the erasure. Returns the BSUIDs Meta failed on, to
/// try again later.
pub async fn delete_from_contact_book(
    merchant: &Client,
    phone_number_id: PhoneNumberId,
    identities: &[String],
) -> Vec<UserId> {
    let number = merchant.phone_number(phone_number_id);
    let mut failed = Vec::new();
    for id in identities {
        let id = UserId::new(id.as_str());
        if !id.is_bsuid() {
            continue; // a contact key, a phone number or a parent BSUID
        }
        if let Err(e) = number.delete_contact_book_entry(&id).await {
            tracing::warn!(kind = ?e.kind(), "contact book entry kept"); // never log the BSUID
            failed.push(id); // the erasure goes on: steps 5 and 6 still run
        }
    }
    failed // no undo; a repeat answers `false`
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

    #[tokio::test]
    async fn the_contact_book_gets_the_bsuids_only_and_a_failure_stops_nothing() {
        use std::sync::Arc;

        use meta_whatsapp_rs::adapters::store::MemoryConversationStore;
        use meta_whatsapp_rs::core::store::{IdentityLink, StoredContact};
        use meta_whatsapp_rs::core::testing::ScriptedTransport;
        use serde_json::json;
        use time::OffsetDateTime;

        const NUMBER: &str = "106540352242922";
        const BSUID: &str = "US.13491208655302741918";
        let store: Arc<dyn ConversationStore> = Arc::new(MemoryConversationStore::new());
        // The address book ties their phone number, BSUID and parent BSUID;
        // an earlier BSUID is linked to the current one.
        store
            .put_contact(StoredContact {
                key: ConversationKey::new(NUMBER, "16505551234"),
                full_name: None,
                first_name: None,
                phone_number: Some("16505551234".to_owned()),
                user_id: Some(UserId::new(BSUID)),
                parent_user_id: Some(UserId::new("US.ENT.11815799212886844830")),
                username: None,
                synced_at: OffsetDateTime::now_utc(),
            })
            .await
            .unwrap();
        store
            .link_identity(IdentityLink::new(
                NUMBER,
                "US.1",
                BSUID,
                OffsetDateTime::now_utc(),
            ))
            .await
            .unwrap();
        let transport = ScriptedTransport::new();
        transport.push_json(500, json!({"error": {"message": "x", "code": 1}}));
        transport.push_json(
            200,
            json!({"messaging_product": "whatsapp", "success": true, "deleted": true}),
        );
        let merchant = Client::builder()
            .transport(transport.clone())
            .access_token("MERCHANT")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let inbox = Inbox::new(merchant.clone(), NUMBER, store);

        // The procedure's order: step 1 collects every identity, step 2
        // erases them (after which the store connects nothing to the
        // phone number), then step 4 deletes with what step 1 collected.
        let key = inbox.key("16505551234");
        let identities: Vec<String> = inbox.identities(&key).await.unwrap().into_iter().collect();
        inbox.erase_all(&identities).await.unwrap();
        assert_eq!(inbox.identities(&key).await.unwrap().len(), 1);
        let failed = delete_from_contact_book(&merchant, NUMBER.into(), &identities).await;
        assert_eq!(failed, [UserId::new("US.1")]); // Meta failed on it; the next one still went
        let requests = transport.requests();
        let asked: Vec<String> = requests.iter().map(|r| r.query("bsuid").unwrap()).collect();
        assert_eq!(asked, ["US.1", BSUID]); // not the phone number, not the parent BSUID
        for request in &requests {
            assert_eq!(request.method, "DELETE");
            assert_eq!(request.path(), "/v25.0/106540352242922/contact_book");
            assert_eq!(request.bearer(), Some("MERCHANT"));
        }
        assert_eq!(transport.remaining(), 0);
    }
}
