// Deleting a token takes `Authorizer::forget_for_admin` or `OwnedWaba::forget`.
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_server_core::authz::Tokens;

async fn delete(tokens: &Tokens, waba_id: &WabaId) {
    let _ = tokens.delete(waba_id).await;
}

fn main() {}
