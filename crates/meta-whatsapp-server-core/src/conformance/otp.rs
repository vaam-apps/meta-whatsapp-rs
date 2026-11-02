//! Roadmap M3c: the core's OTP services ([`crate::otp`]) over a backend's
//! bundle, with no HTTP layer: its records (the tenants' OTP settings,
//! [`RecordStore::otp_settings`], and the bindings), the library's codes
//! in its key/value store, the vault on that store, and a Graph client on a
//! [`ScriptedTransport`]. What it holds a backend to:
//!
//! - **A tenant's OTP settings are its own record**: set, read back
//!   whole, replaced, cleared, refused for a tenant that does not exist,
//!   gone with the tenant and absent for a tenant created again under its
//!   id; setting them moves the tenant's `updated_at`.
//! - **The namespace** (acceptance test M3.5, decisive): tenant A's code,
//!   sent from the number tenant B sends from too (the default OTP
//!   tenant's), verifies at no other tenant: B's checks are `not_found` and
//!   count no attempt against A's code, which then verifies at A.
//! - **The sending number is its holder's** (decision D8): settings naming
//!   a number the tenant no longer holds are `409 otp_not_configured`, with
//!   no request to Meta.
//! - **Another authorizer's caller** opens nothing (`403 forbidden`).
//! - **The default OTP tenant's number has one issue limit per
//!   recipient** (M3c's review), shared by every tenant and purpose that
//!   sends from it, kept in the backend's key/value store; and **a
//!   suspended default tenant lends nothing** (`409 otp_not_configured`).
//!
//! [`RecordStore::otp_settings`]: crate::store::RecordStore::otp_settings

use std::sync::Arc;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::clock::SystemClock;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::{AccessToken, SecretBytes};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use serde_json::json;

use crate::admin::mint;
use crate::authz::{Authorizer, Caller};
use crate::backend::Backend;
use crate::model::{
    BindOutcome, DeleteTenantOutcome, KeyOwner, OtpSettings, Scope, TenantId, TenantStatus,
};
use crate::otp::{Checked, IssueRequest, Issued, OtpCodes, OtpDeployment, OtpKeys, VerifyRequest};

/// The default OTP tenant, its WABA, its number and its token.
const PLATFORM: (&str, &str, &str) = ("otp-platform", "102290129340410", "106540352242930");
const PLATFORM_TOKEN: &str = "EAAG-the-platforms-token";
const A: &str = "otp-shop-a";
const B: &str = "otp-shop-b";
const PHONE: &str = "+16505551234";

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    a_tenants_otp_settings_are_its_own_record(backend).await;
    a_code_verifies_at_no_other_tenant_on_the_same_number(backend).await;
    the_default_numbers_issue_limit_is_shared_and_fails_closed(backend).await;
}

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

fn settings(pn: &str) -> OtpSettings {
    OtpSettings::new(PhoneNumberId::new(pn).unwrap(), "login_code", "en_US")
}

/// A tenant's OTP settings: the record's contract.
pub async fn a_tenants_otp_settings_are_its_own_record(backend: &dyn Backend) {
    let records = backend.records();
    let store = records.as_ref();
    let id = tenant("otp-settings-tenant");
    let nobody = tenant("otp-settings-nobody");
    assert!(
        !store
            .set_otp_settings(&nobody, Some(&settings("1")))
            .await
            .unwrap()
    );
    assert!(!store.set_otp_settings(&nobody, None).await.unwrap());
    assert_eq!(store.otp_settings(&nobody).await.unwrap(), None);
    let created = store.create_tenant(&id, "").await.unwrap().unwrap();
    assert_eq!(
        store.otp_settings(&id).await.unwrap(),
        None,
        "none at first"
    );
    let mut first = settings("106540352242931");
    first.code_length = 8;
    first.ttl_seconds = 300;
    first.max_attempts = 3;
    first.resend_cooldown_seconds = 60;
    first.max_issues = 4;
    first.issue_window_seconds = 7200;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert!(store.set_otp_settings(&id, Some(&first)).await.unwrap());
    assert_eq!(
        store.otp_settings(&id).await.unwrap(),
        Some(first),
        "read back whole"
    );
    let updated = store.tenant(&id).await.unwrap().unwrap();
    assert!(updated.updated_at > created.updated_at, "updated_at moved");
    let second = settings("106540352242932");
    assert!(store.set_otp_settings(&id, Some(&second)).await.unwrap());
    assert_eq!(store.otp_settings(&id).await.unwrap(), Some(second.clone()));
    // Another tenant's are its own.
    let other = tenant("otp-settings-other");
    store.create_tenant(&other, "").await.unwrap().unwrap();
    assert_eq!(store.otp_settings(&other).await.unwrap(), None);
    assert!(store.set_otp_settings(&id, None).await.unwrap(), "cleared");
    assert_eq!(store.otp_settings(&id).await.unwrap(), None);
    assert!(store.set_otp_settings(&id, Some(&second)).await.unwrap());
    // Gone with the tenant: one created again under its id has none.
    assert_eq!(
        store.delete_tenant(&id).await.unwrap(),
        DeleteTenantOutcome::Deleted
    );
    assert_eq!(store.otp_settings(&id).await.unwrap(), None);
    store.create_tenant(&id, "").await.unwrap().unwrap();
    assert_eq!(
        store.otp_settings(&id).await.unwrap(),
        None,
        "a tenant created again under the id inherits nothing"
    );
}

/// A caller of `tenant` with scope `otp`, admitted by `authz`.
async fn caller(backend: &dyn Backend, authz: &Authorizer, tenant: &str) -> Caller {
    let key = mint(
        backend.records().as_ref(),
        KeyOwner::Tenant(TenantId::parse(tenant).unwrap()),
        vec![Scope::Otp],
        String::new(),
        None,
    )
    .await
    .unwrap()
    .0;
    authz
        .tenant_caller(
            Some(&format!("Bearer {}", key.expose_key())),
            None,
            Scope::Otp,
        )
        .await
        .unwrap()
}

/// `code` checked for [`PHONE`] by `caller`.
async fn check(codes: &OtpCodes, authz: &Authorizer, caller: &Caller, code: &str) -> Checked {
    codes
        .open(authz, caller)
        .unwrap()
        .verify(VerifyRequest {
            phone: PHONE.to_owned(),
            purpose: None,
            code: code.to_owned(),
        })
        .await
        .unwrap()
}

/// M3.5's decisive case and the holder check, through the core's services.
pub async fn a_code_verifies_at_no_other_tenant_on_the_same_number(backend: &dyn Backend) {
    let records = backend.records();
    let (platform, waba, pn) = PLATFORM;
    for t in [platform, A, B] {
        records.create_tenant(&tenant(t), "").await.unwrap();
    }
    let waba = WabaId::new(waba).unwrap();
    let number = PhoneNumberId::new(pn).unwrap();
    assert_eq!(
        records
            .bind_waba(&tenant(platform), &waba, std::slice::from_ref(&number))
            .await
            .unwrap(),
        BindOutcome::Bound
    );
    let vault = TokenVault::new(
        backend.kv(),
        VaultKeys::new(VaultKey::generate("otp-suite").unwrap()),
    )
    .unwrap();
    vault
        .store(
            &StoredBusinessToken::new(waba.clone(), AccessToken::new(PLATFORM_TOKEN))
                .phone_number_ids([number.clone()]),
        )
        .await
        .unwrap();
    assert!(
        records
            .set_otp_settings(&tenant(platform), Some(&settings(pn)))
            .await
            .unwrap()
    );
    let script = ScriptedTransport::new();
    let client = Client::builder().transport(script.clone()).build().unwrap();
    let authz = Authorizer::new(records.clone(), vault, client).unwrap();
    let keys = OtpKeys::new(SecretBytes::new(vec![42u8; 32])).unwrap();
    let codes = OtpCodes::new(
        backend.kv(),
        Arc::new(SystemClock),
        Some(OtpDeployment::new(keys, Some(tenant(platform)))),
    );
    let a = caller(backend, &authz, A).await;
    let b = caller(backend, &authz, B).await;

    script.push_json(
        200,
        json!({
            "messaging_product": "whatsapp",
            "contacts": [{"input": PHONE, "wa_id": "16505551234"}],
            "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
        }),
    );
    let issued = codes
        .open(&authz, &a)
        .unwrap()
        .issue(IssueRequest {
            phone: PHONE.to_owned(),
            purpose: None,
        })
        .await
        .unwrap();
    assert!(matches!(issued, Issued::Sent { .. }), "{issued:?}");
    let request = script.last_request().unwrap();
    assert_eq!(request.path(), format!("/v25.0/{pn}/messages"));
    assert_eq!(request.bearer(), Some(PLATFORM_TOKEN));
    let code = request.json().unwrap()["template"]["components"][0]["parameters"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    // B, on the same number, five times with A's right code: nothing.
    for _ in 0..5 {
        assert_eq!(check(&codes, &authz, &b, &code).await, Checked::NotFound);
    }
    assert_eq!(
        check(&codes, &authz, &a, &code).await,
        Checked::Verified,
        "B's checks counted no attempt"
    );
    assert_eq!(
        check(&codes, &authz, &a, &code).await,
        Checked::NotFound,
        "used up"
    );

    // The platform's number moves to B: the platform's settings no longer
    // name its own number, and A's codes go out from nowhere.
    assert!(records.unbind_waba(&waba).await.unwrap());
    assert_eq!(
        records
            .bind_waba(&tenant(B), &waba, std::slice::from_ref(&number))
            .await
            .unwrap(),
        BindOutcome::Bound
    );
    let asked = script.requests().len();
    let stale = codes
        .open(&authz, &a)
        .unwrap()
        .issue(IssueRequest {
            phone: PHONE.to_owned(),
            purpose: Some("again".to_owned()),
        })
        .await
        .unwrap_err();
    assert_eq!(stale.code(), "otp_not_configured");
    assert_eq!(script.requests().len(), asked, "Meta was not asked");

    // Another authorizer's caller opens nothing.
    let (other, _) = {
        let script = ScriptedTransport::new();
        let client = Client::builder().transport(script.clone()).build().unwrap();
        let vault = TokenVault::new(
            backend.kv(),
            VaultKeys::new(VaultKey::generate("otp-suite-other").unwrap()),
        )
        .unwrap();
        (
            Authorizer::new(records.clone(), vault, client).unwrap(),
            script,
        )
    };
    let forged = caller(backend, &other, A).await;
    assert_eq!(codes.open(&authz, &forged).unwrap_err().code(), "forbidden");
    assert_eq!(script.remaining(), 0);
}

/// The default OTP tenant's number allows a recipient its settings' issue
/// limit (here 1 code) across tenants and purposes: A's code is sent, then
/// B's and A's under another purpose are `rate_limited`, nothing sent;
/// suspended, the default tenant lends nothing (`409
/// otp_not_configured`). On the backend's key/value store and records.
pub async fn the_default_numbers_issue_limit_is_shared_and_fails_closed(backend: &dyn Backend) {
    let (platform, a, b) = ("otp-shared-platform", "otp-shared-a", "otp-shared-b");
    let (waba, pn) = ("102290129340420", "106540352242940");
    let records = backend.records();
    for t in [platform, a, b] {
        records.create_tenant(&tenant(t), "").await.unwrap();
    }
    let waba = WabaId::new(waba).unwrap();
    let number = PhoneNumberId::new(pn).unwrap();
    assert_eq!(
        records
            .bind_waba(&tenant(platform), &waba, std::slice::from_ref(&number))
            .await
            .unwrap(),
        BindOutcome::Bound
    );
    let vault = TokenVault::new(
        backend.kv(),
        VaultKeys::new(VaultKey::generate("otp-shared-suite").unwrap()),
    )
    .unwrap();
    vault
        .store(
            &StoredBusinessToken::new(waba.clone(), AccessToken::new(PLATFORM_TOKEN))
                .phone_number_ids([number.clone()]),
        )
        .await
        .unwrap();
    let mut limited = settings(pn);
    limited.max_issues = 1;
    assert!(
        records
            .set_otp_settings(&tenant(platform), Some(&limited))
            .await
            .unwrap()
    );
    let script = ScriptedTransport::new();
    let client = Client::builder().transport(script.clone()).build().unwrap();
    let authz = Authorizer::new(records.clone(), vault, client).unwrap();
    // A pepper of this case's own: a shared count of another run on the
    // same store is under other keys.
    let mut pepper = vec![0u8; 32];
    getrandom::fill(&mut pepper).unwrap();
    let keys = OtpKeys::new(SecretBytes::new(pepper)).unwrap();
    let codes = OtpCodes::new(
        backend.kv(),
        Arc::new(SystemClock),
        Some(OtpDeployment::new(keys, Some(tenant(platform)))),
    );
    let a = caller(backend, &authz, a).await;
    let b = caller(backend, &authz, b).await;
    let request = |purpose: &str| IssueRequest {
        phone: PHONE.to_owned(),
        purpose: Some(purpose.to_owned()),
    };
    script.push_json(
        200,
        json!({
            "messaging_product": "whatsapp",
            "contacts": [{"input": PHONE, "wa_id": "16505551234"}],
            "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
        }),
    );
    let sent = codes
        .open(&authz, &a)
        .unwrap()
        .issue(request("login"))
        .await
        .unwrap();
    assert!(matches!(sent, Issued::Sent { .. }), "{sent:?}");
    for (caller, purpose) in [(&b, "login"), (&a, "reset")] {
        let refused = codes
            .open(&authz, caller)
            .unwrap()
            .issue(request(purpose))
            .await
            .unwrap();
        assert!(
            matches!(refused, Issued::RateLimited { retry_after_seconds } if retry_after_seconds > 3500),
            "{purpose}: {refused:?}"
        );
    }
    assert_eq!(script.requests().len(), 1, "one code sent");

    records
        .update_tenant(&tenant(platform), None, Some(TenantStatus::Suspended))
        .await
        .unwrap()
        .unwrap();
    let suspended = codes
        .open(&authz, &b)
        .unwrap()
        .issue(request("other"))
        .await
        .unwrap_err();
    assert_eq!(suspended.code(), "otp_not_configured");
    assert_eq!(script.requests().len(), 1, "Meta was not asked");
    assert_eq!(script.remaining(), 0);
}
