//! Erasing a customer on a number over HTTP (scope `erasure`; roadmap
//! M2f; the core's `erasure` module, whose cases with no HTTP layer are
//! `conformance::inbox`'s): the decisive scenario and what an erasure
//! does not reach on memory (on Postgres, with a sweep of every table:
//! `live_postgres.rs`); each refusal before anything is read; a platform
//! key. What the journal and the audit event hold is `erasure_logs.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::erasure::{
    OTHER, conversations_of, erase, erased, events_of, merchant, naming, the_customer_writes,
};
use common::meta::{EXAMPLE_PN, EXAMPLE_WA_ID};
use common::{Call, Harness, test_settings};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{AllowedTenants, Scope, TenantId};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

/// The decisive scenario on memory.
#[tokio::test]
async fn an_erasure_by_bsuid_leaves_nothing_of_the_customer() {
    common::erasure::an_erasure_by_bsuid_leaves_nothing_of_the_customer(&Harness::new()).await;
}

/// Decisive: another tenant's key cannot erase on the number; an erasure
/// stays on its number and its tenant.
#[tokio::test]
async fn an_erasure_stays_on_its_number_and_tenant() {
    common::erasure::an_erasure_stays_on_its_number_and_tenant(&Harness::new()).await;
}

/// Decisive (design D90, narrow by default): on a number moved from
/// another tenant, the erasure is `409 erasure_crosses_binding` and the
/// previous holder's hidden records stay.
#[tokio::test]
async fn an_erasure_on_a_moved_number_erases_nothing() {
    common::erasure::an_erasure_on_a_moved_number_erases_nothing(&Harness::new()).await;
}

/// Every refusal comes before anything is read or erased, and quotes no
/// contact: a body naming another number (`confirm`), no contact, 17, a
/// blank one, one with a control character, an unknown field; a
/// deployment without an erasure key (`409 erasure_not_configured`).
/// Decisive: each check (the erasure would go through otherwise).
#[tokio::test]
async fn every_refusal_comes_before_anything_is_erased() {
    let h = Harness::new();
    let (reader, eraser) = merchant(&h).await;
    the_customer_writes(&h).await;
    let pn = EXAMPLE_PN;
    let many: Vec<String> = (0..17).map(|i| format!("1650555{i:04}")).collect();
    let cases: Vec<(Value, &str)> = vec![
        (
            json!({"confirm": "106540352242999", "contacts": [EXAMPLE_WA_ID]}),
            "confirm",
        ),
        (json!({"contacts": [EXAMPLE_WA_ID]}), "body"),
        (json!({"confirm": pn, "contacts": []}), "contacts"),
        (json!({"confirm": pn, "contacts": many}), "contacts"),
        (json!({"confirm": pn, "contacts": ["  "]}), "contacts"),
        (
            json!({"confirm": pn, "contacts": [format!("{EXAMPLE_WA_ID}\n")]}),
            "contacts",
        ),
        (
            json!({"confirm": pn, "contacts": [EXAMPLE_WA_ID], "all_numbers": true}),
            "body",
        ),
    ];
    for (body, field) in cases {
        let reply = h
            .call(
                Call::new(Method::POST, format!("/v1/numbers/{pn}/erasures"))
                    .key(&eraser)
                    .json(&body),
            )
            .await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{body}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
        assert!(!reply.text.contains(EXAMPLE_WA_ID), "{}", reply.text);
    }
    assert_eq!(conversations_of(&h, &reader, pn).await.len(), 3);
    assert_eq!(naming(&events_of(&h, &reader, pn).await), 3);

    // Without an erasure key: off.
    let off = Harness::with_settings(meta_whatsapp_server::state::Settings {
        erasure: None,
        ..test_settings()
    });
    let (reader, eraser) = merchant(&off).await;
    the_customer_writes(&off).await;
    let reply = off.call(erase(&eraser, pn, pn, &[EXAMPLE_WA_ID])).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "erasure_not_configured"),
        "{}",
        reply.text
    );
    assert_eq!(conversations_of(&off, &reader, pn).await.len(), 3);
}

/// A platform key acts as the tenant it names, within its set: allowed
/// merchant-42 and the scope, it erases on merchant-42's number; allowed
/// only merchant-43, the number does not exist for it (`404`).
#[tokio::test]
async fn a_platform_key_erases_for_the_tenants_it_may_name_only() {
    let h = Harness::new();
    let (reader, _) = merchant(&h).await;
    the_customer_writes(&h).await;
    h.tenant("merchant-43").await;
    let pn = EXAMPLE_PN;
    let elsewhere = h
        .platform_key(
            AllowedTenants::Only(vec![TenantId::parse("merchant-43").unwrap()]),
            &[Scope::Erasure],
        )
        .await;
    let refused = h
        .call(erase(&elsewhere, pn, pn, &[EXAMPLE_WA_ID]).tenant("merchant-43"))
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.text);
    let not_allowed = h
        .call(erase(&elsewhere, pn, pn, &[EXAMPLE_WA_ID]).tenant("merchant-42"))
        .await;
    assert_eq!(
        not_allowed.status,
        StatusCode::FORBIDDEN,
        "{}",
        not_allowed.text
    );
    assert_eq!(conversations_of(&h, &reader, pn).await.len(), 3);
    let platform = h.platform_key(AllowedTenants::All, &[Scope::Erasure]).await;
    let reply = h
        .call(erase(&platform, pn, pn, &[EXAMPLE_WA_ID]).tenant("merchant-42"))
        .await;
    erased(&reply, pn);
    assert_eq!(conversations_of(&h, &reader, pn).await, [OTHER]);
}
