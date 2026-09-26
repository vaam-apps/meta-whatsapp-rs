//! Conversation history port, for in-app chat (a merchant's inbox).
//!
//! Besides the history ([`StoredMessage`]) and the inbox rows
//! ([`ConversationSummary`]), a [`ConversationStore`] keeps what the inbox
//! needs about a conversation that is not a message: [window
//! events](WindowEvent) (a call, a standby message: they reopen the
//! customer service window), [thread ownership](ThreadOwnership) under
//! Conversation Routing, the coexistence address book
//! ([`StoredContact`]) and the links between a person's identities
//! ([`IdentityLink`]). It finds a person's identities on a number
//! ([`ConversationStore::identities`]), erases them
//! ([`ConversationStore::erase_all`], [`ErasureMode`]) and purges by age
//! ([`ConversationStore::purge_before`], [`Retention`]).

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::StorageError;
use crate::ids::{AppId, MessageId, PhoneNumberId, UserId};

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
    /// `media_placeholder`, …), except [`StoredMessage::REVOKED`] and
    /// [`StoredMessage::ERASED`], this crate's own: the tombstone of a
    /// revoke that arrived before its message, and an erased person's
    /// message redacted in a group's history.
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

    /// The `kind` of a message whose sender was erased while it stays in
    /// another conversation's history (a group's), under
    /// [`ErasureMode::Redact`]: [`redact`](Self::redact) gives it this
    /// kind, no text, an empty JSON object as payload (`{}`) and no error.
    /// Not a Meta message type: this crate's own, kept by the stores, so
    /// it never changes.
    pub const ERASED: &'static str = "erased";

    /// Who sent this message, as an [erasure](ConversationStore::erase_all)
    /// matches it in conversations keyed by someone else (a group's): for
    /// an inbound message, the payload's `from_user_id` (the sender's
    /// BSUID), else its `from` (their phone number), when a non-empty
    /// string without U+0000 (Meta assigns none); `None` for an outbound
    /// message (its `from`, when there is one, is the business).
    ///
    /// A store keeps it beside the message when it is appended (the
    /// Postgres adapter in `wa_messages.sender`); a later
    /// [`fill_media_placeholder`](ConversationStore::fill_media_placeholder)
    /// does not change it.
    pub fn sender(&self) -> Option<&str> {
        if self.direction != Direction::Inbound {
            return None;
        }
        ["from_user_id", "from"].into_iter().find_map(|field| {
            self.payload
                .get(field)?
                .as_str()
                .filter(|s| !s.is_empty() && !s.contains('\0'))
        })
    }

    /// Redact this message in place, as [`ErasureMode::Redact`] does to an
    /// erased person's message in someone else's conversation: its content
    /// goes (`kind` becomes [`Self::ERASED`], no text, `{}` as payload, no
    /// error), and with it its [`sender`](Self::sender); its id,
    /// conversation, direction, status and times stay, so it keeps its
    /// place in the history.
    pub fn redact(&mut self) {
        Self::ERASED.clone_into(&mut self.kind);
        self.text = None;
        self.payload = serde_json::Value::Object(serde_json::Map::new());
        self.error = None;
    }

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

/// Why a [`WindowEvent`] opened or refreshed the customer service window.
///
/// Stored by name ([`as_str`](Self::as_str)): a store keeps the name, so
/// the names never change. [`Other`](Self::Other) carries any other name,
/// for a reason this crate does not name yet; parsing a known name never
/// gives `Other`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WindowEventKind {
    /// The customer called the business number, answered or not
    /// (`calling/pricing`, "How calling changes the 24 hour customer
    /// service window"). `customer_call`.
    CustomerCall,
    /// The customer accepted a call from the business (same page).
    /// `call_accepted`.
    CallAccepted,
    /// A customer's message observed in standby under Conversation Routing
    /// (`webhooks/reference/standby`): it refreshes the window, but it is
    /// not the business's to answer, nor unread (`OPEN_QUESTIONS.md` #44).
    /// `standby_message`.
    StandbyMessage,
    /// Any other name.
    Other(String),
}

impl WindowEventKind {
    /// The stored name.
    pub fn as_str(&self) -> &str {
        match self {
            Self::CustomerCall => "customer_call",
            Self::CallAccepted => "call_accepted",
            Self::StandbyMessage => "standby_message",
            Self::Other(name) => name,
        }
    }

    /// The kind a stored name stands for.
    pub fn from_name(name: &str) -> Self {
        match name {
            "customer_call" => Self::CustomerCall,
            "call_accepted" => Self::CallAccepted,
            "standby_message" => Self::StandbyMessage,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl fmt::Display for WindowEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for WindowEventKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WindowEventKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(Self::from_name(&name))
    }
}

/// Something that opened or refreshed a conversation's 24-hour customer
/// service window without being a message of its history: a call, or a
/// message observed in standby ([`WindowEventKind`]).
///
/// Recorded with [`ConversationStore::record_window_event`], read with
/// [`ConversationStore::window_events`]. A window event is never part of
/// the history ([`ConversationStore::messages`]) nor of the summary: it
/// moves neither `last_message_at`, `last_text`,
/// [`last_inbound_at`](ConversationSummary::last_inbound_at) nor the unread
/// count, and creates no summary. Whoever checks the window combines it
/// with `last_inbound_at` (roadmap L7).
///
/// Its `Debug` shows the business number, the kind and the time, never the
/// contact nor the id (a message id encodes the customer's phone number).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowEvent {
    /// The conversation whose window it opens.
    pub conversation: ConversationKey,
    /// What happened.
    pub kind: WindowEventKind,
    /// Meta's id of what happened: the call's id for a call, the message's
    /// id (`wamid.…`) for a standby message. With the business number it
    /// identifies the event: recording it again is a no-op.
    pub id: String,
    /// When it happened (Meta's timestamp).
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// A value a `Debug` must not show: `<redacted>`, or `None` for an absent
/// optional one, so that what is present still shows.
struct Redacted(bool);

impl Redacted {
    fn of<T>(value: Option<&T>) -> Self {
        Self(value.is_some())
    }
}

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0 { "<redacted>" } else { "None" })
    }
}

impl fmt::Debug for WindowEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowEvent")
            .field("phone_number_id", &self.conversation.phone_number_id)
            .field("contact", &Redacted(true))
            .field("kind", &self.kind)
            .field("id", &Redacted(true))
            .field("at", &self.at)
            .finish()
    }
}

/// Who holds a thread under Conversation Routing
/// (`conversation-routing/thread-lifecycle`).
///
/// Stored by its serde name (`this_app`, `another_app`, `idle`), which
/// never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ThreadOwner {
    /// The app this store belongs to: control was passed to it
    /// (`control_passed`), or it received the thread's messages on the
    /// `messages` field.
    ThisApp,
    /// Another responder: control was taken from this app
    /// (`control_taken`), or a standby copy arrived.
    AnotherApp,
    /// Nobody: released (`release`, which no webhook reports) or back to
    /// idle after 24 hours without the customer.
    Idle,
}

/// Who owns a conversation's thread under Conversation Routing, and since
/// when: [`ConversationStore::set_thread_owner`],
/// [`ConversationStore::thread_owner`].
///
/// No endpoint reports the owner (`conversation-routing/thread-control`,
/// "Tracking ownership"): the integrator keeps it from the handovers, the
/// field a customer's messages arrive on, its own `release` and the
/// 24-hour idle timeout. The store keeps the latest record and interprets
/// nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadOwnership {
    /// Who holds the thread.
    pub owner: ThreadOwner,
    /// The holder's role identifier as Meta names it
    /// (`conversation-routing/overview`: `customer_service`, `escalation`,
    /// `ai_agent`, …), when known: a handover's `new_owner_role`.
    pub role: Option<String>,
    /// The holder's app, when a handover named it (`new_owner_app_id`).
    pub app_id: Option<AppId>,
    /// Since when: the handover's `timestamp`, or the time of what set it.
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
}

/// A contact of a business's WhatsApp Business app address book, synced
/// under coexistence (`webhooks/reference/smb_app_state_sync`, whose
/// `state_sync[].contact` fields these are, and the
/// `smb_app_state_sync` section of `business-scoped-user-ids`):
/// [`ConversationStore::put_contact`], [`ConversationStore::contacts`].
///
/// Names and the username are content: kept exactly, U+0000 included,
/// like a message's text.
///
/// Its `Debug` shows the business number, the time and which fields are
/// present, never a value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredContact {
    /// The business number whose address book it is, and the contact:
    /// its BSUID (`user_id`) when Meta sent one, else its `phone_number`,
    /// as in a [`ConversationKey`].
    pub key: ConversationKey,
    /// `full_name`, as in the business's address book.
    pub full_name: Option<String>,
    /// `first_name`, as in the business's address book.
    pub first_name: Option<String>,
    /// `phone_number`: omitted by Meta when it may not share it.
    pub phone_number: Option<String>,
    /// `user_id`: the BSUID.
    pub user_id: Option<UserId>,
    /// `parent_user_id`: the parent BSUID, when enabled.
    pub parent_user_id: Option<UserId>,
    /// `username`, when the customer uses one.
    pub username: Option<String>,
    /// When the address book changed: `state_sync[].metadata.timestamp`.
    #[serde(with = "time::serde::rfc3339")]
    pub synced_at: OffsetDateTime,
}

impl fmt::Debug for StoredContact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredContact")
            .field("phone_number_id", &self.key.phone_number_id)
            .field("contact", &Redacted(true))
            .field("full_name", &Redacted::of(self.full_name.as_ref()))
            .field("first_name", &Redacted::of(self.first_name.as_ref()))
            .field("phone_number", &Redacted::of(self.phone_number.as_ref()))
            .field("user_id", &Redacted::of(self.user_id.as_ref()))
            .field(
                "parent_user_id",
                &Redacted::of(self.parent_user_id.as_ref()),
            )
            .field("username", &Redacted::of(self.username.as_ref()))
            .field("synced_at", &self.synced_at)
            .finish()
    }
}

/// Two identities of one person on one business number: `previous`
/// became `current`. Meta reports one when a customer's BSUID changes
/// (the `user_id_update` field, whose `user_id.previous` and
/// `user_id.current` these are, `business-scoped-user-ids`) and when they
/// change their phone number (a `system` message naming the new `wa_id`
/// and `user_id`).
///
/// Stored with [`ConversationStore::link_identity`] and read with
/// [`ConversationStore::identity_links`]; [`ConversationStore::identities`]
/// follows them, so that an erasure reaches the person's records under
/// their earlier identities. A link is personal data:
/// [`erase_all`](ConversationStore::erase_all) deletes the links naming an
/// erased identity. Recording them from the webhooks is `InboxSink`'s
/// (roadmap L7, L8); until then an integrator records them from its own
/// sink.
///
/// Its `Debug` shows the business number and the time, never the
/// identities.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityLink {
    /// The business number the link was reported on.
    pub phone_number_id: PhoneNumberId,
    /// The earlier identity: a BSUID, or a phone number (`wa_id`).
    pub previous: String,
    /// The identity that replaced it.
    pub current: String,
    /// When Meta reported it (the webhook's `timestamp`).
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

impl IdentityLink {
    /// Build a link.
    pub fn new(
        phone_number_id: impl Into<PhoneNumberId>,
        previous: impl Into<String>,
        current: impl Into<String>,
        at: OffsetDateTime,
    ) -> Self {
        Self {
            phone_number_id: phone_number_id.into(),
            previous: previous.into(),
            current: current.into(),
            at,
        }
    }
}

impl fmt::Debug for IdentityLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdentityLink")
            .field("phone_number_id", &self.phone_number_id)
            .field("previous", &Redacted(true))
            .field("current", &Redacted(true))
            .field("at", &self.at)
            .finish()
    }
}

/// What an erasure does to an erased person's messages in conversations
/// keyed by someone else: a group's, where a message's
/// [`sender`](StoredMessage::sender) is the person
/// ([`ConversationStore::erase_all`]). Their own conversations are deleted
/// either way.
///
/// Set per store, like [`Retention`]: the adapters take it as
/// configuration (`with_erasure_mode`), [`ErasureMode::Redact`] unless
/// set. To swap it, configure the store; to decide per request, keep two
/// stores over the same tables (the Postgres adapter is cheap to clone).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErasureMode {
    /// Keep the message in its place (its id, conversation, direction,
    /// status and times) and replace its content, payload and sender with
    /// the [erased](StoredMessage::ERASED) marker
    /// ([`StoredMessage::redact`]), so that the other participants' history
    /// stays whole without the person's data. The default. The message id
    /// stays: Meta's `wamid` encodes the sender's phone number, so choose
    /// [`Delete`](Self::Delete) when it must go too.
    #[default]
    Redact,
    /// Delete the message (its id is then free to be recorded again).
    Delete,
}

/// What [`ConversationStore::erase_all`] deleted, by record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Erased {
    /// Messages, tombstones included, of the erased keys' conversations.
    pub messages: u64,
    /// Conversation summaries of the erased keys (at most one per key).
    pub conversations: u64,
    /// Window events.
    pub window_events: u64,
    /// Thread ownership records (at most one per key).
    pub thread_owners: u64,
    /// Synced address book contacts, and removals of them kept under the
    /// keys ([`ConversationStore::remove_contact`]).
    pub contacts: u64,
    /// Identity links naming an erased identity.
    pub identity_links: u64,
    /// The erased person's messages in conversations keyed by someone
    /// else (a group's), redacted or deleted per the store's
    /// [`ErasureMode`].
    pub group_messages: u64,
}

impl Erased {
    /// Whether nothing was deleted.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// What [`ConversationStore::purge_before`] deleted, by record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Purged {
    /// Messages, tombstones included.
    pub messages: u64,
    /// Conversation summaries whose latest message was purged.
    pub conversations: u64,
    /// Window events.
    pub window_events: u64,
    /// Thread ownership records.
    pub thread_owners: u64,
    /// Removals of synced contacts kept to refuse a late sync
    /// ([`ConversationStore::remove_contact`]).
    pub contact_removals: u64,
}

impl Purged {
    /// Whether nothing was deleted.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// How long a conversation store keeps its history (design D10: set per
/// store, kept by default).
///
/// A store applies it when [`ConversationStore::apply_retention`] is
/// called: nothing purges on its own, so the inbox or the integrator calls
/// it on a schedule. The adapters take it as configuration
/// (`with_retention`); to swap the policy (per tenant, per number), call
/// [`ConversationStore::purge_before`] with a cutoff of your own instead.
/// Which retention a deployment needs is its privacy obligations' call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Retention {
    /// Keep everything (the default).
    #[default]
    Keep,
    /// Purge what is older than this.
    For(Duration),
}

impl Retention {
    /// Keep `n` days.
    pub fn days(n: u32) -> Self {
        Self::For(Duration::from_hours(24 * u64::from(n)))
    }

    /// The cutoff at `now`: what is older is purged. `None` keeps
    /// everything: [`Retention::Keep`], or a duration reaching before the
    /// earliest representable time.
    pub fn cutoff(self, now: OffsetDateTime) -> Option<OffsetDateTime> {
        match self {
            Self::Keep => None,
            Self::For(d) => now.checked_sub(time::Duration::try_from(d).ok()?),
        }
    }
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
/// (`meta_whatsapp_adapters::store::conversation_conformance`) checks the content
/// rule.
///
/// Beside the history, a store keeps [window events](WindowEvent),
/// [thread ownership](ThreadOwnership) and the coexistence address book
/// ([`StoredContact`]), each under a [`ConversationKey`] (BSUID first,
/// never a phone number when a BSUID is known), and the
/// [links](IdentityLink) between a person's identities. Records keyed by
/// a contact are personal data, and a person also appears in group
/// conversations (as a message's [sender](StoredMessage::sender)) and in
/// other conversations' payloads (a quoted message, a shared contact
/// card), which no erasure reaches. [`identities`](Self::identities)
/// finds a person's keys on a number, [`erase_all`](Self::erase_all)
/// erases them in one step ([`erase`](Self::erase) is its one-key case),
/// and [`purge_before`](Self::purge_before) and
/// [`apply_retention`](Self::apply_retention) delete history by age.
///
/// Every method is required but [`erase`](Self::erase),
/// [`erasure_mode`](Self::erasure_mode), [`retention`](Self::retention)
/// and [`apply_retention`](Self::apply_retention), whose defaults are
/// correct for any store (redact group messages and keep everything
/// unless configured).
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
    /// is never stored, as long as the tombstone is: an
    /// [erasure](Self::erase_all) or a [purge](Self::purge_before) that
    /// deletes it frees the id, and the message, arriving after that, is
    /// stored with its content.
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

    /// Message `id` of business number `phone_number_id`, if one is stored
    /// for that number: a live or synced message, or a revoke's tombstone
    /// ([`StoredMessage::REVOKED`]). Never a message of another number,
    /// whether the store keeps a message id once (today's rule,
    /// `OPEN_QUESTIONS.md` #33) or once per number.
    async fn message(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
    ) -> Result<Option<StoredMessage>, StorageError>;

    /// Record a [`WindowEvent`]. Returns `false` (and changes nothing) when
    /// an event with the same `id` is stored for the same business number,
    /// whatever its kind, time or conversation: webhook retries make this
    /// common. It touches neither the history nor the summary (see
    /// [`WindowEvent`]).
    async fn record_window_event(&self, event: WindowEvent) -> Result<bool, StorageError>;

    /// Window events of one conversation, newest first by `(at, id)`,
    /// strictly older than `before` (an `(at, id)` cursor taken from the
    /// last row of the previous page), as [`messages`](Self::messages)
    /// pages. The first of `window_events(key, None, 1)` is the latest.
    async fn window_events(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<WindowEvent>, StorageError>;

    /// Record who owns conversation `key`'s thread: stored unless the
    /// stored record is later (`ownership.since` before the stored
    /// `since`), so a late handover never overwrites a newer one, while a
    /// change in the same second as the stored one (Meta's timestamps are
    /// seconds) does. Returns whether it was stored. It touches neither
    /// the history nor the summary.
    ///
    /// Two records of the same second end in the order the store receives
    /// them: the last one stored wins, whichever replica wrote it, and a
    /// redelivery of the first, processed after the second, wins back.
    /// Meta's timestamps give no finer order; the webhook dedup markers
    /// keep redeliveries of a delivered event out (seven days by default).
    async fn set_thread_owner(
        &self,
        key: &ConversationKey,
        ownership: ThreadOwnership,
    ) -> Result<bool, StorageError>;

    /// The latest [`ThreadOwnership`] stored for conversation `key`.
    async fn thread_owner(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<ThreadOwnership>, StorageError>;

    /// Store a synced address book contact (an `add` of
    /// `smb_app_state_sync`, which is also how an edit arrives), replacing
    /// the one stored under its key unless that one is later
    /// (`contact.synced_at` before the stored `synced_at`), by the same
    /// rule as [`set_thread_owner`](Self::set_thread_owner). A removal
    /// counts as stored: an `add` older than a removal of its key is
    /// refused, one of the same second or later stores the contact again.
    /// Returns whether it was stored. Contacts are not conversations: this
    /// creates no summary.
    async fn put_contact(&self, contact: StoredContact) -> Result<bool, StorageError>;

    /// Remove the synced contact stored under `key` (a `remove` of
    /// `smb_app_state_sync`), unless it was synced after `at`. Returns
    /// whether a contact was removed.
    ///
    /// Webhooks arrive out of order (a failed delivery is retried later),
    /// so the removal is kept: its key and `at`, nothing else of the
    /// contact (no name, username, phone number or BSUID), so that an
    /// older `add` arriving after it is refused. It is kept too when no
    /// contact is stored yet (the removal arrived first), or moves to `at`
    /// when a removal is stored already. [`contact`](Self::contact) and
    /// [`contacts`](Self::contacts) never return one;
    /// [`erase`](Self::erase) deletes it with the key's records, and
    /// [`purge_before`](Self::purge_before) once it is older than the
    /// cutoff.
    async fn remove_contact(
        &self,
        key: &ConversationKey,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError>;

    /// The synced contact stored under `key`.
    async fn contact(&self, key: &ConversationKey) -> Result<Option<StoredContact>, StorageError>;

    /// The synced contacts of business number `phone_number_id`, by
    /// [`contact`](ConversationKey::contact) ascending in byte order (the
    /// order of Rust's `str`), strictly after `after` (the last contact of
    /// the previous page). Never another number's.
    async fn contacts(
        &self,
        phone_number_id: &PhoneNumberId,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<StoredContact>, StorageError>;

    /// Store a link between two identities of one person
    /// ([`IdentityLink`]). Returns `false` (and changes nothing) when the
    /// same link (business number, `previous`, `current`) is stored,
    /// whatever its time: webhook retries make this common. Links are not
    /// history: [`purge_before`](Self::purge_before) keeps them, and
    /// [`erase_all`](Self::erase_all) deletes them.
    async fn link_identity(&self, link: IdentityLink) -> Result<bool, StorageError>;

    /// The links of business number `key.phone_number_id` that name
    /// `key.contact` as their `previous` or `current` identity, oldest
    /// first by `(at, previous, current)` (identities in byte order).
    /// Never another number's.
    async fn identity_links(
        &self,
        key: &ConversationKey,
    ) -> Result<Vec<IdentityLink>, StorageError>;

    /// Every identity this store connects to `key.contact` on business
    /// number `key.phone_number_id`, `key.contact` included: the closure
    /// over the synced contacts (a contact's key, `user_id`,
    /// `parent_user_id` and `phone_number` are one person; a kept removal
    /// connects nothing) and the [identity links](IdentityLink). Read only.
    /// Never another number's: a `wa_id` is the same on every number, and
    /// another number's records may be another business's.
    ///
    /// Pass the result to [`erase_all`](Self::erase_all) to erase a
    /// person rather than one key. It is only as complete as what was
    /// stored: an identity no contact and no link connects (a history
    /// thread keyed by a phone number Meta did not share with the address
    /// book, a BSUID change nobody recorded) is not found, and a phone
    /// number recycled by the operator connects its two owners.
    async fn identities(&self, key: &ConversationKey) -> Result<BTreeSet<String>, StorageError>;

    /// Erase one person on one business number, known by `contacts` (their
    /// BSUIDs, parent BSUIDs and phone numbers: [`identities`](Self::identities)
    /// collects them): delete, not hide, in one step:
    ///
    /// - every record keyed by one of `contacts`: its messages, live,
    ///   synced and tombstones, content and all (so an erased message id
    ///   can be recorded again), its conversation summary (the latest
    ///   message's preview, the window, the unread count), its window
    ///   events and its thread ownership record;
    /// - the synced address book contacts of the number that name one of
    ///   `contacts` as their key, `user_id`, `parent_user_id` or
    ///   `phone_number`, and the removals kept under those keys
    ///   ([`remove_contact`](Self::remove_contact));
    /// - the [identity links](IdentityLink) naming one of `contacts`;
    /// - the person's messages in conversations keyed by someone else (a
    ///   group's): those whose [`sender`](StoredMessage::sender) is one of
    ///   `contacts`, redacted or deleted per [`erasure_mode`](Self::erasure_mode).
    ///   Each such conversation's summary is then made from what stays, so
    ///   its preview never holds the erased text: under
    ///   [`ErasureMode::Redact`] the redacted message keeps its place and
    ///   the preview becomes `None` when it is the latest; under
    ///   [`ErasureMode::Delete`] the latest remaining message (never a
    ///   tombstone) becomes the summary's, and a conversation left with
    ///   none has no summary. Its window and unread count stay.
    ///
    /// Records of other numbers are never touched: erase each number's.
    /// There is no erasure across numbers on purpose: a `wa_id` is the
    /// same on every number, so one would delete other businesses'
    /// customers. Returns what was deleted; an empty `contacts`, or ids
    /// nothing is stored under, delete nothing.
    ///
    /// What this does not reach:
    ///
    /// - in this store: the person as quoted or shared in someone else's
    ///   message (a reply's `context`, a contact card, a number-change
    ///   `system` message under their earlier key names the new one: erase
    ///   both), and, under [`ErasureMode::Redact`], the ids of their group
    ///   messages (Meta's `wamid`, which encodes their phone number);
    /// - identities nothing connects (see [`identities`](Self::identities));
    /// - outside it: the webhook dedup markers and OTP challenges
    ///   (`KvStore`: hashed keys, expiring; keep the dedup markers, they
    ///   stop Meta's redeliveries from recording the data again), events a
    ///   sink forwarded elsewhere (the service's event outbox and its
    ///   idempotency answers, a dead-letter store, live clients), copies
    ///   the integrator made (downloaded media), the database's own
    ///   remnants (on Postgres: dead rows until `VACUUM`, the WAL,
    ///   replicas, backups), logs, and Meta's side (the business's contact
    ///   book, the WhatsApp Business app under coexistence).
    ///
    /// Records are created again by what arrives after the erasure: a new
    /// message, an echo, a history chunk or an address book sync not
    /// delivered yet, a revoke (its tombstone holds the BSUID and the
    /// message id), a redelivery once its dedup marker expired (seven
    /// days). Erase again after that window.
    ///
    /// An erased tombstone frees its message id: the message it stood for,
    /// arriving later in a history chunk, is then stored with the content
    /// its sender revoked (see [`revoke`](Self::revoke)). Erasing again
    /// after the redelivery window deletes it.
    ///
    /// Against concurrent writes, an erasure is one step: a message whose
    /// [`append`](Self::append) or [`append_synced`](Self::append_synced)
    /// runs at the same time on the number is either erased (or redacted)
    /// with the rest or recorded after the erasure, with a summary of its
    /// own; never kept without its summary, nor a summary without its
    /// messages.
    ///
    /// Call it behind the caller's ownership check of the business number:
    /// it trusts `phone_number_id` (`meta_whatsapp_rs::inbox::Inbox::erase_all`
    /// is bound to its inbox's number).
    async fn erase_all(
        &self,
        phone_number_id: &PhoneNumberId,
        contacts: &[String],
    ) -> Result<Erased, StorageError>;

    /// Erase one key on its business number: [`erase_all`](Self::erase_all)
    /// with `key.contact` alone. A person stored under several keys (a
    /// history thread keyed by their phone number, live messages by their
    /// BSUID, a new BSUID after a number change) keeps their records under
    /// the others: erase a person with [`identities`](Self::identities) and
    /// [`erase_all`](Self::erase_all). Like it, call it behind the caller's
    /// ownership check (`meta_whatsapp_rs::inbox::Inbox::erase` makes it).
    async fn erase(&self, key: &ConversationKey) -> Result<Erased, StorageError> {
        self.erase_all(&key.phone_number_id, std::slice::from_ref(&key.contact))
            .await
    }

    /// What [`erase_all`](Self::erase_all) does to an erased person's
    /// messages in someone else's conversation: the adapter's
    /// configuration, [`ErasureMode::Redact`] unless set.
    fn erasure_mode(&self) -> ErasureMode {
        ErasureMode::Redact
    }

    /// Purge by age: delete the messages (tombstones included) timestamped
    /// strictly before `cutoff`, the window events before it, the thread
    /// ownership records set before it, the removals of synced contacts
    /// made before it ([`remove_contact`](Self::remove_contact)), and the
    /// summary of every conversation whose latest message was purged (it
    /// holds that message's preview). Of business number
    /// `phone_number_id` only, or of every number with `None`. Synced
    /// address book contacts and [identity links](IdentityLink) are not
    /// history (a later erasure follows them to the person's other keys):
    /// [`remove_contact`](Self::remove_contact) and
    /// [`erase_all`](Self::erase_all) delete them.
    ///
    /// A conversation keeping newer messages keeps its summary as it was:
    /// its latest message, preview and window, and an unread count that
    /// may include purged messages until the next
    /// [`mark_read`](Self::mark_read). Returns what was deleted. A message
    /// recorded while the purge runs may be kept even when older than the
    /// cutoff (a late history chunk): the next purge deletes it.
    async fn purge_before(
        &self,
        phone_number_id: Option<&PhoneNumberId>,
        cutoff: OffsetDateTime,
    ) -> Result<Purged, StorageError>;

    /// The [`Retention`] this store applies in
    /// [`apply_retention`](Self::apply_retention): the adapter's
    /// configuration, [`Retention::Keep`] unless set.
    fn retention(&self) -> Retention {
        Retention::Keep
    }

    /// Apply [`retention`](Self::retention) at `now`: purge every number's
    /// records older than its cutoff ([`purge_before`](Self::purge_before)
    /// with `None`), or nothing under [`Retention::Keep`]. Nothing calls
    /// it on its own: schedule it (the inbox or the integrator).
    async fn apply_retention(&self, now: OffsetDateTime) -> Result<Purged, StorageError> {
        match self.retention().cutoff(now) {
            Some(cutoff) => self.purge_before(None, cutoff).await,
            None => Ok(Purged::default()),
        }
    }
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

    /// Stores keep these names: a change strands the rows written before.
    #[test]
    fn stored_names_are_pinned() {
        use WindowEventKind as K;
        for (kind, name) in [
            (K::CustomerCall, "customer_call"),
            (K::CallAccepted, "call_accepted"),
            (K::StandbyMessage, "standby_message"),
            (K::Other("future".to_owned()), "future"),
        ] {
            assert_eq!(kind.as_str(), name);
            assert_eq!(WindowEventKind::from_name(name), kind);
            assert_eq!(
                serde_json::to_value(&kind).unwrap(),
                serde_json::json!(name)
            );
            assert_eq!(
                serde_json::from_value::<WindowEventKind>(serde_json::json!(name)).unwrap(),
                kind
            );
        }
        for (owner, name) in [
            (ThreadOwner::ThisApp, "this_app"),
            (ThreadOwner::AnotherApp, "another_app"),
            (ThreadOwner::Idle, "idle"),
        ] {
            assert_eq!(
                serde_json::to_value(owner).unwrap(),
                serde_json::json!(name)
            );
        }
        // This crate's own message kinds (`\x72` is `r`, `\x65` is `e`).
        assert_eq!(StoredMessage::REVOKED, "\x72evoked");
        assert_eq!(StoredMessage::ERASED, "\x65rased");
    }

    fn inbound(payload: serde_json::Value) -> StoredMessage {
        StoredMessage {
            id: MessageId::new("wamid.1"),
            conversation: ConversationKey::new("pn", "HBgGROUP"),
            direction: Direction::Inbound,
            kind: "text".to_owned(),
            text: Some("hello".to_owned()),
            payload,
            status: DeliveryStatus::Received,
            timestamp: datetime!(2026-09-24 12:00 UTC),
            status_at: None,
            error: Some(serde_json::json!({"code": 1})),
        }
    }

    /// The sender an erasure matches: the BSUID, else the phone number, of
    /// an inbound message; never the business of an outbound one.
    #[test]
    fn the_sender_is_the_bsuid_else_the_phone_number_of_an_inbound_message() {
        use serde_json::json;
        let both = json!({"from": "16505551234", "from_user_id": "US.1", "group_id": "HBgGROUP"});
        assert_eq!(inbound(both.clone()).sender(), Some("US.1"));
        assert_eq!(
            inbound(json!({"from": "16505551234"})).sender(),
            Some("16505551234")
        );
        for (payload, why) in [
            (
                json!({"from": "16505551234", "from_user_id": ""}),
                "empty BSUID",
            ),
            (
                json!({"from": "16505551234", "from_user_id": 7}),
                "not a string",
            ),
            (
                json!({"from": "16505551234", "from_user_id": "US.\u{0}1"}),
                "a NUL",
            ),
        ] {
            assert_eq!(inbound(payload).sender(), Some("16505551234"), "{why}");
        }
        for payload in [
            json!({}),
            json!([]),
            json!({"from": ""}),
            json!({"text": {"from": "16505551234"}}),
        ] {
            assert_eq!(inbound(payload.clone()).sender(), None, "{payload}");
        }
        let outbound = StoredMessage {
            direction: Direction::Outbound,
            ..inbound(both)
        };
        assert_eq!(
            outbound.sender(),
            None,
            "an outbound message's is the business"
        );
    }

    #[test]
    fn redact_keeps_the_place_and_drops_the_content() {
        let original = inbound(serde_json::json!({"from": "16505551234", "text": {"body": "hi"}}));
        let mut redacted = original.clone();
        redacted.redact();
        assert_eq!(
            redacted,
            StoredMessage {
                kind: "erased".to_owned(),
                text: None,
                payload: serde_json::json!({}),
                error: None,
                ..original
            }
        );
        assert_eq!(redacted.sender(), None);
    }

    /// Review of roadmap L5 (L1): the records of a person print no value
    /// of theirs, only the business number, times and which fields are
    /// present.
    #[test]
    fn debug_shows_no_personal_data() {
        let at = datetime!(2026-09-24 12:00 UTC);
        let contact = StoredContact {
            key: ConversationKey::new("106540352242922", "US.1349"),
            full_name: Some("Pablo Morales".to_owned()),
            first_name: None,
            phone_number: Some("16505551234".to_owned()),
            user_id: Some(UserId::new("US.1349")),
            parent_user_id: None,
            username: Some("pablo".to_owned()),
            synced_at: at,
        };
        let event = WindowEvent {
            conversation: ConversationKey::new("106540352242922", "US.1349"),
            kind: WindowEventKind::CustomerCall,
            id: "wacid.HBgLMTY1MDU1NTEyMzQVAgASGBQ".to_owned(),
            at,
        };
        let link = IdentityLink::new("106540352242922", "US.1349", "US.2468", at);
        assert_eq!(
            format!("{contact:?}"),
            "StoredContact { phone_number_id: PhoneNumberId(106540352242922), \
             contact: <redacted>, full_name: <redacted>, first_name: None, \
             phone_number: <redacted>, user_id: <redacted>, parent_user_id: None, \
             username: <redacted>, synced_at: 2026-09-24 12:00:00.0 +00:00:00 }"
        );
        assert_eq!(
            format!("{event:?}"),
            "WindowEvent { phone_number_id: PhoneNumberId(106540352242922), \
             contact: <redacted>, kind: CustomerCall, id: <redacted>, \
             at: 2026-09-24 12:00:00.0 +00:00:00 }"
        );
        assert_eq!(
            format!("{link:?}"),
            "IdentityLink { phone_number_id: PhoneNumberId(106540352242922), \
             previous: <redacted>, current: <redacted>, at: 2026-09-24 12:00:00.0 +00:00:00 }"
        );
        for rendered in [
            format!("{contact:?}"),
            format!("{contact:#?}"),
            format!("{event:?}"),
            format!("{link:#?}"),
        ] {
            for value in [
                "US.1349",
                "US.2468",
                "Pablo",
                "16505551234",
                "pablo",
                "wacid",
            ] {
                assert!(!rendered.contains(value), "{value} in {rendered}");
            }
        }
    }

    #[test]
    fn retention_cutoffs() {
        let now = datetime!(2026-09-24 10:00 UTC);
        assert_eq!(Retention::default(), Retention::Keep);
        assert_eq!(Retention::Keep.cutoff(now), None);
        assert_eq!(
            Retention::days(30).cutoff(now),
            Some(datetime!(2026-08-25 10:00 UTC))
        );
        assert_eq!(
            Retention::For(Duration::from_secs(90)).cutoff(now),
            Some(datetime!(2026-09-24 09:58:30 UTC))
        );
        assert_eq!(
            Retention::For(Duration::MAX).cutoff(now),
            None,
            "nothing is older than the earliest time"
        );
        assert!(Erased::default().is_empty() && Purged::default().is_empty());
        for erased in [
            Erased {
                contacts: 1,
                ..Erased::default()
            },
            Erased {
                identity_links: 1,
                ..Erased::default()
            },
            Erased {
                group_messages: 1,
                ..Erased::default()
            },
        ] {
            assert!(!erased.is_empty(), "{erased:?}");
        }
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
