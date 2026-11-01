// The state is composed from one backend alone (roadmap S4): its ports
// share a database's guarantees, so loose ports (here a memory store,
// which is records and idempotency records but not a bundle) do not make
// a state. `pass/app_state.rs` is the same call on a backend.
use std::sync::Arc;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::VaultKeys;
use meta_whatsapp_server_http::events::Inbound;
use meta_whatsapp_server_http::metrics::Metrics;
use meta_whatsapp_server_http::state::{AppState, Settings};
use meta_whatsapp_server_store_memory::MemoryStore;

fn loose(keys: VaultKeys, client: Client, inbound: Inbound) {
    let _ = AppState::from_backend(
        Arc::new(MemoryStore::new()),
        keys,
        client,
        inbound,
        Metrics::new(),
        Settings::default(),
    );
}

fn main() {}
