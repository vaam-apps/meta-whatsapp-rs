//! A WABA's Flow private key (scope `flows`; roadmap M5d2, design D370 to
//! D374, docs/design/server.md section 4.2).
//!
//! | Route | What |
//! | --- | --- |
//! | `PUT /v1/wabas/{waba_id}/flow-key` | set the key the Flows data endpoint decrypts this WABA's requests with (replacing any: a rotation) |
//! | `GET /v1/wabas/{waba_id}/flow-key` | whether one is set, and its **public** half |
//! | `DELETE /v1/wabas/{waba_id}/flow-key` | delete it |
//!
//! Nothing is asked of Meta. **The private key goes in and never comes
//! out**: it is in no answer, log, audit event, idempotency record or
//! error (the core's [`meta_whatsapp_server_core::flow_key`]); the answer
//! is the public half, which is what each number's business public key is
//! set to. Setting takes an `Idempotency-Key`, over the WABA and the
//! public half's digest, never the private key.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::flow_key::{self as domain, FlowKey, FlowKeyInfo};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::common::ApiJson;
use crate::auth::{Caller, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

/// The key to set.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = FlowKeyRequest)]
pub struct FlowKeyBody {
    /// The private key as PEM text: an **unencrypted** 2048-bit RSA key,
    /// PKCS#8 (`-----BEGIN PRIVATE KEY-----`) or PKCS#1 (`-----BEGIN RSA
    /// PRIVATE KEY-----`). Meta's own `openssl genrsa -des3` makes a
    /// password-protected one, which is refused: remove the password once
    /// with `openssl pkcs8 -topk8 -nocrypt`. A secret: it is stored sealed
    /// in the token vault and never answered, logged or audited. At most
    /// 16 KiB.
    #[schema(format = Password)]
    pub private_key: String,
}

impl std::fmt::Debug for FlowKeyBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowKeyBody").finish_non_exhaustive()
    }
}

/// A WABA's Flow key, as its public half.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = FlowKey)]
pub struct FlowKeyView {
    /// Whether the caller's tenant has set a Flow key for this WABA. If
    /// not, the data endpoint uses the deployment's key when the
    /// deployment allows it (`WA_SERVER_FLOWS_DEPLOYMENT_KEY_FALLBACK`),
    /// else answers `404`.
    pub configured: bool,
    /// The public key as PEM (`-----BEGIN PUBLIC KEY-----`): set it as the
    /// business public key of each number a Flow is sent from, at Meta.
    #[schema(required = true)]
    pub public_key: Option<String>,
    /// The SHA-256 of `public_key`'s text, lowercase hex: tells two keys
    /// apart.
    #[schema(required = true)]
    pub public_key_sha256: Option<String>,
}

impl FlowKeyView {
    fn of(info: Option<FlowKeyInfo>) -> Self {
        match info {
            Some(info) => Self {
                configured: true,
                public_key: Some(info.public_key),
                public_key_sha256: Some(info.public_key_sha256),
            },
            None => Self {
                configured: false,
                public_key: None,
                public_key_sha256: None,
            },
        }
    }
}

/// The core's Flow key operations on `owned`, for `caller` (the state's
/// own: the extractors made them with `state.authz()`, which admits them).
fn flow_key<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedWaba,
) -> Result<FlowKey<'a>, ApiError> {
    Ok(FlowKey::new(
        state.authz(),
        caller,
        owned.capability(state),
    )?)
}

/// `PUT /v1/wabas/{waba_id}/flow-key`: set the WABA's Flow private key.
/// Audited (`flow_key_set`). The data endpoint decrypts this WABA's
/// requests with it from the next one; set the answer's `public_key` as
/// each number's business public key at Meta, or its Flows answer `421`.
#[utoipa::path(
    put,
    path = "/v1/wabas/{waba_id}/flow-key",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: a repeat with the same key and the same public key gets the kept answer (`Idempotent-Replayed: true`)"),
    ),
    request_body = FlowKeyBody,
    responses(
        (status = 200, description = "Set (or replaced): the key's public half", body = FlowKeyView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `private_key` (not an unencrypted 2048-bit RSA private key in PEM, or over 16 KiB; never quoting it) or `body`, `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn set_flow_key(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<FlowKeyBody>,
) -> Response {
    // The local check, before the key is claimed; the key becomes a
    // secret type here, and its public half is what the fingerprint holds.
    let (private_key, info) = match domain::private_key(body.private_key) {
        Ok(checked) => checked,
        Err(error) => return ApiError::from(error).into_response(),
    };
    // Never over the private key: the record holds nothing derived from it
    // but the public half's digest.
    let fingerprint = Fingerprint::set_flow_key(owned.waba_id(), &info.public_key_sha256);
    let operation = async {
        let set = flow_key(&state, &caller, &owned)?.set(&private_key).await?;
        Success::json(StatusCode::OK, &FlowKeyView::of(Some(set)))
    };
    state.idempotent(&caller, key, fingerprint, operation).await
}

/// `GET /v1/wabas/{waba_id}/flow-key`: whether the caller's tenant set a
/// Flow key for the WABA, and its public half. The private key is never
/// answered.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/flow-key",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The key's public half, or `configured: false`", body = FlowKeyView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn get_flow_key(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
) -> Result<Json<FlowKeyView>, ApiError> {
    let info = flow_key(&state, &caller, &owned)?.get().await?;
    Ok(Json(FlowKeyView::of(info)))
}

/// `DELETE /v1/wabas/{waba_id}/flow-key`: delete the WABA's Flow key.
/// Audited (`flow_key_deleted`) when there was one. `204` either way. The
/// data endpoint then uses the deployment's key if the deployment allows
/// it, else answers `404`.
#[utoipa::path(
    delete,
    path = "/v1/wabas/{waba_id}/flow-key",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Deleted, or there was none"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn delete_flow_key(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
) -> Result<StatusCode, ApiError> {
    flow_key(&state, &caller, &owned)?.delete().await?;
    Ok(StatusCode::NO_CONTENT)
}
