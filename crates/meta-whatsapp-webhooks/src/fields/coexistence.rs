//! Coexistence fields: WhatsApp Business app users onboarded to Cloud API.
//!
//! Doc paths: `webhooks/reference/history`,
//! `webhooks/reference/smb_app_state_sync`,
//! `webhooks/reference/smb_message_echoes`,
//! `embedded-signup/onboarding-business-app-users`, and the coexistence
//! section of `business-scoped-user-ids`.
//!
//! A `history` change carries either chat-history chunks (`history[]`), the
//! content of media messages announced earlier as `media_placeholder`
//! (`messages[]` / `message_echoes[]`), or a `history[].errors` entry when
//! the business declined to share its history (error `2593109`).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use meta_whatsapp_core::GraphApiError;
use meta_whatsapp_core::ids::{MessageId, UserId, WaId};

use super::common::{Contact, Metadata};
use super::messages::{InboundMessage, MessageContent};
use crate::open_enum::open_enum;

/// `value` of an `smb_message_echoes` change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmbMessageEchoesValue {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// The business phone number.
    pub metadata: Metadata,
    /// The recipients of the echoed messages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<Contact>,
    /// Messages the business sent from the WhatsApp Business app or a
    /// companion device.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_echoes: Vec<MessageEcho>,
}

/// A message the business sent outside Cloud API (`message_echoes[]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageEcho {
    /// Business display phone number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Recipient phone number; omitted when Meta may not share it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<WaId>,
    /// Recipient BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_user_id: Option<UserId>,
    /// Recipient parent BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_parent_user_id: Option<UserId>,
    /// Message id.
    pub id: MessageId,
    /// When the webhook was triggered.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub timestamp: OffsetDateTime,
    /// `type` and payload (`text`, `image`, `video`, `document`, `revoke`,
    /// `edit` per the page; any other type is kept as unknown).
    #[serde(flatten)]
    pub content: MessageContent,
}

/// `value` of a `history` change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryValue {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// The business customer's phone number.
    pub metadata: Metadata,
    /// Users referenced by `messages` / `message_echoes` (media content webhooks).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<Contact>,
    /// History chunks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryChunk>,
    /// Content of media messages users sent (follows a `media_placeholder`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<InboundMessage>,
    /// Content of media messages the business sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_echoes: Vec<MessageEcho>,
}

/// `history[]`: one chunk of synchronized chat history.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryChunk {
    /// Phase / chunk / progress; absent on the declined-sharing error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HistoryMetadata>,
    /// Chat threads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<HistoryThread>,
    /// Set instead of `threads` when sharing was declined (code `2593109`).
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::graph_errors::deserialize",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub errors: Vec<GraphApiError>,
}

/// `history[].metadata`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryMetadata {
    /// `0` = day 0–1, `1` = day 1–90, `2` = day 90–180.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<i64>,
    /// Chunk number within the phase; chunks may arrive out of order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_order: Option<i64>,
    /// Overall progress, `0`–`100`; `100` means synchronization is complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<i64>,
}

/// `history[].threads[]`: one chat.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryThread {
    /// The user's phone number; omitted when Meta may not share it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The user's identifiers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ThreadContext>,
    /// Messages, both directions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<HistoryMessage>,
}

/// `threads[].context`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadContext {
    /// Phone number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wa_id: Option<WaId>,
    /// BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<UserId>,
    /// Parent BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_user_id: Option<UserId>,
    /// Username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// `threads[].messages[]`: a message in either direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryMessage {
    /// Business number (outgoing) or user number (incoming).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Sender BSUID (incoming).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_user_id: Option<UserId>,
    /// Sender parent BSUID (incoming).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_parent_user_id: Option<UserId>,
    /// Recipient number (outgoing messages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Message id.
    pub id: MessageId,
    /// When the recipient's device received it.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub timestamp: OffsetDateTime,
    /// Latest delivery status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_context: Option<HistoryContext>,
    /// `type` and payload; `media_placeholder` for media sent before the
    /// separate media webhook.
    #[serde(flatten)]
    pub content: MessageContent,
}

open_enum! {
    /// `history_context.status`.
    pub enum HistoryMessageStatus {
        /// Delivered.
        Delivered => "DELIVERED",
        /// Error.
        Error => "ERROR",
        /// Pending.
        Pending => "PENDING",
        /// Played.
        Played => "PLAYED",
        /// Read.
        Read => "READ",
        /// Sent.
        Sent => "SENT",
    }
}

/// `messages[].history_context`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryContext {
    /// Latest delivery status.
    pub status: HistoryMessageStatus,
}

/// `value` of an `smb_app_state_sync` change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmbAppStateSyncValue {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// The business phone number.
    pub metadata: Metadata,
    /// Address book changes.
    #[serde(default)]
    pub state_sync: Vec<StateSyncItem>,
}

open_enum! {
    /// `state_sync[].type`.
    pub enum StateSyncType {
        /// An address book contact.
        Contact => "contact",
    }
}

open_enum! {
    /// `state_sync[].action`.
    pub enum StateSyncAction {
        /// Added or edited.
        Add => "add",
        /// Removed.
        Remove => "remove",
    }
}

/// `state_sync[]`: one address book change in the WhatsApp Business app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSyncItem {
    /// What is synced.
    #[serde(rename = "type")]
    pub sync_type: StateSyncType,
    /// The contact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<SyncedContact>,
    /// Added/edited or removed.
    pub action: StateSyncAction,
    /// When.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<StateSyncMetadata>,
}

/// `state_sync[].contact`. Names are omitted on removals.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncedContact {
    /// Full name in the business's address book.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_name: Option<String>,
    /// First name in the business's address book.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    /// Phone number; omitted when Meta may not share it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
    /// BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<UserId>,
    /// Parent BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_user_id: Option<UserId>,
    /// Username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// `state_sync[].metadata`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSyncMetadata {
    /// When the webhook was triggered.
    #[serde(
        default,
        with = "meta_whatsapp_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub timestamp: Option<OffsetDateTime>,
}
