//! The Markdown renderer over a table of inputs and outputs, and the split
//! into messages at Meta's 4096-character text limit.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use meta_whatsapp_bot::markdown::{self, NoEscape, Renderer, TEXT_MAX_CHARS, WordJoinerEscape};
use meta_whatsapp_bot::{Bot, Command, Ctx};
use meta_whatsapp_client::messages::OutboundMessage;
use meta_whatsapp_core::recipient::Recipient;
use pretty_assertions::assert_eq;
use serde_json::json;

use common::{Recording, message_id, text_event};

/// U+2060 WORD JOINER, the default escape.
const J: &str = "\u{2060}";

fn one(markdown: &str) -> String {
    let parts = markdown::render(markdown);
    assert_eq!(parts.len(), 1, "{markdown:?} → {parts:?}");
    parts.into_iter().next().unwrap()
}

#[test]
fn markdown_becomes_whatsapp_formatting() {
    let table: &[(&str, String)] = &[
        // Emphasis.
        ("**bold**", "*bold*".into()),
        ("__bold__", "*bold*".into()),
        ("*italic* and _italic_", "_italic_ and _italic_".into()),
        ("~~strike~~", "~strike~".into()),
        ("***both***", "_*both*_".into()),
        ("**bold _and italic_**", "*bold _and italic_*".into()),
        ("`let x = 1;`", "`let x = 1;`".into()),
        // Headings: a bold line; bold inside adds no second marker.
        ("# Title", "*Title*".into()),
        ("## Hello **world**", "*Hello world*".into()),
        ("Setext\n======", "*Setext*".into()),
        // Code blocks: fenced, language dropped, content verbatim.
        (
            "```rust\nfn main() { a*b }\n```",
            "```fn main() { a*b }```".into(),
        ),
        ("    indented_code()", "```indented_code()```".into()),
        ("```\nline 1\nline 2\n```", "```line 1\nline 2```".into()),
        // Quotes.
        ("> quoted\n> still", "> quoted\n> still".into()),
        ("> one\n>\n> two", "> one\n> two".into()),
        // Lists.
        ("- a\n- b\n* c", "• a\n• b\n\n• c".into()),
        ("1. one\n2. two", "1. one\n2. two".into()),
        ("7. seven\n8. eight", "7. seven\n8. eight".into()),
        ("- a\n  - nested\n- b", "• a\n  • nested\n• b".into()),
        ("1. a\n\n   more\n2. b", "1. a\n   more\n2. b".into()),
        // Links and images.
        (
            "[Meta](https://www.meta.com)",
            "Meta (https://www.meta.com)".into(),
        ),
        (
            "<https://example.com/a_b>",
            "https://example.com/a_b".into(),
        ),
        ("[https://x.io](https://x.io)", "https://x.io".into()),
        ("<me@example.com>", "me@example.com".into()),
        (
            "[write](mailto:me@example.com)",
            "write (mailto:me@example.com)".into(),
        ),
        (
            "![logo](https://x.io/l.png)",
            "logo (https://x.io/l.png)".into(),
        ),
        ("![](https://x.io/l.png)", "https://x.io/l.png".into()),
        (
            "[**bold** link](https://x.io)",
            "*bold* link (https://x.io)".into(),
        ),
        // Breaks and rules.
        ("line one\nline two", "line one\nline two".into()),
        ("hard  \nbreak", "hard\nbreak".into()),
        ("a\n\nb", "a\n\nb".into()),
        ("above\n\n---\n\nbelow", "above\n\n———\n\nbelow".into()),
        // Raw HTML is text.
        ("<b>hi</b> there", "<b>hi</b> there".into()),
        // Literal text stays as written: the default escape is `NoEscape`
        // (`WordJoinerEscape` is the opt-in, tested below).
        ("2 \\* 3 \\* 4", "2 * 3 * 4".into()),
        ("snake_case_name", "snake_case_name".into()),
        ("\\~tilde\\~ and \\`tick\\`", "~tilde~ and `tick`".into()),
        ("a > b", "a > b".into()),
        (
            "see https://example.com/a_b_c?q=1~2 now",
            "see https://example.com/a_b_c?q=1~2 now".into(),
        ),
        ("`a_b*c`", "`a_b*c`".into()),
    ];
    for (markdown, expected) in table {
        assert_eq!(&one(markdown), expected, "input: {markdown:?}");
    }
}

/// `WordJoinerEscape`, opted into: literal markup gets U+2060 on each side
/// so it cannot format; URLs and code stay usable.
#[test]
fn word_joiner_escape_keeps_literal_markup_from_formatting() {
    let renderer = Renderer::new().escape(WordJoinerEscape);
    let table: &[(&str, String)] = &[
        ("2 \\* 3 \\* 4", format!("2 {J}*{J} 3 {J}*{J} 4")),
        ("snake_case_name", format!("snake{J}_{J}case{J}_{J}name")),
        (
            "\\~tilde\\~ and \\`tick\\`",
            format!("{J}~{J}tilde{J}~{J} and {J}`{J}tick{J}`{J}"),
        ),
        ("\\> not a quote", format!("{J}> not a quote")),
        ("a > b", "a > b".into()),
        (
            "see https://example.com/a_b_c?q=1~2 now",
            "see https://example.com/a_b_c?q=1~2 now".into(),
        ),
        ("`a_b*c`", "`a_b*c`".into()),
        ("**bold** and *it*", "*bold* and _it_".into()),
    ];
    for (markdown, expected) in table {
        assert_eq!(
            &renderer.render(markdown),
            std::slice::from_ref(expected),
            "input: {markdown:?}"
        );
    }
}

/// Decisive for the default: what a reader copies out of a reply is what
/// the Markdown said. An invisible character inside an email address, a
/// code, a command or a bare link rides along on copy and cuts WhatsApp's
/// link detection short; a command copied back must still run.
#[tokio::test]
async fn the_default_escape_leaves_copyable_text_as_written() {
    let text = "Mail john_doe@example.com, use code SAVE_20*, \
                open www.example.com/my_page or send /add_item";
    let rendered = markdown::render(text);
    assert_eq!(rendered, [text]);
    assert!(!rendered[0].contains('\u{2060}'));

    let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runs_in = std::sync::Arc::clone(&runs);
    let bot = Bot::builder()
        .outbound(Recording::default())
        .command(Command::new("add_item", move |_ctx: Ctx| {
            let runs = std::sync::Arc::clone(&runs_in);
            async move {
                runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        }))
        .build()
        .await
        .unwrap();
    let copied = rendered[0].split_whitespace().last().unwrap();
    bot.handle(text_event("messages/text.json", copied))
        .await
        .unwrap();
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Only `http`, `https`, `mailto` and `tel` URLs (and relative ones) are
/// written into a message: a `javascript:`, `data:` or `file:` link keeps
/// its text and loses its target, so a data-URI image in an LLM's answer
/// is its alt text, not pages of base64.
#[test]
fn only_web_mail_and_phone_urls_are_written() {
    let data = format!("data:image/png;base64,{}", "iVBORw0KGgo".repeat(500));
    let table: &[(String, &[&str])] = &[
        ("[click](javascript:alert(1))".into(), &["click"]),
        ("[click](JavaScript:alert(1))".into(), &["click"]),
        ("[click](<\tjavascript:alert(1)>)".into(), &["click"]),
        ("[click](< javascript:alert(1)>)".into(), &["click"]),
        ("[x](vbscript:msgbox)".into(), &["x"]),
        ("[notes](file:///etc/passwd)".into(), &["notes"]),
        (format!("![chart]({data})"), &["chart"]),
        (format!("![]({data})"), &[]),
        ("<javascript:alert(1)>".into(), &[]),
        (
            "[javascript:alert(1)](javascript:alert(1)) and more".into(),
            &["and more"],
        ),
        (
            "[call](tel:+16505551234)".into(),
            &["call (tel:+16505551234)"],
        ),
        ("[site](http://x.io)".into(), &["site (http://x.io)"]),
        ("[site](HTTPS://x.io)".into(), &["site (HTTPS://x.io)"]),
        ("[docs](/help/start)".into(), &["docs (/help/start)"]),
    ];
    for (markdown, expected) in table {
        assert_eq!(&markdown::render(markdown), expected, "input: {markdown:?}");
    }
}

/// Render `markdown` on a thread with tokio's default worker stack (2 MiB).
fn render_on_a_worker_stack(markdown: String) -> Vec<String> {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || markdown::render(&markdown))
        .unwrap()
        .join()
        .unwrap()
}

/// Decisive: one WhatsApp message's worth of nesting (4096 characters of
/// `- ` or `>`, echoed or quoted by an LLM) renders on a worker's stack
/// instead of overflowing it, which aborts the whole process. Quotes and
/// lists deeper than the renderer keeps are flattened into the deepest
/// kept one, their text intact.
#[test]
fn deep_nesting_is_flattened_not_a_stack_overflow() {
    let lists = render_on_a_worker_stack("- ".repeat(2047) + "x");
    assert_eq!(lists.len(), 1);
    assert!(
        lists[0].ends_with("• x"),
        "{:?}",
        &lists[0][lists[0].len() - 40..]
    );
    let quotes = render_on_a_worker_stack(">".repeat(4095) + "x");
    assert_eq!(quotes.len(), 1);
    assert!(quotes[0].ends_with('x'));
    let long = render_on_a_worker_stack(">".repeat(50_000) + "x\n\n" + &"- ".repeat(25_000) + "y");
    assert!(long.concat().contains('x'));
    // The quotes closed before the lists opened: the lists nest again.
    assert!(long.concat().ends_with("• y"), "{:?}", long.last());
    // Shallow nesting keeps its structure.
    assert_eq!(
        markdown::render("> a\n>> b\n\n- one\n  - two\n    - three"),
        ["> a\n> > b\n\n• one\n  • two\n    • three"]
    );
}

#[test]
fn tables_become_a_padded_monospace_block() {
    let markdown = "| Item | Qty |\n| --- | ---: |\n| Aloe *vera* | 3 |\n| Pot_S | 12 |";
    assert_eq!(
        one(markdown),
        "```Item      | Qty\n----------|----\nAloe vera | 3\nPot_S     | 12```"
    );
}

/// A table cell wider than `format!` can pad (65 535) is padded all the
/// same, never a panic (an LLM's or a user's table reaches the renderer
/// as is).
#[test]
fn a_table_cell_wider_than_formatting_allows_is_padded() {
    let wide = "a".repeat(70_000);
    let markdown = format!("| {wide} | b |\n| --- | --- |\n| c | d |");
    let parts = markdown::render(&markdown);
    assert!(parts.len() > 30, "{}", parts.len());
    for part in &parts {
        assert!(part.encode_utf16().count() <= TEXT_MAX_CHARS);
    }
    let unsplit = Renderer::new().render_unsplit(&markdown);
    let lines: Vec<&str> = unsplit.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2], format!("c{} | d```", " ".repeat(69_999)));
}

#[test]
fn nothing_renders_to_no_message() {
    assert!(markdown::render("").is_empty());
    assert!(markdown::render("  \n\n  ").is_empty());
    assert!(markdown::render("```\n```").is_empty());
}

#[test]
fn escaping_is_swappable() {
    let plain = Renderer::new().escape(NoEscape);
    assert_eq!(
        plain.render("2 \\* 3 and snake_case"),
        ["2 * 3 and snake_case"]
    );
    assert_eq!(plain.render_unsplit("**b**\n\n- x"), "*b*\n\n• x");
    // `NoEscape` is the default; `WordJoinerEscape` swaps in.
    assert_eq!(
        markdown::render("2 \\* 3 and snake_case"),
        plain.render("2 \\* 3 and snake_case")
    );
    assert_eq!(
        Renderer::new()
            .escape(WordJoinerEscape)
            .render("snake_case"),
        [format!("snake{J}_{J}case")]
    );
}

/// `TEXT_MAX_CHARS` is the client's own limit: a text body of exactly
/// that many characters passes `OutboundMessage::validate`, one more fails;
/// and no renderer makes longer parts.
#[test]
fn the_limit_is_the_clients_text_limit() {
    let to = || Recipient::phone("+16505551234");
    assert_eq!(
        TEXT_MAX_CHARS,
        meta_whatsapp_client::messages::TEXT_BODY_MAX_CHARS
    );
    assert_eq!(TEXT_MAX_CHARS, 4096);
    // `max_chars` above the limit is clamped to it.
    let text = [paragraph(3000, 'a'), paragraph(3000, 'b')].join("\n\n");
    let parts = Renderer::new().max_chars(10_000).render(&text);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts, markdown::render(&text));
    assert!(
        OutboundMessage::text(to(), "é".repeat(TEXT_MAX_CHARS))
            .validate()
            .is_ok()
    );
    let err = OutboundMessage::text(to(), "é".repeat(TEXT_MAX_CHARS + 1))
        .validate()
        .unwrap_err();
    assert_eq!(err.field, "text.body");
}

fn paragraph(n: usize, letter: char) -> String {
    // Words of 9 letters and a space: no markup, so nothing is escaped.
    let mut text: String = std::iter::repeat_n(letter.to_string().repeat(9), n / 10)
        .collect::<Vec<_>>()
        .join(" ");
    while text.chars().count() < n {
        text.push(letter);
    }
    text
}

/// Decisive: 5000 characters of Markdown split into exactly two messages,
/// at a paragraph boundary, each within the limit.
#[test]
fn five_thousand_characters_split_into_two_messages_at_a_paragraph() {
    let paragraphs = [
        paragraph(1000, 'a'),
        paragraph(1000, 'b'),
        paragraph(1000, 'c'),
        paragraph(1000, 'd'),
        paragraph(992, 'e'),
    ];
    let markdown = paragraphs.join("\n\n");
    assert_eq!(markdown.chars().count(), 5000);

    let parts = markdown::render(&markdown);

    assert_eq!(
        parts.len(),
        2,
        "{:?}",
        parts.iter().map(String::len).collect::<Vec<_>>()
    );
    assert_eq!(parts[0], paragraphs[..4].join("\n\n"));
    assert_eq!(parts[1], paragraphs[4]);
    for part in &parts {
        assert!(part.chars().count() <= TEXT_MAX_CHARS);
        OutboundMessage::text(Recipient::phone("+16505551234"), part.clone())
            .validate()
            .unwrap();
    }
}

/// Two blocks that fill a message exactly, separator included, share it.
#[test]
fn blocks_that_fill_a_message_exactly_share_it() {
    let markdown = [paragraph(2047, 'a'), paragraph(2047, 'b')].join("\n\n");
    assert_eq!(markdown.chars().count(), TEXT_MAX_CHARS);
    assert_eq!(markdown::render(&markdown), std::slice::from_ref(&markdown));
    let one_more = format!("{markdown}b");
    assert_eq!(markdown::render(&one_more).len(), 2);
}

/// Parts are measured in UTF-16 code units, never bytes: 4096 two-byte
/// letters (one unit each) are one message, but emoji outside the Basic
/// Multilingual Plane count twice, so emoji-dense text makes parts that
/// fit whether Meta counts characters or UTF-16 units.
#[test]
fn the_limit_counts_utf16_units_not_bytes() {
    let text = "é".repeat(TEXT_MAX_CHARS);
    assert_eq!(markdown::render(&text), std::slice::from_ref(&text));

    let emoji = "😀".repeat(TEXT_MAX_CHARS);
    let parts = markdown::render(&emoji);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts.concat(), emoji);
    // Emoji-dense prose, with words: every part within the limit either way.
    let prose = "Great 🎉🎉 news 😀😀😀 for 👍 you ".repeat(400);
    let parts = markdown::render(&prose);
    assert!(parts.len() > 1);
    for part in &parts {
        assert!(
            part.encode_utf16().count() <= TEXT_MAX_CHARS,
            "{}",
            part.len()
        );
        assert!(part.chars().count() <= TEXT_MAX_CHARS);
        OutboundMessage::text(Recipient::phone("+16505551234"), part.clone())
            .validate()
            .unwrap();
    }
    // A limit of 1 still makes progress over a two-unit emoji.
    assert_eq!(
        Renderer::new().max_chars(1).render("a😀b"),
        ["a", "😀", "b"]
    );
}

/// WhatsApp formats no part of a word: emphasis inside one loses its
/// markers instead of showing them.
#[test]
fn emphasis_inside_a_word_drops_its_markers() {
    let table: &[(&str, &str)] = &[
        ("foo**bar**baz", "foobarbaz"),
        ("foo*bar*baz", "foobarbaz"),
        ("un~~real~~ly", "unreally"),
        ("**Bold**ly", "Boldly"),
        ("pre**fix**", "prefix"),
        ("a***b***c", "abc"),
        // Both markers of a nested span go, whichever side the word is on
        // (the outer span's markers are skipped when looking for it).
        ("a***b*** c", "ab c"),
        ("a ***b***c", "a bc"),
        ("déjà**vu**", "déjàvu"),
        // At a word's edge, the markers stay.
        (
            "**bold**, (*it*) and ~~gone~~.",
            "*bold*, (_it_) and ~gone~.",
        ),
        ("**a** b **c**", "*a* b *c*"),
        ("***both***", "_*both*_"),
        ("# Title **x**y", "*Title xy*"),
        ("[**in** link](https://x.io)", "*in* link (https://x.io)"),
    ];
    for (markdown, expected) in table {
        assert_eq!(&one(markdown), expected, "input: {markdown:?}");
    }
}

/// Code holding a backtick cannot be fenced without breaking the fence:
/// its text goes out plain, never a broken span.
#[test]
fn code_holding_backticks_goes_out_plain() {
    let table: &[(&str, &str)] = &[
        ("``a ` b``", "a ` b"),
        ("say `` `hi` `` now", "say `hi` now"),
        ("```\nlet s = \"```\";\n```", "let s = \"```\";"),
        ("```\n`tick\n```", "`tick"),
        ("```\ntock`\n```", "tock`"),
        // Without a backtick: fenced as usual.
        ("```\nplain\n```", "```plain```"),
        ("`plain`", "`plain`"),
    ];
    for (markdown, expected) in table {
        assert_eq!(&one(markdown), expected, "input: {markdown:?}");
    }
    // In a list item too.
    assert_eq!(one("- item\n\n  ```\n  a```b\n  ```"), "• item\n  a```b");
}

/// A code block too long for one message is cut between lines and each
/// piece fenced again; a piece that starts or ends with a backtick (a line
/// of the code does) could not close its fence either, so it goes out
/// plain, like such a block that fits.
#[test]
fn a_cut_piece_with_a_backtick_at_an_end_goes_out_plain() {
    let markdown = "```\naaaaaaaaaa`\n`bbbbbbbbbb\ncccccccccc\n```";
    assert_eq!(
        Renderer::new().max_chars(20).render(markdown),
        ["aaaaaaaaaa`", "`bbbbbbbbbb", "```cccccccccc```"]
    );

    // At the real limit: shell lines quoting a command, in a block longer
    // than a message. Every part is a whole fence or holds no fence.
    let lines: Vec<String> = (0..400).map(|i| format!("`step {i:03}` done")).collect();
    let markdown = format!("```sh\nstart\n{}\nend\n```", lines.join("\n"));
    let parts = markdown::render(&markdown);
    assert!(parts.len() > 1);
    for part in &parts {
        match part.strip_prefix("```").and_then(|p| p.strip_suffix("```")) {
            Some(inner) => assert!(
                !inner.starts_with('`') && !inner.ends_with('`') && !inner.contains("```"),
                "a broken fence: {part:?}"
            ),
            None => assert!(!part.contains("```"), "{part:?}"),
        }
    }
    let seen: Vec<&str> = parts
        .iter()
        .flat_map(|p| p.trim_start_matches("```").trim_end_matches("```").lines())
        .collect();
    assert_eq!(seen.len(), 402);
    assert_eq!((seen[0], seen[401]), ("start", "end"));
}

/// The escape's word joiners are characters Meta counts too: a text
/// dense with escaped markup still splits into parts the client accepts.
#[test]
fn escapes_count_toward_the_limit() {
    let renderer = Renderer::new().escape(WordJoinerEscape);
    let markdown = "a\\_b ".repeat(1500);
    let parts = renderer.render(&markdown);
    assert!(parts.len() > 1, "{}", parts.len());
    for part in &parts {
        assert!(part.contains('\u{2060}'));
        assert!(
            part.chars().count() <= TEXT_MAX_CHARS,
            "{}",
            part.chars().count()
        );
        OutboundMessage::text(Recipient::phone("+16505551234"), part.clone())
            .validate()
            .unwrap();
    }
}

/// `WordJoinerEscape` leaves `http://` and `https://` URLs as written.
#[test]
fn word_joiner_escape_leaves_urls_alone() {
    let renderer = Renderer::new().escape(WordJoinerEscape);
    assert_eq!(
        renderer.render("see http://example.com/a_b~c and HTTPS://x.io/*"),
        ["see http://example.com/a_b~c and HTTPS://x.io/*"]
    );
}

/// A code block that fits in a message moves to the next one whole
/// instead of being cut where the limit falls.
#[test]
fn a_code_block_that_fits_is_never_cut() {
    let intro = paragraph(3900, 'x');
    let code: Vec<String> = (0..40).map(|i| format!("step_{i:02}();")).collect();
    let markdown = format!("{intro}\n\n```\n{}\n```\n\nThe end.", code.join("\n"));
    let parts = markdown::render(&markdown);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], intro);
    assert_eq!(parts[1], format!("```{}```\n\nThe end.", code.join("\n")));
}

/// A list's items stay one per line within a message; a list longer than
/// a message is cut between items.
#[test]
fn a_long_list_is_cut_between_items() {
    let items: Vec<String> = (0..600).map(|i| format!("item number {i:03}")).collect();
    let markdown = items
        .iter()
        .map(|i| format!("- {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let parts = markdown::render(&markdown);
    assert!(parts.len() > 1);
    let lines: Vec<&str> = parts.iter().flat_map(|p| p.lines()).collect();
    assert_eq!(lines.len(), 600);
    for (line, item) in lines.iter().zip(&items) {
        assert_eq!(*line, format!("• {item}"));
    }
    assert!(parts.iter().all(|p| p.chars().count() <= TEXT_MAX_CHARS));
}

/// `reply_markdown` sends the parts in order; the first quotes the message.
#[tokio::test]
async fn reply_markdown_sends_every_part_in_order() {
    let out = Recording::default();
    let long = [paragraph(3000, 'a'), paragraph(3000, 'b')].join("\n\n");
    let bot = Bot::builder()
        .outbound(out.clone())
        .command(Command::new("essay", move |ctx: Ctx| {
            let long = long.clone();
            async move {
                assert_eq!(
                    ctx.reply_markdown(&format!("# Essay\n\n{long}"))
                        .await?
                        .len(),
                    2
                );
                Ok(())
            }
        }))
        .build()
        .await
        .unwrap();
    bot.handle(text_event("messages/text.json", "/essay"))
        .await
        .unwrap();
    let sent = out.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0],
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505551234",
            "context": {"message_id": message_id("messages/text.json")},
            "type": "text",
            "text": {"body": format!("*Essay*\n\n{}", paragraph(3000, 'a'))}
        })
    );
    assert_eq!(
        sent[1],
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505551234",
            "type": "text",
            "text": {"body": paragraph(3000, 'b')}
        })
    );
}

// ─── Fuzzed split ────────────────────────────────────────────────────────

/// xorshift64: a fixed seed, so a failure replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

/// Words with astral emoji (skin tones, ZWJ families, flags), CJK, accents,
/// Markdown and WhatsApp markup, backticks, links, HTML and invisible
/// joiners.
const ATOMS: &[&str] = &[
    "word",
    "déjà",
    "😀",
    "👍🏽",
    "👨‍👩‍👧",
    "🇫🇷",
    "𝔘𝔫𝔦",
    "漢字",
    "テスト",
    "한국어",
    "a*b",
    "snake_case",
    "~x~",
    "`",
    "``",
    "```",
    ">",
    "**",
    "_",
    "~~",
    "http://x.io/a_b",
    "[link](https://x.io/😀)",
    "[bad](javascript:alert(1))",
    "<https://auto.link>",
    "`code`",
    "``a ` b``",
    "**bold**",
    "*it*",
    "~~s~~",
    "foo**bar**baz",
    "***both***",
    "\\*",
    "<b>html</b>",
    "|",
    "\u{2060}",
    "\u{200d}",
    "\u{fe0f}",
    "é\u{301}",
];

fn fuzz_word(rng: &mut Rng) -> String {
    let atom = rng.pick(ATOMS);
    if rng.chance(1) {
        // One word longer than a message: cut between characters.
        atom.repeat(1 + rng.below(1200))
    } else if rng.chance(20) {
        let other = rng.pick(ATOMS);
        format!("{atom}{other}")
    } else {
        atom.to_owned()
    }
}

fn fuzz_line(rng: &mut Rng, words: usize) -> String {
    (0..=rng.below(words))
        .map(|_| fuzz_word(rng))
        .collect::<Vec<_>>()
        .join(" ")
}

fn fuzz_block(rng: &mut Rng) -> String {
    match rng.below(10) {
        0 => format!("{} {}", "#".repeat(1 + rng.below(6)), fuzz_line(rng, 8)),
        1 => {
            // A list, nested up to past the nesting cap.
            let mut depth = 0;
            (0..=rng.below(30))
                .map(|i| {
                    depth = (depth + rng.below(3)).saturating_sub(1).min(20);
                    let marker = if rng.chance(50) {
                        "- ".to_owned()
                    } else {
                        format!("{}. ", i + 1)
                    };
                    format!("{}{marker}{}", "   ".repeat(depth), fuzz_line(rng, 12))
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        2 => format!("{}{}", "> ".repeat(1 + rng.below(20)), fuzz_line(rng, 30)),
        3 => {
            // A fenced code block, some lines starting or ending with a
            // backtick, some holding a fence.
            let most = if rng.chance(5) { 150 } else { 12 };
            let lines = (0..=rng.below(most))
                .map(|_| {
                    let line = fuzz_line(rng, 10);
                    match rng.below(8) {
                        0 => format!("`{line}"),
                        1 => format!("{line}`"),
                        2 => format!("x ``` {line}"),
                        _ => line,
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("```{}\n{lines}\n```", rng.pick(&["", "rust", "sh"]))
        }
        4 => format!("    {}\n    {}", fuzz_line(rng, 10), fuzz_line(rng, 10)),
        5 => {
            let columns = 1 + rng.below(4);
            let row = |rng: &mut Rng| {
                let cells: Vec<String> = (0..columns).map(|_| fuzz_line(rng, 3)).collect();
                format!("| {} |", cells.join(" | "))
            };
            let mut table = vec![row(rng), format!("|{}", "---|".repeat(columns))];
            for _ in 0..rng.below(10) {
                table.push(row(rng));
            }
            table.join("\n")
        }
        6 => "---".to_owned(),
        _ => {
            // A paragraph, with line breaks, sometimes long.
            let words = if rng.chance(5) { 300 } else { 40 };
            (0..=rng.below(3))
                .map(|_| fuzz_line(rng, words))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

fn fuzz_document(rng: &mut Rng) -> String {
    (0..=rng.below(8))
        .map(|_| fuzz_block(rng))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Without whitespace or backticks: what a part may add (fences on a cut
/// code block) or drop (the whitespace at a cut) is left out.
fn content(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && *c != '`')
        .collect()
}

/// Decisive for the split: 3000 random Markdown documents (fixed seed)
/// with emoji, CJK, nesting, tables and code, many longer than a message.
/// At the default limit every part is at most 4096 UTF-16 code units, not
/// blank, and accepted by the client's own `OutboundMessage::validate`;
/// under a random smaller limit (2 to 400) every part fits it too; and the
/// parts hold what one unsplit message would, in order.
#[test]
fn fuzzed_markdown_always_splits_into_parts_the_client_accepts() {
    let to = Recipient::phone("+16505551234");
    let mut rng = Rng(0x05ee_d202_6092_6b07);
    let mut split_documents = 0;
    for case in 0..3000 {
        let markdown = fuzz_document(&mut rng);
        let parts = markdown::render(&markdown);
        split_documents += usize::from(parts.len() > 1);
        for part in &parts {
            let units = part.encode_utf16().count();
            assert!(units <= TEXT_MAX_CHARS, "case {case}: {units} units");
            assert!(!part.trim().is_empty(), "case {case}: a blank part");
            if let Err(e) = OutboundMessage::text(to.clone(), part.clone()).validate() {
                panic!("case {case}: {e}");
            }
        }
        let unsplit = Renderer::new().render_unsplit(&markdown);
        assert_eq!(
            content(&parts.concat()),
            content(&unsplit),
            "case {case}: content lost or moved"
        );

        let max = 2 + rng.below(399);
        let parts = Renderer::new().max_chars(max).render(&markdown);
        for part in &parts {
            let units = part.encode_utf16().count();
            assert!(units <= max, "case {case}: {units} units > {max}");
            assert!(!part.trim().is_empty(), "case {case}: a blank part");
        }
        assert_eq!(
            content(&parts.concat()),
            content(&unsplit),
            "case {case} (max {max}): content lost or moved"
        );
    }
    // The generator does exercise the split.
    assert!(split_documents > 100, "{split_documents}");
}
