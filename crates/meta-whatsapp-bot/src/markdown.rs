//! Markdown → WhatsApp formatting, split into text messages.
//!
//! [`render`] turns Markdown (a bot's help page, an LLM's answer) into the
//! markup the WhatsApp clients display, then splits it into messages of at
//! most [`TEXT_MAX_CHARS`] characters on block boundaries. [`Renderer`]
//! does it with options; [`MarkdownRenderer`] is the trait a bot renders
//! replies with, for rules of your own.
//!
//! | Markdown | WhatsApp |
//! | --- | --- |
//! | `**bold**`, `__bold__`, headings | `*bold*` (a heading is a bold line) |
//! | `*italic*`, `_italic_` | `_italic_` |
//! | `~~strike~~` | `~strike~` |
//! | emphasis inside a word (`foo**bar**baz`) | the text alone (WhatsApp formats no part of a word) |
//! | `` `code` `` | `` `code` ``; its text alone when it holds a backtick |
//! | fenced or indented code | ```` ```code``` ```` (language dropped); its text alone when it holds ```` ``` ```` or starts or ends with a backtick (a fence could not close) |
//! | `> quote` | `> quote`, on every line |
//! | `- item`, `1. item` | `• item`, `1. item` (nested: indented; quotes and lists deeper than 16 levels are flattened into the 16th) |
//! | `[text](url)` | `text (url)`; a link whose text is its URL, or an autolink: `url` (only `http`, `https`, `mailto`, `tel` or relative URLs: any other scheme, such as `javascript:` or `data:`, keeps just the text) |
//! | tables | a monospace block, columns padded |
//! | `![alt](url)` | `alt (url)`, or `url` without alt text (the same URL rule) |
//! | `---` | a line of `———` |
//! | raw HTML | its text |
//!
//! A line break in the source is a line break in the message (WhatsApp
//! shows what the author typed, not a reflowed paragraph).
//!
//! # Escaping
//!
//! Meta documents no escape syntax for WhatsApp text, so literal `*`, `_`,
//! `~`, `` ` `` or a leading `>` in the Markdown's text (`\*`, `snake_case`)
//! can still format in WhatsApp. The default, [`NoEscape`], leaves the text
//! exactly as written: what a reader copies out of a reply (an email
//! address, a coupon code, a `/command`, a bare `www.` link) is what the
//! Markdown said. [`WordJoinerEscape`] is the opt-in alternative: it puts
//! an invisible U+2060 WORD JOINER around each such character so it can
//! neither open nor close a span, but those invisible characters are
//! copied with the text (a copied `john_doe@example.com` or `/add_item`
//! no longer works) and cut WhatsApp's link detection short; whether a
//! client honours them at all is WhatsApp's behaviour, not a documented
//! contract. Choose with [`Renderer::escape`] (or your own [`Escape`]).
//!
//! # Splitting
//!
//! Meta's text body limit is 4096 characters (`messages/text-messages`,
//! the client's `TEXT_BODY_MAX_CHARS`). Meta does not say which unit it
//! counts, so parts are measured in UTF-16 code units: never fewer than
//! the Unicode scalar values the client counts, so a part fits under
//! either reading (an emoji outside the Basic Multilingual Plane counts
//! twice, and dense emoji make more, shorter parts). Blocks
//! (paragraphs, headings, list items, quotes, code blocks, tables) are
//! packed into messages whole, separated as in one message; a block goes
//! to the next message when it does not fit, so a code block that fits in
//! a message is never cut. A block longer than a message is cut at line
//! breaks, then at spaces, then anywhere (a code block keeps its fences on
//! every piece, but for a piece that starts or ends with a backtick, which
//! goes out plain); formatting spanning such a cut is not repaired.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd, TextMergeStream};

/// Longest text body Meta accepts, in characters: the client's
/// `meta_whatsapp_client::messages::TEXT_BODY_MAX_CHARS`
/// (`messages/text-messages`: "Maximum 4096 characters"). The renderer
/// measures parts in UTF-16 code units against it (see
/// [Splitting](self#splitting)).
pub const TEXT_MAX_CHARS: usize = meta_whatsapp_client::messages::TEXT_BODY_MAX_CHARS;

/// What a thematic break (`---`) renders to.
const RULE: &str = "———";

/// Invisible, zero-width, and no line break opportunity.
const WORD_JOINER: char = '\u{2060}';

/// [`Renderer::render`] with the defaults: [`TEXT_MAX_CHARS`] per message,
/// [`NoEscape`].
pub fn render(markdown: &str) -> Vec<String> {
    Renderer::default().render(markdown)
}

/// Split already formatted `text` into messages of at most `max_chars`
/// UTF-16 code units (so at most as many characters), at blank lines
/// (paragraph boundaries) when possible, then as [`render`] cuts an
/// oversized block. Fences in `text` are not recognised: render Markdown
/// with [`render`] instead.
pub fn split(text: &str, max_chars: usize) -> Vec<String> {
    let blocks = text
        .split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .map(|b| Block::text(b.to_owned(), Sep::Paragraph))
        .collect();
    pack(blocks, max_chars.max(1))
}

/// Keeps literal text from turning into WhatsApp formatting. See the
/// [module docs](self#escaping).
pub trait Escape: Send + Sync + fmt::Debug + 'static {
    /// Append `text`, escaped, to `out`. Called once per run of literal
    /// text (never for code, URLs of links, or table cells).
    fn escape(&self, text: &str, out: &mut String);
}

impl<T: Escape + ?Sized> Escape for Arc<T> {
    fn escape(&self, text: &str, out: &mut String) {
        (**self).escape(text, out);
    }
}

/// Markdown → the text messages a reply sends (`Ctx::reply_markdown`).
/// The default is [`Renderer`]; implement it for rules of your own
/// (another heading style, another table layout, no splitting of your
/// own content) and set it with `BotBuilder::markdown`.
pub trait MarkdownRenderer: Send + Sync + fmt::Debug + 'static {
    /// `markdown` as messages, in order; none for empty Markdown. Each
    /// part must pass the client's text check (at most
    /// [`TEXT_MAX_CHARS`] characters, not blank), or its send fails.
    fn render(&self, markdown: &str) -> Vec<String>;
}

impl<T: MarkdownRenderer + ?Sized> MarkdownRenderer for Arc<T> {
    fn render(&self, markdown: &str) -> Vec<String> {
        (**self).render(markdown)
    }
}

/// An opt-in [`Escape`]: U+2060 WORD JOINER around `*`, `_`, `~` and
/// `` ` ``, before a `>` that starts a run or a line; `http(s)://` URLs
/// untouched. The joiners are copied with the text, so an email address,
/// a code or a `/command` with an `_` in it no longer works once copied;
/// see the [module docs](self#escaping).
#[derive(Debug, Clone, Copy, Default)]
pub struct WordJoinerEscape;

impl Escape for WordJoinerEscape {
    fn escape(&self, text: &str, out: &mut String) {
        let mut rest = text;
        let mut line_start = true;
        while let Some(c) = rest.chars().next() {
            if let Some(len) = url_len(rest) {
                out.push_str(&rest[..len]);
                rest = &rest[len..];
                line_start = false;
                continue;
            }
            match c {
                '*' | '_' | '~' | '`' => {
                    out.push(WORD_JOINER);
                    out.push(c);
                    out.push(WORD_JOINER);
                }
                '>' if line_start => {
                    out.push(WORD_JOINER);
                    out.push(c);
                }
                c => out.push(c),
            }
            line_start = c == '\n';
            rest = &rest[c.len_utf8()..];
        }
    }
}

/// The length of the URL `text` starts with (`http://` or `https://`, any
/// case, up to the next whitespace), if it starts with one.
fn url_len(text: &str) -> Option<usize> {
    let starts = |scheme: &str| {
        text.get(..scheme.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
    };
    if !(starts("http://") || starts("https://")) {
        return None;
    }
    Some(text.find(char::is_whitespace).unwrap_or(text.len()))
}

/// The default [`Escape`]: escapes nothing, so text reads and copies
/// exactly as written; literal `*bold*` in the Markdown's text may show as
/// bold.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEscape;

impl Escape for NoEscape {
    fn escape(&self, text: &str, out: &mut String) {
        out.push_str(text);
    }
}

/// Markdown → WhatsApp text messages: the default [`MarkdownRenderer`].
/// See the [module docs](self).
#[derive(Debug, Clone)]
pub struct Renderer {
    max_chars: usize,
    escape: Arc<dyn Escape>,
}

impl Default for Renderer {
    fn default() -> Self {
        Self {
            max_chars: TEXT_MAX_CHARS,
            escape: Arc::new(NoEscape),
        }
    }
}

impl Renderer {
    /// The defaults: [`TEXT_MAX_CHARS`], [`NoEscape`].
    pub fn new() -> Self {
        Self::default()
    }

    /// At most `max_chars` per message, in UTF-16 code units: at least 1,
    /// at most [`TEXT_MAX_CHARS`] (a longer part would fail the client's
    /// check before it is sent).
    #[must_use]
    pub fn max_chars(mut self, max_chars: usize) -> Self {
        self.max_chars = max_chars.clamp(1, TEXT_MAX_CHARS);
        self
    }

    /// Escape literal text with `escape`.
    #[must_use]
    pub fn escape(mut self, escape: impl Escape) -> Self {
        self.escape = Arc::new(escape);
        self
    }

    /// Render `markdown` and split it into messages, in order. Empty (or
    /// blank) Markdown gives no message.
    pub fn render(&self, markdown: &str) -> Vec<String> {
        pack(self.blocks(markdown), self.max_chars)
    }

    /// Render `markdown` into one text, however long.
    pub fn render_unsplit(&self, markdown: &str) -> String {
        let mut out = String::new();
        for (i, block) in self.blocks(markdown).into_iter().enumerate() {
            if i > 0 {
                out.push_str(block.sep.as_str());
            }
            out.push_str(&block.text);
        }
        out
    }

    fn blocks(&self, markdown: &str) -> Vec<Block> {
        // (Tables and strikethrough only: footnotes, math and the rest are
        // read as text.)
        let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
        let events = TextMergeStream::new(Parser::new_ext(markdown, options));
        let mut builder = Builder::new(self.escape.as_ref());
        for event in events {
            builder.event(event);
        }
        to_blocks(builder.finish())
    }
}

impl MarkdownRenderer for Renderer {
    fn render(&self, markdown: &str) -> Vec<String> {
        Renderer::render(self, markdown)
    }
}

// ─── Parsing into a tree ─────────────────────────────────────────────────

/// A block, its inline content already rendered.
#[derive(Debug)]
enum Node {
    /// A paragraph, a heading, raw HTML.
    Text(String),
    /// A code block's content, or a formatted table: rendered in a fence.
    Code(String),
    Quote(Vec<Node>),
    List {
        start: Option<u64>,
        items: Vec<Vec<Node>>,
    },
    Rule,
}

/// An open container.
enum Frame {
    Root(Vec<Node>),
    Quote(Vec<Node>),
    List {
        start: Option<u64>,
        items: Vec<Vec<Node>>,
    },
    Item(Vec<Node>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    Bold,
    Italic,
    Strike,
}

impl Style {
    fn marker(self) -> char {
        match self {
            Self::Bold => '*',
            Self::Italic => '_',
            Self::Strike => '~',
        }
    }
}

/// An open link or image: where its text starts, its URL, its text as
/// written.
struct Link {
    start: usize,
    url: String,
    raw: String,
    auto: bool,
}

/// Inline content being rendered.
struct Inline {
    out: String,
    heading: bool,
    /// A table cell: no markers, no escaping (the table is monospace).
    plain: bool,
    depth: [usize; 3],
    /// Where each style's opening marker is in `out`, while it is open.
    opened: [Option<usize>; 3],
    /// The markers written, as byte positions of the opening and the
    /// closing one in `out`.
    spans: Vec<(usize, usize)>,
    links: Vec<Link>,
}

impl Inline {
    fn new(heading: bool, plain: bool) -> Self {
        Self {
            out: String::new(),
            heading,
            plain,
            // A heading is bold already: bold inside it adds no markers.
            depth: [usize::from(heading), 0, 0],
            opened: [None; 3],
            spans: Vec::new(),
            links: Vec::new(),
        }
    }

    fn open(&mut self, style: Style) {
        let depth = &mut self.depth[style as usize];
        *depth += 1;
        if *depth == 1 && !self.plain {
            self.opened[style as usize] = Some(self.out.len());
            self.out.push(style.marker());
        }
    }

    fn close(&mut self, style: Style) {
        let depth = &mut self.depth[style as usize];
        *depth = depth.saturating_sub(1);
        if *depth == 0
            && let Some(open) = self.opened[style as usize].take()
        {
            self.spans.push((open, self.out.len()));
            self.out.push(style.marker());
        }
    }

    fn text(&mut self, text: &str, escape: &dyn Escape) {
        if let Some(link) = self.links.last_mut() {
            link.raw.push_str(text);
        }
        if self.plain {
            self.out.push_str(text);
        } else {
            escape.escape(text, &mut self.out);
        }
    }

    /// Verbatim: code, line breaks.
    fn raw(&mut self, text: &str) {
        if let Some(link) = self.links.last_mut() {
            link.raw.push_str(text);
        }
        self.out.push_str(text);
    }

    fn open_link(&mut self, url: &str, auto: bool) {
        self.links.push(Link {
            start: self.out.len(),
            url: url.to_owned(),
            raw: String::new(),
            auto,
        });
    }

    /// `text (url)`; just `url` when the text is empty or is the URL
    /// (autolinks included), just the text when there is no URL or it is
    /// the text's `mailto:`. A URL [`url_allowed`] refuses is never
    /// written: the link is its text alone (nothing when that text is the
    /// URL).
    fn close_link(&mut self) {
        let Some(link) = self.links.pop() else {
            return;
        };
        if !url_allowed(&link.url) {
            if link.auto || link.raw == link.url {
                self.truncate(link.start);
            }
            return;
        }
        let text = self.out[link.start..].trim();
        let url = link.url.as_str();
        let mailto = url
            .strip_prefix("mailto:")
            .is_some_and(|address| address == link.raw);
        if link.auto || text.is_empty() || link.raw == url {
            self.truncate(link.start);
            self.out.push_str(url);
        } else if !url.is_empty() && !mailto {
            self.out.push_str(" (");
            self.out.push_str(url);
            self.out.push(')');
        }
    }

    /// Drop what was written from `at` on, markers included.
    fn truncate(&mut self, at: usize) {
        self.out.truncate(at);
        self.spans.retain(|&(open, _)| open < at);
    }

    fn finish(self) -> String {
        let out = without_intraword_markers(&self.out, &self.spans);
        let text = out.trim();
        if self.heading && !text.is_empty() {
            format!("*{text}*")
        } else {
            text.to_owned()
        }
    }
}

/// `out` without the markers of the `spans` that sit inside a word: a
/// letter or digit right before the opening marker or right after the
/// closing one (other markers skipped). WhatsApp formats no part of a
/// word, so `foo*bar*baz` would show its asterisks.
fn without_intraword_markers(out: &str, spans: &[(usize, usize)]) -> String {
    let markers: HashSet<usize> = spans.iter().flat_map(|&(o, c)| [o, c]).collect();
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let mut dropped = HashSet::new();
    for &(open, close) in spans {
        let before = out[..open]
            .char_indices()
            .rev()
            .find(|(i, _)| !markers.contains(i))
            .map(|(_, c)| c);
        let after = out[close + 1..]
            .char_indices()
            .find(|(i, _)| !markers.contains(&(close + 1 + i)))
            .map(|(_, c)| c);
        if word(before) || word(after) {
            dropped.extend([open, close]);
        }
    }
    if dropped.is_empty() {
        return out.to_owned();
    }
    out.char_indices()
        .filter(|(i, _)| !dropped.contains(i))
        .map(|(_, c)| c)
        .collect()
}

/// Schemes a link or image URL may have to be written into a message.
const URL_SCHEMES: [&str; 4] = ["http", "https", "mailto", "tel"];

/// Whether a link's or image's `url` may be written into a message: an
/// `http`, `https`, `mailto` or `tel` URL, or a relative one (no scheme).
/// Anything else (`javascript:`, `data:`, `file:`, …) is left out. The
/// scheme is read the way browsers read it: leading spaces and control
/// characters skipped, tabs and line breaks ignored, any case.
fn url_allowed(url: &str) -> bool {
    let cleaned: String = url
        .trim_start_matches(|c: char| c == ' ' || c.is_ascii_control())
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let Some((scheme, _)) = cleaned.split_once(':') else {
        return true;
    };
    let is_scheme = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    // Not a scheme (`/a:b`, `?q=x:y`): a relative URL.
    !is_scheme || URL_SCHEMES.iter().any(|s| scheme.eq_ignore_ascii_case(s))
}

/// A table being read: rows of rendered cells, the header first.
#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    header: bool,
}

/// Deepest nesting of quotes and lists kept. Deeper ones are flattened
/// into the deepest kept one (their text stays): WhatsApp shows no such
/// structure, and rendering recurses once per level, so 4096 characters
/// of `>` or `- ` would otherwise overflow a worker thread's stack and
/// abort the process.
const MAX_NESTING: usize = 16;

struct Builder<'e> {
    escape: &'e dyn Escape,
    frames: Vec<Frame>,
    /// For each open quote or list, whether it got its own frame (at most
    /// [`MAX_NESTING`] do).
    containers: Vec<bool>,
    /// How many of `containers` got a frame.
    kept: usize,
    inline: Option<Inline>,
    code: Option<String>,
    table: Option<Table>,
    cell: Option<Inline>,
}

impl<'e> Builder<'e> {
    fn new(escape: &'e dyn Escape) -> Self {
        Self {
            escape,
            frames: vec![Frame::Root(Vec::new())],
            containers: Vec::new(),
            kept: 0,
            inline: None,
            code: None,
            table: None,
            cell: None,
        }
    }

    /// The inline run text goes to: the open table cell, else the open
    /// paragraph or heading, else a new run (the text of a tight list item
    /// comes without a paragraph).
    fn inline(&mut self) -> &mut Inline {
        if let Some(cell) = self.cell.as_mut() {
            return cell;
        }
        self.inline.get_or_insert_with(|| Inline::new(false, false))
    }

    /// A quote or list opens: whether it gets its own frame (not beyond
    /// [`MAX_NESTING`]; deeper ones add their content to the open frame).
    fn open_container(&mut self) -> bool {
        let keep = self.kept < MAX_NESTING;
        self.containers.push(keep);
        self.kept += usize::from(keep);
        keep
    }

    /// A quote or list closes: whether it had its own frame to close.
    fn close_container(&mut self) -> bool {
        let kept = self.containers.pop().unwrap_or(false);
        self.kept -= usize::from(kept);
        kept
    }

    fn push(&mut self, node: Node) {
        match self.frames.last_mut() {
            Some(Frame::Root(nodes) | Frame::Quote(nodes) | Frame::Item(nodes)) => nodes.push(node),
            // Markdown puts nothing in a list but items; keep it anyway.
            Some(Frame::List { items, .. }) => match items.last_mut() {
                Some(item) => item.push(node),
                None => items.push(vec![node]),
            },
            None => self.frames.push(Frame::Root(vec![node])),
        }
    }

    fn flush(&mut self) {
        if let Some(inline) = self.inline.take() {
            let text = inline.finish();
            if !text.is_empty() {
                self.push(Node::Text(text));
            }
        }
    }

    fn event(&mut self, event: Event<'_>) {
        if let Some(code) = self.code.as_mut() {
            match event {
                Event::Text(text) => code.push_str(&text),
                Event::End(TagEnd::CodeBlock) => {
                    let code = self.code.take().unwrap_or_default();
                    let code = code.trim_end_matches('\n');
                    if !code.trim().is_empty() {
                        self.push(Node::Code(code.to_owned()));
                    }
                }
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                let escape = self.escape;
                self.inline().text(&text, escape);
            }
            Event::Code(code) => {
                let inline = self.inline();
                // A backtick inside would close the span early: the code's
                // text alone.
                if inline.plain || code.contains('`') {
                    inline.raw(&code);
                } else {
                    inline.raw("`");
                    inline.raw(&code);
                    inline.raw("`");
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                let inline = self.inline();
                let brk = if inline.plain { " " } else { "\n" };
                inline.raw(brk);
            }
            Event::Rule => {
                self.flush();
                self.push(Node::Rule);
            }
            Event::TaskListMarker(done) => {
                self.inline().raw(if done { "☑ " } else { "☐ " });
            }
            Event::FootnoteReference(_) | Event::InlineMath(_) | Event::DisplayMath(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph | Tag::HtmlBlock => {
                self.flush();
                self.inline = Some(Inline::new(false, false));
            }
            Tag::Heading { .. } => {
                self.flush();
                self.inline = Some(Inline::new(true, false));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                if self.open_container() {
                    self.frames.push(Frame::Quote(Vec::new()));
                }
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.code = Some(String::new());
            }
            Tag::List(start) => {
                self.flush();
                if self.open_container() {
                    self.frames.push(Frame::List {
                        start,
                        items: Vec::new(),
                    });
                }
            }
            Tag::Item => {
                self.flush();
                self.frames.push(Frame::Item(Vec::new()));
            }
            Tag::Table(_) => {
                self.flush();
                self.table = Some(Table::default());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.row.clear();
                }
            }
            Tag::TableCell => self.cell = Some(Inline::new(false, true)),
            Tag::Emphasis => self.inline().open(Style::Italic),
            Tag::Strong => self.inline().open(Style::Bold),
            Tag::Strikethrough => self.inline().open(Style::Strike),
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => {
                let auto = matches!(link_type, LinkType::Autolink | LinkType::Email);
                self.inline().open_link(&dest_url, auto);
            }
            Tag::Image { dest_url, .. } => self.inline().open_link(&dest_url, false),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::HtmlBlock => self.flush(),
            TagEnd::BlockQuote(_) => {
                self.flush();
                if self.close_container()
                    && let Some(Frame::Quote(nodes)) = self.frames.pop()
                {
                    self.push(Node::Quote(nodes));
                }
            }
            TagEnd::List(_) => {
                self.flush();
                if self.close_container()
                    && let Some(Frame::List { start, items }) = self.frames.pop()
                {
                    self.push(Node::List { start, items });
                }
            }
            TagEnd::Item => {
                self.flush();
                if let Some(Frame::Item(nodes)) = self.frames.pop() {
                    match self.frames.last_mut() {
                        Some(Frame::List { items, .. }) => items.push(nodes),
                        _ => {
                            for node in nodes {
                                self.push(node);
                            }
                        }
                    }
                }
            }
            TagEnd::TableCell => {
                if let (Some(cell), Some(table)) = (self.cell.take(), self.table.as_mut()) {
                    table.row.push(cell.finish());
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    let row = std::mem::take(&mut table.row);
                    table.header |= matches!(tag, TagEnd::TableHead) && table.rows.is_empty();
                    table.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.push(Node::Code(format_table(&table)));
                }
            }
            TagEnd::Emphasis => self.inline().close(Style::Italic),
            TagEnd::Strong => self.inline().close(Style::Bold),
            TagEnd::Strikethrough => self.inline().close(Style::Strike),
            TagEnd::Link | TagEnd::Image => self.inline().close_link(),
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Node> {
        self.flush();
        // Close whatever the parser left open (it never does).
        while self.frames.len() > 1 {
            match self.frames.pop() {
                Some(Frame::Quote(nodes)) => self.push(Node::Quote(nodes)),
                Some(Frame::List { start, items }) => self.push(Node::List { start, items }),
                Some(Frame::Item(nodes) | Frame::Root(nodes)) => {
                    for node in nodes {
                        self.push(node);
                    }
                }
                None => break,
            }
        }
        match self.frames.pop() {
            Some(Frame::Root(nodes)) => nodes,
            _ => Vec::new(),
        }
    }
}

/// Characters as displayed (table columns, indentation).
fn chars(text: &str) -> usize {
    text.chars().count()
}

/// Length as the split measures it: UTF-16 code units, never fewer than
/// the characters the client counts.
fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Columns padded to their widest cell, ` | ` between them, a `-|-` rule
/// under the header.
fn format_table(table: &Table) -> String {
    let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0; columns];
    for row in &table.rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(chars(cell));
        }
    }
    let line = |cells: Vec<String>| cells.join(" | ").trim_end().to_owned();
    let mut lines = Vec::new();
    for (r, row) in table.rows.iter().enumerate() {
        let cells = (0..columns)
            .map(|i| {
                // Padded by hand: `format!`'s width is capped (65 535), and
                // a wider cell would panic there.
                let cell = row.get(i).map_or("", String::as_str);
                let mut padded = cell.to_owned();
                padded.extend(std::iter::repeat_n(' ', widths[i] - chars(cell)));
                padded
            })
            .collect();
        lines.push(line(cells));
        if r == 0 && table.header {
            lines.push(
                widths
                    .iter()
                    .map(|w| "-".repeat((*w).max(1)))
                    .collect::<Vec<_>>()
                    .join("-|-"),
            );
        }
    }
    lines.join("\n")
}

// ─── Blocks and packing ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sep {
    /// A blank line: paragraphs, headings, code, quotes, lists.
    Paragraph,
    /// A line break: the items of one list.
    Line,
}

impl Sep {
    fn as_str(self) -> &'static str {
        match self {
            Self::Paragraph => "\n\n",
            Self::Line => "\n",
        }
    }
}

/// A unit of packing: never cut unless it is longer than a message.
#[derive(Debug)]
struct Block {
    text: String,
    /// What goes between it and the block before, in the same message.
    sep: Sep,
    /// A code block's content: cut, it is fenced again piece by piece.
    code: Option<String>,
}

impl Block {
    fn text(text: String, sep: Sep) -> Self {
        Self {
            text,
            sep,
            code: None,
        }
    }
}

fn fence(code: &str) -> String {
    format!("```{code}```")
}

/// Whether `code` cannot sit in a fence: it holds one, or a backtick at an
/// end would run into it.
fn unfenceable(code: &str) -> bool {
    code.contains("```") || code.starts_with('`') || code.ends_with('`')
}

fn quote(text: &str) -> String {
    text.lines()
        .map(|l| format!("> {l}").trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn marker(start: Option<u64>, index: usize) -> String {
    match start {
        Some(n) => format!("{}. ", n.saturating_add(index as u64)),
        None => "• ".to_owned(),
    }
}

/// `marker` before the first line, the others indented under it.
fn item(marker: &str, nodes: Vec<Node>) -> String {
    let body = nested(nodes);
    let indent = " ".repeat(chars(marker));
    let mut out = String::new();
    for (i, line) in body.lines().enumerate() {
        if i == 0 {
            out.push_str(marker);
        } else {
            out.push('\n');
            if !line.is_empty() {
                out.push_str(&indent);
            }
        }
        out.push_str(line);
    }
    if out.is_empty() {
        out.push_str(marker.trim_end());
    }
    out
}

/// Nodes inside a quote or a list item: one per line.
fn nested(nodes: Vec<Node>) -> String {
    let mut lines = Vec::new();
    for node in nodes {
        match node {
            Node::Text(text) => lines.push(text),
            Node::Code(code) if unfenceable(&code) => lines.push(code),
            Node::Code(code) => lines.push(fence(&code)),
            Node::Rule => lines.push(RULE.to_owned()),
            Node::Quote(children) => lines.push(quote(&nested(children))),
            Node::List { start, items } => {
                for (i, children) in items.into_iter().enumerate() {
                    lines.push(item(&marker(start, i), children));
                }
            }
        }
    }
    lines.join("\n")
}

fn to_blocks(nodes: Vec<Node>) -> Vec<Block> {
    let mut blocks = Vec::new();
    for node in nodes {
        match node {
            Node::Text(text) => blocks.push(Block::text(text, Sep::Paragraph)),
            Node::Code(code) if unfenceable(&code) => {
                blocks.push(Block::text(code, Sep::Paragraph));
            }
            Node::Code(code) => blocks.push(Block {
                text: fence(&code),
                sep: Sep::Paragraph,
                code: Some(code),
            }),
            Node::Rule => blocks.push(Block::text(RULE.to_owned(), Sep::Paragraph)),
            Node::Quote(children) => {
                blocks.push(Block::text(quote(&nested(children)), Sep::Paragraph));
            }
            Node::List { start, items } => {
                for (i, children) in items.into_iter().enumerate() {
                    let sep = if i == 0 { Sep::Paragraph } else { Sep::Line };
                    blocks.push(Block::text(item(&marker(start, i), children), sep));
                }
            }
        }
    }
    blocks.retain(|b| !b.text.trim().is_empty());
    blocks
}

/// Blocks into messages of at most `max` UTF-16 code units: whole blocks
/// while they fit, oversized ones cut ([`cut`]).
fn pack(blocks: Vec<Block>, max: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut length = 0;
    for block in blocks {
        let pieces = if units(&block.text) <= max {
            vec![block.text]
        } else {
            cut(&block, max)
        };
        for (i, piece) in pieces.into_iter().enumerate() {
            let piece_length = units(&piece);
            let sep = block.sep.as_str();
            if current.is_empty() {
                current = piece;
                length = piece_length;
            } else if i == 0 && length + sep.len() + piece_length <= max {
                current.push_str(sep);
                current.push_str(&piece);
                length += sep.len() + piece_length;
            } else {
                parts.push(std::mem::take(&mut current));
                current = piece;
                length = piece_length;
            }
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// An oversized block in pieces of at most `max` code units: a code block
/// by lines, fenced again (when a fence fits at all; a piece that starts
/// or ends with a backtick could not close its fence, so it goes out
/// plain, like a whole block would); anything else by lines, then words,
/// then characters.
fn cut(block: &Block, max: usize) -> Vec<String> {
    let fences = units(&fence(""));
    match &block.code {
        // Room for a character of two units inside the fences.
        Some(code) if max >= fences + 2 => lines(code, max - fences)
            .into_iter()
            .map(|piece| {
                if unfenceable(&piece) {
                    piece
                } else {
                    fence(&piece)
                }
            })
            .collect(),
        _ => lines(&block.text, max),
    }
}

/// `text` in pieces of at most `max` code units, cut at line breaks where
/// possible; blank pieces are dropped.
fn lines(text: &str, max: usize) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    let mut current: Option<(String, usize)> = None;
    for line in text.split('\n') {
        let parts = if units(line) > max {
            words(line, max)
        } else {
            vec![line.to_owned()]
        };
        for part in parts {
            let n = units(&part);
            match current.as_mut() {
                Some((text, length)) if *length + 1 + n <= max => {
                    text.push('\n');
                    text.push_str(&part);
                    *length += 1 + n;
                }
                _ => {
                    if let Some((text, _)) = current.take() {
                        pieces.push(text);
                    }
                    current = Some((part, n));
                }
            }
        }
    }
    if let Some((text, _)) = current {
        pieces.push(text);
    }
    pieces.retain(|p| !p.trim().is_empty());
    pieces
}

/// One line in pieces of at most `max` code units, cut at spaces where
/// possible, else between characters (a character wider than `max`, an
/// emoji under a limit of 1, is a piece of its own).
fn words(line: &str, max: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut length = 0;
    for word in line.split(' ') {
        let n = units(word);
        if n > max {
            if length > 0 {
                pieces.push(std::mem::take(&mut current));
                length = 0;
            }
            let mut chunk = String::new();
            let mut chunk_length = 0;
            for c in word.chars() {
                let width = c.len_utf16();
                if chunk_length > 0 && chunk_length + width > max {
                    pieces.push(std::mem::take(&mut chunk));
                    chunk_length = 0;
                }
                chunk.push(c);
                chunk_length += width;
            }
            if chunk_length > 0 {
                pieces.push(chunk);
            }
            continue;
        }
        if length > 0 && length + 1 + n > max {
            pieces.push(std::mem::take(&mut current));
            length = 0;
        }
        if length > 0 {
            current.push(' ');
            length += 1;
        }
        current.push_str(word);
        length += n;
    }
    if length > 0 {
        pieces.push(current);
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutting_always_makes_progress_and_respects_the_limit() {
        let text = "word ".repeat(50) + &"x".repeat(37) + "\n\n" + &"y ".repeat(9);
        for max in 1..40 {
            let parts = split(&text, max);
            assert!(!parts.is_empty(), "{max}");
            for part in &parts {
                assert!(units(part) <= max, "{max}: {part:?}");
                assert!(!part.trim().is_empty(), "{max}");
            }
        }
    }

    #[test]
    fn a_zero_limit_is_one_character() {
        assert_eq!(split("abc", 0), ["a", "b", "c"]);
        assert_eq!(Renderer::new().max_chars(0).render("abc"), ["a", "b", "c"]);
    }

    #[test]
    fn a_cut_never_yields_a_blank_part() {
        let parts = split("aaaaaaaaaa\n \nbbbbbbbbbb", 10);
        assert_eq!(parts, ["aaaaaaaaaa", "bbbbbbbbbb"]);
        let parts = Renderer::new()
            .max_chars(16)
            .render("```\naaaaaaaaaa\n \nbbbbbbbbbb\n```");
        assert_eq!(parts, ["```aaaaaaaaaa```", "```bbbbbbbbbb```"]);
    }

    #[test]
    fn an_oversized_code_block_keeps_fences_on_every_piece() {
        let code = (0..30).map(|i| format!("line {i:02}")).collect::<Vec<_>>();
        let markdown = format!("```\n{}\n```", code.join("\n"));
        let parts = Renderer::new().max_chars(40).render(&markdown);
        assert!(parts.len() > 1);
        let mut seen = Vec::new();
        for part in &parts {
            assert!(units(part) <= 40, "{part:?}");
            let inner = part
                .strip_prefix("```")
                .and_then(|p| p.strip_suffix("```"))
                .unwrap_or_else(|| panic!("not fenced: {part:?}"));
            seen.extend(inner.lines().map(str::to_owned));
        }
        assert_eq!(seen, code);
    }

    #[test]
    fn a_limit_below_the_fences_still_cuts() {
        let parts = Renderer::new().max_chars(5).render("```\nabcdefghij\n```");
        assert!(parts.iter().all(|p| units(p) <= 5), "{parts:?}");
        assert_eq!(parts.concat(), "```abcdefghij```");
        // One unit of room inside the fences cannot hold an emoji: cut as
        // text rather than a fenced piece over the limit.
        let parts = Renderer::new().max_chars(7).render("```\n😀😀\n```");
        assert!(parts.iter().all(|p| units(p) <= 7), "{parts:?}");
        assert_eq!(parts.concat(), "```😀😀```");
    }
}
