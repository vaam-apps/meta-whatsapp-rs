//! In-app chat between a merchant and their customers — the CMS inbox.
//!
//! Wires the three halves together:
//!
//! ```text
//! Meta ─webhook─► WebhookHandler ─► FanoutSink ─┬─► InboxSink ──► ConversationStore
//!                                               └─► BroadcastSink ─► SSE to the merchant UI
//! merchant UI ─► Inbox::reply ─► Client (merchant's business token) ─► Meta
//!                      └────────► ConversationStore (outbound, status Accepted)
//! ```
//!
//! **Conversation keys.** A conversation is a business phone number plus
//! one contact. The contact part is the business-scoped user id (BSUID) when
//! Meta sent one — every messages webhook carries `user_id` since April
//! 2026 — else the `wa_id`, else, for group messages, the group id. BSUIDs
//! (`CC.digits`, e.g. `US.1349…`) always contain a `.`; phone numbers never
//! do, which is how [`Inbox::reply`] picks the addressing mode.
//!
//! **The 24-hour window.** Free-form replies are only accepted within 24
//! hours of the customer's last message; [`Inbox::reply`] checks the store
//! first and refuses (with a validation error on
//! `customer_service_window`) instead of paying for a request Meta would
//! reject with `131047`. Templates and Direct Send (`category`) messages are
//! exempt. Use [`Inbox::window`] to decide up front.
//!
//! **Not recorded yet:** coexistence message echoes (messages the merchant
//! sent from the WhatsApp Business app) and history sync; handle
//! `WebhookEvent::MessageEchoed` / `HistorySynced` yourself for now.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use time::OffsetDateTime;
use wa_client::Client;
use wa_client::messages::{MessageContent, OutboundMessage, SendResponse};
use wa_core::clock::{Clock, SystemClock};
use wa_core::error::{SinkError, StorageError, ValidationError};
use wa_core::ids::{MessageId, PhoneNumberId, UserId};
use wa_core::recipient::Recipient;
use wa_core::sink::EventSink;
use wa_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, CustomerServiceWindow, DeliveryStatus,
    Direction, StoredMessage,
};
use wa_core::{Error, Result};
use wa_webhooks::WebhookEvent;
use wa_webhooks::fields::common::Contact;
use wa_webhooks::fields::messages::{InboundMessage, InteractiveReply, MessageContent as In};

/// The conversation an inbound message belongs to, see the
/// [module docs](self) for the key rules. `None` when Meta identified the
/// sender by nothing usable.
pub fn conversation_key(
    phone_number_id: &PhoneNumberId,
    contact: Option<&Contact>,
    message: &InboundMessage,
) -> Option<ConversationKey> {
    let who = if let Some(group) = &message.group_id {
        group.to_string()
    } else if let Some(user) = message
        .from_user_id
        .as_ref()
        .or_else(|| contact.and_then(|c| c.user_id.as_ref()))
    {
        user.to_string()
    } else {
        message
            .from
            .as_ref()
            .or_else(|| contact.and_then(|c| c.wa_id.as_ref()))?
            .to_string()
    };
    Some(ConversationKey::new(phone_number_id.clone(), who))
}

/// A short plain-text rendering of an inbound message for inbox lists and
/// search. `None` for content without text.
pub fn preview(content: &In) -> Option<String> {
    match content {
        In::Text(t) => Some(t.body.clone()),
        In::Image(m) | In::Video(m) | In::Document(m) | In::Audio(m) | In::Sticker(m) => {
            m.caption.clone().or_else(|| m.filename.clone())
        }
        In::Location(l) => l.name.clone(),
        In::Button(b) => b.text.clone(),
        In::Reaction(r) => r.emoji.clone(),
        In::Interactive(InteractiveReply::ButtonReply(b)) => Some(b.title.clone()),
        In::Interactive(InteractiveReply::ListReply(l)) => Some(l.title.clone()),
        _ => None,
    }
}

fn delivery(e: StorageError) -> SinkError {
    SinkError::Delivery(anyhow::Error::new(e))
}

/// Records inbound messages and status updates into a
/// [`ConversationStore`]. Idempotent: the store ignores a message id it
/// already has, and a status that does not supersede the stored one.
///
/// Put a `wa_webhooks::DedupGuard` in front of the handler anyway — it
/// saves the store the work — and fan this sink out next to a broadcast
/// sink for live updates.
#[derive(Clone)]
pub struct InboxSink {
    store: Arc<dyn ConversationStore>,
}

impl fmt::Debug for InboxSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InboxSink").finish_non_exhaustive()
    }
}

impl InboxSink {
    /// Record into `store`.
    pub fn new(store: Arc<dyn ConversationStore>) -> Self {
        Self { store }
    }

    async fn record_inbound(
        &self,
        phone_number_id: &PhoneNumberId,
        contact: Option<&Contact>,
        message: &InboundMessage,
    ) -> std::result::Result<(), SinkError> {
        // A revoke is a status change of the original message, not a new row.
        if let In::Revoke(r) = &message.content {
            self.store
                .update_status(
                    &r.original_message_id,
                    DeliveryStatus::Deleted,
                    message.timestamp,
                    None,
                )
                .await
                .map_err(delivery)?;
            return Ok(());
        }
        let Some(conversation) = conversation_key(phone_number_id, contact, message) else {
            tracing::warn!(
                kind = message.message_type().unwrap_or("unknown"),
                "inbound message without a usable sender id; not recorded"
            );
            return Ok(());
        };
        let payload = serde_json::to_value(message)
            .map_err(|e| SinkError::Delivery(anyhow::Error::new(e)))?;
        self.store
            .append(StoredMessage {
                id: message.id.clone(),
                conversation,
                direction: Direction::Inbound,
                kind: message.message_type().unwrap_or("unknown").to_owned(),
                text: preview(&message.content),
                payload,
                status: DeliveryStatus::Received,
                timestamp: message.timestamp,
                status_at: None,
                error: None,
            })
            .await
            .map_err(delivery)?;
        Ok(())
    }
}

#[async_trait]
impl EventSink<WebhookEvent> for InboxSink {
    async fn deliver(&self, event: WebhookEvent) -> std::result::Result<(), SinkError> {
        match event {
            WebhookEvent::MessageReceived {
                phone_number_id,
                contact,
                message,
                ..
            } => {
                self.record_inbound(&phone_number_id, contact.as_ref(), &message)
                    .await
            }
            WebhookEvent::StatusUpdated { status, .. } => {
                let Some(new) = DeliveryStatus::from_webhook(status.status.as_str()) else {
                    return Ok(());
                };
                let error = if status.errors.is_empty() {
                    None
                } else {
                    serde_json::to_value(&status.errors).ok()
                };
                self.store
                    .update_status(&status.id, new, status.timestamp, error)
                    .await
                    .map_err(delivery)?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// A merchant's inbox on one business phone number.
///
/// Build it with a [`Client`] already carrying that merchant's business
/// token (`client.with_token(..)` from the Embedded Signup token vault) and
/// the same store the [`InboxSink`] writes to.
#[derive(Clone)]
pub struct Inbox {
    client: Client,
    phone_number_id: PhoneNumberId,
    store: Arc<dyn ConversationStore>,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for Inbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inbox")
            .field("phone_number_id", &self.phone_number_id)
            .finish_non_exhaustive()
    }
}

impl Inbox {
    /// Inbox of `phone_number_id`, replying through `client`.
    pub fn new(
        client: Client,
        phone_number_id: impl Into<PhoneNumberId>,
        store: Arc<dyn ConversationStore>,
    ) -> Self {
        Self {
            client,
            phone_number_id: phone_number_id.into(),
            store,
            clock: Arc::new(SystemClock),
        }
    }

    /// Use `clock` for the window check (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The business phone number this inbox serves.
    pub fn phone_number_id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The conversation key for `contact` (a BSUID or a `wa_id`) on this
    /// number.
    pub fn key(&self, contact: impl Into<String>) -> ConversationKey {
        ConversationKey::new(self.phone_number_id.clone(), contact)
    }

    /// Conversations, most recent activity first; pass the last row's
    /// `(last_message_at, key.contact)` as `before` for the next page.
    pub async fn conversations(
        &self,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>> {
        Ok(self
            .store
            .conversations(&self.phone_number_id, before, limit)
            .await?)
    }

    /// Messages of one conversation, newest first; pass the last row's
    /// `(timestamp, id)` as `before` for the next page.
    pub async fn history(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, MessageId)>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        self.check_key(key)?;
        Ok(self.store.messages(key, before, limit).await?)
    }

    /// The customer service window of a conversation.
    pub async fn window(&self, key: &ConversationKey) -> Result<CustomerServiceWindow> {
        self.check_key(key)?;
        let last = self.store.last_inbound_at(key).await?;
        Ok(CustomerServiceWindow::from_last_inbound(last))
    }

    /// Reset the unread count (the merchant opened the conversation).
    pub async fn mark_read(&self, key: &ConversationKey) -> Result<()> {
        self.check_key(key)?;
        Ok(self.store.mark_read(key).await?)
    }

    /// Send `content` to the conversation's contact and record it.
    ///
    /// Free-form content outside the 24-hour window is refused locally
    /// (validation error on `customer_service_window`); templates are always
    /// allowed. The recorded row has status
    /// [`DeliveryStatus::Accepted`] until status webhooks move it on.
    pub async fn reply(
        &self,
        key: &ConversationKey,
        content: MessageContent,
    ) -> Result<SendResponse> {
        self.send(key, OutboundMessage::new(recipient_for(key), content))
            .await
    }

    /// Like [`Inbox::reply`], for a fully built message (context/quoted
    /// reply, Direct Send `category`, callback data). The message's
    /// recipient must be the conversation's contact.
    pub async fn send(
        &self,
        key: &ConversationKey,
        message: OutboundMessage,
    ) -> Result<SendResponse> {
        self.check_key(key)?;
        let exempt =
            matches!(message.content, MessageContent::Template(_)) || message.category.is_some();
        if !exempt && !self.window(key).await?.is_open(self.clock.now()) {
            return Err(ValidationError::new(
                "customer_service_window",
                "more than 24 hours since the customer's last message; send a template",
            )
            .into());
        }
        let response = self
            .client
            .messages(self.phone_number_id.clone())
            .send(&message)
            .await?;
        if let Some(sent) = response.messages.first() {
            let text = match &message.content {
                MessageContent::Text(t) => Some(t.body.clone()),
                _ => None,
            };
            let payload =
                serde_json::to_value(&message).map_err(|e| Error::Other(anyhow::Error::new(e)))?;
            // The message is already sent: a storage failure here must not
            // read as a send failure (a retry would send it twice).
            if let Err(e) = self
                .store
                .append(StoredMessage {
                    id: sent.id.clone(),
                    conversation: key.clone(),
                    direction: Direction::Outbound,
                    kind: message.content.message_type().to_owned(),
                    text,
                    payload,
                    status: DeliveryStatus::Accepted,
                    timestamp: self.clock.now(),
                    status_at: None,
                    error: None,
                })
                .await
            {
                tracing::error!(error = %e, "message sent but not recorded in the inbox");
            }
        }
        Ok(response)
    }

    fn check_key(&self, key: &ConversationKey) -> Result<()> {
        if key.phone_number_id == self.phone_number_id {
            Ok(())
        } else {
            Err(ValidationError::new(
                "conversation",
                "belongs to a different business phone number than this inbox",
            )
            .into())
        }
    }
}

/// Addressing mode for a conversation contact (see the [module docs](self)):
/// BSUIDs contain a `.`, phone numbers are digits (optionally `+`-prefixed),
/// anything else is a group id.
fn recipient_for(key: &ConversationKey) -> Recipient {
    let c = key.contact.as_str();
    let digits = c.strip_prefix('+').unwrap_or(c);
    if c.contains('.') {
        Recipient::User(UserId::new(c))
    } else if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        Recipient::phone(c)
    } else {
        Recipient::group(c)
    }
}
