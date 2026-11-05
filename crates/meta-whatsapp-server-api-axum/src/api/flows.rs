//! WhatsApp Flows' management (scope `templates`; roadmap M5d,
//! docs/design/server.md section 4.2, parity row 61).
//!
//! | Route | Graph calls, with the tenant's WABA token |
//! | --- | --- |
//! | `POST /v1/wabas/{waba_id}/flows` | with a `clone_flow_id`, `GET /{clone_flow_id}` first; then `POST /{waba_id}/flows` |
//! | `GET /v1/wabas/{waba_id}/flows?cursor=` | `GET /{waba_id}/flows[?after=…]` |
//! | `GET /v1/wabas/{waba_id}/flows/{flow_id}` | `GET /{flow_id}?fields=…` (its WABA must be the path's) |
//! | `PATCH /v1/wabas/{waba_id}/flows/{flow_id}` | the same read, then `POST /{flow_id}` |
//! | `PUT /v1/wabas/{waba_id}/flows/{flow_id}/json` | the same read, then `POST /{flow_id}/assets` (multipart) |
//! | `POST /v1/wabas/{waba_id}/flows/{flow_id}/preview` | the same read, then `GET /{flow_id}?fields=preview.invalidate(…)` |
//! | `POST /v1/wabas/{waba_id}/flows/{flow_id}/publish`, `…/deprecate` | the same read, then `POST /{flow_id}/publish` (or `deprecate`) |
//! | `DELETE /v1/wabas/{waba_id}/flows/{flow_id}` | the same read, then `DELETE /{flow_id}` |
//!
//! **A Flow id is the WABA's, or it does not exist** (the core's
//! [`meta_whatsapp_server_core::flows`]): another WABA's Flow, an id Meta
//! refuses, or another kind of node is `404 not_found`, and nothing of it
//! is answered, changed or deleted. A creation takes an `Idempotency-Key`.
//! The Flow JSON is Meta's to validate: its errors come back in the answer
//! of a creation and of an upload (`validation_errors`), not as an error.
//! The Flows data endpoint, which Meta calls, is not here (it is on the
//! public listener: `meta_whatsapp_server_http`'s `api::flows`).

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::body::Bytes;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequest, Path, Request, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::flows::{self as domain, Flows};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::{ApiJson, decode_cursor, encode_cursor};
use super::templates::ApiQuery;
use crate::auth::{Caller, OwnedWaba};
use crate::error::{ApiError, ErrorBody, ErrorCode};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

/// The largest body of the Flow routes: 2 MiB. A Flow JSON is a few tens
/// of KiB in practice; Meta accepts 10 MiB (`flows/guides/flowsapi`), and
/// the internal listener's other routes 64 KiB. A larger Flow JSON is
/// uploaded to Meta directly.
pub const MAX_FLOW_BODY_BYTES: usize = 2 * 1024 * 1024;

/// The core's Flow operations on `owned`, for `caller`, counted in
/// `state`'s metrics (`caller` and `owned` are the state's own: the
/// extractors made them with `state.authz()`, which admits them).
fn flows<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedWaba,
) -> Result<Flows<'a>, ApiError> {
    Ok(Flows::new(state.authz(), caller, owned.capability(state))?.observed_by(state.metrics()))
}

// ─── Schemas ─────────────────────────────────────────────────────────────

/// A Flow to create, in Meta's JSON (`flows/guides/flowsapi`, "Creating a
/// Flow"). Read as Meta's object: a key Meta's page does not list is
/// refused on `body`.
#[derive(ToSchema)]
#[allow(dead_code)] // read as Meta's object; the type documents it
#[schema(as = FlowCreateRequest)]
pub struct FlowCreateBody {
    /// The Flow's name, not shown to users (at most 256 characters).
    pub name: String,
    /// Its use cases, 1 to 8 of Meta's: `SIGN_UP`, `SIGN_IN`,
    /// `APPOINTMENT_BOOKING`, `LEAD_GENERATION`, `CONTACT_US`,
    /// `CUSTOMER_SUPPORT`, `SURVEY`, `OTHER`.
    pub categories: Vec<String>,
    /// A Flow of this WABA to copy.
    pub clone_flow_id: Option<String>,
    /// The URL of the Flow's data endpoint (`https`, no credentials).
    /// Meta says to leave it out when cloning a Flow whose JSON is older
    /// than version 3.0.
    pub endpoint_uri: Option<String>,
    /// The Flow JSON: the object itself, or its text (Meta's form), at
    /// most 10 MiB.
    #[schema(value_type = Option<Object>)]
    pub flow_json: Option<Value>,
    /// Publish right away; Meta publishes only with a valid `flow_json`.
    pub publish: Option<bool>,
}

/// What to change in a Flow, in Meta's JSON: every field optional, at
/// least one set. Only a draft's metadata is editable (a published Flow
/// returns to `DRAFT` when edited, `flows/guides/lifecycle`).
#[derive(ToSchema)]
#[allow(dead_code)] // read as Meta's object; the type documents it
#[schema(as = FlowUpdateRequest)]
pub struct FlowUpdateBody {
    /// A new name.
    pub name: Option<String>,
    /// New categories, replacing the current ones (1 to 8).
    pub categories: Option<Vec<String>>,
    /// A new data endpoint URL (`https`, no credentials).
    pub endpoint_uri: Option<String>,
}

/// Whether to revoke the Flow's current preview link first.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = FlowPreviewRequest)]
pub struct FlowPreviewBody {
    /// `true` revokes the current link and returns a new one: use it when
    /// a link was shared too widely. Default `false`.
    #[serde(default)]
    pub invalidate: bool,
}

/// One place a validation error points at.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = FlowValidationPointer)]
pub struct FlowIssuePointerView {
    /// First line.
    #[schema(required = true)]
    pub line_start: Option<u32>,
    /// Last line.
    #[schema(required = true)]
    pub line_end: Option<u32>,
    /// First column.
    #[schema(required = true)]
    pub column_start: Option<u32>,
    /// Last column.
    #[schema(required = true)]
    pub column_end: Option<u32>,
    /// The path inside the Flow JSON (`screens [0]. layout.children [0].type`,
    /// spacing as Meta writes it).
    #[schema(required = true)]
    pub path: Option<String>,
}

/// A problem Meta found in a Flow's JSON, as Meta writes it. Every one
/// must be fixed before the Flow is published.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = FlowValidationError)]
pub struct FlowIssueView {
    /// Machine-readable error (`INVALID_PROPERTY_VALUE`).
    pub error: String,
    /// Its family (`FLOW_JSON_ERROR`).
    pub error_type: String,
    /// Meta's explanation of it.
    pub message: String,
    /// The first line of the offending span in the uploaded JSON.
    #[schema(required = true)]
    pub line_start: Option<u32>,
    /// Its last line.
    #[schema(required = true)]
    pub line_end: Option<u32>,
    /// Its first column.
    #[schema(required = true)]
    pub column_start: Option<u32>,
    /// Its last column.
    #[schema(required = true)]
    pub column_end: Option<u32>,
    /// The places in the Flow JSON it refers to.
    pub pointers: Vec<FlowIssuePointerView>,
}

impl From<domain::FlowIssue> for FlowIssueView {
    fn from(issue: domain::FlowIssue) -> Self {
        let domain::FlowIssue {
            error,
            error_type,
            message,
            line_start,
            line_end,
            column_start,
            column_end,
            pointers,
        } = issue;
        Self {
            error,
            error_type,
            message,
            line_start,
            line_end,
            column_start,
            column_end,
            pointers: pointers
                .into_iter()
                .map(|p| FlowIssuePointerView {
                    line_start: p.line_start,
                    line_end: p.line_end,
                    column_start: p.column_start,
                    column_end: p.column_end,
                    path: p.path,
                })
                .collect(),
        }
    }
}

/// A Flow, as Meta describes it.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Flow)]
pub struct FlowView {
    /// Its id.
    pub id: String,
    /// Its name (not shown to users).
    #[schema(required = true)]
    pub name: Option<String>,
    /// Where it is in its lifecycle, as Meta names it: `DRAFT`,
    /// `PUBLISHED`, `DEPRECATED`, `BLOCKED` or `THROTTLED`
    /// (`flows/guides/lifecycle`).
    #[schema(required = true)]
    pub status: Option<String>,
    /// Its categories, as Meta names them.
    pub categories: Vec<String>,
    /// What blocks its publication.
    pub validation_errors: Vec<FlowIssueView>,
    /// The `version` of its uploaded Flow JSON (on a single Flow's read).
    #[schema(required = true)]
    pub json_version: Option<String>,
    /// The `data_api_version` of its uploaded Flow JSON (Flows with an
    /// endpoint; on a single Flow's read).
    #[schema(required = true)]
    pub data_api_version: Option<String>,
    /// Its data endpoint's URL (on a single Flow's read).
    #[schema(required = true)]
    pub endpoint_uri: Option<String>,
}

impl From<domain::FlowInfo> for FlowView {
    fn from(flow: domain::FlowInfo) -> Self {
        let domain::FlowInfo {
            id,
            name,
            status,
            categories,
            validation_errors,
            json_version,
            data_api_version,
            endpoint_uri,
        } = flow;
        Self {
            id: id.into_inner(),
            name,
            status,
            categories,
            validation_errors: validation_errors
                .into_iter()
                .map(FlowIssueView::from)
                .collect(),
            json_version,
            data_api_version,
            endpoint_uri,
        }
    }
}

/// A page of a WABA's Flows.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = FlowList)]
pub struct FlowListView {
    /// The Flows (Meta's default fields: id, name, status, categories and
    /// validation errors).
    pub data: Vec<FlowView>,
    /// Pass as `cursor` for the page after; absent on the last.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// A created Flow.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = FlowCreated)]
pub struct FlowCreatedView {
    /// Its id.
    pub id: String,
    /// The problems Meta found in the `flow_json` sent with it: the Flow
    /// is created as a draft all the same, but cannot be published until
    /// they are fixed.
    pub validation_errors: Vec<FlowIssueView>,
}

/// What Meta made of an uploaded Flow JSON.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = FlowJsonUpload)]
pub struct FlowJsonUploadView {
    /// Meta's `success`: the upload was stored, errors or not.
    pub success: bool,
    /// The problems Meta found in it.
    pub validation_errors: Vec<FlowIssueView>,
}

/// A Flow's web preview link.
#[derive(Serialize, ToSchema)]
#[schema(as = FlowPreview)]
pub struct FlowPreviewView {
    /// The preview page, which needs no login: **a secret**, its `token`
    /// opens the Flow for anyone holding it, until `expires_at` or until
    /// invalidated. Can be embedded in an `<iframe>`.
    pub preview_url: String,
    /// When it expires, as Meta writes it (`2023-05-21T11:18:09+0000`):
    /// 30 days after its creation.
    pub expires_at: String,
}

impl std::fmt::Debug for FlowPreviewView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowPreviewView")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// `?cursor=`, as the OpenAPI document declares it: Meta's flow list takes
/// a cursor and no page size.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct FlowCursor {
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// The request's body as it arrived, for a Flow JSON uploaded as it is
/// (the validation errors' lines refer to it). Over the body limit, it is
/// `413 payload_too_large`.
pub struct ApiBytes(pub Bytes);

impl<S: Send + Sync> FromRequest<S> for ApiBytes {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        match Bytes::from_request(request, state).await {
            Ok(body) => Ok(Self(body)),
            Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                Err(ApiError::new(ErrorCode::PAYLOAD_TOO_LARGE))
            }
            Err(_) => Err(ApiError::invalid("body")),
        }
    }
}

// ─── Create, list, get ───────────────────────────────────────────────────

/// Create `flow`: `201` with its id and Meta's validation errors, or
/// Meta's refusal.
async fn create(flows: &Flows<'_>, flow: &domain::NewFlow) -> Result<Success, ApiError> {
    let created = flows.create(flow).await?;
    Success::json(
        StatusCode::CREATED,
        &FlowCreatedView {
            id: created.id.into_inner(),
            validation_errors: created
                .validation_errors
                .into_iter()
                .map(FlowIssueView::from)
                .collect(),
        },
    )
}

/// `POST /v1/wabas/{waba_id}/flows`: create a Flow (a draft, unless
/// published with it).
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/flows",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never create twice"),
    ),
    request_body = FlowCreateBody,
    responses(
        (status = 201, description = "Created, as a draft (published too when `publish` was set and Meta found the Flow JSON valid)", body = FlowCreatedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or a `clone_flow_id` not in this WABA; nothing created", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 413, description = "`payload_too_large`: over 2 MiB", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `body` (not Meta's object, or a key Meta's page does not list), `name`, `categories`, `categories[i]`, `clone_flow_id`, `endpoint_uri` or `flow_json`; `invalid_parameter`, `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the Flow may have been created", body = ErrorBody),
    )
)]
pub(crate) async fn create_flow(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::create_flow(owned.waba_id(), &body);
    let flow = match domain::new_flow(&body) {
        Ok(flow) => flow,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let flows = match flows(&state, &caller, &owned) {
        Ok(flows) => flows,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, create(&flows, &flow))
        .await
}

/// `GET /v1/wabas/{waba_id}/flows`: a page of the WABA's Flows.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/flows",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        FlowCursor,
    ),
    responses(
        (status = 200, description = "A page of Flows", body = FlowListView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `query` (a parameter but `cursor`) or `cursor`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_flows(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiQuery(query): ApiQuery<FlowCursor>,
) -> Result<Json<FlowListView>, ApiError> {
    let after = query
        .cursor
        .map(|cursor| decode_cursor(&cursor).ok_or_else(|| ApiError::invalid("cursor")))
        .transpose()?;
    let page = flows(&state, &caller, &owned)?.list(after).await?;
    Ok(Json(FlowListView {
        data: page.flows.into_iter().map(FlowView::from).collect(),
        next_cursor: page.next_after.as_deref().map(encode_cursor),
    }))
}

/// `GET /v1/wabas/{waba_id}/flows/{flow_id}`: one Flow.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/flows/{flow_id}",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("flow_id" = String, Path, description = "Flow id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The Flow", body = FlowView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such Flow in this WABA (another WABA's Flow looks like a missing one)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `flow_id`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_flow(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, flow_id)): Path<(String, String)>,
) -> Result<Json<FlowView>, ApiError> {
    let flow = flows(&state, &caller, &owned)?.get(&flow_id).await?;
    Ok(Json(FlowView::from(flow)))
}

// ─── Changes ─────────────────────────────────────────────────────────────

/// `PATCH /v1/wabas/{waba_id}/flows/{flow_id}`: change a Flow's name,
/// categories or endpoint.
#[utoipa::path(
    patch,
    path = "/v1/wabas/{waba_id}/flows/{flow_id}",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("flow_id" = String, Path, description = "Flow id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = FlowUpdateBody,
    responses(
        (status = 204, description = "Changed"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such Flow in this WABA; nothing changed", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 413, description = "`payload_too_large`: over 2 MiB", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `flow_id`, `body` (not Meta's object, a key the service does not send, or nothing to change), `name`, `categories`, `categories[i]` or `endpoint_uri`; `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the Flow may have been changed", body = ErrorBody),
    )
)]
pub(crate) async fn update_flow(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, flow_id)): Path<(String, String)>,
    ApiJson(body): ApiJson<Value>,
) -> Result<StatusCode, ApiError> {
    let change = domain::flow_change(&body)?;
    flows(&state, &caller, &owned)?
        .update(&flow_id, &change)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /v1/wabas/{waba_id}/flows/{flow_id}/json`: replace a Flow's Flow
/// JSON. The body is the Flow JSON itself, sent to Meta as it arrived.
#[utoipa::path(
    put,
    path = "/v1/wabas/{waba_id}/flows/{flow_id}/json",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("flow_id" = String, Path, description = "Flow id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body(content = Object, description = "The Flow JSON (`flows/guides/flowjson`): a JSON object of at most 2 MiB, uploaded as sent", content_type = "application/json"),
    responses(
        (status = 200, description = "Stored; Meta's validation errors, if any, in the answer", body = FlowJsonUploadView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such Flow in this WABA; nothing uploaded", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 413, description = "`payload_too_large`: over 2 MiB", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `flow_id` or `body` (not a JSON object); `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the Flow JSON may have been replaced", body = ErrorBody),
    )
)]
pub(crate) async fn upload_flow_json(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, flow_id)): Path<(String, String)>,
    ApiBytes(body): ApiBytes,
) -> Result<Json<FlowJsonUploadView>, ApiError> {
    let json = domain::flow_json_upload(body)?;
    let uploaded = flows(&state, &caller, &owned)?
        .upload_json(&flow_id, json)
        .await?;
    Ok(Json(FlowJsonUploadView {
        success: uploaded.success,
        validation_errors: uploaded
            .validation_errors
            .into_iter()
            .map(FlowIssueView::from)
            .collect(),
    }))
}

/// `POST /v1/wabas/{waba_id}/flows/{flow_id}/preview`: the Flow's web
/// preview link.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/flows/{flow_id}/preview",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("flow_id" = String, Path, description = "Flow id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = FlowPreviewBody,
    responses(
        (status = 200, description = "The link (a secret: do not log it)", body = FlowPreviewView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such Flow in this WABA", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `flow_id` or `body`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn preview_flow(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, flow_id)): Path<(String, String)>,
    ApiJson(body): ApiJson<FlowPreviewBody>,
) -> Result<Json<FlowPreviewView>, ApiError> {
    let link = flows(&state, &caller, &owned)?
        .preview(&flow_id, body.invalidate)
        .await?;
    Ok(Json(FlowPreviewView {
        preview_url: link.expose_url().to_owned(),
        expires_at: link.expires_at,
    }))
}

/// `POST /v1/wabas/{waba_id}/flows/{flow_id}/publish`: publish a Flow.
/// A published Flow can no longer be deleted, only deprecated.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/flows/{flow_id}/publish",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("flow_id" = String, Path, description = "Flow id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Published"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such Flow in this WABA; nothing published", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `flow_id`; `invalid_parameter` (validation errors or publishing checks outstanding are Meta's)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the Flow may have been published", body = ErrorBody),
    )
)]
pub(crate) async fn publish_flow(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, flow_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    flows(&state, &caller, &owned)?.publish(&flow_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/wabas/{waba_id}/flows/{flow_id}/deprecate`: deprecate a
/// published Flow: it can no longer be sent or opened.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/flows/{flow_id}/deprecate",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("flow_id" = String, Path, description = "Flow id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Deprecated"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such Flow in this WABA; nothing deprecated", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `flow_id`; `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the Flow may have been deprecated", body = ErrorBody),
    )
)]
pub(crate) async fn deprecate_flow(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, flow_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    flows(&state, &caller, &owned)?.deprecate(&flow_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/wabas/{waba_id}/flows/{flow_id}`: delete a draft Flow.
#[utoipa::path(
    delete,
    path = "/v1/wabas/{waba_id}/flows/{flow_id}",
    tag = "flows",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("flow_id" = String, Path, description = "Flow id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such Flow in this WABA; nothing deleted", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `flow_id`; `invalid_parameter` (only a draft can be deleted: Meta's refusal)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the Flow may have been deleted", body = ErrorBody),
    )
)]
pub(crate) async fn delete_flow(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, flow_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    flows(&state, &caller, &owned)?.delete(&flow_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::ids::FlowId;
    use serde_json::json;

    use super::*;

    /// Every field of the core's Flow reaches the service's answer.
    /// Decisive: each field of the conversion.
    #[test]
    fn every_field_of_the_cores_flow_is_answered() {
        let flow = domain::FlowInfo {
            id: FlowId::new("1234567890"),
            name: Some("flow 1".to_owned()),
            status: Some("DRAFT".to_owned()),
            categories: vec!["CONTACT_US".to_owned()],
            validation_errors: vec![domain::FlowIssue {
                error: "INVALID_PROPERTY_VALUE".to_owned(),
                error_type: "FLOW_JSON_ERROR".to_owned(),
                message: "Invalid value found for property 'type'.".to_owned(),
                line_start: Some(10),
                line_end: Some(10),
                column_start: Some(21),
                column_end: Some(34),
                pointers: vec![domain::FlowIssuePointer {
                    line_start: Some(10),
                    line_end: Some(10),
                    column_start: Some(21),
                    column_end: Some(34),
                    path: Some("screens [0]. layout.children [0].type".to_owned()),
                }],
            }],
            json_version: Some("3.0".to_owned()),
            data_api_version: Some("3.0".to_owned()),
            endpoint_uri: Some("https://example.com".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(FlowView::from(flow)).unwrap(),
            json!({"id": "1234567890", "name": "flow 1", "status": "DRAFT",
                   "categories": ["CONTACT_US"],
                   "validation_errors": [{
                       "error": "INVALID_PROPERTY_VALUE", "error_type": "FLOW_JSON_ERROR",
                       "message": "Invalid value found for property 'type'.",
                       "line_start": 10, "line_end": 10, "column_start": 21, "column_end": 34,
                       "pointers": [{"line_start": 10, "line_end": 10, "column_start": 21,
                           "column_end": 34, "path": "screens [0]. layout.children [0].type"}]}],
                   "json_version": "3.0", "data_api_version": "3.0",
                   "endpoint_uri": "https://example.com"})
        );
    }

    /// A preview link's token never reaches `Debug`.
    #[test]
    fn a_preview_views_token_is_not_in_debug() {
        let view = FlowPreviewView {
            preview_url: "https://business.facebook.com/wa/manage/flows/1/preview/?token=SECRET"
                .to_owned(),
            expires_at: "2023-05-21T11:18:09+0000".to_owned(),
        };
        let shown = format!("{view:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
    }
}
