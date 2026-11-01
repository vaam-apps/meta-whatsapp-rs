//! What the server serves, and the only way it reads a file.
//!
//! The corpus is an allow list, enumerated from the file system on every
//! request (the server keeps no state, and an edit shows on the next
//! call): the root documents in [`ROOT_FILES`], and under each of
//! [`TREES`] the files with one of its extensions. A walk skips
//! hidden entries (`.vitepress/`, and so `.meta-docs/` were it ever
//! under a tree), `node_modules/`, generated `*.d.ts` files, files over
//! [`MAX_FILE_BYTES`] and **every symbolic link**, file or directory.
//!
//! A `read` goes through three guards, each tested alone:
//!
//! 1. [`check_path`]: a relative, `/`-separated path of plain segments
//!    (no `..`, no `.`, no empty segment, no leading `/`, no `\`, no
//!    `:`, no NUL);
//! 2. membership: the path must be one the walk listed;
//! 3. [`open_in_root`]: the path, resolved by the file system, must be
//!    exactly the root joined with it, so a symbolic link anywhere on it
//!    (one swapped in after the walk included) is refused.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Root documents served, when they are regular files.
pub const ROOT_FILES: &[&str] = &[
    "README.md",
    "AGENTS.md",
    "CONTRIBUTING.md",
    "CHANGELOG.md",
    "OPEN_QUESTIONS.md",
];

/// Directories served, each with the extensions it serves: the docs
/// (guides, design, planning, `public/llms.txt`) and the consumer skills
/// with their reference pages and example sources.
pub const TREES: &[(&str, &[&str])] = &[("docs", &["md", "txt"]), ("skills", &["md", "rs", "ts"])];

/// A file larger than this is left out of the corpus (the largest page
/// on 2026-11-01, the changelog, is under a quarter of it).
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The longest path a tool accepts.
pub const MAX_PATH_LEN: usize = 512;

/// How deep a walk goes under a tree.
const MAX_DEPTH: usize = 12;

/// The most files a corpus lists (far above the ~100 it has).
pub const MAX_FILES: usize = 2000;

/// Why a path or a read was refused. Each is a tool execution error (the
/// caller can correct it), never a protocol error.
#[derive(Debug, PartialEq, Eq)]
pub enum CorpusError {
    /// Not a plain relative path inside the corpus.
    BadPath(String),
    /// A plain path, but not a corpus page.
    NotFound(String),
    /// A corpus path that no longer resolves inside the root (a symbolic
    /// link, or a file changed since the walk).
    Escapes(String),
    /// The page has no such section.
    NoSection {
        /// The page.
        path: String,
        /// The section asked for.
        section: String,
        /// The page's headings, to choose from.
        headings: Vec<String>,
    },
    /// The file could not be read.
    Io(String),
}

impl fmt::Display for CorpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadPath(why) => write!(
                f,
                "not a corpus path: {why}. Paths are relative to the repository, `/`-separated, \
                 e.g. `docs/guides/server.md`; `list` gives them all"
            ),
            Self::NotFound(path) => write!(
                f,
                "no such page: `{path}`. `list` gives every page, `search` finds one by its text"
            ),
            Self::Escapes(path) => write!(
                f,
                "`{path}` does not resolve inside the corpus (a symbolic link, or a file \
                 changed since it was listed); it is not served"
            ),
            Self::NoSection {
                path,
                section,
                headings,
            } => {
                write!(f, "`{path}` has no section `{section}`. Its sections:")?;
                for heading in headings {
                    write!(f, "\n- {heading}")?;
                }
                Ok(())
            }
            Self::Io(why) => write!(f, "could not read the page: {why}"),
        }
    }
}

/// The files served, from one root.
#[derive(Debug)]
pub struct Corpus {
    root: PathBuf,
    files: Vec<String>,
}

impl Corpus {
    /// Walk `root` (canonicalized here) for the corpus.
    pub fn load(root: &Path) -> io::Result<Self> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "the root is not a directory",
            ));
        }
        let mut files = Vec::new();
        for name in ROOT_FILES {
            if is_regular_small_file(&root.join(name)) {
                files.push((*name).to_owned());
            }
        }
        for (tree, extensions) in TREES {
            let dir = root.join(tree);
            let is_real_dir = fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_dir());
            if is_real_dir {
                walk(&dir, tree, extensions, 0, &mut files);
            }
        }
        files.sort();
        files.dedup();
        files.truncate(MAX_FILES);
        Ok(Self { root, files })
    }

    /// Every page, sorted.
    #[cfg(test)]
    pub fn files(&self) -> &[String] {
        &self.files
    }

    /// The pages under `prefix` (a path prefix such as `skills/` or
    /// `docs/guides`), all when `None`.
    pub fn files_under(&self, prefix: Option<&str>) -> Result<Vec<&str>, CorpusError> {
        let Some(prefix) = prefix else {
            return Ok(self.files.iter().map(String::as_str).collect());
        };
        let trimmed = prefix.trim_end_matches('/');
        if trimmed.is_empty() {
            return Ok(self.files.iter().map(String::as_str).collect());
        }
        check_path(trimmed)?;
        Ok(self
            .files
            .iter()
            .map(String::as_str)
            .filter(|f| {
                f.strip_prefix(trimmed)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
            .collect())
    }

    /// The text of the page at `path`, through the three guards.
    pub fn read(&self, path: &str) -> Result<String, CorpusError> {
        check_path(path)?;
        if !self.files.iter().any(|f| f == path) {
            return Err(CorpusError::NotFound(path.to_owned()));
        }
        open_in_root(&self.root, path)
    }
}

/// Guard 1: `path` is a plain relative path of normal segments.
pub fn check_path(path: &str) -> Result<(), CorpusError> {
    let bad = |why: &str| Err(CorpusError::BadPath(why.to_owned()));
    if path.is_empty() {
        return bad("the path is empty");
    }
    if path.len() > MAX_PATH_LEN {
        return bad("the path is too long");
    }
    if path.starts_with('/') {
        return bad("an absolute path");
    }
    if path.contains('\\') {
        return bad("a `\\` in the path");
    }
    if path.contains(':') {
        return bad("a `:` in the path");
    }
    if path.chars().any(char::is_control) {
        return bad("a control character in the path");
    }
    for segment in path.split('/') {
        match segment {
            "" => return bad("an empty segment"),
            "." | ".." => return bad("a `.` or `..` segment"),
            _ => {}
        }
    }
    Ok(())
}

/// Guard 3: read `root/rel` only if the file system resolves it to exactly
/// that path (no symbolic link on the way) and it is a regular file within
/// [`MAX_FILE_BYTES`]. `root` must be canonical (as [`Corpus::load`]
/// makes it) and `rel` checked by [`check_path`].
pub fn open_in_root(root: &Path, rel: &str) -> Result<String, CorpusError> {
    let joined = rel.split('/').fold(root.to_path_buf(), |p, s| p.join(s));
    let resolved = joined
        .canonicalize()
        .map_err(|_| CorpusError::NotFound(rel.to_owned()))?;
    if resolved != joined {
        return Err(CorpusError::Escapes(rel.to_owned()));
    }
    if !is_regular_small_file(&resolved) {
        return Err(CorpusError::Escapes(rel.to_owned()));
    }
    let bytes = fs::read(&resolved).map_err(|e| CorpusError::Io(e.kind().to_string()))?;
    String::from_utf8(bytes).map_err(|_| CorpusError::Io("the page is not UTF-8".to_owned()))
}

fn is_regular_small_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file() && m.len() <= MAX_FILE_BYTES)
}

fn walk(dir: &Path, rel: &str, extensions: &[&str], depth: usize, out: &mut Vec<String>) {
    if depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<fs::DirEntry> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') || name == "node_modules" || check_path(&name).is_err() {
            continue;
        }
        // `DirEntry::file_type` does not follow symbolic links: a link is
        // neither a file nor a directory here, and is skipped.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let child = format!("{rel}/{name}");
        if kind.is_dir() {
            walk(&entry.path(), &child, extensions, depth + 1, out);
        } else if kind.is_file()
            && !name.ends_with(".d.ts")
            && Path::new(&name)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| extensions.contains(&e))
            && entry.metadata().is_ok_and(|m| m.len() <= MAX_FILE_BYTES)
        {
            out.push(child);
        }
    }
}

/// Is `path` a page whose headings are read (Markdown or text)?
pub fn is_markdown(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "md" || e == "txt")
}

/// An ATX heading of a Markdown page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// 1 to 6.
    pub level: usize,
    /// The heading's text, without its `#`s.
    pub title: String,
    /// Its line, 0-based.
    pub line: usize,
}

/// The ATX headings of `text`, outside fenced code blocks. Setext
/// headings (a title underlined by `===` or `---`) are not read: the
/// repository's docs do not use them.
pub fn headings(text: &str) -> Vec<Heading> {
    let mut out = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for (line, raw) in text.lines().enumerate() {
        let indent = raw.len() - raw.trim_start_matches(' ').len();
        let body = raw.trim_start_matches(' ');
        if indent <= 3
            && let Some(marker) = body.chars().next().filter(|c| *c == '`' || *c == '~')
        {
            let run = body.chars().take_while(|c| *c == marker).count();
            if run >= 3 {
                match fence {
                    None => fence = Some((marker, run)),
                    Some((open, len))
                        if open == marker && run >= len && body[run..].trim().is_empty() =>
                    {
                        fence = None;
                    }
                    Some(_) => {}
                }
                continue;
            }
        }
        if fence.is_some() || indent > 3 {
            continue;
        }
        let level = body.chars().take_while(|c| *c == '#').count();
        if !(1..=6).contains(&level) {
            continue;
        }
        let rest = &body[level..];
        if !(rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')) {
            continue;
        }
        let mut title = rest.trim();
        // A closing sequence: ` ###` at the end.
        let stripped = title.trim_end_matches('#');
        if stripped.len() < title.len() && (stripped.is_empty() || stripped.ends_with(' ')) {
            title = stripped.trim_end();
        }
        out.push(Heading {
            level,
            title: title.to_owned(),
            line,
        });
    }
    out
}

/// A heading's anchor as GitHub and `VitePress` write it, near enough to
/// match a link's `#fragment`: lowercase, spaces to `-`, only letters,
/// digits, `-` and `_` kept.
pub fn slug(title: &str) -> String {
    title
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// The section of `text` named `wanted`: by its title (case-insensitive,
/// `#`s and a leading `#` of an anchor allowed), else by its anchor. The
/// first match wins. Returns the heading and its line range, `[start,
/// end)`, 0-based: the heading through the line before the next heading
/// of its level or above.
pub fn find_section(text: &str, wanted: &str) -> Option<(Heading, usize, usize)> {
    let all = headings(text);
    let plain = wanted.trim().trim_start_matches('#').trim();
    let lower = plain.to_lowercase();
    let wanted_slug = slug(plain);
    let index = all
        .iter()
        .position(|h| h.title.to_lowercase() == lower)
        .or_else(|| {
            all.iter()
                .position(|h| !wanted_slug.is_empty() && slug(&h.title) == wanted_slug)
        })?;
    let heading = all[index].clone();
    let end = all[index + 1..]
        .iter()
        .find(|h| h.level <= heading.level)
        .map_or_else(|| text.lines().count(), |h| h.line);
    let start = heading.line;
    Some((heading, start, end))
}

/// A Markdown page's title: its first heading.
pub fn title(path: &str, text: &str) -> Option<String> {
    if !is_markdown(path) {
        return None;
    }
    headings(text).into_iter().next().map(|h| h.title)
}

/// The `description:` of a page's YAML front matter (a skill's), on one
/// line, unquoted.
pub fn description(text: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    for line in lines.take(40) {
        if line == "---" {
            return None;
        }
        if let Some(value) = line.strip_prefix("description:") {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            return Some(value.to_owned());
        }
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A scratch directory, removed on drop.
    pub(crate) struct Scratch(pub PathBuf);

    impl Scratch {
        pub(crate) fn new(name: &str) -> Self {
            // Unique per call: tests run in parallel, some with one name.
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "meta-whatsapp-docs-mcp-{}-{n}-{name}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir.canonicalize().unwrap())
        }

        pub(crate) fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn check_path_takes_plain_relative_paths_only() {
        assert_eq!(check_path("docs/guides/server.md"), Ok(()));
        assert_eq!(check_path("README.md"), Ok(()));
        for bad in [
            "",
            "/etc/passwd",
            "../Cargo.toml",
            "docs/../Cargo.toml",
            "docs/./guides/server.md",
            "docs//guides",
            "docs/guides/",
            "docs\\guides\\server.md",
            "C:/Windows",
            "docs/\0server.md",
            "docs/\nserver.md",
            "..",
            ".",
        ] {
            assert!(
                matches!(check_path(bad), Err(CorpusError::BadPath(_))),
                "{bad:?} was accepted"
            );
        }
        assert!(check_path(&"a/".repeat(300)).is_err(), "too long");
    }

    #[test]
    fn the_corpus_is_the_allow_list_and_nothing_else() {
        let s = Scratch::new("allow-list");
        s.write("README.md", "# r\n");
        s.write("Cargo.toml", "[workspace]\n");
        s.write("docs/a.md", "# a\n");
        s.write("docs/public/llms.txt", "# l\n");
        s.write("docs/logo.svg", "<svg/>");
        s.write("docs/.vitepress/config.md", "# hidden\n");
        s.write("docs/node_modules/x/README.md", "# dep\n");
        s.write("skills/s/SKILL.md", "# s\n");
        s.write("skills/s/examples/a.rs", "fn main() {}\n");
        s.write("skills/s/examples/a.ts", "export {};\n");
        s.write("skills/s/examples/gen.d.ts", "export {};\n");
        s.write("skills/s/examples/a.txt", "not served under skills\n");
        s.write(".meta-docs/README.md", "# Meta's\n");
        s.write("crates/c/src/lib.rs", "//! c\n");
        let corpus = Corpus::load(&s.0).unwrap();
        assert_eq!(
            corpus.files(),
            [
                "README.md",
                "docs/a.md",
                "docs/public/llms.txt",
                "skills/s/SKILL.md",
                "skills/s/examples/a.rs",
                "skills/s/examples/a.ts",
            ]
        );
        assert_eq!(
            corpus.read("Cargo.toml"),
            Err(CorpusError::NotFound("Cargo.toml".to_owned()))
        );
        assert_eq!(
            corpus.read(".meta-docs/README.md"),
            Err(CorpusError::NotFound(".meta-docs/README.md".to_owned()))
        );
        assert_eq!(corpus.read("docs/a.md").as_deref(), Ok("# a\n"));
        assert_eq!(
            corpus.files_under(Some("skills/")).unwrap(),
            [
                "skills/s/SKILL.md",
                "skills/s/examples/a.rs",
                "skills/s/examples/a.ts"
            ]
        );
        // A prefix matches whole segments: `doc` is not `docs`.
        assert!(corpus.files_under(Some("doc")).unwrap().is_empty());
        assert!(corpus.files_under(Some("../")).is_err());
    }

    #[test]
    fn a_file_over_the_size_bound_is_left_out() {
        let s = Scratch::new("size");
        s.write("docs/small.md", "# s\n");
        s.write(
            "docs/big.md",
            &"x".repeat(usize::try_from(MAX_FILE_BYTES).unwrap() + 1),
        );
        let corpus = Corpus::load(&s.0).unwrap();
        assert_eq!(corpus.files(), ["docs/small.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_walk_skips_symbolic_links() {
        use std::os::unix::fs::symlink;
        let s = Scratch::new("walk-links");
        let outside = Scratch::new("walk-links-outside");
        outside.write("secret.md", "# secret\n");
        s.write("docs/a.md", "# a\n");
        symlink(outside.0.join("secret.md"), s.0.join("docs/link.md")).unwrap();
        symlink(&outside.0, s.0.join("docs/linkdir")).unwrap();
        symlink(outside.0.join("secret.md"), s.0.join("README.md")).unwrap();
        let corpus = Corpus::load(&s.0).unwrap();
        assert_eq!(corpus.files(), ["docs/a.md"]);
        assert!(matches!(
            corpus.read("docs/link.md"),
            Err(CorpusError::NotFound(_))
        ));
        assert!(matches!(
            corpus.read("docs/linkdir/secret.md"),
            Err(CorpusError::NotFound(_))
        ));
    }

    /// Guard 3 alone, without the walk's list in front of it: a link
    /// swapped in after the walk, as a file or as a directory on the way,
    /// is refused.
    #[cfg(unix)]
    #[test]
    fn open_in_root_refuses_a_symbolic_link_on_the_way() {
        use std::os::unix::fs::symlink;
        let s = Scratch::new("open-links");
        let outside = Scratch::new("open-links-outside");
        outside.write("secret.md", "# secret\n");
        s.write("docs/a.md", "# a\n");
        symlink(outside.0.join("secret.md"), s.0.join("docs/link.md")).unwrap();
        symlink(&outside.0, s.0.join("docs/linkdir")).unwrap();
        // A link that stays inside the root is refused too: it is not
        // what the walk listed.
        symlink(s.0.join("docs/a.md"), s.0.join("docs/inside.md")).unwrap();
        assert_eq!(open_in_root(&s.0, "docs/a.md").as_deref(), Ok("# a\n"));
        for rel in ["docs/link.md", "docs/linkdir/secret.md", "docs/inside.md"] {
            assert_eq!(
                open_in_root(&s.0, rel),
                Err(CorpusError::Escapes(rel.to_owned())),
                "{rel}"
            );
        }
        assert!(matches!(
            open_in_root(&s.0, "docs/missing.md"),
            Err(CorpusError::NotFound(_))
        ));
        // A directory is not a page.
        assert!(matches!(
            open_in_root(&s.0, "docs"),
            Err(CorpusError::Escapes(_))
        ));
    }

    #[test]
    fn headings_skip_fenced_code_and_strip_closing_hashes() {
        let text = "# Title\n\nintro\n\n```bash\n# not a heading\n```\n\n## Two ##\n\
                    ~~~~\n## nor this\n~~~\n## still fenced\n~~~~\n### Three\n#nospace\n    # indented code\n";
        let found: Vec<(usize, String, usize)> = headings(text)
            .into_iter()
            .map(|h| (h.level, h.title, h.line))
            .collect();
        assert_eq!(
            found,
            [
                (1, "Title".to_owned(), 0),
                (2, "Two".to_owned(), 8),
                (3, "Three".to_owned(), 14),
            ]
        );
    }

    #[test]
    fn a_section_runs_to_the_next_heading_of_its_level() {
        let text = "# T\n## A\na\n### A1\na1\n## B\nb\n";
        let (h, start, end) = find_section(text, "A").unwrap();
        assert_eq!((h.title.as_str(), start, end), ("A", 1, 5));
        let (_, start, end) = find_section(text, "### a1").unwrap();
        assert_eq!((start, end), (3, 5));
        let (_, start, end) = find_section(text, "b").unwrap();
        assert_eq!((start, end), (5, 7));
        assert!(find_section(text, "C").is_none());
        // By anchor.
        let text = "# T\n## Check a deployment: doctor\nx\n";
        let (h, _, _) = find_section(text, "#check-a-deployment-doctor").unwrap();
        assert_eq!(h.title, "Check a deployment: doctor");
    }

    #[test]
    fn a_skill_has_its_front_matter_description() {
        let text = "---\nname: x\ndescription: \"Does x. Load when y.\"\n---\n\n# x\n";
        assert_eq!(description(text).as_deref(), Some("Does x. Load when y."));
        assert_eq!(title("skills/x/SKILL.md", text).as_deref(), Some("x"));
        assert_eq!(description("# no front matter\n"), None);
        assert_eq!(title("skills/x/examples/a.rs", "# not markdown"), None);
    }
}
