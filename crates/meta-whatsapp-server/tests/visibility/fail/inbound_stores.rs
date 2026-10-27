// The webhook pipeline is built inside the state, on the backend's stores
// (roadmap S4): `Inbound` takes what Meta's deliveries are checked with,
// and no store of the caller's (a dedup store, an inbox or an outbox of
// another database).
use std::sync::Arc;

use meta_whatsapp_rs::core::secret::AppSecret;
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_server::events::Inbound;
use meta_whatsapp_server::store::Outbox;

fn loose(kv: Arc<dyn KvStore>, conversations: Arc<dyn ConversationStore>, outbox: Arc<dyn Outbox>) {
    let _ = Inbound::new(
        vec![AppSecret::new("app-secret")],
        kv,
        conversations,
        outbox,
    );
}

fn main() {}
