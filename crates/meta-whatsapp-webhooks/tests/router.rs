//! axum router and SSE helper (feature `axum`), driven with
//! `tower::ServiceExt::oneshot`.

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "axum")]

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use futures::StreamExt;
use http_body_util::BodyExt;
use tower::ServiceExt;
use meta_whatsapp_adapters::store::MemoryKvStore;
use meta_whatsapp_core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_webhooks::{
    Claim, DedupGuard, SignatureVerifier, WebhookEvent, WebhookHandler, WebhookPayload, router,
    server::SIGNATURE_HEADER, sign, sse,
};

use common::RecordingSink;

fn secret() -> AppSecret {
    AppSecret::new("5e1f0c2d3b4a59687f6e5d4c3b2a1908")
}

fn app(sink: Arc<RecordingSink>, limit: Option<usize>) -> axum::Router {
    let mut builder = WebhookHandler::builder(
        SignatureVerifier::new(vec![secret()]).unwrap(),
        VerifyToken::new("vibecoding"),
        sink,
    );
    if let Some(limit) = limit {
        builder = builder.max_body_bytes(limit);
    }
    router(Arc::new(builder.build()))
}

async fn send(app: axum::Router, request: Request<Body>) -> (StatusCode, Option<String>, String) {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        content_type,
        String::from_utf8(body.to_vec()).unwrap(),
    )
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn post(body: Vec<u8>, signature: Option<String>) -> Request<Body> {
    let mut request = Request::post("/").header(header::CONTENT_TYPE, "application/json");
    if let Some(signature) = signature {
        request = request.header(SIGNATURE_HEADER, signature);
    }
    request.body(Body::from(body)).unwrap()
}

#[tokio::test]
async fn get_echoes_the_challenge_as_inert_text() {
    let response = app(Arc::default(), None)
        .oneshot(get(
            "/?hub.mode=subscribe&hub.challenge=%3Cscript%3Ealert(1)%3C/script%3E&hub.verify_token=vibecoding",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"<script>alert(1)</script>");

    let (status, _, body) = send(
        app(Arc::default(), None),
        get("/?hub.mode=subscribe&hub.challenge=1158201444&hub.verify_token=vibecoding"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "1158201444");
}

#[tokio::test]
async fn a_blank_configured_verify_token_answers_403_to_everyone() {
    for token in ["", " "] {
        let app = router(Arc::new(
            WebhookHandler::builder(
                SignatureVerifier::new(vec![secret()]).unwrap(),
                VerifyToken::new(token),
                Arc::new(RecordingSink::default()),
            )
            .build(),
        ));
        for uri in [
            "/?hub.mode=subscribe&hub.challenge=1&hub.verify_token=",
            "/?hub.mode=subscribe&hub.challenge=1&hub.verify_token=%20",
            "/?hub.mode=subscribe&hub.challenge=1&hub.verify_token=vibecoding",
        ] {
            let (status, _, body) = send(app.clone(), get(uri)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{token:?} {uri}");
            assert!(body.is_empty(), "challenge leaked: {body}");
        }
    }
}

#[tokio::test]
async fn post_is_503_while_another_request_delivers_the_same_event() {
    let kv = Arc::new(MemoryKvStore::new());
    let sink = Arc::new(RecordingSink::default());
    let handler = WebhookHandler::builder(
        SignatureVerifier::new(vec![secret()]).unwrap(),
        VerifyToken::new("vibecoding"),
        sink.clone(),
    )
    .dedup(DedupGuard::new(kv.clone()))
    .build();
    let app = router(Arc::new(handler));
    let body = common::fixture_bytes("messages/text.json");
    let event = WebhookPayload::from_slice(&body)
        .unwrap()
        .into_events()
        .remove(0);
    let other = DedupGuard::new(kv);
    let Claim::Acquired(ticket) = other.claim(&event).await.unwrap() else {
        panic!()
    };

    let (status, _, _) = send(
        app.clone(),
        post(body.clone(), Some(sign(&secret(), &body))),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(sink.calls(), 0);

    other.complete(&ticket).await.unwrap();
    let (status, _, _) = send(app, post(body.clone(), Some(sign(&secret(), &body)))).await;
    assert_eq!(status, StatusCode::OK, "a duplicate by now");
    assert_eq!(sink.calls(), 0);
}

#[tokio::test]
async fn get_rejects_wrong_token_wrong_mode_and_garbage_with_403() {
    for uri in [
        "/?hub.mode=subscribe&hub.challenge=1&hub.verify_token=nope",
        "/?hub.mode=unsubscribe&hub.challenge=1&hub.verify_token=vibecoding",
        "/?hub.mode=subscribe&hub.verify_token=vibecoding",
        "/",
        "/?hub.mode=%ZZ",
    ] {
        let (status, _, body) = send(app(Arc::default(), None), get(uri)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        assert!(body.is_empty(), "{uri}: challenge leaked: {body}");
    }
}

#[tokio::test]
async fn post_with_valid_signature_is_delivered() {
    let sink = Arc::new(RecordingSink::default());
    let body = common::fixture_bytes("messages/text.json");
    let signature = sign(&secret(), &body);
    let (status, _, _) = send(app(sink.clone(), None), post(body, Some(signature))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(matches!(
        sink.delivered()[..],
        [WebhookEvent::MessageReceived { .. }]
    ));
}

#[tokio::test]
async fn post_with_bad_or_missing_signature_is_401() {
    let sink = Arc::new(RecordingSink::default());
    let body = common::fixture_bytes("messages/text.json");
    let wrong = sign(&AppSecret::new("other"), &body);
    for signature in [None, Some(wrong), Some("sha256=xyz".to_owned())] {
        let (status, _, _) = send(app(sink.clone(), None), post(body.clone(), signature)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let request = Request::post("/")
        .header(SIGNATURE_HEADER, &b"sha256=\xff"[..])
        .body(Body::from(body))
        .unwrap();
    let (status, _, _) = send(app(sink.clone(), None), request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "non-ASCII header");
    assert_eq!(sink.calls(), 0);
}

/// Security review L3: a delivery without a well-formed
/// `X-Hub-Signature-256` is refused before a byte of its body is read or
/// buffered (the router used to buffer up to the 3 MiB limit first).
#[tokio::test]
async fn post_without_a_well_formed_signature_is_401_before_the_body_is_read() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let sink = Arc::new(RecordingSink::default());
    for signature in [
        None,
        Some(""),
        Some("sha256=xyz"),
        Some("md5=0123"),
        Some("sha256= "),
    ] {
        let polled = Arc::new(AtomicBool::new(false));
        let flag = polled.clone();
        let body = Body::from_stream(futures::stream::poll_fn(move |_| {
            flag.store(true, Ordering::SeqCst);
            std::task::Poll::Ready(None::<Result<axum::body::Bytes, std::io::Error>>)
        }));
        let mut request = Request::post("/");
        if let Some(signature) = signature {
            request = request.header(SIGNATURE_HEADER, signature);
        }
        let (status, _, _) = send(app(sink.clone(), None), request.body(body).unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{signature:?}");
        assert!(
            !polled.load(Ordering::SeqCst),
            "{signature:?}: the body was read"
        );
    }
    // Unsigned and over the limit: refused for the signature, so the size
    // is never learnt.
    let (status, _, _) = send(app(sink.clone(), Some(64)), post(vec![b'x'; 1024], None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(sink.calls(), 0);
}

#[tokio::test]
async fn post_over_the_limit_is_413() {
    let sink = Arc::new(RecordingSink::default());
    let body = common::fixture_bytes("messages/text.json");
    let signature = sign(&secret(), &body);
    let (status, _, _) = send(app(sink.clone(), Some(64)), post(body, Some(signature))).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(sink.calls(), 0);
}

/// Meta sends payloads up to 3 MB; axum's own implicit limit is 2 MiB. The
/// router must raise it to the handler's limit or big history webhooks get
/// a 413 and are retried for 7 days, then dropped.
#[tokio::test]
async fn post_between_axum_default_and_our_limit_is_accepted() {
    let sink = Arc::new(RecordingSink::default());
    let mut value: serde_json::Value =
        serde_json::from_slice(&common::fixture_bytes("messages/text.json")).unwrap();
    let text = "a".repeat(2_500_000);
    value["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"] = text.clone().into();
    let body = serde_json::to_vec(&value).unwrap();
    assert!(body.len() > 2_097_152 && body.len() < meta_whatsapp_webhooks::DEFAULT_MAX_BODY_BYTES);
    let signature = sign(&secret(), &body);
    let (status, _, text_body) = send(app(sink.clone(), None), post(body, Some(signature))).await;
    assert_eq!(status, StatusCode::OK, "{text_body}");
    let delivered = sink.delivered();
    let [WebhookEvent::MessageReceived { message, .. }] = &delivered[..] else {
        panic!("{} events", delivered.len())
    };
    let meta_whatsapp_webhooks::fields::MessageContent::Text(t) = &message.content else {
        panic!()
    };
    assert_eq!(t.body.len(), text.len());
}

#[tokio::test]
async fn post_is_500_when_the_sink_fails() {
    let sink = Arc::new(RecordingSink::failing_on(0));
    let body = common::fixture_bytes("messages/text.json");
    let signature = sign(&secret(), &body);
    let (status, _, _) = send(app(sink, None), post(body, Some(signature))).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn post_of_signed_garbage_is_200() {
    let sink = Arc::new(RecordingSink::default());
    let body = b"<html>not a webhook</html>".to_vec();
    let signature = sign(&secret(), &body);
    let (status, _, _) = send(app(sink.clone(), None), post(body, Some(signature))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(matches!(
        sink.delivered()[..],
        [WebhookEvent::Unparsed { .. }]
    ));
}

#[tokio::test]
async fn sse_streams_filtered_events_and_reports_lag() {
    let (tx, rx) = tokio::sync::broadcast::channel(2);
    let wanted = common::events("messages/text.json").remove(0);
    let other = common::events("fields/security.json").remove(0);

    // Overflow the channel before the stream reads: 4 sends into capacity 2.
    for _ in 0..3 {
        tx.send(other.clone()).unwrap();
    }
    tx.send(wanted.clone()).unwrap();
    drop(tx);

    let response =
        axum::response::IntoResponse::into_response(sse(rx, |e| e.phone_number_id().is_some()));
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/event-stream"
    );
    // Bounded twice over: a stream that fails to end when the channel
    // closes must fail this test, not collect until the machine runs out
    // of memory.
    let mut body = response.into_body().into_data_stream();
    let collect = async {
        let mut text = String::new();
        while let Some(chunk) = body.next().await {
            text.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
            assert!(
                text.len() < 64 * 1024,
                "SSE stream did not end: {} bytes so far",
                text.len()
            );
        }
        text
    };
    let text = tokio::time::timeout(std::time::Duration::from_secs(5), collect)
        .await
        .expect("the SSE stream ends when the channel closes");
    assert!(text.contains("event: lagged\ndata: 2\n"), "{text}");
    let json = serde_json::to_string(&wanted).unwrap();
    assert!(
        text.contains(&format!("event: whatsapp\ndata: {json}\n")),
        "{text}"
    );
    assert!(!text.contains("security_updated"), "filtered out: {text}");
}
