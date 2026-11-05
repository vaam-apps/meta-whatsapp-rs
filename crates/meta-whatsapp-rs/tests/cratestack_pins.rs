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

use toml::{Table, Value};

/// The only source a `cratestack*` package may come from.
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// One `cratestack*` requirement found in a manifest.
#[derive(Debug, PartialEq, Eq)]
struct Pin {
    /// The manifest, relative to the workspace.
    manifest: String,
    /// The package's name on crates.io, whatever key the manifest renames it to.
    package: String,
    /// The requirement string, when there is one.
    version: Option<String>,
    /// Why the dependency does not come from crates.io at its requirement:
    /// a git or path source, another registry, a `[patch]` or `[replace]`.
    elsewhere: Option<String>,
}

fn is_cratestack(name: &str) -> bool {
    name.starts_with("cratestack")
}

/// A dependency entry (`name = "req"` or `name = { ... }`) under `key`, if it
/// names a `cratestack*` package. `workspace = true` entries take the root's.
fn pin_of(manifest: &str, key: &str, value: &Value, patched: Option<&str>) -> Option<Pin> {
    let table = value.as_table();
    let package = table
        .and_then(|t| t.get("package"))
        .and_then(Value::as_str)
        .unwrap_or(key);
    if !is_cratestack(package) && !is_cratestack(key) {
        return None;
    }
    let version = match value {
        Value::String(v) => Some(v.clone()),
        Value::Table(t) => t.get("version").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    };
    let elsewhere = patched.map(str::to_owned).or_else(|| {
        let t = table?;
        ["git", "path", "registry", "registry-index"]
            .into_iter()
            .find(|k| t.contains_key(*k))
            .map(|k| format!("a `{k}` source"))
    });
    // `name = { workspace = true }` takes the root's entry, which is read.
    if elsewhere.is_none() && table.is_some_and(|t| t.contains_key("workspace")) {
        return None;
    }
    Some(Pin {
        manifest: manifest.to_owned(),
        package: package.to_owned(),
        version,
        elsewhere,
    })
}

fn walk(manifest: &str, table: &Table, out: &mut Vec<Pin>) {
    for (key, value) in table {
        let Some(inner) = value.as_table() else {
            continue;
        };
        match key.as_str() {
            "dependencies" | "dev-dependencies" | "build-dependencies" => {
                for (name, dep) in inner {
                    out.extend(pin_of(manifest, name, dep, None));
                }
            }
            "patch" => {
                for (registry, entries) in inner {
                    for (name, dep) in entries.as_table().into_iter().flatten() {
                        out.extend(pin_of(
                            manifest,
                            name,
                            dep,
                            Some(&format!("a `[patch.{registry}]` entry")),
                        ));
                    }
                }
            }
            "replace" => {
                for name in inner.keys().filter(|n| is_cratestack(n)) {
                    out.push(Pin {
                        manifest: manifest.to_owned(),
                        package: name.clone(),
                        version: None,
                        elsewhere: Some("a `[replace]` entry".to_owned()),
                    });
                }
            }
            // `[workspace]`, `[target.'cfg(..)']`, and the rest.
            _ => walk(manifest, inner, out),
        }
    }
}

/// Every `cratestack*` dependency of `text` (a manifest), in any table of
/// any shape, `[patch]` and `[replace]` included.
fn pins(manifest: &str, text: &str) -> Vec<Pin> {
    let table: Table = text.parse().expect("a manifest that parses");
    let mut found = Vec::new();
    walk(manifest, &table, &mut found);
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
        if let Some(why) = &pin.elsewhere {
            problems.push(format!(
                "{}: {} comes from {why}, not crates.io at a pinned minor",
                pin.manifest, pin.package
            ));
        } else if let Some(version) = &pin.version {
            if !is_one_minor(version) {
                problems.push(format!(
                    "{}: {} = {version:?} is not pinned to one minor (use ~X.Y.Z)",
                    pin.manifest, pin.package
                ));
            }
        } else {
            problems.push(format!(
                "{}: {} has no version requirement",
                pin.manifest, pin.package
            ));
        }
    }
    let mut versions: BTreeMap<&str, Vec<&Pin>> = BTreeMap::new();
    for pin in pins {
        if let Some(version) = &pin.version {
            versions.entry(version.as_str()).or_default().push(pin);
        }
    }
    if versions.len() > 1 {
        let listing: Vec<String> = pins
            .iter()
            .map(|p| {
                format!(
                    "{} in {}: {}",
                    p.package,
                    p.manifest,
                    p.version.as_deref().unwrap_or("-")
                )
            })
            .collect();
        problems.push(format!(
            "the cratestack crates are pinned to different versions ({})",
            listing.join("; ")
        ));
    }
    problems
}

/// The (version, source) pairs a `Cargo.lock` resolves each `cratestack*`
/// package to.
fn locked(lock: &str) -> BTreeMap<String, Vec<(String, Option<String>)>> {
    let table: Table = lock.parse().expect("a Cargo.lock that parses");
    let mut out: BTreeMap<String, Vec<(String, Option<String>)>> = BTreeMap::new();
    for package in table
        .get("package")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let field = |k: &str| package.get(k).and_then(Value::as_str).map(str::to_owned);
        if let (Some(name), Some(version)) = (field("name"), field("version"))
            && is_cratestack(&name)
        {
            out.entry(name)
                .or_default()
                .push((version, field("source")));
        }
    }
    out
}

/// What is wrong with a lock's cratestack packages.
fn lock_problems(locked: &BTreeMap<String, Vec<(String, Option<String>)>>) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, entries) in locked {
        for (version, source) in entries {
            if source.as_deref() != Some(CRATES_IO) {
                problems.push(format!("{name} {version} is locked from {source:?}"));
            }
        }
    }
    let mut versions: Vec<&str> = locked.values().flatten().map(|(v, _)| v.as_str()).collect();
    versions.sort_unstable();
    versions.dedup();
    if versions.len() > 1 {
        problems.push(format!("locked versions differ: {locked:?}"));
    }
    problems
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every file named `name` under `dir`, skipping build output, vendored
/// code and the agent worktrees (which are copies of this repository).
fn find(dir: &Path, name: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if path.is_dir() {
            if !matches!(
                file_name.as_ref(),
                "target" | "node_modules" | ".git" | ".claude" | ".meta-docs"
            ) {
                find(&path, name, found);
            }
        } else if file_name == name {
            found.push(path);
        }
    }
}

/// Every `Cargo.toml` of the repository, the separate `.xtask` workspace
/// and the tools included.
fn manifests() -> Vec<PathBuf> {
    let mut found = Vec::new();
    find(&root(), "Cargo.toml", &mut found);
    found.sort();
    found
}

fn relative(path: &Path) -> String {
    path.strip_prefix(root())
        .map_or_else(|_| path.display().to_string(), |p| p.display().to_string())
}

fn workspace_pins() -> Vec<Pin> {
    let mut all = Vec::new();
    for manifest in manifests() {
        let text = std::fs::read_to_string(&manifest).expect("a manifest");
        all.extend(pins(&relative(&manifest), &text));
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
fn every_lock_resolves_the_cratestack_crates_to_one_version_from_crates_io() {
    let mut locks = Vec::new();
    find(&root(), "Cargo.lock", &mut locks);
    let mut seen = false;
    for path in locks {
        let lock = std::fs::read_to_string(&path).expect("a Cargo.lock");
        let locked = locked(&lock);
        seen |= !locked.is_empty();
        let problems = lock_problems(&locked);
        assert!(problems.is_empty(), "{}: {problems:#?}", relative(&path));
    }
    assert!(seen, "no Cargo.lock names a cratestack crate");
}

/// Whether a dependency table of `text` (any shape) names the package `name`.
fn depends_on(text: &str, name: &str) -> bool {
    fn scan(table: &Table, name: &str) -> bool {
        table.iter().any(|(key, value)| {
            let Some(inner) = value.as_table() else {
                return false;
            };
            if matches!(
                key.as_str(),
                "dependencies" | "dev-dependencies" | "build-dependencies"
            ) {
                inner.iter().any(|(dep, spec)| {
                    dep == name
                        || spec
                            .get("package")
                            .and_then(Value::as_str)
                            .is_some_and(|p| p == name)
                })
            } else {
                scan(inner, name)
            }
        })
    }
    scan(
        &text.parse::<Table>().expect("a manifest that parses"),
        name,
    )
}

#[test]
fn only_the_services_crates_may_depend_on_the_store_crate() {
    // The store calls its models as CrateStack's system principal, which
    // every policy of its schema admits (roadmap S11): the library, the
    // facade and the tools never link the crate that can make one.
    const STORE: &str = "meta-whatsapp-server-store-cratestack";
    let mut checked = 0;
    for manifest in manifests() {
        let relative = relative(&manifest);
        let text = std::fs::read_to_string(&manifest).expect("a manifest");
        let service = relative.starts_with("crates/meta-whatsapp-server");
        if !service {
            checked += 1;
            assert!(
                !depends_on(&text, STORE),
                "{relative} depends on {STORE}, the system principal's crate"
            );
        }
    }
    assert!(checked > 5, "{checked}");
    // The root's `[workspace.dependencies]` must not offer it either, and
    // the crate is no default member.
    let root = std::fs::read_to_string(root().join("Cargo.toml")).expect("the root manifest");
    let table: Table = root.parse().expect("the root manifest parses");
    let default_members = table["workspace"]
        .get("default-members")
        .and_then(Value::as_array)
        .expect("default-members");
    assert!(
        default_members
            .iter()
            .all(|m| m.as_str().is_some_and(|m| !m.contains("store-cratestack"))),
        "{default_members:?}"
    );
}

#[test]
fn the_dependency_scan_reads_every_shape() {
    assert!(!depends_on("[dependencies]\ny = \"1\"", "x"));
    for text in [
        "[dependencies]\nx = { path = \"p\" }",
        "[dev-dependencies]\nx = \"1\"",
        "[dependencies.x]\npath = \"p\"",
        "[workspace.dependencies]\nx = { path = \"p\" }",
        "[target.'cfg(unix)'.build-dependencies]\nrenamed = { package = \"x\", path = \"p\" }",
    ] {
        assert!(depends_on(text, "x"), "{text}");
    }
}

// The checker itself, against manifests that break each rule.

fn pin(manifest: &str, package: &str, version: &str) -> Pin {
    Pin {
        manifest: manifest.to_owned(),
        package: package.to_owned(),
        version: Some(version.to_owned()),
        elsewhere: None,
    }
}

fn problems_of(text: &str) -> Vec<String> {
    problems(&pins("Cargo.toml", text))
}

#[test]
fn every_pin_shape_is_read() {
    let text = r#"
[workspace.dependencies]
cratestack = { package = "cratestack-api", version = "~0.17.2", default-features = false }
cratestack-core = "~0.17.2"
serde = "1"

[dependencies.cratestack-pg]
version = "~0.17.2"

[target.'cfg(unix)'.dev-dependencies]
renamed = { package = "cratestack-sql", version = "~0.17.2" }

[dependencies]
cratestack-axum = { workspace = true }
"#;
    let mut found: Vec<String> = pins("Cargo.toml", text)
        .into_iter()
        .map(|p| format!("{} {:?} {:?}", p.package, p.version, p.elsewhere))
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            "cratestack-api Some(\"~0.17.2\") None",
            "cratestack-core Some(\"~0.17.2\") None",
            "cratestack-pg Some(\"~0.17.2\") None",
            "cratestack-sql Some(\"~0.17.2\") None",
        ]
    );
    assert!(problems_of(text).is_empty());
}

#[test]
fn a_requirement_that_is_not_one_minor_is_reported() {
    for bad in [
        "0.17",
        "^0.17.2",
        "0.17.2",
        "*",
        ">=0.17",
        "~0",
        "=0.17.2",
        "~0.17.x",
        ">=0.17, <0.18",
        "~0.17.2, ~0.17.3",
    ] {
        let found = problems(&[pin("Cargo.toml", "cratestack-api", bad)]);
        assert_eq!(found.len(), 1, "{bad}: {found:?}");
    }
    for good in ["~0.17", "~0.17.2"] {
        assert!(problems(&[pin("Cargo.toml", "cratestack-api", good)]).is_empty());
    }
}

#[test]
fn a_bad_requirement_is_reported_in_every_table_shape() {
    for text in [
        "[dependencies]\ncratestack = { package = \"cratestack-api\", version = \"0.17\" }",
        "[dependencies]\nrenamed = { package = \"cratestack-pg\", version = \"*\" }",
        "[dependencies.cratestack-pg]\nversion = \"=0.17.2\"",
        "[dev-dependencies]\ncratestack-sql = \"0.17\"",
        "[build-dependencies]\ncratestack-sql = \">=0.17\"",
        "[workspace.dependencies]\ncratestack-core = \"*\"",
        "[target.'cfg(unix)'.dependencies]\ncratestack-core = \"0.17\"",
    ] {
        assert_eq!(problems_of(text).len(), 1, "{text}");
    }
}

#[test]
fn a_dependency_from_anywhere_but_crates_io_is_reported() {
    for text in [
        "[dependencies]\ncratestack-api = { git = \"https://example.com/x\", version = \"~0.17.2\" }",
        "[dependencies]\ncratestack-api = { git = \"https://example.com/x\" }",
        "[dependencies]\ncratestack-api = { path = \"../x\" }",
        "[dependencies]\nrenamed = { package = \"cratestack-pg\", path = \"../x\" }",
        "[dependencies]\ncratestack-api = { version = \"~0.17.2\", registry = \"mine\" }",
        "[patch.crates-io]\ncratestack-api = { path = \"../x\" }",
        "[patch.crates-io]\ncratestack-api = { git = \"https://example.com/x\" }",
        "[patch.\"https://example.com/index\"]\ncratestack-api = { path = \"../x\" }",
        "[replace]\n\"cratestack-api:0.17.2\" = { path = \"../x\" }",
        "[dependencies]\ncratestack-api = {}",
    ] {
        assert_eq!(problems_of(text).len(), 1, "{text}");
    }
}

#[test]
fn an_unrelated_dependency_is_left_alone() {
    let text = "[dependencies]\nserde = \"1\"\nx = { path = \"../x\" }\n[patch.crates-io]\nfoo = { path = \"../foo\" }";
    assert!(pins("Cargo.toml", text).is_empty());
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
    // `~0.17` and `~0.17.2` are the same minor but not the same pin.
    let found = problems(&[
        pin("Cargo.toml", "cratestack-api", "~0.17"),
        pin("crates/a/Cargo.toml", "cratestack-pg", "~0.17.2"),
    ]);
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn the_manifest_walk_finds_the_stores_and_the_roots() {
    let found: Vec<String> = manifests().iter().map(|p| relative(p)).collect();
    for expected in [
        "Cargo.toml",
        "crates/meta-whatsapp-server-store-cratestack/Cargo.toml",
        "crates/meta-whatsapp-server-api-cratestack/Cargo.toml",
        ".xtask/Cargo.toml",
    ] {
        assert!(found.iter().any(|f| f == expected), "{expected}: {found:?}");
    }
}

fn lock_with(entries: &[(&str, &str, Option<&str>)]) -> String {
    use std::fmt::Write as _;
    let mut lock = String::new();
    for (name, version, source) in entries {
        let _ = writeln!(
            lock,
            "[[package]]\nname = \"{name}\"\nversion = \"{version}\""
        );
        if let Some(source) = source {
            let _ = writeln!(lock, "source = \"{source}\"");
        }
        lock.push('\n');
    }
    lock
}

#[test]
fn the_lock_reader_checks_version_and_source() {
    let lock = lock_with(&[
        ("cratestack-api", "0.17.2", Some(CRATES_IO)),
        ("cratestack-pg", "0.17.2", Some(CRATES_IO)),
        ("serde", "1.0.0", Some("git+https://example.com/serde")),
    ]);
    assert!(lock_problems(&locked(&lock)).is_empty());

    // A version that differs.
    let lock = lock_with(&[
        ("cratestack-api", "0.17.2", Some(CRATES_IO)),
        ("cratestack-pg", "0.17.3", Some(CRATES_IO)),
    ]);
    assert_eq!(lock_problems(&locked(&lock)).len(), 1);

    // A second registry or a git source, at the same version.
    for source in [
        Some("registry+https://example.com/index"),
        Some("git+https://example.com/cratestack"),
        None,
    ] {
        let lock = lock_with(&[
            ("cratestack-api", "0.17.2", Some(CRATES_IO)),
            ("cratestack-api", "0.17.2", source),
        ]);
        assert_eq!(lock_problems(&locked(&lock)).len(), 1, "{source:?}");
    }
}
