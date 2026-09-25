//! Reference code for the Solution Partner part of the
//! `meta-whatsapp-rs-embedded-signup` skill: the deployment decides once whether it
//! onboards as a Tech Provider (merchants pay Meta) or as a Solution
//! Partner (your credit line pays), then onboarding behind your approval,
//! resume, offboarding and the `account_update` events that end a
//! merchant's funding follow from that choice.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use std::sync::Arc;
use std::time::Duration;

use meta_whatsapp_rs::client::credit_lines::{CreditRevocation, WabaCurrency};
use meta_whatsapp_rs::client::embedded_signup::{
    CreditSharing, EmbeddedSignup, Offboarded, Onboarded, OnboardingRequest, PendingShareClearance,
    SolutionPartner, TokenVault,
};
use meta_whatsapp_rs::core::error::ValidationError;
use meta_whatsapp_rs::core::ids::{BusinessId, CreditLineId, FundingId, WabaId};
use meta_whatsapp_rs::core::store::{Expiry, KvStore, StoreKey};
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::webhooks::fields::{AccountUpdateEvent, DisconnectionInitiator};

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
) -> meta_whatsapp_rs::Result<EmbeddedSignup> {
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
) -> meta_whatsapp_rs::Result<OnboardingRequest> {
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
/// meta-whatsapp-rs decides none.
pub async fn onboard_for_tenant(
    es: &EmbeddedSignup,
    vault: &TokenVault,
    reservations: &Arc<dyn KvStore>, // your WABA → tenant table
    request: &OnboardingRequest,
    tenant: &str,
) -> meta_whatsapp_rs::Result<Onboarded> {
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
) -> meta_whatsapp_rs::Result<()> {
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
/// (`EmbeddedSignup::is_credit_line_revoked(&err)`). A successful re-share
/// clears the business's revocation marker: its other WABAs are then no
/// longer refused either.
pub fn fund_again(request: OnboardingRequest) -> OnboardingRequest {
    request.reshare_after_revocation()
}

/// One WABA's reconnect grant, for the tenant it was bound to.
fn reconnect_key(waba_id: &WabaId, tenant: &str) -> StoreKey {
    StoreKey::new("tenant.reconnect", format!("{waba_id}/{tenant}"))
}

/// Allow ONE reconnect of `waba_id` by the tenant it is bound to, funded
/// again. `on_account_update` grants it for a disconnection the merchant
/// made (a new device, a new number); anything else (an unshared WABA, an
/// offboarding, inactivity or enforcement, unpaid invoices) is your staff's
/// decision: they call this by hand, or not at all. `why` is for your
/// records. The grant expires after 30 days (your policy).
pub async fn grant_reconnect(
    reservations: &Arc<dyn KvStore>,
    waba_id: &WabaId,
    why: &str,
) -> meta_whatsapp_rs::Result<bool> {
    let binding = StoreKey::new("tenant.waba", waba_id.as_str());
    let Some(tenant) = reservations.get(&binding).await? else {
        return Ok(false); // no tenant to grant it to
    };
    let tenant = String::from_utf8_lossy(&tenant.value).into_owned();
    let expiry = Expiry::After(Duration::from_hours(30 * 24));
    let key = reconnect_key(waba_id, &tenant);
    reservations
        .put(&key, why.as_bytes().to_vec(), expiry)
        .await?;
    Ok(true)
}

/// A merchant whose line was revoked connects again: Embedded Signup runs
/// anew, funded again only with a grant for this WABA and tenant, which
/// the approval consumes (once, atomically) before anything is stored.
pub async fn reconnect(
    es: &EmbeddedSignup,
    vault: &TokenVault,
    reservations: &Arc<dyn KvStore>,
    request: OnboardingRequest,
    tenant: &str,
) -> meta_whatsapp_rs::Result<Onboarded> {
    let request = fund_again(request);
    es.onboard_with_approval(&request, vault, |verified| async move {
        reserve(reservations, &verified.waba_id, tenant).await?;
        if !reservations
            .delete(&reconnect_key(&verified.waba_id, tenant))
            .await?
        {
            let why = "no reconnect grant: funding this business again is your staff's decision";
            return Err(ValidationError::new("waba_id", why).into());
        }
        Ok(())
    })
    .await
}

/// What an `account_update` asked of a Solution Partner.
#[derive(Debug)]
pub enum PartnerAction {
    /// Nothing for your credit line.
    Ignored,
    /// The merchant unshared the WABA (`PARTNER_REMOVED`): revoked.
    Revoked(CreditRevocation),
    /// A coexistence number disconnected (`PARTNER_REMOVED` with
    /// `disconnection_info`: a device change, a re-registration,
    /// inactivity, enforcement): revoked at once too. `reconnect_granted`
    /// when the merchant made it (`initiated_by: USER`): ask them to
    /// reconnect (`reconnect`). Otherwise your staff decides.
    Disconnected {
        /// What was revoked.
        revoked: CreditRevocation,
        /// Whether one funded reconnect was granted.
        reconnect_granted: bool,
    },
    /// Your app was uninstalled from the WABA: revoked, then the token
    /// deleted.
    Offboarded(Offboarded),
}

/// `account_update`, from a signature-checked delivery only: the
/// `owner_business_id` it carries is what revocation falls back on when the
/// vault no longer knows the merchant. `our_business` is your own business
/// portfolio (the one `credit_lines` lists your lines for); `reservations`
/// your WABA → tenant table, where reconnect grants go.
pub async fn on_account_update(
    es: &EmbeddedSignup,
    vault: &TokenVault,
    reservations: &Arc<dyn KvStore>,
    our_business: &BusinessId,
    event: &WebhookEvent,
) -> meta_whatsapp_rs::Result<PartnerAction> {
    let WebhookEvent::AccountUpdated { update, .. } = event else {
        return Ok(PartnerAction::Ignored);
    };
    let info = update.waba_info.as_ref();
    let owner = info.and_then(|i| i.owner_business_id.as_ref());
    // The merchant's WABA: `waba_info.waba_id` for the PARTNER_* events.
    let waba_id = event.waba_id();
    // Under a Multi-Partner Solution, `solution_partner_business_ids` names
    // its partners: a removal from a solution you are not in is not yours.
    // Meta sends the list only then, and does not say whose business the
    // entry id is, so nothing else identifies you.
    let partners = info.map(|i| i.solution_partner_business_ids.as_slice());
    if partners.is_some_and(|ids| !ids.is_empty() && !ids.contains(our_business)) {
        return Ok(PartnerAction::Ignored);
    }
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
        // Unshared, or a coexistence number disconnected (it may
        // reconnect): revoke at once either way, as Meta recommends.
        // Revocation is per business: its other WABAs lose the line too,
        // and funding it again needs an explicit opt-in (`reconnect`).
        (AccountUpdateEvent::PartnerRemoved, Some(waba_id)) => {
            let revoked = es.revoke_credit_line(waba_id, owner, vault).await?;
            let Some(info) = &update.disconnection_info else {
                return Ok(PartnerAction::Revoked(revoked)); // no reconnect grant
            };
            // Only a disconnection the merchant made earns a reconnect.
            let by_merchant = info.initiated_by == Some(DisconnectionInitiator::User);
            let why = "coexistence disconnection initiated by the merchant";
            let reconnect_granted =
                by_merchant && grant_reconnect(reservations, waba_id, why).await?;
            Ok(PartnerAction::Disconnected {
                revoked,
                reconnect_granted,
            })
        }
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

/// What your admin tool shows after `clear_lost_share`.
#[derive(Debug, PartialEq)]
pub enum Clearance {
    /// Cleared, and sealed in the WABA's credit ledger.
    Cleared,
    /// Meta shows a record of your line that may be live: nothing cleared.
    MayBeLive,
    /// Something no record of your line explains pays for the WABA: it may
    /// be the lost share itself. Show it; call again with it only once the
    /// admin has seen in Meta Business Suite that it is not your line.
    ConfirmFunding(FundingId),
}

/// Your admin tool, once someone checked the WABA's funding in Meta
/// Business Suite: a share whose answer was lost and that Meta never lists
/// keeps every revocation of the WABA incomplete (`share_pending`) and
/// `offboard` from deleting the token, until it is cleared. meta-whatsapp-rs checks
/// Meta again first and clears nothing while a record may be live.
pub async fn clear_lost_share(
    es: &EmbeddedSignup,
    vault: &TokenVault,
    waba_id: &WabaId,
    admin: &str, // your staff session's operator id, never the request's: sealed in the ledger
    confirmed: Option<&FundingId>, // a ConfirmFunding the admin confirmed, else None
) -> meta_whatsapp_rs::Result<Clearance> {
    let outcome = es.clear_pending_share(waba_id, admin, confirmed, vault);
    Ok(match outcome.await? {
        PendingShareClearance::Cleared(_) => Clearance::Cleared, // vault.credit(waba_id) → cleared_shares
        PendingShareClearance::NotCleared(found) => match found.unexplained_funding() {
            Some(funding) => Clearance::ConfirmFunding(funding.clone()),
            None => Clearance::MayBeLive,
        },
        _ => Clearance::MayBeLive, // nothing cleared
    })
}

/// The merchant disconnects in your CMS: stop the webhooks while the token
/// still works, then offboard (revoke first, delete second).
pub async fn disconnect(
    es: &EmbeddedSignup,
    client: &Client,
    vault: &TokenVault,
    waba_id: &WabaId,
) -> meta_whatsapp_rs::Result<Option<CreditRevocation>> {
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
) -> meta_whatsapp_rs::Result<Vec<CreditLineId>> {
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
    use meta_whatsapp_rs::adapters::store::MemoryKvStore;
    use meta_whatsapp_rs::client::embedded_signup::{
        EmbeddedSignupEvent, SignupCode, StoredBusinessToken, VaultKey, VaultKeys, steps,
    };
    use meta_whatsapp_rs::core::error::TransportError;
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use meta_whatsapp_rs::webhooks::WebhookPayload;

    use super::*;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="; // test only
    const APP_ID: &str = "1234";
    const WABA: &str = "980198427658004";
    const OWNER: &str = "2329417887457253";
    /// Your business portfolio (the entry id of Meta's PARTNER_* examples).
    const US: &str = "2949482758682047";

    fn ours() -> BusinessId {
        BusinessId::new(US)
    }

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

    /// Your WABA → tenant table (reservations and reconnect grants).
    fn table() -> Arc<dyn KvStore> {
        Arc::new(MemoryKvStore::new())
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

    /// A removal from a Multi-Partner Solution we are in is ours.
    #[tokio::test]
    async fn a_removal_from_our_solution_revokes() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        let reservations = table();
        stored(&vault).await;
        script_revocation(&transport);
        let event = partner_event(
            "PARTNER_REMOVED",
            &json!({"waba_id": WABA, "owner_business_id": OWNER,
                    "solution_id": "1715120619246906",
                    "solution_partner_business_ids": [US, "520744086200222"]}),
        );
        let action = on_account_update(&es, &vault, &reservations, &ours(), &event)
            .await
            .unwrap();
        assert!(matches!(action, PartnerAction::Revoked(_)), "{action:?}");
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn partner_removed_revokes_from_the_stored_owner() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        let reservations = table();
        stored(&vault).await;
        script_revocation(&transport);
        let PartnerAction::Revoked(revoked) =
            on_account_update(&es, &vault, &reservations, &ours(), &removed())
                .await
                .unwrap()
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
        let reservations = table();
        stored(&vault).await;
        script_revocation(&transport);
        let action = on_account_update(&es, &vault, &reservations, &ours(), &uninstalled(APP_ID))
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
            on_account_update(&es, &vault, &reservations, &ours(), &removed())
                .await
                .unwrap()
        else {
            panic!("not revoked")
        };
        assert_eq!(again.already_revoked.len(), 1);
        assert_eq!(transport.remaining(), 0);
    }

    /// Another partner of a Multi-Partner Solution uninstalling its own app,
    /// or removing a solution we are not in, changes nothing of ours.
    #[tokio::test]
    async fn only_our_uninstall_and_our_solutions_removal_count() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        let reservations = table();
        stored(&vault).await;
        let action = on_account_update(&es, &vault, &reservations, &ours(), &uninstalled("9999"))
            .await
            .unwrap();
        assert!(matches!(action, PartnerAction::Ignored), "{action:?}");
        assert!(vault.get(&WABA.into()).await.unwrap().is_some());
        // Without a partner_app_id, an uninstall is nobody's in particular:
        // not ours either.
        let anonymous = partner_event(
            "PARTNER_APP_UNINSTALLED",
            &json!({"waba_id": WABA, "owner_business_id": OWNER}),
        );
        let action = on_account_update(&es, &vault, &reservations, &ours(), &anonymous)
            .await
            .unwrap();
        assert!(matches!(action, PartnerAction::Ignored), "{action:?}");
        assert!(vault.get(&WABA.into()).await.unwrap().is_some());
        // A removal from a Multi-Partner Solution we are not in.
        let theirs = partner_event(
            "PARTNER_REMOVED",
            &json!({"waba_id": WABA, "owner_business_id": OWNER,
                    "solution_id": "1715120619246906",
                    "solution_partner_business_ids": ["520744086200222", "506914307656634"]}),
        );
        let action = on_account_update(&es, &vault, &reservations, &ours(), &theirs)
            .await
            .unwrap();
        assert!(matches!(action, PartnerAction::Ignored), "{action:?}");
        assert!(transport.requests().is_empty(), "nothing revoked");
    }

    /// Everything up to the approval of an onboarding of `WABA`, whose
    /// owner is `OWNER`: code, token check, owner, numbers.
    fn script_until_approval(transport: &ScriptedTransport) {
        transport.push_json(200, json!({"access_token": "EAAB"}));
        transport.push_json(
            200,
            json!({"data": {"app_id": APP_ID, "is_valid": true, "granular_scopes": [
                {"scope": "whatsapp_business_management", "target_ids": [WABA]}]}}),
        );
        transport.push_json(
            200,
            json!({"owner_business_info": {"id": OWNER}, "id": WABA}),
        );
        transport.push_json(200, json!({"data": [{"id": "106540352242922"}]}));
    }

    /// Everything up to the credit step: the approval's, then subscribe
    /// and the system user.
    fn script_until_share(transport: &ScriptedTransport) {
        script_until_approval(transport);
        transport.push_json(200, json!({"success": true})); // subscribed_apps
        transport.push_json(200, json!({"success": true})); // assigned_users
    }

    /// The credit step of an opted-in re-share: the revoked record, then
    /// the new share.
    fn script_reshare(transport: &ScriptedTransport) {
        let business = json!({"id": OWNER});
        transport.push_json(
            200,
            json!({"id": "58501441721238", "receiving_business": business}),
        );
        transport.push_json(
            200,
            json!({"receiving_business": business, "request_status": "DELETED"}),
        );
        transport.push_json(
            200,
            json!({"allocation_config_id": "58501441721239", "waba_id": WABA}),
        );
    }

    /// A coexistence merchant's new Embedded Signup.
    fn reconnect_request() -> OnboardingRequest {
        let event = EmbeddedSignupEvent::from_json(&format!(
            r#"{{"type":"WA_EMBEDDED_SIGNUP","event":"FINISH_WHATSAPP_BUSINESS_APP_ONBOARDING","data":{{"waba_id":"{WABA}"}}}}"#
        ))
        .unwrap();
        OnboardingRequest::from_event(SignupCode::new("code").unwrap(), &event).unwrap()
    }

    /// Decision D14 (2026-09-25): a coexistence `PARTNER_REMOVED` (with
    /// `disconnection_info`, a number that may reconnect) revokes at once,
    /// like any removal. The merchant made this one (`USER`), so one
    /// reconnect of this WABA by its tenant is granted: it alone funds the
    /// business again, once.
    #[tokio::test]
    async fn a_coexistence_disconnection_revokes_at_once_and_grants_one_reconnect() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        let reservations = table();
        stored(&vault).await;
        reserve(&reservations, &WABA.into(), "m7").await.unwrap(); // onboarded by m7
        script_revocation(&transport);
        // `embedded-signup/onboarding-business-app-users`: the WABA is the
        // entry id, with disconnection details.
        let body = json!({"object": "whatsapp_business_account", "entry": [{
            "id": WABA, "time": 1748477359,
            "changes": [{"field": "account_update", "value": {
                "event": "PARTNER_REMOVED", "phone_number": "15550783881",
                "disconnection_info": {"reason": "USER_RE_REGISTERED", "initiated_by": "USER"}
            }}]
        }]});
        let event = WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events()
            .remove(0);
        let action = on_account_update(&es, &vault, &reservations, &ours(), &event)
            .await
            .unwrap();
        let PartnerAction::Disconnected {
            revoked,
            reconnect_granted: true,
        } = action
        else {
            panic!("not revoked at once with a grant: {action:?}")
        };
        assert_eq!(revoked.revoked.len(), 1);
        let requests = transport.requests();
        assert_eq!(
            requests[0].query("receiving_business_id").as_deref(),
            Some(OWNER),
            "the owner stored at onboarding"
        );
        assert_eq!(requests[2].method.as_str(), "DELETE");
        assert_eq!(transport.remaining(), 0);

        // The merchant reconnects: a new Embedded Signup, which does not
        // fund the business again on its own.
        let request = reconnect_request();
        let sent = transport.requests().len();
        script_until_share(&transport);
        let err = onboard_for_tenant(&es, &vault, &reservations, &request, "m7")
            .await
            .unwrap_err();
        assert!(EmbeddedSignup::is_credit_line_revoked(&err), "{err}");
        assert!(
            transport.requests()[sent..]
                .iter()
                .all(|r| !r.path().contains("credit")),
            "no credit call"
        );
        assert_eq!(transport.remaining(), 0);

        // Another tenant cannot use the grant.
        script_until_approval(&transport);
        let err = reconnect(&es, &vault, &reservations, reconnect_request(), "m42")
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
        assert_eq!(transport.remaining(), 0);

        // `reconnect` consumes the grant: funded again, once.
        script_until_share(&transport);
        script_reshare(&transport);
        let done = reconnect(&es, &vault, &reservations, request, "m7")
            .await
            .unwrap();
        assert_eq!(done.allocation_config_id, Some("58501441721239".into()));
        assert!(
            vault
                .revoked_business(&OWNER.into())
                .await
                .unwrap()
                .is_none(),
            "the re-share cleared the business-wide marker"
        );
        assert!(
            reservations
                .get(&reconnect_key(&WABA.into(), "m7"))
                .await
                .unwrap()
                .is_none(),
            "the grant is spent"
        );
        assert_eq!(transport.remaining(), 0);
    }

    /// A grant is for the WABA and the tenant it was bound to: after the
    /// WABA moved to another tenant, neither the old tenant (no longer
    /// bound) nor the new one (no grant) reconnects it funded.
    #[tokio::test]
    async fn a_reconnect_grant_belongs_to_the_waba_and_its_tenant() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        let reservations = table();
        reserve(&reservations, &WABA.into(), "m7").await.unwrap();
        assert!(
            grant_reconnect(&reservations, &WABA.into(), "reviewed by op_7f3a")
                .await
                .unwrap()
        );
        // The WABA is bound to m42 now (your CMS moved it).
        reservations
            .delete(&StoreKey::new("tenant.waba", WABA))
            .await
            .unwrap();
        reserve(&reservations, &WABA.into(), "m42").await.unwrap();
        for tenant in ["m7", "m42"] {
            script_until_approval(&transport);
            let err = reconnect(&es, &vault, &reservations, reconnect_request(), tenant)
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
                "{tenant}: {err}"
            );
        }
        assert!(
            vault.get(&WABA.into()).await.unwrap().is_none(),
            "nothing stored"
        );
        assert_eq!(transport.remaining(), 0);
    }

    /// A merchant who unshared the WABA (no `disconnection_info`) gets no
    /// reconnect grant: `reconnect` refuses at the approval, before
    /// anything is stored, subscribed or shared.
    #[tokio::test]
    async fn an_unshared_waba_gets_no_funded_reconnect() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        let reservations = table();
        stored(&vault).await;
        reserve(&reservations, &WABA.into(), "m7").await.unwrap();
        script_revocation(&transport);
        let action = on_account_update(&es, &vault, &reservations, &ours(), &removed())
            .await
            .unwrap();
        assert!(matches!(action, PartnerAction::Revoked(_)), "{action:?}");

        let sent = transport.requests().len();
        script_until_approval(&transport);
        let err = reconnect(&es, &vault, &reservations, reconnect_request(), "m7")
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
            transport.requests()[sent..]
                .iter()
                .all(|r| r.method.as_str() == "GET" || r.path().contains("oauth")),
            "nothing subscribed or shared"
        );
        assert!(
            vault
                .revoked_business(&OWNER.into())
                .await
                .unwrap()
                .is_some(),
            "still revoked"
        );
        assert_eq!(transport.remaining(), 0);
    }

    /// A disconnection Meta made (`SYSTEM`: inactivity, enforcement) gets
    /// no grant either; funding the merchant again is your staff's
    /// explicit call (`grant_reconnect`).
    #[tokio::test]
    async fn a_system_disconnection_needs_your_staffs_grant() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        let reservations = table();
        stored(&vault).await;
        reserve(&reservations, &WABA.into(), "m7").await.unwrap();
        script_revocation(&transport);
        // Meta's `account_update` example of a removal with
        // `disconnection_info` (`webhooks/reference/account_update`).
        let body = json!({"object": "whatsapp_business_account", "entry": [{
            "id": US, "time": 1748477359,
            "changes": [{"field": "account_update", "value": {
                "event": "PARTNER_REMOVED",
                "waba_info": {"waba_id": WABA, "owner_business_id": OWNER},
                "disconnection_info": {"reason": "PRIMARY_INACTIVITY", "initiated_by": "SYSTEM"}
            }}]
        }]});
        let event = WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events()
            .remove(0);
        let action = on_account_update(&es, &vault, &reservations, &ours(), &event)
            .await
            .unwrap();
        assert!(
            matches!(
                action,
                PartnerAction::Disconnected {
                    reconnect_granted: false,
                    ..
                }
            ),
            "{action:?}"
        );
        script_until_approval(&transport);
        let err = reconnect(&es, &vault, &reservations, reconnect_request(), "m7")
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

        // Your staff reviewed it and decided to fund the merchant again.
        assert!(
            grant_reconnect(&reservations, &WABA.into(), "reviewed by op_7f3a")
                .await
                .unwrap()
        );
        script_until_share(&transport);
        script_reshare(&transport);
        reconnect(&es, &vault, &reservations, reconnect_request(), "m7")
            .await
            .unwrap();
        assert_eq!(transport.remaining(), 0);
    }

    /// A share whose answer was lost and that Meta never lists: the
    /// revocation stays incomplete until an admin clears it, and the
    /// clearance is sealed in the ledger.
    #[tokio::test]
    async fn an_admin_clears_a_lost_share_meta_never_lists() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        let reservations: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let event = EmbeddedSignupEvent::from_json(&format!(
            r#"{{"type":"WA_EMBEDDED_SIGNUP","event":"FINISH_WHATSAPP_BUSINESS_APP_ONBOARDING","data":{{"waba_id":"{WABA}"}}}}"#
        ))
        .unwrap();
        let request =
            OnboardingRequest::from_event(SignupCode::new("code").unwrap(), &event).unwrap();
        script_until_share(&transport);
        transport.push_json(200, json!({"data": []})); // nothing shared yet
        transport.push_error(|| TransportError::Timeout); // the share's answer is lost
        let err = onboard_for_tenant(&es, &vault, &reservations, &request, "m7")
            .await
            .unwrap_err();
        assert!(err.may_have_been_sent() && !err.is_retryable(), "{err}");
        // Meta never lists it: the revocation revokes nothing, and says so.
        transport.push_json(200, json!({"data": []}));
        let err = on_account_update(&es, &vault, &reservations, &ours(), &removed())
            .await
            .unwrap_err();
        assert!(err.is_retryable(), "{err}");

        let sent = transport.requests().len();
        // Something pays for the WABA that no record of the line explains:
        // it may be the lost share. The admin checks, then confirms it.
        transport.push_json(200, json!({"data": []})); // the line's records
        transport.push_json(
            200,
            json!({"primary_funding_id": "MERCHANTS_CARD", "id": WABA}),
        );
        let card = FundingId::new("MERCHANTS_CARD");
        assert_eq!(
            clear_lost_share(&es, &vault, &WABA.into(), "op_7f3a", None)
                .await
                .unwrap(),
            Clearance::ConfirmFunding(card.clone())
        );
        transport.push_json(200, json!({"data": []}));
        transport.push_json(
            200,
            json!({"primary_funding_id": "MERCHANTS_CARD", "id": WABA}),
        );
        assert_eq!(
            clear_lost_share(&es, &vault, &WABA.into(), "op_7f3a", Some(&card))
                .await
                .unwrap(),
            Clearance::Cleared
        );
        assert!(
            transport.requests()[sent..]
                .iter()
                .all(|r| r.method.as_str() == "GET"),
            "it checks, and posts nothing"
        );
        let cleared = vault.credit(&WABA.into()).await.unwrap().unwrap();
        assert_eq!(cleared.cleared_shares[0].cleared_by, "op_7f3a");
        // The revocation finishes now; nothing is left to clear.
        transport.push_json(200, json!({"data": []}));
        let PartnerAction::Revoked(done) =
            on_account_update(&es, &vault, &reservations, &ours(), &removed())
                .await
                .unwrap()
        else {
            panic!("not revoked")
        };
        assert_eq!(done.all().count(), 0);
        assert!(
            clear_lost_share(&es, &vault, &WABA.into(), "op_7f3a", None)
                .await
                .is_err()
        );
        assert_eq!(transport.remaining(), 0);
    }

    /// `PARTNER_REMOVED` naming the owner business and no WABA: revoked by
    /// business.
    #[tokio::test]
    async fn a_removal_without_a_waba_revokes_by_business() {
        let transport = ScriptedTransport::new();
        let es = onboarding_mode(signup(&transport), Some(settings(Some("USD")))).unwrap();
        let vault = vault();
        let reservations = table();
        let business = json!({"id": OWNER});
        transport.push_json(
            200,
            json!({"id": "58501441721238", "receiving_business": business}),
        ); // the business has records: marked
        script_revocation(&transport);
        let event = partner_event("PARTNER_REMOVED", &json!({"owner_business_id": OWNER}));
        assert_eq!(event.waba_id(), None);
        let PartnerAction::Revoked(revoked) =
            on_account_update(&es, &vault, &reservations, &ours(), &event)
                .await
                .unwrap()
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
