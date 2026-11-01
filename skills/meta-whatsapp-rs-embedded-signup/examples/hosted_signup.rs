//! Reference code for the hosted Embedded Signup part of the
//! `meta-whatsapp-rs-embedded-signup` skill: Meta's own onboarding page,
//! no JavaScript on yours. The `PARTNER_ADDED` `account_update` webhook
//! starts it; the business token comes from `system_user_access_tokens`
//! with your system user token and its `appsecret_proof`; then the code
//! flow's steps, and the same vault record.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use meta_whatsapp_rs::client::embedded_signup::{
    EmbeddedSignup, HostedSignup, Onboarded, OnboardingRequest, TokenVault,
};
use meta_whatsapp_rs::client::phone_numbers::TwoStepPin;
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::webhooks::fields::AccountUpdateEvent;

/// The hosted signup a verified webhook event announces, if it is one:
/// `account_update` `PARTNER_ADDED`, its `waba_info` naming the WABA and
/// the customer's business portfolio.
pub fn hosted_signup(event: &WebhookEvent) -> Option<HostedSignup> {
    let WebhookEvent::AccountUpdated { update, .. } = event else {
        return None;
    };
    if update.event != AccountUpdateEvent::PartnerAdded {
        return None;
    }
    let info = update.waba_info.as_ref()?;
    let (waba_id, owner) = (info.waba_id.clone()?, info.owner_business_id.clone()?);
    Some(HostedSignup::new(waba_id, owner))
}

/// Onboard it, unless the WABA is onboarded already: the code flow's
/// onboardings send `PARTNER_ADDED` too. `system` is your system user
/// token; `pin` the two-step PIN you chose for this number and keep (the
/// hosted page asks the merchant for none).
pub async fn onboard_hosted(
    es: &EmbeddedSignup,
    vault: &TokenVault,
    system: &AccessToken,
    signup: &HostedSignup,
    pin: TwoStepPin,
) -> meta_whatsapp_rs::Result<Option<Onboarded>> {
    if vault.get(&signup.waba_id).await?.is_some() {
        return Ok(None); // yours already: nothing to do
    }
    let request = OnboardingRequest::for_hosted(signup).register_with_pin(pin);
    // No code is spent: a failure before `store_token` can run this again;
    // after it, `es.resume(&signup.waba_id, &request, vault)`.
    let done = es.onboard_hosted(signup, system, &request, vault).await?;
    Ok(Some(done)) // the webhook names no merchant: bind done.waba_id yourself
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use meta_whatsapp_rs::adapters::store::MemoryKvStore;
    use meta_whatsapp_rs::client::embedded_signup::{VaultKey, VaultKeys};
    use meta_whatsapp_rs::core::ids::AppId;
    use meta_whatsapp_rs::core::testing::{RecordedBody, ScriptedTransport};
    use meta_whatsapp_rs::webhooks::WebhookPayload;
    use serde_json::json;

    use super::*;

    const APP_ID: &str = "236484624622562";
    const APP_SECRET: &str = "614fc2afde15eee07a26b2fe3eaee9b9";
    const SYSTEM_TOKEN: &str = "EAAJB-PARTNER-SYSTEM-TOKEN";
    /// `echo -n EAAJB-PARTNER-SYSTEM-TOKEN | openssl dgst -sha256 -hmac
    /// 614fc2afde15eee07a26b2fe3eaee9b9` (`hosted-es`, step 4).
    const PROOF: &str = "dc5f466f5a81cf17e395e091a0a86c1abdf0a286f44fff9f8be1128b497e9723";
    const BUSINESS_TOKEN: &str = "EAAN-HOSTED-BUSINESS-TOKEN";
    const PHONE: &str = "106540352242922";

    /// `webhooks/reference/account_update`, § Partner added, verbatim.
    fn partner_added() -> WebhookEvent {
        let payload: WebhookPayload = serde_json::from_value(json!({
          "object": "whatsapp_business_account",
          "entry": [{
            "id": "2949482758682047",
            "time": 1744823932,
            "changes": [{
              "value": {
                "event": "PARTNER_ADDED",
                "waba_info": {
                  "waba_id": "980198427658004",
                  "owner_business_id": "2329417887457253",
                  "solution_id": "1715120619246906",
                  "solution_partner_business_ids": ["2949482758682047", "520744086200222"]
                }
              },
              "field": "account_update"
            }]
          }]
        }))
        .unwrap();
        payload.into_events().remove(0)
    }

    fn wired(t: &ScriptedTransport) -> (EmbeddedSignup, TokenVault) {
        let client = Client::builder()
            .transport(t.clone())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let es =
            client.embedded_signup(AppCredentials::new(AppId::new(APP_ID).unwrap(), APP_SECRET));
        let vault = TokenVault::new(
            Arc::new(MemoryKvStore::new()),
            VaultKeys::new(VaultKey::generate("test").unwrap()),
        )
        .unwrap();
        (es, vault)
    }

    #[tokio::test]
    async fn a_partner_added_onboards_with_the_proof_of_the_app_secret() {
        let signup = hosted_signup(&partner_added()).unwrap();
        assert_eq!(signup.waba_id.as_str(), "980198427658004");
        assert_eq!(signup.owner_business_id.as_str(), "2329417887457253");

        let t = ScriptedTransport::new();
        let (es, vault) = wired(&t);
        t.push_json(200, json!({"access_token": BUSINESS_TOKEN})); // hosted-es, step 5
        t.push_json(200, json!({"data": {
            "app_id": APP_ID, "is_valid": true, "expires_at": 0,
            "granular_scopes": [{"scope": "whatsapp_business_management", "target_ids": ["980198427658004"]}]
        }}));
        t.push_json(200, json!({"owner_business_info": {"name": "Wind & Wool", "id": "2329417887457253"}, "id": "980198427658004"}));
        t.push_json(200, json!({"data": [{"id": PHONE}]}));
        t.push_json(200, json!({"success": true})); // subscribed_apps
        t.push_json(200, json!({"success": true})); // register
        let pin = TwoStepPin::new("581063").unwrap();
        let system_token = AccessToken::new(SYSTEM_TOKEN);
        let done = onboard_hosted(&es, &vault, &system_token, &signup, pin.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.phone_number_id.unwrap().as_str(), PHONE);

        let token_request = &t.requests()[0];
        assert_eq!(
            token_request.path(),
            "/v25.0/2329417887457253/system_user_access_tokens"
        );
        assert_eq!(token_request.url.query(), None);
        assert_eq!(token_request.bearer(), Some(SYSTEM_TOKEN));
        assert_eq!(
            token_request.body,
            RecordedBody::Bytes {
                content_type: "application/x-www-form-urlencoded".into(),
                data: format!("appsecret_proof={PROOF}&fetch_only=true").into(),
            }
        );
        assert_eq!(t.remaining(), 0);
        let stored = vault.get(&signup.waba_id).await.unwrap().unwrap();
        assert_eq!(stored.token.expose_secret(), BUSINESS_TOKEN);

        // Meta redelivers, or the code flow onboarded it: nothing is sent.
        assert!(
            onboard_hosted(&es, &vault, &system_token, &signup, pin)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(t.requests().len(), 6);
    }

    #[test]
    fn other_account_updates_start_nothing() {
        let mut event = partner_added();
        if let WebhookEvent::AccountUpdated { update, .. } = &mut event {
            update.event = AccountUpdateEvent::PartnerRemoved;
        }
        assert!(hosted_signup(&event).is_none());
    }
}
