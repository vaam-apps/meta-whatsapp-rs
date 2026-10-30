//! Roadmap S5e: the core's webhook pipeline ([`crate::inbound`]) over a
//! backend's bundle, with no listener and no HTTP layer: Meta's documented
//! payloads, signed as Meta signs them, handed to [`Pipeline::deliver`] as
//! bytes. M1.2's routing cases (docs/design/server.md, section 9): an
//! event reaches only the tenant the backend's records bind its number
//! (under the WABA it names) or, naming no number, its WABA to, since
//! before the event; an unowned number's event is operator-only, whatever
//! the payload says, and so are a stale binding's, a replay's, and the
//! types no tenant receives. The signature is checked before any store is
//! touched and before a turn to record is waited for; each event is
//! recorded once, under its dedup lease; the answer comes only once every
//! event is recorded; and what the service's metrics count is told to the
//! [`Observer`](crate::observe::Observer).
//!
//! The pipeline runs on a view of the backend (`Watched`) that passes
//! everything through to the backend's own stores and records what the
//! pipeline did with them: every call to the key/value store (the dedup
//! leases), and every row the pipeline asked the outbox for, with the
//! answer. Its outbox can also hold inserts at a gate, or fail them. What
//! each tenant polls, and what the inbox holds, are read from the
//! backend itself.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_rs::core::store::{
    ConversationKey, ConversationStore, Expiry, KvStore, StoreKey, Versioned,
};
use meta_whatsapp_rs::webhooks::{Claim, DedupGuard, WebhookPayload, sign};
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::sync::watch;

use super::messages::Told;
use crate::backend::{Backend, BackendKind};
use crate::inbound::{
    Delivered, Inbound, MAX_DELIVERIES_RECORDING, MAX_WEBHOOK_BODY_BYTES, Pipeline, RECORDING_WAIT,
    Refused,
};
use crate::model::{BindOutcome, TenantId};
use crate::outbox::{EventPage, EventQuery, NewEvent, Outbox, StoredEvent};
use crate::store::{
    IdempotencyRecords, Janitor, LeaderLock, RecordStore, SchemaMigrator, StoreResult,
};

/// The Meta app's secret, which signs the deliveries.
const APP_SECRET: &str = "app-secret-of-the-webhook-suite";
/// The previous one, still accepted while rotating.
const PREVIOUS_APP_SECRET: &str = "previous-app-secret-of-the-webhook-suite";
/// A secret that is not the app's.
const FORGERS_SECRET: &str = "a-forger-s-secret";

/// A tenant of a case: its id, its WABA and its number.
#[derive(Clone, Copy)]
struct Holder {
    id: &'static str,
    waba: &'static str,
    pn: &'static str,
}

/// The decisive case's tenant A: Meta's example WABA and number.
const ROUTE_A: Holder = Holder {
    id: "wh-route-a",
    waba: "102290129340398",
    pn: "106540352242922",
};
/// The decisive case's tenant B.
const ROUTE_B: Holder = Holder {
    id: "wh-route-b",
    waba: "102290129340399",
    pn: "106540352242923",
};
/// A WABA no tenant holds.
const UNBOUND_WABA: &str = "102290129349999";
/// A number no tenant holds.
const UNBOUND_PN: &str = "106540352249999";
/// The tenant of the signature case.
const SIGNED: Holder = Holder {
    id: "wh-signed",
    waba: "102290129340410",
    pn: "106540352242930",
};
/// The tenant of the dedup case.
const DEDUP: Holder = Holder {
    id: "wh-dedup",
    waba: "102290129340411",
    pn: "106540352242931",
};
/// The tenant of the recording case.
const RECORDED: Holder = Holder {
    id: "wh-recorded",
    waba: "102290129340412",
    pn: "106540352242932",
};
/// The tenant of the operator-only case.
const OPERATOR: Holder = Holder {
    id: "wh-operator",
    waba: "102290129340413",
    pn: "106540352242933",
};
/// The customer of Meta's examples (`wa_id`): the inbox's contact.
const CONTACT: &str = "16505551234";

/// A routing case: what it is, its payload, the tenant its row must
/// carry (`None`: operator-only), and the type and audience it is told
/// to the observer with.
type Case = (
    &'static str,
    Value,
    Option<&'static str>,
    (&'static str, &'static str),
);

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    routing_is_an_allow_list_with_no_listener(backend).await;
    a_delivery_is_verified_before_anything_is_stored(backend).await;
    an_event_is_recorded_once_under_its_dedup_lease(backend).await;
    the_answer_comes_only_once_every_event_is_recorded(backend).await;
    unknown_and_unparsed_bodies_are_acknowledged_as_operator_only_rows(backend).await;
}

/// Roadmap S5e, decisive: M1.2's routing cases, through the core's
/// pipeline with no listener. Routing is an allow-list: an event reaches a
/// tenant only when the backend's records bind the tenant its number
/// (under the WABA the event names) or, for an event naming no number, its
/// WABA. An unowned number's event is operator-only, even under a WABA the
/// tenant holds and whatever the payload says of a tenant; so is a number
/// the records put under another WABA than Meta says, and an event Meta
/// dated before the dedup lease's memory (a replay). The outbox stores
/// each row as routed (each tenant polls exactly its own), the inbox holds
/// only an owned number's message, and each row is told to the observer
/// by its audience. Decisive: routing an unowned number's event to a
/// tenant (the number's binding skipped for its WABA's, or the WABA the
/// event names not compared with the number's).
pub async fn routing_is_an_allow_list_with_no_listener(backend: &dyn Backend) {
    let records = backend.records();
    hold(records.as_ref(), ROUTE_A).await;
    hold(records.as_ref(), ROUTE_B).await;
    let watched = Watched::of(backend);
    let told = Arc::new(Told::default());
    let pipeline = pipeline(&watched, &told);
    let (a, b) = (Some(ROUTE_A.id), Some(ROUTE_B.id));
    let year_ago = now() - 365 * 24 * 60 * 60;
    // An unbound number's message whose payload names tenant A: a field
    // Meta does not send, which the routing never reads.
    let mut claims_a = text(UNBOUND_WABA, UNBOUND_PN, "wamid.route.7");
    claims_a["entry"][0]["tenant_id"] = json!(ROUTE_A.id);
    claims_a["entry"][0]["changes"][0]["value"]["metadata"]["tenant_id"] = json!(ROUTE_A.id);
    let cases: Vec<Case> = vec![
        (
            "A's number",
            text(ROUTE_A.waba, ROUTE_A.pn, "wamid.route.1"),
            a,
            ("message_received", "tenant"),
        ),
        (
            "B's number",
            text(ROUTE_B.waba, ROUTE_B.pn, "wamid.route.2"),
            b,
            ("message_received", "tenant"),
        ),
        (
            "an unbound number under A's WABA",
            text(ROUTE_A.waba, UNBOUND_PN, "wamid.route.3"),
            None,
            ("message_received", "operator"),
        ),
        (
            "an unbound number under an unbound WABA",
            text(UNBOUND_WABA, UNBOUND_PN, "wamid.route.4"),
            None,
            ("message_received", "operator"),
        ),
        (
            "A's number under B's WABA",
            text(ROUTE_B.waba, ROUTE_A.pn, "wamid.route.5"),
            None,
            ("message_received", "operator"),
        ),
        (
            "A's number under an unbound WABA",
            text(UNBOUND_WABA, ROUTE_A.pn, "wamid.route.6"),
            None,
            ("message_received", "operator"),
        ),
        (
            "an unbound number's message naming A as its tenant",
            claims_a,
            None,
            ("message_received", "operator"),
        ),
        (
            "A's number, dated before the dedup lease's memory (a replay)",
            dated(text(ROUTE_A.waba, ROUTE_A.pn, "wamid.route.8"), year_ago),
            None,
            ("message_received", "operator"),
        ),
        (
            "a status of A's number",
            status(ROUTE_A.waba, ROUTE_A.pn, "wamid.route.1"),
            a,
            ("status_updated", "tenant"),
        ),
        (
            "A's WABA, no number",
            template_approved(ROUTE_A.waba),
            a,
            ("template_status_updated", "tenant"),
        ),
        (
            "an unbound WABA, no number",
            template_approved(UNBOUND_WABA),
            None,
            ("template_status_updated", "operator"),
        ),
        (
            "an error of B's number",
            error(ROUTE_B.waba, ROUTE_B.pn),
            b,
            ("error_reported", "tenant"),
        ),
    ];
    for (i, (case, payload, owner, _)) in cases.iter().enumerate() {
        let report = deliver(&pipeline, &body(payload))
            .await
            .unwrap_or_else(|refused| panic!("{case}: refused {refused:?}"));
        assert_eq!(
            (report.delivered, report.duplicates),
            (1, 0),
            "{case}: one event, handed to the sink"
        );
        let inserted = watched.outbox.inserted();
        assert_eq!(inserted.len(), i + 1, "{case}: one row asked for");
        let (row, sequence) = &inserted[i];
        assert!(sequence.is_some(), "{case}: recorded");
        assert_eq!(
            row.tenant.as_ref().map(TenantId::as_str),
            *owner,
            "{case}: the row's tenant"
        );
        assert_eq!(
            row.route_guard.is_some(),
            owner.is_some(),
            "{case}: a tenant's row is guarded by the binding it was routed by"
        );
    }
    // As stored: the outbox checked each row's routing again, and each
    // tenant polls exactly its own.
    assert_eq!(
        types(&polled(backend, ROUTE_A.id).await),
        [
            "message_received",
            "status_updated",
            "template_status_updated"
        ],
        "A polls its number's message and status, and its WABA's template"
    );
    assert_eq!(
        types(&polled(backend, ROUTE_B.id).await),
        ["message_received", "error_reported"],
        "B polls its number's message and error"
    );
    for (tenant, holder) in [(ROUTE_A.id, ROUTE_A), (ROUTE_B.id, ROUTE_B)] {
        for event in polled(backend, tenant).await {
            assert_eq!(event.tenant.as_ref().map(TenantId::as_str), Some(tenant));
            assert_eq!(
                event.waba_id.as_ref().map(WabaId::as_str),
                Some(holder.waba)
            );
        }
    }
    // The inbox records only an owned number's message.
    assert_eq!(inbox(backend, ROUTE_A.pn).await, 1, "A's own message only");
    assert_eq!(inbox(backend, ROUTE_B.pn).await, 1, "B's own message");
    assert_eq!(inbox(backend, UNBOUND_PN).await, 0, "no unowned number's");
    // Counted by audience, as the service's metrics count them.
    let audiences: Vec<(&str, &str)> = cases.iter().map(|(_, _, _, told)| *told).collect();
    assert_eq!(told.take_webhook_events(), audiences);
    assert!(told.take_webhook_failures().is_empty());
    assert!(told.take_webhook_duplicates().is_empty());
}

/// A delivery's signature is checked against every app secret before
/// anything else: unsigned, malformed, forged, or signed over other bytes,
/// it is refused `401` with the dedup leases' store untouched, no row
/// asked for and nothing in the inbox (a forged body that is not JSON
/// included: it never becomes an `unparsed` row), and a forged delivery is
/// refused at once while every turn to record is held, waiting for none.
/// The previous app secret is accepted, a body of 3 MiB is read and one
/// byte more is `413`, with nothing stored. Decisive: the signature checked
/// after the turn (or only by the library's handler), and either secret
/// dropped.
pub async fn a_delivery_is_verified_before_anything_is_stored(backend: &dyn Backend) {
    hold(backend.records().as_ref(), SIGNED).await;
    let watched = Watched::of(backend);
    let told = Arc::new(Told::default());
    let pipeline = Arc::new(pipeline(&watched, &told));
    let message = body(&text(SIGNED.waba, SIGNED.pn, "wamid.signed.1"));
    let not_json = b"not even JSON".to_vec();
    let refusals: Vec<(&str, Option<String>, &[u8])> = vec![
        ("unsigned", None, &message),
        ("an empty signature", Some(String::new()), &message),
        (
            "a malformed signature",
            Some(format!("sha256={}", "zz".repeat(32))),
            &message,
        ),
        (
            "another algorithm",
            Some(format!("sha1={}", "ab".repeat(32))),
            &message,
        ),
        (
            "signed by another secret",
            Some(sign(&AppSecret::new(FORGERS_SECRET), &message)),
            &message,
        ),
        (
            "signed over other bytes",
            Some(sign(&AppSecret::new(APP_SECRET), b"{}")),
            &message,
        ),
        (
            "a forged body that is not JSON",
            Some(sign(&AppSecret::new(FORGERS_SECRET), &not_json)),
            &not_json,
        ),
    ];
    for (case, signature, payload) in &refusals {
        let refused = pipeline
            .deliver(signature.as_deref(), Bytes::copy_from_slice(payload))
            .await;
        let Err(refused) = refused else {
            panic!("{case}: acknowledged {refused:?}");
        };
        assert!(
            matches!(refused, Refused::Unauthenticated),
            "{case}: {refused:?}"
        );
        assert_eq!(
            (refused.status(), refused.outcome()),
            (401, "unauthenticated"),
            "{case}"
        );
        assert_eq!(watched.kv.calls(), 0, "{case}: the dedup store touched");
        assert_eq!(watched.outbox.attempts(), 0, "{case}: a row asked for");
    }
    assert_eq!(inbox(backend, SIGNED.pn).await, 0);
    assert!(polled(backend, SIGNED.id).await.is_empty());

    // Every turn to record held (each delivery parked at the outbox): a
    // forged delivery is refused at once, and touches nothing.
    watched.outbox.close_gate();
    let parked: Vec<_> = (0..MAX_DELIVERIES_RECORDING)
        .map(|i| {
            let pipeline = pipeline.clone();
            let payload = body(&text(SIGNED.waba, SIGNED.pn, &format!("wamid.parked.{i}")));
            tokio::spawn(async move { deliver(&pipeline, &payload).await })
        })
        .collect();
    watched.outbox.parked(MAX_DELIVERIES_RECORDING).await;
    let kv_calls = watched.kv.calls();
    let forged = tokio::time::timeout(
        RECORDING_WAIT / 2,
        pipeline.deliver(
            Some(&sign(&AppSecret::new(FORGERS_SECRET), &message)),
            Bytes::copy_from_slice(&message),
        ),
    )
    .await;
    assert!(
        matches!(forged, Ok(Err(Refused::Unauthenticated))),
        "a forged delivery waits for no turn to record: {forged:?}"
    );
    assert_eq!(
        watched.kv.calls(),
        kv_calls,
        "the forged one touched the dedup store"
    );
    watched.outbox.open_gate();
    for delivery in parked {
        let report = delivery.await.unwrap().unwrap();
        assert_eq!(report.delivered, 1);
    }
    assert_eq!(
        polled(backend, SIGNED.id).await.len(),
        MAX_DELIVERIES_RECORDING
    );

    // The previous app secret still verifies (a rotation).
    let rotated = body(&text(SIGNED.waba, SIGNED.pn, "wamid.signed.previous"));
    let report = pipeline
        .deliver(
            Some(&sign(&AppSecret::new(PREVIOUS_APP_SECRET), &rotated)),
            Bytes::copy_from_slice(&rotated),
        )
        .await
        .unwrap();
    assert_eq!(report.delivered, 1, "signed by the previous secret");

    // 3 MiB is read; one byte more is refused before it is parsed or any
    // store touched.
    let whole = padded(
        &body(&text(SIGNED.waba, SIGNED.pn, "wamid.signed.3mib")),
        MAX_WEBHOOK_BODY_BYTES,
    );
    assert_eq!(
        deliver(&pipeline, &whole).await.unwrap().delivered,
        1,
        "3 MiB"
    );
    let over = padded(
        &body(&text(SIGNED.waba, SIGNED.pn, "wamid.signed.over")),
        MAX_WEBHOOK_BODY_BYTES + 1,
    );
    let (kv_calls, attempts) = (watched.kv.calls(), watched.outbox.attempts());
    let refused = deliver(&pipeline, &over).await.unwrap_err();
    assert!(matches!(refused, Refused::TooLarge), "{refused:?}");
    assert_eq!(
        (refused.status(), refused.outcome()),
        (413, "payload_too_large")
    );
    assert_eq!(
        (watched.kv.calls(), watched.outbox.attempts()),
        (kv_calls, attempts),
        "3 MiB + 1 byte: nothing stored"
    );
    assert_eq!(
        polled(backend, SIGNED.id).await.len(),
        MAX_DELIVERIES_RECORDING + 2
    );
    assert_eq!(
        inbox(backend, SIGNED.pn).await,
        MAX_DELIVERIES_RECORDING + 2
    );
    assert!(told.take_webhook_failures().is_empty());
}

/// Each event is recorded once, under its dedup lease (the library's, in
/// the backend's key/value store, shared by every replica): the same body
/// again is a duplicate the lease recognises before the outbox is asked
/// (told as `dedup`), and an event whose lease another request holds is
/// refused `503 in_flight` with nothing recorded, then recorded once its
/// lease is released. Decisive: the dedup lease skipped.
pub async fn an_event_is_recorded_once_under_its_dedup_lease(backend: &dyn Backend) {
    hold(backend.records().as_ref(), DEDUP).await;
    let watched = Watched::of(backend);
    let told = Arc::new(Told::default());
    let pipeline = pipeline(&watched, &told);

    let first = body(&text(DEDUP.waba, DEDUP.pn, "wamid.dedup.1"));
    let report = deliver(&pipeline, &first).await.unwrap();
    assert_eq!((report.delivered, report.duplicates), (1, 0));
    assert!(watched.kv.calls() > 0, "the event's lease was claimed");
    let report = deliver(&pipeline, &first).await.unwrap();
    assert_eq!(
        (report.delivered, report.duplicates),
        (0, 1),
        "the same body again: a duplicate, acknowledged"
    );
    assert_eq!(
        watched.outbox.attempts(),
        1,
        "the lease recognised it before the outbox was asked"
    );
    assert_eq!(told.take_webhook_duplicates(), [("dedup", 1)]);
    assert_eq!(polled(backend, DEDUP.id).await.len(), 1, "one row");
    assert_eq!(inbox(backend, DEDUP.pn).await, 1, "one message");

    // Another request holds the event's lease (another replica is
    // delivering it): 503, nothing recorded; once released, recorded once.
    let second = body(&text(DEDUP.waba, DEDUP.pn, "wamid.dedup.2"));
    let [event] = WebhookPayload::from_slice(&second)
        .unwrap()
        .into_events()
        .try_into()
        .unwrap();
    let guard = DedupGuard::new(backend.kv());
    let Claim::Acquired(ticket) = guard.claim(&event).await.unwrap() else {
        panic!("the lease of a new event");
    };
    let attempts = watched.outbox.attempts();
    let refused = deliver(&pipeline, &second).await.unwrap_err();
    assert!(matches!(refused, Refused::InFlight), "{refused:?}");
    assert_eq!((refused.status(), refused.outcome()), (503, "in_flight"));
    assert_eq!(watched.outbox.attempts(), attempts, "nothing recorded");
    assert_eq!(inbox(backend, DEDUP.pn).await, 1, "nothing in the inbox");
    assert!(guard.release(&ticket).await.unwrap());
    let report = deliver(&pipeline, &second).await.unwrap();
    assert_eq!((report.delivered, report.duplicates), (1, 0));
    assert_eq!(polled(backend, DEDUP.id).await.len(), 2);
    assert_eq!(inbox(backend, DEDUP.pn).await, 2);
    assert_eq!(
        told.take_webhook_events(),
        [
            ("message_received", "tenant"),
            ("message_received", "tenant")
        ]
    );
    assert!(told.take_webhook_duplicates().is_empty());
}

/// The answer comes only once every event is recorded: a delivery whose
/// row is still being inserted has not answered, and answers once it is
/// stored; an outbox that fails is `500 failed` (told as `outbox`), a busy
/// one `503 busy` (told as `outbox_busy`), each with the event's lease
/// released, so that Meta's redelivery records it, once, its message in
/// the inbox once (the inbox is written before the outbox). Decisive: the
/// answer given before the recording, or whatever it did.
pub async fn the_answer_comes_only_once_every_event_is_recorded(backend: &dyn Backend) {
    hold(backend.records().as_ref(), RECORDED).await;
    let watched = Watched::of(backend);
    let told = Arc::new(Told::default());
    let pipeline = Arc::new(pipeline(&watched, &told));

    // Held at the outbox: no answer yet, and nothing to poll.
    watched.outbox.close_gate();
    let held = body(&text(RECORDED.waba, RECORDED.pn, "wamid.recorded.1"));
    let delivery = {
        let pipeline = pipeline.clone();
        tokio::spawn(async move { deliver(&pipeline, &held).await })
    };
    watched.outbox.parked(1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !delivery.is_finished(),
        "answered before its row was recorded"
    );
    assert!(polled(backend, RECORDED.id).await.is_empty());
    watched.outbox.open_gate();
    let report = delivery.await.unwrap().unwrap();
    assert_eq!(report.delivered, 1);
    assert_eq!(
        polled(backend, RECORDED.id).await.len(),
        1,
        "recorded, then answered"
    );

    // The outbox fails, then is busy: refused, and Meta's redelivery is
    // recorded once.
    for (mode, status, outcome, stage) in [
        (Insert::Fail, 500, "failed", "outbox"),
        (Insert::Busy, 503, "busy", "outbox_busy"),
    ] {
        let wamid = format!("wamid.recorded.{outcome}");
        let payload = body(&text(RECORDED.waba, RECORDED.pn, &wamid));
        watched.outbox.set(mode);
        let refused = deliver(&pipeline, &payload).await.unwrap_err();
        assert_eq!(
            (refused.status(), refused.outcome()),
            (status, outcome),
            "{refused:?}"
        );
        assert_eq!(told.take_webhook_failures(), [stage]);
        watched.outbox.set(Insert::Pass);
        let report = deliver(&pipeline, &payload)
            .await
            .unwrap_or_else(|refused| panic!("{outcome}: the redelivery refused {refused:?}"));
        assert_eq!(
            (report.delivered, report.duplicates),
            (1, 0),
            "{outcome}: the lease was released"
        );
    }
    assert_eq!(polled(backend, RECORDED.id).await.len(), 3, "each once");
    assert_eq!(
        inbox(backend, RECORDED.pn).await,
        3,
        "each message once, the failed attempts' included"
    );
    assert_eq!(told.take_webhook_events().len(), 3);
}

/// A signed body is acknowledged whatever it holds: a field no library
/// types (`unknown`) and a body that is not a webhook (`unparsed`) are
/// operator-only rows, even on a WABA and a number a tenant holds, and no
/// tenant polls them; a body that is not a webhook, delivered again, is
/// one row (keyed by the body and its position in it). Decisive: the type
/// allow-list, and the keyless events' key.
pub async fn unknown_and_unparsed_bodies_are_acknowledged_as_operator_only_rows(
    backend: &dyn Backend,
) {
    hold(backend.records().as_ref(), OPERATOR).await;
    let watched = Watched::of(backend);
    let told = Arc::new(Told::default());
    let pipeline = pipeline(&watched, &told);
    let not_a_webhook = br#"{"not": "a webhook envelope"}"#.to_vec();
    let bodies: Vec<(&str, Vec<u8>, usize)> = vec![
        (
            "unknown",
            body(&unknown_field(OPERATOR.waba, OPERATOR.pn)),
            0,
        ),
        ("unparsed", not_a_webhook.clone(), 1),
        ("unparsed", b"not even JSON".to_vec(), 1),
    ];
    for (kind, payload, unparsed) in &bodies {
        let report = deliver(&pipeline, payload)
            .await
            .unwrap_or_else(|refused| panic!("{kind}: refused {refused:?}"));
        assert_eq!(
            (report.delivered, report.unparsed),
            (1, *unparsed),
            "{kind}"
        );
    }
    let inserted = watched.outbox.inserted();
    let kinds: Vec<&str> = inserted
        .iter()
        .map(|(row, _)| row.event_type.as_str())
        .collect();
    assert_eq!(kinds, ["unknown", "unparsed", "unparsed"]);
    for (row, sequence) in &inserted {
        assert!(sequence.is_some());
        assert_eq!(row.tenant, None, "{}: operator-only", row.event_type);
    }
    assert!(polled(backend, OPERATOR.id).await.is_empty());
    assert_eq!(
        told.take_webhook_events(),
        [
            ("unknown", "operator"),
            ("unparsed", "operator"),
            ("unparsed", "operator")
        ]
    );
    // The same body again, within the keyless window: one row.
    let report = deliver(&pipeline, &not_a_webhook).await.unwrap();
    assert_eq!(
        report.delivered, 1,
        "handed to the sink: no lease to skip it by"
    );
    assert_eq!(
        watched.outbox.inserted().last().unwrap().1,
        None,
        "not recorded again"
    );
    assert_eq!(told.take_webhook_duplicates(), [("outbox", 1)]);
    assert!(told.take_webhook_events().is_empty());
}

/// Create `holder`'s tenant and bind its WABA and number to it.
async fn hold(records: &dyn RecordStore, holder: Holder) {
    let tenant = TenantId::parse(holder.id).unwrap();
    records.create_tenant(&tenant, "").await.unwrap();
    let outcome = records
        .bind_waba(
            &tenant,
            &WabaId::new(holder.waba).unwrap(),
            &[PhoneNumberId::new(holder.pn).unwrap()],
        )
        .await
        .unwrap();
    assert_eq!(outcome, BindOutcome::Bound, "{}", holder.id);
}

/// The pipeline over `watched`, verifying with the app's secret and the
/// previous one, telling `told`.
fn pipeline(watched: &Watched, told: &Arc<Told>) -> Pipeline {
    let inbound = Inbound::new(
        vec![
            AppSecret::new(APP_SECRET),
            AppSecret::new(PREVIOUS_APP_SECRET),
        ],
        VerifyToken::new("verify-token-of-the-webhook-suite"),
    )
    .unwrap();
    Pipeline::new(inbound, watched, told.clone())
}

/// `payload` delivered as Meta delivers it: signed with the app's secret.
async fn deliver(pipeline: &Pipeline, payload: &[u8]) -> Result<Delivered, Refused> {
    pipeline
        .deliver(
            Some(&sign(&AppSecret::new(APP_SECRET), payload)),
            Bytes::copy_from_slice(payload),
        )
        .await
}

/// Every event `tenant` polls.
async fn polled(backend: &dyn Backend, tenant: &str) -> Vec<StoredEvent> {
    backend
        .outbox()
        .page(&EventQuery {
            tenant: TenantId::parse(tenant).unwrap(),
            after: None,
            types: None,
            phone_number_id: None,
            limit: 100,
            max_bytes: 8 * 1024 * 1024,
        })
        .await
        .unwrap()
        .events
}

/// The events' types, in order.
fn types(events: &[StoredEvent]) -> Vec<&str> {
    events.iter().map(|e| e.event_type.as_str()).collect()
}

/// The messages the inbox holds for [`CONTACT`] on `pn`.
async fn inbox(backend: &dyn Backend, pn: &str) -> usize {
    backend
        .conversations()
        .messages(
            &ConversationKey::new(PhoneNumberId::new(pn).unwrap(), CONTACT),
            None,
            100,
        )
        .await
        .unwrap()
        .len()
}

/// Now, in Unix seconds.
fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

/// The date of an event Meta sends now: the next second. An event reaches
/// a tenant only when its binding began in a second before the event's,
/// and a case binds, then delivers, within a second.
fn sent_now() -> i64 {
    now() + 1
}

/// `payload` with every `timestamp` (Meta's strings of Unix seconds) and
/// every entry's `time` set to `at`.
fn dated(mut payload: Value, at: i64) -> Value {
    fn walk(value: &mut Value, at: i64) {
        match value {
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    match (key.as_str(), &*item) {
                        ("timestamp", Value::String(_)) => *item = json!(at.to_string()),
                        ("timestamp" | "time", Value::Number(_)) => *item = json!(at),
                        _ => walk(item, at),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| walk(item, at)),
            _ => {}
        }
    }
    walk(&mut payload, at);
    payload
}

/// webhooks/reference/messages/text, its example: a customer's text to
/// `pn` of `waba`, message id `wamid`, sent now.
fn text(waba: &str, pn: &str, wamid: &str) -> Value {
    json!({
        "object": "whatsapp_business_account",
        "entry": [{
            "id": waba,
            "changes": [{
                "value": {
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": pn},
                    "contacts": [{"profile": {"name": "Sheena Nelson"}, "wa_id": CONTACT}],
                    "messages": [{
                        "from": CONTACT,
                        "id": wamid,
                        "timestamp": sent_now().to_string(),
                        "type": "text",
                        "text": {"body": "Does it come in another color?"}
                    }]
                },
                "field": "messages"
            }]
        }]
    })
}

/// webhooks/reference/messages/status, its `sent` example: the status of
/// message `wamid` sent from `pn` of `waba`, now.
fn status(waba: &str, pn: &str, wamid: &str) -> Value {
    json!({
        "object": "whatsapp_business_account",
        "entry": [{
            "id": waba,
            "changes": [{
                "value": {
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": pn},
                    "statuses": [{
                        "id": wamid,
                        "status": "sent",
                        "timestamp": sent_now().to_string(),
                        "recipient_id": CONTACT,
                        "conversation": {
                            "id": "72b14d6bd5407799e66f64d1b338e567",
                            "expiration_timestamp": "1750116480",
                            "origin": {"type": "marketing"}
                        },
                        "pricing": {
                            "billable": true,
                            "pricing_model": "PMP",
                            "type": "regular",
                            "category": "marketing"
                        }
                    }]
                },
                "field": "messages"
            }]
        }]
    })
}

/// `webhooks/reference/message_template_status_update`, its `APPROVED`
/// example: a WABA-level field, naming no number, now.
fn template_approved(waba: &str) -> Value {
    json!({
        "entry": [{
            "id": waba,
            "time": sent_now(),
            "changes": [{
                "value": {
                    "event": "APPROVED",
                    "message_template_id": 1689556908129832_u64,
                    "message_template_name": "order_confirmation",
                    "message_template_language": "en-US",
                    "reason": "NONE",
                    "message_template_category": "UTILITY"
                },
                "field": "message_template_status_update"
            }]
        }],
        "object": "whatsapp_business_account"
    })
}

/// webhooks/reference/messages/errors, its example: an error of `pn` of
/// `waba`, undated (and keyless: the library gives it no dedup key).
fn error(waba: &str, pn: &str) -> Value {
    json!({
        "object": "whatsapp_business_account",
        "entry": [{
            "id": waba,
            "changes": [{
                "value": {
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": pn},
                    "errors": [{
                        "code": 130429,
                        "title": "Rate limit hit",
                        "message": "Rate limit hit",
                        "error_data": {
                            "details": "Message failed to send because there were too many messages sent from this phone number in a short period of time"
                        },
                        "href": "/documentation/business-messaging/whatsapp/support/error-codes"
                    }]
                },
                "field": "messages"
            }]
        }]
    })
}

/// A field no library types yet, on `waba`, naming `pn`: an `unknown`
/// event.
fn unknown_field(waba: &str, pn: &str) -> Value {
    json!({
        "object": "whatsapp_business_account",
        "entry": [{
            "id": waba,
            "time": sent_now(),
            "changes": [{
                "field": "a_field_meta_adds_later",
                "value": {"metadata": {"phone_number_id": pn}, "note": "anything"}
            }]
        }]
    })
}

/// `payload` as bytes.
fn body(payload: &Value) -> Vec<u8> {
    serde_json::to_vec(payload).unwrap()
}

/// `body` padded with trailing whitespace (still the same JSON) to `len`
/// bytes.
fn padded(body: &[u8], len: usize) -> Vec<u8> {
    assert!(body.len() <= len);
    let mut padded = body.to_vec();
    padded.resize(len, b' ');
    padded
}

/// What the outbox does with the next inserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Insert {
    /// Passes them to the backend's outbox.
    Pass,
    /// Fails them, as a broken database does.
    Fail,
    /// Refuses them as contention (`StorageError::Busy`).
    Busy,
}

/// The backend's outbox, recording every row asked for and the answer,
/// able to hold inserts at a gate (while it is closed) and to fail them.
struct WatchedOutbox {
    inner: Arc<dyn Outbox>,
    mode: Mutex<Insert>,
    gate: watch::Sender<bool>,
    waiting: AtomicUsize,
    attempts: AtomicUsize,
    inserted: Mutex<Vec<(NewEvent, Option<i64>)>>,
}

impl WatchedOutbox {
    fn new(inner: Arc<dyn Outbox>) -> Self {
        Self {
            inner,
            mode: Mutex::new(Insert::Pass),
            gate: watch::Sender::new(true),
            waiting: AtomicUsize::new(0),
            attempts: AtomicUsize::new(0),
            inserted: Mutex::new(Vec::new()),
        }
    }

    /// Every row the backend's outbox recorded or met again, and its
    /// answer (`None`: already stored).
    fn inserted(&self) -> Vec<(NewEvent, Option<i64>)> {
        self.inserted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Every insert asked for, held, failed or passed.
    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    fn set(&self, mode: Insert) {
        *self.mode.lock().unwrap_or_else(PoisonError::into_inner) = mode;
    }

    fn close_gate(&self) {
        self.gate.send_replace(false);
    }

    fn open_gate(&self) {
        self.gate.send_replace(true);
    }

    /// Until `n` inserts wait at the closed gate (10 s at most).
    async fn parked(&self, n: usize) {
        let parked = tokio::time::timeout(Duration::from_secs(10), async {
            while self.waiting.load(Ordering::SeqCst) < n {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(parked.is_ok(), "{n} inserts never reached the outbox");
    }
}

#[async_trait]
impl Outbox for WatchedOutbox {
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        let mut gate = self.gate.subscribe();
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let _ = gate.wait_for(|open| *open).await;
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        let mode = *self.mode.lock().unwrap_or_else(PoisonError::into_inner);
        match mode {
            Insert::Fail => {
                return Err(StorageError::Backend(anyhow::anyhow!(
                    "the suite's outbox failure"
                )));
            }
            Insert::Busy => return Err(StorageError::Busy),
            Insert::Pass => {}
        }
        let answer = self.inner.insert(event).await?;
        self.inserted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((event.clone(), answer));
        Ok(answer)
    }

    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage> {
        self.inner.page(query).await
    }

    async fn purge(&self, older_than: Duration) -> StoreResult<u64> {
        self.inner.purge(older_than).await
    }
}

/// The backend's key/value store, counting every call.
#[derive(Debug)]
struct WatchedKv {
    inner: Arc<dyn KvStore>,
    calls: AtomicUsize,
}

impl WatchedKv {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn call(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl KvStore for WatchedKv {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        self.call();
        self.inner.get(key).await
    }

    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.call();
        self.inner.put(key, value, expiry).await
    }

    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.call();
        self.inner.put_if_absent(key, value, expiry).await
    }

    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.call();
        self.inner
            .compare_and_swap(key, expected, new, expiry)
            .await
    }

    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.call();
        self.inner.delete(key).await
    }
}

/// A view of a backend for the pipeline: its records and conversation
/// store as they are, its key/value store and outbox watched. The pipeline
/// reaches no other port.
struct Watched {
    records: Arc<dyn RecordStore>,
    conversations: Arc<dyn ConversationStore>,
    kv: Arc<WatchedKv>,
    outbox: Arc<WatchedOutbox>,
}

impl Watched {
    fn of(backend: &dyn Backend) -> Self {
        Self {
            records: backend.records(),
            conversations: backend.conversations(),
            kv: Arc::new(WatchedKv {
                inner: backend.kv(),
                calls: AtomicUsize::new(0),
            }),
            outbox: Arc::new(WatchedOutbox::new(backend.outbox())),
        }
    }
}

#[async_trait]
impl Backend for Watched {
    fn kind(&self) -> BackendKind {
        BackendKind::Other("conformance-webhooks")
    }

    fn records(&self) -> Arc<dyn RecordStore> {
        self.records.clone()
    }

    fn idempotency(&self) -> Arc<dyn IdempotencyRecords> {
        unreachable!("the webhook pipeline keeps no idempotency key")
    }

    fn outbox(&self) -> Arc<dyn Outbox> {
        self.outbox.clone()
    }

    fn leader_lock(&self) -> Arc<dyn LeaderLock> {
        unreachable!("the webhook pipeline takes no lock")
    }

    fn janitor(&self) -> Arc<dyn Janitor> {
        unreachable!("the webhook pipeline purges nothing")
    }

    fn migrator(&self) -> Arc<dyn SchemaMigrator> {
        unreachable!("the webhook pipeline migrates nothing")
    }

    fn kv(&self) -> Arc<dyn KvStore> {
        self.kv.clone()
    }

    fn conversations(&self) -> Arc<dyn ConversationStore> {
        self.conversations.clone()
    }

    async fn close(&self) {}
}
