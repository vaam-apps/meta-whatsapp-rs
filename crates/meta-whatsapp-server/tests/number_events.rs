//! The service's own number events over HTTP (roadmap M2d;
//! docs/design/server.md, sections 2.3 and 3.4; the core's
//! `number_events`, whose rules over each backend are
//! `conformance::number_events`): an admin attach records
//! `number_connected` for each number Meta lists, a `190` on the number's
//! token `number_reconnect_required` once per binding generation, an
//! attach again `number_connected` again, the operator's unbind and the
//! tenant's disconnect `number_disconnected` (with their `reason`), each
//! polled, streamed and read by id by the number's tenant only.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::time::Duration;

use common::sse::{Sse, open};
use common::{Call, Harness, test_settings};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::live::LiveSettings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const A: &str = "merchant-a";
const B: &str = "merchant-b";
const WABA_A: &str = "102290129340398";
const WABA_B: &str = "102290129340399";
const PN_1: &str = "1972385232742141";
const PN_2: &str = "1972385232742142";
const PN_B: &str = "1972385232742149";
const SYSTEM_TOKEN: &str = "EAAG-system-user-token-of-the-number-events";
const WITHIN: Duration = Duration::from_secs(2);
const QUIET: Duration = Duration::from_millis(400);

/// The test settings, the streams woken only.
fn settings() -> Settings {
    Settings {
        live: LiveSettings {
            heartbeat: Duration::from_secs(3600),
            repoll: Duration::from_secs(3600),
            ..LiveSettings::default()
        },
        ..test_settings()
    }
}

/// Attach `waba` to `tenant` as the operator, Meta listing `numbers` and
/// subscribing the app.
async fn attach(h: &Harness, admin: &str, tenant: &str, waba: &str, numbers: &[&str]) {
    let data: Vec<Value> = numbers
        .iter()
        .map(|id| json!({"id": id, "display_phone_number": "+1 631-555-1111"}))
        .collect();
    h.graph.push_json(200, json!({"data": data}));
    h.graph.push_json(200, json!({"success": true}));
    let reply = h
        .call(
            Call::new(Method::POST, format!("/v1/admin/tenants/{tenant}/wabas"))
                .key(admin)
                .json(&json!({"waba_id": waba, "token": SYSTEM_TOKEN})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
}

/// Meta refusing the number's stored token (`190`), on a read of `pn`.
async fn refused_token(h: &Harness, key: &str, pn: &str) {
    h.graph.push_json(
        401,
        json!({"error": {"message": "Invalid OAuth access token", "type": "OAuthException",
                         "code": 190, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let reply = h
        .call(Call::get(format!("/v1/numbers/{pn}")).key(key))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "reconnect_required"),
        "{}",
        reply.text
    );
}

/// `tenant`'s polled events: `(type, data)`.
async fn polled(h: &Harness, key: &str) -> Vec<(String, Value)> {
    let reply = h.call(Call::get("/v1/events").key(key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    reply.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["type"].as_str().unwrap().to_owned(), e["data"].clone()))
        .collect()
}

fn event(kind: &str, pn: &str, waba: &str) -> (String, Value) {
    (
        kind.to_owned(),
        json!({"event": kind, "phone_number_id": pn, "waba_id": waba}),
    )
}

fn disconnected(pn: &str, waba: &str, reason: &str) -> (String, Value) {
    (
        "number_disconnected".to_owned(),
        json!({"event": "number_disconnected", "phone_number_id": pn, "waba_id": waba,
               "reason": reason}),
    )
}

async fn opened(h: &Harness, call: Call) -> Sse {
    match open(&h.internal, call).await {
        Ok(sse) => sse,
        Err(reply) => panic!("the stream was refused: {} {}", reply.status, reply.text),
    }
}

/// Decisive (roadmap M2d): every path that changes a binding records its
/// event for the number's tenant, with the data the core pins, and no
/// other tenant's poll or stream sees one: the attach (`number_connected`,
/// each number Meta listed), a `190` (`number_reconnect_required` for each
/// number it marks), an attach again (`number_connected` again: a new
/// binding generation), and the tenant's disconnect (`number_disconnected`,
/// `tenant_disconnected`). Each removal fails it: of an event's recording,
/// of the generation from its key (the second attach's events kept out).
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn each_change_of_a_binding_is_its_tenants_event_only() {
    let h = Harness::with_settings(settings());
    let admin = h.admin_key().await;
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(B, WABA_B, &[PN_B], "TOKEN-OF-B").await;
    let key_a = h
        .tenant_key(A, &[Scope::Events, Scope::Numbers, Scope::Send])
        .await;
    let key_b = h.tenant_key(B, &[Scope::Events, Scope::Numbers]).await;
    let mut stream_a = opened(&h, Call::get("/v1/events/stream").key(&key_a)).await;
    let mut stream_b = opened(&h, Call::get("/v1/events/stream").key(&key_b)).await;

    attach(&h, &admin, A, WABA_A, &[PN_1, PN_2]).await;
    let mut expected = vec![
        event("number_connected", PN_1, WABA_A),
        event("number_connected", PN_2, WABA_A),
    ];
    assert_eq!(polled(&h, &key_a).await, expected, "after the attach");
    let mut number_stream = opened(
        &h,
        Call::get(format!("/v1/numbers/{PN_2}/events/stream?after=0")).key(&key_a),
    )
    .await;
    let frame = number_stream.event(WITHIN).await.json();
    assert_eq!(
        (frame["type"].clone(), frame["phone_number_id"].clone()),
        (json!("number_connected"), json!(PN_2)),
        "the number's stream: its own"
    );

    // One refusal marks the WABA's numbers: an event for each, and a
    // number already marked is refused before Meta is asked again.
    refused_token(&h, &key_a, PN_1).await;
    let marked = h
        .call(Call::get(format!("/v1/numbers/{PN_2}")).key(&key_a))
        .await;
    assert_eq!(marked.code(), "reconnect_required", "{}", marked.text);
    expected.extend([
        event("number_reconnect_required", PN_1, WABA_A),
        event("number_reconnect_required", PN_2, WABA_A),
    ]);
    assert_eq!(
        polled(&h, &key_a).await,
        expected,
        "a 190: one event per number"
    );

    // Attached again (a new token): connected again.
    attach(&h, &admin, A, WABA_A, &[PN_1, PN_2]).await;
    expected.extend([
        event("number_connected", PN_1, WABA_A),
        event("number_connected", PN_2, WABA_A),
    ]);
    assert_eq!(polled(&h, &key_a).await, expected, "attached again");

    h.graph.push_json(200, json!({"success": true}));
    let gone = h
        .call(Call::new(Method::DELETE, format!("/v1/wabas/{WABA_A}")).key(&key_a))
        .await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT, "{}", gone.text);
    expected.extend([
        disconnected(PN_1, WABA_A, "tenant_disconnected"),
        disconnected(PN_2, WABA_A, "tenant_disconnected"),
    ]);
    assert_eq!(
        polled(&h, &key_a).await,
        expected,
        "the tenant keeps its events"
    );
    assert_eq!(h.graph.remaining(), 0);

    for (i, (kind, data)) in expected.iter().enumerate() {
        let frame = stream_a.event(WITHIN).await;
        let envelope = frame.json();
        assert_eq!(
            (&envelope["type"], &envelope["data"]),
            (&json!(kind), data),
            "a's stream, event {i}"
        );
        assert_eq!(envelope["tenant_id"], A);
        assert_eq!(envelope["waba_id"], WABA_A);
        // B reads none of A's by id.
        let id = envelope["id"].as_str().unwrap();
        let other = h
            .call(Call::get(format!("/v1/events/{id}")).key(&key_b))
            .await;
        assert_eq!(other.status, StatusCode::NOT_FOUND, "{}", other.text);
        let own = h
            .call(Call::get(format!("/v1/events/{id}")).key(&key_a))
            .await;
        assert_eq!(own.json(), envelope, "a reads its own by id");
    }
    assert!(polled(&h, &key_b).await.is_empty(), "b saw a's events");
    assert!(
        stream_b.next(QUIET).await.is_none(),
        "b's stream saw a's events"
    );
    // The types filter takes them.
    let filtered = h
        .call(Call::get("/v1/events?types=number_disconnected").key(&key_a))
        .await;
    assert_eq!(filtered.status, StatusCode::OK, "{}", filtered.text);
    assert_eq!(filtered.json()["data"].as_array().unwrap().len(), 2);
}

/// The operator's unbind records `number_disconnected` with `reason`
/// `operator_unbound` for the tenant that held the WABA, and the tenant it
/// is attached to next gets its own `number_connected`, never the first
/// tenant's events. Decisive: the unbind's recording before the binding
/// goes (after it, the outbox keeps the rows operator-only).
#[tokio::test]
async fn the_operators_unbind_is_the_previous_holders_event_and_the_next_holder_starts_anew() {
    let h = Harness::with_settings(settings());
    let admin = h.admin_key().await;
    h.tenant(A).await;
    h.tenant(B).await;
    let key_a = h.tenant_key(A, &[Scope::Events]).await;
    let key_b = h.tenant_key(B, &[Scope::Events]).await;
    attach(&h, &admin, A, WABA_A, &[PN_1]).await;
    h.graph.push_json(200, json!({"success": true}));
    let unbound = h
        .call(Call::new(Method::DELETE, format!("/v1/admin/wabas/{WABA_A}/binding")).key(&admin))
        .await;
    assert_eq!(unbound.status, StatusCode::NO_CONTENT, "{}", unbound.text);
    attach(&h, &admin, B, WABA_A, &[PN_1]).await;
    assert_eq!(
        polled(&h, &key_a).await,
        [
            event("number_connected", PN_1, WABA_A),
            disconnected(PN_1, WABA_A, "operator_unbound"),
        ]
    );
    assert_eq!(
        polled(&h, &key_b).await,
        [event("number_connected", PN_1, WABA_A)],
        "the next holder: its own only"
    );
    // An unbind without a usable token records it too.
    h.tenant("merchant-c").await;
    let key_c = h.tenant_key("merchant-c", &[Scope::Events]).await;
    attach(
        &h,
        &admin,
        "merchant-c",
        "102290129340397",
        &["1972385232742147"],
    )
    .await;
    h.vault
        .delete(&meta_whatsapp_rs::core::ids::WabaId::new("102290129340397").unwrap())
        .await
        .unwrap();
    let unbound = h
        .call(Call::new(Method::DELETE, "/v1/admin/wabas/102290129340397/binding").key(&admin))
        .await;
    assert_eq!(unbound.status, StatusCode::NO_CONTENT, "{}", unbound.text);
    assert_eq!(
        polled(&h, &key_c).await,
        [
            event("number_connected", "1972385232742147", "102290129340397"),
            disconnected("1972385232742147", "102290129340397", "operator_unbound"),
        ]
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// `data` names the business's ids only: no token, no display number, no
/// Meta error text, in the event or in what the service logged while
/// recording it (`tests/logs.rs` checks the logs of every route).
#[tokio::test]
async fn a_number_events_data_names_the_businesss_ids_only() {
    let h = Harness::with_settings(settings());
    let admin = h.admin_key().await;
    h.tenant(A).await;
    let key_a = h.tenant_key(A, &[Scope::Events, Scope::Numbers]).await;
    attach(&h, &admin, A, WABA_A, &[PN_1]).await;
    refused_token(&h, &key_a, PN_1).await;
    let reply = h.call(Call::get("/v1/events").key(&key_a)).await;
    for private in [
        SYSTEM_TOKEN,
        "+1 631-555-1111",
        "Invalid OAuth",
        "AXsgnV2Cm3ZMGF3dF",
    ] {
        assert!(!reply.text.contains(private), "{private}: {}", reply.text);
    }
    assert_eq!(reply.json()["data"].as_array().unwrap().len(), 2);
}

/// What each recording that fails answers: the attach, the tenant's
/// disconnect and the operator's unbind are `503 storage_unavailable` with
/// the binding kept, and their repeat records the events (the attach's
/// again for its new generation, the disconnections' once); a `190` whose
/// event the outbox refuses still answers the call's own `409
/// reconnect_required`, the numbers marked (best effort: no event).
/// Decisive: each `?` on a recording (dropped, the call answers success
/// with no event), and the `190`'s recording kept out of its answer.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn a_number_event_the_outbox_refuses_is_503_and_its_repeat_records_it() {
    let h = Harness::with_settings(settings());
    let admin = h.admin_key().await;
    h.tenant(A).await;
    let key_a = h
        .tenant_key(A, &[Scope::Events, Scope::Numbers, Scope::Send])
        .await;
    let unavailable = |reply: &common::Reply, what: &str| {
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
            "{what}: {}",
            reply.text
        );
    };
    let waba = meta_whatsapp_rs::core::ids::WabaId::new(WABA_A).unwrap();

    // The attach: bound and stored, its event refused, `503` before the
    // app is subscribed; the repeat (a new generation) records it.
    h.graph.push_json(
        200,
        json!({"data": [{"id": PN_1, "display_phone_number": "+1 631-555-1111"}]}),
    );
    h.outbox.fail_next(1);
    let refused = h
        .call(
            Call::new(Method::POST, format!("/v1/admin/tenants/{A}/wabas"))
                .key(&admin)
                .json(&json!({"waba_id": WABA_A, "token": SYSTEM_TOKEN})),
        )
        .await;
    unavailable(&refused, "the attach");
    assert_eq!(h.graph.remaining(), 0, "not subscribed");
    assert!(polled(&h, &key_a).await.is_empty());
    attach(&h, &admin, A, WABA_A, &[PN_1]).await;
    let mut expected = vec![event("number_connected", PN_1, WABA_A)];
    assert_eq!(polled(&h, &key_a).await, expected, "the repeat's");

    // A `190`: the call's own answer, the number marked, no event.
    h.outbox.fail_next(1);
    refused_token(&h, &key_a, PN_1).await;
    assert_eq!(polled(&h, &key_a).await, expected, "best effort");

    // The tenant's disconnect: unsubscribed, its event refused, `503`, the
    // binding and the token kept; the repeat records it once.
    attach(&h, &admin, A, WABA_A, &[PN_1]).await;
    expected.push(event("number_connected", PN_1, WABA_A));
    h.graph.push_json(200, json!({"success": true}));
    h.outbox.fail_next(1);
    let disconnect = || Call::new(Method::DELETE, format!("/v1/wabas/{WABA_A}")).key(&key_a);
    unavailable(&h.call(disconnect()).await, "the disconnect");
    assert!(h.store.waba(&waba).await.unwrap().is_some(), "bound still");
    assert!(h.vault.get(&waba).await.unwrap().is_some(), "token kept");
    assert_eq!(polled(&h, &key_a).await, expected);
    h.graph.push_json(200, json!({"success": true}));
    let gone = h.call(disconnect()).await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT, "{}", gone.text);
    expected.push(disconnected(PN_1, WABA_A, "tenant_disconnected"));
    assert_eq!(polled(&h, &key_a).await, expected);

    // The operator's unbind: the same.
    attach(&h, &admin, A, WABA_A, &[PN_1]).await;
    expected.push(event("number_connected", PN_1, WABA_A));
    let unbind =
        || Call::new(Method::DELETE, format!("/v1/admin/wabas/{WABA_A}/binding")).key(&admin);
    h.graph.push_json(200, json!({"success": true}));
    h.outbox.fail_next(1);
    unavailable(&h.call(unbind()).await, "the unbind");
    assert!(h.store.waba(&waba).await.unwrap().is_some(), "bound still");
    h.graph.push_json(200, json!({"success": true}));
    let unbound = h.call(unbind()).await;
    assert_eq!(unbound.status, StatusCode::NO_CONTENT, "{}", unbound.text);
    expected.push(disconnected(PN_1, WABA_A, "operator_unbound"));
    assert_eq!(polled(&h, &key_a).await, expected);

    // And an unbind with no usable token (nothing to unsubscribe with).
    attach(&h, &admin, A, WABA_A, &[PN_1]).await;
    expected.push(event("number_connected", PN_1, WABA_A));
    h.vault.delete(&waba).await.unwrap();
    h.outbox.fail_next(1);
    unavailable(&h.call(unbind()).await, "the unbind without a token");
    assert!(h.store.waba(&waba).await.unwrap().is_some(), "bound still");
    let unbound = h.call(unbind()).await;
    assert_eq!(unbound.status, StatusCode::NO_CONTENT, "{}", unbound.text);
    expected.push(disconnected(PN_1, WABA_A, "operator_unbound"));
    assert_eq!(polled(&h, &key_a).await, expected);
    assert_eq!(h.graph.remaining(), 0);
}

/// After a move, the next holder's views of the number hold its own
/// events only: its number's stream from the start, its poll filtered by
/// the number, and the previous holder's events by id are `404`; the
/// previous holder keeps its own and gets none of the next holder's.
/// Decisive: the row's tenant (the epoch's) and the number stream's
/// tenant filter.
#[tokio::test]
async fn after_a_move_each_holder_sees_its_own_number_events_only() {
    let h = Harness::with_settings(settings());
    let admin = h.admin_key().await;
    h.tenant(A).await;
    h.tenant(B).await;
    let key_a = h.tenant_key(A, &[Scope::Events, Scope::Numbers]).await;
    let key_b = h.tenant_key(B, &[Scope::Events, Scope::Numbers]).await;
    attach(&h, &admin, A, WABA_A, &[PN_1]).await;
    h.graph.push_json(200, json!({"success": true}));
    let unbound = h
        .call(Call::new(Method::DELETE, format!("/v1/admin/wabas/{WABA_A}/binding")).key(&admin))
        .await;
    assert_eq!(unbound.status, StatusCode::NO_CONTENT, "{}", unbound.text);
    let as_ids: Vec<String> = h.call(Call::get("/v1/events").key(&key_a)).await.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(as_ids.len(), 2);
    attach(&h, &admin, B, WABA_A, &[PN_1]).await;
    let mut stream = opened(
        &h,
        Call::get(format!("/v1/numbers/{PN_1}/events/stream?after=0")).key(&key_b),
    )
    .await;
    let frame = stream.event(WITHIN).await.json();
    assert_eq!(
        (&frame["type"], &frame["tenant_id"]),
        (&json!("number_connected"), &json!(B)),
        "the next holder's own first"
    );
    assert!(stream.next(QUIET).await.is_none(), "and nothing else");
    let filtered = h
        .call(Call::get(format!("/v1/events?phone_number_id={PN_1}")).key(&key_b))
        .await;
    assert_eq!(filtered.status, StatusCode::OK, "{}", filtered.text);
    let filtered = filtered.json()["data"].as_array().unwrap().clone();
    assert_eq!(filtered.len(), 1, "{filtered:?}");
    assert_eq!(filtered[0]["id"], frame["id"]);
    for id in &as_ids {
        let other = h
            .call(Call::get(format!("/v1/events/{id}")).key(&key_b))
            .await;
        assert_eq!(other.status, StatusCode::NOT_FOUND, "{}", other.text);
    }
    assert_eq!(
        polled(&h, &key_a).await,
        [
            event("number_connected", PN_1, WABA_A),
            disconnected(PN_1, WABA_A, "operator_unbound"),
        ],
        "the previous holder: its own, none of the next's"
    );
    assert_eq!(h.graph.remaining(), 0);
}
