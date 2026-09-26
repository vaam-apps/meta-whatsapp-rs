//! Media (scope `media`; docs/design/server.md, section 4.2).
//!
//! | Route | Graph calls, with the tenant's WABA token |
//! | --- | --- |
//! | `POST /v1/numbers/{pn}/media` | `POST /{pn}/media` (multipart: `messaging_product`, `type`, `file`) |
//! | `GET /v1/numbers/{pn}/media/{media_id}` | `GET /{media_id}?phone_number_id={pn}` (URL, MIME type, SHA-256, size), then the URL (`lookaside.fbsbx.com`, the only other host a token may reach) |
//! | `DELETE /v1/numbers/{pn}/media/{media_id}` | `GET /{media_id}?phone_number_id={pn}` (it must be that media), then `DELETE /{media_id}?phone_number_id={pn}` |
//!
//! Pages: `business-phone-numbers/media`, `reference/media/media-api`,
//! `reference/media/media-download-api`,
//! `reference/whatsapp-business-phone-number/media-upload-api`.
//!
//! **Uploads** are multipart, `file` and `type` (the MIME type, one of
//! Meta's supported types), checked before any request: an unsupported
//! type is `422` on `type`, a file larger than its kind allows (5 MiB for
//! images, 16 MiB for audio and video, 500 KiB for stickers, 100 MiB for
//! documents) or than `WA_SERVER_MEDIA_MAX_BYTES` is `413
//! media_too_large`; when `type` comes first, the body is read no further
//! than its kind's limit (and the form's framing). The form's framing (a
//! preamble, a part's headers) is read up to 16 KiB and a chunk (`422` on
//! `body` past it), and frames already there reach the parser merged into
//! chunks of up to 64 KiB. They take an `Idempotency-Key`.
//!
//! **Downloads** are verified against the SHA-256 Meta reports:
//!
//! - by default the file is read whole (at most `?max_bytes=`, default and
//!   cap 16 MiB), verified, and only then answered: a mismatch is `502
//!   integrity` and **no byte** of the file is sent;
//! - with `?stream=true` (`max_bytes` up to `WA_SERVER_MEDIA_MAX_BYTES`),
//!   bytes are forwarded as they arrive, one chunk behind, and hashed on
//!   the way; a mismatch (or a file growing past `max_bytes`) **aborts the
//!   connection** and the chunk held back is never sent, so unverified
//!   bytes never arrive as a complete body, nor as the whole file.
//!
//! Either way `X-WA-SHA256` carries the digest (hex), the answer is an
//! `attachment` with `nosniff` and a sandboxing `Content-Security-Policy`
//! (a customer's file is never active content), and a size Meta reports
//! over the limit is `413 media_too_large` before the download.
//!
//! Uploads and whole-file downloads hold memory: at most
//! `WA_SERVER_MEDIA_CONCURRENCY` run at once on a replica, streamed
//! downloads at most `WA_SERVER_MEDIA_STREAMS`; one tenant holds half of
//! either at most (one at least), and the next is `429
//! too_many_requests`. An upload takes its slot before its body is read;
//! a stream holds its slot until its body ends.
//!
//! **A media id is the number's, or it does not exist.** It is digits
//! (`422` on `media_id` otherwise), and the node Meta answers for it must
//! be media with that id: a deletion looks it up first, since `DELETE
//! /{id}` deletes whatever node an id names. The number is the
//! tenant's (step 4 of the authorization order); the media id is Meta's,
//! and one token may reach the media of several tenants' numbers (the
//! platform's system user token attached to WABAs of different tenants).
//! So the lookup and the deletion carry `phone_number_id={pn}`
//! (`reference/media/media-api`: Meta then acts only on media of that
//! number), and Meta refusing it is `404 not_found`, the answer for a
//! media id that does not exist, without Meta's code or text: another
//! tenant's media id is never read, downloaded or deleted, and never
//! told apart from a missing one. Meta's pages describe the check for
//! media "uploaded on" the number; that it holds for media received by
//! webhook on it is not documented (docs/guides/server.md, "Media").

use base64::Engine as _;
use futures::StreamExt;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::media::{MediaInfo, MediaKind, validate_upload};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::MediaId;
use meta_whatsapp_rs::webhooks::axum::body::{Body, Bytes};
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequestParts, Path, Query, Request, State};
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use meta_whatsapp_rs::webhooks::axum::http::{
    HeaderName, HeaderValue, Method, StatusCode, Uri, header,
};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use serde::Serialize;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use super::common::graph_id;
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{self, Fingerprint, KeyHeader, Success};
use crate::model::TenantId;
use crate::ratelimit::Slot;
use crate::state::AppState;

/// `X-WA-SHA256`: the downloaded file's SHA-256, hex.
pub static X_WA_SHA256: HeaderName = HeaderName::from_static("x-wa-sha256");

/// Largest file a download answers whole (and the default `max_bytes`):
/// 16 MiB (docs/design/server.md, section 4.2). Larger ones need
/// `?stream=true`.
pub const MAX_WHOLE_DOWNLOAD: u64 = 16 * 1024 * 1024;

/// Room for the multipart framing and the `type` field around the file:
/// an upload's body may be this much larger than
/// `WA_SERVER_MEDIA_MAX_BYTES`.
pub const FORM_OVERHEAD: u64 = 64 * 1024;

/// Longest `type` field.
const MAX_TYPE_LEN: u64 = 255;

/// Longest file name sent to Meta.
const MAX_FILENAME_CHARS: usize = 128;

/// The answer to an upload.
#[derive(Debug, Serialize, ToSchema)]
pub struct MediaUploaded {
    /// The media id (kept by Meta for 30 days): send it in a message's
    /// `id`.
    pub media_id: String,
}

/// An upload's form (`multipart/form-data`).
#[derive(Debug, ToSchema)]
pub struct MediaUpload {
    /// The file.
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
    /// Its MIME type, one of Meta's supported media types (e.g.
    /// `image/png`, `application/pdf`).
    #[schema(rename = "type")]
    pub kind: String,
}

/// A downloaded file's bytes.
#[derive(Debug, ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct MediaBytes(pub Vec<u8>);

// ─── Upload ──────────────────────────────────────────────────────────────

/// A checked upload.
struct Upload {
    mime_type: String,
    filename: String,
    data: Bytes,
}

/// The extension of each supported MIME type, from the table of
/// `business-phone-numbers/media`, for a file sent without a name (Meta
/// reads the extension).
const EXTENSIONS: &[(&str, &str)] = &[
    ("audio/aac", "aac"),
    ("audio/amr", "amr"),
    ("audio/mpeg", "mp3"),
    ("audio/mp4", "m4a"),
    ("audio/ogg", "ogg"),
    ("text/plain", "txt"),
    ("application/vnd.ms-excel", "xls"),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsx",
    ),
    ("application/msword", "doc"),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "docx",
    ),
    ("application/vnd.ms-powerpoint", "ppt"),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "pptx",
    ),
    ("application/pdf", "pdf"),
    ("image/jpeg", "jpeg"),
    ("image/png", "png"),
    ("image/webp", "webp"),
    ("video/3gpp", "3gp"),
    ("video/mp4", "mp4"),
];

/// The file name sent to Meta: the caller's, reduced to `A-Z a-z 0-9 . _
/// -` (anything else is `_`: the name goes into a multipart header) and
/// 128 characters; `upload.<extension>` when there is none.
fn filename(given: Option<&str>, mime_type: &str) -> String {
    let cleaned: String = given
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(MAX_FILENAME_CHARS)
        .collect();
    if cleaned.trim_matches(['.', '_']).is_empty() {
        let essence = mime_type.split(';').next().unwrap_or_default().trim();
        let extension = EXTENSIONS
            .iter()
            .find(|(m, _)| m.eq_ignore_ascii_case(essence))
            .map_or("bin", |(_, e)| e);
        format!("upload.{extension}")
    } else {
        cleaned
    }
}

/// A `type` field: one of Meta's supported MIME types, without a control
/// character (it becomes a header of the part Meta receives).
fn checked_type(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if !value.bytes().all(|b| b == b' ' || b.is_ascii_graphic()) {
        return Err(ApiError::invalid("type"));
    }
    MediaKind::for_mime_type(value)
        .map(|_| value.to_owned())
        .ok_or_else(|| ApiError::invalid("type"))
}

fn form_error(error: &multer::Error) -> ApiError {
    match error {
        multer::Error::FieldSizeExceeded { field_name, .. }
            if field_name.as_deref() == Some("file") =>
        {
            ApiError::new("media_too_large")
        }
        multer::Error::FieldSizeExceeded { field_name, .. }
            if field_name.as_deref() == Some("type") =>
        {
            ApiError::invalid("type")
        }
        multer::Error::StreamSizeExceeded { .. } => ApiError::new("payload_too_large"),
        _ => ApiError::invalid("body"),
    }
}

/// Frames already there are merged into chunks of up to this many bytes
/// before multer sees them: multer scans what it holds on every chunk, so
/// a body sent as tiny frames would cost it work per frame.
pub const FORM_CHUNK: usize = 64 * 1024;

/// Most bytes read while waiting for a part's headers (the form's
/// preamble, a part's headers), past one chunk: a form is `type` and
/// `file`, whose framing is a few hundred bytes.
pub const FORM_FRAMING: u64 = 16 * 1024;

/// Whether the reader is waiting for a part's headers, how much it read
/// meanwhile (see [`FORM_FRAMING`]), and whether the budget was met once
/// already.
#[derive(Debug, Default)]
struct Framing {
    armed: std::sync::atomic::AtomicBool,
    read: std::sync::atomic::AtomicU64,
    spent: std::sync::atomic::AtomicBool,
}

impl Framing {
    fn arm(&self) {
        self.read.store(0, std::sync::atomic::Ordering::SeqCst);
        self.spent.store(false, std::sync::atomic::Ordering::SeqCst);
        self.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn disarm(&self) {
        self.armed.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The form's framing ran past [`FORM_FRAMING`].
#[derive(Debug, thiserror::Error)]
#[error("the form's framing is too long")]
struct FramingTooLong;

/// An upload's body as multer reads it:
///
/// - **one chunk at a time**: after each, it answers "not yet" once (and
///   wakes its reader at once). multer reads every chunk already there
///   before it parses any; without the pause, a client sending faster than
///   the form is parsed would have the whole body read, whatever limit
///   parsing it finds;
/// - **frames merged** into chunks of up to [`FORM_CHUNK`] bytes, from
///   what is already there (a body of one-byte frames is not parsed a
///   byte at a time);
/// - **framing bounded**: while the reader waits for a part's headers,
///   at most [`FORM_FRAMING`] bytes and a chunk are read.
struct FormBody<S> {
    inner: S,
    paused: bool,
    ended: bool,
    framing: std::sync::Arc<Framing>,
}

impl<S, E> futures::Stream for FormBody<S>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Item = Result<Bytes, Box<dyn std::error::Error + Send + Sync>>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::sync::atomic::Ordering;
        use std::task::Poll;
        // The pause comes first, the end included: multer, handed a chunk
        // and the end in one read of the stream, takes a form whose first
        // boundary it has not looked for yet as incomplete.
        if !self.paused {
            self.paused = true;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        if self.ended {
            return Poll::Ready(None);
        }
        if self.framing.armed.load(Ordering::SeqCst)
            && self.framing.read.load(Ordering::SeqCst) >= FORM_FRAMING
        {
            // multer looks at what it holds after each read of the stream:
            // the first time, "not yet", so that it parses what it has;
            // asked again, it needs more, and the framing is too long.
            if self.framing.spent.swap(true, Ordering::SeqCst) {
                self.ended = true;
                return Poll::Ready(Some(Err(Box::new(FramingTooLong))));
            }
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        let mut merged: Vec<u8> = Vec::new();
        while merged.len() < FORM_CHUNK {
            match self.inner.poll_next_unpin(cx) {
                Poll::Pending if merged.is_empty() => return Poll::Pending,
                Poll::Pending => break,
                Poll::Ready(Some(Ok(frame))) => merged.extend_from_slice(&frame),
                Poll::Ready(Some(Err(error))) => {
                    self.ended = true;
                    return Poll::Ready(Some(Err(error.into())));
                }
                Poll::Ready(None) => {
                    self.ended = true;
                    if merged.is_empty() {
                        return Poll::Ready(None);
                    }
                    break;
                }
            }
        }
        self.paused = false;
        if self.framing.armed.load(Ordering::SeqCst) {
            self.framing.read.fetch_add(
                u64::try_from(merged.len()).unwrap_or(u64::MAX),
                Ordering::SeqCst,
            );
        }
        Poll::Ready(Some(Ok(Bytes::from(merged))))
    }
}

/// Read and check an upload's form: `type` and the size before any
/// request. When `type` comes first, the body is read no further than the
/// file's kind allows (and a chunk).
async fn read_upload(max_bytes: u64, request: Request) -> Result<Upload, ApiError> {
    let boundary = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| multer::parse_boundary(v).ok())
        .ok_or_else(|| ApiError::invalid("body"))?;
    let framing = std::sync::Arc::new(Framing::default());
    let body = FormBody {
        inner: request.into_body().into_data_stream(),
        paused: false,
        ended: false,
        framing: framing.clone(),
    };
    let constraints = multer::Constraints::new()
        .allowed_fields(vec!["file", "type"])
        .size_limit(
            multer::SizeLimit::new()
                .whole_stream(max_bytes.saturating_add(FORM_OVERHEAD))
                .for_field("type", MAX_TYPE_LEN)
                .for_field("file", max_bytes),
        );
    let mut form = multer::Multipart::with_constraints(body, boundary, constraints);
    let mut mime_type: Option<String> = None;
    let mut file: Option<(Option<String>, Vec<u8>)> = None;
    loop {
        framing.arm();
        let next = form.next_field().await;
        framing.disarm();
        let Some(mut field) = next.map_err(|e| form_error(&e))? else {
            break;
        };
        match field.name() {
            Some("type") if mime_type.is_none() => {
                let value = field.text().await.map_err(|e| form_error(&e))?;
                mime_type = Some(checked_type(&value)?);
            }
            Some("file") if file.is_none() => {
                let given = field.file_name().map(str::to_owned);
                // A known type caps the file at its kind's size as it
                // streams in: no more of the body is read.
                let cap = mime_type
                    .as_deref()
                    .and_then(MediaKind::for_mime_type)
                    .map_or(max_bytes, |kind| kind.max_bytes().min(max_bytes));
                let mut data = Vec::new();
                while let Some(chunk) = field.chunk().await.map_err(|e| form_error(&e))? {
                    if u64::try_from(data.len() + chunk.len()).unwrap_or(u64::MAX) > cap {
                        return Err(ApiError::new("media_too_large"));
                    }
                    data.extend_from_slice(&chunk);
                }
                file = Some((given, data));
            }
            Some("type") => return Err(ApiError::invalid("type")),
            Some("file") => return Err(ApiError::invalid("file")),
            _ => return Err(ApiError::invalid("body")),
        }
    }
    let mime_type = mime_type.ok_or_else(|| ApiError::invalid("type"))?;
    let (given, data) = file.ok_or_else(|| ApiError::invalid("file"))?;
    // The library's own check, the one `upload` repeats: Meta's table.
    if let Err(refused) = validate_upload(&mime_type, u64::try_from(data.len()).unwrap_or(u64::MAX))
    {
        return Err(if refused.field == "type" {
            ApiError::invalid("type")
        } else {
            ApiError::new("media_too_large")
        });
    }
    Ok(Upload {
        filename: filename(given.as_deref(), &mime_type),
        mime_type,
        data: Bytes::from(data),
    })
}

/// Upload with the number's token: `201` with the media id.
async fn upload(
    state: &AppState,
    owned: &OwnedNumber,
    upload: &Upload,
) -> Result<Success, ApiError> {
    let uploaded = owned
        .client()
        .media(owned.phone_number_id().clone())
        .upload(upload.data.clone(), &upload.mime_type, &upload.filename)
        .await;
    match uploaded {
        Ok(id) => Success::json(
            StatusCode::CREATED,
            &MediaUploaded {
                media_id: id.into_inner(),
            },
        ),
        Err(error) => Err(owned.failed(state, &error).await.with_details(&error)),
    }
}

/// A slot for a transfer held in memory, or `429 too_many_requests` when
/// the tenant's share or the replica's slots are taken.
fn media_slot(state: &AppState, tenant: &TenantId) -> Result<Slot, ApiError> {
    state
        .media_slots()
        .try_acquire(tenant)
        .ok_or_else(|| ApiError::too_many_requests(1))
}

/// `POST /v1/numbers/{pn}/media`: upload a file.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/media",
    tag = "media",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and file never upload twice (a repeat gets the kept answer, with `Idempotent-Replayed: true`)"),
    ),
    request_body(content = MediaUpload, content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "Uploaded", body = MediaUploaded),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 413, description = "`media_too_large`: larger than its type allows or `WA_SERVER_MEDIA_MAX_BYTES`; `payload_too_large`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `type` (not a supported media type), `file` or `body` (not a form of `type` and `file`, or its framing past 16 KiB); `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's rate limit, or its share of the replica's media slots busy", body = ErrorBody),
        (status = 502, description = "Meta failed (`media_upload_failed`, …)", body = ErrorBody),
        (status = 504, description = "`timeout`: the upload may have happened", body = ErrorBody),
    )
)]
pub(crate) async fn upload_media(
    State(state): State<AppState>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    uri: Uri,
    request: Request,
) -> Response {
    // The slot is taken before the body is read: a body that trickles in
    // holds one of its tenant's slots, never another tenant's.
    let permit = match media_slot(&state, caller.tenant()) {
        Ok(permit) => permit,
        Err(error) => return error.into_response(),
    };
    let form = match read_upload(state.settings().media_max_bytes, request).await {
        Ok(form) => form,
        Err(error) => return error.into_response(),
    };
    let digest = Sha256::digest(&form.data);
    let fingerprint = Fingerprint::parts(
        &Method::POST,
        uri.path(),
        &[
            ("type", form.mime_type.as_bytes()),
            ("filename", form.filename.as_bytes()),
            ("file", digest.as_slice()),
        ],
    );
    let response = idempotency::run(
        &state,
        caller.tenant(),
        key,
        fingerprint,
        upload(&state, &owned, &form),
    )
    .await;
    drop(permit);
    response
}

// ─── Download ────────────────────────────────────────────────────────────

/// `?max_bytes=` and `?stream=`, as the OpenAPI document declares them
/// ([`DownloadQuery`] reads them).
#[derive(Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DownloadParams {
    /// Largest file accepted, in bytes: default and cap 16 MiB; with
    /// `stream=true`, default and cap `WA_SERVER_MEDIA_MAX_BYTES`.
    #[param(minimum = 1)]
    pub max_bytes: Option<u64>,
    /// Forward the bytes as they arrive (a mismatch aborts the
    /// connection) instead of answering a verified whole file.
    pub stream: Option<bool>,
}

/// A download's limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadQuery {
    /// Largest file accepted.
    pub max_bytes: u64,
    /// Stream it.
    pub stream: bool,
}

impl FromRequestParts<AppState> for DownloadQuery {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let Query(query) =
            Query::<std::collections::HashMap<String, String>>::from_request_parts(parts, state)
                .await
                .map_err(|_| ApiError::invalid("query"))?;
        let stream = match query.get("stream").map(String::as_str) {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => return Err(ApiError::invalid("stream")),
        };
        let cap = if stream {
            state.settings().media_max_bytes
        } else {
            MAX_WHOLE_DOWNLOAD
        };
        let max_bytes = match query.get("max_bytes") {
            None => cap,
            Some(value) => value
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=cap).contains(n))
                .ok_or_else(|| ApiError::invalid("max_bytes"))?,
        };
        Ok(Self { max_bytes, stream })
    }
}

/// A SHA-256 as Meta reports it, hex (64 characters) or base64, as the
/// library reads it.
fn reported_sha256(value: &str) -> Option<[u8; 32]> {
    let value = value.trim();
    let bytes = if value.len() == 64 {
        hex::decode(value).ok()?
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .ok()?
    };
    bytes.try_into().ok()
}

/// The content type to answer: Meta's, when it is a plain MIME type.
fn content_type(info: &MediaInfo) -> HeaderValue {
    let valid = info.mime_type.contains('/')
        && info
            .mime_type
            .bytes()
            .all(|b| b.is_ascii_graphic() || b == b' ');
    valid
        .then(|| HeaderValue::from_str(&info.mime_type).ok())
        .flatten()
        .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream"))
}

/// A failed download step: the lookaside URL Meta gave being refused by
/// the library's host allow-list is Meta's answer gone wrong (`upstream`),
/// not the caller's input.
async fn download_failed(state: &AppState, owned: &OwnedNumber, error: &Error) -> ApiError {
    if matches!(error, Error::Validation(_)) {
        tracing::warn!("Meta's media URL was refused by the library's host allow-list");
        return ApiError::new("upstream");
    }
    owned.failed(state, error).await.with_details(error)
}

/// `GET /v1/numbers/{pn}/media/{media_id}`: download a file, verified
/// against Meta's SHA-256.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/media/{media_id}",
    tag = "media",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("media_id" = String, Path, description = "Media id (an upload's, or a received message's)"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        DownloadParams,
    ),
    responses(
        (status = 200, description = "The file, with Meta's MIME type as `Content-Type`; whole and verified, or streamed (`stream=true`: a digest mismatch aborts the connection)", content_type = "application/octet-stream", body = MediaBytes,
            headers(("X-WA-SHA256" = String, description = "The file's SHA-256, hex"))),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such media on this number (another number's media id looks like a missing one)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 413, description = "`media_too_large`: larger than `max_bytes`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `media_id` (digits), `max_bytes` (over 16 MiB without `stream=true`) or `stream`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's rate limit, or its share of the replica's media (or stream) slots busy", body = ErrorBody),
        (status = 502, description = "`integrity` (the digest does not match: nothing of the file was answered), `media_download_failed`, `upstream`", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn download_media(
    State(state): State<AppState>,
    caller: Caller,
    owned: OwnedNumber,
    Path((_, media_id)): Path<(String, String)>,
    query: DownloadQuery,
) -> Result<Response, ApiError> {
    graph_id("media_id", &media_id)?;
    // A whole file is held in memory, a stream holds two connections:
    // take a slot of the tenant's share first.
    let permit = if query.stream {
        state
            .stream_slots()
            .try_acquire(caller.tenant())
            .ok_or_else(|| ApiError::too_many_requests(1))?
    } else {
        media_slot(&state, caller.tenant())?
    };
    let media = owned
        .client()
        .media(owned.phone_number_id().clone())
        .restrict_to_phone_number();
    let info = match media.url(&MediaId::new(media_id.as_str())).await {
        // A media node, the one asked for.
        Ok(info) if info.id.as_str() == media_id => info,
        Ok(_) => return Err(ApiError::not_found()),
        Err(error) => return Err(owned.failed_on_object(&state, &error).await),
    };
    // Meta's reported size, before a byte is downloaded (checked again
    // while reading: the report may be wrong).
    if info.file_size.is_some_and(|size| size > query.max_bytes) {
        return Err(ApiError::new("media_too_large"));
    }
    let Some(digest) = reported_sha256(&info.sha256) else {
        tracing::warn!("Meta reported no usable SHA-256 for a media file");
        return Err(ApiError::new("integrity"));
    };
    let content_type = content_type(&info);
    let download = match media.download_with_info(info).await {
        Ok(download) => download,
        Err(error) => return Err(download_failed(&state, &owned, &error).await),
    };
    let verified = download
        .verified()
        .map_err(|_| ApiError::new("integrity"))?;
    let headers = [
        (header::CONTENT_TYPE, content_type),
        (
            X_WA_SHA256.clone(),
            HeaderValue::from_str(&hex::encode(digest)).map_err(|_| ApiError::internal())?,
        ),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        ),
        // A customer's file is never active content, whatever its type (an
        // HTML or SVG document shown by a CMS that drops the attachment).
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("sandbox; default-src 'none'"),
        ),
    ];
    if query.stream {
        let body = Body::from_stream(HeldBack {
            inner: verified.body,
            held: None,
            read: 0,
            max_bytes: query.max_bytes,
            done: false,
            _slot: permit,
        });
        return Ok((StatusCode::OK, headers, body).into_response());
    }
    let mut body = verified.body;
    let mut data = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|error| ApiError::from_library(&error))?;
        if u64::try_from(data.len() + chunk.len()).unwrap_or(u64::MAX) > query.max_bytes {
            return Err(ApiError::new("media_too_large"));
        }
        data.extend_from_slice(&chunk);
    }
    // Verified: the stream ended without the library's integrity error.
    drop(permit);
    Ok((StatusCode::OK, headers, data).into_response())
}

/// Why a streamed download stopped: the connection is aborted, so the
/// caller never sees a complete body.
#[derive(Debug, thiserror::Error)]
enum StreamStopped {
    #[error("the media failed its integrity check")]
    Integrity,
    #[error("the media grew past max_bytes")]
    TooLarge,
    #[error("the media download failed")]
    Failed,
}

/// The verified body of a streamed download, forwarded **one chunk
/// behind**: the last chunk goes out only once the stream ended with the
/// digest matching, so even a client that ignores the aborted connection
/// never holds the whole of a tampered file. Past `max_bytes`, or on a
/// mismatch or a failure, it errs (the connection aborts), the held chunk
/// is dropped, and nothing follows. It holds the download's stream slot
/// until it ends.
struct HeldBack {
    inner: meta_whatsapp_rs::client::media::VerifiedBody,
    held: Option<Bytes>,
    read: u64,
    max_bytes: u64,
    done: bool,
    _slot: Slot,
}

impl HeldBack {
    fn stop(&mut self, why: StreamStopped) -> Result<Bytes, StreamStopped> {
        self.done = true;
        self.held = None;
        tracing::warn!(reason = %why, "a streamed media download stopped: connection aborted");
        Err(why)
    }
}

impl futures::Stream for HeldBack {
    type Item = Result<Bytes, StreamStopped>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        loop {
            if self.done {
                return Poll::Ready(None);
            }
            let chunk = match self.inner.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    // Verified: the digest matched.
                    self.done = true;
                    return Poll::Ready(self.held.take().map(Ok));
                }
                Poll::Ready(Some(Err(Error::Transport(TransportError::Integrity(_))))) => {
                    return Poll::Ready(Some(self.stop(StreamStopped::Integrity)));
                }
                Poll::Ready(Some(Err(_))) => {
                    return Poll::Ready(Some(self.stop(StreamStopped::Failed)));
                }
                Poll::Ready(Some(Ok(chunk))) => chunk,
            };
            self.read = self
                .read
                .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if self.read > self.max_bytes {
                return Poll::Ready(Some(self.stop(StreamStopped::TooLarge)));
            }
            if chunk.is_empty() {
                continue;
            }
            if let Some(previous) = self.held.replace(chunk) {
                return Poll::Ready(Some(Ok(previous)));
            }
        }
    }
}

// ─── Delete ──────────────────────────────────────────────────────────────

/// `DELETE /v1/numbers/{pn}/media/{media_id}`: delete an uploaded file.
#[utoipa::path(
    delete,
    path = "/v1/numbers/{pn}/media/{media_id}",
    tag = "media",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("media_id" = String, Path, description = "Media id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such media on this number (another number's media id looks like a missing one)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `media_id` (digits)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn delete_media(
    State(state): State<AppState>,
    owned: OwnedNumber,
    Path((_, media_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    graph_id("media_id", &media_id)?;
    let media = owned
        .client()
        .media(owned.phone_number_id().clone())
        .restrict_to_phone_number();
    let id = MediaId::new(media_id.as_str());
    // `DELETE /{id}` deletes whatever node the id names (a flow, a QR
    // code) that the token reaches: look it up first, as this number's
    // media, and delete only that.
    match media.url(&id).await {
        Ok(info) if info.id == id => {}
        Ok(_) => return Err(ApiError::not_found()),
        Err(error) => return Err(owned.failed_on_object(&state, &error).await),
    }
    let deleted = media.delete(&id).await;
    match deleted {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(error) => Err(owned.failed_on_object(&state, &error).await),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_inert_and_have_an_extension() {
        assert_eq!(filename(Some("voucher.png"), "image/png"), "voucher.png");
        assert_eq!(
            filename(Some("a;b\r\nContent-Type: x.png"), "image/png"),
            "a_b__Content-Type__x.png"
        );
        // A quote would end the multipart header's `filename="…"`.
        assert_eq!(
            filename(Some("a\"b\r\nContent-Type: x.png"), "image/png"),
            "a_b__Content-Type__x.png"
        );
        assert_eq!(filename(None, "image/png"), "upload.png");
        assert_eq!(filename(Some(""), "audio/ogg; codecs=opus"), "upload.ogg");
        assert_eq!(filename(Some("é"), "application/pdf"), "upload.pdf");
        assert_eq!(filename(Some(&"x".repeat(300)), "image/png").len(), 128);
    }

    /// Frames already there reach multer merged, up to [`FORM_CHUNK`]
    /// bytes: a body of one-byte frames is not parsed a byte at a time.
    /// Decisive: the merge.
    #[tokio::test]
    async fn ready_frames_are_merged_before_multer_sees_them() {
        let frames = futures::stream::iter(
            (0..FORM_CHUNK + 10).map(|_| Ok::<_, std::io::Error>(Bytes::from_static(b"x"))),
        );
        let body = FormBody {
            inner: frames,
            paused: false,
            ended: false,
            framing: std::sync::Arc::new(Framing::default()),
        };
        let chunks: Vec<usize> = body.map(|chunk| chunk.unwrap().len()).collect().await;
        assert_eq!(chunks, [FORM_CHUNK, 10]);
    }

    #[test]
    fn meta_digests_are_read_in_hex_or_base64() {
        let hex = "3f9d94d399fa61c191bc1d4ca71375a035cd9b9f5b1128e1f0963a415c16b0cc";
        let raw = reported_sha256(hex).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        assert_eq!(reported_sha256(&b64), Some(raw));
        assert_eq!(reported_sha256("PHOTO_HASH"), None);
        assert_eq!(reported_sha256(""), None);
    }

    #[test]
    fn the_extension_table_is_metas_supported_types() {
        for (mime, _) in EXTENSIONS {
            assert!(MediaKind::for_mime_type(mime).is_some(), "{mime}");
        }
        assert_eq!(EXTENSIONS.len(), 18);
    }
}
