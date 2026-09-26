//! The planning docs are tables that other documents, coding agents and
//! reviewers act on: a count that drifts, a row no item plans, an item
//! that names a milestone that does not exist, or a symbol renamed under a
//! "done" cell all send the next pull request the wrong way. These tests
//! keep the tables consistent with each other and with the code:
//!
//! - **`docs/parity.md`**: rows numbered once, 1 to N; every status is
//!   `library / service`, each done, partial, gap or n/a, or "n/a —
//!   unofficial protocol"; a service that is partial or a gap names, in
//!   parentheses, the roadmap items that bring it, and no other status
//!   does; the headline, the count table, the uncounted rows and the tally
//!   by family equal a recount.
//! - **`docs/categories.md`**: categories numbered once; the count table
//!   equals a recount; every row of `docs/coverage.md` is cited, each link
//!   lands on its row's anchor; every parity row cited exists; a side is
//!   done only when every cited parity row is done there, a gap only when
//!   every one is a gap, and the service's items are its cited rows' items.
//! - **`docs/coverage.md`**: rows numbered once, each with its anchor, each
//!   status one of the file's words.
//! - **`docs/roadmap.md`**: item ids unique; every item has its `After:`
//!   and `Decisive:` lines, and a library item its `Kind:`; every item is
//!   in exactly one wave, no earlier than what it comes after; every parity
//!   row an item cites exists and is open (partial or a gap) on some side
//!   while the item is not ticked; every library row that is partial or a
//!   gap is cited by an item, and every item a service status names cites
//!   that row.
//! - **`OPEN_QUESTIONS.md`**: entries numbered once, each decided or left
//!   open exactly once, and the header's counts equal a recount.
//! - **Citations** in the planning docs: every roadmap item id (S1, L20a,
//!   M5c3, or a family such as M5), design decision (D26) and open
//!   question (`OPEN_QUESTIONS` #26) exists.
//! - **Symbols**: every backticked Rust path in a Library or Service cell
//!   of the two tables resolves. `client::`, `webhooks::`, `core::`,
//!   `adapters::`, `typst::`, `inbox::` and `server::` paths resolve module
//!   by module in their crate to a `pub` item, then each member to a
//!   variant, field, method or constant of the type before it; other
//!   `Type::member` paths and type names resolve to a declaration anywhere
//!   in `crates/`.
//! - **Links**: every relative link, and its `#anchor`, in the root and
//!   `docs/` Markdown files resolves.
//!
//! This is consistency, not truth: whether a row that says done is done is
//! the code's to show and the reviewer's to check.
//!
//! `tests/skills.rs` has a fuller name index, private to its own test
//! binary; the resolver here is a smaller one of the same kind.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

// ─── Files ───────────────────────────────────────────────────────────────

const PARITY: &str = "docs/parity.md";
const CATEGORIES: &str = "docs/categories.md";
const COVERAGE: &str = "docs/coverage.md";
const ROADMAP: &str = "docs/roadmap.md";
const QUESTIONS: &str = "OPEN_QUESTIONS.md";
const DESIGN: &str = "docs/design/server.md";

/// The documents whose citations of items, decisions and questions must
/// resolve.
const PLANNING: &[&str] = &[
    PARITY,
    CATEGORIES,
    COVERAGE,
    ROADMAP,
    QUESTIONS,
    DESIGN,
    "AGENTS.md",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn doc(relative: &str) -> String {
    read(&repo().join(relative))
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

fn walk(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
    for path in sorted_dir(dir) {
        if path.is_dir() {
            if path.file_name().is_none_or(|n| n != "target") {
                walk(&path, extension, out);
            }
        } else if path.extension().is_some_and(|e| e == extension) {
            out.push(path);
        }
    }
}

/// Fails with every problem found, one per line.
fn assert_none(problems: &[String], what: &str) {
    assert!(problems.is_empty(), "{what}:\n{}", problems.join("\n"));
}

// ─── Markdown ────────────────────────────────────────────────────────────

/// Lines outside fenced code blocks, numbered from 1.
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

/// Inline code spans of a line.
fn code_spans(line: &str) -> Vec<&str> {
    line.split('`')
        .enumerate()
        .filter(|(i, span)| i % 2 == 1 && !span.is_empty())
        .map(|(_, span)| span)
        .collect()
}

/// `line` with its code spans blanked.
fn without_code(line: &str) -> String {
    line.split('`')
        .enumerate()
        .map(|(i, part)| if i % 2 == 0 { part } else { " " })
        .collect()
}

/// `text` without HTML tags.
fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// The rows of the tables of `markdown` whose first cell is a number (an
/// HTML anchor in it aside), with their line numbers.
fn numbered_rows(markdown: &str) -> Vec<(usize, u32, Vec<String>)> {
    prose_lines(markdown)
        .into_iter()
        .filter_map(|(line, text)| {
            let inner = text.strip_prefix("| ")?.strip_suffix(" |")?;
            let cells: Vec<String> = inner.split(" | ").map(str::to_owned).collect();
            let number = strip_tags(&cells[0]).trim().parse().ok()?;
            Some((line, number, cells))
        })
        .collect()
}

/// Problems with the numbering of `rows`: each number once, 1 to N.
fn numbering_problems<T>(what: &str, rows: &[(usize, u32, T)]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for (line, n, _) in rows {
        if !seen.insert(*n) {
            problems.push(format!("{what}:{line}: number {n} used twice"));
        }
    }
    let expected: BTreeSet<u32> = (1..=u32::try_from(rows.len()).unwrap()).collect();
    if seen != expected {
        problems.push(format!(
            "{what}: the numbers are not 1 to {} (missing {:?})",
            rows.len(),
            expected.difference(&seen).collect::<Vec<_>>()
        ));
    }
    problems
}

/// `text` without what its parentheses hold (nested ones included).
fn without_parentheses(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// The number written just before `marker`, if `marker` occurs.
fn number_before(text: &str, marker: &str) -> Option<u32> {
    let at = text.find(marker)?;
    let digits: String = text[..at]
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.chars().rev().collect::<String>().parse().ok()
}

/// The number written just after `marker`, if `marker` occurs.
fn number_after(text: &str, marker: &str) -> Option<u32> {
    let at = text.find(marker)? + marker.len();
    let digits: String = text[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The numbers of a count table's row: `| <label> | 1 | 2 | 3 |`.
fn count_row(markdown: &str, label: &str) -> Option<Vec<u32>> {
    let prefix = format!("| {label} | ");
    let line = markdown.lines().find(|l| l.starts_with(&prefix))?;
    line[prefix.len()..]
        .trim_end_matches(" |")
        .split(" | ")
        .map(|n| n.trim().parse().ok())
        .collect()
}

/// `11–13, 19–36` as numbers.
fn expand_numbers(list: &str) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    for part in list.split(", ").map(str::trim) {
        if part == "—" || part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('–') {
            let (a, b): (u32, u32) = (
                a.parse().map_err(|_| format!("`{part}`"))?,
                b.parse().map_err(|_| format!("`{part}`"))?,
            );
            if a >= b {
                return Err(format!("`{part}` is not a range"));
            }
            out.extend(a..=b);
        } else {
            out.push(part.parse().map_err(|_| format!("`{part}`"))?);
        }
    }
    Ok(out)
}

/// The parity rows `text` cites: `row 48`, `rows 17, 85–88`,
/// `rows 90's Meta side, 127`; never `coverage row 33`.
fn cited_rows(text: &str) -> BTreeSet<u32> {
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = text.to_lowercase();
    let mut rows = BTreeSet::new();
    for (at, _) in lower.match_indices("row") {
        let starts_word = lower[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        if !starts_word || lower[..at].ends_with("coverage ") {
            continue;
        }
        let after = &text[at + 3..];
        let after = after.strip_prefix('s').unwrap_or(after);
        let Some(mut rest) = after.strip_prefix(' ') else {
            continue;
        };
        loop {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let Ok(first) = digits.parse::<u32>() else {
                break;
            };
            rest = &rest[digits.len()..];
            if let Some(tail) = rest.strip_prefix('–') {
                let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
                let last: u32 = digits.parse().unwrap_or(first);
                rows.extend(first..=last);
                rest = &tail[digits.len()..];
            } else {
                rows.insert(first);
            }
            if let Some(tail) = rest.strip_prefix("'s ") {
                let end = tail.find([',', ';', ')']).unwrap_or(tail.len());
                rest = &tail[end..];
            }
            match rest
                .strip_prefix(", ")
                .or_else(|| rest.strip_prefix(" and "))
            {
                Some(tail) if tail.starts_with(|c: char| c.is_ascii_digit()) => rest = tail,
                _ => break,
            }
        }
    }
    rows
}

// ─── Statuses ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grade {
    Done,
    Partial,
    Gap,
    NotCarried,
}

impl Grade {
    fn parse(word: &str) -> Option<Self> {
        match word {
            "done" => Some(Self::Done),
            "partial" => Some(Self::Partial),
            "gap" => Some(Self::Gap),
            "n/a" => Some(Self::NotCarried),
            _ => None,
        }
    }

    fn open(self) -> bool {
        matches!(self, Self::Partial | Self::Gap)
    }
}

/// `library / service (items)`.
#[derive(Debug)]
struct Status {
    library: Grade,
    service: Grade,
    items: Vec<String>,
}

/// A status cell; `None` for a row that is n/a for us ("n/a — unofficial
/// protocol", with an optional note in parentheses).
fn parse_status(cell: &str) -> Result<Option<Status>, String> {
    if let Some(note) = cell.strip_prefix("n/a — unofficial protocol") {
        return if note.is_empty() || (note.starts_with(" (") && note.ends_with(')')) {
            Ok(None)
        } else {
            Err(format!(
                "`{cell}`: an unofficial-protocol row takes only a note"
            ))
        };
    }
    let (sides, items) = match cell.split_once(" (") {
        Some((sides, rest)) => {
            let list = rest
                .strip_suffix(')')
                .ok_or_else(|| format!("`{cell}`: no closing parenthesis"))?;
            (sides, list.split(", ").map(str::to_owned).collect())
        }
        None => (cell, Vec::new()),
    };
    let (library, service) = sides
        .split_once(" / ")
        .ok_or_else(|| format!("`{cell}` is not `library / service`"))?;
    let grade =
        |word: &str| Grade::parse(word).ok_or_else(|| format!("`{cell}`: `{word}` is not a grade"));
    let status = Status {
        library: grade(library)?,
        service: grade(service)?,
        items,
    };
    if status.service.open() == status.items.is_empty() {
        return Err(format!(
            "`{cell}`: a service that is partial or a gap names the roadmap items that bring it, and only then"
        ));
    }
    if let Some(bad) = status.items.iter().find(|i| !is_item_id(i)) {
        return Err(format!("`{cell}`: `{bad}` is not a roadmap item id"));
    }
    Ok(Some(status))
}

/// A parity row: its line, number and status.
struct ParityRow {
    line: usize,
    number: u32,
    status: Option<Status>,
}

fn parity_rows() -> (Vec<ParityRow>, Vec<String>) {
    let mut problems = Vec::new();
    let mut rows = Vec::new();
    for (line, number, cells) in numbered_rows(&doc(PARITY)) {
        if cells.len() != 9 {
            problems.push(format!("{PARITY}:{line}: {} cells, not 9", cells.len()));
            continue;
        }
        let status = parse_status(&cells[8]).unwrap_or_else(|why| {
            problems.push(format!("{PARITY}:{line}: {why}"));
            None
        });
        rows.push(ParityRow {
            line,
            number,
            status,
        });
    }
    (rows, problems)
}

// ─── Roadmap ─────────────────────────────────────────────────────────────

/// Whether `s` is an item id: one of S, U, B, L, M, P, a number, then
/// optionally a lowercase letter and digits (`S1`, `L20a`, `M5c3`).
fn is_item_id(s: &str) -> bool {
    let mut chars = s.chars().peekable();
    if !chars.next().is_some_and(|c| "SUBLMP".contains(c)) {
        return false;
    }
    let mut digits = 0;
    while chars.next_if(char::is_ascii_digit).is_some() {
        digits += 1;
    }
    if digits == 0 {
        return false;
    }
    if chars.next_if(char::is_ascii_lowercase).is_some() {
        while chars.next_if(char::is_ascii_digit).is_some() {}
    }
    chars.next().is_none()
}

/// The item ids written in `text`, each with the end of its range when
/// it starts one (`S5–S9`). An id preceded by a letter, digit or `-`
/// (`SR-L2`) is not one.
fn id_mentions(text: &str) -> Vec<(String, Option<String>)> {
    let chars: Vec<char> = text.chars().collect();
    let id_at = |start: usize| -> Option<usize> {
        let before = start.checked_sub(1).map(|i| chars[i]);
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-') {
            return None;
        }
        let mut end = start + 1;
        while chars.get(end).is_some_and(char::is_ascii_digit) {
            end += 1;
        }
        if end == start + 1 || !"SUBLMP".contains(chars[start]) {
            return None;
        }
        if chars.get(end).is_some_and(char::is_ascii_lowercase) {
            end += 1;
            while chars.get(end).is_some_and(char::is_ascii_digit) {
                end += 1;
            }
        }
        (!chars
            .get(end)
            .is_some_and(|c| c.is_alphanumeric() || *c == '_'))
        .then_some(end)
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let Some(end) = id_at(i) else {
            i += 1;
            continue;
        };
        let id: String = chars[i..end].iter().collect();
        if chars.get(end) == Some(&'–')
            && let Some(last) = id_at(end + 1)
        {
            out.push((id, Some(chars[end + 1..last].iter().collect())));
            i = last;
        } else {
            out.push((id, None));
            i = end;
        }
    }
    out
}

/// `L20a` as `('L', 20, "a")`.
fn id_parts(id: &str) -> (char, u32, &str) {
    let letter = id.chars().next().unwrap();
    let digits = id[1..].chars().take_while(char::is_ascii_digit).count();
    (letter, id[1..=digits].parse().unwrap(), &id[1 + digits..])
}

/// A roadmap item.
struct Item {
    id: String,
    ticked: bool,
    block: String,
    after: Option<String>,
    decisive: bool,
    kind: Option<String>,
}

impl Item {
    fn new(lines: &[&str]) -> Self {
        let first = lines[0];
        let ticked = first.starts_with("- [x]");
        let id = first[8..].split('.').next().unwrap().to_owned();
        let bullet = |label: &str| -> Option<String> {
            let start = lines
                .iter()
                .position(|l| l.trim_start().starts_with(&format!("- **{label}:**")))?;
            let mut text = lines[start].trim_start()[label.len() + 7..].to_owned();
            for line in &lines[start + 1..] {
                if line.trim_start().starts_with("- ") {
                    break;
                }
                text.push(' ');
                text.push_str(line.trim());
            }
            Some(text.trim().to_owned())
        };
        Self {
            id,
            ticked,
            block: lines.join("\n"),
            after: bullet("After"),
            decisive: bullet("Decisive").is_some(),
            kind: bullet("Kind"),
        }
    }
}

fn roadmap_items(markdown: &str) -> Vec<Item> {
    let mut items = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in markdown.lines() {
        let starts = line.starts_with("- [ ] **") || line.starts_with("- [x] **");
        if (starts || line.starts_with('#')) && !current.is_empty() {
            items.push(Item::new(&current));
            current.clear();
        }
        if starts || !current.is_empty() {
            current.push(line);
        }
    }
    if !current.is_empty() {
        items.push(Item::new(&current));
    }
    items
}

/// The items `id` stands for: itself, or the items of a range ending at
/// `last`, among `ids`.
fn expand_id(id: &str, last: Option<&str>, ids: &BTreeSet<String>) -> Vec<String> {
    let Some(last) = last else {
        return vec![id.to_owned()];
    };
    let (letter, from, from_suffix) = id_parts(id);
    let (to_letter, to, to_suffix) = id_parts(last);
    if letter != to_letter {
        return vec![id.to_owned(), last.to_owned()];
    }
    ids.iter()
        .filter(|candidate| {
            let (l, n, suffix) = id_parts(candidate);
            if l != letter || n < from || n > to {
                return false;
            }
            if from != to {
                return true;
            }
            let first = suffix.chars().next();
            first >= from_suffix.chars().next() && first <= to_suffix.chars().next()
        })
        .cloned()
        .collect()
}

/// Whether `id` names an item of `ids` or a family of them (`M5` for M5a,
/// `M5c` for M5c1).
fn names_items(id: &str, ids: &BTreeSet<String>) -> bool {
    ids.contains(id)
        || ids.iter().any(|item| {
            item.strip_prefix(id).is_some_and(|rest| {
                let next = rest.chars().next();
                next.is_some_and(|c| c.is_ascii_lowercase())
                    || (id.ends_with(|c: char| c.is_ascii_lowercase())
                        && next.is_some_and(|c| c.is_ascii_digit()))
            })
        })
}

/// The ids the design's own tables use for work done before the roadmap:
/// its §9 library rows (L1–L6) and the M1 milestone's parts.
fn design_ids() -> BTreeSet<String> {
    let design = doc(DESIGN);
    let mut ids: BTreeSet<String> = design
        .lines()
        .filter_map(|l| l.strip_prefix("| L"))
        .filter_map(|rest| rest.split(' ').next())
        .filter(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty())
        .map(|n| format!("L{n}"))
        .collect();
    ids.extend(["M1", "M1a", "M1b", "M1c"].map(str::to_owned));
    ids
}

/// The wave table: (rank, the items in the wave's cell), in order.
fn waves(markdown: &str) -> Vec<(u32, String)> {
    let Some(start) = markdown.find("## Order") else {
        return Vec::new();
    };
    markdown[start..]
        .lines()
        .skip_while(|l| !l.starts_with('|'))
        .take_while(|l| l.starts_with('|'))
        .skip(2)
        .filter_map(|l| {
            let inner = l.strip_prefix("| ")?.strip_suffix(" |")?;
            let (wave, items) = inner.split_once(" | ")?;
            let rank = if wave == "last" {
                u32::MAX
            } else {
                wave.parse().ok()?
            };
            Some((rank, items.to_owned()))
        })
        .collect()
}

// ─── Open questions and decisions ────────────────────────────────────────

/// The open questions: number → (line, decided, left open).
fn questions() -> BTreeMap<u32, (usize, usize, usize)> {
    let markdown = doc(QUESTIONS);
    let mut out: BTreeMap<u32, (usize, usize, usize)> = BTreeMap::new();
    let mut current: Option<u32> = None;
    for (line, text) in prose_lines(&markdown) {
        if text.starts_with("## ") || text.starts_with("<!--") {
            current = None;
        }
        let number = text
            .split_once(". **")
            .and_then(|(n, _)| n.parse::<u32>().ok());
        if let Some(n) = number {
            assert!(
                out.insert(n, (line, 0, 0)).is_none(),
                "{QUESTIONS}:{line}: #{n} twice"
            );
            current = Some(n);
        }
        if let Some(entry) = current.and_then(|n| out.get_mut(&n)) {
            entry.1 += text.matches("**Decided ").count();
            entry.2 += text.matches("**Left open on ").count();
        }
    }
    out
}

/// The questions the CHANGELOG records as closed (and deleted from
/// `OPEN_QUESTIONS.md`): a citation of one is a citation of history.
fn closed_questions() -> BTreeSet<u32> {
    let changelog = doc("CHANGELOG.md");
    let Some(start) = changelog.find("### Open questions closed") else {
        return BTreeSet::new();
    };
    changelog[start..]
        .lines()
        .skip(1)
        .take_while(|l| !l.starts_with("### "))
        .filter_map(|l| l.strip_prefix("- #")?.split(' ').next()?.parse().ok())
        .collect()
}

/// The design's decision ids (§10's table).
fn decisions() -> BTreeSet<u32> {
    doc(DESIGN)
        .lines()
        .filter_map(|l| l.strip_prefix("| D"))
        .filter_map(|rest| rest.split(' ').next()?.parse().ok())
        .collect()
}

/// Open questions cited as `OPEN_QUESTIONS #5, #7`, `OPEN_QUESTIONS.md #43`
/// or `OQ #10`.
fn cited_questions(text: &str) -> Vec<u32> {
    let line = text.replace('`', "");
    let mut out = Vec::new();
    for keyword in ["OPEN_QUESTIONS.md", "OPEN_QUESTIONS", "OQ"] {
        for (at, _) in line.match_indices(keyword) {
            let after = &line[at + keyword.len()..];
            if keyword == "OPEN_QUESTIONS" && after.starts_with(".md") {
                continue;
            }
            let mut rest = after.trim_start();
            while let Some(tail) = rest.strip_prefix('#') {
                let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
                let Ok(n) = digits.parse() else { break };
                out.push(n);
                rest = &tail[digits.len()..];
                rest = rest
                    .strip_prefix(',')
                    .or_else(|| rest.trim_start().strip_prefix("and "))
                    .map_or("", str::trim_start);
            }
        }
    }
    out
}

/// The decisions cited as `D26`.
fn cited_decisions(line: &str) -> Vec<u32> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    for (i, c) in chars.iter().enumerate() {
        let starts = i == 0 || !chars[i - 1].is_alphanumeric();
        if *c != 'D' || !starts {
            continue;
        }
        let digits: String = chars[i + 1..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let end = i + 1 + digits.len();
        if !digits.is_empty() && !chars.get(end).is_some_and(|c| c.is_alphanumeric()) {
            out.push(digits.parse().unwrap());
        }
    }
    out
}

// ─── The tests: tables ───────────────────────────────────────────────────

#[test]
#[allow(clippy::too_many_lines)]
fn parity_rows_statuses_and_counts() {
    let markdown = doc(PARITY);
    let (rows, mut problems) = parity_rows();
    let numbered: Vec<(usize, u32, ())> = rows.iter().map(|r| (r.line, r.number, ())).collect();
    problems.extend(numbering_problems(PARITY, &numbered));
    let counted: Vec<&Status> = rows.iter().filter_map(|r| r.status.as_ref()).collect();
    let tally = |grade: fn(&Status) -> Grade| -> Vec<u32> {
        [Grade::Done, Grade::Partial, Grade::Gap, Grade::NotCarried]
            .iter()
            .map(|g| u32::try_from(counted.iter().filter(|s| grade(s) == *g).count()).unwrap())
            .collect()
    };
    let len = |n: usize| u32::try_from(n).unwrap();
    let both = len(counted
        .iter()
        .filter(|s| s.library == Grade::Done && s.service == Grade::Done)
        .count());
    let library_open = len(counted.iter().filter(|s| s.library.open()).count());
    let service_open = len(counted.iter().filter(|s| s.service.open()).count());
    let mut families: BTreeMap<String, u32> = BTreeMap::new();
    for status in &counted {
        let names: BTreeSet<String> = status
            .items
            .iter()
            .map(|id| {
                let take = if id.starts_with('M') { 2 } else { 1 };
                id.chars().take(take).collect()
            })
            .collect();
        for name in names {
            *families.entry(name).or_default() += 1;
        }
    }
    let head = &markdown[..markdown
        .find("## What parity means")
        .unwrap_or(markdown.len())];
    let flat = head.split_whitespace().collect::<Vec<_>>().join(" ");
    let written_families: BTreeMap<String, u32> = flat
        .split_once("by family (a row can name two): ")
        .map(|(_, rest)| rest.split_once(". ").map_or(rest, |(list, _)| list))
        .unwrap_or_default()
        .split(", ")
        .filter_map(|entry| {
            let mut words = entry.split_whitespace();
            Some((words.next()?.to_owned(), words.next()?.parse().ok()?))
        })
        .collect();
    let expect = [
        (
            "rows in the table",
            number_after(&flat, "The table has "),
            len(rows.len()),
        ),
        (
            "uncounted rows",
            number_before(&flat, " of them are not counted"),
            len(rows.len() - counted.len()),
        ),
        (
            "counted rows",
            number_before(&flat, " counted rows |"),
            len(counted.len()),
        ),
        (
            "counted rows (headline)",
            number_before(&flat, " counted rows are done"),
            len(counted.len()),
        ),
        (
            "rows done on both sides",
            number_after(&flat, "Where we stand **"),
            both,
        ),
        (
            "library rows partial or a gap",
            number_after(&flat, "Each of the library's "),
            library_open,
        ),
        (
            "service rows partial or a gap",
            number_after(&flat, "Each of the service's "),
            service_open,
        ),
    ];
    for (what, written, recount) in expect {
        if written != Some(recount) {
            problems.push(format!(
                "{PARITY}: {what}: the header says {written:?}, a recount {recount}"
            ));
        }
    }
    for (label, recount) in [
        ("Library", tally(|s| s.library)),
        ("Service", tally(|s| s.service)),
    ] {
        if count_row(head, label).as_ref() != Some(&recount) {
            problems.push(format!(
                "{PARITY}: the {label} counts say {:?}, a recount {recount:?} (done, partial, gap, n/a)",
                count_row(head, label)
            ));
        }
    }
    if written_families != families {
        problems.push(format!(
            "{PARITY}: the tally by family says {written_families:?}, a recount {families:?}"
        ));
    }
    assert_none(&problems, "parity.md's rows, statuses and counts");
}

#[test]
fn coverage_rows_and_anchors() {
    let markdown = doc(COVERAGE);
    let rows = numbered_rows(&markdown);
    let mut problems = numbering_problems(COVERAGE, &rows);
    for (line, number, cells) in &rows {
        if !cells[0].contains(&format!("<a id=\"row-{number}\"></a>")) {
            problems.push(format!(
                "{COVERAGE}:{line}: row {number} has no `row-{number}` anchor"
            ));
        }
        let status = cells.last().unwrap();
        if !["**done**", "**partial**", "planned", "out of scope"]
            .iter()
            .any(|word| status.starts_with(word))
        {
            problems.push(format!(
                "{COVERAGE}:{line}: the status starts with none of **done**, **partial**, planned, out of scope"
            ));
        }
    }
    assert_none(&problems, "coverage.md's rows");
}

#[test]
#[allow(clippy::too_many_lines)]
fn categories_agree_with_parity_and_coverage() {
    let markdown = doc(CATEGORIES);
    let (parity, _) = parity_rows();
    let parity: HashMap<u32, &ParityRow> = parity.iter().map(|r| (r.number, r)).collect();
    let coverage: BTreeSet<u32> = numbered_rows(&doc(COVERAGE))
        .into_iter()
        .map(|(_, n, _)| n)
        .collect();
    let rows = numbered_rows(&markdown);
    let mut problems = numbering_problems(CATEGORIES, &rows);
    let mut cited_coverage = BTreeSet::new();
    let mut counts = [[0u32; 3]; 2];
    for (line, number, cells) in &rows {
        let at = format!("{CATEGORIES}:{line} (category {number})");
        if cells.len() != 9 {
            problems.push(format!("{at}: {} cells, not 9", cells.len()));
            continue;
        }
        let mut rest = cells[5].as_str();
        while let Some(start) = rest.find('[') {
            let link = &rest[start..];
            let Some((label, tail)) = link[1..].split_once("](") else {
                break;
            };
            let target = tail.split(')').next().unwrap_or_default();
            if target != format!("coverage.md#row-{label}") {
                problems.push(format!(
                    "{at}: coverage link `[{label}]({target})` does not land on row {label}"
                ));
            }
            if let Ok(n) = label.parse::<u32>() {
                if !coverage.contains(&n) {
                    problems.push(format!("{at}: coverage row {n} does not exist"));
                }
                cited_coverage.insert(n);
            }
            rest = &link[1..];
        }
        let status = match parse_status(&cells[7]) {
            Ok(Some(status)) => status,
            Ok(None) => {
                problems.push(format!("{at}: a category is never n/a for us"));
                continue;
            }
            Err(why) => {
                problems.push(format!("{at}: {why}"));
                continue;
            }
        };
        for (side, grade) in [(0, status.library), (1, status.service)] {
            match grade {
                Grade::Done => counts[side][0] += 1,
                Grade::Partial => counts[side][1] += 1,
                Grade::Gap => counts[side][2] += 1,
                Grade::NotCarried => problems.push(format!("{at}: categories have no n/a")),
            }
        }
        let cited = match expand_numbers(&cells[6]) {
            Ok(cited) => cited,
            Err(why) => {
                problems.push(format!("{at}: parity rows {why}"));
                continue;
            }
        };
        let mut sides: [Vec<Grade>; 2] = [Vec::new(), Vec::new()];
        let mut items = BTreeSet::new();
        for n in &cited {
            let Some(row) = parity.get(n) else {
                problems.push(format!("{at}: parity row {n} does not exist"));
                continue;
            };
            let Some(row_status) = &row.status else {
                continue;
            };
            for (side, grade) in [(0, row_status.library), (1, row_status.service)] {
                if grade != Grade::NotCarried {
                    sides[side].push(grade);
                }
            }
            items.extend(row_status.items.iter().cloned());
        }
        for (side, grade, name) in [
            (0, status.library, "library"),
            (1, status.service, "service"),
        ] {
            let rows = &sides[side];
            if grade == Grade::Done && rows.iter().any(|g| *g != Grade::Done) {
                problems.push(format!(
                    "{at}: the {name} is done, but a parity row it cites is not"
                ));
            }
            if grade == Grade::Gap && rows.iter().any(|g| *g != Grade::Gap) {
                problems.push(format!(
                    "{at}: the {name} is a gap, but a parity row it cites is not"
                ));
            }
        }
        let named: BTreeSet<String> = status.items.iter().cloned().collect();
        if status.service.open() && named != items {
            problems.push(format!(
                "{at}: the service names {named:?}, its parity rows' services {items:?}"
            ));
        }
    }
    if cited_coverage != coverage {
        problems.push(format!(
            "{CATEGORIES}: coverage rows cited by no category: {:?}",
            coverage.difference(&cited_coverage).collect::<Vec<_>>()
        ));
    }
    let head = &markdown[..markdown.find("## The table").unwrap_or(markdown.len())];
    let total = u32::try_from(rows.len()).unwrap();
    if number_before(head, " categories |") != Some(total) {
        problems.push(format!(
            "{CATEGORIES}: the header's category count is not {total}"
        ));
    }
    for (side, label) in [(0, "Library"), (1, "Service")] {
        if count_row(head, label).as_deref() != Some(&counts[side][..]) {
            problems.push(format!(
                "{CATEGORIES}: the {label} counts say {:?}, a recount {:?} (done, partial, gap)",
                count_row(head, label),
                counts[side]
            ));
        }
    }
    assert_none(&problems, "categories.md against parity.md and coverage.md");
}

#[test]
fn roadmap_items_waves_and_dependencies() {
    let items = roadmap_items(&doc(ROADMAP));
    assert!(
        items.len() >= 50,
        "only {} roadmap items found",
        items.len()
    );
    let mut problems = Vec::new();
    let mut ids = BTreeSet::new();
    for item in &items {
        if !is_item_id(&item.id) {
            problems.push(format!("{ROADMAP}: `{}` is not an item id", item.id));
        }
        if !ids.insert(item.id.clone()) {
            problems.push(format!("{ROADMAP}: {} is used twice", item.id));
        }
        if item.after.is_none() {
            problems.push(format!("{ROADMAP}: {} has no `After:` line", item.id));
        }
        if !item.decisive {
            problems.push(format!("{ROADMAP}: {} has no `Decisive:` line", item.id));
        }
        let kind_ok = item.kind.as_deref().is_some_and(|kind| {
            ["additive", "breaking", "port change"]
                .iter()
                .any(|k| kind.starts_with(k))
        });
        if item.id.starts_with('L') && !kind_ok {
            problems.push(format!(
                "{ROADMAP}: {} has no `Kind:` line saying additive, breaking or port change",
                item.id
            ));
        }
    }
    let mut wave_of: BTreeMap<String, u32> = BTreeMap::new();
    for (rank, cell) in waves(&doc(ROADMAP)) {
        for (id, last) in id_mentions(&without_parentheses(&cell)) {
            let expanded = expand_id(&id, last.as_deref(), &ids);
            if expanded.is_empty() {
                problems.push(format!("{ROADMAP}: the order's `{id}` range names no item"));
            }
            for id in expanded {
                if !ids.contains(&id) {
                    problems.push(format!("{ROADMAP}: the order names {id}, which is no item"));
                } else if wave_of.insert(id.clone(), rank).is_some() {
                    problems.push(format!("{ROADMAP}: {id} is in two waves"));
                }
            }
        }
    }
    for item in &items {
        let Some(rank) = wave_of.get(&item.id) else {
            problems.push(format!("{ROADMAP}: {} is in no wave", item.id));
            continue;
        };
        let after = without_parentheses(item.after.as_deref().unwrap_or_default());
        for (id, last) in id_mentions(&after) {
            for before in expand_id(&id, last.as_deref(), &ids) {
                match wave_of.get(&before) {
                    None if !ids.contains(&before) => problems.push(format!(
                        "{ROADMAP}: {} comes after {before}, which is no item",
                        item.id
                    )),
                    Some(earlier) if earlier > rank => problems.push(format!(
                        "{ROADMAP}: {} comes after {before}, which is in a later wave",
                        item.id
                    )),
                    _ => {}
                }
                if before == item.id {
                    problems.push(format!("{ROADMAP}: {} comes after itself", item.id));
                }
            }
        }
    }
    assert_none(&problems, "roadmap.md's items and order");
}

#[test]
fn every_open_row_is_planned_and_every_cited_row_is_real() {
    let items = roadmap_items(&doc(ROADMAP));
    let (rows, _) = parity_rows();
    let by_number: HashMap<u32, &ParityRow> = rows.iter().map(|r| (r.number, r)).collect();
    let citations: HashMap<&str, BTreeSet<u32>> = items
        .iter()
        .map(|item| (item.id.as_str(), cited_rows(&item.block)))
        .collect();
    let mut problems = Vec::new();
    for item in &items {
        for n in &citations[item.id.as_str()] {
            match by_number.get(n).map(|r| r.status.as_ref()) {
                None => problems.push(format!(
                    "{ROADMAP}: {} cites row {n}, which does not exist",
                    item.id
                )),
                Some(None) => problems.push(format!(
                    "{ROADMAP}: {} cites row {n}, which is n/a (unofficial protocol)",
                    item.id
                )),
                Some(Some(status))
                    if !item.ticked && !status.library.open() && !status.service.open() =>
                {
                    problems.push(format!(
                        "{ROADMAP}: {} (not ticked) cites row {n}, which is done or n/a on both sides",
                        item.id
                    ));
                }
                Some(Some(_)) => {}
            }
        }
    }
    for row in &rows {
        let Some(status) = &row.status else { continue };
        if status.library.open() && !citations.values().any(|rows| rows.contains(&row.number)) {
            problems.push(format!(
                "{PARITY}:{}: row {} is {:?} in the library, and no roadmap item cites it",
                row.line, row.number, status.library
            ));
        }
        for id in &status.items {
            match citations.get(id.as_str()) {
                None => problems.push(format!(
                    "{PARITY}:{}: row {} names {id}, which is no roadmap item",
                    row.line, row.number
                )),
                Some(cited) if !cited.contains(&row.number) => problems.push(format!(
                    "{PARITY}:{}: row {} names {id}, which does not cite it",
                    row.line, row.number
                )),
                Some(_) => {}
            }
        }
    }
    assert_none(&problems, "parity.md against roadmap.md");
}

#[test]
fn open_questions_are_counted_and_decided_once() {
    let questions = questions();
    assert!(
        questions.len() >= 20,
        "only {} open questions found",
        questions.len()
    );
    let mut problems = Vec::new();
    for (n, (line, decided, left)) in &questions {
        if decided + left != 1 {
            problems.push(format!(
                "{QUESTIONS}:{line}: #{n} is decided {decided} and left open {left} times, not once in all"
            ));
        }
    }
    let markdown = doc(QUESTIONS);
    let decided = u32::try_from(questions.values().filter(|q| q.1 == 1).count()).unwrap();
    let open = u32::try_from(questions.values().filter(|q| q.2 == 1).count()).unwrap();
    if number_before(&markdown, " entries are decided") != Some(decided) {
        problems.push(format!(
            "{QUESTIONS}: the header's decided count is not {decided}"
        ));
    }
    let written_open = number_after(&markdown, " entries are decided and ");
    if written_open != Some(open) {
        problems.push(format!(
            "{QUESTIONS}: the header's open count is {written_open:?}, not {open}"
        ));
    }
    assert_none(&problems, "OPEN_QUESTIONS.md's entries");
}

#[test]
fn cited_items_decisions_and_questions_exist() {
    let items: BTreeSet<String> = roadmap_items(&doc(ROADMAP))
        .into_iter()
        .map(|item| item.id)
        .collect();
    let design = design_ids();
    let decisions = decisions();
    let questions = questions();
    let closed = closed_questions();
    assert!(
        closed.len() >= 10,
        "only {} closed questions found",
        closed.len()
    );
    assert!(
        decisions.len() >= 29,
        "only {} design decisions found",
        decisions.len()
    );
    let known = |id: &str| names_items(id, &items) || design.contains(id);
    let mut problems = Vec::new();
    let mut checked = 0;
    for file in PLANNING {
        for (line, text) in prose_lines(&doc(file)) {
            let prose = without_code(text);
            for (id, last) in id_mentions(&prose) {
                checked += 1;
                for id in std::iter::once(id).chain(last) {
                    if !known(&id) {
                        problems.push(format!("{file}:{line}: {id} is no roadmap item or family"));
                    }
                }
            }
            for n in cited_decisions(&prose) {
                if !decisions.contains(&n) {
                    problems.push(format!("{file}:{line}: D{n} is no design decision"));
                }
            }
        }
        // A citation may wrap (`OPEN_QUESTIONS` at a line's end, `#10` on
        // the next), so questions are read from the prose as one text.
        let markdown = doc(file);
        let prose: Vec<&str> = prose_lines(&markdown).into_iter().map(|(_, l)| l).collect();
        for n in cited_questions(&prose.join(" ")) {
            if !questions.contains_key(&n) && !closed.contains(&n) {
                problems.push(format!("{file}: open question #{n} does not exist"));
            }
        }
    }
    assert!(checked >= 300, "only {checked} item ids found");
    assert_none(&problems, "citations in the planning docs");
}

// ─── Names in the code ───────────────────────────────────────────────────

/// Identifier and punctuation tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Punct(char),
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_alphabetic() || c == '_' {
            let mut ident = String::from(c);
            while let Some(next) = chars.next_if(|&n| is_ident_char(n)) {
                ident.push(next);
            }
            out.push(Token::Ident(ident));
        } else if c.is_ascii_digit() {
            while chars.next_if(|&n| is_ident_char(n)).is_some() {}
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

/// `text` without comments, and with the contents of string, raw string
/// and char literals removed (the rules of `tests/skills.rs`'s).
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
            'r' | 'b' | 'c' if boundary && raw_string_at(&chars, i) => {
                let mut j = if c == 'r' { i + 1 } else { i + 2 };
                let mut hashes = 0;
                while at(j) == Some('#') {
                    hashes += 1;
                    j += 1;
                }
                j += 1;
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

/// Whether a raw string (`r"`, `r#"`, `br"`, `cr#"`) starts at `i`.
fn raw_string_at(chars: &[char], i: usize) -> bool {
    let at = |i: usize| chars.get(i).copied();
    let c = chars[i];
    let start = if c != 'r' && at(i + 1) == Some('r') {
        i + 2
    } else {
        i + 1
    };
    if c != 'r' && start != i + 2 {
        return false;
    }
    let mut j = start;
    while at(j) == Some('#') {
        j += 1;
    }
    at(j) == Some('"')
}

/// End (exclusive) of the `<…>` starting at `start`.
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

/// The last segment of the type path at `i`, and the index after it.
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

/// The member the identifier at `toks[i]` declares directly inside a
/// `body`: a variant, a field, or the name after `fn`, `const`, `type`.
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

/// What one source file declares.
#[derive(Default)]
struct SourceFile {
    path: PathBuf,
    /// Declared items (`struct`, `enum`, `trait`, `type`, `fn`, `const`,
    /// `static`, `mod`, `union`, `macro_rules!`) and whether they are `pub`.
    decls: HashMap<String, bool>,
    /// Names in `pub use` statements.
    reexports: HashSet<String>,
    /// Variants, fields and `impl`/`trait` items, by the type whose body
    /// declares them.
    members: HashMap<String, HashSet<String>>,
    /// Traits each type implements.
    traits: HashMap<String, HashSet<String>>,
    /// Types declared inside a macro invocation (`open_enum! { … }`).
    macro_types: HashSet<String>,
    has_macro_rules: bool,
}

const KEYWORDS: &[&str] = &[
    "fn", "mut", "for", "impl", "where", "const", "unsafe", "async",
];

impl SourceFile {
    fn parse(path: PathBuf, text: &str) -> Self {
        let toks = tokens(&code_only(text));
        let mut file = Self {
            path,
            has_macro_rules: text.contains("macro_rules!"),
            ..Self::default()
        };
        let mut in_use = false;
        for (i, token) in toks.iter().enumerate() {
            match token {
                Token::Ident(word) => {
                    if in_use {
                        file.reexports.insert(word.clone());
                    }
                    let decl = matches!(
                        word.as_str(),
                        "struct"
                            | "enum"
                            | "trait"
                            | "type"
                            | "fn"
                            | "const"
                            | "static"
                            | "mod"
                            | "union"
                    );
                    if let (true, Some(name)) = (decl, ident(toks.get(i + 1)))
                        && !KEYWORDS.contains(&name)
                    {
                        let public = is_pub(&toks, i);
                        let entry = file.decls.entry(name.to_owned()).or_default();
                        *entry |= public;
                    }
                    if word == "macro_rules"
                        && let Some(name) = ident(toks.get(i + 2))
                    {
                        file.decls.insert(name.to_owned(), true);
                    }
                    if word == "use" && is_pub(&toks, i) {
                        in_use = true;
                    }
                }
                Token::Punct(';') => in_use = false,
                Token::Punct(_) => {}
            }
        }
        file.scope_members(&toks);
        file
    }

    /// Type members by brace-matched body.
    fn scope_members(&mut self, toks: &[Token]) {
        let mut depth = 0usize;
        let mut open: Vec<(usize, Body, String)> = Vec::new();
        let mut pending: Option<(Body, String)> = None;
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
                        invocations.push(depth + 1);
                    }
                }
                Token::Punct('{') => {
                    depth += 1;
                    if let Some((body, name)) = pending.take() {
                        if !invocations.is_empty() {
                            self.macro_types.insert(name.clone());
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
                    match (word.as_str(), ident(toks.get(i + 1))) {
                        ("enum", Some(name)) => pending = Some((Body::Enum, name.to_owned())),
                        ("struct" | "union", Some(name)) => {
                            pending = Some((Body::Struct, name.to_owned()));
                        }
                        ("trait", Some(name)) => pending = Some((Body::Items, name.to_owned())),
                        ("impl", _) => {
                            if let Some((self_ty, trait_name)) = impl_header(&toks[i + 1..]) {
                                if let Some(trait_name) = trait_name {
                                    self.traits
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
                        self.members
                            .entry(name.clone())
                            .or_default()
                            .insert(member.to_owned());
                    }
                }
                Token::Punct(_) => {}
            }
        }
    }
}

/// Whether the declaration keyword at `toks[at]` is `pub` (or
/// `pub(crate)`), past `async`, `unsafe`, `const` and `extern`.
fn is_pub(toks: &[Token], at: usize) -> bool {
    let mut i = at;
    while i > 0 {
        i -= 1;
        match &toks[i] {
            Token::Ident(w)
                if ["async", "unsafe", "const", "extern", "default"].contains(&w.as_str()) => {}
            Token::Punct('"') => {}
            Token::Punct(')') => {
                while i > 0 && toks[i] != Token::Punct('(') {
                    i -= 1;
                }
                return i > 0 && ident(toks.get(i - 1)) == Some("pub");
            }
            Token::Ident(w) => return w == "pub",
            Token::Punct(_) => return false,
        }
    }
    false
}

/// Members every type may be named with: derived or blanket methods.
const DERIVED_MEMBERS: &[&str] = &["default", "clone", "to_string", "fmt", "eq", "hash"];

/// Names that are Rust's own, never ours to check.
const LANGUAGE: &[&str] = &[
    "Self", "Some", "None", "Ok", "Err", "Option", "Result", "Vec", "String", "Box", "Arc", "Send",
    "Sync", "Clone", "Copy", "Debug", "Default", "Display",
];

/// The crates a table's prefix names: prefix, source directory, and the
/// module file when the prefix is a module (`inbox::`).
const PREFIXES: &[(&str, &str, Option<&str>)] = &[
    ("client", "crates/meta-whatsapp-client/src", None),
    ("webhooks", "crates/meta-whatsapp-webhooks/src", None),
    ("core", "crates/meta-whatsapp-core/src", None),
    ("adapters", "crates/meta-whatsapp-adapters/src", None),
    ("typst", "crates/meta-whatsapp-typst/src", None),
    (
        "inbox",
        "crates/meta-whatsapp-rs/src/inbox",
        Some("crates/meta-whatsapp-rs/src/inbox.rs"),
    ),
    ("server", "crates/meta-whatsapp-server/src", None),
    ("server_core", "crates/meta-whatsapp-server-core/src", None),
];

/// Every source file of `crates/*/src`.
struct Index {
    files: Vec<SourceFile>,
}

impl Index {
    fn build() -> Self {
        let mut paths = Vec::new();
        for krate in sorted_dir(&repo().join("crates")) {
            walk(&krate.join("src"), "rs", &mut paths);
        }
        let files: Vec<SourceFile> = paths
            .into_iter()
            .map(|p| {
                let text = read(&p);
                SourceFile::parse(p, &text)
            })
            .collect();
        assert!(files.len() > 100, "indexed only {} files", files.len());
        Self { files }
    }

    /// Whether some file declares `name` as an item or a variant.
    fn declared_anywhere(&self, name: &str) -> bool {
        self.files.iter().any(|f| {
            f.decls.contains_key(name)
                || f.reexports.contains(name)
                || f.members.values().any(|m| m.contains(name))
        })
    }

    /// Whether `member` is a variant, field, method or constant of the type
    /// `owner` (or of a trait it implements). A type without a body in the
    /// source (made by a macro) accepts what its crate's files define.
    fn member(&self, files: &[&SourceFile], owner: &str, member: &str) -> bool {
        if DERIVED_MEMBERS.contains(&member) {
            return true;
        }
        let mut found = false;
        let mut traits = HashSet::new();
        for file in &self.files {
            if let Some(members) = file.members.get(owner) {
                found = true;
                if members.contains(member) {
                    return true;
                }
            }
            if let Some(t) = file.traits.get(owner) {
                traits.extend(t.iter().cloned());
            }
        }
        let by_trait = self.files.iter().any(|file| {
            traits
                .iter()
                .any(|t| file.members.get(t).is_some_and(|m| m.contains(member)))
        });
        if by_trait {
            return true;
        }
        let made_by_macro = self.files.iter().any(|f| f.macro_types.contains(owner));
        if found && !made_by_macro {
            return false;
        }
        files
            .iter()
            .any(|f| f.decls.contains_key(member) || f.members.values().any(|m| m.contains(member)))
            || (made_by_macro
                && self
                    .files
                    .iter()
                    .filter(|f| f.has_macro_rules)
                    .any(|f| f.decls.contains_key(member)))
    }

    /// Resolve `prefix::module::…::Item::member` in its crate.
    fn resolve(&self, segments: &[&str]) -> Result<(), String> {
        let root = repo();
        let (_, dir, file) = PREFIXES
            .iter()
            .find(|(prefix, _, _)| *prefix == segments[0])
            .ok_or_else(|| "unknown prefix".to_owned())?;
        let crate_dir = root.join(dir.split("/src").next().unwrap()).join("src");
        let mut dir = root.join(dir);
        let mut file = file.map(|f| root.join(f));
        let mut rest = &segments[1..];
        let mut moved = file.is_some();
        while let [segment, tail @ ..] = rest {
            if !segment.starts_with(|c: char| c.is_ascii_lowercase()) {
                break;
            }
            if dir.join(segment).is_dir() {
                dir = dir.join(segment);
                file = [dir.with_extension("rs"), dir.join("mod.rs")]
                    .into_iter()
                    .find(|f| f.is_file());
            } else if dir.join(format!("{segment}.rs")).is_file() {
                file = Some(dir.join(format!("{segment}.rs")));
                dir = dir.join(segment);
            } else {
                break;
            }
            moved = true;
            rest = tail;
        }
        let Some((item, members)) = rest.split_first() else {
            return Ok(());
        };
        let crate_files: Vec<&SourceFile> = self
            .files
            .iter()
            .filter(|f| f.path.starts_with(&crate_dir))
            .collect();
        let scope: Vec<&SourceFile> = if moved {
            crate_files
                .iter()
                .copied()
                .filter(|f| file.as_ref() == Some(&f.path) || f.path.starts_with(&dir))
                .collect()
        } else {
            crate_files.clone()
        };
        if item.starts_with(|c: char| c.is_ascii_lowercase()) && !members.is_empty() {
            return if scope.iter().any(|f| f.reexports.contains(*item)) {
                Ok(())
            } else {
                Err(format!("`{item}` is not a module there"))
            };
        }
        let declared: Vec<bool> = scope
            .iter()
            .filter_map(|f| f.decls.get(*item).copied())
            .collect();
        match (declared.is_empty(), declared.contains(&true)) {
            (false, false) => return Err(format!("`{item}` is not `pub`")),
            (false, true) => {}
            (true, _) => {
                let reexported = scope.iter().any(|f| f.reexports.contains(*item))
                    && crate_files.iter().any(|f| f.decls.contains_key(*item));
                let by_macro = scope.iter().any(|f| f.macro_types.contains(*item));
                if !reexported && !by_macro {
                    return Err(format!("`{item}` is not declared there"));
                }
            }
        }
        let mut owner = *item;
        for member in members {
            if !self.member(&crate_files, owner, member) {
                return Err(format!("`{owner}` has no member `{member}`"));
            }
            owner = member;
        }
        Ok(())
    }

    /// Problems with one backticked span of a table cell.
    fn span_problem(&self, span: &str) -> Option<String> {
        let is_path = span.split("::").all(|s| {
            s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && s.chars().all(is_ident_char)
        });
        if !is_path {
            return None;
        }
        let segments: Vec<&str> = span.split("::").collect();
        let camel = |s: &str| {
            s.starts_with(|c: char| c.is_ascii_uppercase())
                && s.chars().any(|c| c.is_ascii_lowercase())
        };
        let result = if PREFIXES.iter().any(|(p, _, _)| *p == segments[0]) {
            self.resolve(&segments)
        } else if LANGUAGE.contains(&segments[0]) || !camel(segments[0]) {
            Ok(())
        } else if segments.len() == 1 {
            if self.declared_anywhere(segments[0]) {
                Ok(())
            } else {
                Err("not declared in crates/".to_owned())
            }
        } else if !self.files.iter().any(|f| f.decls.contains_key(segments[0])) {
            Err(format!("`{}` is not declared in crates/", segments[0]))
        } else {
            let all: Vec<&SourceFile> = self.files.iter().collect();
            segments.windows(2).try_for_each(|pair| {
                if self.member(&all, pair[0], pair[1]) {
                    Ok(())
                } else {
                    Err(format!("`{}` has no member `{}`", pair[0], pair[1]))
                }
            })
        };
        result.err().map(|why| format!("`{span}`: {why}"))
    }
}

#[test]
fn table_symbols_resolve() {
    let index = Index::build();
    let mut problems = Vec::new();
    let mut checked = 0;
    for (file, columns) in [(PARITY, [6, 7]), (CATEGORIES, [3, 4])] {
        for (line, _, cells) in numbered_rows(&doc(file)) {
            for column in columns {
                for span in code_spans(cells.get(column).map_or("", String::as_str)) {
                    checked += 1;
                    if let Some(problem) = index.span_problem(span) {
                        problems.push(format!("{file}:{line}: {problem}"));
                    }
                }
            }
        }
    }
    assert!(checked >= 300, "only {checked} spans found");
    assert_none(
        &problems,
        "symbols the tables cite (rename the cell, or the code it names)",
    );
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

/// The anchors of a Markdown file: its headings (a repeated one numbered
/// as GitHub does) and its HTML `id`s and `name`s.
fn anchors(markdown: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (_, line) in prose_lines(markdown) {
        let hashes = line.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes)
            && let Some(heading) = line[hashes..].strip_prefix(' ')
        {
            let base = slug(&heading.replace('`', ""));
            let n = seen.entry(base.clone()).or_default();
            out.insert(if *n == 0 {
                base.clone()
            } else {
                format!("{base}-{n}")
            });
            *n += 1;
        }
        for attribute in ["id=\"", "name=\""] {
            for (at, _) in line.match_indices(attribute) {
                let rest = &line[at + attribute.len()..];
                if let Some(end) = rest.find('"') {
                    out.insert(rest[..end].to_owned());
                }
            }
        }
    }
    out
}

/// Markdown link targets of a line, outside code spans.
fn link_targets(line: &str) -> Vec<String> {
    let plain = without_code(line);
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

fn link_problem(
    file: &Path,
    target: &str,
    cache: &mut HashMap<PathBuf, HashSet<String>>,
) -> Option<String> {
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
        normalize(&file.parent().unwrap().join(path))
    };
    if !resolved.exists() {
        return Some(format!("`{target}`: {} does not exist", rel(&resolved)));
    }
    if anchor.is_empty() || resolved.extension().is_none_or(|e| e != "md") {
        return None;
    }
    let known = cache
        .entry(resolved.clone())
        .or_insert_with(|| anchors(&read(&resolved)));
    (!known.contains(anchor)).then(|| format!("`{target}`: no heading or anchor #{anchor}"))
}

#[test]
fn links_and_anchors_in_the_docs_resolve() {
    let root = repo();
    let mut files: Vec<PathBuf> = [
        "README.md",
        "AGENTS.md",
        "CLAUDE.md",
        "CONTRIBUTING.md",
        "OPEN_QUESTIONS.md",
        "CHANGELOG.md",
    ]
    .iter()
    .map(|f| root.join(f))
    .collect();
    walk(&root.join("docs"), "md", &mut files);
    let mut cache = HashMap::new();
    let mut problems = Vec::new();
    let mut checked = 0;
    for path in &files {
        for (line, text) in prose_lines(&read(path)) {
            for target in link_targets(text) {
                checked += 1;
                if let Some(problem) = link_problem(path, &target, &mut cache) {
                    let mut entry = String::new();
                    write!(entry, "{}:{line}: {problem}", rel(path)).unwrap();
                    problems.push(entry);
                }
            }
        }
    }
    assert!(checked >= 200, "only {checked} links found");
    assert_none(&problems, "links in the docs");
}

// ─── The checks catch what they claim to ─────────────────────────────────

#[test]
fn the_parsers_read_what_the_docs_write() {
    assert_eq!(
        cited_rows("rows 90's Meta side, 127, 146–148 and 152; coverage row 33; arrows 5"),
        BTreeSet::from([90, 127, 146, 147, 148, 152])
    );
    assert_eq!(
        cited_rows("Row 112 (the bundle), (rows 136–138, 154)"),
        BTreeSet::from([112, 136, 137, 138, 154])
    );
    assert_eq!(
        id_mentions("S5–S9, M5c3 and SR-L2, U+0000, M1.3, L20a."),
        vec![
            ("S5".to_owned(), Some("S9".to_owned())),
            ("M5c3".to_owned(), None),
            ("M1".to_owned(), None),
            ("L20a".to_owned(), None),
        ]
    );
    let ids: BTreeSet<String> = ["M5a", "M5b", "M5c1", "M5c2", "M5l", "L7", "L10a", "L25"]
        .map(str::to_owned)
        .into();
    assert_eq!(
        expand_id("M5a", Some("M5c"), &ids),
        ["M5a", "M5b", "M5c1", "M5c2"]
    );
    assert_eq!(expand_id("L7", Some("L25"), &ids), ["L10a", "L25", "L7"]);
    assert!(names_items("M5", &ids) && names_items("M5c", &ids) && names_items("L10", &ids));
    assert!(!names_items("L1", &ids) && !names_items("M6", &ids));
    assert_eq!(
        cited_questions("(`OPEN_QUESTIONS.md` #43; OQ #10, OPEN_QUESTIONS #5, #7 and #8)"),
        [43, 5, 7, 8, 10]
    );
    assert_eq!(
        cited_questions("OPEN_QUESTIONS\n  #5,\n  #11 (a note), #12"),
        [5, 11]
    );
    assert_eq!(cited_decisions("D20 (a), D26; not ID3 or D3x"), [20, 26]);
    assert!(parse_status("done / gap (M5a)").is_ok_and(|s| s.is_some()));
    for bad in [
        "done / gap",
        "done / done (M5a)",
        "done / gap (wave 2)",
        "finished / gap (M5a)",
        "done / partial (M5a",
    ] {
        assert!(parse_status(bad).is_err(), "{bad}");
    }
    assert!(parse_status("n/a — unofficial protocol (the card)").is_ok_and(|s| s.is_none()));
    assert_eq!(slug("10. Decisions"), "10-decisions");
}

#[test]
fn the_resolver_refuses_what_does_not_exist() {
    let index = Index::build();
    for good in [
        "client::messages::OutboundMessage::text",
        "webhooks::WebhookEvent::StatusUpdated",
        "core::store::ConversationStore::revoke",
        "inbox::Inbox::reply",
        "server::events::TENANT_EVENT_TYPES",
        "server::config",
        "OutboundMessage::reply_to",
        "MessageEchoed",
    ] {
        assert_eq!(index.span_problem(good), None, "{good}");
    }
    for bad in [
        "client::messages::OutboundMessage::no_such_constructor",
        "webhooks::WebhookEvent::NoSuchEvent",
        "client::no_such_module::Thing",
        "client::messages::Interactive::Carousel2",
        "server::events::NO_SUCH_CONST",
        "OutboundMessage::no_such_member",
        "NoSuchType",
    ] {
        assert!(index.span_problem(bad).is_some(), "{bad} resolved");
    }
}
