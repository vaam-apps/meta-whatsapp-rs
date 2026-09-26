//! [`Ctx`]: what a middleware, a command or a listener gets for one event —
//! the event, who sent it, where to answer, the matched command, and the
//! reply helpers.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use meta_whatsapp_client::messages::{MessageContent, OutboundMessage, SendResponse, Text};
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{GroupId, PhoneNumberId, UserId, WaId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_webhooks::WebhookEvent;
use meta_whatsapp_webhooks::fields::InboundMessage;

use crate::command::{Args, CommandInfo, Invocation};
use crate::help::{Catalog, HelpSection};
use crate::markdown::MarkdownRenderer;
use crate::outbound::Outbound;
use crate::parse::ParsedCommand;

/// Who sent a message (or who an event is about).
///
/// Identity follows Meta's business-scoped user ids
/// (`business-scoped-user-ids`): [`BotSender::user_id`] (the BSUID) is the
/// key whenever Meta sent one, and [`BotSender::wa_id`] (the phone number)
/// only when it did not. Never key a user by phone number alone: a user
/// who adopted a username may have none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct BotSender {
    /// Business-scoped user id (BSUID), e.g. `US.13491208655302741918`.
    pub user_id: Option<UserId>,
    /// Parent BSUID (`US.ENT.…`), for portfolios enrolled in parent BSUIDs.
    pub parent_user_id: Option<UserId>,
    /// Phone number as WhatsApp reports it (`wa_id`), digits only.
    pub wa_id: Option<WaId>,
    /// Profile name, when Meta sent one.
    pub name: Option<String>,
    /// Username, when the user adopted one.
    pub username: Option<String>,
}

impl BotSender {
    /// The stable key of this user: the BSUID when there is one, else the
    /// phone number. `None` when the event named neither.
    pub fn key(&self) -> Option<&str> {
        self.user_id
            .as_ref()
            .map(UserId::as_str)
            .or_else(|| self.wa_id.as_ref().map(WaId::as_str))
    }

    /// How to address this user directly: by BSUID when there is one (like
    /// the CMS inbox), else by phone number with a `+` (Meta reads a number
    /// without it as local to the business number's country).
    pub fn recipient(&self) -> Option<Recipient> {
        if let Some(user) = &self.user_id {
            return Some(Recipient::User(user.clone()));
        }
        self.wa_id
            .as_ref()
            .map(|wa| Recipient::phone(format!("+{}", wa.as_str().trim_start_matches('+'))))
    }

    /// The sender of `message`, completed from the change's `contact`:
    /// `from_user_id` else the contact's `user_id`, `from` else the
    /// contact's `wa_id`. `None` when neither names anyone.
    pub(crate) fn of_message(
        message: &InboundMessage,
        contact: Option<&meta_whatsapp_webhooks::fields::Contact>,
    ) -> Option<Self> {
        let sender = Self {
            user_id: message
                .from_user_id
                .clone()
                .or_else(|| contact.and_then(|c| c.user_id.clone())),
            parent_user_id: message
                .from_parent_user_id
                .clone()
                .or_else(|| contact.and_then(|c| c.parent_user_id.clone())),
            wa_id: message
                .from
                .clone()
                .or_else(|| contact.and_then(|c| c.wa_id.clone())),
            name: contact.and_then(|c| c.name().map(str::to_owned)),
            username: contact.and_then(|c| c.username().map(str::to_owned)),
        };
        sender.key().is_some().then_some(sender)
    }

    /// The user a non-message event is about, from its contact.
    pub(crate) fn of_contact(contact: &meta_whatsapp_webhooks::fields::Contact) -> Option<Self> {
        let sender = Self {
            user_id: contact.user_id.clone(),
            parent_user_id: contact.parent_user_id.clone(),
            wa_id: contact.wa_id.clone(),
            name: contact.name().map(str::to_owned),
            username: contact.username().map(str::to_owned),
        };
        sender.key().is_some().then_some(sender)
    }
}

/// Where a received message was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Chat {
    /// A one-to-one chat with the business.
    Private,
    /// A group created with the Groups API: the message's `group_id`
    /// (`groups/groups-messaging`).
    Group(GroupId),
}

impl Chat {
    /// Whether this is a group chat.
    pub fn is_group(&self) -> bool {
        matches!(self, Self::Group(_))
    }
}

/// Values a middleware hands on to later middleware and handlers, one per
/// type (for example the integrator's user record, loaded once).
#[derive(Clone, Default)]
pub(crate) struct Extensions(HashMap<TypeId, Arc<dyn Any + Send + Sync>>);

impl fmt::Debug for Extensions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Extensions")
            .field("len", &self.0.len())
            .finish()
    }
}

/// The context of one event: the event itself, its sender and chat, the
/// matched command, and the reply helpers.
///
/// Cheap to clone. Its `Debug` output names the event kind and the command
/// only: never the message's content or the sender.
#[derive(Clone)]
pub struct Ctx {
    event: Arc<WebhookEvent>,
    sender: Option<BotSender>,
    chat: Option<Chat>,
    invocation: Option<Arc<Invocation>>,
    unknown: Option<Arc<ParsedCommand>>,
    outbound: Arc<dyn Outbound>,
    renderer: Arc<dyn MarkdownRenderer>,
    pub(crate) catalog: Arc<Catalog>,
    extensions: Extensions,
}

impl fmt::Debug for Ctx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ctx")
            .field("event", &self.event.kind())
            .field("command", &self.invocation().map(|i| i.command.as_str()))
            .finish_non_exhaustive()
    }
}

/// The empty argument list of a context without a command.
static NO_ARGS: std::sync::LazyLock<Args> = std::sync::LazyLock::new(Args::default);

impl Ctx {
    /// A context for `event`, sending through `outbound` and rendering
    /// Markdown with `renderer`. The bot builds one per event; build your
    /// own (with [`Self::with_invocation`] for a command) to unit-test a
    /// handler. A context built here knows no commands
    /// ([`Self::commands`] is empty).
    pub fn new(
        event: WebhookEvent,
        outbound: Arc<dyn Outbound>,
        renderer: Arc<dyn MarkdownRenderer>,
    ) -> Self {
        let (sender, chat) = match &event {
            WebhookEvent::MessageReceived {
                message, contact, ..
            } => (
                BotSender::of_message(message, contact.as_ref()),
                Some(message.group_id.clone().map_or(Chat::Private, Chat::Group)),
            ),
            other => (other.contact().and_then(BotSender::of_contact), None),
        };
        Self {
            event: Arc::new(event),
            sender,
            chat,
            invocation: None,
            unknown: None,
            outbound,
            renderer,
            catalog: Arc::default(),
            extensions: Extensions::default(),
        }
    }

    /// This context, invoking `invocation`: what the bot sets when a
    /// command matched, for a handler under test.
    #[must_use]
    pub fn with_invocation(mut self, invocation: Invocation) -> Self {
        self.invocation = Some(Arc::new(invocation));
        self
    }

    /// The bot's command registry, for a context the bot builds.
    pub(crate) fn with_catalog(mut self, catalog: Arc<Catalog>) -> Self {
        self.catalog = catalog;
        self
    }

    /// The text the parser read as a command whose name no command has.
    pub(crate) fn set_unknown(&mut self, parsed: ParsedCommand) {
        self.unknown = Some(Arc::new(parsed));
    }

    /// The event.
    pub fn event(&self) -> &WebhookEvent {
        &self.event
    }

    /// The received message, when the event is a
    /// [`WebhookEvent::MessageReceived`]. Never a standby copy or an echo of
    /// the business's own message: those are other events.
    pub fn message(&self) -> Option<&InboundMessage> {
        match self.event.as_ref() {
            WebhookEvent::MessageReceived { message, .. } => Some(message),
            _ => None,
        }
    }

    /// The body of a received text message (a caption is in the media's
    /// content: `ctx.message()`).
    pub fn text(&self) -> Option<&str> {
        match &self.message()?.content {
            meta_whatsapp_webhooks::fields::MessageContent::Text(t) => Some(&t.body),
            _ => None,
        }
    }

    /// Who sent the message (or who the event is about), when the event
    /// names anyone.
    pub fn sender(&self) -> Option<&BotSender> {
        self.sender.as_ref()
    }

    /// Where the message was sent; `None` for events that are not a
    /// received message.
    pub fn chat(&self) -> Option<&Chat> {
        self.chat.as_ref()
    }

    /// The business phone number the event arrived on, when it has one.
    pub fn phone_number_id(&self) -> Option<&PhoneNumberId> {
        self.event.phone_number_id()
    }

    /// The command this event invokes. The bot matches it before the
    /// middleware run, so a middleware sees it too (and can act for some
    /// commands only); the command's own guards run after the middleware.
    /// `None` for events no command matched, and in listeners.
    pub fn invocation(&self) -> Option<&Invocation> {
        self.invocation.as_deref()
    }

    /// A text (or caption) the parser read as a command, whose name no
    /// registered command has: `ctx.unknown_command().map(|c| &c.name)`
    /// is the name typed. Set from the match on, like
    /// [`Self::invocation`].
    pub fn unknown_command(&self) -> Option<&ParsedCommand> {
        self.unknown.as_deref()
    }

    /// The command's arguments; empty outside a command.
    pub fn args(&self) -> &Args {
        self.invocation().map_or(&NO_ARGS, |i| &i.args)
    }

    /// Every command of the bot, hidden ones included, in registration
    /// order (as `Bot::commands`): to build your own help or menu in a
    /// handler.
    pub fn commands(&self) -> &[CommandInfo] {
        &self.catalog.commands
    }

    /// The bot's commands that are not hidden, grouped by category (as
    /// `Bot::help_sections`); format them with a [`crate::HelpFormatter`]
    /// or your own code.
    pub fn help_sections(&self) -> Vec<HelpSection> {
        crate::help::sections(&self.catalog.commands)
    }

    /// The outbound this context sends through.
    pub fn outbound(&self) -> &Arc<dyn Outbound> {
        &self.outbound
    }

    /// The Markdown renderer [`Self::reply_markdown`] uses.
    pub fn renderer(&self) -> &Arc<dyn MarkdownRenderer> {
        &self.renderer
    }

    /// Hand `value` on to the middleware and handlers after this one
    /// (replacing a value of the same type).
    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) {
        self.extensions.0.insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// A value an earlier middleware inserted.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.extensions
            .0
            .get(&TypeId::of::<T>())
            .and_then(|v| v.downcast_ref())
    }

    /// Who the reply helpers answer: the group for a group message, else
    /// the sender ([`BotSender::recipient`]: BSUID first, else
    /// `+<wa_id>`). To answer someone else, build the message yourself and
    /// use [`Self::send`].
    pub fn reply_recipient(&self) -> Result<Recipient> {
        match self.chat() {
            Some(Chat::Group(group)) => Ok(Recipient::group(group.clone())),
            Some(Chat::Private) => self
                .sender()
                .and_then(BotSender::recipient)
                .ok_or_else(|| not_replyable("the message names no sender").into()),
            None => Err(not_replyable("the event is not a received message").into()),
        }
    }

    /// Send `message` from the business number the event arrived on, as
    /// built: no recipient or quote is chosen for you. The reply helpers
    /// below go through it.
    pub async fn send(&self, message: &OutboundMessage) -> Result<SendResponse> {
        let from = self
            .phone_number_id()
            .ok_or_else(|| not_replyable("the event names no business phone number"))?;
        self.outbound.send(from, message).await
    }

    /// Reply with `text`, quoting the received message (`context.message_id`,
    /// `messages/contextual-replies`), to [`Self::reply_recipient`]. For
    /// another recipient or no quote, use [`Self::send`].
    ///
    /// A reply is a free-form message: Meta accepts it only within 24
    /// hours of the user's last message (the customer service window),
    /// else it fails with `ErrorKind::CustomerServiceWindowClosed`
    /// (`131047`). The message being answered opens that window, but Meta
    /// redelivers a webhook for up to 7 days after an outage: when a late
    /// command must not act, compare `ctx.message()`'s `timestamp` with
    /// the clock first, and reach the user later with a template.
    pub async fn reply(&self, text: impl Into<String>) -> Result<SendResponse> {
        self.reply_with(Text::new(text)).await
    }

    /// Reply with any content, quoting the received message (see
    /// [`Self::reply`]).
    pub async fn reply_with(&self, content: impl Into<MessageContent>) -> Result<SendResponse> {
        let quoted = self
            .message()
            .ok_or_else(|| not_replyable("the event is not a received message"))?;
        let message =
            OutboundMessage::new(self.reply_recipient()?, content).reply_to(quoted.id.clone());
        self.send(&message).await
    }

    /// React to the received message with `emoji`
    /// (`messages/reaction-messages`), sent to [`Self::reply_recipient`].
    /// Meta's group messaging page (`groups/groups-messaging`) lists text,
    /// media and template messages only, so a reaction in a group may be
    /// refused.
    pub async fn react(&self, emoji: impl Into<String>) -> Result<SendResponse> {
        let reacted = self
            .message()
            .ok_or_else(|| not_replyable("the event is not a received message"))?;
        let message = OutboundMessage::reaction(self.reply_recipient()?, reacted.id.clone(), emoji);
        self.send(&message).await
    }

    /// Render `markdown` with the bot's [`MarkdownRenderer`] (by default
    /// [`crate::markdown::Renderer`]) and send it as text messages, in
    /// order: the first quotes the received message, the rest follow it.
    /// Empty Markdown sends nothing.
    ///
    /// The parts go out one by one: on an error the earlier ones were sent
    /// (never resent here: see `Error::may_have_been_sent`).
    pub async fn reply_markdown(&self, markdown: &str) -> Result<Vec<SendResponse>> {
        let parts = self.renderer.render(markdown);
        self.reply_parts(parts).await
    }

    /// Send already formatted `parts` as text messages, in order, the first
    /// quoting the received message. Split long text with
    /// [`crate::markdown::split`] first.
    pub async fn reply_parts(&self, parts: Vec<String>) -> Result<Vec<SendResponse>> {
        let quoted = self
            .message()
            .ok_or_else(|| not_replyable("the event is not a received message"))?;
        let to = self.reply_recipient()?;
        let mut sent = Vec::with_capacity(parts.len());
        for (i, part) in parts.into_iter().enumerate() {
            let mut message = OutboundMessage::text(to.clone(), part);
            if i == 0 {
                message = message.reply_to(quoted.id.clone());
            }
            sent.push(self.send(&message).await?);
        }
        Ok(sent)
    }
}

fn not_replyable(reason: &str) -> ValidationError {
    ValidationError::new("reply", format!("nothing to reply to: {reason}"))
}
