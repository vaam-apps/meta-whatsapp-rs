// The control for the handler cases: the modules they name resolve, and
// what stays public there (the key minting the CLI and the tests use,
// the request and answer types, and the HTTP crate's idempotency header)
// compiles.
use meta_whatsapp_server_api_axum::api::admin::{MintPlatformKey, mint};
use meta_whatsapp_server_api_axum::api::numbers::WabaList;
use meta_whatsapp_server_http::idempotency::{KeyHeader, Success};

#[allow(dead_code)]
fn public(_: Option<MintPlatformKey>, _: Option<WabaList>, _: Option<KeyHeader>, _: Option<Success>) {
    let _ = mint;
}

fn main() {}
