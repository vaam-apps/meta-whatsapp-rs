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
//!   altered; only an id or unix time may come back as a string where Meta
//!   printed a number, and the typed form invents nothing;
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
use serde_json::{Value, json};

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

/// Values the pages spell two ways, and the one this crate writes back:
/// a call terminate `status` (`fields::calls` module docs). Accepted for a
/// `status` property only.
const VALUE_ALIASES: &[(&str, &str)] = &[("Failed", "FAILED"), ("Completed", "COMPLETED")];

fn case(name: &str) -> &'static Case {
    CASES
        .iter()
        .find(|c| c.fixture == name)
        .unwrap_or_else(|| panic!("{name}: not in the manifest (tests/conformance/manifest.rs)"))
}

/// What is wrong with the manifest's list of fixtures, against the files on
/// disk: both directions, so neither a new fixture nor a deleted one slips by.
fn manifest_problems<'a>(
    files: &BTreeSet<String>,
    cases: impl IntoIterator<Item = &'a Case>,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut listed = BTreeSet::new();
    for c in cases {
        if !listed.insert(c.fixture.to_owned()) {
            out.push(format!("{}: listed twice", c.fixture));
        }
        if c.sources.is_empty() {
            out.push(format!("{}: no source", c.fixture));
        }
        if c.events.is_empty() {
            out.push(format!("{}: no expected event", c.fixture));
        }
    }
    for f in files.difference(&listed) {
        out.push(format!("{f}: fixture without a manifest case"));
    }
    for f in listed.difference(files) {
        out.push(format!("{f}: manifest case without a fixture"));
    }
    out
}

#[test]
fn the_manifest_lists_every_fixture_exactly_once() {
    let files: BTreeSet<String> = common::all_fixtures().into_iter().collect();
    let problems = manifest_problems(&files, CASES);
    assert!(problems.is_empty(), "{problems:#?}");
}

/// What is wrong with the page list against the cases: a case citing an
/// unlisted page, an unreadable page or an example the page does not show,
/// and a page example no `Verbatim`/`Filled` case covers.
fn coverage_problems<'a>(pages: &[Page], cases: impl IntoIterator<Item = &'a Case>) -> Vec<String> {
    let mut out = Vec::new();
    let by_path: BTreeMap<&str, &Page> = pages.iter().map(|p| (p.path, p)).collect();
    if by_path.len() != pages.len() {
        out.push("a page is listed twice".to_owned());
    }
    let mut covered: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for c in cases {
        for &(origin, page, example) in c.sources {
            let Some(listed) = by_path.get(page) else {
                out.push(format!(
                    "{}: cites {page}, which PAGES does not list",
                    c.fixture
                ));
                continue;
            };
            if matches!(listed.status, Status::Unreadable(_)) {
                out.push(format!("{}: cites unreadable {page}", c.fixture));
            }
            if origin != Origin::Composed {
                if !listed.examples.contains(&example) {
                    out.push(format!("{}: {page} has no example {example:?}", c.fixture));
                }
                covered.entry(page).or_default().insert(example);
            }
        }
    }
    for page in pages {
        let have = covered.remove(page.path).unwrap_or_default();
        match page.status {
            Status::Unreadable(why) | Status::Fragments(why) => {
                if why.is_empty() || !page.examples.is_empty() {
                    out.push(format!(
                        "{}: a page without examples says why, and lists none",
                        page.path
                    ));
                }
                if !have.is_empty() {
                    out.push(format!("{}: cited as an example: {have:?}", page.path));
                }
            }
            Status::Typed | Status::Partial(_) => {
                if page.examples.is_empty() {
                    out.push(format!("{}: no example listed", page.path));
                }
                let want: BTreeSet<&str> = page.examples.iter().copied().collect();
                if want.len() != page.examples.len() {
                    out.push(format!("{}: duplicate example", page.path));
                }
                let uncovered: Vec<_> = want.difference(&have).collect();
                if !uncovered.is_empty() {
                    out.push(format!("{}: no fixture for {uncovered:?}", page.path));
                }
            }
        }
    }
    out
}

#[test]
fn every_example_on_every_page_has_a_fixture() {
    let problems = coverage_problems(PAGES, CASES);
    assert!(problems.is_empty(), "{problems:#?}");
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

/// Whether `key` names an id or a unix time: the only properties whose
/// number Meta prints may come back as a string (this crate types ids and
/// timestamps as strings).
fn id_or_time(key: &str) -> bool {
    key == "id"
        || key.ends_with("_id")
        || key.ends_with("_ids")
        || key == "time"
        || key.ends_with("_time")
        || key.ends_with("timestamp")
        || key == "expiration"
}

/// Whether `typed` carries the same value as `orig` under property `key`:
/// equal; the same number (`25000` typed as `f64` writes back `25000.0`,
/// compared exactly on purpose); an id or unix time Meta printed as a
/// number, written back as the same digits in a string; or a documented
/// second spelling of a `status`. Nothing else crosses JSON types: a
/// number typed as a string, a boolean as a string, or a string as a number
/// is a wrong type, not the same value.
fn same_scalar(key: &str, orig: &Value, typed: &Value) -> bool {
    if orig == typed {
        return true;
    }
    match (orig, typed) {
        (Value::Number(a), Value::Number(b)) => matches!(
            (a.as_f64(), b.as_f64()),
            (Some(x), Some(y)) if x.to_bits() == y.to_bits()
        ),
        (Value::Number(n), Value::String(s)) => id_or_time(key) && n.to_string() == *s,
        (Value::String(a), Value::String(b)) => {
            key == "status" && VALUE_ALIASES.contains(&(a.as_str(), b.as_str()))
        }
        _ => false,
    }
}

/// An empty list or object says nothing; typed lists and options skip
/// serializing when empty.
fn says_nothing(value: &Value) -> bool {
    value.is_null()
        || value.as_array().is_some_and(Vec::is_empty)
        || value.as_object().is_some_and(serde_json::Map::is_empty)
}

/// Every difference between the example (`orig`) and its typed parse
/// written back (`typed`): a value dropped, altered or retyped, a list that
/// changed length, and a value the typed form invents.
fn compare(orig: &Value, typed: &Value, path: &str, out: &mut Vec<String>) {
    compare_at("", orig, typed, path, out);
}

fn compare_at(key: &str, orig: &Value, typed: &Value, path: &str, out: &mut Vec<String>) {
    match (orig, typed) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in a {
                let canonical = ALIASES
                    .iter()
                    .find(|(alias, _)| alias == key)
                    .map_or(key.as_str(), |(_, to)| to);
                let p = format!("{path}.{key}");
                match b.get(canonical) {
                    Some(w) => compare_at(canonical, value, w, &p, out),
                    None if says_nothing(value) => {}
                    None => out.push(format!("{p}: dropped")),
                }
            }
            for (key, value) in b {
                let spellings: Vec<&str> = std::iter::once(key.as_str())
                    .chain(
                        ALIASES
                            .iter()
                            .filter(|(_, to)| to == key)
                            .map(|(alias, _)| *alias),
                    )
                    .collect();
                if !spellings.iter().any(|k| a.contains_key(*k)) && !says_nothing(value) {
                    out.push(format!("{path}.{key}: added {value}"));
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() == b.len() {
                for (i, (v, w)) in a.iter().zip(b).enumerate() {
                    compare_at(key, v, w, &format!("{path}[{i}]"), out);
                }
            } else {
                out.push(format!("{path}: {} items became {}", a.len(), b.len()));
            }
        }
        (orig, typed) if same_scalar(key, orig, typed) => {}
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

/// A value in a catch-all (`Other("…")` of an open enum, an `Unknown` or
/// `Invalid` message or change), or JSON kept untyped outside [`OPAQUE`],
/// in the `Debug` of a parsed payload.
fn typing_problems(debug: &str) -> Vec<String> {
    let mut out = Vec::new();
    for catch_all in ["Other(\"", "Unknown {", "Unknown(", "Invalid {"] {
        if debug.contains(catch_all) {
            out.push(format!("a value fell into `{catch_all}`"));
        }
    }
    for name in untyped_properties(debug) {
        if !OPAQUE.contains(&name.as_str()) {
            out.push(format!("`{name}` kept as JSON"));
        }
    }
    out
}

#[test]
fn no_value_falls_into_a_catch_all_or_stays_json() {
    for c in CASES {
        let debug = format!("{:?}", common::payload(c.fixture));
        let problems = typing_problems(&debug);
        assert!(problems.is_empty(), "{}: {problems:?}\n{debug}", c.fixture);
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
        // Only error reports go without a key (`WebhookEvent::dedup_key`);
        // counted from the variant, not from the key under test.
        let unkeyed = expected
            .iter()
            .filter(|e| matches!(e, WebhookEvent::ErrorReported { .. }))
            .count();
        assert_eq!(
            expected.iter().filter(|e| e.dedup_key().is_none()).count(),
            unkeyed,
            "{}: an event without a dedup key",
            c.fixture
        );
        assert_eq!(retry.delivered, unkeyed, "{}", c.fixture);
        assert_eq!(retry.duplicates, expected.len() - unkeyed, "{}", c.fixture);
    }
}

// The guards above are only as good as their failure paths: each check,
// fed what it must refuse, refuses it.

fn text_example() -> (Value, Value) {
    let c = case("messages/text.json");
    let orig: Value = serde_json::from_slice(&common::fixture_bytes(c.fixture)).unwrap();
    let typed = serde_json::to_value(common::payload(c.fixture)).unwrap();
    (orig, typed)
}

fn diff(orig: &Value, typed: &Value) -> Vec<String> {
    let mut out = Vec::new();
    compare(orig, typed, "", &mut out);
    out
}

#[test]
fn compare_catches_dropped_altered_retyped_and_invented_values() {
    let (orig, typed) = text_example();
    assert_eq!(diff(&orig, &typed), Vec::<String>::new());

    // An altered value.
    let mut altered = orig.clone();
    altered["entry"][0]["changes"][0]["value"]["messages"][0]["timestamp"] = "1".into();
    assert_eq!(diff(&altered, &typed).len(), 1);

    // A property the typed parse drops.
    let mut added = orig.clone();
    added["entry"][0]["changes"][0]["value"]["messages"][0]["brand_new"] = "x".into();
    let reparsed = serde_json::to_value(
        WebhookPayload::from_slice(&serde_json::to_vec(&added).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        diff(&added, &reparsed),
        [".entry[0].changes[0].value.messages[0].brand_new: dropped"]
    );

    // A list that changed length.
    let mut shorter = typed.clone();
    shorter["entry"][0]["changes"][0]["value"]["contacts"] = json!([]);
    assert_eq!(
        diff(&orig, &shorter),
        [".entry[0].changes[0].value.contacts: 1 items became 0"]
    );

    // A value the typed form invents.
    let mut invented = typed.clone();
    invented["entry"][0]["changes"][0]["value"]["messages"][0]["invented"] = "x".into();
    assert_eq!(
        diff(&orig, &invented),
        [".entry[0].changes[0].value.messages[0].invented: added \"x\""]
    );
    let mut empty = typed;
    empty["entry"][0]["changes"][0]["value"]["messages"][0]["nothing"] = json!({});
    assert_eq!(diff(&orig, &empty), Vec::<String>::new());

    // Types: only an id or unix time may go from number to string.
    for (orig, typed, same) in [
        (
            json!({"timestamp": 1_750_101_000}),
            json!({"timestamp": "1750101000"}),
            true,
        ),
        (json!({"waba_id": 102}), json!({"waba_id": "102"}), true),
        (json!({"time": 1}), json!({"time": "1"}), true),
        (json!({"amount": 25_000}), json!({"amount": 25_000.0}), true),
        (json!({"amount": 25_000}), json!({"amount": "25000"}), false),
        (json!({"timestamp": 1}), json!({"timestamp": "2"}), false),
        (json!({"id": "1"}), json!({"id": 1}), false),
        (
            json!({"billable": true}),
            json!({"billable": "true"}),
            false,
        ),
        (
            json!({"billable": "true"}),
            json!({"billable": true}),
            false,
        ),
        (
            json!({"status": "Failed"}),
            json!({"status": "FAILED"}),
            true,
        ),
        (
            json!({"event": "Failed"}),
            json!({"event": "FAILED"}),
            false,
        ),
        (
            json!({"status": "Failed"}),
            json!({"status": "COMPLETED"}),
            false,
        ),
    ] {
        assert_eq!(diff(&orig, &typed).is_empty(), same, "{orig} vs {typed}");
    }
}

#[test]
fn the_typing_check_catches_catch_alls_and_untyped_json() {
    let (orig, _) = text_example();
    // Catch-alls and untyped JSON.
    let mut other = orig.clone();
    other["entry"][0]["changes"][0]["value"]["messages"][0]["referral"] =
        json!({"source_type": "billboard"});
    let mut unknown = orig;
    unknown["entry"][0]["changes"][0]["value"]["messages"][0]["type"] = "hologram".into();
    for (body, want) in [(other, "Other(\""), (unknown, "Unknown {")] {
        let debug = format!(
            "{:?}",
            WebhookPayload::from_slice(&serde_json::to_vec(&body).unwrap()).unwrap()
        );
        let problems = typing_problems(&debug);
        assert!(
            problems.iter().any(|p| p.contains(want)),
            "{want}: {problems:?}"
        );
    }
    assert_eq!(
        typing_problems("X { a: 1, raw: Some(Object {}) }"),
        ["`raw` kept as JSON"]
    );
    assert!(typing_problems("X { message: Object {}, flow: Some(Object {}) }").is_empty());
}

#[test]
fn the_manifest_check_catches_both_directions_and_duplicates() {
    // The manifest against the files: both directions, and duplicates.
    let files: BTreeSet<String> = common::all_fixtures().into_iter().collect();
    let mut extra = files.clone();
    extra.insert("pages/not_a_fixture.json".to_owned());
    assert_eq!(
        manifest_problems(&extra, CASES),
        ["pages/not_a_fixture.json: fixture without a manifest case"]
    );
    let mut fewer = files.clone();
    fewer.remove(CASES[0].fixture);
    assert_eq!(
        manifest_problems(&fewer, CASES),
        [format!(
            "{}: manifest case without a fixture",
            CASES[0].fixture
        )]
    );
    let problems = manifest_problems(&files, CASES.iter().chain([&CASES[0]]));
    assert_eq!(problems, [format!("{}: listed twice", CASES[0].fixture)]);
}

#[test]
fn the_coverage_check_catches_uncovered_and_wrongly_cited_examples() {
    const PAGES_: &[Page] = &[
        Page {
            path: "p",
            status: Status::Typed,
            examples: &["A", "B"],
        },
        Page {
            path: "gone",
            status: Status::Unreadable("404"),
            examples: &[],
        },
    ];
    const ONLY_A: Case = Case {
        fixture: "a.json",
        sources: &[(Origin::Filled, "p", "A"), (Origin::Composed, "p", "B")],
        events: &[],
    };
    const B: Case = Case {
        fixture: "b.json",
        sources: &[(Origin::Verbatim, "p", "B")],
        events: &[],
    };
    const WRONG: Case = Case {
        fixture: "w.json",
        sources: &[
            (Origin::Filled, "p", "C"),
            (Origin::Composed, "gone", "x"),
            (Origin::Filled, "nowhere", "x"),
        ],
        events: &[],
    };
    assert!(coverage_problems(PAGES_, [&ONLY_A, &B]).is_empty());
    assert_eq!(
        coverage_problems(PAGES_, [&ONLY_A]),
        ["p: no fixture for [\"B\"]"]
    );
    assert_eq!(
        coverage_problems(PAGES_, [&ONLY_A, &B, &WRONG]),
        [
            "w.json: p has no example \"C\"",
            "w.json: cites unreadable gone",
            "w.json: cites nowhere, which PAGES does not list",
        ]
    );
}
