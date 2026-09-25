//! Webhook scenarios run on memory (`webhooks.rs`) and on Postgres
//! (`live_postgres.rs`).

use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use serde_json::{Value, json};

use super::meta::{bytes, text};
use super::{Call, Harness};

/// Every event `tenant` polls (a fresh key with the `events` scope).
pub async fn polled(h: &Harness, tenant: &str) -> Vec<Value> {
    let key = h.tenant_key(tenant, &[Scope::Events]).await;
    let reply = h.call(Call::get("/v1/events").key(&key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    reply.json()["data"].as_array().unwrap().clone()
}

/// The message ids of `events`.
fn message_ids(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| e["data"]["message"]["id"].as_str().map(str::to_owned))
        .collect()
}

/// Fix #1 of the M1c review: a tenant deleted and created again under the
/// same id polls nothing of the deleted one's events (they were deleted
/// with it, and an old cursor is expired), and polls its own once it holds
/// a number again, numbered after the deleted ones; another tenant is
/// untouched. Decisive: the deletion reaching the outbox (the foreign key
/// on Postgres, the store's own outbox in memory).
pub async fn a_recreated_tenant_polls_nothing_from_before(h: &Harness) {
    const WABA_A: &str = "102290129340398";
    const PN_A: &str = "106540352242922";
    const WABA_B: &str = "102290129340399";
    const PN_B: &str = "106540352242923";
    h.tenant("tenant-a").await;
    h.tenant("tenant-b").await;
    h.connect("tenant-a", WABA_A, &[PN_A], "TOKEN-OF-A").await;
    h.connect("tenant-b", WABA_B, &[PN_B], "TOKEN-OF-B").await;
    for body in [
        text(WABA_A, PN_A, "wamid.OLD-A"),
        text(WABA_B, PN_B, "wamid.B"),
    ] {
        assert_eq!(h.webhook(&bytes(&body)).await.status, StatusCode::OK);
    }
    assert_eq!(message_ids(&polled(h, "tenant-a").await), ["wamid.OLD-A"]);

    // The operator deletes tenant-a (Meta unsubscribes its WABA), then
    // creates a tenant with the same id.
    let admin = h.admin_key().await;
    h.graph.push_json(200, json!({"success": true}));
    let deleted = h
        .call(Call::new(Method::DELETE, "/v1/admin/tenants/tenant-a").key(&admin))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.text);
    let created = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants")
                .key(&admin)
                .json(&json!({"id": "tenant-a", "name": "a new merchant"})),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    assert!(
        polled(h, "tenant-a").await.is_empty(),
        "the new tenant-a sees the old one's events"
    );
    // A cursor of the deleted tenant's (it had one event) is expired: its
    // events are gone. From there on, the new tenant's stream goes on.
    let key = h.tenant_key("tenant-a", &[Scope::Events]).await;
    let old_cursor = h.call(Call::get("/v1/events?after=0").key(&key)).await;
    assert_eq!(
        (old_cursor.status, old_cursor.code().as_str()),
        (StatusCode::GONE, "cursor_expired")
    );
    let from_there = h.call(Call::get("/v1/events?after=1").key(&key)).await;
    assert_eq!(from_there.status, StatusCode::OK, "{}", from_there.text);
    assert_eq!(from_there.json()["data"], json!([]));
    assert_eq!(from_there.json()["next_after"], 1);

    // Its own events, once it holds the number again.
    h.connect("tenant-a", WABA_A, &[PN_A], "TOKEN-OF-NEW-A")
        .await;
    let body = text(WABA_A, PN_A, "wamid.NEW-A");
    assert_eq!(h.webhook(&bytes(&body)).await.status, StatusCode::OK);
    let new = polled(h, "tenant-a").await;
    assert_eq!(message_ids(&new), ["wamid.NEW-A"]);
    assert_eq!(new[0]["sequence"], 2, "after the deleted tenant's");
    assert_eq!(message_ids(&polled(h, "tenant-b").await), ["wamid.B"]);
    assert_eq!(h.graph.remaining(), 0);
}
