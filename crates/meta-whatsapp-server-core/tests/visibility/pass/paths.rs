// The control: every path the compile-fail cases name resolves, and what
// another crate may do with them compiles. A case failing on a typo would
// fail here too.
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::embedded_signup::{StoredBusinessToken, TokenVault};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_server_core::ServiceError;
use meta_whatsapp_server_core::authz::{
    AdminCaller, Authorizer, Caller, OwnedNumber, OwnedWaba, Tokens, rotate_vault,
};
use meta_whatsapp_server_core::model::TenantId;
use meta_whatsapp_server_core::store::RecordStore;

#[allow(dead_code, clippy::too_many_arguments)]
async fn public(
    authz: &Authorizer,
    admin: &AdminCaller,
    caller: &Caller,
    tokens: &Tokens,
    records: &dyn RecordStore,
    vault: &TokenVault,
    token: &StoredBusinessToken,
    waba_id: &WabaId,
    pn: PhoneNumberId,
    error: &Error,
) -> Result<(), ServiceError> {
    let _: &Client = authz.client();
    let _: &str = admin.key_id();
    let _: &TenantId = caller.tenant();
    let _ = format!("{tokens:?}");
    let number: OwnedNumber = authz.owned_number(caller, pn).await?;
    let _: ServiceError = number.failed(authz, error).await;
    let _: &Client = number.client();
    let waba: OwnedWaba = authz.owned_waba(caller, waba_id.clone()).await?;
    let _: ServiceError = waba.failed(authz, error).await;
    authz.store_token(admin, token).await?;
    authz.rotate_vault(admin).await?;
    let _: ServiceError = authz.graph_failed_for_admin(admin, waba_id, error).await;
    authz.forget_for_admin(admin, waba_id).await?;
    waba.forget(authz).await?;
    // With the vault itself (the CLI's `vault rotate`), no capability.
    let _ = rotate_vault(records, vault).await;
    Ok(())
}

fn main() {}
