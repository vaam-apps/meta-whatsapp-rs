//! The log half of `forged.rs`: a forged admin's unbind logs no unbinding,
//! and a genuine admin's does. Alone in its binary, as `logs.rs` and
//! `live_logs.rs` are: the capture is a thread's default subscriber, and
//! `tracing` caches whether a call site is enabled across threads, so a
//! test running beside it can leave the capture's call sites disabled (the
//! capture then sees nothing).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;
#[path = "forged/support.rs"]
mod support;

use support::*;

/// An admin unbind with a forged `AdminCaller` (`OwnedWaba::unbind_for_admin`
/// is public) unbinds nothing, deletes no token, and does not log the
/// unbinding it did not do; a genuine admin's unbind of a tokenless WABA
/// still logs it. Decisive: the log's place, after `forget_for_admin`
/// accepted the admin.
#[tokio::test]
async fn a_forged_admin_unbinds_nothing_and_logs_no_unbinding() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = service().await;
    let (_, admin) = forged().await;
    for waba in [WABA, TOKENLESS_WABA] {
        let binding = h.store.waba(&WabaId::new(waba)).await.unwrap().unwrap();
        let error = OwnedWaba::unbind_for_admin(&h.state, &admin, &binding)
            .await
            .unwrap_err();
        assert_eq!(
            (error.status(), error.code()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{waba}"
        );
        assert!(
            h.store.waba(&WabaId::new(waba)).await.unwrap().is_some(),
            "{waba}: unbound"
        );
    }
    let kept = h
        .vault
        .get(&WabaId::new(WABA))
        .await
        .unwrap()
        .expect("the victim's token");
    assert_eq!(kept.token.expose_secret(), VICTIM_TOKEN);
    let logs = captured.text();
    assert!(
        logs.contains("refused a capability another authorizer made"),
        "{logs}"
    );
    assert!(!logs.contains("unbound a WABA"), "{logs}");

    // The control: the operator's own admin unbinds the tokenless WABA,
    // and the log says so.
    let admin_key = h.admin_key().await;
    let reply = h
        .call(
            Call::new(
                Method::DELETE,
                format!("/v1/admin/wabas/{TOKENLESS_WABA}/binding"),
            )
            .key(&admin_key),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    assert!(
        h.store
            .waba(&WabaId::new(TOKENLESS_WABA))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        captured
            .text()
            .contains("unbound a WABA without a usable token"),
        "{}",
        captured.text()
    );
}
