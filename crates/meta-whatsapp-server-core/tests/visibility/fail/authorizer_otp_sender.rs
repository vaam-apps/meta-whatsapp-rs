// Opening a number for a tenant other than the caller's (the default OTP
// tenant's sending number, decision D8) is the OTP services' own, which
// pass only the caller's tenant or the deployment's setting (roadmap M3c):
// an adapter calling it could send with any tenant's token.
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_server_core::authz::{Authorizer, Caller};
use meta_whatsapp_server_core::model::TenantId;

async fn open(authz: &Authorizer, caller: &Caller, holder: &TenantId, pn: PhoneNumberId) {
    let _ = authz.otp_sender(caller, holder, pn).await;
}

fn main() {}
