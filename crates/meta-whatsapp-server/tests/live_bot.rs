//! Roadmap M5k, slice 2, on a real Postgres (the memory twin is `bot.rs`):
//! a bot's rules, the markers that claim its answers and its cooldowns are
//! rows of the bundle's key/value table, so two replicas on one database
//! answer a message once, and a cooldown one replica started holds for the
//! other. Skipped unless `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::bot::*;
use common::{Call, Harness, Stores, TestDb};
use meta_whatsapp_rs::adapters::store::postgres::sqlx::PgPool;
use meta_whatsapp_rs::core::clock::ManualClock;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_rs::webhooks::{WebhookPayload, events};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server::store::migrate;
use meta_whatsapp_server_core::broadcasts::BroadcastSettings;
use meta_whatsapp_server_core::inbound::MessageActions;
use pretty_assertions::assert_eq;
use serde_json::json;
use time::OffsetDateTime;

/// One replica on `pool`, its clock and the key/value store's the fake one.
fn replica(pool: &PgPool, clock: &ManualClock) -> Harness {
    let stores = Stores::postgres(pool);
    let settings = Settings {
        broadcasts: BroadcastSettings {
            timer: Some(Arc::new(clock.clone())),
            ..BroadcastSettings::default()
        },
        ..common::test_settings()
    };
    Harness::with_clock(stores, settings, clock.clone(), |graph| Arc::new(graph))
}

/// Rules are rows: made through one replica, read through another; a
/// message is answered once whichever replica hears of it, the same event
/// handed to the other replica's bot is not answered again, and a cooldown
/// the first started holds for the second.
#[tokio::test]
async fn live_postgres_a_bot_answers_once_across_replicas() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(10).await;
    migrate(&pool).await.unwrap();
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let a = replica(&pool, &clock);
    let b = replica(&pool, &clock);
    a.tenant(TENANT).await;
    a.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = a.tenant_key(TENANT, &[Scope::Bot]).await;

    let mut rule = command("hello", "Hi from the bot");
    rule["cooldown_seconds"] = json!(2);
    let created = a.call(create(&key, PN, &rule)).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let id = created.json()["id"].as_str().unwrap().to_owned();
    let on = a.call(put(&key, PN, &json!({"enabled": true}))).await;
    assert_eq!(on.status, StatusCode::OK, "{}", on.text);
    // Read through the other replica.
    let seen = b.call(rules(&key, PN)).await.json();
    assert_eq!(seen["data"][0]["id"], id.as_str(), "{seen}");
    let read = b.call(Call::get(rule_path(PN, &id)).key(&key)).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.text);

    // The message reaches replica A, and its event is handed to B's bot too.
    script(&a, 2);
    script(&b, 2);
    let body = inbound("wamid.LIVE1", "/hello");
    deliver(&a, &body).await;
    assert_eq!(a.graph.requests().len(), 1);
    let event = events(&WebhookPayload::from_slice(&body).unwrap())
        .into_iter()
        .next()
        .unwrap();
    let tenant = meta_whatsapp_server::model::TenantId::parse(TENANT).unwrap();
    b.grant.bot_replies().on_message(&tenant, &event).await.unwrap();
    assert_eq!(b.graph.requests().len(), 0, "answered by both replicas");

    // The cooldown A started holds for B; after it, B answers.
    deliver(&b, &inbound("wamid.LIVE2", "/hello")).await;
    assert_eq!(b.graph.requests().len(), 0, "the cooldown was not shared");
    // The database's clock ends it (the fake one does not reach a row).
    tokio::time::sleep(Duration::from_millis(2500)).await;
    deliver(&b, &inbound("wamid.LIVE3", "/hello")).await;
    assert_eq!(b.graph.requests().len(), 1);

    // Another tenant reads none of it, and deletes are rows too.
    a.tenant(OTHER_TENANT).await;
    a.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], "EAAG-other")
        .await;
    let rival = a.tenant_key(OTHER_TENANT, &[Scope::Bot]).await;
    assert_eq!(
        b.call(Call::get(rule_path(PN, &id)).key(&rival)).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        b.call(Call::get(rule_path(OTHER_PN, &id)).key(&rival))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let gone = b
        .call(Call::new(Method::DELETE, rule_path(PN, &id)).key(&key))
        .await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT, "{}", gone.text);
    assert_eq!(
        a.call(Call::get(rule_path(PN, &id)).key(&key)).await.status,
        StatusCode::NOT_FOUND
    );
}
