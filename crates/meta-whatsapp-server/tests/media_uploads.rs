//! Resumable uploads and conversion on upload (scope `media`;
//! docs/design/server.md, section 4.2; roadmap M5c1): Meta's Resumable
//! Upload API through the library (the three calls of its Graph guide, the
//! token in `Authorization: OAuth`), the session kept as the number's
//! alone, every bound checked before any request, and a converted file
//! uploaded in place of the one sent when the deployment configures a
//! converter (and refused as before when it does not). Design M5.1 and
//! M5.2 for each route: another tenant's number and another tenant's (or
//! number's) upload id are `404`, with nothing asked of Meta.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use common::{Call, Harness};
use meta_whatsapp_rs::Result;
use meta_whatsapp_rs::core::error::{ConversionError, TransportError};
use meta_whatsapp_rs::core::ids::AppId;
use meta_whatsapp_rs::core::media::{ConvertedMedia, MediaConverter, MediaInput, TargetFormat};
use meta_whatsapp_rs::core::testing::RecordedBody;
use meta_whatsapp_rs::webhooks::axum::body::{Body, Bytes};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
/// Another number of the same tenant's WABA.
const PN_SECOND: &str = "106540352242999";
const TOKEN: &str = "EAAG-merchant-token";
const OTHER_TENANT: &str = "merchant-43";
const OTHER_WABA: &str = "102290129340399";
const OTHER_PN: &str = "106540352242923";
const OTHER_TOKEN: &str = "EAAG-other-merchant-token";
const APP_ID: &str = "634974688087057";
/// The session id and handle shapes of the Graph guide
/// (`docs/graph-api/guides/upload`: `upload:<UPLOAD_SESSION_ID>`, and a
/// handle such as `2:c2FtcGxl...`).
const SESSION: &str = "upload:MTphdHRhY2htZW50OjZiMDI1ZWI3";
const HANDLE: &str = "2:c2FtcGxl...";
const MEDIA_ID: &str = "1037543291543636";

fn settings() -> Settings {
    Settings {
        app_id: Some(AppId::new(APP_ID).unwrap()),
        ..common::test_settings()
    }
}

async fn connected_with(settings: Settings) -> (Harness, String) {
    let h = Harness::with_settings(settings);
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN, PN_SECOND], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Media]).await;
    (h, key)
}

async fn connected() -> (Harness, String) {
    connected_with(settings()).await
}

fn start_call(pn: &str, key: &str, body: &Value) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{pn}/media-uploads"))
        .key(key)
        .json(body)
}

fn chunk_call(pn: &str, key: &str, upload_id: &str, query: &str, data: &[u8]) -> Call {
    Call::new(
        Method::POST,
        format!("/v1/numbers/{pn}/media-uploads/{upload_id}{query}"),
    )
    .key(key)
    .header("content-type", "application/octet-stream")
    .body(Body::from(data.to_vec()))
}

fn status_call(pn: &str, key: &str, upload_id: &str) -> Call {
    Call::get(format!("/v1/numbers/{pn}/media-uploads/{upload_id}")).key(key)
}

fn png_start(len: usize) -> Value {
    json!({"file_name": "offer.png", "file_length": len, "file_type": "image/png"})
}

/// Open a session for `len` bytes of PNG on `pn`; its upload id.
async fn open(h: &Harness, pn: &str, key: &str, session: &str, len: usize) -> String {
    h.graph.push_json(200, json!({"id": session}));
    let reply = h.call(start_call(pn, key, &png_start(len))).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    reply.json()["upload_id"].as_str().unwrap().to_owned()
}

// ─── The three calls ─────────────────────────────────────────────────────

/// The Graph guide's three steps, over HTTP: `POST /{app_id}/uploads` with
/// `file_name`, `file_length` and `file_type` in the query, then
/// `POST /upload:<session>` with `Authorization: OAuth <token>`, the
/// `file_offset` header and the bytes, then the status. The answer is the
/// service's id, never Meta's; the handle comes with the chunk that ends
/// the file and with the status after it (which asks Meta nothing).
#[tokio::test]
async fn a_resumable_upload_is_the_graph_guides_three_calls() {
    let (h, key) = connected().await;
    h.graph.push_json(200, json!({"id": SESSION}));
    let started = h
        .call(start_call(PN, &key, &png_start(12)).header("idempotency-key", "offer-1"))
        .await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.text);
    let body = started.json();
    let upload_id = body["upload_id"].as_str().unwrap().to_owned();
    assert!(
        upload_id.starts_with("up_") && upload_id.len() == 35,
        "{upload_id}"
    );
    assert_eq!(
        body,
        json!({"upload_id": upload_id, "file_name": "offer.png", "file_length": 12,
               "file_type": "image/png", "file_offset": 0, "complete": false})
    );
    assert!(
        !started.text.contains("MTphdHRh"),
        "Meta's session id stays in"
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{APP_ID}/uploads"));
    assert_eq!(request.query("file_name").as_deref(), Some("offer.png"));
    assert_eq!(request.query("file_length").as_deref(), Some("12"));
    assert_eq!(request.query("file_type").as_deref(), Some("image/png"));
    assert_eq!(
        request.query("access_token"),
        None,
        "the token stays out of the URL"
    );
    assert_eq!(
        request.header("authorization"),
        Some(format!("OAuth {TOKEN}").as_str())
    );

    // The whole file in one chunk.
    h.graph.push_json(200, json!({"h": HANDLE}));
    let chunk = h
        .call(chunk_call(
            PN,
            &key,
            &upload_id,
            "?file_offset=0",
            b"png bytes...",
        ))
        .await;
    assert_eq!(chunk.status, StatusCode::OK, "{}", chunk.text);
    assert_eq!(
        chunk.json(),
        json!({"upload_id": upload_id, "file_name": "offer.png", "file_length": 12,
               "file_type": "image/png", "file_offset": 12, "complete": true, "handle": HANDLE})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{SESSION}"));
    assert_eq!(
        request.header("authorization"),
        Some(format!("OAuth {TOKEN}").as_str())
    );
    assert_eq!(request.header("file_offset"), Some("0"));
    assert_eq!(
        request.body,
        RecordedBody::Bytes {
            content_type: "application/octet-stream".into(),
            data: Bytes::from_static(b"png bytes...")
        }
    );

    // The status after it comes from the record: the handle, no request.
    let asked = h.graph.requests().len();
    let status = h.call(status_call(PN, &key, &upload_id)).await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.text);
    assert_eq!(status.json(), chunk.json());
    assert_eq!(h.graph.requests().len(), asked, "nothing asked of Meta");

    // A repeat of the start, under its key: the kept answer, no session.
    let again = h
        .call(start_call(PN, &key, &png_start(12)).header("idempotency-key", "offer-1"))
        .await;
    assert_eq!(again.status, StatusCode::CREATED);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.json()["upload_id"], upload_id.as_str());
    assert_eq!(h.graph.requests().len(), asked);
    // A finished session takes no more chunks, and asks nothing.
    let more = h
        .call(chunk_call(PN, &key, &upload_id, "?file_offset=0", b"x"))
        .await;
    assert_eq!(
        (
            more.status,
            more.code().as_str(),
            more.json()["error"]["field"].as_str()
        ),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            Some("upload_id")
        )
    );
    assert_eq!(h.graph.requests().len(), asked);
    assert_eq!(h.graph.remaining(), 0);
}

/// An interrupted upload resumes from the offset Meta reports (the guide's
/// `file_offset` as a string), and a session id carrying a signature
/// (`?sig=`) keeps it in Meta's URL, never in the service's answer.
#[tokio::test]
async fn an_interrupted_upload_resumes_from_the_offset_meta_reports() {
    let (h, key) = connected().await;
    let upload_id = open(&h, PN, &key, "upload:abc?sig=SIGNATURE-7Q", 10).await;
    // Four bytes arrive; Meta has no handle yet.
    h.graph.push_json(200, json!({}));
    let first = h
        .call(chunk_call(PN, &key, &upload_id, "?file_offset=0", b"abcd"))
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    assert_eq!(first.json()["file_offset"], 4);
    assert_eq!(first.json()["complete"], false);
    assert!(first.json().get("handle").is_none());
    // The next one times out: it may have arrived.
    h.graph.push_error(|| TransportError::Timeout);
    let lost = h
        .call(chunk_call(PN, &key, &upload_id, "?file_offset=4", b"efgh"))
        .await;
    assert_eq!(
        (lost.status, lost.code().as_str()),
        (StatusCode::GATEWAY_TIMEOUT, "timeout"),
        "{}",
        lost.text
    );
    assert_eq!(lost.json()["error"]["may_have_been_sent"], true);
    // Meta's status says where to resume.
    h.graph
        .push_json(200, json!({"id": "upload:abc", "file_offset": "8"}));
    let status = h.call(status_call(PN, &key, &upload_id)).await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.text);
    assert_eq!(status.json()["file_offset"], 8);
    assert_eq!(status.json()["complete"], false);
    h.graph.push_json(200, json!({"h": HANDLE}));
    let finished = h
        .call(chunk_call(PN, &key, &upload_id, "?file_offset=8", b"ij"))
        .await;
    assert_eq!(finished.json()["complete"], true, "{}", finished.text);
    assert_eq!(finished.json()["handle"], HANDLE);

    let requests = h.graph.requests();
    assert_eq!(requests.len(), 5);
    for request in &requests[1..] {
        assert_eq!(request.path(), "/v25.0/upload:abc");
        assert_eq!(request.query("sig").as_deref(), Some("SIGNATURE-7Q"));
    }
    assert_eq!(requests[1].header("file_offset"), Some("0"));
    assert_eq!(requests[2].header("file_offset"), Some("4"));
    assert_eq!(requests[3].method, Method::GET);
    assert_eq!(requests[4].header("file_offset"), Some("8"));
    for answer in [&first.text, &lost.text, &status.text, &finished.text] {
        assert!(!answer.contains("SIGNATURE-7Q"), "{answer}");
    }
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Bounds, before any request ──────────────────────────────────────────

/// Every bound is checked before Meta is asked: the five file types, a
/// length from 1 byte to the kind's limit in Meta's media table (5 MiB for
/// an image, 16 MiB for a video, 100 MiB for a document), a chunk of a
/// byte at least and within the declared file. Decisive: each check in
/// `NewUpload::new` and `OpenUpload::max_chunk`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one table of refusals
async fn every_bound_is_checked_before_any_request() {
    let (h, key) = connected().await;
    let mib = 1024 * 1024;
    let refused = |reply: &common::Reply, status: StatusCode, code: &str, field: Option<&str>| {
        assert_eq!(
            (
                reply.status,
                reply.code(),
                reply.json()["error"]["field"].as_str().map(str::to_owned)
            ),
            (status, code.to_owned(), field.map(str::to_owned)),
            "{}",
            reply.text
        );
    };
    let invalid = |field: &'static str| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            Some(field),
        )
    };
    for (body, (status, code, field)) in [
        (
            json!({"file_name": "a.gif", "file_length": 5, "file_type": "image/gif"}),
            invalid("file_type"),
        ),
        (
            json!({"file_name": "a.wav", "file_length": 5, "file_type": "audio/wav"}),
            invalid("file_type"),
        ),
        (
            json!({"file_name": "a.png", "file_length": 0, "file_type": "image/png"}),
            invalid("file_length"),
        ),
        (
            json!({"file_name": "  ", "file_length": 5, "file_type": "image/png"}),
            invalid("file_name"),
        ),
        (
            json!({"file_name": "a.png", "file_length": 5 * mib + 1, "file_type": "image/png"}),
            (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large", None),
        ),
        (
            json!({"file_name": "a.jpg", "file_length": 5 * mib + 1, "file_type": "image/jpg"}),
            (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large", None),
        ),
        (
            json!({"file_name": "a.mp4", "file_length": 16 * mib + 1, "file_type": "video/mp4"}),
            (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large", None),
        ),
        (
            json!({"file_name": "a.pdf", "file_length": 100 * mib + 1, "file_type": "application/pdf"}),
            (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large", None),
        ),
        (
            json!({"file_name": "a.png", "file_length": -1, "file_type": "image/png"}),
            invalid("body"),
        ),
        (
            json!({"file_name": "a.png", "file_length": 5, "file_type": "image/png", "extra": 1}),
            invalid("body"),
        ),
        (json!({"file_name": "a.png"}), invalid("body")),
    ] {
        let reply = h.call(start_call(PN, &key, &body)).await;
        refused(&reply, status, code, field);
    }
    assert!(
        h.graph.requests().is_empty(),
        "Meta was asked: {:?}",
        h.graph.requests()
    );

    // The limits themselves are accepted.
    for (len, file_type) in [
        (5 * mib, "image/png"),
        (16 * mib, "video/mp4"),
        (100 * mib, "application/pdf"),
    ] {
        h.graph.push_json(200, json!({"id": SESSION}));
        let reply = h
            .call(start_call(
                PN,
                &key,
                &json!({"file_name": "a", "file_length": len, "file_type": file_type}),
            ))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::CREATED,
            "{file_type}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.remaining(), 0);

    // Chunks, against a 10-byte file.
    let upload_id = open(&h, PN, &key, SESSION, 10).await;
    let asked = h.graph.requests().len();
    for (query, data, (status, code, field)) in [
        ("", &b"abcd"[..], invalid("file_offset")),
        ("?file_offset=abc", b"abcd", invalid("file_offset")),
        ("?file_offset=-1", b"abcd", invalid("file_offset")),
        ("?file_offset=10", b"abcd", invalid("file_offset")),
        ("?file_offset=99", b"abcd", invalid("file_offset")),
        ("?file_offset=0", b"", invalid("body")),
        (
            "?file_offset=0",
            b"0123456789a",
            (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large", None),
        ),
        (
            "?file_offset=8",
            b"abc",
            (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large", None),
        ),
    ] {
        let reply = h.call(chunk_call(PN, &key, &upload_id, query, data)).await;
        refused(&reply, status, code, field);
    }
    for id in [
        "placeholder",
        "up_",
        "up_zz",
        &format!("up_{}", "a".repeat(31)),
        "upload:abc",
    ] {
        let reply = h
            .call(chunk_call(PN, &key, id, "?file_offset=0", b"x"))
            .await;
        refused(
            &reply,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            Some("upload_id"),
        );
    }
    // A well-formed id nobody holds.
    let unknown = format!("up_{}", "0123456789abcdef".repeat(2));
    for reply in [
        h.call(chunk_call(PN, &key, &unknown, "?file_offset=0", b"x"))
            .await,
        h.call(status_call(PN, &key, &unknown)).await,
    ] {
        refused(&reply, StatusCode::NOT_FOUND, "not_found", None);
    }
    assert_eq!(h.graph.requests().len(), asked, "Meta was asked");
    assert_eq!(h.graph.remaining(), 0);
}

/// `WA_SERVER_MEDIA_MAX_BYTES` caps a declared length below the kind's
/// limit, and a chunk is read no further than the declared file's end:
/// one more byte is `413`, and nothing reaches Meta.
#[tokio::test]
async fn the_deployments_cap_bounds_the_declared_length() {
    let (h, key) = connected_with(Settings {
        media_max_bytes: 1000,
        ..settings()
    })
    .await;
    let over = h
        .call(start_call(
            PN,
            &key,
            &json!({"file_name": "a.png", "file_length": 1001, "file_type": "image/png"}),
        ))
        .await;
    assert_eq!(
        (over.status, over.code().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large")
    );
    assert!(h.graph.requests().is_empty());
    let upload_id = open(&h, PN, &key, SESSION, 1000).await;
    let asked = h.graph.requests().len();
    let big = h
        .call(chunk_call(
            PN,
            &key,
            &upload_id,
            "?file_offset=0",
            &vec![7u8; 1001],
        ))
        .await;
    assert_eq!(
        (big.status, big.code().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large")
    );
    assert_eq!(h.graph.requests().len(), asked);
}

/// Without `WA_APP_ID` no session is opened (the deployment's to set):
/// `409 resumable_upload_not_configured`, with nothing asked of Meta.
#[tokio::test]
async fn without_an_app_id_the_routes_say_so_and_ask_nothing() {
    let (h, key) = connected_with(common::test_settings()).await;
    let reply = h.call(start_call(PN, &key, &png_start(12))).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "resumable_upload_not_configured"),
        "{}",
        reply.text
    );
    assert!(h.graph.requests().is_empty());
}

// ─── Authorization (design M5.1 and M5.2) ────────────────────────────────

/// Tenant B's key on tenant A's number is `404`, with zero requests, for
/// each route; its own number with A's upload id is `404` too (the
/// session is the number's, M5.2), and so is A's other number with it. No
/// answer holds A's id or Meta's. Decisive: the record's tenant and number
/// checks in `Uploads::open`.
#[tokio::test]
#[allow(clippy::vec_init_then_push)] // each push is commented by whose call it is
async fn another_tenants_number_and_upload_are_not_found_with_no_request() {
    let (h, key) = connected().await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], OTHER_TOKEN)
        .await;
    let other_key = h.tenant_key(OTHER_TENANT, &[Scope::Media]).await;
    let upload_id = open(&h, PN, &key, SESSION, 12).await;
    let asked = h.graph.requests().len();
    let reads = |h: &Harness| h.graph.requests().len();

    let mut calls = Vec::new();
    // B's key on A's number.
    calls.push(start_call(PN, &other_key, &png_start(12)));
    calls.push(chunk_call(
        PN,
        &other_key,
        &upload_id,
        "?file_offset=0",
        b"png bytes...",
    ));
    calls.push(status_call(PN, &other_key, &upload_id));
    // B's key, B's number, A's upload.
    calls.push(chunk_call(
        OTHER_PN,
        &other_key,
        &upload_id,
        "?file_offset=0",
        b"png bytes...",
    ));
    calls.push(status_call(OTHER_PN, &other_key, &upload_id));
    // A's key, A's other number, A's upload.
    calls.push(chunk_call(
        PN_SECOND,
        &key,
        &upload_id,
        "?file_offset=0",
        b"png bytes...",
    ));
    calls.push(status_call(PN_SECOND, &key, &upload_id));
    for (n, call) in calls.into_iter().enumerate() {
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "call {n}: {}",
            reply.text
        );
        assert!(!reply.text.contains(&upload_id), "call {n}: {}", reply.text);
    }
    assert_eq!(
        reads(&h),
        asked,
        "Meta was asked: {:?}",
        &h.graph.requests()[asked..]
    );

    // The owner still can.
    h.graph.push_json(200, json!({"h": HANDLE}));
    let done = h
        .call(chunk_call(
            PN,
            &key,
            &upload_id,
            "?file_offset=0",
            b"png bytes...",
        ))
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// A session's record expires 24 hours from the session's start, and the
/// chunk that keeps the handle does not extend it (the store's expiry then
/// reads the id as `404`: its conformance suite's `born_expired_is_invisible`
/// and the `CAS` of a dead record). The harness's store runs on the real
/// clock, so the deadline itself is what is pinned here. Decisive:
/// `SESSION_TTL` in `Uploads::start`, `Expiry::Keep` in `keep_handle`.
#[tokio::test]
async fn a_sessions_record_expires_a_day_from_its_start_and_the_handle_keeps_that() {
    use meta_whatsapp_rs::core::store::JsonStore;
    let (h, key) = connected().await;
    let store = JsonStore::<Value>::new(h.kv.clone(), "wa.server.upload");
    let before = time::OffsetDateTime::now_utc();
    let upload_id = open(&h, PN, &key, SESSION, 12).await;
    let (record, _, expires) = store.get(&upload_id).await.unwrap().unwrap();
    let expires = expires.expect("a session's record expires");
    let day = time::Duration::hours(24);
    assert!(
        expires >= before + day && expires <= time::OffsetDateTime::now_utc() + day,
        "{expires}"
    );
    assert_eq!(record["tenant"], TENANT);
    assert_eq!(record["phone_number_id"], PN);
    // Meta's session id is in the record alone.
    h.clock.advance(Duration::from_secs(3600));
    h.graph.push_json(200, json!({"h": HANDLE}));
    let done = h
        .call(chunk_call(
            PN,
            &key,
            &upload_id,
            "?file_offset=0",
            b"png bytes...",
        ))
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);
    let (kept, _, expires_after) = store.get(&upload_id).await.unwrap().unwrap();
    assert_eq!(kept["handle"], HANDLE);
    assert_eq!(expires_after, Some(expires));
}

/// A chunk holds one of its tenant's transfer slots: with the share busy
/// it is `429` before its body is read or Meta asked, and a chunk that
/// fails (Meta refuses it) gives its slot back. Decisive: `transfer_slot`
/// in the route; the slot's drop on the error path.
#[tokio::test]
async fn a_chunk_holds_a_slot_and_gives_it_back_on_every_path() {
    use meta_whatsapp_server::model::TenantId;
    let (h, key) = connected_with(Settings {
        media_concurrency: 1,
        ..settings()
    })
    .await;
    let upload_id = open(&h, PN, &key, SESSION, 12).await;
    let asked = h.graph.requests().len();
    let held = h
        .state
        .media_slots()
        .try_acquire(&TenantId::parse(TENANT).unwrap())
        .unwrap();
    let busy = h
        .call(chunk_call(
            PN,
            &key,
            &upload_id,
            "?file_offset=0",
            b"png bytes...",
        ))
        .await;
    assert_eq!(
        (busy.status, busy.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
        "{}",
        busy.text
    );
    assert_eq!(h.graph.requests().len(), asked);
    drop(held);
    // Refused by this service (past the declared file) after the slot
    // is taken, then refused by Meta: neither keeps it.
    let over = h
        .call(chunk_call(
            PN,
            &key,
            &upload_id,
            "?file_offset=0",
            &[7u8; 13],
        ))
        .await;
    assert_eq!(over.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", over.text);
    h.graph.push_json(
        400,
        json!({"error": {"message": "Invalid offset", "type": "OAuthException", "code": 100}}),
    );
    let refused = h
        .call(chunk_call(
            PN,
            &key,
            &upload_id,
            "?file_offset=0",
            b"png bytes...",
        ))
        .await;
    assert!(refused.status.is_client_error(), "{}", refused.text);
    h.graph.push_json(200, json!({"h": HANDLE}));
    let ok = h
        .call(chunk_call(
            PN,
            &key,
            &upload_id,
            "?file_offset=0",
            b"png bytes...",
        ))
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// A number that comes under another tenant (its WABA disconnected, then
/// connected by the other) does not hand over the sessions the first
/// tenant opened on it: the record names the tenant as well as the
/// number. Decisive: the tenant comparison in `Uploads::open`.
#[tokio::test]
async fn a_number_that_changes_tenant_does_not_hand_over_its_sessions() {
    let (h, key) = connected().await;
    h.tenant(OTHER_TENANT).await;
    let upload_id = open(&h, PN, &key, SESSION, 12).await;
    let waba = meta_whatsapp_rs::core::ids::WabaId::new(WABA).unwrap();
    h.store.unbind_waba(&waba).await.unwrap();
    h.connect(OTHER_TENANT, OTHER_WABA, &[PN], OTHER_TOKEN)
        .await;
    let other_key = h.tenant_key(OTHER_TENANT, &[Scope::Media]).await;
    let asked = h.graph.requests().len();
    for call in [
        status_call(PN, &other_key, &upload_id),
        chunk_call(
            PN,
            &other_key,
            &upload_id,
            "?file_offset=0",
            b"png bytes...",
        ),
    ] {
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), asked);
}

/// A key without the `media` scope is `403 forbidden` on each route, and
/// none without a key `401`: nothing is asked of Meta.
#[tokio::test]
async fn the_media_scope_guards_each_route() {
    let (h, key) = connected().await;
    let upload_id = open(&h, PN, &key, SESSION, 12).await;
    let asked = h.graph.requests().len();
    let send_key = h.tenant_key(TENANT, &[Scope::Send]).await;
    for call in [
        start_call(PN, &send_key, &png_start(12)),
        chunk_call(PN, &send_key, &upload_id, "?file_offset=0", b"png bytes..."),
        status_call(PN, &send_key, &upload_id),
    ] {
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{}",
            reply.text
        );
    }
    for call in [
        Call::new(Method::POST, format!("/v1/numbers/{PN}/media-uploads")),
        Call::get(format!("/v1/numbers/{PN}/media-uploads/{upload_id}")),
        Call::new(
            Method::POST,
            format!("/v1/numbers/{PN}/media-uploads/{upload_id}?file_offset=0"),
        ),
    ] {
        let reply = h.call(call).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.text);
    }
    assert_eq!(h.graph.requests().len(), asked);
}

// ─── Meta's answers ──────────────────────────────────────────────────────

/// Meta's refusals map as on every route: an expired token (190) is `409
/// reconnect_required` (the number is marked), a parameter it refuses is
/// `422 invalid_parameter` with its code; a session id that is none of
/// `upload:<id>` is `502 upstream` and keeps no record; a completed
/// session answered without a handle is `502 upstream`.
#[tokio::test]
async fn meta_s_answers_are_mapped_as_everywhere_else() {
    let (h, key) = connected().await;
    h.graph.push_json(
        400,
        json!({"error": {"message": "(#100) Invalid parameter", "type": "OAuthException",
                         "code": 100, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h.call(start_call(PN, &key, &png_start(12))).await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_parameter"),
        "{}",
        refused.text
    );
    assert_eq!(refused.json()["error"]["graph"]["code"], 100);

    h.graph.push_json(200, json!({"id": "123456789"}));
    let odd = h.call(start_call(PN, &key, &png_start(12))).await;
    assert_eq!(
        (odd.status, odd.code().as_str()),
        (StatusCode::BAD_GATEWAY, "upstream")
    );

    let upload_id = open(&h, PN, &key, SESSION, 12).await;
    h.graph.push_json(200, json!({}));
    let no_handle = h
        .call(chunk_call(
            PN,
            &key,
            &upload_id,
            "?file_offset=0",
            b"png bytes...",
        ))
        .await;
    assert_eq!(
        (no_handle.status, no_handle.code().as_str()),
        (StatusCode::BAD_GATEWAY, "upstream")
    );

    h.graph.push_json(
        401,
        json!({"error": {"message": "Error validating access token", "type": "OAuthException",
                         "code": 190, "fbtrace_id": "A"}}),
    );
    let expired = h.call(status_call(PN, &key, &upload_id)).await;
    assert_eq!(
        (expired.status, expired.code().as_str()),
        (StatusCode::CONFLICT, "reconnect_required"),
        "{}",
        expired.text
    );
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Conversion ──────────────────────────────────────────────────────────

/// A converter that answers a fixed result and counts its runs.
#[derive(Debug)]
struct Scripted {
    result: std::sync::Mutex<Option<Result<ConvertedMedia>>>,
    runs: AtomicUsize,
    seen: std::sync::Mutex<Vec<(Bytes, TargetFormat)>>,
}

impl Scripted {
    fn new(result: Result<ConvertedMedia>) -> Arc<Self> {
        Arc::new(Self {
            result: std::sync::Mutex::new(Some(result)),
            runs: AtomicUsize::new(0),
            seen: std::sync::Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl MediaConverter for Scripted {
    async fn convert(&self, input: MediaInput, target: TargetFormat) -> Result<ConvertedMedia> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push((input.data.clone(), target));
        self.result
            .lock()
            .unwrap()
            .take()
            .expect("the converter ran more often than scripted")
    }
}

fn upload_call(key: &str, parts: &[(&str, Option<&str>, &[u8])]) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{PN}/media"))
        .key(key)
        .multipart(parts)
}

const WAV: &[u8] = b"RIFF$\x00\x00\x00WAVEfmt a recording";
const OGG: &[u8] = b"OggS\x00\x02 converted to opus";

/// With a converter configured, the converted bytes are what Meta gets:
/// the form's `convert` names the format, the claimed `type` is ignored,
/// the upload is the format's MIME type, named after the file with the
/// format's extension. The converter saw the source bytes and the target
/// only.
#[tokio::test]
async fn with_a_converter_the_converted_bytes_are_what_is_uploaded() {
    let converter = Scripted::new(Ok(ConvertedMedia::new(
        Bytes::from_static(OGG),
        TargetFormat::OggOpus,
    )));
    let (h, key) = connected_with(Settings {
        media_converter: Some(converter.clone()),
        ..settings()
    })
    .await;
    h.graph.push_json(200, json!({"id": MEDIA_ID}));
    let reply = h
        .call(upload_call(
            &key,
            &[
                ("type", None, b"audio/wav"),
                ("convert", None, b"ogg_opus"),
                ("file", Some("../my voice (1).wav"), WAV),
            ],
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(reply.json(), json!({"media_id": MEDIA_ID}));
    assert_eq!(
        *converter.seen.lock().unwrap(),
        [(Bytes::from_static(WAV), TargetFormat::OggOpus)]
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{PN}/media"));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(
        &request.multipart_field("type").unwrap().2[..],
        b"audio/ogg"
    );
    let (filename, content_type, data) = request.multipart_field("file").unwrap();
    assert_eq!(filename, Some(".._my_voice__1_.ogg"));
    assert_eq!(content_type, Some("audio/ogg"));
    assert_eq!(&data[..], OGG, "the converted bytes, not the source");
    assert_eq!(h.graph.remaining(), 0);
}

/// A converted upload under an `Idempotency-Key` converts once: the key's
/// input is the source file, the conversion runs after the claim, and a
/// repeat gets the kept answer.
#[tokio::test]
async fn a_converted_upload_under_a_key_converts_once() {
    let converter = Scripted::new(Ok(ConvertedMedia::new(
        Bytes::from_static(OGG),
        TargetFormat::OggOpus,
    )));
    let (h, key) = connected_with(Settings {
        media_converter: Some(converter.clone()),
        ..settings()
    })
    .await;
    h.graph.push_json(200, json!({"id": MEDIA_ID}));
    let call = || {
        upload_call(
            &key,
            &[("convert", None, b"ogg_opus"), ("file", Some("v.wav"), WAV)],
        )
        .header("idempotency-key", "voice-1")
    };
    let first = h.call(call()).await;
    let again = h.call(call()).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.text, first.text);
    assert_eq!(converter.runs.load(Ordering::SeqCst), 1);
    // Another target under the same key is another request.
    let other = h
        .call(
            upload_call(
                &key,
                &[("convert", None, b"jpeg"), ("file", Some("v.wav"), WAV)],
            )
            .header("idempotency-key", "voice-1"),
        )
        .await;
    assert_eq!(other.code(), "idempotency_key_reused");
    assert_eq!(h.graph.requests().len(), 1);
    assert_eq!(h.graph.remaining(), 0);
}

/// Without a converter configured an upload that needs conversion is
/// refused as it was before roadmap M5c1: the `type` Meta does not take is
/// `422` on `type`, and `convert` is a field the form does not have
/// (`422` on `body`), each with zero requests. Decisive: the form's
/// allowed fields when no converter is configured.
#[tokio::test]
async fn without_a_converter_an_upload_that_needs_one_is_refused_as_before() {
    let (h, key) = connected().await;
    let wav = h
        .call(upload_call(
            &key,
            &[("type", None, b"audio/wav"), ("file", Some("v.wav"), WAV)],
        ))
        .await;
    assert_eq!(
        (
            wav.status,
            wav.code().as_str(),
            wav.json()["error"]["field"].as_str()
        ),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            Some("type")
        ),
        "{}",
        wav.text
    );
    let convert = h
        .call(upload_call(
            &key,
            &[
                ("convert", None, b"ogg_opus"),
                ("type", None, b"audio/wav"),
                ("file", Some("v.wav"), WAV),
            ],
        ))
        .await;
    assert_eq!(
        (
            convert.status,
            convert.code().as_str(),
            convert.json()["error"]["field"].as_str()
        ),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            Some("body")
        ),
        "{}",
        convert.text
    );
    // `type` first: refused at once, as it always was.
    let typed_first = h
        .call(upload_call(
            &key,
            &[
                ("type", None, b"audio/wav"),
                ("convert", None, b"ogg_opus"),
                ("file", Some("v.wav"), WAV),
            ],
        ))
        .await;
    assert_eq!(
        typed_first.json()["error"]["field"],
        "type",
        "{}",
        typed_first.text
    );
    // Without `type` either: the same refusal as ever.
    let bare = h
        .call(upload_call(
            &key,
            &[("convert", None, b"ogg_opus"), ("file", Some("v.wav"), WAV)],
        ))
        .await;
    assert_eq!(bare.json()["error"]["field"], "body");
    // An image over the limit is still `413`, never converted.
    let big = vec![7u8; 5 * 1024 * 1024 + 1];
    let large = h
        .call(upload_call(
            &key,
            &[("type", None, b"image/png"), ("file", Some("a.png"), &big)],
        ))
        .await;
    assert_eq!(
        (large.status, large.code().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large")
    );
    assert!(h.graph.requests().is_empty(), "{:?}", h.graph.requests());
}

/// What a converter that fails sends: nothing. An input it refuses is `422`
/// on `file`; a converter that is missing, past its time, answering the
/// wrong format or killed is `502 media_conversion_failed` (retryable after
/// a timeout only), and a result past its format's limit `413`. The key a
/// failed conversion held is released, so the same request may run again.
/// Decisive: `ServiceError`'s classification of `Error::Conversion`.
#[tokio::test]
#[allow(clippy::too_many_lines, clippy::type_complexity)] // one table of cases
async fn a_failed_conversion_sends_nothing_and_releases_its_key() {
    let cases: Vec<(
        &str,
        Result<ConvertedMedia>,
        StatusCode,
        &str,
        Option<&str>,
        bool,
    )> = vec![
        (
            "input rejected",
            Err(ConversionError::InputRejected("exit status 1".into()).into()),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            Some("file"),
            false,
        ),
        (
            "input too large",
            Err(meta_whatsapp_rs::core::error::ValidationError::new("input", "too large").into()),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            Some("file"),
            false,
        ),
        (
            "timeout",
            Err(ConversionError::Timeout(Duration::from_secs(20)).into()),
            StatusCode::BAD_GATEWAY,
            "media_conversion_failed",
            None,
            true,
        ),
        (
            "missing",
            Err(ConversionError::BinaryMissing("ffmpeg".into()).into()),
            StatusCode::BAD_GATEWAY,
            "media_conversion_failed",
            None,
            false,
        ),
        (
            "killed",
            Err(ConversionError::Killed("signal 9".into()).into()),
            StatusCode::BAD_GATEWAY,
            "media_conversion_failed",
            None,
            false,
        ),
        (
            "another format",
            Ok(ConvertedMedia::new(
                Bytes::from_static(b"RIFFWEBP"),
                TargetFormat::StaticSticker,
            )),
            StatusCode::BAD_GATEWAY,
            "media_conversion_failed",
            None,
            false,
        ),
        (
            "too large",
            Ok(ConvertedMedia::new(
                vec![0u8; 100 * 1024 * 1024 + 1],
                TargetFormat::OggOpus,
            )),
            StatusCode::PAYLOAD_TOO_LARGE,
            "media_too_large",
            None,
            false,
        ),
    ];
    for (name, result, status, code, field, retryable) in cases {
        let converter = Scripted::new(result);
        let (h, key) = connected_with(Settings {
            media_converter: Some(converter.clone()),
            ..settings()
        })
        .await;
        let call = || {
            upload_call(
                &key,
                &[("convert", None, b"ogg_opus"), ("file", Some("v.wav"), WAV)],
            )
            .header("idempotency-key", "voice-1")
        };
        let reply = h.call(call()).await;
        assert_eq!(
            (
                reply.status,
                reply.code(),
                reply.json()["error"]["field"].as_str().map(str::to_owned)
            ),
            (status, code.to_owned(), field.map(str::to_owned)),
            "{name}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["may_have_been_sent"], false, "{name}");
        assert_eq!(reply.json()["error"]["retryable"], retryable, "{name}");
        for private in ["exit status", "signal 9", "ffmpeg"] {
            assert!(!reply.text.contains(private), "{name}: {}", reply.text);
        }
        assert!(
            h.graph.requests().is_empty(),
            "{name}: {:?}",
            h.graph.requests()
        );
        assert_eq!(converter.runs.load(Ordering::SeqCst), 1, "{name}");
    }
}

/// The key of a failed conversion is released: a repeat runs the converter
/// again, and a good result uploads.
#[tokio::test]
async fn a_released_key_converts_again() {
    struct Flaky(AtomicUsize);
    impl std::fmt::Debug for Flaky {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Flaky")
        }
    }
    #[async_trait]
    impl MediaConverter for Flaky {
        async fn convert(&self, _: MediaInput, target: TargetFormat) -> Result<ConvertedMedia> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(ConversionError::Timeout(Duration::from_secs(20)).into())
            } else {
                Ok(ConvertedMedia::new(Bytes::from_static(OGG), target))
            }
        }
    }
    let converter = Arc::new(Flaky(AtomicUsize::new(0)));
    let (h, key) = connected_with(Settings {
        media_converter: Some(converter.clone()),
        ..settings()
    })
    .await;
    let call = || {
        upload_call(
            &key,
            &[("convert", None, b"ogg_opus"), ("file", Some("v.wav"), WAV)],
        )
        .header("idempotency-key", "voice-1")
    };
    assert_eq!(h.call(call()).await.code(), "media_conversion_failed");
    assert!(h.graph.requests().is_empty());
    h.graph.push_json(200, json!({"id": MEDIA_ID}));
    let second = h.call(call()).await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.text);
    assert_eq!(converter.0.load(Ordering::SeqCst), 2);
    assert_eq!(h.graph.remaining(), 0);
}

/// The form with a converter: `convert` names one of the five formats,
/// once; without it `type` is checked as ever (and an unsupported one is
/// `422` on `type`); no file is `422` on `file`. Nothing reaches the
/// converter or Meta.
#[tokio::test]
async fn the_form_with_a_converter_is_checked_before_anything_runs() {
    let converter = Scripted::new(Ok(ConvertedMedia::new(
        Bytes::from_static(OGG),
        TargetFormat::OggOpus,
    )));
    let (h, key) = connected_with(Settings {
        media_converter: Some(converter.clone()),
        ..settings()
    })
    .await;
    for (parts, field) in [
        (
            vec![("convert", None, &b"mp3"[..]), ("file", Some("v.wav"), WAV)],
            "convert",
        ),
        (
            vec![("convert", None, &b""[..]), ("file", Some("v.wav"), WAV)],
            "convert",
        ),
        (
            vec![
                ("convert", None, &b"jpeg"[..]),
                ("convert", None, &b"png"[..]),
                ("file", Some("v.wav"), WAV),
            ],
            "convert",
        ),
        (vec![("convert", None, &b"ogg_opus"[..])], "file"),
        (
            vec![
                ("type", None, &b"audio/wav"[..]),
                ("file", Some("v.wav"), WAV),
            ],
            "type",
        ),
        (vec![("file", Some("v.wav"), WAV)], "type"),
        (
            vec![
                ("convert", None, &[b'x'; 65][..]),
                ("file", Some("v.wav"), WAV),
            ],
            "convert",
        ),
    ] {
        let reply = h.call(upload_call(&key, &parts)).await;
        assert_eq!(
            (
                reply.status,
                reply.code().as_str(),
                reply.json()["error"]["field"].as_str()
            ),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                Some(field)
            ),
            "{}",
            reply.text
        );
    }
    assert_eq!(converter.runs.load(Ordering::SeqCst), 0);
    assert!(h.graph.requests().is_empty());
    // A supported `type` and no `convert` uploads as ever, unconverted.
    h.graph.push_json(200, json!({"id": MEDIA_ID}));
    let png = h
        .call(upload_call(
            &key,
            &[
                ("type", None, b"image/png"),
                ("file", Some("a.png"), b"\x89PNG\r\n\x1a\n-"),
            ],
        ))
        .await;
    assert_eq!(png.status, StatusCode::CREATED, "{}", png.text);
    assert_eq!(converter.runs.load(Ordering::SeqCst), 0);
}

/// A converted upload is the number's like any: another tenant's key on
/// the number is `404` before the converter runs.
#[tokio::test]
async fn another_tenants_key_never_reaches_the_converter() {
    let converter = Scripted::new(Ok(ConvertedMedia::new(
        Bytes::from_static(OGG),
        TargetFormat::OggOpus,
    )));
    let (h, _key) = connected_with(Settings {
        media_converter: Some(converter.clone()),
        ..settings()
    })
    .await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], OTHER_TOKEN)
        .await;
    let other_key = h.tenant_key(OTHER_TENANT, &[Scope::Media]).await;
    let reply = h
        .call(upload_call(
            &other_key,
            &[("convert", None, b"ogg_opus"), ("file", Some("v.wav"), WAV)],
        ))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );
    assert_eq!(converter.runs.load(Ordering::SeqCst), 0);
    assert!(h.graph.requests().is_empty());
}

// ─── A real ffmpeg, under the library's guards ───────────────────────────

/// The deployment's converter as `serve` builds it
/// (`WA_SERVER_MEDIA_CONVERTER=ffmpeg`), against a real `ffmpeg`: a PNG
/// becomes the JPEG Meta is sent, and bytes that are no media at all are
/// refused by the library's guards (`422` on `file`) with nothing sent.
/// Skips, printing why, without an `ffmpeg` that starts, unless
/// `META_WHATSAPP_RS_REQUIRE_FFMPEG=1` (CI sets it; as the library's
/// `tests/ffmpeg.rs`).
#[cfg(feature = "ffmpeg")]
#[tokio::test]
async fn a_real_ffmpeg_converts_an_image_and_refuses_what_is_no_media() {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let program =
        std::env::var_os("META_WHATSAPP_RS_TEST_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
    let png = Command::new(&program)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=32x32",
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-c:v",
            "png",
            "-",
        ])
        .stdin(Stdio::null())
        .output();
    let png = match png {
        Ok(out) if out.status.success() && !out.stdout.is_empty() => out.stdout,
        other => {
            assert!(
                !std::env::var("META_WHATSAPP_RS_REQUIRE_FFMPEG").is_ok_and(|v| v == "1"),
                "no usable ffmpeg: {other:?}"
            );
            let _ = std::io::stderr().write_all(b"skipped: no usable ffmpeg\n");
            return;
        }
    };
    // As `config.rs` builds it from `WA_SERVER_MEDIA_CONVERTER=ffmpeg`.
    let converter: Arc<dyn MediaConverter> = Arc::new(
        meta_whatsapp_rs::adapters::media::FfmpegConverter::new()
            .with_program(&program)
            .with_timeout(Duration::from_secs(20)),
    );
    let (h, key) = connected_with(Settings {
        media_converter: Some(converter),
        ..settings()
    })
    .await;
    h.graph.push_json(200, json!({"id": MEDIA_ID}));
    let reply = h
        .call(upload_call(
            &key,
            &[
                ("convert", None, b"jpeg"),
                ("file", Some("photo.png"), &png),
            ],
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(
        &request.multipart_field("type").unwrap().2[..],
        b"image/jpeg"
    );
    let (filename, _, data) = request.multipart_field("file").unwrap();
    assert_eq!(filename, Some("photo.jpeg"));
    assert_eq!(&data[..3], b"\xff\xd8\xff", "a JPEG, not the PNG sent");

    let asked = h.graph.requests().len();
    for hostile in [
        &b"not media at all"[..],
        b"ffconcat version 1.0\nfile /etc/passwd\n",
        b"#EXTM3U\n#EXT-X-VERSION:3\nhttp://169.254.169.254/latest/meta-data/\n",
    ] {
        let refused = h
            .call(upload_call(
                &key,
                &[("convert", None, b"jpeg"), ("file", Some("x.png"), hostile)],
            ))
            .await;
        assert_eq!(
            (
                refused.status,
                refused.code().as_str(),
                refused.json()["error"]["field"].as_str()
            ),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                Some("file")
            ),
            "{}",
            refused.text
        );
    }
    assert_eq!(
        h.graph.requests().len(),
        asked,
        "nothing sent for hostile input"
    );
}
