//! The `#[non_exhaustive]` rule (roadmap L20b, `OPEN_QUESTIONS` #28,
//! `docs/architecture.md` § The `#[non_exhaustive]` rule): Meta adds fields
//! and values, so naming one must not break an integrator.
//!
//! A scan of the source of `meta-whatsapp-client` and
//! `meta-whatsapp-webhooks` fails on:
//!
//! - a **response or webhook struct** without the attribute: a `pub struct`
//!   (parsed with `syn`, in inline modules too) with named `pub` fields that derives `Deserialize`
//!   (tuple structs and structs with private fields cannot be built by a
//!   literal anyway);
//! - an **open enum** without it: a `pub enum` with a catch-all variant
//!   (named `Other*`, `Unknown*`, `Unparsed*`, or `#[serde(other)]`); the enums
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

use quote::ToTokens;

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

/// A `pub` struct or enum of one file, however nested in inline modules.
struct Item {
    /// Path under the crate's `src`.
    file: String,
    name: String,
    kind: Kind,
    non_exhaustive: bool,
    deserialize: bool,
    /// What makes it matter: for a struct, a named `pub` field (a literal
    /// can build it); for an enum, a catch-all variant.
    shaped: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Struct,
    Enum,
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

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && a.parse_args::<syn::Ident>().is_ok_and(|i| i == "test")
    })
}

/// A derive of `Deserialize` however written: split over lines, by path
/// (`serde::Deserialize`) or under `cfg_attr`.
fn derives_deserialize(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        (a.path().is_ident("derive") || a.path().is_ident("cfg_attr"))
            && a.to_token_stream().to_string().contains("Deserialize")
    })
}

fn has_non_exhaustive(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("non_exhaustive")
            || (a.path().is_ident("cfg_attr")
                && a.to_token_stream().to_string().contains("non_exhaustive"))
    })
}

/// A variant that takes whatever Meta adds: named `Other`, `Unknown` or
/// `Unparsed` (any suffix), or marked `#[serde(other)]`.
fn is_catch_all(v: &syn::Variant) -> bool {
    let name = v.ident.to_string();
    ["Other", "Unknown", "Unparsed"]
        .iter()
        .any(|p| name.starts_with(p))
        || v.attrs.iter().any(|a| {
            a.path().is_ident("serde") && a.to_token_stream().to_string().contains("other")
        })
}

/// Types with a hand-written `impl Deserialize<'de> for T` in these items.
fn manual_deserialize(items: &[syn::Item]) -> BTreeSet<String> {
    items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Impl(i) => {
                let (_, trait_path, _) = i.trait_.as_ref()?;
                if trait_path.segments.last()?.ident != "Deserialize" {
                    return None;
                }
                match &*i.self_ty {
                    syn::Type::Path(p) => Some(p.path.segments.last()?.ident.to_string()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

fn collect(file: &str, items: &[syn::Item], found: &mut Vec<Item>) {
    let manual = manual_deserialize(items);
    for item in items {
        match item {
            syn::Item::Struct(s) if matches!(s.vis, syn::Visibility::Public(_)) => {
                let pub_field = match &s.fields {
                    syn::Fields::Named(f) => f
                        .named
                        .iter()
                        .any(|f| matches!(f.vis, syn::Visibility::Public(_))),
                    _ => false,
                };
                found.push(Item {
                    file: file.to_owned(),
                    name: s.ident.to_string(),
                    kind: Kind::Struct,
                    non_exhaustive: has_non_exhaustive(&s.attrs),
                    deserialize: derives_deserialize(&s.attrs)
                        || manual.contains(&s.ident.to_string()),
                    shaped: pub_field,
                });
            }
            syn::Item::Enum(e) if matches!(e.vis, syn::Visibility::Public(_)) => {
                found.push(Item {
                    file: file.to_owned(),
                    name: e.ident.to_string(),
                    kind: Kind::Enum,
                    non_exhaustive: has_non_exhaustive(&e.attrs),
                    deserialize: derives_deserialize(&e.attrs)
                        || manual.contains(&e.ident.to_string()),
                    shaped: e.variants.iter().any(is_catch_all),
                });
            }
            syn::Item::Mod(m) if !is_cfg_test(&m.attrs) => {
                if let Some((_, inner)) = &m.content {
                    collect(file, inner, found);
                }
            }
            _ => {}
        }
    }
}

/// The `pub` structs and enums of a crate's `src` (not `pub(crate)`, which
/// no integrator names; not `#[cfg(test)]` modules; the enums
/// `open_enum!` expands are written by the macro, checked below).
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
        let parsed = syn::parse_file(&text).unwrap_or_else(|e| panic!("{file}: {e}"));
        collect(&file, &parsed.items, &mut found);
    }
    found
}

impl Item {
    fn has_attribute(&self) -> bool {
        self.non_exhaustive
    }

    fn derives_deserialize(&self) -> bool {
        self.deserialize
    }

    fn has_pub_field(&self) -> bool {
        self.kind == Kind::Struct && self.shaped
    }

    /// Must carry the attribute, given the exemptions.
    fn is_response_like(&self, client: bool) -> bool {
        if self.kind == Kind::Enum {
            return self.shaped;
        }
        if !self.derives_deserialize() || !self.has_pub_field() {
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
            if i.has_attribute() && i.kind == Kind::Struct {
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

/// The scan is not fooled by how a type is written: a derive split over
/// lines, by path or under `cfg_attr`, an inline module, a generic struct,
/// a catch-all of another name; `pub(crate)` and test modules are not API.
#[test]
fn the_scan_sees_every_way_to_write_a_type() {
    let src = r#"
        #[derive(
            Debug,
            Deserialize,
        )]
        pub struct Split { pub a: u8 }

        #[derive(serde::Deserialize)]
        pub struct ByPath { pub a: u8 }

        #[cfg_attr(feature = "x", derive(Deserialize))]
        pub struct Gated { pub a: u8 }

        /// Docs.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        #[non_exhaustive]
        pub struct Marked { pub a: u8 }

        pub mod inner {
            #[derive(Deserialize)]
            pub struct Nested { pub a: u8 }
        }

        #[derive(Deserialize)]
        pub struct Generic<T> { pub a: T }

        #[derive(Deserialize)]
        pub(crate) struct Hidden { pub a: u8 }

        pub struct Manual { pub a: u8 }
        impl<'de> serde::Deserialize<'de> for Manual {
            fn deserialize<D>(d: D) -> Result<Self, D::Error> { todo!() }
        }

        #[derive(Deserialize)]
        pub struct Private { a: u8 }

        pub enum Fallback { A, #[serde(other)] Rest }
        pub enum Named { A, OtherThing(String) }
        pub enum Closed { A, B }

        #[cfg(test)]
        mod tests {
            #[derive(Deserialize)]
            pub struct InTest { pub a: u8 }
        }

        #[cfg(feature = "testing")]
        pub mod helpers {
            #[derive(Deserialize)]
            pub struct Testing { pub a: u8 }
        }
    "#;
    let mut found = Vec::new();
    collect("x.rs", &syn::parse_file(src).unwrap().items, &mut found);
    let like: BTreeSet<_> = found
        .iter()
        .filter(|i| i.is_response_like(false))
        .map(|i| (i.name.as_str(), i.has_attribute()))
        .collect();
    let want: BTreeSet<_> = [
        ("Split", false),
        ("ByPath", false),
        ("Gated", false),
        ("Marked", true),
        ("Nested", false),
        ("Generic", false),
        ("Manual", false),
        ("Testing", false),
        ("Fallback", false),
        ("Named", false),
    ]
    .into_iter()
    .collect();
    assert_eq!(like, want);
}

/// Library types the service maps field by field to its API views
/// (`From<..>` in `meta-whatsapp-server-core` and
/// `meta-whatsapp-server-api-axum`), with every public field they have.
/// Before `#[non_exhaustive]` those impls destructured the type
/// exhaustively, so a field Meta added broke the build until someone chose
/// to map it or to leave it out. A destructure now ends in `..` and no
/// longer fails, so this list does: a new field fails it, and the person
/// adding the field decides in the `From` impl, then lists it here.
const SERVICE_MAPPED: &[(&str, &str, &[&str])] = &[
    (
        "flows/types.rs",
        "FlowValidationError",
        &[
            "error",
            "error_type",
            "message",
            "line_start",
            "line_end",
            "column_start",
            "column_end",
            "pointers",
        ],
    ),
    (
        "flows/types.rs",
        "FlowDetails",
        &[
            "id",
            "name",
            "status",
            "categories",
            "validation_errors",
            "json_version",
            "data_api_version",
            "endpoint_uri",
            "preview",
            "whatsapp_business_account",
            "application",
        ],
    ),
    (
        "templates/info.rs",
        "ComparisonMetric",
        &[
            "metric",
            "kind",
            "order_by_relative_metric",
            "number_values",
            "string_values",
        ],
    ),
    (
        "templates/info.rs",
        "LibraryTemplate",
        &[
            "id",
            "name",
            "language",
            "category",
            "topic",
            "usecase",
            "industry",
            "header",
            "body",
            "body_params",
            "body_param_types",
            "buttons",
        ],
    ),
    (
        "templates/info.rs",
        "MigrationResult",
        &["migrated_templates", "failed_templates"],
    ),
    (
        "templates/groups.rs",
        "ArchiveResult",
        &["archived_templates", "failed_templates"],
    ),
    (
        "templates/groups.rs",
        "UnarchiveResult",
        &["unarchived_templates", "failed_templates"],
    ),
    (
        "templates/groups.rs",
        "TemplateGroup",
        &[
            "id",
            "name",
            "description",
            "creation_time",
            "update_time",
            "whatsapp_business_account",
            "whatsapp_business_templates",
        ],
    ),
    (
        "commerce/mod.rs",
        "CommerceSettings",
        &["id", "is_cart_enabled", "is_catalog_visible"],
    ),
    (
        "qr_codes/mod.rs",
        "QrCode",
        &[
            "code",
            "prefilled_message",
            "deep_link_url",
            "qr_image_url",
            "creation_time",
        ],
    ),
];

fn pub_fields(items: &[syn::Item], name: &str, out: &mut Option<Vec<String>>) {
    for item in items {
        match item {
            syn::Item::Struct(s) if s.ident == name => {
                if let syn::Fields::Named(f) = &s.fields {
                    *out = Some(
                        f.named
                            .iter()
                            .filter(|f| matches!(f.vis, syn::Visibility::Public(_)))
                            .filter_map(|f| f.ident.as_ref().map(ToString::to_string))
                            .collect(),
                    );
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    pub_fields(inner, name, out);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn the_service_mappings_see_every_field_of_the_library_types() {
    let src = repo().join("crates/meta-whatsapp-client/src");
    let mut drift = Vec::new();
    for (file, name, listed) in SERVICE_MAPPED {
        let text = fs::read_to_string(src.join(file)).unwrap();
        let parsed = syn::parse_file(&text).unwrap();
        let mut actual = None;
        pub_fields(&parsed.items, name, &mut actual);
        let actual = actual.unwrap_or_else(|| panic!("{file}: no struct {name}"));
        if actual != *listed {
            drift.push(format!("{file}: {name}: has {actual:?}, listed {listed:?}"));
        }
    }
    assert!(
        drift.is_empty(),
        "a library type the service maps field by field changed: map the new field in its \
         `From` impl (meta-whatsapp-server-core, meta-whatsapp-server-api-axum), or leave it \
         out on purpose, then update SERVICE_MAPPED: {drift:#?}"
    );
}
