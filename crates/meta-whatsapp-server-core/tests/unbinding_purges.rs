//! Roadmap M5f3: a WABA is unbound through the `Authorizer` (which purges
//! its numbers' group records afterwards), never straight through the
//! `RecordStore`: a source scan of the core, so that a new unbinding path
//! that skips the purge fails here. The cases that the purge works are
//! `conformance::groups` (run by the backends' suites).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::path::Path;

fn sources(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            // The suites run the store's contract itself.
            if path.file_name().is_some_and(|n| n == "conformance") {
                continue;
            }
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push((
                path.display().to_string(),
                std::fs::read_to_string(&path).unwrap(),
            ));
        }
    }
}

/// Decisive: a call of the records' `unbind_waba` or `unbind_waba_if` in
/// the core outside `authz.rs` (the Authorizer's own two helpers).
#[test]
fn only_the_authorizer_unbinds_through_the_records() {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut found = Vec::new();
    for (file, source) in &files {
        if file.ends_with("/authz.rs") {
            continue;
        }
        for (at, _) in source.match_indices(".unbind_waba") {
            let before = &source[at.saturating_sub(40)..at];
            if before.contains("records") {
                found.push(format!("{file}: …{}", before.replace('\n', " ")));
            }
        }
    }
    assert!(
        found.is_empty(),
        "an unbinding that skips the group records' purge: {found:#?}"
    );
}
