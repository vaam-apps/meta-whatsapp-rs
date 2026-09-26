// An admin handler takes an `AdminCaller` by value and writes the records
// with it: only the router, behind `admin_guard`, reaches it. A forger's
// `AdminCaller` minted a platform key through it (the security review of
// 3237040).
use meta_whatsapp_server::api::admin::mint_platform_key;

fn main() {
    let _ = mint_platform_key;
}
