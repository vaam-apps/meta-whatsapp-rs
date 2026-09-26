// A tenant handler takes a `Caller` by value and reads the records for
// `Caller::tenant()`: only the router, behind `tenant_guard`, reaches it.
// A forger's `Caller` listed any tenant's WABAs through it (the security
// review of 3237040).
use meta_whatsapp_server::api::numbers::list_wabas;

fn main() {
    let _ = list_wabas;
}
