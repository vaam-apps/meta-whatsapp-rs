//! [`RichReply`]: a reply built from Markdown and a few typed parts, each
//! sent as the message Meta documents for it.
//!
//! | Part | Sent as |
//! | --- | --- |
//! | [`RichReply::markdown`] | text messages, through the bot's [`MarkdownRenderer`] (split as [`Ctx::reply_markdown`] splits) |
//! | [`RichReply::image`] | an image message by link (`messages/image-messages`), the URL checked first (see [Images](self#images)) |
//! | [`RichReply::suggestions`], up to [`BUTTONS_MAX`] (3) | reply buttons (`messages/interactive-reply-buttons-messages`) |
//! | [`RichReply::suggestions`], 4 to [`SUGGESTIONS_MAX`] (10) | a list of one section (`messages/interactive-list-messages`) |
//! | [`RichReply::products`] | a product carousel, 2 to 10 cards of one catalog (`catalogs/interactive-product-carousel-messages`) |
//!
//! Each message is the client's own builder ([`ReplyButtons`],
//! [`ListMessage`], [`ProductCarousel`], [`Image`]), checked by
//! [`OutboundMessage::validate`]: its [`ValidationError`] is the reply's.
//!
//! # Bodies
//!
//! Buttons, a list and a carousel need a body text. Theirs is the text
//! right before them: the last message the [`RichReply::markdown`] part
//! just before them renders to is sent as their body, not as a text
//! message of its own (cut first when it is longer than the body allows,
//! 1024 characters for buttons and carousels, 4096 for a list: the pieces
//! before the last go as text messages). Without a Markdown part right
//! before them (first in the reply, after an image, after other buttons),
//! the reply is refused. To ask a separate question, make it that part:
//! `.markdown(answer).markdown("Anything else?").suggestions(..)`.
//!
//! # Suggestions
//!
//! A [`Suggestion`] is a label and the id Meta returns when it is tapped
//! (`button_reply` or `list_reply`). The bot runs a command for a tapped id
//! that is one of its payloads ([`crate::Command::payload`]), so
//! [`Suggestion::command`] takes the command's first payload: tapping it
//! runs the command, and a command without a payload is refused when the
//! reply is built. [`Suggestion::new`] sends the id as given (a payload of
//! yours, or an id a listener reads). A list's button is labelled
//! [`LIST_BUTTON`] unless [`RichReply::list_button`] says otherwise, and its
//! one section is titled the same (the list page marks a section's title
//! required). A suggestion's description shows in a list only: buttons
//! have none.
//!
//! # Images
//!
//! An image link must pass the renderer's URL rule (the one it writes
//! links and images by: no `javascript:`, `data:`, `file:` or other
//! scheme) and be what Meta can fetch from "your public server"
//! (`messages/image-messages`): an absolute `http` or `https` URL with a
//! host, no spaces or control characters. A relative, `mailto:` or `tel:`
//! URL, which the renderer would write as text, is refused here. Meta's
//! pages do not say `http` is refused, so it is not; prefer `https`. The
//! refusal does not repeat the URL (a signed URL is a secret).
//!
//! # Sending
//!
//! [`Ctx::reply_rich`] builds every message first, so a reply with any
//! part over a limit (more than 10 suggestions, one card or eleven, a
//! button title over 20 characters, an image URL refused above) sends
//! nothing. It then sends them in order, one message each, the first
//! quoting the received message. When one fails the rest are not sent,
//! and the error names the step ([`STEP_TEXT`], [`STEP_IMAGE`],
//! [`STEP_BUTTONS`], [`STEP_LIST`], [`STEP_PRODUCTS`]); the earlier ones
//! were sent and are never sent again here (sends are not idempotent, and
//! the client never replays a timed-out send: see
//! `Error::may_have_been_sent`, which the step keeps).
//!
//! Interactive messages in a group: Meta's group messaging page
//! (`groups/groups-messaging`) lists text, media and template messages
//! only, so buttons, lists and carousels sent to a group may be refused.
//!
//! [`Ctx::reply_markdown`]: crate::Ctx::reply_markdown
//! [`Ctx::reply_rich`]: crate::Ctx::reply_rich

use meta_whatsapp_client::messages::{
    Image, ListMessage, ListRow, ListSection, MediaSource, OutboundMessage, ProductCard,
    ProductCarousel, ReplyButton, ReplyButtons, Text,
};
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::recipient::Recipient;

use crate::command::CommandInfo;
use crate::markdown::{self, MarkdownRenderer};

/// Most suggestions sent as reply buttons; more become a list
/// (`messages/interactive-reply-buttons-messages`: "up to three").
pub const BUTTONS_MAX: usize = 3;

/// Most suggestions a reply part carries: a list's rows
/// (`messages/interactive-list-messages`: "up to 10 rows for all sections
/// combined"). The client refuses more.
pub const SUGGESTIONS_MAX: usize = 10;

/// The label of a list's button (and its section's title) unless
/// [`RichReply::list_button`] sets another; at most 20 characters
/// (`messages/interactive-list-messages`).
pub const LIST_BUTTON: &str = "Options";

/// The step of a failed text message of a [`RichReply`] (`Error::Step`).
pub const STEP_TEXT: &str = "reply_text";
/// The step of a failed image message of a [`RichReply`].
pub const STEP_IMAGE: &str = "reply_image";
/// The step of a failed reply buttons message of a [`RichReply`].
pub const STEP_BUTTONS: &str = "reply_buttons";
/// The step of a failed list message of a [`RichReply`].
pub const STEP_LIST: &str = "reply_list";
/// The step of a failed product carousel of a [`RichReply`].
pub const STEP_PRODUCTS: &str = "reply_products";

/// Longest body of reply buttons and of a product carousel ("Maximum 1024
/// characters" on both pages).
const SHORT_BODY_MAX: usize = 1024;

/// Longest body of a list (`messages/interactive-list-messages`: "Maximum
/// 4096 characters").
const LIST_BODY_MAX: usize = 4096;

/// One suggested answer: a reply button or a list row. See the
/// [module docs](self#suggestions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    target: Target,
    title: String,
    description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// Sent as is.
    Id(String),
    /// A command's path (`order` or `order status`), resolved to its
    /// first payload.
    Command(String),
}

impl Suggestion {
    /// Labelled `title`, returning `id` when tapped: at most 20 characters
    /// for a button title, 24 for a list row's, and an id of at most 256
    /// (button) or 200 (row) characters, each unique in the message (the
    /// client checks them).
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            target: Target::Id(id.into()),
            title: title.into(),
            description: None,
        }
    }

    /// Labelled `title`, running the command at `path` when tapped: its
    /// name or an alias, then a subcommand's (`"order status"`), as the
    /// parser normalizes them (lowercase by default), without the prefix.
    /// The id sent is the command's first payload
    /// ([`crate::Command::payload`]); a reply naming a command that has
    /// none, or no such command, is refused before anything is sent.
    pub fn command(path: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            target: Target::Command(path.into()),
            title: title.into(),
            description: None,
        }
    }

    /// A line under the title, in a list (at most 72 characters); buttons
    /// have none, so it is left out of them.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// The label.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The id it returns when tapped, as it will be sent.
    fn id(&self, index: usize, commands: &[CommandInfo]) -> Result<String, ValidationError> {
        match &self.target {
            Target::Id(id) => Ok(id.clone()),
            Target::Command(path) => {
                payload_of(path, commands)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        ValidationError::new(
                            format!("suggestions[{index}]"),
                            format!(
                                "`{path}` names no command with a payload: register one with \
                             `Command::payload`"
                            ),
                        )
                    })
            }
        }
    }
}

/// The first payload of the command (or subcommand) at `path`.
fn payload_of<'a>(path: &str, commands: &'a [CommandInfo]) -> Option<&'a str> {
    let named = |info: &&CommandInfo, name: &str| {
        info.name == name || info.aliases.iter().any(|a| a == name)
    };
    let mut words = path.split_whitespace();
    let name = words.next()?;
    let sub = words.next();
    if words.next().is_some() {
        return None;
    }
    let command = commands.iter().find(|c| named(c, name))?;
    let target = match sub {
        None => command,
        Some(sub) => command.subcommands.iter().find(|c| named(c, sub))?,
    };
    target.payloads.first().map(String::as_str)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Markdown(String),
    Image {
        url: String,
        caption: Option<String>,
    },
    Suggestions(Vec<Suggestion>),
    Products(Vec<ProductCard>),
}

/// A reply of Markdown and typed parts, sent in order with
/// [`Ctx::reply_rich`](crate::Ctx::reply_rich). See the
/// [module docs](self).
///
/// ```
/// use meta_whatsapp_bot::rich::{RichReply, Suggestion};
///
/// let reply = RichReply::new()
///     .markdown("**Order 42** has shipped.")
///     .image("https://shop.example/parcels/42.png")
///     .markdown("Anything else?")
///     .suggestions([
///         Suggestion::command("order status", "Track it"),
///         Suggestion::new("thanks", "No, thanks"),
///     ]);
/// assert!(!reply.is_empty());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichReply {
    parts: Vec<Part>,
    list_button: Option<String>,
}

/// A message of a [`RichReply`] and the step it fails as.
pub(crate) struct Planned {
    pub(crate) step: &'static str,
    pub(crate) message: OutboundMessage,
}

impl RichReply {
    /// An empty reply (it sends nothing).
    pub fn new() -> Self {
        Self::default()
    }

    /// Then `markdown`, rendered by the bot's [`MarkdownRenderer`] into
    /// text messages; its last one is the body of suggestions or products
    /// right after it (see [Bodies](self#bodies)).
    #[must_use]
    pub fn markdown(mut self, markdown: impl Into<String>) -> Self {
        self.parts.push(Part::Markdown(markdown.into()));
        self
    }

    /// Then an image message, by link (see [Images](self#images)).
    #[must_use]
    pub fn image(mut self, url: impl Into<String>) -> Self {
        self.parts.push(Part::Image {
            url: url.into(),
            caption: None,
        });
        self
    }

    /// Then an image message with `caption`, sent as written (already in
    /// WhatsApp formatting; at most 1024 characters).
    #[must_use]
    pub fn image_with_caption(
        mut self,
        url: impl Into<String>,
        caption: impl Into<String>,
    ) -> Self {
        self.parts.push(Part::Image {
            url: url.into(),
            caption: Some(caption.into()),
        });
        self
    }

    /// Then `suggestions`, on the text right before them: up to
    /// [`BUTTONS_MAX`] as reply buttons, more (up to [`SUGGESTIONS_MAX`])
    /// as a list (see [Suggestions](self#suggestions)).
    #[must_use]
    pub fn suggestions(mut self, suggestions: impl IntoIterator<Item = Suggestion>) -> Self {
        self.parts
            .push(Part::Suggestions(suggestions.into_iter().collect()));
        self
    }

    /// Then a product carousel of `cards`, on the text right before them:
    /// 2 to 10 cards, every one from the same catalog.
    #[must_use]
    pub fn products(mut self, cards: impl IntoIterator<Item = ProductCard>) -> Self {
        self.parts.push(Part::Products(cards.into_iter().collect()));
        self
    }

    /// Label the button of this reply's lists `label` (and title their
    /// section so) instead of [`LIST_BUTTON`]: at most 20 characters.
    #[must_use]
    pub fn list_button(mut self, label: impl Into<String>) -> Self {
        self.list_button = Some(label.into());
        self
    }

    /// Whether the reply has no part.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// The messages this reply sends to `to`, in order, each checked with
    /// [`OutboundMessage::validate`]: Markdown rendered with `renderer`,
    /// [`Suggestion::command`] resolved in `commands` (`Ctx::commands`).
    /// None is quoted: [`Ctx::reply_rich`](crate::Ctx::reply_rich) quotes
    /// the first. To send them yourself, through `Ctx::send` or a client.
    pub fn to_messages(
        &self,
        to: &Recipient,
        renderer: &dyn MarkdownRenderer,
        commands: &[CommandInfo],
    ) -> Result<Vec<OutboundMessage>, ValidationError> {
        Ok(self
            .plan(to, renderer, commands)?
            .into_iter()
            .map(|p| p.message)
            .collect())
    }

    /// The messages, in order, with their steps, each validated.
    pub(crate) fn plan(
        &self,
        to: &Recipient,
        renderer: &dyn MarkdownRenderer,
        commands: &[CommandInfo],
    ) -> Result<Vec<Planned>, ValidationError> {
        let mut plan = Plan {
            to,
            out: Vec::new(),
            body: None,
        };
        for part in &self.parts {
            match part {
                Part::Markdown(markdown) => {
                    plan.flush();
                    let mut messages = renderer.render(markdown);
                    plan.body = messages.pop();
                    for text in messages {
                        plan.text(text);
                    }
                }
                Part::Image { url, caption } => {
                    plan.flush();
                    if !image_link_allowed(url) {
                        return Err(ValidationError::new(
                            "image.link",
                            "must be an absolute http or https URL with a host (the renderer's \
                             URL rule: no `javascript:`, `data:` or other scheme, and Meta \
                             fetches it, so no relative URL)",
                        ));
                    }
                    let mut image = Image::new(MediaSource::link(url.clone()));
                    if let Some(caption) = caption {
                        image = image.caption(caption.clone());
                    }
                    plan.push(STEP_IMAGE, image);
                }
                Part::Suggestions(suggestions) => {
                    let as_buttons = suggestions.len() <= BUTTONS_MAX;
                    let max = if as_buttons {
                        SHORT_BODY_MAX
                    } else {
                        LIST_BODY_MAX
                    };
                    let body = plan.take_body("suggestions", max)?;
                    let mut ids = Vec::with_capacity(suggestions.len());
                    for (i, s) in suggestions.iter().enumerate() {
                        ids.push(s.id(i, commands)?);
                    }
                    let items = suggestions.iter().zip(ids);
                    if as_buttons {
                        let buttons = items.map(|(s, id)| ReplyButton::new(id, s.title.clone()));
                        plan.push(STEP_BUTTONS, ReplyButtons::new(body, buttons));
                    } else {
                        let label = self.list_button.as_deref().unwrap_or(LIST_BUTTON);
                        let rows = items.map(|(s, id)| {
                            let row = ListRow::new(id, s.title.clone());
                            match &s.description {
                                Some(d) => row.description(d.clone()),
                                None => row,
                            }
                        });
                        let section = ListSection::new(label, rows);
                        plan.push(STEP_LIST, ListMessage::new(body, label, [section]));
                    }
                }
                Part::Products(cards) => {
                    let body = plan.take_body("products", SHORT_BODY_MAX)?;
                    plan.push(STEP_PRODUCTS, ProductCarousel::new(body, cards.clone()));
                }
            }
        }
        plan.flush();
        for planned in &plan.out {
            planned.message.validate()?;
        }
        Ok(plan.out)
    }
}

/// The messages being planned, and the last rendered text, held back in
/// case the next part takes it as its body.
struct Plan<'a> {
    to: &'a Recipient,
    out: Vec<Planned>,
    body: Option<String>,
}

impl Plan<'_> {
    fn push(
        &mut self,
        step: &'static str,
        content: impl Into<meta_whatsapp_client::messages::MessageContent>,
    ) {
        self.out.push(Planned {
            step,
            message: OutboundMessage::new(self.to.clone(), content),
        });
    }

    fn text(&mut self, text: String) {
        self.push(STEP_TEXT, Text::new(text));
    }

    /// Send the held-back text as a text message.
    fn flush(&mut self) {
        if let Some(text) = self.body.take() {
            self.text(text);
        }
    }

    /// The held-back text as a body of at most `max` characters, the
    /// pieces before it cut off as text messages; refused without one.
    fn take_body(&mut self, part: &str, max: usize) -> Result<String, ValidationError> {
        let Some(text) = self.body.take() else {
            return Err(ValidationError::new(
                part,
                "needs a Markdown part right before it: its last message is the body",
            ));
        };
        if text.encode_utf16().count() <= max {
            return Ok(text);
        }
        let mut pieces = markdown::split(&text, max);
        let body = pieces.pop().unwrap_or_default();
        for piece in pieces {
            self.text(piece);
        }
        Ok(body)
    }
}

/// Whether `url` may be sent as an image link: the renderer's URL rule,
/// then an absolute `http(s)` URL with a host and no whitespace or control
/// characters (see [Images](self#images)).
fn image_link_allowed(url: &str) -> bool {
    if !markdown::url_allowed(url) {
        return false;
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let web = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    web && !host.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_links_follow_the_renderers_rule_and_must_be_fetchable() {
        for ok in [
            "https://shop.example/a.png",
            "HTTPS://shop.example/a.png?x=1",
            "http://shop.example/a.png",
        ] {
            assert!(image_link_allowed(ok), "{ok}");
        }
        for refused in [
            "javascript:alert(1)",
            " javascript:alert(1)",
            "java\tscript:alert(1)",
            "data:image/png;base64,AAAA",
            "file:///etc/passwd",
            "/images/a.png",
            "a.png",
            "mailto:a@b.example",
            "tel:+15550000",
            "https://",
            "https:///a.png",
            "https://shop.example/a b.png",
            "https://shop.example/a.png\n",
            "ftp://shop.example/a.png",
            "",
        ] {
            assert!(!image_link_allowed(refused), "{refused:?}");
        }
        // Under the renderer's rule: never a link it would not write.
        for ok in ["https://shop.example/a.png", "http://shop.example/a.png"] {
            assert!(markdown::url_allowed(ok));
        }
    }
}
