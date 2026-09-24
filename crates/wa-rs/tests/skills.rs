//! The consumer skills in `skills/` instruct the coding agents of the
//! repositories that use wa-rs. A wrong skill makes those agents generate
//! broken code, confidently and at scale, so these tests keep every skill
//! true of this commit:
//!
//! - **Compiled code.** Every `skills/<name>/examples/*.rs` is compiled into
//!   this test crate (see `tests/skill_examples/mod.rs`) and its tests run.
//!   Every ```` ```rust ```` block in `skills/**/*.md` is an excerpt of one
//!   compiled file — the skill's own `examples/*.rs` or a
//!   `crates/wa-rs/examples/*.rs` program: all its non-blank lines, trimmed,
//!   each paragraph contiguous in the file and the paragraphs in order (the
//!   README rule of `tests/readme.rs`, plus order).
//! - **Frontmatter** the `npx skills` CLI accepts: `name` is lowercase
//!   words joined by hyphens and equals the directory, `description` is a
//!   double-quoted string of at most 1024 characters that says when to load
//!   the skill, and no unquoted value contains `": "` (YAML reads it as a
//!   nested mapping and the CLI skips the skill). Consumer skills are never
//!   `internal`; the developer skills in `.claude/skills/` always are, so
//!   `npx skills add vaam-apps/wa-rs` does not offer them.
//! - **Links**: every relative link in `skills/**` resolves (and stays
//!   inside its skill, which is installed on its own), every link to a file
//!   of this repository on GitHub names a file that exists, and every
//!   `#anchor` into a Markdown file names one of its headings.
//! - **Names**: every backticked Rust path, type, function, constant or
//!   `snake_case` name in the prose is defined in `crates/**/*.rs`, unless
//!   `skills/.allowlist` lists it (placeholders, other crates' names); every
//!   backticked `wa-rs-*` skill name exists.
//! - **Shape**: each skill is stamped under its title, stays short, links
//!   its example files, and is routed to from the `wa-rs` hub and from
//!   `skills/README.md`.
//!
//! The stamp's commit (`Verified against wa-rs <sha>`) must exist and be an
//! ancestor of HEAD: that needs git, so `just skills-check` checks it.

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
                         vaam-apps/wa-rs` offers it to consumers",
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

// ─── Stamp and shape ─────────────────────────────────────────────────────

/// Whether `line` is `> **Verified against wa-rs <40 hex> (<YYYY-MM-DD>).**…`.
fn is_stamp(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("> **Verified against wa-rs ") else {
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

/// Longest a `SKILL.md` may be; longer tables go to `references/`.
const MAX_SKILL_LINES: usize = 160;

#[test]
fn every_skill_is_stamped_short_and_routed_to() {
    let skills = consumer_skills();
    let hub = read(&repo().join("skills/wa-rs/SKILL.md"));
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
                "{file}: the line under the title must be `> **Verified against wa-rs \
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
            "## What wa-rs does not do",
            "## Related skills",
        ] {
            if !skill.markdown.lines().any(|l| l.trim() == section) {
                writeln!(failures, "{file}: no `{section}` section").unwrap();
            }
        }
        let quoted = format!("`{}`", skill.name);
        if skill.name != "wa-rs" && !hub.contains(&quoted) {
            writeln!(
                failures,
                "skills/wa-rs/SKILL.md: does not route to {quoted}"
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
        for example in rust_files(&skill.dir.join("examples")) {
            let name = example.file_name().unwrap().to_string_lossy();
            if !skill.markdown.contains(&format!("](examples/{name})")) {
                writeln!(failures, "{file}: does not link examples/{name}").unwrap();
            }
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}

// ─── Compiled code ───────────────────────────────────────────────────────

const EXAMPLES_MOD: &str = "crates/wa-rs/tests/skill_examples/mod.rs";

#[test]
fn every_skill_example_is_compiled() {
    let module = read(&repo().join(EXAMPLES_MOD));
    let included: BTreeSet<PathBuf> = module
        .lines()
        .filter_map(|l| l.trim().strip_prefix("#[path = \""))
        .filter_map(|l| l.strip_suffix("\"]"))
        .map(|p| {
            repo()
                .join("crates/wa-rs/tests/skill_examples")
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

/// Trimmed, non-blank lines.
fn lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
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
    let crate_examples: Vec<(PathBuf, String)> = rust_files(&repo().join("crates/wa-rs/examples"))
        .into_iter()
        .map(|p| {
            let text = read(&p);
            (p, text)
        })
        .collect();
    let mut checked = 0;
    let mut failures = String::new();
    for (path, markdown) in skill_markdown() {
        let own: Vec<(PathBuf, String)> = owning_skill(&path)
            .map(|dir| rust_files(&dir.join("examples")))
            .unwrap_or_default()
            .into_iter()
            .map(|p| {
                let text = read(&p);
                (p, text)
            })
            .collect();
        for (line, paragraphs) in rust_blocks(&markdown) {
            checked += 1;
            let found = own
                .iter()
                .chain(&crate_examples)
                .any(|(_, source)| is_excerpt(&lines(source), &paragraphs));
            if !found {
                writeln!(
                    failures,
                    "{}:{line}: not an excerpt of the skill's examples/*.rs or of \
                     crates/wa-rs/examples/*.rs (edit the example, then copy it):\n{}\n",
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

const GITHUB: &str = "https://github.com/vaam-apps/wa-rs/";

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
        file
    }
}

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
        let this = repo().join("crates/wa-rs/tests/skills.rs");
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

    /// Whether `member` is a method, field, variant or constant of `head`.
    fn member(&self, head: &str, member: &str) -> bool {
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

    /// Resolve `wa_rs::a::b::Item::member` module by module.
    fn path(&self, segments: &[&str]) -> Result<(), String> {
        let crates = repo().join("crates");
        let (krate, mut rest) = match segments {
            ["wa_rs", module, rest @ ..] if FACADE.iter().any(|(m, _)| m == module) => {
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
        for member in members {
            if !(self.function(member)
                || self.declared(member)
                || self.constant(member)
                || self.word(member))
            {
                return Err(format!("`{member}` is not defined"));
            }
        }
        Ok(())
    }
}

/// `wa_rs::<module>` → the crate it re-exports.
const FACADE: &[(&str, &str)] = &[
    ("client", "wa_client"),
    ("core", "wa_core"),
    ("webhooks", "wa_webhooks"),
    ("adapters", "wa_adapters"),
    ("typst", "wa_typst"),
];

const CRATES: &[&str] = &[
    "wa_rs",
    "wa_core",
    "wa_client",
    "wa_webhooks",
    "wa_adapters",
    "wa_typst",
];

/// Rust's own words: never wa-rs API, never worth listing.
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
        if span.starts_with("wa-rs-") && !skills.contains(span) {
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
fn the_checks_reject_known_bad_input() {
    // Frontmatter.
    let skill = |markdown: &str| Skill {
        name: "wa-rs-x".into(),
        dir: PathBuf::new(),
        markdown: markdown.into(),
    };
    let ok = "---\nname: wa-rs-x\ndescription: \"Does x. Load when y.\"\n---\n";
    assert!(frontmatter_problems(&skill(ok)).is_empty());
    for bad in [
        "---\nname: wa-rs-x\ndescription: Does x: Load when y.\n---\n",
        "---\nname: wa-rs-y\ndescription: \"Does x. Load when y.\"\n---\n",
        "---\nname: WA_rs\ndescription: \"Does x. Load when y.\"\n---\n",
        "---\nname: wa-rs-x\ndescription: \"Does x.\"\n---\n",
        "---\nname: wa-rs-x\ndescription: \"Does x. Load when y.\"\nmetadata:\n  internal: true\n---\n",
    ] {
        assert!(!frontmatter_problems(&skill(bad)).is_empty(), "{bad}");
    }
    let long = format!(
        "---\nname: wa-rs-x\ndescription: \"Load when {}\"\n---\n",
        "y".repeat(1024)
    );
    assert!(!frontmatter_problems(&skill(&long)).is_empty());

    // Stamps.
    let sha = "41fe5f9c963f4718db1362663a62de97244846ee";
    assert!(is_stamp(&format!(
        "> **Verified against wa-rs {sha} (2026-09-24).**"
    )));
    assert!(!is_stamp(
        "> **Verified against wa-rs 41fe5f9 (2026-09-24).**"
    ));
    assert!(!is_stamp(&format!(
        "> **Verified against wa-rs {sha} (24.09.2026).**"
    )));

    // Excerpts: every paragraph, contiguous, in order.
    let source = ["a", "b", "c", "d"];
    assert!(is_excerpt(&source, &[vec!["a", "b"], vec!["d"]]));
    assert!(!is_excerpt(&source, &[vec!["a", "c"]]));
    assert!(!is_excerpt(&source, &[vec!["d"], vec!["a"]]));

    // Names.
    let index = Index::build();
    let allowed = HashSet::new();
    let skills = HashSet::from(["wa-rs-errors".to_owned()]);
    let bad = |span: &str| !undefined_names(&index, &allowed, &skills, span).is_empty();
    for good in [
        "ErrorKind::CustomerServiceWindowClosed",
        "wa_rs::client::messages::OutboundMessage",
        "Recipient::phone(\"+1\")",
        "client.messages(pnid).send(&msg)",
        "DEFAULT_TIMEOUT",
        "wa-rs-errors",
    ] {
        assert!(!bad(good), "{good}");
    }
    for wrong in [
        "ErrorKind::WindowClosed",
        "wa_rs::client::messages::OutboundMesage",
        "Recipient::telephone(\"+1\")",
        "client.messages(pnid).transmit(&msg)",
        "DEFAULT_TIMEOUT_MS",
        "wa-rs-messaging",
        "TokenSafe",
        "biz_opaque_callback",
    ] {
        assert!(bad(wrong), "{wrong}");
    }

    // Links.
    let file = repo().join("skills/wa-rs/SKILL.md");
    assert!(link_problem(&file, "references/nope.md").is_some());
    assert!(link_problem(&file, "../wa-rs-errors/SKILL.md").is_some());
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
