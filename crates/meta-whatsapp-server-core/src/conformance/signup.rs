//! Roadmap M3a: what Embedded Signup needs of a backend, and the core's
//! signup services over its bundle with no HTTP layer: its records (the
//! kept attempts, the vault's records kept past a binding, the bindings
//! with their credit allocation), the library's signup sessions and vault
//! in its key/value store, and a Graph client on a [`ScriptedTransport`].
//! What it holds a backend to:
//!
//! - **A kept attempt is its tenant's, and goes with its binding**
//!   ([`RecordStore::save_signup_attempt`]): kept only while the WABA is
//!   bound to the attempt's tenant, read back whole by that tenant only
//!   (another tenant's read finds none), replaced (its start kept),
//!   listed per tenant in byte order, deleted; every unbind deletes it, so
//!   a WABA bound again has none.
//! - **The vault's records kept past a binding are kept for good**
//!   ([`RecordStore::record_vault_entry`]): idempotent, listed in byte
//!   order of their keys, page by page, whatever is unbound or deleted.
//! - **A credit allocation is recorded on the binding it was made for**
//!   ([`RecordStore::set_credit_allocation_if`]): not on one refreshed or
//!   moved since.
//! - **Decision D4 before any credit call** (acceptance test M3.4, through
//!   the library's post-verification gate): a second tenant onboarding a
//!   WABA another tenant holds is `409 waba_owned_by_another_tenant`,
//!   whether it claims the WABA (refused before the attempt is redeemed,
//!   with no request) or Meta's verification finds it (refused in the
//!   approval, after Meta's reads), and nothing is stored, subscribed or
//!   shared: the holder's binding and token unchanged.
//! - **A key rotation reaches the records partner mode keeps past a
//!   binding** (roadmap M3a): a WABA offboarded (its line revoked, its
//!   token and binding gone) keeps its credit ledger and its business's
//!   revocation marker, which the rotation re-encrypts, so the old key can
//!   go once it reports no failure.
//!
//! [`RecordStore::save_signup_attempt`]: crate::store::RecordStore::save_signup_attempt
//! [`RecordStore::record_vault_entry`]: crate::store::RecordStore::record_vault_entry
//! [`RecordStore::set_credit_allocation_if`]: crate::store::RecordStore::set_credit_allocation_if

use std::sync::Arc;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::RetryPolicy;
use meta_whatsapp_rs::client::AppCredentials;
use meta_whatsapp_rs::client::credit_lines::WabaCurrency;
use meta_whatsapp_rs::client::embedded_signup::{SolutionPartner, TokenVault, VaultKey, VaultKeys};
use meta_whatsapp_rs::core::clock::SystemClock;
use meta_whatsapp_rs::core::ids::{AppId, BusinessId, WabaId};
use meta_whatsapp_rs::core::secret::{AccessToken, SecretBytes};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use serde_json::{Value, json};

use crate::admin::mint;
use crate::authz::{Authorizer, Caller, rotate_vault};
use crate::backend::Backend;
use crate::error::ErrorCode;
use crate::model::{
    BindOutcome, DeleteTenantOutcome, KeyOwner, NewSignupAttempt, PageRequest, Scope, TenantId,
    VaultEntry,
};
use crate::signup::{Completed, Completion, SignupDeployment, Signups};

/// The Meta app (`embedded-signup/implementation`'s example values).
const APP_ID: &str = "236484624622562";
const APP_SECRET: &str = "614fc2afde15eee07a26b2fe3eaee9b9";
/// The merchant's number in the records case (the same page's session
/// logging example).
const PHONE: &str = "106540352242922";
/// The business token the code exchange answers.
const TOKEN: &str = "EAAAN6tcBzAUBOwtDtTfmZCJ9n3FHpSDcDTH86ekf89Xnn";
/// The partner's system user, token and credit line, and the allocation
/// a share answers (`solution-providers/share-and-revoke-credit-lines`).
const SYSTEM_TOKEN: &str = "EAAG-the-partners-system-token";
const SYSTEM_USER: &str = "1972555232742222";
const LINE: &str = "1972385232742146";
const ALLOCATION: &str = "58501441721238";

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    // Boxed: the scenarios' futures are large, and `conformance::run`
    // holds them all (clippy's `large_futures` in the backends' crates).
    Box::pin(the_records_keep_attempts_and_vault_entries(backend)).await;
    Box::pin(m3_4_d4_is_enforced_before_any_credit_call(backend)).await;
    Box::pin(a_rotation_reaches_the_records_kept_past_a_binding(backend)).await;
}

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

fn page(after: Option<&str>, limit: usize) -> PageRequest {
    PageRequest {
        after: after.map(str::to_owned),
        limit,
    }
}

fn attempt(tenant: &TenantId, waba: &WabaId, step: &str) -> NewSignupAttempt {
    NewSignupAttempt {
        tenant_id: tenant.clone(),
        waba_id: waba.clone(),
        failed_step: step.to_owned(),
        session: json!({"waba_id": waba.as_str(), "phone_number_id": PHONE}),
        finish_kind: Some("FINISH".to_owned()),
    }
}

/// The records' contract for attempts, vault entries and allocations.
pub async fn the_records_keep_attempts_and_vault_entries(backend: &dyn Backend) {
    let records = backend.records();
    let store = records.as_ref();
    let digits = super::unique_digits();
    let a = tenant(&format!("signup-a-{}", super::unique()));
    let b = tenant(&format!("signup-b-{}", super::unique()));
    for t in [&a, &b] {
        store.create_tenant(t, "").await.unwrap().unwrap();
    }
    let first = WabaId::new(format!("{digits}1")).unwrap();
    let second = WabaId::new(format!("{digits}2")).unwrap();
    let theirs = WabaId::new(format!("{digits}3")).unwrap();

    // Not kept without the tenant's binding: none, or another tenant's.
    assert!(
        !store
            .save_signup_attempt(&attempt(&a, &first, "register_phone"))
            .await
            .unwrap(),
        "an unbound WABA keeps no attempt"
    );
    assert_eq!(
        store.bind_waba(&b, &theirs, &[]).await.unwrap(),
        BindOutcome::Bound
    );
    assert!(
        !store
            .save_signup_attempt(&attempt(&a, &theirs, "register_phone"))
            .await
            .unwrap(),
        "another tenant's WABA keeps no attempt of this tenant's"
    );
    for waba in [&first, &second] {
        assert_eq!(
            store.bind_waba(&a, waba, &[]).await.unwrap(),
            BindOutcome::Bound
        );
    }
    assert!(
        store
            .save_signup_attempt(&attempt(&a, &second, "subscribe_app"))
            .await
            .unwrap()
    );
    assert!(
        store
            .save_signup_attempt(&attempt(&a, &first, "register_phone"))
            .await
            .unwrap()
    );
    let kept = store.signup_attempt(&a, &first).await.unwrap().unwrap();
    assert_eq!(
        (
            &kept.tenant_id,
            &kept.waba_id,
            kept.failed_step.as_str(),
            &kept.session,
            kept.finish_kind.as_deref()
        ),
        (
            &a,
            &first,
            "register_phone",
            &json!({"waba_id": first.as_str(), "phone_number_id": PHONE}),
            Some("FINISH")
        ),
        "read back whole"
    );
    assert_eq!(
        store.signup_attempt(&b, &first).await.unwrap(),
        None,
        "another tenant's read finds none"
    );
    // Replaced: its step moves, its start stays.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert!(
        store
            .save_signup_attempt(&attempt(&a, &first, "sync_contacts"))
            .await
            .unwrap()
    );
    let replaced = store.signup_attempt(&a, &first).await.unwrap().unwrap();
    assert_eq!(replaced.failed_step, "sync_contacts");
    assert_eq!(replaced.created_at, kept.created_at, "its start kept");
    assert!(replaced.updated_at > kept.updated_at, "updated_at moved");
    // Listed per tenant, in byte order, page by page.
    let one = store.signup_attempts(&a, &page(None, 1)).await.unwrap();
    assert_eq!(
        one.items
            .iter()
            .map(|x| x.waba_id.clone())
            .collect::<Vec<_>>(),
        std::slice::from_ref(&first)
    );
    let rest = store
        .signup_attempts(&a, &page(one.next_after.as_deref(), 10))
        .await
        .unwrap();
    assert_eq!(
        rest.items
            .iter()
            .map(|x| x.waba_id.clone())
            .collect::<Vec<_>>(),
        std::slice::from_ref(&second)
    );
    assert_eq!(rest.next_after, None);
    assert!(
        store
            .signup_attempts(&b, &page(None, 10))
            .await
            .unwrap()
            .items
            .is_empty(),
        "another tenant lists none of them"
    );
    // Deleted by its tenant only.
    assert!(!store.delete_signup_attempt(&b, &second).await.unwrap());
    assert!(store.delete_signup_attempt(&a, &second).await.unwrap());
    assert!(!store.delete_signup_attempt(&a, &second).await.unwrap());
    // Gone with the binding, by either unbind; a binding made again has none.
    let epoch = store.waba(&first).await.unwrap().unwrap().epoch();
    assert!(store.unbind_waba_if(&epoch).await.unwrap());
    assert_eq!(store.signup_attempt(&a, &first).await.unwrap(), None);
    assert_eq!(
        store.bind_waba(&a, &first, &[]).await.unwrap(),
        BindOutcome::Bound
    );
    assert_eq!(
        store.signup_attempt(&a, &first).await.unwrap(),
        None,
        "a binding made again inherits nothing"
    );
    assert!(
        store
            .save_signup_attempt(&attempt(&a, &first, "register_phone"))
            .await
            .unwrap()
    );
    assert!(store.unbind_waba(&first).await.unwrap());
    assert_eq!(store.signup_attempt(&a, &first).await.unwrap(), None);

    // A credit allocation on the binding it was made for only.
    let binding = store.waba(&second).await.unwrap().unwrap();
    assert_eq!(binding.credit_allocation_id, None);
    assert!(
        store
            .set_credit_allocation_if(&binding.epoch(), ALLOCATION)
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .waba(&second)
            .await
            .unwrap()
            .unwrap()
            .credit_allocation_id
            .as_deref(),
        Some(ALLOCATION)
    );
    // Refreshed since (another generation): not recorded.
    assert_eq!(
        store.bind_waba(&a, &second, &[]).await.unwrap(),
        BindOutcome::Bound
    );
    assert!(
        !store
            .set_credit_allocation_if(&binding.epoch(), "58501441721239")
            .await
            .unwrap(),
        "a refreshed binding is not the epoch's"
    );

    // Vault entries: kept for good, idempotent, in byte order of their keys.
    let business = BusinessId::new(format!("{digits}4")).unwrap();
    let entries = [
        VaultEntry::Waba(second.clone()),
        VaultEntry::Business(business.clone()),
        VaultEntry::Waba(first.clone()),
    ];
    for entry in entries.iter().chain(&entries) {
        store.record_vault_entry(entry).await.unwrap();
    }
    for waba in [&second, &theirs] {
        assert!(store.unbind_waba(waba).await.unwrap());
    }
    assert_eq!(
        store.delete_tenant(&a).await.unwrap(),
        DeleteTenantOutcome::Deleted
    );
    let mut listed = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let listing = store
            .vault_entries(&page(after.as_deref(), 1))
            .await
            .unwrap();
        listed.extend(listing.items);
        match listing.next_after {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    let ours: Vec<VaultEntry> = listed
        .into_iter()
        .filter(|entry| entry.id().starts_with(&digits))
        .collect();
    assert_eq!(
        ours,
        [
            VaultEntry::Business(business),
            VaultEntry::Waba(first),
            VaultEntry::Waba(second),
        ],
        "each once, in byte order of `<kind>/<id>`, past every unbind and deletion"
    );
}

/// The suite's deployment: a Solution Partner, sharing with the method
/// Meta documents first (`ShareAndAttach`), in USD.
fn partner() -> SignupDeployment {
    SignupDeployment::new(
        AppCredentials::new(AppId::new(APP_ID).unwrap(), APP_SECRET),
        "<CONFIGURATION_ID>",
    )
    .solution_partner(
        SolutionPartner::new(
            AccessToken::new(SYSTEM_TOKEN),
            SYSTEM_USER.to_owned(),
            LINE.to_owned(),
        )
        .default_currency(WabaCurrency::Usd),
    )
}

/// The services over `backend`, the vault under `keys`, Graph scripted.
struct Rig {
    authz: Authorizer,
    signups: Signups,
    graph: ScriptedTransport,
}

fn rig(backend: &dyn Backend, keys: VaultKeys) -> Rig {
    let graph = ScriptedTransport::new();
    let client = Client::builder()
        .transport(graph.clone())
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap();
    let vault = TokenVault::new(backend.kv(), keys).unwrap();
    let authz = Authorizer::new(backend.records(), vault, client).unwrap();
    let signups = Signups::new(backend.kv(), Arc::new(SystemClock), Some(partner()));
    Rig {
        authz,
        signups,
        graph,
    }
}

/// A caller of `tenant` with scope `signup` (and `numbers`, to disconnect).
async fn caller(backend: &dyn Backend, authz: &Authorizer, tenant: &TenantId) -> Caller {
    let key = mint(
        backend.records().as_ref(),
        KeyOwner::Tenant(tenant.clone()),
        vec![Scope::Signup, Scope::Numbers],
        String::new(),
        None,
    )
    .await
    .unwrap()
    .0;
    authz
        .tenant_caller(
            Some(&format!("Bearer {}", key.expose_key())),
            None,
            Scope::Signup,
        )
        .await
        .unwrap()
}

/// A merchant of one case: its WABA, number and business portfolio, ids
/// of their own (the cases share one backend, and its vault).
struct Merchant {
    waba: String,
    phone: String,
    business: String,
}

fn merchant() -> Merchant {
    let digits = super::unique_digits();
    Merchant {
        waba: format!("{digits}1"),
        phone: format!("{digits}2"),
        business: format!("{digits}3"),
    }
}

/// The page's completion: `data` the session info, `event` the finish.
fn finished(state: &str, event: &str, data: &Value) -> Completion {
    Completion {
        state: state.to_owned(),
        code: "AQBhlXsctMxJYbwbrpybxlo9tLPGy".to_owned(),
        event: json!({"type": "WA_EMBEDDED_SIGNUP", "event": event, "data": data}),
        pin: None,
    }
}

/// Meta's answers up to and including `verify_assets`: the code
/// exchange, `debug_token` (the WABA granted), the WABA's owner, its
/// numbers.
fn script_until_verified(graph: &ScriptedTransport, m: &Merchant) {
    graph.push_json(200, json!({"access_token": TOKEN, "token_type": "bearer"}));
    graph.push_json(
        200,
        json!({"data": {
          "app_id": APP_ID, "type": "SYSTEM_USER", "is_valid": true,
          "granular_scopes": [
            {"scope": "whatsapp_business_management", "target_ids": [m.waba]},
            {"scope": "whatsapp_business_messaging", "target_ids": [m.waba]}
          ]
        }}),
    );
    graph.push_json(
        200,
        json!({"owner_business_info": {"name": "Wind & Wool", "id": m.business}, "id": m.waba}),
    );
    graph.push_json(200, json!({"data": [{"id": m.phone}]}));
}

/// A Solution Partner's onboarding of `m`'s WABA without a PIN: verified,
/// subscribed, the system user added, nothing shared yet, then shared.
fn script_partner_onboarding(graph: &ScriptedTransport, m: &Merchant) {
    script_until_verified(graph, m);
    graph.push_json(200, json!({"success": true})); // subscribed_apps
    graph.push_json(200, json!({"success": true})); // assigned_users
    graph.push_json(200, json!({"data": []})); // owning_credit_allocation_configs
    graph.push_json(
        200,
        json!({"allocation_config_id": ALLOCATION, "waba_id": m.waba}),
    );
}

/// `tenant` onboards `m`'s WABA (a Solution Partner's flow): connected.
async fn onboard(backend: &dyn Backend, rig: &Rig, tenant: &TenantId, m: &Merchant) {
    let caller = caller(backend, &rig.authz, tenant).await;
    let signup = rig.signups.open(&rig.authz, &caller).unwrap();
    let launch = signup.start(false).await.unwrap();
    script_partner_onboarding(&rig.graph, m);
    let done = signup
        .complete(finished(
            launch.state.as_str(),
            "FINISH_ONLY_WABA",
            &json!({"waba_id": m.waba, "business_id": m.business}),
        ))
        .await
        .unwrap();
    let Completed::Connected(connected) = done else {
        panic!("not connected: {done:?}")
    };
    assert_eq!(connected.credit_allocation_id.as_deref(), Some(ALLOCATION));
    assert_eq!(rig.graph.remaining(), 0);
}

/// M3.4's D4 case, through the core's services.
pub async fn m3_4_d4_is_enforced_before_any_credit_call(backend: &dyn Backend) {
    let records = backend.records();
    let holder = tenant(&format!("signup-holder-{}", super::unique()));
    let second = tenant(&format!("signup-second-{}", super::unique()));
    for t in [&holder, &second] {
        records.create_tenant(t, "").await.unwrap().unwrap();
    }
    let rig = rig(
        backend,
        VaultKeys::new(VaultKey::generate("signup-d4").unwrap()),
    );
    let m = merchant();
    onboard(backend, &rig, &holder, &m).await;
    let waba = WabaId::new(m.waba.as_str()).unwrap();
    let bound = records.waba(&waba).await.unwrap().unwrap();
    assert_eq!(bound.tenant_id, holder);
    assert_eq!(bound.credit_allocation_id.as_deref(), Some(ALLOCATION));
    let (_, stored_version) = rig
        .authz
        .vault_for_signup()
        .get_versioned(&waba)
        .await
        .unwrap()
        .unwrap();

    let caller = caller(backend, &rig.authz, &second).await;
    let signup = rig.signups.open(&rig.authz, &caller).unwrap();
    // Claiming the WABA: refused before the attempt is redeemed, nothing
    // asked of Meta.
    let launch = signup.start(false).await.unwrap();
    let asked = rig.graph.requests().len();
    let refused = signup
        .complete(finished(
            launch.state.as_str(),
            "FINISH_ONLY_WABA",
            &json!({"waba_id": m.waba}),
        ))
        .await
        .unwrap_err();
    assert_eq!(
        refused.error_code(),
        ErrorCode::WABA_OWNED_BY_ANOTHER_TENANT
    );
    assert_eq!(rig.graph.requests().len(), asked, "nothing asked of Meta");
    // Claiming nothing: Meta's verification finds the WABA, and the
    // approval refuses it before anything is stored, subscribed or shared.
    script_until_verified(&rig.graph, &m);
    let refused = signup
        .complete(finished(
            launch.state.as_str(),
            "FINISH_ONLY_WABA",
            &json!({"business_id": m.business}),
        ))
        .await
        .unwrap_err();
    assert_eq!(
        refused.error_code(),
        ErrorCode::WABA_OWNED_BY_ANOTHER_TENANT
    );
    assert_eq!(rig.graph.remaining(), 0);
    let after: Vec<_> = rig.graph.requests()[asked..].to_vec();
    assert_eq!(after.len(), 4, "the exchange, debug_token, owner, numbers");
    assert!(
        after.iter().all(|r| r.method.as_str() == "GET"),
        "nothing subscribed, assigned or shared"
    );
    assert!(
        after.iter().all(|r| r.bearer() != Some(SYSTEM_TOKEN)),
        "no credit call"
    );
    let now = records.waba(&waba).await.unwrap().unwrap();
    assert_eq!(now.epoch(), bound.epoch(), "the binding unchanged");
    let (stored, version) = rig
        .authz
        .vault_for_signup()
        .get_versioned(&waba)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version, stored_version, "the holder's token not rewritten");
    assert_eq!(stored.token.expose_secret(), TOKEN);
    assert!(
        records
            .signup_attempts(&second, &page(None, 10))
            .await
            .unwrap()
            .items
            .is_empty(),
        "no attempt kept for the second tenant"
    );
}

/// A rotation reaches the ledger and the marker an offboarded WABA left.
pub async fn a_rotation_reaches_the_records_kept_past_a_binding(backend: &dyn Backend) {
    let records = backend.records();
    let merchant = tenant(&format!("signup-rotated-{}", super::unique()));
    records.create_tenant(&merchant, "").await.unwrap().unwrap();
    let old = || VaultKey::new("signup-k1", SecretBytes::new([7u8; 32])).unwrap();
    let new = || VaultKey::new("signup-k2", SecretBytes::new([9u8; 32])).unwrap();
    let rig = rig(backend, VaultKeys::new(old()));
    let m = self::merchant();
    onboard(backend, &rig, &merchant, &m).await;
    let waba = WabaId::new(m.waba.as_str()).unwrap();
    // The tenant disconnects: unsubscribed by the route; here the core's
    // offboarding: the line revoked (lookup, status, DELETE, status), then
    // the token, then the binding.
    let caller = caller(backend, &rig.authz, &merchant).await;
    let owned = rig.authz.owned_waba(&caller, waba.clone()).await.unwrap();
    let business = json!({"name": "Wind & Wool", "id": m.business});
    rig.graph.push_json(
        200,
        json!({"id": ALLOCATION, "receiving_business": business}),
    );
    rig.graph
        .push_json(200, json!({"receiving_business": business}));
    rig.graph.push_json(200, json!({"success": true}));
    rig.graph.push_json(
        200,
        json!({"receiving_business": business, "request_status": "DELETED"}),
    );
    assert!(rig.signups.offboard(&rig.authz, &owned, &()).await.unwrap());
    assert_eq!(rig.graph.remaining(), 0);
    assert_eq!(records.waba(&waba).await.unwrap(), None, "unbound");
    let vault = rig.authz.vault_for_signup();
    assert!(vault.get(&waba).await.unwrap().is_none(), "token deleted");
    assert!(
        vault.credit(&waba).await.unwrap().is_some(),
        "the ledger stays"
    );
    let business = BusinessId::new(m.business.as_str()).unwrap();
    assert!(vault.revoked_business(&business).await.unwrap().is_some());

    // The new key, the old one previous: the walk reaches both records,
    // which no binding names any more.
    let rotating = TokenVault::new(backend.kv(), VaultKeys::new(new()).with_previous(old()))
        .unwrap()
        .rotate_on_read(false);
    let report = rotate_vault(records.as_ref(), &rotating).await.unwrap();
    // The backend is shared with the other cases, whose records are under
    // keys of their own: this case's records only.
    let ours = [
        format!("waba/{}", m.waba),
        format!("business/{}", m.business),
    ];
    assert!(report.records >= 2, "{report:?}");
    assert!(
        !report.failed.contains(&m.waba) && !report.failed_records.iter().any(|f| ours.contains(f)),
        "{report:?}"
    );
    assert!(
        report.rotated >= 1,
        "the ledger, and its marker: {report:?}"
    );
    // The old key can go: both open under the new key alone.
    let new_only = TokenVault::new(backend.kv(), VaultKeys::new(new())).unwrap();
    assert!(new_only.credit(&waba).await.unwrap().is_some());
    assert!(
        new_only
            .revoked_business(&business)
            .await
            .unwrap()
            .is_some()
    );
    // A rotation that cannot open a record says so, and the old key stays.
    let third = VaultKey::new("signup-k3", SecretBytes::new([11u8; 32])).unwrap();
    let lost = TokenVault::new(backend.kv(), VaultKeys::new(third)).unwrap();
    let report = rotate_vault(records.as_ref(), &lost).await.unwrap();
    assert!(
        report.failed_records.contains(&format!("waba/{}", m.waba)),
        "{report:?}"
    );
}
