//! The consumer skills in `skills/` instruct the coding agents of the
//! repositories that use meta-whatsapp-rs. A wrong skill makes those agents generate
//! broken code, confidently and at scale, so these tests keep every skill
//! true of this commit:
//!
//! - **Compiled code.** Every `skills/<name>/examples/*.rs` is compiled into
//!   this test crate (see `tests/skill_examples/mod.rs`) and its tests run.
//!   Every ```` ```rust ```` block in `skills/**/*.md` is an excerpt of one
//!   compiled file — the skill's own `examples/*.rs` or a
//!   `crates/meta-whatsapp-rs/examples/*.rs` program: all its non-blank lines, trimmed,
//!   each paragraph contiguous in the file and the paragraphs in order (the
//!   README rule of `tests/readme.rs`, plus order). Every fence is
//!   ```` ``` ```` with a known language, so no Rust block escapes as
//!   ```` ```rs ````, ```` ```rust,ignore ````, `~~~` or unlabeled; the
//!   skills' example files hide no uncompiled code a block could quote
//!   (block comments, `macro_rules!`, any `cfg` but `cfg(test)`); and no
//!   block quotes a line of any example that starts inside a string (raw
//!   or not) or a block comment, or that belongs to an item under a `cfg`
//!   `--all-features` never enables (the `cfg(not(feature = …))` arms of
//!   `crates/meta-whatsapp-rs/examples/*.rs`).
//! - **Discovery**: the installer finds no `SKILL.md` but `skills/<name>/`
//!   and internal ones in agent directories (a root `SKILL.md` would hide
//!   every other skill).
//! - **Frontmatter** the `npx skills` CLI accepts: `name` is lowercase
//!   words joined by hyphens and equals the directory, `description` is a
//!   double-quoted string of at most 1024 characters that says when to load
//!   the skill, and no unquoted value contains `": "` (YAML reads it as a
//!   nested mapping and the CLI skips the skill). Consumer skills are never
//!   `internal`; the developer skills in `.claude/skills/` always are, so
//!   `npx skills add vaam-apps/meta-whatsapp-rs` does not offer them.
//! - **Links**: every relative link in `skills/**` resolves (and stays
//!   inside its skill, which is installed on its own), every link to a file
//!   of this repository on GitHub names a file that exists, and every
//!   `#anchor` into a Markdown file names one of its headings.
//! - **Names**: every backticked Rust path, type, function, constant or
//!   `snake_case` name in the prose is defined in `crates/**/*.rs`, unless
//!   `skills/.allowlist` lists it (placeholders, other crates' names); a
//!   `Type::member` must be a variant, field or item of that type's own
//!   bodies (or of a trait it implements), not merely of the same file;
//!   every backticked `meta-whatsapp-rs-*` skill name exists. A method called on a
//!   variable (`inbox.reply(..)`) is only checked to exist on some type.
//! - **Shape**: each skill is stamped under its title, stays short, links
//!   its example files, and is routed to from the `meta-whatsapp-rs` hub and from
//!   `skills/README.md`. Each `references/*.md` is stamped under its title
//!   too, and every other mention of a stamp is a well-formed one.
//!
//! The stamp's commit (`Verified against meta-whatsapp-rs <sha>`) must be in HEAD's
//! history (an ancestor, or listed as `Squashed-commit:` by a squash commit
//! on main):
//! that needs git, so `just skills-check` checks it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

// Every example needs one feature or another; `just check`, `just lint` and
// `just test` build with all of them.
#[cfg(all(
    feature = "reqwest",
    feature = "memory",
    feature = "sinks",
    feature = "postgres",
    feature = "redis",
    feature = "axum",
    feature = "typst",
    feature = "flows-endpoint"
))]
mod skill_examples;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

// ─── Files ───────────────────────────────────────────────────────────────

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn rel(path: &Path) -> String {
    path.strip_prefix(repo())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn sorted_dir(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.map(|e| e.unwrap().path()).collect();
    paths.sort();
    paths
}

/// A consumer skill: `skills/<name>/SKILL.md`.
struct Skill {
    name: String,
    dir: PathBuf,
    markdown: String,
}

fn consumer_skills() -> Vec<Skill> {
    let skills: Vec<Skill> = sorted_dir(&repo().join("skills"))
        .into_iter()
        .filter(|dir| dir.join("SKILL.md").is_file())
        .map(|dir| Skill {
            name: dir.file_name().unwrap().to_string_lossy().into_owned(),
            markdown: read(&dir.join("SKILL.md")),
            dir,
        })
        .collect();
    assert!(skills.len() >= 20, "found only {} skills", skills.len());
    skills
}

fn developer_skills() -> Vec<(PathBuf, String)> {
    let skills: Vec<_> = sorted_dir(&repo().join(".claude/skills"))
        .into_iter()
        .map(|dir| dir.join("SKILL.md"))
        .filter(|md| md.is_file())
        .map(|md| {
            let text = read(&md);
            (md, text)
        })
        .collect();
    assert!(!skills.is_empty(), "no developer skills found");
    skills
}

/// Every Markdown file under `skills/`.
fn skill_markdown() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for path in sorted_dir(dir) {
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&repo().join("skills"), &mut files);
    files
        .into_iter()
        .map(|path| {
            let text = read(&path);
            (path, text)
        })
        .collect()
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    sorted_dir(dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect()
}

/// The skill directory a file under `skills/` belongs to, if any.
fn owning_skill(path: &Path) -> Option<PathBuf> {
    let skills = repo().join("skills");
    let first = path.strip_prefix(&skills).ok()?.components().next()?;
    let dir = skills.join(first.as_os_str());
    dir.join("SKILL.md").is_file().then_some(dir)
}

// ─── Markdown ────────────────────────────────────────────────────────────

/// Lines outside fenced code blocks, with their 1-based numbers.
fn prose_lines(markdown: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut fenced = false;
    for (i, line) in markdown.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push((i + 1, line));
        }
    }
    out
}

/// The text after the frontmatter.
fn body(markdown: &str) -> &str {
    let Some(rest) = markdown.strip_prefix("---\n") else {
        return markdown;
    };
    rest.find("\n---\n")
        .map_or(markdown, |end| &rest[end + 5..])
}

/// Inline code spans of a line (single backticks).
fn code_spans(line: &str) -> Vec<&str> {
    let mut spans = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else { break };
        if end > 0 {
            spans.push(&after[..end]);
        }
        rest = &after[end + 1..];
    }
    spans
}

/// Markdown link targets of a line, outside code spans.
fn link_targets(line: &str) -> Vec<String> {
    let mut plain = String::new();
    for (i, part) in line.split('`').enumerate() {
        if i % 2 == 0 {
            plain.push_str(part);
        }
        plain.push(' ');
    }
    let mut targets = Vec::new();
    let mut rest = plain.as_str();
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        let end = after.find(')').unwrap_or(after.len());
        targets.push(after[..end].trim().to_owned());
        rest = &after[end..];
    }
    targets
}

/// GitHub's anchor for a heading.
fn slug(heading: &str) -> String {
    heading
        .trim()
        .chars()
        .filter_map(|c| match c {
            c if c.is_alphanumeric() => Some(c.to_lowercase().next().unwrap_or(c)),
            ' ' | '-' => Some('-'),
            '_' => Some('_'),
            _ => None,
        })
        .collect()
}

fn anchors(markdown: &str) -> HashSet<String> {
    prose_lines(markdown)
        .into_iter()
        .filter_map(|(_, line)| {
            let hashes = line.chars().take_while(|&c| c == '#').count();
            (1..=6)
                .contains(&hashes)
                .then(|| line[hashes..].strip_prefix(' '))
                .flatten()
                .map(|heading| slug(&heading.replace('`', "")))
        })
        .collect()
}

// ─── Frontmatter ─────────────────────────────────────────────────────────

/// `(key, value)` of each top-level frontmatter line, and nested lines as
/// `("<parent>.<key>", value)`.
fn frontmatter(markdown: &str) -> Result<Vec<(String, String)>, String> {
    let mut lines = markdown.lines();
    if lines.next() != Some("---") {
        return Err("the first line must be `---`".into());
    }
    let mut entries = Vec::new();
    let mut parent = String::new();
    for line in lines {
        if line == "---" {
            return Ok(entries);
        }
        if line.trim().is_empty() {
            continue;
        }
        let nested = line.starts_with("  ");
        let (key, value) = line
            .trim()
            .split_once(':')
            .ok_or_else(|| format!("not a `key: value` line: {line}"))?;
        let value = value.trim().to_owned();
        if nested {
            if parent.is_empty() {
                return Err(format!("indented line without a parent key: {line}"));
            }
            entries.push((format!("{parent}.{key}"), value));
        } else {
            key.clone_into(&mut parent);
            entries.push((key.to_owned(), value));
        }
    }
    Err("no closing `---`".into())
}

fn is_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// Problems with a consumer skill's frontmatter.
fn frontmatter_problems(skill: &Skill) -> Vec<String> {
    let entries = match frontmatter(&skill.markdown) {
        Ok(entries) => entries,
        Err(e) => return vec![e],
    };
    let mut problems = Vec::new();
    let value = |key: &str| {
        entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    for (key, v) in &entries {
        let top = key.split('.').next().unwrap_or(key);
        if ![
            "name",
            "description",
            "license",
            "compatibility",
            "metadata",
            "allowed-tools",
        ]
        .contains(&top)
        {
            problems.push(format!("unknown frontmatter key `{key}`"));
        }
        let quoted = v.starts_with('"') || v.starts_with('\'');
        if !quoted && v.contains(": ") {
            problems.push(format!(
                "`{key}` has an unquoted \": \": YAML reads a nested mapping and \
                 `npx skills` skips the skill; double-quote the value"
            ));
        }
        if key == "metadata.internal" && v == "true" {
            problems.push("a consumer skill must not be `internal: true`".into());
        }
    }
    match value("name") {
        None => problems.push("no `name`".into()),
        Some(name) if !is_skill_name(name) => problems.push(format!(
            "name `{name}` is not lowercase words joined by hyphens (at most 64 characters)"
        )),
        Some(name) if name != skill.name => {
            problems.push(format!("name `{name}` differs from the directory"));
        }
        Some(_) => {}
    }
    match value("description") {
        None => problems.push("no `description`".into()),
        Some(d) => match d.strip_prefix('"').and_then(|d| d.strip_suffix('"')) {
            None => problems.push("the description must be one double-quoted line".into()),
            Some(inner) => {
                if inner.contains('"') || inner.contains('\\') {
                    problems
                        .push("the description contains `\"` or `\\`: no escapes, please".into());
                }
                let chars = inner.chars().count();
                if !(1..=1024).contains(&chars) {
                    problems.push(format!("the description has {chars} characters (1–1024)"));
                }
                if !inner.contains("Load when") {
                    problems.push(
                        "the description must say when to load the skill (\"Load when …\")".into(),
                    );
                }
            }
        },
    }
    problems
}

#[test]
fn consumer_skills_have_frontmatter_the_cli_accepts() {
    let mut failures = String::new();
    for skill in consumer_skills() {
        for problem in frontmatter_problems(&skill) {
            writeln!(failures, "skills/{}/SKILL.md: {problem}", skill.name).unwrap();
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}

#[test]
fn developer_skills_are_internal() {
    let mut failures = String::new();
    for (path, markdown) in developer_skills() {
        match frontmatter(&markdown) {
            Ok(entries) => {
                if !entries
                    .iter()
                    .any(|(k, v)| k == "metadata.internal" && v == "true")
                {
                    writeln!(
                        failures,
                        "{}: add `metadata:` / `  internal: true`, or `npx skills add \
                         vaam-apps/meta-whatsapp-rs` offers it to consumers",
                        rel(&path)
                    )
                    .unwrap();
                }
            }
            Err(e) => writeln!(failures, "{}: {e}", rel(&path)).unwrap(),
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}

/// Directories the `npx skills` CLI (1.7) searches besides `skills/`: its
/// `AGENT_PROJECT_SKILL_DIRS`.
const AGENT_SKILL_DIRS: &[&str] = &[
    ".agents/skills",
    ".claude/skills",
    ".cline/skills",
    ".codebuddy/skills",
    ".codex/skills",
    ".commandcode/skills",
    ".continue/skills",
    ".factory/skills",
    ".github/skills",
    ".goose/skills",
    ".grok/skills",
    ".iflow/skills",
    ".junie/skills",
    ".kilo/skills",
    ".kilocode/skills",
    ".kimchi/skills",
    ".kiro/skills",
    ".minimax/skills",
    ".mux/skills",
    ".neovate/skills",
    ".opencode/skills",
    ".openhands/skills",
    ".pi/skills",
    ".posit/assistant/skills",
    ".qoder/skills",
    ".roo/skills",
    ".trae/skills",
    ".windsurf/skills",
    ".zcode/skills",
    ".zencoder/skills",
];

/// The `SKILL.md` files the CLI finds under `dir`: each subdirectory that
/// has one, else its subdirectories, down to `depth` levels.
fn discovered_skill_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    for child in sorted_dir(dir).into_iter().filter(|p| p.is_dir()) {
        let md = child.join("SKILL.md");
        if md.is_file() {
            out.push(md);
        } else if depth > 1 {
            discovered_skill_files(&child, depth - 1, out);
        }
    }
}

/// `npx skills add vaam-apps/meta-whatsapp-rs` must offer exactly `skills/<name>/`.
/// A `SKILL.md` at the repository root makes the CLI offer that one skill
/// and nothing else; one in any top-level directory, deeper under
/// `skills/`, or in an agent directory without `internal: true` is offered
/// to consumers although no check here reads it.
#[test]
fn the_installer_finds_no_other_skill_files() {
    let root = repo();
    let mut failures = String::new();
    let mut stray = Vec::new();
    if root.join("SKILL.md").is_file() {
        stray.push(root.join("SKILL.md"));
    }
    discovered_skill_files(&root, 1, &mut stray);
    let mut consumer = Vec::new();
    discovered_skill_files(&root.join("skills"), 3, &mut consumer);
    for md in consumer {
        if md.parent().and_then(Path::parent) != Some(root.join("skills").as_path()) {
            stray.push(md);
        }
    }
    for path in stray {
        writeln!(
            failures,
            "{}: the installer would offer it (or, at the root, only it); consumer skills \
             live at skills/<name>/SKILL.md",
            rel(&path)
        )
        .unwrap();
    }
    for dir in AGENT_SKILL_DIRS {
        let mut found = Vec::new();
        discovered_skill_files(&root.join(dir), 3, &mut found);
        for md in found {
            let internal = frontmatter(&read(&md)).is_ok_and(|entries| {
                entries
                    .iter()
                    .any(|(k, v)| k == "metadata.internal" && v == "true")
            });
            if !internal {
                writeln!(
                    failures,
                    "{}: add `metadata:` / `  internal: true`, or `npx skills add \
                     vaam-apps/meta-whatsapp-rs` offers it to consumers",
                    rel(&md)
                )
                .unwrap();
            }
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}

// ─── Stamp and shape ─────────────────────────────────────────────────────

/// Whether `line` is `> **Verified against meta-whatsapp-rs <40 hex> (<YYYY-MM-DD>).**…`.
fn is_stamp(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("> **Verified against meta-whatsapp-rs ") else {
        return false;
    };
    let (Some(sha), Some(rest)) = (rest.get(..40), rest.get(40..)) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(" (") else {
        return false;
    };
    let (Some(date), Some(rest)) = (rest.get(..10), rest.get(10..)) else {
        return false;
    };
    let date_ok = date.bytes().enumerate().all(|(i, b)| match i {
        4 | 7 => b == b'-',
        _ => b.is_ascii_digit(),
    });
    sha.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && date_ok
        && rest.starts_with(").**")
}

/// Problems with the stamps of a skill's Markdown: under the `# title` of
/// a `references/*.md` file (`under_title`) the first line must be a
/// stamp, and every prose line that says "Verified against meta-whatsapp-rs" must be a
/// well-formed one. `just skills-check` only greps `Verified against meta-whatsapp-rs
/// <hex>` and checks the commit: a stamp with a typo in its words escapes
/// it, and one with a malformed date or no date passes it.
fn stamp_problems(markdown: &str, under_title: bool) -> Vec<String> {
    let mut problems = Vec::new();
    if under_title {
        let mut lines = markdown.lines().filter(|l| !l.trim().is_empty());
        if !lines.next().is_some_and(|l| l.starts_with("# ")) {
            problems.push("the first line must be its `# title`".to_owned());
        }
        if !lines.next().is_some_and(is_stamp) {
            problems.push(
                "the line under the title must be `> **Verified against meta-whatsapp-rs <full sha> \
                 (<YYYY-MM-DD>).**`"
                    .to_owned(),
            );
        }
    }
    for (n, line) in prose_lines(markdown) {
        if line.contains("Verified against meta-whatsapp-rs") && !is_stamp(line) {
            problems.push(format!(
                "line {n}: a malformed stamp (`> **Verified against meta-whatsapp-rs <full sha> \
                 (<YYYY-MM-DD>).**`)"
            ));
        }
    }
    problems
}

/// The stamp problems of a `references/*.md` file: its stamp goes under its
/// title.
fn reference_problems(markdown: &str) -> Vec<String> {
    stamp_problems(markdown, true)
}

/// `references/*.md` travel with their skill and make claims about the
/// code as much as `SKILL.md` does: each carries a stamp, checked like the
/// skill's, and so does every other mention of one.
#[test]
fn references_are_stamped_like_their_skill() {
    let mut checked = 0;
    let mut failures = String::new();
    for skill in consumer_skills() {
        for problem in stamp_problems(body(&skill.markdown), false) {
            writeln!(failures, "skills/{}/SKILL.md: {problem}", skill.name).unwrap();
        }
        for path in sorted_dir(&skill.dir.join("references")) {
            if path.extension().is_some_and(|e| e == "md") {
                checked += 1;
                for problem in reference_problems(&read(&path)) {
                    writeln!(failures, "{}: {problem}", rel(&path)).unwrap();
                }
            }
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
    assert!(checked >= 4, "only {checked} reference files found");
}

/// Longest a `SKILL.md` may be; longer tables go to `references/`.
const MAX_SKILL_LINES: usize = 160;

#[test]
fn every_skill_is_stamped_short_and_routed_to() {
    let skills = consumer_skills();
    let hub = read(&repo().join("skills/meta-whatsapp-rs/SKILL.md"));
    let readme = read(&repo().join("skills/README.md"));
    let mut failures = String::new();
    for skill in &skills {
        let file = format!("skills/{}/SKILL.md", skill.name);
        let mut lines = body(&skill.markdown)
            .lines()
            .filter(|l| !l.trim().is_empty());
        let title = lines.next().unwrap_or_default();
        if title != format!("# {}", skill.name) {
            writeln!(
                failures,
                "{file}: the first line after the frontmatter must be `# {}`",
                skill.name
            )
            .unwrap();
        }
        if !lines.next().is_some_and(is_stamp) {
            writeln!(
                failures,
                "{file}: the line under the title must be `> **Verified against meta-whatsapp-rs \
                 <full sha> (<YYYY-MM-DD>).**`"
            )
            .unwrap();
        }
        let count = skill.markdown.lines().count();
        if count > MAX_SKILL_LINES {
            writeln!(
                failures,
                "{file}: {count} lines (at most {MAX_SKILL_LINES}): move tables to references/"
            )
            .unwrap();
        }
        for section in [
            "## When to use",
            "## What meta-whatsapp-rs does not do",
            "## Related skills",
        ] {
            if !skill.markdown.lines().any(|l| l.trim() == section) {
                writeln!(failures, "{file}: no `{section}` section").unwrap();
            }
        }
        let quoted = format!("`{}`", skill.name);
        if skill.name != "meta-whatsapp-rs" && !hub.contains(&quoted) {
            writeln!(
                failures,
                "skills/meta-whatsapp-rs/SKILL.md: does not route to {quoted}"
            )
            .unwrap();
        }
        if !readme.contains(&format!("[{quoted}]({}/)", skill.name)) {
            writeln!(
                failures,
                "skills/README.md: does not list [{quoted}]({}/)",
                skill.name
            )
            .unwrap();
        }
        let examples = skill.dir.join("examples");
        for example in rust_files(&examples).into_iter().chain(ts_files(&examples)) {
            let name = example.file_name().unwrap().to_string_lossy();
            if !skill.markdown.contains(&format!("](examples/{name})")) {
                writeln!(failures, "{file}: does not link examples/{name}").unwrap();
            }
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}

// ─── Compiled code ───────────────────────────────────────────────────────

const EXAMPLES_MOD: &str = "crates/meta-whatsapp-rs/tests/skill_examples/mod.rs";

#[test]
fn every_skill_example_is_compiled() {
    let module = read(&repo().join(EXAMPLES_MOD));
    let included: BTreeSet<PathBuf> = module
        .lines()
        .filter_map(|l| l.trim().strip_prefix("#[path = \""))
        .filter_map(|l| l.strip_suffix("\"]"))
        .map(|p| {
            repo()
                .join("crates/meta-whatsapp-rs/tests/skill_examples")
                .join(p)
                .canonicalize()
                .unwrap_or_else(|e| panic!("{EXAMPLES_MOD}: {p}: {e}"))
        })
        .collect();
    let on_disk: BTreeSet<PathBuf> = consumer_skills()
        .iter()
        .flat_map(|s| rust_files(&s.dir.join("examples")))
        .collect();
    let missing: Vec<String> = on_disk.difference(&included).map(|p| rel(p)).collect();
    assert!(
        missing.is_empty(),
        "not compiled: add a `#[path = \"../../../../<file>\"] mod …;` to {EXAMPLES_MOD} for \
         {missing:?}"
    );
    assert!(on_disk.len() >= 15, "only {} skill examples", on_disk.len());
}

/// The `cfg`s a skill's `examples/*.rs` may use: they are compiled into
/// the `skills` test binary, so `cfg(test)` holds there.
const SKILL_EXAMPLE_CFGS: &[&str] = &["cfg(test)"];

/// The `cfg`s `crates/meta-whatsapp-rs/examples/*.rs` may use: the arms that keep a
/// build without the `postgres` feature working. They are built as
/// examples, never with `cfg(test)`.
const CRATE_EXAMPLE_CFGS: &[&str] = &[
    "cfg(feature = \"postgres\")",
    "cfg(not(feature = \"postgres\"))",
];

/// What `cfg(test)` is in `crates/meta-whatsapp-rs/examples/*.rs`: they are built as
/// examples, never as tests. One constant for the gate and its own test,
/// so the two cannot disagree.
const CRATE_EXAMPLE_TEST: Built = Built::Never;

/// Constructs of an example file whose code is never compiled, so a skill
/// could quote it as if it were: block comments, `macro_rules!` (an arm
/// that never matches is never compiled as written), and any `cfg(` but
/// the `allowed` ones, wherever it appears (in a `cfg_attr` too).
/// `just test` builds with every feature, so `cfg(not(feature = …))` and
/// `cfg(any())` never build.
fn uncompiled_code(source: &str, allowed: &[&str]) -> Vec<String> {
    scan_example(source, allowed).0
}

/// [`uncompiled_code`], and the `allowed` `cfg`s the scan met on the way
/// (proof that it read the file).
fn scan_example(source: &str, allowed: &[&str]) -> (Vec<String>, BTreeSet<String>) {
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, line) in source.lines().enumerate() {
        let n = i + 1;
        if line.contains("/*") {
            problems.push(format!("line {n}: a block comment"));
        }
        if line.contains("macro_rules!") {
            problems.push(format!(
                "line {n}: `macro_rules!` (its body is not compiled as written)"
            ));
        }
        let mut rest = line;
        while let Some(at) = rest.find("cfg(") {
            match allowed.iter().find(|ok| rest[at..].starts_with(**ok)) {
                Some(ok) => {
                    seen.insert((*ok).to_owned());
                }
                None => problems.push(format!("line {n}: a `cfg` other than {allowed:?}")),
            }
            rest = &rest[at + 4..];
        }
    }
    (problems, seen)
}

#[test]
fn example_files_hide_no_uncompiled_code() {
    let mut failures = String::new();
    for skill in consumer_skills() {
        for example in rust_files(&skill.dir.join("examples")) {
            for problem in uncompiled_code(&read(&example), SKILL_EXAMPLE_CFGS) {
                writeln!(failures, "{}: {problem}", rel(&example)).unwrap();
            }
        }
    }
    // Skills quote the crate's examples too.
    let crate_examples = rust_files(&repo().join("crates/meta-whatsapp-rs/examples"));
    assert!(crate_examples.len() >= 4, "no crate examples found");
    let mut seen = BTreeSet::new();
    for example in crate_examples {
        let (problems, allowed) = scan_example(&read(&example), CRATE_EXAMPLE_CFGS);
        seen.extend(allowed);
        for problem in problems {
            writeln!(failures, "{}: {problem}", rel(&example)).unwrap();
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
    // The scan read them: `stores()` in cms_inbox.rs has this arm.
    assert!(
        seen.contains("cfg(not(feature = \"postgres\"))"),
        "the crate examples' scan saw only {seen:?}"
    );
}

// ─── What an excerpt may quote ───────────────────────────────────────────
//
// `crates/meta-whatsapp-rs/examples/*.rs` may use `cfg` (their `#[cfg(not(feature =
// "postgres"))]` arms keep a build without the feature working), and any
// example may hold a multi-line string. Neither is code `just test` builds
// with every feature: a block quoting it would look like compiled Rust.

/// What `--all-features` enables for the examples: the keys of
/// `crates/meta-whatsapp-rs/Cargo.toml`'s `[features]`.
fn meta_whatsapp_rs_features() -> HashSet<String> {
    let manifest = read(&repo().join("crates/meta-whatsapp-rs/Cargo.toml"));
    let mut in_features = false;
    let mut features = HashSet::new();
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_features = line == "[features]";
        } else if in_features
            && let Some((key, _)) = line.split_once('=')
            && !line.starts_with('#')
        {
            features.insert(key.trim().to_owned());
        }
    }
    assert!(
        features.contains("postgres"),
        "no [features] in the manifest"
    );
    features
}

/// `source` with the brackets, `;`, `,` and `#` inside string, raw string
/// and char literals and every character of comments blanked (newlines
/// kept, so offsets and lines stay put), and for each line whether it
/// starts in code rather than inside a string or a block comment.
#[allow(clippy::too_many_lines)] // one arm per lexer state and token
fn code_mask(source: &str) -> (Vec<char>, Vec<bool>) {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Lex {
        Code,
        Str,
        RawStr(usize),
        Block(usize),
    }
    let chars: Vec<char> = source.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut masked = chars.clone();
    let blank = |masked: &mut Vec<char>, from: usize, to: usize, all: bool| {
        for c in &mut masked[from..to.min(chars.len())] {
            if *c != '\n' && (all || "(){}[];,#".contains(*c)) {
                *c = ' ';
            }
        }
    };
    let mut starts = vec![true];
    let mut state = Lex::Code;
    let mut i = 0;
    while let Some(c) = at(i) {
        if c == '\n' {
            starts.push(state == Lex::Code);
            i += 1;
            continue;
        }
        let boundary = i == 0 || !at(i - 1).is_some_and(is_ident_char);
        match state {
            Lex::Code => match c {
                '/' if at(i + 1) == Some('/') => {
                    let from = i;
                    while at(i).is_some_and(|c| c != '\n') {
                        i += 1;
                    }
                    blank(&mut masked, from, i, true);
                }
                '/' if at(i + 1) == Some('*') => {
                    blank(&mut masked, i, i + 2, true);
                    state = Lex::Block(1);
                    i += 2;
                }
                '"' => {
                    state = Lex::Str;
                    i += 1;
                }
                // b"…" (and c"…", which the `"` arm reads the same).
                'b' if boundary && at(i + 1) == Some('"') => {
                    state = Lex::Str;
                    i += 2;
                }
                // Raw strings: r"…", br#"…"#, cr#"…"#.
                'r' | 'b' | 'c' if boundary => {
                    let mut j = if c == 'r' { i } else { i + 1 };
                    if c != 'r' && at(j) != Some('r') {
                        i += 1;
                        continue;
                    }
                    j += 1;
                    let mut hashes = 0;
                    while at(j) == Some('#') {
                        hashes += 1;
                        j += 1;
                    }
                    if at(j) == Some('"') {
                        state = Lex::RawStr(hashes);
                        i = j + 1;
                    } else {
                        i += 1;
                    }
                }
                // A char literal ('x', '{', '\'', '\u{1F44D}'), not a
                // lifetime ('a).
                '\'' if at(i + 1) == Some('\\') || at(i + 2) == Some('\'') => {
                    let from = i + 1;
                    // Past the quote, and past an escape's first character.
                    i += if at(i + 1) == Some('\\') { 3 } else { 1 };
                    while at(i).is_some_and(|c| c != '\'' && c != '\n') {
                        i += 1;
                    }
                    blank(&mut masked, from, i, false);
                    i += 1;
                }
                _ => i += 1,
            },
            Lex::Str => {
                if c == '\\' {
                    blank(&mut masked, i, i + 1, false);
                    // `\` + newline continues the string on the next line.
                    i += if at(i + 1) == Some('\n') { 1 } else { 2 };
                    blank(&mut masked, i - 1, i, false);
                } else if c == '"' {
                    state = Lex::Code;
                    i += 1;
                } else {
                    blank(&mut masked, i, i + 1, false);
                    i += 1;
                }
            }
            Lex::RawStr(hashes) => {
                if c == '"' && (1..=hashes).all(|k| at(i + k) == Some('#')) {
                    state = Lex::Code;
                    i += 1 + hashes;
                } else {
                    blank(&mut masked, i, i + 1, false);
                    i += 1;
                }
            }
            Lex::Block(depth) => {
                if c == '/' && at(i + 1) == Some('*') {
                    blank(&mut masked, i, i + 2, true);
                    state = Lex::Block(depth + 1);
                    i += 2;
                } else if c == '*' && at(i + 1) == Some('/') {
                    blank(&mut masked, i, i + 2, true);
                    state = if depth == 1 {
                        Lex::Code
                    } else {
                        Lex::Block(depth - 1)
                    };
                    i += 2;
                } else {
                    blank(&mut masked, i, i + 1, true);
                    i += 1;
                }
            }
        }
    }
    (masked, starts)
}

/// Whether code under a `cfg` predicate is built: always, in some builds
/// (`test`), or never, when `features` are all the features there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Built {
    Always,
    Sometimes,
    Never,
}

/// Evaluate a `cfg(...)` predicate. `feature = "x"` is on when `x` is a
/// feature, `test` is `test` (`Sometimes` for a skill's examples, compiled
/// into a test binary; [`CRATE_EXAMPLE_TEST`] for the crate's examples,
/// built as examples), and anything else (`unix`, `doc`, `debug_assertions`, a
/// typo, a predicate that does not parse) is `Never`: the gate only vouches
/// for what `--all-features` builds.
fn eval_cfg(predicate: &str, features: &HashSet<String>, test: Built) -> Built {
    #[derive(Debug, PartialEq, Eq)]
    enum Tok {
        Word(String),
        Str(String),
        Punct(char),
    }
    fn parse(
        toks: &[Tok],
        i: &mut usize,
        features: &HashSet<String>,
        test: Built,
    ) -> Option<Built> {
        let Tok::Word(name) = toks.get(*i)? else {
            return None;
        };
        *i += 1;
        match toks.get(*i) {
            Some(Tok::Punct('(')) => {
                *i += 1;
                let mut parts = Vec::new();
                while toks.get(*i) != Some(&Tok::Punct(')')) {
                    parts.push(parse(toks, i, features, test)?);
                    match toks.get(*i) {
                        Some(Tok::Punct(',')) => *i += 1,
                        Some(Tok::Punct(')')) => {}
                        _ => return None,
                    }
                }
                *i += 1;
                let has = |b: Built| parts.contains(&b);
                Some(match (name.as_str(), parts.as_slice()) {
                    ("not", [Built::Always]) => Built::Never,
                    ("not", [Built::Never]) => Built::Always,
                    ("all", _) if has(Built::Never) => Built::Never,
                    ("all", _) if !has(Built::Sometimes) => Built::Always,
                    ("any", _) if has(Built::Always) => Built::Always,
                    ("any", _) if !has(Built::Sometimes) => Built::Never,
                    ("not", [Built::Sometimes]) | ("all" | "any", _) => Built::Sometimes,
                    _ => return None,
                })
            }
            Some(Tok::Punct('=')) => {
                *i += 1;
                let Some(Tok::Str(value)) = toks.get(*i) else {
                    return None;
                };
                *i += 1;
                Some(if name == "feature" && features.contains(value) {
                    Built::Always
                } else {
                    Built::Never
                })
            }
            _ if name == "test" => Some(test),
            _ => Some(Built::Never),
        }
    }
    let mut toks = Vec::new();
    let mut chars = predicate.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '"' {
            toks.push(Tok::Str(chars.by_ref().take_while(|&d| d != '"').collect()));
        } else if is_ident_start(c) {
            let mut word = String::from(c);
            while let Some(d) = chars.next_if(|&d| is_ident_char(d)) {
                word.push(d);
            }
            toks.push(Tok::Word(word));
        } else {
            toks.push(Tok::Punct(c));
        }
    }
    let mut i = 0;
    match parse(&toks, &mut i, features, test) {
        Some(built) if i == toks.len() => built,
        _ => Built::Never,
    }
}

/// The first index at or after `from` that is not whitespace.
fn skip_blank(masked: &[char], mut from: usize) -> usize {
    while masked.get(from).is_some_and(|c| c.is_whitespace()) {
        from += 1;
    }
    from
}

/// End (exclusive index into `masked`) of the item that starts at `from`:
/// past its `;` or `,`, or its closing `}` (a following `else` continues
/// it; a following `;` or `,` belongs to it), or up to the bracket that
/// closes what encloses it.
fn item_end(masked: &[char], mut from: usize) -> usize {
    let mut depth = 0usize;
    while let Some(&c) = masked.get(from) {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => return from,
            ')' | ']' => depth -= 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let next = skip_blank(masked, from + 1);
                    let word: String = masked[next..].iter().take(5).collect();
                    if word.starts_with("else") && !word[4..].starts_with(is_ident_char) {
                        from = next + 4;
                        continue;
                    }
                    return if matches!(masked.get(next), Some(';' | ',')) {
                        next + 1
                    } else {
                        from + 1
                    };
                }
            }
            ';' | ',' if depth == 0 => return from + 1,
            _ => {}
        }
        from += 1;
    }
    masked.len()
}

/// For each line of `source`, whether it belongs to an item under a `cfg`
/// that `features` never enable ([`Built::Never`], `test` evaluating to
/// `test`): from the attribute's line through the item's last line. An
/// inner `#![cfg]` that never holds marks the whole file. A `cfg_attr`
/// that carries a `cfg(` (`#[cfg_attr(p, cfg(q))]`) counts as never built:
/// the gate does not evaluate the pair.
fn never_built_lines(source: &str, features: &HashSet<String>, test: Built) -> Vec<bool> {
    let chars: Vec<char> = source.chars().collect();
    let (masked, _) = code_mask(source);
    let line_of = |at: usize| {
        masked[..at.min(masked.len())]
            .iter()
            .filter(|&&c| c == '\n')
            .count()
    };
    let mut never = vec![false; line_of(masked.len()) + 1];
    let cfg: Vec<char> = "cfg(".chars().collect();
    for at in 0..masked.len().saturating_sub(cfg.len()) {
        if masked[at..at + cfg.len()] != cfg[..] {
            continue;
        }
        // Only an attribute removes code: `#[cfg(` / `#![cfg(`, or a `cfg(`
        // inside `#[cfg_attr(…)]` (not evaluated: never built). `cfg!(…)`
        // builds either way, and `x_cfg(` is a call.
        let cfg_attr = enclosing_cfg_attr(&masked, at);
        let name = cfg_attr.unwrap_or(at);
        let mut open = name;
        while open > 0 && masked[open - 1].is_whitespace() {
            open -= 1;
        }
        if open == 0 || masked[open - 1] != '[' {
            continue;
        }
        let bracket = open - 1;
        let inner = bracket >= 2 && masked[bracket - 1] == '!' && masked[bracket - 2] == '#';
        if !(inner || (bracket >= 1 && masked[bracket - 1] == '#')) {
            continue;
        }
        // The `)` that closes the attribute's `cfg(` or `cfg_attr(`.
        let paren = name + if cfg_attr.is_some() { 8 } else { 3 };
        let Some(close) = matching_paren(&masked, paren) else {
            continue;
        };
        if cfg_attr.is_none() {
            let predicate: String = chars[at + cfg.len()..close].iter().collect();
            if eval_cfg(&predicate, features, test) != Built::Never {
                continue;
            }
        }
        let (first, last) = if inner {
            (0, never.len() - 1)
        } else {
            // Past this attribute's `]` and any attribute after it.
            let mut item = skip_blank(&masked, close + 1);
            if masked.get(item) == Some(&']') {
                item = skip_blank(&masked, item + 1);
            }
            while masked.get(item) == Some(&'#') {
                item = skip_blank(&masked, item_end(&masked, item + 2) + 1);
            }
            let end = item_end(&masked, item).max(item + 1);
            (line_of(bracket - 1), line_of(end - 1))
        };
        for line in &mut never[first..=last] {
            *line = true;
        }
    }
    never
}

/// The index of the `)` matching the `(` at `open`.
fn matching_paren(masked: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &c) in masked.iter().enumerate().skip(open) {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The start (the index of `cfg_attr`) of the `#[cfg_attr(` or
/// `#![cfg_attr(` attribute whose parentheses enclose index `at`, if any.
fn enclosing_cfg_attr(masked: &[char], at: usize) -> Option<usize> {
    let needle: Vec<char> = "cfg_attr(".chars().collect();
    let mut depth = 0usize;
    let mut i = at;
    while i > 0 {
        i -= 1;
        match masked[i] {
            ')' | ']' | '}' => depth += 1,
            '(' if depth == 0 => {
                let start = (i + 1).checked_sub(needle.len())?;
                return (masked[start..=i] == needle[..]).then_some(start);
            }
            '(' | '[' | '{' => depth = depth.checked_sub(1)?,
            _ => {}
        }
    }
    None
}

/// Stands for a line an excerpt may not quote; no trimmed Markdown line is
/// equal to it.
const UNCOMPILED: &str = "\u{0}(not compiled)";

/// The trimmed, non-blank lines of `source` a Rust block may quote. A line
/// that starts inside a string (raw or not) or a block comment, or that
/// belongs to an item under a `cfg` the build never enables, is
/// [`UNCOMPILED`]: a block can then quote neither it nor across it.
fn quotable_lines<'a>(source: &'a str, features: &HashSet<String>, test: Built) -> Vec<&'a str> {
    let (_, starts_in_code) = code_mask(source);
    let never = never_built_lines(source, features, test);
    source
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                None
            } else if !starts_in_code[i] || never[i] {
                Some(UNCOMPILED)
            } else {
                Some(trimmed)
            }
        })
        .collect()
}

/// Languages a fence in `skills/**/*.md` may be labeled with.
const FENCE_LANGUAGES: &[&str] = &[
    "rust", "toml", "text", "js", "ts", "bash", "sh", "markdown", "json", "sql", "yaml", "html",
];

/// Problems with the code fences of `markdown`: every block is fenced with
/// ```` ``` ```` and a known language, so a Rust block cannot escape the
/// excerpt check as ```` ```rs ````, ```` ```rust,ignore ````, an unlabeled
/// block or a `~~~` fence.
fn fence_problems(markdown: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut open: Option<usize> = None;
    for (i, line) in markdown.lines().enumerate() {
        let n = i + 1;
        let trimmed = line.trim();
        if trimmed.starts_with("~~~") {
            problems.push(format!("line {n}: `~~~` fence (use ```)"));
            continue;
        }
        let Some(info) = trimmed.strip_prefix("```") else {
            continue;
        };
        match open {
            Some(_) if info.is_empty() => open = None,
            Some(start) => problems.push(format!(
                "line {n}: fence opened inside the block of line {start}"
            )),
            None if FENCE_LANGUAGES.contains(&info) => open = Some(n),
            None => problems.push(format!(
                "line {n}: fence label `{info}` (one of {FENCE_LANGUAGES:?}; Rust is exactly \
                 `rust`, and every `rust` block is checked as an excerpt)"
            )),
        }
    }
    if let Some(start) = open {
        problems.push(format!("line {start}: unterminated fence"));
    }
    problems
}

#[test]
fn code_fences_are_labeled() {
    let mut failures = String::new();
    for (path, markdown) in skill_markdown() {
        for problem in fence_problems(&markdown) {
            writeln!(failures, "{}: {problem}", rel(&path)).unwrap();
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}

/// The ```` ```rust ```` blocks of `markdown`: first line number and
/// paragraphs of trimmed lines.
fn rust_blocks(markdown: &str) -> Vec<(usize, Vec<Vec<&str>>)> {
    let mut blocks = Vec::new();
    let mut block: Option<(usize, Vec<Vec<&str>>)> = None;
    let mut paragraph = Vec::new();
    for (i, line) in markdown.lines().enumerate() {
        let trimmed = line.trim();
        match &mut block {
            None if trimmed == "```rust" => block = Some((i + 1, Vec::new())),
            None => {}
            Some((_, current)) if trimmed.starts_with("```") => {
                if !paragraph.is_empty() {
                    current.push(std::mem::take(&mut paragraph));
                }
                blocks.push(block.take().unwrap());
            }
            Some((_, current)) if trimmed.is_empty() => {
                if !paragraph.is_empty() {
                    current.push(std::mem::take(&mut paragraph));
                }
            }
            Some(_) => paragraph.push(trimmed),
        }
    }
    assert!(block.is_none(), "unterminated ```rust block");
    blocks
}

/// Whether the paragraphs occur in `source`, each contiguous, in order.
fn is_excerpt(source: &[&str], paragraphs: &[Vec<&str>]) -> bool {
    let mut from = 0;
    for paragraph in paragraphs {
        let Some(at) = source
            .get(from..)
            .unwrap_or_default()
            .windows(paragraph.len())
            .position(|window| window == paragraph.as_slice())
        else {
            return false;
        };
        from += at + paragraph.len();
    }
    true
}

#[test]
fn rust_blocks_are_excerpts_of_compiled_files() {
    let features = meta_whatsapp_rs_features();
    // A skill's examples are compiled into the `skills` test binary
    // (`cfg(test)` may hold); the crate's are built as examples (it never
    // does).
    let quotable = |text: String, test: Built| {
        quotable_lines(&text, &features, test)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<String>>()
    };
    let crate_examples: Vec<Vec<String>> =
        rust_files(&repo().join("crates/meta-whatsapp-rs/examples"))
            .into_iter()
            .map(|p| quotable(read(&p), CRATE_EXAMPLE_TEST))
            .collect();
    let mut checked = 0;
    let mut failures = String::new();
    for (path, markdown) in skill_markdown() {
        let own: Vec<Vec<String>> = owning_skill(&path)
            .map(|dir| rust_files(&dir.join("examples")))
            .unwrap_or_default()
            .into_iter()
            .map(|p| quotable(read(&p), Built::Sometimes))
            .collect();
        for (line, paragraphs) in rust_blocks(&markdown) {
            checked += 1;
            let found = own.iter().chain(&crate_examples).any(|source| {
                let source: Vec<&str> = source.iter().map(String::as_str).collect();
                is_excerpt(&source, &paragraphs)
            });
            if !found {
                writeln!(
                    failures,
                    "{}:{line}: not an excerpt of the compiled code of the skill's \
                     examples/*.rs or of crates/meta-whatsapp-rs/examples/*.rs (not inside a \
                     string or comment, nor under a `cfg` --all-features never \
                     enables; edit the example, then copy it):\n{}\n",
                    rel(&path),
                    paragraphs
                        .iter()
                        .map(|p| p.join("\n"))
                        .collect::<Vec<_>>()
                        .join("\n\n")
                )
                .unwrap();
            }
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
    assert!(checked >= 30, "only {checked} rust blocks found");
}

// ─── Links ───────────────────────────────────────────────────────────────

const GITHUB: &str = "https://github.com/vaam-apps/meta-whatsapp-rs/";

/// Normalize `a/b/../c` without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// Problems with one link target, found in `file`.
fn link_problem(file: &Path, target: &str) -> Option<String> {
    let (path, anchor) = target.split_once('#').unwrap_or((target, ""));
    let resolved = if let Some(in_repo) = path.strip_prefix(GITHUB) {
        let in_repo = in_repo
            .strip_prefix("blob/main/")
            .or_else(|| in_repo.strip_prefix("tree/main/"))?;
        repo().join(in_repo)
    } else if path.contains("://") || path.starts_with("mailto:") {
        return None;
    } else if path.is_empty() {
        file.to_path_buf()
    } else {
        let resolved = normalize(&file.parent().unwrap().join(path));
        if let Some(skill) = owning_skill(file)
            && !resolved.starts_with(&skill)
        {
            return Some(format!(
                "`{target}` leaves the skill (installed alone, the link breaks): link \
                 {GITHUB}blob/main/… instead"
            ));
        }
        resolved
    };
    if !resolved.exists() {
        return Some(format!("`{target}`: {} does not exist", rel(&resolved)));
    }
    let markdown = resolved.extension().is_some_and(|e| e == "md");
    if !anchor.is_empty() && markdown && !anchors(&read(&resolved)).contains(anchor) {
        return Some(format!("`{target}`: no heading with anchor #{anchor}"));
    }
    None
}

#[test]
fn links_resolve() {
    let mut checked = 0;
    let mut failures = String::new();
    for (path, markdown) in skill_markdown() {
        for (line, text) in prose_lines(&markdown) {
            for target in link_targets(text) {
                checked += 1;
                if let Some(problem) = link_problem(&path, &target) {
                    writeln!(failures, "{}:{line}: {problem}", rel(&path)).unwrap();
                }
            }
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
    assert!(checked >= 50, "only {checked} links found");
}

#[test]
fn docs_link_to_skills_that_exist() {
    let mut files = vec![repo().join("README.md"), repo().join("CONTRIBUTING.md")];
    files.extend(
        sorted_dir(&repo().join("docs/guides"))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "md")),
    );
    let mut failures = String::new();
    for path in files {
        for (line, text) in prose_lines(&read(&path)) {
            for target in link_targets(text).iter().filter(|t| t.contains("skills/")) {
                if let Some(problem) = link_problem(&path, target) {
                    writeln!(failures, "{}:{line}: {problem}", rel(&path)).unwrap();
                }
            }
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}

// ─── Names ───────────────────────────────────────────────────────────────

/// Identifier and punctuation tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Punct(char),
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if is_ident_start(c) {
            let mut ident = String::from(c);
            while let Some(&next) = chars.peek().filter(|&&n| is_ident_char(n)) {
                ident.push(next);
                chars.next();
            }
            out.push(Token::Ident(ident));
        } else if c.is_ascii_digit() {
            while chars.peek().is_some_and(|&n| is_ident_char(n)) {
                chars.next();
            }
        } else if !c.is_whitespace() {
            out.push(Token::Punct(c));
        }
    }
    out
}

fn ident(token: Option<&Token>) -> Option<&str> {
    match token {
        Some(Token::Ident(name)) => Some(name),
        _ => None,
    }
}

/// What one source file defines, comments stripped.
#[derive(Default)]
struct SourceFile {
    path: PathBuf,
    words: HashSet<String>,
    fns: HashSet<String>,
    /// `struct|enum|trait|type|mod|union|macro_rules! Name`.
    decls: HashSet<String>,
    consts: HashSet<String>,
    /// Indented lines starting with `Name` followed by `,` `(` `{` `=` or
    /// nothing: enum variants, match arms, macro-generated types.
    heads: HashSet<String>,
    /// `pub name:` fields.
    fields: HashSet<String>,
    /// Names in `pub use` statements.
    reexports: HashSet<String>,
    /// Names in `impl … {` headers.
    impls: HashSet<String>,
    /// Lines that are exactly one identifier (macro invocation arguments).
    lone: HashSet<String>,
    has_macro_rules: bool,
    /// Members declared in the body of a type: variants of `enum Name {…}`,
    /// fields of `struct Name {…}`, `fn`/`const`/`type` items of
    /// `impl … Name {…}` and `trait Name {…}`.
    scoped: HashMap<String, HashSet<String>>,
    /// Traits implemented by a type (`impl Trait for Name`): the trait's
    /// provided methods are members too.
    traits: HashMap<String, HashSet<String>>,
    /// Types declared inside a macro invocation (`open_enum! { enum … }`):
    /// the macro adds members the source does not spell out.
    macro_types: HashSet<String>,
}

/// `text` without comments, and with the contents of string, raw string and
/// char literals removed, so that braces and keywords inside them do not
/// count when brace-matching type bodies.
///
/// The same string and comment rules as [`code_mask`] (keep the two in
/// step), but it drops what they cover instead of blanking it in place: the
/// names inside a string must not read as declarations here.
fn code_only(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(c) = at(i) {
        let boundary = i == 0 || !at(i - 1).is_some_and(is_ident_char);
        match c {
            '/' if at(i + 1) == Some('/') => {
                while at(i).is_some_and(|c| c != '\n') {
                    i += 1;
                }
            }
            '/' if at(i + 1) == Some('*') => {
                let mut depth = 0;
                while let Some(c) = at(i) {
                    if c == '/' && at(i + 1) == Some('*') {
                        depth += 1;
                        i += 2;
                    } else if c == '*' && at(i + 1) == Some('/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                out.push(' ');
            }
            // r"…", r#"…"#, br"…", br#"…"#, cr"…", cr#"…"#
            'r' | 'b' | 'c'
                if boundary && {
                    let start = if c != 'r' && at(i + 1) == Some('r') {
                        i + 2
                    } else {
                        i + 1
                    };
                    (c == 'r' || start == i + 2) && {
                        let mut j = start;
                        while at(j) == Some('#') {
                            j += 1;
                        }
                        at(j) == Some('"')
                    }
                } =>
            {
                let mut j = if c == 'r' { i + 1 } else { i + 2 };
                let mut hashes = 0;
                while at(j) == Some('#') {
                    hashes += 1;
                    j += 1;
                }
                j += 1; // the opening quote
                while let Some(c) = at(j) {
                    if c == '"' && (1..=hashes).all(|k| at(j + k) == Some('#')) {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                out.push_str("\"\"");
                i = j;
            }
            '"' => {
                i += 1;
                while let Some(c) = at(i) {
                    i += 1;
                    if c == '\\' {
                        i += 1;
                    } else if c == '"' {
                        break;
                    }
                }
                out.push_str("\"\"");
            }
            // A char literal ('x', '\n', '\u{1F44D}'), not a lifetime ('a).
            '\'' if at(i + 1) == Some('\\') || at(i + 2) == Some('\'') => {
                i += if at(i + 1) == Some('\\') { 2 } else { 1 };
                while at(i).is_some_and(|c| c != '\'') {
                    i += 1;
                }
                i += 1;
                out.push_str("''");
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// End (exclusive) of the `<…>` starting at `start`; `->` inside does not
/// close it.
fn skip_angles(toks: &[Token], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    while let Some(token) = toks.get(i) {
        match token {
            Token::Punct('<') => depth += 1,
            Token::Punct('>') if i > 0 && toks[i - 1] != Token::Punct('-') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            Token::Punct('{' | ';') => return i,
            _ => {}
        }
        i += 1;
    }
    i
}

/// The last segment of the type path at `i` (`&`, `dyn`, `mut` and
/// lifetimes skipped), and the index after it and its generics. `None` for
/// a macro placeholder (`$name`), a tuple or anything else.
fn type_path(toks: &[Token], mut i: usize) -> Option<(String, usize)> {
    loop {
        match toks.get(i) {
            Some(Token::Punct('&')) => i += 1,
            Some(Token::Punct('\'')) => i += 2,
            Some(Token::Ident(w)) if w == "dyn" || w == "mut" => i += 1,
            _ => break,
        }
    }
    let mut last = ident(toks.get(i))?.to_owned();
    i += 1;
    while toks.get(i) == Some(&Token::Punct(':')) && toks.get(i + 1) == Some(&Token::Punct(':')) {
        ident(toks.get(i + 2))?.clone_into(&mut last);
        i += 3;
    }
    if toks.get(i) == Some(&Token::Punct('<')) {
        i = skip_angles(toks, i);
    }
    Some((last, i))
}

/// The self type and the trait of an `impl` header (`toks` starts after
/// `impl`).
fn impl_header(toks: &[Token]) -> Option<(String, Option<String>)> {
    let start = if toks.first() == Some(&Token::Punct('<')) {
        skip_angles(toks, 0)
    } else {
        0
    };
    let (first, next) = type_path(toks, start)?;
    if ident(toks.get(next)) == Some("for") {
        let (self_ty, _) = type_path(toks, next + 1)?;
        Some((self_ty, Some(first)))
    } else {
        Some((first, None))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Body {
    Enum,
    Struct,
    Items,
}

/// What [`scoped_members`] finds; see the fields of [`SourceFile`] with
/// the same names.
#[derive(Default)]
struct Scopes {
    members: HashMap<String, HashSet<String>>,
    traits: HashMap<String, HashSet<String>>,
    macro_types: HashSet<String>,
}

/// Type members by brace-matched body, see [`SourceFile::scoped`].
fn scoped_members(toks: &[Token]) -> Scopes {
    let mut members: HashMap<String, HashSet<String>> = HashMap::new();
    let mut traits: HashMap<String, HashSet<String>> = HashMap::new();
    let mut macro_types = HashSet::new();
    let mut depth = 0usize;
    // (body depth, kind, type name)
    let mut open: Vec<(usize, Body, String)> = Vec::new();
    let mut pending: Option<(Body, String)> = None;
    // Depths at which a macro invocation's body starts.
    let mut invocations: Vec<usize> = Vec::new();
    let mut invocation_next = false;
    for (i, token) in toks.iter().enumerate() {
        match token {
            Token::Punct('{' | '(' | '[') if invocation_next => {
                invocation_next = false;
                if *token == Token::Punct('{') {
                    depth += 1;
                    invocations.push(depth);
                } else {
                    // `name!(…)` / `name![…]`: parentheses are not tracked;
                    // treat what follows as inside until the next `;`.
                    invocations.push(depth + 1);
                }
            }
            Token::Punct('{') => {
                depth += 1;
                if let Some((body, name)) = pending.take() {
                    if !invocations.is_empty() {
                        macro_types.insert(name.clone());
                    }
                    open.push((depth, body, name));
                }
            }
            Token::Punct('}') => {
                if open.last().is_some_and(|(d, _, _)| *d == depth) {
                    open.pop();
                }
                while invocations.last().is_some_and(|d| *d >= depth) {
                    invocations.pop();
                }
                depth = depth.saturating_sub(1);
            }
            Token::Punct(';') => {
                pending = None;
                if invocations.last() == Some(&(depth + 1)) {
                    invocations.pop();
                }
            }
            Token::Punct('!') => {
                invocation_next = i > 0
                    && matches!(&toks[i - 1], Token::Ident(w) if w != "macro_rules")
                    && matches!(toks.get(i + 1), Some(Token::Punct('{' | '(' | '[')));
            }
            Token::Ident(word) => {
                let next = ident(toks.get(i + 1));
                match (word.as_str(), next) {
                    ("enum", Some(name)) => pending = Some((Body::Enum, name.to_owned())),
                    ("struct" | "union", Some(name)) => {
                        pending = Some((Body::Struct, name.to_owned()));
                    }
                    ("trait", Some(name)) => pending = Some((Body::Items, name.to_owned())),
                    ("impl", _) => {
                        if let Some((self_ty, trait_name)) = impl_header(&toks[i + 1..]) {
                            if let Some(trait_name) = trait_name {
                                traits
                                    .entry(self_ty.clone())
                                    .or_default()
                                    .insert(trait_name);
                            }
                            pending = Some((Body::Items, self_ty));
                        }
                    }
                    _ => {}
                }
                if let Some((d, body, name)) = open.last()
                    && *d == depth
                    && let Some(member) = body_member(*body, toks, i)
                {
                    members
                        .entry(name.clone())
                        .or_default()
                        .insert(member.to_owned());
                }
            }
            Token::Punct(_) => {}
        }
    }
    Scopes {
        members,
        traits,
        macro_types,
    }
}

/// The member the identifier at `toks[i]` declares, directly inside a
/// `body` (a variant, a field, or the name after `fn`/`const`/`type`).
fn body_member(body: Body, toks: &[Token], i: usize) -> Option<&str> {
    let word = ident(toks.get(i))?;
    let prev = i.checked_sub(1).and_then(|p| toks.get(p));
    match body {
        Body::Enum => matches!(prev, Some(Token::Punct('{' | ',' | ']'))).then_some(word),
        Body::Struct => (toks.get(i + 1) == Some(&Token::Punct(':'))
            && toks.get(i + 2) != Some(&Token::Punct(':'))
            && prev != Some(&Token::Punct(':')))
        .then_some(word),
        Body::Items => match word {
            "fn" | "const" | "type" => ident(toks.get(i + 1)),
            _ => None,
        },
    }
}

impl SourceFile {
    fn parse(path: PathBuf, text: &str) -> Self {
        let code: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .map(|l| match l.find(" //") {
                Some(at) if !l[at..].contains('"') => &l[..at],
                _ => l,
            })
            .collect();
        let mut file = Self {
            path,
            has_macro_rules: text.contains("macro_rules!"),
            ..Self::default()
        };
        for line in &code {
            let trimmed = line.trim_start();
            let indented = trimmed.len() < line.len();
            let line_tokens = tokens(trimmed);
            if let Some(first) = ident(line_tokens.first()) {
                let rest = trimmed[first.len()..].trim_start();
                if indented
                    && trimmed.starts_with(first)
                    && (rest.is_empty() || rest.starts_with([',', '(', '{', '=']))
                {
                    file.heads.insert(first.to_owned());
                }
                if line_tokens.len() == 1 {
                    file.lone.insert(first.to_owned());
                }
                if first == "pub"
                    && let (Some(name), Some(Token::Punct(':'))) =
                        (ident(line_tokens.get(1)), line_tokens.get(2))
                    && line_tokens.get(3) != Some(&Token::Punct(':'))
                {
                    file.fields.insert(name.to_owned());
                }
            }
        }
        let all = tokens(&code.join("\n"));
        let mut in_use = false;
        let mut in_impl = false;
        for (i, token) in all.iter().enumerate() {
            let next = ident(all.get(i + 1));
            match token {
                Token::Ident(word) => {
                    file.words.insert(word.clone());
                    if in_use {
                        file.reexports.insert(word.clone());
                    }
                    if in_impl {
                        file.impls.insert(word.clone());
                    }
                    match (word.as_str(), next) {
                        ("fn", Some(name)) => {
                            file.fns.insert(name.to_owned());
                        }
                        ("struct" | "enum" | "trait" | "type" | "mod" | "union", Some(name)) => {
                            file.decls.insert(name.to_owned());
                        }
                        ("const" | "static", Some(name)) => {
                            file.consts.insert(name.to_owned());
                        }
                        ("macro_rules", _) => {
                            if let Some(name) = ident(all.get(i + 2)) {
                                file.decls.insert(name.to_owned());
                            }
                        }
                        ("use", _) if i > 0 && ident(all.get(i - 1)) == Some("pub") => {
                            in_use = true;
                        }
                        ("impl", _) => in_impl = true,
                        _ => {}
                    }
                }
                Token::Punct(';') => in_use = false,
                Token::Punct('{') => in_impl = false,
                Token::Punct(_) => {}
            }
        }
        let scopes = scoped_members(&tokens(&code_only(text)));
        file.scoped = scopes.members;
        file.traits = scopes.traits;
        file.macro_types = scopes.macro_types;
        file
    }
}

/// Members every type may be named with in prose: derived or blanket trait
/// methods the scope parser does not see.
const DERIVED_MEMBERS: &[&str] = &[
    "default",
    "clone",
    "to_string",
    "to_owned",
    "fmt",
    "eq",
    "hash",
    "serialize",
    "deserialize",
];

/// Everything `crates/**/*.rs` defines, plus the compiled examples of the
/// skill being checked (`extra`): its prose may name the helpers they
/// define.
struct Index {
    files: Vec<SourceFile>,
    extra: Vec<SourceFile>,
}

impl Index {
    fn build() -> Self {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for path in sorted_dir(dir) {
                if path.is_dir() {
                    if path.file_name().is_none_or(|n| n != "target") {
                        walk(&path, out);
                    }
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let mut paths = Vec::new();
        walk(&repo().join("crates"), &mut paths);
        // Not this file: its test strings name things that must not exist.
        let this = repo().join("crates/meta-whatsapp-rs/tests/skills.rs");
        let files: Vec<SourceFile> = paths
            .into_iter()
            .filter(|p| *p != this)
            .map(|p| {
                let text = read(&p);
                SourceFile::parse(p, &text)
            })
            .collect();
        assert!(files.len() > 100, "indexed only {} files", files.len());
        Self {
            files,
            extra: Vec::new(),
        }
    }

    /// Also look names up in the example files of the skill owning `path`.
    fn scope_to(&mut self, path: &Path) {
        self.extra = owning_skill(path)
            .map(|dir| rust_files(&dir.join("examples")))
            .unwrap_or_default()
            .into_iter()
            .map(|p| {
                let text = read(&p);
                SourceFile::parse(p, &text)
            })
            .collect();
    }

    fn all(&self) -> impl Iterator<Item = &SourceFile> {
        self.files.iter().chain(&self.extra)
    }

    fn any(&self, f: impl Fn(&SourceFile) -> bool) -> bool {
        self.all().any(f)
    }

    fn word(&self, name: &str) -> bool {
        self.any(|f| f.words.contains(name))
    }

    fn function(&self, name: &str) -> bool {
        self.any(|f| f.fns.contains(name))
    }

    fn constant(&self, name: &str) -> bool {
        self.any(|f| f.consts.contains(name))
    }

    fn declared(&self, name: &str) -> bool {
        self.any(|f| f.decls.contains(name) || f.heads.contains(name) || f.reexports.contains(name))
    }

    /// The members declared in the bodies of `head` (and of the traits it
    /// implements), when its source declares them all: `None` for a type
    /// without a body in the source, or one a macro invocation declares.
    fn scoped_members(&self, head: &str) -> Option<HashSet<&str>> {
        if self.all().any(|f| f.macro_types.contains(head)) {
            return None;
        }
        let mut found = false;
        let mut members = HashSet::new();
        let mut traits = HashSet::new();
        for file in self.all() {
            if let Some(m) = file.scoped.get(head) {
                found = true;
                members.extend(m.iter().map(String::as_str));
            }
            if let Some(t) = file.traits.get(head) {
                traits.extend(t.iter().map(String::as_str));
            }
        }
        for file in self.all() {
            for t in &traits {
                if let Some(m) = file.scoped.get(*t) {
                    members.extend(m.iter().map(String::as_str));
                }
            }
        }
        found.then_some(members)
    }

    /// Whether `member` is a method, field, variant or constant of `head`.
    fn member(&self, head: &str, member: &str) -> bool {
        if let Some(members) = self.scoped_members(head) {
            return members.contains(member) || DERIVED_MEMBERS.contains(&member);
        }
        // Macro-generated types: whatever the files around them define.
        let mut files: Vec<&SourceFile> = self
            .all()
            .filter(|f| f.decls.contains(head) || f.impls.contains(head))
            .collect();
        if files.is_empty() {
            // Macro-generated types: `id_type!(… Name)` next to the macro.
            files = self
                .all()
                .filter(|f| f.has_macro_rules && f.lone.contains(head))
                .collect();
        } else if self.all().any(|f| f.macro_types.contains(head)) {
            // `open_enum! { enum Name {…} }`: the macro, in another file,
            // adds members (`Other`, `as_str`).
            files.extend(self.all().filter(|f| f.has_macro_rules));
        }
        files.iter().any(|f| {
            f.fns.contains(member)
                || f.fields.contains(member)
                || f.heads.contains(member)
                || f.consts.contains(member)
                || (member == "default" && f.words.contains("Default"))
        })
    }

    /// The files of a module directory (or file plus its directory).
    fn scope(&self, file: Option<&Path>, dir: &Path) -> Vec<&SourceFile> {
        self.files
            .iter()
            .filter(|f| file.is_some_and(|file| f.path == file) || f.path.starts_with(dir))
            .collect()
    }

    /// Resolve `meta_whatsapp_rs::a::b::Item::member` module by module.
    fn path(&self, segments: &[&str]) -> Result<(), String> {
        let crates = repo().join("crates");
        let (krate, mut rest) = match segments {
            ["meta_whatsapp_rs", module, rest @ ..] if FACADE.iter().any(|(m, _)| m == module) => {
                let krate = FACADE.iter().find(|(m, _)| m == module).unwrap().1;
                (krate, rest)
            }
            [krate, rest @ ..] => (*krate, rest),
            [] => return Ok(()),
        };
        let dir_name = krate.replace('_', "-");
        let mut dir = crates.join(dir_name).join("src");
        let mut file: Option<PathBuf> = None;
        while let [segment, tail @ ..] = rest {
            if !segment.starts_with(|c: char| c.is_ascii_lowercase()) {
                break;
            }
            if dir.join(segment).is_dir() {
                dir = dir.join(segment);
                file = dir
                    .with_extension("rs")
                    .is_file()
                    .then(|| dir.with_extension("rs"));
            } else if dir.join(format!("{segment}.rs")).is_file() {
                file = Some(dir.join(format!("{segment}.rs")));
                dir = dir.join(segment);
            } else {
                break;
            }
            rest = tail;
        }
        let Some((item, members)) = rest.split_first() else {
            return Ok(());
        };
        let scope = self.scope(file.as_deref(), &dir);
        // A lowercase segment with more after it is a module path: one that
        // is neither a file nor a directory must be a re-exported crate or
        // module (`meta_whatsapp_rs::webhooks::axum::…`), whose insides are not ours.
        if item.starts_with(|c: char| c.is_ascii_lowercase()) && !members.is_empty() {
            return if scope.iter().any(|f| f.reexports.contains(*item)) {
                Ok(())
            } else {
                Err(format!("`{item}` is not a module of `{krate}`"))
            };
        }
        if !scope.iter().any(|f| f.words.contains(*item)) {
            if !(self.declared(item) || self.function(item) || self.constant(item)) {
                return Err(format!("`{item}` is not defined in `{krate}`"));
            }
            if !scope
                .iter()
                .any(|f| f.reexports.contains(*item) || f.reexports.contains("*"))
            {
                return Err(format!("`{item}` is not visible there"));
            }
        }
        // Each member must belong to the segment before it:
        // `VerifyOutcome::CoolingDown` fails although `CoolingDown` is a
        // variant of `IssueOutcome` in the same file.
        let mut owner = *item;
        for member in members {
            let ok = if owner.starts_with(|c: char| c.is_ascii_uppercase()) {
                self.member(owner, member)
            } else {
                self.function(member) || self.declared(member) || self.constant(member)
            };
            if !ok {
                return Err(format!("`{owner}` has no member `{member}`"));
            }
            owner = member;
        }
        Ok(())
    }
}

/// `meta_whatsapp_rs::<module>` → the crate it re-exports.
const FACADE: &[(&str, &str)] = &[
    ("client", "meta_whatsapp_client"),
    ("core", "meta_whatsapp_core"),
    ("webhooks", "meta_whatsapp_webhooks"),
    ("adapters", "meta_whatsapp_adapters"),
    ("typst", "meta_whatsapp_typst"),
];

const CRATES: &[&str] = &[
    "meta_whatsapp_rs",
    "meta_whatsapp_core",
    "meta_whatsapp_client",
    "meta_whatsapp_webhooks",
    "meta_whatsapp_adapters",
    "meta_whatsapp_typst",
];

/// Rust's own words: never meta-whatsapp-rs API, never worth listing.
const LANGUAGE: &[&str] = &[
    "Self",
    "Some",
    "None",
    "Ok",
    "Err",
    "Option",
    "Result",
    "Vec",
    "String",
    "Box",
    "Arc",
    "Fn",
    "FnMut",
    "FnOnce",
    "Send",
    "Sync",
    "Clone",
    "Copy",
    "Debug",
    "Default",
    "Display",
    "From",
    "Into",
    "AsRef",
    "Iterator",
    "IntoIterator",
    "PartialEq",
    "Eq",
    "Hash",
    "Sized",
    "self",
    "crate",
    "super",
];

/// Names `skills/.allowlist` accepts: one per line, `#` starts a comment.
fn allowlist() -> HashSet<String> {
    read(&repo().join("skills/.allowlist"))
        .lines()
        .map(|l| l.split('#').next().unwrap_or_default().trim())
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Spans that are not Rust: shell, JSON, URLs, HTTP, numbers, file names.
fn is_not_rust(span: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "npx ", "cargo ", "openssl ", "just ", "git ", "curl ", "{", "\"", "[", "#", "wa.me",
        "https://", "http://", "+", "/", "POST", "GET", "DELETE", "event:", "<", "--",
    ];
    const FILES: &[&str] = &[
        ".rs", ".md", ".toml", ".json", ".typ", ".sql", ".yml", ".yaml",
    ];
    PREFIXES.iter().any(|p| span.starts_with(p))
        || (!span.contains(' ') && FILES.iter().any(|ext| span.ends_with(ext)))
        || span
            .chars()
            .all(|c| c.is_ascii_digit() || " ,–.-%".contains(c))
        // KEY=value shell assignments
        || span.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
}

fn is_camel(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase())
        && name.chars().any(|c| c.is_ascii_lowercase())
}

fn is_screaming(name: &str) -> bool {
    name.contains('_')
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn is_snake(name: &str) -> bool {
    name.contains('_')
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Remove `"…"` literals, so their contents are not taken for names.
fn without_strings(span: &str) -> String {
    let mut out = String::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in span.chars() {
        match (in_string, c) {
            (true, '\\') if !escaped => escaped = true,
            (true, '"') if !escaped => in_string = false,
            (true, _) => escaped = false,
            (false, '"') => {
                in_string = true;
                out.push_str("\"\"");
            }
            (false, c) => out.push(c),
        }
    }
    out
}

/// The names in `span` that are not defined, as `(kind, name, why)`.
fn undefined_names(
    index: &Index,
    allowed: &HashSet<String>,
    skills: &HashSet<String>,
    span: &str,
) -> Vec<String> {
    let mut problems = Vec::new();
    let span = span.trim();
    // A skill name, a crate name, an HTTP header: hyphenated words.
    if span.contains('-') && span.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        if span.starts_with("meta-whatsapp-rs-") && !skills.contains(span) {
            problems.push(format!("skill `{span}` does not exist"));
        }
        return problems;
    }
    if is_not_rust(span) {
        return problems;
    }
    let skip = |name: &str| LANGUAGE.contains(&name) || allowed.contains(name);
    let code = without_strings(span);
    let toks = tokens(&code);
    // Paths: runs of identifiers joined by `::`.
    let mut i = 0;
    while i < toks.len() {
        let Some(first) = ident(toks.get(i)) else {
            i += 1;
            continue;
        };
        let mut segments = vec![first];
        let mut j = i + 1;
        while toks.get(j) == Some(&Token::Punct(':'))
            && toks.get(j + 1) == Some(&Token::Punct(':'))
            && let Some(next) = ident(toks.get(j + 2))
        {
            segments.push(next);
            j += 3;
        }
        if segments.len() > 1 && !skip(segments[0]) {
            let result = if CRATES.contains(&segments[0]) {
                index.path(&segments)
            } else {
                let head = segments[0];
                if index.declared(head) || index.function(head) {
                    segments
                        .windows(2)
                        .find(|pair| {
                            let (owner, member) = (pair[0], pair[1]);
                            if owner.starts_with(|c: char| c.is_ascii_uppercase()) {
                                !index.member(owner, member)
                            } else {
                                !(index.function(member) || index.declared(member))
                            }
                        })
                        .map_or(Ok(()), |pair| {
                            Err(format!("`{}` has no member `{}`", pair[0], pair[1]))
                        })
                } else {
                    Err(format!("`{head}` is not defined"))
                }
            };
            if let Err(why) = result {
                problems.push(format!("path `{}`: {why}", segments.join("::")));
            }
        }
        i = j.max(i + 1);
    }
    for (k, token) in toks.iter().enumerate() {
        let Some(name) = ident(Some(token)) else {
            continue;
        };
        if skip(name) {
            continue;
        }
        let call = toks.get(k + 1) == Some(&Token::Punct('('));
        let ok = if call && name.starts_with(|c: char| c.is_ascii_lowercase()) {
            index.function(name)
        } else if is_camel(name) {
            index.declared(name)
        } else if is_screaming(name) {
            index.constant(name) || index.declared(name)
        } else if is_snake(name) {
            index.word(name)
        } else {
            true
        };
        if !ok {
            problems.push(format!("`{name}` is not defined in crates/"));
        }
    }
    problems
}

#[test]
fn backticked_names_exist() {
    let mut index = Index::build();
    let allowed = allowlist();
    let skills: HashSet<String> = consumer_skills().into_iter().map(|s| s.name).collect();
    let server_spec = Spec::load();
    let mut failures: HashMap<String, Vec<String>> = HashMap::new();
    let mut checked = 0;
    for (path, markdown) in skill_markdown() {
        index.scope_to(&path);
        let text = if path.ends_with("SKILL.md") {
            body(&markdown)
        } else {
            &markdown
        };
        let offset = markdown.lines().count() - text.lines().count();
        for (line, prose) in prose_lines(text) {
            // A span split over two lines would pair the wrong backticks
            // and hide every name after it from this check.
            if prose.matches('`').count() % 2 == 1 {
                failures
                    .entry("a code span is split across lines (odd number of backticks)".into())
                    .or_default()
                    .push(format!("{}:{}", rel(&path), line + offset));
            }
            for span in code_spans(prose) {
                checked += 1;
                // A server skill's routes, schemas, codes and variables are
                // the OpenAPI document's (server_skills_cite_the_document…).
                if in_server_skill(&path) && server_span_problems(&server_spec, span).is_some() {
                    continue;
                }
                for problem in undefined_names(&index, &allowed, &skills, span) {
                    failures.entry(problem).or_default().push(format!(
                        "{}:{}",
                        rel(&path),
                        line + offset
                    ));
                }
            }
        }
    }
    let mut report: Vec<String> = failures
        .into_iter()
        .map(|(problem, places)| format!("{problem} ({})", places.join(", ")))
        .collect();
    report.sort();
    assert!(
        report.is_empty(),
        "names the skills use but the code does not define (fix the skill, or list a \
         placeholder or another crate's name in skills/.allowlist):\n{}",
        report.join("\n")
    );
    assert!(checked >= 500, "only {checked} code spans found");
}

// ─── The checks catch what they claim to ─────────────────────────────────

#[test]
fn the_stamp_checks_reject_known_bad_input() {
    // Stamps.
    let sha = "41fe5f9c963f4718db1362663a62de97244846ee";
    assert!(is_stamp(&format!(
        "> **Verified against meta-whatsapp-rs {sha} (2026-09-24).**"
    )));
    assert!(!is_stamp(
        "> **Verified against meta-whatsapp-rs 41fe5f9 (2026-09-24).**"
    ));
    assert!(!is_stamp(&format!(
        "> **Verified against meta-whatsapp-rs {sha} (24.09.2026).**"
    )));
    assert!(!is_stamp(&format!(
        "> **Verified against meta-whatsapp-rs {sha} (2026x09-24).**"
    )));
    assert!(!is_stamp(&format!(
        "> **Verified against meta-whatsapp-rs {sha} (2026-09x24).**"
    )));
    // References: a stamp under the title, and no malformed stamp anywhere.
    let good = format!(
        "# Error kinds\n\n> **Verified against meta-whatsapp-rs {sha} (2026-09-24).** Source: x.\n"
    );
    assert!(stamp_problems(&good, true).is_empty());
    assert!(reference_problems(&good).is_empty());
    assert!(
        !reference_problems("# Error kinds\n\nA table without a stamp.\n").is_empty(),
        "a reference needs its stamp under the title"
    );
    for bad in [
        "# Error kinds\n\nA table without a stamp.\n".to_owned(),
        "# Error kinds\n\n> **Verified against meta-whatsapp-rs 41fe5f9 (2026-09-24).**\n"
            .to_owned(),
        format!("# Error kinds\n\n> **Verified against meta-whatsapp-rs {sha} (24.09.2026).**\n"),
        format!("# Error kinds\n\n> **Verifed against meta-whatsapp-rs {sha} (2026-09-24).**\n"),
        format!("> **Verified against meta-whatsapp-rs {sha} (2026-09-24).**\n\n# Error kinds\n"),
        format!("{good}\nRe-checked: Verified against meta-whatsapp-rs 41fe5f9 (2026-09-24).\n"),
    ] {
        assert!(!stamp_problems(&bad, true).is_empty(), "{bad}");
    }
    // In a `SKILL.md` body the title check is the shape test's; a second,
    // malformed stamp is still refused. One in a code block is an example.
    assert!(!stamp_problems("Verified against meta-whatsapp-rs 41fe5f9.\n", false).is_empty());
    assert!(
        stamp_problems(
            "```markdown\n> **Verified against meta-whatsapp-rs <sha> (<date>).**\n```\n",
            false
        )
        .is_empty()
    );
}

#[test]
fn the_checks_reject_known_bad_input() {
    // Frontmatter.
    let skill = |markdown: &str| Skill {
        name: "meta-whatsapp-rs-x".into(),
        dir: PathBuf::new(),
        markdown: markdown.into(),
    };
    let ok = "---\nname: meta-whatsapp-rs-x\ndescription: \"Does x. Load when y.\"\n---\n";
    assert!(frontmatter_problems(&skill(ok)).is_empty());
    for bad in [
        "---\nname: meta-whatsapp-rs-x\ndescription: Does x: Load when y.\n---\n",
        "---\nname: meta-whatsapp-rs-y\ndescription: \"Does x. Load when y.\"\n---\n",
        "---\nname: WA_rs\ndescription: \"Does x. Load when y.\"\n---\n",
        "---\nname: meta-whatsapp-rs-x\ndescription: \"Does x.\"\n---\n",
        "---\nname: meta-whatsapp-rs-x\ndescription: \"Does x. Load when y.\"\nmetadata:\n  internal: true\n---\n",
    ] {
        assert!(!frontmatter_problems(&skill(bad)).is_empty(), "{bad}");
    }
    let long = format!(
        "---\nname: meta-whatsapp-rs-x\ndescription: \"Load when {}\"\n---\n",
        "y".repeat(1024)
    );
    assert!(!frontmatter_problems(&skill(&long)).is_empty());

    // Excerpts: every paragraph, contiguous, in order.
    let source = ["a", "b", "c", "d"];
    assert!(is_excerpt(&source, &[vec!["a", "b"], vec!["d"]]));
    assert!(!is_excerpt(&source, &[vec!["a", "c"]]));
    assert!(!is_excerpt(&source, &[vec!["d"], vec!["a"]]));

    // Names.
    let index = Index::build();
    let allowed = HashSet::new();
    let skills = HashSet::from(["meta-whatsapp-rs-errors".to_owned()]);
    let bad = |span: &str| !undefined_names(&index, &allowed, &skills, span).is_empty();
    for good in [
        "ErrorKind::CustomerServiceWindowClosed",
        "meta_whatsapp_rs::client::messages::OutboundMessage",
        // Re-exported crates: their insides are not checked.
        "meta_whatsapp_rs::webhooks::axum::routing",
        "meta_whatsapp_rs::adapters::store::postgres::sqlx",
        "Recipient::phone(\"+1\")",
        "client.messages(pnid).send(&msg)",
        "DEFAULT_TIMEOUT",
        "meta-whatsapp-rs-errors",
        // Members by type: variants, fields, inherent and trait methods,
        // macro-generated types and derives.
        "VerifyOutcome::Verified",
        "IssueOutcome::RateLimited",
        "OtpConfig::namespace",
        "meta_whatsapp_rs::client::authentication::OtpConfig::namespace",
        "meta_whatsapp_rs::inbox::Inbox::window_is_open",
        "Error::may_have_been_sent",
        "ErrorKind::is_rejected_before_processing",
        "MemoryKvStore::get",
        "PhoneNumberId::new",
        "PreferenceValue::Other",
        "ProfileUpdate::default()",
    ] {
        assert!(!bad(good), "{good}");
    }
    for wrong in [
        "ErrorKind::WindowClosed",
        "meta_whatsapp_rs::client::messages::OutboundMesage",
        "meta_whatsapp_rs::client::message::OutboundMessage",
        "Recipient::telephone(\"+1\")",
        "client.messages(pnid).transmit(&msg)",
        "DEFAULT_TIMEOUT_MS",
        "meta-whatsapp-rs-messaging",
        "TokenSafe",
        "biz_opaque_callback",
        // A member of a sibling type in the same file, or of another type.
        "VerifyOutcome::RateLimited",
        "IssueOutcome::Verified",
        "meta_whatsapp_rs::client::authentication::VerifyOutcome::CoolingDown",
        "meta_whatsapp_rs::inbox::Inbox::publish",
        "OtpConfig::pepper",
    ] {
        assert!(bad(wrong), "{wrong}");
    }

    // Links.
    let file = repo().join("skills/meta-whatsapp-rs/SKILL.md");
    assert!(link_problem(&file, "references/nope.md").is_some());
    assert!(link_problem(&file, "../meta-whatsapp-rs-errors/SKILL.md").is_some());
    assert!(link_problem(&file, &format!("{GITHUB}blob/main/NOPE.md")).is_some());
    assert!(link_problem(&file, &format!("{GITHUB}blob/main/OPEN_QUESTIONS.md#nope")).is_some());
    assert!(
        link_problem(
            &file,
            &format!("{GITHUB}blob/main/OPEN_QUESTIONS.md#webhooks")
        )
        .is_none()
    );
}

#[test]
fn the_code_checks_reject_known_bad_input() {
    // Fences.
    assert!(fence_problems("```rust\nlet a = 1;\n```\n").is_empty());
    for bad_fence in [
        "```rs\nlet a = 1;\n```\n",
        "```rust,ignore\nlet a = 1;\n```\n",
        "```\nlet a = 1;\n```\n",
        "~~~rust\nlet a = 1;\n~~~\n",
        "```rust\nlet a = 1;\n",
    ] {
        assert!(!fence_problems(bad_fence).is_empty(), "{bad_fence}");
    }

    // Example files: code the gate would quote without compiling it.
    assert!(uncompiled_code("#[cfg(test)]\nmod tests {}\n", SKILL_EXAMPLE_CFGS).is_empty());
    for hidden in [
        "/*\nlet a = 1;\n*/\n",
        "#[cfg(any())]\nfn f() {}\n",
        "#[cfg(not(feature = \"postgres\"))]\nfn f() {}\n",
        "macro_rules! skip { ($($t:tt)*) => {}; }\n",
        "#[cfg_attr(test, cfg(any()))]\nfn f() {}\n",
    ] {
        assert!(
            !uncompiled_code(hidden, SKILL_EXAMPLE_CFGS).is_empty(),
            "{hidden}"
        );
    }
    // The crate's examples: the postgres arms, and nothing else.
    for arm in [
        "#[cfg(feature = \"postgres\")]\nfn f() {}\n",
        "#[cfg(not(feature = \"postgres\"))]\nfn f() {}\n",
        "#[cfg_attr(not(feature = \"postgres\"), allow(clippy::unused_async))]\nfn f() {}\n",
    ] {
        assert!(uncompiled_code(arm, CRATE_EXAMPLE_CFGS).is_empty(), "{arm}");
    }
    for hidden in [
        "#[cfg(test)]\nmod tests {}\n",
        "#[cfg(feature = \"redis\")]\nfn f() {}\n",
        "macro_rules! skip { ($($t:tt)*) => {}; }\n",
        "/*\nlet a = 1;\n*/\n",
    ] {
        assert!(
            !uncompiled_code(hidden, CRATE_EXAMPLE_CFGS).is_empty(),
            "{hidden}"
        );
    }
}

#[test]
fn excerpts_quote_compiled_code_only() {
    // Excerpts quote compiled code only: never the inside of a string or a
    // comment, nor an item under a `cfg` that --all-features never enables.
    let features: HashSet<String> = ["postgres", "flows-endpoint"].map(str::to_owned).into();
    let quotes = |source: &str, block: &[&str]| {
        is_excerpt(
            &quotable_lines(source, &features, Built::Sometimes),
            &[block.to_vec()],
        )
    };
    let quotes_crate = |source: &str, block: &[&str]| {
        is_excerpt(
            &quotable_lines(source, &features, CRATE_EXAMPLE_TEST),
            &[block.to_vec()],
        )
    };
    let raw = "fn page() -> &'static str {\n    r#\"\n    fn fake() -> u8 { 1 }\n    \"#\n}\n";
    assert!(quotes(raw, &["fn page() -> &'static str {"]));
    assert!(!quotes(raw, &["fn fake() -> u8 { 1 }"]));
    assert!(!quotes(raw, &["r#\"", "fn fake() -> u8 { 1 }"]));
    // A lone `"` inside a raw string does not end it (a plain string would).
    let lone = "let s = r#\"\nsay \"hi\nfn fake() {}\n\"#;\n";
    assert!(!quotes(lone, &["fn fake() {}"]));
    let raw_bytes = "let body = br##\"{\n\"a\": 1\n}\"##;\nlet next = 2;\n";
    assert!(!quotes(raw_bytes, &["\"a\": 1"]));
    assert!(quotes(raw_bytes, &["let next = 2;"]));
    let plain = "let s = \"first line\nfn fake() {}\";\nlet t = '\\'';\nlet u = '{';\nlet q = '\"';\nfn real() {}\n";
    assert!(!quotes(plain, &["fn fake() {}\";"]));
    assert!(quotes(plain, &["fn real() {}"]), "char literals end");
    let comment = "/*\nfn fake() {}\n*/\nfn real() {}\n";
    assert!(!quotes(comment, &["fn fake() {}"]));
    assert!(quotes(comment, &["fn real() {}"]));
    // C strings, raw or not, are strings too (security review L1).
    // (A lone `"` inside a raw one must not end it.)
    for c_string in [
        "let s = cr#\"\nsay \"hi\nfn fake() {}\n\"#;\nfn real() {}\n",
        "let s = cr\"\nfn fake() {}\n\";\nfn real() {}\n",
        "let s = c\"\nfn fake() {}\n\";\nfn real() {}\n",
    ] {
        assert!(!quotes(c_string, &["fn fake() {}"]), "{c_string}");
        assert!(quotes(c_string, &["fn real() {}"]), "{c_string}");
    }
    // An escaped quote does not end a string; a nested block comment ends
    // at its own `*/`.
    let escaped = "let s = \"say \\\"hi\nfn fake() {}\n\";\nfn real() {}\n";
    assert!(!quotes(escaped, &["fn fake() {}"]));
    assert!(quotes(escaped, &["fn real() {}"]));
    let nested = "/*\n/* inner */\nfn fake() {}\n*/\nfn real() {}\n";
    assert!(!quotes(nested, &["fn fake() {}"]));
    assert!(quotes(nested, &["fn real() {}"]));
    // An `if … else` under a `cfg` is removed whole, `else` branch included.
    let branches =
        "#[cfg(feature = \"nope\")]\nif a {\n    one();\n} else {\n    fake();\n}\nreal();\n";
    assert!(!quotes(branches, &["fake();"]));
    assert!(quotes(branches, &["real();"]));
    // A `cfg(` inside `cfg_attr` removes the item: never quotable.
    let attr = "#[cfg_attr(test, cfg(any()))]\nfn fake() {\n    one();\n}\nfn real() {}\n";
    assert!(!quotes(attr, &["fn fake() {"]));
    assert!(!quotes(attr, &["one();"]));
    assert!(quotes(attr, &["fn real() {}"]));
    // `cfg(test)` holds in a skill's examples, never in the crate's.
    let tests = "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\nfn real() {}\n";
    assert!(quotes(tests, &["fn t() {}"]));
    assert!(!quotes_crate(tests, &["fn t() {}"]));
    assert!(quotes_crate(tests, &["fn real() {}"]));
    // The `stores()` shape of crates/meta-whatsapp-rs/examples: a `not(feature)` arm.
    let gated = "match url {\n    #[cfg(feature = \"postgres\")]\n    Ok(url) => {\n        \
                 connect(url)\n    }\n    #[cfg(not(feature = \"postgres\"))]\n    \
                 Ok(_) => bail!(\"lacks postgres\"),\n    Err(_) => memory(),\n}\n";
    assert!(quotes(gated, &["Ok(url) => {", "connect(url)", "}"]));
    assert!(!quotes(gated, &["Ok(_) => bail!(\"lacks postgres\"),"]));
    assert!(quotes(gated, &["Err(_) => memory(),"]));
    for never in [
        "#[cfg(feature = \"nope\")]",
        "#[cfg(windows)]",
        "#[cfg(any())]",
        "#[cfg(not(feature = \"flows-endpoint\"))]",
        "#[cfg(all(test, feature = \"nope\"))]",
    ] {
        let source =
            format!("{never}\n#[allow(dead_code)]\nfn fake() {{\n    one();\n}}\nfn real() {{}}\n");
        assert!(!quotes(&source, &["fn fake() {"]), "{never}");
        assert!(!quotes(&source, &["one();"]), "{never}");
        assert!(quotes(&source, &["fn real() {}"]), "{never}");
    }
    assert!(!quotes(
        "#![cfg(feature = \"nope\")]\nfn fake() {}\n",
        &["fn fake() {}"]
    ));
    for built in [
        "#[cfg(test)]",
        "#[cfg(feature = \"flows-endpoint\")]",
        "#[cfg(any(test, feature = \"nope\"))]",
        "#[cfg_attr(not(feature = \"postgres\"), allow(clippy::unused_async))]",
    ] {
        let source = format!("{built}\nfn real() {{\n    one();\n}}\n");
        assert!(quotes(&source, &["fn real() {", "one();"]), "{built}");
    }
}

#[test]
fn cfg_predicates_and_scopes_parse_as_built() {
    let features: HashSet<String> = ["postgres", "flows-endpoint"].map(str::to_owned).into();
    let eval = |predicate: &str| eval_cfg(predicate, &features, Built::Sometimes);
    assert_eq!(eval("feature = \"postgres\""), Built::Always);
    assert_eq!(eval("not(test)"), Built::Sometimes);
    assert_eq!(eval("feature ="), Built::Never);
    assert_eq!(eval("not(feature = \"x\", test)"), Built::Never);
    assert_eq!(eval("not(any())"), Built::Always);
    assert_eq!(eval("not(feature = \"postgres\") "), Built::Never);
    assert_eq!(eval("any(feature = \"postgres\", test)"), Built::Always);
    assert_eq!(
        eval("all(feature = \"postgres\") x"),
        Built::Never,
        "trailing tokens"
    );
    assert_eq!(
        eval("target_os = \"postgres\""),
        Built::Never,
        "only `feature` keys"
    );
    assert_eq!(
        eval_cfg("not(test)", &features, Built::Never),
        Built::Always
    );

    // Scope parsing survives braces and keywords inside literals.
    let members = scoped_members(&tokens(&code_only(
        "enum A { X, Y(u8) }\nfn f() { let s = \"} enum B { Z }\"; let c = '{'; }\nimpl A { fn g() {} }\n",
    )))
    .members;
    assert_eq!(
        members
            .get("A")
            .map(|m| m.iter().map(String::as_str).collect::<BTreeSet<_>>()),
        Some(BTreeSet::from(["X", "Y", "g"]))
    );
    assert!(!members.contains_key("B"));
    // C strings, raw ones included, are strings to it too.
    let members = scoped_members(&tokens(&code_only(
        "fn f() { let s = cr#\"say \"} enum B { Z }\"#; let t = c\"} enum C { W }\"; }\nenum A { X }\n",
    )))
    .members;
    assert!(members.contains_key("A"), "{members:?}");
    assert!(
        !members.contains_key("B") && !members.contains_key("C"),
        "{members:?}"
    );
}

// ─── The service's skills (`meta-whatsapp-rs-server*`) ───────────────────
//
// Skills for callers of meta-whatsapp-server speak HTTP, not Rust
// (docs/design/server.md, section 9, "the skills gate for HTTP callers"):
//
// - a ```` ```ts ```` block is an excerpt of the skill's own
//   `examples/*.ts`, which `just skills-ts` type-checks (Node pinned)
//   against types generated from the committed OpenAPI document;
// - backticked routes, schemas, error codes and field names in the prose,
//   the `curl` routes of its ```` ```bash ```` blocks and the error bodies
//   of its ```` ```json ```` blocks are checked against that document, and
//   backticked environment variables against the service's source.

/// Every skill for the service's callers starts with this name.
/// `tools/skills-ts/tsconfig.json` type-checks exactly their examples.
const SERVER_SKILLS: &str = "meta-whatsapp-rs-server";

/// The committed OpenAPI document of the service.
const SERVER_SPEC: &str = "crates/meta-whatsapp-server/openapi/v1.json";

/// The service's source: the environment variables it reads are string
/// literals there.
const SERVER_SRC: &str = "crates/meta-whatsapp-server/src";

/// The public listener's operations, which the document leaves out
/// (Meta's contract, not the callers'); `PUBLIC_OPERATIONS` in
/// crates/meta-whatsapp-server/src/api/mod.rs, checked below (and the
/// service's own test sends each to its router).
const SERVER_PUBLIC_ROUTES: &[(&str, &str)] = &[
    ("GET", "/webhooks/meta"),
    ("POST", "/webhooks/meta"),
    ("GET", "/livez"),
];

const HTTP_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];

/// Whether `path` is (in) a skill for the service's callers.
fn in_server_skill(path: &Path) -> bool {
    owning_skill(path).is_some_and(|dir| {
        dir.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(SERVER_SKILLS))
    })
}

/// A skill's `examples/*.ts` (generated `*.d.ts` left out).
fn ts_files(dir: &Path) -> Vec<PathBuf> {
    sorted_dir(dir)
        .into_iter()
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.ends_with(".ts") && !name.ends_with(".d.ts")
        })
        .collect()
}

/// What a server skill may cite.
struct Spec {
    /// `(METHOD, path template)`.
    operations: HashSet<(String, String)>,
    /// Schema names.
    schemas: HashSet<String>,
    /// Error codes (`ErrorCode`).
    codes: HashSet<String>,
    /// Property and parameter names, enum values.
    words: HashSet<String>,
    /// `ErrorObject`'s properties.
    error_fields: HashSet<String>,
    /// Environment variables the service's source names.
    env: HashSet<String>,
}

impl Spec {
    fn load() -> Self {
        Self::from(&read(&repo().join(SERVER_SPEC)), &server_sources())
    }

    fn from(document: &str, sources: &str) -> Self {
        let doc: serde_json::Value = serde_json::from_str(document).unwrap();
        let mut spec = Spec {
            operations: HashSet::new(),
            schemas: HashSet::new(),
            codes: HashSet::new(),
            words: HashSet::new(),
            error_fields: HashSet::new(),
            env: HashSet::new(),
        };
        for (path, item) in doc["paths"].as_object().unwrap() {
            for (method, operation) in item.as_object().unwrap() {
                spec.operations
                    .insert((method.to_uppercase(), path.clone()));
                for parameter in operation["parameters"].as_array().into_iter().flatten() {
                    if let Some(name) = parameter["name"].as_str() {
                        spec.words.insert(name.to_owned());
                    }
                }
            }
        }
        for (method, path) in SERVER_PUBLIC_ROUTES {
            spec.operations
                .insert(((*method).to_owned(), (*path).to_owned()));
        }
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        spec.schemas.extend(schemas.keys().cloned());
        collect_words(&doc["components"], &mut spec.words);
        // The codes: `KnownErrorCode` (what the open set `ErrorCode`
        // refers to), or an `ErrorCode` enum.
        let known = schemas
            .get("KnownErrorCode")
            .and_then(|k| k["enum"].as_array())
            .or_else(|| schemas["ErrorCode"]["enum"].as_array());
        spec.codes.extend(
            known
                .expect("ErrorCode lists the codes")
                .iter()
                .map(|c| c.as_str().unwrap().to_owned()),
        );
        spec.error_fields.extend(
            schemas["ErrorObject"]["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned(),
        );
        // String literals of the source shaped like environment variables.
        for piece in sources.split('"').skip(1).step_by(2) {
            if is_env_name(piece) {
                spec.env.insert(piece.to_owned());
            }
        }
        spec
    }

    /// Whether `method` (any, for `None`) on `path` is an operation. A path
    /// segment matches a `{parameter}` segment of the template when it is
    /// not empty; `{…}`, `$VAR` and `<…>` segments of `path` match only
    /// parameters.
    fn route(&self, method: Option<&str>, path: &str) -> bool {
        let path = path.split(['?', '#']).next().unwrap_or_default();
        let segments: Vec<&str> = path.split('/').collect();
        self.operations.iter().any(|(m, template)| {
            let parts: Vec<&str> = template.split('/').collect();
            method.is_none_or(|method| method == m)
                && parts.len() == segments.len()
                && parts.iter().zip(&segments).all(|(t, s)| {
                    let parameter = t.starts_with('{');
                    let placeholder =
                        s.starts_with('{') || s.starts_with('$') || s.starts_with('<');
                    if parameter {
                        !s.is_empty()
                    } else {
                        !placeholder && t == s
                    }
                })
        })
    }
}

/// Property names and enum values anywhere under `value`.
fn collect_words(value: &serde_json::Value, words: &mut HashSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(props) = map.get("properties").and_then(|p| p.as_object()) {
                words.extend(props.keys().cloned());
            }
            for value in map
                .get("enum")
                .and_then(|e| e.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(s) = value.as_str() {
                    words.insert(s.to_owned());
                }
            }
            map.values().for_each(|v| collect_words(v, words));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_words(v, words)),
        _ => {}
    }
}

/// Every `.rs` file of the service, concatenated.
fn server_sources() -> String {
    fn walk(dir: &Path, out: &mut String) {
        for path in sorted_dir(dir) {
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push_str(&read(&path));
            }
        }
    }
    let mut out = String::new();
    walk(&repo().join(SERVER_SRC), &mut out);
    assert!(!out.is_empty(), "no service source found");
    out
}

/// `WA_…`, `DATABASE_URL`, `RUST_LOG`: upper case, digits and `_`, with a
/// `_`.
fn is_env_name(s: &str) -> bool {
    s.len() > 2
        && s.contains('_')
        && s.starts_with(|c: char| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// The problems of a backticked span of a server skill, or `None` when it
/// is not HTTP-shaped (the Rust checks then apply to it).
fn server_span_problems(spec: &Spec, span: &str) -> Option<Vec<String>> {
    let span = span.trim();
    let fail = |why: String| Some(vec![why]);
    // `GET /v1/numbers`, `GET|PATCH /v1/numbers/{pn}/profile`
    if let Some((methods, path)) = span.split_once(' ')
        && path.starts_with('/')
        && !path.contains(' ')
        && methods.split('|').all(|m| HTTP_METHODS.contains(&m))
    {
        return Some(
            methods
                .split('|')
                .filter(|m| !spec.route(Some(m), path))
                .map(|m| format!("route `{m} {path}` is not in {SERVER_SPEC}"))
                .collect(),
        );
    }
    // `/v1/openapi.json`, `/readyz`
    if span.starts_with('/') && !span.contains(' ') {
        return if spec.route(None, span) {
            Some(Vec::new())
        } else {
            fail(format!("path `{span}` is not in {SERVER_SPEC}"))
        };
    }
    if is_env_name(span) {
        let base = span.strip_suffix("_FILE").unwrap_or(span);
        return if spec.env.contains(span) || spec.env.contains(base) {
            Some(Vec::new())
        } else {
            fail(format!(
                "`{span}` is not an environment variable the service reads ({SERVER_SRC})"
            ))
        };
    }
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    if span.starts_with(|c: char| c.is_ascii_lowercase()) && span.chars().all(word) {
        return if spec.codes.contains(span) || spec.words.contains(span) {
            Some(Vec::new())
        } else {
            fail(format!(
                "`{span}` is neither an error code, a field, a parameter nor a value of {SERVER_SPEC}"
            ))
        };
    }
    if is_camel(span) && span.chars().all(word) {
        return if spec.schemas.contains(span) {
            Some(Vec::new())
        } else {
            fail(format!("`{span}` is not a schema of {SERVER_SPEC}"))
        };
    }
    None
}

/// The `curl` commands of a ```` ```bash ```` block, continuation lines
/// joined.
fn curl_commands(block: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut current = String::new();
    for line in block.lines() {
        let line = line.split(" #").next().unwrap_or_default();
        let (text, continued) = match line.trim_end().strip_suffix('\\') {
            Some(text) => (text, true),
            None => (line, false),
        };
        current.push_str(text.trim());
        current.push(' ');
        if !continued {
            if current.contains("curl ") {
                commands.push(current.trim().to_owned());
            }
            current.clear();
        }
    }
    commands
}

/// Problems with the `curl` routes of a bash block: each names a method
/// and path of the document.
fn curl_problems(spec: &Spec, block: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for command in curl_commands(block) {
        let words: Vec<String> = command
            .split_whitespace()
            .map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_owned())
            .collect();
        let explicit = words
            .iter()
            .position(|w| w == "-X" || w == "--request")
            .and_then(|i| words.get(i + 1))
            .cloned();
        let body = words
            .iter()
            .any(|w| w == "-d" || w.starts_with("--data") || w.starts_with("-d"));
        let method = explicit.unwrap_or_else(|| if body { "POST" } else { "GET" }.to_owned());
        let Some(url) = words
            .iter()
            .find(|w| w.contains("/v1/") || w.contains("://") || w.starts_with("$WA_SERVER"))
        else {
            problems.push(format!("`{command}`: no URL"));
            continue;
        };
        let path = match url.split_once("://") {
            Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
            None => url.find('/').map_or("/", |i| &url[i..]),
        };
        if !spec.route(Some(&method), path) {
            problems.push(format!(
                "`{method} {path}` (a curl) is not in {SERVER_SPEC}"
            ));
        }
    }
    problems
}

/// Problems with an error body in a json block: its code and fields are
/// the document's.
fn json_error_problems(spec: &Spec, block: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(block) else {
        return Vec::new();
    };
    let Some(error) = value.get("error").and_then(|e| e.as_object()) else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    match error.get("code").and_then(|c| c.as_str()) {
        Some(code) if spec.codes.contains(code) => {}
        other => problems.push(format!("error code {other:?} is not in {SERVER_SPEC}")),
    }
    for field in error.keys() {
        if !spec.error_fields.contains(field) {
            problems.push(format!("error field `{field}` is not in {SERVER_SPEC}"));
        }
    }
    problems
}

/// The blocks of `markdown` labeled `language`: first line number and
/// text.
fn blocks_of<'a>(markdown: &'a str, language: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut open: Option<(usize, Vec<&'a str>)> = None;
    for (i, line) in markdown.lines().enumerate() {
        let trimmed = line.trim();
        match &mut open {
            None if trimmed.strip_prefix("```") == Some(language) => {
                open = Some((i + 1, Vec::new()));
            }
            None => {}
            Some(_) if trimmed == "```" => {
                let (start, lines) = open.take().unwrap();
                out.push((start, lines.join("\n")));
            }
            Some((_, lines)) => lines.push(line),
        }
    }
    out
}

/// The paragraphs of trimmed lines of a block's text.
fn paragraphs(text: &str) -> Vec<Vec<&str>> {
    let mut out = Vec::new();
    let mut paragraph = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                out.push(std::mem::take(&mut paragraph));
            }
        } else {
            paragraph.push(trimmed);
        }
    }
    if !paragraph.is_empty() {
        out.push(paragraph);
    }
    out
}

/// What a TypeScript example may not hold: code a block could quote that
/// the type-check does not see as code (a block comment, a template
/// literal spanning lines).
fn ts_example_problems(source: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (i, line) in source.lines().enumerate() {
        if line.contains("/*") {
            problems.push(format!("line {}: block comment (use //)", i + 1));
        }
        if line.matches('`').count() % 2 == 1 {
            problems.push(format!("line {}: a template literal spans lines", i + 1));
        }
    }
    problems
}

#[test]
fn server_skills_cite_the_document_and_the_source() {
    let spec = Spec::load();
    // The public operations this test adds are the service's, method and
    // path (compared without whitespace: rustfmt lays the list out).
    let api = read(&repo().join(SERVER_SRC).join("api/mod.rs"));
    let squeeze = |s: &str| s.split_whitespace().collect::<String>().replace(",]", "]");
    let declared = api
        .split_once("pub const PUBLIC_OPERATIONS")
        .and_then(|(_, rest)| rest.split_once("];"))
        .map(|(list, _)| squeeze(&format!("pub const PUBLIC_OPERATIONS{list}];")))
        .expect("PUBLIC_OPERATIONS in the service's api/mod.rs");
    let expected = squeeze(&format!(
        "pub const PUBLIC_OPERATIONS: [(&str, &str); {}] = [{}];",
        SERVER_PUBLIC_ROUTES.len(),
        SERVER_PUBLIC_ROUTES
            .iter()
            .map(|(m, p)| format!("(\"{m}\", \"{p}\")"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    assert_eq!(
        declared, expected,
        "SERVER_PUBLIC_ROUTES is not the service's PUBLIC_OPERATIONS"
    );
    let mut failures = String::new();
    let mut checked = 0;
    for (path, markdown) in skill_markdown() {
        if !in_server_skill(&path) {
            continue;
        }
        for (line, prose) in prose_lines(&markdown) {
            for span in code_spans(prose) {
                if let Some(problems) = server_span_problems(&spec, span) {
                    checked += 1;
                    for problem in problems {
                        writeln!(failures, "{}:{line}: {problem}", rel(&path)).unwrap();
                    }
                }
            }
        }
        for (line, block) in blocks_of(&markdown, "bash") {
            checked += curl_commands(&block).len();
            for problem in curl_problems(&spec, &block) {
                writeln!(failures, "{}:{line}: {problem}", rel(&path)).unwrap();
            }
        }
        for (line, block) in blocks_of(&markdown, "json") {
            for problem in json_error_problems(&spec, &block) {
                writeln!(failures, "{}:{line}: {problem}", rel(&path)).unwrap();
            }
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
    assert!(
        checked >= 20,
        "only {checked} routes, codes, fields and variables checked"
    );
}

#[test]
fn ts_blocks_are_excerpts_of_type_checked_examples() {
    let mut failures = String::new();
    let mut blocks = 0;
    let mut examples = 0;
    for skill in consumer_skills() {
        for file in ts_files(&skill.dir.join("examples")) {
            examples += 1;
            if !skill.name.starts_with(SERVER_SKILLS) {
                writeln!(
                    failures,
                    "{}: only {SERVER_SKILLS}* skills' TypeScript is type-checked \
                     (tools/skills-ts/tsconfig.json)",
                    rel(&file)
                )
                .unwrap();
            }
            for problem in ts_example_problems(&read(&file)) {
                writeln!(failures, "{}: {problem}", rel(&file)).unwrap();
            }
        }
    }
    for (path, markdown) in skill_markdown() {
        let own: Vec<String> = owning_skill(&path)
            .map(|dir| ts_files(&dir.join("examples")))
            .unwrap_or_default()
            .iter()
            .map(|f| read(f))
            .collect();
        for (line, block) in blocks_of(&markdown, "ts") {
            blocks += 1;
            let wanted = paragraphs(&block);
            let found = own.iter().any(|source| {
                let lines: Vec<&str> = source
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect();
                is_excerpt(&lines, &wanted)
            });
            if !found {
                writeln!(
                    failures,
                    "{}:{line}: not an excerpt of the skill's examples/*.ts (edit the example, \
                     then copy it):\n{block}\n",
                    rel(&path)
                )
                .unwrap();
            }
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
    assert!(
        examples >= 1 && blocks >= 1,
        "{examples} examples, {blocks} ts blocks"
    );
    // The type-check covers exactly the server skills.
    let tsconfig = read(&repo().join("tools/skills-ts/tsconfig.json"));
    assert!(
        tsconfig.contains(&format!("\"../../skills/{SERVER_SKILLS}*/examples/*.ts\"")),
        "tools/skills-ts/tsconfig.json does not include the server skills' examples"
    );
}

/// Node is pinned once: the major of `tools/skills-ts/.nvmrc` (what CI
/// installs) is the `engines` of its package.json and the devcontainer's
/// base image, so `just skills-ts` runs alike everywhere.
#[test]
fn node_is_pinned_once() {
    let nvmrc = read(&repo().join("tools/skills-ts/.nvmrc"));
    let version = nvmrc.trim();
    let major = version.split('.').next().unwrap();
    assert!(
        version.split('.').count() == 3 && version.split('.').all(|p| p.parse::<u32>().is_ok()),
        "tools/skills-ts/.nvmrc: an exact version, e.g. 24.20.0, not {version:?}"
    );
    let package = read(&repo().join("tools/skills-ts/package.json"));
    assert!(
        package.contains(&format!("\"node\": \"{major}.x\"")),
        "tools/skills-ts/package.json: engines.node is not {major}.x"
    );
    let dockerfile = read(&repo().join(".devcontainer/Dockerfile"));
    assert!(
        dockerfile.contains(&format!("FROM node:{major}-")),
        ".devcontainer/Dockerfile: its node base is not Node {major}"
    );
}

/// A small document for the checks' own tests.
const TEST_DOCUMENT: &str = r#"{"openapi": "3.1.0",
      "paths": {
        "/v1/numbers/{pn}": {"get": {"parameters": [{"name": "pn", "in": "path"}]}},
        "/v1/admin/tenants": {"post": {}}
      },
      "components": {"schemas": {
        "ErrorCode": {"type": "string", "enum": ["not_found", "reconnect_required"]},
        "ErrorObject": {"properties": {"code": {}, "message": {}}},
        "TenantView": {"properties": {"status": {"enum": ["active"]}}}
      }}}"#;

#[test]
fn the_server_span_checks_reject_known_bad_input() {
    let spec = Spec::from(
        TEST_DOCUMENT,
        r#"r.plain("WA_SERVER_ENV"); r.secret("DATABASE_URL")"#,
    );
    let ok = |span: &str| server_span_problems(&spec, span) == Some(Vec::new());
    let bad = |span: &str| server_span_problems(&spec, span).is_some_and(|p| !p.is_empty());
    for span in [
        "GET /v1/numbers/{pn}",
        "GET /v1/numbers/106540352242922",
        "POST /v1/admin/tenants",
        "/v1/numbers/{pn}",
        "GET /livez",
        "GET /webhooks/meta",
        "POST /webhooks/meta",
        "not_found",
        "status",
        "active",
        "pn",
        "TenantView",
        "WA_SERVER_ENV",
        "DATABASE_URL_FILE",
    ] {
        assert!(ok(span), "{span}");
    }
    for span in [
        "PATCH /v1/numbers/{pn}",
        "GET /v1/numbers",
        "GET|POST /v1/admin/tenants",
        "PUT /webhooks/meta",
        "POST /livez",
        "/v1/numberz/{pn}",
        "reconect_required",
        "TenantViews",
        "WA_SERVER_ENVIRONMENT",
    ] {
        assert!(bad(span), "{span}");
    }
    assert!(
        server_span_problems(&spec, "Error::kind").is_none(),
        "Rust stays Rust's"
    );
}

#[test]
fn the_server_fence_checks_reject_known_bad_input() {
    let spec = Spec::from(TEST_DOCUMENT, "");

    assert!(
        curl_problems(
            &spec,
            "curl -sS \"$WA_SERVER/v1/numbers/$PN\" \\\n  -H \"Authorization: Bearer $KEY\""
        )
        .is_empty()
    );
    assert!(
        curl_problems(
            &spec,
            "curl -sS -X POST http://127.0.0.1:8081/v1/admin/tenants -d '{}'"
        )
        .is_empty()
    );
    assert!(
        !curl_problems(
            &spec,
            "curl -sS -X DELETE http://127.0.0.1:8081/v1/admin/tenants"
        )
        .is_empty()
    );
    assert!(
        !curl_problems(&spec, "curl -sS http://127.0.0.1:8081/v1/admin/tenants").is_empty(),
        "GET is not documented"
    );
    assert!(!curl_problems(&spec, "curl -sS \"$WA_SERVER/v1/tenants\"").is_empty());

    assert!(
        json_error_problems(&spec, r#"{"error": {"code": "not_found", "message": "x"}}"#)
            .is_empty()
    );
    assert!(
        !json_error_problems(&spec, r#"{"error": {"code": "gone", "message": "x"}}"#).is_empty()
    );
    assert!(
        !json_error_problems(&spec, r#"{"error": {"code": "not_found", "reason": "x"}}"#)
            .is_empty()
    );

    assert!(ts_example_problems("const a = `x`;\n// ok\n").is_empty());
    assert!(!ts_example_problems("/* hidden\nconst a = 1;\n*/\n").is_empty());
    assert!(!ts_example_problems("const a = `\nconst b = 1;\n`;\n").is_empty());
    let example = [
        "const a = 1;",
        "",
        "export function f() {",
        "return a;",
        "}",
    ];
    let lines: Vec<&str> = example.iter().copied().filter(|l| !l.is_empty()).collect();
    assert!(is_excerpt(
        &lines,
        &paragraphs("export function f() {\n  return a;\n}")
    ));
    assert!(!is_excerpt(
        &lines,
        &paragraphs("export function f() {\n  return b;\n}")
    ));
}
