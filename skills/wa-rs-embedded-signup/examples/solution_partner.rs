//! Reference code for the Solution Partner part of the
//! `wa-rs-embedded-signup` skill: the deployment decides once whether it
//! onboards as a Tech Provider (merchants pay Meta) or as a Solution
//! Partner (your credit line pays), then onboarding, resume, offboarding
//! and the revocation after `PARTNER_REMOVED` follow from that choice.
//!
//! wa-rs compiles this file and runs its tests in its own gate
//! (`crates/wa-rs/tests/skills.rs`).

use wa_rs::client::credit_lines::{CreditRevocation, WabaCurrency};
use wa_rs::client::embedded_signup::{
    CreditSharing, EmbeddedSignup, Onboarded, OnboardingRequest, SolutionPartner, TokenVault,
};
use wa_rs::core::error::ValidationError;
use wa_rs::core::ids::{BusinessId, CreditLineId, WabaId};
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

/// The callback: onboard behind your tenant check. It runs once Meta has
/// verified the WABA and before anything is stored, subscribed or shared
/// (step `approve`): a credit line cannot be taken back from a WABA once
/// attached, so checking after `onboard` is too late.
pub async fn onboard_for_tenant(
    es: &EmbeddedSignup,
    vault: &TokenVault,
    request: &OnboardingRequest,
    tenant: &str,
    bound_to: impl FnOnce(&WabaId) -> Option<String>, // your WABA → tenant table
) -> wa_rs::Result<Onboarded> {
    es.onboard_with_approval(request, vault, |verified| {
        let taken = bound_to(&verified.waba_id).is_some_and(|other| other != tenant);
        async move {
            if taken {
                return Err(
                    ValidationError::new("waba_id", "connected to another merchant").into(),
                );
            }
            Ok(())
        }
    })
    .await
}

/// Funding a merchant again after a revocation is a product decision:
/// without this, `onboard` and `resume` refuse
/// (`EmbeddedSignup::is_credit_line_revoked(&err)`).
pub fn fund_again(request: OnboardingRequest) -> OnboardingRequest {
    request.reshare_after_revocation()
}

/// `account_update`, from a signature-checked delivery only: the
/// `owner_business_id` it carries is what revocation falls back on when the
/// vault no longer knows the merchant.
pub async fn on_account_update(
    es: &EmbeddedSignup,
    vault: &TokenVault,
    event: &WebhookEvent,
) -> wa_rs::Result<Option<CreditRevocation>> {
    let WebhookEvent::AccountUpdated { update, .. } = event else {
        return Ok(None);
    };
    // The merchant's WABA: `waba_info.waba_id` for the PARTNER_* events.
    let Some(waba_id) = event.waba_id() else {
        return Ok(None);
    };
    let owner = update
        .waba_info
        .as_ref()
        .and_then(|i| i.owner_business_id.as_ref());
    match update.event {
        // Unshared: messaging on the WABA is blocked and Meta recommends
        // revoking at once. Revocation is per business: its other WABAs
        // lose the line too, and funding it again needs an explicit opt-in.
        AccountUpdateEvent::PartnerRemoved => {
            Ok(Some(es.revoke_credit_line(waba_id, owner, vault).await?))
        }
        // The app was removed: revoke first, then forget the token.
        AccountUpdateEvent::PartnerAppUninstalled => {
            Ok(es.offboard(waba_id, owner, vault).await?.credit)
        }
        _ => Ok(None),
    }
}

/// The merchant disconnects in your CMS: stop the webhooks while the token
/// still works, then offboard (revoke first, delete second).
pub async fn disconnect(
    es: &EmbeddedSignup,
    client: &Client,
    vault: &TokenVault,
    waba_id: &WabaId,
) -> wa_rs::Result<Option<CreditRevocation>> {
    if let Some(stored) = vault.get(waba_id).await? {
        let merchant = client.with_token(stored.token);
        if let Err(e) = merchant.waba(waba_id.clone()).unsubscribe_app().await {
            tracing::warn!(kind = ?e.kind(), "unsubscribe failed; offboarding anyway");
        }
    }
    Ok(es.offboard(waba_id, None, vault).await?.credit)
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
    async fn another_tenants_waba_is_refused_before_anything_is_stored() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        transport.push_json(200, json!({"access_token": "EAAB"}));
        transport.push_json(
            200,
            json!({"data": {"app_id": "1234", "is_valid": true, "granular_scopes": [
                {"scope": "whatsapp_business_management", "target_ids": ["102290129340398"]}]}}),
        );
        transport.push_json(
            200,
            json!({"owner_business_info": {"id": "2729063490586005"}, "id": "102290129340398"}),
        );
        transport.push_json(200, json!({"data": []}));
        let err = onboard_for_tenant(&es, &vault, &request(), "m42", |_| Some("m7".into()))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::Step {
                    step: steps::APPROVE,
                    ..
                }
            ),
            "{err}"
        );
        assert!(
            vault
                .get(&"102290129340398".into())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(transport.remaining(), 0, "no subscribe, no credit call");
        assert!(fund_again(request()).reshare_after_revocation);
    }

    fn partner_event(event: &str) -> WebhookEvent {
        // Meta's examples (webhooks/reference/account_update): the entry id
        // is a business portfolio, the WABA is in waba_info.
        let body = json!({"object": "whatsapp_business_account", "entry": [{
            "id": "2949482758682047", "time": 1748477359,
            "changes": [{"field": "account_update", "value": {
                "event": event,
                "waba_info": {"waba_id": "980198427658004", "owner_business_id": "2329417887457253"}
            }}]
        }]});
        WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events()
            .remove(0)
    }

    /// Lookup, status, DELETE, status: one record revoked.
    fn script_revocation(transport: &ScriptedTransport) {
        let business = json!({"id": "2329417887457253"});
        transport.push_json(
            200,
            json!({"id": "58501441721238", "receiving_business": business}),
        );
        transport.push_json(200, json!({"receiving_business": business}));
        transport.push_json(200, json!({"success": true}));
        transport.push_json(
            200,
            json!({"receiving_business": business, "request_status": "DELETED"}),
        );
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
        script_revocation(&transport);
        let revoked = on_account_update(&es, &vault, &partner_event("PARTNER_REMOVED"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(revoked.revoked.len(), 1);
        let requests = transport.requests();
        assert_eq!(
            requests[0].query("receiving_business_id").as_deref(),
            Some("2329417887457253")
        );
        assert_eq!(requests[2].method.as_str(), "DELETE");
        assert_eq!(requests[2].bearer(), Some("SYSTEM_TOKEN"));
        assert_eq!(transport.remaining(), 0);
        assert!(
            vault
                .get(&"980198427658004".into())
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(steps::SHARE_CREDIT_LINE, "share_credit_line");
    }

    /// The app removed first, the WABA unshared second: the line is revoked
    /// once, the token deleted, and the second event still finds the owner
    /// (from the ledger, or the webhook's `owner_business_id`).
    #[tokio::test]
    async fn uninstall_then_removal_ends_revoked_and_deleted() {
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
        script_revocation(&transport);
        on_account_update(&es, &vault, &partner_event("PARTNER_APP_UNINSTALLED"))
            .await
            .unwrap();
        assert!(
            vault
                .get(&"980198427658004".into())
                .await
                .unwrap()
                .is_none()
        );
        let business = json!({"id": "2329417887457253"});
        transport.push_json(
            200,
            json!({"id": "58501441721238", "receiving_business": business}),
        );
        transport.push_json(
            200,
            json!({"receiving_business": business, "request_status": "DELETED"}),
        );
        let again = on_account_update(&es, &vault, &partner_event("PARTNER_REMOVED"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.already_revoked.len(), 1);
        assert_eq!(transport.remaining(), 0);
    }
}
