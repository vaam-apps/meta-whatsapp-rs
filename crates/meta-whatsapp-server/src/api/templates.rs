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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use meta_whatsapp_rs::client::templates::{
    TemplateDefinition, TemplateInfo, TemplateListQuery, TemplateStatus,
};
use meta_whatsapp_rs::core::ids::{TemplateId, WabaId};
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequestParts, Path, Query, State};
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode, Uri};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::{Duration, Instant};
use utoipa::ToSchema;

use super::common::{ApiJson, PageParams, PageQuery, encode_cursor, graph_id, meta_object};
use crate::auth::{Caller, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{self, Fingerprint, KeyHeader, Success};
use crate::model::TenantId;
use crate::state::AppState;
use crate::telemetry;

/// The fields asked of Meta for each template.
pub const TEMPLATE_FIELDS: [&str; 5] = ["name", "language", "status", "category", "components"];

/// Most list pages cached on a replica; past it, the least recently used
/// page goes.
pub const TEMPLATE_CACHE_ENTRIES: usize = 512;

/// Most list pages cached for one tenant's WABA (its queries and cursors);
/// past it, that WABA's least recently used page goes, never another
/// WABA's.
pub const TEMPLATE_CACHE_PER_WABA: usize = 32;

/// Longest template name (`templates/template-management`: 512
/// characters of `a-z 0-9 _`).
const MAX_NAME_LEN: usize = 512;

/// Page size of the lookups that find a template id among a WABA's
/// templates of one name.
const LOOKUP_PAGE_SIZE: u32 = 100;

/// Most pages such a lookup reads: a name has one template per language
/// (Meta supports about 70), so one page is the rule; past these, the id
/// counts as not the WABA's.
const LOOKUP_PAGES: usize = 5;

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

/// A value of Meta's enum, as its wire string.
fn wire<T: Serialize>(value: Option<&T>) -> Option<String> {
    value
        .and_then(|v| serde_json::to_value(v).ok())
        .and_then(|v| v.as_str().map(str::to_owned))
}

fn view(info: TemplateInfo) -> TemplateView {
    TemplateView {
        status: wire(info.status.as_ref()),
        category: wire(info.category.as_ref()),
        components: info
            .components
            .iter()
            .filter_map(|c| serde_json::to_value(c).ok())
            .collect(),
        id: info.id.into_inner(),
        name: info.name,
        language: info.language,
    }
}

// ─── The cache ───────────────────────────────────────────────────────────

/// What a cached page answers: one tenant's WABA, one query. The tenant
/// is in the key although a WABA has one tenant at a time: a page cached
/// before an unbind is never answered to the next tenant bound to it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    tenant: String,
    waba_id: String,
    status: Option<String>,
    name: Option<String>,
    after: Option<String>,
    limit: usize,
}

#[derive(Debug)]
struct Cached {
    /// When Meta answered it.
    at: Instant,
    /// When it was last answered from here.
    used: Instant,
    page: Arc<TemplateList>,
}

/// Template lists cached per WABA, on one replica.
#[derive(Debug)]
pub struct TemplateCache {
    ttl: Duration,
    entries: Mutex<HashMap<CacheKey, Cached>>,
}

impl TemplateCache {
    /// An empty cache keeping pages for `ttl`.
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, key: &CacheKey) -> Option<Arc<TemplateList>> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let cached = entries.get_mut(key)?;
        if cached.at.elapsed() >= self.ttl {
            return None;
        }
        cached.used = Instant::now();
        Some(cached.page.clone())
    }

    /// Keep `page`: expired pages go first; then, past
    /// [`TEMPLATE_CACHE_PER_WABA`], the WABA's least recently used page,
    /// and past [`TEMPLATE_CACHE_ENTRIES`], the replica's.
    fn put(&self, key: CacheKey, page: Arc<TemplateList>) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let ttl = self.ttl;
        entries.retain(|_, cached| cached.at.elapsed() < ttl);
        entries.remove(&key);
        let same_waba = |k: &CacheKey| k.tenant == key.tenant && k.waba_id == key.waba_id;
        if entries.keys().filter(|k| same_waba(k)).count() >= TEMPLATE_CACHE_PER_WABA {
            let lru = entries
                .iter()
                .filter(|(k, _)| same_waba(k))
                .min_by_key(|(_, cached)| cached.used)
                .map(|(k, _)| k.clone());
            if let Some(lru) = lru {
                entries.remove(&lru);
            }
        }
        if entries.len() >= TEMPLATE_CACHE_ENTRIES {
            let lru = entries
                .iter()
                .min_by_key(|(_, cached)| cached.used)
                .map(|(k, _)| k.clone());
            if let Some(lru) = lru {
                entries.remove(&lru);
            }
        }
        let now = Instant::now();
        entries.insert(
            key,
            Cached {
                at: now,
                used: now,
                page,
            },
        );
    }

    /// Drop every page of `waba_id`: its templates changed.
    pub fn invalidate(&self, waba_id: &WabaId) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|key, _| key.waba_id != waba_id.as_str());
    }
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

/// A template name: `a-z 0-9 _`, at most 512 characters.
fn template_name(field: &'static str, name: &str) -> Result<(), ApiError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(ApiError::invalid(field))
    }
}

/// A status filter: letters and `_`, upper-cased as Meta writes it.
fn status_filter(status: &str) -> Result<String, ApiError> {
    let valid = !status.is_empty()
        && status.len() <= 64
        && status.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_');
    if valid {
        Ok(status.to_ascii_uppercase())
    } else {
        Err(ApiError::invalid("status"))
    }
}

/// One page of the WABA's templates, from the cache or from Meta.
pub(crate) async fn list(
    state: &AppState,
    tenant: &TenantId,
    owned: &OwnedWaba,
    filter: TemplateFilter,
    after: Option<String>,
    limit: usize,
) -> Result<Arc<TemplateList>, ApiError> {
    let status = filter.status.as_deref().map(status_filter).transpose()?;
    if let Some(name) = &filter.name {
        template_name("name", name)?;
    }
    let key = CacheKey {
        tenant: tenant.as_str().to_owned(),
        waba_id: owned.waba_id().as_str().to_owned(),
        status,
        name: filter.name,
        after,
        limit,
    };
    let cache = state.template_cache();
    if let Some(page) = cache.get(&key) {
        return Ok(page);
    }
    let mut query = TemplateListQuery::new()
        .fields(TEMPLATE_FIELDS)
        .limit(u32::try_from(limit).unwrap_or(u32::MAX));
    query.status = key.status.as_deref().map(|s| {
        s.parse::<TemplateStatus>()
            .unwrap_or_else(|never| match never {})
    });
    query.name.clone_from(&key.name);
    query.after.clone_from(&key.after);
    let page = owned
        .client()
        .templates(owned.waba_id().clone())
        .list(&query)
        .await;
    let page = match page {
        Ok(page) => page,
        Err(error) => return Err(owned.failed(state, &error).await.with_details(&error)),
    };
    let next_cursor = page.next_cursor().map(encode_cursor);
    let listed = Arc::new(TemplateList {
        data: page.data.into_iter().map(view).collect(),
        next_cursor,
    });
    cache.put(key, listed.clone());
    Ok(listed)
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
    let listed = list(
        &state,
        caller.tenant(),
        &owned,
        filter,
        page.after,
        page.limit,
    )
    .await?;
    Ok(Json(TemplateList::clone(&listed)))
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
    owned: OwnedWaba,
    Path((_, id)): Path<(String, String)>,
) -> Result<Json<TemplateView>, ApiError> {
    graph_id("id", &id)?;
    let id = TemplateId::new(id);
    // Its name: what the WABA's edge finds templates by. Nothing else of
    // it is read before it is known to be the WABA's.
    let named = owned
        .client()
        .templates(owned.waba_id().clone())
        .get_fields(&id, &["name"])
        .await;
    let name = match named {
        Ok(info) => info.name.ok_or_else(ApiError::not_found)?,
        Err(error) => return Err(owned.failed_on_object(&state, &error).await),
    };
    let template = in_waba(&state, &owned, &name, &id, &TEMPLATE_FIELDS)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(view(template)))
}

/// The template `id` among the WABA's templates named `name`, with
/// `fields`, looked for through the WABA's own edge (`GET
/// /{waba_id}/message_templates?name=…`), or `None` when it is not the
/// WABA's: Meta's template object does not name its WABA, and a token may
/// reach other tenants' WABAs.
async fn in_waba(
    state: &AppState,
    owned: &OwnedWaba,
    name: &str,
    id: &TemplateId,
    fields: &[&str],
) -> Result<Option<TemplateInfo>, ApiError> {
    let templates = owned.client().templates(owned.waba_id().clone());
    let mut query = TemplateListQuery::new()
        .fields(fields.iter().copied())
        .name(name)
        .limit(LOOKUP_PAGE_SIZE);
    for _ in 0..LOOKUP_PAGES {
        let page = match templates.list(&query).await {
            Ok(page) => page,
            Err(error) => return Err(owned.failed(state, &error).await.with_details(&error)),
        };
        let next = page.next_cursor().map(str::to_owned);
        if let Some(found) = page.data.into_iter().find(|t| t.id == *id) {
            return Ok(Some(found));
        }
        match next {
            Some(after) => query.after = Some(after),
            None => return Ok(None),
        }
    }
    tracing::warn!("a template id was not found in the first pages of its name's templates");
    Ok(None)
}

// ─── Create ──────────────────────────────────────────────────────────────

/// Create `definition` in the WABA: `201`, or Meta's refusal with its
/// `details`. The WABA's cached lists go either way (a refusal can follow
/// a creation Meta did make).
pub(crate) async fn create(
    state: &AppState,
    owned: &OwnedWaba,
    definition: &TemplateDefinition,
) -> Result<Success, ApiError> {
    let created = owned
        .client()
        .templates(owned.waba_id().clone())
        .create(definition)
        .await;
    state.template_cache().invalidate(owned.waba_id());
    match created {
        Ok(created) => Success::json(
            StatusCode::CREATED,
            &TemplateCreatedView {
                status: wire(created.status.as_ref()),
                category: wire(created.category.as_ref()),
                id: created.id.into_inner(),
            },
        ),
        Err(error) => Err(owned.failed(state, &error).await.with_details(&error)),
    }
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
    let definition: TemplateDefinition = match meta_object("", &body) {
        Ok(definition) => definition,
        Err(error) => return error.into_response(),
    };
    if let Err(invalid) = definition.validate() {
        return ApiError::invalid(invalid.field).into_response();
    }
    idempotency::run(
        &state,
        caller.tenant(),
        key,
        fingerprint,
        create(&state, &owned, &definition),
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
    template_name("name", &deletion.name)?;
    if let Some(id) = &deletion.id {
        graph_id("id", id)?;
    }
    let templates = owned.client().templates(owned.waba_id().clone());
    let deleted = match &deletion.id {
        Some(id) => {
            // One of the WABA's own, or nothing is deleted: Meta's page
            // does not say it checks an `hsm_id` against the WABA.
            let id = TemplateId::new(id.clone());
            if in_waba(&state, &owned, &deletion.name, &id, &["name"])
                .await?
                .is_none()
            {
                return Err(ApiError::not_found());
            }
            templates.delete_by_id(&deletion.name, &id).await
        }
        None => templates.delete_by_name(&deletion.name).await,
    };
    state.template_cache().invalidate(owned.waba_id());
    match deleted {
        Ok(()) => {
            telemetry::tenant_audit(
                if deletion.id.is_some() {
                    "template_deleted"
                } else {
                    "templates_deleted"
                },
                caller.key_id(),
                &telemetry::Subject {
                    tenant: Some(caller.tenant().as_str()),
                    waba_id: Some(owned.waba_id().as_str()),
                    ..telemetry::Subject::default()
                },
            );
            Ok(StatusCode::NO_CONTENT)
        }
        Err(error) => Err(owned.failed(&state, &error).await.with_details(&error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(id: &str) -> Arc<TemplateList> {
        Arc::new(TemplateList {
            data: vec![TemplateView {
                id: id.to_owned(),
                name: None,
                language: None,
                status: None,
                category: None,
                components: Vec::new(),
            }],
            next_cursor: None,
        })
    }

    fn key(waba: &str) -> CacheKey {
        CacheKey {
            tenant: "merchant-42".to_owned(),
            waba_id: waba.to_owned(),
            status: None,
            name: None,
            after: None,
            limit: 50,
        }
    }

    /// Pages are kept per WABA and query for the TTL, and a WABA's
    /// invalidation drops its own only. Decisive: the WABA in the key.
    #[tokio::test(start_paused = true)]
    async fn pages_are_cached_per_waba_for_the_ttl() {
        let cache = TemplateCache::new(Duration::from_secs(60));
        cache.put(key("1"), page("a"));
        assert!(cache.get(&key("2")).is_none(), "another WABA's page");
        assert_eq!(cache.get(&key("1")).unwrap().data[0].id, "a");
        let other_query = CacheKey {
            status: Some("APPROVED".to_owned()),
            ..key("1")
        };
        assert!(cache.get(&other_query).is_none());
        let other_tenant = CacheKey {
            tenant: "merchant-43".to_owned(),
            ..key("1")
        };
        assert!(cache.get(&other_tenant).is_none(), "another tenant's page");
        cache.put(key("2"), page("b"));
        cache.invalidate(&WabaId::new("1"));
        assert!(cache.get(&key("1")).is_none());
        assert_eq!(cache.get(&key("2")).unwrap().data[0].id, "b");
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(cache.get(&key("2")).is_none(), "expired");
    }

    /// Bounded per WABA and per replica, the least recently used page
    /// going first: a new page is always kept, and one WABA's many queries
    /// never push another WABA's pages out. Decisive: the per-WABA cap and
    /// the recency.
    #[tokio::test(start_paused = true)]
    async fn the_cache_is_bounded_per_waba_and_least_recently_used_first() {
        let cache = TemplateCache::new(Duration::from_secs(60));
        let query = |waba: &str, after: usize| CacheKey {
            after: Some(after.to_string()),
            ..key(waba)
        };
        cache.put(key("other"), page("kept"));
        for i in 0..TEMPLATE_CACHE_PER_WABA + 10 {
            tokio::time::advance(Duration::from_millis(1)).await;
            cache.put(query("busy", i), page("x"));
            // The other WABA's page stays in use.
            assert!(cache.get(&key("other")).is_some(), "{i}");
        }
        let busy = cache
            .entries
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.waba_id == "busy")
            .count();
        assert_eq!(busy, TEMPLATE_CACHE_PER_WABA);
        assert!(
            cache.get(&query("busy", 0)).is_none(),
            "its oldest page went"
        );
        assert!(
            cache
                .get(&query("busy", TEMPLATE_CACHE_PER_WABA + 9))
                .is_some()
        );
        // Across the replica: the least recently used page goes, a new
        // page is always kept.
        for i in 0..TEMPLATE_CACHE_ENTRIES + 10 {
            tokio::time::advance(Duration::from_millis(1)).await;
            cache.put(key(&i.to_string()), page("x"));
            assert!(cache.get(&key(&i.to_string())).is_some(), "{i} was kept");
        }
        assert_eq!(cache.entries.lock().unwrap().len(), TEMPLATE_CACHE_ENTRIES);
        assert!(
            cache.get(&key("0")).is_none(),
            "the least recently used went"
        );
    }

    /// A page answered from the cache counts as used: the pages not asked
    /// for since go before it, in a WABA and across the replica (least
    /// recently used, not first in first out). Decisive: a hit refreshing
    /// the page's recency.
    #[tokio::test(start_paused = true)]
    async fn a_cache_hit_keeps_its_page_longest() {
        let cache = TemplateCache::new(Duration::from_secs(60));
        let tick = || tokio::time::advance(Duration::from_millis(1));
        let query = |after: usize| CacheKey {
            after: Some(after.to_string()),
            ..key("busy")
        };
        for i in 0..TEMPLATE_CACHE_PER_WABA {
            tick().await;
            cache.put(query(i), page("x"));
        }
        tick().await;
        assert!(
            cache.get(&query(0)).is_some(),
            "the oldest page, used again"
        );
        tick().await;
        cache.put(query(TEMPLATE_CACHE_PER_WABA), page("x"));
        assert!(cache.get(&query(0)).is_some(), "the page used again stayed");
        assert!(
            cache.get(&query(1)).is_none(),
            "the least recently used went"
        );
        // Across the replica: fill it with other WABAs' pages, use the
        // oldest again, add one more.
        let other = |i: usize| key(&format!("w{i}"));
        let room = TEMPLATE_CACHE_ENTRIES - cache.entries.lock().unwrap().len();
        for i in 0..room {
            tick().await;
            cache.put(other(i), page("x"));
        }
        assert_eq!(cache.entries.lock().unwrap().len(), TEMPLATE_CACHE_ENTRIES);
        tick().await;
        // `busy`'s pages were put before `other(0)`; use them all again,
        // then `other(0)`, the oldest left.
        for i in 2..=TEMPLATE_CACHE_PER_WABA {
            assert!(cache.get(&query(i)).is_some(), "{i}");
        }
        assert!(cache.get(&query(0)).is_some());
        assert!(cache.get(&other(0)).is_some());
        tick().await;
        cache.put(key("new"), page("x"));
        assert!(cache.get(&other(0)).is_some(), "used again: kept");
        assert!(
            cache.get(&other(1)).is_none(),
            "the least recently used went"
        );
    }

    #[test]
    fn filters_are_checked() {
        assert_eq!(status_filter("approved").unwrap(), "APPROVED");
        assert!(status_filter("APPROVED&x=1").is_err());
        assert!(template_name("name", "order_confirmation").is_ok());
        for bad in ["", "Order", "order confirmation", "a&b"] {
            assert!(template_name("name", bad).is_err(), "{bad:?}");
        }
        assert!(template_name("name", &"a".repeat(513)).is_err());
        assert!(graph_id("id", "1407680676729941").is_ok());
        assert!(graph_id("id", "../x").is_err());
    }
}
