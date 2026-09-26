// Only `Authorizer::tenant_caller` makes a `Caller`.
use meta_whatsapp_server_core::authz::Caller;
use meta_whatsapp_server_core::model::TenantId;

fn forge(tenant: TenantId) -> Caller {
    Caller {
        key_id: "forged".to_owned(),
        tenant,
    }
}

fn main() {}
