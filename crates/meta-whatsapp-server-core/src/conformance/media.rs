//! Roadmap S5d: the core's media services ([`crate::media`]) over a
//! backend's bundle, with no HTTP layer: the core's [`Authorizer`] over
//! the backend's records and a vault on its key/value store, and a Graph
//! client on a [`ScriptedTransport`] whose media downloads arrive in the
//! chunks the case gives (`Chunked`). A download through the core's
//! stream ends before its last chunk on a digest mismatch (M1b's rule,
//! without axum), passes the file on while it arrives (one chunk held,
//! never the file), and stops past its `max_bytes`; a whole download
//! answers no byte of a tampered file; the size Meta reports, its digest
//! and its URL are checked before the download (the token goes only where
//! the library's host allow-list lets it); a media id is the number's or
//! it is not found; uploads and deletions send Meta's documented requests
//! with the number's token; the services open only on their own
//! [`Authorizer`]'s capability, and take a slot of the number's tenant's
//! share first.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{FutureExt as _, StreamExt as _};
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::testing::{RecordedRequest, ScriptedTransport};
use meta_whatsapp_rs::core::transport::{
    ByteStream, HttpRequest, HttpResponse, HttpTransport, StreamingResponse,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::messages::Told;
use super::templates::{pairs, query};
use crate::admin::mint;
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::backend::Backend;
use crate::error::ServiceError;
use crate::media::{
    DownloadBody, DownloadLimits, Downloaded, MAX_WHOLE_DOWNLOAD, Media, MediaSlots, MediaStream,
    StreamStopped, Upload, Uploaded, upload_cap,
};
use crate::model::{BindOutcome, KeyOwner, Scope, TenantId};
use crate::ratelimit::Slots;

/// The tenant, its WABA, its number and the token in the vault, of the
/// download case; the upload case's own (both run on one backend).
const TENANT: &str = "media-owner";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const UPLOADER: (&str, &str, &str) = ("media-uploader", "102290129340399", "106540352242923");
const TOKEN: &str = "EAAG-the-media-owners-token";
/// A media id, as `business-phone-numbers/media` writes one.
const MEDIA: &str = "1037543291543636";
/// The download URL shape of `business-phone-numbers/media`: Meta's
/// lookaside host, where the library lets the token go.
const URL: &str = "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1037543291543636&ext=1&hash=abc";
/// `WA_SERVER_MEDIA_MAX_BYTES`'s default, the streamed downloads' cap.
const STREAM_CAP: u64 = 100 * 1024 * 1024;

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    a_download_through_the_cores_stream_ends_before_its_last_chunk_on_a_digest_mismatch(backend)
        .await;
    the_media_services_upload_and_delete_as_meta_documents(backend).await;
}

/// The script, with each media download's body in the chunks a case
/// queued ([`Self::body`]): the URL answers its scripted status and
/// headers, then the next queued body (the scripted body when none is
/// queued). Every request is the script's, and recorded there.
#[derive(Clone, Default)]
pub(crate) struct Chunked {
    script: ScriptedTransport,
    bodies: Arc<Mutex<VecDeque<ByteStream>>>,
}

impl std::fmt::Debug for Chunked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chunked")
            .field("script", &self.script)
            .finish_non_exhaustive()
    }
}

impl Chunked {
    /// The next media download's body: `chunks`, then its end.
    fn chunks(&self, chunks: &[Bytes]) {
        let chunks: Vec<Result<Bytes, TransportError>> = chunks.iter().cloned().map(Ok).collect();
        self.body(Box::pin(futures::stream::iter(chunks)));
    }

    /// The next media download's body: `body`.
    fn body(&self, body: ByteStream) {
        self.bodies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(body);
    }
}

#[async_trait]
impl HttpTransport for Chunked {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.script.send(request).await
    }

    async fn send_streaming(
        &self,
        request: HttpRequest,
    ) -> Result<StreamingResponse, TransportError> {
        let head = self.script.send(request).await?;
        let queued = self
            .bodies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        let body = queued.unwrap_or_else(|| {
            let body = head.body;
            Box::pin(futures::stream::once(async move { Ok(body) }))
        });
        Ok(StreamingResponse {
            status: head.status,
            headers: head.headers,
            body,
        })
    }
}

/// The SHA-256 of `data`, in hex.
fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// `GET /{media-id}`'s answer (`business-phone-numbers/media`, "Response
/// syntax": `file_size` a string, as documented), its digest `sha256`.
fn media_info(url: &str, sha256: &str, file_size: Option<usize>) -> Value {
    let mut info = json!({"messaging_product": "whatsapp", "url": url, "mime_type": "image/jpeg",
                          "sha256": sha256, "id": MEDIA});
    if let Some(size) = file_size {
        info["file_size"] = json!(size.to_string());
    }
    info
}

/// Three chunks of 64 KiB, each its own bytes, and the file they make.
fn three_chunks() -> (Vec<Bytes>, Vec<u8>) {
    let chunks: Vec<Bytes> = (1u8..=3).map(|i| Bytes::from(vec![i; 64 * 1024])).collect();
    let file = chunks.concat();
    (chunks, file)
}

/// Every item of a stream, to its end.
async fn drain(mut stream: MediaStream) -> Vec<Result<Bytes, StreamStopped>> {
    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item);
    }
    items
}

/// The stream of a download, which must be one.
fn streamed(downloaded: Downloaded) -> MediaStream {
    match downloaded.body {
        DownloadBody::Stream(stream) => stream,
        DownloadBody::Whole(_) => panic!("a whole download, where a stream was asked for"),
    }
}

/// The two requests of a download: the lookup of `MEDIA` on the number,
/// then Meta's URL, each with the number's token.
fn assert_downloaded(requests: &[RecordedRequest]) {
    let [.., lookup, file] = requests else {
        panic!("{requests:?}")
    };
    assert_lookup(lookup, "GET", PN);
    assert_eq!(file.method.as_str(), "GET");
    assert_eq!(file.url.as_str(), URL);
    assert_eq!(file.bearer(), Some(TOKEN));
}

/// A request on `MEDIA`, scoped to the number `pn`, with its token.
fn assert_lookup(request: &RecordedRequest, method: &str, pn: &str) {
    assert_eq!(
        (request.method.as_str(), request.path()),
        (method, format!("/v25.0/{MEDIA}").as_str())
    );
    assert_eq!(query(request), pairs(&[("phone_number_id", pn)]));
    assert_eq!(request.bearer(), Some(TOKEN));
}

/// A `media` key for `tenant` that `authz` admits.
async fn caller_of(backend: &dyn Backend, authz: &Authorizer, tenant: &str) -> Caller {
    let key = mint(
        backend.records().as_ref(),
        KeyOwner::Tenant(TenantId::parse(tenant).unwrap()),
        vec![Scope::Media],
        String::new(),
        None,
    )
    .await
    .unwrap()
    .0;
    authz
        .tenant_caller(
            Some(&format!("Bearer {}", key.expose_key())),
            None,
            Scope::Media,
        )
        .await
        .unwrap()
}

/// The tenant's number bound (`(tenant, waba, pn)`), its token in the
/// vault, an [`Authorizer`] over the backend with a client on
/// `transport`, and the number's capability.
async fn connected(
    backend: &dyn Backend,
    transport: Chunked,
    (tenant, waba, pn): (&str, &str, &str),
) -> (Authorizer, OwnedNumber, TokenVault) {
    let records = backend.records();
    let tenant_id = TenantId::parse(tenant).unwrap();
    records.create_tenant(&tenant_id, "").await.unwrap();
    let waba_id = WabaId::new(waba).unwrap();
    let pn = PhoneNumberId::new(pn).unwrap();
    let bound = records
        .bind_waba(&tenant_id, &waba_id, std::slice::from_ref(&pn))
        .await
        .unwrap();
    assert_eq!(bound, BindOutcome::Bound);
    let vault = TokenVault::new(
        backend.kv(),
        VaultKeys::new(VaultKey::generate("media-suite").unwrap()),
    )
    .unwrap();
    vault
        .store(
            &StoredBusinessToken::new(waba_id, AccessToken::new(TOKEN))
                .phone_number_ids([pn.clone()]),
        )
        .await
        .unwrap();
    let client = Client::builder().transport(transport).build().unwrap();
    let authz = Authorizer::new(records.clone(), vault.clone(), client).unwrap();
    let caller = caller_of(backend, &authz, tenant).await;
    let number = authz.owned_number(&caller, pn).await.unwrap();
    (authz, number, vault)
}

/// `(status, code, field)` of a refusal.
fn refusal(error: &ServiceError) -> (u16, &'static str, Option<&str>) {
    (error.status(), error.code(), error.field())
}

/// Roadmap S5d, decisive: a download through the core's stream ends
/// before its last chunk on a digest mismatch (M1b's rule, with no HTTP
/// framework), and every check of a download holds without one.
///
/// 1. A capability another [`Authorizer`] made, or this one's through the
///    other order, is `403 forbidden` at `Media::new`, nothing asked.
/// 2. Streamed, a file whose digest matches: its three chunks, in order,
///    then the end; the lookup `GET /{media}?phone_number_id={pn}` and
///    Meta's URL each with the number's token; Meta's digest answered.
/// 3. Streamed, a tampered file: its first two chunks, then
///    [`StreamStopped::Integrity`] instead of the last, then the end: the
///    bytes yielded are never the whole file.
/// 4. Streamed, a file still arriving: its first chunk is yielded while
///    Meta's body is open, the second held until the third arrives, the
///    last once the body ended verified: one chunk held, never the file.
/// 5. Streamed past `max_bytes`: [`StreamStopped::TooLarge`], nothing of
///    the file yielded.
/// 6. Whole: the verified file; a tampered one `502 integrity` with no
///    byte of it; one past `max_bytes` `413 media_too_large`.
/// 7. Before the download: a size Meta reports over `max_bytes` is `413`,
///    a digest it did not send usably `502 integrity`, a URL off Meta's
///    media host `502 upstream` (the library's allow-list refuses it: the
///    token never goes there); an answer for another id, or Meta refusing
///    the id, `404 not_found` without Meta's code; a media id that is not
///    a Meta id `422` on `media_id`, with no request.
/// 8. A slot first: the tenant's share of the pool its download needs
///    taken is `429`, with no request; a stream holds its slot until it is
///    dropped.
///
/// Decisive: the digest check dropped (a mismatch taken for the end)
/// fails step 3; the digest checked only once the last chunk was yielded
/// (no chunk held back) fails steps 3 and 4; `Media::new`'s admission
/// dropped fails step 1; a size cap dropped (the stream's, the whole
/// read's, the reported size's) fails step 5, 6 or 7.
pub async fn a_download_through_the_cores_stream_ends_before_its_last_chunk_on_a_digest_mismatch(
    backend: &dyn Backend,
) {
    let meta = Chunked::default();
    let script = meta.script.clone();
    let (authz, number, vault) = connected(backend, meta.clone(), (TENANT, WABA, PN)).await;
    let transfers = Slots::new(8);
    let streams = Slots::new(8);
    let slots = MediaSlots {
        transfers: &transfers,
        streams: &streams,
    };
    let told = Told::default();
    let whole = DownloadLimits::parse(None, None, STREAM_CAP).unwrap();
    let stream = DownloadLimits::parse(Some("true"), None, STREAM_CAP).unwrap();
    assert_eq!(whole.max_bytes(), MAX_WHOLE_DOWNLOAD);

    // 1. Only the order's own capability.
    let other_script = ScriptedTransport::new();
    let other_client = Client::builder()
        .transport(other_script.clone())
        .build()
        .unwrap();
    let other = Authorizer::new(backend.records(), vault, other_client).unwrap();
    let forged = caller_of(backend, &other, TENANT).await;
    let theirs = other
        .owned_number(&forged, PhoneNumberId::new(PN).unwrap())
        .await
        .unwrap();
    for (refused, which) in [
        (Media::new(&authz, &theirs, slots).map(drop), "their number"),
        (
            Media::new(&other, &number, slots).map(drop),
            "the number through their order",
        ),
    ] {
        assert_eq!(
            refusal(&refused.unwrap_err()),
            (403, "forbidden", None),
            "{which}"
        );
    }
    assert!(Media::new(&other, &theirs, slots).is_ok());
    let media = Media::new(&authz, &number, slots)
        .unwrap()
        .observed_by(&told);

    // 2. Streamed, verified: every chunk, in order, then the end.
    let (chunks, file) = three_chunks();
    script.push_json(200, media_info(URL, &sha256_hex(&file), Some(file.len())));
    script.push_bytes(200, "image/jpeg", Vec::new());
    meta.chunks(&chunks);
    let downloaded = media.download(MEDIA, stream).await.unwrap();
    assert_eq!(downloaded.content_type, "image/jpeg");
    assert_eq!(downloaded.sha256_hex(), sha256_hex(&file));
    assert_eq!(
        hex::encode(downloaded.sha256),
        sha256_hex(&file),
        "Meta's digest"
    );
    let items = drain(streamed(downloaded)).await;
    assert_eq!(items, chunks.iter().cloned().map(Ok).collect::<Vec<_>>());
    assert_downloaded(&script.requests());

    // 3. Streamed, tampered: the last chunk never comes, the error does.
    script.push_json(200, media_info(URL, &sha256_hex(b"the original"), None));
    script.push_bytes(200, "image/jpeg", Vec::new());
    meta.chunks(&chunks);
    let items = drain(streamed(media.download(MEDIA, stream).await.unwrap())).await;
    assert_eq!(
        items,
        [
            Ok(chunks[0].clone()),
            Ok(chunks[1].clone()),
            Err(StreamStopped::Integrity)
        ],
        "the stream must end before its last chunk on a mismatch"
    );
    let yielded: Vec<u8> = items.into_iter().filter_map(Result::ok).flatten().collect();
    assert!(
        yielded.len() < file.len(),
        "the whole tampered file came out"
    );
    assert_downloaded(&script.requests());

    // 4. Streamed while it arrives: one chunk held, never the file.
    let (feed, arriving) = futures::channel::mpsc::unbounded::<Result<Bytes, TransportError>>();
    script.push_json(200, media_info(URL, &sha256_hex(&file), Some(file.len())));
    script.push_bytes(200, "image/jpeg", Vec::new());
    meta.body(Box::pin(arriving));
    let mut open = streamed(media.download(MEDIA, stream).await.unwrap());
    feed.unbounded_send(Ok(chunks[0].clone())).unwrap();
    feed.unbounded_send(Ok(chunks[1].clone())).unwrap();
    assert_eq!(
        open.next().await,
        Some(Ok(chunks[0].clone())),
        "nothing passed on before the end of the file"
    );
    assert!(
        open.next().now_or_never().is_none(),
        "the last chunk received waits for the next one"
    );
    feed.unbounded_send(Ok(chunks[2].clone())).unwrap();
    assert_eq!(open.next().await, Some(Ok(chunks[1].clone())));
    assert!(open.next().now_or_never().is_none(), "the last chunk waits");
    drop(feed);
    assert_eq!(open.next().await, Some(Ok(chunks[2].clone())));
    assert_eq!(open.next().await, None);
    drop(open);

    // 5. Streamed past `max_bytes`: stopped, nothing of the file out.
    script.push_json(200, media_info(URL, &sha256_hex(&file), None));
    script.push_bytes(200, "image/jpeg", Vec::new());
    meta.chunks(&chunks);
    let capped = DownloadLimits::parse(Some("true"), Some("100000"), STREAM_CAP).unwrap();
    let items = drain(streamed(media.download(MEDIA, capped).await.unwrap())).await;
    assert_eq!(items, [Err(StreamStopped::TooLarge)]);
    // At the limit, it passes.
    script.push_json(200, media_info(URL, &sha256_hex(&file), None));
    script.push_bytes(200, "image/jpeg", Vec::new());
    meta.chunks(&chunks);
    let exact =
        DownloadLimits::parse(Some("true"), Some(&file.len().to_string()), STREAM_CAP).unwrap();
    let items = drain(streamed(media.download(MEDIA, exact).await.unwrap())).await;
    assert_eq!(items.len(), 3);
    assert!(items.iter().all(Result::is_ok));

    // 6. Whole: verified, or not one byte.
    script.push_json(200, media_info(URL, &sha256_hex(&file), Some(file.len())));
    script.push_bytes(200, "image/jpeg", Vec::new());
    meta.chunks(&chunks);
    let downloaded = media.download(MEDIA, whole).await.unwrap();
    let DownloadBody::Whole(data) = downloaded.body else {
        panic!("a stream, where a whole file was asked for")
    };
    assert_eq!(&data[..], &file[..]);
    assert_downloaded(&script.requests());
    script.push_json(200, media_info(URL, &sha256_hex(b"the original"), None));
    script.push_bytes(200, "image/jpeg", Vec::new());
    meta.chunks(&chunks);
    let tampered = media.download(MEDIA, whole).await.unwrap_err();
    assert_eq!(refusal(&tampered), (502, "integrity", None));
    script.push_json(200, media_info(URL, &sha256_hex(&file), None));
    script.push_bytes(200, "image/jpeg", Vec::new());
    meta.chunks(&chunks);
    let capped = DownloadLimits::parse(None, Some("100000"), STREAM_CAP).unwrap();
    let large = media.download(MEDIA, capped).await.unwrap_err();
    assert_eq!(refusal(&large), (413, "media_too_large", None));
    script.push_json(200, media_info(URL, &sha256_hex(&file), None));
    script.push_bytes(200, "image/jpeg", Vec::new());
    meta.chunks(&chunks);
    let exact = DownloadLimits::parse(None, Some(&file.len().to_string()), STREAM_CAP).unwrap();
    assert!(media.download(MEDIA, exact).await.is_ok(), "at the limit");
    assert_eq!(script.remaining(), 0);

    // 7. Before the download: the limits' caps (16 MiB whole, the
    // adapter's streamed), what Meta reports, and the id.
    for (refused, which) in [
        (
            DownloadLimits::parse(None, Some("16777217"), STREAM_CAP),
            "whole",
        ),
        (
            DownloadLimits::parse(Some("true"), Some("104857601"), STREAM_CAP),
            "streamed",
        ),
        (DownloadLimits::parse(None, Some("0"), STREAM_CAP), "none"),
        (
            DownloadLimits::new(MAX_WHOLE_DOWNLOAD + 1, false, STREAM_CAP),
            "whole, as values",
        ),
        (
            DownloadLimits::new(STREAM_CAP + 1, true, STREAM_CAP),
            "streamed, as values",
        ),
        (DownloadLimits::new(0, true, STREAM_CAP), "none, as values"),
    ] {
        assert_eq!(
            refusal(&refused.unwrap_err()),
            (422, "invalid_request", Some("max_bytes")),
            "{which}"
        );
    }
    assert_eq!(
        DownloadLimits::new(STREAM_CAP, true, STREAM_CAP),
        DownloadLimits::parse(Some("true"), None, STREAM_CAP)
    );
    let before = script.requests().len();
    script.push_json(200, media_info(URL, &sha256_hex(&file), Some(100_001)));
    let reported = media.download(MEDIA, capped).await.unwrap_err();
    assert_eq!(refusal(&reported), (413, "media_too_large", None));
    script.push_json(200, media_info(URL, "PHOTO_HASH", None));
    let unusable = media.download(MEDIA, whole).await.unwrap_err();
    assert_eq!(refusal(&unusable), (502, "integrity", None));
    script.push_json(
        200,
        media_info(
            "https://lookaside.fbsbx.com.evil.example/x",
            &sha256_hex(&file),
            None,
        ),
    );
    let elsewhere = media.download(MEDIA, stream).await.unwrap_err();
    assert_eq!(refusal(&elsewhere), (502, "upstream", None));
    let mut other_node = media_info(URL, &sha256_hex(&file), None);
    other_node["id"] = json!("1037543291543637");
    script.push_json(200, other_node);
    let another = media.download(MEDIA, whole).await.unwrap_err();
    assert_eq!(refusal(&another), (404, "not_found", None));
    let requests = script.requests();
    assert_eq!(requests.len(), before + 4, "one lookup each, no download");
    for lookup in &requests[before..] {
        assert_lookup(lookup, "GET", PN);
    }
    script.push_json(
        400,
        json!({"error": {"message": "(#100) Invalid parameter", "type": "OAuthException",
                         "code": 100, "error_data": {"messaging_product": "whatsapp",
                         "details": "MEDIA-DETAILS-SENTINEL"},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = media.download(MEDIA, whole).await.unwrap_err();
    assert_eq!(refusal(&refused), (404, "not_found", None));
    assert!(refused.graph().is_none(), "Meta's code and text answered");
    assert_eq!(told.graph_errors(), ["invalid_parameter"]);
    let before = script.requests().len();
    for id in ["0123", "12a", "1/2", ""] {
        let refused = media.download(id, whole).await.unwrap_err();
        assert_eq!(
            refusal(&refused),
            (422, "invalid_request", Some("media_id")),
            "{id:?}"
        );
    }
    assert_eq!(script.requests().len(), before, "a request was sent");
    assert_eq!(script.remaining(), 0);

    // 8. A slot of the tenant's share first; a stream holds its own.
    let transfers = Slots::new(2);
    let streams = Slots::new(2);
    let narrow = Media::new(
        &authz,
        &number,
        MediaSlots {
            transfers: &transfers,
            streams: &streams,
        },
    )
    .unwrap();
    let held = narrow.transfer_slot().unwrap();
    let before = script.requests().len();
    let busy = narrow.download(MEDIA, whole).await.unwrap_err();
    assert_eq!(refusal(&busy), (429, "too_many_requests", None));
    assert_eq!(busy.retry_after(), Some(1));
    assert!(narrow.transfer_slot().is_err(), "the tenant's share is one");
    drop(held);
    script.push_json(200, media_info(URL, &sha256_hex(&file), None));
    script.push_bytes(200, "image/jpeg", Vec::new());
    meta.chunks(&chunks);
    let open = streamed(narrow.download(MEDIA, stream).await.unwrap());
    let busy = narrow.download(MEDIA, stream).await.unwrap_err();
    assert_eq!(refusal(&busy), (429, "too_many_requests", None));
    assert!(narrow.transfer_slot().is_ok(), "another pool");
    assert_eq!(drain(open).await.len(), 3);
    script.push_json(200, media_info(URL, &sha256_hex(&file), None));
    script.push_bytes(200, "image/jpeg", Vec::new());
    meta.chunks(&chunks);
    let again = streamed(narrow.download(MEDIA, stream).await.unwrap());
    assert_eq!(drain(again).await.len(), 3);
    assert_eq!(script.requests().len(), before + 4);
    assert_eq!(script.remaining(), 0);
    assert!(other_script.requests().is_empty());
}

/// The upload and the deletion as Meta documents them.
///
/// 1. An upload: an [`Upload`] refused before any request when its type
///    is not Meta's or its file larger than its kind allows; else `POST
///    /{pn}/media`, multipart `messaging_product`, `type` and `file` (the
///    inert file name, the type), with the number's token, answering the
///    media id; Meta refusing it answers its error with its `details`,
///    told to the [`Observer`](crate::observe::Observer).
/// 2. A deletion: `GET /{media}?phone_number_id={pn}` first, then `DELETE
///    /{media}?phone_number_id={pn}`, each with the number's token; an id
///    Meta answers for with another node, or refuses, is `404` with no
///    `DELETE`; a media id that is not a Meta id `422` with no request.
///
/// Decisive: the upload's checks skipped fail step 1 (a request sent), as
/// does Meta's `details` dropped; the lookup before the deletion, or the
/// id compared, dropped fails step 2 (a `DELETE`).
pub async fn the_media_services_upload_and_delete_as_meta_documents(backend: &dyn Backend) {
    let meta = Chunked::default();
    let script = meta.script.clone();
    let (authz, number, _) = connected(backend, meta, UPLOADER).await;
    let pn = UPLOADER.2;
    let transfers = Slots::new(8);
    let streams = Slots::new(8);
    let told = Told::default();
    let media = Media::new(
        &authz,
        &number,
        MediaSlots {
            transfers: &transfers,
            streams: &streams,
        },
    )
    .unwrap()
    .observed_by(&told);

    // 1. Uploads, checked first: what an adapter reads of a file (its
    // kind's limit once its type is known, never past its own cap), then
    // the upload itself.
    for (kind, cap, read) in [
        (Some("image/png"), u64::MAX, 5 * 1024 * 1024),
        (Some("image/png"), 1000, 1000),
        (Some("video/mp4"), u64::MAX, 16 * 1024 * 1024),
        (Some("application/pdf"), u64::MAX, 100 * 1024 * 1024),
        (Some("image/gif"), 1000, 1000),
        (None, 1000, 1000),
    ] {
        assert_eq!(upload_cap(kind, cap), read, "{kind:?}");
    }
    for (kind, data, refused) in [
        (
            "image/gif",
            Bytes::from_static(b"GIF89a"),
            (422, "invalid_request", Some("type")),
        ),
        (
            "image/png\r\nX-Injected: 1",
            Bytes::from_static(b"png"),
            (422, "invalid_request", Some("type")),
        ),
        (
            "image/png",
            Bytes::from(vec![0u8; 5 * 1024 * 1024 + 1]),
            (413, "media_too_large", None),
        ),
        (
            "image/webp",
            Bytes::from(vec![0u8; 5 * 1024 * 1024 + 1]),
            (413, "media_too_large", None),
        ),
    ] {
        let error = Upload::new(kind, Some("a.png"), data).unwrap_err();
        assert_eq!(refusal(&error), refused, "{kind:?}");
    }
    // business-phone-numbers/media, "Upload media": `{"id": …}`.
    script.push_json(200, json!({"id": MEDIA}));
    let upload = Upload::new(
        "image/png",
        Some("voucher 1.png"),
        Bytes::from_static(b"\x89PNG the voucher"),
    )
    .unwrap();
    let uploaded = media.upload(&upload).await.unwrap();
    assert_eq!(
        uploaded,
        Uploaded {
            media_id: MEDIA.try_into().unwrap()
        }
    );
    let request = script.last_request().unwrap();
    assert_eq!(
        (request.method.as_str(), request.path()),
        ("POST", format!("/v25.0/{pn}/media").as_str())
    );
    assert_eq!(request.url.query(), None);
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(
        request
            .multipart_field("messaging_product")
            .map(|f| &f.2[..]),
        Some(&b"whatsapp"[..])
    );
    assert_eq!(
        request.multipart_field("type").map(|f| &f.2[..]),
        Some(&b"image/png"[..])
    );
    assert_eq!(
        request.multipart_field("file"),
        Some((
            Some("voucher_1.png"),
            Some("image/png"),
            &Bytes::from_static(b"\x89PNG the voucher")
        ))
    );
    script.push_json(
        400,
        json!({"error": {"message": "(#131053) Media upload error", "type": "OAuthException",
                         "code": 131053, "error_data": {"messaging_product": "whatsapp",
                         "details": "UPLOAD-DETAILS-SENTINEL"},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = media.upload(&upload).await.unwrap_err();
    assert_eq!(
        refused.graph().map(|g| (g.code, g.details.as_deref())),
        Some((131053, Some("UPLOAD-DETAILS-SENTINEL"))),
        "Meta's details"
    );
    assert_eq!(told.graph_errors(), [refused.code()]);
    assert_eq!(script.requests().len(), 2);

    // 2. A deletion: looked up first, as this number's media.
    script.push_json(200, media_info(URL, &sha256_hex(b"x"), None));
    script.push_json(200, json!({"success": true}));
    media.delete(MEDIA).await.unwrap();
    let requests = script.requests();
    let [.., lookup, deleted] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_lookup(lookup, "GET", pn);
    assert_lookup(deleted, "DELETE", pn);
    let before = script.requests().len();
    // Another node (a flow, say), or another id: nothing deleted.
    script.push_json(200, json!({"id": MEDIA, "name": "a flow"}));
    let mut other_id = media_info(URL, &sha256_hex(b"x"), None);
    other_id["id"] = json!("1037543291543637");
    script.push_json(200, other_id);
    script.push_json(
        400,
        json!({"error": {"message": "(#100) Invalid parameter", "type": "OAuthException",
                         "code": 100, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    for which in ["another node", "another id", "refused"] {
        let refused = media.delete(MEDIA).await.unwrap_err();
        assert_eq!(refusal(&refused), (404, "not_found", None), "{which}");
        assert!(refused.graph().is_none(), "{which}: Meta's code answered");
    }
    let requests = script.requests();
    assert_eq!(requests.len(), before + 3, "a DELETE was sent");
    for lookup in &requests[before..] {
        assert_lookup(lookup, "GET", pn);
    }
    for id in ["0123", "x"] {
        let refused = media.delete(id).await.unwrap_err();
        assert_eq!(
            refusal(&refused),
            (422, "invalid_request", Some("media_id"))
        );
    }
    assert_eq!(script.requests().len(), before + 3, "a request was sent");
    assert_eq!(script.remaining(), 0);
}
