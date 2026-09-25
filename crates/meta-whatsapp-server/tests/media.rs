//! Media (scope `media`; docs/design/server.md, section 4.2): uploads
//! checked (type, size) before any request, downloads verified against
//! Meta's SHA-256 before the first byte (or aborted mid-stream with
//! `stream=true`), deletes.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use base64::Engine as _;
use common::capture::sha256_hex;
use common::{Call, Harness};
use futures::StreamExt as _;
use http_body_util::BodyExt;
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::testing::RecordedBody;
use meta_whatsapp_rs::core::transport::{
    HttpRequest, HttpResponse, HttpTransport, StreamingResponse,
};
use meta_whatsapp_rs::webhooks::axum::body::Bytes;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{Scope, TenantId};
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tower::ServiceExt;

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const MEDIA_ID: &str = "1037543291543636";
/// The download URL shape of `business-phone-numbers/media`: Meta's
/// lookaside host, where the library lets the token go.
const URL: &str = "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1037543291543636&ext=1&hash=abc";

async fn connected_with(settings: Settings) -> (Harness, String) {
    let h = Harness::with_settings(settings);
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Media]).await;
    (h, key)
}

async fn connected() -> (Harness, String) {
    connected_with(common::test_settings()).await
}

fn upload(key: &str, parts: &[(&str, Option<&str>, &[u8])]) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{PN}/media"))
        .key(key)
        .multipart(parts)
}

/// `GET /{media-id}`'s answer (`business-phone-numbers/media`, "Response
/// syntax") for `data`, with `sha256` as given.
fn media_info(data: &[u8], sha256: &str, file_size: Option<usize>) -> Value {
    let mut info = json!({"messaging_product": "whatsapp", "url": URL, "mime_type": "image/jpeg",
                          "sha256": sha256, "id": MEDIA_ID});
    if let Some(size) = file_size {
        info["file_size"] = json!(size.to_string());
    }
    let _ = data;
    info
}

fn download(key: &str, query: &str) -> Call {
    Call::get(format!("/v1/numbers/{PN}/media/{MEDIA_ID}{query}")).key(key)
}

fn code(reply: &common::Reply) -> String {
    reply.code()
}

/// The upload request Meta documents: multipart `messaging_product`,
/// `type` and `file`, with the merchant's token; `201 {media_id}`.
#[tokio::test]
async fn an_upload_is_metas_multipart_request() {
    let (h, key) = connected().await;
    // business-phone-numbers/media, "Example response".
    h.graph.push_json(200, json!({"id": MEDIA_ID}));
    let jpeg = b"\xff\xd8\xff\xe0 a jpeg".to_vec();
    let reply = h
        .call(upload(
            &key,
            &[
                ("type", None, b"image/jpeg"),
                ("file", Some("../black friday (1).jpeg"), &jpeg),
            ],
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(reply.json(), json!({"media_id": MEDIA_ID}));
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{PN}/media"));
    assert_eq!(request.bearer(), Some(TOKEN));
    let RecordedBody::Multipart(parts) = &request.body else {
        panic!("not multipart: {:?}", request.body)
    };
    let names: Vec<&str> = parts.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(names, ["messaging_product", "type", "file"]);
    assert_eq!(
        &request.multipart_field("messaging_product").unwrap().2[..],
        b"whatsapp"
    );
    assert_eq!(
        &request.multipart_field("type").unwrap().2[..],
        b"image/jpeg"
    );
    let (filename, content_type, data) = request.multipart_field("file").unwrap();
    assert_eq!(
        filename,
        Some(".._black_friday__1_.jpeg"),
        "an inert file name"
    );
    assert_eq!(content_type, Some("image/jpeg"));
    assert_eq!(&data[..], &jpeg[..]);
    assert_eq!(h.graph.remaining(), 0);
}

/// Type and size are checked before any request: an unsupported type is
/// `422` on `type`, a file over its kind's limit `413 media_too_large`
/// (5 MiB for an image), and a form without either part `422`. Decisive:
/// the checks before `upload`.
#[tokio::test]
async fn uploads_are_checked_before_any_request() {
    let (h, key) = connected().await;
    let five_mib = 5 * 1024 * 1024;
    let at_limit = vec![7u8; five_mib];
    let over = vec![7u8; five_mib + 1];
    for (parts, expected) in [
        (
            vec![
                ("type", None, &b"image/gif"[..]),
                ("file", Some("a.gif"), &b"GIF89a"[..]),
            ],
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                Some("type"),
            ),
        ),
        (
            vec![
                ("type", None, &b"image/png"[..]),
                ("file", Some("a.png"), &over[..]),
            ],
            (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large", None),
        ),
        // The file first: read whole, then checked.
        (
            vec![
                ("file", Some("a.png"), &over[..]),
                ("type", None, &b"image/png"[..]),
            ],
            (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large", None),
        ),
        (
            vec![("file", Some("a.png"), &b"x"[..])],
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                Some("type"),
            ),
        ),
        (
            vec![("type", None, &b"image/png"[..])],
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                Some("file"),
            ),
        ),
        (
            vec![
                ("type", None, &b"image/png"[..]),
                ("file", Some("a.png"), &b"x"[..]),
                ("caption", None, &b"x"[..]),
            ],
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                Some("body"),
            ),
        ),
    ] {
        let reply = h.call(upload(&key, &parts)).await;
        let body = reply.json();
        assert_eq!(
            (
                reply.status,
                body["error"]["code"].as_str().unwrap(),
                body["error"]["field"].as_str()
            ),
            expected,
            "{}",
            reply.text
        );
    }
    let not_a_form = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{PN}/media"))
                .key(&key)
                .json(&json!({"type": "image/png"})),
        )
        .await;
    assert_eq!(not_a_form.code(), "invalid_request");
    assert!(h.graph.requests().is_empty(), "nothing reached Meta");
    // At the limit, it goes.
    h.graph.push_json(200, json!({"id": MEDIA_ID}));
    let reply = h
        .call(upload(
            &key,
            &[
                ("type", None, b"image/png"),
                ("file", Some("a.png"), &at_limit),
            ],
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// `WA_SERVER_MEDIA_MAX_BYTES` caps every upload, a document's 100 MiB
/// included. Decisive: the configured cap.
#[tokio::test]
async fn the_configured_cap_bounds_every_upload() {
    let (h, key) = connected_with(Settings {
        media_max_bytes: 1000,
        ..common::test_settings()
    })
    .await;
    let over = vec![1u8; 1001];
    let reply = h
        .call(upload(
            &key,
            &[
                ("type", None, b"application/pdf"),
                ("file", Some("a.pdf"), &over),
            ],
        ))
        .await;
    assert_eq!(
        (reply.status, code(&reply).as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large"),
        "{}",
        reply.text
    );
    assert!(h.graph.requests().is_empty());
}

/// An upload with an `Idempotency-Key` never uploads twice.
#[tokio::test]
async fn an_upload_key_replays_the_media_id() {
    let (h, key) = connected().await;
    h.graph.push_json(200, json!({"id": MEDIA_ID}));
    let call = || {
        upload(
            &key,
            &[
                ("type", None, b"image/png"),
                ("file", Some("a.png"), common::SAMPLE_PNG),
            ],
        )
        .header("idempotency-key", "voucher-42")
    };
    let first = h.call(call()).await;
    assert_eq!(first.status, StatusCode::CREATED);
    let again = h.call(call()).await;
    assert_eq!(again.status, StatusCode::CREATED);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.text, first.text);
    let other = h
        .call(
            upload(
                &key,
                &[
                    ("type", None, b"image/png"),
                    ("file", Some("a.png"), b"another file"),
                ],
            )
            .header("idempotency-key", "voucher-42"),
        )
        .await;
    assert_eq!(other.code(), "idempotency_key_reused");
    assert_eq!(h.graph.requests().len(), 1);
    assert_eq!(h.graph.remaining(), 0);
}

/// A whole download: the URL from `GET /{media-id}`, then the file from
/// Meta's host, both with the merchant's token; answered once verified,
/// with `X-WA-SHA256` in hex (Meta's digest may be hex or base64).
#[tokio::test]
async fn a_download_is_verified_before_it_is_answered() {
    let (h, key) = connected().await;
    let file = b"\xff\xd8\xff\xe0 the customer's photo".to_vec();
    let hex = sha256_hex(&file);
    let base64 = base64::engine::general_purpose::STANDARD.encode(hex::decode(&hex).unwrap());
    for reported in [hex.clone(), base64] {
        h.graph
            .push_json(200, media_info(&file, &reported, Some(file.len())));
        h.graph.push_bytes(200, "image/jpeg", file.clone());
        let response = h
            .internal
            .clone()
            .oneshot(download(&key, "").build())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-wa-sha256"], hex.as_str());
        assert_eq!(response.headers()["content-type"], "image/jpeg");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["content-disposition"], "attachment");
        assert_eq!(
            response.headers()["content-security-policy"],
            "sandbox; default-src 'none'"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], &file[..]);
        let requests = h.graph.requests();
        let [.., url, bytes] = &requests[..] else {
            panic!("two requests")
        };
        assert_eq!(
            (url.method.clone(), url.path()),
            (Method::GET, "/v25.0/1037543291543636")
        );
        assert_eq!(
            url.query("phone_number_id").as_deref(),
            Some(PN),
            "only this number's media"
        );
        assert_eq!(url.bearer(), Some(TOKEN));
        assert_eq!(bytes.url.as_str(), URL);
        assert_eq!(bytes.bearer(), Some(TOKEN));
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// A file that does not match Meta's digest is `502 integrity`, and not
/// one byte of it is answered. Decisive: verifying before the first byte.
#[tokio::test]
async fn a_mismatched_digest_is_502_integrity_with_no_byte_of_the_file() {
    let (h, key) = connected().await;
    let file = b"SUBSTITUTED-BYTES-OF-THE-FILE".to_vec();
    h.graph
        .push_json(200, media_info(&file, &sha256_hex(b"the original"), None));
    h.graph.push_bytes(200, "image/jpeg", file.clone());
    let reply = h.call(download(&key, "")).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::BAD_GATEWAY, "integrity"),
        "{}",
        reply.text
    );
    assert!(!reply.text.contains("SUBSTITUTED"), "{}", reply.text);
    assert!(reply.headers.get("x-wa-sha256").is_none());
    // A digest Meta did not send usably: nothing can be verified.
    h.graph
        .push_json(200, media_info(&file, "PHOTO_HASH", None));
    let reply = h.call(download(&key, "")).await;
    assert_eq!(reply.code(), "integrity");
    assert_eq!(h.graph.requests().len(), 3, "no download without a digest");
    assert_eq!(h.graph.remaining(), 0);
}

/// `max_bytes` (default and cap 16 MiB): a size Meta reports over it is
/// `413` before the download; a body growing past it is `413` too; more
/// than 16 MiB needs `stream=true`. Decisive: the caps.
#[tokio::test]
async fn downloads_are_capped() {
    let (h, key) = connected().await;
    let file = vec![9u8; 100];
    let digest = sha256_hex(&file);
    // Reported too large: no download.
    h.graph
        .push_json(200, media_info(&file, &digest, Some(100)));
    let reply = h.call(download(&key, "?max_bytes=99")).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large")
    );
    assert_eq!(h.graph.requests().len(), 1, "refused before the download");
    // Not reported, but larger.
    h.graph.push_json(200, media_info(&file, &digest, None));
    h.graph.push_bytes(200, "image/jpeg", file.clone());
    let reply = h.call(download(&key, "?max_bytes=99")).await;
    assert_eq!(reply.code(), "media_too_large");
    // At the limit.
    h.graph.push_json(200, media_info(&file, &digest, None));
    h.graph.push_bytes(200, "image/jpeg", file.clone());
    let reply = h.call(download(&key, "?max_bytes=100")).await;
    assert_eq!(reply.status, StatusCode::OK);
    // The default is 16 MiB: Meta reporting more is refused.
    h.graph
        .push_json(200, media_info(&file, &digest, Some(16 * 1024 * 1024 + 1)));
    assert_eq!(h.call(download(&key, "")).await.code(), "media_too_large");
    h.graph
        .push_json(200, media_info(&file, &digest, Some(16 * 1024 * 1024)));
    h.graph.push_bytes(200, "image/jpeg", file.clone());
    assert_eq!(h.call(download(&key, "")).await.status, StatusCode::OK);
    // Above 16 MiB only with stream=true (up to WA_SERVER_MEDIA_MAX_BYTES).
    let before = h.graph.requests().len();
    for query in [
        "?max_bytes=16777217",
        "?max_bytes=0",
        "?max_bytes=x",
        "?stream=yes",
    ] {
        let reply = h.call(download(&key, query)).await;
        assert_eq!(reply.code(), "invalid_request", "{query}");
    }
    let reply = h
        .call(download(
            &key,
            &format!("?stream=true&max_bytes={}", 100 * 1024 * 1024 + 1),
        ))
        .await;
    assert_eq!(reply.json()["error"]["field"], "max_bytes");
    assert_eq!(
        h.graph.requests().len(),
        before,
        "refused before any request"
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// `stream=true` forwards the bytes; a mismatch aborts the connection, so
/// the body never completes. Decisive: the verifying stream.
#[tokio::test]
async fn a_streamed_download_aborts_on_a_mismatch() {
    let (h, key) = connected().await;
    let file = vec![5u8; 20 * 1024 * 1024];
    let digest = sha256_hex(&file);
    h.graph
        .push_json(200, media_info(&file, &digest, Some(file.len())));
    h.graph.push_bytes(200, "image/jpeg", file.clone());
    let response = h
        .internal
        .clone()
        .oneshot(download(&key, "?stream=true").build())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-wa-sha256"], digest.as_str());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.len(), file.len());
    // A mismatch: the body errors instead of ending.
    h.graph
        .push_json(200, media_info(&file, &sha256_hex(b"other"), None));
    h.graph.push_bytes(200, "image/jpeg", b"tampered".to_vec());
    let response = h
        .internal
        .clone()
        .oneshot(download(&key, "?stream=true").build())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "the head went out");
    // Not even the bytes before the abort hold the whole tampered file:
    // the last chunk waits for the digest.
    let mut body = response.into_body();
    let mut received = Vec::new();
    let aborted = loop {
        match body.frame().await {
            None => break false,
            Some(Err(_)) => break true,
            Some(Ok(frame)) => received.extend_from_slice(frame.data_ref().unwrap()),
        }
    };
    assert!(aborted, "the body must not complete");
    assert!(
        received.len() < b"tampered".len(),
        "the whole tampered file arrived before the abort: {received:?}"
    );
    // Past max_bytes while streaming: the same.
    h.graph.push_json(200, media_info(&file, &digest, None));
    h.graph.push_bytes(200, "image/jpeg", file.clone());
    let response = h
        .internal
        .clone()
        .oneshot(download(&key, "?stream=true&max_bytes=1000").build())
        .await
        .unwrap();
    assert!(response.into_body().collect().await.is_err());
    assert_eq!(h.graph.remaining(), 0);
}

/// The tenant's share of the media slots busy: `429`, before any request.
#[tokio::test]
async fn busy_media_slots_are_429() {
    let (h, key) = connected_with(Settings {
        media_concurrency: 1,
        ..common::test_settings()
    })
    .await;
    let held = h
        .state
        .media_slots()
        .try_acquire(&TenantId::parse(TENANT).unwrap())
        .unwrap();
    let reply = h
        .call(upload(
            &key,
            &[("type", None, b"image/png"), ("file", Some("a.png"), b"x")],
        ))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_requests")
    );
    assert_eq!(reply.headers["retry-after"], "1");
    assert_eq!(h.call(download(&key, "")).await.code(), "too_many_requests");
    drop(held);
    assert!(h.graph.requests().is_empty());
}

/// `DELETE /{media-id}?phone_number_id={pn}` with the merchant's token,
/// once `GET /{media-id}?phone_number_id={pn}` said it is that media.
#[tokio::test]
async fn a_delete_is_metas_request() {
    let (h, key) = connected().await;
    h.graph
        .push_json(200, media_info(b"x", &sha256_hex(b"x"), None));
    h.graph.push_json(200, json!({"success": true}));
    let reply = h
        .call(Call::new(Method::DELETE, format!("/v1/numbers/{PN}/media/{MEDIA_ID}")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(
        (request.method.clone(), request.path()),
        (Method::DELETE, "/v25.0/1037543291543636")
    );
    assert_eq!(request.query("phone_number_id").as_deref(), Some(PN));
    assert_eq!(request.bearer(), Some(TOKEN));
    let requests = h.graph.requests();
    let [lookup, _] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(
        (lookup.method.clone(), lookup.path()),
        (Method::GET, "/v25.0/1037543291543636")
    );
    assert_eq!(lookup.query("phone_number_id").as_deref(), Some(PN));
    assert_eq!(h.graph.remaining(), 0);
}

/// With `type` first, a file over its kind's limit is refused without
/// reading the rest of the form: at most the limit and a chunk are
/// pulled. Decisive: the per-kind cap applied while the file streams in.
#[tokio::test]
async fn a_known_type_stops_reading_the_upload_at_its_limit() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::StreamExt as _;
    use meta_whatsapp_rs::webhooks::axum::body::{Body, Bytes};

    let (h, key) = connected().await;
    let image = vec![7u8; 12 * 1024 * 1024];
    let (content_type, form) = common::multipart(&[
        ("type", None, b"image/png"),
        ("file", Some("big.png"), &image),
    ]);
    let pulled = Arc::new(AtomicUsize::new(0));
    let counter = pulled.clone();
    let chunks: Vec<Bytes> = form.chunks(64 * 1024).map(Bytes::copy_from_slice).collect();
    let body = futures::stream::iter(chunks).map(move |chunk| {
        counter.fetch_add(chunk.len(), Ordering::SeqCst);
        Ok::<_, std::io::Error>(chunk)
    });
    let reply = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{PN}/media"))
                .key(&key)
                .header("content-type", &content_type)
                .body(Body::from_stream(body)),
        )
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large"),
        "{}",
        reply.text
    );
    let read = pulled.load(Ordering::SeqCst);
    assert!(
        read < 6 * 1024 * 1024,
        "read {read} bytes of a 12 MiB form for a 5 MiB image limit"
    );
    assert!(h.graph.requests().is_empty());
}

/// A media id is digits: anything else is `422` on `media_id`, before any
/// request (an id names any Graph node the token reaches).
#[tokio::test]
async fn a_media_id_is_digits() {
    let (h, key) = connected().await;
    // Longer than 64 digits, too.
    let too_long = "9".repeat(65);
    for bad in ["Y2FwaV9ncm91cDox", "0123", "12a", "1%2F2", &too_long] {
        for method in [Method::GET, Method::DELETE] {
            let reply = h
                .call(Call::new(method.clone(), format!("/v1/numbers/{PN}/media/{bad}")).key(&key))
                .await;
            assert_eq!(
                (reply.status, reply.json()["error"]["field"].as_str()),
                (StatusCode::UNPROCESSABLE_ENTITY, Some("media_id")),
                "{method} {bad}: {}",
                reply.text
            );
        }
    }
    assert!(h.graph.requests().is_empty());
}

/// An id Meta answers for with another kind of node (a flow, say), or
/// with another id: `404`, nothing downloaded, and a deletion sends no
/// `DELETE` (it would delete that node). Decisive: the lookup before the
/// deletion, and the id compared.
#[tokio::test]
async fn an_id_that_is_not_this_media_is_not_found_and_not_deleted() {
    let (h, key) = connected().await;
    // flows/reference: a flow node, not a media object (no `url`).
    let flow = json!({"id": MEDIA_ID, "name": "My flow", "status": "DRAFT"});
    let other = media_info(b"x", &sha256_hex(b"x"), None);
    let mut other_id = other.clone();
    other_id["id"] = json!("1037543291543699");
    for answer in [flow, other_id] {
        for method in [Method::GET, Method::DELETE] {
            let asked = h.graph.requests().len();
            h.graph.push_json(200, answer.clone());
            let reply = h
                .call(
                    Call::new(method.clone(), format!("/v1/numbers/{PN}/media/{MEDIA_ID}"))
                        .key(&key),
                )
                .await;
            assert_eq!(
                (reply.status, reply.code().as_str()),
                (StatusCode::NOT_FOUND, "not_found"),
                "{method} <- {answer}: {}",
                reply.text
            );
            let requests = h.graph.requests();
            assert_eq!(requests.len(), asked + 1, "{method}: the lookup only");
            let lookup = requests.last().unwrap();
            assert_eq!(lookup.method, Method::GET, "no DELETE was sent");
            assert_eq!(lookup.query("phone_number_id").as_deref(), Some(PN));
        }
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// A `type` with a control character is `422` on `type`, before any
/// request (it would be a header of the part Meta receives).
#[tokio::test]
async fn a_type_with_a_control_character_is_refused() {
    let (h, key) = connected().await;
    let reply = h
        .call(upload(
            &key,
            &[
                ("type", None, b"audio/ogg; codecs=\x01opus"),
                ("file", Some("a.ogg"), b"OggS"),
            ],
        ))
        .await;
    assert_eq!(
        (reply.status, reply.json()["error"]["field"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("type")),
        "{}",
        reply.text
    );
    assert!(h.graph.requests().is_empty());
}

/// A form's framing (a preamble, a part's headers) is read up to 16 KiB
/// and a chunk, not to the end of the body: `422` on `body`. Decisive:
/// the framing budget.
#[tokio::test]
async fn a_form_whose_framing_never_ends_is_refused_early() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::StreamExt as _;
    use meta_whatsapp_rs::webhooks::axum::body::{Body, Bytes};

    let (h, key) = connected().await;
    let (content_type, form) =
        common::multipart(&[("type", None, b"image/png"), ("file", Some("a.png"), b"x")]);
    let long_name = format!("{}.png", "n".repeat(4 * 1024 * 1024));
    let (_, long_header) = common::multipart(&[("file", Some(&long_name), b"x")]);
    let preamble = [vec![b'p'; 4 * 1024 * 1024], form].concat();
    for body in [preamble, long_header] {
        let pulled = Arc::new(AtomicUsize::new(0));
        let counter = pulled.clone();
        let chunks: Vec<Bytes> = body.chunks(1024).map(Bytes::copy_from_slice).collect();
        let stream = futures::stream::iter(chunks).map(move |chunk| {
            counter.fetch_add(chunk.len(), Ordering::SeqCst);
            Ok::<_, std::io::Error>(chunk)
        });
        let reply = h
            .call(
                Call::new(Method::POST, format!("/v1/numbers/{PN}/media"))
                    .key(&key)
                    .header("content-type", &content_type)
                    .body(Body::from_stream(stream)),
            )
            .await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("body")),
            "{}",
            reply.text
        );
        let read = pulled.load(Ordering::SeqCst);
        assert!(read < 256 * 1024, "read {read} bytes of a 4 MiB framing");
    }
    assert!(h.graph.requests().is_empty());
}

/// Tenant A's upload that never finishes holds A's share of the media
/// slots, not the replica's: B still uploads, A's next upload is `429`.
/// Decisive: the tenant's share.
#[tokio::test]
async fn one_tenants_stuck_upload_leaves_the_others_theirs() {
    use futures::StreamExt as _;
    use meta_whatsapp_rs::webhooks::axum::body::{Body, Bytes};

    let (h, a_key) = connected_with(Settings {
        media_concurrency: 2,
        ..common::test_settings()
    })
    .await;
    h.tenant("merchant-43").await;
    h.connect(
        "merchant-43",
        "102290129340399",
        &["106540352242923"],
        "TOKEN-43",
    )
    .await;
    let b_key = h.tenant_key("merchant-43", &[Scope::Media]).await;
    // A's upload: the `type` part, then nothing, ever.
    let (content_type, form) =
        common::multipart(&[("type", None, b"image/png"), ("file", Some("a.png"), b"x")]);
    let (reading, read) = tokio::sync::oneshot::channel::<()>();
    let mut reading = Some(reading);
    let head = Bytes::copy_from_slice(&form[..form.len() / 2]);
    let stuck = futures::stream::once(async move { Ok::<_, std::io::Error>(head) })
        .chain(futures::stream::pending())
        .inspect(move |_| {
            if let Some(reading) = reading.take() {
                let _ = reading.send(());
            }
        });
    let router = h.internal.clone();
    let request = Call::new(Method::POST, format!("/v1/numbers/{PN}/media"))
        .key(&a_key)
        .header("content-type", &content_type)
        .body(Body::from_stream(stuck))
        .build();
    let task = tokio::spawn(async move { common::send(&router, request).await });
    read.await.unwrap();
    // B uploads.
    h.graph.push_json(200, json!({"id": MEDIA_ID}));
    let theirs = h
        .call(
            Call::new(Method::POST, "/v1/numbers/106540352242923/media")
                .key(&b_key)
                .multipart(&[("type", None, b"image/png"), ("file", Some("b.png"), b"x")]),
        )
        .await;
    assert_eq!(theirs.status, StatusCode::CREATED, "{}", theirs.text);
    // A's share is taken.
    let again = h
        .call(upload(
            &a_key,
            &[("type", None, b"image/png"), ("file", Some("a.png"), b"x")],
        ))
        .await;
    assert_eq!(
        (again.status, again.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_requests")
    );
    task.abort();
    assert_eq!(h.graph.remaining(), 0);
}

/// A streamed download holds a stream slot of its tenant's share until
/// its body ends; another tenant streams meanwhile. Decisive: the slot
/// moved into the body.
#[tokio::test]
async fn a_streamed_download_holds_its_slot_until_its_body_ends() {
    let (h, key) = connected_with(Settings {
        media_streams: 2,
        ..common::test_settings()
    })
    .await;
    h.tenant("merchant-43").await;
    h.connect(
        "merchant-43",
        "102290129340399",
        &["106540352242923"],
        "TOKEN-43",
    )
    .await;
    let b_key = h.tenant_key("merchant-43", &[Scope::Media]).await;
    let file = b"a streamed file".to_vec();
    let digest = sha256_hex(&file);
    h.graph.push_json(200, media_info(&file, &digest, None));
    h.graph.push_bytes(200, "image/jpeg", file.clone());
    let open = h
        .internal
        .clone()
        .oneshot(download(&key, "?stream=true").build())
        .await
        .unwrap();
    assert_eq!(open.status(), StatusCode::OK);
    // The body is not read yet: the slot is still held.
    let refused = h.call(download(&key, "?stream=true")).await;
    assert_eq!(refused.code(), "too_many_requests", "{}", refused.text);
    h.graph.push_json(200, media_info(&file, &digest, None));
    h.graph.push_bytes(200, "image/jpeg", file.clone());
    let theirs = h
        .call(
            Call::get(format!(
                "/v1/numbers/106540352242923/media/{MEDIA_ID}?stream=true"
            ))
            .key(&b_key),
        )
        .await;
    assert_eq!(theirs.status, StatusCode::OK, "{}", theirs.text);
    // Once the body ended, the slot is free.
    let body = open.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], &file[..]);
    h.graph.push_json(200, media_info(&file, &digest, None));
    h.graph.push_bytes(200, "image/jpeg", file.clone());
    assert_eq!(
        h.call(download(&key, "?stream=true")).await.status,
        StatusCode::OK
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// What an HTTP client sees of a streamed download, on the wire: `200`
/// and a chunked body that ends with its terminating chunk when the file
/// is verified. When it is not, the connection closes without that chunk
/// (curl: "transfer closed with outstanding read data remaining"; a fetch
/// body rejects), and the chunk held back never arrives: a file of one
/// chunk closes the connection before even the head went out (curl:
/// "Empty reply from server"). Decisive: the abort, and the chunk held
/// back.
#[tokio::test]
async fn over_http_a_tampered_stream_ends_without_its_terminating_chunk() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let (h, key) = connected().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(meta_whatsapp_server::listen::serve(
        listener,
        h.internal.clone(),
        meta_whatsapp_server::listen::INTERNAL_LIMITS,
        async {
            let _ = stopped.await;
        },
    ));
    let get = |key: String| async move {
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "GET /v1/numbers/{PN}/media/{MEDIA_ID}?stream=true HTTP/1.1\r\nHost: wa\r\n\
             Authorization: Bearer {key}\r\nConnection: close\r\n\r\n"
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        let _ = socket.read_to_end(&mut raw).await;
        String::from_utf8_lossy(&raw).into_owned()
    };
    let file = b"THE-VERIFIED-FILE".to_vec();
    h.graph
        .push_json(200, media_info(&file, &sha256_hex(&file), None));
    h.graph.push_bytes(200, "image/jpeg", file.clone());
    let verified = get(key.clone()).await;
    assert!(verified.starts_with("HTTP/1.1 200"), "{verified}");
    assert!(
        verified
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked"),
        "{verified}"
    );
    assert!(verified.contains("THE-VERIFIED-FILE"), "{verified}");
    assert!(
        verified.ends_with("0\r\n\r\n"),
        "a complete body: {verified:?}"
    );

    h.graph
        .push_json(200, media_info(b"x", &sha256_hex(b"the original"), None));
    h.graph
        .push_bytes(200, "image/jpeg", b"TAMPERED-BYTES".to_vec());
    let tampered = get(key).await;
    assert!(
        tampered.is_empty() || tampered.starts_with("HTTP/1.1 200"),
        "{tampered}"
    );
    assert!(
        !tampered.ends_with("0\r\n\r\n"),
        "the body must not end cleanly: {tampered:?}"
    );
    assert!(!tampered.contains("TAMPERED-BYTES"), "{tampered:?}");
    let _ = stop.send(());
    server.await.unwrap().unwrap();
    assert_eq!(h.graph.remaining(), 0);
}

/// Meta, with a download body the test feeds chunk by chunk: the media
/// URL answers its scripted status and headers, then the chunks sent on
/// the channel as they come, and ends when the sender is dropped.
/// Everything else is the script.
#[derive(Debug)]
struct Fed {
    graph: meta_whatsapp_rs::core::testing::ScriptedTransport,
    body: std::sync::Mutex<Option<futures::channel::mpsc::UnboundedReceiver<Bytes>>>,
}

#[async_trait::async_trait]
impl HttpTransport for Fed {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.graph.send(request).await
    }

    async fn send_streaming(
        &self,
        request: HttpRequest,
    ) -> Result<StreamingResponse, TransportError> {
        let head = self.graph.send(request).await?;
        let chunks = self
            .body
            .lock()
            .unwrap()
            .take()
            .expect("one streamed download");
        Ok(StreamingResponse {
            status: head.status,
            headers: head.headers,
            body: Box::pin(chunks.map(Ok)),
        })
    }
}

/// The next data frame of `body` within `wait`: `None` when none came in
/// time, `Some(None)` at its end.
async fn frame_within(
    body: &mut meta_whatsapp_rs::webhooks::axum::body::Body,
    wait: std::time::Duration,
) -> Option<Option<Bytes>> {
    let frame = tokio::time::timeout(wait, body.frame()).await.ok()?;
    Some(frame.map(|frame| frame.unwrap().into_data().unwrap()))
}

/// A streamed download forwards the file while it arrives: with Meta's
/// body still open, every chunk but the last one received is already
/// out. A stream holds one chunk, not the file: 16 streams of 100 MiB
/// would otherwise hold 1.6 GiB on a replica. Decisive: the chunks
/// passed on before the body ends, one held back.
#[tokio::test]
async fn a_streamed_download_forwards_the_file_before_it_ends() {
    use std::time::Duration;

    let (meta, received) = futures::channel::mpsc::unbounded::<Bytes>();
    let h = Harness::with_transport(common::test_settings(), |graph| {
        std::sync::Arc::new(Fed {
            graph,
            body: std::sync::Mutex::new(Some(received)),
        })
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Media]).await;
    let chunks: Vec<Bytes> = (0u8..3).map(|i| Bytes::from(vec![i; 64 * 1024])).collect();
    let file = chunks.concat();
    h.graph
        .push_json(200, media_info(&file, &sha256_hex(&file), Some(file.len())));
    h.graph.push_bytes(200, "image/jpeg", Vec::new());
    let response = h
        .internal
        .clone()
        .oneshot(download(&key, "?stream=true").build())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    meta.unbounded_send(chunks[0].clone()).unwrap();
    meta.unbounded_send(chunks[1].clone()).unwrap();
    // Meta's body is still open: the first chunk is out, the second held.
    let first = frame_within(&mut body, Duration::from_secs(10))
        .await
        .expect("nothing forwarded before the end of the file");
    assert_eq!(first.as_ref(), Some(&chunks[0]));
    assert_eq!(
        frame_within(&mut body, Duration::from_millis(100)).await,
        None,
        "the last chunk received waits for the next one"
    );
    meta.unbounded_send(chunks[2].clone()).unwrap();
    let second = frame_within(&mut body, Duration::from_secs(10)).await;
    assert_eq!(second, Some(Some(chunks[1].clone())));
    // The end: the digest matches, the chunk held back goes out.
    drop(meta);
    let last = frame_within(&mut body, Duration::from_secs(10)).await;
    assert_eq!(last, Some(Some(chunks[2].clone())));
    assert_eq!(
        frame_within(&mut body, Duration::from_secs(10)).await,
        Some(None)
    );
    assert_eq!(h.graph.remaining(), 0);
}
