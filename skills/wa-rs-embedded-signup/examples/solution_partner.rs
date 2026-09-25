//! Reference code for the Solution Partner part of the
//! `wa-rs-embedded-signup` skill: the deployment decides once whether it
//! onboards as a Tech Provider (merchants pay Meta) or as a Solution
//! Partner (your credit line pays), then onboarding behind your approval,
//! resume, offboarding and the `account_update` events that end a
//! merchant's funding follow from that choice.
//!
//! wa-rs compiles this file and runs its tests in its own gate
//! (`crates/wa-rs/tests/skills.rs`).

use std::sync::Arc;

use wa_rs::client::credit_lines::{CreditRevocation, WabaCurrency};
use wa_rs::client::embedded_signup::{
    CreditSharing, EmbeddedSignup, Offboarded, Onboarded, OnboardingRequest, SolutionPartner,
    TokenVault,
};
use wa_rs::core::error::ValidationError;
use wa_rs::core::ids::{BusinessId, CreditLineId, WabaId};
use wa_rs::core::store::{Expiry, KvStore, StoreKey};
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

/// The callback: onboard behind your tenant check. A Solution Partner must
/// (plain `onboard` is refused): the approval runs once Meta has verified
/// the WABA and before anything is stored, subscribed or shared (step
/// `approve`), and is recorded so `resume` may share later. A credit line
/// cannot be taken back from a WABA once attached, so checking after
/// `onboard` is too late. Which tenant may have a WABA is your policy:
/// wa-rs decides none.
pub async fn onboard_for_tenant(
    es: &EmbeddedSignup,
    vault: &TokenVault,
    reservations: &Arc<dyn KvStore>, // your WABA → tenant table
    request: &OnboardingRequest,
    tenant: &str,
) -> wa_rs::Result<Onboarded> {
    es.onboard_with_approval(request, vault, |verified| async move {
        reserve(reservations, &verified.waba_id, tenant).await
    })
    .await
}

/// Bind the WABA to the tenant in one atomic write. A lookup followed by a
/// write would let two tenants onboarding the same WABA at once both pass.
pub async fn reserve(
    reservations: &Arc<dyn KvStore>,
    waba_id: &WabaId,
    tenant: &str,
) -> wa_rs::Result<()> {
    let key = StoreKey::new("tenant.waba", waba_id.as_str());
    let mine = tenant.as_bytes().to_vec();
    if reservations
        .put_if_absent(&key, mine.clone(), Expiry::Never)
        .await?
        .is_some()
    {
        return Ok(()); // reserved for this tenant just now
    }
    match reservations.get(&key).await? {
        Some(bound) if bound.value == mine => Ok(()), // this tenant again: a retry, a reconnect
        _ => Err(ValidationError::new("waba_id", "connected to another merchant").into()),
    }
}

/// Funding a merchant again after a revocation is a product decision:
/// without this, `onboard_with_approval` and `resume` refuse
/// (`EmbeddedSignup::is_credit_line_revoked(&err)`).
pub fn fund_again(request: OnboardingRequest) -> OnboardingRequest {
    request.reshare_after_revocation()
}

/// What an `account_update` asked of a Solution Partner.
#[derive(Debug)]
pub enum PartnerAction {
    /// Nothing for your credit line.
    Ignored,
    /// The merchant unshared the WABA (`PARTNER_REMOVED`): revoked.
    Revoked(CreditRevocation),
    /// Your app was uninstalled from the WABA: revoked, then the token
    /// deleted.
    Offboarded(Offboarded),
    /// A coexistence number disconnected (`PARTNER_REMOVED` with
    /// `disconnection_info`). **Your policy decides** whether its line is
    /// revoked at once or after a grace period for a reconnect: see
    /// `on_coexistence_disconnect`.
    CoexistenceDisconnected {
        waba_id: WabaId,
        owner: Option<BusinessId>,
    },
}

/// `account_update`, from a signature-checked delivery only: the
/// `owner_business_id` it carries is what revocation falls back on when the
/// vault no longer knows the merchant.
pub async fn on_account_update(
    es: &EmbeddedSignup,
    vault: &TokenVault,
    event: &WebhookEvent,
) -> wa_rs::Result<PartnerAction> {
    let WebhookEvent::AccountUpdated { update, .. } = event else {
        return Ok(PartnerAction::Ignored);
    };
    let info = update.waba_info.as_ref();
    let owner = info.and_then(|i| i.owner_business_id.as_ref());
    // The merchant's WABA: `waba_info.waba_id` for the PARTNER_* events.
    let waba_id = event.waba_id();
    match (&update.event, waba_id) {
        // Only YOUR app: under a Multi-Partner Solution another partner
        // uninstalling its own app is not a reason to revoke your line.
        (AccountUpdateEvent::PartnerAppUninstalled, Some(waba_id)) => {
            let ours = info.and_then(|i| i.partner_app_id.as_ref()) == Some(&es.app().app_id);
            if !ours {
                return Ok(PartnerAction::Ignored);
            }
            Ok(PartnerAction::Offboarded(
                es.offboard(waba_id, owner, vault).await?,
            ))
        }
        // A coexistence number disconnected: hand it to your policy.
        (AccountUpdateEvent::PartnerRemoved, Some(waba_id))
            if update.disconnection_info.is_some() =>
        {
            Ok(PartnerAction::CoexistenceDisconnected {
                waba_id: waba_id.clone(),
                owner: owner.cloned(),
            })
        }
        // Unshared: messaging on the WABA is blocked and Meta recommends
        // revoking at once. Revocation is per business: its other WABAs
        // lose the line too, and funding it again needs an explicit opt-in.
        (AccountUpdateEvent::PartnerRemoved, Some(waba_id)) => Ok(PartnerAction::Revoked(
            es.revoke_credit_line(waba_id, owner, vault).await?,
        )),
        // No WABA named, only its owner: revoke by business.
        (AccountUpdateEvent::PartnerRemoved, None) => match owner {
            Some(owner) => Ok(PartnerAction::Revoked(
                es.revoke_business_credit_line(owner, vault).await?,
            )),
            None => Ok(PartnerAction::Ignored),
        },
        _ => Ok(PartnerAction::Ignored),
    }
}

/// Your policy for a disconnected coexistence number. wa-rs does not pick
/// one, and neither does this example: the choice is yours (and, for the
/// wa-rs server, still open).
#[derive(Debug, Clone, Copy)]
pub enum CoexistencePolicy {
    /// Revoke at once, as for any `PARTNER_REMOVED`; a reconnect then needs
    /// `fund_again`.
    RevokeNow,
    /// Keep funding for a grace period: schedule your own check, and revoke
    /// (as below) if the number has not reconnected by then.
    GracePeriod,
}

/// The policy point: `PartnerAction::CoexistenceDisconnected` lands here.
pub async fn on_coexistence_disconnect(
    es: &EmbeddedSignup,
    vault: &TokenVault,
    policy: CoexistencePolicy,
    waba_id: &WabaId,
    owner: Option<&BusinessId>,
) -> wa_rs::Result<Option<CreditRevocation>> {
    match policy {
        CoexistencePolicy::RevokeNow => {
            Ok(Some(es.revoke_credit_line(waba_id, owner, vault).await?))
        }
        CoexistencePolicy::GracePeriod => Ok(None), // your scheduler calls revoke_credit_line later
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
    use serde_json::json;
    use wa_rs::adapters::store::MemoryKvStore;
    use wa_rs::client::embedded_signup::{
        EmbeddedSignupEvent, SignupCode, StoredBusinessToken, VaultKey, VaultKeys, steps,
    };
    use wa_rs::core::testing::ScriptedTransport;
    use wa_rs::webhooks::WebhookPayload;

    use super::*;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="; // test only
    const APP_ID: &str = "1234";
    const WABA: &str = "980198427658004";
    const OWNER: &str = "2329417887457253";

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
        client.embedded_signup(AppCredentials::new(APP_ID, "app-secret"))
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
    async fn without_a_currency_or_an_approval_nothing_is_spent() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(None))).unwrap();
        let err = es.onboard(&request(), &vault()).await.unwrap_err();
        assert!(matches!(err, Error::Validation(ref v) if v.field == "currency"));
        // With a currency, plain `onboard` is refused: approve first.
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let err = es.onboard(&request(), &vault()).await.unwrap_err();
        assert!(err.credit().is_some(), "{err}");
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
        let reservations: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        reserve(&reservations, &"102290129340398".into(), "m7")
            .await
            .unwrap();
        reserve(&reservations, &"102290129340398".into(), "m7")
            .await
            .unwrap(); // the same tenant again
        transport.push_json(200, json!({"access_token": "EAAB"}));
        transport.push_json(
            200,
            json!({"data": {"app_id": APP_ID, "is_valid": true, "granular_scopes": [
                {"scope": "whatsapp_business_management", "target_ids": ["102290129340398"]}]}}),
        );
        transport.push_json(
            200,
            json!({"owner_business_info": {"id": "2729063490586005"}, "id": "102290129340398"}),
        );
        transport.push_json(200, json!({"data": []}));
        let err = onboard_for_tenant(&es, &vault, &reservations, &request(), "m42")
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

    fn partner_event(event: &str, info: &serde_json::Value) -> WebhookEvent {
        // Meta's examples (webhooks/reference/account_update): the entry id
        // is a business portfolio, the WABA is in waba_info.
        let body = json!({"object": "whatsapp_business_account", "entry": [{
            "id": "2949482758682047", "time": 1748477359,
            "changes": [{"field": "account_update", "value": {"event": event, "waba_info": info}}]
        }]});
        WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events()
            .remove(0)
    }

    fn removed() -> WebhookEvent {
        partner_event(
            "PARTNER_REMOVED",
            &json!({"waba_id": WABA, "owner_business_id": OWNER}),
        )
    }

    fn uninstalled(app: &str) -> WebhookEvent {
        partner_event(
            "PARTNER_APP_UNINSTALLED",
            &json!({"waba_id": WABA, "owner_business_id": OWNER, "partner_app_id": app}),
        )
    }

    /// Lookup, status, DELETE, status: one record revoked.
    fn script_revocation(transport: &ScriptedTransport) {
        let business = json!({"id": OWNER});
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

    async fn stored(vault: &TokenVault) {
        vault
            .store(&StoredBusinessToken::new(WABA, AccessToken::new("EAAB")).business_id(OWNER))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn partner_removed_revokes_from_the_stored_owner() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        stored(&vault).await;
        script_revocation(&transport);
        let PartnerAction::Revoked(revoked) =
            on_account_update(&es, &vault, &removed()).await.unwrap()
        else {
            panic!("not revoked")
        };
        assert_eq!(revoked.revoked.len(), 1);
        let requests = transport.requests();
        assert_eq!(
            requests[0].query("receiving_business_id").as_deref(),
            Some(OWNER)
        );
        assert_eq!(requests[2].method.as_str(), "DELETE");
        assert_eq!(requests[2].bearer(), Some("SYSTEM_TOKEN"));
        assert_eq!(transport.remaining(), 0);
        assert!(vault.get(&WABA.into()).await.unwrap().is_some());
        assert_eq!(steps::SHARE_CREDIT_LINE, "share_credit_line");
    }

    /// Our app removed first, the WABA unshared second: the line is revoked
    /// once, the token deleted, and the second event still finds the owner
    /// (from the ledger, or the webhook's `owner_business_id`).
    #[tokio::test]
    async fn uninstall_then_removal_ends_revoked_and_deleted() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        stored(&vault).await;
        script_revocation(&transport);
        let action = on_account_update(&es, &vault, &uninstalled(APP_ID))
            .await
            .unwrap();
        assert!(matches!(action, PartnerAction::Offboarded(_)), "{action:?}");
        assert!(vault.get(&WABA.into()).await.unwrap().is_none());
        let business = json!({"id": OWNER});
        transport.push_json(
            200,
            json!({"id": "58501441721238", "receiving_business": business}),
        );
        transport.push_json(
            200,
            json!({"receiving_business": business, "request_status": "DELETED"}),
        );
        let PartnerAction::Revoked(again) =
            on_account_update(&es, &vault, &removed()).await.unwrap()
        else {
            panic!("not revoked")
        };
        assert_eq!(again.already_revoked.len(), 1);
        assert_eq!(transport.remaining(), 0);
    }

    /// Another partner of a Multi-Partner Solution uninstalling its own app
    /// changes nothing of ours; a coexistence disconnection goes to the
    /// integrator's policy, and only `RevokeNow` revokes.
    #[tokio::test]
    async fn only_our_uninstall_offboards_and_coexistence_is_a_policy() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        stored(&vault).await;
        let action = on_account_update(&es, &vault, &uninstalled("9999"))
            .await
            .unwrap();
        assert!(matches!(action, PartnerAction::Ignored), "{action:?}");
        assert!(vault.get(&WABA.into()).await.unwrap().is_some());

        // `embedded-signup/onboarding-business-app-users`: the WABA is the
        // entry id, with disconnection details.
        let body = json!({"object": "whatsapp_business_account", "entry": [{
            "id": WABA, "time": 1748477359,
            "changes": [{"field": "account_update", "value": {
                "event": "PARTNER_REMOVED", "phone_number": "15550783881",
                "disconnection_info": {"reason": "ACCOUNT_DELETED", "initiated_by": "USER"}
            }}]
        }]});
        let event = WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events()
            .remove(0);
        let PartnerAction::CoexistenceDisconnected { waba_id, owner } =
            on_account_update(&es, &vault, &event).await.unwrap()
        else {
            panic!("not routed to the policy")
        };
        assert!(transport.requests().is_empty(), "nothing decided for you");
        let kept = on_coexistence_disconnect(
            &es,
            &vault,
            CoexistencePolicy::GracePeriod,
            &waba_id,
            owner.as_ref(),
        )
        .await
        .unwrap();
        assert!(kept.is_none());
        script_revocation(&transport);
        let revoked = on_coexistence_disconnect(
            &es,
            &vault,
            CoexistencePolicy::RevokeNow,
            &waba_id,
            owner.as_ref(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(revoked.revoked.len(), 1);
        assert_eq!(transport.remaining(), 0);
    }

    /// `PARTNER_REMOVED` naming the owner business and no WABA: revoked by
    /// business.
    #[tokio::test]
    async fn a_removal_without_a_waba_revokes_by_business() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        let business = json!({"id": OWNER});
        transport.push_json(
            200,
            json!({"id": "58501441721238", "receiving_business": business}),
        ); // the business has records: marked
        script_revocation(&transport);
        let event = partner_event("PARTNER_REMOVED", &json!({"owner_business_id": OWNER}));
        assert_eq!(event.waba_id(), None);
        let PartnerAction::Revoked(revoked) = on_account_update(&es, &vault, &event).await.unwrap()
        else {
            panic!("not revoked")
        };
        assert_eq!(revoked.revoked.len(), 1);
        assert!(
            vault
                .revoked_business(&OWNER.into())
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(transport.remaining(), 0);
    }
}
