//! Template groups (scope `templates`; roadmap M5b,
//! `templates/template-groups`).
//!
//! | Route | Graph calls, with the tenant's WABA token |
//! | --- | --- |
//! | `POST /v1/wabas/{waba_id}/template-groups` | `GET /{waba_id}/message_templates?fields=id&limit=100` until every template is found (10 pages at most), then `POST /{waba_id}/template_groups` |
//! | `GET /v1/wabas/{waba_id}/template-groups/{group_id}` | `GET /{group_id}` (its `whatsapp_business_account` must be the path's WABA) |
//! | `PATCH /v1/wabas/{waba_id}/template-groups/{group_id}` | the same read; the templates it adds found in the WABA's list; then `POST /{group_id}` |
//! | `DELETE /v1/wabas/{waba_id}/template-groups/{group_id}` | the same read, then `DELETE /{group_id}` |
//!
//! **A group id is the WABA's, or it does not exist** (the core's
//! [`meta_whatsapp_server_core::templates`]): another WABA's group, an id
//! Meta refuses, or another kind of node is `404 not_found`, and nothing
//! of it is answered, changed or deleted. Not offered, because no page of
//! Meta's documents them: the list of a WABA's groups, and the pages after
//! the first of a group's templates. A creation takes an
//! `Idempotency-Key`.

use meta_whatsapp_rs::client::templates::TemplateGroupCreate;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::templates::{self as domain, Templates};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use super::common::ApiJson;
use super::templates::{TemplateView, templates};
use crate::auth::{Caller, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

/// A new template group, in Meta's JSON (`templates/template-groups`). Read
/// as Meta's object: a key the service would not pass on is refused on its
/// path.
#[derive(ToSchema)]
#[allow(dead_code)] // read as Meta's object; the type documents it
#[schema(as = TemplateGroupCreate)]
pub struct TemplateGroupCreateBody {
    /// Its name: unique, at most 256 characters.
    pub name: String,
    /// Its description, at most 512 characters.
    pub description: Option<String>,
    /// Its templates' ids (strings, or Meta's integers), each the WABA's
    /// and in no other group.
    pub whatsapp_business_templates: Vec<String>,
}

/// What to change in a template group, in Meta's JSON: every field
/// optional, at least one set.
#[derive(ToSchema)]
#[allow(dead_code)] // read as Meta's object; the type documents it
#[schema(as = TemplateGroupUpdate)]
pub struct TemplateGroupUpdateBody {
    /// A new name: unique, at most 256 characters.
    pub name: Option<String>,
    /// A new description, at most 512 characters.
    pub description: Option<String>,
    /// Templates to add (each the WABA's).
    pub add_templates: Option<Vec<String>>,
    /// Templates to remove (the metrics they gave the group stay).
    pub remove_templates: Option<Vec<String>>,
}

/// A created group.
#[derive(Debug, Serialize, ToSchema)]
pub struct TemplateGroupCreated {
    /// Its id.
    pub id: String,
}

/// A template group, as Meta describes it.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = TemplateGroup)]
pub struct TemplateGroupView {
    /// Its id.
    pub id: String,
    /// Its name.
    #[schema(required = true)]
    pub name: Option<String>,
    /// Its description.
    #[schema(required = true)]
    pub description: Option<String>,
    /// When it was created, as Meta writes it (`2025-01-06T23:05:12+0000`).
    #[schema(required = true)]
    pub creation_time: Option<String>,
    /// When it was last updated, in the same format.
    #[schema(required = true)]
    pub update_time: Option<String>,
    /// The first page of its templates (Meta's
    /// `whatsapp_business_templates`), with their components.
    pub templates: Vec<TemplateView>,
}

impl From<domain::TemplateGroupDetails> for TemplateGroupView {
    fn from(group: domain::TemplateGroupDetails) -> Self {
        let domain::TemplateGroupDetails {
            id,
            name,
            description,
            creation_time,
            update_time,
            templates,
        } = group;
        Self {
            id: id.into_inner(),
            name,
            description,
            creation_time,
            update_time,
            templates: templates.into_iter().map(TemplateView::from).collect(),
        }
    }
}

/// Create `group`: `201` with its id, or Meta's refusal.
async fn create(
    templates: &Templates<'_>,
    group: &TemplateGroupCreate,
) -> Result<Success, ApiError> {
    let id = templates.create_group(group).await?;
    Success::json(
        StatusCode::CREATED,
        &TemplateGroupCreated {
            id: id.into_inner(),
        },
    )
}

/// `POST /v1/wabas/{waba_id}/template-groups`: create a template group.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/template-groups",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never create twice"),
    ),
    request_body = TemplateGroupCreateBody,
    responses(
        (status = 201, description = "Created", body = TemplateGroupCreated),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or a template not in this WABA (among its first 1,000); nothing created", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `name`, `description`, `whatsapp_business_templates[i]`, `body` or a key the service would not send to Meta; `invalid_parameter` (a name in use, a template in another group), `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the group may have been created", body = ErrorBody),
    )
)]
pub(crate) async fn create_template_group(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::create_template_group(owned.waba_id(), &body);
    let group = match domain::group_creation(&body) {
        Ok(group) => group,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let templates = match templates(&state, &caller, &owned) {
        Ok(templates) => templates,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, create(&templates, &group))
        .await
}

/// `GET /v1/wabas/{waba_id}/template-groups/{group_id}`: one group, with
/// the first page of its templates.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/template-groups/{group_id}",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("group_id" = String, Path, description = "Template group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The group", body = TemplateGroupView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such group in this WABA (another WABA's group looks like a missing one)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `group_id`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_template_group(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, group_id)): Path<(String, String)>,
) -> Result<Json<TemplateGroupView>, ApiError> {
    let group = templates(&state, &caller, &owned)?.group(&group_id).await?;
    Ok(Json(TemplateGroupView::from(group)))
}

/// `PATCH /v1/wabas/{waba_id}/template-groups/{group_id}`: rename a group,
/// describe it, add or remove templates.
#[utoipa::path(
    patch,
    path = "/v1/wabas/{waba_id}/template-groups/{group_id}",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("group_id" = String, Path, description = "Template group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = TemplateGroupUpdateBody,
    responses(
        (status = 204, description = "Changed"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, no such group in this WABA, or a template added not in this WABA; nothing changed", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `group_id`, `update` (nothing to change), `name`, `description`, `add_templates[i]`, `remove_templates[i]`, `body` or a key the service would not send to Meta; `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the group may have been changed", body = ErrorBody),
    )
)]
pub(crate) async fn update_template_group(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, group_id)): Path<(String, String)>,
    ApiJson(body): ApiJson<Value>,
) -> Result<StatusCode, ApiError> {
    let update = domain::group_update(&body)?;
    templates(&state, &caller, &owned)?
        .update_group(&group_id, &update)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/wabas/{waba_id}/template-groups/{group_id}`: delete a group
/// (its templates stay).
#[utoipa::path(
    delete,
    path = "/v1/wabas/{waba_id}/template-groups/{group_id}",
    tag = "templates",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("group_id" = String, Path, description = "Template group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Deleted; its templates stay"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such group in this WABA; nothing deleted", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `group_id`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the group may have been deleted", body = ErrorBody),
    )
)]
pub(crate) async fn delete_template_group(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, group_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    templates(&state, &caller, &owned)?
        .delete_group(&group_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::ids::{TemplateGroupId, TemplateId};
    use serde_json::json;

    use super::*;

    /// Every field of the core's group reaches the service's answer.
    /// Decisive: each field of the conversion.
    #[test]
    fn every_field_of_the_cores_group_is_answered() {
        let group = domain::TemplateGroupDetails {
            id: TemplateGroupId::new("9020555671393375"),
            name: Some("Black Friday 2024".to_owned()),
            description: Some("US-based Black Friday sale 2024 templates.".to_owned()),
            creation_time: Some("2025-01-06T23:05:12+0000".to_owned()),
            update_time: Some("2025-01-06T23:05:13+0000".to_owned()),
            templates: vec![domain::Template {
                id: TemplateId::new("1945418102598215").unwrap(),
                name: Some("black_friday_2024".to_owned()),
                language: Some("en_US".to_owned()),
                status: Some("APPROVED".to_owned()),
                category: Some("MARKETING".to_owned()),
                components: vec![json!({"type": "BODY", "text": "Hi"})],
            }],
        };
        assert_eq!(
            serde_json::to_value(TemplateGroupView::from(group)).unwrap(),
            json!({"id": "9020555671393375", "name": "Black Friday 2024",
                   "description": "US-based Black Friday sale 2024 templates.",
                   "creation_time": "2025-01-06T23:05:12+0000",
                   "update_time": "2025-01-06T23:05:13+0000",
                   "templates": [{"id": "1945418102598215", "name": "black_friday_2024",
                       "language": "en_US", "status": "APPROVED", "category": "MARKETING",
                       "components": [{"type": "BODY", "text": "Hi"}]}]})
        );
    }
}
