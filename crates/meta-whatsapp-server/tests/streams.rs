//! Live updates over HTTP (roadmap M2b; docs/design/server.md, section
//! 4.5): `GET /v1/events/stream` and `GET /v1/numbers/{pn}/events/stream`
//! as Server-Sent Events, and `GET /v1/events/{id}`, through the internal
//! router, on memory (two replicas on Postgres: `live_postgres.rs`).
//!
//! Acceptance tests M2.3 on the streams (no `unknown` or `unparsed` event
//! reaches one) and M2.2's tenant clause (a stream of tenant B, or on its
//! number, sees none of A's); `Last-Event-ID` resumes exactly after; a lost
//! notification still delivers; the cursor's `410` and `422` before the
//! stream; the per-tenant bound (`429 too_many_streams`, with
//! `Retry-After`); heartbeats; a slow client dropped with `lagged`, its
//! place given back though it never reads again; a stream ending once its
//! key is revoked; the open streams as a metric; a shutdown ends every
//! stream with `shutdown`; one event by id, the tenant's only. The core's suites hold
//! the same rules over each backend (`conformance::live`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::meta::{EXAMPLE_PN, EXAMPLE_WABA, bytes, text, unknown_field};
use common::sse::{Sse, open};
use common::{Call, Harness, Stores, test_settings};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_server::model::{AllowedTenants, Scope, TenantId};
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server::store::Outbox as _;
use meta_whatsapp_server_core::conformance::live::Deaf;
use meta_whatsapp_server_core::live::LiveSettings;
use serde_json::{Value, json};

const A: &str = "tenant-a";
const B: &str = "tenant-b";
/// A's second number, under its WABA.
const PN_A2: &str = "106540352242924";
const WABA_B: &str = "102290129340399";
const PN_B: &str = "106540352242923";
/// M2.2's two seconds.
const WITHIN: Duration = Duration::from_secs(2);
/// How long a test waits to see that nothing comes.
const QUIET: Duration = Duration::from_millis(400);

/// The test settings, the streams woken only (no re-poll, no heartbeat in
/// a test's time), with `live`'s changes.
fn settings(live: impl FnOnce(LiveSettings) -> LiveSettings) -> Settings {
    Settings {
        live: live(LiveSettings {
            heartbeat: Duration::from_secs(3600),
            repoll: Duration::from_secs(3600),
            ..LiveSettings::default()
        }),
        ..test_settings()
    }
}

/// Tenant A (Meta's example WABA, its number and [`PN_A2`]) and tenant B
/// ([`WABA_B`], [`PN_B`]), on `h`.
async fn two_tenants(h: Harness) -> Harness {
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, EXAMPLE_WABA, &[EXAMPLE_PN, PN_A2], "TOKEN-OF-A")
        .await;
    h.connect(B, WABA_B, &[PN_B], "TOKEN-OF-B").await;
    h
}

fn stream(key: &str) -> Call {
    Call::get("/v1/events/stream").key(key)
}

async fn opened(h: &Harness, call: Call) -> Sse {
    match open(&h.internal, call).await {
        Ok(sse) => sse,
        Err(reply) => panic!("the stream was refused: {} {}", reply.status, reply.text),
    }
}

async fn refused(h: &Harness, call: Call) -> (StatusCode, Value) {
    match open(&h.internal, call).await {
        Ok(_) => panic!("the stream opened"),
        Err(reply) => (reply.status, reply.json()),
    }
}

/// Meta's text example to `pn` of `waba`, delivered.
async fn deliver_text(h: &Harness, waba: &str, pn: &str, wamid: &str) {
    let reply = h.webhook(&bytes(&text(waba, pn, wamid))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
}

/// A delivered event reaches its tenant's open stream within 2 s as
/// `event: whatsapp`, `id: <sequence>` and `data: <envelope>`, the
/// envelope `GET /v1/events` and `GET /v1/events/{id}` answer.
#[tokio::test]
async fn a_webhook_reaches_its_tenants_stream_as_server_sent_events() {
    let h = two_tenants(Harness::with_settings(settings(|l| l))).await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    let mut live = opened(&h, stream(&key)).await;
    assert_eq!(
        live.headers["content-type"].to_str().unwrap(),
        "text/event-stream"
    );
    assert_eq!(live.headers["cache-control"].to_str().unwrap(), "no-cache");
    let started = tokio::time::Instant::now();
    deliver_text(&h, EXAMPLE_WABA, EXAMPLE_PN, "wamid.live.1").await;
    let frame = live.event(WITHIN).await;
    assert!(started.elapsed() <= WITHIN);
    let envelope = frame.json();
    assert_eq!(frame.sequence(), 1);
    assert_eq!(envelope["sequence"], 1);
    assert_eq!(envelope["type"], "message_received");
    assert_eq!(envelope["tenant_id"], A);
    assert_eq!(envelope["phone_number_id"], EXAMPLE_PN);
    assert_eq!(envelope["waba_id"], EXAMPLE_WABA);
    assert_eq!(envelope["api_version"], "v1");
    assert_eq!(envelope["truncated"], false);
    assert_eq!(envelope["data"]["event"], "message_received");
    let polled = h.call(Call::get("/v1/events").key(&key)).await;
    assert_eq!(polled.json()["data"][0], envelope, "the poll's envelope");
    let id = envelope["id"].as_str().unwrap();
    let one = h
        .call(Call::get(format!("/v1/events/{id}")).key(&key))
        .await;
    assert_eq!(one.status, StatusCode::OK, "{}", one.text);
    assert_eq!(one.json(), envelope, "GET /v1/events/{{id}}'s envelope");
}

/// M2.3 on the streams: an `unknown` field and a signed body that is not a
/// webhook (`unparsed`), on tenant A's own WABA and number, reach no
/// stream of A's, nor the number's; A's next event is the first frame
/// either sends. Decisive: the routing allow-list.
#[tokio::test]
async fn no_unknown_or_unparsed_event_reaches_a_stream() {
    let h = two_tenants(Harness::with_settings(settings(|l| l))).await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    let mut of_a = opened(&h, stream(&key)).await;
    let mut on_number = opened(
        &h,
        Call::get(format!("/v1/numbers/{EXAMPLE_PN}/events/stream")).key(&key),
    )
    .await;
    assert_eq!(
        h.webhook(&bytes(&unknown_field(EXAMPLE_WABA))).await.status,
        StatusCode::OK
    );
    assert_eq!(
        h.webhook(br#"{"object":"whatsapp_business_account","entry":"not a list"}"#)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(h.outbox.rows().len(), 2, "both recorded, operator-only");
    assert!(h.outbox.rows().iter().all(|row| row.tenant.is_none()));
    assert!(
        of_a.next(QUIET).await.is_none(),
        "an operator-only event streamed"
    );
    deliver_text(&h, EXAMPLE_WABA, EXAMPLE_PN, "wamid.live.after").await;
    for live in [&mut of_a, &mut on_number] {
        let frame = live.event(WITHIN).await;
        assert_eq!(frame.json()["type"], "message_received");
    }
}

/// M2.2's third clause over HTTP: tenant B's stream, and B's number's, see
/// none of A's events, nor does a platform key acting as B; A's stream of
/// its second number sees none of its first number's. B's key on A's
/// number's stream is `404`. Decisive: the stream's tenant (and the
/// number filter).
#[tokio::test]
async fn a_stream_sees_only_its_tenants_events() {
    let h = two_tenants(Harness::with_settings(settings(|l| l))).await;
    let a_key = h.tenant_key(A, &[Scope::Events]).await;
    let b_key = h.tenant_key(B, &[Scope::Events]).await;
    let platform = h
        .platform_key(
            AllowedTenants::Only(vec![TenantId::parse(B).unwrap()]),
            &[Scope::Events],
        )
        .await;
    let mut of_b = opened(&h, stream(&b_key)).await;
    let mut as_b = opened(&h, stream(&platform).tenant(B)).await;
    let mut on_bs_number = opened(
        &h,
        Call::get(format!("/v1/numbers/{PN_B}/events/stream")).key(&b_key),
    )
    .await;
    let mut on_a2 = opened(
        &h,
        Call::get(format!("/v1/numbers/{PN_A2}/events/stream")).key(&a_key),
    )
    .await;
    let mut of_a = opened(&h, stream(&a_key)).await;
    for i in 0..3 {
        deliver_text(&h, EXAMPLE_WABA, EXAMPLE_PN, &format!("wamid.a.{i}")).await;
    }
    for i in 1..=3 {
        assert_eq!(of_a.event(WITHIN).await.sequence(), i);
    }
    for (name, live) in [
        ("b's stream", &mut of_b),
        ("a platform key acting as b", &mut as_b),
        ("b's number", &mut on_bs_number),
        ("a's other number", &mut on_a2),
    ] {
        assert!(live.next(QUIET).await.is_none(), "{name} saw a's events");
    }
    // B's own event: the one event B's streams send, and the one a stream
    // of B's from the start of its stream sends (the message it carries
    // is B's: the envelope's tenant is the caller's, so it is the message
    // that tells).
    deliver_text(&h, WABA_B, PN_B, "wamid.b.0").await;
    let mut from_start = opened(&h, stream(&b_key).header("last-event-id", "0")).await;
    for live in [&mut of_b, &mut as_b, &mut on_bs_number, &mut from_start] {
        let envelope = live.event(WITHIN).await.json();
        assert_eq!(
            (
                envelope["tenant_id"].as_str(),
                envelope["sequence"].as_i64(),
                envelope["phone_number_id"].as_str(),
                envelope["data"]["message"]["id"].as_str(),
            ),
            (Some(B), Some(1), Some(PN_B), Some("wamid.b.0"))
        );
        assert!(live.next(QUIET).await.is_none(), "b's stream sent more");
    }
    let (status, body) = refused(
        &h,
        Call::get(format!("/v1/numbers/{EXAMPLE_PN}/events/stream")).key(&b_key),
    )
    .await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );
}

/// `Last-Event-ID` resumes exactly after the sequence it names: the events
/// after it, once each, none before, then the new ones; it wins over
/// `?after=`, which works alone too. Decisive: the resume's cursor.
#[tokio::test]
async fn last_event_id_resumes_exactly_after_it() {
    let h = two_tenants(Harness::with_settings(settings(|l| l))).await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    for i in 0..5 {
        deliver_text(&h, EXAMPLE_WABA, EXAMPLE_PN, &format!("wamid.r.{i}")).await;
    }
    let mut resumed = opened(&h, stream(&key).header("last-event-id", "2")).await;
    for expected in 3..=5 {
        assert_eq!(resumed.event(WITHIN).await.sequence(), expected);
    }
    assert!(resumed.next(QUIET).await.is_none(), "nothing twice");
    deliver_text(&h, EXAMPLE_WABA, EXAMPLE_PN, "wamid.r.5").await;
    assert_eq!(resumed.event(WITHIN).await.sequence(), 6);
    let mut header_wins = opened(
        &h,
        Call::get("/v1/events/stream?after=1")
            .key(&key)
            .header("last-event-id", "4"),
    )
    .await;
    assert_eq!(header_wins.event(WITHIN).await.sequence(), 5);
    let mut by_query = opened(&h, Call::get("/v1/events/stream?after=5").key(&key)).await;
    assert_eq!(by_query.event(WITHIN).await.sequence(), 6);
    // Without a cursor: only what comes next.
    let mut fresh = opened(&h, stream(&key)).await;
    assert!(fresh.next(QUIET).await.is_none());
}

/// A cursor past the tenant's newest sequence, or malformed, is `422` on
/// the parameter it came in; one below what housekeeping purged is `410
/// cursor_expired`; all before the stream starts.
#[tokio::test]
async fn a_cursor_is_checked_before_the_stream_starts() {
    let h = two_tenants(Harness::with_settings(settings(|l| l))).await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    deliver_text(&h, EXAMPLE_WABA, EXAMPLE_PN, "wamid.c.0").await;
    for (call, field) in [
        (stream(&key).header("last-event-id", "2"), "Last-Event-ID"),
        (
            stream(&key).header("last-event-id", "evt_1"),
            "Last-Event-ID",
        ),
        (Call::get("/v1/events/stream?after=-1").key(&key), "after"),
        (
            Call::get("/v1/events/stream?types=unknown").key(&key),
            "types",
        ),
    ] {
        let (status, body) = refused(&h, call).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"]["field"], field, "{body}");
    }
    assert_eq!(h.outbox.purge(Duration::ZERO).await.unwrap(), 1);
    let (status, body) = refused(&h, stream(&key).header("last-event-id", "0")).await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (StatusCode::GONE, Some("cursor_expired"))
    );
}

/// At most `WA_SERVER_STREAMS_PER_TENANT` streams of one tenant: the next
/// is `429 too_many_streams`, retryable; another tenant still opens; a
/// closed stream frees its place. Decisive: the bound.
#[tokio::test]
async fn streams_are_bounded_per_tenant() {
    let h = two_tenants(Harness::with_settings(settings(|l| LiveSettings {
        per_tenant: 2,
        ..l
    })))
    .await;
    let a_key = h.tenant_key(A, &[Scope::Events]).await;
    let b_key = h.tenant_key(B, &[Scope::Events]).await;
    let first = opened(&h, stream(&a_key)).await;
    let _second = opened(&h, stream(&a_key)).await;
    let reply = match open(&h.internal, stream(&a_key)).await {
        Ok(_) => panic!("the stream opened"),
        Err(reply) => reply,
    };
    let body = reply.json();
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "too_many_streams");
    assert_eq!(body["error"]["retryable"], true);
    // A limit of the service's own, as `too_many_requests` is.
    assert_eq!(reply.headers["retry-after"], "5");
    let (status, _) = refused(
        &h,
        Call::get(format!("/v1/numbers/{EXAMPLE_PN}/events/stream")).key(&a_key),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "a number's stream counts"
    );
    let _of_b = opened(&h, stream(&b_key)).await;
    assert_eq!(h.state.open_streams(), 3);
    drop(first);
    let _again = opened(&h, stream(&a_key)).await;
}

/// An idle stream sends a `: keepalive` comment every heartbeat.
#[tokio::test]
async fn an_idle_stream_sends_heartbeats() {
    let h = two_tenants(Harness::with_settings(settings(|l| LiveSettings {
        heartbeat: Duration::from_millis(100),
        ..l
    })))
    .await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    let mut live = opened(&h, stream(&key)).await;
    for _ in 0..2 {
        let frame = live.frame(Duration::from_secs(1)).await.unwrap();
        assert_eq!(frame.comment.as_deref(), Some("keepalive"), "{frame:?}");
    }
}

/// A client that reads nothing is dropped once its buffer stayed full for
/// the slow-client wait. It gives its place back at once, though it never
/// reads again (stalled on a full TCP window): the tenant's only place
/// opens another stream, and no stream is open. When it reads, it gets
/// `event: lagged` naming the last sequence it took (none here: the cursor
/// it started after), and the stream ends; resumed from there it gets
/// every event. Decisive: the bounded buffer, the drop, and the slot given
/// back when the stream ends rather than when its body is dropped.
#[tokio::test]
async fn a_slow_client_is_dropped_with_lagged() {
    let h = two_tenants(Harness::with_settings(settings(|l| LiveSettings {
        per_tenant: 1,
        buffer_frames: 2,
        slow_client: Duration::from_millis(200),
        ..l
    })))
    .await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    let mut slow = opened(&h, stream(&key)).await;
    for i in 0..8 {
        deliver_text(&h, EXAMPLE_WABA, EXAMPLE_PN, &format!("wamid.s.{i}")).await;
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(h.state.open_streams(), 0, "a stalled client kept its place");
    drop(opened(&h, stream(&key)).await);
    let lagged = slow.next(WITHIN).await.expect("lagged");
    assert_eq!(lagged.event.as_deref(), Some("lagged"), "{lagged:?}");
    assert_eq!(lagged.json(), json!({"after": 0}));
    assert!(slow.ended(WITHIN).await, "lagged is the last frame");
    drop(slow);
    let mut resumed = opened(&h, stream(&key).header("last-event-id", "0")).await;
    for expected in 1..=8 {
        assert_eq!(resumed.event(WITHIN).await.sequence(), expected);
    }
}

/// A stream asks again whether its key would be admitted before it reads:
/// once the key is revoked (the admin API), the stream ends with no last
/// frame and sends nothing delivered after (the event before is the
/// control), and its client reconnecting is `401`. Decisive: the check
/// before a read (the core's `Recheck`).
#[tokio::test]
async fn a_stream_ends_once_its_key_is_revoked() {
    let h = two_tenants(Harness::with_settings(settings(|l| LiveSettings {
        recheck: Duration::ZERO,
        ..l
    })))
    .await;
    let admin = h.admin_key().await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    let key_id = key.split('_').nth(1).unwrap().to_owned();
    let mut live = opened(&h, stream(&key)).await;
    deliver_text(&h, EXAMPLE_WABA, EXAMPLE_PN, "wamid.before").await;
    assert_eq!(live.event(WITHIN).await.sequence(), 1);
    let revoked = h
        .call(
            Call::new(
                meta_whatsapp_rs::webhooks::axum::http::Method::DELETE,
                format!("/v1/admin/tenants/{A}/keys/{key_id}"),
            )
            .key(&admin),
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{}", revoked.text);
    deliver_text(&h, EXAMPLE_WABA, EXAMPLE_PN, "wamid.after").await;
    assert!(live.ended(WITHIN).await, "the stream outlived its key");
    assert_eq!(h.state.open_streams(), 0);
    let (status, _) = refused(&h, stream(&key).header("last-event-id", "1")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// `/metrics` counts the live streams open on the replica
/// (`wa_server_event_streams_open`, design section 7.3), never by tenant.
#[tokio::test]
async fn the_open_streams_are_a_metric() {
    let h = two_tenants(Harness::with_settings(settings(|l| l))).await;
    let a_key = h.tenant_key(A, &[Scope::Events]).await;
    let b_key = h.tenant_key(B, &[Scope::Events]).await;
    let first = opened(&h, stream(&a_key)).await;
    let _second = opened(&h, stream(&b_key)).await;
    let metrics = h.call(Call::get("/metrics")).await.text;
    assert!(
        metrics.contains("\nwa_server_event_streams_open 2\n"),
        "{metrics}"
    );
    assert!(!metrics.contains(A) && !metrics.contains(B), "{metrics}");
    drop(first);
    let metrics = h.call(Call::get("/metrics")).await.text;
    assert!(
        metrics.contains("\nwa_server_event_streams_open 1\n"),
        "{metrics}"
    );
}

/// Shutting down ends every stream with `event: shutdown` (the last
/// sequence sent), and a new one is `503 shutting_down`.
#[tokio::test]
async fn shutting_down_ends_every_stream() {
    let h = two_tenants(Harness::with_settings(settings(|l| l))).await;
    let key = h.tenant_key(A, &[Scope::Events]).await;
    let mut first = opened(&h, stream(&key)).await;
    let mut second = opened(&h, stream(&key).header("last-event-id", "0")).await;
    deliver_text(&h, EXAMPLE_WABA, EXAMPLE_PN, "wamid.down").await;
    assert_eq!(first.event(WITHIN).await.sequence(), 1);
    h.state.begin_shutdown();
    for (live, after) in [(&mut first, 1), (&mut second, 0)] {
        // The second never read its event: it may come first.
        let mut frame = live.next(WITHIN).await.unwrap();
        if frame.event.as_deref() == Some("whatsapp") {
            frame = live.next(WITHIN).await.unwrap();
        }
        assert_eq!(frame.event.as_deref(), Some("shutdown"), "{frame:?}");
        let said = frame.json()["after"].as_i64().unwrap();
        assert!(said == after || said == 1, "{said}");
        assert!(live.ended(WITHIN).await);
    }
    let (status, body) = refused(&h, stream(&key)).await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("shutting_down"))
    );
}

/// Every notification lost (a backend whose notifier never wakes): the
/// stream still sends the event, by its re-poll; with no re-poll in the
/// test's time, nothing comes (the control). Decisive: the re-poll.
#[tokio::test]
async fn a_lost_notification_still_delivers() {
    let deaf = |repoll: Duration| {
        Harness::with_stores_and_settings(
            Stores {
                notifier: Arc::new(Deaf::default()),
                ..Stores::memory()
            },
            settings(|l| LiveSettings { repoll, ..l }),
        )
    };
    for (repoll, delivered) in [
        (Duration::from_millis(300), true),
        (Duration::from_secs(3600), false),
    ] {
        let h = two_tenants(deaf(repoll)).await;
        let key = h.tenant_key(A, &[Scope::Events]).await;
        let mut live = opened(&h, stream(&key)).await;
        deliver_text(&h, EXAMPLE_WABA, EXAMPLE_PN, "wamid.deaf").await;
        let frame = live.next(Duration::from_secs(1)).await;
        assert_eq!(frame.is_some(), delivered, "re-poll {repoll:?}: {frame:?}");
    }
}

/// `GET /v1/events/{id}`: the tenant's own event, whole; another tenant's
/// id (with its key, or a platform key acting as it), an unknown id and
/// one of no event's shape are `404 not_found`, alike, with no detail.
#[tokio::test]
async fn one_event_by_id_is_the_tenants_only() {
    let h = two_tenants(Harness::with_settings(settings(|l| l))).await;
    let a_key = h.tenant_key(A, &[Scope::Events]).await;
    let b_key = h.tenant_key(B, &[Scope::Events]).await;
    let platform = h
        .platform_key(
            AllowedTenants::Only(vec![TenantId::parse(B).unwrap()]),
            &[Scope::Events],
        )
        .await;
    deliver_text(&h, EXAMPLE_WABA, EXAMPLE_PN, "wamid.one").await;
    let polled = h.call(Call::get("/v1/events").key(&a_key)).await.json();
    let id = polled["data"][0]["id"].as_str().unwrap().to_owned();
    let own = h
        .call(Call::get(format!("/v1/events/{id}")).key(&a_key))
        .await;
    assert_eq!(own.status, StatusCode::OK);
    assert_eq!(own.json(), polled["data"][0]);
    let mut bodies = Vec::new();
    for call in [
        Call::get(format!("/v1/events/{id}")).key(&b_key),
        Call::get(format!("/v1/events/{id}"))
            .key(&platform)
            .tenant(B),
        Call::get("/v1/events/evt_00000000000000000000000000000000").key(&a_key),
        Call::get("/v1/events/not-an-event").key(&a_key),
    ] {
        let reply = h.call(call).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
        let mut body = reply.json();
        body["error"]["request_id"] = Value::Null;
        bodies.push(body);
    }
    assert!(bodies.windows(2).all(|w| w[0] == w[1]), "{bodies:?}");
    assert_eq!(bodies[0]["error"]["code"], "not_found");
}
