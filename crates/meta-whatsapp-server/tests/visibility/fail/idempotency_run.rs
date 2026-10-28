// The idempotency engine over HTTP runs over the service's own records
// (the core's `idempotency::run`, given the state's): only the handlers
// run it. Another crate would claim a tenant's keys in the service's
// records. (Only imported: naming it would also ask for its future's type.)
#[allow(unused_imports)]
use meta_whatsapp_server::idempotency::run;

fn main() {}
