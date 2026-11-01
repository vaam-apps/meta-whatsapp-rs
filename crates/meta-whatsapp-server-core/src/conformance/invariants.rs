//! What crosses ports, which a backend made of loose stores (each port
//! over a database of its own, or a store made anew per call) loses: the
//! bundle's accessors hand out the same data on every call; deleting a
//! tenant ([`RecordStore::delete_tenant`]) purges its event stream; the
//! outbox checks the routing again with each insert, against the bindings
//! [`Backend::records`] writes ([`Outbox::insert`]: the [`RouteGuard`]
//! cases, roadmap S2 and its security review's L3 and L4); and each
//! tenant's inserts commit in sequence order. Formerly the service's
//! `tests/common/backend_suite.rs`, and the cases of its
//! `events_suite.rs` that read the records.
//!
//! [`RecordStore::delete_tenant`]: crate::store::RecordStore::delete_tenant
//! [`Outbox::insert`]: crate::outbox::Outbox::insert
//! [`RouteGuard`]: crate::outbox::RouteGuard

use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::store::{Expiry, StoreKey};

use super::outbox::{bind, query, row, waba_of};
use crate::backend::Backend;
use crate::model::{BindOutcome, DeleteTenantOutcome, TenantId};
use crate::outbox::{AfterMove, GuardedBinding, NewEvent, RouteGuard};

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

/// Each accessor hands out the same data on every call: what one handle
/// writes, another reads. Decisive: a backend that builds a fresh
/// in-process store per call (`kv()`, `records()`).
pub async fn every_accessor_hands_out_the_same_data(backend: &dyn Backend) {
    let key = StoreKey::new("wa.test.backend", "same-data");
    let written = backend
        .kv()
        .put(&key, b"value".to_vec(), Expiry::Never)
        .await
        .unwrap();
    let read = backend
        .kv()
        .get(&key)
        .await
        .unwrap()
        .expect("written through another kv() handle");
    assert_eq!(read.value, b"value");
    assert_eq!(read.version, written);

    let tenant = tenant("backend-same-data");
    backend
        .records()
        .create_tenant(&tenant, "")
        .await
        .unwrap()
        .expect("a new tenant");
    assert!(
        backend.records().tenant(&tenant).await.unwrap().is_some(),
        "created through another records() handle"
    );
    every_other_accessor(backend, &tenant).await;
}

/// Roadmap S2, the rest of the bundle: the idempotency records, the
/// outbox, the leader lock and the conversation store also hand out the
/// same data on every call, and the outbox reads the bindings `records()`
/// wrote (its insert keeps a tenant only while its guarded binding holds:
/// a binding in another database would leave the row operator-only).
/// Decisive: a backend building any of them anew per call.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn every_other_accessor(backend: &dyn Backend, tenant: &TenantId) {
    use crate::model::{IdempotencyClaim, IdempotencyKey};
    use crate::outbox::EventQuery;
    use crate::store::HOUSEKEEPING_LEASE;
    use meta_whatsapp_rs::core::ids::MessageId;
    use meta_whatsapp_rs::core::store::{ConversationKey, Direction, StoredMessage};

    let key = IdempotencyKey::parse("order:same-data").unwrap();
    let minute = std::time::Duration::from_secs(60);
    let claim = |claim_id: &'static str| {
        let key = key.clone();
        async move {
            backend
                .idempotency()
                .claim_idempotency_key(tenant, &key, &[1; 32], claim_id, minute, minute)
                .await
                .unwrap()
        }
    };
    assert_eq!(claim("first").await, IdempotencyClaim::Claimed);
    assert!(
        matches!(claim("second").await, IdempotencyClaim::Existing(_)),
        "claimed through another idempotency() handle"
    );

    let waba = WabaId::new_unchecked("backend-same-data-waba");
    assert_eq!(
        backend
            .records()
            .bind_waba(tenant, &waba, &[])
            .await
            .unwrap(),
        BindOutcome::Bound
    );
    let event = NewEvent {
        id: format!("evt_same_data_{}", super::unique()),
        dedup_key: None,
        dedup_window: None,
        tenant: Some(tenant.clone()),
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Waba(waba.clone()),
            not_after: None,
            after_move: AfterMove::default(),
        }),
        phone_number_id: None,
        waba_id: Some(waba),
        event_type: "template_status_updated".to_owned(),
        data: "{}".to_owned(),
    };
    backend.outbox().insert(&event).await.unwrap().unwrap();
    let page = backend
        .outbox()
        .page(&EventQuery {
            tenant: tenant.clone(),
            after: None,
            types: None,
            phone_number_id: None,
            limit: 10,
            max_bytes: 1024,
        })
        .await
        .unwrap();
    let ids: Vec<&str> = page.events.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        [event.id.as_str()],
        "inserted through another outbox() handle, checked against records()"
    );

    // Advisory locks are the database's, not a schema's: a name of its own.
    let name = format!("backend-same-data-{}", super::unique());
    let turn = backend
        .leader_lock()
        .try_exclusive(&name, HOUSEKEEPING_LEASE)
        .await
        .unwrap()
        .expect("nobody holds it");
    assert!(
        backend
            .leader_lock()
            .try_exclusive(&name, HOUSEKEEPING_LEASE)
            .await
            .unwrap()
            .is_none(),
        "held through another leader_lock() handle"
    );
    turn.release().await.unwrap();

    let conversation = ConversationKey::new(
        PhoneNumberId::new("106540352242922").unwrap(),
        "16505551234",
    );
    let message = StoredMessage::tombstone(
        &conversation,
        &MessageId::new("wamid.same-data"),
        Direction::Inbound,
        time::OffsetDateTime::now_utc(),
    );
    assert!(backend.conversations().append(message).await.unwrap());
    let stored = backend
        .conversations()
        .messages(&conversation, None, 10)
        .await
        .unwrap();
    assert_eq!(
        stored.len(),
        1,
        "appended through another conversations() handle"
    );
}

/// A deleted tenant's rows go with it: a tenant created later with the
/// same id polls none of them, its stream records them purged (so an old
/// cursor is expired) and its sequences go on after them. Deleting the
/// tenant through `backend.records()` does it (the same database).
pub async fn deleting_a_tenant_purges_its_stream(backend: &dyn Backend) {
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    let tenants = records.as_ref();
    bind(records.as_ref(), "suite-t", "81").await;
    for _ in 0..2 {
        store
            .insert(&row(Some("suite-t"), "message_received", "81", None))
            .await
            .unwrap()
            .unwrap();
    }
    let id = tenant("suite-t");
    tenants
        .unbind_waba(&WabaId::new(waba_of("81")).unwrap())
        .await
        .unwrap();
    assert_eq!(
        tenants.delete_tenant(&id).await.unwrap(),
        DeleteTenantOutcome::Deleted
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

/// Roadmap S2, the port's requirement on every backend: an insert keeps
/// its row's tenant only while the binding its `RouteGuard` names still
/// holds it, checked with the insert, else the row is operator-only (in
/// no tenant's stream). A number's binding moved to another tenant, a
/// WABA's, a binding that began after the event's second, a guard naming
/// another WABA than the number's, and a tenant with no guard: each
/// operator-only; the same guards holding: the tenant's. Decisive: the
/// check in each backend's insert (the memory one's `holds`).
pub async fn the_route_guard_is_checked_with_the_insert(backend: &dyn Backend) {
    use time::{Duration as Span, OffsetDateTime};
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    let tenants = records.as_ref();
    bind(records.as_ref(), "suite-g", "31").await;
    bind(records.as_ref(), "suite-g", "32").await;
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
                phone_number_id: PhoneNumberId::new("31").unwrap(),
                waba_id: WabaId::new(waba_of("31")).unwrap(),
            },
            not_after: Some(now + Span::HOUR),
            after_move: AfterMove::default(),
        }),
        ..row(Some("suite-g"), "message_received", "31", None)
    };
    let waba_kept = NewEvent {
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Waba(WabaId::new(waba_of("31")).unwrap()),
            not_after: Some(now + Span::HOUR),
            after_move: AfterMove::default(),
        }),
        ..row(Some("suite-g"), "template_status_updated", "31", None)
    };
    // Not holding: a binding that began after Meta dated the event (an
    // hour before now: the binding is from this test's start).
    let dated_before = NewEvent {
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Waba(WabaId::new(waba_of("31")).unwrap()),
            not_after: Some(now - Span::HOUR),
            after_move: AfterMove::default(),
        }),
        ..row(Some("suite-g"), "template_status_updated", "31", None)
    };
    // Not holding: the number under another WABA than its binding's (one
    // suite-g holds too: only the number's own binding refuses it).
    let other_waba = NewEvent {
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Number {
                phone_number_id: PhoneNumberId::new("31").unwrap(),
                waba_id: WabaId::new(waba_of("32")).unwrap(),
            },
            not_after: None,
            after_move: AfterMove::default(),
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
    let waba = WabaId::new(waba_of("31")).unwrap();
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
                &[PhoneNumberId::new("31").unwrap()]
            )
            .await
            .unwrap(),
        BindOutcome::Bound
    );
    let moved = row(Some("suite-g"), "message_received", "31", None);
    let waba_moved = NewEvent {
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Waba(waba.clone()),
            not_after: None,
            after_move: AfterMove::default(),
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
pub async fn a_binding_begun_after_the_event_fails_the_number_guard(backend: &dyn Backend) {
    use time::{Duration as Span, OffsetDateTime};
    let outbox = backend.outbox();
    let store = outbox.as_ref();
    let records = backend.records();
    bind(records.as_ref(), "suite-n", "35").await;
    let guarded = |not_after: OffsetDateTime| NewEvent {
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Number {
                phone_number_id: PhoneNumberId::new("35").unwrap(),
                waba_id: WabaId::new(waba_of("35")).unwrap(),
            },
            not_after: Some(not_after),
            after_move: AfterMove::default(),
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

/// Bind `pn`'s WABA ([`waba_of`]) to `from`, unbind it, and bind it to
/// `to` (both created when missing): a binding of `to`'s whose WABA had a
/// previous tenant (roadmap S2b). Its `previous_tenant_unbound_at`, which
/// the store read from the tombstone the unbind left.
async fn moved(
    records: &dyn crate::store::RecordStore,
    from: &str,
    to: &str,
    pn: &str,
) -> time::OffsetDateTime {
    bind(records, from, pn).await;
    let waba = WabaId::new(waba_of(pn)).unwrap();
    assert!(records.unbind_waba(&waba).await.unwrap(), "{pn}: unbound");
    bind(records, to, pn).await;
    let binding = records.waba(&waba).await.unwrap().unwrap();
    assert_eq!(binding.tenant_id.as_str(), to, "{pn}: moved");
    binding
        .previous_tenant_unbound_at
        .unwrap_or_else(|| panic!("{pn}: a move from {from} to {to} left no previous tenant"))
}

/// The ids of the rows `tenant_id`'s stream holds, in order.
async fn ids_of(backend: &dyn Backend, tenant_id: &str) -> Vec<String> {
    backend
        .outbox()
        .page(&query(tenant_id, None, 100))
        .await
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.id)
        .collect()
}

/// A row for `tenant_id` about `pn`, guarded by its number's binding (or,
/// `by_number` false, its WABA's), dated `not_after`, under `after_move`.
fn guarded_row(
    tenant_id: &str,
    pn: &str,
    by_number: bool,
    not_after: Option<time::OffsetDateTime>,
    after_move: AfterMove,
) -> NewEvent {
    let waba = WabaId::new(waba_of(pn)).unwrap();
    NewEvent {
        route_guard: Some(RouteGuard {
            binding: if by_number {
                GuardedBinding::Number {
                    phone_number_id: PhoneNumberId::new(pn).unwrap(),
                    waba_id: waba,
                }
            } else {
                GuardedBinding::Waba(waba)
            },
            not_after,
            after_move,
        }),
        ..row(Some(tenant_id), "message_received", pn, None)
    }
}

/// The start of `pn`'s WABA's binding, truncated to its second.
async fn began_second(records: &dyn crate::store::RecordStore, pn: &str) -> time::OffsetDateTime {
    let waba = WabaId::new(waba_of(pn)).unwrap();
    let began = records.waba(&waba).await.unwrap().unwrap().attached_at;
    time::OffsetDateTime::from_unix_timestamp(began.unix_timestamp()).unwrap()
}

/// Roadmap S2, the security review's L3, on every backend: after a move
/// (the WABA had a previous tenant), a row keeps its tenant only when the
/// guarded WABA's binding began in a second before the event's. Dated in
/// the binding's own second (its first instant, and its last
/// millisecond), through a number's guard and through the WABA's:
/// operator-only; dated the next second: the tenant's. Meta's dates are
/// whole seconds, so an event of the binding's own second may be a
/// previous holder's. Decisive: the strict comparison after a move in each
/// backend's insert, in each branch (memory: `RouteGuard::admits`;
/// Postgres: `attached_at < to_timestamp(n - margin)`).
pub async fn a_binding_of_the_events_own_second_fails_the_guard(backend: &dyn Backend) {
    use time::Duration as Span;
    let records = backend.records();
    moved(records.as_ref(), "suite-e-before", "suite-e", "38").await;
    let second = began_second(records.as_ref(), "38").await;
    let outbox = backend.outbox();
    let mut kept = Vec::new();
    for by_number in [true, false] {
        for same_second in [second, second + Span::milliseconds(999)] {
            let row = guarded_row(
                "suite-e",
                "38",
                by_number,
                Some(same_second),
                AfterMove::default(),
            );
            outbox.insert(&row).await.unwrap().unwrap();
        }
        let next_second = guarded_row(
            "suite-e",
            "38",
            by_number,
            Some(second + Span::SECOND),
            AfterMove::default(),
        );
        outbox.insert(&next_second).await.unwrap().unwrap();
        kept.push(next_second.id);
    }
    assert_eq!(
        ids_of(backend, "suite-e").await,
        kept,
        "only the rows dated after the binding's own second"
    );
}

/// Roadmap S2b, on every backend: a first onboarding (no tenant held the
/// WABA before) keeps the events of its binding's first second, through a
/// number's guard and through the WABA's, its first instant and its last
/// millisecond; never one of the second before. Decisive: the relaxed
/// comparison without a previous tenant in each backend's insert, in each
/// branch (memory: `RouteGuard::admits`; Postgres: `attached_at <
/// to_timestamp(n + 1)`), and the store's record of no previous tenant.
pub async fn a_first_onboarding_keeps_its_first_second(backend: &dyn Backend) {
    use time::Duration as Span;
    let records = backend.records();
    bind(records.as_ref(), "suite-f", "39").await;
    let waba = WabaId::new(waba_of("39")).unwrap();
    assert_eq!(
        records
            .waba(&waba)
            .await
            .unwrap()
            .unwrap()
            .previous_tenant_unbound_at,
        None,
        "a first onboarding has no previous tenant"
    );
    let second = began_second(records.as_ref(), "39").await;
    let outbox = backend.outbox();
    let mut kept = Vec::new();
    for by_number in [true, false] {
        let before = guarded_row(
            "suite-f",
            "39",
            by_number,
            Some(second - Span::milliseconds(1)),
            AfterMove::default(),
        );
        outbox.insert(&before).await.unwrap().unwrap();
        for same_second in [second, second + Span::milliseconds(999)] {
            let row = guarded_row(
                "suite-f",
                "39",
                by_number,
                Some(same_second),
                AfterMove::default(),
            );
            outbox.insert(&row).await.unwrap().unwrap();
            kept.push(row.id);
        }
    }
    assert_eq!(
        ids_of(backend, "suite-f").await,
        kept,
        "a first onboarding's first second is its own, the second before is not"
    );
}

/// Roadmap S2b, on every backend: the skew margin applies after a move
/// only. With a margin of 5 seconds, a moved WABA's binding keeps no event
/// dated up to 5 seconds after its own second, and keeps one dated 6
/// seconds after; a first onboarding's binding keeps an event of its own
/// second under the same margin. Through a number's guard and the WABA's.
/// A margin reaching before the Unix epoch fails no insert (Postgres
/// refused the instant it bound, for every dated row). Decisive: the
/// margin in each backend's insert, its application to a binding with a
/// previous tenant only, and `RouteGuard::dated_bounds` keeping its bound
/// at the epoch.
pub async fn the_skew_margin_applies_after_a_move_only(backend: &dyn Backend) {
    use time::Duration as Span;
    let margin = AfterMove {
        skew_margin: std::time::Duration::from_secs(5),
        undated_moved_by: None,
    };
    let records = backend.records();
    moved(records.as_ref(), "suite-k-before", "suite-k", "47").await;
    bind(records.as_ref(), "suite-l", "48").await;
    let moved_second = began_second(records.as_ref(), "47").await;
    let first_second = began_second(records.as_ref(), "48").await;
    let outbox = backend.outbox();
    let (mut moved_kept, mut first_kept) = (Vec::new(), Vec::new());
    for by_number in [true, false] {
        for within in [1, 5] {
            let row = guarded_row(
                "suite-k",
                "47",
                by_number,
                Some(moved_second + Span::seconds(within)),
                margin,
            );
            outbox.insert(&row).await.unwrap().unwrap();
        }
        let past = guarded_row(
            "suite-k",
            "47",
            by_number,
            Some(moved_second + Span::seconds(6)),
            margin,
        );
        outbox.insert(&past).await.unwrap().unwrap();
        moved_kept.push(past.id);
        let first = guarded_row("suite-l", "48", by_number, Some(first_second), margin);
        outbox.insert(&first).await.unwrap().unwrap();
        first_kept.push(first.id);
        // A margin reaching before the Unix epoch (a setting of
        // `Duration::MAX`): the inserts succeed, the moved WABA's row a day
        // later still no tenant's, the first onboarding's its own.
        let absurd = AfterMove {
            skew_margin: std::time::Duration::MAX,
            undated_moved_by: None,
        };
        let moved_row = guarded_row(
            "suite-k",
            "47",
            by_number,
            Some(moved_second + Span::DAY),
            absurd,
        );
        outbox.insert(&moved_row).await.unwrap().unwrap();
        let first = guarded_row("suite-l", "48", by_number, Some(first_second), absurd);
        outbox.insert(&first).await.unwrap().unwrap();
        first_kept.push(first.id);
    }
    assert_eq!(
        ids_of(backend, "suite-k").await,
        moved_kept,
        "after a move: only the rows dated past the skew margin"
    );
    assert_eq!(
        ids_of(backend, "suite-l").await,
        first_kept,
        "a first onboarding: the skew margin applied without a previous tenant"
    );
}

/// Roadmap S2b (design D36), on every backend: an undated row of a moved
/// WABA keeps its tenant only once the previous tenant left by the guard's
/// `undated_moved_by` (`OperatorOnly`: the event's arrival less the
/// window): left after it (within the window), operator-only; left at it,
/// the tenant's; with none (`CurrentHolder`), the tenant's; and a first
/// onboarding's undated row under the same window is its own. Through a
/// number's guard and the WABA's. Decisive: the window check in each
/// backend's insert, and the first onboarding's exemption from it.
pub async fn an_undated_event_after_a_move_waits_out_the_window(backend: &dyn Backend) {
    use time::Duration as Span;
    let records = backend.records();
    let left = moved(records.as_ref(), "suite-u-before", "suite-u", "49").await;
    bind(records.as_ref(), "suite-v", "50").await;
    let within = AfterMove {
        skew_margin: std::time::Duration::ZERO,
        undated_moved_by: Some(left - Span::SECOND),
    };
    let past = AfterMove {
        undated_moved_by: Some(left),
        ..within
    };
    let outbox = backend.outbox();
    let (mut moved_kept, mut first_kept) = (Vec::new(), Vec::new());
    for by_number in [true, false] {
        let operator_only = guarded_row("suite-u", "49", by_number, None, within);
        outbox.insert(&operator_only).await.unwrap().unwrap();
        for kept in [past, AfterMove::default()] {
            let row = guarded_row("suite-u", "49", by_number, None, kept);
            outbox.insert(&row).await.unwrap().unwrap();
            moved_kept.push(row.id);
        }
        let first = guarded_row("suite-v", "50", by_number, None, within);
        outbox.insert(&first).await.unwrap().unwrap();
        first_kept.push(first.id);
    }
    assert_eq!(
        ids_of(backend, "suite-u").await,
        moved_kept,
        "after a move: an undated row within the window kept its tenant"
    );
    assert_eq!(
        ids_of(backend, "suite-v").await,
        first_kept,
        "a first onboarding: an undated row held back by a window"
    );
}

/// Roadmap S3, the outbox's first contract across tasks: a stream's
/// inserts commit in sequence order, so a reader that sees sequence `n`
/// of a tenant has seen every event before it, and a poller following
/// `next_after` ([`crate::events::poll`]) while eight writers insert sees
/// every event exactly once, in sequence order, 1 to the last. The port's
/// side of the service's live test over `GET /v1/events`
/// (`live_postgres_polling_during_concurrent_inserts_misses_nothing`,
/// which stays), on every backend. Decisive (on Postgres): the tenant's
/// stream row, locked from the drawing of its sequence to the commit
/// (without it, a sequence drawn before another's commit can commit after
/// a poll moved past it: drawing the sequence in a transaction of its own
/// fails this case). On memory, one lock over the records and the outbox
/// meets it by construction; a memory outbox that drew the sequence under
/// its lock and stored the row after an await would fail it.
pub async fn a_tenants_inserts_commit_in_sequence_order(backend: &dyn Backend) {
    const WRITERS: usize = 8;
    const EACH: usize = 40;
    let records = backend.records();
    bind(records.as_ref(), "suite-o", "45").await;
    let writers: Vec<_> = (0..WRITERS)
        .map(|w| {
            let outbox = backend.outbox();
            tokio::spawn(async move {
                let mut ids = Vec::with_capacity(EACH);
                for i in 0..EACH {
                    let event = NewEvent {
                        id: format!("evt_order_{w}_{i}_{}", super::unique()),
                        ..row(Some("suite-o"), "message_received", "45", None)
                    };
                    outbox.insert(&event).await.unwrap().unwrap();
                    ids.push(event.id);
                    // A turn for the others on a current-thread runtime.
                    tokio::task::yield_now().await;
                }
                ids
            })
        })
        .collect();
    let outbox = backend.outbox();
    let mut seen: Vec<(i64, String)> = Vec::new();
    let mut after: Option<i64> = None;
    let mut done = false;
    let mut idle_rounds = 0;
    while idle_rounds < 3 {
        let polled = crate::events::poll(outbox.as_ref(), &query("suite-o", after, 7))
            .await
            .unwrap_or_else(|e| panic!("polling while inserting: {}", e.code()));
        if polled.events.is_empty() && done {
            idle_rounds += 1;
        }
        seen.extend(polled.events.into_iter().map(|e| (e.sequence, e.id)));
        after = Some(polled.next_after);
        if !done && writers.iter().all(tokio::task::JoinHandle::is_finished) {
            done = true;
        }
        // The writers' turn on a current-thread runtime (a memory outbox
        // never awaits).
        tokio::task::yield_now().await;
    }
    let mut written = Vec::with_capacity(WRITERS * EACH);
    for writer in writers {
        written.extend(writer.await.unwrap());
    }
    let sequences: Vec<i64> = seen.iter().map(|(sequence, _)| *sequence).collect();
    let expected: Vec<i64> = (1..=i64::try_from(WRITERS * EACH).unwrap()).collect();
    assert_eq!(
        sequences, expected,
        "a poll following next_after skipped, repeated or reordered an event"
    );
    let mut seen_ids: Vec<String> = seen.into_iter().map(|(_, id)| id).collect();
    seen_ids.sort();
    written.sort();
    assert_eq!(seen_ids, written, "an event was skipped");
}

/// Every case above, on `backend`, empty. The guard's cases come right
/// after the same-data case: an outbox that ignores the guard, and only
/// that, fails here on the guard rather than on what it lacks besides (a
/// stream its records' deletion does not purge, say).
pub async fn run(backend: &dyn Backend) {
    every_accessor_hands_out_the_same_data(backend).await;
    the_route_guard_is_checked_with_the_insert(backend).await;
    a_binding_begun_after_the_event_fails_the_number_guard(backend).await;
    a_binding_of_the_events_own_second_fails_the_guard(backend).await;
    a_first_onboarding_keeps_its_first_second(backend).await;
    the_skew_margin_applies_after_a_move_only(backend).await;
    an_undated_event_after_a_move_waits_out_the_window(backend).await;
    deleting_a_tenant_purges_its_stream(backend).await;
    a_tenants_inserts_commit_in_sequence_order(backend).await;
}
