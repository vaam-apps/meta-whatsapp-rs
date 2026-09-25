//! Media (scope `media`; docs/design/server.md, section 4.2).
//!
//! | Route | Graph calls, with the tenant's WABA token |
//! | --- | --- |
//! | `POST /v1/numbers/{pn}/media` | `POST /{pn}/media` (multipart: `messaging_product`, `type`, `file`) |
//! | `GET /v1/numbers/{pn}/media/{media_id}` | `GET /{media_id}` (URL, MIME type, SHA-256, size), then the URL (`lookaside.fbsbx.com`, the only other host a token may reach) |
//! | `DELETE /v1/numbers/{pn}/media/{media_id}` | `DELETE /{media_id}` |
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
//! media_too_large`, read no further than the limit when `type` comes
//! first. They take an `Idempotency-Key`.
//!
//! **Downloads** are verified against the SHA-256 Meta reports:
//!
//! - by default the file is read whole (at most `?max_bytes=`, default and
//!   cap 16 MiB), verified, and only then answered: a mismatch is `502
//!   integrity` and **no byte** of the file is sent;
//! - with `?stream=true` (`max_bytes` up to `WA_SERVER_MEDIA_MAX_BYTES`),
//!   bytes are forwarded as they arrive and hashed on the way; a mismatch
//!   (or a file growing past `max_bytes`) **aborts the connection**, so
//!   unverified bytes never arrive as a complete body.
//!
//! Either way `X-WA-SHA256` carries the digest (hex), and a size Meta
//! reports over the limit is `413 media_too_large` before the download.
//! Uploads and whole-file downloads hold memory: at most
//! `WA_SERVER_MEDIA_CONCURRENCY` run at once on a replica, the next is
//! `429 too_many_requests`.
//!
//! Media ids are Meta's: the service checks the number is the tenant's,
//! not the media id's owner (Meta answers for the token's reach).

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

use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{self, Fingerprint, KeyHeader, Success};
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

/// A `type` field: one of Meta's supported MIME types.
fn checked_type(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
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

/// Read and check an upload's form: `type` and the size before any
/// request (and, when `type` comes first, before reading more of the file
/// than its kind allows).
async fn read_upload(max_bytes: u64, request: Request) -> Result<Upload, ApiError> {
    let boundary = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| multer::parse_boundary(v).ok())
        .ok_or_else(|| ApiError::invalid("body"))?;
    let constraints = multer::Constraints::new()
        .allowed_fields(vec!["file", "type"])
        .size_limit(
            multer::SizeLimit::new()
                .whole_stream(max_bytes.saturating_add(FORM_OVERHEAD))
                .for_field("type", MAX_TYPE_LEN)
                .for_field("file", max_bytes),
        );
    let mut form = multer::Multipart::with_constraints(
        request.into_body().into_data_stream(),
        boundary,
        constraints,
    );
    let mut mime_type: Option<String> = None;
    let mut file: Option<(Option<String>, Vec<u8>)> = None;
    while let Some(mut field) = form.next_field().await.map_err(|e| form_error(&e))? {
        match field.name() {
            Some("type") if mime_type.is_none() => {
                let value = field.text().await.map_err(|e| form_error(&e))?;
                mime_type = Some(checked_type(&value)?);
            }
            Some("file") if file.is_none() => {
                let given = field.file_name().map(str::to_owned);
                // A known type caps the file at its kind's size as it
                // streams in.
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

/// A permit for a transfer held in memory, or `429 too_many_requests`.
fn media_permit(state: &AppState) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    state
        .media_permits()
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::too_many_requests(1))
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
        (status = 422, description = "`invalid_request` on `type` (not a supported media type), `file` or `body`; `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's limit, or every media slot of the replica busy", body = ErrorBody),
        (status = 502, description = "Meta failed (`media_upload_failed`, …)", body = ErrorBody),
        (status = 504, description = "`timeout`: the upload may have happened", body = ErrorBody),
    )
)]
pub async fn upload_media(
    State(state): State<AppState>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    uri: Uri,
    request: Request,
) -> Response {
    let permit = match media_permit(&state) {
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
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 413, description = "`media_too_large`: larger than `max_bytes`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `max_bytes` (over 16 MiB without `stream=true`) or `stream`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 502, description = "`integrity` (the digest does not match: nothing of the file was answered), `media_download_failed`, `upstream`", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub async fn download_media(
    State(state): State<AppState>,
    owned: OwnedNumber,
    Path((_, media_id)): Path<(String, String)>,
    query: DownloadQuery,
) -> Result<Response, ApiError> {
    if media_id.trim().is_empty() {
        return Err(ApiError::invalid("media_id"));
    }
    // A whole file is held in memory: take a slot first.
    let permit = if query.stream {
        None
    } else {
        Some(media_permit(&state)?)
    };
    let media = owned.client().media(owned.phone_number_id().clone());
    let info = match media.url(&MediaId::new(media_id)).await {
        Ok(info) => info,
        Err(error) => return Err(owned.failed(&state, &error).await.with_details(&error)),
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
    ];
    if query.stream {
        let body = Body::from_stream(bounded(verified.body, query.max_bytes));
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

/// The verified body, cut (as an error: the connection aborts) past
/// `max_bytes`; nothing follows the first error.
fn bounded(
    body: meta_whatsapp_rs::client::media::VerifiedBody,
    max_bytes: u64,
) -> impl futures::Stream<Item = Result<Bytes, StreamStopped>> + Send + 'static {
    let mut read: u64 = 0;
    body.map(move |chunk| {
        let chunk = chunk.map_err(|error| match error {
            Error::Transport(TransportError::Integrity(_)) => StreamStopped::Integrity,
            _ => StreamStopped::Failed,
        })?;
        read = read.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if read > max_bytes {
            return Err(StreamStopped::TooLarge);
        }
        Ok(chunk)
    })
    .scan(false, |stopped, item| {
        if *stopped {
            return std::future::ready(None);
        }
        if let Err(why) = &item {
            *stopped = true;
            tracing::warn!(reason = %why, "a streamed media download stopped: connection aborted");
        }
        std::future::ready(Some(item))
    })
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
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `media_id`, or Meta's `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub async fn delete_media(
    State(state): State<AppState>,
    owned: OwnedNumber,
    Path((_, media_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    if media_id.trim().is_empty() {
        return Err(ApiError::invalid("media_id"));
    }
    let deleted = owned
        .client()
        .media(owned.phone_number_id().clone())
        .delete(&MediaId::new(media_id))
        .await;
    match deleted {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(error) => Err(owned.failed(&state, &error).await.with_details(&error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_inert_and_have_an_extension() {
        assert_eq!(filename(Some("voucher.png"), "image/png"), "voucher.png");
        assert_eq!(
            filename(Some("a\"b\r\nContent-Type: x.png"), "image/png"),
            "a_b__Content-Type__x.png"
        );
        assert_eq!(filename(None, "image/png"), "upload.png");
        assert_eq!(filename(Some(""), "audio/ogg; codecs=opus"), "upload.ogg");
        assert_eq!(filename(Some("é"), "application/pdf"), "upload.pdf");
        assert_eq!(filename(Some(&"x".repeat(300)), "image/png").len(), 128);
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
