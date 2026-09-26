// Marking a WABA's numbers `reconnect_required` takes a capability.
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_server_core::authz::Authorizer;

async fn mark(authz: &Authorizer, waba_id: &WabaId, error: &Error) {
    let _ = authz.graph_failed(waba_id, error).await;
}

fn main() {}
