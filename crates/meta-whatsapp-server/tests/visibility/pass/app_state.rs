// The control: the path the compile-fail cases name resolves, and what
// another crate may do with an `AppState` compiles.
use meta_whatsapp_server::metrics::Metrics;
use meta_whatsapp_server::state::{AppState, Settings};

#[allow(dead_code)]
fn public(state: &AppState) {
    let _: &Metrics = state.metrics();
    let _: &Settings = state.settings();
    let _: bool = state.is_shutting_down();
}

fn main() {}
