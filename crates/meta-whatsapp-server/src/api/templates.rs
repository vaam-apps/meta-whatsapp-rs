//! Templates (scope `templates`; docs/design/server.md, section 4.2).
//!
//! | Route | Graph call, with the tenant's WABA token |
//! | --- | --- |
//! | `GET /v1/wabas/{waba_id}/templates` | `GET /{waba_id}/message_templates?fields=name,language,status,category,components&limit=…[&status=…][&name=…][&after=…]`, cached 60 s per WABA |
//! | `GET /v1/wabas/{waba_id}/templates/{id}` | `GET /{id}?fields=name,language,status,category,components` |
//! | `POST /v1/wabas/{waba_id}/templates` | `POST /{waba_id}/message_templates` |
//! | `DELETE /v1/wabas/{waba_id}/templates?name=[&id=]` | `DELETE /{waba_id}/message_templates?name=…[&hsm_id=…]` |
//!
//! Pages: `templates/template-management`, `templates/overview`,
//! `reference/whatsapp-business-account/message-template-api`.
//!
//! Meta allows 200 management calls an hour per WABA: a list is cached
//! for 60 seconds **per WABA** (and per query), on each replica, and a
//! creation or deletion on that replica drops the WABA's entries. A
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

use super::common::{ApiJson, PageParams, PageQuery, encode_cursor};
use crate::auth::{Caller, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{self, Fingerprint, KeyHeader, Success};
use crate::state::AppState;

/// The fields asked of Meta for each template.
pub const TEMPLATE_FIELDS: [&str; 5] = ["name", "language", "status", "category", "components"];

/// Most list pages cached on a replica; past it, expired entries go, then
/// the new page is not cached.
pub const TEMPLATE_CACHE_ENTRIES: usize = 512;

/// Longest template name (`templates/template-management`: 512
/// characters of `a-z 0-9 _`).
const MAX_NAME_LEN: usize = 512;

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

/// What a cached page answers: one WABA, one query.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    waba_id: String,
    status: Option<String>,
    name: Option<String>,
    after: Option<String>,
    limit: usize,
}

#[derive(Debug)]
struct Cached {
    at: Instant,
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
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .get(key)
            .filter(|cached| cached.at.elapsed() < self.ttl)
            .map(|cached| cached.page.clone())
    }

    fn put(&self, key: CacheKey, page: Arc<TemplateList>) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.len() >= TEMPLATE_CACHE_ENTRIES {
            let ttl = self.ttl;
            entries.retain(|_, cached| cached.at.elapsed() < ttl);
        }
        if entries.len() < TEMPLATE_CACHE_ENTRIES {
            entries.insert(
                key,
                Cached {
                    at: Instant::now(),
                    page,
                },
            );
        }
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
pub async fn list(
    state: &AppState,
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
pub async fn list_templates(
    State(state): State<AppState>,
    owned: OwnedWaba,
    ApiQuery(filter): ApiQuery<TemplateFilter>,
    PageQuery(page): PageQuery,
) -> Result<Json<TemplateList>, ApiError> {
    let listed = list(&state, &owned, filter, page.after, page.limit).await?;
    Ok(Json(TemplateList::clone(&listed)))
}

// ─── Get ─────────────────────────────────────────────────────────────────

/// A Meta id from a path or a query: digits, not starting with `0`.
fn graph_id(field: &'static str, id: &str) -> Result<(), ApiError> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_digit())
        && !id.starts_with('0');
    if valid {
        Ok(())
    } else {
        Err(ApiError::invalid(field))
    }
}

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
        (status = 404, description = "`not_found`: no such WABA for this tenant, or Meta's", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `id`, or Meta's `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub async fn get_template(
    State(state): State<AppState>,
    owned: OwnedWaba,
    Path((_, id)): Path<(String, String)>,
) -> Result<Json<TemplateView>, ApiError> {
    graph_id("id", &id)?;
    let template = owned
        .client()
        .templates(owned.waba_id().clone())
        .get_fields(&TemplateId::new(id), &TEMPLATE_FIELDS)
        .await;
    match template {
        Ok(info) => Ok(Json(view(info))),
        Err(error) => Err(owned.failed(&state, &error).await.with_details(&error)),
    }
}

// ─── Create ──────────────────────────────────────────────────────────────

/// Create `definition` in the WABA: `201`, or Meta's refusal with its
/// `details`. The WABA's cached lists go either way (a refusal can follow
/// a creation Meta did make).
pub async fn create(
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
        (status = 422, description = "`invalid_request` (with `field`: the definition breaks a documented limit), `template_rejected`, `invalid_parameter`, `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the template may have been created", body = ErrorBody),
    )
)]
pub async fn create_template(
    State(state): State<AppState>,
    caller: Caller,
    owned: OwnedWaba,
    KeyHeader(key): KeyHeader,
    uri: Uri,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::json(&Method::POST, uri.path(), &body);
    let definition: TemplateDefinition = match serde_json::from_value(body) {
        Ok(definition) => definition,
        Err(_) => return ApiError::invalid("body").into_response(),
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
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `name` or `id`, or Meta's `invalid_parameter` (no such template)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub async fn delete_templates(
    State(state): State<AppState>,
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
            templates
                .delete_by_id(&deletion.name, &TemplateId::new(id.clone()))
                .await
        }
        None => templates.delete_by_name(&deletion.name).await,
    };
    state.template_cache().invalidate(owned.waba_id());
    match deleted {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
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
        cache.put(key("2"), page("b"));
        cache.invalidate(&WabaId::new("1"));
        assert!(cache.get(&key("1")).is_none());
        assert_eq!(cache.get(&key("2")).unwrap().data[0].id, "b");
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(cache.get(&key("2")).is_none(), "expired");
    }

    #[test]
    fn the_cache_is_bounded() {
        let cache = TemplateCache::new(Duration::from_secs(60));
        for i in 0..TEMPLATE_CACHE_ENTRIES + 10 {
            cache.put(key(&i.to_string()), page("x"));
        }
        assert_eq!(cache.entries.lock().unwrap().len(), TEMPLATE_CACHE_ENTRIES);
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
