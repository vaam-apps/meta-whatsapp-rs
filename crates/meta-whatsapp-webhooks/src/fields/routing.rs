//! Conversation Routing fields: thread ownership changes and standby copies.
//!
//! Doc paths: `webhooks/reference/messaging-handovers`,
//! `webhooks/reference/standby`, `conversation-routing/overview` (role
//! identifiers), `conversation-routing/thread-control` and
//! `conversation-routing/conversation-context`.
//!
//! Under Conversation Routing one responder owns a thread at a time.
//! `messaging_handovers` tells a responder that it gained (`control_passed`)
//! or lost (`control_taken`) a thread. No endpoint reports the owner: derive
//! it from these, from which field (`messages` or `standby`) a user's
//! messages arrive on, from your own `release` (which fires no handover) and
//! from the 24-hour idle timeout (`conversation-routing/thread-control`
//! § Tracking ownership). `standby`
//! delivers copies of a thread's inbound messages, the owner's sends
//! (echoes) and their statuses to partners that observe it without owning
//! it: a standby partner must not reply.
//!
//! # What the pages leave open
//!
//! - Every example on both pages is written with placeholders (`<TIMESTAMP>`,
//!   `<MESSAGE_ID>`, …); the fixtures fill them with values in the documented
//!   formats.
//! - The standby page's "Common envelope" shows `"standby": {}`; a standby
//!   value with no item this crate recognizes is kept as
//!   [`crate::ChangeValue::Unknown`] (see [`crate::payload::NO_RECOGNIZED_ITEMS`]).
//! - A standby echo's `message` is "the exact request body" of the Send API,
//!   and its sibling `template` / `flow` are the full template and Flow
//!   definitions. This crate does not depend on the client's send types, so
//!   the three stay JSON ([`StandbyEcho::message`]); [`StandbyEcho::to`],
//!   [`StandbyEcho::recipient`] and [`StandbyEcho::message_type`] read the
//!   addressing (`to` or the BSUID `recipient`, one of which every send has)
//!   and the type.
//! - Neither page shows a business-scoped user id: `sender.phone_number` is
//!   the only user identity on a handover and "may be omitted", and standby
//!   items carry the identities of the `messages` webhook they copy.

use meta_whatsapp_core::ids::{AppId, MessageId, PhoneNumberId, UserId, WaId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use super::common::{Contact, Metadata};
use super::messages::{InboundMessage, Status};
use crate::open_enum::open_enum;

/// `value` of a `messaging_handovers` change: a thread changed owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagingHandoversValue {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// The WhatsApp user the thread is with. Meta omits it (or its phone
    /// number) where the number is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<HandoverSender>,
    /// The business phone number the thread belongs to.
    pub recipient: HandoverRecipient,
    /// Which notification this is; names the object that is present
    /// ([`Self::control_passed`] or [`Self::control_taken`]).
    #[serde(rename = "type")]
    pub handover_type: HandoverType,
    /// When ownership changed (whole seconds).
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub timestamp: OffsetDateTime,
    /// Set when [`Self::handover_type`] is [`HandoverType::ControlPassed`]: you are
    /// now the owner and are expected to reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_passed: Option<Handover>,
    /// Set when [`Self::handover_type`] is [`HandoverType::ControlTaken`]: you lost
    /// the thread and must stop replying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_taken: Option<Handover>,
}

impl MessagingHandoversValue {
    /// The notification object matching [`Self::handover_type`], or whichever one is
    /// present when the type is one this crate does not know.
    pub fn handover(&self) -> Option<&Handover> {
        match self.handover_type {
            HandoverType::ControlPassed => self.control_passed.as_ref(),
            HandoverType::ControlTaken => self.control_taken.as_ref(),
            HandoverType::Other(_) => self.control_passed.as_ref().or(self.control_taken.as_ref()),
        }
    }
}

/// `sender` of a handover: the WhatsApp user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoverSender {
    /// The user's phone number; optional per the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<WaId>,
}

/// `recipient` of a handover: the business phone number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoverRecipient {
    /// The business phone number id.
    #[serde(deserialize_with = "crate::serde_ext::id::deserialize")]
    pub phone_number_id: PhoneNumberId,
    /// The business number, as displayed.
    pub display_phone_number: String,
}

open_enum! {
    /// `value.type` of a handover.
    pub enum HandoverType {
        /// Control was passed to you (`pass`).
        ControlPassed => "control_passed",
        /// The escalation partner took the thread from you (`take`, or a
        /// Service message it sent).
        ControlTaken => "control_taken",
    }
}

open_enum! {
    /// A responder's role identifier (`conversation-routing/overview`).
    /// Key ownership state on these, not on app ids.
    pub enum ThreadRole {
        /// Primary for the Service entry point.
        CustomerService => "customer_service",
        /// Primary for the Marketing Message Response entry point.
        Marketing => "marketing",
        /// Primary for the Utility Response entry point.
        Utility => "utility",
        /// Primary for the Click to WhatsApp entry point.
        Ctwa => "ctwa",
        /// The designated AI agent (currently Meta Business Agent).
        AiAgent => "ai_agent",
        /// The designated escalation partner.
        Escalation => "escalation",
    }
}

open_enum! {
    /// `previous_owner_app_role` (deprecated by Meta).
    pub enum HandoverAppRole {
        /// Meta Business Agent; the only value the page lists.
        MetaBusinessAgent => "meta_business_agent",
    }
}

/// `control_passed` / `control_taken`: who held the thread and who holds it
/// now. Every property is optional per the page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handover {
    /// App that held the thread. Deprecated by Meta: read
    /// [`Self::previous_owner_role`].
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::id_option::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub previous_owner_app_id: Option<AppId>,
    /// App role of the previous owner (`control_passed` only). Deprecated
    /// by Meta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_owner_app_role: Option<HandoverAppRole>,
    /// Role of the responder that held the thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_owner_role: Option<ThreadRole>,
    /// App that holds the thread now, when it is identified by app.
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::id_option::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub new_owner_app_id: Option<AppId>,
    /// Role of the responder that holds the thread now (always
    /// `escalation` for `control_taken`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_owner_role: Option<ThreadRole>,
    /// Free-form text from the `pass` / `take` request (up to 2,000
    /// characters), or `Control taken via service message` for an implicit
    /// take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
    /// AI-generated summary of the conversation (`control_passed` only, and
    /// only under the conditions of `conversation-routing/conversation-context`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_context: Option<ConversationContext>,
}

open_enum! {
    /// `conversation_context.type`.
    pub enum ConversationContextType {
        /// An AI-generated summary in [`ConversationContext::summary`].
        Summary => "summary",
    }
}

/// `conversation_context`: a summary of the conversation so far, on a
/// `control_passed` handover and on the `messages` webhook
/// ([`super::MessagesValue::conversation_context`]). Model-generated prose:
/// show it to a person, never parse it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationContext {
    /// Kind of context.
    #[serde(rename = "type")]
    pub context_type: ConversationContextType,
    /// The summary, for [`ConversationContextType::Summary`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<ContextSummary>,
}

/// `conversation_context.summary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSummary {
    /// The summary text.
    pub text: String,
}

/// `value` of a `standby` change: copies for a partner that observes a
/// thread it does not own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StandbyValue {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// The business phone number.
    pub metadata: Metadata,
    /// The copies; one of the three lists is set per the page.
    pub standby: Standby,
}

/// `value.standby`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Standby {
    /// The senders of [`Self::messages`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<Contact>,
    /// Inbound messages, in the schema of the `messages` webhook.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<InboundMessage>,
    /// Messages the thread's owner sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_echoes: Vec<StandbyEcho>,
    /// Statuses of messages the thread's owner sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<Status>,
}

impl Standby {
    /// Whether no list holds an item.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty() && self.message_echoes.is_empty() && self.statuses.is_empty()
    }
}

/// `standby.message_echoes[]`: a message the thread's owner sent through
/// Cloud API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StandbyEcho {
    /// Message id assigned at send time.
    pub id: MessageId,
    /// When it was sent.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub timestamp: OffsetDateTime,
    /// The Send API request body, verbatim (see the [module docs](self)).
    pub message: Value,
    /// Full template definition, for template messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<Value>,
    /// Full Flow definition (as `GET /{flow-id}` returns it), for Flow messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<Value>,
}

impl StandbyEcho {
    /// `message.to`: the recipient's phone number (or a group id), when
    /// the owner addressed it that way.
    pub fn to(&self) -> Option<&str> {
        self.message.get("to").and_then(Value::as_str)
    }

    /// `message.recipient`: the recipient's BSUID, when the owner addressed
    /// it that way (`business-scoped-user-ids`). Key the user by this when
    /// it is set.
    pub fn recipient(&self) -> Option<UserId> {
        self.message
            .get("recipient")
            .and_then(Value::as_str)
            .map(UserId::new)
    }

    /// `message.type` (`text`, `template`, `interactive`, …).
    pub fn message_type(&self) -> Option<&str> {
        self.message.get("type").and_then(Value::as_str)
    }
}

/// One standby copy, as [`crate::WebhookEvent::StandbyObserved`] carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StandbyItem {
    /// An inbound message routed to another responder.
    Message(InboundMessage),
    /// A message the owner sent.
    Echo(StandbyEcho),
    /// A status of a message the owner sent.
    Status(Status),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn handover_picks_the_object_its_type_names() {
        let mut v: MessagingHandoversValue = serde_json::from_value(json!({
            "recipient": {"phone_number_id": "1", "display_phone_number": "2"},
            "type": "control_taken", "timestamp": "1750101000",
            "control_passed": {"metadata": "p"},
            "control_taken": {"metadata": "t"}
        }))
        .unwrap();
        assert_eq!(v.handover().unwrap().metadata.as_deref(), Some("t"));
        v.handover_type = HandoverType::ControlPassed;
        assert_eq!(v.handover().unwrap().metadata.as_deref(), Some("p"));
        v.handover_type = HandoverType::from("control_shared");
        v.control_passed = None;
        assert_eq!(v.handover().unwrap().metadata.as_deref(), Some("t"));
    }

    #[test]
    fn a_handover_without_sender_or_owner_fields_parses() {
        let v: MessagingHandoversValue = serde_json::from_value(json!({
            "recipient": {"phone_number_id": 106_540_352_242_922_u64, "display_phone_number": "2"},
            "type": "control_taken", "timestamp": 1_750_101_000,
            "control_taken": {}
        }))
        .unwrap();
        assert!(v.sender.is_none());
        assert_eq!(v.recipient.phone_number_id.as_str(), "106540352242922");
        assert_eq!(v.handover(), Some(&Handover::default()));
    }

    #[test]
    fn standby_echo_reads_recipient_and_type() {
        let e: StandbyEcho = serde_json::from_value(json!({
            "id": "wamid.1", "timestamp": "1750101000",
            "message": {"to": "16505551234", "type": "text", "text": {"body": "x"}}
        }))
        .unwrap();
        assert_eq!(e.to(), Some("16505551234"));
        assert_eq!(e.recipient(), None);
        assert_eq!(e.message_type(), Some("text"));

        let e: StandbyEcho = serde_json::from_value(json!({
            "id": "wamid.1", "timestamp": "1750101000",
            "message": {"recipient": "US.13491208655302741918", "type": "text"}
        }))
        .unwrap();
        assert_eq!(e.to(), None);
        assert_eq!(
            e.recipient().as_ref().map(UserId::as_str),
            Some("US.13491208655302741918")
        );
    }

    #[test]
    fn numeric_app_ids_do_not_untype_the_handover() {
        let h: Handover = serde_json::from_value(json!({
            "previous_owner_app_id": 1_066_355_071_287_456_u64,
            "new_owner_app_id": "42"
        }))
        .unwrap();
        assert_eq!(
            h.previous_owner_app_id.as_ref().map(AppId::as_str),
            Some("1066355071287456")
        );
        assert_eq!(h.new_owner_app_id.as_ref().map(AppId::as_str), Some("42"));
    }
}
