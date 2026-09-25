//! `GET /v1/events` (docs/design/server.md, section 4.2): a tenant polls
//! its events after a sequence; `410 cursor_expired` past retention; only
//! the caller's tenant's events, behind the authorization order.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::time::Duration;

use common::events_suite::row;
use common::{Call, Harness, Reply};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_server::model::{AllowedTenants, Scope, TenantId};
use meta_whatsapp_server::store::events::NewEvent;
use serde_json::Value;

const A: &str = "tenant-a";
const B: &str = "tenant-b";

async fn harness() -> Harness {
    let h = Harness::new();
    h.tenant(A).await;
    h.tenant(B).await;
    h
}

/// Insert `event` straight into the outbox; its sequence.
async fn insert(h: &Harness, event: &NewEvent) -> i64 {
    use meta_whatsapp_server::store::EventStore as _;
    h.outbox.insert(event).await.unwrap().unwrap()
}

async fn poll(h: &Harness, key: &str, query: &str) -> Reply {
    h.call(Call::get(format!("/v1/events{query}")).key(key))
        .await
}

fn sequences(reply: &Reply) -> Vec<i64> {
    reply.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["sequence"].as_i64().unwrap())
        .collect()
}

fn next_after(reply: &Reply) -> i64 {
    reply.json()["next_after"].as_i64().unwrap()
}

/// Following `next_after` page by page returns every event of the tenant
/// once, in order, and ends at the outbox's newest sequence (other
/// tenants' included), so the next poll starts there. Decisive: the
/// `after` cursor.
#[tokio::test]
async fn following_next_after_returns_every_event_once_in_order() {
    let h = harness().await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    let mut mine = Vec::new();
    for i in 0..7 {
        mine.push(insert(&h, &row(Some(A), "message_received", "1", None)).await);
        if i % 2 == 0 {
            insert(&h, &row(Some(B), "message_received", "1", None)).await;
            insert(&h, &row(None, "unknown", "1", None)).await;
        }
    }
    let newest = insert(&h, &row(Some(B), "status_updated", "1", None)).await;
    let mut seen = Vec::new();
    let mut cursor: Option<i64> = None;
    let mut pages = 0;
    loop {
        let query = match cursor {
            None => "?limit=3".to_owned(),
            Some(after) => format!("?limit=3&after={after}"),
        };
        let reply = poll(&h, &key, &query).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        let page = sequences(&reply);
        pages += 1;
        let next = next_after(&reply);
        if page.len() == 3 {
            assert_eq!(
                next,
                *page.last().unwrap(),
                "a full page continues after its last"
            );
        }
        seen.extend(page.iter().copied());
        if page.is_empty() {
            assert_eq!(next, newest, "caught up: the newest sequence");
            break;
        }
        cursor = Some(next);
        assert!(pages < 10);
    }
    assert_eq!(seen, mine, "every event once, in order");
    // Caught up, the cursor stays; a new event comes next.
    let reply = poll(&h, &key, &format!("?after={newest}")).await;
    assert_eq!((sequences(&reply), next_after(&reply)), (vec![], newest));
    let later = insert(&h, &row(Some(A), "status_updated", "1", None)).await;
    let reply = poll(&h, &key, &format!("?after={newest}")).await;
    assert_eq!(sequences(&reply), [later]);
    assert_eq!(next_after(&reply), later);
}

/// Only the caller's tenant's events: a tenant key sees its own, a platform
/// key the named tenant's, never another's and never an operator-only row.
/// Decisive: the tenant filter.
#[tokio::test]
async fn a_tenant_polls_only_its_own_events() {
    let h = harness().await;
    let a = insert(&h, &row(Some(A), "message_received", "1", None)).await;
    let b = insert(&h, &row(Some(B), "message_received", "2", None)).await;
    insert(&h, &row(None, "unknown", "1", None)).await;
    let a_key = h.tenant_key(A, &[Scope::Events]).await;
    let b_key = h.tenant_key(B, &[Scope::Events]).await;
    let platform = h.platform_key(AllowedTenants::All, &[Scope::Events]).await;
    assert_eq!(sequences(&poll(&h, &a_key, "").await), [a]);
    assert_eq!(sequences(&poll(&h, &b_key, "").await), [b]);
    let as_a = h
        .call(Call::get("/v1/events").key(&platform).tenant(A))
        .await;
    assert_eq!(sequences(&as_a), [a]);
    let as_b = h
        .call(Call::get("/v1/events").key(&platform).tenant(B))
        .await;
    assert_eq!(sequences(&as_b), [b]);
    // Filtering by the other tenant's number finds nothing of theirs.
    assert!(sequences(&poll(&h, &a_key, "?phone_number_id=2").await).is_empty());
    let envelope = &poll(&h, &a_key, "").await.json()["data"][0];
    assert_eq!(envelope["tenant_id"], A);
}

/// The authorization order: no key `401`, a key without `events` `403
/// forbidden`, a suspended tenant `403 tenant_suspended`, a platform key
/// naming a tenant outside its set `403 forbidden`.
#[tokio::test]
async fn polling_follows_the_authorization_order() {
    let h = harness().await;
    insert(&h, &row(Some(A), "message_received", "1", None)).await;
    let reply = h.call(Call::get("/v1/events")).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::UNAUTHORIZED, "unauthenticated")
    );
    let numbers_only = h.tenant_key(A, &[Scope::Numbers]).await;
    let reply = poll(&h, &numbers_only, "").await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden")
    );
    let only_b = h
        .platform_key(
            AllowedTenants::Only(vec![TenantId::parse(B).unwrap()]),
            &[Scope::Events],
        )
        .await;
    let reply = h.call(Call::get("/v1/events").key(&only_b).tenant(A)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden")
    );
    let a_key = h.tenant_key(A, &[Scope::Events]).await;
    h.store
        .update_tenant(
            &TenantId::parse(A).unwrap(),
            None,
            Some(meta_whatsapp_server::model::TenantStatus::Suspended),
        )
        .await
        .unwrap();
    let reply = poll(&h, &a_key, "").await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::FORBIDDEN, "tenant_suspended")
    );
}

/// `types` and `phone_number_id` narrow the page; `types` may be repeated.
#[tokio::test]
async fn types_and_phone_number_narrow_the_page() {
    let h = harness().await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    let m1 = insert(&h, &row(Some(A), "message_received", "1", None)).await;
    let s1 = insert(&h, &row(Some(A), "status_updated", "1", None)).await;
    let m2 = insert(&h, &row(Some(A), "message_received", "2", None)).await;
    let t = insert(&h, &row(Some(A), "template_status_updated", "2", None)).await;
    let only = |q: &'static str| {
        let h = &h;
        let key = key.clone();
        async move { sequences(&poll(h, &key, q).await) }
    };
    assert_eq!(only("?types=message_received").await, [m1, m2]);
    assert_eq!(
        only("?types=message_received,status_updated").await,
        [m1, s1, m2]
    );
    assert_eq!(
        only("?types=status_updated&types=template_status_updated").await,
        [s1, t]
    );
    assert_eq!(only("?phone_number_id=2").await, [m2, t]);
    assert_eq!(only("?phone_number_id=1&types=status_updated").await, [s1]);
    // A filtered page that finds nothing still moves the cursor to the
    // newest sequence.
    let reply = poll(&h, &key, "?types=call_updated").await;
    assert_eq!((sequences(&reply), next_after(&reply)), (vec![], t));
}

/// Past retention: once housekeeping purged events after a cursor, that
/// cursor is `410 cursor_expired`; polling from the purge on (or without
/// a cursor) works. Decisive: the retention check.
#[tokio::test]
async fn a_cursor_past_retention_is_410() {
    let h = harness().await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    let first = insert(&h, &row(Some(A), "message_received", "1", None)).await;
    let last = insert(&h, &row(Some(A), "message_received", "1", None)).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let purged = meta_whatsapp_server::events::purge_outbox(h.outbox.as_ref(), Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(purged, Some(2));
    for after in [0, first - 1, first, last - 1] {
        let reply = poll(&h, &key, &format!("?after={after}")).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::GONE, "cursor_expired"),
            "after={after}"
        );
    }
    // From where the purge ended, and without a cursor: fine.
    for query in [format!("?after={last}"), String::new()] {
        let reply = poll(&h, &key, &query).await;
        assert_eq!(reply.status, StatusCode::OK, "{query}: {}", reply.text);
        assert_eq!((sequences(&reply), next_after(&reply)), (vec![], last));
    }
    let next = insert(&h, &row(Some(A), "message_received", "1", None)).await;
    assert_eq!(sequences(&poll(&h, &key, "").await), [next]);
}

/// A cursor this outbox never issued (past its newest sequence: a restored
/// or another database) is `422 invalid_request` on `after`, not an empty
/// page that would wait for ever.
#[tokio::test]
async fn a_cursor_never_issued_is_422() {
    let h = harness().await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    let newest = insert(&h, &row(Some(B), "message_received", "1", None)).await;
    let reply = poll(&h, &key, &format!("?after={}", newest + 1)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request")
    );
    assert_eq!(reply.json()["error"]["field"], "after");
    assert_eq!(
        poll(&h, &key, &format!("?after={newest}")).await.status,
        StatusCode::OK
    );
}

/// Bad parameters are `422` on the parameter.
#[tokio::test]
async fn bad_parameters_are_422_on_the_parameter() {
    let h = harness().await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    for (query, field) in [
        ("?after=-1", "after"),
        ("?after=soon", "after"),
        ("?types=no_such_type", "types"),
        ("?types=unknown", "types"),
        ("?phone_number_id=%2B15550783881", "phone_number_id"),
        ("?limit=0", "limit"),
        ("?limit=101", "limit"),
    ] {
        let reply = poll(&h, &key, query).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert_eq!(reply.json()["error"]["field"], field, "{query}");
    }
}

/// A page stops once its events' data passes 8 MiB (history syncs are
/// large), with at least one event; `next_after` continues after it.
#[tokio::test]
async fn a_page_stops_past_8_mib_of_data() {
    let h = harness().await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    let big = |n: usize| NewEvent {
        data: format!(
            "{{\"event\":\"history_synced\",\"pad\":\"{}\"}}",
            "x".repeat(n)
        ),
        ..row(Some(A), "history_synced", "1", None)
    };
    let three_mib = 3 * 1024 * 1024;
    let s1 = insert(&h, &big(three_mib)).await;
    let s2 = insert(&h, &big(three_mib)).await;
    let s3 = insert(&h, &big(three_mib)).await;
    let first = poll(&h, &key, "").await;
    assert_eq!(sequences(&first), [s1, s2]);
    assert_eq!(next_after(&first), s2);
    let second = poll(&h, &key, &format!("?after={s2}")).await;
    assert_eq!(sequences(&second), [s3]);
    // One event larger than the budget still comes, alone.
    let huge = insert(&h, &big(9 * 1024 * 1024)).await;
    let alone = poll(&h, &key, &format!("?after={s3}")).await;
    assert_eq!(sequences(&alone), [huge]);
    assert_eq!(next_after(&alone), huge);
}

/// The envelope (docs/design/server.md, section 4.4) has exactly the
/// documented fields.
#[tokio::test]
async fn the_envelope_has_the_documented_fields() {
    let h = harness().await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    let sequence = insert(
        &h,
        &row(Some(A), "message_received", "106540352242922", None),
    )
    .await;
    let reply = poll(&h, &key, "").await;
    let envelope = &reply.json()["data"][0];
    let mut fields: Vec<&str> = envelope
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "api_version",
            "data",
            "id",
            "phone_number_id",
            "received_at",
            "sequence",
            "tenant_id",
            "truncated",
            "type",
            "waba_id"
        ]
    );
    assert_eq!(envelope["sequence"], sequence);
    assert_eq!(envelope["type"], "message_received");
    assert_eq!(envelope["data"]["event"], "message_received");
    assert_eq!(
        envelope["data"]["text"],
        Value::String("a\u{0}b é".to_owned())
    );
    let received = envelope["received_at"].as_str().unwrap();
    assert!(received.ends_with('Z'), "{received}");
}
