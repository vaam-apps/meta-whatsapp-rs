//! The three tools: `list`, `search` and `read`, their schemas and what
//! they answer. Every answer is bounded: a page or section read returns at
//! most [`MAX_READ_BYTES`] (and says where to continue), a search at most
//! [`MAX_LIMIT`] hits with a snippet of at most [`SNIPPET_CHARS`].

use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::corpus::{self, Corpus, CorpusError};

/// The most a `read` returns of a page's text, in bytes.
pub const MAX_READ_BYTES: usize = 40 * 1024;

/// Hits a search returns by default.
pub const DEFAULT_LIMIT: u64 = 10;

/// The most hits a search returns.
pub const MAX_LIMIT: u64 = 25;

/// The longest query, in characters.
pub const MAX_QUERY_CHARS: usize = 200;

/// The most terms a query is split into.
pub const MAX_TERMS: usize = 8;

/// A hit's snippet, in characters.
pub const SNIPPET_CHARS: usize = 240;

/// The headings a truncated read lists.
const MAX_LISTED_HEADINGS: usize = 60;

/// What a tool answers: its text, and for `list` and `search` the same as
/// structured content.
#[derive(Debug)]
pub struct Output {
    /// The text content block.
    pub text: String,
    /// `structuredContent`, matching the tool's `outputSchema`.
    pub structured: Option<Value>,
}

/// Why a call failed. A protocol error is a JSON-RPC error; a tool error a
/// result with `isError: true`, which the model can act on.
#[derive(Debug)]
pub enum CallError {
    /// No tool of that name (JSON-RPC `-32602`, as the spec's example).
    UnknownTool(String),
    /// The arguments are wrong, or the page or section is not there.
    Tool(String),
}

impl From<CorpusError> for CallError {
    fn from(e: CorpusError) -> Self {
        Self::Tool(e.to_string())
    }
}

/// The tools, in the order `tools/list` gives them (deterministic, as the
/// spec asks).
pub fn definitions() -> Value {
    let read_only = json!({
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false
    });
    json!([
        list_tool(&read_only),
        search_tool(&read_only),
        read_tool(&read_only)
    ])
}

/// `list`.
fn list_tool(annotations: &Value) -> Value {
    json!({
        "name": "list",
        "title": "List the docs and skills",
        "description": "List every page this server serves: meta-whatsapp-rs's root documents \
            (README, AGENTS, CONTRIBUTING, CHANGELOG, OPEN_QUESTIONS), its docs (guides, \
            design, architecture, parity, roadmap, llms.txt) and its consumer skills \
            (SKILL.md, references, examples). Each with its title and, for a skill, its \
            description. `under` narrows it to a directory, e.g. `skills` or `docs/guides`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "under": {
                    "type": "string",
                    "description": "A directory prefix, e.g. `docs/guides` or `skills/meta-whatsapp-rs-server`."
                }
            },
            "additionalProperties": false
        },
        "outputSchema": {
            "type": "object",
            "properties": {
                "pages": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "title": { "type": ["string", "null"] },
                            "description": { "type": ["string", "null"] },
                            "bytes": { "type": "integer" }
                        },
                        "required": ["path", "title", "description", "bytes"]
                    }
                }
            },
            "required": ["pages"]
        },
        "annotations": annotations
    })
}

/// `search`.
fn search_tool(annotations: &Value) -> Value {
    json!({
        "name": "search",
        "title": "Search the docs and skills",
        "description": "Find the sections that contain every term of `query` (split on \
            spaces; ASCII case-insensitive; `Idempotency-Key` is one term). Hits are \
            sections, best first: the page's path, the section's heading (pass it to `read` \
            as `section`), the line of the first match and a snippet.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Terms that must all appear, e.g. `Idempotency-Key` or `token vault rotate`."
                },
                "under": {
                    "type": "string",
                    "description": "Only pages under this directory prefix, e.g. `skills`."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LIMIT,
                    "description": "Hits to return (default 10)."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        },
        "outputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "total": { "type": "integer", "description": "Sections that matched, before the limit." },
                "hits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "section": { "type": ["string", "null"] },
                            "line": { "type": "integer" },
                            "score": { "type": "integer" },
                            "snippet": { "type": "string" }
                        },
                        "required": ["path", "section", "line", "score", "snippet"]
                    }
                }
            },
            "required": ["query", "total", "hits"]
        },
        "annotations": annotations
    })
}

/// `read`.
fn read_tool(annotations: &Value) -> Value {
    json!({
        "name": "read",
        "title": "Read a page or one section",
        "description": "Read a page by its path (from `list` or `search`), or only one of its \
            sections: `section` is a heading's text (`Idempotency keys`) or its anchor \
            (`idempotency-keys`), and the section runs to the next heading of its level. \
            At most 40 KiB is returned; a longer text ends with where to continue \
            (`from_line`) and the page's sections.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "A page's path, relative to the repository, e.g. `docs/guides/server.md`."
                },
                "section": {
                    "type": "string",
                    "description": "A heading's text or anchor; the whole page when absent."
                },
                "from_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Start at this line of the page (1-based), to continue a truncated read."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        },
        "annotations": annotations
    })
}

/// Run tool `name` with `arguments` against the corpus under `root`.
pub fn call(root: &Path, name: &str, arguments: &Map<String, Value>) -> Result<Output, CallError> {
    let allowed: &[&str] = match name {
        "list" => &["under"],
        "search" => &["query", "under", "limit"],
        "read" => &["path", "section", "from_line"],
        _ => return Err(CallError::UnknownTool(name.to_owned())),
    };
    if let Some(key) = arguments.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(CallError::Tool(format!(
            "`{name}` takes no argument `{key}` (it takes: {})",
            allowed.join(", ")
        )));
    }
    let corpus = Corpus::load(root)
        .map_err(|e| CallError::Tool(format!("the docs root cannot be read: {}", e.kind())))?;
    match name {
        "list" => list(&corpus, string_arg(arguments, "under")?),
        "search" => search(
            &corpus,
            string_arg(arguments, "query")?
                .ok_or_else(|| CallError::Tool("`search` needs a `query`".to_owned()))?,
            string_arg(arguments, "under")?,
            integer_arg(arguments, "limit", 1, MAX_LIMIT)?.unwrap_or(DEFAULT_LIMIT),
        ),
        _ => read(
            &corpus,
            string_arg(arguments, "path")?
                .ok_or_else(|| CallError::Tool("`read` needs a `path`".to_owned()))?,
            string_arg(arguments, "section")?,
            integer_arg(arguments, "from_line", 1, u64::MAX)?,
        ),
    }
}

fn string_arg<'a>(
    arguments: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, CallError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(CallError::Tool(format!("`{key}` must be a string"))),
    }
}

fn integer_arg(
    arguments: &Map<String, Value>,
    key: &str,
    min: u64,
    max: u64,
) -> Result<Option<u64>, CallError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) if (min..=max).contains(&n) => Ok(Some(n)),
            _ => Err(CallError::Tool(format!(
                "`{key}` must be an integer from {min} to {max}"
            ))),
        },
    }
}

fn structured(value: Value) -> Output {
    Output {
        text: value.to_string(),
        structured: Some(value),
    }
}

fn list(corpus: &Corpus, under: Option<&str>) -> Result<Output, CallError> {
    let mut pages = Vec::new();
    for path in corpus.files_under(under)? {
        let (title, description, bytes) = match corpus.read(path) {
            Ok(text) => (
                corpus::title(path, &text),
                corpus::description(&text),
                text.len(),
            ),
            Err(_) => continue,
        };
        pages.push(json!({
            "path": path,
            "title": title,
            "description": description,
            "bytes": bytes,
        }));
    }
    Ok(structured(json!({ "pages": pages })))
}

/// One searchable chunk: a heading and its text up to the next heading of
/// any level (the text before the first heading has none).
struct Chunk<'a> {
    heading: Option<&'a str>,
    /// 0-based line of the chunk's first line.
    first_line: usize,
    lines: &'a [&'a str],
}

fn chunks<'a>(path: &str, text: &'a str, lines: &'a [&'a str]) -> Vec<Chunk<'a>> {
    let heads = if corpus::is_markdown(path) {
        corpus::headings(text)
    } else {
        Vec::new()
    };
    let mut out = Vec::new();
    let first = heads.first().map_or(lines.len(), |h| h.line);
    if first > 0 {
        out.push(Chunk {
            heading: None,
            first_line: 0,
            lines: &lines[..first],
        });
    }
    for (i, h) in heads.iter().enumerate() {
        let end = heads.get(i + 1).map_or(lines.len(), |n| n.line);
        let title = lines[h.line]
            .find(h.title.as_str())
            .map_or("", |at| &lines[h.line][at..at + h.title.len()]);
        out.push(Chunk {
            heading: Some(title),
            first_line: h.line,
            lines: &lines[h.line..end],
        });
    }
    out
}

fn search(
    corpus: &Corpus,
    query: &str,
    under: Option<&str>,
    limit: u64,
) -> Result<Output, CallError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(CallError::Tool("the query is empty".to_owned()));
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(CallError::Tool(format!(
            "the query is over {MAX_QUERY_CHARS} characters"
        )));
    }
    let terms: Vec<String> = query
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    if terms.len() > MAX_TERMS {
        return Err(CallError::Tool(format!(
            "the query has over {MAX_TERMS} terms"
        )));
    }
    let phrase = terms.join(" ");
    // The words of the terms, for the heading bonus: `Idempotency-Key`
    // gives `idempotency` and `key`, which `Idempotency keys` contains.
    let words: Vec<String> = terms
        .iter()
        .flat_map(|t| t.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|w| w.len() > 1)
        .map(str::to_owned)
        .collect();
    let mut hits: Vec<(u64, String, Option<String>, usize, String)> = Vec::new();
    for path in corpus.files_under(under)? {
        let Ok(text) = corpus.read(path) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for chunk in chunks(path, &text, &lines) {
            let body = chunk.lines.join("\n").to_ascii_lowercase();
            if !terms.iter().all(|t| body.contains(t.as_str())) {
                continue;
            }
            let mut score: u64 = terms
                .iter()
                .map(|t| body.matches(t.as_str()).take(50).count() as u64)
                .sum();
            if let Some(heading) = chunk.heading {
                let heading = heading.to_ascii_lowercase();
                score += 10
                    * words
                        .iter()
                        .filter(|w| heading.contains(w.as_str()))
                        .count() as u64;
            }
            if terms.len() > 1 && body.contains(&phrase) {
                score += 20;
            }
            let needle = if terms.len() > 1 && body.contains(&phrase) {
                phrase.as_str()
            } else {
                terms[0].as_str()
            };
            let (offset, line) = chunk
                .lines
                .iter()
                .enumerate()
                .find_map(|(i, l)| l.to_ascii_lowercase().find(needle).map(|at| (i, (at, *l))))
                .unwrap_or((0, (0, "")));
            hits.push((
                score,
                path.to_owned(),
                chunk.heading.map(str::to_owned),
                chunk.first_line + offset + 1,
                snippet(line.1, line.0),
            ));
        }
    }
    let total = hits.len();
    hits.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then(a.3.cmp(&b.3))
    });
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let hits: Vec<Value> = hits
        .into_iter()
        .take(limit)
        .map(|(score, path, section, line, snippet)| {
            json!({
                "path": path,
                "section": section,
                "line": line,
                "score": score,
                "snippet": snippet,
            })
        })
        .collect();
    Ok(structured(json!({
        "query": query,
        "total": total,
        "hits": hits,
    })))
}

/// At most [`SNIPPET_CHARS`] of `line` around byte `at`, trimmed.
fn snippet(line: &str, at: usize) -> String {
    let line_chars: Vec<(usize, char)> = line.char_indices().collect();
    let hit = line_chars.iter().position(|(i, _)| *i >= at).unwrap_or(0);
    let start = hit.saturating_sub(SNIPPET_CHARS / 3);
    let end = (start + SNIPPET_CHARS).min(line_chars.len());
    let mut out: String = line_chars[start..end].iter().map(|(_, c)| *c).collect();
    out = out.trim().to_owned();
    if start > 0 {
        out.insert(0, '…');
    }
    if end < line_chars.len() {
        out.push('…');
    }
    out
}

fn read(
    corpus: &Corpus,
    path: &str,
    section: Option<&str>,
    from_line: Option<u64>,
) -> Result<Output, CallError> {
    let text = corpus.read(path)?;
    let lines: Vec<&str> = text.lines().collect();
    let (start, end) = match section {
        None => (0, lines.len()),
        Some(wanted) => {
            let found = if corpus::is_markdown(path) {
                corpus::find_section(&text, wanted)
            } else {
                None
            };
            let Some((_, start, end)) = found else {
                return Err(CorpusError::NoSection {
                    path: path.to_owned(),
                    section: wanted.to_owned(),
                    headings: outline(path, &text),
                }
                .into());
            };
            (start, end)
        }
    };
    let begin = match from_line {
        None => start,
        Some(n) => {
            let n = usize::try_from(n - 1).unwrap_or(usize::MAX);
            if n < start || n >= end.max(start + 1) {
                return Err(CallError::Tool(format!(
                    "`from_line` {} is outside lines {}–{} of what was asked",
                    n.saturating_add(1),
                    start + 1,
                    end
                )));
            }
            n
        }
    };
    let mut out = String::new();
    let mut shown_to = begin;
    let mut cut_line = None;
    for line in &lines[begin..end] {
        if out.len() + line.len() + 1 > MAX_READ_BYTES {
            if shown_to == begin {
                // One line longer than the bound (none is, on 2026-11-01:
                // the longest is under 9 KiB): cut it at a character.
                let mut cut = MAX_READ_BYTES;
                while !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                out.push_str(&line[..cut]);
                out.push('\n');
                shown_to += 1;
                cut_line = Some(shown_to);
            }
            break;
        }
        out.push_str(line);
        out.push('\n');
        shown_to += 1;
    }
    if shown_to < end || cut_line.is_some() {
        let _ = write!(
            out,
            "\n[truncated at {MAX_READ_BYTES} bytes: lines {}–{} of {}–{} shown",
            begin + 1,
            shown_to,
            start + 1,
            end,
        );
        if let Some(line) = cut_line {
            let _ = write!(out, ", line {line} cut short");
        }
        if shown_to < end {
            let _ = write!(out, ". Continue with `from_line`: {}", shown_to + 1);
            if section.is_some() {
                out.push_str(" and the same `section`");
            }
            out.push(',');
        } else {
            out.push('.');
        }
        out.push_str(" Or read one section:");
        for heading in outline(path, &text).into_iter().take(MAX_LISTED_HEADINGS) {
            let _ = write!(out, "\n- {heading}");
        }
        out.push(']');
    }
    Ok(Output {
        text: out,
        structured: None,
    })
}

/// A page's headings as `## Title (line N)`.
fn outline(path: &str, text: &str) -> Vec<String> {
    if !corpus::is_markdown(path) {
        return Vec::new();
    }
    corpus::headings(text)
        .into_iter()
        .map(|h| format!("{} {} (line {})", "#".repeat(h.level), h.title, h.line + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::tests::Scratch;

    fn args(value: Value) -> Map<String, Value> {
        let Value::Object(map) = value else {
            panic!("not an object: {value}");
        };
        map
    }

    fn page() -> String {
        let mut text = String::from("# Guide\n\nintro Idempotency-Key once\n\n## Send\n\nsend\n\n");
        text.push_str(
            "### Idempotency keys\n\nan `Idempotency-Key` header; the Idempotency-Key again\n\n",
        );
        text.push_str("## Media\n\nmedia\n");
        text
    }

    #[test]
    fn search_finds_sections_best_first() {
        let s = Scratch::new("search");
        s.write("docs/guide.md", &page());
        s.write("skills/x/SKILL.md", "# x\n\nnothing here\n");
        let out = call(&s.0, "search", &args(json!({"query": "idempotency-key"}))).unwrap();
        let v = out.structured.unwrap();
        assert_eq!(v["total"], 2);
        assert_eq!(v["hits"][0]["path"], "docs/guide.md");
        assert_eq!(v["hits"][0]["section"], "Idempotency keys");
        assert_eq!(v["hits"][0]["line"], 11);
        assert_eq!(v["hits"][1]["section"], "Guide");
        assert_eq!(serde_json::from_str::<Value>(&out.text).unwrap(), v);
        // Every term must be there.
        let none = call(&s.0, "search", &args(json!({"query": "idempotency media"}))).unwrap();
        assert_eq!(none.structured.unwrap()["total"], 0);
        // `under` narrows.
        let skills = call(
            &s.0,
            "search",
            &args(json!({"query": "nothing", "under": "skills"})),
        )
        .unwrap();
        assert_eq!(
            skills.structured.unwrap()["hits"][0]["path"],
            "skills/x/SKILL.md"
        );
    }

    #[test]
    fn search_is_bounded() {
        let s = Scratch::new("search-bounds");
        for i in 0..30 {
            s.write(
                &format!("docs/p{i:02}.md"),
                &format!("# P{i}\n\nneedle {}\n", "x".repeat(1000)),
            );
        }
        let out = call(&s.0, "search", &args(json!({"query": "needle"}))).unwrap();
        let v = out.structured.unwrap();
        assert_eq!(v["total"], 30);
        assert_eq!(v["hits"].as_array().unwrap().len(), 10);
        for hit in v["hits"].as_array().unwrap() {
            assert!(hit["snippet"].as_str().unwrap().chars().count() <= SNIPPET_CHARS + 2);
        }
        let all = call(
            &s.0,
            "search",
            &args(json!({"query": "needle", "limit": 25})),
        )
        .unwrap();
        assert_eq!(
            all.structured.unwrap()["hits"].as_array().unwrap().len(),
            25
        );
        for bad in [
            json!({"query": "needle", "limit": 26}),
            json!({"query": "needle", "limit": 0}),
        ] {
            assert!(matches!(
                call(&s.0, "search", &args(bad)),
                Err(CallError::Tool(_))
            ));
        }
        let long = "q".repeat(MAX_QUERY_CHARS + 1);
        assert!(matches!(
            call(&s.0, "search", &args(json!({"query": long}))),
            Err(CallError::Tool(_))
        ));
        assert!(matches!(
            call(&s.0, "search", &args(json!({"query": "  "}))),
            Err(CallError::Tool(_))
        ));
    }

    #[test]
    fn read_gives_a_page_or_a_section() {
        let s = Scratch::new("read");
        s.write("docs/guide.md", &page());
        let whole = call(&s.0, "read", &args(json!({"path": "docs/guide.md"}))).unwrap();
        assert_eq!(whole.text, page());
        let section = call(
            &s.0,
            "read",
            &args(json!({"path": "docs/guide.md", "section": "Idempotency keys"})),
        )
        .unwrap();
        assert_eq!(
            section.text,
            "### Idempotency keys\n\nan `Idempotency-Key` header; the Idempotency-Key again\n\n"
        );
        let missing = call(
            &s.0,
            "read",
            &args(json!({"path": "docs/guide.md", "section": "Nope"})),
        );
        let Err(CallError::Tool(why)) = missing else {
            panic!("{missing:?}");
        };
        assert!(why.contains("no section `Nope`"), "{why}");
        assert!(why.contains("### Idempotency keys (line 9)"), "{why}");
    }

    #[test]
    fn a_long_read_is_cut_and_says_where_to_continue() {
        let s = Scratch::new("read-long");
        let mut text = String::from("# Long\n");
        for i in 0..2000 {
            let _ = writeln!(text, "line {i} {}", "y".repeat(40));
        }
        text.push_str("## End\nlast\n");
        s.write("docs/long.md", &text);
        let first = call(&s.0, "read", &args(json!({"path": "docs/long.md"}))).unwrap();
        let (body, notice) = first.text.split_once("\n[truncated").unwrap();
        assert!(body.len() <= MAX_READ_BYTES, "{}", body.len());
        assert!(notice.contains("- # Long (line 1)"), "{notice}");
        assert!(notice.contains("- ## End (line 2002)"), "{notice}");
        let next: u64 = notice
            .split("`from_line`: ")
            .nth(1)
            .unwrap()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let rest = call(
            &s.0,
            "read",
            &args(json!({"path": "docs/long.md", "from_line": next})),
        )
        .unwrap();
        let first_line = rest.text.lines().next().unwrap();
        assert_eq!(first_line, format!("line {} {}", next - 2, "y".repeat(40)));
        // One line longer than the bound is cut at a character.
        s.write(
            "docs/wide.md",
            &format!("# W\n{}\n", "é".repeat(MAX_READ_BYTES)),
        );
        let wide = call(
            &s.0,
            "read",
            &args(json!({"path": "docs/wide.md", "from_line": 2})),
        )
        .unwrap();
        let (cut, notice) = wide.text.split_once("\n[truncated").unwrap();
        assert!(cut.trim_end().len() <= MAX_READ_BYTES, "{}", cut.len());
        assert!(notice.contains("line 2 cut short"), "{notice}");
        assert!(matches!(
            call(
                &s.0,
                "read",
                &args(json!({"path": "docs/long.md", "from_line": 99999}))
            ),
            Err(CallError::Tool(_))
        ));
    }

    #[test]
    fn arguments_are_checked() {
        let s = Scratch::new("args");
        s.write("docs/a.md", "# a\n");
        for (tool, bad) in [
            ("read", json!({})),
            ("read", json!({"path": 3})),
            ("read", json!({"path": "docs/a.md", "extra": 1})),
            ("search", json!({})),
            ("list", json!({"under": ["docs"]})),
        ] {
            assert!(
                matches!(
                    call(&s.0, tool, &args(bad.clone())),
                    Err(CallError::Tool(_))
                ),
                "{tool} {bad}"
            );
        }
        assert!(matches!(
            call(&s.0, "write", &Map::new()),
            Err(CallError::UnknownTool(_))
        ));
    }

    #[test]
    fn list_gives_titles_and_skill_descriptions() {
        let s = Scratch::new("list");
        s.write("docs/a.md", "# Alpha\n");
        s.write(
            "skills/x/SKILL.md",
            "---\nname: x\ndescription: \"Does x.\"\n---\n\n# x\n",
        );
        s.write("skills/x/examples/a.rs", "fn main() {}\n");
        let out = call(&s.0, "list", &Map::new()).unwrap().structured.unwrap();
        assert_eq!(
            out,
            json!({"pages": [
                {"path": "docs/a.md", "title": "Alpha", "description": null, "bytes": 8},
                {"path": "skills/x/SKILL.md", "title": "x", "description": "Does x.", "bytes": 44},
                {"path": "skills/x/examples/a.rs", "title": null, "description": null, "bytes": 13},
            ]})
        );
    }
}
