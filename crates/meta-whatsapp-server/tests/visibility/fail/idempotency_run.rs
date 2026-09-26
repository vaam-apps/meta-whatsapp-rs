// The idempotency engine over HTTP takes a tenant as given: only the
// handlers, which have it from an admitted `Caller`, run it. Another
// crate would claim a tenant's keys, or replay its kept answers. (Only
// imported: naming it would also ask for its future's type.)
#[allow(unused_imports)]
use meta_whatsapp_server::idempotency::run;

fn main() {}
