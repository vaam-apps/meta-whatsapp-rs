// Rotating the vault takes `Authorizer::rotate_vault` and an `AdminCaller`.
use meta_whatsapp_server_core::authz::Tokens;
use meta_whatsapp_server_core::store::RecordStore;

async fn rotate(tokens: &Tokens, records: &dyn RecordStore) {
    let _ = tokens.rotate_all(records).await;
}

fn main() {}
