//! Shared by `forged.rs` and `forged_logs.rs`: the service, the forger, the
//! routes and the paths they both use.
#![allow(dead_code, unused_imports)] // each binary uses a different subset

pub use std::sync::Arc;

pub use crate::common::capture::{Captured, subscriber};
pub use crate::common::{ALL_SCOPES, Call, Harness, send};
pub use meta_whatsapp_rs::Client;
pub use meta_whatsapp_rs::adapters::store::MemoryKvStore;
pub use meta_whatsapp_rs::client::embedded_signup::{TokenVault, VaultKey, VaultKeys};
pub use meta_whatsapp_rs::core::ids::WabaId;
pub use meta_whatsapp_rs::core::testing::ScriptedTransport;
pub use meta_whatsapp_rs::webhooks::axum::Router;
pub use meta_whatsapp_rs::webhooks::axum::extract::Request;
pub use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
pub use meta_whatsapp_rs::webhooks::axum::middleware::{self, Next};
pub use meta_whatsapp_rs::webhooks::axum::routing::get;
pub use meta_whatsapp_server::api::admin::mint;
pub use meta_whatsapp_server::auth::{
    AdminCaller, Authorizer, Caller, OwnedNumber, OwnedWaba, admin_guard, guard, tenant_guard,
};
pub use meta_whatsapp_server::model::{KeyOwner, Scope, TenantId};
pub use meta_whatsapp_server::state::AppState;
pub use meta_whatsapp_server::store::{MemoryStore, RecordStore};
pub use serde_json::json;

pub const VICTIM: &str = "victim-b";
pub const WABA: &str = "102290129340398";
pub const PN: &str = "106540352242922";
pub const VICTIM_TOKEN: &str = "EAAG-token-of-the-victim";
/// A second WABA of the victim's, bound without a token in the vault.
pub const TOKENLESS_WABA: &str = "102290129340399";

/// The service: the victim's WABA and number bound, the token in the
/// vault; a second WABA bound without one.
pub async fn service() -> Harness {
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
pub async fn forged() -> (Caller, AdminCaller) {
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
pub fn planted(router: Router, caller: Caller, admin: AdminCaller) -> Router {
    router.layer(middleware::from_fn(
        move |mut request: Request, next: Next| {
            request.extensions_mut().insert(caller.clone());
            request.extensions_mut().insert(admin.clone());
            next.run(request)
        },
    ))
}

/// The token a client carries, or nothing.
pub fn token(client: &Client) -> String {
    client
        .token()
        .map(|token| token.expose_secret().to_owned())
        .unwrap_or_default()
}

/// Tenant routes that extract, with the service's own extractors, each
/// tenant capability its handlers take, and answer what it gives: the
/// tenant a `Caller` acts as (what `list_wabas` listed), and the token an
/// owned number or WABA carries.
pub fn tenant_routes() -> Router<AppState> {
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
pub fn admin_routes() -> Router<AppState> {
    Router::new().route(
        "/admin",
        get(|admin: AdminCaller| async move { admin.key_id().to_owned() }),
    )
}

pub fn paths() -> [String; 4] {
    [
        "/caller".to_owned(),
        "/admin".to_owned(),
        format!("/numbers/{PN}"),
        format!("/wabas/{WABA}"),
    ]
}
