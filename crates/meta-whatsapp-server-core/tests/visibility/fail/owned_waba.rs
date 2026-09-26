// Only an `Authorizer` makes an `OwnedWaba`.
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_server_core::authz::OwnedWaba;

fn forge(waba_id: WabaId, client: Client) -> OwnedWaba {
    OwnedWaba { waba_id, client }
}

fn main() {}
