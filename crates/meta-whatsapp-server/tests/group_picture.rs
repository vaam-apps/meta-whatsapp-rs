//! Roadmap M5f2: a group's picture through the service
//! (`PUT /v1/numbers/{pn}/groups/{group_id}/picture`; scope `groups`;
//! docs/design/server.md, section 4.2).
//!
//! The request is Meta's (`groups/reference`, "Update group settings": the
//! media upload's request structure with `messaging_product` and `file`).
//! The design's M5.2 is the decisive part: **a group the number does not
//! hold is `404` with zero vault reads, nothing sent, and its body never
//! read**; and a file that is not a JPEG of at most 5 MiB, square and at
//! least 192 by 192 pixels is refused with no request to Meta, the body
//! read no further than the bound. M5.1 over the document is
//! `tests/authz.rs`; the logs and the audit event are `tests/logs.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{Call, Harness, jpeg};
use futures::StreamExt as _;
use meta_whatsapp_rs::webhooks::axum::body::{Body, Bytes};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{Scope, TenantId};
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::json;

const TENANT: &str = "merchant-42";
const OTHER_TENANT: &str = "merchant-43";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const PN_2: &str = "106540352242923";
const OTHER_WABA: &str = "102290129340399";
const OTHER_PN: &str = "106540352242924";
const TOKEN: &str = "EAAG-merchant-token";
const OTHER_TOKEN: &str = "EAAG-other-token";

/// `groups/webhooks`, "Group create succeed": its group.
const GROUP: &str = "Y2FwaV9ncm91cDoxOTUwNTU1MDA3OToxMjAzNjMzOTQzMjAdOTY0MTUZD";

async fn connected_with(settings: Settings) -> (Harness, String) {
    let h = Harness::with_settings(settings);
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN, PN_2], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], OTHER_TOKEN)
        .await;
    let key = h.tenant_key(TENANT, &[Scope::Groups]).await;
    (h, key)
}

/// Two tenants (the first with two numbers), the first tenant's key with
/// the scope `groups`, and `GROUP` the first number's (Meta's creation
/// event).
async fn holding() -> (Harness, String) {
    let (h, key) = connected_with(common::test_settings()).await;
    hold(&h, WABA, PN).await;
    (h, key)
}

async fn hold(h: &Harness, waba: &str, pn: &str) {
    let reply = h
        .webhook(&common::meta::group_created(waba, pn, GROUP))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
}

fn path() -> String {
    format!("/v1/numbers/{PN}/groups/{GROUP}/picture")
}

fn put(key: &str, file: &[u8]) -> Call {
    Call::new(Method::PUT, path())
        .key(key)
        .multipart(&[("file", Some("group.jpg"), file)])
}

/// groups/reference, "Update group settings": a picture is the upload
/// endpoint's request on the group, with the number's WABA token. The
/// form is `messaging_product` and `file` (a JPEG, named and typed as the
/// library sends it, whatever the caller's part said); the answer is `204`
/// (Meta's `group_settings_update` event tells the outcome). Decisive:
/// `Groups::set_picture` and the route's body.
#[tokio::test]
async fn a_picture_is_metas_multipart_request() {
    let (h, key) = holding().await;
    let picture = jpeg(512, 512, b"scan");
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(put(&key, &picture)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let requests = h.graph.requests();
    let [sent] = &requests[..] else {
        panic!("one request: {requests:?}")
    };
    assert_eq!(sent.method, Method::POST);
    assert_eq!(sent.path(), format!("/v25.0/{GROUP}"));
    assert_eq!(sent.bearer(), Some(TOKEN));
    assert_eq!(
        &sent.multipart_field("messaging_product").unwrap().2[..],
        b"whatsapp"
    );
    let (filename, content_type, data) = sent.multipart_field("file").unwrap();
    assert_eq!(filename, Some("group-picture.jpg"));
    assert_eq!(content_type, Some("image/jpeg"));
    assert_eq!(&data[..], &picture[..]);
    assert_eq!(h.graph.remaining(), 0);
}

/// A file the service can see is not what Meta takes is refused with `422`
/// on `file` and **no request**: a PNG named `.jpg` (the bytes decide, not
/// the name or the part's type), an empty file, a JPEG that is not square,
/// one under 192 pixels, one cut inside its headers, one with no frame
/// header before its scan, and one whose frame has no side. Exactly 192 by
/// 192 is accepted. Decisive: `Picture::new`, each clause.
#[tokio::test]
async fn a_file_that_is_not_a_square_jpeg_of_192_pixels_is_refused_with_no_request() {
    let (h, key) = holding().await;
    let mut no_frame = vec![0xFF, 0xD8, 0xFF, 0xDA, 0, 4, 0, 0];
    no_frame.extend([0xFF, 0xD9]);
    let mut cut = jpeg(512, 512, b"");
    cut.truncate(24); // inside the frame header, before its sides
    let refused: [(&str, Vec<u8>); 8] = [
        ("a PNG", common::SAMPLE_PNG.to_vec()),
        ("empty", Vec::new()),
        ("wider than tall", jpeg(512, 256, b"")),
        ("taller than wide", jpeg(256, 512, b"")),
        ("191 pixels", jpeg(191, 191, b"")),
        ("cut in its headers", cut),
        ("a scan before a frame", no_frame),
        ("no sides", jpeg(0, 0, b"")),
    ];
    for (label, file) in refused {
        let reply = h.call(put(&key, &file)).await;
        assert_eq!(
            (
                reply.status,
                reply.code().as_str(),
                reply.json()["error"]["field"].as_str()
            ),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                Some("file")
            ),
            "{label}: {}",
            reply.text
        );
    }
    assert!(h.graph.requests().is_empty(), "Meta was never asked");
    // The smallest accepted, and a large square (no upper bound of ours).
    for side in [192, 4000] {
        h.graph.push_json(200, json!({"success": true}));
        let reply = h.call(put(&key, &jpeg(side, side, b"scan"))).await;
        assert_eq!(
            reply.status,
            StatusCode::NO_CONTENT,
            "{side}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), 2);
    assert_eq!(h.graph.remaining(), 0);
}

/// The form is `file` alone: another field, a second file, no file, a body
/// that is not a form, and a form whose framing never ends are `422` before
/// any request. Decisive: `read_file_form`'s allowed fields.
#[tokio::test]
async fn the_form_is_one_file() {
    let (h, key) = holding().await;
    let good = jpeg(192, 192, b"scan");
    let call = || Call::new(Method::PUT, path()).key(&key);
    let cases = [
        (
            "an extra field",
            call().multipart(&[
                ("messaging_product", None, b"whatsapp"),
                ("file", Some("g.jpg"), &good),
            ]),
            "body",
        ),
        (
            "two files",
            call().multipart(&[
                ("file", Some("g.jpg"), &good),
                ("file", Some("h.jpg"), &good),
            ]),
            "file",
        ),
        (
            "no file",
            call().multipart(&[("other", None, b"x")]),
            "body",
        ),
        ("an empty form", call().multipart(&[]), "file"),
        ("JSON", call().json(&json!({"file": "x"})), "body"),
        (
            "a bare image",
            call()
                .header("content-type", "image/jpeg")
                .body(Body::from(good.clone())),
            "body",
        ),
    ];
    for (label, request, field) in cases {
        let reply = h.call(request).await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{label}: {}",
            reply.text
        );
    }
    assert!(h.graph.requests().is_empty());
}

/// A file over 5 MiB is `413 media_too_large` with no request, and the body
/// is read no further than the bound and a chunk: a 12 MiB form pulls well
/// under 6 MiB; exactly 5 MiB is accepted. Decisive: the cap applied while
/// the file streams in.
#[tokio::test]
async fn a_picture_over_5_mib_stops_the_body_at_the_bound() {
    let (h, key) = holding().await;
    let big = jpeg(512, 512, &vec![7u8; 12 * 1024 * 1024]);
    let (content_type, form) = common::multipart(&[("file", Some("big.jpg"), &big)]);
    let pulled = Arc::new(AtomicUsize::new(0));
    let counter = pulled.clone();
    let chunks: Vec<Bytes> = form.chunks(64 * 1024).map(Bytes::copy_from_slice).collect();
    let body = futures::stream::iter(chunks).map(move |chunk| {
        counter.fetch_add(chunk.len(), Ordering::SeqCst);
        Ok::<_, std::io::Error>(chunk)
    });
    let reply = h
        .call(
            Call::new(Method::PUT, path())
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
        "read {read} bytes of a 12 MiB form for a 5 MiB bound"
    );
    assert!(h.graph.requests().is_empty());
    // One byte over, and the bound itself.
    let header = jpeg(512, 512, b"").len();
    let bound = 5 * 1024 * 1024;
    let over = jpeg(512, 512, &vec![7u8; bound - header + 1]);
    assert_eq!(over.len(), bound + 1);
    let reply = h.call(put(&key, &over)).await;
    assert_eq!(reply.code(), "media_too_large", "{}", reply.text);
    let exact = jpeg(512, 512, &vec![7u8; bound - header]);
    assert_eq!(exact.len(), bound);
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(put(&key, &exact)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    assert_eq!(h.graph.requests().len(), 1);
}

/// The design's M5.2, the decisive test: a group the number does not hold
/// (nobody reported it; another tenant's number holds it; another of the
/// tenant's numbers holds it; an id that is no group's) is `404 not_found`
/// with **zero vault reads**, nothing sent, and **the body never read**: a
/// body that never ends is answered at once. Once the number's own event
/// reports it, the same call reads the vault and asks Meta. Decisive:
/// `OwnedGroup` before the slot and the body.
#[tokio::test]
async fn a_group_the_number_does_not_hold_is_not_found_with_zero_vault_reads() {
    let (h, key) = connected_with(common::test_settings()).await;
    let picture = jpeg(192, 192, b"scan");
    // Another tenant's number holds it; another of ours holds another.
    hold(&h, OTHER_WABA, OTHER_PN).await;
    let never_ending = |group: &str, pn: &str| {
        let head = Bytes::from_static(b"--x\r\n");
        let body = futures::stream::once(async move { Ok::<_, std::io::Error>(head) })
            .chain(futures::stream::pending());
        Call::new(
            Method::PUT,
            format!("/v1/numbers/{pn}/groups/{group}/picture"),
        )
        .key(&key)
        .header("content-type", "multipart/form-data; boundary=x")
        .body(Body::from_stream(body))
    };
    for (group, pn) in [
        (GROUP, PN),
        (GROUP, PN_2),
        ("not-a-group", PN),
        ("Y2FwaV9ncm91cDoxMjM0NTY3ODkwOjEyMzQ1Njc4OTAZZZZZ", PN),
        ("../../x", PN),
    ] {
        let reads = h.kv.vault_reads();
        let answered = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            h.call(never_ending(group, pn)),
        )
        .await
        .expect("answered before the body was read");
        assert_eq!(
            (answered.status, answered.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{group} on {pn}: {}",
            answered.text
        );
        assert_eq!(h.kv.vault_reads(), reads, "{group} on {pn}: vault read");
        let reply = h
            .call(
                Call::new(
                    Method::PUT,
                    format!("/v1/numbers/{pn}/groups/{group}/picture"),
                )
                .key(&key)
                .multipart(&[("file", Some("g.jpg"), &picture)]),
            )
            .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
        assert_eq!(h.kv.vault_reads(), reads, "{group} on {pn}: vault read");
    }
    assert!(h.graph.requests().is_empty(), "nothing was sent");
    // The first number's own event: now the token is opened and Meta asked.
    hold(&h, WABA, PN).await;
    let reads = h.kv.vault_reads();
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(put(&key, &picture)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    assert!(h.kv.vault_reads() > reads, "the token was opened");
    assert_eq!(h.graph.requests().len(), 1);
    // ...and it is still not the tenant's other number's.
    let reads = h.kv.vault_reads();
    let other = Call::new(
        Method::PUT,
        format!("/v1/numbers/{PN_2}/groups/{GROUP}/picture"),
    )
    .key(&key)
    .multipart(&[("file", Some("g.jpg"), &picture)]);
    assert_eq!(h.call(other).await.status, StatusCode::NOT_FOUND);
    assert_eq!(h.kv.vault_reads(), reads);
}

/// The design's M5.1: another tenant's number is `404` with zero vault
/// reads, whatever the group, and a key without the scope `groups` is
/// `403`: neither reads the body nor asks Meta.
#[tokio::test]
async fn another_tenants_number_and_a_missing_scope_are_refused_first() {
    let (h, key) = holding().await;
    let picture = jpeg(192, 192, b"scan");
    let reads = h.kv.vault_reads();
    let theirs = h
        .call(
            Call::new(
                Method::PUT,
                format!("/v1/numbers/{OTHER_PN}/groups/{GROUP}/picture"),
            )
            .key(&key)
            .multipart(&[("file", Some("g.jpg"), &picture)]),
        )
        .await;
    assert_eq!(
        (theirs.status, theirs.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "{}",
        theirs.text
    );
    assert_eq!(h.kv.vault_reads(), reads);
    let media_only = h.tenant_key(TENANT, &[Scope::Media]).await;
    let reply = h.call(put(&media_only, &picture)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden"),
        "{}",
        reply.text
    );
    assert_eq!(h.kv.vault_reads(), reads);
    assert!(h.graph.requests().is_empty());
}

/// The tenant's share of the media transfer slots busy: `429` with
/// `Retry-After: 1`, before the body is read and any request; the slot
/// comes back when the call ends (the next picture goes through).
/// Decisive: `Groups::transfer_slot` on the pool the media routes use.
#[tokio::test]
async fn busy_transfer_slots_are_429_and_the_slot_comes_back() {
    let (h, key) = connected_with(Settings {
        media_concurrency: 1,
        ..common::test_settings()
    })
    .await;
    hold(&h, WABA, PN).await;
    let picture = jpeg(192, 192, b"scan");
    let held = h
        .state
        .media_slots()
        .try_acquire(&TenantId::parse(TENANT).unwrap())
        .unwrap();
    let reply = h.call(put(&key, &picture)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
        "{}",
        reply.text
    );
    assert_eq!(reply.headers["retry-after"], "1");
    assert!(h.graph.requests().is_empty());
    drop(held);
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(put(&key, &picture)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    // A refused picture gives its slot back too.
    let reply = h.call(put(&key, b"not an image")).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(put(&key, &picture)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
}

/// A tenant's picture upload that never finishes holds that tenant's share
/// of the slots, not the replica's: another tenant still sets a picture,
/// the first tenant's next one is `429`. Decisive: the tenant's share.
#[tokio::test]
async fn one_tenants_stuck_picture_leaves_the_others_their_slots() {
    let (h, a_key) = connected_with(Settings {
        media_concurrency: 2,
        ..common::test_settings()
    })
    .await;
    hold(&h, WABA, PN).await;
    hold(&h, OTHER_WABA, OTHER_PN).await;
    let b_key = h.tenant_key(OTHER_TENANT, &[Scope::Groups]).await;
    let picture = jpeg(192, 192, b"scan");
    // A's upload: the first half of the form, then nothing, ever.
    let (content_type, form) = common::multipart(&[("file", Some("g.jpg"), &picture)]);
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
    let request = Call::new(Method::PUT, path())
        .key(&a_key)
        .header("content-type", &content_type)
        .body(Body::from_stream(stuck))
        .build();
    let task = tokio::spawn(async move { common::send(&router, request).await });
    read.await.unwrap();
    h.graph.push_json(200, json!({"success": true}));
    let theirs = h
        .call(
            Call::new(
                Method::PUT,
                format!("/v1/numbers/{OTHER_PN}/groups/{GROUP}/picture"),
            )
            .key(&b_key)
            .multipart(&[("file", Some("g.jpg"), &picture)]),
        )
        .await;
    assert_eq!(theirs.status, StatusCode::NO_CONTENT, "{}", theirs.text);
    let again = h.call(put(&a_key, &picture)).await;
    assert_eq!(
        (again.status, again.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_requests")
    );
    task.abort();
    assert_eq!(h.graph.requests().len(), 1);
    assert_eq!(h.graph.remaining(), 0);
}

/// Meta's refusals, answered as their kind with Meta's code and none of its
/// text: a suspended group (`131207`) is `403`; a refusal quoting a text is
/// not echoed; a failure is `502`. A picture Meta refuses (an invalid
/// parameter) is not `404` (the group is the number's: the service's own
/// record says so).
#[tokio::test]
async fn metas_refusals_are_answered_by_kind_without_its_text() {
    let (h, key) = holding().await;
    let picture = jpeg(192, 192, b"scan");
    for (status, code, expected, kind) in [
        (400, 131_207, StatusCode::FORBIDDEN, "group_suspended"),
        (
            400,
            100,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_parameter",
        ),
        (500, 2, StatusCode::BAD_GATEWAY, "service_unavailable"),
    ] {
        h.graph.push_json(
            status,
            json!({"error": {"message": "(#x) quoted text MetaQuote-9Qz", "type": "OAuthException",
                             "code": code, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn",
                             "error_data": {"messaging_product": "whatsapp", "details": "MetaQuote-9Qz"}}}),
        );
        let reply = h.call(put(&key, &picture)).await;
        assert_ne!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{code}: {}",
            reply.text
        );
        assert_eq!(reply.status, expected, "{code}: {}", reply.text);
        assert_eq!(reply.code(), kind, "{code}: {}", reply.text);
        assert!(!reply.text.contains("MetaQuote-9Qz"), "{}", reply.text);
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// A picture cut by a timeout may have been set: `504 timeout` with
/// `may_have_been_sent`, one request made (the service does not repeat it),
/// and the audit event says `unknown`.
#[tokio::test]
async fn a_timeout_is_not_repeated_and_may_have_been_sent() {
    let (h, key) = holding().await;
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    let reply = h.call(put(&key, &jpeg(192, 192, b"scan"))).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::GATEWAY_TIMEOUT, "timeout"),
        "{}",
        reply.text
    );
    assert_eq!(reply.json()["error"]["may_have_been_sent"], true);
    assert_eq!(h.graph.requests().len(), 1, "sent once");
    assert_eq!(h.graph.remaining(), 0);
}
