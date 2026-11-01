// The idempotency engine over HTTP runs over the service's own records
// (the core's `idempotency::run`, given the state's): only the handlers
// run it, this crate's through this function and an API adapter's
// through the state's `Grant` (`Grant::idempotent`, roadmap S7), which no
// one given only the state can make (`grant_from_state.rs`). Another crate
// would claim a tenant's keys in the service's records. (Only imported:
// naming it would also ask for its future's type.)
#[allow(unused_imports)]
use meta_whatsapp_server_http::idempotency::run;

fn main() {}
