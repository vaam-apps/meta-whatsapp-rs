//! Roadmap M5k, slice 1: the broadcast API through the service (scope
//! `broadcasts`; docs/design/server.md, section 4.2, D420 to D427): a
//! template queued for many people from a number, paced, durable, read and
//! cancelled by id.
//!
//! The service's one clock is a `ManualClock` that is also the pacer's
//! timer, so a broadcast of hundreds runs without waiting and the instant
//! each send reached Meta is read exactly. Each test sends the requests
//! the Messages API documents and checks the exact requests that reach
//! Meta (method, path, the tenant's token, the JSON body) and
//! `remaining() == 0`. M5.1 (another tenant's number, zero vault reads) is
//! `tests/authz.rs`, which iterates every route of the document, these
//! included; M5.2 here: a broadcast is its tenant's or it does not exist.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::broadcasts::*;
use common::{Harness, Stores};
use meta_whatsapp_rs::core::clock::ManualClock;
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{NumberStatus, Scope};
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::broadcasts::BroadcastSettings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use time::OffsetDateTime;

// ─── Tests ───────────────────────────────────────────────────────────────

/// The exact request per person: the Messages API's template send, with
/// the number's token, the template's shared parameters or the person's
/// own, a person known by BSUID alone addressed by `recipient`, and the
/// broadcast's callback data. Decisive: `compose` and `NewBroadcast::new`.
#[tokio::test]
async fn each_person_is_sent_metas_template_message() {
    let w = world(rate(20)).await;
    let own = json!([{"type": "body", "parameters": [{"type": "text", "text": "ALOE"}]}]);
    let id = queued(
        &w,
        PN,
        &broadcast(vec![
            json!({"phone": phone(0)}),
            json!({"user_id": BSUID}),
            json!({"phone": phone(2), "user_id": "US.99", "components": own}),
        ]),
    )
    .await;
    let view = status(&w, &id).await;
    assert_eq!(view["state"], "queued");
    assert_eq!(view["queued"], 3);
    assert_eq!(view["recipients"], 3);
    assert_eq!(view["phone_number_id"], PN);
    script(&w.h, 3);
    work(&w.h).await;

    let callback = format!("wa-broadcast:{id}");
    let message = |to: Value, components: &Value| {
        let mut body = json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "type": "template",
            "template": {"name": "spring_sale", "language": {"code": "en_US"},
                         "components": components},
            "biz_opaque_callback_data": callback,
        });
        for (k, v) in to.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    };
    let shared = template()["components"].clone();
    let mut sent = bodies(&w.h);
    sent.sort_by_key(Value::to_string);
    let mut expected = vec![
        message(json!({"to": phone(0)}), &shared),
        message(json!({"recipient": BSUID}), &shared),
        message(json!({"to": phone(2), "recipient": "US.99"}), &own),
    ];
    expected.sort_by_key(Value::to_string);
    assert_eq!(sent, expected);
    for request in w.h.graph.requests() {
        assert_eq!(request.method, Method::POST);
        assert_eq!(request.path(), format!("/v25.0/{PN}/messages"));
        assert_eq!(request.bearer(), Some(TOKEN));
    }
    assert_eq!(w.h.graph.remaining(), 0);

    let done = status(&w, &id).await;
    assert_eq!(done["state"], "completed");
    assert_eq!(
        (
            done["sent"].clone(),
            done["queued"].clone(),
            done["failed"].clone()
        ),
        (json!(3), json!(0), json!(0))
    );
    assert_eq!(done["failures"], json!({}));
}

/// A broadcast never exceeds the configured rate: 61 people at 20 a second
/// under a fake clock, no second holds more than 20 sends, and the whole
/// takes the time 20 a second takes. Decisive: the pacer (`Pacer`
/// dropped or the rate raised fails it).
#[tokio::test]
async fn a_broadcast_never_exceeds_its_rate() {
    let w = world(rate(20)).await;
    queued(&w, PN, &broadcast(people(61))).await;
    script(&w.h, 61);
    work(&w.h).await;
    let times = w.controls.times();
    assert_eq!(times.len(), 61);
    within(&times, 20);
    // 61 sends at 20 a second span three seconds.
    assert!(times[60] - times[0] >= time::Duration::seconds(3));
    assert_eq!(w.h.graph.remaining(), 0);
}

/// Several replicas draw from one budget per number: two broadcasts on one
/// number, each claimed by another replica, send together no more than the
/// rate (and both replicas send). Decisive: the shared `KvRateLimiter`
/// (a limiter of each replica's own sends twice the rate).
#[tokio::test(start_paused = true)]
async fn two_replicas_share_one_budget() {
    let time = Virtual::new();
    let controls = Controls::new(Arc::new(time.clone()));
    let stores = Stores::memory();
    let replica = |stores: Stores| {
        let controls = controls.clone();
        let settings = Settings {
            broadcasts: BroadcastSettings {
                timer: Some(Arc::new(time.clone())),
                ..rate(20)
            },
            ..common::test_settings()
        };
        Harness::with_state_clock(stores, settings, Arc::new(time.clone()), move |graph| {
            Arc::new(Wire { graph, controls })
        })
    };
    let b = replica(shared(&stores));
    let a = replica(stores);
    a.tenant(TENANT).await;
    a.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = a.tenant_key(TENANT, &[Scope::Broadcasts]).await;
    for _ in 0..2 {
        let reply = a.call(create(&key, PN, &broadcast(people(30)))).await;
        assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    }
    script(&a, 60);
    script(&b, 60);
    let (wa, wb) = (
        a.state.broadcast_workers().unwrap(),
        b.state.broadcast_workers().unwrap(),
    );
    let (ra, rb) = tokio::join!(wa[0].tick(), wb[0].tick());
    ra.unwrap();
    rb.unwrap();
    let (from_a, from_b) = (a.graph.requests().len(), b.graph.requests().len());
    assert_eq!(from_a + from_b, 60);
    assert!(
        from_a > 0 && from_b > 0,
        "{from_a} and {from_b}: one replica did it all"
    );
    let times = controls.times();
    assert_eq!(times.len(), 60);
    within(&times, 20);
    // 60 sends at 20 a second take about three seconds, not one.
    assert!(times[59] - times[0] >= time::Duration::milliseconds(2900));
}

/// A replica that dies mid-broadcast: another takes the job over once the
/// lease ends and sends the people left, and nobody gets a second message.
/// The ones in flight at the crash are reported `unknown`, never resent.
/// Decisive: the jobs' per-person markers (`put_if_absent` before the
/// request): without them the recipients in flight are sent again.
#[tokio::test]
async fn a_restart_resumes_without_resending() {
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let controls = Controls::new(Arc::new(clock.clone()));
    let stores = Stores::memory();
    let first = replica(shared(&stores), &clock, &controls, rate(20));
    first.tenant(TENANT).await;
    first.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = first.tenant_key(TENANT, &[Scope::Broadcasts]).await;
    let reply = first.call(create(&key, PN, &broadcast(people(20)))).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    let id = reply.json()["id"].as_str().unwrap().to_owned();

    // The first replica sends three, and dies with the next ones in flight.
    controls.hold_after(3);
    script(&first, 20);
    let workers = first.state.broadcast_workers().unwrap();
    let died = tokio::time::timeout(Duration::from_millis(300), workers[0].tick()).await;
    assert!(died.is_err(), "the replica was meant to stall");
    let before = first.graph.requests().len();
    assert_eq!(before, 3);
    drop(workers);

    // Past its lease, another replica on the same data takes over.
    clock.advance(Duration::from_secs(300));
    controls.hold_after(usize::MAX);
    let second = replica(stores, &clock, &controls, rate(20));
    script(&second, 20);
    work(&second).await;

    let mut seen: Vec<String> = bodies(&first)
        .into_iter()
        .chain(bodies(&second))
        .map(|b| b["to"].as_str().unwrap().to_owned())
        .collect();
    let all = seen.len();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), all, "a person was sent twice");
    let view = second.call(read(&key, PN, &id)).await.json();
    assert_eq!(view["state"], "completed", "{view}");
    assert_eq!(view["sent"].as_u64().unwrap(), u64::try_from(all).unwrap());
    assert!(view["unknown"].as_u64().unwrap() > 0, "{view}");
    assert_eq!(
        view["sent"].as_u64().unwrap() + view["unknown"].as_u64().unwrap(),
        20,
        "{view}"
    );
    assert_eq!(view["queued"], 0);
}

/// Cancel stops the sends: at most the ones past their last check go out,
/// the rest are never sent, the state is `cancelled`, and a worker coming
/// back sends nothing more. Decisive: `Broadcasts::cancel`'s job cancel.
#[tokio::test]
async fn cancel_stops_further_sends() {
    let w = world(rate(20)).await;
    let id = queued(&w, PN, &broadcast(people(20))).await;
    script(&w.h, 20);
    w.controls.hold_after(3);
    let worker = w.h.state.broadcast_workers().unwrap();
    let control = async {
        while w.controls.seen.load(Ordering::SeqCst) < 4 {
            tokio::task::yield_now().await;
        }
        let reply = w.h.call(cancel(&w.key, PN, &id)).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        assert_eq!(reply.json()["state"], "cancelled");
        w.controls.open();
    };
    let (ticked, ()) = tokio::join!(worker[0].tick(), control);
    ticked.unwrap();
    let sent = w.h.graph.requests().len();
    assert!((3..=8).contains(&sent), "{sent} sends after a cancel");
    let view = status(&w, &id).await;
    assert_eq!(view["state"], "cancelled");
    assert_eq!(
        view["queued"].as_u64().unwrap(),
        u64::try_from(20 - sent).unwrap()
    );
    // Nothing more, later.
    w.clock.advance(Duration::from_secs(600));
    work(&w.h).await;
    assert_eq!(w.h.graph.requests().len(), sent);
    // A broadcast cancelled before any worker came sends nothing at all.
    let other = queued(&w, PN, &broadcast(people(5))).await;
    let reply = w.h.call(cancel(&w.key, PN, &other)).await;
    assert_eq!(reply.json()["state"], "cancelled");
    work(&w.h).await;
    assert_eq!(w.h.graph.requests().len(), sent);
}

/// Meta's refusals are counted by the error kind, never by text: an opted
/// out person is `marketing_opted_out`, the others are sent, and the
/// broadcast completes. Decisive: the failure counters.
#[tokio::test]
async fn failures_are_counted_by_metas_error_kind() {
    let w = world(rate(20)).await;
    let id = queued(&w, PN, &broadcast(people(3))).await;
    w.h.graph.push_json(200, accepted(0));
    w.h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#131050) {} stopped marketing messages", phone(1)),
                         "type": "OAuthException", "code": 131_050,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    w.h.graph.push_json(200, accepted(2));
    work(&w.h).await;
    let view = status(&w, &id).await;
    assert_eq!(view["state"], "completed", "{view}");
    assert_eq!(view["sent"], 2);
    assert_eq!(view["failed"], 1);
    assert_eq!(view["failures"], json!({"marketing_opted_out": 1}));
    assert!(
        !view.to_string().contains("stopped marketing"),
        "Meta's text"
    );
    assert!(!view.to_string().contains(&phone(1)));
}

/// A send that timed out is never sent again, here or by the client: it is
/// `unknown`. Decisive: `RetryPolicy::NONE` on the sender's client and the
/// runner's `may_resend` rule.
#[tokio::test]
async fn a_timed_out_send_is_never_repeated() {
    let w = world(rate(20)).await;
    let id = queued(&w, PN, &broadcast(people(2))).await;
    w.h.graph.push_error(|| TransportError::Timeout);
    w.h.graph.push_json(200, accepted(1));
    work(&w.h).await;
    let view = status(&w, &id).await;
    assert_eq!(
        (view["sent"].clone(), view["unknown"].clone()),
        (json!(1), json!(1))
    );
    assert_eq!(w.h.graph.requests().len(), 2);
    assert_eq!(w.h.graph.remaining(), 0);
}

/// A broadcast is its tenant's or it does not exist (the design's M5.2):
/// another tenant's id, the same tenant's id on another of its numbers,
/// an id never made and a malformed one are the same `404`, and another
/// tenant's cancel changes nothing. Decisive: the record's tenant and
/// number checks in `Broadcasts::owned` (each dropped fails a row).
#[tokio::test]
async fn another_tenants_broadcast_is_not_found() {
    let w = world(rate(20)).await;
    let id = queued(&w, PN, &broadcast(people(2))).await;
    let rival = w.h.tenant_key(OTHER_TENANT, &[Scope::Broadcasts]).await;
    let missing = "bc_00000000000000000000000000000000";

    let unknown = w.h.call(read(&rival, OTHER_PN, missing)).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    for call in [
        read(&rival, OTHER_PN, &id),
        read(&rival, PN, &id),
        read(&w.key, PN2, &id),
        read(&w.key, PN, missing),
        read(&w.key, PN, "nonsense"),
    ] {
        let reply = w.h.call(call).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
        assert_eq!(reply.code(), "not_found");
        assert_eq!(
            without_request_id(&reply),
            without_request_id(&unknown),
            "an answer that tells them apart"
        );
    }
    for call in [
        cancel(&rival, OTHER_PN, &id),
        cancel(&rival, PN, &id),
        cancel(&w.key, PN2, &id),
    ] {
        let reply = w.h.call(call).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    }
    // Untouched: still queued, and it still sends.
    assert_eq!(status(&w, &id).await["state"], "queued");
    script(&w.h, 2);
    work(&w.h).await;
    assert_eq!(status(&w, &id).await["state"], "completed");

    // Another tenant cannot create on this tenant's number either.
    let reply = w.h.call(create(&rival, PN, &broadcast(people(1)))).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    // And a key without the scope may do none of it.
    let sender = w.h.tenant_key(TENANT, &[Scope::Send]).await;
    for call in [
        read(&sender, PN, &id),
        cancel(&sender, PN, &id),
        create(&sender, PN, &broadcast(people(1))),
    ] {
        assert_eq!(w.h.call(call).await.status, StatusCode::FORBIDDEN);
    }
}

/// A queued send finds its token by the broadcast's own record, steps 4
/// and 5: a number whose connection was lost sends nothing, the first
/// person it reaches is `reconnect_required`, and the rest wait. Decisive:
/// `Authorizer::broadcast_sender` (a worker with a token of its own sends).
#[tokio::test]
async fn a_number_that_lost_its_connection_sends_nothing() {
    let w = world(rate(20)).await;
    let id = queued(&w, PN, &broadcast(people(4))).await;
    w.h.store
        .set_waba_status(
            &meta_whatsapp_rs::core::ids::WabaId::new(WABA).unwrap(),
            NumberStatus::Disconnected,
        )
        .await
        .unwrap();
    work(&w.h).await;
    assert!(w.h.graph.requests().is_empty(), "a send without a token");
    let view = status(&w, &id).await;
    assert_eq!(view["sent"], 0);
    assert_eq!(view["failures"], json!({"reconnect_required": 1}));
    assert_ne!(view["state"], "completed");
}

/// The limits: recipients (none, too many, a number that is not E.164, a
/// blank BSUID, a person with neither), the template (a message the
/// library refuses, a catalog), and the tenant's broadcasts at once. A
/// refusal queues nothing and sends nothing. Decisive: each bound.
#[tokio::test]
async fn the_limits_are_refused_before_anything_is_queued() {
    let w = world(BroadcastSettings {
        max_recipients: 3,
        max_concurrent: 2,
        ..rate(20)
    })
    .await;
    let field = |reply: &common::Reply| reply.json()["error"]["field"].clone();
    for (body, at) in [
        (broadcast(vec![]), "recipients"),
        (broadcast(people(4)), "recipients"),
        (
            broadcast(vec![
                json!({"phone": phone(0)}),
                json!({"phone": "16505551234"}),
            ]),
            "recipients[1]",
        ),
        (broadcast(vec![json!({"user_id": " "})]), "recipients[0]"),
        (broadcast(vec![json!({})]), "recipients[0]"),
        (
            broadcast(vec![
                json!({"phone": phone(0)}),
                json!({"phone": phone(1), "components": "x"}),
            ]),
            "recipients[1]",
        ),
        (
            json!({"template": {"language": {"code": "en_US"}}, "recipients": people(1)}),
            "template",
        ),
        (
            json!({"template": {"name": "t", "language": {"code": "en_US"}, "components": [{"type": "header",
                   "parameters": [{"type": "product", "product": {"product_retailer_id": "p", "catalog_id": "194836987003835"}}]}]},
                   "recipients": people(1)}),
            "template",
        ),
    ] {
        let reply = w.h.call(create(&w.key, PN, &body)).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {}",
            reply.text
        );
        assert_eq!(field(&reply), json!(at), "{}", reply.text);
    }
    for unknown in [
        json!({"template": template(), "recipients": people(1), "to": "x"}),
        json!({"template": template(), "recipients": [{"phone": phone(0), "group_id": "1"}]}),
    ] {
        let reply = w.h.call(create(&w.key, PN, &unknown)).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            reply.text
        );
    }
    assert!(w.h.graph.requests().is_empty());

    // Two at once, then the third waits; a cancel frees a place.
    let first = queued(&w, PN, &broadcast(people(1))).await;
    queued(&w, PN2, &broadcast(people(1))).await;
    let third = w.h.call(create(&w.key, PN, &broadcast(people(1)))).await;
    assert_eq!(
        third.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        third.text
    );
    assert_eq!(third.code(), "too_many_requests");
    assert!(third.headers.contains_key("retry-after"));
    // Another tenant is not counted.
    let rival = w.h.tenant_key(OTHER_TENANT, &[Scope::Broadcasts]).await;
    let theirs =
        w.h.call(create(&rival, OTHER_PN, &broadcast(people(1))))
            .await;
    assert_eq!(theirs.status, StatusCode::ACCEPTED, "{}", theirs.text);
    w.h.call(cancel(&w.key, PN, &first)).await;
    let again = w.h.call(create(&w.key, PN, &broadcast(people(1)))).await;
    assert_eq!(again.status, StatusCode::ACCEPTED, "{}", again.text);
}

/// The `Idempotency-Key` covers the whole broadcast: the same key and body
/// queue it once (the second answer is the first, replayed), another body
/// under the key is `idempotency_key_reused`, and only one set of people
/// is sent. Decisive: the route's fingerprint and `Grant::idempotent`.
#[tokio::test]
async fn one_key_is_one_broadcast() {
    let w = world(rate(20)).await;
    let body = broadcast(people(2));
    let keyed = |body: &Value| create(&w.key, PN, body).header("idempotency-key", "spring-1");
    let first = w.h.call(keyed(&body)).await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.text);
    let second = w.h.call(keyed(&body)).await;
    assert_eq!(second.status, StatusCode::ACCEPTED);
    assert_eq!(second.text, first.text);
    assert_eq!(second.headers["idempotent-replayed"], "true");
    let other = w.h.call(keyed(&broadcast(people(3)))).await;
    assert_eq!(
        other.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        other.text
    );
    assert_eq!(other.code(), "idempotency_key_reused");
    script(&w.h, 4);
    work(&w.h).await;
    assert_eq!(w.h.graph.requests().len(), 2, "queued twice");
}

/// A broadcast is a template send like any other: the binding's WABA id is
/// named as the `messaging_account_id` when the setting is on (the one
/// function every send uses). Decisive: `billed_to_binding` in the sender.
#[tokio::test]
async fn the_binding_is_named_when_the_setting_is_on() {
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let controls = Controls::new(Arc::new(clock.clone()));
    let mut on = settings(&clock, rate(20));
    on.send_messaging_account_id = true;
    let c = controls.clone();
    let h = Harness::with_clock(Stores::memory(), on, clock.clone(), move |graph| {
        Arc::new(Wire { graph, controls: c })
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Broadcasts]).await;
    let reply = h.call(create(&key, PN, &broadcast(people(1)))).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    script(&h, 1);
    work(&h).await;
    assert_eq!(bodies(&h)[0]["messaging_account_id"], WABA);
}

/// A number handed to another tenant (the first unbound its WABA, the
/// second attached it) brings none of the first's broadcasts with it: the
/// new holder reads and cancels them as `404` (the record's own tenant, not
/// the number's, decides: M5.2), and the first's queued sends go nowhere
/// (a worker finds a token only for the record's tenant). Decisive: the
/// tenant clause of `Broadcasts::owned` and `Authorizer::broadcast_sender`.
#[tokio::test]
async fn a_number_rebound_to_another_tenant_keeps_nothing_of_the_first() {
    let w = world(rate(20)).await;
    let id = queued(&w, PN, &broadcast(people(3))).await;
    let waba = meta_whatsapp_rs::core::ids::WabaId::new(WABA).unwrap();
    assert!(w.h.store.unbind_waba(&waba).await.unwrap());
    w.h.connect(OTHER_TENANT, WABA, &[PN], "EAAG-the-new-holder")
        .await;
    let rival = w.h.tenant_key(OTHER_TENANT, &[Scope::Broadcasts]).await;
    let seen = w.h.call(read(&rival, PN, &id)).await;
    assert_eq!(seen.status, StatusCode::NOT_FOUND, "{}", seen.text);
    let cancelled = w.h.call(cancel(&rival, PN, &id)).await;
    assert_eq!(
        cancelled.status,
        StatusCode::NOT_FOUND,
        "{}",
        cancelled.text
    );
    // The first tenant's worker sends nothing with the new holder's token.
    script(&w.h, 3);
    work(&w.h).await;
    assert!(w.h.graph.requests().is_empty(), "sent as the new holder");
    assert_eq!(w.h.graph.remaining(), 3);
}

/// Meta's throttles (`131056`, the pair rate limit; `130429`, throughput)
/// are waited out, never hammered and never a person marked failed: the
/// pair limit is resent after 1 s, then 4 s (Meta's `4^X` schedule), and
/// five throttles in a row leave the person queued for the job's retry,
/// not failed, then sent. Decisive: `TenantOutbound::send` leaving a
/// `may_resend` error uncounted and the runner unmarking it.
#[tokio::test]
async fn a_throttled_person_waits_and_is_never_marked_failed() {
    let throttle = |code: u32, text: &str| {
        json!({"error": {"message": format!("({code}) {text}"), "type": "OAuthException",
                         "code": code, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}})
    };
    let w = world(rate(20)).await;
    let id = queued(&w, PN, &broadcast(people(1))).await;
    w.h.graph
        .push_json(400, throttle(131_056, "pair rate limit"));
    w.h.graph
        .push_json(400, throttle(131_056, "pair rate limit"));
    w.h.graph.push_json(200, accepted(0));
    work(&w.h).await;
    let view = status(&w, &id).await;
    assert_eq!(view["state"], "completed", "{view}");
    assert_eq!((view["sent"].clone(), view["failed"].clone()), (json!(1), json!(0)));
    assert_eq!(view["failures"], json!({}));
    let times = w.controls.times();
    assert_eq!(times.len(), 3);
    assert!(times[1] - times[0] >= time::Duration::SECOND, "{times:?}");
    assert!(times[2] - times[1] >= time::Duration::seconds(4), "{times:?}");

    // Throughput (130429): five refusals in a row; the person is given
    // back to the job, not failed, and sent at its retry.
    let id = queued(&w, PN, &broadcast(people(1))).await;
    for _ in 0..5 {
        w.h.graph
            .push_json(400, throttle(130_429, "throughput limit"));
    }
    w.h.graph.push_json(200, accepted(0));
    let before = w.h.graph.requests().len();
    work(&w.h).await;
    assert_eq!(w.h.graph.requests().len() - before, 5, "five tries, no more");
    let view = status(&w, &id).await;
    assert_eq!(view["failed"], 0, "{view}");
    assert_eq!(view["failures"], json!({}), "{view}");
    assert_eq!(view["queued"], 1, "{view}");
    assert_ne!(view["state"], "completed");
    assert_eq!(w.h.graph.requests().len() - before, 5, "no hammering");
    w.clock.advance(Duration::from_secs(120));
    work(&w.h).await;
    let view = status(&w, &id).await;
    assert_eq!(view["state"], "completed", "{view}");
    assert_eq!(view["sent"], 1);
    assert_eq!(w.h.graph.remaining(), 0);
}

/// The tenant's limit holds under concurrent creates: with room for two,
/// six creates at once queue exactly two. Decisive: `reserve`'s
/// compare-and-swap (a plain read-then-write admits more).
#[tokio::test]
async fn concurrent_creates_never_pass_the_limit() {
    let w = world(BroadcastSettings {
        max_concurrent: 2,
        ..rate(20)
    })
    .await;
    let body = broadcast(people(1));
    let replies = futures::future::join_all(
        (0..6).map(|_| w.h.call(create(&w.key, PN, &body))),
    )
    .await;
    let accepted = replies
        .iter()
        .filter(|r| r.status == StatusCode::ACCEPTED)
        .count();
    let busy = replies
        .iter()
        .filter(|r| r.status == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!((accepted, busy), (2, 4), "{:?}", replies.iter().map(|r| &r.text).collect::<Vec<_>>());
}

/// An `Idempotency-Key` is for one number's broadcast: the same key and
/// body on another of the tenant's numbers is a reuse (`422`), never the
/// first number's broadcast replayed, and queues nothing. Decisive: the
/// number in `Fingerprint::create_broadcast`.
#[tokio::test]
async fn a_key_is_not_replayed_on_another_number() {
    let w = world(rate(20)).await;
    let body = broadcast(people(1));
    let on = |pn: &str| create(&w.key, pn, &body).header("idempotency-key", "spring-2");
    let first = w.h.call(on(PN)).await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.text);
    let other = w.h.call(on(PN2)).await;
    assert_eq!(other.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", other.text);
    assert_eq!(other.code(), "idempotency_key_reused");
    script(&w.h, 2);
    work(&w.h).await;
    assert_eq!(w.h.graph.requests().len(), 1);
}
