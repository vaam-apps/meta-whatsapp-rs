//! Conversation history port, for in-app chat (a merchant's inbox).

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::StorageError;
use crate::ids::{MessageId, PhoneNumberId};

/// One conversation: a business phone number and one contact.
///
/// `contact` is the business-scoped user id when known (Meta includes
/// `user_id` in every messages webhook since April 2026), otherwise the
/// `wa_id`. Keep it stable per contact: key by BSUID once you have one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConversationKey {
    /// The business phone number the conversation happens on.
    pub phone_number_id: PhoneNumberId,
    /// BSUID, or `wa_id` when no BSUID is known.
    pub contact: String,
}

impl ConversationKey {
    /// Build a key.
    pub fn new(phone_number_id: impl Into<PhoneNumberId>, contact: impl Into<String>) -> Self {
        Self {
            phone_number_id: phone_number_id.into(),
            contact: contact.into(),
        }
    }
}

impl fmt::Display for ConversationKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.phone_number_id, self.contact)
    }
}

/// Which way a message went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// From the WhatsApp user to the business.
    Inbound,
    /// From the business to the WhatsApp user.
    Outbound,
}

/// Lifecycle of a message.
///
/// Status webhooks can arrive out of order (a `delivered` after its `read`);
/// [`DeliveryStatus::supersedes`] is the rule adapters apply so a late
/// webhook never moves a message backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DeliveryStatus {
    /// Inbound message, recorded on receipt.
    Received,
    /// Outbound, accepted by the API, no webhook yet.
    Accepted,
    /// `sent` webhook.
    Sent,
    /// `delivered` webhook.
    Delivered,
    /// `read` webhook.
    Read,
    /// `played` webhook (voice messages).
    Played,
    /// `failed` webhook.
    Failed,
    /// Deleted for everyone by its sender, the customer or the business
    /// (a revoke: [`ConversationStore::revoke`]).
    Deleted,
}

impl DeliveryStatus {
    fn rank(self) -> u8 {
        match self {
            Self::Received | Self::Accepted => 0,
            Self::Sent => 1,
            Self::Delivered => 2,
            Self::Read => 3,
            Self::Played => 4,
            // Terminal states win over any progress state.
            Self::Failed | Self::Deleted => 5,
        }
    }

    /// Whether moving from `current` to `self` is progress (and should be
    /// stored). Equal ranks do not supersede, which makes replays no-ops.
    pub fn supersedes(self, current: Self) -> bool {
        self.rank() > current.rank()
    }

    /// Parse the `status` string of a status webhook.
    pub fn from_webhook(s: &str) -> Option<Self> {
        Some(match s {
            "sent" => Self::Sent,
            "delivered" => Self::Delivered,
            "read" => Self::Read,
            "played" => Self::Played,
            "failed" => Self::Failed,
            "deleted" => Self::Deleted,
            _ => return None,
        })
    }
}

/// A persisted message.
///
/// Content (`kind`, `text`, `payload`, `error`) is kept exactly as given,
/// U+0000 included; identifiers (`id` and the `conversation`'s ids) must
/// not contain U+0000, which a store may refuse there (see
/// [`ConversationStore`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredMessage {
    /// WhatsApp message id; unique. Appending an existing id is a no-op.
    pub id: MessageId,
    /// Conversation it belongs to.
    pub conversation: ConversationKey,
    /// Direction.
    pub direction: Direction,
    /// Message type as Meta names it (`text`, `image`, `template`,
    /// `media_placeholder`, …), except [`StoredMessage::REVOKED`], which is
    /// this crate's own: the tombstone of a revoke that arrived before its
    /// message.
    pub kind: String,
    /// Plain-text rendering for list previews and search, when there is one.
    pub text: Option<String>,
    /// The full message JSON (webhook message object, or send request body).
    pub payload: serde_json::Value,
    /// Current status.
    pub status: DeliveryStatus,
    /// When the message was sent/received (Meta's timestamp when known).
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    /// When `status` last changed.
    #[serde(with = "time::serde::rfc3339::option")]
    pub status_at: Option<OffsetDateTime>,
    /// Error object from a `failed` status, verbatim.
    pub error: Option<serde_json::Value>,
}

impl StoredMessage {
    /// The `kind` of a media message in a coexistence history sync
    /// (`webhooks/reference/history`): Meta sends it without its media and
    /// sends the content in a later `history` webhook, which
    /// [`ConversationStore::fill_media_placeholder`] records into it.
    pub const MEDIA_PLACEHOLDER: &'static str = "media_placeholder";

    /// The `kind` of the row [`ConversationStore::revoke`] stores for a
    /// revoke whose message is not stored (yet): it holds the message's id,
    /// the revoke's conversation and direction, no text, an empty JSON
    /// object as payload (`{}`), and status [`DeliveryStatus::Deleted`].
    /// Not a Meta message type: this crate's own. It is part of the history
    /// ([`ConversationStore::messages`]), never of the inbox summary.
    pub const REVOKED: &'static str = "revoked";

    /// The tombstone [`ConversationStore::revoke`] stores for message `id`
    /// of conversation `key` when the revoke arrives first: kind
    /// [`Self::REVOKED`], no text, an empty JSON object as payload (`{}`),
    /// status [`DeliveryStatus::Deleted`], timestamped (and deleted) at
    /// `at`.
    pub fn tombstone(
        key: &ConversationKey,
        id: &MessageId,
        direction: Direction,
        at: OffsetDateTime,
    ) -> Self {
        Self {
            id: id.clone(),
            conversation: key.clone(),
            direction,
            kind: Self::REVOKED.to_owned(),
            text: None,
            payload: serde_json::Value::Object(serde_json::Map::new()),
            status: DeliveryStatus::Deleted,
            timestamp: at,
            status_at: Some(at),
            error: None,
        }
    }
}

/// Inbox row.
///
/// Built from the messages stored with [`ConversationStore::append`] and
/// [`ConversationStore::append_synced`]; a revoke's tombstone
/// ([`StoredMessage::REVOKED`]) never counts: it moves nothing here, and a
/// conversation whose only rows are tombstones has no summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationSummary {
    /// Conversation.
    pub key: ConversationKey,
    /// Latest message of either direction.
    #[serde(with = "time::serde::rfc3339")]
    pub last_message_at: OffsetDateTime,
    /// Latest inbound message recorded with [`ConversationStore::append`];
    /// drives the customer service window. Synced history
    /// ([`ConversationStore::append_synced`]) and tombstones never move it.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_inbound_at: Option<OffsetDateTime>,
    /// Text preview of the latest message.
    pub last_text: Option<String>,
    /// Inbound messages recorded with [`ConversationStore::append`] since
    /// the last [`ConversationStore::mark_read`], counted by arrival: a late
    /// webhook for an older message still counts. Synced history
    /// ([`ConversationStore::append_synced`]) never does.
    pub unread: u64,
}

/// Paged, ordered message history.
///
/// Ordering is by `(timestamp, id)` descending — newest first — and the
/// `before` cursor is exclusive, so paging never repeats or skips a row even
/// when timestamps collide.
///
/// Content round-trips exactly through every method: a message's `kind`,
/// `text`, `payload` (strings and object keys) and `error`, and a summary's
/// `last_text`, read back as written, U+0000 included — never replaced
/// (U+FFFD) or dropped, and two object keys differing only by one stay
/// two keys. Identifiers (message ids, contacts, phone number ids) never
/// contain U+0000 in what Meta sends; a store may refuse one that does
/// with an error (the Postgres adapter does), but never store it as
/// another id. The conformance suite
/// (`wa_adapters::store::conversation_conformance`) checks the content
/// rule.
#[async_trait]
pub trait ConversationStore: Send + Sync + fmt::Debug + 'static {
    /// Insert a message. Returns `false` (and changes nothing) if a message
    /// with the same id exists — webhook retries make this common. An
    /// inbound message moves the conversation's `last_inbound_at` (the
    /// customer service window) to its timestamp if later, and counts as
    /// unread.
    async fn append(&self, message: StoredMessage) -> Result<bool, StorageError>;

    /// Insert messages synchronized from the WhatsApp Business app
    /// (coexistence history), as one batch: each like
    /// [`append`](Self::append) — the same id rule, history order and
    /// conversation `last_message_at` / `last_text` — except that an
    /// inbound one neither moves `last_inbound_at` nor counts as unread.
    /// Meta opens no customer service window for a message sent before the
    /// business was onboarded (`embedded-signup/onboarding-business-app-users`,
    /// "Customer service window"), and the merchant has seen these messages
    /// in the app.
    ///
    /// Returns, for each message in order, whether it was inserted: `false`
    /// for an id already stored, or already earlier in the batch. The id
    /// rule spans both methods: a message stored by one is never changed by
    /// the other. A history webhook can carry thousands of messages: store
    /// the batch in as few round trips as the backend allows (the Postgres
    /// adapter uses one statement).
    async fn append_synced(&self, messages: Vec<StoredMessage>) -> Result<Vec<bool>, StorageError>;

    /// Record the content of message `id` of business number
    /// `phone_number_id` if it is stored as a
    /// [media placeholder](StoredMessage::MEDIA_PLACEHOLDER) that is not
    /// [`DeliveryStatus::Deleted`]: its `kind`, `text` and `payload` become
    /// the given ones, and so does the conversation's `last_text` when it
    /// is the latest message. Its conversation, direction, status,
    /// timestamps and error stay, and so do `last_inbound_at` and the
    /// unread count. Returns whether it changed: `false` when no message
    /// `id` is stored for that number, it is not (or no longer) a
    /// placeholder, so a redelivered content changes nothing, or it was
    /// revoked: the content its sender deleted is never stored.
    async fn fill_media_placeholder(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        kind: String,
        text: Option<String>,
        payload: serde_json::Value,
    ) -> Result<bool, StorageError>;

    /// Apply a revoke (its sender deleted message `id` for everyone,
    /// [`webhooks/reference/messages/revoke`](https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/reference/messages/revoke))
    /// that arrived on business number `key.phone_number_id`: the message,
    /// if stored for that number **and** in `direction` (a customer revokes
    /// what they sent, the business what it sent), becomes
    /// [`DeliveryStatus::Deleted`] at `at` when that
    /// [supersedes](DeliveryStatus::supersedes) its status. Its content
    /// stays: `text` and `payload` are never erased, so the merchant keeps
    /// a record of what was deleted (the owner's decision, 2026-09-25). A
    /// message `id` of another number or of the other direction is not
    /// touched.
    ///
    /// The revoke does **not** match the message's conversation
    /// (`key.contact`; the owner's decision, 2026-09-25): a message stored
    /// under another conversation of the number is deleted all the same.
    /// The two keys can differ for one customer: a message recorded under
    /// the phone number (a history thread without a BSUID) and revoked by a
    /// live webhook keyed by the BSUID, or a customer whose BSUID changed
    /// with their number. So nothing but the number and the direction ties
    /// a revoke to its message. The conformance suite checks it.
    ///
    /// A revoke can arrive before its message (a live revoke, then the
    /// history chunk that carries the original; or redeliveries out of
    /// order). When no message `id` is stored at all, a tombstone takes its
    /// place: [`StoredMessage::tombstone`], a row of kind
    /// [`StoredMessage::REVOKED`] in conversation `key` with `direction`,
    /// no text, `{}` as payload, status `Deleted`, timestamped `at`. It is
    /// in the conversation's history ([`messages`](Self::messages)) but
    /// never in its summary: it moves neither `last_message_at`,
    /// `last_text`, `last_inbound_at` nor the unread count, and a
    /// conversation with no other row gets no summary
    /// ([`conversations`](Self::conversations) does not list it). The
    /// message then finds its id taken, so the content its sender deleted
    /// is never stored.
    ///
    /// Returns whether anything changed (a message deleted, or a tombstone
    /// stored).
    async fn revoke(
        &self,
        key: &ConversationKey,
        id: &MessageId,
        direction: Direction,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError>;

    /// Apply a status update to message `id` of business number
    /// `phone_number_id` if it [supersedes](DeliveryStatus::supersedes) the
    /// stored one. Returns whether anything changed; `false` also when no
    /// message `id` is stored for that number (a status for a message sent
    /// elsewhere, or delivered for another business number: a message is
    /// only ever changed by events of its own number). When applied,
    /// `error: None` keeps any error already stored.
    async fn update_status(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<serde_json::Value>,
    ) -> Result<bool, StorageError>;

    /// Messages of one conversation, newest first, strictly older than
    /// `before` (a `(timestamp, id)` cursor taken from the last row of the
    /// previous page).
    async fn messages(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, MessageId)>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StorageError>;

    /// Conversations on a business number, most recent activity first,
    /// strictly older than `before` (a `(last_message_at, contact)` cursor).
    /// See [`ConversationSummary`] for what a summary counts.
    async fn conversations(
        &self,
        phone_number_id: &PhoneNumberId,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>, StorageError>;

    /// Reset the unread count of a conversation.
    async fn mark_read(&self, key: &ConversationKey) -> Result<(), StorageError>;

    /// The conversation's [`ConversationSummary::last_inbound_at`]: the
    /// timestamp of the latest inbound message recorded with
    /// [`append`](Self::append), if any. Synced history
    /// ([`append_synced`](Self::append_synced)) and tombstones
    /// ([`StoredMessage::REVOKED`]) never count.
    async fn last_inbound_at(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<OffsetDateTime>, StorageError>;
}

/// The 24-hour customer service window.
///
/// Free-form (non-template) messages are only allowed within 24 hours of the
/// user's last message; outside it Meta rejects them with `131047`. Check
/// here first and send a template instead of paying for a failed request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomerServiceWindow {
    last_inbound_at: Option<OffsetDateTime>,
}

impl CustomerServiceWindow {
    /// Length of the window.
    pub const LENGTH: Duration = Duration::from_hours(24);

    /// Window opened by the user's last inbound message.
    pub fn from_last_inbound(last_inbound_at: Option<OffsetDateTime>) -> Self {
        Self { last_inbound_at }
    }

    /// When the window closes, if it was ever opened.
    pub fn closes_at(&self) -> Option<OffsetDateTime> {
        self.last_inbound_at.map(|t| t + Self::LENGTH)
    }

    /// Whether free-form messages are allowed at `now`.
    pub fn is_open(&self, now: OffsetDateTime) -> bool {
        self.closes_at().is_some_and(|close| now < close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn statuses_never_regress_and_replays_are_noops() {
        use DeliveryStatus as S;
        assert!(S::Delivered.supersedes(S::Sent));
        assert!(!S::Delivered.supersedes(S::Read));
        assert!(!S::Read.supersedes(S::Read));
        assert!(S::Failed.supersedes(S::Sent));
        assert!(!S::Sent.supersedes(S::Failed));
        assert_eq!(S::from_webhook("read"), Some(S::Read));
        assert_eq!(S::from_webhook("warning"), None);
    }

    #[test]
    fn window_is_open_for_exactly_24_hours() {
        let t = datetime!(2026-09-24 10:00 UTC);
        let w = CustomerServiceWindow::from_last_inbound(Some(t));
        assert!(w.is_open(datetime!(2026-09-25 09:59:59 UTC)));
        assert!(!w.is_open(datetime!(2026-09-25 10:00 UTC)));
        assert!(!CustomerServiceWindow::from_last_inbound(None).is_open(t));
    }
}
