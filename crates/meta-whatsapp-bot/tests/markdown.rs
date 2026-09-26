//! The Markdown renderer over a table of inputs and outputs, and the split
//! into messages at Meta's 4096-character text limit.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use meta_whatsapp_bot::markdown::{self, NoEscape, Renderer, TEXT_MAX_CHARS};
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
        // Escaping: literal markup cannot format; URLs and code stay usable.
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
    ];
    for (markdown, expected) in table {
        assert_eq!(&one(markdown), expected, "input: {markdown:?}");
    }
}

#[test]
fn tables_become_a_padded_monospace_block() {
    let markdown = "| Item | Qty |\n| --- | ---: |\n| Aloe *vera* | 3 |\n| Pot_S | 12 |";
    assert_eq!(
        one(markdown),
        "```Item      | Qty\n----------|----\nAloe vera | 3\nPot_S     | 12```"
    );
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
}

/// `TEXT_MAX_CHARS` is the client's own limit: a text body of exactly
/// that many characters passes `OutboundMessage::validate`, one more fails.
#[test]
fn the_limit_is_the_clients_text_limit() {
    let to = || Recipient::phone("+16505551234");
    assert_eq!(TEXT_MAX_CHARS, 4096);
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
