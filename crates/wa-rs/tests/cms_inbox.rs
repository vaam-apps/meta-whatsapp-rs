//! The `cms_inbox` example's app, driven in-process with
//! `tower::ServiceExt::oneshot`: Meta's signed webhook in, the merchant's
//! inbox out, the reply sent with the merchant's token.
//!
//! The example file itself is compiled into this test (`#[path]` below), not
//! a copy of it: what passes here is what `cargo run --example cms_inbox`
//! serves. Graph calls go to a `ScriptedTransport`; stores are in memory.

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(all(
    feature = "reqwest",
    feature = "memory",
    feature = "sinks",
    feature = "axum"
))]

// `main` and its environment helpers are the example's; only `app` is used.
#[allow(dead_code)]
#[path = "../examples/cms_inbox.rs"]
mod cms_inbox;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::{BodyExt, Limited};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tower::ServiceExt;
use wa_rs::adapters::store::{MemoryConversationStore, MemoryKvStore};
use wa_rs::client::embedded_signup::{StoredBusinessToken, TokenVault, VaultKey, VaultKeys};
use wa_rs::core::testing::ScriptedTransport;
use wa_rs::prelude::*;
use wa_rs::webhooks::server::SIGNATURE_HEADER;
use wa_rs::webhooks::{WebhookPayload, dedup, sign};

/// Ids from `business-scoped-user-ids` (text message, BSUID, no `wa_id`).
const WABA: &str = "102290129340398";
const PNID: &str = "106540352242922";
const BSUID: &str = "US.13491208655302741918";
const INBOUND_ID: &str = "wamid.HBgLMTY1MDM4Nzk0MzkVAgASGBQzQTRBNjU5OUFFRTAzODEwMTQ0RgA=";
const INBOUND_TEXT: &str = "Does it come in another color?";
/// Another merchant's number on the same app.
const OTHER_PNID: &str = "106540352242923";
/// Same page, "Send message response", addressed by BSUID.
const SENT_ID: &str = "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA";

const APP_SECRET: &str = "5e1f0c2d3b4a59687f6e5d4c3b2a1908";
const VERIFY_TOKEN: &str = "vibecoding";
const MERCHANT_TOKEN: &str = "EAAMERCHANT-BUSINESS-TOKEN";

/// Every read of a response body is capped in size and time: a route that
/// streams by mistake fails the test instead of hanging it.
const BODY_LIMIT: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);

struct Harness {
    app: Router,
    graph: ScriptedTransport,
    /// Backs webhook dedup and the token vault.
    kv: Arc<dyn KvStore>,
}

/// The example's app on a scripted Graph API, with one merchant connected:
/// `PNID` belongs to `WABA`, whose business token is `MERCHANT_TOKEN`.
async fn harness() -> Harness {
    let graph = ScriptedTransport::new();
    // No default token, as in the example's `main`.
    let client = Client::builder()
        .transport(graph.clone())
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap();
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
    let vault = TokenVault::new(
        kv.clone(),
        VaultKeys::new(VaultKey::generate("test").unwrap()),
    )
    .unwrap();
    vault
        .store(
            &StoredBusinessToken::new(WABA, AccessToken::new(MERCHANT_TOKEN))
                .phone_number_ids([PNID]),
        )
        .await
        .unwrap();
    let app = cms_inbox::app(
        client,
        AppSecret::new(APP_SECRET),
        VerifyToken::new(VERIFY_TOKEN),
        kv.clone(),
        Arc::new(MemoryConversationStore::new()),
        vault,
    )
    .unwrap();
    Harness { app, graph, kv }
}

/// The dedup guard's marker for the (single) event in `body`: `done` once it
/// was delivered.
async fn dedup_marker(kv: &Arc<dyn KvStore>, body: &[u8]) -> Option<String> {
    let events = WebhookPayload::from_slice(body).unwrap().into_events();
    let [event] = &events[..] else {
        panic!("{} events", events.len())
    };
    let key = dedup::store_key(&event.dedup_key().unwrap());
    kv.get(&key)
        .await
        .unwrap()
        .map(|v| String::from_utf8(v.value).unwrap())
}

/// A `messages` webhook with one inbound text, in the shape of the
/// `business-scoped-user-ids` example (BSUID and username, no `wa_id`).
fn inbound_text(phone_number_id: &str, message_id: &str, timestamp: i64) -> Vec<u8> {
    serde_json::to_vec(&json!({
      "object": "whatsapp_business_account",
      "entry": [{
        "id": WABA,
        "changes": [{
          "value": {
            "messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": phone_number_id},
            "contacts": [{
              "profile": {"name": "Sheena Nelson", "username": "realsheenanelson"},
              "user_id": BSUID,
              "parent_user_id": "US.ENT.11815799212886844830"
            }],
            "messages": [{
              "from_user_id": BSUID,
              "from_parent_user_id": "US.ENT.11815799212886844830",
              "id": message_id,
              "timestamp": timestamp.to_string(),
              "type": "text",
              "text": {"body": INBOUND_TEXT}
            }]
          },
          "field": "messages"
        }]
      }]
    }))
    .unwrap()
}

fn signed_webhook(body: Vec<u8>) -> Request<Body> {
    let signature = sign(&AppSecret::new(APP_SECRET), &body);
    webhook(body, Some(&signature))
}

fn webhook(body: Vec<u8>, signature: Option<&str>) -> Request<Body> {
    let mut request = Request::post("/webhook").header(header::CONTENT_TYPE, "application/json");
    if let Some(signature) = signature {
        request = request.header(SIGNATURE_HEADER, signature);
    }
    request.body(Body::from(body)).unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn reply(phone_number_id: &str, contact: &str, text: &str) -> Request<Body> {
    Request::post(format!("/inbox/{phone_number_id}/reply"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"contact": contact, "text": text}).to_string(),
        ))
        .unwrap()
}

/// Send `request`, read the (bounded) body as text.
async fn call(app: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = tokio::time::timeout(
        READ_TIMEOUT,
        Limited::new(response.into_body(), BODY_LIMIT).collect(),
    )
    .await
    .expect("the response body ends")
    .expect("the response body fits the limit")
    .to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

/// [`call`], expecting JSON.
async fn call_json(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let (status, body) = call(app, request).await;
    let json = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{e}: {body}"));
    (status, json)
}

fn rfc3339(unix: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix)
        .unwrap()
        .format(&Rfc3339)
        .unwrap()
}

/// Read Server-Sent Events until the first complete `event: whatsapp` one;
/// return everything read. Bounded in size and time.
async fn first_whatsapp_event(body: &mut Body) -> String {
    let read = async {
        let mut text = String::new();
        loop {
            let frame = body
                .frame()
                .await
                .expect("the SSE stream stays open")
                .unwrap();
            if let Ok(data) = frame.into_data() {
                text.push_str(std::str::from_utf8(&data).unwrap());
            }
            assert!(text.len() < BODY_LIMIT, "no event in {} bytes", text.len());
            if let Some(start) = text.find("event: whatsapp\n")
                && text[start..].contains("\n\n")
            {
                return text;
            }
        }
    };
    tokio::time::timeout(READ_TIMEOUT, read)
        .await
        .expect("an SSE event within the timeout")
}

/// Exactly one Graph request was made, and it was the reply: the merchant's
/// number, the merchant's token, addressed by BSUID.
fn assert_sent_as_the_merchant(graph: &ScriptedTransport, text: &str) {
    let requests = graph.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.url.host_str(), Some("graph.facebook.com"));
    assert_eq!(request.path(), format!("/v25.0/{PNID}/messages"));
    assert_eq!(request.url.query(), None);
    assert_eq!(request.bearer(), Some(MERCHANT_TOKEN));
    assert_eq!(
        request.json().unwrap(),
        json!({
          "messaging_product": "whatsapp",
          "recipient_type": "individual",
          "recipient": BSUID,
          "type": "text",
          "text": {"body": text}
        })
    );
    assert_eq!(graph.remaining(), 0);
}

#[tokio::test]
async fn inbound_webhook_to_inbox_to_reply_with_the_merchants_token() {
    let Harness { app, graph, .. } = harness().await;
    // Two minutes ago: inside the 24-hour window.
    let received_at = OffsetDateTime::now_utc().unix_timestamp() - 120;

    // The merchant's inbox is open and listening.
    let events = app
        .clone()
        .oneshot(get(&format!("/inbox/{PNID}/events")))
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    assert_eq!(events.headers()[header::CONTENT_TYPE], "text/event-stream");
    let mut events = events.into_body();

    // Meta delivers a message for another merchant, then one for ours.
    let other = inbound_text(OTHER_PNID, "wamid.OTHER", received_at);
    assert_eq!(call(&app, signed_webhook(other)).await.0, StatusCode::OK);
    let body = inbound_text(PNID, INBOUND_ID, received_at);
    assert_eq!(call(&app, signed_webhook(body)).await.0, StatusCode::OK);

    // Live: only our number's event reaches our inbox.
    let sse = first_whatsapp_event(&mut events).await;
    assert!(
        !sse.contains(OTHER_PNID),
        "another merchant's event leaked: {sse}"
    );
    assert!(sse.contains(r#""event":"message_received""#), "{sse}");
    assert!(sse.contains(INBOUND_ID), "{sse}");
    drop(events);

    // Stored: the conversation is listed, keyed by the BSUID.
    let (status, conversations) =
        call_json(&app, get(&format!("/inbox/{PNID}/conversations"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        conversations,
        json!([{
            "key": {"phone_number_id": PNID, "contact": BSUID},
            "last_message_at": rfc3339(received_at),
            "last_inbound_at": rfc3339(received_at),
            "last_text": INBOUND_TEXT,
            "unread": 1
        }])
    );

    // The merchant replies inside the window.
    graph.push_json(
        200,
        json!({
          "messaging_product": "whatsapp",
          "contacts": [{"input": BSUID, "user_id": BSUID}],
          "messages": [{"id": SENT_ID}]
        }),
    );
    let (status, sent) = call_json(&app, reply(PNID, BSUID, "Yes: navy and olive.")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sent, json!({"message_id": SENT_ID}));

    assert_sent_as_the_merchant(&graph, "Yes: navy and olive.");

    // Recorded: newest first, the reply `accepted` until status webhooks
    // move it on; the window closes 24 hours after the customer wrote.
    let (status, conversation) =
        call_json(&app, get(&format!("/inbox/{PNID}/conversations/{BSUID}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        conversation["window_closes_at"],
        json!(received_at + 24 * 60 * 60)
    );
    let messages = conversation["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "{conversation}");
    let summary: Vec<_> = messages
        .iter()
        .map(|m| {
            json!([
                m["id"],
                m["direction"],
                m["kind"],
                m["text"],
                m["status"],
                m["conversation"]["contact"]
            ])
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            json!([
                SENT_ID,
                "outbound",
                "text",
                "Yes: navy and olive.",
                "accepted",
                BSUID
            ]),
            json!([
                INBOUND_ID,
                "inbound",
                "text",
                INBOUND_TEXT,
                "received",
                BSUID
            ]),
        ]
    );

    // Opening the conversation marked it read.
    let (_, conversations) = call_json(&app, get(&format!("/inbox/{PNID}/conversations"))).await;
    assert_eq!(conversations[0]["unread"], 0);
    assert_eq!(conversations[0]["last_text"], "Yes: navy and olive.");
}

#[tokio::test]
async fn webhook_route_checks_the_verify_token_signature_and_retries() {
    let Harness { app, graph, kv } = harness().await;

    // The dashboard's subscription check, on the nested route.
    let (status, challenge) = call(
        &app,
        get(&format!(
            "/webhook?hub.mode=subscribe&hub.challenge=1158201444&hub.verify_token={VERIFY_TOKEN}"
        )),
    )
    .await;
    assert_eq!((status, challenge.as_str()), (StatusCode::OK, "1158201444"));
    let (status, _) = call(
        &app,
        get("/webhook?hub.mode=subscribe&hub.challenge=1&hub.verify_token=guess"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Unsigned, or signed with another secret: rejected, nothing recorded.
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let body = inbound_text(PNID, INBOUND_ID, now);
    let forged = sign(&AppSecret::new("not-the-app-secret"), &body);
    assert_eq!(
        call(&app, webhook(body.clone(), None)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, webhook(body.clone(), Some(&forged))).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (_, conversations) = call_json(&app, get(&format!("/inbox/{PNID}/conversations"))).await;
    assert_eq!(conversations, json!([]));
    assert_eq!(dedup_marker(&kv, &body).await, None);

    // Meta's retry of a delivered body is acknowledged and not recorded
    // twice; the dedup guard in front of the sinks marked it done.
    for _ in 0..2 {
        assert_eq!(
            call(&app, signed_webhook(body.clone())).await.0,
            StatusCode::OK
        );
    }
    assert_eq!(dedup_marker(&kv, &body).await.as_deref(), Some("done"));
    let (_, conversation) =
        call_json(&app, get(&format!("/inbox/{PNID}/conversations/{BSUID}"))).await;
    assert_eq!(conversation["messages"].as_array().unwrap().len(), 1);
    assert!(graph.requests().is_empty());
}

#[tokio::test]
async fn replies_are_refused_before_meta_when_they_cannot_go() {
    let Harness { app, graph, .. } = harness().await;
    let day_and_an_hour_ago = OffsetDateTime::now_utc().unix_timestamp() - 25 * 60 * 60;
    let body = inbound_text(PNID, INBOUND_ID, day_and_an_hour_ago);
    assert_eq!(call(&app, signed_webhook(body)).await.0, StatusCode::OK);

    // Outside the 24-hour window.
    let (status, error) = call_json(&app, reply(PNID, BSUID, "Still there?")).await;
    assert_eq!(
        (status, error),
        (
            StatusCode::CONFLICT,
            json!({"error": "customer_service_window_closed"})
        )
    );
    // A number no merchant connected.
    let (status, error) = call_json(&app, reply(OTHER_PNID, BSUID, "hi")).await;
    assert_eq!(
        (status, error),
        (StatusCode::NOT_FOUND, json!({"error": "not_connected"}))
    );
    let (status, _) = call(&app, get(&format!("/inbox/{OTHER_PNID}/events"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(graph.requests().is_empty(), "nothing reached Meta");
}

#[tokio::test]
async fn invalid_replies_and_metas_window_refusal_map_to_stable_codes() {
    let Harness { app, graph, .. } = harness().await;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let body = inbound_text(PNID, INBOUND_ID, now);
    assert_eq!(call(&app, signed_webhook(body)).await.0, StatusCode::OK);

    // An empty text fails local validation: no request.
    let (status, error) = call_json(&app, reply(PNID, BSUID, "")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "invalid");
    assert_eq!(error["field"], "text.body");
    assert!(graph.requests().is_empty());

    // Meta can still refuse on the window (clock skew, another app replied):
    // 131047 maps to the same answer as the local check.
    graph.push_json(
        400,
        json!({"error": {
            "message": "(#131047) Re-engagement message",
            "type": "OAuthException",
            "code": 131047,
            "error_data": {"messaging_product": "whatsapp", "details": "Message failed to send because more than 24 hours have passed since the customer last replied to this number."},
            "fbtrace_id": "A1b2"
        }}),
    );
    let (status, error) = call_json(&app, reply(PNID, BSUID, "hello")).await;
    assert_eq!(
        (status, error),
        (
            StatusCode::CONFLICT,
            json!({"error": "customer_service_window_closed"})
        )
    );
    assert_eq!(graph.requests().len(), 1);
    assert_eq!(graph.remaining(), 0);
    // A refused reply is not recorded.
    let (_, conversation) =
        call_json(&app, get(&format!("/inbox/{PNID}/conversations/{BSUID}"))).await;
    assert_eq!(conversation["messages"].as_array().unwrap().len(), 1);
}
