//! The server-layer bypass the security review of `3237040` proved (its
//! program held only an `AppState`), as tests. The core's brand held: a
//! capability another `Authorizer` made is refused by every core method.
//! The server re-opened it, in three steps:
//!
//! 1. `api::numbers::list_wabas`, a `pub` handler, took a `Caller` by
//!    value and listed `Caller::tenant()`'s WABAs without asking the
//!    `Authorizer`: a `Caller` from a throwaway `Authorizer` named any
//!    tenant.
//! 2. `api::admin::mint_platform_key` did the same with an `AdminCaller`,
//!    and answered a real platform key for every tenant.
//! 3. With that key, the service's own `tenant_guard` and `OwnedNumber`
//!    extractor, on a router of the forger's, answered the victim's vault
//!    token.
//!
//! Steps 1 and 2 no longer compile: the handlers, and
//! `idempotency::run`, which takes a tenant as given, are crate-private
//! (`tests/visibility/fail/api_*.rs` and `idempotency_run.rs` pin it, with
//! the compiler's `E0603`). Step 3 needs step 2's key. What remains is a
//! forged capability put straight into a request's extensions, in front of
//! the service's own extractors: refused, `403 forbidden`, and the vault
//! never read (this file). Decisive: the `admit` calls in the `Caller` and
//! `AdminCaller` extractors (`src/auth.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;
#[path = "forged/support.rs"]
mod support;

use support::*;

/// The review's step 3 without step 2's key, and its steps 1 and 2 as
/// far as they still compile: a forged `Caller` or `AdminCaller` in a
/// request's extensions reaches none of the service's extractors, and
/// the vault is never read. Decisive: the `admit` in each extractor
/// (`/caller` and `/admin` answer `200` without it; the owned number and
/// WABA are refused by the core too).
#[tokio::test]
async fn a_forged_capability_in_a_requests_extensions_is_403_at_every_extractor() {
    let h = service().await;
    let (caller, admin) = forged().await;
    let app = planted(
        tenant_routes()
            .merge(admin_routes())
            .with_state(h.state.clone()),
        caller,
        admin,
    );
    let reads = h.kv.vault_reads();
    for path in paths() {
        let reply = send(&app, Call::get(&path).build()).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{path}: {}",
            reply.text
        );
        assert!(!reply.text.contains(VICTIM_TOKEN), "{path}: {}", reply.text);
    }
    assert_eq!(h.kv.vault_reads(), reads, "the vault was read");
}

/// The control: the same extractors behind the service's own guards,
/// with the service's own keys, accept the capabilities the guards made.
#[tokio::test]
async fn the_services_own_capabilities_pass_the_same_extractors() {
    let h = service().await;
    let admin_key = h.admin_key().await;
    let tenant_key = h.tenant_key(VICTIM, &ALL_SCOPES).await;
    let app = tenant_routes()
        .route_layer(middleware::from_fn_with_state(
            guard(&h.state, Scope::Numbers),
            tenant_guard,
        ))
        .merge(
            admin_routes()
                .route_layer(middleware::from_fn_with_state(h.state.clone(), admin_guard)),
        )
        .with_state(h.state.clone());
    let admin_key_id = admin_key["wak_".len()..].split_once('_').unwrap().0;
    let [caller, admin, number, waba] = paths();
    for (path, key, answer) in [
        (caller, &tenant_key, VICTIM),
        (admin, &admin_key, admin_key_id),
        (number, &tenant_key, VICTIM_TOKEN),
        (waba, &tenant_key, VICTIM_TOKEN),
    ] {
        let reply = send(&app, Call::get(&path).key(key).build()).await;
        assert_eq!(
            (reply.status, reply.text.as_str()),
            (StatusCode::OK, answer),
            "{path}"
        );
    }
}

/// The service's own router ignores a planted capability: its guards
/// authenticate the request and put their own in its extensions, over
/// the forger's. The review's steps 1 and 2 over HTTP: no key is `401`;
/// another tenant's key lists that tenant's WABAs, not the victim's.
#[tokio::test]
async fn the_services_router_ignores_a_planted_capability() {
    let h = service().await;
    h.tenant("tenant-c").await;
    let other = h.tenant_key("tenant-c", &ALL_SCOPES).await;
    let (caller, admin) = forged().await;
    let app = planted(h.internal.clone(), caller, admin);
    let reads = h.kv.vault_reads();
    let mint_platform_key = Call::new(Method::POST, "/v1/admin/platform-keys")
        .json(&json!({"tenants": "*", "scopes": ["send", "numbers"]}));
    for (label, call) in [
        ("GET /v1/wabas", Call::get("/v1/wabas")),
        ("POST /v1/admin/platform-keys", mint_platform_key),
        (
            "GET /v1/numbers/{pn}",
            Call::get(format!("/v1/numbers/{PN}")),
        ),
    ] {
        let reply = send(&app, call.build()).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNAUTHORIZED, "unauthenticated"),
            "{label}: {}",
            reply.text
        );
    }
    let listed = send(&app, Call::get("/v1/wabas").key(&other).build()).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(listed.json()["data"], json!([]), "{}", listed.text);
    let number = send(
        &app,
        Call::get(format!("/v1/numbers/{PN}")).key(&other).build(),
    )
    .await;
    assert_eq!(
        (number.status, number.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );
    assert_eq!(h.kv.vault_reads(), reads, "the vault was read");
}
