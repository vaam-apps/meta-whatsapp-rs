//! Roadmap M5k on a real Postgres (the memory twin is `broadcasts.rs`): a
//! broadcast's jobs, markers, counts and shared pacer are rows of the
//! bundle's key/value table, so a replica that dies mid-broadcast is
//! replaced by another on the same database, nobody is sent twice, and the
//! rate holds across the two. Skipped unless
//! `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::broadcasts::*;
use common::{Stores, TestDb};
use meta_whatsapp_rs::core::clock::ManualClock;
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::store::migrate;
use pretty_assertions::assert_eq;
use time::OffsetDateTime;

/// A replica dies with sends in flight; another, on the same database,
/// takes the broadcast over after its lease: each person was sent at most
/// once, the ones in flight are `unknown`, the rest were sent, and no
/// second of the combined sends holds more than the number's rate.
#[tokio::test]
async fn live_postgres_a_broadcast_survives_a_replica_and_keeps_its_rate() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(10).await;
    migrate(&pool).await.unwrap();
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let controls = Controls::new(Arc::new(clock.clone()));
    let first = replica(Stores::postgres(&pool), &clock, &controls, rate(20));
    first.tenant(TENANT).await;
    first.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = first.tenant_key(TENANT, &[Scope::Broadcasts]).await;
    let reply = first.call(create(&key, PN, &broadcast(people(30)))).await;
    assert_eq!(reply.status.as_u16(), 202, "{}", reply.text);
    let id = reply.json()["id"].as_str().unwrap().to_owned();

    // The first replica sends five and dies with the next ones in flight.
    controls.hold_after(5);
    script(&first, 30);
    let workers = first.state.broadcast_workers().unwrap();
    let died = tokio::time::timeout(Duration::from_secs(2), workers[0].tick()).await;
    assert!(died.is_err(), "the replica was meant to stall");
    assert_eq!(first.graph.requests().len(), 5);
    drop(workers);

    // Past its lease, a second replica on the same database finishes.
    clock.advance(Duration::from_secs(300));
    controls.hold_after(usize::MAX);
    let second = replica(Stores::postgres(&pool), &clock, &controls, rate(20));
    script(&second, 30);
    work(&second).await;

    let mut seen: Vec<String> = bodies(&first)
        .into_iter()
        .chain(bodies(&second))
        .map(|b| b["to"].as_str().unwrap().to_owned())
        .collect();
    let all = seen.len();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), all, "a person was sent twice");
    let view = second.call(read(&key, PN, &id)).await.json();
    assert_eq!(view["state"], "completed", "{view}");
    assert_eq!(view["sent"].as_u64().unwrap() as usize, all, "{view}");
    assert_eq!(
        view["sent"].as_u64().unwrap() + view["unknown"].as_u64().unwrap(),
        30,
        "{view}"
    );
    assert!(view["unknown"].as_u64().unwrap() > 0, "{view}");
    within(&controls.times(), 20);

    // A tenant that is not the broadcast's reads nothing of it.
    second.tenant(OTHER_TENANT).await;
    second
        .connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], "EAAG-other")
        .await;
    let rival = second.tenant_key(OTHER_TENANT, &[Scope::Broadcasts]).await;
    assert_eq!(
        second
            .call(read(&rival, OTHER_PN, &id))
            .await
            .status
            .as_u16(),
        404
    );
    assert_eq!(
        second.call(read(&rival, PN, &id)).await.status.as_u16(),
        404
    );
}
