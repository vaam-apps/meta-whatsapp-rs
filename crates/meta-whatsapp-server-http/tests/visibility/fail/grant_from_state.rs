// The grant an API adapter's routes are served with (the records, the
// authorization order, the idempotency engine) is handed to the state's
// composer by `AppState::from_backend`, and made nowhere else: a handler,
// a router or a crate given only the state cannot make one (roadmap S7,
// when the API adapter left this crate). `pass/app_state.rs` composes one.
use meta_whatsapp_server_http::state::{AppState, Grant};

fn grant(state: &AppState) -> Grant {
    Grant(state.clone())
}

fn main() {}
