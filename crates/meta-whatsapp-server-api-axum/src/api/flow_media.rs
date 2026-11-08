//! A Flow's uploaded media, downloaded and decrypted (scope `flows`;
//! roadmap M5d2, design D454 to D457, docs/design/server.md section 4.2).
//!
//! | Route | What |
//! | --- | --- |
//! | `POST /v1/wabas/{waba_id}/flow-media` | download the file at a `cdn_url` of a data exchange request, check it, decrypt it, answer its bytes |
//!
//! The integrating app receives, for each file a user attached with a
//! `PhotoPicker` or `DocumentPicker`, an object with a `cdn_url` and
//! `encryption_metadata` (`flows/guides/media_upload`). It posts that
//! object here. The service does what Meta's page tells an endpoint to do
//! ([`meta_whatsapp_server_core::flow_media`]): the URL is checked by its
//! text first (`https`, a listed host name: `*.whatsapp.net` by default,
//! `WA_SERVER_FLOWS_MEDIA_HOSTS`, never an address, a port or user
//! info), the host's addresses must all be public, the file is fetched
//! without credentials and without following a redirect, bounded to 25 MiB
//! and a minute, then its SHA-256, its HMAC and the SHA-256 of the result
//! are checked. **Nothing is answered unless every check passed**: the
//! answer is the file's bytes (`application/octet-stream`, an
//! `attachment`, `nosniff`, a sandboxing `Content-Security-Policy`,
//! `no-store`), which are untrusted input (Meta's warning).
//!
//! The URL, the keys, the hashes and the bytes are in no log, error,
//! audit event or metric. Not idempotent-keyed: it changes nothing.

use meta_whatsapp_rs::webhooks::axum::body::Body;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderValue, StatusCode, header};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::error::ServiceError;
use meta_whatsapp_server_core::flow_media::{FlowMediaDownload, MediaKeys};
use serde::Deserialize;
use utoipa::ToSchema;

use super::common::ApiJson;
use crate::auth::{Caller, OwnedWaba};
use crate::error::{ApiError, ErrorBody, ErrorCode};
use crate::state::Grant;

/// The keys and hashes that open the file: `encryption_metadata` of
/// Meta's object.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = FlowMediaEncryptionMetadata)]
pub struct EncryptionMetadataBody {
    /// SHA-256 of the file on the CDN, base64 (32 bytes).
    pub encrypted_hash: String,
    /// The AES-CBC initialization vector, base64 (16 bytes).
    pub iv: String,
    /// The AES-256 key, base64 (32 bytes). A secret.
    #[schema(format = Password)]
    pub encryption_key: String,
    /// The HMAC-SHA256 key, base64 (16 to 64 bytes; Meta's example 32). A
    /// secret.
    #[schema(format = Password)]
    pub hmac_key: String,
    /// SHA-256 of the decrypted file, base64 (32 bytes).
    pub plaintext_hash: String,
}

/// One uploaded file, as Meta's data exchange request carries it.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = FlowMediaRequest)]
pub struct FlowMediaBody {
    /// Meta's id of the upload. Accepted so that the object Meta sent can
    /// be posted whole; not used.
    #[serde(default)]
    pub media_id: Option<String>,
    /// Where the encrypted file is: `https`, a host name this deployment
    /// lists (`*.whatsapp.net` by default), the default port, no user
    /// info, at most 2,048 characters. Never an address. Anything else is
    /// `422` on `cdn_url` before any lookup or connection.
    pub cdn_url: String,
    /// The file's name. Accepted for the same reason; not used (it is
    /// never put in a header).
    #[serde(default)]
    pub file_name: Option<String>,
    /// The keys and hashes.
    pub encryption_metadata: EncryptionMetadataBody,
}

impl std::fmt::Debug for FlowMediaBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Nothing of it: the URL's path may carry a token, the metadata
        // holds the keys.
        f.debug_struct("FlowMediaBody").finish_non_exhaustive()
    }
}

/// A decrypted file's bytes.
#[derive(Debug, ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct FlowMediaBytes(pub Vec<u8>);

/// `POST /v1/wabas/{waba_id}/flow-media`: download, verify and decrypt a
/// Flow's uploaded file. Audited (`flow_media_downloaded`: tenant, WABA,
/// size).
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/flow-media",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = FlowMediaBody,
    responses(
        (status = 200, description = "The decrypted file, only after every check passed (the CDN file's SHA-256 and HMAC, the result's SHA-256)", content_type = "application/octet-stream", body = FlowMediaBytes),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or `feature_not_available`: this deployment has no download", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant; nothing was looked up or fetched", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 413, description = "`media_too_large`: the file is past 25 MiB", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `cdn_url` (not an `https` URL of a listed host name on the default port without user info, or a host that resolves to a non-public address; the reason is not said) or on a value of `encryption_metadata` (`encryption_metadata.iv`, …: not base64 of the documented length); never quoted; nothing was fetched", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's budget, or four downloads in flight on the replica (`Retry-After`)", body = ErrorBody),
        (status = 502, description = "`media_download_failed` (lookup, connection, TLS, time limit, or the CDN answered other than 200: a redirect is never followed; retryable) or `integrity` (a hash or the HMAC did not match: nothing is returned)", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn download_flow_media(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiJson(body): ApiJson<FlowMediaBody>,
) -> Result<Response, ApiError> {
    let Some(deployment) = state.settings().flow_media.as_ref() else {
        return Err(ServiceError::new(ErrorCode::FEATURE_NOT_AVAILABLE).into());
    };
    let FlowMediaBody {
        cdn_url,
        encryption_metadata,
        ..
    } = body;
    let keys = MediaKeys {
        encrypted_hash: encryption_metadata.encrypted_hash,
        iv: encryption_metadata.iv,
        encryption_key: encryption_metadata.encryption_key,
        hmac_key: encryption_metadata.hmac_key,
        plaintext_hash: encryption_metadata.plaintext_hash,
    };
    let bytes =
        FlowMediaDownload::new(state.authz(), &caller, owned.capability(&state), deployment)?
            .download(&cdn_url, &keys)
            .await?;
    let headers = [
        (
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        ),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        ),
        // A user's file is never active content, whatever its type.
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("sandbox; default-src 'none'"),
        ),
        (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
    ];
    Ok((StatusCode::OK, headers, Body::from(bytes)).into_response())
}
