//! Templates (scope `templates`; docs/design/server.md, section 4.2).
//!
//! | Route | Graph call, with the tenant's WABA token |
//! | --- | --- |
//! | `GET /v1/wabas/{waba_id}/templates` | `GET /{waba_id}/message_templates?fields=name,language,status,category,components&limit=…[&status=…][&name=…][&after=…]`, cached 60 s per WABA |
//! | `GET /v1/wabas/{waba_id}/templates/{id}` | `GET /{id}?fields=name`, then `GET /{waba_id}/message_templates?fields=…&name=…` (the id must be among them) |
//! | `POST /v1/wabas/{waba_id}/templates` | `POST /{waba_id}/message_templates` |
//! | `DELETE /v1/wabas/{waba_id}/templates?name=[&id=]` | `DELETE /{waba_id}/message_templates?name=…`; with an id, `GET /{waba_id}/message_templates?fields=name&name=…` first (the id must be among them), then `…&hsm_id=…` |
//!
//! Pages: `templates/template-management`, `templates/overview`,
//! `reference/whatsapp-business-account/message-template-api`.
//!
//! **A template id is the WABA's, or it does not exist.** The WABA is the
//! tenant's (step 4 of the authorization order); a template id is Meta's,
//! and one token may reach several tenants' WABAs (the platform's system
//! user token attached to WABAs of different tenants). Meta's template
//! object does not name its WABA
//! (`reference/whatsapp-business-account/message-template-api`), so the
//! id is looked for through the WABA's own edge, by the template's name:
//! a template of another WABA, or none, is `404 not_found`, the same
//! answer, without Meta's code or text, and nothing of it is answered or
//! deleted. The bare id is only asked for its name, never acted on. The
//! lookup reads at most 5 pages of 100 templates (a template of the WABA
//! past them is `404` too), through the list's `name` filter, which
//! Meta's reference for the list does not document; it costs 2 to 6
//! management calls (`GET …/{id}`: the name, then the pages; a deletion
//! by id: the pages, then the deletion), never cached.
//!
//! Meta allows 200 management calls an hour per WABA: a list is cached
//! for 60 seconds **per WABA** (and per query and tenant), on each
//! replica, and a creation or deletion on that replica drops the WABA's
//! entries. A
//! creation is Meta's own JSON (a `TemplateDefinition`), checked locally
//! against every limit the docs state before any request (`422
//! invalid_request` with `field`); Meta refusing its content is `422
//! template_rejected`, a WABA at its template limit `409
//! template_limit_reached`. Creation takes an `Idempotency-Key`. Review
//! results arrive as `template_status_updated` events.
//!
//! Since roadmap S5c the rules are the core's
//! ([`meta_whatsapp_server_core::templates`]: the filters, the cache, the
//! lookup of an id in its WABA, the definition's checks, creation and
//! deletion, as the `Templates` service, which opens only on a WABA of the
//! admitted caller's own tenant). This module keeps the routes, the query
//! and answer types with their OpenAPI schema, converts them to and from
//! the core's (every field named on both sides), and renders the answers;
//! the replica's cache is the state's.

use std::collections::HashMap;

use meta_whatsapp_rs::client::templates::TemplateDefinition;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequestParts, Path, Query, State};
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode, Uri};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::templates::{self as domain, Templates};
pub use meta_whatsapp_server_core::templates::{
    TEMPLATE_CACHE_ENTRIES, TEMPLATE_CACHE_PER_WABA, TEMPLATE_FIELDS, TemplateCache,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::{ApiJson, PageParams, PageQuery, encode_cursor};
use crate::auth::{Caller, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{self, Fingerprint, KeyHeader, Success};
use crate::state::AppState;

/// A template, as Meta describes it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = Template)]
pub struct TemplateView {
    /// Template id.
    pub id: String,
    /// Name (several languages share one).
    #[schema(required = true)]
    pub name: Option<String>,
    /// Language code, e.g. `en_US`.
    #[schema(required = true)]
    pub language: Option<String>,
    /// Review status, as Meta names it (`APPROVED`, `PENDING`, `REJECTED`,
    /// `PAUSED`, …): only `APPROVED` templates can be sent.
    #[schema(required = true)]
    pub status: Option<String>,
    /// Category, as Meta names it (`MARKETING`, `UTILITY`,
    /// `AUTHENTICATION`).
    #[schema(required = true)]
    pub category: Option<String>,
    /// Components, in Meta's JSON (`templates/components`).
    #[schema(value_type = Vec<HashMap<String, Value>>)]
    pub components: Vec<Value>,
}

/// A page of templates.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TemplateList {
    /// The templates.
    pub data: Vec<TemplateView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// A created template, as Meta answered.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = TemplateCreated)]
pub struct TemplateCreatedView {
    /// Template id.
    pub id: String,
    /// Initial status (usually `PENDING`).
    #[schema(required = true)]
    pub status: Option<String>,
    /// The category Meta assigned.
    #[schema(required = true)]
    pub category: Option<String>,
}

/// A template definition in Meta's JSON (`name`, `language`, `category`,
/// `parameter_format`, `components`), as `templates/overview` writes it.
#[derive(Debug, ToSchema)]
#[schema(as = TemplateDefinition, value_type = HashMap<String, Value>)]
pub struct TemplateDefinitionBody(pub HashMap<String, Value>);

// ─── To and from the core's types ────────────────────────────────────────

impl From<domain::Template> for TemplateView {
    fn from(template: domain::Template) -> Self {
        let domain::Template {
            id,
            name,
            language,
            status,
            category,
            components,
        } = template;
        Self {
            id: id.into_inner(),
            name,
            language,
            status,
            category,
            components,
        }
    }
}

impl From<&domain::TemplatePage> for TemplateList {
    fn from(page: &domain::TemplatePage) -> Self {
        let domain::TemplatePage {
            templates,
            next_after,
        } = page;
        Self {
            data: templates.iter().cloned().map(TemplateView::from).collect(),
            next_cursor: next_after.as_deref().map(encode_cursor),
        }
    }
}

impl From<domain::TemplateCreated> for TemplateCreatedView {
    fn from(created: domain::TemplateCreated) -> Self {
        let domain::TemplateCreated {
            id,
            status,
            category,
        } = created;
        Self {
            id: id.into_inner(),
            status,
            category,
        }
    }
}

impl From<TemplateFilter> for domain::TemplateFilter {
    fn from(filter: TemplateFilter) -> Self {
        let TemplateFilter { status, name } = filter;
        Self { status, name }
    }
}

/// The core's template operations on `owned`, for `caller`, cached in
/// `state`'s cache and counted in its metrics (`caller` and `owned` are
/// the state's own: the extractors made them with `state.authz()`, which
/// admits them).
fn templates<'a>(
    state: &'a AppState,
    caller: &'a Caller,
    owned: &'a OwnedWaba,
) -> Result<Templates<'a>, ApiError> {
    Ok(Templates::new(
        state.authz(),
        caller,
        owned.capability(),
        state.template_cache(),
    )?
    .observed_by(state.metrics()))
}

// ─── List ────────────────────────────────────────────────────────────────

/// A query string of `T`; one that does not parse (a missing `name`, say)
/// is `422 invalid_request` on `query`.
#[derive(Debug)]
pub struct ApiQuery<T>(pub T);

impl<T, S> FromRequestParts<S> for ApiQuery<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|_| ApiError::invalid("query"))
    }
}

/// `?status=` and `?name=`, as the OpenAPI document declares them.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TemplateFilter {
    /// Only templates with this status, as Meta names it (`APPROVED`,
    /// `REJECTED`, …).
    pub status: Option<String>,
    /// Only templates with this name.
    pub name: Option<String>,
}

/// `GET /v1/wabas/{waba_id}/templates`: the WABA's templates, cached 60 s
/// per WABA.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/templates",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        TemplateFilter,
        PageParams,
    ),
    responses(
        (status = 200, description = "A page of templates (at most 60 seconds old)", body = TemplateList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `status`, `name`, `limit` or `cursor`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_templates(
    State(state): State<AppState>,
    caller: Caller,
    owned: OwnedWaba,
    ApiQuery(filter): ApiQuery<TemplateFilter>,
    PageQuery(page): PageQuery,
) -> Result<Json<TemplateList>, ApiError> {
    let listed = templates(&state, &caller, &owned)?
        .list(filter.into(), page.after, page.limit)
        .await?;
    Ok(Json(TemplateList::from(listed.as_ref())))
}

// ─── Get ─────────────────────────────────────────────────────────────────

/// `GET /v1/wabas/{waba_id}/templates/{id}`: one template.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/templates/{id}",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("id" = String, Path, description = "Template id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The template", body = TemplateView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such template in this WABA (another WABA's template id looks like a missing one)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `id`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_template(
    State(state): State<AppState>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, id)): Path<(String, String)>,
) -> Result<Json<TemplateView>, ApiError> {
    let template = templates(&state, &caller, &owned)?.get(&id).await?;
    Ok(Json(TemplateView::from(template)))
}

// ─── Create ──────────────────────────────────────────────────────────────

/// Create `definition` in the WABA: `201`, or Meta's refusal with its
/// `details` (the core's [`Templates::create`]; the WABA's cached lists go
/// either way).
async fn create(
    templates: &Templates<'_>,
    definition: &TemplateDefinition,
) -> Result<Success, ApiError> {
    let created = templates.create(definition).await?;
    Success::json(StatusCode::CREATED, &TemplateCreatedView::from(created))
}

/// `POST /v1/wabas/{waba_id}/templates`: create a template from Meta's
/// JSON, checked locally first.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/templates",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and definition never create twice"),
    ),
    request_body(content = TemplateDefinitionBody, description = "Meta's template definition: `name`, `language`, `category`, `parameter_format`, `components` (templates/overview)"),
    responses(
        (status = 201, description = "Submitted for review; the result arrives as a `template_status_updated` event", body = TemplateCreatedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`template_limit_reached`, `number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (with `field`: the definition breaks a documented limit, or holds a key the service would not send to Meta), `template_rejected`, `invalid_parameter`, `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the template may have been created", body = ErrorBody),
    )
)]
pub(crate) async fn create_template(
    State(state): State<AppState>,
    caller: Caller,
    owned: OwnedWaba,
    KeyHeader(key): KeyHeader,
    uri: Uri,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::json(&Method::POST, uri.path(), &body);
    let definition = match domain::definition(&body) {
        Ok(definition) => definition,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let templates = match templates(&state, &caller, &owned) {
        Ok(templates) => templates,
        Err(error) => return error.into_response(),
    };
    idempotency::run(
        &state,
        &caller,
        key,
        fingerprint,
        create(&templates, &definition),
    )
    .await
}

// ─── Delete ──────────────────────────────────────────────────────────────

/// `?name=` and `?id=` of a deletion.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TemplateDeletion {
    /// The template's name: every language of it goes, unless `id` names
    /// one.
    pub name: String,
    /// One template (one language) of that name.
    pub id: Option<String>,
}

/// `DELETE /v1/wabas/{waba_id}/templates?name=[&id=]`: delete every
/// language of a name, or one.
#[utoipa::path(
    delete,
    path = "/v1/wabas/{waba_id}/templates",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        TemplateDeletion,
    ),
    responses(
        (status = 204, description = "Deleted (an approved template's name cannot be reused for 30 days)"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or (with `id`) no template of that name and id in this WABA", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `name` or `id`, or Meta's `invalid_parameter` (no template of that name)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn delete_templates(
    State(state): State<AppState>,
    caller: Caller,
    owned: OwnedWaba,
    ApiQuery(deletion): ApiQuery<TemplateDeletion>,
) -> Result<StatusCode, ApiError> {
    let TemplateDeletion { name, id } = deletion;
    templates(&state, &caller, &owned)?
        .delete(&name, id.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::ids::TemplateId;
    use serde_json::json;

    use super::*;

    /// Every field of the core's answers reaches the service's, the
    /// cursor hex-encoded as before roadmap S5c, and the filter reaches
    /// the core as given (the cache and the checks are the core's:
    /// `meta_whatsapp_server_core::templates`' tests). Decisive: each
    /// field of the conversions.
    #[test]
    fn every_field_of_the_cores_answers_is_answered() {
        let template = domain::Template {
            id: TemplateId::new("1304694804498707").unwrap(),
            name: Some("coupon_expiration_reminder_number_vars".to_owned()),
            language: Some("en".to_owned()),
            status: Some("APPROVED".to_owned()),
            category: Some("MARKETING".to_owned()),
            components: vec![json!({"type": "FOOTER", "text": "Lucky Shrub Succulents"})],
        };
        let page = domain::TemplatePage {
            templates: vec![template],
            next_after: Some("QVFIUafter".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(TemplateList::from(&page)).unwrap(),
            json!({
                "data": [{
                    "id": "1304694804498707",
                    "name": "coupon_expiration_reminder_number_vars",
                    "language": "en",
                    "status": "APPROVED",
                    "category": "MARKETING",
                    "components": [{"type": "FOOTER", "text": "Lucky Shrub Succulents"}]
                }],
                "next_cursor": encode_cursor("QVFIUafter")
            })
        );
        let created = domain::TemplateCreated {
            id: TemplateId::new("1627019861106475").unwrap(),
            status: Some("PENDING".to_owned()),
            category: Some("MARKETING".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(TemplateCreatedView::from(created)).unwrap(),
            json!({"id": "1627019861106475", "status": "PENDING", "category": "MARKETING"})
        );
        let filter = domain::TemplateFilter::from(TemplateFilter {
            status: Some("approved".to_owned()),
            name: Some("order_confirmation".to_owned()),
        });
        assert_eq!(
            filter,
            domain::TemplateFilter {
                status: Some("approved".to_owned()),
                name: Some("order_confirmation".to_owned()),
            }
        );
    }
}
