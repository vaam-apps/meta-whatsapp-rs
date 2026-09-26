//! [`Ctx`]: what a middleware, a command or a listener gets for one event —
//! the event, who sent it, where to answer, and the reply helpers.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use meta_whatsapp_client::messages::{MessageContent, OutboundMessage, SendResponse, Text};
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{GroupId, PhoneNumberId, UserId, WaId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_webhooks::WebhookEvent;
use meta_whatsapp_webhooks::fields::InboundMessage;

use crate::command::{Args, Invocation};
use crate::markdown::Renderer;
use crate::outbound::Outbound;

/// Who sent a message (or who an event is about).
///
/// Identity follows the business-scoped user id rules: [`Sender::user_id`]
/// (the BSUID) is the key whenever Meta sent one, and [`Sender::wa_id`] (the
/// phone number) only when it did not. Never key a user by phone number
/// alone: a user who adopted a username may have none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Sender {
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

impl Sender {
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
    /// A group created with the Groups API (the message's `group_id`).
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
pub struct Extensions(HashMap<TypeId, Arc<dyn Any + Send + Sync>>);

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
    sender: Option<Sender>,
    chat: Option<Chat>,
    /// Shared by every clone, so the error handler sees the command a
    /// handler failed in.
    pub(crate) invocation: Arc<OnceLock<Invocation>>,
    outbound: Arc<dyn Outbound>,
    renderer: Arc<Renderer>,
    extensions: Extensions,
}

impl fmt::Debug for Ctx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ctx")
            .field("event", &self.event.kind())
            .field(
                "command",
                &self.invocation.get().map(|i| i.command.as_str()),
            )
            .finish_non_exhaustive()
    }
}

/// The empty argument list of a context without a command.
static NO_ARGS: std::sync::LazyLock<Args> = std::sync::LazyLock::new(Args::default);

impl Ctx {
    /// A context for `event`. The bot builds one per event; build your own
    /// to unit-test a handler.
    pub fn new(event: WebhookEvent, outbound: Arc<dyn Outbound>, renderer: Arc<Renderer>) -> Self {
        let (sender, chat) = match &event {
            WebhookEvent::MessageReceived {
                message, contact, ..
            } => (
                Sender::of_message(message, contact.as_ref()),
                Some(message.group_id.clone().map_or(Chat::Private, Chat::Group)),
            ),
            other => (other.contact().and_then(Sender::of_contact), None),
        };
        Self {
            event: Arc::new(event),
            sender,
            chat,
            invocation: Arc::default(),
            outbound,
            renderer,
            extensions: Extensions::default(),
        }
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

    /// The body of a received text message.
    pub fn text(&self) -> Option<&str> {
        match &self.message()?.content {
            meta_whatsapp_webhooks::fields::MessageContent::Text(t) => Some(&t.body),
            _ => None,
        }
    }

    /// Who sent the message (or who the event is about), when the event
    /// names anyone.
    pub fn sender(&self) -> Option<&Sender> {
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

    /// The command this event invoked, once the bot matched one: `None`
    /// in middleware before `next.run` (the match comes after them), in
    /// listeners, and for events no command matched. Every clone of the
    /// context sees it once it is set.
    pub fn invocation(&self) -> Option<&Invocation> {
        self.invocation.get()
    }

    /// The command's arguments; empty outside a command.
    pub fn args(&self) -> &Args {
        self.invocation.get().map_or(&NO_ARGS, |i| &i.args)
    }

    /// The outbound this context sends through.
    pub fn outbound(&self) -> &Arc<dyn Outbound> {
        &self.outbound
    }

    /// The Markdown renderer [`Self::reply_markdown`] uses.
    pub fn renderer(&self) -> &Renderer {
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

    /// Who a reply goes to: the group for a group message, else the sender
    /// ([`Sender::recipient`]: BSUID first, else `+<wa_id>`).
    pub fn reply_recipient(&self) -> Result<Recipient> {
        match self.chat() {
            Some(Chat::Group(group)) => Ok(Recipient::group(group.clone())),
            Some(Chat::Private) => self
                .sender()
                .and_then(Sender::recipient)
                .ok_or_else(|| not_replyable("the message names no sender").into()),
            None => Err(not_replyable("the event is not a received message").into()),
        }
    }

    /// Send `message` from the business number the event arrived on.
    pub async fn send(&self, message: &OutboundMessage) -> Result<SendResponse> {
        let from = self
            .phone_number_id()
            .ok_or_else(|| not_replyable("the event names no business phone number"))?;
        self.outbound.send(from, message).await
    }

    /// Reply with `text`, quoting the received message.
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

    /// Reply with any content, quoting the received message.
    pub async fn reply_with(&self, content: impl Into<MessageContent>) -> Result<SendResponse> {
        let quoted = self
            .message()
            .ok_or_else(|| not_replyable("the event is not a received message"))?;
        let message =
            OutboundMessage::new(self.reply_recipient()?, content).reply_to(quoted.id.clone());
        self.send(&message).await
    }

    /// Render `markdown` to WhatsApp formatting ([`crate::markdown`]) and
    /// send it as text messages, in order: the first quotes the received
    /// message, the rest follow it. Empty Markdown sends nothing.
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
