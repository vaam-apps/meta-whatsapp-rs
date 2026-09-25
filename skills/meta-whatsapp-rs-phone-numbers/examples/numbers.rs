//! Reference code for the `meta-whatsapp-rs-phone-numbers` skill: registering a
//! number, its two-step verification PIN, display name, business profile,
//! conversational components, and the WABA's webhook subscription.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use meta_whatsapp_rs::client::business_profile::{ProfileField, ProfileUpdate};
use meta_whatsapp_rs::client::phone_numbers::{
    BotCommand, ConversationalAutomationConfig, SmbSyncType, TwoStepPin,
};
use meta_whatsapp_rs::client::waba::CallbackOverride;
use meta_whatsapp_rs::prelude::*;

/// Register a number for Cloud API: `pin` becomes its two-step PIN, or must
/// match the one it has (wrong PIN: `ErrorKind::TwoStepVerification`).
pub async fn register(
    client: &Client,
    phone_number_id: PhoneNumberId,
    pin_typed_by_the_merchant: &str,
) -> meta_whatsapp_rs::Result<()> {
    let pin = TwoStepPin::new(pin_typed_by_the_merchant)?; // exactly 6 digits; never log it
    let number = client.phone_number(phone_number_id);
    number.register(&pin, None).await // 10 (de)registrations per 72 h: never in a retry loop
}

/// Make sure the WABA's webhooks reach this app (Embedded Signup already
/// does this for merchants' WABAs).
pub async fn ensure_subscribed(
    client: &Client,
    waba_id: WabaId,
    app_id: &str,
) -> meta_whatsapp_rs::Result<()> {
    let waba = client.waba(waba_id);
    let apps = waba.subscribed_apps().await?;
    if !apps
        .data
        .iter()
        .any(|a| a.whatsapp_business_api_data.id == app_id)
    {
        waba.subscribe_app(None).await?; // Some(&CallbackOverride) sends them elsewhere
    }
    Ok(())
}

/// One number's `messages` (and other supported fields) to another
/// callback; account and template webhooks keep going to the app's.
pub async fn route_number_elsewhere(
    client: &Client,
    phone_number_id: PhoneNumberId,
) -> meta_whatsapp_rs::Result<()> {
    let callback = CallbackOverride::new(
        "https://eu.cms.example/webhooks/whatsapp",
        "a-random-verify-token",
    );
    client
        .phone_number(phone_number_id)
        .set_webhook_override(&callback)
        .await
}

/// The profile customers see, and the chat's first screen.
pub async fn set_up_profile(
    client: &Client,
    phone_number_id: PhoneNumberId,
) -> meta_whatsapp_rs::Result<()> {
    let update = ProfileUpdate {
        about: Some("Linen and leather, made in Berlin".into()),
        email: Some("hello@shop.example".into()),
        websites: Some(vec!["https://shop.example".into()]),
        ..ProfileUpdate::default() // `None` fields are left as they are
    };
    client
        .business_profile(phone_number_id.clone())
        .update(&update)
        .await?;
    let components = ConversationalAutomationConfig::new()
        .prompts(["Where is my order?", "Opening hours"]) // at most 4 ice breakers
        .commands([BotCommand::new("track", "Track an order")]);
    let number = client.phone_number(phone_number_id);
    number
        .configure_conversational_automation(&components)
        .await
}

/// Coexistence (the merchant keeps the WhatsApp Business app): once per
/// sync type, within 24 hours of onboarding; the data arrives by webhook.
pub async fn start_coexistence_sync(
    merchant: &Client,
    phone_number_id: PhoneNumberId,
) -> meta_whatsapp_rs::Result<()> {
    let number = merchant.phone_number(phone_number_id);
    number
        .sync_smb_app_data(SmbSyncType::SmbAppStateSync)
        .await?; // contacts
    number.sync_smb_app_data(SmbSyncType::History).await?; // a second call: SyncNotAllowed
    Ok(())
}

/// A new display name is reviewed; once approved, register again to apply it.
pub async fn rename(
    client: &Client,
    phone_number_id: PhoneNumberId,
) -> meta_whatsapp_rs::Result<()> {
    let number = client.phone_number(phone_number_id);
    number.request_display_name_change("Example Boutique").await // watch PhoneNumberNameUpdated
}

/// What the number looks like to Meta right now.
pub async fn health(
    client: &Client,
    phone_number_id: PhoneNumberId,
) -> meta_whatsapp_rs::Result<()> {
    let number = client.phone_number(phone_number_id.clone());
    let info = number
        .get(&["quality_rating", "name_status", "status"])
        .await?;
    let profile = client
        .business_profile(phone_number_id)
        .get(&[ProfileField::About])
        .await?;
    tracing::info!(quality = ?info.quality_rating, about = ?profile.about, "number health");
    Ok(())
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use serde_json::json;

    use super::*;

    fn client(transport: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn register_sends_the_pin_once() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"success": true}));
        register(&client(&transport), "106540352242922".into(), "581063")
            .await
            .unwrap();
        let request = transport.last_request().unwrap();
        assert_eq!(request.path(), "/v25.0/106540352242922/register");
        assert_eq!(
            request.json().unwrap(),
            json!({"messaging_product": "whatsapp", "pin": "581063"})
        );
        let short = register(&client(&transport), "106540352242922".into(), "1234").await;
        assert!(matches!(short, Err(Error::Validation(ref v)) if v.field == "pin"));
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn subscribes_only_when_missing() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"data": []}));
        transport.push_json(200, json!({"success": true}));
        ensure_subscribed(&client(&transport), "102290129340398".into(), "1234")
            .await
            .unwrap();
        let requests = transport.requests();
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].path(), "/v25.0/102290129340398/subscribed_apps");
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn coexistence_sync_asks_once_per_type() {
        let transport = ScriptedTransport::new();
        transport.push_json(
            200,
            json!({"messaging_product": "whatsapp", "request_id": "r1"}),
        );
        transport.push_json(
            200,
            json!({"messaging_product": "whatsapp", "request_id": "r2"}),
        );
        start_coexistence_sync(&client(&transport), "106540352242922".into())
            .await
            .unwrap();
        let kinds: Vec<_> = transport
            .requests()
            .iter()
            .map(|r| r.json().unwrap()["sync_type"].clone())
            .collect();
        assert_eq!(kinds, [json!("smb_app_state_sync"), json!("history")]);
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn profile_and_components() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"success": true}));
        transport.push_json(200, json!({"success": true}));
        set_up_profile(&client(&transport), "106540352242922".into())
            .await
            .unwrap();
        let requests = transport.requests();
        let profile = requests[0].json().unwrap();
        assert_eq!(profile["messaging_product"], "whatsapp");
        assert_eq!(profile["websites"], json!(["https://shop.example"]));
        assert!(profile.get("address").is_none()); // unset fields are not sent
        assert_eq!(
            requests[1].path(),
            "/v25.0/106540352242922/conversational_automation"
        );
        assert_eq!(transport.remaining(), 0);
    }
}
