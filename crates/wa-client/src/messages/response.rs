//! The send response (`messages/send-messages`, "Responses";
//! `business-scoped-user-ids`, "Send message response").
//!
//! A 200 only means Meta **accepted** the request; delivery arrives later
//! as `messages` status webhooks keyed by [`SentMessage::id`].

use serde::Deserialize;
use wa_core::ids::{GroupId, MessageId, UserId, WaId};

/// Response of `POST /{phone_number_id}/messages`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SendResponse {
    /// Always `whatsapp`. Some older samples omit it.
    #[serde(default)]
    pub messaging_product: String,
    /// The addressee as Meta resolved it.
    #[serde(default)]
    pub contacts: Vec<SentContact>,
    /// The accepted message(s).
    #[serde(default)]
    pub messages: Vec<SentMessage>,
}

impl SendResponse {
    /// Id of the (first) accepted message, the key of its status webhooks.
    pub fn message_id(&self) -> Option<&MessageId> {
        self.messages.first().map(|m| &m.id)
    }
}

/// `contacts[]` entry.
///
/// Since usernames: sent to a phone number → `input` is that number and
/// `wa_id` is set; sent to a BSUID → `input` and `user_id` are the BSUID and
/// `wa_id` is absent; sent to both → the phone number wins. Sent to a group
/// → `input` is the group id.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SentContact {
    /// What the request addressed (phone number, BSUID or group id).
    #[serde(default)]
    pub input: String,
    /// The user's WhatsApp id; only when addressed by phone number.
    #[serde(default)]
    pub wa_id: Option<WaId>,
    /// The user's BSUID; only when addressed by BSUID.
    #[serde(default)]
    pub user_id: Option<UserId>,
    /// The parent BSUID, for portfolios enrolled in parent BSUIDs
    /// (`calling/user-call-permissions` shows it in a send response).
    #[serde(default)]
    pub parent_user_id: Option<UserId>,
}

/// `messages[]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SentMessage {
    /// The new message's `wamid`.
    pub id: MessageId,
    /// Present when the message went to a group.
    #[serde(default)]
    pub group_id: Option<GroupId>,
    /// Template pacing status; present for templates.
    #[serde(default)]
    pub message_status: Option<MessageStatus>,
}

/// `message_status` (`templates/template-pacing`; values from the message
/// API reference).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MessageStatus {
    /// Accepted and being processed.
    Accepted,
    /// Held while Meta assesses the template's quality (pacing).
    HeldForQualityAssessment,
    /// Delivery paused.
    Paused,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}
