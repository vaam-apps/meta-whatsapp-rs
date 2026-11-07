//! A template's life after its creation (scope `templates`; roadmap M5b,
//! docs/design/server.md section 4.2).
//!
//! | Route | Graph calls, with the tenant's WABA token |
//! | --- | --- |
//! | `PATCH /v1/wabas/{waba_id}/templates/{id}` | `GET /{id}?fields=name`, `GET /{waba_id}/message_templates?fields=name&name=…` (the id must be among them), then `POST /{id}` |
//! | `POST /v1/wabas/{waba_id}/templates/{id}/unpause` | the same lookup, then `POST /{id}/unpause` |
//! | `GET /v1/wabas/{waba_id}/templates/{id}/compare?template_ids=&start=&end=` | the lookup of both ids, then `GET /{id}/compare?template_ids=[…]&start=…&end=…` |
//! | `POST /v1/wabas/{waba_id}/templates/archive`, `…/unarchive` | `GET /{waba_id}/message_templates?fields=id&limit=100[&status=ARCHIVED]` until every id is found (10 pages at most), then `POST /{waba_id}/message_templates/archive` (or `unarchive`) |
//! | `GET /v1/wabas/{waba_id}/templates/library?search=&topic=&usecase=&industry=&language=&name=` | `GET /message_template_library?…` |
//! | `POST /v1/wabas/{waba_id}/templates/library` | `POST /{waba_id}/message_templates` with `library_template_name` |
//! | `POST /v1/wabas/{waba_id}/templates/migrations` | the source WABA opened as the caller's (another tenant's is `404`); its `template_ids` found in its list (its token); then `POST /{waba_id}/migrate_message_templates` |
//!
//! Pages: `templates/template-management`, `templates/template-library`,
//! `templates/template-migration`, `templates/template-comparison`,
//! `templates/template-pausing`, `templates/template-archival`.
//!
//! The rules are the core's ([`meta_whatsapp_server_core::templates`]:
//! the checks, the lookups of a template id in its WABA, the audit
//! events). This module keeps the routes, the request and answer types
//! with their OpenAPI schema (Meta's field names), converts them, and
//! renders the answers. A creation from the library and a migration take
//! an `Idempotency-Key`, as a template's creation does.

use std::collections::BTreeMap;

use meta_whatsapp_rs::client::templates::{
    ArchiveResult, ComparisonMetric, LibraryTemplate, LibraryTemplateRequest, MigrationResult,
    UnarchiveResult,
};
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::templates::{self as domain, Templates};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::ApiJson;
use super::templates::{ApiQuery, SupportedAppBody, TemplateCreatedView, templates};
use crate::auth::{Caller, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

/// A value of one of Meta's enums, as Meta wrote it.
fn wire<T: Serialize>(value: Option<&T>) -> Option<String> {
    value
        .and_then(|v| serde_json::to_value(v).ok())
        .and_then(|v| v.as_str().map(str::to_owned))
}

// ─── Edit ────────────────────────────────────────────────────────────────

/// What to change in a template, in Meta's JSON (`POST /{TEMPLATE_ID}`,
/// `templates/template-management` § Edit templates): only `APPROVED`,
/// `REJECTED` or `PAUSED` templates can be edited; `components` replaces
/// all of them; an approved template's category cannot change, and it can
/// be edited 10 times in 30 days (once in 24 hours). Read as Meta's object:
/// a key the service would not pass on is refused on its path.
#[derive(ToSchema)]
#[allow(dead_code)] // read as Meta's object; the type documents it
#[schema(as = TemplateEdit)]
pub struct TemplateEditBody {
    /// `MARKETING`, `UTILITY` or `AUTHENTICATION`.
    pub category: Option<String>,
    /// Every component of the template, replaced (`templates/components`).
    #[schema(value_type = Option<Vec<std::collections::HashMap<String, Value>>>)]
    pub components: Option<Vec<Value>>,
    /// `POSITIONAL` or `NAMED`.
    pub parameter_format: Option<String>,
    /// Let Meta assign another category.
    pub allow_category_change: Option<bool>,
    /// Opt out of Meta's link tracking for URL buttons.
    pub cta_url_link_tracking_opted_out: Option<bool>,
    /// The message's time to live, in seconds (`templates/time-to-live`).
    pub message_send_ttl_seconds: Option<i64>,
    /// Meta's sub-category.
    pub sub_category: Option<String>,
    /// Meta's display format.
    pub display_format: Option<String>,
    /// Deliver to the user's primary device only.
    pub is_primary_device_delivery_only: Option<bool>,
}

/// `PATCH /v1/wabas/{waba_id}/templates/{id}`: edit a template.
#[utoipa::path(
    patch,
    path = "/v1/wabas/{waba_id}/templates/{id}",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("id" = String, Path, description = "Template id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = TemplateEditBody,
    responses(
        (status = 204, description = "Edited: Meta reviews it again; the result arrives as a `template_status_updated` event"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such template in this WABA (another WABA's template id looks like a missing one); nothing edited", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`id`, or with `field`: the edit breaks a documented limit, holds a key the service would not send to Meta, changes nothing, or accepts Meta's zero-tap terms), `template_rejected`, `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the template may have been edited", body = ErrorBody),
    )
)]
pub(crate) async fn edit_template(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, id)): Path<(String, String)>,
    ApiJson(body): ApiJson<Value>,
) -> Result<StatusCode, ApiError> {
    let edit = domain::edit(&body)?;
    templates(&state, &caller, &owned)?.edit(&id, &edit).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Unpause ─────────────────────────────────────────────────────────────

/// `POST /v1/wabas/{waba_id}/templates/{id}/unpause`: unpause a template
/// paused for its quality or by pacing.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/templates/{id}/unpause",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("id" = String, Path, description = "Template id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Unpaused; the new status arrives as a `template_status_updated` event"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such template in this WABA; nothing unpaused", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `id`, `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the template may have been unpaused", body = ErrorBody),
    )
)]
pub(crate) async fn unpause_template(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    templates(&state, &caller, &owned)?.unpause(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Compare ─────────────────────────────────────────────────────────────

/// `?template_ids=&start=&end=` of a comparison
/// (`templates/template-comparison`).
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ComparisonQuery {
    /// The other template's id (Meta's `template_ids`, one id): of the same
    /// WABA, sent 1,000 times or more in the window.
    pub template_ids: String,
    /// The window's start, a UNIX timestamp (7, 30, 60 or 90 days before
    /// `end`).
    pub start: i64,
    /// The window's end, a UNIX timestamp (now).
    pub end: i64,
}

/// A template's value in a comparison metric.
#[derive(Debug, Serialize, ToSchema)]
pub struct ComparisonNumberValue {
    /// The template's id.
    pub key: String,
    /// The number (`MESSAGE_SENDS`: its sends).
    pub value: i64,
}

/// A template's value in a comparison metric.
#[derive(Debug, Serialize, ToSchema)]
pub struct ComparisonStringValue {
    /// The template's id.
    pub key: String,
    /// The value (`TOP_BLOCK_REASON`: `SPAM`, `NO_LONGER_NEEDED`, …).
    pub value: String,
}

/// One metric of a comparison, as Meta answered it.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ComparisonMetric)]
pub struct ComparisonMetricView {
    /// `BLOCK_RATE`, `MESSAGE_SENDS` or `TOP_BLOCK_REASON`, as Meta names
    /// it.
    #[schema(required = true)]
    pub metric: Option<String>,
    /// `RELATIVE`, `NUMBER_VALUES` or `STRING_VALUES`.
    #[serde(rename = "type")]
    pub kind: String,
    /// `BLOCK_RATE`: the template ids, lowest block rate first.
    pub order_by_relative_metric: Option<Vec<String>>,
    /// `MESSAGE_SENDS`: each template's sends.
    pub number_values: Option<Vec<ComparisonNumberValue>>,
    /// `TOP_BLOCK_REASON`: each template's top block reason.
    pub string_values: Option<Vec<ComparisonStringValue>>,
}

impl From<ComparisonMetric> for ComparisonMetricView {
    fn from(metric: ComparisonMetric) -> Self {
        let ComparisonMetric {
            metric,
            kind,
            order_by_relative_metric,
            number_values,
            string_values,
            ..
        } = metric;
        Self {
            metric: wire(Some(&metric)),
            kind,
            order_by_relative_metric: order_by_relative_metric
                .map(|ids| ids.into_iter().map(|id| id.as_str().to_owned()).collect()),
            number_values: number_values.map(|values| {
                values
                    .into_iter()
                    .map(|v| ComparisonNumberValue {
                        key: v.key.as_str().to_owned(),
                        value: v.value,
                    })
                    .collect()
            }),
            string_values: string_values.map(|values| {
                values
                    .into_iter()
                    .map(|v| ComparisonStringValue {
                        key: v.key.as_str().to_owned(),
                        value: v.value,
                    })
                    .collect()
            }),
        }
    }
}

/// A comparison's metrics.
#[derive(Debug, Serialize, ToSchema)]
pub struct TemplateComparison {
    /// The metrics, as Meta answered them.
    pub data: Vec<ComparisonMetricView>,
}

/// `GET /v1/wabas/{waba_id}/templates/{id}/compare`: compare two of the
/// WABA's templates.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/templates/{id}/compare",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("id" = String, Path, description = "Template id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ComparisonQuery,
    ),
    responses(
        (status = 200, description = "The block rate, sends and top block reason of both", body = TemplateComparison),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or either template not in this WABA", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `id`, `template_ids` (not an id, or `id` itself) or `query`; `invalid_parameter` (too few sends in the window is Meta's)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn compare_templates(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, id)): Path<(String, String)>,
    ApiQuery(query): ApiQuery<ComparisonQuery>,
) -> Result<Json<TemplateComparison>, ApiError> {
    let ComparisonQuery {
        template_ids,
        start,
        end,
    } = query;
    let metrics = templates(&state, &caller, &owned)?
        .compare(&id, &template_ids, start, end)
        .await?;
    Ok(Json(TemplateComparison {
        data: metrics
            .into_iter()
            .map(ComparisonMetricView::from)
            .collect(),
    }))
}

// ─── Archive and unarchive ───────────────────────────────────────────────

/// The templates to archive or unarchive (Meta's body,
/// `templates/template-archival`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = TemplateIds)]
pub struct TemplateIdsBody {
    /// 1 to 100 template ids, each the WABA's.
    pub hsm_ids: Vec<String>,
}

/// The templates archived, and why each other one was not.
#[derive(Debug, Serialize, ToSchema)]
pub struct TemplatesArchived {
    /// The ids archived: each is deleted 28 days later, unless unarchived
    /// first.
    pub archived_templates: Vec<String>,
    /// Template id → Meta's reason it was not archived ("Only approved or
    /// rejected templates can be archived").
    pub failed_templates: BTreeMap<String, String>,
}

impl From<ArchiveResult> for TemplatesArchived {
    fn from(result: ArchiveResult) -> Self {
        let ArchiveResult {
            archived_templates,
            failed_templates,
            ..
        } = result;
        Self {
            archived_templates: archived_templates
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            failed_templates: failed_templates
                .into_iter()
                .map(|(id, reason)| (id.as_str().to_owned(), reason))
                .collect(),
        }
    }
}

/// The templates unarchived, and why each other one was not.
#[derive(Debug, Serialize, ToSchema)]
pub struct TemplatesUnarchived {
    /// The ids unarchived: back to their status before the archive
    /// (`APPROVED` when there was none).
    pub unarchived_templates: Vec<String>,
    /// Template id → Meta's reason it was not unarchived ("Template is not
    /// archived").
    pub failed_templates: BTreeMap<String, String>,
}

impl From<UnarchiveResult> for TemplatesUnarchived {
    fn from(result: UnarchiveResult) -> Self {
        let UnarchiveResult {
            unarchived_templates,
            failed_templates,
            ..
        } = result;
        Self {
            unarchived_templates: unarchived_templates
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            failed_templates: failed_templates
                .into_iter()
                .map(|(id, reason)| (id.as_str().to_owned(), reason))
                .collect(),
        }
    }
}

/// The ids `body` carries, checked.
fn bulk_ids(body: Value) -> Result<Vec<meta_whatsapp_rs::core::ids::TemplateId>, ApiError> {
    let body: TemplateIdsBody =
        serde_json::from_value(body).map_err(|_| ApiError::invalid("body"))?;
    Ok(domain::bulk_ids(&body.hsm_ids)?)
}

/// `POST /v1/wabas/{waba_id}/templates/archive`: archive templates.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/templates/archive",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = TemplateIdsBody,
    responses(
        (status = 200, description = "Meta's answer: the templates archived, and why each other one was not; each arrives as a `template_status_updated` event", body = TemplatesArchived),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or a template not in this WABA (among its first 1,000); nothing archived", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `hsm_ids` (none, or more than 100), `hsm_ids[i]` or `body`; `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: templates may have been archived", body = ErrorBody),
    )
)]
pub(crate) async fn archive_templates(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiJson(body): ApiJson<Value>,
) -> Result<Json<TemplatesArchived>, ApiError> {
    let ids = bulk_ids(body)?;
    let archived = templates(&state, &caller, &owned)?.archive(&ids).await?;
    Ok(Json(TemplatesArchived::from(archived)))
}

/// `POST /v1/wabas/{waba_id}/templates/unarchive`: unarchive templates
/// within 28 days of their archive.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/templates/unarchive",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = TemplateIdsBody,
    responses(
        (status = 200, description = "Meta's answer: the templates unarchived, and why each other one was not; each arrives as a `template_status_updated` event", body = TemplatesUnarchived),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or a template not among this WABA's archived ones (its first 1,000); nothing unarchived", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `hsm_ids` (none, or more than 100), `hsm_ids[i]` or `body`; `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: templates may have been unarchived", body = ErrorBody),
    )
)]
pub(crate) async fn unarchive_templates(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiJson(body): ApiJson<Value>,
) -> Result<Json<TemplatesUnarchived>, ApiError> {
    let ids = bulk_ids(body)?;
    let unarchived = templates(&state, &caller, &owned)?.unarchive(&ids).await?;
    Ok(Json(TemplatesUnarchived::from(unarchived)))
}

// ─── The Template Library ────────────────────────────────────────────────

/// The Template Library's filters (`templates/template-library`, "Query
/// string parameters").
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LibraryFilter {
    /// A substring of a library template's content, name, header, body or
    /// footer.
    pub search: Option<String>,
    /// Its topic: `ACCOUNT_UPDATE`, `CUSTOMER_FEEDBACK`,
    /// `ORDER_MANAGEMENT`, `PAYMENTS`.
    pub topic: Option<String>,
    /// Its use case (`SHIPMENT_CONFIRMATION`, `DELIVERY_FAILED`, …).
    pub usecase: Option<String>,
    /// Its industry: `E_COMMERCE`, `FINANCIAL_SERVICES`.
    pub industry: Option<String>,
    /// Its language (`en_US`).
    pub language: Option<String>,
    /// Its exact name (`delivery_update_1`).
    pub name: Option<String>,
}

impl From<LibraryFilter> for domain::LibraryFilter {
    fn from(filter: LibraryFilter) -> Self {
        let LibraryFilter {
            search,
            topic,
            usecase,
            industry,
            language,
            name,
        } = filter;
        Self {
            search,
            topic,
            usecase,
            industry,
            language,
            name,
        }
    }
}

/// A Template Library entry, as Meta describes it.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = LibraryTemplate)]
pub struct LibraryTemplateView {
    /// The entry's id.
    pub id: String,
    /// Its name: a creation's `library_template_name`.
    pub name: String,
    /// Its language.
    #[schema(required = true)]
    pub language: Option<String>,
    /// Its category, as Meta names it (`UTILITY`, `AUTHENTICATION`).
    #[schema(required = true)]
    pub category: Option<String>,
    /// Its topic.
    #[schema(required = true)]
    pub topic: Option<String>,
    /// Its use case.
    #[schema(required = true)]
    pub usecase: Option<String>,
    /// Its industries.
    pub industry: Vec<String>,
    /// Its header text.
    #[schema(required = true)]
    pub header: Option<String>,
    /// Its body, with positional placeholders.
    #[schema(required = true)]
    pub body: Option<String>,
    /// Example values of the body's placeholders.
    pub body_params: Vec<String>,
    /// Their types (`TEXT`, `AMOUNT`, `DATE`, …), checked by Meta when the
    /// template is sent.
    pub body_param_types: Vec<String>,
    /// Its buttons, in Meta's JSON.
    #[schema(value_type = Vec<std::collections::HashMap<String, Value>>)]
    pub buttons: Vec<Value>,
}

impl From<LibraryTemplate> for LibraryTemplateView {
    fn from(entry: LibraryTemplate) -> Self {
        let LibraryTemplate {
            id,
            name,
            language,
            category,
            topic,
            usecase,
            industry,
            header,
            body,
            body_params,
            body_param_types,
            buttons,
            ..
        } = entry;
        Self {
            id,
            name,
            language,
            category: wire(category.as_ref()),
            topic,
            usecase,
            industry,
            header,
            body,
            body_params,
            body_param_types,
            buttons: buttons
                .iter()
                .filter_map(|button| serde_json::to_value(button).ok())
                .collect(),
        }
    }
}

/// The Template Library's entries.
#[derive(Debug, Serialize, ToSchema)]
pub struct LibraryTemplateList {
    /// Meta's first page (the library documents no cursor).
    pub data: Vec<LibraryTemplateView>,
}

/// `GET /v1/wabas/{waba_id}/templates/library`: browse the Template
/// Library, with the WABA's token.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/templates/library",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        LibraryFilter,
    ),
    responses(
        (status = 200, description = "The library's entries matching the filters", body = LibraryTemplateList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `search`, `topic`, `usecase`, `industry`, `language`, `name` or `query`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_library_templates(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiQuery(filter): ApiQuery<LibraryFilter>,
) -> Result<Json<LibraryTemplateList>, ApiError> {
    let entries = templates(&state, &caller, &owned)?
        .library(filter.into())
        .await?;
    Ok(Json(LibraryTemplateList {
        data: entries.into_iter().map(LibraryTemplateView::from).collect(),
    }))
}

/// A library button's URL.
#[derive(Deserialize, ToSchema)]
#[allow(dead_code)] // schema only
pub struct LibraryUrlInput {
    /// The URL, with an optional `{{1}}` suffix placeholder.
    pub base_url: String,
    /// The full URL with its suffix filled in.
    pub url_suffix_example: Option<String>,
}

/// A library template's button input (`library_template_button_inputs`).
#[derive(Deserialize, ToSchema)]
#[allow(dead_code)] // schema only
pub struct LibraryButtonInput {
    /// `QUICK_REPLY`, `URL`, `PHONE_NUMBER`, `OTP`, `MPM`, `CATALOG`,
    /// `FLOW`, `VOICE_CALL` or `APP`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The business's phone number.
    pub phone_number: Option<String>,
    /// The business's website.
    pub url: Option<LibraryUrlInput>,
    /// `COPY_CODE` or `ONE_TAP` (`ZERO_TAP` accepts Meta's zero-tap terms,
    /// which the service does not do for a tenant:
    /// `zero_tap_terms_accepted: true` is refused).
    pub otp_type: Option<String>,
    /// A one-tap button's Android apps.
    pub supported_apps: Option<Vec<SupportedAppBody>>,
}

/// A library template's optional additions (`library_template_body_inputs`).
#[derive(ToSchema)]
#[allow(dead_code)] // schema only
pub struct LibraryBodyInputs {
    /// Mention calling the business.
    pub add_contact_number: Option<bool>,
    /// Add a learn-more link (not widely available).
    pub add_learn_more_link: Option<bool>,
    /// Add "do not share this code".
    pub add_security_recommendation: Option<bool>,
    /// Add a package-tracking link (not widely available).
    pub add_track_package_link: Option<bool>,
    /// When the code expires, in minutes.
    pub code_expiration_minutes: Option<u32>,
}

/// A template created from a Template Library entry, in Meta's JSON
/// (`templates/template-library`, "Creating templates"): the button inputs
/// are an array of objects. Read as Meta's object: a key the service
/// would not pass on is refused on its path.
#[derive(ToSchema)]
#[allow(dead_code)] // read as Meta's object; the type documents it
#[schema(as = LibraryTemplateRequest)]
pub struct LibraryTemplateBody {
    /// Your template's name (lowercase letters, digits and `_`).
    pub name: String,
    /// Its language (`en_US`).
    pub language: String,
    /// `UTILITY` (or `AUTHENTICATION` for an authentication entry).
    pub category: String,
    /// The library entry's exact name (`delivery_update_1`).
    pub library_template_name: String,
    /// The business's website or phone number, for an entry with buttons
    /// (then required).
    pub library_template_button_inputs: Option<Vec<LibraryButtonInput>>,
    /// Optional additions.
    pub library_template_body_inputs: Option<LibraryBodyInputs>,
}

/// Create `request` in the WABA: `201`, or Meta's refusal.
async fn create_from_library(
    templates: &Templates<'_>,
    request: &LibraryTemplateRequest,
) -> Result<Success, ApiError> {
    let created = templates.create_from_library(request).await?;
    Success::json(StatusCode::CREATED, &TemplateCreatedView::from(created))
}

/// `POST /v1/wabas/{waba_id}/templates/library`: create a template from a
/// Template Library entry.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/templates/library",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never create twice"),
    ),
    request_body = LibraryTemplateBody,
    responses(
        (status = 201, description = "Created (a library template unchanged is usually `APPROVED` at once)", body = TemplateCreatedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`template_limit_reached`, `number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (with `field`: `name`, `language`, `library_template_name`, a key the service would not send to Meta, or `zero_tap_terms_accepted: true`), `template_rejected`, `invalid_parameter`, `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the template may have been created", body = ErrorBody),
    )
)]
pub(crate) async fn create_library_template(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::create_library_template(owned.waba_id(), &body);
    let request = match domain::library_request(&body) {
        Ok(request) => request,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let templates = match templates(&state, &caller, &owned) {
        Ok(templates) => templates,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(
            &caller,
            key,
            fingerprint,
            create_from_library(&templates, &request),
        )
        .await
}

// ─── Migrate ─────────────────────────────────────────────────────────────

/// A migration into the path's WABA (`templates/template-migration`):
/// Meta recreates the source's `APPROVED` templates of `GREEN` or
/// `UNKNOWN` quality; both WABAs must be the same business's, and the
/// source the same tenant's.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = TemplateMigration)]
pub struct MigrationBody {
    /// The source WABA: one of the tenant's.
    pub source_waba_id: String,
    /// The zero-based batch of 500 templates.
    pub page_number: Option<u32>,
    /// The batch's size, 1 to 500 (lower it if Meta times out).
    pub count: Option<u32>,
    /// Only these templates of the source (retrying failures, say), at
    /// most 500.
    pub template_ids: Option<Vec<String>>,
}

/// The templates migrated, and why each other one was not.
#[derive(Debug, Serialize, ToSchema)]
pub struct TemplateMigrationResult {
    /// The ids of the templates recreated in the destination.
    pub migrated_templates: Vec<String>,
    /// Source template id → Meta's reason ("Incorrect category").
    pub failed_templates: BTreeMap<String, String>,
}

impl From<MigrationResult> for TemplateMigrationResult {
    fn from(result: MigrationResult) -> Self {
        let MigrationResult {
            migrated_templates,
            failed_templates,
            ..
        } = result;
        Self {
            migrated_templates: migrated_templates
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            failed_templates: failed_templates
                .into_iter()
                .map(|(id, reason)| (id.as_str().to_owned(), reason))
                .collect(),
        }
    }
}

/// The core's migration for `body`, or `422` on the field.
fn migration(body: Value) -> Result<domain::Migration, ApiError> {
    let body: MigrationBody =
        serde_json::from_value(body).map_err(|_| ApiError::invalid("body"))?;
    let MigrationBody {
        source_waba_id,
        page_number,
        count,
        template_ids,
    } = body;
    Ok(domain::migration(domain::MigrationRequest {
        source_waba_id,
        page_number,
        count,
        template_ids: template_ids.unwrap_or_default(),
    })?)
}

/// Migrate `migration`: `200` with Meta's answer, or its refusal.
async fn migrate(
    templates: &Templates<'_>,
    migration: &domain::Migration,
) -> Result<Success, ApiError> {
    let migrated = templates.migrate(migration).await?;
    Success::json(StatusCode::OK, &TemplateMigrationResult::from(migrated))
}

/// `POST /v1/wabas/{waba_id}/templates/migrations`: recreate another of the
/// tenant's WABAs' templates in this one.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/templates/migrations",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "The destination WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never migrate twice"),
    ),
    request_body = MigrationBody,
    responses(
        (status = 200, description = "Meta's answer: the templates recreated, and why each other one was not", body = TemplateMigrationResult),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA (destination or source) for this tenant, or a template id not the source's; nothing migrated", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required` (either WABA), `template_limit_reached`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `source_waba_id`, `count`, `template_ids`, `template_ids[i]` or `body`; `invalid_parameter`, `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: templates may have been migrated", body = ErrorBody),
    )
)]
pub(crate) async fn migrate_templates(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::migrate_templates(owned.waba_id(), &body);
    let migration = match migration(body) {
        Ok(migration) => migration,
        Err(error) => return error.into_response(),
    };
    let templates = match templates(&state, &caller, &owned) {
        Ok(templates) => templates,
        Err(error) => return error.into_response(),
    };
    // Boxed: the migration's future (two WABAs, a walk) is large.
    Box::pin(state.idempotent(&caller, key, fingerprint, migrate(&templates, &migration))).await
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::ids::TemplateId;
    use serde_json::json;

    use super::*;

    fn id(s: &str) -> TemplateId {
        TemplateId::new(s).unwrap()
    }

    /// Every field of the library's answers reaches the service's, under
    /// Meta's names. Decisive: each field of the conversions.
    #[test]
    fn every_field_of_metas_answers_is_answered() {
        // The library's answers are `#[non_exhaustive]` (roadmap L20b): read
        // from Meta's JSON, as the library does.
        let metric: ComparisonMetric = serde_json::from_value(json!({
            "metric": "MESSAGE_SENDS",
            "type": "NUMBER_VALUES",
            "order_by_relative_metric": ["1533406637136032"],
            "number_values": [{"key": "5289179717853347", "value": 1273}],
            "string_values": [{"key": "5289179717853347", "value": "UNKNOWN_BLOCK_REASON"}],
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(ComparisonMetricView::from(metric)).unwrap(),
            json!({"metric": "MESSAGE_SENDS", "type": "NUMBER_VALUES",
                   "order_by_relative_metric": ["1533406637136032"],
                   "number_values": [{"key": "5289179717853347", "value": 1273}],
                   "string_values": [{"key": "5289179717853347", "value": "UNKNOWN_BLOCK_REASON"}]})
        );
        let failed: BTreeMap<TemplateId, String> =
            [(id("1019496902803242"), "Incorrect category".to_owned())].into();
        assert_eq!(
            serde_json::to_value(TemplateMigrationResult::from(
                serde_json::from_value::<MigrationResult>(json!({
                    "migrated_templates": ["1473688840035974"],
                    "failed_templates": failed.clone(),
                }))
                .unwrap(),
            ))
            .unwrap(),
            json!({"migrated_templates": ["1473688840035974"],
                   "failed_templates": {"1019496902803242": "Incorrect category"}})
        );
        assert_eq!(
            serde_json::to_value(TemplatesArchived::from(
                serde_json::from_value::<ArchiveResult>(json!({
                    "archived_templates": ["1473688840035974"],
                    "failed_templates": failed.clone(),
                }))
                .unwrap(),
            ))
            .unwrap(),
            json!({"archived_templates": ["1473688840035974"],
                   "failed_templates": {"1019496902803242": "Incorrect category"}})
        );
        assert_eq!(
            serde_json::to_value(TemplatesUnarchived::from(
                serde_json::from_value::<UnarchiveResult>(json!({
                    "unarchived_templates": ["1473688840035974"],
                    "failed_templates": failed,
                }))
                .unwrap()
            ))
            .unwrap(),
            json!({"unarchived_templates": ["1473688840035974"],
                   "failed_templates": {"1019496902803242": "Incorrect category"}})
        );
        let filter = domain::LibraryFilter::from(LibraryFilter {
            search: Some("payments".to_owned()),
            topic: Some("PAYMENTS".to_owned()),
            usecase: Some("LOW_BALANCE_WARNING".to_owned()),
            industry: Some("FINANCIAL_SERVICES".to_owned()),
            language: Some("en_US".to_owned()),
            name: Some("low_balance_warning_1".to_owned()),
        });
        assert_eq!(
            filter,
            domain::LibraryFilter {
                search: Some("payments".to_owned()),
                topic: Some("PAYMENTS".to_owned()),
                usecase: Some("LOW_BALANCE_WARNING".to_owned()),
                industry: Some("FINANCIAL_SERVICES".to_owned()),
                language: Some("en_US".to_owned()),
                name: Some("low_balance_warning_1".to_owned()),
            }
        );
    }
}
