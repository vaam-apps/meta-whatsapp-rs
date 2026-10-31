// Marking a WABA's numbers `reconnect_required` for an admin takes the
// admin services (the attach of `Admin::attach_waba`), not the
// `Authorizer` itself (roadmap S6).
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_server_core::authz::{AdminCaller, Authorizer};

async fn mark(authz: &Authorizer, admin: &AdminCaller, waba_id: &WabaId, error: &Error) {
    let _ = authz.graph_failed_for_admin(admin, waba_id, error).await;
}

fn main() {}
