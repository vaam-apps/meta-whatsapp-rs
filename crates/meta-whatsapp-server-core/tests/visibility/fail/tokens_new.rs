// No `Tokens` of one's own.
use meta_whatsapp_rs::client::embedded_signup::TokenVault;
use meta_whatsapp_server_core::authz::Tokens;

fn own(vault: TokenVault) -> Tokens {
    Tokens::new(vault)
}

fn main() {}
