//! Acceptance test M2.6 over HTTP (roadmap M2d, design D25): Meta's
//! examples of `standby_observed`, `thread_control_changed` and
//! `user_action_reported` reach their number's tenant (`GET /v1/events`,
//! `GET /v1/events/{id}`, both streams) and no other; the `types` filter
//! takes them; the document's `KnownEventType` lists them. And what the
//! inbox makes of them (roadmap L7) is what a tenant reading them through
//! the inbox routes meets: a standby copy of the customer's message opens
//! the window without an unread message, a `control_taken` refuses a
//! free-form reply `409 thread_owned_elsewhere`, unless the number's
//! `ReplyChecks` (`WA_SERVER_INBOX_REPLY_CHECKS`) turn the check off. The
//! core's suites hold the routing over each backend
//! (`conformance::webhooks`, `conformance::live`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::time::Duration;

use common::inbox::{TOKEN, accepted, reply};
use common::meta::{EXAMPLE_PN, EXAMPLE_WA_ID, EXAMPLE_WABA, bytes, sent_now};
use common::sse::{Sse, open};
use common::{Call, Harness, test_settings};
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::inbox::ReplyChecks;
use meta_whatsapp_rs::webhooks::WebhookPayload;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_server::events::{TENANT_EVENT_TYPES, event_data};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::conformance::webhooks::{REVIEWED_EXAMPLES, example_on};
use meta_whatsapp_server_core::inbox::ReplyChecksByNumber;
use meta_whatsapp_server_core::live::LiveSettings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const A: &str = "tenant-a";
const B: &str = "tenant-b";
const WABA_B: &str = "102290129340399";
const PN_B: &str = "106540352242923";
const WITHIN: Duration = Duration::from_secs(2);
const QUIET: Duration = Duration::from_millis(400);
/// The three types D25 made tenant-visible.
const REVIEWED: [&str; 3] = [
    "standby_observed",
    "thread_control_changed",
    "user_action_reported",
];

/// `settings`, the streams woken only.
fn woken_only(settings: Settings) -> Settings {
    Settings {
        live: LiveSettings {
            heartbeat: Duration::from_secs(3600),
            repoll: Duration::from_secs(3600),
            ..LiveSettings::default()
        },
        ..settings
    }
}

async fn opened(h: &Harness, call: Call) -> Sse {
    match open(&h.internal, call).await {
        Ok(sse) => sse,
        Err(reply) => panic!("the stream was refused: {} {}", reply.status, reply.text),
    }
}

/// Meta's example `i`, on `pn` of `waba`, dated `at`.
fn example(i: usize, waba: &str, pn: &str, tag: &str, at: i64) -> Value {
    example_on(REVIEWED_EXAMPLES[i].1, waba, pn, &format!("{tag}-{i}"), at)
}

/// What the service serves an example as: the library's `WebhookEvent`
/// JSON (its snapshot, `tests/event_data.rs`).
fn served(payload: &Value) -> Value {
    let events = WebhookPayload::from_slice(&bytes(payload))
        .unwrap()
        .into_events();
    let [event] = events.as_slice() else {
        panic!("{events:?}")
    };
    serde_json::from_str(&event_data(event).unwrap()).unwrap()
}

/// M2.6, decisive: each of Meta's examples, delivered on tenant A's number,
/// is A's (its poll, in order, with the data the snapshots pin; by id; its
/// stream and its number's stream) and no other tenant's (B polls,
/// streams and reads by id none). Taking one of the three types out of
/// `TENANT_EVENT_TYPES` fails it.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn m2_6_the_reviewed_types_reach_their_numbers_tenant_and_no_other() {
    let h = Harness::with_settings(woken_only(test_settings()));
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
        .await;
    h.connect(B, WABA_B, &[PN_B], "TOKEN-OF-B").await;
    let key_a = h.tenant_key(A, &[Scope::Events]).await;
    let key_b = h.tenant_key(B, &[Scope::Events]).await;
    let mut of_a = opened(&h, Call::get("/v1/events/stream").key(&key_a)).await;
    let mut on_as_number = opened(
        &h,
        Call::get(format!("/v1/numbers/{EXAMPLE_PN}/events/stream")).key(&key_a),
    )
    .await;
    let mut of_b = opened(&h, Call::get("/v1/events/stream").key(&key_b)).await;
    let mut on_bs_number = opened(
        &h,
        Call::get(format!("/v1/numbers/{PN_B}/events/stream")).key(&key_b),
    )
    .await;
    let at = sent_now();
    let mut data = Vec::new();
    for (i, (kind, _)) in REVIEWED_EXAMPLES.iter().enumerate() {
        let payload = example(
            i,
            EXAMPLE_WABA,
            EXAMPLE_PN,
            "m2-6",
            at + i64::try_from(i).unwrap(),
        );
        let reply = h.webhook(&bytes(&payload)).await;
        assert_eq!(reply.status, StatusCode::OK, "{i} ({kind}): {}", reply.text);
        data.push(served(&payload));
    }
    let polled = h.call(Call::get("/v1/events").key(&key_a)).await;
    assert_eq!(polled.status, StatusCode::OK, "{}", polled.text);
    let envelopes = polled.json()["data"].as_array().unwrap().clone();
    assert_eq!(
        envelopes
            .iter()
            .map(|e| e["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        REVIEWED_EXAMPLES.map(|(kind, _)| kind),
        "A polls each example, in order"
    );
    for (i, envelope) in envelopes.iter().enumerate() {
        assert_eq!(envelope["data"], data[i], "example {i}'s data");
        assert_eq!(envelope["tenant_id"], A);
        assert_eq!(envelope["phone_number_id"], EXAMPLE_PN);
        assert_eq!(envelope["waba_id"], EXAMPLE_WABA);
        for (name, stream) in [("a's stream", &mut of_a), ("a's number", &mut on_as_number)] {
            assert_eq!(&stream.event(WITHIN).await.json(), envelope, "{name}, {i}");
        }
        let id = envelope["id"].as_str().unwrap();
        let own = h
            .call(Call::get(format!("/v1/events/{id}")).key(&key_a))
            .await;
        assert_eq!(&own.json(), envelope, "A reads it by id");
        let other = h
            .call(Call::get(format!("/v1/events/{id}")).key(&key_b))
            .await;
        assert_eq!(
            (other.status, other.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "B reads A's by id"
        );
    }
    let of_b_polled = h.call(Call::get("/v1/events").key(&key_b)).await;
    assert_eq!(of_b_polled.json()["data"], json!([]), "B polled A's");
    for (name, stream) in [("b's stream", &mut of_b), ("b's number", &mut on_bs_number)] {
        assert!(stream.next(QUIET).await.is_none(), "{name} saw A's");
    }
    // The filter takes each type, alone or together.
    for (types, expected) in [
        ("standby_observed", 5),
        ("thread_control_changed", 5),
        ("user_action_reported", 1),
        (
            "standby_observed,thread_control_changed,user_action_reported",
            11,
        ),
    ] {
        let filtered = h
            .call(Call::get(format!("/v1/events?types={types}")).key(&key_a))
            .await;
        assert_eq!(
            filtered.status,
            StatusCode::OK,
            "{types}: {}",
            filtered.text
        );
        assert_eq!(
            filtered.json()["data"].as_array().unwrap().len(),
            expected,
            "{types}"
        );
    }
}

/// The document's `KnownEventType` is `TENANT_EVENT_TYPES`, the three
/// reviewed types and the number events among them, so a generated client
/// switches on them (`EventType` stays `anyOf[KnownEventType, string]`).
#[test]
fn the_reviewed_types_are_known_event_types_of_the_document() {
    let document: Value = serde_json::from_str(include_str!("../openapi/v1.json")).unwrap();
    let known: Vec<&str> = document["components"]["schemas"]["KnownEventType"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(known, TENANT_EVENT_TYPES);
    for kind in REVIEWED.iter().chain(&[
        "number_connected",
        "number_disconnected",
        "number_reconnect_required",
    ]) {
        assert!(known.contains(kind), "{kind}");
    }
    assert_eq!(
        document["components"]["schemas"]["EventType"]["anyOf"],
        json!([{"$ref": "#/components/schemas/KnownEventType"}, {"type": "string"}])
    );
}

/// What the inbox records from the reviewed types (roadmap L7,
/// OPEN_QUESTIONS #44) is what the tenant polling them meets on M2a's and
/// M2g's routes. Meta's standby copy of the customer's message: the tenant
/// polls `standby_observed`; the inbox records a window (the window
/// route's `closes_at`) and another app owning the thread, so a free-form
/// reply is `409 thread_owned_elsewhere` with zero requests (and the
/// window route says it is not open), unless the number's `ReplyChecks`
/// (`WA_SERVER_INBOX_REPLY_CHECKS`) turn the ownership check off: then the
/// reply goes, on the window the standby copy opened (with no copy, the
/// same reply is `409 customer_service_window_closed`). Meta's
/// `control_taken`: the tenant polls `thread_control_changed`, and the
/// reply is refused the same way. Decisive: the inbox's recordings (a
/// standby copy as a window event and an owner; the handover's owner) and
/// the per-number setting.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn the_inbox_answers_what_the_reviewed_events_say() {
    // Meta's standby inbound message, and its `control_taken` from
    // `webhooks/reference/messaging-handovers`, on the same customer.
    let standby = REVIEWED_EXAMPLES
        .iter()
        .position(|(kind, example)| {
            *kind == "standby_observed" && example.contains("Test standby message")
        })
        .unwrap();
    let taken = REVIEWED_EXAMPLES
        .iter()
        .position(|(kind, example)| {
            *kind == "thread_control_changed"
                && example.contains("control_taken")
                && example.contains("previous_owner_app_id")
        })
        .unwrap();
    let window = format!("/v1/numbers/{EXAMPLE_PN}/conversations/{EXAMPLE_WA_ID}/window");
    // (the ownership check on, a standby copy first)
    for (owner_checked, copied) in [(true, true), (false, true), (false, false)] {
        let case = format!("ownership checked: {owner_checked}, a standby copy: {copied}");
        let mut settings = test_settings();
        if !owner_checked {
            settings.inbox_reply_checks = ReplyChecksByNumber::new().with(
                PhoneNumberId::new(EXAMPLE_PN).unwrap(),
                ReplyChecks::ALL.thread_owner(false),
            );
        }
        let h = Harness::with_settings(settings);
        h.tenant(A).await;
        h.connect(A, EXAMPLE_WABA, &[EXAMPLE_PN], TOKEN).await;
        let key = h.tenant_key(A, &[Scope::Inbox, Scope::Events]).await;
        let at = sent_now();
        let mut expected_types = Vec::new();
        if copied {
            let copy = h
                .webhook(&bytes(&example(
                    standby,
                    EXAMPLE_WABA,
                    EXAMPLE_PN,
                    "inbox",
                    at,
                )))
                .await;
            assert_eq!(copy.status, StatusCode::OK, "{case}: {}", copy.text);
            expected_types.push("standby_observed");
            h.clock
                .set(time::OffsetDateTime::now_utc() + time::Duration::seconds(3));
            let listed = h
                .call(Call::get(format!("/v1/numbers/{EXAMPLE_PN}/conversations")).key(&key))
                .await
                .json();
            assert!(
                listed["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|c| c["unread"] == 0),
                "{case}: a standby copy is no unread message: {listed}"
            );
            let answered = h.call(Call::get(window.as_str()).key(&key)).await;
            assert_eq!(answered.status, StatusCode::OK, "{case}: {}", answered.text);
            let answered = answered.json();
            assert!(
                answered["closes_at"].is_string(),
                "{case}: the copy opened the window: {answered}"
            );
            assert_eq!(
                answered["open"], !owner_checked,
                "{case}: what a reply meets"
            );
            let requests = h.graph.requests().len();
            if owner_checked {
                let answer = h
                    .call(reply(&key, EXAMPLE_PN, EXAMPLE_WA_ID, "k-standby", "Hello"))
                    .await;
                assert_eq!(
                    (answer.status, answer.code().as_str()),
                    (StatusCode::CONFLICT, "thread_owned_elsewhere"),
                    "{case}: {}",
                    answer.text
                );
                assert_eq!(h.graph.requests().len(), requests, "{case}: zero requests");
            } else {
                h.graph.push_json(200, accepted("wamid.AFTER-STANDBY"));
                let answer = h
                    .call(reply(&key, EXAMPLE_PN, EXAMPLE_WA_ID, "k-standby", "Hello"))
                    .await;
                assert_eq!(
                    answer.status,
                    StatusCode::ACCEPTED,
                    "{case}: {}",
                    answer.text
                );
            }
        }
        let handover = h
            .webhook(&bytes(&example(
                taken,
                EXAMPLE_WABA,
                EXAMPLE_PN,
                "inbox",
                at + 1,
            )))
            .await;
        assert_eq!(handover.status, StatusCode::OK, "{case}: {}", handover.text);
        expected_types.push("thread_control_changed");
        h.clock
            .set(time::OffsetDateTime::now_utc() + time::Duration::seconds(4));
        let polled = h.call(Call::get("/v1/events").key(&key)).await.json();
        let polled = polled["data"].as_array().unwrap();
        assert_eq!(
            polled
                .iter()
                .map(|e| e["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected_types,
            "{case}: the tenant polls what the inbox recorded from"
        );
        assert_eq!(
            polled.last().unwrap()["data"]["update"]["type"],
            "control_taken"
        );
        let requests = h.graph.requests().len();
        let (status, code) = match (owner_checked, copied) {
            (true, _) => (StatusCode::CONFLICT, "thread_owned_elsewhere"),
            (false, true) => (StatusCode::ACCEPTED, ""),
            (false, false) => (StatusCode::CONFLICT, "customer_service_window_closed"),
        };
        if status == StatusCode::ACCEPTED {
            h.graph.push_json(200, accepted("wamid.PARTNER"));
        }
        let answer = h
            .call(reply(
                &key,
                EXAMPLE_PN,
                EXAMPLE_WA_ID,
                "k-taken",
                "Escalation here",
            ))
            .await;
        assert_eq!(answer.status, status, "{case}: {}", answer.text);
        if status == StatusCode::ACCEPTED {
            assert_eq!(h.graph.requests().len(), requests + 1, "{case}");
        } else {
            assert_eq!(answer.code(), code, "{case}");
            assert_eq!(h.graph.requests().len(), requests, "{case}: zero requests");
        }
        assert_eq!(h.graph.remaining(), 0, "{case}");
    }
}
