// Storing a token takes the admin services (`Admin::attach_waba`, opened
// with `Admin::new`'s admission), not the `Authorizer` itself (roadmap
// S6).
use meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken;
use meta_whatsapp_server_core::authz::{AdminCaller, Authorizer};
use meta_whatsapp_server_core::model::TenantId;

async fn store(
    authz: &Authorizer,
    admin: &AdminCaller,
    tenant: &TenantId,
    token: &StoredBusinessToken,
) {
    let _ = authz.store_token(admin, tenant, token).await;
}

fn main() {}
