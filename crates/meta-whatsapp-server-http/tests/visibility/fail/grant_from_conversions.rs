// No conversion makes a `Grant` from a state either (`grant_from_state.rs`
// is the constructor): not `From`, and not axum's `FromRef`, which would
// let any route served with an `AppState` extract one (`State<Grant>`) and
// reach the records and the authorization order. Only
// `AppState::from_backend` hands one out, to the state's composer
// (roadmap S7). A conversion added later makes this compile.
use meta_whatsapp_rs::webhooks::axum::extract::FromRef;
use meta_whatsapp_server_http::state::{AppState, Grant};

fn converted(state: &AppState) -> Grant {
    <Grant as From<AppState>>::from(state.clone())
}

fn extracted(state: &AppState) -> Grant {
    <Grant as FromRef<AppState>>::from_ref(state)
}

fn main() {}
