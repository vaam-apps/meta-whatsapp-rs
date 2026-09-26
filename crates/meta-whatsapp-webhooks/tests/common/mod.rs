//! Shared test helpers: fixture loading and tiny in-test sinks (the sink
//! adapters live in `meta-whatsapp-adapters` and are not used here on purpose).

#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use meta_whatsapp_core::error::SinkError;
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_webhooks::{WebhookEvent, WebhookPayload};

pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Raw bytes of `tests/fixtures/<name>`.
pub fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = fixture_dir().join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Parsed payload of `tests/fixtures/<name>`.
pub fn payload(name: &str) -> WebhookPayload {
    WebhookPayload::from_slice(&fixture_bytes(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Events of `tests/fixtures/<name>`.
pub fn events(name: &str) -> Vec<WebhookEvent> {
    payload(name).into_events()
}

/// Every fixture file, relative to the fixture directory (`/`-separated),
/// sorted. Walks every directory, so a fixture in a new one is not missed.
pub fn all_fixtures() -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), &rel, out);
            } else if Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            {
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    walk(&fixture_dir(), "", &mut out);
    out.sort();
    out
}

/// Records every event; optionally fails on the n-th delivery (0-based).
#[derive(Debug, Default)]
pub struct RecordingSink {
    pub events: Mutex<Vec<WebhookEvent>>,
    calls: AtomicUsize,
    fail_on: Mutex<Option<usize>>,
}

impl RecordingSink {
    pub fn failing_on(call: usize) -> Self {
        let sink = Self::default();
        *sink.fail_on.lock().unwrap() = Some(call);
        sink
    }

    pub fn stop_failing(&self) {
        *self.fail_on.lock().unwrap() = None;
    }

    pub fn delivered(&self) -> Vec<WebhookEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl EventSink<WebhookEvent> for RecordingSink {
    async fn deliver(&self, event: WebhookEvent) -> Result<(), SinkError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if *self.fail_on.lock().unwrap() == Some(call) {
            return Err(SinkError::Closed);
        }
        self.events.lock().unwrap().push(event);
        Ok(())
    }
}
