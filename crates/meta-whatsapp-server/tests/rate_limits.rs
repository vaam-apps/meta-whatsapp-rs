//! A request is charged by its operation, not by its HTTP method (roadmap
//! S6, design D27's companion): the tenant guard resolves the request to
//! the core's `Operation` (its method and route template, through
//! `auth::Operations`) and charges that operation's class
//! (`RateLimiter::charge`). In `/v1` every read is a `GET` (and the `HEAD`
//! axum answers on it), so the HTTP adapter's own routes cannot tell the
//! two rules apart (`ratelimit::tests::v1s_routes_keep_their_class`); an
//! adapter of another shape can, and these tests are one: an RPC-style
//! router whose reads are `POST`s, behind the service's own guard and
//! limiter.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness, send, test_settings, unlimited};
use meta_whatsapp_rs::webhooks::axum::Router;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_rs::webhooks::axum::middleware;
use meta_whatsapp_rs::webhooks::axum::routing::post;
use meta_whatsapp_server::auth::{Operation, Operations, guard, tenant_guard};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::ratelimit::{Rate, RateLimits};
use meta_whatsapp_server::state::Settings;

const TENANT: &str = "merchant-42";

/// A service whose tenants may make `reads` reads, and `sends` writes,
/// before they wait a second.
fn harness(reads: u32, sends: u32) -> Harness {
    Harness::with_settings(Settings {
        rate_limits: RateLimits {
            read: Rate {
                per_second: 1,
                burst: reads,
            },
            send: Rate {
                per_second: 1,
                burst: sends,
            },
            ..unlimited()
        },
        ..test_settings()
    })
}

/// An adapter of RPC shape over the service's state: every operation is a
/// `POST /rpc/<operation id>` behind the service's tenant guard, which
/// charges it by the operation the route stands for.
fn rpc(h: &Harness, operations: &[Operation]) -> Router {
    let charged = Operations::new(operations.iter().map(|operation| {
        (
            "POST".to_owned(),
            format!("/rpc/{}", operation.id()),
            *operation,
        )
    }));
    let mut router = Router::new();
    for operation in operations {
        router = router.route(
            &format!("/rpc/{}", operation.id()),
            post(|| async { "done" }),
        );
    }
    router
        .route_layer(middleware::from_fn_with_state(
            guard(&h.state, Scope::Numbers, &charged),
            tenant_guard,
        ))
        .with_state(h.state.clone())
}

/// The decisive case of roadmap S6 over HTTP: a read served by `POST`
/// (`list_numbers`, as an RPC-style adapter serves it) lands in the
/// tenant's read budget, the one the service's own `GET /v1/numbers`
/// spends, and counts as `class="read"`; a write served the same way
/// (`update_profile`) is the send budget's. Decisive: the class read from
/// the method (a `POST` charged as a send) passes the first read.
#[tokio::test]
async fn a_read_served_by_post_lands_in_the_read_budget() {
    let h = harness(1, 2);
    h.tenant(TENANT).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    let app = rpc(&h, &[Operation::ListNumbers, Operation::UpdateProfile]);
    let call = |path: &str| Call::new(Method::POST, path).key(&key).build();
    let first = send(&app, call("/rpc/list_numbers")).await;
    assert_eq!(
        (first.status, first.text.as_str()),
        (StatusCode::OK, "done")
    );
    let second = send(&app, call("/rpc/list_numbers")).await;
    assert_eq!(
        (second.status, second.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
        "a POST read is the read budget's: {}",
        second.text
    );
    assert_eq!(second.headers["retry-after"], "1");
    // The service's own read route shares it.
    let listed = h.call(Call::get("/v1/numbers").key(&key)).await;
    assert_eq!(
        (listed.status, listed.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
        "GET /v1/numbers spends the same budget"
    );
    // A write served by POST is the send budget's, untouched by the reads.
    for _ in 0..2 {
        let written = send(&app, call("/rpc/update_profile")).await;
        assert_eq!(written.status, StatusCode::OK, "{}", written.text);
    }
    let third = send(&app, call("/rpc/update_profile")).await;
    assert_eq!(third.status, StatusCode::TOO_MANY_REQUESTS);
    let metrics = h.state.metrics().render();
    assert!(
        metrics.contains("wa_server_rate_limited_total{class=\"read\"} 2"),
        "{metrics}"
    );
    assert!(
        metrics.contains("wa_server_rate_limited_total{class=\"send\"} 1"),
        "{metrics}"
    );
}

/// `HEAD`, which axum answers on every `GET` route, is that `GET`'s
/// operation: a read, from the read budget, never the send budget (one
/// request here: were a `HEAD` a send, the second would be refused).
#[tokio::test]
async fn a_head_is_its_gets_operation() {
    let h = harness(2, 1);
    h.tenant(TENANT).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers, Scope::Events]).await;
    let head = h
        .call(Call::new(Method::HEAD, "/v1/numbers").key(&key))
        .await;
    assert_eq!(head.status, StatusCode::OK);
    let polled = h
        .call(Call::new(Method::HEAD, "/v1/events").key(&key))
        .await;
    assert_eq!(polled.status, StatusCode::OK);
    let refused = h.call(Call::get("/v1/numbers").key(&key)).await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
        "the two HEADs spent the read budget"
    );
    let metrics = h.state.metrics().render();
    assert!(
        metrics.contains("wa_server_rate_limited_total{class=\"read\"} 1"),
        "{metrics}"
    );
    assert!(!metrics.contains("class=\"send\""), "{metrics}");
}
