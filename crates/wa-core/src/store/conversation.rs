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
    /// The user deleted/revoked the message.
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredMessage {
    /// WhatsApp message id; unique. Appending an existing id is a no-op.
    pub id: MessageId,
    /// Conversation it belongs to.
    pub conversation: ConversationKey,
    /// Direction.
    pub direction: Direction,
    /// Message type as Meta names it: `text`, `image`, `template`, …
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

/// Inbox row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationSummary {
    /// Conversation.
    pub key: ConversationKey,
    /// Latest message of either direction.
    #[serde(with = "time::serde::rfc3339")]
    pub last_message_at: OffsetDateTime,
    /// Latest inbound message; drives the customer service window.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_inbound_at: Option<OffsetDateTime>,
    /// Text preview of the latest message.
    pub last_text: Option<String>,
    /// Inbound messages appended since the last [`ConversationStore::mark_read`],
    /// counted by arrival: a late webhook for an older message still counts.
    pub unread: u64,
}

/// Paged, ordered message history.
///
/// Ordering is by `(timestamp, id)` descending — newest first — and the
/// `before` cursor is exclusive, so paging never repeats or skips a row even
/// when timestamps collide.
#[async_trait]
pub trait ConversationStore: Send + Sync + fmt::Debug + 'static {
    /// Insert a message. Returns `false` (and changes nothing) if a message
    /// with the same id exists — webhook retries make this common.
    async fn append(&self, message: StoredMessage) -> Result<bool, StorageError>;

    /// Apply a status update if it [supersedes](DeliveryStatus::supersedes)
    /// the stored one. Returns whether anything changed; `false` also when
    /// the message is unknown (a status for a message sent elsewhere).
    /// When applied, `error: None` keeps any error already stored.
    async fn update_status(
        &self,
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
    async fn conversations(
        &self,
        phone_number_id: &PhoneNumberId,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>, StorageError>;

    /// Reset the unread count of a conversation.
    async fn mark_read(&self, key: &ConversationKey) -> Result<(), StorageError>;

    /// Timestamp of the latest inbound message, if any.
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
