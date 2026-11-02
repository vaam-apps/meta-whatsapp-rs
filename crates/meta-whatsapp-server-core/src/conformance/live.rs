//! [`EventNotifier`] and the live streams over a backend's bundle
//! (roadmap M2b; design section 4.5, acceptance tests M2.2, M2.3 and
//! M2.5): the notifier wakes its tenant's subscriptions only, and every
//! handle the bundle hands out is the same; a stream sends what the outbox
//! holds after its cursor and resumes exactly after it; it sends only its
//! tenant's events, never an `unknown` or an `unparsed` one; a lost
//! notification still delivers (the re-poll); its cursor is checked before
//! it starts (`410`, `422`); streams are bounded per tenant and replica; a
//! slow client is dropped with its buffer bounded, and its place and
//! buffer given back whether or not it ever reads again; a stream ends
//! once its key is revoked or its tenant suspended; a shutdown ends every
//! stream; an event is read by id in its tenant's stream only; a stream
//! behind a purge ends lagged. What crosses replicas (one database, two
//! pools) is the Postgres backend's own live test.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{TokenVault, VaultKey, VaultKeys};
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::core::testing::ScriptedTransport;

use super::outbox::{bind, row};
use super::webhooks::{Holder, body, deliver, hold, plain_pipeline, text, unknown_field};
use crate::ServiceError;
use crate::admin::mint;
use crate::authz::{Authorizer, Caller};
use crate::backend::Backend;
use crate::live::{Frame, LiveSettings, LiveStream, StreamQuery, Streams};
use crate::model::{KeyOwner, KeyScope, Scope, TenantId, TenantStatus};
use crate::notify::{EventNotifier, Hub, Subscription, Wakeup};
use crate::outbox::NewEvent;
use crate::store::StoreResult;

/// How long a wake-up may take to reach a stream: M2.2's two seconds.
pub const WITHIN: Duration = Duration::from_secs(2);

/// How long a case waits to see that nothing comes.
pub(super) const QUIET: Duration = Duration::from_millis(400);

/// Settings where only a wake-up reads the outbox in time: the re-poll and
/// the heartbeat far off.
pub fn woken_only() -> LiveSettings {
    LiveSettings {
        heartbeat: Duration::from_secs(3600),
        repoll: Duration::from_secs(3600),
        ..LiveSettings::default()
    }
}

/// Every case, on one backend. The purge's case runs last: it purges every
/// stream of the backend.
pub async fn run(backend: &dyn Backend) {
    the_notifier_wakes_its_tenants_subscriptions_only(backend).await;
    a_stream_sends_what_follows_its_cursor_and_resumes_exactly_after_it(backend).await;
    a_stream_sends_only_its_tenants_events(backend).await;
    the_reviewed_types_reach_their_numbers_tenants_streams_only(backend).await;
    no_unknown_or_unparsed_event_reaches_a_stream(backend).await;
    a_lost_notification_still_delivers(backend).await;
    a_cursor_past_the_newest_sequence_is_refused_before_the_stream(backend).await;
    streams_are_bounded_per_tenant_and_per_replica(backend).await;
    a_slow_client_is_dropped_with_its_buffer_bounded(backend).await;
    a_stream_ends_once_its_key_or_tenant_is_no_longer_admitted(backend).await;
    shutting_down_ends_every_stream(backend).await;
    an_event_is_read_by_id_in_its_tenants_stream_only(backend).await;
    a_number_stream_needs_the_tenants_number(backend).await;
    a_stream_behind_a_purge_ends_lagged(backend).await;
}

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

/// An [`Authorizer`] over the backend's records (a vault and a Graph
/// client it never uses: streams need no token).
pub(super) fn authorizer(backend: &dyn Backend) -> Authorizer {
    let vault = TokenVault::new(
        backend.kv(),
        VaultKeys::new(VaultKey::generate("live-suite").unwrap()),
    )
    .unwrap();
    let client = Client::builder()
        .transport(ScriptedTransport::new())
        .build()
        .unwrap();
    Authorizer::new(backend.records(), vault, client).unwrap()
}

/// An `events` key's caller for `tenant`, which `authz` admits.
pub(super) async fn caller(backend: &dyn Backend, authz: &Authorizer, tenant_id: &str) -> Caller {
    let records = backend.records();
    let _ = records.create_tenant(&tenant(tenant_id), "").await;
    let key = mint(
        records.as_ref(),
        KeyOwner::Tenant(tenant(tenant_id)),
        vec![Scope::Events],
        String::new(),
        None,
    )
    .await
    .unwrap()
    .0;
    authz
        .tenant_caller(
            Some(&format!("Bearer {}", key.expose_key())),
            None,
            Scope::Events,
        )
        .await
        .unwrap()
}

/// The next frame other than a heartbeat within `wait`, or `None`.
pub async fn next_frame(stream: &mut LiveStream, wait: Duration) -> Option<Frame> {
    tokio::time::timeout(wait, async {
        loop {
            match stream.next().await {
                Some(Frame::Heartbeat) => {}
                other => return other,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// The next event's sequence within `wait`; panics on anything else.
async fn next_event(stream: &mut LiveStream, wait: Duration, what: &str) -> i64 {
    match next_frame(stream, wait).await {
        Some(Frame::Event(event)) => event.sequence,
        other => panic!("{what}: expected an event within {wait:?}, got {other:?}"),
    }
}

/// Insert `event` and wake its tenant, as the webhook pipeline does.
async fn record(backend: &dyn Backend, event: &NewEvent) -> i64 {
    let sequence = backend.outbox().insert(event).await.unwrap().unwrap();
    if let Some(tenant) = &event.tenant {
        backend
            .notifier()
            .notify(&Wakeup {
                tenant: tenant.clone(),
                sequence,
            })
            .await
            .unwrap();
    }
    sequence
}

/// A notification reaches the subscriptions to its tenant, all of them,
/// within [`WITHIN`], through any handle of the backend's notifier (the
/// bundle's same-data rule), and no other tenant's; a subscription made
/// after it does not see it. Decisive: the channel per tenant, and a
/// notifier made anew per call.
pub async fn the_notifier_wakes_its_tenants_subscriptions_only(backend: &dyn Backend) {
    let (a, b) = (tenant("live-notify-a"), tenant("live-notify-b"));
    let mut a1 = backend.notifier().subscribe(&a).await.unwrap();
    let mut a2 = backend.notifier().subscribe(&a).await.unwrap();
    let mut other = backend.notifier().subscribe(&b).await.unwrap();
    // A notifier may wake every subscription when it starts listening (it
    // cannot know what it missed before): not what this case is about.
    for subscription in [&mut a1, &mut a2, &mut other] {
        let _ = tokio::time::timeout(QUIET, subscription.woken()).await;
    }
    backend
        .notifier()
        .notify(&Wakeup {
            tenant: a.clone(),
            sequence: 7,
        })
        .await
        .unwrap();
    for (name, subscription) in [("first", &mut a1), ("second", &mut a2)] {
        let woken = tokio::time::timeout(WITHIN, subscription.woken()).await;
        assert!(
            woken.is_ok(),
            "the {name} subscription to a's stream was not woken"
        );
    }
    assert!(
        tokio::time::timeout(QUIET, other.woken()).await.is_err(),
        "b's subscription was woken by a's event"
    );
}

/// A stream started after a cursor sends the events after it, in order,
/// then the new ones as they are recorded; a stream resumed after the last
/// sequence it sent (`Last-Event-ID`) sends exactly the ones it had not:
/// none twice, none missed, those recorded while it was gone included. A
/// stream without a cursor sends only what comes next. Decisive: the
/// cursor (`> after` in the reads, the resume's start).
pub async fn a_stream_sends_what_follows_its_cursor_and_resumes_exactly_after_it(
    backend: &dyn Backend,
) {
    const T: &str = "live-resume";
    const PN: &str = "106540352243001";
    let records = backend.records();
    bind(records.as_ref(), T, PN).await;
    let authz = authorizer(backend);
    let caller = caller(backend, &authz, T).await;
    let streams = Streams::new(backend.outbox(), backend.notifier(), woken_only());
    let mut written = Vec::new();
    for _ in 0..3 {
        written.push(record(backend, &row(Some(T), "message_received", PN, None)).await);
    }
    // From the start (cursor 0): 1, 2, 3.
    let mut stream = streams
        .open_stream(
            &authz,
            &caller,
            StreamQuery {
                after: Some(0),
                ..StreamQuery::default()
            },
        )
        .await
        .unwrap();
    for expected in &written {
        assert_eq!(
            next_event(&mut stream, WITHIN, "the backlog").await,
            *expected
        );
    }
    // Live: the next one, woken.
    let fourth = record(backend, &row(Some(T), "status_updated", PN, None)).await;
    assert_eq!(
        next_event(&mut stream, WITHIN, "a live event").await,
        fourth
    );
    assert_eq!(stream.last(), fourth);
    // A stream with no cursor: only what comes next.
    let mut fresh = streams
        .open_stream(&authz, &caller, StreamQuery::default())
        .await
        .unwrap();
    assert_eq!(fresh.last(), fourth, "it starts at the newest sequence");
    // The client goes away after the second; two more are recorded meanwhile.
    let resume_after = written[1];
    drop(stream);
    let fifth = record(backend, &row(Some(T), "message_received", PN, None)).await;
    let sixth = record(backend, &row(Some(T), "message_received", PN, None)).await;
    assert_eq!(
        next_event(&mut fresh, WITHIN, "the fresh stream").await,
        fifth
    );
    assert_eq!(
        next_event(&mut fresh, WITHIN, "the fresh stream").await,
        sixth
    );
    let mut resumed = streams
        .open_stream(
            &authz,
            &caller,
            StreamQuery {
                after: Some(resume_after),
                after_field: "Last-Event-ID",
                ..StreamQuery::default()
            },
        )
        .await
        .unwrap();
    let mut got = Vec::new();
    for _ in 0..4 {
        got.push(next_event(&mut resumed, WITHIN, "the resumed stream").await);
    }
    assert_eq!(
        got,
        [written[2], fourth, fifth, sixth],
        "exactly the events after the cursor"
    );
    assert!(
        next_frame(&mut resumed, QUIET).await.is_none(),
        "nothing more, nothing twice"
    );
}

/// M2.2's third clause and the tenant filter, through the core's webhook
/// pipeline: a stream of tenant B, and one on B's number, see none of A's
/// events, recorded before they opened or while they are open; A's sees
/// its own. The type and number filters hold too. Decisive: the stream's
/// tenant in every read.
pub async fn a_stream_sends_only_its_tenants_events(backend: &dyn Backend) {
    const A: Holder = Holder {
        id: "live-tenant-a",
        waba: "102290129343001",
        pn: "106540352243011",
    };
    const B: Holder = Holder {
        id: "live-tenant-b",
        waba: "102290129343002",
        pn: "106540352243012",
    };
    let records = backend.records();
    hold(records.as_ref(), A).await;
    hold(records.as_ref(), B).await;
    let pipeline = plain_pipeline(backend);
    let authz = authorizer(backend);
    let (caller_a, caller_b) = (
        caller(backend, &authz, A.id).await,
        caller(backend, &authz, B.id).await,
    );
    let streams = Streams::new(backend.outbox(), backend.notifier(), woken_only());
    // One of A's recorded before the streams open, from the start.
    deliver(&pipeline, &body(&text(A.waba, A.pn, "wamid.live.a.before")))
        .await
        .unwrap();
    let from_start = StreamQuery {
        after: Some(0),
        ..StreamQuery::default()
    };
    let mut of_b = streams
        .open_stream(&authz, &caller_b, from_start.clone())
        .await
        .unwrap();
    let mut on_bs_number = streams
        .open_number_stream(
            &authz,
            &caller_b,
            PhoneNumberId::new(B.pn).unwrap(),
            from_start.clone(),
        )
        .await
        .unwrap();
    let mut of_a = streams
        .open_stream(&authz, &caller_a, from_start.clone())
        .await
        .unwrap();
    let mut a_texts_only = streams
        .open_stream(
            &authz,
            &caller_a,
            StreamQuery {
                types: Some(vec!["status_updated".to_owned()]),
                ..from_start.clone()
            },
        )
        .await
        .unwrap();
    for i in 0..3 {
        deliver(
            &pipeline,
            &body(&text(A.waba, A.pn, &format!("wamid.live.a.{i}"))),
        )
        .await
        .unwrap();
    }
    for i in 0..4 {
        let sequence = next_event(&mut of_a, WITHIN, "a's own stream").await;
        assert_eq!(sequence, i + 1, "a's events, in order");
    }
    // Then one of B's: the first (and only) event either of B's streams sends.
    deliver(&pipeline, &body(&text(B.waba, B.pn, "wamid.live.b.0")))
        .await
        .unwrap();
    for (name, stream) in [("b's stream", &mut of_b), ("b's number", &mut on_bs_number)] {
        match next_frame(stream, WITHIN).await {
            Some(Frame::Event(event)) => {
                assert_eq!(event.tenant, Some(tenant(B.id)), "{name}");
                assert_eq!(
                    event.phone_number_id.as_ref().map(PhoneNumberId::as_str),
                    Some(B.pn)
                );
                assert_eq!(event.sequence, 1, "{name}: b's own first sequence");
            }
            other => panic!("{name}: expected b's event, got {other:?}"),
        }
        assert!(
            next_frame(stream, QUIET).await.is_none(),
            "{name}: nothing else"
        );
    }
    assert!(
        next_frame(&mut a_texts_only, QUIET).await.is_none(),
        "a type filter left out a's messages"
    );
}

/// Roadmap M2d's decisive case on the streams (acceptance test M2.6,
/// design D25), through the core's webhook pipeline: Meta's examples of
/// `standby_observed`, `thread_control_changed` and `user_action_reported`
/// on tenant A's number reach A's stream and A's number's stream, each in
/// order, and no stream of tenant B's (its own, nor its number's).
/// Decisive: the three types in `TENANT_EVENT_TYPES` (one taken out, its
/// examples reach no stream).
#[allow(clippy::similar_names)] // a's and b's streams, side by side
pub async fn the_reviewed_types_reach_their_numbers_tenants_streams_only(backend: &dyn Backend) {
    use super::webhooks::{REVIEWED_EXAMPLES, example_for};
    const A: Holder = Holder {
        id: "live-reviewed-a",
        waba: "102290129343061",
        pn: "106540352243061",
    };
    const B: Holder = Holder {
        id: "live-reviewed-b",
        waba: "102290129343062",
        pn: "106540352243062",
    };
    let records = backend.records();
    hold(records.as_ref(), A).await;
    hold(records.as_ref(), B).await;
    let pipeline = plain_pipeline(backend);
    let authz = authorizer(backend);
    let (caller_a, caller_b) = (
        caller(backend, &authz, A.id).await,
        caller(backend, &authz, B.id).await,
    );
    let streams = Streams::new(backend.outbox(), backend.notifier(), woken_only());
    let number = |holder: Holder| PhoneNumberId::new(holder.pn).unwrap();
    let mut of_a = streams
        .open_stream(&authz, &caller_a, StreamQuery::default())
        .await
        .unwrap();
    let mut on_as_number = streams
        .open_number_stream(&authz, &caller_a, number(A), StreamQuery::default())
        .await
        .unwrap();
    let mut of_b = streams
        .open_stream(&authz, &caller_b, StreamQuery::default())
        .await
        .unwrap();
    let mut on_bs_number = streams
        .open_number_stream(&authz, &caller_b, number(B), StreamQuery::default())
        .await
        .unwrap();
    // Each example a second of its own, from the next (the binding's own
    // second is too close after a move; this is a first onboarding, but
    // the dates need not say so).
    let at = time::OffsetDateTime::now_utc().unix_timestamp() + 1;
    for (i, (kind, example)) in REVIEWED_EXAMPLES.iter().enumerate() {
        let payload = example_for(
            example,
            A,
            &format!("live-reviewed-{i}"),
            at + i64::try_from(i).unwrap(),
        );
        deliver(&pipeline, &body(&payload)).await.unwrap();
        for (name, stream) in [("a's stream", &mut of_a), ("a's number", &mut on_as_number)] {
            match next_frame(stream, WITHIN).await {
                Some(Frame::Event(event)) => {
                    assert_eq!(event.event_type, *kind, "{name}, example {i}");
                    assert_eq!(event.tenant, Some(tenant(A.id)), "{name}");
                    assert_eq!(
                        event.phone_number_id.as_ref().map(PhoneNumberId::as_str),
                        Some(A.pn),
                        "{name}"
                    );
                }
                other => panic!("{name}, example {i} ({kind}): expected it, got {other:?}"),
            }
        }
    }
    for (name, stream) in [("b's stream", &mut of_b), ("b's number", &mut on_bs_number)] {
        assert!(
            next_frame(stream, QUIET).await.is_none(),
            "{name}: another tenant's number's event reached it"
        );
    }
}

/// M2.3 on the streams, through the core's webhook pipeline: an
/// `unknown` field and a body that is not a webhook (`unparsed`) on the
/// tenant's own number reach no stream (operator-only rows), and the next
/// event the stream sends is the tenant's message after them. Decisive:
/// the routing allow-list (an operator-only row has no tenant, and no
/// stream reads one).
pub async fn no_unknown_or_unparsed_event_reaches_a_stream(backend: &dyn Backend) {
    const H: Holder = Holder {
        id: "live-unknown",
        waba: "102290129343003",
        pn: "106540352243013",
    };
    hold(backend.records().as_ref(), H).await;
    let pipeline = plain_pipeline(backend);
    let authz = authorizer(backend);
    let caller = caller(backend, &authz, H.id).await;
    let streams = Streams::new(backend.outbox(), backend.notifier(), woken_only());
    let mut stream = streams
        .open_stream(&authz, &caller, StreamQuery::default())
        .await
        .unwrap();
    deliver(&pipeline, &body(&unknown_field(H.waba, H.pn)))
        .await
        .unwrap();
    deliver(
        &pipeline,
        br#"{"object":"whatsapp_business_account","entry":"not a list"}"#,
    )
    .await
    .unwrap();
    assert!(
        next_frame(&mut stream, QUIET).await.is_none(),
        "an operator-only event reached a stream"
    );
    deliver(
        &pipeline,
        &body(&text(H.waba, H.pn, "wamid.live.after-unknown")),
    )
    .await
    .unwrap();
    match next_frame(&mut stream, WITHIN).await {
        Some(Frame::Event(event)) => assert_eq!(event.event_type, "message_received"),
        other => panic!("expected the message, got {other:?}"),
    }
}

/// A notifier that never wakes anyone: every notification lost.
#[derive(Debug, Default)]
pub struct Deaf {
    hub: Hub,
}

#[async_trait]
impl EventNotifier for Deaf {
    async fn notify(&self, _: &Wakeup) -> StoreResult<()> {
        Ok(())
    }

    async fn subscribe(&self, tenant: &TenantId) -> StoreResult<Subscription> {
        Ok(self.hub.subscribe(tenant))
    }
}

/// Every notification lost: the stream still sends the event, by its
/// re-poll, within the re-poll's interval; with the re-poll far off it
/// does not (the control: it is the re-poll that delivered). Decisive: the
/// re-poll.
pub async fn a_lost_notification_still_delivers(backend: &dyn Backend) {
    const T: &str = "live-deaf";
    const PN: &str = "106540352243014";
    bind(backend.records().as_ref(), T, PN).await;
    let authz = authorizer(backend);
    let caller = caller(backend, &authz, T).await;
    let deaf: Arc<dyn EventNotifier> = Arc::new(Deaf::default());
    let repolled = Streams::new(
        backend.outbox(),
        deaf.clone(),
        LiveSettings {
            repoll: Duration::from_millis(300),
            ..woken_only()
        },
    );
    let never = Streams::new(backend.outbox(), deaf, woken_only());
    let mut stream = repolled
        .open_stream(&authz, &caller, StreamQuery::default())
        .await
        .unwrap();
    let mut control = never
        .open_stream(&authz, &caller, StreamQuery::default())
        .await
        .unwrap();
    let sequence = record(backend, &row(Some(T), "message_received", PN, None)).await;
    assert_eq!(
        next_event(&mut stream, WITHIN, "a lost notification").await,
        sequence
    );
    assert!(
        next_frame(&mut control, Duration::from_secs(1))
            .await
            .is_none(),
        "with neither a notification nor a re-poll, nothing should come"
    );
}

/// A cursor past the tenant's newest sequence is `422 invalid_request` on
/// the cursor's own name, before any stream starts (and holds no slot).
pub async fn a_cursor_past_the_newest_sequence_is_refused_before_the_stream(backend: &dyn Backend) {
    const T: &str = "live-ahead";
    const PN: &str = "106540352243015";
    bind(backend.records().as_ref(), T, PN).await;
    let authz = authorizer(backend);
    let caller = caller(backend, &authz, T).await;
    let streams = Streams::new(backend.outbox(), backend.notifier(), woken_only());
    let newest = record(backend, &row(Some(T), "message_received", PN, None)).await;
    let refused = streams
        .open_stream(
            &authz,
            &caller,
            StreamQuery {
                after: Some(newest + 1),
                after_field: "Last-Event-ID",
                ..StreamQuery::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        (refused.status(), refused.code(), refused.field()),
        (422, "invalid_request", Some("Last-Event-ID"))
    );
    assert_eq!(streams.open(), 0, "a refused stream holds no slot");
    // At the newest: open, nothing to send.
    let mut stream = streams
        .open_stream(
            &authz,
            &caller,
            StreamQuery {
                after: Some(newest),
                ..StreamQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(next_frame(&mut stream, QUIET).await.is_none());
}

/// At most `per_tenant` streams of one tenant and `per_replica` in all:
/// the next is `429 too_many_streams`, retryable; another tenant still
/// opens while one is full; a stream dropped gives its slot back.
/// Decisive: the bound.
pub async fn streams_are_bounded_per_tenant_and_per_replica(backend: &dyn Backend) {
    let authz = authorizer(backend);
    let a = caller(backend, &authz, "live-bound-a").await;
    let b = caller(backend, &authz, "live-bound-b").await;
    let c = caller(backend, &authz, "live-bound-c").await;
    let streams = Streams::new(
        backend.outbox(),
        backend.notifier(),
        LiveSettings {
            per_tenant: 2,
            per_replica: 3,
            ..woken_only()
        },
    );
    let open = async |caller: &Caller| {
        streams
            .open_stream(&authz, caller, StreamQuery::default())
            .await
    };
    let first = open(&a).await.unwrap();
    let _second = open(&a).await.unwrap();
    let refused: ServiceError = open(&a).await.unwrap_err();
    assert_eq!(
        (refused.status(), refused.code()),
        (429, "too_many_streams")
    );
    assert!(refused.is_retryable());
    // A limit of the service's own: it says when to try again.
    assert_eq!(refused.retry_after(), Some(5));
    assert_eq!(streams.open_for(&tenant("live-bound-a")), 2);
    let _of_b = open(&b).await.unwrap();
    let full = open(&c).await.unwrap_err();
    assert_eq!(full.code(), "too_many_streams", "the replica's bound");
    drop(first);
    let _of_c = open(&c).await.unwrap();
    assert_eq!(streams.open(), 3);
}

/// A client that takes nothing is dropped once its buffer stayed full for
/// `slow_client`, and never held more than its byte budget meanwhile (one
/// event larger than the budget alone). Once dropped, its stream gives
/// its place among the bounds and its buffer back at once, though its
/// client never takes another frame (stalled on a full TCP window): a
/// stream of the same tenant opens past a bound of one, and nothing is
/// held for it. When it does read again, it gets `lagged`, naming the
/// last sequence it took (here the cursor it started after), and nothing
/// more; resumed from there, it gets every event, in order. Decisive: the
/// budget, the drop, the slot held by the producer, and the queue emptied
/// when it lags.
pub async fn a_slow_client_is_dropped_with_its_buffer_bounded(backend: &dyn Backend) {
    const T: &str = "live-slow";
    const PN: &str = "106540352243016";
    bind(backend.records().as_ref(), T, PN).await;
    let authz = authorizer(backend);
    let caller = caller(backend, &authz, T).await;
    let budget = 4 * 1024;
    let streams = Streams::new(
        backend.outbox(),
        backend.notifier(),
        LiveSettings {
            per_tenant: 1,
            buffer_bytes: budget,
            buffer_frames: 8,
            slow_client: Duration::from_millis(500),
            ..woken_only()
        },
    );
    let mut slow = streams
        .open_stream(&authz, &caller, StreamQuery::default())
        .await
        .unwrap();
    let started_after = slow.last();
    let mut written = Vec::new();
    for _ in 0..40 {
        let mut event = row(Some(T), "message_received", PN, None);
        // About 1 KiB each: four fill the budget.
        event.data = format!(
            "{{\"event\":\"message_received\",\"pad\":\"{}\"}}",
            "x".repeat(1000)
        );
        written.push(record(backend, &event).await);
        assert!(
            streams.buffered_bytes() <= budget,
            "{} bytes held for one client",
            streams.buffered_bytes()
        );
        // What it holds, counted apart from the budget's own count: four
        // events of a kibibyte at most, though eight frames would fit.
        assert!(slow.pending() <= 4, "{} frames held", slow.pending());
    }
    tokio::time::sleep(Duration::from_millis(900)).await;
    // Dropped, though `slow` was never polled again: its place and its
    // buffer are back.
    assert_eq!(
        streams.open_for(&tenant(T)),
        0,
        "a stalled client kept its place"
    );
    assert_eq!(
        streams.buffered_bytes(),
        0,
        "a stalled client's buffer held"
    );
    let other = streams
        .open_stream(&authz, &caller, StreamQuery::default())
        .await
        .expect("the tenant's only place, given back");
    drop(other);
    assert_eq!(
        next_frame(&mut slow, WITHIN).await,
        Some(Frame::Lagged {
            after: started_after
        }),
        "lagged first, naming what it took"
    );
    assert!(slow.next().await.is_none(), "lagged is the last frame");
    drop(slow);
    // Resumed from there, it gets every event.
    let mut resumed = streams
        .open_stream(
            &authz,
            &caller,
            StreamQuery {
                after: Some(started_after),
                ..StreamQuery::default()
            },
        )
        .await
        .unwrap();
    for expected in &written {
        assert_eq!(
            next_event(&mut resumed, WITHIN, "the events after it").await,
            *expected
        );
    }
}

/// A stream asks again, before it reads, whether its key would still be
/// admitted: once the key is revoked, or its tenant suspended, the stream
/// ends with no last frame and sends nothing recorded after (the event
/// before is the control: it arrives); an idle stream ends by its
/// re-poll. Its place is given back. Decisive: the check before a read.
pub async fn a_stream_ends_once_its_key_or_tenant_is_no_longer_admitted(backend: &dyn Backend) {
    const T: &str = "live-revoked";
    const S: &str = "live-suspended";
    const PN_T: &str = "106540352243041";
    const PN_S: &str = "106540352243042";
    bind(backend.records().as_ref(), T, PN_T).await;
    bind(backend.records().as_ref(), S, PN_S).await;
    let authz = authorizer(backend);
    let records = backend.records();
    let streams = Streams::new(
        backend.outbox(),
        backend.notifier(),
        LiveSettings {
            recheck: Duration::ZERO,
            ..woken_only()
        },
    );
    // Revoked, then suspended: each ends its tenant's stream.
    for (t, pn) in [(T, PN_T), (S, PN_S)] {
        let caller = caller(backend, &authz, t).await;
        let mut live = streams
            .open_stream(&authz, &caller, StreamQuery::default())
            .await
            .unwrap();
        let before = record(backend, &row(Some(t), "message_received", pn, None)).await;
        assert_eq!(next_event(&mut live, WITHIN, "the control").await, before);
        if t == T {
            assert!(
                records
                    .revoke_key(&KeyScope::Tenant(tenant(t)), caller.key_id())
                    .await
                    .unwrap()
            );
        } else {
            records
                .update_tenant(&tenant(t), None, Some(TenantStatus::Suspended))
                .await
                .unwrap()
                .unwrap();
        }
        record(backend, &row(Some(t), "message_received", pn, None)).await;
        assert_eq!(
            tokio::time::timeout(WITHIN, live.next()).await,
            Ok(None),
            "{t}: the stream went on"
        );
        assert_eq!(streams.open_for(&tenant(t)), 0, "{t}");
    }
    // An idle stream: no event wakes it, its re-poll checks.
    let idle_streams = Streams::new(
        backend.outbox(),
        backend.notifier(),
        LiveSettings {
            recheck: Duration::ZERO,
            repoll: Duration::from_millis(200),
            ..woken_only()
        },
    );
    let caller = caller(backend, &authz, T).await;
    let mut idle = idle_streams
        .open_stream(&authz, &caller, StreamQuery::default())
        .await
        .unwrap();
    assert!(next_frame(&mut idle, QUIET).await.is_none(), "still open");
    records
        .revoke_key(&KeyScope::Tenant(tenant(T)), caller.key_id())
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(WITHIN, idle.next()).await,
        Ok(None),
        "an idle stream outlived its key"
    );
}

/// [`Streams::shut_down`] ends every open stream with `shutdown`, naming
/// its last sequence, and refuses new ones (`503 shutting_down`).
pub async fn shutting_down_ends_every_stream(backend: &dyn Backend) {
    let authz = authorizer(backend);
    let caller = caller(backend, &authz, "live-shutdown").await;
    let streams = Streams::new(backend.outbox(), backend.notifier(), woken_only());
    let mut open = Vec::new();
    for _ in 0..3 {
        open.push(
            streams
                .open_stream(&authz, &caller, StreamQuery::default())
                .await
                .unwrap(),
        );
    }
    streams.shut_down();
    for stream in &mut open {
        let after = stream.last();
        assert_eq!(
            next_frame(stream, WITHIN).await,
            Some(Frame::Shutdown { after })
        );
        assert!(stream.next().await.is_none());
    }
    let refused = streams
        .open_stream(&authz, &caller, StreamQuery::default())
        .await
        .unwrap_err();
    assert_eq!((refused.status(), refused.code()), (503, "shutting_down"));
}

/// [`crate::outbox::Outbox::get`] and the core's `events::get_event`: a
/// tenant reads its own event by id, whole; another tenant's id, an
/// operator-only row's and an id that is not an event's are not found
/// (`404`), alike. Decisive: the tenant in the lookup.
pub async fn an_event_is_read_by_id_in_its_tenants_stream_only(backend: &dyn Backend) {
    const A: &str = "live-get-a";
    const B: &str = "live-get-b";
    const PN_A: &str = "106540352243017";
    const PN_B: &str = "106540352243018";
    let records = backend.records();
    bind(records.as_ref(), A, PN_A).await;
    bind(records.as_ref(), B, PN_B).await;
    let outbox = backend.outbox();
    let mut ids = Vec::new();
    for (owner, pn) in [(Some(A), PN_A), (Some(B), PN_B), (None, PN_A)] {
        let mut event = row(owner, "message_received", pn, None);
        // The shape the service gives ids (`evt_` and 32 hex digits).
        event.id = format!("evt_{:032x}", ids.len() as u128 + 0xa11ce);
        outbox.insert(&event).await.unwrap().unwrap();
        ids.push(event);
    }
    let found = crate::events::get_event(outbox.as_ref(), &tenant(A), &ids[0].id)
        .await
        .unwrap();
    assert_eq!(
        (found.id.as_str(), found.data.as_str()),
        (ids[0].id.as_str(), ids[0].data.as_str())
    );
    assert_eq!(found.tenant, Some(tenant(A)));
    for (what, id) in [
        ("another tenant's", ids[1].id.as_str()),
        ("an operator-only row's", ids[2].id.as_str()),
        ("an id of no event", "evt_00000000000000000000000000000000"),
        ("an id of the wrong shape", "stream"),
    ] {
        let refused = crate::events::get_event(outbox.as_ref(), &tenant(A), id)
            .await
            .unwrap_err();
        assert_eq!(
            (refused.status(), refused.code()),
            (404, "not_found"),
            "{what}"
        );
    }
    // The port itself, without the core's check.
    assert!(outbox.get(&tenant(A), &ids[1].id).await.unwrap().is_none());
    assert!(outbox.get(&tenant(A), &ids[2].id).await.unwrap().is_none());
    assert!(outbox.get(&tenant(B), &ids[1].id).await.unwrap().is_some());
}

/// A number stream opens on the tenant's own number only: another
/// tenant's number, or one nobody holds, is `404 not_found` before the
/// outbox is read.
pub async fn a_number_stream_needs_the_tenants_number(backend: &dyn Backend) {
    const A: &str = "live-number-a";
    const B: &str = "live-number-b";
    const PN_A: &str = "106540352243019";
    const PN_B: &str = "106540352243020";
    let records = backend.records();
    bind(records.as_ref(), A, PN_A).await;
    bind(records.as_ref(), B, PN_B).await;
    let authz = authorizer(backend);
    let caller_a = caller(backend, &authz, A).await;
    let streams = Streams::new(backend.outbox(), backend.notifier(), woken_only());
    for pn in [PN_B, "106540352249998"] {
        let refused = streams
            .open_number_stream(
                &authz,
                &caller_a,
                PhoneNumberId::new(pn).unwrap(),
                StreamQuery::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            (refused.status(), refused.code()),
            (404, "not_found"),
            "{pn}"
        );
    }
    assert_eq!(streams.open(), 0);
    streams
        .open_number_stream(
            &authz,
            &caller_a,
            PhoneNumberId::new(PN_A).unwrap(),
            StreamQuery::default(),
        )
        .await
        .unwrap();
}

/// Once housekeeping purged events a stream had not sent, the stream ends
/// with `lagged` (its client lost them), and a stream asked to start
/// below what was purged is `410 cursor_expired`. Purges every stream of
/// the backend: last.
pub async fn a_stream_behind_a_purge_ends_lagged(backend: &dyn Backend) {
    const T: &str = "live-purged";
    const PN: &str = "106540352243021";
    bind(backend.records().as_ref(), T, PN).await;
    let authz = authorizer(backend);
    let caller = caller(backend, &authz, T).await;
    let streams = Streams::new(backend.outbox(), backend.notifier(), woken_only());
    let first = record(backend, &row(Some(T), "message_received", PN, None)).await;
    let mut behind = streams
        .open_stream(
            &authz,
            &caller,
            StreamQuery {
                after: Some(first),
                ..StreamQuery::default()
            },
        )
        .await
        .unwrap();
    // Two events it is not woken for, purged before it reads them.
    let outbox = backend.outbox();
    for _ in 0..2 {
        outbox
            .insert(&row(Some(T), "message_received", PN, None))
            .await
            .unwrap()
            .unwrap();
    }
    assert!(outbox.purge(Duration::ZERO).await.unwrap() >= 3);
    // Woken by the next one, it finds them gone.
    record(backend, &row(Some(T), "message_received", PN, None)).await;
    assert_eq!(
        next_frame(&mut behind, WITHIN).await,
        Some(Frame::Lagged { after: first }),
        "a stream behind a purge ends lagged, naming the last sequence it sent"
    );
    assert!(behind.next().await.is_none());
    let refused = streams
        .open_stream(
            &authz,
            &caller,
            StreamQuery {
                after: Some(first),
                ..StreamQuery::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!((refused.status(), refused.code()), (410, "cursor_expired"));
}

/// M2.2 over two bundles on one database (two replicas): an event recorded
/// through `a` (its outbox, then its notifier, as `a`'s webhook pipeline
/// does) reaches a stream opened on `b` within [`WITHIN`], with no re-poll;
/// a stream of another tenant on `b` sees none of it; a stream resumed on
/// `b` after an earlier sequence replays exactly the events after it.
/// Not in [`run`], which has one backend: a shared backend runs it with
/// two handles on one database (the Postgres crate: two pools). Decisive:
/// the notification crossing replicas (a notifier local to each process
/// fails it).
pub async fn an_event_through_one_replica_wakes_a_stream_on_another(
    a: &dyn Backend,
    b: &dyn Backend,
) {
    const T: &str = "live-replicas";
    const OTHER: &str = "live-replicas-other";
    const PN: &str = "106540352243031";
    const PN_OTHER: &str = "106540352243032";
    bind(a.records().as_ref(), T, PN).await;
    bind(a.records().as_ref(), OTHER, PN_OTHER).await;
    let authz = authorizer(b);
    let (caller_t, caller_other) = (caller(b, &authz, T).await, caller(b, &authz, OTHER).await);
    let on_b = Streams::new(b.outbox(), b.notifier(), woken_only());
    let mut stream = on_b
        .open_stream(&authz, &caller_t, StreamQuery::default())
        .await
        .unwrap();
    let mut other = on_b
        .open_stream(&authz, &caller_other, StreamQuery::default())
        .await
        .unwrap();
    let start = stream.last();
    let mut written = Vec::new();
    for _ in 0..3 {
        let started = tokio::time::Instant::now();
        let sequence = record(a, &row(Some(T), "message_received", PN, None)).await;
        assert_eq!(
            next_event(&mut stream, WITHIN, "a stream on the other replica").await,
            sequence
        );
        assert!(started.elapsed() <= WITHIN);
        written.push(sequence);
    }
    assert!(
        next_frame(&mut other, QUIET).await.is_none(),
        "another tenant's stream on b saw t's events"
    );
    drop(stream);
    let mut resumed = on_b
        .open_stream(
            &authz,
            &caller_t,
            StreamQuery {
                after: Some(written[0]),
                ..StreamQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(start < written[0]);
    for expected in &written[1..] {
        assert_eq!(
            next_event(&mut resumed, WITHIN, "resumed on b").await,
            *expected
        );
    }
    assert!(next_frame(&mut resumed, QUIET).await.is_none());
}

/// M2.5: `count` streams (`count / 20` tenants of the design's 20 each,
/// half of them read, half never) while events are recorded at `rate` a
/// second for `seconds`: the streams read keep up with every event in
/// order, and the data held for clients stays within each stream's budget
/// throughout (the unread ones are dropped as slow). What it saw.
///
/// # Panics
///
/// On a broken bound, a lost or reordered event.
pub async fn many_streams_at_a_steady_rate_hold_bounded_memory(
    backend: &dyn Backend,
    count: usize,
    rate: u32,
    seconds: u32,
) -> Load {
    let per_tenant = crate::live::DEFAULT_STREAMS_PER_TENANT;
    let tenants = count.div_ceil(per_tenant);
    let budget = 16 * 1024;
    let settings = LiveSettings {
        buffer_bytes: budget,
        buffer_frames: 16,
        slow_client: Duration::from_millis(500),
        ..woken_only()
    };
    let streams = Streams::new(backend.outbox(), backend.notifier(), settings);
    let authz = authorizer(backend);
    let mut handles = Vec::new();
    let mut pns = Vec::new();
    for t in 0..tenants {
        let id = format!("live-load-{t}");
        let pn = format!("1065403522440{t:02}");
        bind(backend.records().as_ref(), &id, &pn).await;
        let caller = caller(backend, &authz, &id).await;
        for s in 0..per_tenant.min(count - t * per_tenant) {
            let stream = streams
                .open_stream(&authz, &caller, StreamQuery::default())
                .await
                .unwrap();
            handles.push((s % 2 == 0, t, stream));
        }
        pns.push((id, pn));
    }
    assert_eq!(streams.open(), count);
    let total = usize::try_from(rate * seconds).unwrap();
    let interval = Duration::from_secs(1) / rate;
    // Readers: every event of their tenant, in order.
    let mut readers = Vec::new();
    let mut idle = Vec::new();
    for (read, t, stream) in handles {
        // Tenant `t` gets every `tenants`-th event, from the `t`-th.
        let expected = total / tenants + usize::from(t < total % tenants);
        if read {
            readers.push(tokio::spawn(async move {
                let mut stream = stream;
                let mut seen: Vec<i64> = Vec::new();
                while seen.len() < expected {
                    match next_frame(&mut stream, Duration::from_secs(10)).await {
                        Some(Frame::Event(event)) => seen.push(event.sequence),
                        other => panic!("a reading stream ended: {other:?}"),
                    }
                }
                (t, seen)
            }));
        } else {
            idle.push(stream);
        }
    }
    let mut peak = 0;
    let started = tokio::time::Instant::now();
    let mut per_tenant_written = vec![Vec::new(); tenants];
    for i in 0..total {
        let t = i % tenants;
        let (id, pn) = &pns[t];
        let mut event = row(Some(id), "message_received", pn, None);
        event.data = format!(
            "{{\"event\":\"message_received\",\"pad\":\"{}\"}}",
            "x".repeat(512)
        );
        per_tenant_written[t].push(record(backend, &event).await);
        peak = peak.max(streams.buffered_bytes());
        assert!(
            streams.buffered_bytes() <= count * budget,
            "{} bytes held for {count} streams",
            streams.buffered_bytes()
        );
        tokio::time::sleep_until(started + interval * u32::try_from(i + 1).unwrap()).await;
    }
    let elapsed = started.elapsed();
    let mut delivered = 0;
    for reader in readers {
        let (t, seen) = reader.await.unwrap();
        assert_eq!(
            seen, per_tenant_written[t],
            "tenant {t}'s stream, in order, none lost"
        );
        delivered += seen.len();
    }
    let dropped = idle.len();
    drop(idle);
    Load {
        streams: count,
        events: total,
        delivered,
        dropped,
        peak_buffered_bytes: peak,
        budget_per_stream: budget,
        elapsed,
    }
}

/// What [`many_streams_at_a_steady_rate_hold_bounded_memory`] saw.
#[derive(Debug, Clone, Copy)]
pub struct Load {
    /// Streams open.
    pub streams: usize,
    /// Events recorded.
    pub events: usize,
    /// Events the read streams sent, all of them.
    pub delivered: usize,
    /// Streams never read.
    pub dropped: usize,
    /// The most data held for clients at once, every stream's.
    pub peak_buffered_bytes: usize,
    /// Each stream's budget.
    pub budget_per_stream: usize,
    /// How long recording took.
    pub elapsed: Duration,
}
