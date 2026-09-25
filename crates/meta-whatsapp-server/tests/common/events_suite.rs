//! What every `EventStore` must do, run on memory (`tests/store.rs`) and on
//! Postgres (`tests/live_postgres.rs`).

use std::time::Duration;

use meta_whatsapp_server::model::TenantId;
use meta_whatsapp_server::store::EventStore;
use meta_whatsapp_server::store::events::{EventQuery, NewEvent};

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

/// A row for `tenant` (`None`: operator-only).
pub fn row(tenant_id: Option<&str>, event_type: &str, pn: &str, dedup: Option<&str>) -> NewEvent {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    NewEvent {
        id: format!("evt_suite_{n}_{}", super::unique()),
        dedup_key: dedup.map(str::to_owned),
        tenant: tenant_id.map(tenant),
        phone_number_id: Some(pn.to_owned()),
        waba_id: Some("102290129340398".to_owned()),
        event_type: event_type.to_owned(),
        // U+0000 in a message text: `json` keeps it, `jsonb` would refuse
        // it.
        data: format!("{{\"event\":\"{event_type}\",\"n\":{n},\"text\":\"a\\u0000b é\"}}"),
    }
}

fn query(tenant_id: &str, after: Option<i64>, limit: usize) -> EventQuery {
    EventQuery {
        tenant: tenant(tenant_id),
        after,
        types: None,
        phone_number_id: None,
        limit,
    }
}

/// Inserts get increasing sequences; a page holds only its tenant's rows,
/// in order, one more than the limit when more follow; data comes back as
/// written.
pub async fn insert_and_page(store: &dyn EventStore) {
    let first = store
        .insert(&row(Some("suite-a"), "message_received", "1", None))
        .await
        .unwrap()
        .unwrap();
    let operator = store
        .insert(&row(None, "unknown", "1", None))
        .await
        .unwrap()
        .unwrap();
    let other = store
        .insert(&row(Some("suite-b"), "message_received", "1", None))
        .await
        .unwrap()
        .unwrap();
    let written = row(Some("suite-a"), "status_updated", "2", None);
    let second = store.insert(&written).await.unwrap().unwrap();
    assert!(first < operator && operator < other && other < second);

    let page = store
        .page(&query("suite-a", Some(first - 1), 10))
        .await
        .unwrap();
    let sequences: Vec<i64> = page.events.iter().map(|e| e.sequence).collect();
    assert_eq!(sequences, [first, second], "only suite-a's rows, in order");
    let got = &page.events[1];
    assert_eq!(got.id, written.id);
    assert_eq!(got.data, written.data, "data as written, U+0000 included");
    assert_eq!(got.event_type, "status_updated");
    assert_eq!(got.phone_number_id.as_deref(), Some("2"));
    assert_eq!(got.waba_id.as_deref(), Some("102290129340398"));
    assert_eq!(got.tenant, Some(tenant("suite-a")));
    assert!(page.high_water >= second);

    // One more than the limit when more follow.
    let page = store
        .page(&query("suite-a", Some(first - 1), 1))
        .await
        .unwrap();
    assert_eq!(page.events.len(), 2);
    let page = store.page(&query("suite-a", Some(first), 1)).await.unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].sequence, second);
    let page = store
        .page(&query("suite-a", Some(second), 10))
        .await
        .unwrap();
    assert!(page.events.is_empty());
    // No tenant owns an operator-only row: no query returns it.
    for t in ["suite-a", "suite-b"] {
        let page = store.page(&query(t, Some(first - 1), 10)).await.unwrap();
        assert!(page.events.iter().all(|e| e.sequence != operator), "{t}");
    }
}

/// A dedup key is stored once: the second insert writes nothing.
pub async fn dedup(store: &dyn EventStore) {
    let key = format!("key-{}", super::unique());
    let first = store
        .insert(&row(Some("suite-d"), "message_received", "1", Some(&key)))
        .await
        .unwrap();
    assert!(first.is_some());
    let again = store
        .insert(&row(Some("suite-d"), "message_received", "1", Some(&key)))
        .await
        .unwrap();
    assert_eq!(again, None);
    // Keyless rows are never deduplicated.
    for _ in 0..2 {
        assert!(
            store
                .insert(&row(Some("suite-d"), "error_reported", "1", None))
                .await
                .unwrap()
                .is_some()
        );
    }
    let page = store
        .page(&query("suite-d", Some(first.unwrap() - 1), 10))
        .await
        .unwrap();
    assert_eq!(page.events.len(), 3);
}

/// `types` and `phone_number_id` narrow a page.
pub async fn filters(store: &dyn EventStore) {
    let start = store
        .insert(&row(Some("suite-f"), "message_received", "1", None))
        .await
        .unwrap()
        .unwrap();
    store
        .insert(&row(Some("suite-f"), "status_updated", "1", None))
        .await
        .unwrap();
    store
        .insert(&row(Some("suite-f"), "message_received", "2", None))
        .await
        .unwrap();
    let mut q = query("suite-f", Some(start - 1), 10);
    q.types = Some(vec!["message_received".to_owned()]);
    let page = store.page(&q).await.unwrap();
    let pns: Vec<&str> = page
        .events
        .iter()
        .map(|e| e.phone_number_id.as_deref().unwrap())
        .collect();
    assert_eq!(pns, ["1", "2"]);
    q.phone_number_id = Some("2".to_owned());
    let page = store.page(&q).await.unwrap();
    assert_eq!(page.events.len(), 1);
    q.types = Some(vec![
        "status_updated".to_owned(),
        "message_received".to_owned(),
    ]);
    q.phone_number_id = Some("1".to_owned());
    let page = store.page(&q).await.unwrap();
    assert_eq!(page.events.len(), 2);
}

/// A purge removes a prefix and records it: `purged_through` moves, the
/// high water stays, later inserts go on after it.
pub async fn purge(store: &dyn EventStore) {
    let last = store
        .insert(&row(Some("suite-p"), "message_received", "1", None))
        .await
        .unwrap()
        .unwrap();
    // Nothing is older than a day.
    assert_eq!(
        store.purge(Duration::from_hours(24)).await.unwrap(),
        Some(0)
    );
    let before = store.page(&query("suite-p", None, 10)).await.unwrap();
    assert!(before.events.iter().any(|e| e.sequence == last));
    tokio::time::sleep(Duration::from_millis(20)).await;
    // Everything received before now.
    let purged = store.purge(Duration::ZERO).await.unwrap().unwrap();
    assert!(purged >= 1, "{purged}");
    let after = store.page(&query("suite-p", None, 10)).await.unwrap();
    assert!(after.events.is_empty());
    assert!(after.purged_through >= last);
    assert!(after.high_water >= last);
    let next = store
        .insert(&row(Some("suite-p"), "message_received", "1", None))
        .await
        .unwrap()
        .unwrap();
    assert!(next > after.purged_through);
    let page = store.page(&query("suite-p", None, 10)).await.unwrap();
    let sequences: Vec<i64> = page.events.iter().map(|e| e.sequence).collect();
    assert_eq!(sequences, [next]);
}

/// Everything above, in an order where the purge comes last.
pub async fn run(store: &dyn EventStore) {
    insert_and_page(store).await;
    dedup(store).await;
    filters(store).await;
    purge(store).await;
}
