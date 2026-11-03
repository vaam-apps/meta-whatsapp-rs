//! The BlueOak-1.0.0 licence stays scoped to the two crates the owner
//! allowed it for (design D20 (a), 2026-11-02; roadmap S10): `minicbor`
//! and `minicbor-serde`, as `[[licenses.exceptions]]` entries of the
//! workspace's `deny.toml`. `just deny` fails when an exception is
//! removed, but passes when the licence is widened (moved into `allow`,
//! or excepted for another crate): this test fails then.

use std::collections::BTreeSet;

/// The licence, as `deny.toml` names it.
const LICENCE: &str = "BlueOak-1.0.0";

/// `deny.toml`'s tables: each header with its lines, comments dropped.
fn tables(text: &str) -> Vec<(String, Vec<String>)> {
    let mut tables: Vec<(String, Vec<String>)> = vec![(String::new(), Vec::new())];
    for line in text.lines() {
        let line = line.split_once('#').map_or(line, |(code, _)| code).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            tables.push((line.to_owned(), Vec::new()));
        } else if let Some((_, lines)) = tables.last_mut() {
            lines.push(line.to_owned());
        }
    }
    tables
}

/// A `key = "value"` line's value.
fn string_value<'a>(lines: &'a [String], key: &str) -> Option<&'a str> {
    lines.iter().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"'))
    })
}

#[test]
fn blue_oak_is_excepted_for_minicbor_and_minicbor_serde_only() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deny.toml");
    let text = std::fs::read_to_string(path).expect("the workspace's deny.toml");
    let mut excepted = BTreeSet::new();
    for (header, lines) in tables(&text) {
        let body = lines.join("\n");
        if !body.contains(LICENCE) {
            continue;
        }
        assert_eq!(
            header, "[[licenses.exceptions]]",
            "{LICENCE} outside a scoped exception, in {header:?}"
        );
        let krate = string_value(&lines, "crate").expect("an exception names its crate");
        assert_eq!(
            string_value(&lines, "allow"),
            Some(r#"["BlueOak-1.0.0"]"#),
            "the exception for {krate} allows more than {LICENCE}"
        );
        excepted.insert(krate.to_owned());
    }
    assert_eq!(
        excepted,
        BTreeSet::from(["minicbor".to_owned(), "minicbor-serde".to_owned()])
    );
}
