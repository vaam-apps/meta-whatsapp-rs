// The records decide who owns what: only the service's handlers reach them.
use meta_whatsapp_server::state::AppState;

fn records(state: &AppState) {
    let _ = state.store();
}

fn main() {}
