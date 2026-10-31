// Deleting a token unopened takes the admin services
// (`Admin::unbind_waba`, opened with `Admin::new`'s admission), not the
// `Authorizer` itself (roadmap S6).
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_server_core::authz::{AdminCaller, Authorizer};

async fn forget(authz: &Authorizer, admin: &AdminCaller, waba_id: &WabaId) {
    let _ = authz.forget_for_admin(admin, waba_id).await;
}

fn main() {}
