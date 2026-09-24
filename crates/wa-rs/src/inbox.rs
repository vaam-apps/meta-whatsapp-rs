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
//! do, which is how [`Inbox::reply`] picks the addressing mode. A `wa_id` is
//! the full international number without `+`; replies go to `+<wa_id>`,
//! because Meta prepends the *business* number's country code to a number
//! sent without `+` — the reply would reach a different person.
//!
//! **The 24-hour window.** Free-form replies are only accepted within 24
//! hours of the customer's last message; [`Inbox::reply`] checks the store
//! first and refuses (with
//! [`ValidationError::customer_service_window_closed`], whose
//! [`Error::kind`](wa_core::Error::kind) is `CustomerServiceWindowClosed`,
//! like Meta's `131047`)
//! instead of paying for a request Meta would reject. Templates and Direct
//! Send (`category`) messages are exempt. Use [`Inbox::window`] to decide up
//! front.
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
use wa_core::Result;
use wa_core::clock::{Clock, SystemClock};
use wa_core::error::{SinkError, StorageError, ValidationError};
use wa_core::ids::{MessageId, PhoneNumberId, UserId};
use wa_core::recipient::Recipient;
use wa_core::sink::EventSink;
use wa_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, CustomerServiceWindow, DeliveryStatus,
    Direction, StoredMessage,
};
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

/// `s` with every U+0000 replaced by U+FFFD (the replacement character).
///
/// Postgres cannot store NUL in `TEXT` or `JSONB`, so one NUL typed by a
/// customer would fail every delivery of its webhook batch: Meta retries it
/// for 7 days, then drops the whole batch. Lossy on purpose — a stored
/// message with a visible replacement character beats a lost one.
fn without_nul(s: String) -> String {
    if s.contains('\0') {
        s.replace('\0', "\u{FFFD}")
    } else {
        s
    }
}

/// [`without_nul`] applied to every string of `value`, object keys
/// included. Two keys that differ only by NUL vs U+FFFD collapse into one
/// (the later wins). Recursion depth is bounded by the parse that produced
/// the value (`serde_json` refuses nesting deeper than 128).
fn json_without_nul(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => Value::String(without_nul(s)),
        Value::Array(items) => Value::Array(items.into_iter().map(json_without_nul).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (without_nul(k), json_without_nul(v)))
                .collect(),
        ),
        other => other,
    }
}

/// Records inbound messages and status updates into a
/// [`ConversationStore`]. Idempotent: the store ignores a message id it
/// already has, and a status that does not supersede the stored one.
///
/// Content never makes a delivery fail: U+0000, which Postgres cannot
/// store, is replaced by U+FFFD in the stored `kind`, `text`, `payload`
/// (keys included) and status `error`. Storage errors still fail it (Meta
/// redelivers).
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
        // A revoke is a status change of the original message, not a new row,
        // and only ever of a message of the number the revoke arrived on.
        if let In::Revoke(r) = &message.content {
            self.store
                .update_status(
                    phone_number_id,
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
        // Customer content never fails a delivery: see `without_nul`.
        self.store
            .append(StoredMessage {
                id: message.id.clone(),
                conversation,
                direction: Direction::Inbound,
                kind: without_nul(message.message_type().unwrap_or("unknown").to_owned()),
                text: preview(&message.content).map(without_nul),
                payload: json_without_nul(payload),
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
            WebhookEvent::StatusUpdated {
                phone_number_id,
                status,
                ..
            } => {
                let Some(new) = DeliveryStatus::from_webhook(status.status.as_str()) else {
                    return Ok(());
                };
                let error = if status.errors.is_empty() {
                    None
                } else {
                    serde_json::to_value(&status.errors)
                        .ok()
                        .map(json_without_nul)
                };
                self.store
                    .update_status(&phone_number_id, &status.id, new, status.timestamp, error)
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
    /// ([`ValidationError::customer_service_window_closed`]); templates are
    /// always allowed. The recorded row has status
    /// [`DeliveryStatus::Accepted`] until status webhooks move it on.
    pub async fn reply(
        &self,
        key: &ConversationKey,
        content: MessageContent,
    ) -> Result<SendResponse> {
        self.send(key, OutboundMessage::new(recipient_for(key), content))
            .await
    }

    /// The recipient a message to this conversation must be addressed to
    /// (build [`OutboundMessage`]s for [`Inbox::send`] with it).
    pub fn recipient(&self, key: &ConversationKey) -> Recipient {
        recipient_for(key)
    }

    /// Like [`Inbox::reply`], for a fully built message (context/quoted
    /// reply, Direct Send `category`, callback data). The message must be
    /// addressed to [`Inbox::recipient`] of `key`; anything else is refused,
    /// so a conversation can't be used to message someone outside it.
    pub async fn send(
        &self,
        key: &ConversationKey,
        message: OutboundMessage,
    ) -> Result<SendResponse> {
        self.check_key(key)?;
        if message.recipient != recipient_for(key) {
            return Err(ValidationError::new(
                "recipient",
                "must be the conversation's contact (use Inbox::recipient)",
            )
            .into());
        }
        let exempt =
            matches!(message.content, MessageContent::Template(_)) || message.category.is_some();
        if !exempt && !self.window(key).await?.is_open(self.clock.now()) {
            return Err(ValidationError::customer_service_window_closed().into());
        }
        let response = self
            .client
            .messages(self.phone_number_id.clone())
            .send(&message)
            .await?;
        if let Some(sent) = response.messages.first() {
            self.record_sent(key, &message, sent.id.clone()).await;
        }
        Ok(response)
    }

    /// Record a message Meta accepted. Never fails: the message is already
    /// sent, so neither a serialization nor a storage failure may read as a
    /// send failure (a retry would send it twice). Both are logged, without
    /// the message's content.
    async fn record_sent(&self, key: &ConversationKey, message: &OutboundMessage, id: MessageId) {
        let payload = match serde_json::to_value(message) {
            Ok(payload) => json_without_nul(payload),
            Err(e) => {
                tracing::error!(
                    category = ?e.classify(),
                    "message sent but not recorded in the inbox: it could not be serialized"
                );
                return;
            }
        };
        let text = match &message.content {
            MessageContent::Text(t) => Some(without_nul(t.body.clone())),
            _ => None,
        };
        if let Err(e) = self
            .store
            .append(StoredMessage {
                id,
                conversation: key.clone(),
                direction: Direction::Outbound,
                kind: without_nul(message.content.message_type().to_owned()),
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
/// BSUIDs contain a `.`, phone numbers are digits (optionally `+`-prefixed)
/// and are always sent as `+<digits>`, anything else is a group id.
fn recipient_for(key: &ConversationKey) -> Recipient {
    let c = key.contact.as_str();
    let digits = c.strip_prefix('+').unwrap_or(c);
    if c.contains('.') {
        Recipient::User(UserId::new(c))
    } else if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        Recipient::phone(format!("+{digits}"))
    } else {
        Recipient::group(c)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::macros::datetime;
    use wa_adapters::store::MemoryConversationStore;
    use wa_client::RetryPolicy;
    use wa_client::messages::Text;
    use wa_client::templates::TemplateMessage;
    use wa_core::clock::ManualClock;
    use wa_core::testing::ScriptedTransport;
    use wa_webhooks::{WebhookPayload, events};

    use super::*;
    use wa_core::Error;

    const PNID: &str = "106540352242922";

    fn payload(value: &serde_json::Value) -> Vec<WebhookEvent> {
        let body = json!({
            "object": "whatsapp_business_account",
            "entry": [{"id": "102290129340398", "changes": [{"field": "messages", "value": value}]}]
        });
        let p = WebhookPayload::from_slice(body.to_string().as_bytes()).unwrap();
        events(&p)
    }

    fn inbound(
        id: &str,
        ts: i64,
        user_id: Option<&str>,
        wa_id: Option<&str>,
        body: &str,
    ) -> Vec<WebhookEvent> {
        let mut contact = json!({"profile": {"name": "Sheena"}});
        let mut message =
            json!({"id": id, "timestamp": ts.to_string(), "type": "text", "text": {"body": body}});
        if let Some(u) = user_id {
            contact["user_id"] = json!(u);
            message["from_user_id"] = json!(u);
        }
        if let Some(w) = wa_id {
            contact["wa_id"] = json!(w);
            message["from"] = json!(w);
        }
        payload(&json!({
            "messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
            "contacts": [contact],
            "messages": [message]
        }))
    }

    fn status(id: &str, st: &str, ts: i64) -> Vec<WebhookEvent> {
        payload(&json!({
            "messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
            "statuses": [{"id": id, "status": st, "timestamp": ts.to_string(), "recipient_id": "16505551234"}]
        }))
    }

    async fn deliver_all(sink: &InboxSink, evs: Vec<WebhookEvent>) {
        assert!(!evs.is_empty(), "fixture produced no events");
        for e in evs {
            sink.deliver(e).await.unwrap();
        }
    }

    #[tokio::test]
    async fn records_inbound_by_bsuid_and_ignores_redelivery() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        let evs = inbound(
            "wamid.1",
            1_760_000_000,
            Some("US.13491208655302741918"),
            Some("16505551234"),
            "hi",
        );
        deliver_all(&sink, evs.clone()).await;
        deliver_all(&sink, evs).await; // Meta retry
        let key = ConversationKey::new(PNID, "US.13491208655302741918");
        let rows = store.messages(&key, None, 10).await.unwrap();
        assert_eq!(rows.len(), 1, "redelivery must not duplicate");
        assert_eq!(rows[0].direction, Direction::Inbound);
        assert_eq!(rows[0].kind, "text");
        assert_eq!(rows[0].text.as_deref(), Some("hi"));
        assert_eq!(rows[0].status, DeliveryStatus::Received);
        assert!(store.last_inbound_at(&key).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn falls_back_to_wa_id_and_skips_senderless_messages() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            inbound("wamid.2", 1_760_000_000, None, Some("16505551234"), "yo"),
        )
        .await;
        let rows = store
            .messages(&ConversationKey::new(PNID, "16505551234"), None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        // No identifiers at all: acknowledged, not recorded, not an error.
        deliver_all(&sink, inbound("wamid.3", 1_760_000_001, None, None, "?")).await;
        assert_eq!(
            store
                .conversations(&PNID.into(), None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn statuses_move_forward_only() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            inbound("wamid.4", 1_760_000_000, Some("US.1"), None, "hi"),
        )
        .await;
        deliver_all(&sink, status("wamid.4", "read", 1_760_000_100)).await;
        deliver_all(&sink, status("wamid.4", "delivered", 1_760_000_050)).await; // late
        let rows = store
            .messages(&ConversationKey::new(PNID, "US.1"), None, 1)
            .await
            .unwrap();
        assert_eq!(rows[0].status, DeliveryStatus::Read);
    }

    fn one_message(message: &serde_json::Value) -> Vec<WebhookEvent> {
        payload(&json!({
            "messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
            "contacts": [{"profile": {"name": "Sheena"}, "wa_id": "16505551234", "user_id": "US.1"}],
            "messages": [message]
        }))
    }

    #[tokio::test]
    async fn a_revoke_marks_the_original_deleted_instead_of_adding_a_row() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            inbound("wamid.orig", 1_749_854_000, Some("US.1"), None, "oops"),
        )
        .await;
        // Shape from webhooks/reference/messages/revoke.
        deliver_all(
            &sink,
            one_message(&json!({
                "from": "16505551234", "id": "wamid.rev", "timestamp": "1749854575",
                "type": "revoke", "revoke": {"original_message_id": "wamid.orig"}
            })),
        )
        .await;
        let rows = store
            .messages(&ConversationKey::new(PNID, "US.1"), None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "the revoke itself is not a message");
        assert_eq!(rows[0].status, DeliveryStatus::Deleted);
    }

    /// A store that refuses U+0000 in any text or JSON it is given, the way
    /// Postgres does (`TEXT` cannot hold NUL, `JSONB` rejects `\u0000`), and
    /// delegates everything else to the memory store. With `down`, every
    /// append fails (the database is unreachable).
    #[derive(Debug, Default)]
    struct NulRefusingStore {
        inner: MemoryConversationStore,
        down: bool,
    }

    fn has_nul(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::String(s) => s.contains('\0'),
            serde_json::Value::Array(a) => a.iter().any(has_nul),
            serde_json::Value::Object(o) => o.iter().any(|(k, v)| k.contains('\0') || has_nul(v)),
            _ => false,
        }
    }

    fn refuse() -> StorageError {
        StorageError::Backend(anyhow::anyhow!(
            "unsupported Unicode escape sequence: \\u0000 cannot be converted to text"
        ))
    }

    #[async_trait]
    impl ConversationStore for NulRefusingStore {
        async fn append(&self, m: StoredMessage) -> std::result::Result<bool, StorageError> {
            if self.down {
                return Err(StorageError::Backend(anyhow::anyhow!("connection refused")));
            }
            let texts = [Some(&m.kind), m.text.as_ref()];
            if texts.into_iter().flatten().any(|t| t.contains('\0'))
                || has_nul(&m.payload)
                || m.error.as_ref().is_some_and(has_nul)
            {
                return Err(refuse());
            }
            self.inner.append(m).await
        }
        async fn update_status(
            &self,
            phone_number_id: &PhoneNumberId,
            id: &MessageId,
            status: DeliveryStatus,
            at: OffsetDateTime,
            error: Option<serde_json::Value>,
        ) -> std::result::Result<bool, StorageError> {
            if error.as_ref().is_some_and(has_nul) {
                return Err(refuse());
            }
            self.inner
                .update_status(phone_number_id, id, status, at, error)
                .await
        }
        async fn messages(
            &self,
            key: &ConversationKey,
            before: Option<(OffsetDateTime, MessageId)>,
            limit: usize,
        ) -> std::result::Result<Vec<StoredMessage>, StorageError> {
            self.inner.messages(key, before, limit).await
        }
        async fn conversations(
            &self,
            phone_number_id: &PhoneNumberId,
            before: Option<(OffsetDateTime, String)>,
            limit: usize,
        ) -> std::result::Result<Vec<ConversationSummary>, StorageError> {
            self.inner
                .conversations(phone_number_id, before, limit)
                .await
        }
        async fn mark_read(&self, key: &ConversationKey) -> std::result::Result<(), StorageError> {
            self.inner.mark_read(key).await
        }
        async fn last_inbound_at(
            &self,
            key: &ConversationKey,
        ) -> std::result::Result<Option<OffsetDateTime>, StorageError> {
            self.inner.last_inbound_at(key).await
        }
    }

    /// Security review M1: one NUL in a customer's message made every
    /// delivery of the batch fail on Postgres, so Meta retried it for 7
    /// days and then dropped it, together with every other event in it.
    /// Stored text and payload now carry U+FFFD instead.
    #[tokio::test]
    async fn a_nul_in_customer_content_is_replaced_not_refused() {
        let store = Arc::new(NulRefusingStore::default());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            one_message(&json!({
                "from": "16505551234", "id": "wamid.nul", "timestamp": "1760000000",
                "type": "text", "text": {"body": "order\u{0}42"}
            })),
        )
        .await;
        // A type this crate does not know keeps its raw properties, keys
        // included, and its type name becomes the row's `kind`.
        deliver_all(
            &sink,
            one_message(&json!({
                "from": "16505551234", "id": "wamid.nul2", "timestamp": "1760000001",
                "type": "fut\u{0}ure", "fut\u{0}ure": {"k\u{0}": ["a\u{0}", {"deep\u{0}": "b\u{0}"}]}
            })),
        )
        .await;
        let status = payload(&json!({
            "messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
            "statuses": [{"id": "wamid.nul", "status": "failed", "timestamp": "1760000100",
                "recipient_id": "16505551234",
                "errors": [{"code": 131026, "title": "bad\u{0}", "error_data": {"details": "x\u{0}"}}]}]
        }));
        deliver_all(&sink, status).await;
        let rows = store
            .messages(&ConversationKey::new(PNID, "US.1"), None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2, "both messages recorded");
        let (unknown, text) = (&rows[0], &rows[1]);
        assert_eq!(text.text.as_deref(), Some("order\u{FFFD}42"));
        assert_eq!(text.payload["text"]["body"], "order\u{FFFD}42");
        assert_eq!(text.status, DeliveryStatus::Failed);
        let error = text.error.as_ref().unwrap();
        assert!(!has_nul(error), "{error}");
        assert_eq!(error[0]["title"], "bad\u{FFFD}");
        assert_eq!(unknown.kind, "fut\u{FFFD}ure");
        assert!(!has_nul(&unknown.payload), "{}", unknown.payload);
        assert_eq!(
            unknown.payload["fut\u{FFFD}ure"]["k\u{FFFD}"][1]["deep\u{FFFD}"], "b\u{FFFD}",
            "keys and nested values: {}",
            unknown.payload
        );
    }

    /// A `messages` change on another business number.
    fn on_number(pnid: &str, value: &serde_json::Value) -> Vec<WebhookEvent> {
        let mut value = value.clone();
        value["messaging_product"] = json!("whatsapp");
        value["metadata"] = json!({"display_phone_number": "15550000002", "phone_number_id": pnid});
        payload(&value)
    }

    /// Security review L1 (sec-probe `cross_number_probe`): statuses and
    /// revokes were applied by message id alone, so a status or a revoke
    /// delivered for one business number changed the row with that id on
    /// another number (e.g. marked a merchant's message `Deleted`).
    #[tokio::test]
    async fn statuses_and_revokes_on_another_number_change_nothing() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            inbound(
                "wamid.SHARED",
                1_760_000_000,
                Some("US.1"),
                None,
                "order 42",
            ),
        )
        .await;
        deliver_all(
            &sink,
            on_number(
                "PNID_B",
                &json!({
                    "contacts": [{"profile": {"name": "x"}, "wa_id": "16505551234", "user_id": "US.1"}],
                    "messages": [{"from": "16505551234", "from_user_id": "US.1", "id": "wamid.rev",
                        "timestamp": "1760000001", "type": "revoke",
                        "revoke": {"original_message_id": "wamid.SHARED"}}]
                }),
            ),
        )
        .await;
        deliver_all(
            &sink,
            on_number(
                "PNID_B",
                &json!({"statuses": [{"id": "wamid.SHARED", "status": "failed",
                    "timestamp": "1760000002", "recipient_id": "16505551234",
                    "errors": [{"code": 131026, "title": "Message undeliverable"}]}]}),
            ),
        )
        .await;
        let key = ConversationKey::new(PNID, "US.1");
        let row = store.messages(&key, None, 10).await.unwrap().remove(0);
        assert_eq!(row.status, DeliveryStatus::Received, "{row:?}");
        assert_eq!(row.error, None);
        // The same events on the right number do apply.
        deliver_all(&sink, status("wamid.SHARED", "read", 1_760_000_003)).await;
        let row = store.messages(&key, None, 10).await.unwrap().remove(0);
        assert_eq!(row.status, DeliveryStatus::Read);
        deliver_all(
            &sink,
            one_message(&json!({
                "from": "16505551234", "id": "wamid.rev2", "timestamp": "1760000004",
                "type": "revoke", "revoke": {"original_message_id": "wamid.SHARED"}
            })),
        )
        .await;
        let row = store.messages(&key, None, 10).await.unwrap().remove(0);
        assert_eq!(row.status, DeliveryStatus::Deleted);
    }

    #[tokio::test]
    async fn group_messages_are_keyed_by_the_group() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        // Shape from groups/groups-messaging (group_text fixture).
        deliver_all(
            &sink,
            one_message(&json!({
                "from": "16505551234", "group_id": "HBgLMTY1MDM4Nzk0MzkVAgASGBQ",
                "id": "wamid.g1", "timestamp": "1744344496",
                "text": {"body": "What does everyone think?"}, "type": "text"
            })),
        )
        .await;
        let group = ConversationKey::new(PNID, "HBgLMTY1MDM4Nzk0MzkVAgASGBQ");
        assert_eq!(store.messages(&group, None, 10).await.unwrap().len(), 1);
        assert!(
            store
                .messages(&ConversationKey::new(PNID, "US.1"), None, 10)
                .await
                .unwrap()
                .is_empty(),
            "a group message is not the sender's 1:1 conversation"
        );
        assert_eq!(
            recipient_for(&group),
            Recipient::group("HBgLMTY1MDM4Nzk0MzkVAgASGBQ")
        );
    }

    #[test]
    fn recipient_modes_follow_the_key() {
        let k = |c: &str| ConversationKey::new(PNID, c);
        assert_eq!(
            recipient_for(&k("US.13491208655302741918")),
            Recipient::user("US.13491208655302741918")
        );
        // A wa_id is sent with `+`: without it Meta prepends the business
        // number's country code and the reply reaches someone else.
        assert_eq!(
            recipient_for(&k("16505551234")),
            Recipient::phone("+16505551234")
        );
        assert_eq!(
            recipient_for(&k("+16505551234")),
            Recipient::phone("+16505551234")
        );
        assert_eq!(
            recipient_for(&k("Y2FwaV9ncm91cDox")),
            Recipient::group("Y2FwaV9ncm91cDox")
        );
    }

    fn inbox(
        t: &ScriptedTransport,
        store: Arc<MemoryConversationStore>,
        clock: &ManualClock,
    ) -> Inbox {
        let client = Client::builder()
            .transport(t.clone())
            .access_token("MERCHANT_TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        Inbox::new(client, PNID, store).with_clock(Arc::new(clock.clone()))
    }

    fn sent(id: &str) -> serde_json::Value {
        json!({"messaging_product": "whatsapp", "contacts": [{"input": "US.1", "user_id": "US.1"}], "messages": [{"id": id}]})
    }

    #[tokio::test]
    async fn reply_inside_the_window_sends_by_bsuid_and_records() {
        let store = Arc::new(MemoryConversationStore::new());
        let clock = ManualClock::new(datetime!(2025-10-09 08:00 UTC));
        let sink = InboxSink::new(store.clone());
        // 1_760_000_000 = 2025-10-09 08:53:20 UTC
        deliver_all(
            &sink,
            inbound("wamid.in", 1_760_000_000, Some("US.1"), None, "hi"),
        )
        .await;
        clock.set(datetime!(2025-10-09 10:00 UTC));
        let t = ScriptedTransport::new();
        t.push_json(200, sent("wamid.out"));
        let inbox = inbox(&t, store.clone(), &clock);
        let key = inbox.key("US.1");
        inbox
            .reply(
                &key,
                MessageContent::Text(Text {
                    body: "hello".into(),
                    preview_url: None,
                }),
            )
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.path(), format!("/v25.0/{PNID}/messages"));
        assert_eq!(req.bearer(), Some("MERCHANT_TOKEN"));
        let body = req.json().unwrap();
        assert_eq!(body["recipient"], "US.1");
        assert!(body.get("to").is_none());
        assert_eq!(t.remaining(), 0);
        let rows = inbox.history(&key, None, 10).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, MessageId::new("wamid.out"));
        assert_eq!(rows[0].direction, Direction::Outbound);
        assert_eq!(rows[0].status, DeliveryStatus::Accepted);
        assert_eq!(rows[0].text.as_deref(), Some("hello"));
    }

    /// The outbound side of the NUL rule: a reply whose text carries U+0000
    /// is recorded with U+FFFD, not refused (and not turned into an error).
    #[tokio::test]
    async fn an_outbound_reply_with_nul_is_recorded_with_the_replacement_character() {
        let store = Arc::new(NulRefusingStore::default());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            inbound("wamid.in", 1_760_000_000, Some("US.1"), None, "hi"),
        )
        .await;
        let clock = ManualClock::new(datetime!(2025-10-09 10:00 UTC));
        let t = ScriptedTransport::new();
        t.push_json(200, sent("wamid.out"));
        let client = Client::builder()
            .transport(t.clone())
            .access_token("MERCHANT_TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let inbox = Inbox::new(client, PNID, store.clone()).with_clock(Arc::new(clock));
        let key = inbox.key("US.1");
        inbox
            .reply(
                &key,
                MessageContent::Text(Text {
                    body: "a\0b".into(),
                    preview_url: None,
                }),
            )
            .await
            .unwrap();
        let rows = inbox.history(&key, None, 10).await.unwrap();
        let out = rows
            .iter()
            .find(|r| r.id == MessageId::new("wamid.out"))
            .expect("the reply is recorded");
        assert_eq!(out.text.as_deref(), Some("a\u{FFFD}b"));
        assert_eq!(out.payload["text"]["body"], "a\u{FFFD}b");
    }

    /// Conventions review #18: once Meta accepted a message, nothing about
    /// recording it may turn the send into an error — the caller would
    /// retry and the customer would get it twice.
    #[tokio::test]
    async fn a_recording_failure_after_the_send_is_not_the_callers_error() {
        let store = Arc::new(NulRefusingStore {
            down: true,
            ..NulRefusingStore::default()
        });
        let clock = ManualClock::new(datetime!(2025-10-09 08:00 UTC));
        let t = ScriptedTransport::new();
        t.push_json(200, sent("wamid.out"));
        let client = Client::builder()
            .transport(t.clone())
            .access_token("MERCHANT_TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let inbox = Inbox::new(client, PNID, store.clone()).with_clock(Arc::new(clock));
        let key = inbox.key("US.1");
        let message = OutboundMessage::template(
            inbox.recipient(&key),
            TemplateMessage::new("order_update", "en_US"),
        );
        let response = inbox.send(&key, message).await.unwrap();
        assert_eq!(response.message_id(), Some(&MessageId::new("wamid.out")));
        assert_eq!(t.requests().len(), 1, "sent exactly once");
        assert!(
            store
                .inner
                .messages(&key, None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn free_form_outside_the_window_is_refused_but_templates_go() {
        let store = Arc::new(MemoryConversationStore::new());
        let clock = ManualClock::new(datetime!(2025-10-09 08:00 UTC));
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            inbound("wamid.in", 1_760_000_000, Some("US.1"), None, "hi"),
        )
        .await;
        clock.set(datetime!(2025-10-10 09:00 UTC)); // > 24h later
        let t = ScriptedTransport::new();
        let inbox = inbox(&t, store.clone(), &clock);
        let key = inbox.key("US.1");
        let err = inbox
            .reply(
                &key,
                MessageContent::Text(Text {
                    body: "late".into(),
                    preview_url: None,
                }),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.is_customer_service_window_closed()),
            "{err}"
        );
        assert_eq!(err.kind(), wa_core::ErrorKind::CustomerServiceWindowClosed);
        assert!(t.requests().is_empty(), "refused before any request");

        t.push_json(200, sent("wamid.tpl"));
        inbox
            .reply(
                &key,
                MessageContent::Template(TemplateMessage::new("order_update", "en_US")),
            )
            .await
            .unwrap();
        assert_eq!(t.requests().len(), 1);
        // Never messaged at all: closed too.
        let err = inbox
            .reply(
                &inbox.key("US.2"),
                MessageContent::Text(Text {
                    body: "x".into(),
                    preview_url: None,
                }),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
    }

    #[tokio::test]
    async fn send_refuses_a_message_addressed_outside_the_conversation() {
        let store = Arc::new(MemoryConversationStore::new());
        let clock = ManualClock::new(datetime!(2025-10-09 08:00 UTC));
        let t = ScriptedTransport::new();
        let inbox = inbox(&t, store, &clock);
        let key = inbox.key("US.1");
        let stranger = OutboundMessage::template(
            Recipient::phone("+15550000000"),
            TemplateMessage::new("promo", "en_US"),
        );
        let err = inbox.send(&key, stranger).await.unwrap_err();
        assert!(matches!(&err, Error::Validation(v) if v.field == "recipient"));
        assert!(t.requests().is_empty());
        t.push_json(200, sent("wamid.ok"));
        let ok = OutboundMessage::template(
            inbox.recipient(&key),
            TemplateMessage::new("promo", "en_US"),
        );
        inbox.send(&key, ok).await.unwrap();
        assert_eq!(t.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_wa_id_conversation_is_answered_with_a_plus() {
        let store = Arc::new(MemoryConversationStore::new());
        let clock = ManualClock::new(datetime!(2025-10-09 08:00 UTC));
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            inbound("wamid.in", 1_760_000_000, None, Some("16505551234"), "hi"),
        )
        .await;
        clock.set(datetime!(2025-10-09 10:00 UTC));
        let t = ScriptedTransport::new();
        t.push_json(200, sent("wamid.out"));
        let inbox = inbox(&t, store, &clock);
        inbox
            .reply(
                &inbox.key("16505551234"),
                MessageContent::Text(Text {
                    body: "hello".into(),
                    preview_url: None,
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().json().unwrap()["to"],
            "+16505551234"
        );
    }

    #[tokio::test]
    async fn a_conversation_of_another_number_is_rejected() {
        let store = Arc::new(MemoryConversationStore::new());
        let clock = ManualClock::new(datetime!(2025-10-09 08:00 UTC));
        let t = ScriptedTransport::new();
        let inbox = inbox(&t, store, &clock);
        let foreign = ConversationKey::new("999", "US.1");
        assert!(inbox.history(&foreign, None, 10).await.is_err());
        assert!(
            inbox
                .reply(
                    &foreign,
                    MessageContent::Template(TemplateMessage::new("x", "en_US"))
                )
                .await
                .is_err()
        );
        assert!(t.requests().is_empty());
    }
}
