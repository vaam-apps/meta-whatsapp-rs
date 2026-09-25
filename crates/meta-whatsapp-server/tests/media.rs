//! Media (scope `media`; docs/design/server.md, section 4.2): uploads
//! checked (type, size) before any request, downloads verified against
//! Meta's SHA-256 before the first byte (or aborted mid-stream with
//! `stream=true`), deletes.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use base64::Engine as _;
use common::capture::sha256_hex;
use common::{Call, Harness};
use http_body_util::BodyExt;
use meta_whatsapp_rs::core::testing::RecordedBody;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
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
    assert!(
        response.into_body().collect().await.is_err(),
        "the body must not complete"
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

/// Every media slot of the replica busy: `429`, before any request.
#[tokio::test]
async fn busy_media_slots_are_429() {
    let (h, key) = connected_with(Settings {
        media_concurrency: 1,
        ..common::test_settings()
    })
    .await;
    let held = h.state.media_permits().clone().try_acquire_owned().unwrap();
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

/// `DELETE /{media-id}` with the merchant's token.
#[tokio::test]
async fn a_delete_is_metas_request() {
    let (h, key) = connected().await;
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
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(h.graph.remaining(), 0);
}
