//! The listeners' routers (docs/design/server.md, sections 2 and 4), as
//! far as every API adapter shares them: the public listener's whole
//! router ([`public_router`]), the internal listener's layers around an
//! adapter's routes ([`internal_router_with`]), and the routes every
//! adapter serves the same way: [`media`] (upload, download, delete) and
//! [`ops`] (health, metrics, version), each as a documented route group
//! for the adapter's router and document, and Meta's webhook
//! ([`webhooks`]).
//!
//! | Listener | Routes |
//! | --- | --- |
//! | public (`WA_SERVER_PUBLIC_BIND`) | `GET` and `POST /webhooks/meta` (Meta's subscription check and deliveries, bodies up to 3 MiB), `GET /livez`, nothing else |
//! | internal (`WA_SERVER_INTERNAL_BIND`, loopback by default) | the API adapter's routes (`meta-whatsapp-server-api-axum`: `/v1/admin/…`, `/v1/wabas`, `/v1/numbers/…`, `/v1/events`, `/v1/openapi.json`), with this crate's media routes behind their scope and `/livez`, `/readyz`, `/metrics`, `/v1/version` |

pub mod common;
pub mod media;
pub mod ops;
pub mod webhooks;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use meta_whatsapp_rs::webhooks::axum;
use meta_whatsapp_rs::webhooks::axum::Router;
use meta_whatsapp_rs::webhooks::axum::extract::{DefaultBodyLimit, Request, State};
use meta_whatsapp_rs::webhooks::axum::middleware::{self, Next};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_rs::webhooks::axum::routing::get;

use crate::error::{ApiError, ErrorCode};
use crate::events::MAX_WEBHOOK_BODY_BYTES;
use crate::state::{AppState, Grant};
use crate::telemetry::{Listener, Observed, observe};

/// The target of this module's log lines: the module they were logged
/// from before roadmap S7 moved it out of `meta-whatsapp-server`.
const LOG_TARGET: &str = "meta_whatsapp_server::api";

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
/// contract, not the integrators'): the paths of [`PUBLIC_OPERATIONS`].
pub const PUBLIC_ROUTES: [&str; 2] = ["/webhooks/meta", "/livez"];

/// The public listener's operations, `(method, path)`: Meta's subscription
/// check and deliveries, and liveness. Every other method on these paths is
/// `405`, every other path `404` (a test sends each).
pub const PUBLIC_OPERATIONS: [(&str, &str); 3] = [
    ("GET", "/webhooks/meta"),
    ("POST", "/webhooks/meta"),
    ("GET", "/livez"),
];

/// A panic in a handler answers `500 internal` instead of dropping the
/// connection.
async fn catch_panic(request: Request, next: Next) -> Response {
    AssertUnwindSafe(next.run(request))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            tracing::error!(target: LOG_TARGET, "a handler panicked");
            ApiError::internal().into_response()
        })
}

async fn not_found() -> ApiError {
    ApiError::not_found()
}

/// A path that exists, with a method it does not take: `405
/// method_not_allowed`, with the error body (axum adds `Allow`).
async fn method_not_allowed() -> ApiError {
    ApiError::new(ErrorCode::METHOD_NOT_ALLOWED)
}

/// Answer `504 timeout` for a request not answered within `limit`.
async fn deadline(State(limit): State<Duration>, request: Request, next: Next) -> Response {
    let Ok(response) = tokio::time::timeout(limit, next.run(request)).await else {
        tracing::warn!(
            target: LOG_TARGET,
            deadline_s = limit.as_secs(),
            "request cut at its deadline"
        );
        return ApiError::new(ErrorCode::TIMEOUT)
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

/// The internal listener's router around an API adapter's routes, `api`, served
/// with `grant` (the adapter's handlers take a [`Grant`] as their state;
/// this crate's media and operations handlers an [`AppState`], which a
/// grant gives): `404` and `405` with the error body, a body limit of
/// [`MAX_BODY_BYTES`], a panic answered `500`, the [`REQUEST_DEADLINE`],
/// and each request observed (its log line and metrics) under the route
/// template of `templates` it matches. The adapter adds the guards
/// (`crate::auth::admin_guard`, `crate::auth::tenant_guard`) to its
/// route groups before.
pub fn internal_router_with(api: Router<Grant>, grant: &Grant, templates: Vec<String>) -> Router {
    let observed = Observed {
        listener: Listener::Internal,
        routes: Arc::new(templates),
        metrics: grant.state().metrics().clone(),
    };
    let router = api
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn(catch_panic))
        .with_state(grant.clone());
    with_deadline(router, REQUEST_DEADLINE).layer(middleware::from_fn_with_state(observed, observe))
}

/// The public listener's router: Meta's webhook and `/livez`, nothing
/// else.
pub fn public_router(state: &AppState) -> Router {
    // Meta's subscription check and its deliveries.
    let webhook = get(webhooks::verify).post(webhooks::receive);
    let routes = axum::Router::new()
        .route("/webhooks/meta", webhook)
        .route("/livez", get(ops::livez));
    public_router_with(routes, state)
}

/// The public listener's router around `paths`: its layers, the body limit
/// and the deadline included, are the ones [`public_router`] serves (a test
/// adds a slow route).
fn public_router_with(paths: axum::Router<AppState>, state: &AppState) -> Router {
    let observed = Observed {
        listener: Listener::Public,
        routes: Arc::new(PUBLIC_ROUTES.map(str::to_owned).to_vec()),
        metrics: state.metrics().clone(),
    };
    let router = paths
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        // Meta's payloads reach 3 MB (axum's own default is 2 MiB).
        .layer(DefaultBodyLimit::max(MAX_WEBHOOK_BODY_BYTES))
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

    /// The public listener's router cuts a request at its deadline, as
    /// the internal one does (the service's tests/numbers.rs): `POST
    /// /webhooks/meta` reads bodies and runs sinks. Decisive: the deadline
    /// layer of the public router.
    #[tokio::test(start_paused = true)]
    async fn the_public_router_cuts_a_request_at_its_deadline() {
        let state = AppState::for_tests();
        let router = public_router_with(
            axum::Router::new().route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    "late"
                }),
            ),
            &state,
        );
        let started = tokio::time::Instant::now();
        let response = router
            .oneshot(
                meta_whatsapp_rs::webhooks::axum::http::Request::get("/slow")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(started.elapsed(), REQUEST_DEADLINE);
    }

    /// The public listener's own body limit is 3 MiB, whatever a handler
    /// behind it checks (the webhook's library handler checks it too):
    /// 3 MiB is read, one byte more is `413` before a handler sees it.
    /// Decisive: the public router's body limit.
    #[tokio::test]
    async fn the_public_router_reads_3_mib_and_refuses_one_byte_more() {
        use meta_whatsapp_rs::webhooks::axum::body::Bytes;
        use meta_whatsapp_rs::webhooks::axum::routing::post;
        let state = AppState::for_tests();
        let router = public_router_with(
            axum::Router::new().route(
                "/echo",
                post(|body: Bytes| async move { body.len().to_string() }),
            ),
            &state,
        );
        for (len, status) in [
            (MAX_WEBHOOK_BODY_BYTES, StatusCode::OK),
            (MAX_WEBHOOK_BODY_BYTES + 1, StatusCode::PAYLOAD_TOO_LARGE),
        ] {
            let response = router
                .clone()
                .oneshot(
                    meta_whatsapp_rs::webhooks::axum::http::Request::post("/echo")
                        .body(Body::from(vec![b' '; len]))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{len} bytes");
        }
        assert_eq!(MAX_WEBHOOK_BODY_BYTES, 3 * 1024 * 1024);
    }

    /// The public listener serves exactly [`PUBLIC_OPERATIONS`]: each is
    /// answered (not `404` or `405`), any other method on their paths is
    /// `405`. The skills gate reads the list (`SERVER_PUBLIC_ROUTES` in
    /// crates/meta-whatsapp-rs/tests/skills.rs).
    #[tokio::test]
    async fn the_public_listener_serves_exactly_its_operations() {
        use meta_whatsapp_rs::webhooks::axum::http::{Method, Request};
        let mut paths: Vec<&str> = PUBLIC_OPERATIONS.iter().map(|(_, p)| *p).collect();
        paths.dedup();
        assert_eq!(paths, PUBLIC_ROUTES);
        let router = public_router(&AppState::for_tests());
        for path in PUBLIC_ROUTES {
            for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
                let request = Request::builder()
                    .method(Method::from_bytes(method.as_bytes()).unwrap())
                    .uri(path)
                    .body(Body::empty())
                    .unwrap();
                let status = router.clone().oneshot(request).await.unwrap().status();
                if PUBLIC_OPERATIONS.contains(&(method, path)) {
                    assert!(
                        status != StatusCode::NOT_FOUND && status != StatusCode::METHOD_NOT_ALLOWED,
                        "{method} {path}: {status}"
                    );
                } else {
                    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
                }
            }
        }
    }

    /// Every API route is added with `routes!`, which documents it: the
    /// only plain axum routes are the public listener's, which the document
    /// leaves out on purpose. A route added any other way would escape the
    /// tests that iterate the document (M1.3's among them). The API
    /// adapter's own files are its test's
    /// (`meta-whatsapp-server-api-axum`, `api::tests`).
    #[test]
    fn routes_are_only_added_with_their_documentation() {
        let sources = [
            ("mod.rs", include_str!("mod.rs")),
            ("ops.rs", include_str!("ops.rs")),
            ("webhooks.rs", include_str!("webhooks.rs")),
            ("common.rs", include_str!("common.rs")),
            ("media.rs", include_str!("media.rs")),
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
                "mod.rs: .route(\"/webhooks/meta\", webhook)",
                "mod.rs: .route(\"/livez\", get(ops::livez));",
            ]
        );
    }
}
