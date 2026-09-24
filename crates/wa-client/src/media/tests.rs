//! Media API tests. Responses are the examples from
//! `business-phone-numbers/media`; Resumable Upload shapes are from the
//! Graph guide `docs/graph-api/guides/upload`.

use bytes::Bytes;
use futures::StreamExt;
use http::Method;
use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::{Digest, Sha256};
use wa_core::Error;
use wa_core::error::TransportError;
use wa_core::ids::{AppId, MediaId, UploadHandle, UploadSessionId};
use wa_core::testing::{RecordedBody, ScriptedTransport};
use wa_core::transport::ByteStream;

use super::*;
use crate::{Client, RetryPolicy};

const PNID: &str = "106540352242922";
const LOOKASIDE: &str = "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1037543291543636&ext=1&hash=abc";

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

fn sha_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn info_json(url: &str, sha256: &str, file_size: &serde_json::Value) -> serde_json::Value {
    // business-phone-numbers/media, "Get media URL" response syntax.
    json!({
      "messaging_product": "whatsapp",
      "url": url,
      "mime_type": "image/jpeg",
      "sha256": sha256,
      "file_size": file_size,
      "id": "1037543291543636"
    })
}

fn media_id() -> MediaId {
    MediaId::new("1037543291543636")
}

fn stream(chunks: &[&'static [u8]]) -> ByteStream {
    let items: Vec<Result<Bytes, TransportError>> =
        chunks.iter().map(|c| Ok(Bytes::from_static(c))).collect();
    Box::pin(futures::stream::iter(items))
}

fn info(sha256: &str) -> MediaInfo {
    MediaInfo {
        messaging_product: "whatsapp".into(),
        url: LOOKASIDE.into(),
        mime_type: "image/jpeg".into(),
        sha256: sha256.into(),
        file_size: None,
        id: media_id(),
    }
}

// ─── Upload ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn upload_sends_the_documented_multipart_fields() {
    let t = ScriptedTransport::new();
    // business-phone-numbers/media, "Example response".
    t.push_json(200, json!({"id": "1037543291543636"}));
    let id = client(&t)
        .media(PNID)
        .upload(
            Bytes::from_static(b"\x00\x00mp4"),
            "video/mp4",
            "black_friday_2025.mp4",
        )
        .await
        .unwrap();
    assert_eq!(id, media_id());
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/106540352242922/media");
    assert_eq!(req.bearer(), Some("TOKEN"));
    let (fname, ctype, data) = req.multipart_field("messaging_product").unwrap();
    assert_eq!((fname, ctype, &data[..]), (None, None, &b"whatsapp"[..]));
    let (fname, ctype, data) = req.multipart_field("type").unwrap();
    assert_eq!((fname, ctype, &data[..]), (None, None, &b"video/mp4"[..]));
    let (fname, ctype, data) = req.multipart_field("file").unwrap();
    assert_eq!(fname, Some("black_friday_2025.mp4"));
    assert_eq!(ctype, Some("video/mp4"));
    assert_eq!(&data[..], b"\x00\x00mp4");
    let RecordedBody::Multipart(parts) = &req.body else {
        panic!("expected multipart");
    };
    assert_eq!(parts.len(), 3);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn upload_rejects_unsupported_types_and_oversized_files_locally() {
    let t = ScriptedTransport::new();
    let media = client(&t).media(PNID);
    let err = media
        .upload(vec![0u8; 10], "image/gif", "a.gif")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Validation(v) if v.field == "type"),
        "{err}"
    );
    // 5 MB image limit, read as MiB.
    let err = media
        .upload(vec![0u8; 5 * 1024 * 1024 + 1], "image/png", "a.png")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Validation(v) if v.field == "file"),
        "{err}"
    );
    // 500 KB (animated) sticker limit.
    let err = media
        .upload(vec![0u8; 500 * 1024 + 1], "image/webp", "a.webp")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Validation(v) if v.field == "file"),
        "{err}"
    );
    assert!(t.requests().is_empty(), "nothing is sent");

    t.push_json(200, json!({"id": "1"}));
    media
        .upload(vec![0u8; 5 * 1024 * 1024], "image/png", "a.png")
        .await
        .unwrap();
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn upload_is_not_replayed_after_a_timeout() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    t.push_json(200, json!({"id": "1"}));
    let c = Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::default())
        .build()
        .unwrap();
    assert!(
        c.media(PNID)
            .upload(vec![1u8], "image/png", "a.png")
            .await
            .is_err()
    );
    assert_eq!(t.requests().len(), 1);
}

// ─── URL, delete ─────────────────────────────────────────────────────────

#[tokio::test]
async fn url_parses_the_documented_response_and_scopes_on_request() {
    let t = ScriptedTransport::new();
    t.push_json(200, info_json(LOOKASIDE, &sha_hex(b"x"), &json!("1234")));
    t.push_json(200, info_json(LOOKASIDE, &sha_hex(b"x"), &json!(1234)));
    let media = client(&t).media(PNID);
    let info = media.url(&media_id()).await.unwrap();
    assert_eq!(info.file_size, Some(1234), "string form, as documented");
    assert_eq!(info.url, LOOKASIDE);
    assert_eq!(info.mime_type, "image/jpeg");
    assert_eq!(info.id, media_id());
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), "/v25.0/1037543291543636");
    assert_eq!(req.query("phone_number_id"), None);

    let info = media
        .restrict_to_phone_number()
        .url(&media_id())
        .await
        .unwrap();
    assert_eq!(info.file_size, Some(1234), "number form");
    assert_eq!(
        t.last_request()
            .unwrap()
            .query("phone_number_id")
            .as_deref(),
        Some(PNID)
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn delete_uses_the_media_id_path_and_optional_scope() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    t.push_json(200, json!({"success": true}));
    let media = client(&t).media(PNID);
    media.delete(&media_id()).await.unwrap();
    media
        .clone()
        .restrict_to_phone_number()
        .delete(&media_id())
        .await
        .unwrap();
    let reqs = t.requests();
    assert_eq!(reqs[0].method, Method::DELETE);
    assert_eq!(reqs[0].path(), "/v25.0/1037543291543636");
    assert_eq!(reqs[0].query("phone_number_id"), None);
    assert_eq!(reqs[1].query("phone_number_id").as_deref(), Some(PNID));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn ids_cannot_escape_their_path_segment() {
    // A crafted media id must not turn `DELETE /{media-id}` into
    // `DELETE /{waba}/subscribed_apps` with the business token.
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"id": "1"}));
    t.push_json(200, json!({"success": true}));
    t.push_json(200, info_json(LOOKASIDE, &sha_hex(b"x"), &json!(1)));
    client(&t)
        .media("123/subscribed_apps")
        .upload(vec![1u8], "image/png", "a.png")
        .await
        .unwrap();
    let media = client(&t).media(PNID);
    let crafted = MediaId::new("123/subscribed_apps");
    media.delete(&crafted).await.unwrap();
    media.url(&MediaId::new("1?fields=x")).await.unwrap();
    let reqs = t.requests();
    assert_eq!(reqs[0].path(), "/v25.0/123%2Fsubscribed_apps/media");
    assert_eq!(
        (reqs[1].method.clone(), reqs[1].path()),
        (Method::DELETE, "/v25.0/123%2Fsubscribed_apps")
    );
    assert_eq!(reqs[2].path(), "/v25.0/1%3Ffields=x");
    assert_eq!(reqs[2].url.query(), None);
    assert_eq!(t.remaining(), 0);

    // Segments URL normalization would drop or pop never leave the client.
    let t = ScriptedTransport::new();
    let media = client(&t).media(PNID);
    for bad in ["", "..", "."] {
        let err = client(&t)
            .media(bad)
            .upload(vec![1u8], "image/png", "a.png")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "path"),
            "{bad:?}: {err}"
        );
        let id = MediaId::new(bad);
        for err in [
            media.delete(&id).await.unwrap_err(),
            media.url(&id).await.unwrap_err(),
            media.download_bytes(&id, 10).await.unwrap_err(),
        ] {
            assert!(
                matches!(&err, Error::Validation(v) if v.field == "path"),
                "{bad:?}: {err}"
            );
        }
    }
    assert!(t.requests().is_empty());
}

// ─── Download ────────────────────────────────────────────────────────────

#[tokio::test]
async fn download_streams_from_the_cdn_with_the_token_and_verifies() {
    let body = b"\xff\xd8jpeg bytes";
    let t = ScriptedTransport::new();
    t.push_json(200, info_json(LOOKASIDE, &sha_hex(body), &json!("13")));
    t.push_bytes(200, "image/jpeg", Bytes::from_static(body));
    let dl = client(&t).media(PNID).download(&media_id()).await.unwrap();
    let mut verified = dl.verified().unwrap();
    let mut got = Vec::new();
    while let Some(chunk) = verified.body.next().await {
        got.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(got, body);
    let reqs = t.requests();
    assert_eq!(reqs[1].method, Method::GET);
    assert_eq!(reqs[1].url.as_str(), LOOKASIDE);
    assert_eq!(reqs[1].bearer(), Some("TOKEN"), "the CDN needs the token");
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn tampered_media_fails_at_the_end_of_the_stream() {
    let t = ScriptedTransport::new();
    t.push_json(200, info_json(LOOKASIDE, &sha_hex(b"original"), &json!(8)));
    t.push_bytes(200, "image/jpeg", Bytes::from_static(b"tampered"));
    let dl = client(&t).media(PNID).download(&media_id()).await.unwrap();
    let mut body = dl.verified().unwrap().body;
    // The bytes still arrive (they are streamed), then the verdict.
    assert_eq!(&body.next().await.unwrap().unwrap()[..], b"tampered");
    let err = body.next().await.unwrap().unwrap_err();
    assert!(
        matches!(&err, Error::Transport(TransportError::Integrity(_))),
        "{err:?}"
    );
    assert!(err.is_retryable(), "a fresh download may be intact");
    assert!(body.next().await.is_none(), "stream ends after the error");
}

#[tokio::test]
async fn verification_hashes_across_chunks_and_accepts_base64_digests() {
    let whole = b"chunk-one|chunk-two|chunk-three";
    let b64 = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(whole))
    };
    for digest in [sha_hex(whole), sha_hex(whole).to_uppercase(), b64] {
        let dl = MediaDownload {
            info: info(&digest),
            body: stream(&[b"chunk-one|", b"chunk-two|", b"chunk-three"]),
        };
        let items: Vec<_> = dl.verified().unwrap().body.take(16).collect().await;
        assert_eq!(items.len(), 3, "{digest}: three chunks, no error");
        assert!(items.iter().all(Result::is_ok));
    }
    // Same chunks in a different order: same bytes per chunk, wrong file.
    let dl = MediaDownload {
        info: info(&sha_hex(whole)),
        body: stream(&[b"chunk-two|", b"chunk-one|", b"chunk-three"]),
    };
    let items: Vec<_> = dl.verified().unwrap().body.take(16).collect().await;
    assert_eq!(items.len(), 4);
    assert!(matches!(
        items[3],
        Err(Error::Transport(TransportError::Integrity(_)))
    ));
    // A truncated file fails too.
    let dl = MediaDownload {
        info: info(&sha_hex(whole)),
        body: stream(&[b"chunk-one|"]),
    };
    let items: Vec<_> = dl.verified().unwrap().body.take(16).collect().await;
    assert_eq!(items.len(), 2);
    assert!(matches!(
        items[1],
        Err(Error::Transport(TransportError::Integrity(_)))
    ));
    // An empty body is checked too (the digest of nothing is not `whole`'s).
    let dl = MediaDownload {
        info: info(&sha_hex(whole)),
        body: stream(&[]),
    };
    let items: Vec<_> = dl.verified().unwrap().body.take(16).collect().await;
    assert!(matches!(
        items[..],
        [Err(Error::Transport(TransportError::Integrity(_)))]
    ));
}

#[tokio::test]
async fn a_verified_stream_stays_finished_and_tolerates_padded_digests() {
    let body = b"payload";
    // A digest carried with stray whitespace (hand-built from a webhook).
    let dl = MediaDownload {
        info: info(&format!(" {}\n", sha_hex(body))),
        body: stream(&[b"pay", b"load"]),
    };
    let mut verified = dl.verified().unwrap().body;
    while let Some(chunk) = verified.next().await {
        chunk.unwrap();
    }
    // Polling a finished stream again must not re-run the check on an
    // empty hasher and invent a mismatch.
    assert!(verified.next().await.is_none());
    assert!(verified.next().await.is_none());
}

#[test]
fn media_info_tolerates_missing_optional_fields() {
    // Only `url` and `id` are needed to download; nothing else may be
    // required to parse.
    let info: MediaInfo = serde_json::from_value(json!({"url": LOOKASIDE, "id": "1"})).unwrap();
    assert_eq!(
        (
            info.messaging_product.as_str(),
            info.mime_type.as_str(),
            info.sha256.as_str(),
            info.file_size
        ),
        ("", "", "", None)
    );
}

#[tokio::test]
async fn a_reported_size_is_not_trusted_for_allocation() {
    // `collect(u64::MAX)` with a hostile `file_size` must neither abort the
    // process (allocation failure) nor stop the real bytes from arriving.
    let body = b"small file";
    let mut i = info(&sha_hex(body));
    i.file_size = Some(1 << 60);
    let dl = MediaDownload {
        info: i,
        body: stream(&[b"small ", b"file"]),
    };
    let got = dl.verified().unwrap().collect(u64::MAX).await.unwrap();
    assert_eq!(&got.data[..], body);
}

#[tokio::test]
async fn transport_errors_mid_stream_surface_and_end_the_stream() {
    let items: Vec<Result<Bytes, TransportError>> = vec![
        Ok(Bytes::from_static(b"a")),
        Err(TransportError::Timeout),
        Ok(Bytes::from_static(b"b")),
    ];
    let dl = MediaDownload {
        info: info(&sha_hex(b"ab")),
        body: Box::pin(futures::stream::iter(items)),
    };
    let got: Vec<_> = dl.verified().unwrap().body.take(16).collect().await;
    assert_eq!(got.len(), 2);
    assert!(matches!(
        got[1],
        Err(Error::Transport(TransportError::Timeout))
    ));
}

#[test]
fn a_malformed_digest_fails_before_streaming() {
    let dl = MediaDownload {
        info: info("PHOTO_HASH"),
        body: stream(&[b"x"]),
    };
    assert!(matches!(dl.verified(), Err(Error::Validation(v)) if v.field == "sha256"));
}

#[tokio::test]
async fn download_urls_on_foreign_hosts_are_refused() {
    for url in [
        "https://evil.example/steal?mid=1",
        "http://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1",
        "https://lookaside.fbsbx.com.evil.example/x",
    ] {
        let t = ScriptedTransport::new();
        t.push_json(200, info_json(url, &sha_hex(b"x"), &json!(1)));
        // Would be served if the token were (wrongly) sent there.
        t.push_bytes(200, "image/jpeg", Bytes::from_static(b"x"));
        let err = client(&t)
            .media(PNID)
            .download(&media_id())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "url"),
            "{url}: {err}"
        );
        assert_eq!(
            t.requests().len(),
            1,
            "{url}: only the metadata request was sent"
        );
    }
    let t = ScriptedTransport::new();
    t.push_json(200, info_json("not a url", &sha_hex(b"x"), &json!(1)));
    let err = client(&t)
        .media(PNID)
        .download(&media_id())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Validation(v) if v.field == "url"),
        "{err}"
    );
}

#[tokio::test]
async fn an_expired_download_url_surfaces_the_http_error() {
    let t = ScriptedTransport::new();
    t.push_json(200, info_json(LOOKASIDE, &sha_hex(b"x"), &json!(1)));
    t.push_json(
        404,
        json!({"error": {"message": "(#100) Media not found", "code": 100}}),
    );
    let err = client(&t)
        .media(PNID)
        .download(&media_id())
        .await
        .unwrap_err();
    assert_eq!(err.graph().unwrap().http_status, Some(404));
}

#[tokio::test]
async fn download_bytes_verifies_and_caps_the_size() {
    let body = b"0123456789";
    // Within the cap.
    let t = ScriptedTransport::new();
    t.push_json(200, info_json(LOOKASIDE, &sha_hex(body), &json!("10")));
    t.push_bytes(200, "image/jpeg", Bytes::from_static(body));
    let got = client(&t)
        .media(PNID)
        .download_bytes(&media_id(), 10)
        .await
        .unwrap();
    assert_eq!(&got.data[..], body);
    assert_eq!(got.info.file_size, Some(10));
    assert_eq!(t.remaining(), 0);

    // Reported size over the cap: refused before downloading.
    let t = ScriptedTransport::new();
    t.push_json(200, info_json(LOOKASIDE, &sha_hex(body), &json!("10")));
    let err = client(&t)
        .media(PNID)
        .download_bytes(&media_id(), 9)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Validation(v) if v.field == "file_size"),
        "{err}"
    );
    assert_eq!(t.requests().len(), 1);

    // Reported size lies: the body is cut off at the cap anyway.
    let t = ScriptedTransport::new();
    t.push_json(200, info_json(LOOKASIDE, &sha_hex(body), &json!("3")));
    t.push_bytes(200, "image/jpeg", Bytes::from_static(body));
    let err = client(&t)
        .media(PNID)
        .download_bytes(&media_id(), 9)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Validation(v) if v.field == "file_size"),
        "{err}"
    );

    // Tampered body.
    let t = ScriptedTransport::new();
    t.push_json(200, info_json(LOOKASIDE, &sha_hex(b"other"), &json!("10")));
    t.push_bytes(200, "image/jpeg", Bytes::from_static(body));
    let err = client(&t)
        .media(PNID)
        .download_bytes(&media_id(), 100)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Transport(TransportError::Integrity(_))),
        "{err:?}"
    );
}

// ─── Resumable Upload API ────────────────────────────────────────────────

#[tokio::test]
async fn resumable_upload_follows_the_graph_guide() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"id": "upload:MTphdHRhY2htZW50OjZiMDI1ZWI3"}));
    t.push_json(200, json!({"h": "2:c2FtcGxl..."}));
    t.push_json(
        200,
        json!({"id": "upload:MTphdHRhY2htZW50OjZiMDI1ZWI3", "file_offset": "0"}),
    );
    let media = client(&t).media(PNID);
    let session = media
        .start_upload_session(&AppId::new("634974688087057"), "offer.png", 12, "image/png")
        .await
        .unwrap();
    assert_eq!(session.as_str(), "upload:MTphdHRhY2htZW50OjZiMDI1ZWI3");
    let handle = media
        .upload_chunk(&session, 0, Bytes::from_static(b"png bytes..."))
        .await
        .unwrap();
    assert_eq!(handle, Some(UploadHandle::new("2:c2FtcGxl...")));
    assert_eq!(media.upload_session_status(&session).await.unwrap(), 0);

    let reqs = t.requests();
    // Step 1.
    assert_eq!(reqs[0].method, Method::POST);
    assert_eq!(reqs[0].path(), "/v25.0/634974688087057/uploads");
    assert_eq!(reqs[0].query("file_name").as_deref(), Some("offer.png"));
    assert_eq!(reqs[0].query("file_length").as_deref(), Some("12"));
    assert_eq!(reqs[0].query("file_type").as_deref(), Some("image/png"));
    assert_eq!(
        reqs[0].query("access_token"),
        None,
        "token stays out of the URL"
    );
    assert_eq!(reqs[0].header("authorization"), Some("OAuth TOKEN"));
    // Step 2.
    assert_eq!(reqs[1].method, Method::POST);
    assert_eq!(reqs[1].path(), "/v25.0/upload:MTphdHRhY2htZW50OjZiMDI1ZWI3");
    assert_eq!(reqs[1].header("authorization"), Some("OAuth TOKEN"));
    assert_eq!(reqs[1].header("file_offset"), Some("0"));
    assert_eq!(
        reqs[1].body,
        RecordedBody::Bytes {
            content_type: "application/octet-stream".into(),
            data: Bytes::from_static(b"png bytes...")
        }
    );
    // Resume status.
    assert_eq!(reqs[2].method, Method::GET);
    assert_eq!(reqs[2].path(), "/v25.0/upload:MTphdHRhY2htZW50OjZiMDI1ZWI3");
    assert_eq!(reqs[2].header("authorization"), Some("OAuth TOKEN"));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn resuming_sends_the_offset_and_session_query_stays_a_query() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"id": "upload:abc", "file_offset": 1024}));
    t.push_json(200, json!({"h": "4::aW..."}));
    let media = client(&t).media(PNID);
    // A session id carrying a query part is used as the guide's curl would.
    let session = UploadSessionId::new("upload:abc?sig=ARZ-1_x");
    let offset = media.upload_session_status(&session).await.unwrap();
    assert_eq!(offset, 1024, "numeric form");
    media
        .upload_chunk(&session, offset, vec![7u8; 4])
        .await
        .unwrap();
    let reqs = t.requests();
    for r in &reqs {
        assert_eq!(r.path(), "/v25.0/upload:abc");
        assert_eq!(r.query("sig").as_deref(), Some("ARZ-1_x"));
    }
    assert_eq!(reqs[1].header("file_offset"), Some("1024"));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn session_ids_stay_one_segment_and_must_be_upload_sessions() {
    // A `/` before the `?` is escaped into the single segment; the part
    // after it is still the query the guide's curl would send.
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"id": "upload:abc", "file_offset": "0"}));
    let media = client(&t).media(PNID);
    let crafted = UploadSessionId::new("upload:abc/../../123/subscribed_apps?sig=1");
    assert_eq!(media.upload_session_status(&crafted).await.unwrap(), 0);
    let r = t.last_request().unwrap();
    assert_eq!(
        r.path(),
        "/v25.0/upload:abc%2F..%2F..%2F123%2Fsubscribed_apps"
    );
    assert_eq!(r.url.query(), Some("sig=1"));
    assert_eq!(t.remaining(), 0);

    // Anything but an `upload:` session would aim the token at another
    // object (`GET /123456` is someone's WABA).
    let t = ScriptedTransport::new();
    let media = client(&t).media(PNID);
    for bad in [
        "",
        "123456",
        "upload:",
        "?sig=1",
        "upload:?sig=1",
        "x/upload:1",
        " upload:1",
    ] {
        let s = UploadSessionId::new(bad);
        for err in [
            media.upload_session_status(&s).await.unwrap_err(),
            media.upload_chunk(&s, 0, vec![1u8]).await.unwrap_err(),
        ] {
            assert!(
                matches!(&err, Error::Validation(v) if v.field == "upload_session_id"),
                "{bad:?}: {err}"
            );
        }
    }
    assert!(t.requests().is_empty());
}

#[tokio::test]
async fn resumable_upload_convenience_and_its_failure_steps() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"id": "upload:s1"}));
    t.push_json(200, json!({"h": "4::aW..."}));
    let media = client(&t).media(PNID);
    let app = AppId::new("1");
    let h = media
        .resumable_upload(&app, "a.pdf", "application/pdf", vec![1u8, 2, 3])
        .await
        .unwrap();
    assert_eq!(h.as_str(), "4::aW...");
    assert_eq!(t.requests()[0].query("file_length").as_deref(), Some("3"));

    // No handle in the response.
    t.push_json(200, json!({"id": "upload:s2"}));
    t.push_json(200, json!({}));
    let err = media
        .resumable_upload(&app, "a.pdf", "application/pdf", vec![1u8])
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Step {
                step: "upload_file",
                ..
            }
        ),
        "{err}"
    );

    // Graph error in step 1.
    t.push_json(400, json!({"error": {"message": "bad", "code": 100}}));
    let err = media
        .resumable_upload(&app, "a.pdf", "application/pdf", vec![1u8])
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Step {
                step: "start_upload_session",
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(err.graph().unwrap().code, 100);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn resumable_upload_validates_locally_and_needs_a_token() {
    let t = ScriptedTransport::new();
    let media = client(&t).media(PNID);
    let app = AppId::new("1");
    for (name, ty, field) in [
        ("a.gif", "image/gif", "file_type"),
        ("a.webp", "image/webp", "file_type"),
        ("", "image/png", "file_name"),
    ] {
        let err = media
            .start_upload_session(&app, name, 1, ty)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == field),
            "{err}"
        );
    }
    let err = media
        .start_upload_session(&AppId::new(".."), "a.png", 1, "image/png")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Validation(v) if v.field == "path"),
        "{err}"
    );
    assert!(t.requests().is_empty());

    // An app id smuggling a path stays one segment.
    t.push_json(200, json!({"id": "upload:s"}));
    media
        .start_upload_session(&AppId::new("1/uploads"), "a.png", 1, "image/png")
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().path(),
        "/v25.0/1%2Fuploads/uploads"
    );
    assert_eq!(t.remaining(), 0);

    // No token: the guide's steps all need one, and none may go out bare.
    let t = ScriptedTransport::new();
    let tokenless = Client::builder()
        .transport(t.clone())
        .build()
        .unwrap()
        .media(PNID);
    let session = UploadSessionId::new("upload:x");
    for err in [
        tokenless
            .start_upload_session(&app, "a.png", 1, "image/png")
            .await
            .unwrap_err(),
        tokenless
            .upload_chunk(&session, 0, vec![1u8])
            .await
            .unwrap_err(),
        tokenless.upload_session_status(&session).await.unwrap_err(),
    ] {
        assert!(matches!(err, Error::Config(_)), "{err}");
    }
    assert!(t.requests().is_empty());
}
