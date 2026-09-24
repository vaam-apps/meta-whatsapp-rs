//! Reference code for the Solution Partner part of the
//! `wa-rs-embedded-signup` skill: the deployment decides once whether it
//! onboards as a Tech Provider (merchants pay Meta) or as a Solution
//! Partner (your credit line pays), then onboarding, resume and the
//! revocation after `PARTNER_REMOVED` follow from that choice.
//!
//! wa-rs compiles this file and runs its tests in its own gate
//! (`crates/wa-rs/tests/skills.rs`).

use wa_rs::client::credit_lines::WabaCurrency;
use wa_rs::client::embedded_signup::{
    CreditSharing, EmbeddedSignup, OnboardingRequest, SolutionPartner, TokenVault,
};
use wa_rs::core::ids::{AllocationConfigId, BusinessId, CreditLineId};
use wa_rs::prelude::*;
use wa_rs::webhooks::fields::AccountUpdateEvent;

/// Your partner settings, from your secret manager and configuration.
pub struct PartnerSettings {
    pub system_token: AccessToken, // your system user's token (business_management)
    pub system_user_id: String,    // added to each merchant's WABA before sharing
    pub credit_line_id: String,    // CreditLines::list, once
    pub default_currency: Option<String>, // AUD, EUR, GBP, IDR, INR or USD
}

/// Once, at startup: `None` keeps the Tech Provider flow.
pub fn onboarding_mode(
    es: EmbeddedSignup,
    partner: Option<PartnerSettings>,
) -> wa_rs::Result<EmbeddedSignup> {
    let Some(p) = partner else { return Ok(es) }; // Tech Provider: merchants add a payment method
    let mut sp = SolutionPartner::new(p.system_token, p.system_user_id, p.credit_line_id)
        .method(CreditSharing::ShareAndAttach); // Meta's current method (the default)
    if let Some(code) = p.default_currency {
        sp = sp.default_currency(code.parse::<WabaCurrency>()?); // only the six Meta lists
    }
    Ok(es.solution_partner(sp))
}

/// Per merchant: the currency they are invoiced in, else the default.
/// Checked before the code is exchanged; a line cannot change once attached.
pub fn with_currency(
    request: OnboardingRequest,
    merchant_currency: Option<&str>,
) -> wa_rs::Result<OnboardingRequest> {
    Ok(match merchant_currency {
        Some(code) => request.currency(code.parse::<WabaCurrency>()?),
        None => request, // SolutionPartner::default_currency, or a validation error
    })
}

/// `account_update`: when a merchant removes you, revoke at once. The WABA
/// can no longer be read; the owner business stored at onboarding is used.
pub async fn on_account_update(
    es: &EmbeddedSignup,
    vault: &TokenVault,
    event: &WebhookEvent,
) -> wa_rs::Result<Vec<AllocationConfigId>> {
    let WebhookEvent::AccountUpdated { update, .. } = event else {
        return Ok(Vec::new());
    };
    if update.event != AccountUpdateEvent::PartnerRemoved {
        return Ok(Vec::new());
    }
    // The merchant's WABA is in waba_info: Meta's PARTNER_* examples carry
    // a business id, not the WABA, as the entry id (`event.waba_id()`).
    let Some(waba_id) = update.waba_info.as_ref().and_then(|i| i.waba_id.as_ref()) else {
        return Ok(Vec::new());
    };
    es.revoke_credit_line(waba_id, vault).await // every WABA of that business
}

/// Once, to configure `credit_line_id`: your portfolio's credit lines.
pub async fn credit_lines(
    client: &Client,
    system_token: AccessToken,
    partner_business_id: &str,
) -> wa_rs::Result<Vec<CreditLineId>> {
    let page = client
        .with_token(system_token)
        .credit_lines()
        .list(
            &BusinessId::new(partner_business_id),
            &["id", "legal_entity_name"],
        )
        .await?;
    Ok(page.data.into_iter().map(|line| line.id).collect())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use wa_rs::adapters::store::MemoryKvStore;
    use wa_rs::client::embedded_signup::{
        EmbeddedSignupEvent, SignupCode, StoredBusinessToken, VaultKey, VaultKeys, steps,
    };
    use wa_rs::core::testing::ScriptedTransport;
    use wa_rs::webhooks::WebhookPayload;

    use super::*;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="; // test only

    fn settings(currency: Option<&str>) -> PartnerSettings {
        PartnerSettings {
            system_token: AccessToken::new("SYSTEM_TOKEN"),
            system_user_id: "1972555232742222".into(),
            credit_line_id: "1972385232742146".into(),
            default_currency: currency.map(str::to_owned),
        }
    }

    fn signup(transport: &ScriptedTransport) -> EmbeddedSignup {
        let client = Client::builder()
            .transport(transport.clone())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        client.embedded_signup(AppCredentials::new("1234", "app-secret"))
    }

    fn vault() -> TokenVault {
        TokenVault::new(
            Arc::new(MemoryKvStore::new()),
            VaultKeys::new(VaultKey::from_base64("2026-09", KEY).unwrap()),
        )
        .unwrap()
    }

    fn request() -> OnboardingRequest {
        let event = EmbeddedSignupEvent::from_json(
            r#"{"type":"WA_EMBEDDED_SIGNUP","event":"FINISH","data":{"waba_id":"102290129340398"}}"#,
        )
        .unwrap();
        OnboardingRequest::from_event(SignupCode::new("code").unwrap(), &event).unwrap()
    }

    #[tokio::test]
    async fn without_a_currency_nothing_is_spent() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(None))).unwrap();
        let err = es.onboard(&request(), &vault()).await.unwrap_err();
        assert!(matches!(err, Error::Validation(ref v) if v.field == "currency"));
        assert!(
            transport.requests().is_empty(),
            "the code was not exchanged"
        );
        // Unknown codes are refused when parsed.
        assert!(with_currency(request(), Some("usd")).is_err());
        assert!(onboarding_mode(signup(&transport), Some(settings(Some("BRL")))).is_err());
        // The Tech Provider flow takes no currency.
        let tp = onboarding_mode(signup(&transport), None).unwrap();
        let err = tp
            .onboard(&with_currency(request(), Some("USD")).unwrap(), &vault())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Validation(ref v) if v.field == "currency"));
        assert!(transport.requests().is_empty());
    }

    #[tokio::test]
    async fn partner_removed_revokes_from_the_stored_owner() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        vault
            .store(
                &StoredBusinessToken::new("980198427658004", AccessToken::new("EAAB"))
                    .business_id("2329417887457253"),
            )
            .await
            .unwrap();
        transport.push_json(
            200,
            json!({"id": "58501441721238", "receiving_business": {"id": "2329417887457253"}}),
        );
        transport.push_json(200, json!({"success": true}));
        // Meta's `PARTNER_REMOVED` example (webhooks/reference/account_update).
        let body = json!({"object": "whatsapp_business_account", "entry": [{
            "id": "2949482758682047", "time": 1748477359,
            "changes": [{"field": "account_update", "value": {
                "event": "PARTNER_REMOVED",
                "waba_info": {"waba_id": "980198427658004", "owner_business_id": "2329417887457253"}
            }}]
        }]});
        let events = WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events();
        let revoked = on_account_update(&es, &vault, &events[0]).await.unwrap();
        assert_eq!(revoked, [AllocationConfigId::new("58501441721238")]);
        let requests = transport.requests();
        assert_eq!(
            requests[0].query("receiving_business_id").as_deref(),
            Some("2329417887457253")
        );
        assert_eq!(requests[1].method.as_str(), "DELETE");
        assert_eq!(requests[1].bearer(), Some("SYSTEM_TOKEN"));
        assert_eq!(transport.remaining(), 0);
        assert_eq!(steps::SHARE_CREDIT_LINE, "share_credit_line");
    }
}
