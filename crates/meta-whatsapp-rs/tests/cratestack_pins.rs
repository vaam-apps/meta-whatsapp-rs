//! Roadmap S12: the `cratestack*` crates stay on one minor and one version.
//!
//! CrateStack's crates release together and require their siblings at the
//! same release, and a minor of 0.x may break the generated code, so an
//! upgrade is one pull request through the review pipeline, never a drift.
//! This test reads every manifest in the workspace (the root's
//! `[workspace.dependencies]` and each member's own tables) and `Cargo.lock`,
//! and fails when a `cratestack*` requirement is not pinned to one minor
//! (`~0.17.2`, not `0.17`, `^0.17`, `*` or `>=0.17`), when two of them are
//! pinned to different requirements, or when the lock resolves them to
//! different versions. A weekly job (`.github/workflows/cratestack-release.yml`)
//! opens the issue that starts an upgrade.

#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One `cratestack*` requirement found in a manifest.
#[derive(Debug, PartialEq, Eq)]
struct Pin {
    /// The manifest, relative to the workspace.
    manifest: String,
    /// The package's name on crates.io.
    package: String,
    /// The requirement string.
    version: String,
}

/// The value of `key = "..."` inside an inline table or a line.
fn quoted_after(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("{key} = \""))? + key.len() + 4;
    let rest = &text[at..];
    Some(rest[..rest.find('"')?].to_owned())
}

/// Every `cratestack*` dependency line of `text` (a manifest): `name = "req"`
/// or `name = { package = "cratestack-x", version = "req", ... }`.
fn pins(manifest: &str, text: &str) -> Vec<Pin> {
    let mut found = Vec::new();
    for line in text.lines() {
        let line = line.split_once('#').map_or(line, |(code, _)| code).trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let package = quoted_after(value, "package").unwrap_or_else(|| key.to_owned());
        if !package.starts_with("cratestack") {
            continue;
        }
        let version = if value.starts_with('"') {
            Some(value.trim_matches('"').to_owned())
        } else {
            quoted_after(value, "version")
        };
        // A path dependency (a member named cratestack*) has no requirement.
        if let Some(version) = version {
            found.push(Pin {
                manifest: manifest.to_owned(),
                package,
                version,
            });
        }
    }
    found
}

/// `~X.Y` or `~X.Y.Z`: a tilde requirement naming a minor.
fn is_one_minor(requirement: &str) -> bool {
    let Some(rest) = requirement.strip_prefix('~') else {
        return false;
    };
    let parts: Vec<&str> = rest.split('.').collect();
    (parts.len() == 2 || parts.len() == 3)
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// What is wrong with `pins`, one sentence each.
fn problems(pins: &[Pin]) -> Vec<String> {
    let mut problems = Vec::new();
    for pin in pins {
        if !is_one_minor(&pin.version) {
            problems.push(format!(
                "{}: {} = {:?} is not pinned to one minor (use ~X.Y.Z)",
                pin.manifest, pin.package, pin.version
            ));
        }
    }
    let mut versions: BTreeMap<&str, Vec<&Pin>> = BTreeMap::new();
    for pin in pins {
        versions.entry(pin.version.as_str()).or_default().push(pin);
    }
    if versions.len() > 1 {
        let listing: Vec<String> = pins
            .iter()
            .map(|p| format!("{} in {}: {}", p.package, p.manifest, p.version))
            .collect();
        problems.push(format!(
            "the cratestack crates are pinned to different versions ({})",
            listing.join("; ")
        ));
    }
    problems
}

/// The versions `Cargo.lock` resolves each `cratestack*` package to.
fn locked(lock: &str) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut name: Option<String> = None;
    for line in lock.lines() {
        if let Some(n) = line.strip_prefix("name = \"") {
            name = n.strip_suffix('"').map(str::to_owned);
        } else if let Some(v) = line.strip_prefix("version = \"")
            && let (Some(n), Some(v)) = (&name, v.strip_suffix('"'))
            && n.starts_with("cratestack")
        {
            out.entry(n.clone()).or_default().push(v.to_owned());
        }
    }
    out
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The root manifest and every member's under `crates/` and `tools/`.
fn manifests() -> Vec<PathBuf> {
    let mut found = vec![root().join("Cargo.toml")];
    for dir in ["crates", "tools"] {
        let Ok(entries) = std::fs::read_dir(root().join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let manifest = entry.path().join("Cargo.toml");
            if manifest.is_file() {
                found.push(manifest);
            }
        }
    }
    found.sort();
    found
}

fn workspace_pins() -> Vec<Pin> {
    let mut all = Vec::new();
    for manifest in manifests() {
        let text = std::fs::read_to_string(&manifest).expect("a manifest");
        let relative = manifest.strip_prefix(root()).map_or_else(
            |_| manifest.display().to_string(),
            |p| p.display().to_string(),
        );
        all.extend(pins(&relative, &text));
    }
    all
}

#[test]
fn every_cratestack_requirement_is_one_minor_and_one_version() {
    let pins = workspace_pins();
    assert!(
        pins.len() >= 2,
        "the workspace manifest and the store crate pin CrateStack: {pins:?}"
    );
    let problems = problems(&pins);
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_lock_resolves_every_cratestack_crate_to_one_version() {
    let lock = std::fs::read_to_string(root().join("Cargo.lock")).expect("Cargo.lock");
    let locked = locked(&lock);
    assert!(!locked.is_empty(), "Cargo.lock names no cratestack crate");
    let mut versions: Vec<&str> = locked.values().flatten().map(String::as_str).collect();
    versions.sort_unstable();
    versions.dedup();
    assert_eq!(versions.len(), 1, "locked versions differ: {locked:?}");
}

// The checker itself, against manifests that break each rule.

fn pin(manifest: &str, package: &str, version: &str) -> Pin {
    Pin {
        manifest: manifest.to_owned(),
        package: package.to_owned(),
        version: version.to_owned(),
    }
}

#[test]
fn both_pin_shapes_are_read() {
    let text = r#"
[workspace.dependencies]
cratestack = { package = "cratestack-api", version = "~0.17.2", default-features = false }
cratestack-core = "~0.17.2"
cratestack-local = { path = "x" }
serde = "1"
"#;
    assert_eq!(
        pins("Cargo.toml", text),
        vec![
            pin("Cargo.toml", "cratestack-api", "~0.17.2"),
            pin("Cargo.toml", "cratestack-core", "~0.17.2"),
        ]
    );
}

#[test]
fn a_requirement_that_is_not_one_minor_is_reported() {
    for bad in [
        "0.17", "^0.17.2", "0.17.2", "*", ">=0.17", "~0", "=0.17.2", "~0.17.x",
    ] {
        let found = problems(&[pin("Cargo.toml", "cratestack-api", bad)]);
        assert_eq!(found.len(), 1, "{bad}: {found:?}");
    }
    for good in ["~0.17", "~0.17.2"] {
        assert!(problems(&[pin("Cargo.toml", "cratestack-api", good)]).is_empty());
    }
}

#[test]
fn two_crates_on_different_versions_are_reported() {
    let found = problems(&[
        pin("Cargo.toml", "cratestack-api", "~0.17.2"),
        pin("crates/a/Cargo.toml", "cratestack-pg", "~0.17.3"),
    ]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("different versions"));
    // Another minor is reported too, as both a bad pin of none and a drift.
    let found = problems(&[
        pin("Cargo.toml", "cratestack-api", "~0.17.2"),
        pin("crates/a/Cargo.toml", "cratestack-pg", "~0.18.0"),
    ]);
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn the_lock_reader_names_each_version() {
    let lock = "[[package]]\nname = \"cratestack-api\"\nversion = \"0.17.2\"\n\n[[package]]\nname = \"cratestack-pg\"\nversion = \"0.17.3\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n";
    let locked = locked(lock);
    assert_eq!(locked.len(), 2);
    assert_eq!(locked["cratestack-pg"], vec!["0.17.3".to_owned()]);
}
