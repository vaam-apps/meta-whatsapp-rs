//! Capabilities are branded with the `Authorizer` that made them.
//!
//! The security review of `6309a5a` proved a bypass: `Authorizer::new` is
//! public, so any crate could build a throwaway `Authorizer` over records
//! of its own (a key it wrote, the victim's tenant as that key's owner),
//! have it make an `AdminCaller` or a `Caller`, and hand those to the
//! service's `Authorizer`, which accepted them and read, wrote, rotated
//! and deleted its vault's records. This is that proof as a test, from
//! outside the crate as the forger is: every capability the forger's
//! `Authorizer` (A) makes is refused by every method of the service's (B)
//! with `403 forbidden`, and B's vault and records are neither read nor
//! written. Decisive: `Issuer::is` (the `Arc::ptr_eq` of `authz.rs`), and
//! each method's own check. `Authorizer::admit` and `admit_admin` ask the
//! same of a capability an adapter took from outside its own code; the
//! server's side of it (a forged capability in a request's extensions,
//! and its crate-private handlers) is `crates/meta-whatsapp-server/tests/forged.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

use support::*;

/// The control: each `Authorizer` accepts what it made, and the service
/// refuses the forger's key itself (it knows no such key).
#[tokio::test]
async fn each_authorizer_accepts_its_own_capabilities() {
    let (service, forger) = service_and_forger().await;
    for (side, token) in [(&service, VICTIM_TOKEN), (&forger, FORGED_TOKEN)] {
        let number = side.number().await;
        assert_eq!(number.client().token().unwrap().expose_secret(), token);
        let waba = side.waba().await;
        assert_eq!(waba.client().token().unwrap().expose_secret(), token);
        let admin = side.admin().await;
        let opened = side
            .authz
            .waba_for_admin(&admin, &side.records.waba)
            .await
            .unwrap();
        assert_eq!(opened.client().token().unwrap().expose_secret(), token);
        // Already under the active key: walked, nothing to re-encrypt.
        let rotation = side.authz.rotate_vault(&admin).await.unwrap();
        assert_eq!((rotation.wabas, rotation.rotated), (1, 0));
        assert!(rotation.failed.is_empty());
        side.records.take_calls();
        // Roadmap S2: a capability marks and forgets only the binding (and
        // the token) it was made from, through the conditioned writes.
        let marked = number.failed(&side.authz, &refused_token()).await;
        assert_eq!(marked.code(), "reconnect_required");
        assert_eq!(side.records.take_calls(), ["set_waba_status_if"]);
        // An owned WABA, from `owned_waba` or an admin's opening (both
        // made by `open`), is accepted too: it marks, then it forgets.
        for owned in [&waba, &opened] {
            let marked = owned.failed(&side.authz, &refused_token()).await;
            assert_eq!(marked.code(), "reconnect_required");
            assert_eq!(side.records.take_calls(), ["set_waba_status_if"]);
        }
        // The first forgets the token and the binding; the second, made
        // from the same token, finds it gone and deletes nothing: it only
        // reads the binding, to tell a binding that moved (which it warns
        // of: S2's security review, L1) from a token that did.
        assert!(waba.forget(&side.authz).await.unwrap());
        assert_eq!(side.records.take_calls(), ["unbind_waba_if"]);
        assert!(!opened.forget(&side.authz).await.unwrap());
        assert_eq!(side.records.take_calls(), ["waba"]);
        // Forgotten: the vault holds no token for the WABA any more.
        let gone = side
            .authz
            .waba_for_admin(&admin, &side.records.waba)
            .await
            .unwrap_err();
        assert_eq!(gone.code(), "number_not_connected");
    }
    assert_eq!(
        service
            .authz
            .admin_caller(Some(&forger.admin_bearer))
            .await
            .unwrap_err()
            .code(),
        "unauthenticated"
    );
}

#[tokio::test]
async fn owned_number_refuses_another_authorizers_caller() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.caller().await;
    let before = Before::of(&service);
    let error = service
        .authz
        .owned_number(&foreign, pn())
        .await
        .unwrap_err();
    assert_forbidden(&error, "owned_number");
    before.unchanged(&service, "owned_number").await;
}

#[tokio::test]
async fn owned_waba_refuses_another_authorizers_caller() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.caller().await;
    let before = Before::of(&service);
    let error = service
        .authz
        .owned_waba(&foreign, waba())
        .await
        .unwrap_err();
    assert_forbidden(&error, "owned_waba");
    before.unchanged(&service, "owned_waba").await;
}

#[tokio::test]
async fn waba_for_admin_refuses_another_authorizers_admin() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.admin().await;
    let before = Before::of(&service);
    let error = service
        .authz
        .waba_for_admin(&foreign, &service.records.waba)
        .await
        .unwrap_err();
    assert_forbidden(&error, "waba_for_admin");
    before.unchanged(&service, "waba_for_admin").await;
}

#[tokio::test]
async fn store_token_refuses_another_authorizers_admin() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.admin().await;
    let before = Before::of(&service);
    let error = service
        .authz
        .store_token(&foreign, &victim(), &stored(FORGED_TOKEN))
        .await
        .unwrap_err();
    assert_forbidden(&error, "store_token");
    before.unchanged(&service, "store_token").await;
}

#[tokio::test]
async fn rotate_vault_refuses_another_authorizers_admin() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.admin().await;
    let before = Before::of(&service);
    let error = service.authz.rotate_vault(&foreign).await.unwrap_err();
    assert_forbidden(&error, "rotate_vault");
    before.unchanged(&service, "rotate_vault").await;
}

#[tokio::test]
async fn forget_for_admin_refuses_another_authorizers_admin() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.admin().await;
    let before = Before::of(&service);
    let error = service
        .authz
        .forget_for_admin(&foreign, &waba())
        .await
        .unwrap_err();
    assert_forbidden(&error, "forget_for_admin");
    before.unchanged(&service, "forget_for_admin").await;
}

#[tokio::test]
async fn graph_failed_for_admin_refuses_another_authorizers_admin_and_marks_nothing() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.admin().await;
    let before = Before::of(&service);
    let error = service
        .authz
        .graph_failed_for_admin(&foreign, &waba(), &refused_token())
        .await;
    assert_forbidden(&error, "graph_failed_for_admin");
    before.unchanged(&service, "graph_failed_for_admin").await;
}

#[tokio::test]
async fn owned_number_failed_refuses_another_authorizer_and_marks_nothing() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.number().await;
    let before = Before::of(&service);
    let error = foreign.failed(&service.authz, &refused_token()).await;
    assert_forbidden(&error, "OwnedNumber::failed");
    before.unchanged(&service, "OwnedNumber::failed").await;
}

#[tokio::test]
async fn owned_waba_failed_refuses_another_authorizer_and_marks_nothing() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.waba().await;
    let before = Before::of(&service);
    let error = foreign.failed(&service.authz, &refused_token()).await;
    assert_forbidden(&error, "OwnedWaba::failed");
    before.unchanged(&service, "OwnedWaba::failed").await;
}

#[tokio::test]
async fn owned_waba_forget_refuses_another_authorizer_and_deletes_nothing() {
    let (service, forger) = service_and_forger().await;
    // An admin's opening too: the same type, from another method.
    let admin = forger.admin().await;
    let opened = forger
        .authz
        .waba_for_admin(&admin, &forger.records.waba)
        .await
        .unwrap();
    for foreign in [forger.waba().await, opened] {
        let before = Before::of(&service);
        let error = foreign.forget(&service.authz).await.unwrap_err();
        assert_forbidden(&error, "OwnedWaba::forget");
        before.unchanged(&service, "OwnedWaba::forget").await;
    }
    // The forger's own vault still works: it was never the target.
    assert_eq!(
        forger
            .number()
            .await
            .client()
            .token()
            .unwrap()
            .expose_secret(),
        FORGED_TOKEN
    );
}

/// `admit` and `admit_admin`, what an API adapter asks before it acts on
/// a capability it took from outside its own code (a request's
/// extensions): each `Authorizer` admits what it made and refuses what the
/// other made, reading and writing nothing. Decisive: the check in each.
#[tokio::test]
async fn admit_accepts_only_the_authorizers_own_capabilities() {
    let (service, forger) = service_and_forger().await;
    let (own, own_admin) = (service.caller().await, service.admin().await);
    let (foreign, foreign_admin) = (forger.caller().await, forger.admin().await);
    let before = Before::of(&service);
    assert_forbidden(&service.authz.admit(&foreign).unwrap_err(), "admit");
    assert_forbidden(
        &service.authz.admit_admin(&foreign_admin).unwrap_err(),
        "admit_admin",
    );
    before.unchanged(&service, "admit").await;
    service.authz.admit(&own).unwrap();
    service.authz.admit_admin(&own_admin).unwrap();
    forger.authz.admit(&foreign).unwrap();
    forger.authz.admit_admin(&foreign_admin).unwrap();
    assert_forbidden(&forger.authz.admit(&own).unwrap_err(), "admit, reversed");
    assert_forbidden(
        &forger.authz.admit_admin(&own_admin).unwrap_err(),
        "admit_admin, reversed",
    );
}

/// A clone of a capability is the same capability: still its maker's.
#[tokio::test]
async fn a_clone_keeps_its_makers_brand() {
    let (service, forger) = service_and_forger().await;
    let foreign = forger.caller().await.clone();
    let error = service
        .authz
        .owned_number(&foreign, pn())
        .await
        .unwrap_err();
    assert_forbidden(&error, "a cloned Caller");
    let own = service.caller().await.clone();
    assert!(service.authz.owned_number(&own, pn()).await.is_ok());
}

/// L4: a Graph client built with a token is refused, not stripped: its
/// token would ride along on any call made with `Authorizer::client`
/// that sets none.
#[test]
fn a_client_carrying_a_token_is_refused() {
    let vault = TokenVault::new(
        Arc::new(Kv::default()),
        VaultKeys::new(VaultKey::generate("k").unwrap()),
    )
    .unwrap();
    let client = Client::builder()
        .transport(NoNet)
        .access_token(AccessToken::new("EAAG-default-token"))
        .build()
        .unwrap();
    let error = Authorizer::new(Arc::new(Records::new(Vec::new())), vault, client).unwrap_err();
    assert!(matches!(error, Error::Config(_)), "{error:?}");
    assert!(
        !error.to_string().contains("EAAG-default-token"),
        "the refusal never quotes the token: {error}"
    );
}
