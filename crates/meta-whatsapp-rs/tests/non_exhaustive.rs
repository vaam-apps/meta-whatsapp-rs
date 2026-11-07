//! The `#[non_exhaustive]` rule (roadmap L20b, `OPEN_QUESTIONS` #28,
//! `docs/architecture.md` § The `#[non_exhaustive]` rule): Meta adds fields
//! and values, so naming one must not break an integrator.
//!
//! A scan of the source of `meta-whatsapp-client` and
//! `meta-whatsapp-webhooks` fails on:
//!
//! - a **response or webhook struct** without the attribute: a top-level
//!   `pub struct` with named `pub` fields that derives `Deserialize`
//!   (tuple structs and structs with private fields cannot be built by a
//!   literal anyway);
//! - an **open enum** without it: a top-level `pub enum` with a catch-all
//!   variant (`Other`, `Unknown`, `Unparsed`); the enums
//!   `meta_whatsapp_core::open_enum!` generates get the attribute from the
//!   macro, which the test checks.
//!
//! The exceptions are the request types, ones callers build and the
//! library sends, which a literal must keep building: [`EXEMPT_FILES`]
//! (a whole file) and [`EXEMPT_TYPES`] (single types), each with its
//! reason. Several derive `Deserialize` only to read a document back; a
//! type that is both sent and received stays open when callers have to
//! build it to send it. An entry that no longer matches a struct, or whose
//! struct carries the attribute, fails too, so the lists cannot rot.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Files (or directories, ending in `/`) of `meta-whatsapp-client/src`
/// whose types are all built by callers.
const EXEMPT_FILES: &[(&str, &str)] = &[
    (
        "flows/flow_json/",
        "the Flow JSON document callers write; `Deserialize` reads a stored document back",
    ),
    (
        "templates/definition.rs",
        "template definitions and components: built to create or edit a template, \
         and also what `TemplateInfo::components` reads",
    ),
    (
        "templates/send.rs",
        "template message parameters: built to send",
    ),
    (
        "templates/payments.rs",
        "payment template components: built to send",
    ),
    (
        "messages/payments.rs",
        "order details and payment message parameters: built to send",
    ),
    (
        "messages/payments/checkout.rs",
        "checkout message parameters: built to send",
    ),
];

/// Single types of `meta-whatsapp-client/src`, as `(file, type, reason)`.
const EXEMPT_TYPES: &[(&str, &str, &str)] = &[
    (
        "calling/recording.rs",
        "CallRecording",
        "the `recording` object of connect/accept: built to send",
    ),
    (
        "calling/recording.rs",
        "CallTranscription",
        "the `transcription` object of connect/accept: built to send",
    ),
    (
        "calling/settings.rs",
        "CallingSettings",
        "read by `get` and built, with `Default`, to update; so are the parts below",
    ),
    (
        "calling/settings.rs",
        "CallIcons",
        "part of CallingSettings",
    ),
    (
        "calling/settings.rs",
        "CallHours",
        "part of CallingSettings",
    ),
    (
        "calling/settings.rs",
        "WeeklyHours",
        "part of CallingSettings",
    ),
    (
        "calling/settings.rs",
        "HolidayHours",
        "part of CallingSettings",
    ),
    (
        "calling/settings.rs",
        "SipSettings",
        "part of CallingSettings",
    ),
    (
        "calling/settings.rs",
        "SipServer",
        "part of CallingSettings",
    ),
    (
        "calling/settings.rs",
        "AudioSettings",
        "part of CallingSettings",
    ),
    (
        "calling/settings.rs",
        "Voicemail",
        "part of CallingSettings",
    ),
    (
        "calling/settings.rs",
        "VoicemailAudio",
        "part of CallingSettings",
    ),
    (
        "calling/settings.rs",
        "VoicemailAudioConfig",
        "part of CallingSettings",
    ),
    (
        "flows/endpoint/mod.rs",
        "FlowResponse",
        "what the endpoint answers: built by the integrator",
    ),
    (
        "payments/checkout.rs",
        "CheckoutResponse",
        "what the checkout endpoint answers: built by the integrator",
    ),
    (
        "payments/checkout.rs",
        "Coupon",
        "part of CheckoutResponse: built by the integrator",
    ),
    (
        "templates/info.rs",
        "LibraryTemplateRequest",
        "request of a library template create",
    ),
    (
        "templates/info.rs",
        "LibraryButtonInput",
        "part of the request above",
    ),
    (
        "templates/info.rs",
        "LibraryUrlInput",
        "part of the request above",
    ),
    (
        "templates/info.rs",
        "LibraryBodyInputs",
        "part of the request above",
    ),
    (
        "templates/info.rs",
        "MigrationOptions",
        "request of a template migration",
    ),
    (
        "templates/groups.rs",
        "TemplateGroupCreate",
        "request: a group to create",
    ),
    (
        "templates/groups.rs",
        "TemplateGroupUpdate",
        "request: a group to update",
    ),
    (
        "marketing/welcome.rs",
        "WelcomeMessage",
        "built to create a sequence, read back by `WelcomeSequence::message`",
    ),
    (
        "marketing/welcome.rs",
        "AutofillMessage",
        "part of WelcomeMessage",
    ),
    (
        "marketing/welcome.rs",
        "IceBreaker",
        "part of WelcomeMessage",
    ),
];

/// A top-level type of one file.
struct Item {
    /// Path under the crate's `src`.
    file: String,
    name: String,
    /// The attribute lines above it.
    attrs: String,
    /// `true` for a struct, `false` for an enum.
    is_struct: bool,
    /// Body lines up to the closing brace at column 0.
    body: Vec<String>,
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The top-level `pub struct`/`pub enum` items of a crate's `src`, with
/// braces (`pub struct T(..);` and unit structs have none to build).
fn items(krate: &str) -> Vec<Item> {
    let src = repo().join("crates").join(krate).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    let mut found = Vec::new();
    for path in files {
        let file = path
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let (is_struct, rest) = if let Some(r) = line.strip_prefix("pub struct ") {
                (true, r)
            } else if let Some(r) = line.strip_prefix("pub enum ") {
                (false, r)
            } else {
                continue;
            };
            if !line.trim_end().ends_with('{') {
                continue;
            }
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let mut attrs = Vec::new();
            let mut j = i;
            while j > 0 {
                let above = lines[j - 1];
                if above.starts_with("#[") || above.starts_with("///") || above.starts_with("//") {
                    attrs.push(above);
                    j -= 1;
                } else {
                    break;
                }
            }
            let body = lines[i + 1..]
                .iter()
                .take_while(|l| **l != "}")
                .map(|l| (*l).to_owned())
                .collect();
            found.push(Item {
                file: file.clone(),
                name,
                attrs: attrs.join("\n"),
                is_struct,
                body,
            });
        }
    }
    found
}

impl Item {
    fn has_attribute(&self) -> bool {
        self.attrs.lines().any(|l| l.trim() == "#[non_exhaustive]")
    }

    fn derives_deserialize(&self) -> bool {
        self.attrs
            .lines()
            .any(|l| l.starts_with("#[derive(") && l.contains("Deserialize"))
    }

    fn has_pub_field(&self) -> bool {
        self.body.iter().any(|l| {
            l.strip_prefix("    pub ").is_some_and(|r| {
                r.split(':').next().is_some_and(|n| !n.contains(' ')) && r.contains(':')
            })
        })
    }

    /// An open enum: a catch-all variant at the top level of the body.
    fn has_catch_all(&self) -> bool {
        !self.is_struct
            && self.body.iter().any(|l| {
                ["Other", "Unknown", "Unparsed"].iter().any(|v| {
                    l.strip_prefix("    ").is_some_and(|r| {
                        r.strip_prefix(v)
                            .is_some_and(|after| after.starts_with(['(', ' ', ',', '{']))
                    })
                })
            })
    }

    /// Must carry the attribute, given the exemptions.
    fn is_response_like(&self, client: bool) -> bool {
        if self.has_catch_all() {
            return true;
        }
        if !self.is_struct || !self.derives_deserialize() || !self.has_pub_field() {
            return false;
        }
        if client && exempt(&self.file, &self.name) {
            return false;
        }
        true
    }

    fn label(&self, krate: &str) -> String {
        format!("{krate}/src/{}: {}", self.file, self.name)
    }
}

fn exempt(file: &str, name: &str) -> bool {
    EXEMPT_FILES.iter().any(|(p, _)| file.starts_with(p))
        || EXEMPT_TYPES
            .iter()
            .any(|(f, n, _)| *f == file && *n == name)
}

/// The decisive test: every response or webhook struct and every open enum
/// carries `#[non_exhaustive]`. Delete the attribute from one type and this
/// fails, naming it.
#[test]
fn every_response_webhook_struct_and_open_enum_is_non_exhaustive() {
    let mut missing = Vec::new();
    let mut checked = 0;
    for (krate, client) in [
        ("meta-whatsapp-client", true),
        ("meta-whatsapp-webhooks", false),
    ] {
        for item in items(krate) {
            if item.is_response_like(client) {
                checked += 1;
                if !item.has_attribute() {
                    missing.push(item.label(krate));
                }
            }
        }
    }
    assert!(
        checked > 250,
        "the scan met only {checked} types: it is blind"
    );
    assert!(
        missing.is_empty(),
        "these types need `#[non_exhaustive]` (docs/architecture.md § The \
         `#[non_exhaustive]` rule), or an entry with a reason in EXEMPT_*: {missing:#?}"
    );
}

/// An exemption names a request type that exists, derives `Deserialize`
/// with public fields (else it needs none) and does not carry the
/// attribute (else it needs no entry).
#[test]
fn the_exemptions_are_live() {
    let all = items("meta-whatsapp-client");
    let mut dead = Vec::new();
    for (file, name, reason) in EXEMPT_TYPES {
        assert!(!reason.is_empty());
        match all.iter().find(|i| i.file == *file && i.name == *name) {
            Some(i) if i.derives_deserialize() && i.has_pub_field() && !i.has_attribute() => {}
            _ => dead.push(format!("{file}: {name}")),
        }
    }
    for (prefix, reason) in EXEMPT_FILES {
        assert!(!reason.is_empty());
        let hit = all
            .iter()
            .any(|i| i.file.starts_with(prefix) && i.derives_deserialize() && i.has_pub_field());
        if !hit {
            dead.push((*prefix).to_owned());
        }
        for i in all.iter().filter(|i| i.file.starts_with(prefix)) {
            if i.has_attribute() && i.is_struct {
                dead.push(format!("{}: {} (carries the attribute)", i.file, i.name));
            }
        }
    }
    assert!(dead.is_empty(), "stale exemptions: {dead:#?}");
    let unique: BTreeSet<_> = EXEMPT_TYPES.iter().map(|(f, n, _)| (f, n)).collect();
    assert_eq!(unique.len(), EXEMPT_TYPES.len(), "a duplicate exemption");
}

/// The enums `open_enum!` generates carry the attribute because the macro
/// writes it.
#[test]
fn the_open_enum_macro_adds_the_attribute() {
    let macro_src =
        fs::read_to_string(repo().join("crates/meta-whatsapp-core/src/open_enum.rs")).unwrap();
    let at = macro_src
        .find("#[derive(Debug, Clone, PartialEq, Eq, Hash)]\n        #[non_exhaustive]\n        $vis enum $name")
        .is_some();
    assert!(
        at,
        "open_enum! no longer puts #[non_exhaustive] on its enums"
    );
}
