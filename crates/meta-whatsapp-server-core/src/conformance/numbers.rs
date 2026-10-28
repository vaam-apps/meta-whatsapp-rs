//! Roadmap S5c: the core's number services ([`crate::numbers`]) over a
//! backend's bundle, with no HTTP layer: the core's [`Authorizer`] over
//! the backend's records and a vault on its key/value store, and a Graph
//! client on a [`ScriptedTransport`]. The services open only on a
//! capability their own [`Authorizer`] made; a number's details and its
//! business profile are read, and the profile changed, with Meta's
//! documented requests and the number's token; a change Meta's names or
//! limits refuse sends nothing, and an empty one only reads; a WABA is
//! disconnected only once Meta unsubscribed the app (a refusal deletes
//! nothing), and a capability whose WABA moved since unsubscribes
//! nothing.

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use serde_json::{Value, json};

use super::messages::Told;
use super::templates::{pairs, query};
use crate::admin::mint;
use crate::authz::{Authorizer, Caller};
use crate::backend::Backend;
use crate::model::{BindOutcome, KeyOwner, NumberStatus, Scope, TenantId};
use crate::numbers::{
    BusinessProfile, NUMBER_FIELDS, NumberDetails, Numbers, ProfileChange, Throughput, Waba,
};

/// The tenant, its WABA, its number and the token in the vault.
const TENANT: &str = "num-owner";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-the-owners-token";

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    the_number_services_read_change_and_disconnect_as_meta_documents(backend).await;
}

/// business-profiles, "Get your profile via the API" example response.
fn lucky_shrub() -> Value {
    json!({
        "data": [{
            "about": "Succulent specialists!",
            "address": "1 Hacker Way, Menlo Park, CA 94025",
            "description": "At Lucky Shrub, we specialize in providing a...",
            "email": "lucky@luckyshrub.com",
            "profile_picture_url": "https://pps.whatsapp.net/v/t61.24...",
            "websites": ["https://www.luckyshrub.com/"],
            "vertical": "RETAIL",
            "messaging_product": "whatsapp"
        }]
    })
}

/// [`lucky_shrub`], as the services answer it.
fn lucky_shrub_profile() -> BusinessProfile {
    BusinessProfile {
        about: Some("Succulent specialists!".to_owned()),
        address: Some("1 Hacker Way, Menlo Park, CA 94025".to_owned()),
        description: Some("At Lucky Shrub, we specialize in providing a...".to_owned()),
        email: Some("lucky@luckyshrub.com".to_owned()),
        websites: vec!["https://www.luckyshrub.com/".to_owned()],
        vertical: Some("RETAIL".to_owned()),
    }
}

/// A `numbers` key for `tenant` that `authz` admits.
async fn caller_of(backend: &dyn Backend, authz: &Authorizer, tenant: &str) -> Caller {
    let key = mint(
        backend.records().as_ref(),
        KeyOwner::Tenant(TenantId::parse(tenant).unwrap()),
        vec![Scope::Numbers],
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
            Scope::Numbers,
        )
        .await
        .unwrap()
}

/// The number and WABA services as Meta documents them.
///
/// 1. `Numbers::new` and `Waba::new` with a capability another
///    [`Authorizer`] made (over the same records and vault), or with this
///    one's through the other order, are `403 forbidden`.
/// 2. The details: `GET /{pn}` with [`NUMBER_FIELDS`] and the number's
///    token; Meta's documented answer, field by field, `connected`.
/// 3. The profile: `GET /{pn}/whatsapp_business_profile` with the
///    profile fields; Meta's documented answer, field by field.
/// 4. A change: `POST /{pn}/whatsapp_business_profile` with the given
///    fields only (each of the six reaching Meta), then the profile read
///    again; an empty change only reads; a vertical that can be read but
///    not set (`NOT_A_BIZ`, `UNDEFINED`), or an address over 256
///    characters, is refused on its field before any request (a name the
///    library does not know goes to Meta, which judges it). Meta refusing
///    the details, the profile or a change answers its error with its
///    `details`.
/// 5. Disconnecting: Meta failing (`502`) or refusing (its error, with
///    its `details`, told to the [`Observer`](crate::observe::Observer))
///    to unsubscribe the app deletes nothing: the token, the binding and
///    the number are kept. Meta
///    unsubscribing it deletes the token, the binding and the number,
///    each `DELETE /{waba}/subscribed_apps` with the WABA's token; the
///    same capability again (its WABA gone since) is `503`, retryable,
///    with no request.
///
/// Decisive: `Numbers::new`'s or `Waba::new`'s admission dropped fails
/// step 1; the empty change's guard dropped fails step 4 (a `POST`,
/// which nothing answers); the change's checks skipped fail step 4 (a
/// request sent), as does a field of the change dropped on its way to
/// the library's update; Meta's `details` dropped from the number
/// services' errors fails step 4, from the disconnection's step 5;
/// deleting before Meta unsubscribed, or `still_bound` dropped, fails
/// step 5.
pub async fn the_number_services_read_change_and_disconnect_as_meta_documents(
    backend: &dyn Backend,
) {
    let records = backend.records();
    let tenant = TenantId::parse(TENANT).unwrap();
    records.create_tenant(&tenant, "").await.unwrap();
    let waba_id = WabaId::new(WABA).unwrap();
    let pn = PhoneNumberId::new(PN).unwrap();
    let bound = records
        .bind_waba(&tenant, &waba_id, std::slice::from_ref(&pn))
        .await
        .unwrap();
    assert_eq!(bound, BindOutcome::Bound);
    let vault = TokenVault::new(
        backend.kv(),
        VaultKeys::new(VaultKey::generate("numbers-suite").unwrap()),
    )
    .unwrap();
    vault
        .store(
            &StoredBusinessToken::new(waba_id.clone(), AccessToken::new(TOKEN))
                .phone_number_ids([pn.clone()]),
        )
        .await
        .unwrap();
    let script = ScriptedTransport::new();
    let client = Client::builder().transport(script.clone()).build().unwrap();
    let authz = Authorizer::new(records.clone(), vault.clone(), client).unwrap();
    let caller = caller_of(backend, &authz, TENANT).await;
    let number = authz.owned_number(&caller, pn.clone()).await.unwrap();
    let waba = authz.owned_waba(&caller, waba_id.clone()).await.unwrap();
    let told = Told::default();

    // 1. Only the order's own capabilities.
    let other_script = ScriptedTransport::new();
    let other_client = Client::builder()
        .transport(other_script.clone())
        .build()
        .unwrap();
    let other = Authorizer::new(records.clone(), vault.clone(), other_client).unwrap();
    let forged = caller_of(backend, &other, TENANT).await;
    let their_number = other.owned_number(&forged, pn.clone()).await.unwrap();
    let their_waba = other.owned_waba(&forged, waba_id.clone()).await.unwrap();
    for (refused, which) in [
        (
            Numbers::new(&authz, &their_number).map(drop),
            "their number",
        ),
        (
            Numbers::new(&other, &number).map(drop),
            "the number through their order",
        ),
        (Waba::new(&authz, &their_waba).map(drop), "their WABA"),
        (
            Waba::new(&other, &waba).map(drop),
            "the WABA through their order",
        ),
    ] {
        let refused = refused.unwrap_err();
        assert_eq!(
            (refused.status(), refused.code()),
            (403, "forbidden"),
            "{which}"
        );
    }
    assert!(Numbers::new(&other, &their_number).is_ok());
    assert!(Waba::new(&other, &their_waba).is_ok());
    let numbers = Numbers::new(&authz, &number).unwrap().observed_by(&told);

    // 2. The details (solution-providers/manage-phone-numbers, and the
    // hosted-es listing for the throughput).
    script.push_json(
        200,
        json!({
            "id": PN,
            "display_phone_number": "+1 631-555-1111",
            "verified_name": "John's Cake Shop",
            "quality_rating": "GREEN",
            "name_status": "APPROVED",
            "throughput": {"level": "STANDARD"}
        }),
    );
    let details = numbers.details().await.unwrap();
    assert_eq!(
        details,
        NumberDetails {
            phone_number_id: pn.clone(),
            waba_id: waba_id.clone(),
            status: NumberStatus::Connected,
            display_phone_number: Some("+1 631-555-1111".to_owned()),
            verified_name: Some("John's Cake Shop".to_owned()),
            quality_rating: Some("GREEN".to_owned()),
            name_status: Some("APPROVED".to_owned()),
            throughput: Some(Throughput {
                level: Some("STANDARD".to_owned()),
            }),
        }
    );
    let request = script.last_request().unwrap();
    assert_eq!(
        (request.method.as_str(), request.path()),
        ("GET", format!("/v25.0/{PN}").as_str())
    );
    assert_eq!(
        query(&request),
        pairs(&[("fields", &NUMBER_FIELDS.join(","))])
    );
    assert_eq!(request.bearer(), Some(TOKEN));

    // 3. The profile.
    let profile_path = format!("/v25.0/{PN}/whatsapp_business_profile");
    let profile_fields = pairs(&[(
        "fields",
        "about,address,description,email,websites,vertical",
    )]);
    script.push_json(200, lucky_shrub());
    assert_eq!(numbers.profile().await.unwrap(), lucky_shrub_profile());
    let request = script.last_request().unwrap();
    assert_eq!(
        (request.method.as_str(), request.path()),
        ("GET", profile_path.as_str())
    );
    assert_eq!(query(&request), profile_fields);
    assert_eq!(request.bearer(), Some(TOKEN));

    // 4. A change: the given fields only, then the profile.
    script.push_json(200, json!({"success": true}));
    script.push_json(200, lucky_shrub());
    let changed = numbers
        .update_profile(ProfileChange {
            about: Some("Succulent specialists!".to_owned()),
            address: Some("1 Hacker Way, Menlo Park, CA 94025".to_owned()),
            description: Some("At Lucky Shrub, we specialize in providing a...".to_owned()),
            email: Some("lucky@luckyshrub.com".to_owned()),
            websites: Some(vec!["https://www.luckyshrub.com/".to_owned()]),
            vertical: Some("RETAIL".to_owned()),
        })
        .await
        .unwrap();
    assert_eq!(changed, lucky_shrub_profile());
    let requests = script.requests();
    let [.., posted, read] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(
        (posted.method.as_str(), posted.path()),
        ("POST", profile_path.as_str())
    );
    assert_eq!(posted.bearer(), Some(TOKEN));
    assert_eq!(
        posted.json().unwrap(),
        json!({"messaging_product": "whatsapp", "about": "Succulent specialists!",
               "address": "1 Hacker Way, Menlo Park, CA 94025",
               "description": "At Lucky Shrub, we specialize in providing a...",
               "email": "lucky@luckyshrub.com",
               "websites": ["https://www.luckyshrub.com/"], "vertical": "RETAIL"})
    );
    assert_eq!(
        (read.method.as_str(), read.path()),
        ("GET", profile_path.as_str())
    );
    assert_eq!(query(read), profile_fields);
    script.push_json(200, lucky_shrub());
    numbers
        .update_profile(ProfileChange::default())
        .await
        .unwrap();
    assert_eq!(requests.len() + 1, script.requests().len());
    assert_eq!(script.last_request().unwrap().method.as_str(), "GET");
    let before = script.requests().len();
    for (change, field) in [
        (
            ProfileChange {
                vertical: Some("NOT_A_BIZ".to_owned()),
                ..ProfileChange::default()
            },
            "vertical",
        ),
        (
            ProfileChange {
                vertical: Some("UNDEFINED".to_owned()),
                ..ProfileChange::default()
            },
            "vertical",
        ),
        (
            ProfileChange {
                address: Some("x".repeat(257)),
                ..ProfileChange::default()
            },
            "address",
        ),
    ] {
        let refused = numbers.update_profile(change).await.unwrap_err();
        assert_eq!(
            (refused.status(), refused.code(), refused.field()),
            (422, "invalid_request", Some(field))
        );
        assert!(!refused.may_have_been_sent());
    }
    assert_eq!(script.requests().len(), before, "a refused change was sent");
    assert_eq!(script.remaining(), 0);
    assert!(told.graph_errors().is_empty());
    // Meta refusing a read or a change: its error, with its `details`.
    let unobserved = Numbers::new(&authz, &number).unwrap();
    let refusal = json!({"error": {"message": "(#100) Invalid parameter", "type": "OAuthException",
                                   "code": 100, "error_data": {"messaging_product": "whatsapp",
                                   "details": "NUMBERS-DETAILS-SENTINEL"},
                                   "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}});
    for _ in 0..3 {
        script.push_json(400, refusal.clone());
    }
    let refusals = [
        unobserved.details().await.map(drop),
        unobserved.profile().await.map(drop),
        unobserved
            .update_profile(ProfileChange {
                about: Some("Succulent specialists!".to_owned()),
                ..ProfileChange::default()
            })
            .await
            .map(drop),
    ];
    for (refused, which) in refusals.into_iter().zip(["details", "profile", "change"]) {
        let refused = refused.unwrap_err();
        assert_eq!(
            refused.graph().map(|g| (g.code, g.details.as_deref())),
            Some((100, Some("NUMBERS-DETAILS-SENTINEL"))),
            "{which}"
        );
    }
    assert_eq!(script.remaining(), 0);

    // 5. Disconnecting: only once Meta unsubscribed the app.
    let unsubscribe_path = format!("/v25.0/{WABA}/subscribed_apps");
    let disconnect = Waba::new(&authz, &waba).unwrap().observed_by(&told);
    script.push_json(
        500,
        json!({"error": {"message": "An unexpected error occurred. Please retry your request",
                         "type": "GraphMethodException", "code": 2, "is_transient": true,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let failed = disconnect.disconnect().await.unwrap_err();
    assert_eq!(
        (failed.status(), failed.code()),
        (502, "service_unavailable")
    );
    // Meta refusing it (support/error-codes: 200, a permission), told.
    script.push_json(
        403,
        json!({"error": {"message": "(#200) Permissions error", "type": "OAuthException",
                         "code": 200, "error_data": {"messaging_product": "whatsapp",
                         "details": "DISCONNECT-DETAILS-SENTINEL"},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = disconnect.disconnect().await.unwrap_err();
    assert_eq!(
        refused.graph().map(|g| (g.code, g.details.as_deref())),
        Some((200, Some("DISCONNECT-DETAILS-SENTINEL"))),
        "Meta's details"
    );
    assert_eq!(told.graph_errors(), [refused.code()]);
    assert!(vault.get(&waba_id).await.unwrap().is_some(), "token kept");
    assert!(records.waba(&waba_id).await.unwrap().is_some(), "WABA kept");
    assert!(records.number(&pn).await.unwrap().is_some(), "number kept");
    script.push_json(200, json!({"success": true}));
    disconnect.disconnect().await.unwrap();
    let requests = script.requests();
    let [.., failed, refused, unsubscribed] = &requests[..] else {
        panic!("{requests:?}")
    };
    for request in [failed, refused, unsubscribed] {
        assert_eq!(
            (request.method.as_str(), request.path()),
            ("DELETE", unsubscribe_path.as_str())
        );
        assert_eq!(request.bearer(), Some(TOKEN));
    }
    assert!(vault.get(&waba_id).await.unwrap().is_none(), "token kept");
    assert!(records.waba(&waba_id).await.unwrap().is_none(), "WABA kept");
    assert!(records.number(&pn).await.unwrap().is_none(), "number kept");
    // The same capability again: its WABA is gone, nothing is asked.
    let sent = script.requests().len();
    let moved = disconnect.disconnect().await.unwrap_err();
    assert_eq!((moved.status(), moved.code()), (503, "storage_unavailable"));
    assert!(moved.is_retryable());
    assert_eq!(script.requests().len(), sent, "a request was sent");
    assert_eq!(script.remaining(), 0);
    assert!(other_script.requests().is_empty());
}
