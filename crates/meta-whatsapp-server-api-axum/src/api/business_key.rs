//! A number's Flows business public key (scope `flows`; roadmap M5d2,
//! design D450 to D453, docs/design/server.md section 4.2).
//!
//! | Route | Graph call, with the tenant's WABA token |
//! | --- | --- |
//! | `PUT /v1/numbers/{pn}/business-public-key` | `POST /{pn}/whatsapp_business_encryption` |
//! | `GET /v1/numbers/{pn}/business-public-key` | `GET /{pn}/whatsapp_business_encryption` |
//!
//! The key WhatsApp encrypts the number's Flow data endpoint traffic to:
//! the public half of the WABA's Flow key
//! ([`super::flow_key`]), set on each number a Flow is sent from. A key
//! that is not a 2048-bit RSA public key in PEM is `422` before any
//! request; when the tenant has set a Flow key, one that is not its public
//! half is `422` on `flow_key` too (unless the request says it means it).
//! The change is audited with the key's SHA-256, never the key.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_server_core::business_key::{self as domain, BusinessKey};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::common::ApiJson;
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::state::Grant;

fn service<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedNumber,
) -> Result<BusinessKey<'a>, ApiError> {
    Ok(
        BusinessKey::new(state.authz(), caller, owned.capability(state))?
            .observed_by(state.metrics()),
    )
}

/// The key to set.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = BusinessPublicKeyRequest)]
pub struct BusinessKeyBody {
    /// The public key as PEM: one `-----BEGIN PUBLIC KEY-----` block, a
    /// 2048-bit RSA key (`openssl rsa -pubout`, or the `public_key` of
    /// `GET /v1/wabas/{waba_id}/flow-key`). A private key is refused
    /// without being quoted. At most 4 KiB.
    pub business_public_key: String,
    /// `true` to set a key that is **not** the public half of the WABA's
    /// Flow key although the tenant has set one: for a key the
    /// integrating app holds elsewhere. Default `false`: a mismatch is
    /// `422` on `flow_key` and nothing is sent.
    #[serde(default)]
    pub allow_flow_key_mismatch: bool,
}

/// What was set.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BusinessPublicKeySet)]
pub struct BusinessKeySetView {
    /// The SHA-256 of the key as sent (canonical PEM), lowercase hex: the
    /// `public_key_sha256` of the Flow key's answer, when they match.
    pub public_key_sha256: String,
    /// `true`: the key is the public half of the tenant's Flow key for the
    /// WABA. `false`: it is not (`allow_flow_key_mismatch` was set).
    /// `null`: the tenant has set no Flow key, so there was nothing to
    /// compare.
    #[schema(required = true)]
    pub matches_flow_key: Option<bool>,
}

/// The business public key Meta holds for a number.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BusinessPublicKey)]
pub struct BusinessKeyView {
    /// Whether Meta holds a key for the number.
    pub configured: bool,
    /// The key, as Meta answered.
    #[schema(required = true)]
    pub public_key: Option<String>,
    /// The SHA-256 of the key's canonical PEM, lowercase hex, when it
    /// parses as a 2048-bit RSA public key.
    #[schema(required = true)]
    pub public_key_sha256: Option<String>,
    /// Meta's check of its signature over the key, as it wrote it
    /// (`VALID`, `MISMATCH`: upload the key again).
    #[schema(required = true)]
    pub signature_status: Option<String>,
    /// As `matches_flow_key` of the `PUT`'s answer, for the key Meta holds.
    #[schema(required = true)]
    pub matches_flow_key: Option<bool>,
}

/// `PUT /v1/numbers/{pn}/business-public-key`: set the number's business
/// public key at Meta (replacing any). Audited
/// (`business_public_key_set`) with the key's digest.
#[utoipa::path(
    put,
    path = "/v1/numbers/{pn}/business-public-key",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = BusinessKeyBody,
    responses(
        (status = 200, description = "Set: the key's digest and whether it is the Flow key's public half", body = BusinessKeySetView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant (another tenant's number looks like a missing one; nothing is sent)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `business_public_key` (not one PEM block holding a 2048-bit RSA public key; a private key is refused; never quoted) or `body`, or on `flow_key`: the tenant's Flow key for the WABA exists and the key is not its public half (`allow_flow_key_mismatch`); nothing was sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's budget (`Retry-After`), or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`; the Flow key record cannot be opened", body = ErrorBody),
        (status = 504, description = "`timeout`: read the key before asking again", body = ErrorBody),
    )
)]
pub(crate) async fn set_business_public_key(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiJson(body): ApiJson<BusinessKeyBody>,
) -> Result<Json<BusinessKeySetView>, ApiError> {
    // The local check first: nothing is read or sent for a wrong key.
    let key = domain::public_key(&body.business_public_key)?;
    let set = service(&state, &caller, &owned)?
        .set(&key, body.allow_flow_key_mismatch)
        .await?;
    Ok(Json(BusinessKeySetView {
        public_key_sha256: set.public_key_sha256,
        matches_flow_key: set.matches_flow_key,
    }))
}

/// `GET /v1/numbers/{pn}/business-public-key`: the key Meta holds for the
/// number, its signature status, and whether it is the Flow key's public
/// half.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/business-public-key",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The key, or `configured: false`", body = BusinessKeyView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`; the Flow key record cannot be opened", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_business_public_key(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<BusinessKeyView>, ApiError> {
    let view = service(&state, &caller, &owned)?.get().await?;
    Ok(Json(BusinessKeyView {
        configured: view.public_key.is_some(),
        public_key: view.public_key,
        public_key_sha256: view.public_key_sha256,
        signature_status: view.signature_status,
        matches_flow_key: view.matches_flow_key,
    }))
}
