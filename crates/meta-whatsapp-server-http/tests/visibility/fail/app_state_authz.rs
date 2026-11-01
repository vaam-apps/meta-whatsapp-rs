// The authorization order, and through it the vault, is the service's own.
use meta_whatsapp_server_http::state::AppState;

fn authz(state: &AppState) {
    let _ = state.authz();
}

fn main() {}
