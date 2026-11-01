//! `docs/public/llms.txt` (roadmap L22a): the docs' index for language
//! models, in the llms.txt convention (a `#` title, a `>` summary, `##`
//! sections of `- [name](url): description` links), served at the docs
//! site's root (`VitePress` copies `docs/public/` as it is). It links only
//! files that exist: each link is the Markdown source of a file in this
//! repository, on `main`. And it does not drift from what it indexes:
//! every integrator guide and every consumer skill is listed.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Where every link points: the raw file on `main`.
const RAW: &str = "https://raw.githubusercontent.com/vaam-apps/meta-whatsapp-rs/main/";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn llms_txt() -> String {
    fs::read_to_string(repo().join("docs/public/llms.txt")).unwrap()
}

/// `(line, name, url, description)` of each `- [name](url): description`.
fn links(text: &str) -> Vec<(usize, String, String, String)> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let Some(item) = line.strip_prefix("- [") else {
            continue;
        };
        let (name, rest) = item
            .split_once("](")
            .unwrap_or_else(|| panic!("llms.txt:{}: not `- [name](url)`", i + 1));
        let (url, rest) = rest
            .split_once(')')
            .unwrap_or_else(|| panic!("llms.txt:{}: no closing `)`", i + 1));
        let description = rest.strip_prefix(": ").unwrap_or_default();
        out.push((
            i + 1,
            name.to_owned(),
            url.to_owned(),
            description.to_owned(),
        ));
    }
    out
}

/// The repository paths the links name.
fn linked_paths() -> BTreeSet<String> {
    links(&llms_txt())
        .into_iter()
        .filter_map(|(_, _, url, _)| url.strip_prefix(RAW).map(str::to_owned))
        .collect()
}

#[test]
fn llms_txt_has_the_conventions_shape() {
    let text = llms_txt();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.first().copied(), Some("# meta-whatsapp-rs"));
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("# ")).count(),
        1,
        "one title"
    );
    let summary = lines
        .iter()
        .position(|l| l.starts_with("> "))
        .expect("a `>` summary");
    let first_section = lines
        .iter()
        .position(|l| l.starts_with("## "))
        .expect("a `##` section");
    assert!(
        summary < first_section,
        "the summary comes before the sections"
    );
    // Every list item is a link with a one-line description.
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("- ") {
            assert!(line.starts_with("- ["), "llms.txt:{}: not a link", i + 1);
        }
    }
    let items = links(&text);
    assert!(items.len() > 20, "{} links", items.len());
    for (line, name, _, description) in &items {
        assert!(!name.trim().is_empty(), "llms.txt:{line}: no name");
        assert!(
            !description.trim().is_empty(),
            "llms.txt:{line}: `{name}` has no description"
        );
    }
}

/// What is wrong with the links of `text`, against the files under `root`.
fn link_problems(text: &str, root: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for (line, name, url, _) in links(text) {
        let Some(path) = url.strip_prefix(RAW) else {
            problems.push(format!(
                "llms.txt:{line}: `{name}` links {url}, not {RAW}<path>"
            ));
            continue;
        };
        if path.is_empty()
            || path.contains("..")
            || path.contains('#')
            || path.contains('?')
            || path.starts_with('/')
        {
            problems.push(format!(
                "llms.txt:{line}: `{path}` is not a plain repository path"
            ));
            continue;
        }
        if !root.join(path).is_file() {
            problems.push(format!("llms.txt:{line}: `{path}` does not exist"));
        }
        if !seen.insert(path.to_owned()) {
            problems.push(format!("llms.txt:{line}: `{path}` is linked twice"));
        }
    }
    problems
}

#[test]
fn llms_txt_links_only_files_that_exist() {
    let problems = link_problems(&llms_txt(), &repo());
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn llms_txt_lists_every_guide_and_every_skill() {
    let root = repo();
    let linked = linked_paths();
    let mut missing = Vec::new();
    for entry in fs::read_dir(root.join("docs/guides")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "md") {
            let name = format!(
                "docs/guides/{}",
                path.file_name().unwrap().to_string_lossy()
            );
            if !linked.contains(&name) {
                missing.push(name);
            }
        }
    }
    for entry in fs::read_dir(root.join("skills")).unwrap() {
        let path = entry.unwrap().path();
        if path.join("SKILL.md").is_file() {
            let name = format!(
                "skills/{}/SKILL.md",
                path.file_name().unwrap().to_string_lossy()
            );
            if !linked.contains(&name) {
                missing.push(name);
            }
        }
    }
    missing.sort();
    assert!(missing.is_empty(), "not in llms.txt: {missing:?}");
}

/// The check refuses what it means to: a link to a missing file, one off
/// `main`, one outside the repository, one twice.
#[test]
fn a_dead_or_foreign_link_is_reported() {
    let text = format!(
        "# t\n\n> s\n\n## a\n\n- [gone]({RAW}docs/nope.md): x\n- [off](https://example.com/a.md): y\n\
         - [up]({RAW}../etc/passwd): z\n- [a]({RAW}README.md): w\n- [b]({RAW}README.md): v\n"
    );
    let problems = link_problems(&text, &repo());
    assert_eq!(problems.len(), 4, "{problems:#?}");
    assert!(problems[0].contains("docs/nope.md` does not exist"));
    assert!(problems[1].contains("https://example.com/a.md"));
    assert!(problems[2].contains("not a plain repository path"));
    assert!(problems[3].contains("linked twice"));
}
