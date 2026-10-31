// Storing a token takes the admin services' attach (`Admin::attach_waba`), for an admitted `AdminCaller`.
use meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken;
use meta_whatsapp_server_core::authz::Tokens;

async fn store(tokens: &Tokens, token: &StoredBusinessToken) {
    let _ = tokens.store(token).await;
}

fn main() {}
