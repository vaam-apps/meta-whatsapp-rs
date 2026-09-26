// `Authorizer` has no accessor to its vault.
use meta_whatsapp_server_core::authz::Authorizer;

fn vault(authz: &Authorizer) {
    let _ = authz.tokens();
}

fn main() {}
