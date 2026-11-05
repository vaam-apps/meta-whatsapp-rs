//! Resumable uploads (scope `media`; docs/design/server.md, section 4.2;
//! roadmap M5c1): Meta's Resumable Upload API, whose handle a template's
//! `header_handle` and, from roadmap M5c2, a profile picture's
//! `profile_picture_handle` take.
//!
//! | Route | Graph call, with the tenant's WABA token | `Idempotency-Key` |
//! | --- | --- | --- |
//! | `POST /v1/numbers/{pn}/media-uploads` | `POST /{app_id}/uploads?file_name&file_length&file_type` | yes |
//! | `POST /v1/numbers/{pn}/media-uploads/{upload_id}?file_offset=N` | `POST /upload:<session>` (header `file_offset`, the bytes as the body) | no: the offset is its identity |
//! | `GET /v1/numbers/{pn}/media-uploads/{upload_id}` | `GET /upload:<session>`, none once the handle is kept | no |
//!
//! The paths and the id are the service's ([`meta_whatsapp_server_core::uploads`]
//! says how the session is kept, bounded and owned): the id answered is
//! the service's own, never Meta's session id, and a session of another
//! tenant, another number or an expired one is `404 not_found` with
//! nothing asked of Meta. This module keeps the routes, the chunk's body
//! reading (a raw body, read no further than the declared file's end, one
//! of the tenant's transfer slots held meanwhile) and the answer types
//! with their OpenAPI schema.
//!
//! **The handle** is `handle` in the answer of the chunk that completes the
//! file (and of the status after it): the value for a template's
//! `header_handle` and, from roadmap M5c2, a profile picture's
//! `profile_picture_handle`. It is the tenant's to keep; the service
//! keeps it 24 hours from the session's start
//! ([`meta_whatsapp_server_core::uploads::SESSION_TTL`]) and logs it
//! nowhere.

use futures::StreamExt as _;
use meta_whatsapp_rs::webhooks::axum;
use meta_whatsapp_rs::webhooks::axum::body::Bytes;
use meta_whatsapp_rs::webhooks::axum::extract::{
    FromRef, FromRequestParts, Path, Query, Request, State,
};
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::uploads::{self as domain, NewUpload, Uploads};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::common::ApiJson;
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody, ErrorCode};
use crate::idempotency::{self, Fingerprint, KeyHeader, Success};
use crate::state::{AppState, Grant};

/// The resumable upload routes, each with its documentation (`routes!`):
/// part of [`super::media::routes`], behind the same scope.
pub(crate) fn routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(start_media_upload))
        .routes(routes!(upload_media_chunk, get_media_upload))
}

/// `POST /v1/numbers/{pn}/media-uploads`: the file to upload in chunks.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaUploadStart {
    /// The file's name (reduced to `A-Z a-z 0-9 . _ -`, 128 characters).
    pub file_name: String,
    /// The file's length in bytes: at least 1, and at most its type's
    /// limit in Meta's media table (5 MiB for an image, 16 MiB for a video,
    /// 100 MiB for a document) and `WA_SERVER_MEDIA_MAX_BYTES`. Meta's
    /// Resumable Upload API states no limit of its own.
    pub file_length: u64,
    /// `application/pdf`, `image/jpeg`, `image/jpg`, `image/png` or
    /// `video/mp4`: the types the Resumable Upload API takes.
    pub file_type: String,
}

/// A resumable upload's progress.
#[derive(Debug, Serialize, ToSchema)]
pub struct MediaUploadProgress {
    /// The upload's id (the service's own): the path of its chunks and
    /// its status.
    pub upload_id: String,
    /// The file name sent to Meta.
    pub file_name: String,
    /// The declared length.
    pub file_length: u64,
    /// The declared MIME type.
    pub file_type: String,
    /// The bytes Meta holds: where the next chunk starts
    /// (`file_offset`).
    pub file_offset: u64,
    /// Whether Meta has the whole file: `handle` is then answered.
    pub complete: bool,
    /// The upload's handle, once complete: a template's `header_handle`,
    /// and a profile picture's `profile_picture_handle` (roadmap M5c2).
    /// Kept by the service 24 hours from the upload's start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

impl From<domain::Progress> for MediaUploadProgress {
    fn from(progress: domain::Progress) -> Self {
        let complete = progress.complete();
        let domain::Progress {
            upload_id,
            file_name,
            file_length,
            file_type,
            file_offset,
            handle,
        } = progress;
        Self {
            upload_id,
            file_name,
            file_length,
            file_type,
            file_offset,
            complete,
            handle: handle.map(meta_whatsapp_rs::core::ids::UploadHandle::into_inner),
        }
    }
}

/// The core's resumable upload operations on `owned`, with `state`'s
/// sessions, uploads' slots and app (`owned` is the state's own: the
/// extractor made it with `state.authz()`, which admits it).
fn uploads<'a>(state: &'a AppState, owned: &'a OwnedNumber) -> Result<Uploads<'a>, ApiError> {
    Ok(Uploads::new(
        state.authz(),
        owned.core(),
        state.upload_sessions(),
        state.settings().app_id.as_ref(),
        state.media_slots(),
    )?
    .observed_by(state.metrics()))
}

// ─── Start ───────────────────────────────────────────────────────────────

/// `POST /v1/numbers/{pn}/media-uploads`: open a resumable upload.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/media-uploads",
    tag = "media",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never open two sessions (a repeat gets the kept answer, with `Idempotent-Replayed: true`)"),
    ),
    request_body = MediaUploadStart,
    responses(
        (status = 201, description = "Opened: send the file's bytes to `POST /v1/numbers/{pn}/media-uploads/{upload_id}?file_offset=0`", body = MediaUploadProgress),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`resumable_upload_not_configured` (the deployment sets no `WA_APP_ID`), `number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 413, description = "`media_too_large`: `file_length` over its type's limit or `WA_SERVER_MEDIA_MAX_BYTES`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `file_name` (blank), `file_type` (not one of the five), `file_length` (zero) or `body`; `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 502, description = "Meta failed, or answered a session id that is none of its `upload:` ids (`upstream`)", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`: Meta opened a session no record names (it expires unused)", body = ErrorBody),
        (status = 504, description = "`timeout`: a session may have been opened", body = ErrorBody),
    )
)]
pub(crate) async fn start_media_upload(
    State(state): State<AppState>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<MediaUploadStart>,
) -> Response {
    let MediaUploadStart {
        file_name,
        file_length,
        file_type,
    } = body;
    let new = match NewUpload::new(
        &file_name,
        file_length,
        &file_type,
        state.settings().media_max_bytes,
    ) {
        Ok(new) => new,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let fingerprint = Fingerprint::start_media_upload(owned.phone_number_id(), &new);
    let operation = async {
        let progress = uploads(&state, &owned)?.start(&new).await?;
        Success::json(StatusCode::CREATED, &MediaUploadProgress::from(progress))
    };
    idempotency::run(&state, &caller, key, fingerprint, operation).await
}

// ─── Chunk ───────────────────────────────────────────────────────────────

/// `?file_offset=`, as the OpenAPI document declares it
/// ([`ChunkQuery`] reads it).
#[derive(Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ChunkParams {
    /// Where the chunk starts in the file, in bytes: 0 for the first, then
    /// the `file_offset` of the previous answer (or of the status, to
    /// resume). Before the end of the declared file.
    pub file_offset: u64,
}

/// A chunk's `file_offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkQuery(pub u64);

impl<S> FromRequestParts<S> for ChunkQuery
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        let Query(query) =
            Query::<std::collections::HashMap<String, String>>::from_request_parts(parts, state)
                .await
                .map_err(|_| ApiError::invalid("query"))?;
        query
            .get("file_offset")
            .and_then(|offset| offset.parse::<u64>().ok())
            .map(Self)
            .ok_or_else(|| ApiError::invalid("file_offset"))
    }
}

/// A chunk's body, raw, read no further than `room` bytes: one more is
/// `413 media_too_large`; none is `422` on `body`.
async fn read_chunk(room: u64, request: Request) -> Result<Bytes, ApiError> {
    let mut stream = request.into_body().into_data_stream();
    let mut data: Vec<u8> = Vec::new();
    while let Some(frame) = stream.next().await {
        let frame = frame.map_err(|_| ApiError::invalid("body"))?;
        if u64::try_from(data.len() + frame.len()).unwrap_or(u64::MAX) > room {
            return Err(ApiError::new(ErrorCode::MEDIA_TOO_LARGE));
        }
        data.extend_from_slice(&frame);
    }
    if data.is_empty() {
        return Err(ApiError::invalid("body"));
    }
    Ok(Bytes::from(data))
}

/// `POST /v1/numbers/{pn}/media-uploads/{upload_id}`: send a chunk.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/media-uploads/{upload_id}",
    tag = "media",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("upload_id" = String, Path, description = "The upload's id, from `POST /v1/numbers/{pn}/media-uploads`"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ChunkParams,
    ),
    request_body(content = Vec<u8>, content_type = "application/octet-stream", description = "The bytes of the file from `file_offset` on, to the end of the declared file at most. The whole file in one chunk is the common case."),
    responses(
        (status = 200, description = "Received: `file_offset` is where the next chunk starts; `complete` and `handle` once Meta has the whole file", body = MediaUploadProgress),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such upload on this number (another number's or tenant's, an expired one)", body = ErrorBody),
        (status = 409, description = "`resumable_upload_not_configured`, `number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 413, description = "`media_too_large`: past the end of the declared file", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `upload_id` (not an id of the service's, or complete already), `file_offset` (missing, or not before the end of the declared file) or `body` (empty)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's rate limit, or its share of the replica's media slots busy", body = ErrorBody),
        (status = 502, description = "Meta failed, or the file is complete and Meta answered no handle (`upstream`)", body = ErrorBody),
        (status = 504, description = "`timeout`: the chunk may have arrived; ask the status for the offset Meta holds, and resume from it", body = ErrorBody),
    )
)]
pub(crate) async fn upload_media_chunk(
    State(state): State<AppState>,
    owned: OwnedNumber,
    Path((_, upload_id)): Path<(String, String)>,
    ChunkQuery(file_offset): ChunkQuery,
    request: Request,
) -> Result<Response, ApiError> {
    let uploads = uploads(&state, &owned)?;
    // The session first: another tenant's, another number's, an expired
    // one is `404` before a slot is taken, a byte read or Meta asked.
    let open = uploads.open(&upload_id).await?;
    let room = open.max_chunk(file_offset)?;
    // A body that trickles in holds one of its own tenant's slots.
    let slot = uploads.transfer_slot()?;
    let data = read_chunk(room.min(state.settings().media_max_bytes), request).await?;
    let progress = uploads.chunk(&open, file_offset, data).await;
    drop(slot);
    let body = Success::json(StatusCode::OK, &MediaUploadProgress::from(progress?))?;
    Ok(body.into_response())
}

// ─── Status ──────────────────────────────────────────────────────────────

/// `GET /v1/numbers/{pn}/media-uploads/{upload_id}`: where an upload
/// stands.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/media-uploads/{upload_id}",
    tag = "media",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("upload_id" = String, Path, description = "The upload's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The offset Meta holds (resume from it), or the handle when the upload is complete (answered from the service's record, without asking Meta)", body = MediaUploadProgress),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such upload on this number", body = ErrorBody),
        (status = 409, description = "`resumable_upload_not_configured`, `number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `upload_id` (not an id of the service's)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_media_upload(
    State(state): State<AppState>,
    owned: OwnedNumber,
    Path((_, upload_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let uploads = uploads(&state, &owned)?;
    let open = uploads.open(&upload_id).await?;
    let progress = uploads.status(&open).await?;
    Ok(Success::json(StatusCode::OK, &MediaUploadProgress::from(progress))?.into_response())
}
