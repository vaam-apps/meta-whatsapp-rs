//! Conformance with Meta's webhook pages: every example payload on every
//! page that documents a webhook, parsed through the public API.
//!
//! The manifest (`conformance/manifest.rs`) is the contract. [`PAGES`] lists
//! each page with its status and the examples it shows (by heading, with
//! "(k of n)" when a heading repeats on the page); [`CASES`] maps every
//! fixture file to the example(s) it copies and the events it must produce,
//! in order, with their account, business number and user.
//!
//! Every case, and so every fixture, goes through every check below: the
//! fixture directory and the manifest must list the same files, so a new
//! fixture cannot skip them.
//!
//! - it parses fully typed (no `ChangeValue::Unknown`) into exactly the
//!   manifest's events: variant, WABA, business phone number id, and the
//!   matched user's BSUID and phone number;
//! - every value the example shows survives the typed parse (ids,
//!   timestamps, enum values, text), so nothing documented is dropped or
//!   altered;
//! - no enum value lands in an `Other` catch-all and no message content in
//!   `Unknown`/`Invalid`: every value the page shows is a known variant;
//! - JSON is kept untyped only in the few properties the docs define as
//!   another API's body ([`OPAQUE`]);
//! - the exact bytes, signed, go through [`WebhookHandler`] and reach the
//!   sink as those events; a one-byte change is refused; with a dedup store,
//!   Meta's retry of the same body delivers nothing twice.

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
#[path = "conformance/manifest.rs"]
mod manifest;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use meta_whatsapp_adapters::store::MemoryKvStore;
use meta_whatsapp_core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_webhooks::{
    DedupGuard, SignatureVerifier, WebhookEvent, WebhookHandler, WebhookPayload, sign,
};
use pretty_assertions::assert_eq;
use serde_json::Value;

use common::RecordingSink;
use manifest::{CASES, PAGES};

/// How a fixture relates to the example it copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The example's JSON, unchanged.
    Verbatim,
    /// The example with its placeholders filled in (the page's own example
    /// values first) and its syntax repaired (comments, trailing commas);
    /// no property added or removed.
    Filled,
    /// Built from a page's syntax block or several of its examples; the
    /// label says which. Never counts as covering a page's example.
    Composed,
}

/// What Meta's page is to this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Every example parses into typed events.
    Typed,
    /// Every example parses into typed events, but part of the payload is
    /// kept as JSON; the text says which and why.
    Partial(&'static str),
    /// Shows only part of a payload (one message object), which a composed
    /// fixture wraps; the text says what the page shows. Lists no example.
    Fragments(&'static str),
    /// Listed by Meta but not readable when the mirror was taken; the text
    /// says what was tried. No case may cite it.
    Unreadable(&'static str),
}

/// A documentation page and the example payloads it shows.
#[derive(Debug)]
pub struct Page {
    /// Path relative to
    /// `https://developers.facebook.com/documentation/business-messaging/whatsapp/`.
    pub path: &'static str,
    /// Status.
    pub status: Status,
    /// Every example payload on the page, by heading.
    pub examples: &'static [&'static str],
}

/// One fixture and what it must produce.
#[derive(Debug)]
pub struct Case {
    /// Path under `tests/fixtures/`.
    pub fixture: &'static str,
    /// The examples it copies: `(origin, page, example heading)`.
    pub sources: &'static [(Origin, &'static str, &'static str)],
    /// The events, in order.
    pub events: &'static [Ev],
}

/// One expected event.
#[derive(Debug, PartialEq, Eq)]
pub struct Ev {
    /// [`WebhookEvent::kind`].
    pub kind: &'static str,
    /// [`WebhookEvent::waba_id`].
    pub waba: Option<&'static str>,
    /// [`WebhookEvent::phone_number_id`].
    pub phone: Option<&'static str>,
    /// The BSUID of [`WebhookEvent::contact`].
    pub user: Option<&'static str>,
    /// The phone number of [`WebhookEvent::contact`].
    pub wa_id: Option<&'static str>,
}

/// Shorthand for the manifest.
pub const fn ev(
    kind: &'static str,
    waba: Option<&'static str>,
    phone: Option<&'static str>,
    user: Option<&'static str>,
    wa_id: Option<&'static str>,
) -> Ev {
    Ev {
        kind,
        waba,
        phone,
        user,
        wa_id,
    }
}

/// Properties Meta defines as another API's JSON, kept as
/// `serde_json::Value` on purpose: `calling` (the calling settings API's
/// shape, `account_settings_update`), `connection` (a call's WebRTC
/// session, `calls`), and a standby echo's `message` (the Send API request
/// body), `template` (a template definition) and `flow` (a Flow definition).
/// Plus `error_data` of `meta_whatsapp_core::GraphApiError`, whose shape
/// varies across Graph endpoints; `GraphApiError::details` reads it.
const OPAQUE: &[&str] = &[
    "calling",
    "connection",
    "message",
    "template",
    "flow",
    "error_data",
];

/// Spellings the pages use for the same property, and the one this crate
/// writes back (see the `fields::messages` module docs).
const ALIASES: &[(&str, &str)] = &[
    ("participant_recipient_id", "recipient_participant_id"),
    ("parent_recipient_user_id", "recipient_parent_user_id"),
];

/// Values the pages spell two ways, and the one this crate writes back
/// (`fields::calls` module docs: terminate `status`).
const VALUE_ALIASES: &[(&str, &str)] = &[("Failed", "FAILED"), ("Completed", "COMPLETED")];

fn case(name: &str) -> &'static Case {
    CASES
        .iter()
        .find(|c| c.fixture == name)
        .unwrap_or_else(|| panic!("{name}: not in the manifest (tests/conformance/manifest.rs)"))
}

#[test]
fn the_manifest_lists_every_fixture_exactly_once() {
    let files: BTreeSet<String> = common::all_fixtures().into_iter().collect();
    let mut listed = BTreeSet::new();
    for c in CASES {
        assert!(listed.insert(c.fixture), "{}: listed twice", c.fixture);
        assert!(!c.sources.is_empty(), "{}: no source", c.fixture);
        assert!(!c.events.is_empty(), "{}: no expected event", c.fixture);
    }
    let listed: BTreeSet<String> = listed.into_iter().map(str::to_owned).collect();
    let unlisted: Vec<_> = files.difference(&listed).collect();
    let missing: Vec<_> = listed.difference(&files).collect();
    assert!(
        unlisted.is_empty(),
        "fixtures without a manifest case: {unlisted:?}"
    );
    assert!(
        missing.is_empty(),
        "manifest cases without a fixture: {missing:?}"
    );
}

#[test]
fn every_example_on_every_page_has_a_fixture() {
    let pages: BTreeMap<&str, &Page> = PAGES.iter().map(|p| (p.path, p)).collect();
    assert_eq!(pages.len(), PAGES.len(), "a page is listed twice");
    let mut covered: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for c in CASES {
        for &(origin, page, example) in c.sources {
            let listed = pages.get(page).unwrap_or_else(|| {
                panic!("{}: cites {page}, which PAGES does not list", c.fixture)
            });
            assert!(
                !matches!(listed.status, Status::Unreadable(_)),
                "{}: cites unreadable {page}",
                c.fixture
            );
            if origin != Origin::Composed {
                assert!(
                    listed.examples.contains(&example),
                    "{}: {page} has no example {example:?}",
                    c.fixture
                );
                covered.entry(page).or_default().insert(example);
            }
        }
    }
    for page in PAGES {
        let have = covered.remove(page.path).unwrap_or_default();
        match page.status {
            Status::Unreadable(why) | Status::Fragments(why) => {
                assert!(!why.is_empty() && page.examples.is_empty(), "{}", page.path);
                assert!(have.is_empty(), "{}: {have:?}", page.path);
            }
            Status::Typed | Status::Partial(_) => {
                assert!(
                    !page.examples.is_empty(),
                    "{}: no example listed",
                    page.path
                );
                let want: BTreeSet<&str> = page.examples.iter().copied().collect();
                assert_eq!(want.len(), page.examples.len(), "{}: duplicate", page.path);
                let uncovered: Vec<_> = want.difference(&have).collect();
                assert!(
                    uncovered.is_empty(),
                    "{}: no fixture for {uncovered:?}",
                    page.path
                );
            }
        }
    }
}

fn contact_ids(event: &WebhookEvent) -> (Option<String>, Option<String>) {
    event.contact().map_or((None, None), |c| {
        (
            c.user_id.as_ref().map(ToString::to_string),
            c.wa_id.as_ref().map(ToString::to_string),
        )
    })
}

#[test]
fn every_case_parses_into_exactly_its_events() {
    for c in CASES {
        let payload = common::payload(c.fixture);
        assert_eq!(payload.object, "whatsapp_business_account", "{}", c.fixture);
        for entry in &payload.entry {
            for change in &entry.changes {
                assert!(
                    !change.value.is_unknown(),
                    "{}: `{}` fell back to Unknown: {:?}",
                    c.fixture,
                    change.field,
                    change.parse_error
                );
            }
        }
        let events = payload.into_events();
        let got: Vec<_> = events
            .iter()
            .map(|e| {
                let (user, wa_id) = contact_ids(e);
                (
                    e.kind(),
                    e.waba_id().map(ToString::to_string),
                    e.phone_number_id().map(ToString::to_string),
                    user,
                    wa_id,
                )
            })
            .collect();
        let want: Vec<_> = c
            .events
            .iter()
            .map(|e| {
                (
                    e.kind,
                    e.waba.map(str::to_owned),
                    e.phone.map(str::to_owned),
                    e.user.map(str::to_owned),
                    e.wa_id.map(str::to_owned),
                )
            })
            .collect();
        assert_eq!(got, want, "{}", c.fixture);
    }
}

/// Whether `typed` carries the same value as `orig`: equal, or the same
/// scalar written as a string (ids and unix timestamps are strings once
/// typed; Meta prints some as numbers), or a documented second spelling.
fn same_scalar(orig: &Value, typed: &Value) -> bool {
    let text = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    };
    if orig == typed {
        return true;
    }
    match (text(orig), text(typed)) {
        (Some(a), Some(b)) => {
            a == b
                || VALUE_ALIASES.contains(&(a.as_str(), b.as_str()))
                // `25000` typed as `f64` writes back `25000.0`: the same
                // number, compared exactly on purpose.
                || matches!(
                    (orig.as_f64(), typed.as_f64()),
                    (Some(x), Some(y)) if x.to_bits() == y.to_bits()
                )
        }
        _ => false,
    }
}

fn compare(orig: &Value, typed: &Value, path: &str, out: &mut Vec<String>) {
    match (orig, typed) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in a {
                let canonical = ALIASES
                    .iter()
                    .find(|(alias, _)| alias == key)
                    .map_or(key.as_str(), |(_, to)| to);
                let p = format!("{path}.{key}");
                match b.get(canonical) {
                    Some(w) => compare(value, w, &p, out),
                    None if value.is_null() => {}
                    // An empty list or object says nothing; typed lists
                    // skip serializing when empty.
                    None if value.as_array().is_some_and(Vec::is_empty) => {}
                    None if value.as_object().is_some_and(serde_json::Map::is_empty) => {}
                    None => out.push(format!("{p}: dropped")),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() == b.len() {
                for (i, (v, w)) in a.iter().zip(b).enumerate() {
                    compare(v, w, &format!("{path}[{i}]"), out);
                }
            } else {
                out.push(format!("{path}: {} items became {}", a.len(), b.len()));
            }
        }
        (orig, typed) if same_scalar(orig, typed) => {}
        (orig, typed) => out.push(format!("{path}: {orig} became {typed}")),
    }
}

#[test]
fn every_value_the_examples_show_survives_the_typed_parse() {
    for c in CASES {
        let orig: Value = serde_json::from_slice(&common::fixture_bytes(c.fixture)).unwrap();
        let typed = serde_json::to_value(common::payload(c.fixture)).unwrap();
        let mut changed = Vec::new();
        compare(&orig, &typed, "", &mut changed);
        assert!(changed.is_empty(), "{}: {changed:#?}", c.fixture);
    }
}

/// Every `name: …` in `debug` that opens `serde_json::Value` JSON.
fn untyped_properties(debug: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for opener in [
        ": Object {",
        ": Some(Object {",
        ": Array [",
        ": Some(Array [",
    ] {
        let mut rest = debug;
        while let Some(i) = rest.find(opener) {
            let name: String = rest[..i]
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            // A quoted key (`"sip": Object {`) is inside a JSON value
            // already counted under its property's name.
            if !name.is_empty() {
                out.insert(name);
            }
            rest = &rest[i + opener.len()..];
        }
    }
    out
}

#[test]
fn no_value_falls_into_a_catch_all_or_stays_json() {
    for c in CASES {
        let debug = format!("{:?}", common::payload(c.fixture));
        for catch_all in ["Other(\"", "Unknown {", "Invalid {"] {
            assert!(
                !debug.contains(catch_all),
                "{}: a value fell into `{catch_all}`: {debug}",
                c.fixture
            );
        }
        let untyped: Vec<_> = untyped_properties(&debug)
            .into_iter()
            .filter(|name| !OPAQUE.contains(&name.as_str()))
            .collect();
        assert!(
            untyped.is_empty(),
            "{}: kept as JSON: {untyped:?}",
            c.fixture
        );
    }
}

const SECRET: &str = "4f0c1d2e3b4a59687f6e5d4c3b2a1908";

fn handler(sink: Arc<RecordingSink>, dedup: bool) -> WebhookHandler {
    let builder = WebhookHandler::builder(
        SignatureVerifier::new(vec![AppSecret::new(SECRET)]).unwrap(),
        VerifyToken::new("conformance"),
        sink,
    );
    if dedup {
        builder
            .dedup(DedupGuard::new(Arc::new(MemoryKvStore::new())))
            .build()
    } else {
        builder.build()
    }
}

#[tokio::test]
async fn every_case_arrives_through_the_signed_handler() {
    let secret = AppSecret::new(SECRET);
    for c in CASES {
        let body = common::fixture_bytes(c.fixture);
        let expected = WebhookPayload::from_slice(&body).unwrap().into_events();
        let header = sign(&secret, &body);

        let sink = Arc::new(RecordingSink::default());
        let report = handler(sink.clone(), false)
            .deliver(Some(&header), &body)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", c.fixture));
        assert_eq!(report.delivered, expected.len(), "{}", c.fixture);
        assert_eq!(report.unparsed, 0, "{}", c.fixture);
        assert_eq!(sink.delivered(), expected, "{}", c.fixture);

        // One byte changed (inside the JSON, so it still parses): refused.
        let mut tampered = body.clone();
        let at = tampered.iter().position(|b| *b == b'1').unwrap();
        tampered[at] = b'2';
        let sink = Arc::new(RecordingSink::default());
        assert!(
            handler(sink.clone(), false)
                .deliver(Some(&header), &tampered)
                .await
                .is_err(),
            "{}: a tampered body was accepted",
            c.fixture
        );
        assert!(sink.delivered().is_empty(), "{}", c.fixture);

        // Meta retries a body until it gets a 200, for up to 7 days: with a
        // dedup store the retry delivers only what has no dedup key.
        let sink = Arc::new(RecordingSink::default());
        let deduped = handler(sink.clone(), true);
        let first = deduped.deliver(Some(&header), &body).await.unwrap();
        assert_eq!(first.delivered, expected.len(), "{}: {first:?}", c.fixture);
        assert_eq!(first.duplicates, 0, "{}: keys collide", c.fixture);
        let retry = deduped.deliver(Some(&header), &body).await.unwrap();
        let unkeyed = expected.iter().filter(|e| e.dedup_key().is_none()).count();
        assert_eq!(retry.delivered, unkeyed, "{}", c.fixture);
        assert_eq!(retry.duplicates, expected.len() - unkeyed, "{}", c.fixture);
    }
}

/// The guards above are only as good as their failure paths: a catch-all,
/// an altered value, a dropped property and an unlisted fixture must each
/// be caught.
#[test]
fn the_checks_catch_what_they_claim_to() {
    let c = case("messages/text.json");
    let orig: Value = serde_json::from_slice(&common::fixture_bytes(c.fixture)).unwrap();

    let mut altered = orig.clone();
    altered["entry"][0]["changes"][0]["value"]["messages"][0]["timestamp"] = "1".into();
    let mut changed = Vec::new();
    compare(
        &altered,
        &serde_json::to_value(common::payload(c.fixture)).unwrap(),
        "",
        &mut changed,
    );
    assert_eq!(changed.len(), 1, "{changed:?}");

    let mut added = orig.clone();
    added["entry"][0]["changes"][0]["value"]["messages"][0]["brand_new"] = "x".into();
    let typed = serde_json::to_value(
        WebhookPayload::from_slice(&serde_json::to_vec(&added).unwrap()).unwrap(),
    )
    .unwrap();
    let mut dropped = Vec::new();
    compare(&added, &typed, "", &mut dropped);
    assert_eq!(
        dropped,
        [".entry[0].changes[0].value.messages[0].brand_new: dropped"]
    );

    let mut other = orig;
    other["entry"][0]["changes"][0]["value"]["messages"][0]["referral"] =
        serde_json::json!({"source_type": "billboard"});
    let debug = format!(
        "{:?}",
        WebhookPayload::from_slice(&serde_json::to_vec(&other).unwrap()).unwrap()
    );
    assert!(debug.contains("Other(\""), "{debug}");

    assert!(untyped_properties("X { a: 1, raw: Some(Object {}) }").contains("raw"));
    assert!(
        CASES
            .iter()
            .all(|c| c.fixture != "fields/not_a_fixture.json")
    );
}
