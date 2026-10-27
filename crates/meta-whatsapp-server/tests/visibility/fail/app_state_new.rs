// No constructor takes the state's stores one by one (roadmap S4): the
// ones before it, `new` and `with_settings`, took a records store, a
// vault and a webhook pipeline each built on whatever store the caller
// chose, so one deployment could mix two databases.
use std::sync::Arc;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::TokenVault;
use meta_whatsapp_rs::core::secret::VerifyToken;
use meta_whatsapp_server::events::Inbound;
use meta_whatsapp_server::metrics::Metrics;
use meta_whatsapp_server::state::{AppState, Settings};
use meta_whatsapp_server::store::Store;

fn loose(
    store: Arc<dyn Store>,
    vault: TokenVault,
    client: Client,
    token: VerifyToken,
    inbound: Inbound,
) {
    let _ = AppState::new(store, vault, client, token, Metrics::new(), inbound);
}

fn loose_with_settings(
    store: Arc<dyn Store>,
    vault: TokenVault,
    client: Client,
    token: VerifyToken,
    inbound: Inbound,
) {
    let _ = AppState::with_settings(
        store,
        vault,
        client,
        token,
        Metrics::new(),
        inbound,
        Settings::default(),
    );
}

fn main() {}
