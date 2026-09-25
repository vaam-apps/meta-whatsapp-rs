//! Health, metrics, the OpenAPI document and the version (no key; the
//! internal listener only, `/livez` on both).

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderValue, StatusCode, header};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

/// The `v1` API's version.
pub const API_VERSION: &str = "v1";

/// A probe's answer.
#[derive(Debug, Serialize, ToSchema)]
pub struct Health {
    /// `ok` (alive) or `ready`.
    pub status: &'static str,
}

/// `GET /livez`: the process is alive.
#[utoipa::path(
    get,
    path = "/livez",
    tag = "operations",
    responses((status = 200, description = "Alive", body = Health))
)]
pub async fn livez() -> Json<Health> {
    Json(Health { status: "ok" })
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
        return Err(ApiError::new("shutting_down").retryable(true));
    }
    state.store().ping().await.map_err(ApiError::from)?;
    Ok(Json(Health { status: "ready" }))
}

/// `GET /metrics`: Prometheus text exposition.
#[utoipa::path(
    get,
    path = "/metrics",
    tag = "operations",
    responses((status = 200, description = "Prometheus metrics", content_type = "text/plain", body = String))
)]
pub async fn metrics(State(state): State<AppState>) -> Response {
    let mut response = state.metrics().render().into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/openmetrics-text; version=1.0.0; charset=utf-8"),
    );
    response
}

/// `GET /v1/openapi.json`: this document.
#[utoipa::path(
    get,
    path = "/v1/openapi.json",
    tag = "operations",
    responses((status = 200, description = "The OpenAPI 3.1 document", content_type = "application/json", body = Object))
)]
pub async fn openapi_json() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        super::openapi_document(),
    )
        .into_response()
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
