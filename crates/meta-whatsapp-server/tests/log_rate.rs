//! Security review L5: refused deliveries cost at most one log line a
//! minute per reason, whoever sends them; the metric counts every one.
//! Alone in its binary: the capture is a thread's default subscriber (see
//! `live_logs.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::capture::{Captured, subscriber};
use common::meta::example_text;
use common::{Call, Harness, send};
use meta_whatsapp_rs::core::secret::AppSecret;
use meta_whatsapp_rs::webhooks::axum::body::Body;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_rs::webhooks::sign;

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
