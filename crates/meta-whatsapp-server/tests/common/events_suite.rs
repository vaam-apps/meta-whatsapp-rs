//! What every `EventStore` must do, run on memory (`tests/store.rs`) and on
//! Postgres (`tests/live_postgres.rs`).

use std::time::Duration;

use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_server::model::TenantId;
use meta_whatsapp_server::store::events::{EventQuery, NewEvent};
use meta_whatsapp_server::store::{EventStore, Store};

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

/// The WABA a row of `pn` names: one per number, so that each number can
/// be bound to its tenant on its own.
pub fn waba_of(pn: &str) -> String {
    format!("9000{pn}")
}

/// Bind `pn` (under [`waba_of`]) to `tenant`, creating the tenant first
/// when it does not exist. The Postgres outbox keeps a row's tenant only
/// while the binding the row names still holds it (and only for a tenant
/// that exists); the memory one does not look.
pub async fn bind(store: &dyn Store, tenant_id: &str, pn: &str) {
    let tenant = tenant(tenant_id);
    store.create_tenant(&tenant, "").await.unwrap();
    store
        .bind_waba(
            &tenant,
            &WabaId::new(waba_of(pn)),
            &[PhoneNumberId::new(pn)],
        )
        .await
        .unwrap();
}

/// A row for `tenant` (`None`: operator-only), about `pn` of
/// [`waba_of`]`(pn)`.
pub fn row(tenant_id: Option<&str>, event_type: &str, pn: &str, dedup: Option<&str>) -> NewEvent {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    NewEvent {
        id: format!("evt_suite_{n}_{}", super::unique()),
        dedup_key: dedup.map(str::to_owned),
        tenant: tenant_id.map(tenant),
        phone_number_id: Some(pn.to_owned()),
        waba_id: Some(waba_of(pn)),
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
        .insert(&row(Some("suite-a"), "message_received", "11", None))
        .await
        .unwrap()
        .unwrap();
    let operator = store
        .insert(&row(None, "unknown", "11", None))
        .await
        .unwrap()
        .unwrap();
    let other = store
        .insert(&row(Some("suite-b"), "message_received", "21", None))
        .await
        .unwrap()
        .unwrap();
    let written = row(Some("suite-a"), "status_updated", "12", None);
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
    assert_eq!(got.phone_number_id.as_deref(), Some("12"));
    assert_eq!(got.waba_id, Some(waba_of("12")));
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
        .insert(&row(Some("suite-d"), "message_received", "41", Some(&key)))
        .await
        .unwrap();
    assert!(first.is_some());
    let again = store
        .insert(&row(Some("suite-d"), "message_received", "41", Some(&key)))
        .await
        .unwrap();
    assert_eq!(again, None);
    // Rows without a key are never deduplicated (the sink gives every
    // event one: `crate::events`).
    for _ in 0..2 {
        assert!(
            store
                .insert(&row(Some("suite-d"), "error_reported", "41", None))
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
        .insert(&row(Some("suite-f"), "message_received", "61", None))
        .await
        .unwrap()
        .unwrap();
    store
        .insert(&row(Some("suite-f"), "status_updated", "61", None))
        .await
        .unwrap();
    store
        .insert(&row(Some("suite-f"), "message_received", "62", None))
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
    assert_eq!(pns, ["61", "62"]);
    q.phone_number_id = Some("62".to_owned());
    let page = store.page(&q).await.unwrap();
    assert_eq!(page.events.len(), 1);
    q.types = Some(vec![
        "status_updated".to_owned(),
        "message_received".to_owned(),
    ]);
    q.phone_number_id = Some("61".to_owned());
    let page = store.page(&q).await.unwrap();
    assert_eq!(page.events.len(), 2);
}

/// Purge past `older_than`, waiting while another replica holds the
/// housekeeping lock (`None`): on Postgres that lock is the database's,
/// shared by every test running on it.
pub async fn purge_now(store: &dyn EventStore, older_than: Duration) -> u64 {
    for _ in 0..500 {
        if let Some(purged) = store.purge(older_than).await.unwrap() {
            return purged;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("another replica held the housekeeping lock for 5 s");
}

/// A purge removes a prefix and records it: `purged_through` moves, the
/// high water stays, later inserts go on after it.
pub async fn purge(store: &dyn EventStore) {
    let last = store
        .insert(&row(Some("suite-p"), "message_received", "71", None))
        .await
        .unwrap()
        .unwrap();
    // Nothing is older than a day.
    assert_eq!(purge_now(store, Duration::from_hours(24)).await, 0);
    let before = store.page(&query("suite-p", None, 10)).await.unwrap();
    assert!(before.events.iter().any(|e| e.sequence == last));
    tokio::time::sleep(Duration::from_millis(20)).await;
    // Everything received before now.
    let purged = purge_now(store, Duration::ZERO).await;
    assert!(purged >= 1, "{purged}");
    let after = store.page(&query("suite-p", None, 10)).await.unwrap();
    assert!(after.events.is_empty());
    assert!(after.purged_through >= last);
    assert!(after.high_water >= last);
    let next = store
        .insert(&row(Some("suite-p"), "message_received", "71", None))
        .await
        .unwrap()
        .unwrap();
    assert!(next > after.purged_through);
    let page = store.page(&query("suite-p", None, 10)).await.unwrap();
    let sequences: Vec<i64> = page.events.iter().map(|e| e.sequence).collect();
    assert_eq!(sequences, [next]);
}

/// A deleted tenant's rows are no tenant's: a tenant created later with
/// the same id polls none of them, and polls its own. Deleting the tenant
/// in `tenants` does it (the store of the same database).
pub async fn tenant_deleted(store: &dyn EventStore, tenants: &dyn Store) {
    let old = store
        .insert(&row(Some("suite-t"), "message_received", "81", None))
        .await
        .unwrap()
        .unwrap();
    let id = tenant("suite-t");
    tenants
        .unbind_waba(&WabaId::new(waba_of("81")))
        .await
        .unwrap();
    assert_eq!(
        tenants.delete_tenant(&id).await.unwrap(),
        meta_whatsapp_server::model::DeleteTenantOutcome::Deleted
    );
    bind(tenants, "suite-t", "81").await;
    let page = store
        .page(&query("suite-t", Some(old - 1), 10))
        .await
        .unwrap();
    assert!(page.events.is_empty(), "{:?}", page.events);
    let new = store
        .insert(&row(Some("suite-t"), "message_received", "81", None))
        .await
        .unwrap()
        .unwrap();
    let page = store
        .page(&query("suite-t", Some(old - 1), 10))
        .await
        .unwrap();
    let sequences: Vec<i64> = page.events.iter().map(|e| e.sequence).collect();
    assert_eq!(sequences, [new]);
}

/// Everything above, in an order where the purge comes last. `tenants` is
/// the service's store on the same backend (the tenants and bindings the
/// rows name).
pub async fn run(store: &dyn EventStore, tenants: &dyn Store) {
    for (tenant_id, pn) in [
        ("suite-a", "11"),
        ("suite-a", "12"),
        ("suite-b", "21"),
        ("suite-d", "41"),
        ("suite-f", "61"),
        ("suite-f", "62"),
        ("suite-p", "71"),
        ("suite-t", "81"),
    ] {
        bind(tenants, tenant_id, pn).await;
    }
    insert_and_page(store).await;
    dedup(store).await;
    filters(store).await;
    tenant_deleted(store, tenants).await;
    purge(store).await;
}
