//! The log half of `retention.rs` (roadmap B4): `RetentionSweeper::run`
//! logs what a sweep deleted as counts and a failed sweep as its error,
//! never a message's content, and a failed sweep does not stop it: the
//! next runs on schedule. Alone in its binary, as `throughput_logs.rs` is:
//! the capture is a thread's default subscriber, and `tracing` caches
//! whether a call site is enabled across threads, so a test running beside
//! it could leave a line uncaptured.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write as _;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use meta_whatsapp_adapters::store::postgres::sqlx;
use meta_whatsapp_adapters::store::{MemoryConversationStore, PostgresConversationStore};
use meta_whatsapp_bot::retention::{RetentionPolicy, RetentionSweeper};
use meta_whatsapp_core::clock::{Clock, ManualClock};
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_core::store::{
    ConversationKey, ConversationStore, DeliveryStatus, Direction, StoredMessage,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use time::OffsetDateTime;
use time::macros::datetime;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

const NOW: OffsetDateTime = datetime!(2026-10-01 12:00 UTC);
/// What the stored messages say: no log line may carry it.
const SECRET: &str = "the customer's private words";

/// Every log line as `(target, "LEVEL field=value …")`.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<(String, String)>>>);

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
        let target = event.metadata().target().to_owned();
        self.0.lock().unwrap().push((target, line.0));
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

impl Capture {
    /// The lines captured since the last call, and those of the sweeper
    /// among them.
    fn take(&self) -> (Vec<String>, Vec<String>) {
        let lines = std::mem::take(&mut *self.0.lock().unwrap());
        let ours = lines
            .iter()
            .filter(|(target, _)| target == "meta_whatsapp_bot::retention")
            .map(|(_, line)| line.clone())
            .collect();
        (lines.into_iter().map(|(_, line)| line).collect(), ours)
    }
}

/// `run` until `clock` reaches `stop_at`.
async fn run_until(sweeper: &RetentionSweeper, clock: &ManualClock, stop_at: OffsetDateTime) {
    sweeper
        .run(std::future::poll_fn(|_| {
            if clock.now() >= stop_at {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        }))
        .await;
}

#[tokio::test]
async fn a_sweep_logs_counts_and_a_failed_one_its_error_and_the_next_runs() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let pn = PhoneNumberId::new("106540352242922").unwrap();
    let policy = RetentionPolicy::default()
        .max_age(Duration::from_hours(24))
        .max_messages_per_chat(1);

    // A sweep that deletes: its counts, no content.
    let store = Arc::new(MemoryConversationStore::new());
    for (i, days) in [3_i64, 2, 0].into_iter().enumerate() {
        let m = StoredMessage {
            id: MessageId::new(format!("wamid.logs.{i}")),
            conversation: ConversationKey::new(pn.clone(), "US.logs"),
            direction: Direction::Inbound,
            kind: "text".to_owned(),
            text: Some(SECRET.to_owned()),
            payload: serde_json::json!({"type": "text", "text": {"body": SECRET}}),
            status: DeliveryStatus::Received,
            timestamp: NOW - time::Duration::days(days),
            status_at: None,
            error: None,
        };
        assert!(store.append(m).await.unwrap());
    }
    let clock = Arc::new(ManualClock::new(NOW));
    let sweeper = RetentionSweeper::new(store.clone(), policy)
        .timer(clock.clone())
        .every(Duration::from_hours(1));
    run_until(&sweeper, &clock, NOW + time::Duration::hours(1)).await;
    let (lines, ours) = capture.take();
    assert!(lines.iter().all(|l| !l.contains(SECRET)), "{lines:?}");
    assert_eq!(
        ours,
        ["DEBUG message=retention sweep messages=2 conversations=0"],
        "one line, for the sweep that deleted (the second deleted nothing)"
    );

    // A store that fails every call (nothing listens on port 1): each sweep
    // logs its error, and the next one runs on schedule.
    let options = PgConnectOptions::from_str("postgres://wa:wa@127.0.0.1:1/wa").unwrap();
    let failing = PostgresConversationStore::new(
        PgPoolOptions::new()
            .acquire_timeout(Duration::from_secs(2))
            .connect_lazy_with(options),
    );
    let clock = Arc::new(ManualClock::new(NOW));
    let sweeper = RetentionSweeper::new(Arc::new(failing), policy)
        .timer(clock.clone())
        .every(Duration::from_hours(1));
    assert!(sweeper.sweep().await.is_err(), "the store fails");
    let stop_at = NOW + time::Duration::hours(2);
    run_until(&sweeper, &clock, stop_at).await;
    assert_eq!(
        clock.now(),
        stop_at,
        "a failed sweep does not stop the sweeper: it waited two intervals"
    );
    let (lines, ours) = capture.take();
    let warnings: Vec<_> = ours.iter().filter(|l| l.starts_with("WARN")).collect();
    assert_eq!(
        warnings.len(),
        2,
        "one warning per failed sweep, at NOW and NOW + 1 h: {lines:?}"
    );
    for line in &warnings {
        assert!(
            line.contains("message=a retention sweep failed error="),
            "{line}"
        );
    }
    assert!(
        lines.iter().all(|l| !l.contains(SECRET)),
        "no line names a message's content: {lines:?}"
    );
}
