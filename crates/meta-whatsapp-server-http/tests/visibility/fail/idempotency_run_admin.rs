// The operator's idempotency engine over HTTP (roadmap M5j2) runs over the
// service's own records like the tenant's: only the handlers run it, an API
// adapter's through the state's `Grant` (`Grant::idempotent_admin`), which no
// one given only the state can make (`grant_from_state.rs`). Another crate
// would claim an admin key's records in the service's. (Only imported:
// naming it would also ask for its future's type.)
#[allow(unused_imports)]
use meta_whatsapp_server_http::idempotency::run_admin;

fn main() {}
