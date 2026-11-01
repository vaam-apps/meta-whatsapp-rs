//! Health, metrics and the version (no key; the internal listener only,
//! `/livez` on both). The OpenAPI document's own route
//! (`/v1/openapi.json`) is its API adapter's
//! (`meta-whatsapp-server-api-axum`), which merges [`routes()`] into its
//! router and document.

use meta_whatsapp_rs::webhooks::axum;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderValue, header};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::error::{ApiError, ErrorBody, ErrorCode};
use crate::state::{AppState, Grant};

/// The `v1` API's version.
pub const API_VERSION: &str = "v1";

/// The operations routes of the internal listener, each with its
/// documentation (`routes!`): `/livez`, `/readyz`, `/metrics` and
/// `/v1/version`, served with no key.
pub fn routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(livez))
        .routes(routes!(readyz))
        .routes(routes!(metrics))
        .routes(routes!(version))
}

/// A probe's answer.
#[derive(Debug, Serialize, ToSchema)]
pub struct Health {
    /// `ok` (alive) or `ready`.
    pub status: HealthStatus,
}

/// What a probe says.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// The process is alive (`/livez`).
    Ok,
    /// It takes traffic (`/readyz`).
    Ready,
}

/// `GET /livez`: the process is alive.
#[utoipa::path(
    get,
    path = "/livez",
    tag = "operations",
    responses((status = 200, description = "Alive", body = Health))
)]
pub async fn livez() -> Json<Health> {
    Json(Health {
        status: HealthStatus::Ok,
    })
}

/// `GET /readyz`: the service takes traffic: storage answers and it is not
/// shutting down.
#[utoipa::path(
    get,
    path = "/readyz",
    tag = "operations",
    responses(
        (status = 200, description = "Ready", body = Health),
        (status = 503, description = "`shutting_down` or `storage_unavailable`", body = ErrorBody),
    )
)]
pub async fn readyz(State(state): State<AppState>) -> Result<Json<Health>, ApiError> {
    if state.is_shutting_down() {
        return Err(ApiError::new(ErrorCode::SHUTTING_DOWN).retryable(true));
    }
    state.store().ping().await.map_err(ApiError::from)?;
    Ok(Json(Health {
        status: HealthStatus::Ready,
    }))
}

/// `GET /metrics`: Prometheus text exposition.
#[utoipa::path(
    get,
    path = "/metrics",
    tag = "operations",
    responses((status = 200, description = "Prometheus metrics, in the OpenMetrics text format", content_type = "application/openmetrics-text", body = String))
)]
pub async fn metrics(State(state): State<AppState>) -> Response {
    let mut response = state.metrics().render().into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/openmetrics-text; version=1.0.0; charset=utf-8"),
    );
    response
}

/// `GET /v1/version`.
#[derive(Debug, Serialize, ToSchema)]
pub struct Version {
    /// The service's version (the OpenAPI document's `info.version`).
    pub server: &'static str,
    /// The meta-whatsapp-rs git revision the service was built from, when
    /// the build recorded it (`META_WHATSAPP_RS_REVISION` at build time),
    /// else `unknown`.
    pub meta_whatsapp_rs_revision: &'static str,
    /// The Graph API version calls use, e.g. `v25.0`.
    pub graph_api_version: String,
    /// The HTTP API's version: `v1`.
    pub api_version: &'static str,
}

/// `GET /v1/version`.
#[utoipa::path(
    get,
    path = "/v1/version",
    tag = "operations",
    responses((status = 200, description = "Versions", body = Version))
)]
pub async fn version(State(state): State<AppState>) -> Json<Version> {
    Json(Version {
        server: env!("CARGO_PKG_VERSION"),
        meta_whatsapp_rs_revision: option_env!("META_WHATSAPP_RS_REVISION").unwrap_or("unknown"),
        graph_api_version: state.graph_api_version().to_string(),
        api_version: API_VERSION,
    })
}
