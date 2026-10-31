// Rotating the vault takes the admin services (`Admin::rotate_vault`,
// opened with `Admin::new`'s admission), not the `Authorizer` itself
// (roadmap S6).
use meta_whatsapp_server_core::authz::{AdminCaller, Authorizer};

async fn rotate(authz: &Authorizer, admin: &AdminCaller) {
    let _ = authz.rotate_vault(admin).await;
}

fn main() {}
