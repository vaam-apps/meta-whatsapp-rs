//! Webhook endpoints (scope `webhooks`; docs/design/server.md, sections
//! 4.2 and 4.4; roadmap M2c): a tenant's events pushed to a URL of its
//! own, signed the Standard Webhooks way.
//!
//! | Route | Does |
//! | --- | --- |
//! | `POST /v1/webhook-endpoints` | an endpoint: `{url, types, tenants?}` → `201` with its `secret` (`whsec_…`), shown once; the URL checked against the allow-list (its host looked up, every address checked); `tenants` (a platform key's only) names the tenants whose events it receives |
//! | `GET /v1/webhook-endpoints` | the tenant's endpoints, paged |
//! | `GET`, `PATCH`, `DELETE /v1/webhook-endpoints/{id}` | one; a change of its URL (checked again), types or `enabled`; its deletion, with its deliveries |
//! | `POST /v1/webhook-endpoints/{id}/rotate-secret` | a new secret, shown once; the old one signs too for 24 h |
//! | `POST /v1/webhook-endpoints/{id}/test` | a `ping`, attempted once by the dispatcher: `202` with its delivery |
//! | `GET /v1/webhook-endpoints/{id}/deliveries` | its deliveries, newest first: status, attempts, the last one's status, error and duration, the next one's time; no bodies |
//! | `POST /v1/webhook-endpoints/{id}/deliveries/{delivery_id}/retry` | a delivery pending again, due now, for a new window: `202` |
//!
//! The rules are the core's ([`meta_whatsapp_server_core::webhooks_out`]):
//! an endpoint is the caller's tenant's, another tenant's is `404` like a
//! missing one, before anything of it is read; a deployment without a
//! data key answers `409 webhooks_not_configured`. No answer shows a
//! secret but the creation's and the rotation's. An endpoint made with
//! `tenants` (roadmap M2m) is still its `WA-Tenant`'s, listed and changed
//! there, through a platform key that may name all of its tenants (any
//! other key finds it missing).

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_server_core::webhooks_out::endpoints::{CreateEndpoint, UpdateEndpoint};
use meta_whatsapp_server_core::webhooks_out::{Delivery, Endpoint};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::common::{ApiJson, PageParams, PageQuery, json, next_cursor, rfc3339};
use super::events::KnownEventType;
use crate::auth::Caller;
use crate::error::{ApiError, ErrorBody};
use crate::state::Grant;

// ─── Requests ────────────────────────────────────────────────────────────

/// `POST /v1/webhook-endpoints`.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CreateWebhookEndpoint)]
pub struct CreateWebhookEndpointBody {
    /// Where to deliver: `https` (plain `http` only in a development
    /// deployment), a host the operator's allow-list admits
    /// (`WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS`), at most 2048 bytes.
    pub url: String,
    /// The event types to deliver: `KnownEventType`s, at least one, each
    /// once.
    #[schema(value_type = Vec<KnownEventType>)]
    pub types: Vec<String>,
    /// The tenants whose events it receives, instead of the `WA-Tenant`'s
    /// own: a **platform key's** only (`422` on `tenants` from a tenant
    /// key), one to ten tenant ids, each once, each one the key may name
    /// and that exists (`403 forbidden` otherwise, alike for both). Every
    /// delivery carries one tenant's event, read in that tenant's own
    /// stream; a tenant deleted later leaves the endpoint (its events
    /// stop), and the endpoint does not receive the `WA-Tenant`'s unless
    /// it is named. Absent: the `WA-Tenant`'s own events.
    #[serde(default)]
    pub tenants: Option<Vec<String>>,
}

/// `PATCH /v1/webhook-endpoints/{id}`: what is absent is left.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = UpdateWebhookEndpoint)]
pub struct UpdateWebhookEndpointBody {
    /// A new URL, checked as on a creation.
    pub url: Option<String>,
    /// New event types.
    #[schema(value_type = Option<Vec<KnownEventType>>)]
    pub types: Option<Vec<String>>,
    /// Off stops deliveries (they wait); on resumes them, from where the
    /// endpoint stopped, within the outbox's retention, and clears the
    /// service's `disabled_reason`.
    pub enabled: Option<bool>,
    /// New tenants, replacing those of an endpoint made with `tenants`
    /// (same rules as on a creation; `422` on `tenants` for an endpoint
    /// made without): one kept keeps its place in its stream, one added
    /// receives from now on, one left out stops at once, its pending
    /// deliveries deleted.
    #[serde(default)]
    pub tenants: Option<Vec<String>>,
}

// ─── Answers ─────────────────────────────────────────────────────────────

/// A webhook endpoint (never its secret).
#[derive(Serialize, ToSchema)]
#[schema(as = WebhookEndpoint)]
pub struct WebhookEndpointView {
    /// Its id (`whe_` and 32 hex digits).
    pub id: String,
    /// Where it delivers.
    pub url: String,
    /// The event types it receives.
    #[schema(value_type = Vec<KnownEventType>)]
    pub types: Vec<String>,
    /// Whether deliveries are made.
    pub enabled: bool,
    /// The tenants whose events it receives, when it was made with
    /// `tenants`; absent otherwise (it receives the `WA-Tenant`'s own).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenants: Option<Vec<String>>,
    /// Why the service turned it off: `failing` (its deliveries failed for
    /// the whole retry window); `null` otherwise.
    #[schema(required = true)]
    pub disabled_reason: Option<String>,
    /// Until when the secret before the last rotation signs too (RFC
    /// 3339); `null` without one.
    #[schema(required = true, format = DateTime)]
    pub previous_secret_expires_at: Option<String>,
    /// When it was created (RFC 3339).
    #[schema(format = DateTime)]
    pub created_at: String,
    /// When it last changed (RFC 3339).
    #[schema(format = DateTime)]
    pub updated_at: String,
}

impl From<Endpoint> for WebhookEndpointView {
    fn from(endpoint: Endpoint) -> Self {
        Self {
            id: endpoint.id.as_str().to_owned(),
            url: endpoint.url,
            types: endpoint.types,
            enabled: endpoint.enabled,
            tenants: endpoint
                .tenants
                .map(|named| named.into_iter().map(|t| t.tenant.to_string()).collect()),
            disabled_reason: endpoint.disabled_reason.map(|r| r.as_str().to_owned()),
            previous_secret_expires_at: endpoint
                .previous_secret
                .and(endpoint.previous_secret_until)
                .map(rfc3339),
            created_at: rfc3339(endpoint.created_at),
            updated_at: rfc3339(endpoint.updated_at),
        }
    }
}

/// A created endpoint, and its secret, shown this once.
#[derive(Serialize, ToSchema)]
#[schema(as = WebhookEndpointCreated)]
pub struct WebhookEndpointCreatedView {
    /// The endpoint.
    #[serde(flatten)]
    pub endpoint: WebhookEndpointView,
    /// The signing secret, `whsec_` and the base64 of 32 bytes: verify
    /// each delivery's `webhook-signature` with it (Standard Webhooks).
    /// Shown once: store it now.
    pub secret: String,
}

/// A new secret, shown this once.
#[derive(Serialize, ToSchema)]
#[schema(as = WebhookSecretRotated)]
pub struct WebhookSecretRotatedView {
    /// The new signing secret (`whsec_…`): deliveries carry its signature
    /// first from now on.
    pub secret: String,
    /// Until when the previous secret signs too (RFC 3339): accept either
    /// until then.
    #[schema(format = DateTime)]
    pub previous_secret_expires_at: String,
}

/// A page of endpoints.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = WebhookEndpointList)]
pub struct WebhookEndpointList {
    /// The endpoints, in byte order of their ids.
    pub data: Vec<WebhookEndpointView>,
    /// Pass as `cursor` for the next page; `null` on the last.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// A delivery: one event (or a ping) to one endpoint, and its attempts.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = WebhookDelivery)]
pub struct WebhookDeliveryView {
    /// Its id (`whd_` and 32 hex digits).
    pub id: String,
    /// Its event's id (`evt_…`, the deliveries' `webhook-id`); `null` for
    /// a ping.
    #[schema(required = true)]
    pub event_id: Option<String>,
    /// Its event's sequence; `null` for a ping.
    #[schema(required = true)]
    pub sequence: Option<i64>,
    /// Its event's type, or `ping`.
    #[serde(rename = "type")]
    pub event_type: String,
    /// `pending`, `delivered` or `failed`.
    pub status: String,
    /// Attempts made.
    pub attempts: u32,
    /// When the next attempt is due (RFC 3339), while pending.
    #[schema(required = true, format = DateTime)]
    pub next_attempt_at: Option<String>,
    /// When the last attempt started (RFC 3339).
    #[schema(required = true, format = DateTime)]
    pub last_attempt_at: Option<String>,
    /// The last attempt's HTTP status, when the receiver answered.
    #[schema(required = true)]
    pub last_status: Option<u16>,
    /// Why the last attempt failed: `timeout`, `connect`, `tls`, `status`
    /// (a non-2xx answer, a redirect included: never followed),
    /// `protocol`, `destination_refused` (the URL or an address its host
    /// resolved to is outside the allow-list), `resolve_failed`,
    /// `secret_unavailable`, `event_gone` (past the outbox's retention);
    /// new values may appear.
    #[schema(required = true)]
    pub last_error: Option<String>,
    /// How long the last attempt took, in milliseconds.
    #[schema(required = true)]
    pub last_duration_ms: Option<u32>,
    /// When retries end (RFC 3339): a failed attempt after it fails the
    /// delivery.
    #[schema(format = DateTime)]
    pub give_up_at: String,
    /// When it was made (RFC 3339).
    #[schema(format = DateTime)]
    pub created_at: String,
}

impl From<Delivery> for WebhookDeliveryView {
    fn from(delivery: Delivery) -> Self {
        Self {
            id: delivery.id.as_str().to_owned(),
            event_id: delivery.event_id,
            sequence: delivery.sequence,
            event_type: delivery.event_type,
            status: delivery.status.as_str().to_owned(),
            attempts: delivery.attempts,
            next_attempt_at: delivery.next_attempt_at.map(rfc3339),
            last_attempt_at: delivery.last_attempt_at.map(rfc3339),
            last_status: delivery.last_status,
            last_error: delivery.last_error.map(|e| e.as_str().to_owned()),
            last_duration_ms: delivery.last_duration_ms,
            give_up_at: rfc3339(delivery.give_up_at),
            created_at: rfc3339(delivery.created_at),
        }
    }
}

/// A page of deliveries.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = WebhookDeliveryList)]
pub struct WebhookDeliveryList {
    /// The deliveries, newest first.
    pub data: Vec<WebhookDeliveryView>,
    /// Pass as `cursor` for the next (older) page; `null` on the last.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// The URL's path or query may carry a token of the receiver's: `Debug`
/// omits it.
impl std::fmt::Debug for CreateWebhookEndpointBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateWebhookEndpointBody")
            .field("types", &self.types)
            .field("tenants", &self.tenants)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for UpdateWebhookEndpointBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateWebhookEndpointBody")
            .field("url", &self.url.as_ref().map(|_| ".."))
            .field("types", &self.types)
            .field("enabled", &self.enabled)
            .field("tenants", &self.tenants)
            .finish()
    }
}

impl std::fmt::Debug for WebhookEndpointView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookEndpointView")
            .field("id", &self.id)
            .field("types", &self.types)
            .field("enabled", &self.enabled)
            .field("tenants", &self.tenants)
            .finish_non_exhaustive()
    }
}

// ─── The routes ──────────────────────────────────────────────────────────

/// `POST /v1/webhook-endpoints`.
#[utoipa::path(
    post,
    path = "/v1/webhook-endpoints",
    tag = "webhooks",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as: the endpoint is that tenant's (and receives its events, unless `tenants` names others)"),
    ),
    request_body = CreateWebhookEndpointBody,
    responses(
        (status = 201, description = "Created, its `secret` shown this once. It receives the tenant's events recorded from now on", body = WebhookEndpointCreatedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden` (no `webhooks` scope; or a tenant in `tenants` the platform key may not name, or that does not exist), `tenant_suspended`", body = ErrorBody),
        (status = 409, description = "`webhooks_not_configured` (no data key), `webhook_endpoint_limit_reached` (10 per tenant)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `url` (not `https`, credentials in it, not in the allow-list, or its host resolving to an address the list does not admit), `types` (none, or one a tenant never receives), `tenants` (from a tenant key; empty, over 10, or not a tenant id) or `body`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`)", body = ErrorBody),
    )
)]
pub(crate) async fn create_webhook_endpoint(
    State(state): State<Grant>,
    caller: Caller,
    ApiJson(body): ApiJson<CreateWebhookEndpointBody>,
) -> Result<(StatusCode, Json<WebhookEndpointCreatedView>), ApiError> {
    let (endpoint, secret) = state
        .webhooks()
        .open(state.authz(), &caller)?
        .create(CreateEndpoint {
            url: body.url,
            types: body.types,
            tenants: body.tenants,
        })
        .await?;
    Ok(json(
        StatusCode::CREATED,
        WebhookEndpointCreatedView {
            endpoint: endpoint.into(),
            secret: secret.expose(),
        },
    ))
}

/// `GET /v1/webhook-endpoints`.
#[utoipa::path(
    get,
    path = "/v1/webhook-endpoints",
    tag = "webhooks",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        PageParams,
    ),
    responses(
        (status = 200, description = "The tenant's endpoints", body = WebhookEndpointList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 409, description = "`webhooks_not_configured`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit` or `cursor`", body = ErrorBody),
    )
)]
pub(crate) async fn list_webhook_endpoints(
    State(state): State<Grant>,
    caller: Caller,
    PageQuery(page): PageQuery,
) -> Result<Json<WebhookEndpointList>, ApiError> {
    let listing = state
        .webhooks()
        .open(state.authz(), &caller)?
        .list(&page)
        .await?;
    let next_cursor = next_cursor(&listing);
    Ok(Json(WebhookEndpointList {
        data: listing.items.into_iter().map(Into::into).collect(),
        next_cursor,
    }))
}

/// `GET /v1/webhook-endpoints/{id}`.
#[utoipa::path(
    get,
    path = "/v1/webhook-endpoints/{id}",
    tag = "webhooks",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The endpoint's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The endpoint", body = WebhookEndpointView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no endpoint of this tenant has this id (another tenant's looks alike)", body = ErrorBody),
        (status = 409, description = "`webhooks_not_configured`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`)", body = ErrorBody),
    )
)]
pub(crate) async fn get_webhook_endpoint(
    State(state): State<Grant>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<WebhookEndpointView>, ApiError> {
    let endpoint = state
        .webhooks()
        .open(state.authz(), &caller)?
        .get(&id)
        .await?;
    Ok(Json(endpoint.into()))
}

/// `PATCH /v1/webhook-endpoints/{id}`.
#[utoipa::path(
    patch,
    path = "/v1/webhook-endpoints/{id}",
    tag = "webhooks",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The endpoint's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = UpdateWebhookEndpointBody,
    responses(
        (status = 200, description = "The endpoint, changed", body = WebhookEndpointView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`, as for a read, before the change is checked", body = ErrorBody),
        (status = 409, description = "`webhooks_not_configured`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `url`, `types`, `tenants` (also for an endpoint made without) or `body`, as on a creation", body = ErrorBody),
    )
)]
pub(crate) async fn update_webhook_endpoint(
    State(state): State<Grant>,
    caller: Caller,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateWebhookEndpointBody>,
) -> Result<Json<WebhookEndpointView>, ApiError> {
    let endpoint = state
        .webhooks()
        .open(state.authz(), &caller)?
        .update(
            &id,
            UpdateEndpoint {
                url: body.url,
                types: body.types,
                enabled: body.enabled,
                tenants: body.tenants,
            },
        )
        .await?;
    Ok(Json(endpoint.into()))
}

/// `DELETE /v1/webhook-endpoints/{id}`.
#[utoipa::path(
    delete,
    path = "/v1/webhook-endpoints/{id}",
    tag = "webhooks",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The endpoint's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Deleted, with its deliveries"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`, as for a read", body = ErrorBody),
        (status = 409, description = "`webhooks_not_configured`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`)", body = ErrorBody),
    )
)]
pub(crate) async fn delete_webhook_endpoint(
    State(state): State<Grant>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .webhooks()
        .open(state.authz(), &caller)?
        .delete(&id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/webhook-endpoints/{id}/rotate-secret`.
#[utoipa::path(
    post,
    path = "/v1/webhook-endpoints/{id}/rotate-secret",
    tag = "webhooks",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The endpoint's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The new secret, shown this once. For 24 hours every delivery carries two signatures, the new secret's first; a rotation within them drops the oldest secret", body = WebhookSecretRotatedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`, as for a read", body = ErrorBody),
        (status = 409, description = "`webhooks_not_configured`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`)", body = ErrorBody),
    )
)]
pub(crate) async fn rotate_webhook_secret(
    State(state): State<Grant>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<WebhookSecretRotatedView>, ApiError> {
    let (endpoint, secret) = state
        .webhooks()
        .open(state.authz(), &caller)?
        .rotate_secret(&id)
        .await?;
    Ok(Json(WebhookSecretRotatedView {
        secret: secret.expose(),
        previous_secret_expires_at: endpoint
            .previous_secret_until
            .map(rfc3339)
            .unwrap_or_default(),
    }))
}

/// `POST /v1/webhook-endpoints/{id}/test`.
#[utoipa::path(
    post,
    path = "/v1/webhook-endpoints/{id}/test",
    tag = "webhooks",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The endpoint's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 202, description = "A `ping` delivery, due now: the dispatcher attempts it once (its outcome in the endpoint's deliveries). Its body: `{id, type: \"ping\", api_version, tenant_id, endpoint_id, sent_at}`, signed as every delivery", body = WebhookDeliveryView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`, as for a read", body = ErrorBody),
        (status = 409, description = "`webhooks_not_configured`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`)", body = ErrorBody),
    )
)]
pub(crate) async fn test_webhook_endpoint(
    State(state): State<Grant>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<WebhookDeliveryView>), ApiError> {
    let delivery = state
        .webhooks()
        .open(state.authz(), &caller)?
        .test(&id)
        .await?;
    Ok(json(StatusCode::ACCEPTED, delivery.into()))
}

/// `GET /v1/webhook-endpoints/{id}/deliveries`.
#[utoipa::path(
    get,
    path = "/v1/webhook-endpoints/{id}/deliveries",
    tag = "webhooks",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The endpoint's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        PageParams,
    ),
    responses(
        (status = 200, description = "Its deliveries, newest first; no bodies. Settled ones are kept 30 days", body = WebhookDeliveryList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`, as for a read", body = ErrorBody),
        (status = 409, description = "`webhooks_not_configured`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit` or `cursor`", body = ErrorBody),
    )
)]
pub(crate) async fn list_webhook_deliveries(
    State(state): State<Grant>,
    caller: Caller,
    Path(id): Path<String>,
    PageQuery(page): PageQuery,
) -> Result<Json<WebhookDeliveryList>, ApiError> {
    let listing = state
        .webhooks()
        .open(state.authz(), &caller)?
        .deliveries(&id, &page)
        .await?;
    let next_cursor = next_cursor(&listing);
    Ok(Json(WebhookDeliveryList {
        data: listing.items.into_iter().map(Into::into).collect(),
        next_cursor,
    }))
}

/// `POST /v1/webhook-endpoints/{id}/deliveries/{delivery_id}/retry`.
#[utoipa::path(
    post,
    path = "/v1/webhook-endpoints/{id}/deliveries/{delivery_id}/retry",
    tag = "webhooks",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The endpoint's id"),
        ("delivery_id" = String, Path, description = "The delivery's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 202, description = "Pending again, due now, retried for a new window (a failed or delivered one; a pending one is left as it is). Its event goes again under the same `webhook-id`", body = WebhookDeliveryView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such delivery of this tenant's endpoint", body = ErrorBody),
        (status = 409, description = "`webhooks_not_configured`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`)", body = ErrorBody),
    )
)]
pub(crate) async fn retry_webhook_delivery(
    State(state): State<Grant>,
    caller: Caller,
    Path((id, delivery_id)): Path<(String, String)>,
) -> Result<(StatusCode, Json<WebhookDeliveryView>), ApiError> {
    let delivery = state
        .webhooks()
        .open(state.authz(), &caller)?
        .retry(&id, &delivery_id)
        .await?;
    Ok(json(StatusCode::ACCEPTED, delivery.into()))
}
