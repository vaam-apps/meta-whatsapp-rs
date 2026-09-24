//! The `cms_inbox` example's app, driven in-process with
//! `tower::ServiceExt::oneshot`: Meta's signed webhook in, the merchant's
//! inbox out, the reply sent with the merchant's token — and only for the
//! tenant who owns the number.
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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use http_body_util::{BodyExt, Limited};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tower::ServiceExt;
use wa_rs::adapters::store::{MemoryConversationStore, MemoryKvStore};
use wa_rs::client::embedded_signup::{StoredBusinessToken, TokenVault, VaultKey, VaultKeys};
use wa_rs::core::error::StorageError;
use wa_rs::core::store::{Expiry, StoreKey, Versioned};
use wa_rs::core::testing::ScriptedTransport;
use wa_rs::prelude::*;
use wa_rs::webhooks::axum::Router;
use wa_rs::webhooks::axum::body::Body;
use wa_rs::webhooks::axum::http::{Method, Request, StatusCode, header};
use wa_rs::webhooks::server::SIGNATURE_HEADER;
use wa_rs::webhooks::{WebhookPayload, dedup, sign};

/// Ids from `business-scoped-user-ids` (text message, BSUID, no `wa_id`).
const WABA: &str = "102290129340398";
const PNID: &str = "106540352242922";
const BSUID: &str = "US.13491208655302741918";
const INBOUND_ID: &str = "wamid.HBgLMTY1MDM4Nzk0MzkVAgASGBQzQTRBNjU5OUFFRTAzODEwMTQ0RgA=";
const INBOUND_TEXT: &str = "Does it come in another color?";
/// Another merchant's number on the same app, connected with that
/// merchant's own token.
const OTHER_PNID: &str = "106540352242923";
const OTHER_WABA: &str = "102290129340399";
const OTHER_TOKEN: &str = "EAAOTHER-MERCHANT-BUSINESS-TOKEN";
/// Listed for merchant A, but nobody connected it.
const UNCONNECTED_PNID: &str = "106540352242924";
/// Same page, "Send message response", addressed by BSUID.
const SENT_ID: &str = "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA";

const APP_SECRET: &str = "5e1f0c2d3b4a59687f6e5d4c3b2a1908";
const VERIFY_TOKEN: &str = "vibecoding";
const MERCHANT_TOKEN: &str = "EAAMERCHANT-BUSINESS-TOKEN";

/// The CMS's own logins (the example's stand-in): merchant A owns `PNID`
/// (and `UNCONNECTED_PNID`), merchant B owns `OTHER_PNID`.
const TENANT_A_BEARER: &str = "tenant-a-6b1f0e0d9c8b7a69584736251403f2e1";
const TENANT_B_BEARER: &str = "tenant-b-0f1e2d3c4b5a69788796a5b4c3d2e1f0";

/// Every read of a response body is capped in size and time: a route that
/// streams by mistake fails the test instead of hanging it.
const BODY_LIMIT: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);

struct Harness {
    app: Router,
    graph: ScriptedTransport,
    /// Backs webhook dedup and the token vault.
    kv: Arc<dyn KvStore>,
    /// How many times the vault read its store.
    vault_reads: Arc<AtomicUsize>,
}

/// A `KvStore` that counts `get`s: what the vault reads when it looks up a
/// merchant's token.
#[derive(Debug)]
struct CountingKv {
    inner: Arc<dyn KvStore>,
    gets: Arc<AtomicUsize>,
}

#[async_trait]
impl KvStore for CountingKv {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.inner.get(key).await
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.inner.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.inner.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.inner
            .compare_and_swap(key, expected, new, expiry)
            .await
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.inner.delete(key).await
    }
}

/// The example's tenants: A owns `PNID` and `UNCONNECTED_PNID`, B owns
/// `OTHER_PNID`.
fn tenants() -> cms_inbox::Tenants {
    cms_inbox::Tenants::default()
        .tenant("merchant-a", TENANT_A_BEARER, [PNID, UNCONNECTED_PNID])
        .unwrap()
        .tenant("merchant-b", TENANT_B_BEARER, [OTHER_PNID])
        .unwrap()
}

/// The example's app on a scripted Graph API, with two merchants connected:
/// `PNID` belongs to `WABA`, whose business token is `MERCHANT_TOKEN`, and
/// `OTHER_PNID` to `OTHER_WABA` (`OTHER_TOKEN`).
async fn harness() -> Harness {
    let graph = ScriptedTransport::new();
    // No default token, as in the example's `main`.
    let client = Client::builder()
        .transport(graph.clone())
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap();
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
    let vault_reads = Arc::new(AtomicUsize::new(0));
    let vault_kv = CountingKv {
        inner: kv.clone(),
        gets: vault_reads.clone(),
    };
    let vault = TokenVault::new(
        Arc::new(vault_kv),
        VaultKeys::new(VaultKey::generate("test").unwrap()),
    )
    .unwrap();
    for (waba, token, number) in [
        (WABA, MERCHANT_TOKEN, PNID),
        (OTHER_WABA, OTHER_TOKEN, OTHER_PNID),
    ] {
        vault
            .store(
                &StoredBusinessToken::new(waba, AccessToken::new(token)).phone_number_ids([number]),
            )
            .await
            .unwrap();
    }
    let app = cms_inbox::app(
        client,
        AppSecret::new(APP_SECRET),
        VerifyToken::new(VERIFY_TOKEN),
        kv.clone(),
        Arc::new(MemoryConversationStore::new()),
        vault,
        tenants(),
    )
    .unwrap();
    Harness {
        app,
        graph,
        kv,
        vault_reads,
    }
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

/// A `GET` without credentials (the webhook route needs none).
fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

/// A `GET` as the tenant whose bearer token is `bearer`.
fn get_as(bearer: &str, uri: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap()
}

/// Merchant A's inbox (it owns `PNID`).
fn get_a(uri: &str) -> Request<Body> {
    get_as(TENANT_A_BEARER, uri)
}

/// A reply as merchant A.
fn reply(phone_number_id: &str, contact: &str, text: &str) -> Request<Body> {
    reply_as(Some(TENANT_A_BEARER), phone_number_id, contact, text)
}

fn reply_as(
    bearer: Option<&str>,
    phone_number_id: &str,
    contact: &str,
    text: &str,
) -> Request<Body> {
    let mut request = Request::post(format!("/inbox/{phone_number_id}/reply"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    request
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
        .oneshot(get_a(&format!("/inbox/{PNID}/events")))
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
        call_json(&app, get_a(&format!("/inbox/{PNID}/conversations"))).await;
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
        call_json(&app, get_a(&format!("/inbox/{PNID}/conversations/{BSUID}"))).await;
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
    let (_, conversations) = call_json(&app, get_a(&format!("/inbox/{PNID}/conversations"))).await;
    assert_eq!(conversations[0]["unread"], 0);
    assert_eq!(conversations[0]["last_text"], "Yes: navy and olive.");
}

#[tokio::test]
async fn webhook_route_checks_the_verify_token_signature_and_retries() {
    let Harness { app, graph, kv, .. } = harness().await;

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
    let (_, conversations) = call_json(&app, get_a(&format!("/inbox/{PNID}/conversations"))).await;
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
        call_json(&app, get_a(&format!("/inbox/{PNID}/conversations/{BSUID}"))).await;
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
    // A number the tenant owns, but no merchant connected.
    let (status, error) = call_json(&app, reply(UNCONNECTED_PNID, BSUID, "hi")).await;
    assert_eq!(
        (status, error),
        (StatusCode::NOT_FOUND, json!({"error": "not_connected"}))
    );
    let (status, _) = call(&app, get_a(&format!("/inbox/{UNCONNECTED_PNID}/events"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(graph.requests().is_empty(), "nothing reached Meta");
}

#[tokio::test]
async fn inbox_routes_refuse_callers_without_a_tenant_token() {
    let Harness {
        app,
        graph,
        vault_reads,
        ..
    } = harness().await;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let body = inbound_text(PNID, INBOUND_ID, now);
    assert_eq!(call(&app, signed_webhook(body)).await.0, StatusCode::OK);
    let reads_before = vault_reads.load(Ordering::SeqCst);

    let routes = [
        format!("/inbox/{PNID}/events"),
        format!("/inbox/{PNID}/conversations"),
        format!("/inbox/{PNID}/conversations/{BSUID}"),
    ];
    let credentials = [
        None,
        Some("Bearer not-a-tenant-token-000000000000000000".to_owned()),
        // Merchant A's token, but not as a bearer token.
        Some(format!("Basic {TENANT_A_BEARER}")),
        Some(TENANT_A_BEARER.to_owned()),
        Some("Bearer ".to_owned()),
    ];
    for authorization in &credentials {
        let requests = routes
            .iter()
            .map(|uri| {
                let mut request = Request::get(uri.as_str());
                if let Some(value) = authorization {
                    request = request.header(header::AUTHORIZATION, value);
                }
                request.body(Body::empty()).unwrap()
            })
            .chain([{
                let mut request = reply_as(None, PNID, BSUID, "hello");
                if let Some(value) = authorization {
                    request
                        .headers_mut()
                        .insert(header::AUTHORIZATION, value.parse().unwrap());
                }
                request
            }]);
        for request in requests {
            let uri = request.uri().clone();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{uri} with {authorization:?}"
            );
            assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                json!({"error": "unauthenticated"})
            );
        }
    }
    // Refused before anything was looked up or sent.
    assert_eq!(vault_reads.load(Ordering::SeqCst), reads_before);
    assert!(graph.requests().is_empty(), "nothing reached Meta");

    // The same routes answer the tenant who owns the number.
    let (status, conversations) =
        call_json(&app, get_a(&format!("/inbox/{PNID}/conversations"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(conversations.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_tenant_cannot_read_or_answer_another_tenants_number() {
    let Harness {
        app,
        graph,
        vault_reads,
        ..
    } = harness().await;
    let now = OffsetDateTime::now_utc().unix_timestamp() - 60;
    // Both merchants have a customer conversation.
    let a = inbound_text(PNID, INBOUND_ID, now);
    let b = inbound_text(OTHER_PNID, "wamid.OTHER", now);
    assert_eq!(call(&app, signed_webhook(a)).await.0, StatusCode::OK);
    assert_eq!(call(&app, signed_webhook(b)).await.0, StatusCode::OK);
    let reads_before = vault_reads.load(Ordering::SeqCst);

    // Merchant A, authenticated, on merchant B's (connected) number: every
    // route is refused, before B's token is even read from the vault.
    let forbidden = json!({"error": "forbidden"});
    for uri in [
        format!("/inbox/{OTHER_PNID}/events"),
        format!("/inbox/{OTHER_PNID}/conversations"),
        format!("/inbox/{OTHER_PNID}/conversations/{BSUID}"),
    ] {
        let (status, body) = call_json(&app, get_a(&uri)).await;
        assert_eq!(
            (status, body),
            (StatusCode::FORBIDDEN, forbidden.clone()),
            "{uri}"
        );
    }
    let (status, body) = call_json(&app, reply(OTHER_PNID, BSUID, "Hi, it's A")).await;
    assert_eq!((status, body), (StatusCode::FORBIDDEN, forbidden.clone()));
    // And the other way round.
    let (status, body) = call_json(
        &app,
        get_as(TENANT_B_BEARER, &format!("/inbox/{PNID}/conversations")),
    )
    .await;
    assert_eq!((status, body), (StatusCode::FORBIDDEN, forbidden));
    let (status, _) = call(
        &app,
        reply_as(Some(TENANT_B_BEARER), PNID, BSUID, "Hi, it's B"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    assert_eq!(
        vault_reads.load(Ordering::SeqCst),
        reads_before,
        "the vault was consulted for a number the tenant does not own"
    );
    assert!(graph.requests().is_empty(), "nothing reached Meta");

    // Merchant B reads and answers its own number, with its own token.
    let (status, conversations) = call_json(
        &app,
        get_as(
            TENANT_B_BEARER,
            &format!("/inbox/{OTHER_PNID}/conversations"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(conversations[0]["key"]["phone_number_id"], OTHER_PNID);
    graph.push_json(
        200,
        json!({
          "messaging_product": "whatsapp",
          "contacts": [{"input": BSUID, "user_id": BSUID}],
          "messages": [{"id": SENT_ID}]
        }),
    );
    let (status, _) = call_json(
        &app,
        reply_as(Some(TENANT_B_BEARER), OTHER_PNID, BSUID, "Hi, it's B"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sent = graph.last_request().unwrap();
    assert_eq!(sent.path(), format!("/v25.0/{OTHER_PNID}/messages"));
    assert_eq!(sent.bearer(), Some(OTHER_TOKEN));
    assert_eq!(graph.remaining(), 0);
}

#[test]
fn tenants_config_refuses_what_would_open_the_inbox() {
    use cms_inbox::Tenants;
    // No tenant at all: the example does not start.
    assert!(Tenants::from_json("{}").is_err());
    // A guessable token.
    let short = Tenants::from_json(r#"{"a": {"token": "hunter2", "phone_number_ids": ["1"]}}"#);
    assert!(short.is_err());
    // Two tenants sharing a token: whose request would it be?
    let json = format!(
        r#"{{"a": {{"token": "{TENANT_A_BEARER}"}}, "b": {{"token": "{TENANT_A_BEARER}"}}}}"#
    );
    assert!(Tenants::from_json(&json).is_err());
    // A malformed value is refused without echoing it (it holds tokens).
    let error = Tenants::from_json(r#"{"a": "tenant-a-6b1f0e0d9c8b7a69584736251403f2e1"}"#)
        .err()
        .unwrap()
        .to_string();
    assert!(!error.contains("6b1f0e0d"), "{error}");
    assert!(
        Tenants::from_json(&format!(
            r#"{{"a": {{"token": "{TENANT_A_BEARER}", "phone_number_ids": ["{PNID}"]}}}}"#
        ))
        .is_ok()
    );
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
        call_json(&app, get_a(&format!("/inbox/{PNID}/conversations/{BSUID}"))).await;
    assert_eq!(conversation["messages"].as_array().unwrap().len(), 1);
}
