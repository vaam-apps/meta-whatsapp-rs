//! [`Outbox`] (formerly the service's `tests/common/events_suite.rs`):
//! each tenant's own stream of sequences, pages and their byte budget,
//! dedup and its window, filters, purges, the media ids rows record as
//! received (roadmap M2e). Every row with a tenant is
//! guarded by its number's binding, bound first ([`bind`]): an outbox
//! keeps a row's tenant only while that binding holds it. What crosses
//! into the records (the guard's cases, a tenant's deletion) is
//! [`super::invariants`].

use std::time::Duration;

use meta_whatsapp_rs::core::ids::{MediaId, PhoneNumberId, WabaId};

use crate::backend::Backend;
use crate::model::TenantId;
use crate::outbox::{
    AfterMove, DedupWindow, EventPage, EventQuery, GuardedBinding, HeaderQuery, NewEvent, Outbox,
    RouteGuard,
};
use crate::store::RecordStore;

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
pub async fn bind(store: &dyn RecordStore, tenant_id: &str, pn: &str) {
    let tenant = tenant(tenant_id);
    store.create_tenant(&tenant, "").await.unwrap();
    store
        .bind_waba(
            &tenant,
            &WabaId::new(waba_of(pn)).unwrap(),
            &[PhoneNumberId::new(pn).unwrap()],
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
                phone_number_id: PhoneNumberId::new(pn).unwrap(),
                waba_id: WabaId::new(waba_of(pn)).unwrap(),
            },
            not_after: None,
            after_move: AfterMove::default(),
        }),
        phone_number_id: Some(PhoneNumberId::new(pn).unwrap()),
        waba_id: Some(WabaId::new(waba_of(pn)).unwrap()),
        event_type: event_type.to_owned(),
        // U+0000 in a message text: `json` keeps it, `jsonb` would refuse
        // it.
        data: format!("{{\"event\":\"{event_type}\",\"n\":{n},\"text\":\"a\\u0000b é\"}}"),
    }
}

/// A query of `tenant_id`'s stream after `after`, at most `limit` events.
pub(super) fn query(tenant_id: &str, after: Option<i64>, limit: usize) -> EventQuery {
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
pub async fn insert_and_page(backend: &dyn Backend) {
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    bind(records.as_ref(), "suite-a", "11").await;
    bind(records.as_ref(), "suite-a", "12").await;
    bind(records.as_ref(), "suite-b", "21").await;
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
    assert_eq!(got.phone_number_id, Some(PhoneNumberId::new("12").unwrap()));
    assert_eq!(got.waba_id, Some(WabaId::new(waba_of("12")).unwrap()));
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
pub async fn page_budget(backend: &dyn Backend) {
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    bind(records.as_ref(), "suite-s", "91").await;
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
    let cut = |page: &EventPage| {
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
pub async fn dedup(backend: &dyn Backend) {
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    bind(records.as_ref(), "suite-d", "41").await;
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
pub async fn dedup_window(backend: &dyn Backend) {
    use time::{Duration, macros::datetime};
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    bind(records.as_ref(), "suite-w", "51").await;
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
pub async fn filters(backend: &dyn Backend) {
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    bind(records.as_ref(), "suite-f", "61").await;
    bind(records.as_ref(), "suite-f", "62").await;
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
    q.phone_number_id = Some(PhoneNumberId::new("62").unwrap());
    let page = store.page(&q).await.unwrap();
    assert_eq!(page.events.len(), 1);
    q.types = Some(vec![
        "status_updated".to_owned(),
        "message_received".to_owned(),
    ]);
    q.phone_number_id = Some(PhoneNumberId::new("61").unwrap());
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

/// [`Outbox::headers_after`] (roadmap M2l) answers a page's rows without
/// their data: the same sequences, ids, tenants and types as
/// [`Outbox::page`] for the same tenant, cursor, types and limit, in
/// order, at most the limit with `more` when more follow, the stream's
/// bounds as a page's, never another tenant's rows nor an operator-only
/// one, and large rows as any other.
pub async fn headers(backend: &dyn Backend) {
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    bind(records.as_ref(), "suite-h", "93").await;
    bind(records.as_ref(), "suite-i", "94").await;
    let headers_of = |after: Option<i64>, types: Option<Vec<&str>>, limit: usize| HeaderQuery {
        tenant: tenant("suite-h"),
        after,
        types: types.map(|t| t.into_iter().map(str::to_owned).collect()),
        limit,
    };
    // An empty stream: no rows, no bounds.
    let none = store
        .headers_after(&headers_of(None, None, 10))
        .await
        .unwrap();
    assert!(none.headers.is_empty() && !none.more);
    assert_eq!((none.purged_through, none.high_water), (0, 0));

    let big = NewEvent {
        data: format!("{{\"pad\":\"{}\"}}", "x".repeat(256 * 1024)),
        ..row(Some("suite-h"), "history_synced", "93", None)
    };
    store
        .insert(&row(Some("suite-h"), "message_received", "93", None))
        .await
        .unwrap()
        .unwrap();
    store.insert(&big).await.unwrap().unwrap();
    store
        .insert(&row(None, "unknown", "93", None))
        .await
        .unwrap()
        .unwrap();
    store
        .insert(&row(Some("suite-i"), "message_received", "94", None))
        .await
        .unwrap()
        .unwrap();
    store
        .insert(&row(Some("suite-h"), "status_updated", "93", None))
        .await
        .unwrap()
        .unwrap();
    store
        .insert(&row(Some("suite-h"), "message_received", "93", None))
        .await
        .unwrap()
        .unwrap();

    // The same rows as a page, without their data.
    let page = store.page(&query("suite-h", None, 10)).await.unwrap();
    let all = store
        .headers_after(&headers_of(None, None, 10))
        .await
        .unwrap();
    assert_eq!(all.headers.len(), 4, "only suite-h's rows");
    assert_eq!(all.headers.len(), page.events.len());
    for (header, event) in all.headers.iter().zip(&page.events) {
        assert_eq!(header.sequence, event.sequence);
        assert_eq!(header.id, event.id);
        assert_eq!(header.tenant, event.tenant);
        assert_eq!(header.tenant, Some(tenant("suite-h")));
        assert_eq!(header.event_type, event.event_type);
    }
    assert_eq!(
        all.headers.iter().map(|h| h.sequence).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert!(!all.more);
    assert_eq!((all.purged_through, all.high_water), (0, 4));
    assert_eq!(
        all.headers[1].event_type, "history_synced",
        "the large row is a row like the others"
    );

    // At most the limit, `more` when more follow, and after the cursor.
    let two = store
        .headers_after(&headers_of(None, None, 2))
        .await
        .unwrap();
    assert_eq!(
        two.headers.iter().map(|h| h.sequence).collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(two.more);
    assert_eq!(two.high_water, 4, "the stream's, not the page's");
    let rest = store
        .headers_after(&headers_of(Some(2), None, 2))
        .await
        .unwrap();
    assert_eq!(
        rest.headers.iter().map(|h| h.sequence).collect::<Vec<_>>(),
        [3, 4]
    );
    assert!(!rest.more, "exactly the limit left is not more");
    let past = store
        .headers_after(&headers_of(Some(4), None, 2))
        .await
        .unwrap();
    assert!(past.headers.is_empty() && !past.more);
    assert_eq!(past.high_water, 4);

    // Types narrow it, and the bounds stay the stream's.
    let typed = store
        .headers_after(&headers_of(None, Some(vec!["message_received"]), 10))
        .await
        .unwrap();
    assert_eq!(
        typed.headers.iter().map(|h| h.sequence).collect::<Vec<_>>(),
        [1, 4]
    );
    assert_eq!(typed.high_water, 4);
    let typed = store
        .headers_after(&headers_of(
            None,
            Some(vec!["message_received", "status_updated"]),
            2,
        ))
        .await
        .unwrap();
    assert_eq!(
        typed.headers.iter().map(|h| h.sequence).collect::<Vec<_>>(),
        [1, 3]
    );
    assert!(typed.more);
    let nothing = store
        .headers_after(&headers_of(None, Some(vec!["call_updated"]), 10))
        .await
        .unwrap();
    assert!(nothing.headers.is_empty() && !nothing.more);
    assert_eq!(nothing.high_water, 4);

    // The other tenant's stream is its own.
    let theirs = store
        .headers_after(&HeaderQuery {
            tenant: tenant("suite-i"),
            ..headers_of(None, None, 10)
        })
        .await
        .unwrap();
    assert_eq!(theirs.headers.len(), 1);
    assert_eq!(theirs.headers[0].tenant, Some(tenant("suite-i")));
    assert_eq!(theirs.high_water, 1);
}

/// The time between the older and the newer rows of the purge cases.
const GAP: Duration = Duration::from_millis(400);

/// Purge past `older_than`: the outbox takes no lock of its own (roadmap
/// S2: housekeeping holds one turn per round), so it never waits for
/// another replica's.
async fn purge_now(store: &dyn Outbox, older_than: Duration) -> u64 {
    store.purge(older_than).await.unwrap()
}

/// A purge that keeps whatever was inserted from `newer` on and cuts what
/// was inserted `gap` before it: its age limit is measured from `newer`,
/// halfway into the gap, when the purge is called. (A fixed limit flaked on
/// a loaded CI: a stall between the newer rows' insert and the purge aged
/// them past the limit too.)
async fn purge_before(store: &dyn Outbox, newer: std::time::Instant, gap: Duration) -> u64 {
    store.purge(newer.elapsed() + gap / 2).await.unwrap()
}

/// A purge cuts each stream by its own events' age: an older tenant's
/// events go, a newer one's stay, and each stream records its own cut.
pub async fn purge_is_per_stream(backend: &dyn Backend) {
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    bind(records.as_ref(), "suite-u", "96").await;
    bind(records.as_ref(), "suite-v", "97").await;
    let old = store
        .insert(&row(Some("suite-u"), "message_received", "96", None))
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(GAP).await;
    let newer = std::time::Instant::now();
    let new = store
        .insert(&row(Some("suite-v"), "message_received", "97", None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((old, new), (1, 1));
    assert!(purge_before(store, newer, GAP).await >= 1);
    let u = store.page(&query("suite-u", None, 10)).await.unwrap();
    assert!(u.events.is_empty());
    assert_eq!((u.purged_through, u.high_water), (1, 1));
    let v = store.page(&query("suite-v", None, 10)).await.unwrap();
    assert_eq!(v.events.len(), 1, "the newer stream's event stays");
    assert_eq!((v.purged_through, v.high_water), (0, 1));
    // The header read reports the same bounds (roadmap M2l).
    let header_query = |tenant_id: &str| HeaderQuery {
        tenant: tenant(tenant_id),
        after: None,
        types: None,
        limit: 10,
    };
    let u = store.headers_after(&header_query("suite-u")).await.unwrap();
    assert!(u.headers.is_empty() && !u.more);
    assert_eq!((u.purged_through, u.high_water), (1, 1));
    let v = store.headers_after(&header_query("suite-v")).await.unwrap();
    assert_eq!(v.headers.len(), 1, "the newer stream's event stays");
    assert_eq!((v.purged_through, v.high_water), (0, 1));
}

/// A purge removes a prefix of each stream and records it: the stream's
/// `purged_through` moves, its high water stays, later inserts go on after
/// it.
pub async fn purge(backend: &dyn Backend) {
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    bind(records.as_ref(), "suite-p", "71").await;
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
    let header_query = HeaderQuery {
        tenant: tenant("suite-p"),
        after: None,
        types: None,
        limit: 10,
    };
    let headers = store.headers_after(&header_query).await.unwrap();
    assert!(headers.headers.is_empty() && !headers.more);
    assert_eq!(
        (headers.purged_through, headers.high_water),
        (last, last),
        "the header read reports a purge's bounds (roadmap M2l)"
    );
    let next = store
        .insert(&row(Some("suite-p"), "message_received", "71", None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next, last + 1);
    let page = store.page(&query("suite-p", None, 10)).await.unwrap();
    let sequences: Vec<i64> = page.events.iter().map(|e| e.sequence).collect();
    assert_eq!(sequences, [next]);
    let headers = store.headers_after(&header_query).await.unwrap();
    let sequences: Vec<i64> = headers.headers.iter().map(|h| h.sequence).collect();
    assert_eq!(sequences, [next], "from the oldest retained, as a page");
    assert_eq!((headers.purged_through, headers.high_water), (last, next));
}

/// A purge of an earlier occurrence of a keyless event leaves the key with
/// the later occurrence that took it over: a redelivery within the later
/// one's window is still a duplicate. It purges every stream's rows older
/// than half the gap before the later one: it runs last. Decisive: the earlier row giving the key up
/// (else, in memory, purging it frees the key the later one holds).
pub async fn purge_keeps_a_taken_key(backend: &dyn Backend) {
    use time::{Duration as Span, macros::datetime};
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    bind(records.as_ref(), "suite-k", "56").await;
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
    tokio::time::sleep(GAP).await;
    let newer = std::time::Instant::now();
    assert_eq!(store.insert(&at(Span::HOUR)).await.unwrap(), Some(2));
    assert!(purge_before(store, newer, GAP).await >= 1);
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

/// `row` carrying a `message_received` event whose message is `message`
/// (the library's serialization: `{"event", "message", …}`).
fn message_row(tenant_id: Option<&str>, pn: &str, message: &serde_json::Value) -> NewEvent {
    NewEvent {
        data: serde_json::json!({"event": "message_received", "message": message,
                                 "phone_number_id": pn})
        .to_string(),
        ..row(tenant_id, "message_received", pn, None)
    }
}

/// Roadmap M2e: a row keeps the media id its event records as received
/// ([`NewEvent::received_media`]), and [`Outbox::received_media`] finds
/// every row of it, newest first, the operator-only ones included, each
/// with its tenant and number; a row of another type, or a message with no
/// media, records none; a redelivery (its dedup key stored) adds no row;
/// at most [`RECEIVED_MEDIA_ROWS`](crate::outbox::RECEIVED_MEDIA_ROWS)
/// rows are answered, the newest; a deleted tenant's rows go with it.
/// Decisive: the id kept at insert, the operator-only stream searched
/// too, the order, the bound, the tenant's deletion.
pub async fn received_media(backend: &dyn Backend) {
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    bind(records.as_ref(), "suite-media-a", "771").await;
    bind(records.as_ref(), "suite-media-b", "772").await;
    let media = |id: &str| MediaId::new(id).unwrap();
    let image = serde_json::json!({"type": "image", "image": {"id": "1003383421387256"}});
    let mut received = message_row(Some("suite-media-a"), "771", &image);
    received.dedup_key = Some(format!("suite-media-{}", super::unique()));
    assert!(store.insert(&received).await.unwrap().is_some());
    assert_eq!(store.insert(&received).await.unwrap(), None, "a redelivery");
    let operator_only = message_row(None, "772", &image);
    assert!(store.insert(&operator_only).await.unwrap().is_some());
    // Another type with the same data, a text, a sticker of tenant b.
    let status = NewEvent {
        event_type: "status_updated".to_owned(),
        ..message_row(Some("suite-media-b"), "772", &image)
    };
    store.insert(&status).await.unwrap();
    let text = serde_json::json!({"type": "text", "text": {"body": "1003383421387256"}});
    store
        .insert(&message_row(Some("suite-media-b"), "772", &text))
        .await
        .unwrap();
    let sticker = serde_json::json!({"type": "sticker", "sticker": {"id": "1908647269898587"}});
    store
        .insert(&message_row(Some("suite-media-b"), "772", &sticker))
        .await
        .unwrap();

    let found = store
        .received_media(&media("1003383421387256"))
        .await
        .unwrap();
    let seen: Vec<(Option<&str>, Option<&str>)> = found
        .iter()
        .map(|row| {
            (
                row.tenant.as_ref().map(TenantId::as_str),
                row.phone_number_id.as_ref().map(PhoneNumberId::as_str),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [(None, Some("772")), (Some("suite-media-a"), Some("771"))],
        "every row of the id, newest first, the operator-only one included"
    );
    assert!(found[0].received_at >= found[1].received_at);
    let sticker = store
        .received_media(&media("1908647269898587"))
        .await
        .unwrap();
    assert_eq!(
        sticker
            .iter()
            .map(|row| row.tenant.as_ref().map(TenantId::as_str))
            .collect::<Vec<_>>(),
        [Some("suite-media-b")]
    );
    assert!(
        store
            .received_media(&media("1003383421387299"))
            .await
            .unwrap()
            .is_empty()
    );

    // At most `RECEIVED_MEDIA_ROWS`, the newest: a media id named by more
    // rows (a message and its edits) answers a bounded read.
    let edited = serde_json::json!({"type": "document", "document": {"id": "622684793477189"}});
    for _ in 0..=crate::outbox::RECEIVED_MEDIA_ROWS {
        store
            .insert(&message_row(Some("suite-media-a"), "771", &edited))
            .await
            .unwrap();
    }
    let bounded = store
        .received_media(&media("622684793477189"))
        .await
        .unwrap();
    assert_eq!(
        bounded.len(),
        crate::outbox::RECEIVED_MEDIA_ROWS,
        "more rows than RECEIVED_MEDIA_ROWS answered"
    );
    assert!(
        bounded
            .windows(2)
            .all(|pair| pair[0].received_at >= pair[1].received_at),
        "not newest first"
    );

    // A deleted tenant's rows go with it.
    bind(records.as_ref(), "suite-media-gone", "773").await;
    let video = serde_json::json!({"type": "video", "video": {"id": "731675419373506"}});
    store
        .insert(&message_row(Some("suite-media-gone"), "773", &video))
        .await
        .unwrap();
    assert_eq!(
        store
            .received_media(&media("731675419373506"))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        records
            .unbind_waba(&WabaId::new(waba_of("773")).unwrap())
            .await
            .unwrap()
    );
    assert_eq!(
        records
            .delete_tenant(&tenant("suite-media-gone"))
            .await
            .unwrap(),
        crate::model::DeleteTenantOutcome::Deleted
    );
    assert!(
        store
            .received_media(&media("731675419373506"))
            .await
            .unwrap()
            .is_empty(),
        "a deleted tenant's received media"
    );
}

/// A row for `tenant` on `pn` (as [`row`]) whose event names `from` as
/// its customer (`message.from`), and `dedup` as its key.
pub fn from_row(tenant_id: Option<&str>, pn: &str, from: &str, dedup: Option<&str>) -> NewEvent {
    NewEvent {
        data: serde_json::json!({
            "event": "message_received",
            "display_phone_number": "15550783881",
            "message": {"from": from, "type": "text", "text": {"body": "16505550000"}}
        })
        .to_string(),
        ..row(tenant_id, "message_received", pn, dedup)
    }
}

/// Roadmap M2f (design D89, narrow by default): an erasure deletes the
/// tenant's own rows naming one of its contacts on its number, and
/// nothing else: not the number's operator-only rows naming them (the
/// operator's: their dedup keys stay taken), not another customer's row
/// on the number, not the customer's rows on another number of the
/// tenant, not another tenant's row on the same number (a previous
/// holder's, kept in its own stream), not a row whose text quotes the
/// number. It answers how many went; a repeat deletes nothing; an empty
/// list deletes nothing; a deleted row's dedup key is free again.
/// Decisive: each clause of a backend's erasure (the contacts, the
/// number, the tenant's stream alone: an erasure reaching the
/// operator-only stream deletes its rows and frees their keys).
#[allow(clippy::too_many_lines)] // one case, read top to bottom
pub async fn erase_contacts(backend: &dyn Backend) {
    const X: &str = "16505551234";
    const X_BSUID: &str = "US.13491208655302741918";
    const Y: &str = "16505550000";
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    let a = tenant("suite-erase-a");
    let pn = |n: &str| PhoneNumberId::new(n).unwrap();
    // A previous holder's row on 33, kept in its stream after 33 moved.
    bind(records.as_ref(), "suite-erase-b", "33").await;
    let previous = store
        .insert(&from_row(Some("suite-erase-b"), "33", X, None))
        .await
        .unwrap()
        .unwrap();
    assert!(
        records
            .unbind_waba(&WabaId::new(waba_of("33")).unwrap())
            .await
            .unwrap()
    );
    bind(records.as_ref(), "suite-erase-a", "31").await;
    for number in ["32", "33"] {
        records
            .bind_waba(&a, &WabaId::new(waba_of(number)).unwrap(), &[pn(number)])
            .await
            .unwrap();
    }
    let mut gone = Vec::new();
    for event in [
        from_row(Some("suite-erase-a"), "31", X, Some("erase-k1")),
        from_row(Some("suite-erase-a"), "31", X_BSUID, None),
    ] {
        gone.push(store.insert(&event).await.unwrap().unwrap());
    }
    let mut kept = Vec::new();
    for event in [
        from_row(Some("suite-erase-a"), "31", Y, None),
        from_row(Some("suite-erase-a"), "32", X, None),
        // A text quoting the number names nobody.
        NewEvent {
            data: format!("{{\"event\":\"message_received\",\"text\":\"{X}\"}}"),
            ..row(Some("suite-erase-a"), "message_received", "31", None)
        },
    ] {
        kept.push(store.insert(&event).await.unwrap().unwrap());
    }
    // The number's operator-only rows naming the customer, each by a key
    // of its own: a kept row keeps its key taken.
    let operator_only =
        |number: &str| from_row(None, number, X, Some(&format!("erase-op-{number}")));
    for number in ["31", "32"] {
        store.insert(&operator_only(number)).await.unwrap().unwrap();
    }
    let contacts = [X.to_owned(), X_BSUID.to_owned()];
    assert_eq!(
        store
            .erase_contacts(&a, &pn("31"), &contacts)
            .await
            .unwrap(),
        2,
        "the tenant's two rows on 31, never the operator-only one"
    );
    let page = store
        .page(&query("suite-erase-a", None, 100))
        .await
        .unwrap();
    let left: Vec<i64> = page.events.iter().map(|e| e.sequence).collect();
    assert_eq!(left, kept, "only the rows naming them on 31 went: {gone:?}");
    assert_eq!(page.high_water, kept[2], "the stream's sequences stay");
    assert_eq!(page.purged_through, 0, "an erasure is no purge");
    assert_eq!(
        store
            .erase_contacts(&a, &pn("31"), &contacts)
            .await
            .unwrap(),
        0,
        "a repeat deletes nothing"
    );
    assert_eq!(
        store.erase_contacts(&a, &pn("31"), &[]).await.unwrap(),
        0,
        "no contacts, nothing"
    );
    assert_eq!(
        store
            .erase_contacts(&a, &pn("31"), &[String::new()])
            .await
            .unwrap(),
        0,
        "an empty id names nobody"
    );
    // On 33, the previous holder's row is not the tenant's to erase.
    assert_eq!(
        store
            .erase_contacts(&a, &pn("33"), &contacts)
            .await
            .unwrap(),
        0
    );
    let theirs = store.page(&query("suite-erase-b", None, 10)).await.unwrap();
    assert_eq!(
        theirs.events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        [previous],
        "another tenant's stream is untouched"
    );
    // On 32, the tenant's row alone.
    assert_eq!(
        store
            .erase_contacts(&a, &pn("32"), &contacts)
            .await
            .unwrap(),
        1
    );
    // The operator-only rows naming the customer stay, on both numbers:
    // their keys are still taken (a deleted row's would be free).
    for number in ["31", "32"] {
        assert_eq!(
            store.insert(&operator_only(number)).await.unwrap(),
            None,
            "the operator-only row on {number} was erased"
        );
    }
    // The deleted row's dedup key is free: the same event is recorded
    // again (the webhook pipeline's dedup markers keep redeliveries out).
    assert!(
        store
            .insert(&from_row(Some("suite-erase-a"), "31", X, Some("erase-k1")))
            .await
            .unwrap()
            .is_some()
    );
}

/// Every case above, on `backend`, empty, the purges last: they cut every
/// stream's older rows, not only their own case's.
pub async fn run(backend: &dyn Backend) {
    insert_and_page(backend).await;
    received_media(backend).await;
    page_budget(backend).await;
    dedup(backend).await;
    dedup_window(backend).await;
    filters(backend).await;
    headers(backend).await;
    purge_is_per_stream(backend).await;
    purge(backend).await;
    purge_keeps_a_taken_key(backend).await;
    erase_contacts(backend).await;
}
