//! Roadmap M5c5 on a real Postgres: after a same-tenant attach of a second
//! WABA holding the number, the number moves to the new binding in
//! `wa_server_numbers` and the sends name the second WABA; another
//! tenant's attach of the number stays `409 waba_owned_by_another_tenant`.
//! Skipped unless `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set (the memory
//! twin is `messaging_account.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{ALL_SCOPES, Call, Harness, Stores, TestDb};
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::store::migrate;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const A: &str = "merchant-a";
const B: &str = "merchant-b";
const WABA: &str = "102290129340398";
const SECOND_WABA: &str = "102290129340399";
const PN: &str = "106540352242922";

fn accepted() -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": "+16505551234", "wa_id": "16505551234"}],
        "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
    })
}

fn send(key: &str) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{PN}/messages"))
        .key(key)
        .json(&json!({"to": {"phone": "+16505551234"}, "type": "text",
                      "text": {"body": "Your order has shipped."}}))
}

fn phone_numbers() -> Value {
    json!({"data": [{"id": PN, "display_phone_number": "+1 650-555-2222",
                     "verified_name": "John's Cake Shop", "quality_rating": "GREEN"}]})
}

fn attach(admin: &str, tenant: &str, waba: &str) -> Call {
    Call::new(Method::POST, format!("/v1/admin/tenants/{tenant}/wabas"))
        .key(admin)
        .json(&json!({"waba_id": waba, "token": "EAAG-attached-token"}))
}

#[tokio::test]
async fn live_postgres_the_latest_same_tenant_attach_is_the_account_named() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(10).await;
    migrate(&pool).await.unwrap();
    let h = Harness::with_stores_and_settings(
        Stores::postgres(&pool),
        meta_whatsapp_server::state::Settings {
            send_messaging_account_id: true,
            ..common::test_settings()
        },
    );
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, WABA, &[PN], "EAAG-first-token").await;
    let key = h.tenant_key(A, &ALL_SCOPES).await;
    let admin = h.admin_key().await;

    h.graph.push_json(200, accepted());
    assert_eq!(h.call(send(&key)).await.status, StatusCode::ACCEPTED);
    assert_eq!(
        h.graph.last_request().unwrap().json().unwrap()["messaging_account_id"],
        WABA
    );

    // The same tenant attaches a second WABA that lists the number.
    h.graph.push_json(200, phone_numbers());
    h.graph.push_json(200, json!({"success": true}));
    let attached = h.call(attach(&admin, A, SECOND_WABA)).await;
    assert_eq!(attached.status, StatusCode::CREATED, "{}", attached.text);
    let held: String = sqlx_waba_of(&pool, &PhoneNumberId::new(PN).unwrap()).await;
    assert_eq!(held, SECOND_WABA, "wa_server_numbers moved the number");

    h.graph.push_json(200, accepted());
    assert_eq!(h.call(send(&key)).await.status, StatusCode::ACCEPTED);
    assert_eq!(
        h.graph.last_request().unwrap().json().unwrap()["messaging_account_id"],
        SECOND_WABA
    );

    // Another tenant's attach of the number is refused; nothing moves.
    h.graph.push_json(200, phone_numbers());
    let refused = h.call(attach(&admin, B, "102290129340400")).await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "waba_owned_by_another_tenant"),
        "{}",
        refused.text
    );
    assert_eq!(
        sqlx_waba_of(&pool, &PhoneNumberId::new(PN).unwrap()).await,
        SECOND_WABA
    );
}

/// The WABA `wa_server_numbers` holds the number under.
async fn sqlx_waba_of(
    pool: &meta_whatsapp_rs::adapters::store::postgres::sqlx::PgPool,
    number: &PhoneNumberId,
) -> String {
    meta_whatsapp_rs::adapters::store::postgres::sqlx::query_scalar(
        "SELECT waba_id FROM wa_server_numbers WHERE phone_number_id = $1",
    )
    .bind(number.as_str())
    .fetch_one(pool)
    .await
    .unwrap()
}
