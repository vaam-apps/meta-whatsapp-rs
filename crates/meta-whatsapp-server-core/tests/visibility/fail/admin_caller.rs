// Only `Authorizer::admin_caller` makes an `AdminCaller`.
use meta_whatsapp_server_core::authz::AdminCaller;

fn forge() -> AdminCaller {
    AdminCaller {
        key_id: "forged".to_owned(),
    }
}

fn main() {}
