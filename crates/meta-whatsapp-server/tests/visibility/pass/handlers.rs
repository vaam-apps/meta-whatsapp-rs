// The control for the handler cases: the modules they name resolve, and
// what stays public there (the key minting the CLI and the tests use,
// the request and answer types, the idempotency header) compiles.
use meta_whatsapp_server::api::admin::{MintPlatformKey, mint};
use meta_whatsapp_server::api::numbers::WabaList;
use meta_whatsapp_server::idempotency::{KeyHeader, Success};

#[allow(dead_code)]
fn public(_: Option<MintPlatformKey>, _: Option<WabaList>, _: Option<KeyHeader>, _: Option<Success>) {
    let _ = mint;
}

fn main() {}
