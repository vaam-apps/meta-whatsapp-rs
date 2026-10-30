//! Rich replies (roadmap B1c) over Meta's documented webhook fixtures:
//! each part sent as the message Meta documents for it, in order, with
//! the exact requests; suggestions as buttons or a list by their count;
//! the image URL rule; what is refused before anything is sent; and a
//! failure in the middle.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};

use meta_whatsapp_bot::markdown::Renderer;
use meta_whatsapp_bot::rich::{
    LIST_BUTTON, RichReply, STEP_BUTTONS, STEP_IMAGE, STEP_LIST, STEP_PRODUCTS, STEP_TEXT,
    Suggestion,
};
use meta_whatsapp_bot::{Bot, ClientOutbound, Command, Ctx, PropagateErrors};
use meta_whatsapp_client::messages::{OutboundMessage, ProductCard};
use meta_whatsapp_core::Error;
use meta_whatsapp_core::error::{TransportError, ValidationError};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::testing::ScriptedTransport;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use common::{NUMBER, Recording, client, event, message_id, send_response, text_event};

const TEXT: &str = "messages/text.json";
const BUTTON_REPLY: &str = "messages/interactive_button_reply.json";
const IMAGE_URL: &str = "https://www.luckyshrub.com/assets/succulents/aloe.png";
const CATALOG: &str = "123456789";

/// A context for `messages/text.json`, sending through `t`.
fn ctx(t: &ScriptedTransport) -> Ctx {
    Ctx::new(
        event(TEXT),
        Arc::new(ClientOutbound::new(client(t))),
        Arc::new(Renderer::default()),
    )
}

/// Script `n` successful sends (Meta's example response).
fn script(t: &ScriptedTransport, n: usize) {
    for _ in 0..n {
        t.push_json(200, send_response());
    }
}

/// Every request's JSON body, in order, each a `POST` to the number's
/// `messages` edge with the client's token.
fn bodies(t: &ScriptedTransport) -> Vec<Value> {
    t.requests()
        .iter()
        .map(|r| {
            assert_eq!(r.method.as_str(), "POST");
            assert_eq!(r.path(), format!("/v25.0/{NUMBER}/messages"));
            assert_eq!(r.bearer(), Some("TOKEN"));
            r.json().unwrap()
        })
        .collect()
}

/// A message to the sender of `messages/text.json`; `quote` adds the
/// context of the received message.
fn to_sender(quote: bool, kind: &str, content: Value) -> Value {
    let mut message = json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": "+16505551234",
        "type": kind,
    });
    message[kind] = content;
    if quote {
        message["context"] = json!({"message_id": message_id(TEXT)});
    }
    message
}

fn text(quote: bool, body: &str) -> Value {
    to_sender(quote, "text", json!({"body": body}))
}

fn image(quote: bool, link: &str) -> Value {
    to_sender(quote, "image", json!({"link": link}))
}

fn buttons(quote: bool, body: &str, buttons: &[(&str, &str)]) -> Value {
    let buttons: Vec<Value> = buttons
        .iter()
        .map(|(id, title)| json!({"type": "reply", "reply": {"id": id, "title": title}}))
        .collect();
    to_sender(
        quote,
        "interactive",
        json!({"type": "button", "body": {"text": body}, "action": {"buttons": buttons}}),
    )
}

fn list(quote: bool, body: &str, label: &str, rows: &[Value]) -> Value {
    to_sender(
        quote,
        "interactive",
        json!({
            "type": "list",
            "body": {"text": body},
            "action": {"button": label, "sections": [{"title": label, "rows": rows}]}
        }),
    )
}

/// Suggestions `s0`… labelled `Option 0`….
fn numbered(n: usize) -> Vec<Suggestion> {
    (0..n)
        .map(|i| Suggestion::new(format!("s{i}"), format!("Option {i}")))
        .collect()
}

fn numbered_rows(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| json!({"id": format!("s{i}"), "title": format!("Option {i}")}))
        .collect()
}

/// A reply starting with a part that would be sent on its own (an image),
/// so a refusal of what follows proves nothing went out before it.
fn lead() -> RichReply {
    RichReply::new().image(IMAGE_URL)
}

fn validation(error: &Error) -> &ValidationError {
    match error {
        Error::Validation(v) => v,
        other => panic!("not a validation error: {other:?}"),
    }
}

// ─── Order and the exact requests ────────────────────────────────────────

/// Decisive: a paragraph and an image send a text message and then an
/// image message, in that order, the text quoting the received message.
/// Reverse the parts and the first assertion fails.
#[tokio::test]
async fn a_paragraph_and_an_image_send_a_text_then_an_image() {
    let t = ScriptedTransport::new();
    script(&t, 2);
    let reply = RichReply::new()
        .markdown("Your **aloe** is ready.")
        .image(IMAGE_URL);

    let sent = ctx(&t).reply_rich(&reply).await.unwrap();

    assert_eq!(
        bodies(&t),
        [text(true, "Your *aloe* is ready."), image(false, IMAGE_URL)]
    );
    assert_eq!(sent.len(), 2);
    assert_eq!(t.remaining(), 0);
}

/// An image first quotes the received message itself; a caption is sent
/// as written.
#[tokio::test]
async fn an_image_alone_quotes_the_message() {
    let t = ScriptedTransport::new();
    script(&t, 2);
    ctx(&t)
        .reply_rich(&RichReply::new().image(IMAGE_URL))
        .await
        .unwrap();
    ctx(&t)
        .reply_rich(&RichReply::new().image_with_caption(IMAGE_URL, "The best *succulent* ever?"))
        .await
        .unwrap();

    let mut captioned = image(true, IMAGE_URL);
    captioned["image"]["caption"] = json!("The best *succulent* ever?");
    assert_eq!(bodies(&t), [image(true, IMAGE_URL), captioned]);
    assert_eq!(t.remaining(), 0);
}

/// Every part, in the order given: text, image, text carrying buttons,
/// text carrying a product carousel.
#[tokio::test]
async fn every_part_goes_out_in_order() {
    let t = ScriptedTransport::new();
    script(&t, 5);
    let reply = RichReply::new()
        .markdown("Order 42 has shipped.")
        .image(IMAGE_URL)
        .markdown("Anything else?")
        .suggestions([Suggestion::new("track", "Track it")])
        .markdown("You may like:")
        .products([
            ProductCard::new(CATALOG, "abc123xyz"),
            ProductCard::new(CATALOG, "def456uvw"),
        ])
        .markdown("See you soon.");

    ctx(&t).reply_rich(&reply).await.unwrap();

    let carousel = to_sender(
        false,
        "interactive",
        json!({
            "type": "carousel",
            "body": {"text": "You may like:"},
            "action": {"cards": [
                {"card_index": 0, "type": "product",
                 "action": {"product_retailer_id": "abc123xyz", "catalog_id": CATALOG}},
                {"card_index": 1, "type": "product",
                 "action": {"product_retailer_id": "def456uvw", "catalog_id": CATALOG}}
            ]}
        }),
    );
    assert_eq!(
        bodies(&t),
        [
            text(true, "Order 42 has shipped."),
            image(false, IMAGE_URL),
            buttons(false, "Anything else?", &[("track", "Track it")]),
            carousel,
            text(false, "See you soon."),
        ]
    );
    assert_eq!(t.remaining(), 0);
}

/// An empty reply, or one whose Markdown renders to nothing, sends
/// nothing.
#[tokio::test]
async fn an_empty_reply_sends_nothing() {
    let t = ScriptedTransport::new();
    assert!(
        ctx(&t)
            .reply_rich(&RichReply::new())
            .await
            .unwrap()
            .is_empty()
    );
    let blank = RichReply::new().markdown("  \n\n ");
    assert!(ctx(&t).reply_rich(&blank).await.unwrap().is_empty());
    assert!(RichReply::new().is_empty());
    assert!(t.requests().is_empty());
}

// ─── Suggestions: buttons or a list ──────────────────────────────────────

/// Decisive: three suggestions become reply buttons and four a list.
/// Remove the count check (always buttons, or always a list) and one of
/// the two fails: four buttons are refused, three rows are not buttons.
#[tokio::test]
async fn three_suggestions_become_buttons_and_four_a_list() {
    let t = ScriptedTransport::new();
    script(&t, 2);

    ctx(&t)
        .reply_rich(
            &RichReply::new()
                .markdown("Pick one")
                .suggestions(numbered(3)),
        )
        .await
        .unwrap();
    ctx(&t)
        .reply_rich(
            &RichReply::new()
                .markdown("Pick one")
                .suggestions(numbered(4)),
        )
        .await
        .unwrap();

    assert_eq!(
        bodies(&t),
        [
            buttons(
                true,
                "Pick one",
                &[("s0", "Option 0"), ("s1", "Option 1"), ("s2", "Option 2")]
            ),
            list(true, "Pick one", LIST_BUTTON, &numbered_rows(4)),
        ]
    );
    assert_eq!(LIST_BUTTON, "Options");
    assert_eq!(t.remaining(), 0);
}

/// Ten suggestions are a list; eleven are refused before anything is
/// sent, the text before them included.
#[tokio::test]
async fn ten_suggestions_are_a_list_and_eleven_are_refused() {
    let t = ScriptedTransport::new();
    script(&t, 1);
    let ten = RichReply::new()
        .markdown("Pick one")
        .suggestions(numbered(10))
        .list_button("Shipping Options");
    ctx(&t).reply_rich(&ten).await.unwrap();
    assert_eq!(
        bodies(&t),
        [list(
            true,
            "Pick one",
            "Shipping Options",
            &numbered_rows(10)
        )]
    );

    let eleven = RichReply::new()
        .markdown("First this.")
        .markdown("Pick one")
        .suggestions(numbered(11));
    let error = ctx(&t).reply_rich(&eleven).await.unwrap_err();
    assert_eq!(
        validation(&error).field,
        "interactive.action.sections[].rows"
    );
    assert_eq!(t.requests().len(), 1, "nothing of the second reply is sent");
    assert_eq!(t.remaining(), 0);
}

/// A row's description goes into a list; buttons have none, so it is left
/// out of them.
#[tokio::test]
async fn descriptions_show_in_lists_only() {
    let t = ScriptedTransport::new();
    script(&t, 2);
    let with_description = |n: usize| {
        let mut s = numbered(n);
        s[0] = s[0].clone().description("Next Day to 2 Days");
        s
    };
    ctx(&t)
        .reply_rich(
            &RichReply::new()
                .markdown("Ship")
                .suggestions(with_description(2)),
        )
        .await
        .unwrap();
    ctx(&t)
        .reply_rich(
            &RichReply::new()
                .markdown("Ship")
                .suggestions(with_description(4)),
        )
        .await
        .unwrap();

    let mut rows = numbered_rows(4);
    rows[0]["description"] = json!("Next Day to 2 Days");
    assert_eq!(
        bodies(&t),
        [
            buttons(true, "Ship", &[("s0", "Option 0"), ("s1", "Option 1")]),
            list(true, "Ship", LIST_BUTTON, &rows),
        ]
    );
    assert_eq!(t.remaining(), 0);
}

/// Suggestions need the text right before them: first in the reply, after
/// an image, or after other suggestions, they are refused and nothing is
/// sent.
#[tokio::test]
async fn suggestions_without_text_before_them_are_refused() {
    let t = ScriptedTransport::new();
    for reply in [
        RichReply::new().suggestions(numbered(2)),
        RichReply::new()
            .markdown("Look")
            .image(IMAGE_URL)
            .suggestions(numbered(2)),
        RichReply::new()
            .markdown("Pick")
            .suggestions(numbered(2))
            .suggestions(numbered(2)),
        RichReply::new().markdown("   ").suggestions(numbered(2)),
    ] {
        let error = ctx(&t).reply_rich(&reply).await.unwrap_err();
        assert_eq!(validation(&error).field, "suggestions", "{reply:?}");
    }
    let error = ctx(&t)
        .reply_rich(&RichReply::new().products([
            ProductCard::new(CATALOG, "a"),
            ProductCard::new(CATALOG, "b"),
        ]))
        .await
        .unwrap_err();
    assert_eq!(validation(&error).field, "products");
    assert!(t.requests().is_empty());
}

/// Over a limit Meta documents, the client's own check refuses the reply
/// before anything is sent: a button title over 20 characters, a list row
/// title over 24, a list label over 20, no suggestion, two equal titles;
/// the image before each is not sent either. Skip the check of every
/// message before the first send and the image goes out.
#[tokio::test]
async fn over_limit_suggestions_are_refused_before_anything_is_sent() {
    let t = ScriptedTransport::new();
    let cases = [
        (
            lead()
                .markdown("Hi")
                .suggestions([Suggestion::new("a", "x".repeat(21))]),
            "interactive.action.buttons[0].reply.title",
        ),
        (
            lead().markdown("Hi").suggestions({
                let mut s = numbered(4);
                s[3] = Suggestion::new("long", "y".repeat(25));
                s
            }),
            "interactive.action.sections[0].rows[3].title",
        ),
        (
            lead()
                .markdown("Hi")
                .suggestions(numbered(4))
                .list_button("z".repeat(21)),
            "interactive.action.button",
        ),
        (
            lead().markdown("Hi").suggestions([]),
            "interactive.action.buttons",
        ),
        (
            lead()
                .markdown("Hi")
                .suggestions([Suggestion::new("a", "Same"), Suggestion::new("b", "Same")]),
            "interactive.action.buttons[1].reply.title",
        ),
    ];
    for (reply, field) in cases {
        let error = ctx(&t)
            .reply_rich(&reply.markdown("after"))
            .await
            .unwrap_err();
        assert_eq!(validation(&error).field, field);
    }
    // Exactly at the limits: sent.
    script(&t, 1);
    ctx(&t)
        .reply_rich(
            &RichReply::new()
                .markdown("Hi")
                .suggestions([Suggestion::new("a", "x".repeat(20))]),
        )
        .await
        .unwrap();
    assert_eq!(t.requests().len(), 1);
    assert_eq!(t.remaining(), 0);
}

/// A body longer than the buttons allow (1024 characters) is cut: the
/// pieces before the last go as text messages, the last carries the
/// buttons. A list takes up to 4096.
#[tokio::test]
async fn a_long_body_is_cut_and_its_last_piece_carries_the_buttons() {
    let t = ScriptedTransport::new();
    script(&t, 3);
    let first = "a".repeat(1000);
    let second = "b".repeat(1000);
    let markdown = format!("{first}\n\n{second}");

    ctx(&t)
        .reply_rich(
            &RichReply::new()
                .markdown(markdown.clone())
                .suggestions(numbered(1)),
        )
        .await
        .unwrap();
    ctx(&t)
        .reply_rich(&RichReply::new().markdown(markdown).suggestions(numbered(4)))
        .await
        .unwrap();

    let both = format!("{first}\n\n{second}");
    assert_eq!(
        bodies(&t),
        [
            text(true, &first),
            buttons(false, &second, &[("s0", "Option 0")]),
            list(true, &both, LIST_BUTTON, &numbered_rows(4)),
        ]
    );
    assert_eq!(t.remaining(), 0);
}

// ─── Images ──────────────────────────────────────────────────────────────

/// Decisive: a `javascript:` image URL is never sent, nor anything of its
/// reply; neither is a `data:`, relative, `mailto:` or `tel:` one. Remove
/// the URL check and the first request goes out.
#[tokio::test]
async fn a_javascript_image_url_is_never_sent() {
    let t = ScriptedTransport::new();
    for url in [
        "javascript:alert(1)",
        "JavaScript:alert(1)",
        "\u{1}javascript:alert(1)",
        "data:image/png;base64,iVBORw0KGgo=",
        "/assets/aloe.png",
        "mailto:pablo@luckyshrub.example",
        "tel:+16505551234",
    ] {
        let reply = RichReply::new().markdown("Look:").image(url);
        let error = ctx(&t).reply_rich(&reply).await.unwrap_err();
        let v = validation(&error);
        assert_eq!(v.field, "image.link", "{url:?}");
        assert!(!v.reason.contains(url), "the refusal repeats the URL");
        assert!(t.requests().is_empty(), "{url:?} was sent");
    }
}

/// An `http:` URL passes: the renderer's rule writes it, and Meta's image
/// page does not refuse it.
#[tokio::test]
async fn an_http_image_url_is_sent() {
    let t = ScriptedTransport::new();
    script(&t, 1);
    let url = "http://www.luckyshrub.com/assets/succulents/aloe.png";
    ctx(&t)
        .reply_rich(&RichReply::new().image(url))
        .await
        .unwrap();
    assert_eq!(bodies(&t), [image(true, url)]);
    assert_eq!(t.remaining(), 0);
}

// ─── Product cards ───────────────────────────────────────────────────────

/// The carousel's limits are the client's: one card, eleven, or two
/// catalogs are refused before anything is sent; ten are sent.
#[tokio::test]
async fn a_carousel_takes_two_to_ten_cards_of_one_catalog() {
    let t = ScriptedTransport::new();
    let cards = |n: usize| -> Vec<ProductCard> {
        (0..n)
            .map(|i| ProductCard::new(CATALOG, format!("sku-{i}")))
            .collect()
    };
    for (cards, field) in [
        (cards(1), "interactive.action.cards"),
        (cards(11), "interactive.action.cards"),
        (
            vec![
                ProductCard::new(CATALOG, "a"),
                ProductCard::new("987654321", "b"),
            ],
            "interactive.action.cards[1].action.catalog_id",
        ),
    ] {
        let reply = lead().markdown("Featured").products(cards);
        let error = ctx(&t).reply_rich(&reply).await.unwrap_err();
        assert_eq!(validation(&error).field, field);
    }
    assert!(t.requests().is_empty());

    script(&t, 1);
    ctx(&t)
        .reply_rich(&RichReply::new().markdown("Featured").products(cards(10)))
        .await
        .unwrap();
    let request = bodies(&t).remove(0);
    assert_eq!(request["interactive"]["type"], "carousel");
    assert_eq!(
        request["interactive"]["action"]["cards"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    assert_eq!(t.remaining(), 0);
}

// ─── A failure in the middle ─────────────────────────────────────────────

/// Meta refuses the image (`131053`, a media upload error): the text before
/// it was sent, the buttons after it are not, and the error names the
/// image's step, its kind and retry safety kept. Make the loop go on after
/// a failure and the buttons' request goes out.
#[tokio::test]
async fn a_failure_midway_stops_the_reply_and_names_the_step() {
    let t = ScriptedTransport::new();
    t.push_json(200, send_response());
    t.push_json(
        400,
        json!({"error": {
            "message": "(#131053) Media upload error",
            "type": "OAuthException",
            "code": 131053,
            "error_data": {
                "messaging_product": "whatsapp",
                "details": "Unable to download the media from the URL"
            },
            "fbtrace_id": "AbCdEfGh"
        }}),
    );
    let reply = RichReply::new()
        .markdown("Your plant:")
        .image(IMAGE_URL)
        .markdown("Anything else?")
        .suggestions(numbered(2));

    let error = ctx(&t).reply_rich(&reply).await.unwrap_err();

    let Error::Step { step, source } = &error else {
        panic!("not a step error: {error:?}");
    };
    assert_eq!(*step, STEP_IMAGE);
    assert!(matches!(**source, Error::Api(_)), "{source:?}");
    assert!(!error.may_have_been_sent());
    assert_eq!(
        bodies(&t),
        [text(true, "Your plant:"), image(false, IMAGE_URL)]
    );
    assert_eq!(t.remaining(), 0);
}

/// A timed-out send may have gone out: the reply stops there (never resent,
/// never followed by the rest) and says so.
#[tokio::test]
async fn a_timeout_midway_is_not_resent_or_followed() {
    let t = ScriptedTransport::new();
    t.push_json(200, send_response());
    t.push_error(|| TransportError::Timeout);
    let reply = RichReply::new()
        .markdown("One")
        .markdown("Pick")
        .suggestions(numbered(4))
        .markdown("Three");

    let error = ctx(&t).reply_rich(&reply).await.unwrap_err();

    assert!(matches!(&error, Error::Step { step, .. } if *step == STEP_LIST));
    assert!(error.may_have_been_sent());
    assert_eq!(t.requests().len(), 2);
    assert_eq!(t.remaining(), 0);
}

/// Each kind of message fails as its own step.
#[tokio::test]
async fn each_message_fails_as_its_step() {
    for (reply, step) in [
        (RichReply::new().markdown("Hi"), STEP_TEXT),
        (RichReply::new().image(IMAGE_URL), STEP_IMAGE),
        (
            RichReply::new().markdown("Hi").suggestions(numbered(1)),
            STEP_BUTTONS,
        ),
        (
            RichReply::new().markdown("Hi").suggestions(numbered(5)),
            STEP_LIST,
        ),
        (
            RichReply::new().markdown("Hi").products([
                ProductCard::new(CATALOG, "a"),
                ProductCard::new(CATALOG, "b"),
            ]),
            STEP_PRODUCTS,
        ),
    ] {
        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        let error = ctx(&t).reply_rich(&reply).await.unwrap_err();
        assert!(
            matches!(&error, Error::Step { step: s, .. } if *s == step),
            "{error:?}"
        );
        assert_eq!(t.remaining(), 0);
    }
}

// ─── Suggestions that run commands ───────────────────────────────────────

/// A suggestion given as a command carries the command's payload, so the
/// tapped button runs it: `/menu` replies with a `cancel` suggestion, and
/// Meta's documented `button_reply` (id `cancel-button`) runs `/cancel`.
#[tokio::test]
async fn a_suggestion_given_as_a_command_routes_back_to_it() {
    let out = Recording::default();
    let ran = Arc::new(Mutex::new(Vec::new()));
    let ran_in = Arc::clone(&ran);
    let bot = Bot::builder()
        .outbound(out.clone())
        .errors(PropagateErrors)
        .command(Command::new("menu", |ctx: Ctx| async move {
            let reply = RichReply::new().markdown("What now?").suggestions([
                Suggestion::command("cancel", "Cancel"),
                Suggestion::command("order st", "Status"),
            ]);
            ctx.reply_rich(&reply).await?;
            Ok(())
        }))
        .command(
            Command::new("cancel", move |ctx: Ctx| {
                let ran = Arc::clone(&ran_in);
                async move {
                    ran.lock().unwrap().push(ctx.invocation().unwrap().clone());
                    Ok(())
                }
            })
            .payload("cancel-button"),
        )
        .command(
            Command::new("order", |_ctx: Ctx| async move { Ok(()) }).subcommand(
                Command::new("status", |_ctx: Ctx| async move { Ok(()) })
                    .alias("st")
                    .payload("order-status"),
            ),
        )
        .build()
        .await
        .unwrap();

    bot.handle(text_event(TEXT, "/menu")).await.unwrap();
    assert_eq!(
        out.sent(),
        [buttons(
            true,
            "What now?",
            &[("cancel-button", "Cancel"), ("order-status", "Status")]
        )]
    );

    bot.handle(event(BUTTON_REPLY)).await.unwrap();
    let ran = ran.lock().unwrap().clone();
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].command, "cancel");
}

/// A suggestion naming a command without a payload, or no command, is
/// refused before anything is sent: tapping it would run nothing.
#[tokio::test]
async fn a_suggestion_naming_no_payload_is_refused() {
    let out = Recording::default();
    let refused = Arc::new(Mutex::new(Vec::new()));
    let refused_in = Arc::clone(&refused);
    let bot = Bot::builder()
        .outbound(out.clone())
        .command(Command::new("menu", move |ctx: Ctx| {
            let refused = Arc::clone(&refused_in);
            async move {
                for path in ["help", "nothing", "menu extra words"] {
                    let reply = RichReply::new()
                        .markdown("What now?")
                        .suggestions([Suggestion::command(path, "Go")]);
                    let error = ctx.reply_rich(&reply).await.unwrap_err();
                    refused
                        .lock()
                        .unwrap()
                        .push(validation(&error).field.clone());
                }
                Ok(())
            }
        }))
        .command(Command::new("help", |_ctx: Ctx| async move { Ok(()) }))
        .build()
        .await
        .unwrap();

    bot.handle(text_event(TEXT, "/menu")).await.unwrap();
    assert_eq!(*refused.lock().unwrap(), ["suggestions[0]"; 3]);
    assert!(out.sent().is_empty());
}

// ─── Building without sending ────────────────────────────────────────────

/// `to_messages` gives the messages unquoted, checked, for a send of your
/// own.
#[test]
fn to_messages_builds_without_sending() {
    let to = Recipient::phone("+16505551234");
    let messages = RichReply::new()
        .markdown("Hi")
        .image(IMAGE_URL)
        .to_messages(&to, &Renderer::default(), &[])
        .unwrap();
    assert_eq!(
        messages,
        [
            OutboundMessage::text(to.clone(), "Hi"),
            OutboundMessage::image_link(to.clone(), IMAGE_URL),
        ]
    );
    let error = RichReply::new()
        .image("javascript:alert(1)")
        .to_messages(&to, &Renderer::default(), &[])
        .unwrap_err();
    assert_eq!(error.field, "image.link");
}
