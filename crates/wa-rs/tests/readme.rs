//! The README's Rust snippets are excerpts of the examples, not prose that
//! can drift: every paragraph (run of non-blank lines) of every ```` ```rust ````
//! block must appear, line for line with indentation ignored, in one example
//! file, and all paragraphs of a block in the same file. The examples are
//! compiled by `just check` and `just lint`, and `cms_inbox` and
//! `embedded_signup` run under test, so a snippet that passes here is code
//! that builds and, for those two, works.
//!
//! Every example file the README links to must exist, too.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Trimmed, non-blank lines.
fn lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
}

/// The ```` ```rust ```` blocks of `markdown`, each as its paragraphs of
/// trimmed lines.
fn rust_blocks(markdown: &str) -> Vec<Vec<Vec<&str>>> {
    let mut blocks = Vec::new();
    let mut block: Option<Vec<Vec<&str>>> = None;
    let mut paragraph = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim();
        match &mut block {
            None if trimmed == "```rust" => block = Some(Vec::new()),
            None => {}
            Some(current) if trimmed.starts_with("```") => {
                if !paragraph.is_empty() {
                    current.push(std::mem::take(&mut paragraph));
                }
                blocks.push(block.take().unwrap());
            }
            Some(current) if trimmed.is_empty() => {
                if !paragraph.is_empty() {
                    current.push(std::mem::take(&mut paragraph));
                }
            }
            Some(_) => paragraph.push(trimmed),
        }
    }
    assert!(block.is_none(), "unterminated ```rust block in the README");
    blocks
}

fn contains(haystack: &[&str], needle: &[&str]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn examples() -> Vec<(PathBuf, String)> {
    let mut examples: Vec<_> = fs::read_dir(crate_dir().join("examples"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "rs"))
        .map(|path| {
            let source = fs::read_to_string(&path).unwrap();
            (path, source)
        })
        .collect();
    examples.sort();
    examples
}

fn readme() -> String {
    fs::read_to_string(crate_dir().join("../../README.md")).unwrap()
}

#[test]
fn readme_snippets_are_excerpts_of_the_examples() {
    let readme = readme();
    let examples = examples();
    assert_eq!(examples.len(), 5, "an example was added or removed");
    let blocks = rust_blocks(&readme);
    // send, signup start, signup complete, inbox wiring, inbox reply, OTP.
    assert_eq!(blocks.len(), 6, "README Rust snippets");
    for block in &blocks {
        let home = examples.iter().find(|(_, source)| {
            let source = lines(source);
            block.iter().all(|paragraph| contains(&source, paragraph))
        });
        assert!(
            home.is_some(),
            "this README snippet is not an excerpt of any example (update the \
             README from the example, not the other way round):\n{}",
            block
                .iter()
                .map(|p| p.join("\n"))
                .collect::<Vec<_>>()
                .join("\n\n")
        );
    }
}

#[test]
fn examples_linked_from_the_readme_exist() {
    let readme = readme();
    let linked: Vec<&str> = readme
        .split("](crates/wa-rs/examples/")
        .skip(1)
        .filter_map(|rest| rest.split(')').next())
        .filter(|name| !name.is_empty())
        .collect();
    assert!(linked.len() >= 5, "{linked:?}");
    for name in linked {
        assert!(
            crate_dir().join("examples").join(name).is_file(),
            "README links to a missing example: {name}"
        );
    }
}
