// `Authorizer`'s records, which the admin services write through, are
// crate-private: an adapter reaches them only through the services
// (`admin::Admin`), which admit the admin first (roadmap S5a).
use meta_whatsapp_server_core::authz::Authorizer;

fn records(authz: &Authorizer) {
    let _ = authz.records();
}

fn main() {}
