// `Authorizer`'s vault is a private field.
use meta_whatsapp_server_core::authz::Authorizer;

fn vault(authz: &Authorizer) {
    let _ = &authz.tokens;
}

fn main() {}
