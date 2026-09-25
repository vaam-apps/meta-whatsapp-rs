//! The two listeners' routers and the OpenAPI document
//! (docs/design/server.md, sections 2 and 4).
//!
//! | Listener | Routes |
//! | --- | --- |
//! | public (`WA_SERVER_PUBLIC_BIND`) | `GET /webhooks/meta`, `GET /livez`, nothing else |
//! | internal (`WA_SERVER_INTERNAL_BIND`, loopback by default) | `/v1/admin/…` (admin key), `/v1/wabas`, `/v1/numbers/…` (tenant or platform key, scope `numbers`), `/livez`, `/readyz`, `/metrics`, `/v1/openapi.json`, `/v1/version` |
//!
//! Every API route is registered through utoipa-axum's `routes!`, which
//! adds the handler and its `#[utoipa::path]` documentation at once: a
//! route cannot be served without being in the OpenAPI document, which is
//! what the tests iterate (the committed copy is
//! `crates/meta-whatsapp-server/openapi/v1.json`).

pub mod admin;
pub mod common;
pub mod numbers;
pub mod ops;
pub mod webhooks;

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use futures::FutureExt;
use meta_whatsapp_rs::webhooks::axum;
use meta_whatsapp_rs::webhooks::axum::Router;
use meta_whatsapp_rs::webhooks::axum::extract::{DefaultBodyLimit, Request, State};
use meta_whatsapp_rs::webhooks::axum::middleware::{self, Next};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_rs::webhooks::axum::routing::get;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::auth::{admin_guard, guard, tenant_guard};
use crate::error::{ApiError, ErrorBody, ErrorCode};
use crate::model::Scope;
use crate::state::AppState;
use crate::telemetry::{Listener, Observed, observe};

/// Largest request body on the internal listener (docs/design/server.md,
/// section 4.1).
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// How long a request may take to be answered, on either listener (the
/// response's head: a streamed body goes on). Past it the request is cut
/// and answered `504 timeout`, with `may_have_been_sent: true`: a Graph
/// call in flight may have taken effect. Longer than one Graph attempt
/// (30 s), shorter than an ingress's usual minute; a call whose retries
/// would run past it is cut instead.
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(55);

/// The public listener's routes (not in the OpenAPI document: Meta's
/// contract, not the integrators').
pub const PUBLIC_ROUTES: [&str; 2] = ["/webhooks/meta", "/livez"];

struct Security;

impl Modify for Security {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "api_key",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("wak_<key id>_<secret>")
                    .description(Some(
                        "An API key: a tenant key, a platform key (with the WA-Tenant header) or \
                         an admin key (/v1/admin only).",
                    ))
                    .build(),
            ),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "meta-whatsapp-server",
        description = "The meta-whatsapp-rs HTTP service: WhatsApp Business Platform over REST, for \
                       apps not written in Rust. Errors: every error answers `ErrorBody`; branch on \
                       `error.code` (`ErrorCode`), and resend only when `may_have_been_sent` is false."
    ),
    modifiers(&Security),
    components(schemas(ErrorBody, ErrorCode)),
    tags(
        (name = "admin", description = "Tenants, keys and WABA bindings (admin key)"),
        (name = "numbers", description = "WABAs, numbers and business profiles (scope `numbers`)"),
        (name = "operations", description = "Health, metrics, this document, versions (no key)"),
    )
)]
struct ApiDoc;

fn admin_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(admin::create_tenant, admin::list_tenants))
        .routes(routes!(
            admin::get_tenant,
            admin::update_tenant,
            admin::delete_tenant
        ))
        .routes(routes!(admin::mint_tenant_key, admin::list_tenant_keys))
        .routes(routes!(admin::revoke_tenant_key))
        .routes(routes!(admin::mint_platform_key, admin::list_platform_keys))
        .routes(routes!(admin::revoke_platform_key))
        .routes(routes!(admin::attach_waba))
        .routes(routes!(admin::unbind_waba))
}

fn numbers_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(numbers::list_wabas))
        .routes(routes!(numbers::list_numbers))
        .routes(routes!(numbers::get_number))
        .routes(routes!(numbers::get_profile, numbers::patch_profile))
        .routes(routes!(numbers::disconnect_waba))
}

fn ops_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(ops::livez))
        .routes(routes!(ops::readyz))
        .routes(routes!(ops::metrics))
        .routes(routes!(ops::openapi_json))
        .routes(routes!(ops::version))
}

/// The OpenAPI document of the internal listener's routes.
pub fn openapi() -> utoipa::openapi::OpenApi {
    let (_, document) = OpenApiRouter::<AppState>::with_openapi(ApiDoc::openapi())
        .merge(admin_routes())
        .merge(numbers_routes())
        .merge(ops_routes())
        .split_for_parts();
    document
}

/// The OpenAPI document as committed: pretty JSON and a final newline.
pub fn openapi_document() -> String {
    static DOCUMENT: LazyLock<String> = LazyLock::new(|| {
        let mut json = openapi().to_pretty_json().unwrap_or_default();
        json.push('\n');
        json
    });
    DOCUMENT.clone()
}

/// The internal listener's route templates, for logs and metrics.
pub fn internal_routes() -> Vec<String> {
    openapi().paths.paths.keys().cloned().collect()
}

/// A panic in a handler answers `500 internal` instead of dropping the
/// connection.
async fn catch_panic(request: Request, next: Next) -> Response {
    AssertUnwindSafe(next.run(request))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            tracing::error!("a handler panicked");
            ApiError::internal().into_response()
        })
}

async fn not_found() -> ApiError {
    ApiError::not_found()
}

/// A path that exists, with a method it does not take: `405
/// method_not_allowed`, with the error body (axum adds `Allow`).
async fn method_not_allowed() -> ApiError {
    ApiError::new("method_not_allowed")
}

/// Answer `504 timeout` for a request not answered within `limit`.
async fn deadline(State(limit): State<Duration>, request: Request, next: Next) -> Response {
    let Ok(response) = tokio::time::timeout(limit, next.run(request)).await else {
        tracing::warn!(deadline_s = limit.as_secs(), "request cut at its deadline");
        return ApiError::new("timeout")
            .retryable(true)
            .with_may_have_been_sent(true)
            .into_response();
    };
    response
}

/// `router` with a deadline of `limit` on every request (see
/// [`REQUEST_DEADLINE`]).
pub fn with_deadline(router: Router, limit: Duration) -> Router {
    router.layer(middleware::from_fn_with_state(limit, deadline))
}

/// The internal listener's router.
pub fn internal_router(state: &AppState) -> Router {
    let admin =
        admin_routes().route_layer(middleware::from_fn_with_state(state.clone(), admin_guard));
    let numbers = numbers_routes().route_layer(middleware::from_fn_with_state(
        guard(state, Scope::Numbers),
        tenant_guard,
    ));
    let (router, _) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(admin)
        .merge(numbers)
        .merge(ops_routes())
        .split_for_parts();
    let observed = Observed {
        listener: Listener::Internal,
        routes: Arc::new(internal_routes()),
        metrics: state.metrics().clone(),
    };
    let router = router
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn(catch_panic))
        .with_state(state.clone());
    with_deadline(router, REQUEST_DEADLINE).layer(middleware::from_fn_with_state(observed, observe))
}

/// The public listener's router: Meta's webhook and `/livez`, nothing
/// else.
pub fn public_router(state: &AppState) -> Router {
    let observed = Observed {
        listener: Listener::Public,
        routes: Arc::new(PUBLIC_ROUTES.map(str::to_owned).to_vec()),
        metrics: state.metrics().clone(),
    };
    let router = axum::Router::new()
        .route("/webhooks/meta", get(webhooks::verify))
        .route("/livez", get(ops::livez))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn(catch_panic))
        .with_state(state.clone());
    with_deadline(router, REQUEST_DEADLINE).layer(middleware::from_fn_with_state(observed, observe))
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::webhooks::axum::body::Body;
    use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
    use tower::ServiceExt as _;

    use super::*;

    /// A request not answered within its deadline is cut: `504 timeout`,
    /// and it may have taken effect. Decisive: the deadline layer.
    #[tokio::test(start_paused = true)]
    async fn a_request_past_its_deadline_is_504_and_may_have_taken_effect() {
        let slow = Router::new()
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    "late"
                }),
            )
            .route("/fast", get(|| async { "ok" }));
        let router = with_deadline(slow, REQUEST_DEADLINE);
        let started = tokio::time::Instant::now();
        let response = router
            .clone()
            .oneshot(
                meta_whatsapp_rs::webhooks::axum::http::Request::get("/slow")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(started.elapsed(), REQUEST_DEADLINE);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "timeout");
        assert_eq!(body["error"]["may_have_been_sent"], true);
        let fast = router
            .oneshot(
                meta_whatsapp_rs::webhooks::axum::http::Request::get("/fast")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fast.status(), StatusCode::OK);
    }

    /// Every API route is added with `routes!`, which documents it: the
    /// only plain axum routes are the public listener's, which the document
    /// leaves out on purpose. A route added any other way would escape the
    /// tests that iterate the document (M1.3's among them).
    #[test]
    fn routes_are_only_added_with_their_documentation() {
        let sources = [
            ("mod.rs", include_str!("mod.rs")),
            ("admin.rs", include_str!("admin.rs")),
            ("numbers.rs", include_str!("numbers.rs")),
            ("ops.rs", include_str!("ops.rs")),
            ("webhooks.rs", include_str!("webhooks.rs")),
            ("common.rs", include_str!("common.rs")),
        ];
        let mut plain = Vec::new();
        for (file, source) in sources {
            // The code, not this test.
            let code = source.split("#[cfg(test)]").next().unwrap_or(source);
            for needle in [".route(", ".route_service(", ".nest(", ".nest_service("] {
                for (i, _) in code.match_indices(needle) {
                    let line = code[i..].lines().next().unwrap_or_default();
                    plain.push(format!("{file}: {line}"));
                }
            }
        }
        assert_eq!(
            plain,
            [
                "mod.rs: .route(\"/webhooks/meta\", get(webhooks::verify))",
                "mod.rs: .route(\"/livez\", get(ops::livez))",
            ]
        );
    }
}
