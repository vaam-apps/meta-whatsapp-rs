//! The log half of `throughput.rs` (roadmap B2a): neither the lines a
//! followed throughput upgrade logs nor the `Debug` of a directory or of
//! the upgrades name the WABA or the display number, and the lines name no
//! phone number id. Alone in its binary, as the service's `*_logs.rs` tests
//! are: the capture is a thread's default subscriber, and `tracing` caches
//! whether a call site is enabled across threads, so a test running beside
//! it could leave a line uncaptured.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use common::{NUMBER, event, events_of, fixture_json};
use futures::executor::block_on;
use meta_whatsapp_bot::{KnownNumbers, ThroughputUpgrades, TokenBucket};
use meta_whatsapp_core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_webhooks::WebhookEvent;
use serde_json::json;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// Meta's example: the WABA (`entry.id`) and the display number of
/// `fields/phone_number_quality_update.json`.
const WABA: &str = "102290129340398";
const DISPLAY: &str = "15550783881";
const FIXTURE: &str = "fields/phone_number_quality_update.json";

/// The fixture's number, registered with a formatted display number.
fn ours() -> KnownNumbers {
    KnownNumbers::new()
        .number(
            WabaId::new(WABA).unwrap(),
            "+1 555-078-3881",
            PhoneNumberId::new(NUMBER).unwrap(),
        )
        .unwrap()
}

/// The fixture with its `event` replaced.
fn quality_update(value: &str) -> WebhookEvent {
    let mut json = fixture_json(FIXTURE);
    json["entry"][0]["changes"][0]["value"]["event"] = json!(value);
    let mut events = events_of(&json);
    assert_eq!(events.len(), 1);
    events.remove(0)
}

/// Every log line as `LEVEL field=value …`.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<String>>>);

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

/// Neither the logs (an upgrade followed, one already followed, a number
/// not known, another value) nor the `Debug` of a directory or of the
/// upgrades name the WABA or the display number, in any shape; the logs
/// name no phone number id.
#[test]
fn the_logs_and_debug_name_no_number() {
    const PRIVATE: [&str; 4] = [WABA, DISPLAY, "555-078-3881", "5550783881"];
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let upgrades = ThroughputUpgrades::new(Arc::new(TokenBucket::default()), ours());
    let unknown = ThroughputUpgrades::new(Arc::new(TokenBucket::default()), KnownNumbers::new());
    block_on(async {
        assert!(upgrades.observe(&event(FIXTURE)).await.unwrap().is_some());
        assert!(upgrades.observe(&event(FIXTURE)).await.unwrap().is_none());
        assert!(unknown.observe(&event(FIXTURE)).await.unwrap().is_none());
        assert!(
            upgrades
                .observe(&quality_update("ONBOARDING"))
                .await
                .unwrap()
                .is_none()
        );
    });
    let lines = capture.0.lock().unwrap().clone();
    for level in ["INFO", "WARN", "DEBUG"] {
        assert!(lines.iter().any(|l| l.starts_with(level)), "{lines:?}");
    }
    // The logs carry no phone number id either (the bucket's `Debug`,
    // inside the upgrades', lists its numbers' rates by id).
    for line in &lines {
        assert!(!line.contains(NUMBER), "{line}");
    }
    let debug = format!("{:?} {:?}", ours(), upgrades);
    for text in lines.iter().chain([&debug]) {
        for private in PRIVATE {
            assert!(!text.contains(private), "{private} in {text}");
        }
    }
}
