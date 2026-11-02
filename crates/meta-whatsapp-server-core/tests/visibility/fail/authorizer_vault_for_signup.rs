// The vault the Embedded Signup services hand to the library's onboarding
// (roadmap M3a) is theirs alone: an adapter holding it could store, read
// or delete any WABA's token without a capability.
use meta_whatsapp_server_core::authz::Authorizer;

fn vault(authz: &Authorizer) {
    let _ = authz.vault_for_signup();
}

fn main() {}
