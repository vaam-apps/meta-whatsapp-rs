// The control: the paths the compile-fail cases name resolve, what
// another crate may do with an `AppState` compiles, and so does composing
// one from a backend (`fail/app_state_from_loose_ports.rs` is the same
// call on a memory store), which hands the composer the state's `Grant`
// and what an API adapter reaches through it (`fail/grant_from_state.rs`:
// nothing else makes one).
use std::sync::Arc;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{TokenVault, VaultKeys};
use meta_whatsapp_rs::core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_server_http::events::Inbound;
use meta_whatsapp_server_http::metrics::Metrics;
use meta_whatsapp_server_http::state::{AppState, Grant, Settings};
use meta_whatsapp_server_http::store::{Outbox, RecordStore, Store};
use meta_whatsapp_server_store_memory::{MemoryBackend, MemoryStore};

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
        let composed: Result<(AppState, Grant), _> = AppState::from_backend(
            Arc::new(MemoryBackend::new()),
            keys,
            client,
            inbound,
            Metrics::new(),
            Settings::default(),
        );
        composed
    });
}

#[allow(dead_code)]
fn granted(grant: &Grant) {
    let _: &AppState = grant.state();
    let _: &dyn RecordStore = grant.store();
    let _: &dyn Outbox = grant.outbox();
    let _ = grant.authz();
    let _ = grant.template_cache();
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
