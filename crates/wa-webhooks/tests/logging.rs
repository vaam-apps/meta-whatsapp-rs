//! What `WebhookHandler` logs about a body: sizes, digests, field names and
//! redacted error text, never the content. Captured with a minimal
//! in-test `tracing` subscriber (no `tracing-subscriber` dependency).

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};
use wa_core::secret::{AppSecret, VerifyToken};
use wa_webhooks::{SignatureVerifier, WebhookEvent, WebhookHandler, sign};

use common::RecordingSink;

/// Every event as `LEVEL field=value …`.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<String>>>);

impl Capture {
    fn text(&self) -> String {
        self.0.lock().unwrap().join("\n")
    }
}

struct Line(String);

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, " {}={value:?}", field.name());
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut line = Line(event.metadata().level().to_string());
        event.record(&mut line);
        self.0.lock().unwrap().push(line.0);
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

const PII: [&str; 3] = ["Jane", "4915112345678", "jane@example.com"];
const SECRET: &str = "b1946ac92492d2347c6235b4d2611184";

fn handler(sink: Arc<RecordingSink>) -> WebhookHandler {
    WebhookHandler::builder(
        SignatureVerifier::new(vec![AppSecret::new(SECRET)]).unwrap(),
        VerifyToken::new("vibecoding"),
        sink,
    )
    .build()
}

fn assert_no_pii(logs: &str) {
    for pii in PII {
        assert!(!logs.contains(pii), "`{pii}` leaked into the logs:\n{logs}");
    }
}

#[tokio::test]
async fn an_unparsed_body_is_logged_by_size_and_digest_only() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let sink = Arc::new(RecordingSink::default());

    // Signed by "Meta", but `entry` is a string: the serde error quotes it.
    let body = json!({"object": "whatsapp_business_account",
                      "entry": "Jane Doe, +4915112345678, jane@example.com"})
    .to_string()
    .into_bytes();
    handler(sink.clone())
        .deliver(Some(&sign(&AppSecret::new(SECRET), &body)), &body)
        .await
        .unwrap();

    // Not vacuous: the content does reach the error the integrator gets.
    let delivered = sink.delivered();
    let [WebhookEvent::Unparsed { error, .. }] = &delivered[..] else {
        panic!("{delivered:?}")
    };
    assert!(error.contains("Jane"), "{error}");

    let logs = capture.text();
    assert!(logs.starts_with("ERROR"), "{logs}");
    assert!(logs.contains(&format!("body_len={}", body.len())), "{logs}");
    assert!(
        logs.contains(&hex::encode(Sha256::digest(&body))),
        "digest missing: {logs}"
    );
    assert!(logs.contains("expected a sequence"), "{logs}");
    assert_no_pii(&logs);
}

#[tokio::test]
async fn a_change_kept_untyped_is_logged_without_its_content() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let sink = Arc::new(RecordingSink::default());

    // The message's timestamp is garbage, so the whole `messages` change
    // falls back to `Unknown`, and `wa_core::timestamp` quotes the value.
    let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "1", "changes": [{
        "field": "messages",
        "value": {"messaging_product": "whatsapp",
                  "metadata": {"display_phone_number": "15550783881", "phone_number_id": "106540352242922"},
                  "messages": [{"from": "4915112345678", "id": "wamid.X",
                                "timestamp": "Jane Doe jane@example.com",
                                "type": "text", "text": {"body": "hello"}}]}
    }]}]})
    .to_string()
    .into_bytes();
    handler(sink.clone())
        .deliver(Some(&sign(&AppSecret::new(SECRET), &body)), &body)
        .await
        .unwrap();

    let delivered = sink.delivered();
    let [
        WebhookEvent::Unknown {
            field,
            parse_error: Some(error),
            ..
        },
    ] = &delivered[..]
    else {
        panic!("{delivered:?}")
    };
    assert_eq!(field, "messages");
    assert!(error.contains("Jane"), "not vacuous: {error}");

    let logs = capture.text();
    assert!(logs.starts_with("WARN"), "{logs}");
    assert!(logs.contains("field=messages"), "{logs}");
    assert!(logs.contains("invalid unix timestamp"), "{logs}");
    assert_no_pii(&logs);
}

#[tokio::test]
async fn rejected_deliveries_log_no_content_either() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let body = br#"{"note": "Jane Doe +4915112345678"}"#;
    let h = handler(Arc::default());
    assert!(h.deliver(Some("sha256=00"), body).await.is_err());
    assert!(
        h.deliver(Some(&sign(&AppSecret::new("x"), body)), body)
            .await
            .is_err()
    );
    let logs = capture.text();
    assert!(logs.contains("rejected webhook delivery"), "{logs}");
    assert_no_pii(&logs);
    assert!(!logs.contains(SECRET), "{logs}");
}
