//! Roadmap S5a: the core's admin services ([`crate::admin`]) over a
//! backend's bundle, with no HTTP layer: the core's [`Authorizer`] over the
//! backend's records and a vault on its key/value store, and a Graph
//! client on a [`ScriptedTransport`]. Decision D4's refusal of a WABA
//! another tenant holds stores nothing (the vault's record is the one it
//! was, version included), subscribes nothing (no request beyond what the
//! case scripted, every scripted answer consumed) and shares nothing (the
//! claimant tenant holds no WABA and no number; the holder's binding and
//! numbers are as they were), whether the claimed id is seen held before
//! Meta is asked anything, or only by the binding's own check, when the
//! holder's attach lands while Meta lists the numbers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, TokenVersion, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_rs::core::transport::{HttpRequest, HttpResponse, HttpTransport};
use serde_json::json;

use crate::admin::{Admin, mint};
use crate::authz::{AdminCaller, Authorizer};
use crate::backend::Backend;
use crate::model::{
    BindOutcome, KeyOwner, NumberBinding, NumberStatus, PageRequest, TenantId, WabaBinding,
};
use crate::store::RecordStore;

/// The tenant holding the WABAs, and the one claiming them.
const HOLDER: &str = "adm-holder";
const CLAIMANT: &str = "adm-claimant";
/// A WABA the holder attached through the service, and one it binds while
/// the claimant's attach is listing its numbers.
const WABA: &str = "102290129340398";
const RACED_WABA: &str = "102290129340399";
/// The numbers Meta lists on [`WABA`], and the one on [`RACED_WABA`].
const NUMBERS: [&str; 2] = ["1972385232742141", "1972385232742142"];
const RACED_NUMBER: &str = "1972385232742143";
const HOLDER_TOKEN: &str = "EAAG-the-holders-system-user-token";
const CLAIMANT_TOKEN: &str = "EAAG-the-claimants-system-user-token";

/// Every case, each on WABAs of its own, on one backend.
pub async fn run(backend: &dyn Backend) {
    a_waba_another_tenant_holds_is_refused_with_nothing_stored(backend).await;
}

/// A Graph transport that, the first time it is asked for
/// [`RACED_WABA`]'s numbers, first binds that WABA to [`HOLDER`] and
/// stores the holder's token, as the holder's own attach landing then
/// would: the claimant's attach found the WABA unbound before it asked
/// Meta, and only its binding's check sees the holder. Every request is
/// then answered by the script.
struct HolderLandsWhileMetaLists {
    script: ScriptedTransport,
    records: Arc<dyn RecordStore>,
    vault: TokenVault,
    armed: AtomicBool,
}

impl std::fmt::Debug for HolderLandsWhileMetaLists {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolderLandsWhileMetaLists")
            .field("script", &self.script)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl HttpTransport for HolderLandsWhileMetaLists {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let listing = request
            .url
            .path()
            .ends_with(&format!("/{RACED_WABA}/phone_numbers"));
        if listing && self.armed.swap(false, Ordering::SeqCst) {
            let bound = self
                .records
                .bind_waba(&tenant(HOLDER), &waba(RACED_WABA), &[pn(RACED_NUMBER)])
                .await
                .unwrap();
            assert_eq!(bound, BindOutcome::Bound, "the holder's binding");
            self.vault
                .store(
                    &StoredBusinessToken::new(waba(RACED_WABA), AccessToken::new(HOLDER_TOKEN))
                        .phone_number_ids([pn(RACED_NUMBER)]),
                )
                .await
                .unwrap();
        }
        self.script.send(request).await
    }
}

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

fn waba(id: &str) -> WabaId {
    WabaId::new(id).unwrap()
}

fn pn(id: &str) -> PhoneNumberId {
    PhoneNumberId::new(id).unwrap()
}

/// What the claimant could have changed, read before and after its
/// attach: the WABA's binding, its numbers' bindings, the vault's record
/// (its token and version), and what the claimant holds.
#[derive(Debug, PartialEq)]
struct Held {
    binding: Option<WabaBinding>,
    numbers: Vec<NumberBinding>,
    token: Option<(String, Vec<PhoneNumberId>, TokenVersion)>,
    claimant_wabas: usize,
    claimant_numbers: usize,
}

async fn held(records: &dyn RecordStore, vault: &TokenVault, waba_id: &str) -> Held {
    let page = PageRequest {
        after: None,
        limit: crate::model::MAX_PAGE_SIZE,
    };
    let token = vault
        .get_versioned(&waba(waba_id))
        .await
        .unwrap()
        .map(|(token, version)| {
            (
                token.token.expose_secret().to_owned(),
                token.phone_number_ids.clone(),
                version,
            )
        });
    Held {
        binding: records.waba(&waba(waba_id)).await.unwrap(),
        numbers: records.waba_numbers(&waba(waba_id)).await.unwrap(),
        token,
        claimant_wabas: records
            .wabas(&tenant(CLAIMANT), &page)
            .await
            .unwrap()
            .items
            .len(),
        claimant_numbers: records
            .numbers(&tenant(CLAIMANT), &page)
            .await
            .unwrap()
            .items
            .len(),
    }
}

/// An admin of the service's own `authz`, with a key minted in `records`.
async fn admin(authz: &Authorizer, records: &dyn RecordStore) -> AdminCaller {
    let (key, _) = mint(records, KeyOwner::Admin, Vec::new(), String::new(), None)
        .await
        .unwrap();
    authz
        .admin_caller(Some(&format!("Bearer {}", key.expose_key())))
        .await
        .unwrap()
}

/// The decisive case of roadmap S5a, with no HTTP layer: attaching a WABA
/// another tenant holds is refused (`409 waba_owned_by_another_tenant`)
/// with nothing stored, subscribed or shared.
///
/// 1. The holder attaches a WABA through the service: Meta lists its
///    numbers, then the app is subscribed, in that order, with the
///    holder's token, which the vault now holds for both numbers.
/// 2. The claimant's attach of that WABA is refused before Meta is asked
///    anything: no request is sent at all (a listing would also have
///    spent the claimant's token on the holder's WABA).
/// 3. The claimant's attach of a second WABA, unbound when it looks, is
///    refused by the binding's own D4 check once the holder bound it
///    while Meta listed its numbers: the listing is the only request (no
///    subscription), and the holder's token is not overwritten.
///
/// Decisive: skipping the check before Meta, or moving it after the vault
/// store, fails step 2 (a listing is sent, and nothing answers it);
/// ignoring the binding's refusal fails step 3 (the store's own
/// confirmation, the only guard left, answers `503`); storing before
/// binding fails step 1 already (the store finds no binding of the
/// holder's: `503`).
pub async fn a_waba_another_tenant_holds_is_refused_with_nothing_stored(backend: &dyn Backend) {
    let records = backend.records();
    for id in [HOLDER, CLAIMANT] {
        records.create_tenant(&tenant(id), "").await.unwrap();
    }
    let vault = TokenVault::new(
        backend.kv(),
        VaultKeys::new(VaultKey::generate("admin-suite").unwrap()),
    )
    .unwrap();
    let script = ScriptedTransport::new();
    let transport = HolderLandsWhileMetaLists {
        script: script.clone(),
        records: records.clone(),
        vault: vault.clone(),
        armed: AtomicBool::new(true),
    };
    let client = Client::builder().transport(transport).build().unwrap();
    let authz = Authorizer::new(records.clone(), vault.clone(), client).unwrap();
    let admin = admin(&authz, records.as_ref()).await;
    let service = Admin::new(&authz, &admin).unwrap();

    // 1. The holder's attach, through the service.
    script.push_json(
        200,
        json!({"data": [{"id": NUMBERS[0]}, {"id": NUMBERS[1]}],
               "paging": {"cursors": {"before": "abcdefghij", "after": "klmnopqr"}}}),
    );
    script.push_json(200, json!({"success": true}));
    let attached = service
        .attach_waba(HOLDER, WABA, AccessToken::new(HOLDER_TOKEN))
        .await
        .unwrap();
    assert_eq!(attached.tenant_id, tenant(HOLDER));
    assert_eq!(attached.phone_number_ids, NUMBERS.map(pn));
    let requests = script.requests();
    let sent: Vec<(String, String, Option<String>)> = requests
        .iter()
        .map(|r| {
            (
                r.method.to_string(),
                r.path().to_owned(),
                r.bearer().map(str::to_owned),
            )
        })
        .collect();
    assert_eq!(
        sent,
        [
            (
                "GET".to_owned(),
                format!("/v25.0/{WABA}/phone_numbers"),
                Some(HOLDER_TOKEN.to_owned())
            ),
            (
                "POST".to_owned(),
                format!("/v25.0/{WABA}/subscribed_apps"),
                Some(HOLDER_TOKEN.to_owned())
            ),
        ],
        "the holder's attach: listed, then subscribed"
    );
    assert_eq!(script.remaining(), 0);
    let before = held(records.as_ref(), &vault, WABA).await;
    assert_eq!(
        before.binding.as_ref().map(|b| b.tenant_id.clone()),
        Some(tenant(HOLDER))
    );
    assert_eq!(
        before.token.as_ref().map(|t| t.0.as_str()),
        Some(HOLDER_TOKEN)
    );
    assert_eq!(before.numbers.len(), 2);
    assert!(
        before
            .numbers
            .iter()
            .all(|n| n.tenant_id == tenant(HOLDER) && n.status == NumberStatus::Connected)
    );
    assert_eq!((before.claimant_wabas, before.claimant_numbers), (0, 0));

    // 2. The claimant's attach of the held WABA: refused before Meta.
    let refused = service
        .attach_waba(CLAIMANT, WABA, AccessToken::new(CLAIMANT_TOKEN))
        .await
        .unwrap_err();
    assert_eq!(
        (refused.status(), refused.code()),
        (409, "waba_owned_by_another_tenant")
    );
    assert!(refused.graph().is_none(), "Meta was not asked");
    assert_eq!(
        script.requests().len(),
        2,
        "the claimant's attach sent a request: {:?}",
        script.requests().get(2).map(|r| r.path().to_owned())
    );
    assert_eq!(script.remaining(), 0);
    assert_eq!(
        held(records.as_ref(), &vault, WABA).await,
        before,
        "the claimant's refused attach changed what is held"
    );

    // 3. The claimant's attach of a WABA the holder binds while Meta
    //    lists it: refused by the binding, nothing stored or subscribed.
    script.push_json(200, json!({"data": [{"id": RACED_NUMBER}]}));
    let refused = service
        .attach_waba(CLAIMANT, RACED_WABA, AccessToken::new(CLAIMANT_TOKEN))
        .await
        .unwrap_err();
    assert_eq!(
        (refused.status(), refused.code()),
        (409, "waba_owned_by_another_tenant"),
        "{refused:?}"
    );
    let raced: Vec<(String, String, Option<String>)> = script.requests()[2..]
        .iter()
        .map(|r| {
            (
                r.method.to_string(),
                r.path().to_owned(),
                r.bearer().map(str::to_owned),
            )
        })
        .collect();
    assert_eq!(
        raced,
        [(
            "GET".to_owned(),
            format!("/v25.0/{RACED_WABA}/phone_numbers"),
            Some(CLAIMANT_TOKEN.to_owned())
        )],
        "the claimant's attach: listed, never subscribed"
    );
    assert_eq!(script.remaining(), 0);
    let raced = held(records.as_ref(), &vault, RACED_WABA).await;
    assert_eq!(
        raced.binding.as_ref().map(|b| b.tenant_id.clone()),
        Some(tenant(HOLDER)),
        "the raced WABA's binding"
    );
    assert_eq!(
        raced.token.as_ref().map(|t| (t.0.as_str(), t.1.clone())),
        Some((HOLDER_TOKEN, vec![pn(RACED_NUMBER)])),
        "the holder's token for the raced WABA was overwritten or deleted"
    );
    assert!(
        raced
            .numbers
            .iter()
            .all(|n| n.tenant_id == tenant(HOLDER) && n.status == NumberStatus::Connected),
        "{:?}",
        raced.numbers
    );
    assert_eq!(
        (raced.claimant_wabas, raced.claimant_numbers),
        (0, 0),
        "the claimant holds something"
    );
    // And the first WABA is still as it was.
    assert_eq!(held(records.as_ref(), &vault, WABA).await, before);
}
