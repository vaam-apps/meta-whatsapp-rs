//! The service's own number events ([`crate::number_events`], roadmap
//! M2d) over a backend's bundle: recorded through [`NumberEvents`] into
//! the bundle's outbox, woken through its notifier, they reach their
//! number's tenant (its poll, its streams, by id) and no other tenant; a
//! number its tenant does not hold under the event's WABA, a binding that
//! moved to another tenant, and a binding begun after the event's own (a
//! tenant deleted, created again under its id and bound again) give the
//! row to nobody; each event is recorded once per binding generation;
//! after a move the new holder's own event reaches it at once; and the
//! `Authorizer` records a binding's event for that binding's numbers
//! only (a previous binding's writes no row). The
//! services that record them (the admin attach and unbind, a tenant's
//! disconnect, a `190`) are the service's HTTP tests' (`tests/number_events.rs`).

use std::sync::Arc;

use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AppSecret;
use serde_json::{Value, json};

use super::live::{QUIET, WITHIN, authorizer, caller, next_frame};
use super::webhooks::{Holder, WatchedOutbox, hold};
use crate::backend::Backend;
use crate::events::EventIdKey;
use crate::live::{Frame, StreamQuery, Streams};
use crate::model::{BindingEpoch, TenantId};
use crate::number_events::{DisconnectReason, NumberEvent, NumberEvents};
use crate::outbox::{EventQuery, StoredEvent};
use crate::store::RecordStore;

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    a_number_event_reaches_its_numbers_tenant_only(backend).await;
    a_number_event_is_recorded_once_per_binding_generation(backend).await;
    a_number_event_of_a_binding_that_moved_reaches_nobody(backend).await;
    a_number_event_is_recorded_for_its_bindings_numbers_only(backend).await;
}

/// What records the events: over the bundle's outbox and notifier, ids
/// under a secret of this suite's.
pub fn number_events(backend: &dyn Backend) -> NumberEvents {
    NumberEvents::new(
        backend.outbox(),
        backend.notifier(),
        EventIdKey::from_app_secret(&AppSecret::new("app-secret-of-the-number-events-suite"))
            .unwrap(),
    )
}

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

fn number(pn: &str) -> PhoneNumberId {
    PhoneNumberId::new(pn).unwrap()
}

/// The epoch of `waba`'s binding, as the records hold it now.
async fn epoch(records: &dyn RecordStore, waba: &str) -> BindingEpoch {
    records
        .waba(&WabaId::new(waba).unwrap())
        .await
        .unwrap()
        .unwrap()
        .epoch()
}

/// Every event `tenant` polls.
async fn polled(backend: &dyn Backend, tenant_id: &str) -> Vec<StoredEvent> {
    backend
        .outbox()
        .page(&EventQuery {
            tenant: tenant(tenant_id),
            after: None,
            types: None,
            phone_number_id: None,
            limit: 100,
            max_bytes: crate::events::MAX_PAGE_DATA_BYTES,
        })
        .await
        .unwrap()
        .events
}

/// Roadmap M2d: `number_connected` for tenant A's two numbers reaches A
/// (its poll, exactly the data the module pins; its stream; the number's
/// stream, that number's only; each by id) and no stream, poll or lookup
/// of tenant B's; an event naming B's number under A's binding is kept
/// operator-only by the outbox's own check, A's nor B's. Decisive: the
/// row's tenant (the epoch's), and its guard naming the number (B's number
/// reaching A, or B).
pub async fn a_number_event_reaches_its_numbers_tenant_only(backend: &dyn Backend) {
    const A: Holder = Holder {
        id: "ne-tenant-a",
        waba: "102290129345001",
        pn: "106540352245011",
    };
    const A_SECOND: &str = "106540352245012";
    const B: Holder = Holder {
        id: "ne-tenant-b",
        waba: "102290129345002",
        pn: "106540352245013",
    };
    let records = backend.records();
    hold(records.as_ref(), A).await;
    records
        .bind_waba(
            &tenant(A.id),
            &WabaId::new(A.waba).unwrap(),
            &[number(A.pn), number(A_SECOND)],
        )
        .await
        .unwrap();
    hold(records.as_ref(), B).await;
    let authz = authorizer(backend);
    let (caller_a, caller_b) = (
        caller(backend, &authz, A.id).await,
        caller(backend, &authz, B.id).await,
    );
    let streams = Streams::new(
        backend.outbox(),
        backend.notifier(),
        super::live::woken_only(),
    );
    let mut of_a = streams
        .open_stream(&authz, &caller_a, StreamQuery::default())
        .await
        .unwrap();
    let mut on_as_second = streams
        .open_number_stream(&authz, &caller_a, number(A_SECOND), StreamQuery::default())
        .await
        .unwrap();
    let mut of_b = streams
        .open_stream(&authz, &caller_b, StreamQuery::default())
        .await
        .unwrap();
    let events = number_events(backend);
    let of_as_binding = epoch(records.as_ref(), A.waba).await;
    assert_eq!(
        events
            .record(
                NumberEvent::Connected,
                &of_as_binding,
                &[number(A.pn), number(A_SECOND)],
            )
            .await
            .unwrap(),
        2
    );
    let polled_a = polled(backend, A.id).await;
    let data: Vec<Value> = polled_a
        .iter()
        .map(|e| serde_json::from_str(&e.data).unwrap())
        .collect();
    assert_eq!(
        data,
        [
            json!({"event": "number_connected", "phone_number_id": A.pn, "waba_id": A.waba}),
            json!({"event": "number_connected", "phone_number_id": A_SECOND, "waba_id": A.waba}),
        ]
    );
    for (event, pn) in polled_a.iter().zip([A.pn, A_SECOND]) {
        assert_eq!(event.event_type, "number_connected");
        assert_eq!(
            (
                event.phone_number_id.as_ref().map(PhoneNumberId::as_str),
                event.waba_id.as_ref().map(WabaId::as_str)
            ),
            (Some(pn), Some(A.waba))
        );
        let outbox = backend.outbox();
        assert!(
            crate::events::get_event(outbox.as_ref(), &tenant(A.id), &event.id)
                .await
                .is_ok()
        );
        let refused = crate::events::get_event(outbox.as_ref(), &tenant(B.id), &event.id)
            .await
            .unwrap_err();
        assert_eq!((refused.status(), refused.code()), (404, "not_found"));
    }
    for (i, pn) in [A.pn, A_SECOND].iter().enumerate() {
        match next_frame(&mut of_a, WITHIN).await {
            Some(Frame::Event(event)) => {
                assert_eq!(event.event_type, "number_connected", "{i}");
                assert_eq!(
                    event.phone_number_id.as_ref().map(PhoneNumberId::as_str),
                    Some(*pn)
                );
            }
            other => panic!("a's stream, event {i}: got {other:?}"),
        }
    }
    match next_frame(&mut on_as_second, WITHIN).await {
        Some(Frame::Event(event)) => assert_eq!(
            event.phone_number_id.as_ref().map(PhoneNumberId::as_str),
            Some(A_SECOND)
        ),
        other => panic!("a's second number's stream: got {other:?}"),
    }
    // B's number under A's binding: the guard holds for nobody.
    assert_eq!(
        events
            .record(
                NumberEvent::ReconnectRequired,
                &of_as_binding,
                &[number(B.pn)]
            )
            .await
            .unwrap(),
        1,
        "recorded, operator-only"
    );
    assert_eq!(polled(backend, A.id).await.len(), 2, "A got B's number's");
    assert!(
        polled(backend, B.id).await.is_empty(),
        "B got another tenant's number event"
    );
    for (name, stream) in [
        ("b's stream", &mut of_b),
        ("a's stream", &mut of_a),
        ("a's second number", &mut on_as_second),
    ] {
        assert!(
            next_frame(stream, QUIET).await.is_none(),
            "{name}: an event it must not see"
        );
    }
}

/// An event is keyed by its type, its number and its binding's epoch: the
/// same event of the same binding is recorded once (a `190` repeated), and
/// a binding refreshed for its tenant (an attach again: a new generation)
/// records it again. Decisive: the generation in the key.
pub async fn a_number_event_is_recorded_once_per_binding_generation(backend: &dyn Backend) {
    const T: Holder = Holder {
        id: "ne-generations",
        waba: "102290129345003",
        pn: "106540352245014",
    };
    let records = backend.records();
    hold(records.as_ref(), T).await;
    let events = number_events(backend);
    let first = epoch(records.as_ref(), T.waba).await;
    let event = NumberEvent::ReconnectRequired;
    assert_eq!(
        events.record(event, &first, &[number(T.pn)]).await.unwrap(),
        1
    );
    assert_eq!(
        events.record(event, &first, &[number(T.pn)]).await.unwrap(),
        0,
        "the same binding's event again"
    );
    hold_again(records.as_ref(), T).await;
    let refreshed = epoch(records.as_ref(), T.waba).await;
    assert_eq!(refreshed.generation, first.generation + 1);
    assert_eq!(
        events
            .record(event, &refreshed, &[number(T.pn)])
            .await
            .unwrap(),
        1,
        "a new generation's"
    );
    let polled = polled(backend, T.id).await;
    assert_eq!(polled.len(), 2);
    assert_ne!(polled[0].id, polled[1].id);
}

/// Bind `holder`'s WABA and number to its tenant again (a refresh).
async fn hold_again(records: &dyn RecordStore, holder: Holder) {
    records
        .bind_waba(
            &tenant(holder.id),
            &WabaId::new(holder.waba).unwrap(),
            &[number(holder.pn)],
        )
        .await
        .unwrap();
}

/// A number event of a binding that moved gives its row to nobody: one of
/// tenant LEFT's, recorded once LEFT's WABA is ARRIVED's (`number_disconnected`
/// recorded late), reaches neither; one of a tenant deleted, created again
/// under its id and bound again in a later second reaches the new one
/// neither. And ARRIVED's own event, recorded in the second its binding
/// began after the move, reaches it at once. Decisive: the guard's tenant,
/// and its date, the second after the binding's start (dated in the
/// binding's own second, the new holder's own is refused after a move;
/// dated by the clock, recorded a second after the re-made binding began,
/// the tenant created again gets its namesake's).
pub async fn a_number_event_of_a_binding_that_moved_reaches_nobody(backend: &dyn Backend) {
    const LEFT: Holder = Holder {
        id: "ne-left",
        waba: "102290129345004",
        pn: "106540352245015",
    };
    const ARRIVED: Holder = Holder {
        id: "ne-arrived",
        ..LEFT
    };
    const AGAIN: Holder = Holder {
        id: "ne-again",
        waba: "102290129345005",
        pn: "106540352245016",
    };
    let records = backend.records();
    let events = number_events(backend);
    let disconnected = NumberEvent::Disconnected(DisconnectReason::OperatorUnbound);

    hold(records.as_ref(), LEFT).await;
    let of_left = epoch(records.as_ref(), LEFT.waba).await;
    let waba = WabaId::new(LEFT.waba).unwrap();
    assert!(records.unbind_waba(&waba).await.unwrap());
    hold(records.as_ref(), ARRIVED).await;
    let of_arrived = epoch(records.as_ref(), ARRIVED.waba).await;
    assert!(
        records
            .waba(&waba)
            .await
            .unwrap()
            .unwrap()
            .previous_tenant_unbound_at
            .is_some(),
        "a move"
    );
    assert_eq!(
        events
            .record(disconnected, &of_left, &[number(LEFT.pn)])
            .await
            .unwrap(),
        1
    );
    assert!(
        polled(backend, ARRIVED.id).await.is_empty(),
        "the new holder got the previous one's"
    );
    assert!(polled(backend, LEFT.id).await.is_empty());
    // The new holder's own, right after its binding: its own second.
    assert_eq!(
        events
            .record(NumberEvent::Connected, &of_arrived, &[number(ARRIVED.pn)])
            .await
            .unwrap(),
        1
    );
    let arrived = polled(backend, ARRIVED.id).await;
    assert_eq!(
        arrived
            .iter()
            .map(|e| e.event_type.as_str())
            .collect::<Vec<_>>(),
        ["number_connected"],
        "the new holder's own event, after a move"
    );

    // A tenant deleted, created again under its id, bound again later.
    hold(records.as_ref(), AGAIN).await;
    let of_the_first = epoch(records.as_ref(), AGAIN.waba).await;
    assert!(
        records
            .unbind_waba(&WabaId::new(AGAIN.waba).unwrap())
            .await
            .unwrap()
    );
    records.delete_tenant(&tenant(AGAIN.id)).await.unwrap();
    // The next second: the binding begins after the event's date.
    let next = of_the_first.attached_at.unix_timestamp() + 1;
    while time::OffsetDateTime::now_utc().unix_timestamp() < next {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    hold(records.as_ref(), AGAIN).await;
    // Recorded a second after the new binding began: an event dated by the
    // clock would be past that binding's own second, and reach it.
    let again = epoch(records.as_ref(), AGAIN.waba).await;
    let past = again.attached_at.unix_timestamp() + 1;
    while time::OffsetDateTime::now_utc().unix_timestamp() <= past {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        events
            .record(NumberEvent::Connected, &of_the_first, &[number(AGAIN.pn)])
            .await
            .unwrap(),
        1
    );
    assert!(
        polled(backend, AGAIN.id).await.is_empty(),
        "a tenant created again got its namesake's event"
    );
}

/// What the services record (`Authorizer::record_number_event`, roadmap
/// M2d): the event for the numbers its binding's tenant holds under its
/// WABA, read when it records. Once the WABA moved to another tenant, an
/// event of the previous binding writes no row at all (not even an
/// operator-only one naming the new holder's numbers); the new binding's
/// own is its tenant's. Decisive: the tenant filter on the numbers read
/// (the outbox's guard alone would keep such rows from every tenant, but
/// still write them). Found by M2d's review, where that filter survived
/// every other test.
pub async fn a_number_event_is_recorded_for_its_bindings_numbers_only(backend: &dyn Backend) {
    const LEFT: Holder = Holder {
        id: "ne-filter-left",
        waba: "102290129345006",
        pn: "106540352245017",
    };
    const ARRIVED: Holder = Holder {
        id: "ne-filter-arrived",
        ..LEFT
    };
    let records = backend.records();
    hold(records.as_ref(), LEFT).await;
    let of_left = epoch(records.as_ref(), LEFT.waba).await;
    assert!(
        records
            .unbind_waba(&WabaId::new(LEFT.waba).unwrap())
            .await
            .unwrap()
    );
    hold(records.as_ref(), ARRIVED).await;
    let of_arrived = epoch(records.as_ref(), ARRIVED.waba).await;
    let watched = Arc::new(WatchedOutbox::new(backend.outbox()));
    let authz = authorizer(backend).with_number_events(NumberEvents::new(
        watched.clone(),
        backend.notifier(),
        EventIdKey::from_app_secret(&AppSecret::new("app-secret-of-the-number-events-suite"))
            .unwrap(),
    ));
    authz
        .record_number_event(
            &of_left,
            NumberEvent::Disconnected(DisconnectReason::OperatorUnbound),
        )
        .await
        .unwrap();
    assert!(
        watched.inserted().is_empty(),
        "a previous binding's event wrote a row about the new holder's number"
    );
    authz
        .record_number_event(&of_arrived, NumberEvent::Connected)
        .await
        .unwrap();
    let inserted = watched.inserted();
    assert_eq!(inserted.len(), 1);
    assert_eq!(inserted[0].0.tenant, Some(tenant(ARRIVED.id)));
    assert_eq!(inserted[0].0.phone_number_id, Some(number(ARRIVED.pn)));
    assert!(inserted[0].1.is_some(), "recorded");
    assert_eq!(polled(backend, ARRIVED.id).await.len(), 1);
    assert!(polled(backend, LEFT.id).await.is_empty());
}
