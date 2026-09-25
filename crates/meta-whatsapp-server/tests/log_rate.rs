//! Security review L5: refused deliveries cost at most one log line a
//! minute per reason, whoever sends them; the metric counts every one.
//! Alone in its binary, with only tests that capture: the capture is a
//! thread's default subscriber (see `live_logs.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::capture::{Captured, subscriber};
use common::meta::example_text;
use common::{Call, Harness, send};
use meta_whatsapp_rs::core::secret::AppSecret;
use meta_whatsapp_rs::webhooks::axum::body::{Body, Bytes};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_rs::webhooks::sign;

/// The warnings of `logs` holding `needle`.
fn warnings(logs: &str, needle: &str) -> usize {
    logs.lines()
        .filter(|l| l.contains("\"level\":\"WARN\"") && l.contains(needle))
        .count()
}

#[tokio::test]
async fn refused_deliveries_are_logged_at_most_once_a_minute() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    let body = example_text();
    let forged = sign(&AppSecret::new("a-forger-s-secret"), &body);
    let too_large = vec![b' '; 3 * 1024 * 1024 + 1];
    for _ in 0..30 {
        let unsigned = Call::new(Method::POST, "/webhooks/meta").body(Body::from(body.clone()));
        assert_eq!(
            send(&h.public, unsigned.build()).await.status,
            StatusCode::UNAUTHORIZED
        );
        let wrong = Call::new(Method::POST, "/webhooks/meta")
            .header("x-hub-signature-256", &forged)
            .body(Body::from(body.clone()));
        assert_eq!(
            send(&h.public, wrong.build()).await.status,
            StatusCode::UNAUTHORIZED
        );
        let large = Call::new(Method::POST, "/webhooks/meta")
            .header("x-hub-signature-256", &forged)
            .body(Body::from(too_large.clone()));
        assert_eq!(
            send(&h.public, large.build()).await.status,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
    let logs = captured.text();
    let warnings: Vec<&str> = logs
        .lines()
        .filter(|l| l.contains("\"level\":\"WARN\""))
        .collect();
    for (what, needle) in [
        ("unsigned", "without a well-formed signature header"),
        ("forged", "no app secret produced its signature"),
        ("too large", "over the body limit"),
    ] {
        let lines = warnings.iter().filter(|l| l.contains(needle)).count();
        assert_eq!(lines, 1, "{what}: {warnings:#?}");
    }
    // The library's own line for a forged signature is never reached.
    assert!(
        !warnings
            .iter()
            .any(|l| l.contains("rejected webhook delivery")),
        "{warnings:#?}"
    );
    assert_eq!(warnings.len(), 3, "{warnings:#?}");
    let metrics = h.state.metrics().render();
    assert!(
        metrics.contains("wa_server_webhook_deliveries_total{outcome=\"unauthenticated\"} 60"),
        "{metrics}"
    );
}

/// The same for a replica at capacity (`503` before the body) and bodies
/// slower than 15 s (`408`): 64 deliveries whose bodies never come hold
/// every place, 20 more are refused, then the 64 are cut. One line each.
/// Decisive: the rejection log on those two paths.
#[tokio::test(start_paused = true)]
async fn busy_and_slow_deliveries_are_logged_at_most_once_a_minute() {
    use meta_whatsapp_server::events::MAX_DELIVERIES_IN_FLIGHT;
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    let signature = sign(&AppSecret::new(common::APP_SECRET), &example_text());
    let pending = || {
        Call::new(Method::POST, "/webhooks/meta")
            .header("x-hub-signature-256", &signature)
            .body(Body::from_stream(futures::stream::pending::<
                Result<Bytes, std::io::Error>,
            >()))
            .build()
    };
    let slow: Vec<_> = (0..MAX_DELIVERIES_IN_FLIGHT)
        .map(|_| {
            let router = h.public.clone();
            let request = pending();
            tokio::spawn(async move { send(&router, request).await.status })
        })
        .collect();
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    for _ in 0..20 {
        assert_eq!(
            send(&h.public, pending()).await.status,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    for task in slow {
        assert_eq!(task.await.unwrap(), StatusCode::REQUEST_TIMEOUT);
    }
    let logs = captured.text();
    assert_eq!(warnings(&logs, "too many webhook deliveries at once"), 1);
    assert_eq!(warnings(&logs, "did not arrive in time"), 1);
    let metrics = h.state.metrics().render();
    for series in [
        "wa_server_webhook_deliveries_total{outcome=\"busy\"} 20",
        "wa_server_webhook_deliveries_total{outcome=\"slow_body\"} 64",
    ] {
        assert!(metrics.contains(series), "{series}: {metrics}");
    }
}

/// A body that breaks off before it all arrived (a reset connection): the
/// same, one line a minute, whoever sends them. Decisive: the rejection log
/// on the broken-body path.
#[tokio::test]
async fn broken_bodies_are_logged_at_most_once_a_minute() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    let signature = format!("sha256={}", "ab".repeat(32));
    for _ in 0..20 {
        let broken = futures::stream::iter([Err::<Bytes, _>(std::io::Error::other("reset"))]);
        let reply = send(
            &h.public,
            Call::new(Method::POST, "/webhooks/meta")
                .header("x-hub-signature-256", &signature)
                .body(Body::from_stream(broken))
                .build(),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        warnings(&captured.text(), "could not read a webhook body"),
        1
    );
    let metrics = h.state.metrics().render();
    assert!(
        metrics.contains("wa_server_webhook_deliveries_total{outcome=\"failed\"} 20"),
        "{metrics}"
    );
}
