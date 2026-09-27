//! What every `EventStore` must do, run on memory (`tests/store.rs`) and on
//! Postgres (`tests/live_postgres.rs`).

use std::time::Duration;

use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_server::model::TenantId;
use meta_whatsapp_server::store::events::{
    DedupWindow, EventQuery, GuardedBinding, NewEvent, RouteGuard,
};
use meta_whatsapp_server::store::{Outbox as EventStore, Store};

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

/// The WABA a row of `pn` names: one per number, so that each number can
/// be bound to its tenant on its own.
pub fn waba_of(pn: &str) -> String {
    format!("9000{pn}")
}

/// Bind `pn` (under [`waba_of`]) to `tenant`, creating the tenant first
/// when it does not exist. Every outbox keeps a row's tenant only while
/// the binding the row's guard names still holds it (and only for a tenant
/// that exists): [`row`] guards a tenant's row by its number.
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
/// [`waba_of`]`(pn)`, guarded by that number's binding ([`bind`]), undated.
pub fn row(tenant_id: Option<&str>, event_type: &str, pn: &str, dedup: Option<&str>) -> NewEvent {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    NewEvent {
        id: format!("evt_suite_{n}_{}", super::unique()),
        dedup_key: dedup.map(str::to_owned),
        dedup_window: None,
        tenant: tenant_id.map(tenant),
        route_guard: tenant_id.map(|_| RouteGuard {
            binding: GuardedBinding::Number {
                phone_number_id: PhoneNumberId::new(pn),
                waba_id: WabaId::new(waba_of(pn)),
            },
            not_after: None,
        }),
        phone_number_id: Some(PhoneNumberId::new(pn)),
        waba_id: Some(WabaId::new(waba_of(pn))),
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
        max_bytes: 8 * 1024 * 1024,
    }
}

/// Each tenant has its own stream: its inserts get 1, 2, … whatever other
/// tenants and operator-only rows get; a page holds only its tenant's rows,
/// in order, at most the limit, `more` when more follow; data comes back as
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
    assert_eq!((first, second), (1, 2), "suite-a's own stream");
    assert_eq!(other, 1, "suite-b's own stream");
    assert!(operator >= 1, "the operator-only rows' own stream");

    let page = store.page(&query("suite-a", None, 10)).await.unwrap();
    let sequences: Vec<i64> = page.events.iter().map(|e| e.sequence).collect();
    assert_eq!(sequences, [1, 2], "only suite-a's rows, in order");
    assert!(!page.more);
    assert_eq!((page.purged_through, page.high_water), (0, 2));
    let got = &page.events[1];
    assert_eq!(got.id, written.id);
    assert_eq!(got.data, written.data, "data as written, U+0000 included");
    assert_eq!(got.event_type, "status_updated");
    assert_eq!(got.phone_number_id, Some(PhoneNumberId::new("12")));
    assert_eq!(got.waba_id, Some(WabaId::new(waba_of("12"))));
    assert_eq!(got.tenant, Some(tenant("suite-a")));
    // Another tenant's inserts move nothing of suite-a's.
    store
        .insert(&row(Some("suite-b"), "message_received", "21", None))
        .await
        .unwrap()
        .unwrap();
    let page = store.page(&query("suite-a", Some(2), 10)).await.unwrap();
    assert!(page.events.is_empty());
    assert_eq!(page.high_water, 2);
    let b = store.page(&query("suite-b", None, 10)).await.unwrap();
    assert_eq!(
        b.events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        [1, 2]
    );

    // At most the limit, and `more` when more follow.
    let page = store.page(&query("suite-a", None, 1)).await.unwrap();
    assert_eq!(page.events.len(), 1);
    assert!(page.more);
    let page = store.page(&query("suite-a", Some(1), 1)).await.unwrap();
    assert_eq!(page.events[0].sequence, 2);
    assert!(!page.more);
    // No tenant owns an operator-only row: no query returns it.
    for t in ["suite-a", "suite-b"] {
        let page = store.page(&query(t, None, 10)).await.unwrap();
        assert!(page.events.iter().all(|e| e.event_type != "unknown"), "{t}");
    }
    // A tenant with no events: an empty stream.
    let page = store.page(&query("suite-none", None, 10)).await.unwrap();
    assert!(page.events.is_empty() && !page.more);
    assert_eq!((page.purged_through, page.high_water), (0, 0));
}

/// The store cuts a page to its byte budget: the events whose data fits,
/// always the first, `more` after the cut.
pub async fn page_budget(store: &dyn EventStore) {
    let sized = |len: usize| NewEvent {
        data: format!("{{\"pad\":\"{}\"}}", "x".repeat(len - 10)),
        ..row(Some("suite-s"), "history_synced", "91", None)
    };
    for len in [400, 400, 300, 500] {
        store.insert(&sized(len)).await.unwrap().unwrap();
    }
    let budget = |max_bytes: usize, after: Option<i64>| EventQuery {
        max_bytes,
        ..query("suite-s", after, 100)
    };
    let cut = |page: &meta_whatsapp_server::store::events::EventPage| {
        (
            page.events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            page.more,
        )
    };
    // 400 + 400 = 800 fits exactly; the 300 would pass it.
    let page = store.page(&budget(800, None)).await.unwrap();
    assert_eq!(cut(&page), (vec![1, 2], true));
    assert_eq!(page.events[1].data.len(), 400);
    let page = store.page(&budget(799, None)).await.unwrap();
    assert_eq!(cut(&page), (vec![1], true));
    // The first event always comes, whatever its size.
    let page = store.page(&budget(10, Some(3))).await.unwrap();
    assert_eq!(cut(&page), (vec![4], false));
    let page = store.page(&budget(usize::MAX, None)).await.unwrap();
    assert_eq!(cut(&page), (vec![1, 2, 3, 4], false));
}

/// A dedup key is stored once: the second insert writes nothing and draws
/// no sequence.
pub async fn dedup(store: &dyn EventStore) {
    let key = format!("key-{}", super::unique());
    let first = store
        .insert(&row(Some("suite-d"), "message_received", "41", Some(&key)))
        .await
        .unwrap();
    assert_eq!(first, Some(1));
    let again = store
        .insert(&row(Some("suite-d"), "message_received", "41", Some(&key)))
        .await
        .unwrap();
    assert_eq!(again, None);
    // Rows without a key are never deduplicated (the sink gives every
    // event one: `crate::events`).
    for expected in [2, 3] {
        assert_eq!(
            store
                .insert(&row(Some("suite-d"), "error_reported", "41", None))
                .await
                .unwrap(),
            Some(expected)
        );
    }
    let page = store.page(&query("suite-d", None, 10)).await.unwrap();
    assert_eq!(page.events.len(), 3);
}

/// A keyless event's row holds its dedup key only until its window ends
/// (`DedupWindow`, on the caller's clock, never the store's): the same key
/// before then is a redelivery; at or after it, a new occurrence, recorded
/// under its own id while the earlier row keeps its sequence and id. A
/// row with no window (a library key) holds its key for good. Decisive:
/// the release of an ended window's key, and its bound.
pub async fn dedup_window(store: &dyn EventStore) {
    use time::{Duration, macros::datetime};
    let key = format!("keyless-{}", super::unique());
    // Far from the store's own clock, either way: only the window counts.
    let t0 = datetime!(2031-01-01 0:00 UTC);
    let at = |offset: Duration| {
        let now = t0 + offset;
        NewEvent {
            dedup_window: Some(DedupWindow {
                now,
                until: now + Duration::HOUR,
            }),
            ..row(Some("suite-w"), "error_reported", "51", Some(&key))
        }
    };
    let insert = |event: NewEvent| async move { store.insert(&event).await.unwrap() };
    let first = at(Duration::ZERO);
    assert_eq!(insert(first.clone()).await, Some(1));
    assert_eq!(
        insert(at(Duration::minutes(59))).await,
        None,
        "a redelivery"
    );
    assert_eq!(
        insert(at(Duration::HOUR - Duration::SECOND)).await,
        None,
        "the window's last second"
    );
    let second = at(Duration::HOUR);
    assert_eq!(insert(second.clone()).await, Some(2), "a new occurrence");
    // The new occurrence holds the key now, for its own hour.
    assert_eq!(insert(at(Duration::minutes(119))).await, None);
    assert_eq!(insert(at(Duration::minutes(120))).await, Some(3));
    let page = store.page(&query("suite-w", None, 10)).await.unwrap();
    let ids: Vec<&str> = page.events.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids[..2], [first.id.as_str(), second.id.as_str()]);
    assert_eq!(ids.len(), 3);

    // A library key's row holds it whatever the window of what comes next.
    let library = format!("library-{}", super::unique());
    let held = row(Some("suite-w"), "message_received", "51", Some(&library));
    assert_eq!(insert(held).await, Some(4));
    let later = NewEvent {
        dedup_window: Some(DedupWindow {
            now: t0 + Duration::days(3650),
            until: t0 + Duration::days(3651),
        }),
        ..row(Some("suite-w"), "message_received", "51", Some(&library))
    };
    assert_eq!(insert(later).await, None);
}

/// `types` and `phone_number_id` narrow a page.
pub async fn filters(store: &dyn EventStore) {
    store
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
    let mut q = query("suite-f", None, 10);
    q.types = Some(vec!["message_received".to_owned()]);
    let page = store.page(&q).await.unwrap();
    let pns: Vec<&str> = page
        .events
        .iter()
        .map(|e| e.phone_number_id.as_ref().unwrap().as_str())
        .collect();
    assert_eq!(pns, ["61", "62"]);
    q.phone_number_id = Some(PhoneNumberId::new("62"));
    let page = store.page(&q).await.unwrap();
    assert_eq!(page.events.len(), 1);
    q.types = Some(vec![
        "status_updated".to_owned(),
        "message_received".to_owned(),
    ]);
    q.phone_number_id = Some(PhoneNumberId::new("61"));
    let page = store.page(&q).await.unwrap();
    assert_eq!(page.events.len(), 2);
    // The stream's bounds are the stream's, whatever the filter: a page
    // that leaves the newest event out still reports it (a filtered poll's
    // `next_after` goes past what the filter left out).
    assert!(!page.more);
    assert_eq!(page.high_water, 3);
    // A filter that leaves nothing: the stream's bounds still come.
    q.types = Some(vec!["call_updated".to_owned()]);
    let page = store.page(&q).await.unwrap();
    assert!(page.events.is_empty() && !page.more);
    assert_eq!(page.high_water, 3);
}

/// Purge past `older_than`: the outbox takes no lock of its own (roadmap
/// S2: housekeeping holds one turn per round), so it never waits for
/// another replica's.
pub async fn purge_now(store: &dyn EventStore, older_than: Duration) -> u64 {
    store.purge(older_than).await.unwrap()
}

/// A purge cuts each stream by its own events' age: an older tenant's
/// events go, a newer one's stay, and each stream records its own cut.
pub async fn purge_is_per_stream(store: &dyn EventStore) {
    let old = store
        .insert(&row(Some("suite-u"), "message_received", "96", None))
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let new = store
        .insert(&row(Some("suite-v"), "message_received", "97", None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((old, new), (1, 1));
    assert!(purge_now(store, Duration::from_millis(200)).await >= 1);
    let u = store.page(&query("suite-u", None, 10)).await.unwrap();
    assert!(u.events.is_empty());
    assert_eq!((u.purged_through, u.high_water), (1, 1));
    let v = store.page(&query("suite-v", None, 10)).await.unwrap();
    assert_eq!(v.events.len(), 1, "the newer stream's event stays");
    assert_eq!((v.purged_through, v.high_water), (0, 1));
}

/// A purge removes a prefix of each stream and records it: the stream's
/// `purged_through` moves, its high water stays, later inserts go on after
/// it.
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
    assert_eq!((after.purged_through, after.high_water), (last, last));
    let next = store
        .insert(&row(Some("suite-p"), "message_received", "71", None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next, last + 1);
    let page = store.page(&query("suite-p", None, 10)).await.unwrap();
    let sequences: Vec<i64> = page.events.iter().map(|e| e.sequence).collect();
    assert_eq!(sequences, [next]);
}

/// A deleted tenant's rows go with it: a tenant created later with the
/// same id polls none of them, its stream records them purged (so an old
/// cursor is expired) and its sequences go on after them. Deleting the
/// tenant in `tenants` does it (the store of the same database).
pub async fn tenant_deleted(store: &dyn EventStore, tenants: &dyn Store) {
    for _ in 0..2 {
        store
            .insert(&row(Some("suite-t"), "message_received", "81", None))
            .await
            .unwrap()
            .unwrap();
    }
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
    let page = store.page(&query("suite-t", None, 10)).await.unwrap();
    assert!(page.events.is_empty(), "{:?}", page.events);
    assert_eq!((page.purged_through, page.high_water), (2, 2));
    let new = store
        .insert(&row(Some("suite-t"), "message_received", "81", None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new, 3, "after the deleted tenant's");
    let page = store.page(&query("suite-t", Some(2), 10)).await.unwrap();
    let sequences: Vec<i64> = page.events.iter().map(|e| e.sequence).collect();
    assert_eq!(sequences, [3]);
}

/// A purge of an earlier occurrence of a keyless event leaves the key with
/// the later occurrence that took it over: a redelivery within the later
/// one's window is still a duplicate. It purges every stream's rows older
/// than 200 ms: it runs last. Decisive: the earlier row giving the key up
/// (else, in memory, purging it frees the key the later one holds).
pub async fn purge_keeps_a_taken_key(store: &dyn EventStore) {
    use time::{Duration as Span, macros::datetime};
    let key = format!("keyless-purged-{}", super::unique());
    let t0 = datetime!(2031-06-01 0:00 UTC);
    let at = |offset: Span| NewEvent {
        dedup_window: Some(DedupWindow {
            now: t0 + offset,
            until: t0 + offset + Span::HOUR,
        }),
        ..row(Some("suite-k"), "error_reported", "56", Some(&key))
    };
    assert_eq!(store.insert(&at(Span::ZERO)).await.unwrap(), Some(1));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(store.insert(&at(Span::HOUR)).await.unwrap(), Some(2));
    assert!(purge_now(store, Duration::from_millis(200)).await >= 1);
    let page = store.page(&query("suite-k", None, 10)).await.unwrap();
    let sequences: Vec<i64> = page.events.iter().map(|e| e.sequence).collect();
    assert_eq!(
        sequences,
        [2],
        "the earlier occurrence purged, the later kept"
    );
    assert_eq!(
        store.insert(&at(Span::minutes(90))).await.unwrap(),
        None,
        "the later occurrence still holds the key"
    );
}

/// Roadmap S2, the port's requirement on every backend: an insert keeps
/// its row's tenant only while the binding its `RouteGuard` names still
/// holds it, checked with the insert, else the row is operator-only (in
/// no tenant's stream). A number's binding moved to another tenant, a
/// WABA's, a binding that began after the event's second, a guard naming
/// another WABA than the number's, and a tenant with no guard: each
/// operator-only; the same guards holding: the tenant's. Decisive: the
/// check in each backend's insert (the memory one's `holds`).
pub async fn the_route_guard_is_checked_with_the_insert(
    store: &dyn EventStore,
    tenants: &dyn Store,
) {
    use time::{Duration as Span, OffsetDateTime};
    let polled = |tenant_id: &'static str| async move {
        store
            .page(&query(tenant_id, None, 100))
            .await
            .unwrap()
            .events
            .into_iter()
            .map(|e| e.id)
            .collect::<Vec<_>>()
    };
    let now = OffsetDateTime::now_utc();
    // Holding: the number's guard, then the WABA's, dated after the
    // binding began (and undated, above in `row`).
    let kept = NewEvent {
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Number {
                phone_number_id: PhoneNumberId::new("31"),
                waba_id: WabaId::new(waba_of("31")),
            },
            not_after: Some(now + Span::HOUR),
        }),
        ..row(Some("suite-g"), "message_received", "31", None)
    };
    let waba_kept = NewEvent {
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Waba(WabaId::new(waba_of("31"))),
            not_after: Some(now + Span::HOUR),
        }),
        ..row(Some("suite-g"), "template_status_updated", "31", None)
    };
    // Not holding: a binding that began after Meta dated the event (an
    // hour before now: the binding is from this test's start).
    let dated_before = NewEvent {
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Waba(WabaId::new(waba_of("31"))),
            not_after: Some(now - Span::HOUR),
        }),
        ..row(Some("suite-g"), "template_status_updated", "31", None)
    };
    // Not holding: the number under another WABA than its binding's (one
    // suite-g holds too: only the number's own binding refuses it).
    let other_waba = NewEvent {
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Number {
                phone_number_id: PhoneNumberId::new("31"),
                waba_id: WabaId::new(waba_of("32")),
            },
            not_after: None,
        }),
        ..row(Some("suite-g"), "message_received", "31", None)
    };
    // Not holding: no guard at all.
    let unguarded = NewEvent {
        route_guard: None,
        ..row(Some("suite-g"), "message_received", "31", None)
    };
    for event in [&kept, &waba_kept, &dated_before, &other_waba, &unguarded] {
        store.insert(event).await.unwrap().unwrap();
    }
    assert_eq!(
        polled("suite-g").await,
        [kept.id.clone(), waba_kept.id.clone()],
        "only the rows whose guard held"
    );
    // The WABA and its number move to suite-h between the routing (to
    // suite-g) and the insert: operator-only, neither tenant's.
    let waba = WabaId::new(waba_of("31"));
    assert!(tenants.unbind_waba(&waba).await.unwrap());
    tenants
        .create_tenant(&TenantId::parse("suite-h").unwrap(), "")
        .await
        .unwrap();
    assert_eq!(
        tenants
            .bind_waba(
                &TenantId::parse("suite-h").unwrap(),
                &waba,
                &[PhoneNumberId::new("31")]
            )
            .await
            .unwrap(),
        meta_whatsapp_server::model::BindOutcome::Bound
    );
    let moved = row(Some("suite-g"), "message_received", "31", None);
    let waba_moved = NewEvent {
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Waba(waba.clone()),
            not_after: None,
        }),
        ..row(Some("suite-g"), "template_status_updated", "31", None)
    };
    for event in [&moved, &waba_moved] {
        store.insert(event).await.unwrap().unwrap();
    }
    assert_eq!(
        polled("suite-g").await,
        [kept.id.clone(), waba_kept.id.clone()],
        "suite-g lost the binding before the insert"
    );
    assert!(
        polled("suite-h").await.is_empty(),
        "suite-h got suite-g's events"
    );
    // The new holder's own row, guarded by its binding: its.
    let theirs = row(Some("suite-h"), "message_received", "31", None);
    store.insert(&theirs).await.unwrap().unwrap();
    assert_eq!(polled("suite-h").await, [theirs.id]);
}

/// The route guard's date, checked through a number's guard too (on
/// Postgres the number's branch of the insert has its own `attached_at`
/// clause): a row guarded by the number whose WABA's binding began after
/// Meta dated the event is operator-only; dated after the binding began,
/// the tenant's. Decisive: the number branch's `attached_at` check in each
/// backend's insert.
pub async fn a_binding_begun_after_the_event_fails_the_number_guard(store: &dyn EventStore) {
    use time::{Duration as Span, OffsetDateTime};
    let guarded = |not_after: OffsetDateTime| NewEvent {
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Number {
                phone_number_id: PhoneNumberId::new("35"),
                waba_id: WabaId::new(waba_of("35")),
            },
            not_after: Some(not_after),
        }),
        ..row(Some("suite-n"), "message_received", "35", None)
    };
    let now = OffsetDateTime::now_utc();
    let kept = guarded(now + Span::HOUR);
    let dated_before = guarded(now - Span::HOUR);
    for event in [&kept, &dated_before] {
        store.insert(event).await.unwrap().unwrap();
    }
    let ids: Vec<String> = store
        .page(&query("suite-n", None, 10))
        .await
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(ids, [kept.id], "a number's binding begun after the event");
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
        ("suite-w", "51"),
        ("suite-k", "56"),
        ("suite-f", "61"),
        ("suite-f", "62"),
        ("suite-p", "71"),
        ("suite-t", "81"),
        ("suite-s", "91"),
        ("suite-u", "96"),
        ("suite-v", "97"),
        ("suite-g", "31"),
        ("suite-g", "32"),
        ("suite-n", "35"),
    ] {
        bind(tenants, tenant_id, pn).await;
    }
    the_route_guard_is_checked_with_the_insert(store, tenants).await;
    a_binding_begun_after_the_event_fails_the_number_guard(store).await;
    insert_and_page(store).await;
    page_budget(store).await;
    dedup(store).await;
    dedup_window(store).await;
    filters(store).await;
    tenant_deleted(store, tenants).await;
    purge_is_per_stream(store).await;
    purge(store).await;
    purge_keeps_a_taken_key(store).await;
}
