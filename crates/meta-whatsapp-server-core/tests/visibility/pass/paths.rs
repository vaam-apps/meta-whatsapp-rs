// The control: every path the compile-fail cases name resolves, and what
// another crate may do with them compiles. A case failing on a typo would
// fail here too.
use std::sync::Arc;

use bytes::Bytes;
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::embedded_signup::{StoredBusinessToken, TokenVault};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_server_core::ServiceError;
use meta_whatsapp_server_core::authz::{
    AdminCaller, Authorizer, Caller, OwnedNumber, OwnedWaba, Tokens, rotate_vault,
};
use meta_whatsapp_server_core::backend::Backend;
use meta_whatsapp_server_core::inbound::{DELIVERED, Inbound, Pipeline, Refused};
use meta_whatsapp_server_core::media::{DownloadLimits, Media, MediaSlots, Upload};
use meta_whatsapp_server_core::model::TenantId;
use meta_whatsapp_server_core::operation::Operation;
use meta_whatsapp_server_core::ratelimit::{RateLimiter, RateLimits, RouteClass, Slots};
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
    // A request charged by its operation (the class is the core's).
    let limiter = RateLimiter::new(RateLimits::default());
    let _: Result<RouteClass, _> = limiter.charge(caller.tenant(), Operation::ListNumbers);
    let _ = format!("{tokens:?}");
    let number: OwnedNumber = authz.owned_number(caller, pn).await?;
    let _: ServiceError = number.failed(authz, error).await;
    let _: &Client = number.client();
    // The media services, on the number, with limits and an upload made
    // only by their checks.
    let slots = Slots::new(2);
    let media = Media::new(
        authz,
        &number,
        MediaSlots {
            transfers: &slots,
            streams: &slots,
        },
    )?;
    let limits = DownloadLimits::parse(Some("true"), None, 1024)?;
    let _ = media.download("1", limits).await?;
    let upload = Upload::new("image/png", None, Vec::new().into())?;
    let _ = media.upload(&upload).await?;
    let waba: OwnedWaba = authz.owned_waba(caller, waba_id.clone()).await?;
    authz.admit_waba(&waba)?;
    let _: ServiceError = waba.failed(authz, error).await;
    authz.store_token(admin, caller.tenant(), token).await?;
    authz.rotate_vault(admin).await?;
    let _: ServiceError = authz.graph_failed_for_admin(admin, waba_id, error).await;
    authz.forget_for_admin(admin, waba_id).await?;
    waba.forget(authz).await?;
    // With the vault itself (the CLI's `vault rotate`), no capability.
    let _ = rotate_vault(records, vault).await;
    Ok(())
}

// The webhook pipeline, over a backend: what an adapter hands it (the
// signature header's value and the body) and what it answers.
#[allow(dead_code)]
async fn webhooks(backend: &dyn Backend, body: Bytes) -> meta_whatsapp_rs::Result<()> {
    let inbound = Inbound::new(
        vec![AppSecret::new("app-secret")],
        VerifyToken::new("verify-token"),
    )?;
    let _ = inbound.clock();
    let pipeline = Pipeline::new(inbound, backend, Arc::new(()));
    let _: &VerifyToken = pipeline.verify_token();
    let _ = pipeline.outbox();
    match pipeline.deliver(Some("sha256=..."), body).await {
        Ok(delivered) => {
            let _: usize = delivered.duplicates;
            let _: &str = DELIVERED;
        }
        Err(refused) => {
            let _: (u16, &str) = (refused.status(), refused.outcome());
            let _ = matches!(refused, Refused::Failed(_));
        }
    }
    Ok(())
}

fn main() {}
