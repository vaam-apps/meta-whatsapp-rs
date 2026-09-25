//! The `data` of an event (docs/design/server.md, section 2.3) is the
//! library's `WebhookEvent` JSON, pinned here by snapshots over Meta's
//! documented examples (the library's fixtures,
//! `crates/meta-whatsapp-webhooks/tests/fixtures`): a library change that
//! alters what a caller receives fails this test and forces an API-version
//! decision (within `v1`, only additions).
//!
//! A new fixture needs its snapshot: run
//! `META_WHATSAPP_SERVER_UPDATE_SNAPSHOTS=1 cargo test -p meta-whatsapp-server --test event_data`
//! and review what it wrote under `tests/snapshots/event_data/`. A changed
//! snapshot is a change of the API.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use meta_whatsapp_rs::webhooks::WebhookPayload;
use meta_whatsapp_server::events::{
    OPERATOR_EVENT_TYPES, TENANT_EVENT_TYPES, event_data, meta_time,
};
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../meta-whatsapp-webhooks/tests/fixtures")
}

fn snapshots_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/event_data")
}

/// Every fixture, `dir/name.json`, sorted.
fn fixtures() -> Vec<String> {
    let mut out = Vec::new();
    for dir in ["messages", "fields", "bsuid"] {
        for entry in std::fs::read_dir(fixtures_dir().join(dir)).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            if Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            {
                out.push(format!("{dir}/{name}"));
            }
        }
    }
    out.sort();
    out
}

/// What each fixture's events are served as: `[{"type", "data"}, …]`.
fn served(fixture: &str) -> Value {
    let body = std::fs::read(fixtures_dir().join(fixture)).unwrap();
    let events = WebhookPayload::from_slice(&body)
        .unwrap_or_else(|e| panic!("{fixture}: {e}"))
        .into_events();
    Value::Array(
        events
            .iter()
            .map(|event| {
                let data: Value = serde_json::from_str(&event_data(event).unwrap()).unwrap();
                serde_json::json!({"type": event.kind(), "data": data})
            })
            .collect(),
    )
}

#[test]
fn event_data_is_pinned_over_metas_examples() {
    let update = std::env::var("META_WHATSAPP_SERVER_UPDATE_SNAPSHOTS").is_ok_and(|v| v == "1");
    let fixtures = fixtures();
    assert!(fixtures.len() >= 90, "{} fixtures", fixtures.len());
    let mut failures = Vec::new();
    let mut expected_files = BTreeSet::new();
    for fixture in &fixtures {
        let file = snapshots_dir().join(fixture);
        expected_files.insert(file.clone());
        let served = served(fixture);
        if update {
            let mut text = serde_json::to_string_pretty(&served).unwrap();
            text.push('\n');
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, &text).unwrap();
            continue;
        }
        // Compared as JSON values: key order is not part of the contract
        // (and depends on serde_json's `preserve_order` feature).
        match std::fs::read_to_string(&file) {
            Ok(pinned) if serde_json::from_str::<Value>(&pinned).ok() == Some(served) => {}
            Ok(_) => failures.push(format!("{fixture}: the served JSON changed")),
            Err(_) => failures.push(format!("{fixture}: no snapshot")),
        }
    }
    // No snapshot outlives its fixture.
    for dir in ["messages", "fields", "bsuid"] {
        for entry in std::fs::read_dir(snapshots_dir().join(dir)).unwrap() {
            let path = entry.unwrap().path();
            if !expected_files.contains(&path) {
                failures.push(format!("{}: no such fixture", path.display()));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{failures:#?}\nA changed snapshot is a change of the v1 API: if intended, regenerate \
         with META_WHATSAPP_SERVER_UPDATE_SNAPSHOTS=1 (see this file's docs) and review the diff"
    );
}

/// Every type the library names (`WebhookEvent::kind`) is either one a
/// tenant receives or an operator-only one: a type a library release adds
/// fails here until the service decides which (until then it is
/// operator-only, never shown to a tenant).
#[test]
fn every_library_event_type_is_classified() {
    let source = include_str!("../../meta-whatsapp-webhooks/src/event.rs");
    let body = source
        .split("pub fn kind(&self) -> &'static str {")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("WebhookEvent::kind in the library");
    let named: BTreeSet<&str> = body
        .split("=> \"")
        .skip(1)
        .map(|rest| &rest[..rest.find('"').unwrap()])
        .collect();
    assert!(named.len() >= 31, "{named:?}");
    let classified: BTreeSet<&str> = TENANT_EVENT_TYPES
        .iter()
        .chain(OPERATOR_EVENT_TYPES.iter())
        .copied()
        .collect();
    assert_eq!(named, classified);
    // Every fixture's types are among them.
    for fixture in fixtures() {
        for event in served(&fixture).as_array().unwrap() {
            let kind = event["type"].as_str().unwrap();
            assert!(classified.contains(kind), "{fixture}: {kind}");
        }
    }
}

/// Every event of Meta's examples is dated ([`meta_time`]: the routing
/// refuses an event dated before its binding, security review M3), but
/// for the kinds that carry no date of their own: history and contact
/// syncs, errors. Each date is the one Meta's example says. A library
/// change that loses a date fails here.
#[test]
fn every_dated_event_type_has_its_meta_time() {
    const UNDATED: [&str; 3] = ["history_synced", "app_state_synced", "error_reported"];
    let mut dated = BTreeSet::new();
    for fixture in fixtures() {
        let body = std::fs::read(fixtures_dir().join(&fixture)).unwrap();
        for event in WebhookPayload::from_slice(&body).unwrap().into_events() {
            let kind = event.kind();
            let time = meta_time(&event);
            if UNDATED.contains(&kind) {
                assert_eq!(time, None, "{fixture}: {kind}");
                continue;
            }
            let data = serde_json::to_value(&event).unwrap();
            // The date Meta's example gives: the item's own, else the
            // entry's.
            let seconds = |v: &Value| v.as_i64().or_else(|| v.as_str()?.parse().ok());
            let own = [
                "message",
                "status",
                "echo",
                "call",
                "preference",
                "update",
                "detected",
            ]
            .iter()
            .find_map(|item| seconds(&data[item]["timestamp"]));
            let expected = own.or_else(|| seconds(&data["time"]));
            assert_eq!(
                time.map(time::OffsetDateTime::unix_timestamp),
                expected,
                "{fixture}: {kind}"
            );
            if time.is_some() {
                dated.insert(kind);
            }
        }
    }
    // Every tenant type but the undated ones is dated in some example.
    for kind in TENANT_EVENT_TYPES {
        if !UNDATED.contains(&kind) {
            assert!(dated.contains(kind), "no dated example of {kind}");
        }
    }
}
