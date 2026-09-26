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

use std::sync::Arc;

use common::capture::{Captured, subscriber};
use common::{ALL_SCOPES, Call, Harness, send};
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::adapters::store::MemoryKvStore;
use meta_whatsapp_rs::client::embedded_signup::{TokenVault, VaultKey, VaultKeys};
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_rs::webhooks::axum::Router;
use meta_whatsapp_rs::webhooks::axum::extract::Request;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_rs::webhooks::axum::middleware::{self, Next};
use meta_whatsapp_rs::webhooks::axum::routing::get;
use meta_whatsapp_server::api::admin::mint;
use meta_whatsapp_server::auth::{
    AdminCaller, Authorizer, Caller, OwnedNumber, OwnedWaba, admin_guard, guard, tenant_guard,
};
use meta_whatsapp_server::model::{KeyOwner, Scope, TenantId};
use meta_whatsapp_server::state::AppState;
use meta_whatsapp_server::store::{MemoryStore, RecordStore};
use serde_json::json;

const VICTIM: &str = "victim-b";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const VICTIM_TOKEN: &str = "EAAG-token-of-the-victim";
/// A second WABA of the victim's, bound without a token in the vault.
const TOKENLESS_WABA: &str = "102290129340399";

/// The service: the victim's WABA and number bound, the token in the
/// vault; a second WABA bound without one.
async fn service() -> Harness {
    let h = Harness::new();
    h.tenant(VICTIM).await;
    h.connect(VICTIM, WABA, &[PN], VICTIM_TOKEN).await;
    h.store
        .bind_waba(
            &TenantId::parse(VICTIM).unwrap(),
            &WabaId::new(TOKENLESS_WABA),
            &[],
        )
        .await
        .unwrap();
    h
}

/// What the review's program made: a `Caller` of the victim's and an
/// `AdminCaller`, from a throwaway `Authorizer` over records of the
/// forger's own (the victim's tenant, and two keys it wrote), its own
/// vault and a tokenless client. No key of the service is known to it.
async fn forged() -> (Caller, AdminCaller) {
    let records = MemoryStore::new();
    let victim = TenantId::parse(VICTIM).unwrap();
    records
        .create_tenant(&victim, "victim")
        .await
        .unwrap()
        .unwrap();
    let (admin_key, _) = mint(&records, KeyOwner::Admin, Vec::new(), String::new(), None)
        .await
        .unwrap();
    let (tenant_key, _) = mint(
        &records,
        KeyOwner::Tenant(victim),
        ALL_SCOPES.to_vec(),
        String::new(),
        None,
    )
    .await
    .unwrap();
    let vault = TokenVault::new(
        Arc::new(MemoryKvStore::new()),
        VaultKeys::new(VaultKey::generate("forger").unwrap()),
    )
    .unwrap();
    let client = Client::builder()
        .transport(ScriptedTransport::new())
        .build()
        .unwrap();
    let authz = Authorizer::new(Arc::new(records), vault, client).unwrap();
    let caller = authz
        .tenant_caller(
            Some(&format!("Bearer {}", tenant_key.expose_key())),
            None,
            Scope::Numbers,
        )
        .await
        .unwrap();
    let admin = authz
        .admin_caller(Some(&format!("Bearer {}", admin_key.expose_key())))
        .await
        .unwrap();
    (caller, admin)
}

/// `router` with `caller` and `admin` put into every request's extensions
/// before any route sees it: what a forger's layer does.
fn planted(router: Router, caller: Caller, admin: AdminCaller) -> Router {
    router.layer(middleware::from_fn(
        move |mut request: Request, next: Next| {
            request.extensions_mut().insert(caller.clone());
            request.extensions_mut().insert(admin.clone());
            next.run(request)
        },
    ))
}

/// The token a client carries, or nothing.
fn token(client: &Client) -> String {
    client
        .token()
        .map(|token| token.expose_secret().to_owned())
        .unwrap_or_default()
}

/// Tenant routes that extract, with the service's own extractors, each
/// tenant capability its handlers take, and answer what it gives: the
/// tenant a `Caller` acts as (what `list_wabas` listed), and the token an
/// owned number or WABA carries.
fn tenant_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/caller",
            get(|caller: Caller| async move { caller.tenant().as_str().to_owned() }),
        )
        .route(
            "/numbers/{pn}",
            get(|owned: OwnedNumber| async move { token(owned.client()) }),
        )
        .route(
            "/wabas/{waba_id}",
            get(|owned: OwnedWaba| async move { token(owned.client()) }),
        )
}

/// An admin route that extracts an `AdminCaller` with the service's own
/// extractor (what `mint_platform_key` took), and answers its key id.
fn admin_routes() -> Router<AppState> {
    Router::new().route(
        "/admin",
        get(|admin: AdminCaller| async move { admin.key_id().to_owned() }),
    )
}

fn paths() -> [String; 4] {
    [
        "/caller".to_owned(),
        "/admin".to_owned(),
        format!("/numbers/{PN}"),
        format!("/wabas/{WABA}"),
    ]
}

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
