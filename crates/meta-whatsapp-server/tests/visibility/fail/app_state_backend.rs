// The backend a state was composed from holds the records and the vault:
// like `store`, the service's own (whoever composed the state holds it
// already; a handler or a router given only the state does not).
use meta_whatsapp_server::state::AppState;

fn bundle(state: &AppState) {
    let _ = state.backend();
}

fn main() {}
