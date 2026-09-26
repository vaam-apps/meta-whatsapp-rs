// Only `Authorizer::owned_number` makes an `OwnedNumber`.
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_server_core::authz::OwnedNumber;

fn forge(phone_number_id: PhoneNumberId, waba_id: WabaId, client: Client) -> OwnedNumber {
    OwnedNumber {
        phone_number_id,
        waba_id,
        client,
    }
}

fn main() {}
