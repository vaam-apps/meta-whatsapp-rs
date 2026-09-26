//! Roadmap S2 (the core's security review, SR-L2, and the same race in
//! `failed`): a capability acts only on the WABA's binding and the vault
//! record it was made from. Run on memory (`tests/store.rs`) and on
//! Postgres (`tests/live_postgres.rs`), through the core's `Authorizer`
//! over the backend's records and a vault on its key/value store.

use std::sync::Arc;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::error::GraphApiError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::store::KvStore;
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_server::api::admin::mint;
use meta_whatsapp_server::auth::{Authorizer, Caller};
use meta_whatsapp_server::model::{KeyOwner, NumberStatus, Scope, TenantId};
use meta_whatsapp_server::store::{RecordStore, Store};

/// The tenant the capabilities are made for, and the one a WABA moves to.
const A: &str = "cap-a";
const B: &str = "cap-b";

struct Setup {
    store: Arc<dyn Store>,
    vault: TokenVault,
    authz: Authorizer,
    caller: Caller,
}

impl Setup {
    async fn new(store: Arc<dyn Store>, kv: Arc<dyn KvStore>) -> Self {
        for tenant in [A, B] {
            let _ = store
                .create_tenant(&TenantId::parse(tenant).unwrap(), "")
                .await
                .unwrap();
        }
        let vault =
            TokenVault::new(kv, VaultKeys::new(VaultKey::generate("cap").unwrap())).unwrap();
        let client = Client::builder()
            .transport(ScriptedTransport::new())
            .build()
            .unwrap();
        let records: Arc<dyn RecordStore> = store.clone();
        let authz = Authorizer::new(records, vault.clone(), client).unwrap();
        let (key, _) = mint(
            store.as_ref(),
            KeyOwner::Tenant(TenantId::parse(A).unwrap()),
            vec![Scope::Numbers],
            String::new(),
            None,
        )
        .await
        .unwrap();
        let caller = authz
            .tenant_caller(
                Some(&format!("Bearer {}", key.expose_key())),
                None,
                Scope::Numbers,
            )
            .await
            .unwrap();
        Self {
            store,
            vault,
            authz,
            caller,
        }
    }

    /// Bind `waba` and `pn` to `tenant`, as attaching does, and, with a
    /// `token`, store it (a new vault record).
    async fn attach(&self, tenant: &str, waba: &WabaId, pn: &PhoneNumberId, token: Option<&str>) {
        let bound = self
            .store
            .bind_waba(
                &TenantId::parse(tenant).unwrap(),
                waba,
                std::slice::from_ref(pn),
            )
            .await
            .unwrap();
        assert_eq!(bound, meta_whatsapp_server::model::BindOutcome::Bound);
        if let Some(token) = token {
            self.vault
                .store(
                    &StoredBusinessToken::new(waba.clone(), AccessToken::new(token))
                        .phone_number_ids([pn.clone()]),
                )
                .await
                .unwrap();
        }
    }

    /// The WABA unbound, then attached again to `tenant`, with a new token
    /// or none yet (an attach between its binding and its token).
    async fn attached_again(
        &self,
        tenant: &str,
        waba: &WabaId,
        pn: &PhoneNumberId,
        token: Option<&str>,
    ) {
        assert!(self.store.unbind_waba(waba).await.unwrap());
        self.attach(tenant, waba, pn, token).await;
    }

    async fn holder(&self, waba: &WabaId) -> Option<String> {
        self.store
            .waba(waba)
            .await
            .unwrap()
            .map(|binding| binding.tenant_id.as_str().to_owned())
    }

    async fn token(&self, waba: &WabaId) -> Option<String> {
        self.vault
            .get(waba)
            .await
            .unwrap()
            .map(|token| token.token.expose_secret().to_owned())
    }

    async fn status(&self, pn: &PhoneNumberId) -> Option<NumberStatus> {
        self.store
            .number(pn)
            .await
            .unwrap()
            .map(|number| number.status)
    }
}

/// Meta refusing a token: `190`.
fn refused_token() -> meta_whatsapp_rs::Error {
    let graph: GraphApiError = serde_json::from_value(serde_json::json!({
        "message": "Error validating access token: Session has expired",
        "type": "OAuthException",
        "code": 190,
        "fbtrace_id": "AbCdEf"
    }))
    .unwrap();
    meta_whatsapp_rs::Error::Api(Box::new(graph))
}

/// A WABA and a number of this case's own.
fn ids(case: &str) -> (WabaId, PhoneNumberId) {
    (
        WabaId::new(format!("cap-waba-{case}")),
        PhoneNumberId::new(format!("cap-pn-{case}")),
    )
}

/// SR-L2: an `OwnedWaba` made before its WABA was unbound and attached
/// again (to the same tenant, and to another) forgets neither the new
/// binding nor the new token; with the new token not stored yet (an
/// attach between its binding and its token), still not the new binding.
/// Refreshed for its own tenant with a new token (no unbind: the binding
/// keeps its epoch), it forgets neither either. Unmoved, it forgets both.
/// Decisive: the vault version in `forget`
/// (`TokenVault::delete_if_unchanged`, and `forget` stopping when it
/// deleted nothing) for the first and the third, the binding's epoch
/// (`RecordStore::unbind_waba_if`) for the second.
pub async fn forget_leaves_a_waba_attached_again(store: Arc<dyn Store>, kv: Arc<dyn KvStore>) {
    let s = Setup::new(store, kv).await;
    for target in [A, B] {
        for new_token in [Some("TOKEN-NEW"), None] {
            let case = format!("forget-{target}-{}", new_token.is_some());
            let (waba, pn) = ids(&case);
            s.attach(A, &waba, &pn, Some("TOKEN-OLD")).await;
            let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
            s.attached_again(target, &waba, &pn, new_token).await;
            assert!(
                !owned.forget(&s.authz).await.unwrap(),
                "{case}: forgot what moved"
            );
            assert_eq!(
                s.holder(&waba).await.as_deref(),
                Some(target),
                "{case}: the new binding went"
            );
            if let Some(token) = new_token {
                assert_eq!(
                    s.token(&waba).await.as_deref(),
                    Some(token),
                    "{case}: the new token went"
                );
            }
        }
    }
    // Attached again to its own tenant without an unbind (a new token, as
    // an operator's reconnect does): the binding keeps its epoch, the vault
    // record moved. Neither goes: the binding is not unbound on the old
    // token's word.
    let (waba, pn) = ids("forget-refreshed");
    s.attach(A, &waba, &pn, Some("TOKEN-OLD")).await;
    let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
    s.attach(A, &waba, &pn, Some("TOKEN-NEW")).await;
    assert!(
        !owned.forget(&s.authz).await.unwrap(),
        "forget-refreshed: forgot a binding refreshed with a new token"
    );
    assert_eq!(
        s.holder(&waba).await.as_deref(),
        Some(A),
        "forget-refreshed"
    );
    assert_eq!(
        s.token(&waba).await.as_deref(),
        Some("TOKEN-NEW"),
        "forget-refreshed"
    );
    // Unmoved: both go.
    let (waba, pn) = ids("forget-unmoved");
    s.attach(A, &waba, &pn, Some("TOKEN-OLD")).await;
    let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
    assert!(owned.forget(&s.authz).await.unwrap());
    assert_eq!(s.holder(&waba).await, None);
    assert_eq!(s.token(&waba).await, None);
}

/// The `failed` race: an `OwnedNumber` or an `OwnedWaba` made before its
/// WABA was unbound and attached again, to the same tenant and to
/// another, whose call Meta then answers `190`, leaves the new binding's
/// numbers `connected` in both cases; unmoved, it marks them
/// `reconnect_required`. Decisive: the binding's epoch in
/// `RecordStore::set_waba_status_if` (conditioning on the tenant alone
/// fails the same-tenant case, on nothing both).
pub async fn failed_leaves_a_waba_attached_again(store: Arc<dyn Store>, kv: Arc<dyn KvStore>) {
    let s = Setup::new(store, kv).await;
    for through_number in [true, false] {
        for target in [A, B] {
            let case = format!("failed-{through_number}-{target}");
            let (waba, pn) = ids(&case);
            s.attach(A, &waba, &pn, Some("TOKEN-OLD")).await;
            let failed = if through_number {
                let owned = s.authz.owned_number(&s.caller, pn.clone()).await.unwrap();
                s.attached_again(target, &waba, &pn, Some("TOKEN-NEW"))
                    .await;
                owned.failed(&s.authz, &refused_token()).await
            } else {
                let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
                s.attached_again(target, &waba, &pn, Some("TOKEN-NEW"))
                    .await;
                owned.failed(&s.authz, &refused_token()).await
            };
            assert_eq!(failed.code(), "reconnect_required", "{case}");
            assert_eq!(
                s.status(&pn).await,
                Some(NumberStatus::Connected),
                "{case}: the new binding's number was marked"
            );
        }
        // Unmoved: marked.
        let case = format!("failed-{through_number}-unmoved");
        let (waba, pn) = ids(&case);
        s.attach(A, &waba, &pn, Some("TOKEN-OLD")).await;
        let failed = if through_number {
            let owned = s.authz.owned_number(&s.caller, pn.clone()).await.unwrap();
            owned.failed(&s.authz, &refused_token()).await
        } else {
            let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
            owned.failed(&s.authz, &refused_token()).await
        };
        assert_eq!(failed.code(), "reconnect_required", "{case}");
        assert_eq!(
            s.status(&pn).await,
            Some(NumberStatus::ReconnectRequired),
            "{case}"
        );
    }
}

/// What a capability does, raced against its WABA being unbound and
/// attached again ([`racing_a_reattach`]).
#[derive(Debug, Clone, Copy)]
pub enum Act {
    /// `OwnedWaba::forget`.
    Forget,
    /// `OwnedNumber::failed` after Meta's `190`.
    NumberFailed,
    /// `OwnedWaba::failed` after Meta's `190`.
    WabaFailed,
}

/// A capability's conditioned write raced, on two tasks, against its WABA
/// being unbound and attached again (to the same tenant and to another,
/// round by round) with a new token: whichever lands first, the new
/// binding, its new token and its number's `connected` status survive.
/// `forget` first unbinds the old binding, which the attach then makes
/// again; the attach first leaves `forget` and `failed` nothing to act on.
/// Never the new binding's. Decisive (as far as the rounds meet): the
/// conditions of `delete_if_unchanged`, `unbind_waba_if` and
/// `set_waba_status_if`.
pub async fn racing_a_reattach(
    store: Arc<dyn Store>,
    kv: Arc<dyn KvStore>,
    act: Act,
    rounds: usize,
) {
    let s = Arc::new(Setup::new(store, kv).await);
    for round in 0..rounds {
        let target = if round % 2 == 0 { A } else { B };
        let case = format!("race-{act:?}-{round}-{}", super::unique());
        let (waba, pn) = ids(&case);
        s.attach(A, &waba, &pn, Some("TOKEN-OLD")).await;
        let acted = match act {
            Act::Forget => {
                let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
                let s = s.clone();
                tokio::spawn(async move {
                    super::race_suite::jitter(round, 0).await;
                    owned
                        .forget(&s.authz)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.code())
                })
            }
            Act::NumberFailed => {
                let owned = s.authz.owned_number(&s.caller, pn.clone()).await.unwrap();
                let s = s.clone();
                tokio::spawn(async move {
                    super::race_suite::jitter(round, 0).await;
                    let failed = owned.failed(&s.authz, &refused_token()).await;
                    assert_eq!(failed.code(), "reconnect_required");
                    Ok(())
                })
            }
            Act::WabaFailed => {
                let owned = s.authz.owned_waba(&s.caller, waba.clone()).await.unwrap();
                let s = s.clone();
                tokio::spawn(async move {
                    super::race_suite::jitter(round, 0).await;
                    let failed = owned.failed(&s.authz, &refused_token()).await;
                    assert_eq!(failed.code(), "reconnect_required");
                    Ok(())
                })
            }
        };
        let attached = {
            let (s, waba, pn) = (s.clone(), waba.clone(), pn.clone());
            tokio::spawn(async move {
                super::race_suite::jitter(round, 1).await;
                // `forget` may have unbound it first.
                let _ = s.store.unbind_waba(&waba).await.unwrap();
                s.attach(target, &waba, &pn, Some("TOKEN-NEW")).await;
            })
        };
        let (acted, attached) = tokio::join!(acted, attached);
        attached.unwrap();
        acted
            .unwrap()
            .unwrap_or_else(|code| panic!("{case}: the capability failed: {code}"));
        assert_eq!(
            s.holder(&waba).await.as_deref(),
            Some(target),
            "{case}: the new binding went"
        );
        assert_eq!(
            s.token(&waba).await.as_deref(),
            Some("TOKEN-NEW"),
            "{case}: the new token went"
        );
        assert_eq!(
            s.status(&pn).await,
            Some(NumberStatus::Connected),
            "{case}: the new binding's number was marked"
        );
    }
}

/// Everything above.
pub async fn run(store: Arc<dyn Store>, kv: Arc<dyn KvStore>) {
    forget_leaves_a_waba_attached_again(store.clone(), kv.clone()).await;
    failed_leaves_a_waba_attached_again(store, kv).await;
}
