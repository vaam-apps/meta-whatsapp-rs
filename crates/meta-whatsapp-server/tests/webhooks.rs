//! `POST /webhooks/meta` (docs/design/server.md, section 2.3) and
//! acceptance test M1.2: a delivery signed with
//! `meta_whatsapp_rs::webhooks::sign` is `200` and one outbox row for the
//! owning tenant; no signature is `401` without the body being polled; the
//! same body twice is one row; 3 MiB + 1 byte is `413`; `unknown` and
//! `unparsed` are operator-only. Decisive: routing an unowned number's
//! event to a tenant (`routing_is_an_allow_list`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

use futures::StreamExt as _;

use common::meta::{
    EXAMPLE_PN, EXAMPLE_TEXT, EXAMPLE_WABA, EXAMPLE_WAMID, bytes, example_text, fixture, status,
    template_approved, text, unknown_field, with_ids,
};
use common::{APP_SECRET, Call, Harness, PREVIOUS_APP_SECRET, Reply, Stores, send, signed};
use meta_whatsapp_rs::core::secret::AppSecret;
use meta_whatsapp_rs::core::store::{ConversationKey, KvStore};
use meta_whatsapp_rs::webhooks::axum::body::{Body, Bytes};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_rs::webhooks::{Claim, DedupGuard, WebhookEvent, WebhookPayload, dedup, sign};
use meta_whatsapp_server::events::{Inbound, event_data};
use meta_whatsapp_server::model::Scope;
use serde_json::{Value, json};

const A: &str = "tenant-a";
const B: &str = "tenant-b";
/// Tenant A holds Meta's example WABA and number.
const WABA_A: &str = EXAMPLE_WABA;
const PN_A: &str = EXAMPLE_PN;
const WABA_B: &str = "102290129340399";
const PN_B: &str = "106540352242923";

/// Two tenants: A holds Meta's example WABA and number, B another.
async fn two_tenants() -> Harness {
    let h = Harness::new();
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, WABA_A, &[PN_A], "TOKEN-OF-A").await;
    h.connect(B, WABA_B, &[PN_B], "TOKEN-OF-B").await;
    h
}

/// Every event `tenant` can poll, as JSON.
async fn polled(h: &Harness, tenant: &str) -> Vec<Value> {
    let key = h.tenant_key(tenant, &[Scope::Events]).await;
    let reply = h.call(Call::get("/v1/events").key(&key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    reply.json()["data"].as_array().unwrap().clone()
}

/// The value of a metric series, 0 when absent.
fn metric(h: &Harness, series: &str) -> u64 {
    h.state
        .metrics()
        .render()
        .lines()
        .find_map(|line| line.strip_prefix(series)?.trim().parse().ok())
        .unwrap_or(0)
}

/// The library's events of `body`.
fn events_of(body: &[u8]) -> Vec<WebhookEvent> {
    WebhookPayload::from_slice(body).unwrap().into_events()
}

/// The messages the inbox holds for a contact of `pn`.
async fn inbox(h: &Harness, pn: &str, contact: &str) -> usize {
    h.conversations
        .messages(&ConversationKey::new(pn, contact), None, 100)
        .await
        .unwrap()
        .len()
}

/// M1.2, first clause: signed as Meta signs, `200` and one outbox row for
/// the tenant owning the number, whose `data` is the library's
/// `WebhookEvent` JSON; the inbox holds the message before the row exists.
#[tokio::test]
async fn a_signed_delivery_is_200_and_one_row_for_the_owning_tenant() {
    let h = two_tenants().await;
    let body = example_text();
    let reply = h.webhook(&body).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert!(reply.text.is_empty(), "Meta reads the status only");

    let rows = h.outbox.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(
        row.tenant
            .as_ref()
            .map(meta_whatsapp_server::model::TenantId::as_str),
        Some(A)
    );
    assert_eq!(row.event_type, "message_received");
    let [event] = events_of(&body).try_into().unwrap();
    assert_eq!(row.data, event_data(&event).unwrap());

    let events = polled(&h, A).await;
    assert_eq!(events.len(), 1);
    let envelope = &events[0];
    assert_eq!(envelope["type"], "message_received");
    assert_eq!(envelope["tenant_id"], A);
    assert_eq!(envelope["phone_number_id"], PN_A);
    assert_eq!(envelope["waba_id"], WABA_A);
    assert_eq!(envelope["api_version"], "v1");
    assert_eq!(envelope["truncated"], false);
    assert!(envelope["id"].as_str().unwrap().starts_with("evt_"));
    assert_eq!(envelope["data"]["event"], "message_received");
    assert_eq!(envelope["data"]["message"]["text"]["body"], EXAMPLE_TEXT);
    assert_eq!(
        envelope["data"],
        serde_json::to_value(&event).unwrap(),
        "data is the library's WebhookEvent JSON"
    );
    assert!(polled(&h, B).await.is_empty(), "B sees none of A's");
    // Recorded in A's inbox too (inbox first, then the outbox).
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 1);
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_deliveries_total{outcome=\"delivered\"}"
        ),
        1
    );
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_events_total{event_type=\"message_received\",audience=\"tenant\"}"
        ),
        1
    );
    assert!(
        h.graph.requests().is_empty(),
        "receiving calls no Graph API"
    );
}

/// A body whose reads flag `polled`.
fn watched(body: &'static [u8], polled: &Arc<AtomicBool>) -> Body {
    let flag = polled.clone();
    let mut sent = false;
    Body::from_stream(futures::stream::poll_fn(move |_| {
        flag.store(true, Ordering::SeqCst);
        if sent {
            Poll::Ready(None)
        } else {
            sent = true;
            Poll::Ready(Some(Ok::<_, std::io::Error>(Bytes::from_static(body))))
        }
    }))
}

/// M1.2: no signature, or a malformed one, is `401` and the body is never
/// polled; a well-formed signature that no app secret produced is `401`
/// and records nothing; the previous app secret (while rotating) is
/// accepted. Decisive: the signature check, and its place before the body.
#[tokio::test]
async fn without_a_valid_signature_it_is_401_and_nothing_is_recorded() {
    let h = two_tenants().await;
    let body: &'static [u8] = example_text().leak();
    for header in [
        None,
        Some("sha256=".to_owned()),
        Some("sha256=zz".to_owned()),
        Some(format!("sha1={}", "ab".repeat(20))),
        Some("not a signature".to_owned()),
    ] {
        let polled = Arc::new(AtomicBool::new(false));
        let mut call = Call::new(Method::POST, "/webhooks/meta")
            .header("content-type", "application/json")
            .body(watched(body, &polled));
        if let Some(header) = &header {
            call = call.header("x-hub-signature-256", header);
        }
        let reply = send(&h.public, call.build()).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{header:?}");
        assert!(
            !polled.load(Ordering::SeqCst),
            "{header:?}: the body was read"
        );
    }
    // Well-formed, but signed with another secret, or over another body.
    for signature in [
        sign(&AppSecret::new("not-the-app-secret"), body),
        sign(&AppSecret::new(common::APP_SECRET), b"another body"),
    ] {
        let reply = send(
            &h.public,
            Call::new(Method::POST, "/webhooks/meta")
                .header("x-hub-signature-256", &signature)
                .body(Body::from(body))
                .build(),
        )
        .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    }
    assert!(h.outbox.inserts().is_empty(), "nothing reached the sink");
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 0);
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_deliveries_total{outcome=\"unauthenticated\"}"
        ),
        7
    );
    // The previous secret, while rotating: accepted.
    let reply = send(
        &h.public,
        Call::new(Method::POST, "/webhooks/meta")
            .header(
                "x-hub-signature-256",
                &sign(&AppSecret::new(PREVIOUS_APP_SECRET), body),
            )
            .body(Body::from(body))
            .build(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(h.outbox.rows().len(), 1);
}

/// Fail closed: the pipeline refuses to exist with no app secret or a
/// blank one (configuration refuses them first: tests/config.rs).
#[test]
fn a_blank_or_missing_app_secret_builds_no_pipeline() {
    let kv = Arc::new(meta_whatsapp_rs::adapters::store::MemoryKvStore::new());
    for secrets in [vec![], vec![AppSecret::new("")], vec![AppSecret::new("  ")]] {
        let built = Inbound::new(
            secrets,
            kv.clone(),
            Arc::new(meta_whatsapp_rs::adapters::store::MemoryConversationStore::new()),
            Arc::new(meta_whatsapp_server::store::MemoryEventStore::new()),
        );
        assert!(built.is_err());
    }
}

/// M1.2: the same body twice is one row (and one inbox message); the
/// second delivery is `200`, counted as a duplicate by the dedup lease.
#[tokio::test]
async fn the_same_body_twice_is_one_row() {
    let h = two_tenants().await;
    let body = example_text();
    for _ in 0..2 {
        assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    }
    assert_eq!(
        h.outbox.inserts().len(),
        1,
        "the second never reached the sink"
    );
    assert_eq!(polled(&h, A).await.len(), 1);
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 1);
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_duplicate_events_total{stage=\"dedup\"}"
        ),
        1
    );
}

/// When the dedup marker is lost (a crash after the outbox write and
/// before the marker, a lease that ended), the outbox's own key still
/// makes the redelivery a no-op. Decisive: the outbox insert's dedup key.
#[tokio::test]
async fn a_lost_dedup_marker_still_gives_one_row() {
    let h = two_tenants().await;
    let body = example_text();
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    let marker = dedup::store_key(EXAMPLE_WAMID);
    assert!(h.kv.delete(&marker).await.unwrap(), "the marker existed");
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    let inserts = h.outbox.inserts();
    assert_eq!(inserts.len(), 2, "the sink ran again");
    assert_eq!(inserts[1].1, None, "and wrote nothing");
    assert_eq!(polled(&h, A).await.len(), 1);
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 1);
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_duplicate_events_total{stage=\"outbox\"}"
        ),
        1
    );
}

/// `body` padded with spaces (still the same JSON) to exactly `len` bytes.
fn padded(body: &[u8], len: usize) -> Vec<u8> {
    let mut out = body.to_vec();
    out.resize(len, b' ');
    out
}

/// M1.2: a body of exactly 3 MiB is read and recorded; one byte more is
/// `413` and records nothing, with its length announced or streamed.
/// Decisive: the public listener's body limit (axum's own is 2 MiB).
#[tokio::test]
async fn three_mib_is_read_and_one_byte_more_is_413() {
    const LIMIT: usize = 3 * 1024 * 1024;
    assert_eq!(meta_whatsapp_server::events::MAX_WEBHOOK_BODY_BYTES, LIMIT);
    let h = two_tenants().await;
    let at_limit = padded(&bytes(&text(WABA_A, PN_A, "wamid.AT-THE-LIMIT")), LIMIT);
    let reply = h.webhook(&at_limit).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(h.outbox.rows().len(), 1);

    let over = padded(
        &bytes(&text(WABA_A, PN_A, "wamid.OVER-THE-LIMIT")),
        LIMIT + 1,
    );
    let reply = h.webhook(&over).await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
    // Streamed, without a length.
    let signature = sign(&AppSecret::new(common::APP_SECRET), &over);
    let chunks: Vec<Result<Bytes, std::io::Error>> = over
        .chunks(64 * 1024)
        .map(|c| Ok(Bytes::copy_from_slice(c)))
        .collect();
    let reply = send(
        &h.public,
        Call::new(Method::POST, "/webhooks/meta")
            .header("x-hub-signature-256", &signature)
            .body(Body::from_stream(futures::stream::iter(chunks)))
            .build(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(h.outbox.rows().len(), 1, "nothing more recorded");
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_deliveries_total{outcome=\"payload_too_large\"}"
        ),
        2
    );
}

/// The tenant each case's rows went to (`None`: operator-only).
fn tenants_of(rows: &[meta_whatsapp_server::store::events::NewEvent]) -> Vec<Option<String>> {
    rows.iter()
        .map(|r| r.tenant.as_ref().map(|t| t.as_str().to_owned()))
        .collect()
}

/// M1.2's decisive test: routing is an allow-list. An event reaches a
/// tenant only when the tenant holds its number (under the WABA the event
/// names) or, for an event naming no number, its WABA. An unowned number's
/// event is operator-only, even under a WABA the tenant holds, and so is a
/// number the bindings put under another WABA than Meta says. Decisive:
/// routing an unowned number's event to a tenant.
#[tokio::test]
async fn routing_is_an_allow_list() {
    let h = two_tenants().await;
    let cases: Vec<(&str, Value, Option<&str>)> = vec![
        ("A's number", text(WABA_A, PN_A, "wamid.1"), Some(A)),
        ("B's number", text(WABA_B, PN_B, "wamid.2"), Some(B)),
        (
            "an unbound number under A's WABA",
            text(WABA_A, "106540352249999", "wamid.3"),
            None,
        ),
        (
            "an unbound number under an unbound WABA",
            text("102290129349999", "106540352249999", "wamid.4"),
            None,
        ),
        (
            "A's number under B's WABA",
            text(WABA_B, PN_A, "wamid.5"),
            None,
        ),
        (
            "A's number under an unbound WABA",
            text("102290129349999", PN_A, "wamid.6"),
            None,
        ),
        ("a status of A's number", status(WABA_A, PN_A), Some(A)),
        ("A's WABA, no number", template_approved(WABA_A), Some(A)),
        (
            "an unbound WABA, no number",
            template_approved("102290129349999"),
            None,
        ),
        (
            "an error of B's number",
            with_ids(fixture("messages/errors.json"), WABA_B, PN_B),
            Some(B),
        ),
    ];
    for (i, (case, payload, owner)) in cases.iter().enumerate() {
        let reply = h.webhook(&bytes(payload)).await;
        assert_eq!(reply.status, StatusCode::OK, "{case}");
        let rows = h.outbox.rows();
        assert_eq!(rows.len(), i + 1, "{case}");
        assert_eq!(tenants_of(&rows[i..]), [owner.map(str::to_owned)], "{case}");
    }
    // Each tenant polls exactly its own.
    let a: Vec<String> = polled(&h, A)
        .await
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        a,
        [
            "message_received",
            "status_updated",
            "template_status_updated"
        ]
    );
    let b: Vec<String> = polled(&h, B)
        .await
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(b, ["message_received", "error_reported"]);
    // The inbox records only owned numbers' messages.
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 1);
    assert_eq!(inbox(&h, "106540352249999", "16505551234").await, 0);
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_events_total{event_type=\"message_received\",audience=\"operator\"}"
        ),
        4
    );
}

/// M1.2: `unknown` and `unparsed` events, and `partner_solution_updated`,
/// are operator-only rows, even for a WABA a tenant holds: no tenant polls
/// them, not even by asking for their type. Decisive: the type allow-list.
#[tokio::test]
async fn unknown_unparsed_and_partner_events_are_operator_only() {
    let h = two_tenants().await;
    let bodies: Vec<(&str, Vec<u8>)> = vec![
        ("unknown", bytes(&unknown_field(WABA_A))),
        ("unparsed", br#"{"not": "a webhook envelope"}"#.to_vec()),
        ("unparsed", b"not even JSON".to_vec()),
        (
            "partner_solution_updated",
            bytes(&fixture("fields/partner_solutions.json")),
        ),
    ];
    for (kind, body) in &bodies {
        let reply = h.webhook(body).await;
        assert_eq!(reply.status, StatusCode::OK, "{kind}: acknowledged");
    }
    let rows = h.outbox.rows();
    let kinds: Vec<&str> = rows.iter().map(|r| r.event_type.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "unknown",
            "unparsed",
            "unparsed",
            "partner_solution_updated"
        ]
    );
    assert!(rows.iter().all(|r| r.tenant.is_none()), "{rows:?}");
    for tenant in [A, B] {
        assert!(polled(&h, tenant).await.is_empty(), "{tenant}");
    }
    // Asking for them by type is refused, not answered empty-handed.
    let key = h.tenant_key(A, &[Scope::Events]).await;
    for kind in ["unknown", "unparsed", "partner_solution_updated"] {
        let reply = h
            .call(Call::get(format!("/v1/events?types={kind}")).key(&key))
            .await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{kind}"
        );
    }
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_events_total{event_type=\"unknown\",audience=\"operator\"}"
        ),
        1
    );
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_events_total{event_type=\"unparsed\",audience=\"operator\"}"
        ),
        2
    );
}

/// Another request holds an event's dedup lease (another replica is
/// delivering it): `503`, nothing recorded, and Meta's retry after the
/// claim is released records it once. Decisive: the dedup guard.
#[tokio::test]
async fn a_claim_in_flight_answers_503_and_records_nothing() {
    let h = two_tenants().await;
    let body = example_text();
    let [event] = events_of(&body).try_into().unwrap();
    let guard = DedupGuard::new(h.kv.clone());
    let Claim::Acquired(ticket) = guard.claim(&event).await.unwrap() else {
        panic!("claimed");
    };
    let reply = h.webhook(&body).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(h.outbox.inserts().is_empty());
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 0);
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_deliveries_total{outcome=\"in_flight\"}"
        ),
        1
    );
    assert!(guard.release(&ticket).await.unwrap());
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    assert_eq!(h.outbox.rows().len(), 1);
}

/// A failure between the inbox write and the outbox write: `500`, the
/// message is in the inbox and no row exists; the claim was released, so
/// Meta's redelivery records the row, and the inbox keeps one message.
#[tokio::test]
async fn a_failure_after_the_inbox_is_redelivered_safely() {
    let h = two_tenants().await;
    let body = example_text();
    h.outbox.fail_next(1);
    let reply = h.webhook(&body).await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 1, "inbox first");
    assert!(h.outbox.rows().is_empty());
    assert!(polled(&h, A).await.is_empty());
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_sink_failures_total{stage=\"outbox\"}"
        ),
        1
    );
    assert_eq!(
        metric(&h, "wa_server_webhook_deliveries_total{outcome=\"failed\"}"),
        1
    );
    // Meta redelivers.
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    assert_eq!(h.outbox.rows().len(), 1);
    assert_eq!(polled(&h, A).await.len(), 1);
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 1, "one message");
}

/// A batch: its events are recorded in order, one row each, with
/// increasing sequences.
#[tokio::test]
async fn a_batch_is_recorded_in_order() {
    let h = two_tenants().await;
    let body = bytes(&batch(&["wamid.first", "wamid.second"]));
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    let events = polled(&h, A).await;
    let types: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        ["message_received", "message_received", "status_updated"]
    );
    let sequences: Vec<i64> = events
        .iter()
        .map(|e| e["sequence"].as_i64().unwrap())
        .collect();
    assert!(sequences.windows(2).all(|w| w[0] < w[1]), "{sequences:?}");
}

/// Meta's text example with one message per id, then Meta's status
/// example, for A.
fn batch(wamids: &[&str]) -> Value {
    let mut payload = text(WABA_A, PN_A, wamids[0]);
    let template = payload["entry"][0]["changes"][0]["value"]["messages"][0].clone();
    for wamid in &wamids[1..] {
        let mut message = template.clone();
        message["id"] = json!(wamid);
        payload["entry"][0]["changes"][0]["value"]["messages"]
            .as_array_mut()
            .unwrap()
            .push(message);
    }
    let status = status(WABA_A, PN_A);
    payload["entry"]
        .as_array_mut()
        .unwrap()
        .push(status["entry"][0].clone());
    payload
}

/// A batch that fails part-way (its second event's outbox write): the
/// first stays recorded, `500`, and Meta's redelivery records the rest
/// without repeating the first (a duplicate at the dedup lease).
#[tokio::test]
async fn a_batch_failing_part_way_is_completed_by_the_redelivery() {
    let h = two_tenants().await;
    let body = bytes(&batch(&["wamid.one", "wamid.two"]));
    h.outbox.script(&[false, true]);
    assert_eq!(
        h.webhook(&body).await.status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(h.outbox.rows().len(), 1);
    let reply: Reply = h.webhook(&body).await;
    assert_eq!(reply.status, StatusCode::OK);
    let events = polled(&h, A).await;
    let ids: Vec<&str> = events
        .iter()
        .filter_map(|e| e["data"]["message"]["id"].as_str())
        .collect();
    assert_eq!(ids, ["wamid.one", "wamid.two"]);
    assert_eq!(events.len(), 3, "and the status");
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 2);
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_duplicate_events_total{stage=\"dedup\"}"
        ),
        1
    );
}

/// The public listener's POST is the webhook, nothing else: a signed body
/// sent anywhere else on it is `404`, and the internal listener has no
/// webhook.
#[tokio::test]
async fn deliveries_are_taken_on_the_public_listener_only() {
    let h = two_tenants().await;
    let body = example_text();
    let mut elsewhere = signed(&body).build();
    *elsewhere.uri_mut() = "/webhooks/meta/extra".parse().unwrap();
    assert_eq!(
        send(&h.public, elsewhere).await.status,
        StatusCode::NOT_FOUND
    );
    let internal = send(&h.internal, signed(&body).build()).await;
    assert_eq!(internal.status, StatusCode::NOT_FOUND);
    assert!(h.outbox.inserts().is_empty());
}

/// Fix #1 of the M1c review, in memory: a tenant deleted and created again
/// under the same id polls nothing of the deleted one's events
/// (`common::scenarios`; on Postgres: `live_postgres.rs`).
#[tokio::test]
async fn a_recreated_tenant_polls_nothing_from_before() {
    common::scenarios::a_recreated_tenant_polls_nothing_from_before(&Harness::new()).await;
}

/// Meta's error example for `pn` of `waba`: an `error_reported` event, one
/// the library gives no dedup key.
fn error(waba: &str, pn: &str) -> Value {
    with_ids(fixture("messages/errors.json"), waba, pn)
}

/// Fix #2 of the M1c review (M1.2: the same body twice is one row), for
/// the events the library gives no dedup key: a batch holding an error,
/// failing after it, is redelivered; the error is recorded once, and so is
/// every other event. Decisive: keying keyless events by the body and
/// their position in it.
#[tokio::test]
async fn a_redelivered_batch_records_its_keyless_events_once() {
    let h = two_tenants().await;
    let mut body = text(WABA_A, PN_A, "wamid.keyless-1");
    let entries = body["entry"].as_array_mut().unwrap();
    entries.push(error(WABA_A, PN_A)["entry"][0].clone());
    entries.push(text(WABA_A, PN_A, "wamid.keyless-2")["entry"][0].clone());
    let body = bytes(&body);
    // The third event's outbox write fails: 500, Meta redelivers.
    h.outbox.script(&[false, false, true]);
    assert_eq!(
        h.webhook(&body).await.status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    for _ in 0..2 {
        assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    }
    let kinds: Vec<String> = polled(&h, A)
        .await
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        kinds,
        ["message_received", "error_reported", "message_received"]
    );
    // The error reached the outbox on every delivery; it wrote once.
    let errors: Vec<Option<i64>> = h
        .outbox
        .inserts()
        .into_iter()
        .filter(|(row, _)| row.event_type == "error_reported")
        .map(|(_, sequence)| sequence)
        .collect();
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert!(errors[0].is_some() && errors[1..].iter().all(Option::is_none));
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_duplicate_events_total{stage=\"outbox\"}"
        ),
        2
    );
}

/// Keyless events are one row per body and position: two identical errors
/// in one body are two rows, the same error in another body a third, and
/// an unparsed body sent twice one row (within the dedup window: the
/// harness's clock does not move here).
#[tokio::test]
async fn keyless_events_are_told_apart_by_body_and_position() {
    let h = two_tenants().await;
    let mut twice = error(WABA_A, PN_A);
    let entry = twice["entry"][0].clone();
    twice["entry"].as_array_mut().unwrap().push(entry);
    let mut another = text(WABA_A, PN_A, "wamid.beside-an-error");
    another["entry"]
        .as_array_mut()
        .unwrap()
        .push(error(WABA_A, PN_A)["entry"][0].clone());
    for body in [bytes(&twice), bytes(&another)] {
        for _ in 0..2 {
            assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
        }
    }
    let errors = polled(&h, A)
        .await
        .into_iter()
        .filter(|e| e["type"] == "error_reported")
        .count();
    assert_eq!(errors, 3);
    let unparsed = br#"{"not": "a webhook envelope"}"#;
    for _ in 0..2 {
        assert_eq!(h.webhook(unparsed).await.status, StatusCode::OK);
    }
    let unparsed_rows = h
        .outbox
        .rows()
        .into_iter()
        .filter(|r| r.event_type == "unparsed")
        .count();
    assert_eq!(unparsed_rows, 1);
}

/// Keyless events are deduplicated within an hour only
/// (`common::scenarios`; on Postgres: `live_postgres.rs`).
#[tokio::test]
async fn keyless_events_are_deduplicated_within_the_window_only() {
    common::scenarios::keyless_events_are_deduplicated_within_the_window_only(&Harness::new())
        .await;
}

/// The outbox key: the same key for the same event, another for another
/// number (I3 of the security review), another position, another body or
/// other data; library and delivery keys never meet.
#[test]
fn outbox_keys_are_scoped_by_number_body_and_position() {
    use meta_whatsapp_server::events::{EventKey, outbox_key};
    let library = EventKey::Library("wamid.X".to_owned());
    let delivery = |position| EventKey::Delivery {
        body_sha256: [7; 32],
        position,
    };
    let key = |k: &EventKey, pn: Option<&str>, data: &str| outbox_key(k, pn, data);
    assert_eq!(
        key(&library, Some("1"), "{}"),
        key(&library, Some("1"), "{\"other\": 1}"),
        "a library key does not depend on the data"
    );
    let all = [
        key(&library, Some("1"), "{}"),
        key(&library, Some("2"), "{}"),
        key(&library, None, "{}"),
        key(&delivery(0), Some("1"), "{}"),
        key(&delivery(1), Some("1"), "{}"),
        key(&delivery(0), Some("1"), "{\"other\": 1}"),
        key(
            &EventKey::Delivery {
                body_sha256: [8; 32],
                position: 0,
            },
            Some("1"),
            "{}",
        ),
    ];
    let unique: std::collections::BTreeSet<&String> = all.iter().collect();
    assert_eq!(unique.len(), all.len(), "{all:?}");
    assert!(all.iter().all(|k| k.len() == 64));
}

/// A crash between the inbox write and the outbox write: the request dies
/// there (its future dropped, as a crash or the deadline drops it), so the
/// claim is neither completed nor released. Meta's retries meet the live
/// lease (`503`, nothing recorded) until it ends (60 s: here the marker is
/// removed, as its expiry would); the next one records the row, and the
/// inbox keeps one message. Decisive: the lease, and both writes' keys.
#[tokio::test]
async fn a_crash_between_the_inbox_and_the_outbox_is_redelivered_safely() {
    let h = two_tenants().await;
    let body = example_text();
    h.outbox.fates(&[common::Fate::Hang]);
    let cut = tokio::time::timeout(std::time::Duration::from_millis(300), h.webhook(&body)).await;
    assert!(cut.is_err(), "the request was cut");
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 1, "inbox first");
    assert!(h.outbox.rows().is_empty());
    assert_eq!(
        h.webhook(&body).await.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the crashed request's lease is live"
    );
    assert!(h.outbox.rows().is_empty());
    assert!(
        h.kv.delete(&dedup::store_key(EXAMPLE_WAMID)).await.unwrap(),
        "the pending claim"
    );
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    assert_eq!(h.outbox.rows().len(), 1);
    assert_eq!(polled(&h, A).await.len(), 1);
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 1, "one message");
}

/// Security review M3: an event Meta dated before its WABA's binding began
/// is a previous holder's, operator-only, and never reaches the inbox:
/// WABA W is bound to A, unbound, bound to B; Meta's retry of a message
/// dated before B's binding goes to nobody, one dated after it to B.
/// Decisive: the binding epoch in the routing.
#[tokio::test]
async fn an_event_dated_before_its_binding_is_nobodys() {
    let h = Harness::new();
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, WABA_A, &[PN_A], "TOKEN-OF-A").await;
    assert!(
        h.store
            .unbind_waba(&meta_whatsapp_rs::core::ids::WabaId::new(WABA_A))
            .await
            .unwrap()
    );
    h.connect(B, WABA_A, &[PN_A], "TOKEN-OF-B").await;
    let an_hour_ago = common::meta::now() - 3600;
    let old = common::meta::dated(text(WABA_A, PN_A, "wamid.OF-A"), an_hour_ago);
    assert_eq!(h.webhook(&bytes(&old)).await.status, StatusCode::OK);
    assert_eq!(tenants_of(&h.outbox.rows()), [None]);
    assert!(polled(&h, B).await.is_empty());
    assert!(polled(&h, A).await.is_empty());
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 0, "no inbox write");
    let new = text(WABA_A, PN_A, "wamid.OF-B");
    assert_eq!(h.webhook(&bytes(&new)).await.status, StatusCode::OK);
    assert_eq!(polled(&h, B).await.len(), 1);
    assert_eq!(inbox(&h, PN_A, "16505551234").await, 1);
}

/// `history` Meta's `messages` variant the library could not type (one
/// malformed message): routed as a `history` of the number its raw
/// `metadata` names.
fn untyped_history(waba: &str, pn: &str) -> Value {
    let mut payload = with_ids(fixture("fields/history_threads.json"), waba, pn);
    payload["entry"][0]["changes"][0]["value"]["history"][0]["threads"][0]["messages"][1]["timestamp"] =
        json!("not a time");
    payload
}

/// Security review L1: a `history` change the library did not type is
/// recorded by the inbox under the number its raw `metadata` names, so it
/// is routed by that number, under the WABA the entry names, as typed
/// events are: under A's WABA but naming B's number, it reaches nobody's
/// inbox. Decisive: routing an untyped change by its raw number.
#[tokio::test]
async fn an_untyped_history_change_is_routed_by_its_number() {
    let h = two_tenants().await;
    let body = bytes(&untyped_history(WABA_A, PN_B));
    let [event] = events_of(&body).try_into().unwrap();
    assert_eq!(event.kind(), "unknown", "the change stays untyped");
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    assert_eq!(
        inbox(&h, PN_B, "16505551234").await,
        0,
        "B's inbox untouched"
    );
    // Its own number, under its own WABA: A's inbox recovers what parses.
    let body = bytes(&untyped_history(WABA_A, PN_A));
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    assert!(inbox(&h, PN_A, "16505551234").await > 0);
    assert!(h.outbox.rows().iter().all(|r| r.tenant.is_none()));
}

/// Security review L3: an event's id is derived from the event, so a body
/// captured and replayed once its dedup marker expired and its outbox row
/// was purged comes back under the same id, and receivers that
/// deduplicate on ids see it once. Ids name nothing: not the outbox key.
#[tokio::test]
async fn an_event_keeps_its_id_when_recorded_again() {
    let h = two_tenants().await;
    let body = example_text();
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    let first = h.outbox.rows()[0].clone();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    meta_whatsapp_server::events::purge_outbox(h.outbox.as_ref(), std::time::Duration::ZERO)
        .await
        .unwrap();
    assert!(h.kv.delete(&dedup::store_key(EXAMPLE_WAMID)).await.unwrap());
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    let rows = h.outbox.rows();
    assert_eq!(rows.len(), 2, "recorded again");
    assert_eq!(rows[1].id, first.id, "under the same id");
    assert_eq!(rows[1].dedup_key, first.dedup_key);
    let key = first.dedup_key.unwrap();
    assert!(first.id.starts_with("evt_") && first.id.len() == 36);
    assert_ne!(&first.id[4..], &key[..32], "the id is keyed");
    // Another event, another id.
    let other = bytes(&text(WABA_A, PN_A, "wamid.another"));
    assert_eq!(h.webhook(&other).await.status, StatusCode::OK);
    assert_ne!(h.outbox.rows()[2].id, first.id);
}

/// Event ids are keyed by the first app secret (`EventIdKey`): the same
/// event recorded by a deployment whose first app secret is another gets
/// another id, and the same id when only the previous secret differs. So
/// rotating `WA_APP_SECRET` changes the id of an event recorded again
/// after its row was purged. Decisive: the app secret in the ids' key.
#[tokio::test]
async fn event_ids_are_keyed_by_the_first_app_secret() {
    let body = example_text();
    let recorded = |h: Harness| {
        let body = body.clone();
        async move {
            assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
            let [row] = h.outbox.rows().try_into().unwrap();
            (row.id, row.dedup_key)
        }
    };
    let (id, key) = recorded(Harness::new()).await;
    let (rotated, rotated_key) = recorded(Harness::with_app_secrets(
        Stores::memory(),
        &[PREVIOUS_APP_SECRET, APP_SECRET],
    ))
    .await;
    let (same_first, _) = recorded(Harness::with_app_secrets(
        Stores::memory(),
        &[APP_SECRET, "another-previous-app-secret"],
    ))
    .await;
    assert_eq!(key, rotated_key, "the same event");
    assert_ne!(id, rotated, "another first app secret, another id");
    assert_eq!(id, same_first, "only the first app secret counts");
}

/// Security review L3: an event Meta dated before what the dedup lease
/// remembers (the window the caller passes; the pipeline's is below) is a
/// replay, routed to nobody whoever holds its number; within it, it routes
/// as usual.
#[tokio::test]
async fn an_event_dated_before_the_replay_window_is_nobodys() {
    use meta_whatsapp_server::events::route;
    use time::{Duration, OffsetDateTime};
    let h = two_tenants().await;
    let now = OffsetDateTime::now_utc();
    let not_before = now - Duration::days(7);
    let event_at = |at: OffsetDateTime| {
        let body = bytes(&common::meta::dated(
            text(WABA_A, PN_A, "wamid.dated"),
            at.unix_timestamp(),
        ));
        let [event] = events_of(&body).try_into().unwrap();
        event
    };
    let replay = route(
        h.store.as_ref(),
        &event_at(now - Duration::days(8)),
        not_before,
    )
    .await
    .unwrap();
    assert_eq!(
        (replay.owner, replay.tenant, replay.operator_only),
        (None, None, Some("stale"))
    );
    let fresh = route(h.store.as_ref(), &event_at(now), not_before)
        .await
        .unwrap();
    assert_eq!(
        fresh.tenant.map(|t| t.as_str().to_owned()),
        Some(A.to_owned())
    );
    assert_eq!(fresh.operator_only, None);
}

/// The pipeline's replay window is what the library's dedup markers
/// remember, `DEFAULT_DEDUP_TTL`: 7 days and an hour, the figure the design
/// (section 6), the guide, the CHANGELOG and the events skill give. On the
/// service's clock, eight days after the binding began: an event Meta
/// dated a minute inside the window reaches its tenant, one a minute
/// outside is operator-only. Decisive: `replay_window` in
/// `ServiceSink` (7 days alone, or any longer window, fails).
#[tokio::test]
async fn the_replay_window_is_seven_days_and_an_hour() {
    use meta_whatsapp_rs::webhooks::DEFAULT_DEDUP_TTL;
    use time::{Duration, OffsetDateTime};
    assert_eq!(
        DEFAULT_DEDUP_TTL,
        std::time::Duration::from_hours(7 * 24 + 1)
    );
    let h = two_tenants().await;
    let now = OffsetDateTime::now_utc() + Duration::days(8);
    h.clock.set(now);
    let window = Duration::hours(7 * 24 + 1);
    for (wamid, at) in [
        ("wamid.inside", now - window + Duration::minutes(1)),
        ("wamid.outside", now - window - Duration::minutes(1)),
    ] {
        let body = bytes(&common::meta::dated(
            text(WABA_A, PN_A, wamid),
            at.unix_timestamp(),
        ));
        assert_eq!(h.webhook(&body).await.status, StatusCode::OK, "{wamid}");
    }
    let rows = h.outbox.rows();
    let tenant = |wamid: &str| {
        let row = rows.iter().find(|r| r.data.contains(wamid)).unwrap();
        row.tenant.as_ref().map(|t| t.as_str().to_owned())
    };
    assert_eq!(tenant("wamid.inside"), Some(A.to_owned()), "inside");
    assert_eq!(tenant("wamid.outside"), None, "outside: a replay");
}

/// Security review M1: a replica reads at most 64 deliveries at once, and
/// a body has 15 s to arrive. With 64 deliveries whose bodies never come
/// (well-formed signatures cost an attacker nothing), the next delivery is
/// `503` at once (Meta retries), and each slow one is cut with `408`; then
/// deliveries go through again. Decisive: the places and the body read
/// timeout.
#[tokio::test(start_paused = true)]
async fn deliveries_past_capacity_are_503_and_slow_bodies_408() {
    use meta_whatsapp_server::events::{BODY_READ_TIMEOUT, MAX_DELIVERIES_IN_FLIGHT};
    let h = two_tenants().await;
    let body = example_text();
    let signature = sign(&AppSecret::new(common::APP_SECRET), &body);
    let slow: Vec<_> = (0..MAX_DELIVERIES_IN_FLIGHT)
        .map(|_| {
            let router = h.public.clone();
            let request = Call::new(Method::POST, "/webhooks/meta")
                .header("x-hub-signature-256", &signature)
                .body(Body::from_stream(futures::stream::pending::<
                    Result<Bytes, std::io::Error>,
                >()))
                .build();
            tokio::spawn(async move { send(&router, request).await.status })
        })
        .collect();
    // They all start reading (paused time moves once every task waits).
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let started = tokio::time::Instant::now();
    assert_eq!(
        h.webhook(&body).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(started.elapsed(), std::time::Duration::ZERO, "at once");
    for task in slow {
        assert_eq!(task.await.unwrap(), StatusCode::REQUEST_TIMEOUT);
    }
    assert!(started.elapsed() < BODY_READ_TIMEOUT);
    assert!(h.outbox.inserts().is_empty());
    assert_eq!(h.webhook(&body).await.status, StatusCode::OK);
    assert_eq!(
        metric(&h, "wa_server_webhook_deliveries_total{outcome=\"busy\"}"),
        1
    );
    assert_eq!(
        metric(
            &h,
            "wa_server_webhook_deliveries_total{outcome=\"slow_body\"}"
        ),
        64
    );
}

/// Security review M2: a delivery waits at most `RECORDING_WAIT` for its
/// turn to record, then is `503` (Meta retries). With every turn held by a
/// delivery stuck in the outbox, the next one is answered after the wait,
/// not when the stuck ones' deadline frees a turn. Decisive: the recording
/// wait.
#[tokio::test(start_paused = true)]
async fn a_delivery_with_no_turn_to_record_is_503_after_the_wait() {
    use meta_whatsapp_server::events::{MAX_DELIVERIES_RECORDING, RECORDING_WAIT};
    let h = two_tenants().await;
    h.outbox
        .fates(&[common::Fate::Hang; MAX_DELIVERIES_RECORDING]);
    let stuck: Vec<_> = (0..MAX_DELIVERIES_RECORDING)
        .map(|i| {
            let body = bytes(&text(WABA_A, PN_A, &format!("wamid.stuck-{i}")));
            let request = signed(&body).build();
            let router = h.public.clone();
            tokio::spawn(async move { send(&router, request).await.status })
        })
        .collect();
    // They take every turn (paused time moves once every task waits).
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let started = tokio::time::Instant::now();
    let reply = h
        .webhook(&bytes(&text(WABA_A, PN_A, "wamid.waiting")))
        .await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(started.elapsed(), RECORDING_WAIT);
    assert_eq!(
        metric(&h, "wa_server_webhook_deliveries_total{outcome=\"busy\"}"),
        1
    );
    assert!(h.outbox.rows().is_empty());
    for task in stuck {
        task.abort();
    }
}

/// A body that starts and then stalls is cut too.
#[tokio::test(start_paused = true)]
async fn a_stalled_body_is_cut_at_the_read_timeout() {
    use meta_whatsapp_server::events::BODY_READ_TIMEOUT;
    let h = two_tenants().await;
    let body: &'static [u8] = example_text().leak();
    let signature = sign(&AppSecret::new(common::APP_SECRET), body);
    let first = futures::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(&body[..10]))]);
    let stalled = first.chain(futures::stream::pending());
    let started = tokio::time::Instant::now();
    let reply = send(
        &h.public,
        Call::new(Method::POST, "/webhooks/meta")
            .header("x-hub-signature-256", &signature)
            .body(Body::from_stream(stalled))
            .build(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::REQUEST_TIMEOUT);
    assert_eq!(started.elapsed(), BODY_READ_TIMEOUT);
    assert!(h.outbox.inserts().is_empty());
}
