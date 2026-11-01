// The backend a state was composed from holds the records and the vault:
// like `store`, the service's own (whoever composed the state holds it
// already; a handler or a router given only the state does not). Since
// roadmap S7 the state no longer keeps it at all (its composer does, to
// close it), so the method is not there, rather than private: a public
// one added later would make this compile.
use meta_whatsapp_server_http::state::AppState;

fn bundle(state: &AppState) {
    let _ = state.backend();
}

fn main() {}
