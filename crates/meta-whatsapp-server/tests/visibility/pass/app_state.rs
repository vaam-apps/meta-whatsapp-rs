// The control: the paths the compile-fail cases name resolve, what
// another crate may do with an `AppState` compiles, and so does composing
// one from a backend (`fail/app_state_from_loose_ports.rs` is the same
// call on a memory store).
use std::sync::Arc;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{TokenVault, VaultKeys};
use meta_whatsapp_rs::core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_server::events::Inbound;
use meta_whatsapp_server::metrics::Metrics;
use meta_whatsapp_server::state::{AppState, Settings};
use meta_whatsapp_server::store::{MemoryBackend, MemoryStore, Outbox, Store};

#[allow(dead_code)]
fn public(state: &AppState) {
    let _: &Metrics = state.metrics();
    let _: &Settings = state.settings();
    let _: bool = state.is_shutting_down();
}

#[allow(dead_code)]
fn composed(keys: VaultKeys, client: Client, token: VerifyToken) {
    let inbound = Inbound::new(vec![AppSecret::new("app-secret")], token);
    let _ = inbound.map(|inbound| {
        AppState::from_backend(
            Arc::new(MemoryBackend::new()),
            keys,
            client,
            inbound,
            Metrics::new(),
            Settings::default(),
        )
    });
}

#[allow(dead_code)]
fn named(
    _: Option<Arc<dyn Store>>,
    _: Option<TokenVault>,
    _: Option<Arc<dyn KvStore>>,
    _: Option<Arc<dyn ConversationStore>>,
    _: Option<Arc<dyn Outbox>>,
) {
    let _ = MemoryStore::new;
}

fn main() {}
