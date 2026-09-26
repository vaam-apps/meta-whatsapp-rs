// Storing a token takes `Authorizer::store_token` and an `AdminCaller`.
use meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken;
use meta_whatsapp_server_core::authz::Tokens;

async fn store(tokens: &Tokens, token: &StoredBusinessToken) {
    let _ = tokens.store(token).await;
}

fn main() {}
