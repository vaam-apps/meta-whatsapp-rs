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
//! [`Error::kind`](meta_whatsapp_core::Error::kind) is `CustomerServiceWindowClosed`,
//! like Meta's `131047`)
//! instead of paying for a request Meta would reject. Templates and Direct
//! Send `utility` and `authentication` messages (a `category` Meta sends as
//! a template) are exempt; a Direct Send `service` message is not (Meta
//! drops it outside the window). Use [`Inbox::window`] to decide up front.
//!
//! **Window events.** Meta also starts or refreshes the window when the
//! customer calls the business number, answered or not, and when they
//! accept the business's call (`calling/pricing`, "How calling changes the
//! 24 hour customer service window"). [`InboxSink`] records these as
//! [window events](WindowEvent), never as messages (they are in neither the
//! history nor the unread count), and [`Inbox::window`] opens from the
//! latest of the customer's last inbound message and the latest window
//! event. Which webhooks count, each from Meta's calling pages:
//!
//! - a `calls[]` event with `direction` `USER_INITIATED` and `event`
//!   `connect` (the customer calls: `calling/user-initiated-calls`),
//!   `call_created` (a SIP call attempted: `calling/sip`) or `terminate`
//!   (the call ended; answered or not, it was the customer's call):
//!   [`WindowEventKind::CustomerCall`], at the event's `timestamp`, or a
//!   terminate's `start_time` when it names one;
//! - a call status `ACCEPTED` (`calling/business-initiated-calls`: "the
//!   WhatsApp user accepts the call"): [`WindowEventKind::CallAccepted`], at
//!   its `timestamp`;
//! - a `terminate` with `direction` `BUSINESS_INITIATED` and a `start_time`,
//!   "only present when the call was picked up by the other party" (a
//!   standby partner sees only this webhook of the business's calls,
//!   `conversation-routing/calling-webhooks`):
//!   [`WindowEventKind::CallAccepted`], at its `start_time`.
//!
//! Nothing else: not a call status `RINGING` or `REJECTED`, not the
//! business's own `connect` (sent before the customer answers), not a
//! recording or transcript notice, and not an event without a `direction`
//! (the user-initiated page's first `connect` example has none; the pages
//! disagree on the SDP type that would tell). A call is keyed like a
//! message: the customer's BSUID (`from_user_id`, `to_user_id`, the
//! contact's `user_id`), else their phone number (the contact's `wa_id`,
//! else whichever of `from` and `to` is not the business number). Every
//! event of one call has the call's id, so the first one recorded is kept.
//! A `terminate` recorded before its `connect` (webhooks arrive out of
//! order) dates the call at `start_time` or at the end of an unanswered
//! call, up to its ringing time after the call began: the local window then
//! closes that much later than Meta's.
//!
//! **Conversation Routing** (`conversation-routing/*`). One responder owns a
//! thread; the others may receive standby copies. No endpoint reports the
//! owner, so the inbox keeps it from the signals Meta lists
//! (`conversation-routing/thread-control`, "Tracking ownership"):
//!
//! - a handover (`ThreadControlChanged`): `control_passed` records
//!   [`ThreadOwner::ThisApp`], `control_taken` [`ThreadOwner::AnotherApp`],
//!   with the new owner's role and app when Meta names them, at the
//!   handover's `timestamp`. A handover names the customer by
//!   `sender.phone_number` only (Meta may omit it: then nothing is
//!   recorded), so it is recorded under the conversation that phone number
//!   leads to (see [`InboxSink`]);
//! - a standby copy of a customer's message (`StandbyObserved`): a window
//!   event ([`WindowEventKind::StandbyMessage`]: the customer wrote, but not
//!   to this app, so it is neither history nor unread) and
//!   [`ThreadOwner::AnotherApp`] since just before the message's time (1
//!   ms: that app owned the thread when the message reached it), unless a
//!   record of that second or later is stored. A handover is the stronger
//!   signal: dated before the copy's second, the record never overrides a
//!   handover of that second, in whichever order they are stored, and
//!   also when two replicas race. Standby echoes and receipts record
//!   nothing, and a group's copy records no owner (the routing pages
//!   describe threads with one customer);
//! - a message on the `messages` field after the stored record (one of a
//!   standby copy's second included): this app received it, so it owns the
//!   thread ([`Inbox::thread_owner`] derives it; nothing is written per
//!   message). Not a customer's answer to a call permission request: it
//!   reaches the Incoming Call primary and the standby partners on
//!   `messages`, and "does not change thread ownership"
//!   (`conversation-routing/calling-webhooks`);
//! - this app's own `release` (no webhook reports it):
//!   [`Inbox::record_release`];
//! - 24 hours without the customer ([`Inbox::THREAD_IDLE_AFTER`]): the
//!   thread is idle again ([`Inbox::thread_owner`] derives it).
//!
//! [`Inbox::reply`] and [`Inbox::send`] refuse a service message locally
//! when another app owns the thread (a [`ValidationError`] on field
//! [`THREAD_OWNER`]: [`is_thread_owned_elsewhere`]); templates, and Direct
//! Send `utility` and `authentication` messages, need no ownership
//! (`conversation-routing/thread-lifecycle`, "Sending without ownership").
//! Ownership is advisory: Meta enforces it. [`ReplyChecks`] turns either
//! local check off, per inbox ([`Inbox::with_reply_checks`]): the designated
//! escalation partner, whose service message is an implicit `take`, turns
//! the ownership check off. A thread with no ownership record (no routing)
//! is never refused for ownership, nor is an idle one: Meta refuses a
//! service message on an idle thread too, except from the escalation
//! partner; a thread idle after 24 hours without the customer is usually
//! outside the window too, which the window check refuses.
//!
//! **What the inbox cannot see.** The checks read what this app received.
//! In two setups that is not enough, and a check refuses a reply Meta
//! would take; turn it off there:
//!
//! - an app that receives a handover without standby visibility (Meta
//!   then sends the `control_passed` a `conversation_context` summary
//!   instead: `conversation-routing/conversation-context`) never saw the
//!   customer's messages to the previous owner. Its window reads from the
//!   messages it received, and can be closed while Meta's is open: a
//!   thread is passed only while it is active
//!   (`conversation-routing/thread-control`). Turn the window check off
//!   in that app's inbox (`ReplyChecks::all().window(false)`, and
//!   [`ReplyChecks::none`] for an escalation partner): Meta answers
//!   `131047` when its window is closed;
//! - an app that shares the number with a Meta Business Agent and no
//!   routing configuration receives standby copies, yet its service
//!   message makes it the active handler
//!   (`conversation-routing/standby-partners`): turn the ownership check
//!   off in its inbox.
//!
//! **Identity links.** A customer is stored under several keys (a thread
//! under their phone number from before BSUIDs, live messages under their
//! BSUID, a new BSUID after a number change). [`InboxSink`] records the
//! links between them with [`ConversationStore::link_identity`], so that
//! [`Inbox::identities`] and an erasure ([`Inbox::erase_all`]) reach every
//! key:
//!
//! - an inbound message (on `messages` or in standby) carrying both `from`
//!   and `from_user_id`: the phone number to the BSUID;
//! - a BSUID change (`UserIdChanged`, field `user_id_update`): the previous
//!   BSUID to the current one;
//! - a number change (a `system` message of type `user_changed_number` or
//!   `user_changed_user_id`): the sender's identity (`from_user_id`, else
//!   `from`) to the new one (`system.user_id`, else `system.wa_id`), and
//!   the new phone number to the new BSUID when both are given.
//!
//! An empty value, a value with U+0000 and a link of a value to itself are
//! never recorded. Parent BSUIDs are not linked: nothing is keyed by one.
//!
//! **Coexistence** (a merchant who keeps the WhatsApp Business app next to
//! the API, `embedded-signup/onboarding-business-app-users`):
//!
//! - Messages the merchant sends from the app arrive as
//!   `WebhookEvent::MessageEchoed` (`smb_message_echoes`) and are recorded
//!   as [`Direction::Outbound`] with status [`DeliveryStatus::Sent`] (the
//!   payload carries no status), in the customer's conversation: BSUID
//!   (`to_user_id`, else the contact's `user_id`), else the phone number
//!   (the contact's `wa_id`, else `to`, without `+`). An echoed revoke marks
//!   the original [`DeliveryStatus::Deleted`] if the business sent it.
//! - Synchronized chat history arrives as `WebhookEvent::HistorySynced`
//!   (`history`), in chunks that may come in any order; each message is
//!   recorded with its own device timestamp (bounded, see
//!   [`InboxSink::with_clock`]), so the order of the chunks does not
//!   change the thread (one exception: a revoke in a chunk that arrives
//!   before the chunk carrying its message leaves a tombstone in the
//!   message's place, see below), and a redelivered chunk changes nothing
//!   (a message id is stored once). Each chunk is one [`ConversationStore::append_synced`]
//!   batch, then its revokes. A message whose `from` is the business number is
//!   outbound, with the status its `history_context` names (`PENDING` is
//!   [`DeliveryStatus::Accepted`], `ERROR` is [`DeliveryStatus::Failed`],
//!   none or an unknown one is [`DeliveryStatus::Sent`]); any other is
//!   inbound ([`DeliveryStatus::Received`]). The conversation is the
//!   thread's BSUID, else its phone number. A declined sync (error
//!   `2593109`) records nothing.
//! - Synced history is recorded with [`ConversationStore::append_synced`]:
//!   it is part of the conversation (and may be its latest message), but a
//!   synced *inbound* message neither opens the customer service window
//!   ([`Inbox::window`]) nor counts as unread. Meta opens no window for a
//!   message sent before the business was onboarded, and the merchant has
//!   read these in the app.
//! - A media message arrives in the history as a `media_placeholder`
//!   without its media; its content follows in a later `history` webhook
//!   (`messages` for the customer's, `message_echoes` for the business's).
//!   That content replaces the placeholder's kind, text and payload
//!   ([`ConversationStore::fill_media_placeholder`]); the row keeps its
//!   conversation, direction, status and timestamp from the thread. A
//!   placeholder revoked in the meantime never gets it (the content is
//!   what its sender deleted). A content whose placeholder was never
//!   recorded becomes a row of its own.
//! - A revoke (live, echoed or synced) deletes only a message of its
//!   business number sent in its direction: a customer's revoke an inbound
//!   message, the business's an outbound one
//!   ([`ConversationStore::revoke`]). A revoke that arrives before its
//!   message leaves a tombstone ([`StoredMessage::REVOKED`]) under the
//!   message's id, so the message, when it comes, is never stored with the
//!   content its sender deleted. The tombstone is in the conversation's
//!   history, never in its summary ([`Inbox::conversations`]). A revoke
//!   whose sender (or recipient) has no usable id is skipped.
//! - One malformed history item never fails the delivery: an item without
//!   a direction or a conversation, with U+0000 in an id, or (when the
//!   whole `history` value failed its typed parse and arrived as
//!   `WebhookEvent::Unknown`) one that does not parse on its own, is
//!   skipped with a `tracing` warning that names its position, never its
//!   content. Storage errors still fail it, so Meta redelivers.
//!
//! History is recorded while the webhook request waits; Meta advises
//! capturing large history bodies first and processing them
//! asynchronously.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_client::Client;
use meta_whatsapp_client::messages::{
    DirectSendCategory, MessageContent, OutboundMessage, SendResponse,
};
use meta_whatsapp_core::clock::{Clock, SystemClock};
use meta_whatsapp_core::error::{SinkError, StorageError, ValidationError};
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId, UserId, WaId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, CustomerServiceWindow, DeliveryStatus,
    Direction, Erased, IdentityLink, StoredMessage, ThreadOwner, ThreadOwnership, WindowEvent,
    WindowEventKind,
};
use meta_whatsapp_core::{Error, Result};
use meta_whatsapp_webhooks::WebhookEvent;
use meta_whatsapp_webhooks::fields::calls::{
    Call, CallDirection, CallEventType, CallStatus, CallStatusValue,
};
use meta_whatsapp_webhooks::fields::coexistence::{
    HistoryMessage, HistoryMessageStatus, HistoryValue, MessageEcho, ThreadContext,
};
use meta_whatsapp_webhooks::fields::common::{Contact, Metadata};
use meta_whatsapp_webhooks::fields::messages::{
    InboundMessage, InteractiveReply, MessageContent as In, SystemMessageType,
};
use meta_whatsapp_webhooks::fields::routing::{HandoverType, MessagingHandoversValue, StandbyItem};
use meta_whatsapp_webhooks::fields::users::UserIdUpdate;
use serde_json::Value;
use time::OffsetDateTime;

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

/// Records inbound messages, status updates, and the coexistence events
/// (echoes of messages the merchant sent from the WhatsApp Business app,
/// synchronized history) into a [`ConversationStore`]; see the
/// [module docs](self) for how each is recorded. Idempotent: the store
/// ignores a message id it already has, and a status that does not
/// supersede the stored one.
///
/// Nothing in the content is replaced, U+0000 included: the stored
/// `kind`, `text` (the preview), `payload` (strings and object keys) and
/// status `error` keep every character the parsed event carried, and every
/// store in `meta_whatsapp_adapters` stores them (the owner's decision of 2026-09-25:
/// losslessly). A malformed history item is skipped (and logged without
/// its content). Storage errors still fail the delivery (Meta redelivers).
///
/// Synced history opens no customer service window and is never unread
/// ([`ConversationStore::append_synced`]), and a later media content fills
/// its recorded `media_placeholder`
/// ([`ConversationStore::fill_media_placeholder`]).
///
/// It also records the calls that reopen the window and the customer's
/// messages seen in standby as [window events](WindowEvent)
/// ([`ConversationStore::record_window_event`]), thread ownership under
/// Conversation Routing ([`ConversationStore::set_thread_owner`]) and the
/// links between a customer's identities
/// ([`ConversationStore::link_identity`]); the [module docs](self) say
/// which webhooks count.
///
/// **A handover's conversation.** A handover names the customer by phone
/// number only. It is recorded under the key that number leads to, in this
/// order:
///
/// 1. the identity links: from the phone number, the latest link whose
///    `previous` it is (by `at`), then from that identity the same way,
///    until no link leads further (at most 16 steps; a loop stops where it
///    closes). A phone number whose inbound messages carried a BSUID leads
///    to that BSUID, and a BSUID change on to the new one;
/// 2. else the synced address book: the BSUID (`user_id`) of the contact
///    [`ConversationStore::identities`] connects to the number whose
///    `phone_number` it is, the latest synced if several;
/// 3. else the phone number itself (a `wa_id` key).
///
/// A phone number recycled by the operator can lead to its earlier owner's
/// conversation (as it can connect them in
/// [`ConversationStore::identities`]); the local check that follows is
/// advisory, and [`ReplyChecks`] turns it off. To map handovers another
/// way, filter `ThreadControlChanged` out before this sink
/// (`meta_whatsapp_rs::adapters::sink::FilterSink`) and record them with
/// [`ConversationStore::set_thread_owner`] yourself.
///
/// Put a `meta_whatsapp_webhooks::DedupGuard` in front of the handler anyway — it
/// saves the store the work — and fan this sink out next to a broadcast
/// sink for live updates.
#[derive(Clone)]
pub struct InboxSink {
    store: Arc<dyn ConversationStore>,
    clock: Arc<dyn Clock>,
}

/// How far past the sink's clock a synced message's device timestamp may
/// be; later ones are recorded at that bound.
const DEVICE_CLOCK_SKEW: time::Duration = time::Duration::minutes(5);

impl fmt::Debug for InboxSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InboxSink").finish_non_exhaustive()
    }
}

/// What one webhook item does to the store.
enum Record {
    /// A new row.
    Append(Box<StoredMessage>),
    /// A revoke ([`ConversationStore::revoke`]): the original message, of
    /// the business number the revoke arrived on and sent in `direction`
    /// (a customer revokes what they sent, the business what it sent),
    /// becomes [`DeliveryStatus::Deleted`]; when it is not stored yet, a
    /// tombstone in `key` keeps its content out. Never a row of its own.
    Revoke {
        key: ConversationKey,
        original: MessageId,
        direction: Direction,
        at: OffsetDateTime,
    },
    /// Nothing to record, and why (logged without content).
    Skip(&'static str),
}

/// The row of a message. Content and ids are stored as given, U+0000
/// included.
fn row(
    id: &MessageId,
    conversation: ConversationKey,
    direction: Direction,
    content: &In,
    payload: Value,
    status: DeliveryStatus,
    timestamp: OffsetDateTime,
) -> Record {
    Record::Append(Box::new(StoredMessage {
        id: id.clone(),
        conversation,
        direction,
        kind: content.type_name().unwrap_or("unknown").to_owned(),
        text: preview(content),
        payload,
        status,
        timestamp,
        status_at: None,
        error: None,
    }))
}

/// A customer's message (`messages`, or a `history` media content item).
fn inbound_record(
    phone_number_id: &PhoneNumberId,
    contact: Option<&Contact>,
    message: &InboundMessage,
) -> serde_json::Result<Record> {
    let Some(conversation) = conversation_key(phone_number_id, contact, message) else {
        return Ok(Record::Skip("inbound message without a usable sender id"));
    };
    if let In::Revoke(r) = &message.content {
        return Ok(Record::Revoke {
            key: conversation,
            original: r.original_message_id.clone(),
            direction: Direction::Inbound,
            at: message.timestamp,
        });
    }
    Ok(row(
        &message.id,
        conversation,
        Direction::Inbound,
        &message.content,
        serde_json::to_value(message)?,
        DeliveryStatus::Received,
        message.timestamp,
    ))
}

/// A message the merchant sent from the WhatsApp Business app
/// (`smb_message_echoes`, or a `history` media content item).
fn echo_record(
    phone_number_id: &PhoneNumberId,
    contact: Option<&Contact>,
    echo: &MessageEcho,
) -> serde_json::Result<Record> {
    let Some(conversation) = echo_conversation_key(phone_number_id, contact, echo) else {
        return Ok(Record::Skip("message echo without a usable recipient id"));
    };
    if let In::Revoke(r) = &echo.content {
        return Ok(Record::Revoke {
            key: conversation,
            original: r.original_message_id.clone(),
            direction: Direction::Outbound,
            at: echo.timestamp,
        });
    }
    // The echo carries no status: the app sent it.
    Ok(row(
        &echo.id,
        conversation,
        Direction::Outbound,
        &echo.content,
        serde_json::to_value(echo)?,
        DeliveryStatus::Sent,
        echo.timestamp,
    ))
}

/// The chat thread a history message belongs to.
struct Thread<'a> {
    /// `threads[].id`: the customer's phone number, when Meta may share it.
    id: Option<&'a str>,
    /// `threads[].context`: the customer's BSUID and phone number.
    context: Option<&'a ThreadContext>,
}

/// A message of a synchronized chat thread (`history[].threads[].messages[]`).
fn history_record(
    phone_number_id: &PhoneNumberId,
    business_number: &str,
    thread: &Thread<'_>,
    message: &HistoryMessage,
) -> serde_json::Result<Record> {
    let Some(direction) = history_direction(business_number, message) else {
        return Ok(Record::Skip(
            "history message whose `from` and `to` do not say who sent it",
        ));
    };
    let Some(conversation) = history_conversation_key(phone_number_id, thread, direction, message)
    else {
        return Ok(Record::Skip("history message without a usable customer id"));
    };
    if let In::Revoke(r) = &message.content {
        return Ok(Record::Revoke {
            key: conversation,
            original: r.original_message_id.clone(),
            direction,
            at: message.timestamp,
        });
    }
    let status = match direction {
        Direction::Inbound => DeliveryStatus::Received,
        Direction::Outbound => synced_status(message),
    };
    Ok(row(
        &message.id,
        conversation,
        direction,
        &message.content,
        serde_json::to_value(message)?,
        status,
        message.timestamp,
    ))
}

/// Who sent a history message (`webhooks/reference/history`): `from` is the
/// business number for a message the business sent and the customer's
/// number otherwise; a customer who hides their number is identified by
/// `from_user_id` alone; `to` is set only on messages the business sent.
fn history_direction(business_number: &str, message: &HistoryMessage) -> Option<Direction> {
    let from = message.from.as_deref().filter(|f| !f.trim().is_empty());
    match from {
        Some(from) if same_number(from, business_number) => Some(Direction::Outbound),
        Some(_) => Some(Direction::Inbound),
        None if message.from_user_id.is_some() => Some(Direction::Inbound),
        None if message.to.is_some() => Some(Direction::Outbound),
        None => None,
    }
}

/// Status of a synced outbound message, from `history_context.status`.
fn synced_status(message: &HistoryMessage) -> DeliveryStatus {
    let Some(context) = &message.history_context else {
        return DeliveryStatus::Sent;
    };
    let status = context.status.as_str();
    let is = |s: &HistoryMessageStatus| status.eq_ignore_ascii_case(s.as_str());
    if is(&HistoryMessageStatus::Pending) {
        DeliveryStatus::Accepted
    } else if is(&HistoryMessageStatus::Delivered) {
        DeliveryStatus::Delivered
    } else if is(&HistoryMessageStatus::Read) {
        DeliveryStatus::Read
    } else if is(&HistoryMessageStatus::Played) {
        DeliveryStatus::Played
    } else if is(&HistoryMessageStatus::Error) {
        DeliveryStatus::Failed
    } else {
        // `SENT`, or a value Meta added later.
        DeliveryStatus::Sent
    }
}

/// Two phone numbers are the same when their digits are (Meta prints them
/// with and without `+`).
fn same_number(a: &str, b: &str) -> bool {
    fn digits(s: &str) -> impl Iterator<Item = char> + '_ {
        s.chars().filter(char::is_ascii_digit)
    }
    digits(a).next().is_some() && digits(a).eq(digits(b))
}

/// A phone number as a conversation contact: without `+` (the `wa_id`
/// form), or `None` when nothing is left.
fn bare_phone(phone: &str) -> Option<String> {
    let bare = phone.trim().trim_start_matches('+');
    (!bare.is_empty()).then(|| bare.to_owned())
}

/// Whether `id` can name someone in a record of the inbox's own making (a
/// window event, an ownership record, an identity link): not empty (an
/// empty value names no one, the rule of roadmap L5) and without U+0000
/// (Meta never assigns one, and the Postgres store refuses it in an
/// identifier, which would fail every redelivery).
fn usable(id: &str) -> bool {
    !id.trim().is_empty() && !id.contains('\0')
}

/// The customer on a call: their BSUID (the first of `bsuids`, then the
/// contact's `user_id`), else their phone number (the contact's `wa_id`,
/// then the first of `phones`), never the business's own number
/// `business_number` (the calling pages disagree on which of `from` and
/// `to` is the customer).
fn call_key(
    phone_number_id: &PhoneNumberId,
    business_number: &str,
    contact: Option<&Contact>,
    bsuids: [Option<&UserId>; 2],
    phones: [Option<&str>; 2],
) -> Option<ConversationKey> {
    let bsuid = bsuids
        .into_iter()
        .chain([contact.and_then(|c| c.user_id.as_ref())])
        .flatten()
        .map(UserId::as_str)
        .find(|id| usable(id));
    let who = match bsuid {
        Some(user) => user.to_owned(),
        None => [contact.and_then(|c| c.wa_id.as_ref()).map(WaId::as_str)]
            .into_iter()
            .chain(phones)
            .flatten()
            .filter_map(bare_phone)
            .find(|phone| usable(phone) && !same_number(phone, business_number))?,
    };
    Some(ConversationKey::new(phone_number_id.clone(), who))
}

/// Whether a `calls[]` event opens the customer service window, and as what
/// and when (see the [module docs](self)).
fn call_window(call: &Call) -> Option<(WindowEventKind, OffsetDateTime)> {
    let earliest =
        |start: Option<OffsetDateTime>| start.map_or(call.timestamp, |s| s.min(call.timestamp));
    match (call.direction.as_ref()?, &call.event) {
        (CallDirection::UserInitiated, CallEventType::Connect | CallEventType::CallCreated) => {
            Some((WindowEventKind::CustomerCall, call.timestamp))
        }
        (CallDirection::UserInitiated, CallEventType::Terminate) => {
            Some((WindowEventKind::CustomerCall, earliest(call.start_time)))
        }
        (CallDirection::BusinessInitiated, CallEventType::Terminate) => call
            .start_time
            .map(|start| (WindowEventKind::CallAccepted, earliest(Some(start)))),
        _ => None,
    }
}

/// The new owner's role and app a handover names.
fn handover_owner(
    update: &MessagingHandoversValue,
) -> (Option<String>, Option<meta_whatsapp_core::ids::AppId>) {
    let handover = update.handover();
    (
        handover
            .and_then(|h| h.new_owner_role.as_ref())
            .map(|role| role.as_str().to_owned()),
        handover.and_then(|h| h.new_owner_app_id.clone()),
    )
}

/// How many identity links [`InboxSink`] follows from a handover's phone
/// number at most.
const MAX_LINK_STEPS: usize = 16;

/// How many of a conversation's latest messages [`Inbox::thread_owner`]
/// reads for the latest one that claims the thread ([`claims_thread`]).
const CLAIM_SCAN: usize = 20;

/// Whether receiving `message` on the `messages` field means this app owns
/// the thread (`conversation-routing/thread-control`, "Tracking
/// ownership"): an inbound message recorded live (not a revoke's tombstone,
/// a redacted message or a history placeholder, as for
/// [`ConversationStore::last_inbound_at`]), except a customer's answer to a
/// call permission request, which reaches the Incoming Call primary and the
/// standby partners and "does not change thread ownership"
/// (`conversation-routing/calling-webhooks`).
fn claims_thread(message: &StoredMessage) -> bool {
    let own_kind = [
        StoredMessage::REVOKED,
        StoredMessage::ERASED,
        StoredMessage::MEDIA_PLACEHOLDER,
    ]
    .contains(&message.kind.as_str());
    let permission_reply = message.payload["interactive"]["type"] == "call_permission_reply";
    message.direction == Direction::Inbound && !own_kind && !permission_reply
}

/// How long before a standby copy's timestamp [`InboxSink`] dates the
/// ownership by another app that the copy records: that app owned the
/// thread before the customer's message reached it. Meta's timestamps are
/// whole seconds, so a handover of the copy's second is always the later
/// record ([`ConversationStore::set_thread_owner`] keeps it whichever
/// replica writes last), and so is a message on `messages` of that second
/// ([`Inbox::thread_owner`]: this app then).
const STANDBY_OWNER_LEAD: time::Duration = time::Duration::milliseconds(1);

/// The customer an echo went to: BSUID first, then phone number.
fn echo_conversation_key(
    phone_number_id: &PhoneNumberId,
    contact: Option<&Contact>,
    echo: &MessageEcho,
) -> Option<ConversationKey> {
    let bsuid = echo
        .to_user_id
        .as_ref()
        .or_else(|| contact.and_then(|c| c.user_id.as_ref()));
    let who = match bsuid {
        Some(user) => user.to_string(),
        None => bare_phone(
            contact
                .and_then(|c| c.wa_id.as_ref())
                .or(echo.to.as_ref())?
                .as_str(),
        )?,
    };
    Some(ConversationKey::new(phone_number_id.clone(), who))
}

/// The customer of a history thread: its BSUID (the thread's, else the
/// sender's on an inbound message), else its phone number (the thread's,
/// else the message's `from` or `to`).
fn history_conversation_key(
    phone_number_id: &PhoneNumberId,
    thread: &Thread<'_>,
    direction: Direction,
    message: &HistoryMessage,
) -> Option<ConversationKey> {
    let inbound = direction == Direction::Inbound;
    let bsuid = thread
        .context
        .and_then(|c| c.user_id.as_ref())
        .or_else(|| message.from_user_id.as_ref().filter(|_| inbound));
    let who = if let Some(user) = bsuid {
        user.to_string()
    } else {
        let own = if inbound { &message.from } else { &message.to };
        let phone = thread
            .context
            .and_then(|c| c.wa_id.as_ref().map(WaId::as_str))
            .or(thread.id)
            .or(own.as_deref())?;
        bare_phone(phone)?
    };
    Some(ConversationKey::new(phone_number_id.clone(), who))
}

/// The `contacts` of a history value by the digits of their `wa_id`,
/// indexed once: a media content item without a BSUID of its own finds
/// its customer's BSUID there, and a value can carry many items. (An item
/// that has a BSUID is keyed by it; its contact would add nothing.)
struct Contacts<'a> {
    by_phone: HashMap<String, &'a Contact>,
}

impl<'a> Contacts<'a> {
    fn new(contacts: &'a [Contact]) -> Self {
        let mut by_phone = HashMap::new();
        for contact in contacts {
            if let Some(wa_id) = &contact.wa_id {
                let digits = phone_digits(wa_id.as_str());
                if !digits.is_empty() {
                    // The first contact of a number wins, as Meta lists them.
                    by_phone.entry(digits).or_insert(contact);
                }
            }
        }
        Self { by_phone }
    }

    /// The contact of phone number `phone`, in any format.
    fn find(&self, phone: Option<&str>) -> Option<&'a Contact> {
        self.by_phone.get(&phone_digits(phone?)).copied()
    }
}

/// The digits of a phone number.
fn phone_digits(phone: &str) -> String {
    phone.chars().filter(char::is_ascii_digit).collect()
}

/// A history item must not fail the delivery: skip it when a Meta-assigned
/// id it would be stored under carries U+0000, which the Postgres store
/// refuses (ids are `TEXT`) on every redelivery. Meta never assigns one;
/// content with U+0000 is stored as it is.
fn guard_synced(record: Record) -> Record {
    match &record {
        Record::Append(m)
            if m.id.as_str().contains('\0') || m.conversation.contact.contains('\0') =>
        {
            Record::Skip("history message with U+0000 in its id or its customer's id")
        }
        Record::Revoke { key, original, .. }
            if original.as_str().contains('\0') || key.contact.contains('\0') =>
        {
            Record::Skip("history revoke with U+0000 in the revoked id or its customer's id")
        }
        _ => record,
    }
}

/// `value[key]` as an array, or nothing.
fn items<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// A message that could not be serialized fails the delivery, like a
/// storage error.
fn serialization(e: serde_json::Error) -> SinkError {
    SinkError::Delivery(anyhow::Error::new(e))
}

/// Where a history item sits in its change: indexes, never content.
#[derive(Debug, Clone, Copy)]
enum At {
    /// `history[c].threads[t].messages[m]`.
    Thread(usize, usize, usize),
    /// `<list>[i]`: `messages` or `message_echoes`.
    List(&'static str, usize),
}

impl fmt::Display for At {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Thread(c, t, m) => write!(f, "history[{c}].threads[{t}].messages[{m}]"),
            Self::List(list, i) => write!(f, "{list}[{i}]"),
        }
    }
}

/// The items of one history chunk (or of a value's media contents), ready
/// for the store: one batch of rows, then the revokes, which then find a
/// message of the same chunk.
#[derive(Default)]
struct Batch {
    rows: Vec<StoredMessage>,
    revokes: Vec<(ConversationKey, MessageId, Direction, OffsetDateTime)>,
}

impl InboxSink {
    /// Record into `store`.
    pub fn new(store: Arc<dyn ConversationStore>) -> Self {
        Self {
            store,
            clock: Arc::new(SystemClock),
        }
    }

    /// Use `clock` to bound synced device timestamps (tests): a synced
    /// message dated more than 5 minutes past it is recorded at that bound,
    /// so a phone with a wrong clock cannot pin a conversation to the top
    /// of the inbox. The payload keeps Meta's timestamp.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The latest timestamp a synced item is recorded with.
    fn synced_bound(&self) -> OffsetDateTime {
        self.clock.now().saturating_add(DEVICE_CLOCK_SKEW)
    }

    /// Apply the `record` of a live item (`messages`, `smb_message_echoes`)
    /// of type `kind`: an inbound message opens the window and is unread.
    async fn apply_live(
        &self,
        kind: Option<&str>,
        record: Record,
    ) -> std::result::Result<(), SinkError> {
        match record {
            Record::Append(message) => {
                self.store.append(*message).await.map_err(delivery)?;
            }
            Record::Revoke {
                key,
                original,
                direction,
                at,
            } => {
                self.store
                    .revoke(&key, &original, direction, at)
                    .await
                    .map_err(delivery)?;
            }
            Record::Skip(reason) => {
                tracing::warn!(
                    kind = kind.unwrap_or("unknown"),
                    reason,
                    "webhook message not recorded"
                );
            }
        }
        Ok(())
    }

    /// Add a history item of type `kind` to `batch`, or skip it with a
    /// warning that names its position `at`: a history item never fails the
    /// delivery on its own. Timestamps are bounded (see
    /// [`InboxSink::with_clock`]).
    fn collect(
        batch: &mut Batch,
        kind: Option<&str>,
        at: At,
        record: serde_json::Result<Record>,
        bound: OffsetDateTime,
    ) {
        let record = match record {
            Ok(record) => guard_synced(record),
            Err(e) => {
                tracing::warn!(
                    item = %at,
                    category = ?e.classify(),
                    "history item could not be serialized; skipped"
                );
                return;
            }
        };
        match record {
            Record::Append(mut message) => {
                message.timestamp = message.timestamp.min(bound);
                batch.rows.push(*message);
            }
            Record::Revoke {
                key,
                original,
                direction,
                at: revoked_at,
            } => batch
                .revokes
                .push((key, original, direction, revoked_at.min(bound))),
            Record::Skip(reason) => tracing::warn!(
                item = %at,
                kind = kind.unwrap_or("unknown"),
                reason,
                "history item skipped"
            ),
        }
    }

    /// Store the revokes of a batch whose rows are stored.
    async fn store_revokes(
        &self,
        revokes: Vec<(ConversationKey, MessageId, Direction, OffsetDateTime)>,
    ) -> std::result::Result<(), SinkError> {
        for (key, original, direction, at) in revokes {
            self.store
                .revoke(&key, &original, direction, at)
                .await
                .map_err(delivery)?;
        }
        Ok(())
    }

    /// Store a history chunk: its messages in one
    /// [`ConversationStore::append_synced`] call, then its revokes.
    async fn store_chunk(&self, batch: Batch) -> std::result::Result<(), SinkError> {
        if !batch.rows.is_empty() {
            self.store
                .append_synced(batch.rows)
                .await
                .map_err(delivery)?;
        }
        self.store_revokes(batch.revokes).await
    }

    /// Store media contents: in one [`ConversationStore::append_synced`]
    /// call, then, for each whose id was stored already (the placeholder a
    /// thread recorded), [`ConversationStore::fill_media_placeholder`].
    /// Append first: whichever of a placeholder and its content arrives
    /// first, the row ends with the content.
    async fn store_contents(
        &self,
        phone_number_id: &PhoneNumberId,
        batch: Batch,
    ) -> std::result::Result<(), SinkError> {
        if !batch.rows.is_empty() {
            let contents: Vec<_> = batch
                .rows
                .iter()
                .map(|m| {
                    (
                        m.id.clone(),
                        m.kind.clone(),
                        m.text.clone(),
                        m.payload.clone(),
                    )
                })
                .collect();
            let inserted = self
                .store
                .append_synced(batch.rows)
                .await
                .map_err(delivery)?;
            if inserted.len() != contents.len() {
                return Err(SinkError::Delivery(anyhow::anyhow!(
                    "the conversation store answered {} times for {} messages",
                    inserted.len(),
                    contents.len()
                )));
            }
            for ((id, kind, text, payload), inserted) in contents.into_iter().zip(inserted) {
                if !inserted {
                    self.store
                        .fill_media_placeholder(phone_number_id, &id, kind, text, payload)
                        .await
                        .map_err(delivery)?;
                }
            }
        }
        self.store_revokes(batch.revokes).await
    }

    /// A `history` change: chat threads, media contents, or the error of a
    /// declined sync. One store round trip per chunk.
    async fn record_history(
        &self,
        phone_number_id: &PhoneNumberId,
        history: &HistoryValue,
    ) -> std::result::Result<(), SinkError> {
        let bound = self.synced_bound();
        let business_number = history.metadata.display_phone_number.as_str();
        for (c, chunk) in history.history.iter().enumerate() {
            for error in &chunk.errors {
                // 2593109: the business declined to share its history.
                tracing::info!(
                    chunk = c,
                    code = error.code,
                    "history chunk carries an error; nothing to record"
                );
            }
            if let Some(m) = &chunk.metadata {
                tracing::debug!(
                    chunk = c,
                    phase = m.phase,
                    chunk_order = m.chunk_order,
                    progress = m.progress,
                    threads = chunk.threads.len(),
                    "recording a history chunk"
                );
            }
            let mut batch = Batch::default();
            for (t, thread) in chunk.threads.iter().enumerate() {
                let view = Thread {
                    id: thread.id.as_deref(),
                    context: thread.context.as_ref(),
                };
                for (m, message) in thread.messages.iter().enumerate() {
                    let record = history_record(phone_number_id, business_number, &view, message);
                    let kind = message.content.type_name();
                    Self::collect(&mut batch, kind, At::Thread(c, t, m), record, bound);
                }
            }
            self.store_chunk(batch).await?;
        }
        self.record_media_contents(
            phone_number_id,
            &history.contacts,
            &history.messages,
            &history.message_echoes,
        )
        .await
    }

    /// The media contents Meta sends after `media_placeholder`s: the
    /// customer's (`messages`) and the business's (`message_echoes`).
    async fn record_media_contents(
        &self,
        phone_number_id: &PhoneNumberId,
        contacts: &[Contact],
        messages: &[InboundMessage],
        echoes: &[MessageEcho],
    ) -> std::result::Result<(), SinkError> {
        if messages.is_empty() && echoes.is_empty() {
            return Ok(());
        }
        let bound = self.synced_bound();
        let contacts = Contacts::new(contacts);
        let mut batch = Batch::default();
        for (i, message) in messages.iter().enumerate() {
            let contact = contacts.find(message.from.as_ref().map(WaId::as_str));
            let record = inbound_record(phone_number_id, contact, message);
            let kind = message.message_type();
            Self::collect(&mut batch, kind, At::List("messages", i), record, bound);
        }
        for (i, echo) in echoes.iter().enumerate() {
            let contact = contacts.find(echo.to.as_ref().map(WaId::as_str));
            let record = echo_record(phone_number_id, contact, echo);
            let kind = echo.content.type_name();
            Self::collect(
                &mut batch,
                kind,
                At::List("message_echoes", i),
                record,
                bound,
            );
        }
        self.store_contents(phone_number_id, batch).await
    }

    /// A `history` value that failed its typed parse as a whole (one
    /// malformed item is enough): record every item that parses on its
    /// own, and skip the rest.
    async fn record_unparsed_history(&self, raw: &Value) -> std::result::Result<(), SinkError> {
        let Some(metadata) = raw
            .get("metadata")
            .and_then(|m| serde_json::from_value::<Metadata>(m.clone()).ok())
        else {
            tracing::warn!("history value without a usable `metadata`; nothing recorded");
            return Ok(());
        };
        let bound = self.synced_bound();
        let phone_number_id = &metadata.phone_number_id;
        let business_number = metadata.display_phone_number.as_str();
        for (c, chunk) in items(raw, "history").enumerate() {
            let mut batch = Batch::default();
            for (t, thread) in items(chunk, "threads").enumerate() {
                // A thread whose customer cannot be read is skipped whole:
                // guessing its conversation could split or mix customers.
                let context = match thread.get("context") {
                    None | Some(Value::Null) => None,
                    Some(context) => {
                        if let Ok(context) =
                            serde_json::from_value::<ThreadContext>(context.clone())
                        {
                            Some(context)
                        } else {
                            tracing::warn!(
                                item = %format!("history[{c}].threads[{t}]"),
                                "history thread with a malformed `context`; skipped"
                            );
                            continue;
                        }
                    }
                };
                let view = Thread {
                    id: thread.get("id").and_then(Value::as_str),
                    context: context.as_ref(),
                };
                for (m, message) in items(thread, "messages").enumerate() {
                    match serde_json::from_value::<HistoryMessage>(message.clone()) {
                        Ok(message) => {
                            let record =
                                history_record(phone_number_id, business_number, &view, &message);
                            let kind = message.content.type_name();
                            Self::collect(&mut batch, kind, At::Thread(c, t, m), record, bound);
                        }
                        Err(e) => tracing::warn!(
                            item = %At::Thread(c, t, m),
                            category = ?e.classify(),
                            "history message does not parse; skipped"
                        ),
                    }
                }
            }
            self.store_chunk(batch).await?;
        }
        let contacts: Vec<Contact> = items(raw, "contacts")
            .filter_map(|c| serde_json::from_value(c.clone()).ok())
            .collect();
        let mut messages = Vec::new();
        for (i, item) in items(raw, "messages").enumerate() {
            match serde_json::from_value::<InboundMessage>(item.clone()) {
                Ok(message) => messages.push(message),
                Err(e) => unparsable("messages", i, &e),
            }
        }
        let mut echoes = Vec::new();
        for (i, item) in items(raw, "message_echoes").enumerate() {
            match serde_json::from_value::<MessageEcho>(item.clone()) {
                Ok(echo) => echoes.push(echo),
                Err(e) => unparsable("message_echoes", i, &e),
            }
        }
        self.record_media_contents(phone_number_id, &contacts, &messages, &echoes)
            .await
    }
}

/// Window events, thread ownership and identity links (roadmap L7).
impl InboxSink {
    /// Link `previous` to `current` on `phone_number_id`, at `at`, unless
    /// either is unusable or they are the same.
    async fn link(
        &self,
        phone_number_id: &PhoneNumberId,
        previous: &str,
        current: &str,
        at: OffsetDateTime,
    ) -> std::result::Result<(), SinkError> {
        if !usable(previous) || !usable(current) || previous == current {
            return Ok(());
        }
        self.store
            .link_identity(IdentityLink::new(
                phone_number_id.clone(),
                previous,
                current,
                at,
            ))
            .await
            .map_err(delivery)?;
        Ok(())
    }

    /// The identity links an inbound message carries: its phone number to
    /// its BSUID, and a number change's old identity to the new one.
    async fn link_message(
        &self,
        phone_number_id: &PhoneNumberId,
        message: &InboundMessage,
    ) -> std::result::Result<(), SinkError> {
        let at = message.timestamp;
        let from = message.from.as_ref().map(WaId::as_str);
        let from_user = message.from_user_id.as_ref().map(UserId::as_str);
        if let (Some(phone), Some(user)) = (from, from_user) {
            self.link(phone_number_id, phone, user, at).await?;
        }
        let In::System(system) = &message.content else {
            return Ok(());
        };
        let changed = system.system_type.as_ref().is_some_and(|t| {
            *t == SystemMessageType::UserChangedNumber || *t == SystemMessageType::UserChangedUserId
        });
        if !changed {
            return Ok(());
        }
        let new_phone = system.wa_id.as_ref().map(WaId::as_str);
        let new_user = system.user_id.as_ref().map(UserId::as_str);
        let previous = from_user.filter(|id| usable(id)).or(from);
        let current = new_user.filter(|id| usable(id)).or(new_phone);
        if let (Some(previous), Some(current)) = (previous, current) {
            self.link(phone_number_id, previous, current, at).await?;
        }
        if let (Some(phone), Some(user)) = (new_phone, new_user) {
            self.link(phone_number_id, phone, user, at).await?;
        }
        Ok(())
    }

    /// A BSUID change (`user_id_update`): the previous BSUID to the current.
    async fn link_user_id(
        &self,
        phone_number_id: &PhoneNumberId,
        update: &UserIdUpdate,
    ) -> std::result::Result<(), SinkError> {
        self.link(
            phone_number_id,
            update.user_id.previous.as_str(),
            update.user_id.current.as_str(),
            update.timestamp,
        )
        .await
    }

    /// Record `event` unless its conversation or id is unusable.
    async fn window_event(&self, event: WindowEvent) -> std::result::Result<(), SinkError> {
        if !usable(&event.conversation.contact) || !usable(&event.id) {
            tracing::warn!(
                kind = %event.kind,
                "window event without a usable customer or id; not recorded"
            );
            return Ok(());
        }
        self.store
            .record_window_event(event)
            .await
            .map_err(delivery)?;
        Ok(())
    }

    /// A `calls[]` event: a window event when it is the customer's call.
    async fn record_call(
        &self,
        phone_number_id: &PhoneNumberId,
        business_number: &str,
        contact: Option<&Contact>,
        call: &Call,
    ) -> std::result::Result<(), SinkError> {
        let Some((kind, at)) = call_window(call) else {
            return Ok(());
        };
        let Some(conversation) = call_key(
            phone_number_id,
            business_number,
            contact,
            [call.from_user_id.as_ref(), call.to_user_id.as_ref()],
            [call.from.as_deref(), call.to.as_deref()],
        ) else {
            tracing::warn!(%kind, "call without a usable customer id; not recorded");
            return Ok(());
        };
        self.window_event(WindowEvent {
            conversation,
            kind,
            id: call.id.to_string(),
            at,
        })
        .await
    }

    /// A call status: a window event when the customer accepted.
    async fn record_call_status(
        &self,
        phone_number_id: &PhoneNumberId,
        business_number: &str,
        contact: Option<&Contact>,
        status: &CallStatus,
    ) -> std::result::Result<(), SinkError> {
        if status.status != CallStatusValue::Accepted {
            return Ok(());
        }
        let kind = WindowEventKind::CallAccepted;
        let Some(conversation) = call_key(
            phone_number_id,
            business_number,
            contact,
            [status.recipient_user_id.as_ref(), None],
            [status.recipient_id.as_deref(), None],
        ) else {
            tracing::warn!(%kind, "call status without a usable customer id; not recorded");
            return Ok(());
        };
        self.window_event(WindowEvent {
            conversation,
            kind,
            id: status.id.to_string(),
            at: status.timestamp,
        })
        .await
    }

    /// A standby copy of a customer's message: a window event, ownership by
    /// another app, and the identity links it carries. A revoke is not a
    /// message (as on the `messages` field).
    async fn record_standby(
        &self,
        phone_number_id: &PhoneNumberId,
        contact: Option<&Contact>,
        message: &InboundMessage,
    ) -> std::result::Result<(), SinkError> {
        if matches!(message.content, In::Revoke(_)) {
            return Ok(());
        }
        self.link_message(phone_number_id, message).await?;
        let Some(conversation) = conversation_key(phone_number_id, contact, message) else {
            tracing::warn!("standby message without a usable sender id; not recorded");
            return Ok(());
        };
        let group = message.group_id.is_some();
        self.window_event(WindowEvent {
            conversation: conversation.clone(),
            kind: WindowEventKind::StandbyMessage,
            id: message.id.to_string(),
            at: message.timestamp,
        })
        .await?;
        if group || !usable(&conversation.contact) {
            return Ok(());
        }
        self.observed_in_standby(&conversation, message.timestamp)
            .await
    }

    /// Another app owns `key`'s thread since just before `at` (a standby
    /// copy: [`STANDBY_OWNER_LEAD`] before it), unless a record of that
    /// second or later is stored. The stored owner's role and app stay
    /// when it was another app already.
    ///
    /// The read and the write are two calls, so another replica can store
    /// a handover of the same second between them. Dating the record
    /// before the copy's second keeps the handover anyway:
    /// [`ConversationStore::set_thread_owner`] refuses a record older than
    /// the stored one, whichever replica writes last.
    async fn observed_in_standby(
        &self,
        key: &ConversationKey,
        at: OffsetDateTime,
    ) -> std::result::Result<(), SinkError> {
        let stored = self.store.thread_owner(key).await.map_err(delivery)?;
        if stored.as_ref().is_some_and(|s| s.since >= at) {
            return Ok(());
        }
        let (role, app_id) = match stored {
            Some(s) if s.owner == ThreadOwner::AnotherApp => (s.role, s.app_id),
            _ => (None, None),
        };
        self.store
            .set_thread_owner(
                key,
                ThreadOwnership {
                    owner: ThreadOwner::AnotherApp,
                    role,
                    app_id,
                    since: at.saturating_sub(STANDBY_OWNER_LEAD),
                },
            )
            .await
            .map_err(delivery)?;
        Ok(())
    }

    /// A handover: this app owns the thread (`control_passed`) or another
    /// one does (`control_taken`), under the conversation its phone number
    /// leads to.
    async fn record_handover(
        &self,
        phone_number_id: &PhoneNumberId,
        update: &MessagingHandoversValue,
    ) -> std::result::Result<(), SinkError> {
        let owner = match &update.handover_type {
            HandoverType::ControlPassed => ThreadOwner::ThisApp,
            HandoverType::ControlTaken => ThreadOwner::AnotherApp,
            other => {
                tracing::warn!(handover = %other, "handover of an unknown type; not recorded");
                return Ok(());
            }
        };
        let phone = update
            .sender
            .as_ref()
            .and_then(|s| s.phone_number.as_ref())
            .and_then(|p| bare_phone(p.as_str()))
            .filter(|p| usable(p));
        let Some(phone) = phone else {
            tracing::warn!("handover without the customer's phone number; not recorded");
            return Ok(());
        };
        let key = self
            .handover_key(phone_number_id, &phone)
            .await
            .map_err(delivery)?;
        let (role, app_id) = handover_owner(update);
        self.store
            .set_thread_owner(
                &key,
                ThreadOwnership {
                    owner,
                    role,
                    app_id,
                    since: update.timestamp,
                },
            )
            .await
            .map_err(delivery)?;
        Ok(())
    }

    /// The conversation a handover's phone number leads to (see
    /// [`InboxSink`]): along the identity links, else through a synced
    /// contact, else the phone number itself.
    async fn handover_key(
        &self,
        phone_number_id: &PhoneNumberId,
        phone: &str,
    ) -> std::result::Result<ConversationKey, StorageError> {
        let key = |contact: &str| ConversationKey::new(phone_number_id.clone(), contact);
        let mut current = phone.to_owned();
        let mut seen = BTreeSet::from([current.clone()]);
        for _ in 0..MAX_LINK_STEPS {
            // Oldest first: the last one is the latest.
            let next = self
                .store
                .identity_links(&key(&current))
                .await?
                .into_iter()
                .rev()
                .find(|link| link.previous == current && usable(&link.current))
                .map(|link| link.current);
            match next {
                Some(next) if seen.insert(next.clone()) => current = next,
                _ => break,
            }
        }
        if current != phone {
            return Ok(key(&current));
        }
        let mut latest: Option<(OffsetDateTime, String)> = None;
        for id in self.store.identities(&key(phone)).await? {
            if id == phone || !usable(&id) {
                continue;
            }
            let Some(contact) = self.store.contact(&key(&id)).await? else {
                continue;
            };
            let theirs = contact
                .phone_number
                .as_deref()
                .is_some_and(|number| same_number(number, phone));
            let user = contact.user_id.as_ref().map(UserId::as_str);
            if let Some(user) = user.filter(|u| theirs && usable(u))
                && latest
                    .as_ref()
                    .is_none_or(|(at, _)| contact.synced_at > *at)
            {
                latest = Some((contact.synced_at, user.to_owned()));
            }
        }
        Ok(latest.map_or_else(|| key(phone), |(_, user)| key(&user)))
    }
}

/// Log a skipped history item by position and error category: serde's
/// message would quote the offending value.
fn unparsable(list: &'static str, index: usize, e: &serde_json::Error) {
    tracing::warn!(
        item = %At::List(list, index),
        category = ?e.classify(),
        "history item does not parse; skipped"
    );
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
                let record = inbound_record(&phone_number_id, contact.as_ref(), &message)
                    .map_err(serialization)?;
                self.apply_live(message.message_type(), record).await?;
                self.link_message(&phone_number_id, &message).await
            }
            WebhookEvent::MessageEchoed {
                phone_number_id,
                contact,
                echo,
                ..
            } => {
                let record = echo_record(&phone_number_id, contact.as_ref(), &echo)
                    .map_err(serialization)?;
                self.apply_live(echo.content.type_name(), record).await
            }
            WebhookEvent::HistorySynced {
                phone_number_id,
                history,
                ..
            } => self.record_history(&phone_number_id, &history).await,
            // One malformed item fails a `history` value's typed parse as a
            // whole: recover the items that parse on their own.
            WebhookEvent::Unknown { field, raw, .. } if field == "history" => {
                self.record_unparsed_history(&raw).await
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
                    serde_json::to_value(&status.errors).ok()
                };
                self.store
                    .update_status(&phone_number_id, &status.id, new, status.timestamp, error)
                    .await
                    .map_err(delivery)?;
                Ok(())
            }
            other => self.record_routing(other).await,
        }
    }
}

impl InboxSink {
    /// The events that are not messages: calls, standby copies, handovers
    /// and BSUID changes (see the [module docs](self)); every other event
    /// is ignored.
    async fn record_routing(&self, event: WebhookEvent) -> std::result::Result<(), SinkError> {
        match event {
            WebhookEvent::StandbyObserved {
                phone_number_id,
                contact,
                item,
                ..
            } => match item.as_ref() {
                StandbyItem::Message(message) => {
                    self.record_standby(&phone_number_id, contact.as_ref(), message)
                        .await
                }
                // The owner's sends and their receipts: no window, and no
                // owner of their own (see the module docs).
                _ => Ok(()),
            },
            WebhookEvent::CallUpdated {
                phone_number_id,
                display_phone_number,
                contact,
                call,
                ..
            } => {
                self.record_call(
                    &phone_number_id,
                    &display_phone_number,
                    contact.as_ref(),
                    &call,
                )
                .await
            }
            WebhookEvent::CallStatusUpdated {
                phone_number_id,
                display_phone_number,
                contact,
                status,
                ..
            } => {
                self.record_call_status(
                    &phone_number_id,
                    &display_phone_number,
                    contact.as_ref(),
                    &status,
                )
                .await
            }
            WebhookEvent::ThreadControlChanged {
                phone_number_id,
                update,
                ..
            } => self.record_handover(&phone_number_id, &update).await,
            WebhookEvent::UserIdChanged {
                phone_number_id,
                update,
                ..
            } => self.link_user_id(&phone_number_id, &update).await,
            _ => Ok(()),
        }
    }
}

/// The `field` of the [`ValidationError`] with which [`Inbox::send`] (and
/// [`Inbox::reply`]) refuses a service message, before any request, when
/// another app owns the thread under Conversation Routing. Match with
/// [`is_thread_owned_elsewhere`] rather than on the string. Its
/// [`Error::kind`] is `InvalidParameter`: Meta documents no error code of
/// its own for a service message from a non-owner.
pub const THREAD_OWNER: &str = "thread_owner";

/// Whether `error` is [`Inbox::send`]'s local refusal because another app
/// owns the thread ([`THREAD_OWNER`]). Nothing was sent: send a template
/// instead (it needs no ownership), or wait for the thread to come back.
pub fn is_thread_owned_elsewhere(error: &Error) -> bool {
    matches!(error, Error::Validation(v) if v.field == THREAD_OWNER)
}

/// The refusal [`is_thread_owned_elsewhere`] recognizes.
fn thread_owned_elsewhere() -> Error {
    ValidationError::new(
        THREAD_OWNER,
        "another app owns this thread under Conversation Routing; a template needs no ownership",
    )
    .into()
}

/// The local checks [`Inbox::send`] (and [`Inbox::reply`]) make before any
/// request: both on by default ([`ReplyChecks::all`]). Each only saves a
/// request Meta would refuse, since Meta enforces both itself, so turning
/// one off never lets a message through that Meta would not take
/// (`OPEN_QUESTIONS.md` #32, #44).
///
/// - **window**: free-form content outside the customer service window
///   ([`Inbox::window`]) is refused. Turn it off to let Meta decide (its
///   answer is `131047`), and in an app that receives handovers without
///   standby visibility: it never saw the customer's messages to the
///   previous owner (the [module docs](self), "What the inbox cannot
///   see").
/// - **thread owner**: a service message is refused while another app owns
///   the thread ([`Inbox::thread_owner`]). Turn it off in the designated
///   escalation partner's inbox (its service message takes the thread), in
///   an app sharing the number with a Meta Business Agent and no routing
///   configuration (the same), or when you track ownership yourself.
///
/// Set per inbox with [`Inbox::with_reply_checks`]; for one call, use a
/// clone (`inbox.clone().with_reply_checks(..)`: an `Inbox` is cheap to
/// clone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyChecks {
    window: bool,
    thread_owner: bool,
}

impl Default for ReplyChecks {
    fn default() -> Self {
        Self::all()
    }
}

impl ReplyChecks {
    /// Every check (the default).
    pub const fn all() -> Self {
        Self {
            window: true,
            thread_owner: true,
        }
    }

    /// No check: every message goes to Meta, which decides.
    pub const fn none() -> Self {
        Self {
            window: false,
            thread_owner: false,
        }
    }

    /// Turn the customer service window check on or off.
    #[must_use]
    pub const fn window(mut self, on: bool) -> Self {
        self.window = on;
        self
    }

    /// Turn the thread ownership check on or off.
    #[must_use]
    pub const fn thread_owner(mut self, on: bool) -> Self {
        self.thread_owner = on;
        self
    }

    /// Whether the window is checked.
    pub const fn checks_window(&self) -> bool {
        self.window
    }

    /// Whether thread ownership is checked.
    pub const fn checks_thread_owner(&self) -> bool {
        self.thread_owner
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
    checks: ReplyChecks,
}

impl fmt::Debug for Inbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inbox")
            .field("phone_number_id", &self.phone_number_id)
            .field("checks", &self.checks)
            .finish_non_exhaustive()
    }
}

impl Inbox {
    /// How long a thread keeps its owner without the customer
    /// (`conversation-routing/thread-lifecycle`: "24 hours of WhatsApp user
    /// inactivity" return it to idle).
    pub const THREAD_IDLE_AFTER: std::time::Duration = std::time::Duration::from_hours(24);

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
            checks: ReplyChecks::all(),
        }
    }

    /// Use `clock` for the window check and the thread's idle timeout
    /// (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The local checks [`Inbox::send`] makes ([`ReplyChecks`]; all of them
    /// unless set).
    #[must_use]
    pub fn with_reply_checks(mut self, checks: ReplyChecks) -> Self {
        self.checks = checks;
        self
    }

    /// The local checks this inbox makes.
    pub fn reply_checks(&self) -> ReplyChecks {
        self.checks
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

    /// The customer service window of a conversation: open for 24 hours
    /// from the latest of the customer's last inbound message
    /// ([`ConversationStore::last_inbound_at`]) and the conversation's
    /// latest [window event](WindowEvent) (a call that reopens the window,
    /// a message of theirs seen in standby: see the [module docs](self)).
    pub async fn window(&self, key: &ConversationKey) -> Result<CustomerServiceWindow> {
        self.check_key(key)?;
        let last = self.store.last_inbound_at(key).await?;
        self.window_from(key, last).await
    }

    /// The window of `key`, whose last inbound message is `last`.
    async fn window_from(
        &self,
        key: &ConversationKey,
        last: Option<OffsetDateTime>,
    ) -> Result<CustomerServiceWindow> {
        let event = self
            .store
            .window_events(key, None, 1)
            .await?
            .into_iter()
            .next()
            .map(|e| e.at);
        Ok(CustomerServiceWindow::from_last_inbound(last.max(event)))
    }

    /// Who owns the conversation's thread under Conversation Routing, as
    /// far as the inbox knows now; `None` when nothing was ever recorded
    /// for it (no routing). From the stored record
    /// ([`ConversationStore::thread_owner`]: handovers, standby copies,
    /// [`Inbox::record_release`]), then:
    ///
    /// - a message received on the `messages` field after the record (its
    ///   timestamp strictly later than the record's `since`) means this app
    ///   owns it ([`ThreadOwner::ThisApp`], since that message), except a
    ///   customer's answer to a call permission request: Meta sends it to
    ///   the Incoming Call primary and the standby partners, and it "does
    ///   not change thread ownership" (`conversation-routing/calling-webhooks`).
    ///   The conversation's latest 20 messages are read for it; when all
    ///   of them are later than the record, the latest inbound message
    ///   counts;
    /// - 24 hours ([`Inbox::THREAD_IDLE_AFTER`]) after the latest of that
    ///   message and the record, by this inbox's clock, the thread is idle
    ///   ([`ThreadOwner::Idle`], since then). The record's time counts as
    ///   activity: a thread is active when it changes hands, and this
    ///   errs toward the owner the record names.
    ///
    /// A key of another number is refused, reading nothing.
    pub async fn thread_owner(&self, key: &ConversationKey) -> Result<Option<ThreadOwnership>> {
        self.check_key(key)?;
        let last = self.store.last_inbound_at(key).await?;
        self.thread_owner_from(key, last).await
    }

    /// The thread owner of `key`, whose last inbound message is `last`.
    async fn thread_owner_from(
        &self,
        key: &ConversationKey,
        last: Option<OffsetDateTime>,
    ) -> Result<Option<ThreadOwnership>> {
        let Some(stored) = self.store.thread_owner(key).await? else {
            return Ok(None);
        };
        let activity = last.map_or(stored.since, |at| at.max(stored.since));
        let claimed = match last {
            Some(at) if at > stored.since => self.claimed_after(key, stored.since, at).await?,
            _ => None,
        };
        let mut owner = match claimed {
            Some(at) => ThreadOwnership {
                owner: ThreadOwner::ThisApp,
                role: stored.role.filter(|_| stored.owner == ThreadOwner::ThisApp),
                app_id: stored
                    .app_id
                    .filter(|_| stored.owner == ThreadOwner::ThisApp),
                since: at,
            },
            _ => stored,
        };
        let idle = time::Duration::try_from(Self::THREAD_IDLE_AFTER).unwrap_or(time::Duration::MAX);
        let idle_at = activity.saturating_add(idle);
        if owner.owner != ThreadOwner::Idle && self.clock.now() >= idle_at {
            owner = ThreadOwnership {
                owner: ThreadOwner::Idle,
                role: None,
                app_id: None,
                since: idle_at,
            };
        }
        Ok(Some(owner))
    }

    /// The time of the latest message after `since` that routing delivered
    /// to this app as the thread's owner ([`claims_thread`]), among the
    /// conversation's latest [`CLAIM_SCAN`] messages; `last` (the latest
    /// inbound message) when all of them are later than `since`.
    async fn claimed_after(
        &self,
        key: &ConversationKey,
        since: OffsetDateTime,
        last: OffsetDateTime,
    ) -> Result<Option<OffsetDateTime>> {
        let latest = self.store.messages(key, None, CLAIM_SCAN).await?;
        for message in &latest {
            if message.timestamp <= since {
                return Ok(None);
            }
            if claims_thread(message) {
                return Ok(Some(message.timestamp));
            }
        }
        Ok((latest.len() >= CLAIM_SCAN).then_some(last))
    }

    /// Record that this app released the conversation's thread (`release`,
    /// `conversation-routing/thread-control`), which no webhook reports:
    /// call it once your `release` request succeeded. The thread is
    /// [`ThreadOwner::Idle`] from now (this inbox's clock) until the
    /// customer's next message claims it. Returns whether it was stored
    /// ([`ConversationStore::set_thread_owner`] keeps a later record). A
    /// key of another number is refused before the store is called.
    pub async fn record_release(&self, key: &ConversationKey) -> Result<bool> {
        self.check_key(key)?;
        let released = ThreadOwnership {
            owner: ThreadOwner::Idle,
            role: None,
            app_id: None,
            since: self.clock.now(),
        };
        Ok(self.store.set_thread_owner(key, released).await?)
    }

    /// Whether a free-form reply is allowed now, by this inbox's own clock —
    /// the same check [`Inbox::reply`] applies, so a UI deciding between
    /// "reply" and "send a template" never disagrees with the refusal.
    pub async fn window_is_open(&self, key: &ConversationKey) -> Result<bool> {
        Ok(self.window(key).await?.is_open(self.clock.now()))
    }

    /// Reset the unread count (the merchant opened the conversation).
    pub async fn mark_read(&self, key: &ConversationKey) -> Result<()> {
        self.check_key(key)?;
        Ok(self.store.mark_read(key).await?)
    }

    /// Every identity the store connects to `key`'s contact on this
    /// number ([`ConversationStore::identities`]): what
    /// [`Inbox::erase_all`] takes to erase a person rather than one key.
    /// A key of another number is refused, reading nothing.
    pub async fn identities(&self, key: &ConversationKey) -> Result<BTreeSet<String>> {
        self.check_key(key)?;
        Ok(self.store.identities(key).await?)
    }

    /// Erase one key of this number ([`ConversationStore::erase`]). A key
    /// of another number is refused before the store is called, so a
    /// route cannot erase another merchant's customers by forgetting its
    /// own check (it still needs §3's ownership check of the number:
    /// `docs/guides/cms-inbox.md`). A person has several keys: erase them
    /// with [`Inbox::identities`] and [`Inbox::erase_all`].
    pub async fn erase(&self, key: &ConversationKey) -> Result<Erased> {
        self.check_key(key)?;
        Ok(self.store.erase(key).await?)
    }

    /// Erase one person on this number, known by `contacts` (their
    /// BSUIDs and phone numbers: [`Inbox::identities`] collects them),
    /// with [`ConversationStore::erase_all`], which says what it reaches
    /// and what it does not. Always this inbox's number: erase a person on
    /// each of the merchant's numbers through each one's inbox.
    pub async fn erase_all(&self, contacts: &[String]) -> Result<Erased> {
        Ok(self
            .store
            .erase_all(&self.phone_number_id, contacts)
            .await?)
    }

    /// Send `content` to the conversation's contact and record it.
    ///
    /// Refused locally, before any request ([`ReplyChecks`] turns either
    /// check off): free-form content outside the 24-hour window
    /// ([`ValidationError::customer_service_window_closed`]; the window
    /// counts calls and standby messages, [`Inbox::window`]), and a service
    /// message while another app owns the thread ([`THREAD_OWNER`],
    /// [`Inbox::thread_owner`]). Templates are always allowed. The recorded
    /// row has status [`DeliveryStatus::Accepted`] until status webhooks
    /// move it on.
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
    ///
    /// Both checks exempt templates and the Direct Send `utility` and
    /// `authentication` categories, which Meta sends as templates (outside
    /// the window, and `conversation-routing/thread-lifecycle`: marketing,
    /// utility and authentication templates need no ownership), and leave
    /// a category this crate does not know ([`DirectSendCategory::Other`])
    /// to Meta. A `service` category is a service message: Meta drops it
    /// outside the window
    /// (`direct-send/send-utility-and-authentication-messages`), so both
    /// checks apply. Ownership is checked first: while another app owns the
    /// thread, the merchant should not answer in free text at all.
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
        // Meta sends a Direct Send `utility` or `authentication` message as a
        // template; `service` "follows the existing service-message flow"
        // (`direct-send/send-utility-and-authentication-messages`), and a
        // category this crate does not know is Meta's to judge.
        let as_template = matches!(message.content, MessageContent::Template(_))
            || matches!(
                message.category,
                Some(
                    DirectSendCategory::Utility
                        | DirectSendCategory::Authentication
                        | DirectSendCategory::Other(_)
                )
            );
        let check_window = self.checks.window && !as_template;
        let check_owner = self.checks.thread_owner && !as_template;
        if check_window || check_owner {
            let last = self.store.last_inbound_at(key).await?;
            if check_owner {
                let owner = self.thread_owner_from(key, last).await?;
                if owner.is_some_and(|o| o.owner == ThreadOwner::AnotherApp) {
                    return Err(thread_owned_elsewhere());
                }
            }
            if check_window && !self.window_from(key, last).await?.is_open(self.clock.now()) {
                return Err(ValidationError::customer_service_window_closed().into());
            }
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
            Ok(payload) => payload,
            Err(e) => {
                tracing::error!(
                    category = ?e.classify(),
                    "message sent but not recorded in the inbox: it could not be serialized"
                );
                return;
            }
        };
        let text = match &message.content {
            MessageContent::Text(t) => Some(t.body.clone()),
            _ => None,
        };
        if let Err(e) = self
            .store
            .append(StoredMessage {
                id,
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
    use meta_whatsapp_adapters::store::MemoryConversationStore;
    use meta_whatsapp_client::RetryPolicy;
    use meta_whatsapp_client::messages::Text;
    use meta_whatsapp_client::templates::TemplateMessage;
    use meta_whatsapp_core::clock::ManualClock;
    use meta_whatsapp_core::testing::ScriptedTransport;
    use meta_whatsapp_webhooks::{WebhookPayload, events};
    use serde_json::json;
    use time::macros::datetime;

    use super::*;
    use meta_whatsapp_core::Error;

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

    /// A store with the Postgres store's rules for U+0000
    /// (`live_postgres_keeps_nul_in_content_and_refuses_it_in_ids` pins
    /// them): refused in the identifiers it keeps as `TEXT` (message id,
    /// contact, phone number id), stored exactly in content. Delegates to
    /// the memory store. With `down`, every write fails (the database is
    /// unreachable).
    #[derive(Debug, Default)]
    struct PostgresRulesStore {
        inner: MemoryConversationStore,
        down: bool,
    }

    /// Whether the Postgres store refuses an identifier.
    fn refused_id(id: &str) -> bool {
        id.contains('\0')
    }

    /// Whether the Postgres store refuses `m`: U+0000 in an identifier.
    fn refused(m: &StoredMessage) -> bool {
        [
            m.id.as_str(),
            m.conversation.contact.as_str(),
            m.conversation.phone_number_id.as_str(),
        ]
        .into_iter()
        .any(refused_id)
    }

    fn refuse() -> StorageError {
        StorageError::Backend(anyhow::anyhow!(
            "invalid byte sequence for encoding \"UTF8\": 0x00"
        ))
    }

    #[async_trait]
    impl ConversationStore for PostgresRulesStore {
        async fn append(&self, m: StoredMessage) -> std::result::Result<bool, StorageError> {
            if self.down {
                return Err(StorageError::Backend(anyhow::anyhow!("connection refused")));
            }
            if refused(&m) {
                return Err(refuse());
            }
            self.inner.append(m).await
        }
        async fn append_synced(
            &self,
            messages: Vec<StoredMessage>,
        ) -> std::result::Result<Vec<bool>, StorageError> {
            if self.down {
                return Err(StorageError::Backend(anyhow::anyhow!("connection refused")));
            }
            // Postgres refuses the whole statement.
            if messages.iter().any(refused) {
                return Err(refuse());
            }
            self.inner.append_synced(messages).await
        }
        async fn revoke(
            &self,
            key: &ConversationKey,
            id: &MessageId,
            direction: Direction,
            at: OffsetDateTime,
        ) -> std::result::Result<bool, StorageError> {
            if self.down {
                return Err(StorageError::Backend(anyhow::anyhow!("connection refused")));
            }
            if refused_id(id.as_str())
                || refused_id(&key.contact)
                || refused_id(key.phone_number_id.as_str())
            {
                return Err(refuse());
            }
            self.inner.revoke(key, id, direction, at).await
        }
        async fn fill_media_placeholder(
            &self,
            phone_number_id: &PhoneNumberId,
            id: &MessageId,
            kind: String,
            text: Option<String>,
            payload: serde_json::Value,
        ) -> std::result::Result<bool, StorageError> {
            if self.down {
                return Err(StorageError::Backend(anyhow::anyhow!("connection refused")));
            }
            if refused_id(id.as_str()) || refused_id(phone_number_id.as_str()) {
                return Err(refuse());
            }
            self.inner
                .fill_media_placeholder(phone_number_id, id, kind, text, payload)
                .await
        }
        async fn update_status(
            &self,
            phone_number_id: &PhoneNumberId,
            id: &MessageId,
            status: DeliveryStatus,
            at: OffsetDateTime,
            error: Option<serde_json::Value>,
        ) -> std::result::Result<bool, StorageError> {
            if self.down {
                return Err(StorageError::Backend(anyhow::anyhow!("connection refused")));
            }
            if refused_id(id.as_str()) || refused_id(phone_number_id.as_str()) {
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
        async fn message(
            &self,
            phone_number_id: &PhoneNumberId,
            id: &MessageId,
        ) -> std::result::Result<Option<StoredMessage>, StorageError> {
            self.inner.message(phone_number_id, id).await
        }
        async fn record_window_event(
            &self,
            event: meta_whatsapp_core::store::WindowEvent,
        ) -> std::result::Result<bool, StorageError> {
            self.inner.record_window_event(event).await
        }
        async fn window_events(
            &self,
            key: &ConversationKey,
            before: Option<(OffsetDateTime, String)>,
            limit: usize,
        ) -> std::result::Result<Vec<meta_whatsapp_core::store::WindowEvent>, StorageError>
        {
            self.inner.window_events(key, before, limit).await
        }
        async fn set_thread_owner(
            &self,
            key: &ConversationKey,
            ownership: meta_whatsapp_core::store::ThreadOwnership,
        ) -> std::result::Result<bool, StorageError> {
            self.inner.set_thread_owner(key, ownership).await
        }
        async fn thread_owner(
            &self,
            key: &ConversationKey,
        ) -> std::result::Result<Option<meta_whatsapp_core::store::ThreadOwnership>, StorageError>
        {
            self.inner.thread_owner(key).await
        }
        async fn put_contact(
            &self,
            contact: meta_whatsapp_core::store::StoredContact,
        ) -> std::result::Result<bool, StorageError> {
            self.inner.put_contact(contact).await
        }
        async fn remove_contact(
            &self,
            key: &ConversationKey,
            at: OffsetDateTime,
        ) -> std::result::Result<bool, StorageError> {
            self.inner.remove_contact(key, at).await
        }
        async fn contact(
            &self,
            key: &ConversationKey,
        ) -> std::result::Result<Option<meta_whatsapp_core::store::StoredContact>, StorageError>
        {
            self.inner.contact(key).await
        }
        async fn contacts(
            &self,
            phone_number_id: &PhoneNumberId,
            after: Option<String>,
            limit: usize,
        ) -> std::result::Result<Vec<meta_whatsapp_core::store::StoredContact>, StorageError>
        {
            self.inner.contacts(phone_number_id, after, limit).await
        }
        async fn link_identity(
            &self,
            link: meta_whatsapp_core::store::IdentityLink,
        ) -> std::result::Result<bool, StorageError> {
            self.inner.link_identity(link).await
        }
        async fn identity_links(
            &self,
            key: &ConversationKey,
        ) -> std::result::Result<Vec<meta_whatsapp_core::store::IdentityLink>, StorageError>
        {
            self.inner.identity_links(key).await
        }
        async fn identities(
            &self,
            key: &ConversationKey,
        ) -> std::result::Result<BTreeSet<String>, StorageError> {
            self.inner.identities(key).await
        }
        async fn erase_all(
            &self,
            phone_number_id: &PhoneNumberId,
            contacts: &[String],
        ) -> std::result::Result<Erased, StorageError> {
            self.inner.erase_all(phone_number_id, contacts).await
        }
        async fn purge_before(
            &self,
            phone_number_id: Option<&PhoneNumberId>,
            cutoff: OffsetDateTime,
        ) -> std::result::Result<meta_whatsapp_core::store::Purged, StorageError> {
            self.inner.purge_before(phone_number_id, cutoff).await
        }
    }

    /// Security review M1: one NUL in a customer's message made every
    /// delivery of the batch fail on Postgres, so Meta retried it for 7
    /// days and then dropped it, together with every other event in it.
    /// fd4667e stored U+FFFD instead; since the owner's decision of
    /// 2026-09-25, the content is recorded exactly, and the Postgres store
    /// keeps it.
    #[tokio::test]
    async fn a_nul_in_customer_content_is_recorded_exactly() {
        let store = Arc::new(PostgresRulesStore::default());
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
                "type": "fut\u{0}ure",
                "fut\u{0}ure": {"k\u{0}": ["a\u{0}", {"deep\u{0}": "b\u{0}"}], "k\u{FFFD}": "fffd"}
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
        assert_eq!(text.text.as_deref(), Some("order\u{0}42"));
        assert_eq!(text.payload["text"]["body"], "order\u{0}42");
        assert_eq!(text.status, DeliveryStatus::Failed);
        let error = text.error.as_ref().unwrap();
        assert_eq!(error[0]["title"], "bad\u{0}");
        assert_eq!(error[0]["error_data"]["details"], "x\u{0}");
        assert_eq!(unknown.kind, "fut\u{0}ure");
        let properties = &unknown.payload["fut\u{0}ure"];
        assert_eq!(
            (
                &properties["k\u{0}"][0],
                &properties["k\u{0}"][1]["deep\u{0}"],
                &properties["k\u{FFFD}"],
                properties.as_object().map(serde_json::Map::len),
            ),
            (&json!("a\u{0}"), &json!("b\u{0}"), &json!("fffd"), Some(2)),
            "keys and nested values, and two keys differing by NUL vs U+FFFD: {}",
            unknown.payload
        );
        let text_row = serde_json::to_string(text).unwrap();
        assert!(
            !text_row.contains('\u{FFFD}'),
            "no replacement character anywhere: {text_row}"
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
        assert!(inbox.window_is_open(&key).await.unwrap());
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

    /// The outbound side: a reply whose text carries U+0000 is recorded
    /// exactly, not refused (and not turned into an error).
    #[tokio::test]
    async fn an_outbound_reply_with_nul_is_recorded_exactly() {
        let store = Arc::new(PostgresRulesStore::default());
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
        assert_eq!(out.text.as_deref(), Some("a\u{0}b"));
        assert_eq!(out.payload["text"]["body"], "a\u{0}b");
        assert_eq!(out.kind, "text");
    }

    /// Conventions review #18: once Meta accepted a message, nothing about
    /// recording it may turn the send into an error — the caller would
    /// retry and the customer would get it twice.
    #[tokio::test]
    async fn a_recording_failure_after_the_send_is_not_the_callers_error() {
        let store = Arc::new(PostgresRulesStore {
            down: true,
            ..PostgresRulesStore::default()
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
        assert!(!inbox.window_is_open(&key).await.unwrap());
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
        assert_eq!(
            err.kind(),
            meta_whatsapp_core::ErrorKind::CustomerServiceWindowClosed
        );
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

    /// Review of roadmap L5 (L4): `Inbox::erase` refuses a key of another
    /// number before the store is called, so a route that took the number
    /// from its path cannot erase another merchant's customers; the
    /// identities are read on the inbox's number alone, and `erase_all`
    /// never leaves it.
    #[tokio::test]
    async fn erasing_checks_the_number_first() {
        let store = Arc::new(CountingStore::default());
        let client = Client::builder()
            .transport(ScriptedTransport::new())
            .access_token("MERCHANT_TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let inbox = Inbox::new(client, PNID, store.clone());
        let at = datetime!(2026-09-24 12:00 UTC);
        let message = |number: &str, id: &str| StoredMessage {
            id: MessageId::new(id),
            conversation: ConversationKey::new(number, "16505551234"),
            direction: Direction::Inbound,
            kind: "text".to_owned(),
            text: Some("my address".to_owned()),
            payload: json!({"from": "16505551234"}),
            status: DeliveryStatus::Received,
            timestamp: at,
            status_at: None,
            error: None,
        };
        // The same customer on another merchant's number.
        store
            .inner
            .append(message("999", "wamid.theirs"))
            .await
            .unwrap();
        store
            .inner
            .append(message(PNID, "wamid.ours"))
            .await
            .unwrap();
        let foreign = ConversationKey::new("999", "16505551234");
        for refused in [
            inbox.erase(&foreign).await.map(drop),
            inbox.identities(&foreign).await.map(drop),
        ] {
            assert!(matches!(refused, Err(Error::Validation(_))), "{refused:?}");
        }
        let calls =
            |n: &std::sync::atomic::AtomicUsize| n.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            (calls(&store.erasures), calls(&store.reads)),
            (0, 0),
            "refused before the store is called"
        );
        assert_eq!(
            store
                .inner
                .messages(&foreign, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );

        let ours = inbox.key("16505551234");
        assert_eq!(
            inbox.identities(&ours).await.unwrap(),
            BTreeSet::from(["16505551234".to_owned()])
        );
        assert_eq!(
            inbox
                .erase_all(&["16505551234".to_owned()])
                .await
                .unwrap()
                .messages,
            1
        );
        assert!(
            store
                .inner
                .messages(&ours, None, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .inner
                .messages(&foreign, None, 10)
                .await
                .unwrap()
                .len(),
            1,
            "erase_all is bound to the inbox's number"
        );
        assert!(inbox.erase(&ours).await.unwrap().is_empty());
        assert_eq!(calls(&store.erasures), 2);
    }

    // ─── Coexistence: echoes and history ────────────────────────────────

    /// The events of a whole webhook body.
    fn body(json: &str) -> Vec<WebhookEvent> {
        events(&WebhookPayload::from_slice(json.as_bytes()).unwrap())
    }

    /// The events of one change of `field` with `value`.
    fn change(field: &str, value: &serde_json::Value) -> Vec<WebhookEvent> {
        body(
            &json!({
                "object": "whatsapp_business_account",
                "entry": [{"id": "102290129340398", "changes": [{"field": field, "value": value}]}]
            })
            .to_string(),
        )
    }

    // Fixtures copied from Meta's example payloads (meta-whatsapp-webhooks' tests).
    const ECHO_TEXT: &str = include_str!(
        "../../meta-whatsapp-webhooks/tests/fixtures/fields/smb_message_echoes_text.json"
    );
    const ECHO_REVOKE: &str = include_str!(
        "../../meta-whatsapp-webhooks/tests/fixtures/fields/smb_message_echoes_revoke.json"
    );
    const ECHO_BSUID: &str = include_str!(
        "../../meta-whatsapp-webhooks/tests/fixtures/bsuid/smb_message_echoes_bsuid.json"
    );
    const HISTORY_THREADS: &str =
        include_str!("../../meta-whatsapp-webhooks/tests/fixtures/fields/history_threads.json");
    const HISTORY_MEDIA: &str =
        include_str!("../../meta-whatsapp-webhooks/tests/fixtures/fields/history_media.json");
    const HISTORY_DECLINED: &str =
        include_str!("../../meta-whatsapp-webhooks/tests/fixtures/fields/history_declined.json");
    const HISTORY_BSUID: &str = include_str!(
        "../../meta-whatsapp-webhooks/tests/fixtures/bsuid/history_thread_context.json"
    );

    /// The summary of one conversation.
    async fn summary_of(
        store: &dyn ConversationStore,
        key: &ConversationKey,
    ) -> Option<ConversationSummary> {
        store
            .conversations(&key.phone_number_id, None, 100)
            .await
            .unwrap()
            .into_iter()
            .find(|s| &s.key == key)
    }

    /// The whole conversation, oldest first.
    async fn thread(store: &dyn ConversationStore, contact: &str) -> Vec<StoredMessage> {
        let mut rows = store
            .messages(&ConversationKey::new(PNID, contact), None, 100)
            .await
            .unwrap();
        rows.reverse();
        rows
    }

    /// `smb_message_echoes` example: the merchant answered from the app.
    #[tokio::test]
    async fn an_echo_is_an_outbound_sent_message_in_the_customers_conversation() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(&sink, body(ECHO_TEXT)).await;
        deliver_all(&sink, body(ECHO_TEXT)).await; // Meta retry
        let rows = thread(store.as_ref(), "16505551234").await;
        assert_eq!(rows.len(), 1, "redelivery must not duplicate: {rows:?}");
        let echo = &rows[0];
        assert_eq!(
            echo.id,
            MessageId::new("wamid.HBgLMTY0NjcwNDM1OTUVAgARGBIyNDlBOEI5QUQ4NDc0N0FCNjMA")
        );
        assert_eq!(echo.direction, Direction::Outbound);
        assert_eq!(echo.status, DeliveryStatus::Sent);
        assert_eq!(echo.kind, "text");
        assert_eq!(
            echo.text.as_deref(),
            Some("Here's the info you requested! https://www.meta.com/quest/quest-3/")
        );
        assert_eq!(echo.timestamp.unix_timestamp(), 1_739_321_024);
        assert_eq!(echo.payload["to"], "16505551234");
        // Sent from the app: it opens no customer service window.
        let key = ConversationKey::new(PNID, "16505551234");
        assert_eq!(store.last_inbound_at(&key).await.unwrap(), None);
        let summary = store.conversations(&PNID.into(), None, 10).await.unwrap();
        assert_eq!(summary[0].unread, 0);
    }

    /// BSUID era: the echo goes to the customer's BSUID conversation, the
    /// one their own messages are recorded in.
    #[tokio::test]
    async fn an_echo_joins_the_bsuid_conversation_of_the_customer() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            inbound(
                "wamid.in",
                1_767_168_700,
                Some("US.13491208655302741918"),
                None,
                "Has it shipped?",
            ),
        )
        .await;
        deliver_all(&sink, body(ECHO_BSUID)).await;
        let rows = thread(store.as_ref(), "US.13491208655302741918").await;
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[1].direction, Direction::Outbound);
        assert_eq!(rows[1].text.as_deref(), Some("Your order has shipped!"));
        // Without a BSUID, `to` as the API's `input` may carry a `+`: the
        // conversation is still the `wa_id` one.
        deliver_all(
            &sink,
            change(
                "smb_message_echoes",
                &json!({
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "message_echoes": [{"from": "15550783881", "to": "+16505551234",
                        "id": "wamid.plus", "timestamp": "1739321024", "type": "text",
                        "text": {"body": "hi"}}]
                }),
            ),
        )
        .await;
        assert_eq!(thread(store.as_ref(), "16505551234").await.len(), 1);
    }

    /// `smb_message_echoes` revoke example: the merchant deleted a message
    /// from the app.
    #[tokio::test]
    async fn an_echoed_revoke_marks_the_original_deleted() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        let original = "wamid.HBgLMTQxMjU1NTA4MjkVAgASGBQzQUNCNjk5RDUwNUZGMUZEM0VBRAA=";
        deliver_all(
            &sink,
            change(
                "smb_message_echoes",
                &json!({
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "message_echoes": [{"from": "15550783881", "to": "16505551234",
                        "id": original, "timestamp": "1749854500", "type": "text",
                        "text": {"body": "wrong price"}}]
                }),
            ),
        )
        .await;
        deliver_all(&sink, body(ECHO_REVOKE)).await;
        let rows = thread(store.as_ref(), "16505551234").await;
        assert_eq!(
            rows.len(),
            1,
            "the revoke itself is not a message: {rows:?}"
        );
        assert_eq!(rows[0].status, DeliveryStatus::Deleted);
    }

    /// `history` example: two threads, both directions, statuses from
    /// `history_context`.
    #[tokio::test]
    async fn history_is_recorded_in_its_documented_direction_and_status() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(&sink, body(HISTORY_THREADS)).await;
        deliver_all(&sink, body(HISTORY_THREADS)).await; // Meta retry
        let rows = thread(store.as_ref(), "16505551234").await;
        let got: Vec<_> = rows
            .iter()
            .map(|r| (r.direction, r.status, r.kind.as_str(), r.text.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                (
                    Direction::Outbound,
                    DeliveryStatus::Read,
                    "text",
                    Some("Here's the info you requested! https://www.meta.com/quest/quest-3/")
                ),
                // Same second: ordered by id.
                (
                    Direction::Inbound,
                    DeliveryStatus::Received,
                    "text",
                    Some("Thanks!")
                ),
                (
                    Direction::Outbound,
                    DeliveryStatus::Played,
                    "media_placeholder",
                    None
                ),
            ]
        );
        assert_eq!(rows[0].timestamp.unix_timestamp(), 1_739_230_955);
        assert_eq!(rows[0].payload["history_context"]["status"], "READ");
        let other = thread(store.as_ref(), "12125557890").await;
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].direction, Direction::Outbound);
        assert_eq!(other[0].status, DeliveryStatus::Delivered);
    }

    /// Chunks may arrive in any order and a message may arrive both live
    /// and synced: each message is stored once, by id, and read back in
    /// timestamp order.
    #[tokio::test]
    async fn history_chunks_in_any_order_and_live_duplicates_are_stored_once() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        let chunk = |order: i64, id: &str, ts: &str, text: &str| {
            change(
                "history",
                &json!({
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "history": [{"metadata": {"phase": 1, "chunk_order": order, "progress": 40 * order},
                        "threads": [{"id": "16505551234", "messages": [{"from": "16505551234",
                            "id": id, "timestamp": ts, "type": "text", "text": {"body": text},
                            "history_context": {"status": "READ"}}]}]}]
                }),
            )
        };
        // The echo arrives live first; its message is also in the history.
        deliver_all(&sink, body(ECHO_TEXT)).await;
        deliver_all(&sink, chunk(2, "wamid.late", "1739230990", "second")).await;
        deliver_all(&sink, chunk(1, "wamid.early", "1739230980", "first")).await;
        deliver_all(
            &sink,
            change(
                "history",
                &json!({
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "history": [{"threads": [{"id": "16505551234", "messages": [{
                        "from": "15550783881",
                        "id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBIyNDlBOEI5QUQ4NDc0N0FCNjMA",
                        "timestamp": "1739321000", "type": "text",
                        "text": {"body": "Here's the info you requested! https://www.meta.com/quest/quest-3/"},
                        "history_context": {"status": "DELIVERED"}}]}]}]
                }),
            ),
        )
        .await;
        let rows = thread(store.as_ref(), "16505551234").await;
        let texts: Vec<_> = rows.iter().map(|r| r.text.as_deref().unwrap()).collect();
        assert_eq!(
            texts,
            [
                "first",
                "second",
                "Here's the info you requested! https://www.meta.com/quest/quest-3/"
            ]
        );
        // The row recorded first (the live echo) is kept as it was.
        assert_eq!(rows[2].status, DeliveryStatus::Sent);
        assert_eq!(rows[2].timestamp.unix_timestamp(), 1_739_321_024);
    }

    /// BSUID era history: the thread's `context.user_id` is the
    /// conversation, and the media content of a business message
    /// (`message_echoes`) is outbound in it.
    #[tokio::test]
    async fn bsuid_history_and_its_media_contents_share_the_customers_conversation() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(&sink, body(HISTORY_BSUID)).await;
        let rows = thread(store.as_ref(), "US.13491208655302741918").await;
        let got: Vec<_> = rows
            .iter()
            .map(|r| (r.direction, r.status, r.kind.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (Direction::Inbound, DeliveryStatus::Received, "text"),
                (Direction::Outbound, DeliveryStatus::Sent, "image"),
            ]
        );
        assert_eq!(rows[0].text.as_deref(), Some("Do you ship to Canada?"));
    }

    /// The media content of a customer's message (`history` `messages`)
    /// without an earlier placeholder is an inbound row of its own, recorded
    /// once and opening no window.
    #[tokio::test]
    async fn a_media_content_without_its_placeholder_is_a_row_of_its_own() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(&sink, body(HISTORY_MEDIA)).await;
        deliver_all(&sink, body(HISTORY_MEDIA)).await; // Meta retry
        let rows = thread(store.as_ref(), "16505551234").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].direction, Direction::Inbound);
        assert_eq!(rows[0].kind, "image");
        assert_eq!(rows[0].text.as_deref(), Some("Black Prince echeveria"));
        let key = ConversationKey::new(PNID, "16505551234");
        assert_eq!(store.last_inbound_at(&key).await.unwrap(), None);
        assert_eq!(summary_of(store.as_ref(), &key).await.unwrap().unread, 0);
    }

    /// Synced history is history (`ConversationStore::fill_media_placeholder`,
    /// a port change the owner confirmed on 2026-09-25): the media content
    /// Meta sends after a `media_placeholder` (Meta's two `history`
    /// examples, same message id) replaces the placeholder's content. The row keeps what the thread
    /// said: its conversation, direction, status and timestamp (Meta's
    /// examples disagree on the sender; the thread's is kept).
    #[tokio::test]
    async fn a_late_media_content_fills_its_placeholder() {
        let placeholder = "wamid.QyNUEHBgLMTY0NjcwNDM1OTUVAgARGBI1Rj3NEYxMzAzMzQ5MkEA";
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(&sink, body(HISTORY_THREADS)).await;
        let before = thread(store.as_ref(), "16505551234").await;
        let recorded = before
            .iter()
            .find(|r| r.id.as_str() == placeholder)
            .unwrap()
            .clone();
        assert_eq!(recorded.kind, StoredMessage::MEDIA_PLACEHOLDER);
        deliver_all(&sink, body(HISTORY_MEDIA)).await;
        deliver_all(&sink, body(HISTORY_MEDIA)).await; // Meta retry
        let rows = thread(store.as_ref(), "16505551234").await;
        assert_eq!(rows.len(), 3, "filled, not added: {rows:?}");
        let filled = rows.iter().find(|r| r.id.as_str() == placeholder).unwrap();
        assert_eq!(filled.kind, "image");
        assert_eq!(filled.text.as_deref(), Some("Black Prince echeveria"));
        assert_eq!(filled.payload["image"]["id"], "24230790383178626");
        assert_eq!(
            (
                &filled.conversation,
                filled.direction,
                filled.status,
                filled.timestamp,
            ),
            (
                &recorded.conversation,
                Direction::Outbound,
                DeliveryStatus::Played,
                recorded.timestamp,
            ),
            "only the content changes"
        );
        // The same through a `history` value that fell to
        // `WebhookEvent::Unknown`, and for the business's own media
        // (`message_echoes`).
        let store = Arc::new(PostgresRulesStore::default());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            history_with(
                &json!({"from": "15550783881", "id": "wamid.p", "timestamp": "1739230950",
                "type": "media_placeholder", "history_context": {"status": "READ"}}),
            ),
        )
        .await;
        let content = change(
            "history",
            &json!({
                "messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "message_echoes": [{"from": "15550783881", "to": "16505551234", "id": "wamid.p",
                    "timestamp": "1739231000", "type": "image",
                    "image": {"id": "1", "caption": "catalogue", "mime_type": "image/jpeg"}}],
                "messages": [{"from": "16505551234", "id": "wamid.bad", "timestamp": "never",
                    "type": "text", "text": {"body": "x"}}]
            }),
        );
        assert!(
            matches!(content[0], WebhookEvent::Unknown { .. }),
            "{:?}",
            content[0].kind()
        );
        deliver_all(&sink, content).await;
        let rows = thread(&store.inner, "16505551234").await;
        let got: Vec<_> = rows
            .iter()
            .map(|r| (r.id.as_str(), r.kind.as_str(), r.text.as_deref(), r.status))
            .collect();
        assert_eq!(
            got,
            [
                ("wamid.a", "text", Some("before"), DeliveryStatus::Received),
                ("wamid.p", "image", Some("catalogue"), DeliveryStatus::Read),
                ("wamid.b", "text", Some("after"), DeliveryStatus::Received),
            ]
        );
    }

    /// A placeholder the thread recorded, then revoked live (by the
    /// business, which sent it: an echoed revoke), never gets the media
    /// content Meta sends later: that content is what its sender deleted.
    #[tokio::test]
    async fn a_revoked_placeholder_never_gets_its_content() {
        let placeholder = "wamid.QyNUEHBgLMTY0NjcwNDM1OTUVAgARGBI1Rj3NEYxMzAzMzQ5MkEA";
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(&sink, body(HISTORY_THREADS)).await;
        deliver_all(
            &sink,
            change(
                "smb_message_echoes",
                &json!({
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "message_echoes": [{"from": "15550783881", "to": "16505551234",
                        "id": "wamid.revoke", "timestamp": "1739231000", "type": "revoke",
                        "revoke": {"original_message_id": placeholder}}]
                }),
            ),
        )
        .await;
        let revoked = thread(store.as_ref(), "16505551234")
            .await
            .into_iter()
            .find(|r| r.id.as_str() == placeholder)
            .unwrap();
        assert_eq!(
            (revoked.kind.as_str(), revoked.status),
            (StoredMessage::MEDIA_PLACEHOLDER, DeliveryStatus::Deleted)
        );
        deliver_all(&sink, body(HISTORY_MEDIA)).await;
        let rows = thread(store.as_ref(), "16505551234").await;
        assert_eq!(rows.len(), 3, "{rows:?}");
        let after = rows.iter().find(|r| r.id.as_str() == placeholder).unwrap();
        assert_eq!(after, &revoked, "the revoked placeholder is not filled");
        assert!(
            !format!("{rows:?}").contains("Black Prince"),
            "the deleted caption is stored nowhere: {rows:?}"
        );
    }

    /// Synced history is history (`ConversationStore::append_synced`, a port
    /// change the owner confirmed on 2026-09-25): Meta opens no customer
    /// service window for a message received before onboarding, and the
    /// merchant has read the synced history in the app. Neither the typed nor the recovered
    /// (`WebhookEvent::Unknown`) path may open the window or count unread;
    /// a live message afterwards does both.
    #[tokio::test]
    async fn synced_history_opens_no_window_and_is_never_unread() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        let key = ConversationKey::new(PNID, "16505551234");
        // "Thanks!" was received at 1739230970 (2025-02-10 23:42:50 UTC).
        deliver_all(&sink, body(HISTORY_THREADS)).await;
        let clock = ManualClock::new(datetime!(2025-02-10 23:50 UTC));
        let inbox = inbox(&ScriptedTransport::new(), store.clone(), &clock);
        assert_eq!(store.last_inbound_at(&key).await.unwrap(), None);
        assert!(!inbox.window_is_open(&key).await.unwrap());
        let s = summary_of(store.as_ref(), &key).await.unwrap();
        assert_eq!(s.unread, 0);
        assert_eq!(s.last_message_at.unix_timestamp(), 1_739_230_970);

        // The recovered path (one malformed item): same rule.
        deliver_all(
            &sink,
            history_with(&json!({"from": "16505551234", "timestamp": "1739230950"})),
        )
        .await;
        assert_eq!(store.last_inbound_at(&key).await.unwrap(), None);
        assert_eq!(summary_of(store.as_ref(), &key).await.unwrap().unread, 0);
        assert_eq!(thread(store.as_ref(), "16505551234").await.len(), 5);

        // A live message opens the window and is unread.
        deliver_all(
            &sink,
            inbound(
                "wamid.live",
                1_739_231_000,
                None,
                Some("16505551234"),
                "still there?",
            ),
        )
        .await;
        assert!(inbox.window_is_open(&key).await.unwrap());
        assert_eq!(summary_of(store.as_ref(), &key).await.unwrap().unread, 1);
    }

    /// A revoke in the synced history deletes the original, of this
    /// number only, like a live one.
    #[tokio::test]
    async fn a_synced_revoke_marks_the_original_deleted() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            history_with(
                &json!({"from": "16505551234", "id": "wamid.r", "timestamp": "1739230950",
                "type": "revoke", "revoke": {"original_message_id": "wamid.a"}}),
            ),
        )
        .await;
        let rows = thread(store.as_ref(), "16505551234").await;
        let got: Vec<_> = rows
            .iter()
            .map(|r| (r.id.as_str(), r.kind.as_str(), r.status))
            .collect();
        assert_eq!(
            got,
            [
                ("wamid.a", "text", DeliveryStatus::Deleted),
                ("wamid.b", "text", DeliveryStatus::Received)
            ],
            "the revoke of a message earlier in the chunk deletes it, not a tombstone"
        );
        // The business revokes what it sent, and only that.
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            one_thread(
                Some("16505551234"),
                None,
                &json!([
                    {"from": "16505551234", "id": "wamid.c", "timestamp": "1739230900",
                        "type": "text", "text": {"body": "customer"}},
                    {"from": "15550783881", "id": "wamid.o", "timestamp": "1739230901",
                        "type": "text", "text": {"body": "business"}},
                    {"from": "15550783881", "id": "wamid.r1", "timestamp": "1739230902",
                        "type": "revoke", "revoke": {"original_message_id": "wamid.o"}},
                    {"from": "15550783881", "id": "wamid.r2", "timestamp": "1739230903",
                        "type": "revoke", "revoke": {"original_message_id": "wamid.c"}}
                ]),
            ),
        )
        .await;
        let got: Vec<_> = thread(store.as_ref(), "16505551234")
            .await
            .into_iter()
            .map(|r| (r.id.as_str().to_owned(), r.status))
            .collect();
        assert_eq!(
            got,
            [
                ("wamid.c".to_owned(), DeliveryStatus::Received),
                ("wamid.o".to_owned(), DeliveryStatus::Deleted)
            ]
        );
    }

    #[tokio::test]
    async fn a_declined_history_sync_records_nothing() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(&sink, body(HISTORY_DECLINED)).await;
        assert!(
            store
                .conversations(&PNID.into(), None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// One history message per failure mode, between two good ones.
    fn history_with(bad: &serde_json::Value) -> Vec<WebhookEvent> {
        let good = |id: &str, ts: &str, body: &str| {
            json!({"from": "16505551234", "id": id, "timestamp": ts, "type": "text",
                "text": {"body": body}})
        };
        change(
            "history",
            &json!({
                "messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "history": [{"metadata": {"phase": 0, "chunk_order": 1, "progress": 10},
                    "threads": [{"id": "16505551234", "messages": [
                        good("wamid.a", "1739230900", "before"),
                        bad,
                        good("wamid.b", "1739230999", "after")
                    ]}]}]
            }),
        )
    }

    /// A history sync can be requested once: one bad message must not fail
    /// the delivery (Meta would retry it for 7 days, then drop every
    /// message in it) nor cost the others.
    #[tokio::test]
    async fn a_malformed_history_item_is_skipped_and_the_rest_recorded() {
        for (case, bad, typed) in [
            (
                "no direction",
                json!({"id": "wamid.x", "timestamp": "1739230950", "type": "text",
                    "text": {"body": "who?"}}),
                true,
            ),
            (
                "U+0000 in the message id",
                json!({"from": "16505551234", "id": "wamid.\u{0}x", "timestamp": "1739230950",
                    "type": "text", "text": {"body": "x"}}),
                true,
            ),
            (
                "U+0000 in a revoked id",
                json!({"from": "16505551234", "id": "wamid.r", "timestamp": "1739230950",
                    "type": "revoke", "revoke": {"original_message_id": "wamid.\u{0}a"}}),
                true,
            ),
            (
                "U+0000 in the customer's id (a BSUID)",
                json!({"from": "16505551234", "from_user_id": "US.\u{0}x", "id": "wamid.x",
                    "timestamp": "1739230950", "type": "text", "text": {"body": "x"}}),
                true,
            ),
            (
                "U+0000 in a revoke's customer id",
                json!({"from": "16505551234", "from_user_id": "US.\u{0}x", "id": "wamid.r",
                    "timestamp": "1739230950", "type": "revoke",
                    "revoke": {"original_message_id": "wamid.a"}}),
                true,
            ),
            (
                "no id: the typed parse of the whole value fails",
                json!({"from": "16505551234", "timestamp": "1739230950", "type": "text",
                    "text": {"body": "x"}}),
                false,
            ),
            (
                "a timestamp that is not one",
                json!({"from": "16505551234", "id": "wamid.x", "timestamp": "yesterday",
                    "type": "text", "text": {"body": "x"}}),
                false,
            ),
        ] {
            let store = Arc::new(PostgresRulesStore::default());
            let sink = InboxSink::new(store.clone());
            let events = history_with(&bad);
            assert_eq!(events.len(), 1, "{case}");
            assert_eq!(
                matches!(events[0], WebhookEvent::HistorySynced { .. }),
                typed,
                "{case}: {:?}",
                events[0].kind()
            );
            for event in events {
                sink.deliver(event)
                    .await
                    .unwrap_or_else(|e| panic!("{case}: {e}"));
            }
            let texts: Vec<_> = thread(&store.inner, "16505551234")
                .await
                .into_iter()
                .map(|r| r.text.unwrap_or_default())
                .collect();
            assert_eq!(texts, ["before", "after"], "{case}");
        }
    }

    /// One `history` change of one thread (`id` and optional `context`).
    fn one_thread(
        id: Option<&str>,
        context: Option<&serde_json::Value>,
        messages: &serde_json::Value,
    ) -> Vec<WebhookEvent> {
        let mut thread = json!({"messages": messages});
        if let Some(id) = id {
            thread["id"] = json!(id);
        }
        if let Some(context) = context {
            thread["context"] = context.clone();
        }
        change(
            "history",
            &json!({
                "messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "history": [{"threads": [thread]}]
            }),
        )
    }

    /// Every documented `history_context.status` of a message the business
    /// sent, a missing one, one Meta may add later, and the case Meta does
    /// not use.
    #[tokio::test]
    async fn synced_outbound_statuses_follow_history_context() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        let cases = [
            ("PENDING", DeliveryStatus::Accepted),
            ("SENT", DeliveryStatus::Sent),
            ("DELIVERED", DeliveryStatus::Delivered),
            ("READ", DeliveryStatus::Read),
            ("PLAYED", DeliveryStatus::Played),
            ("ERROR", DeliveryStatus::Failed),
            ("QUEUED_SOMEWHERE", DeliveryStatus::Sent),
            ("read", DeliveryStatus::Read),
            ("", DeliveryStatus::Sent),
        ];
        let mut messages: Vec<_> = cases
            .iter()
            .enumerate()
            .map(|(i, (status, _))| {
                json!({"from": "15550783881", "id": format!("wamid.{i}"),
                    "timestamp": (1_739_230_900 + i).to_string(), "type": "text",
                    "text": {"body": "x"}, "history_context": {"status": status}})
            })
            .collect();
        messages.pop();
        messages.push(json!({"from": "15550783881", "id": "wamid.8",
            "timestamp": "1739230908", "type": "text", "text": {"body": "x"}}));
        let events = one_thread(Some("16505551234"), None, &json!(messages));
        assert!(matches!(events[0], WebhookEvent::HistorySynced { .. }));
        deliver_all(&sink, events).await;
        let got: Vec<_> = thread(store.as_ref(), "16505551234")
            .await
            .into_iter()
            .map(|r| (r.direction, r.status))
            .collect();
        let want: Vec<_> = cases
            .iter()
            .map(|(_, status)| (Direction::Outbound, *status))
            .collect();
        assert_eq!(got, want);
    }

    /// Who sent a synced message: `from` is the business number in any
    /// format; without a `from`, `to` (set only on the business's messages)
    /// makes it outbound, a sender BSUID inbound.
    #[tokio::test]
    async fn synced_direction_follows_from_and_to() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        let messages = json!([
            {"from": "+1 555-078-3881", "id": "wamid.formatted", "timestamp": "1739230901",
                "type": "text", "text": {"body": "formatted business number"}},
            {"to": "16505551234", "id": "wamid.to-only", "timestamp": "1739230902",
                "type": "text", "text": {"body": "no from, a to"}},
            {"from": " ", "to": "16505551234", "id": "wamid.blank-from", "timestamp": "1739230903",
                "type": "text", "text": {"body": "a blank from, a to"}},
            {"from": "16505551234", "id": "wamid.customer", "timestamp": "1739230904",
                "type": "text", "text": {"body": "the customer"}}
        ]);
        deliver_all(&sink, one_thread(Some("16505551234"), None, &messages)).await;
        let got: Vec<_> = thread(store.as_ref(), "16505551234")
            .await
            .into_iter()
            .map(|r| (r.id.as_str().to_owned(), r.direction))
            .collect();
        assert_eq!(
            got,
            [
                ("wamid.formatted".to_owned(), Direction::Outbound),
                ("wamid.to-only".to_owned(), Direction::Outbound),
                ("wamid.blank-from".to_owned(), Direction::Outbound),
                ("wamid.customer".to_owned(), Direction::Inbound),
            ]
        );
        assert!(!same_number("", ""), "no digits is no number");
        assert!(!same_number("+", "-"));
        assert!(same_number("+15550783881", "1 (555) 078-3881"));
    }

    /// BSUID era: the thread's `context.user_id` is the conversation of
    /// the business's messages too, not their `to` phone number; and an
    /// echo carrying both a BSUID and a phone number joins the BSUID one.
    #[tokio::test]
    async fn the_customers_bsuid_wins_over_a_phone_number() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        let context = json!({"user_id": "US.13491208655302741918", "wa_id": "16505551234"});
        let messages = json!([{"from": "15550783881", "to": "16505551234", "id": "wamid.out",
            "timestamp": "1739230901", "type": "text", "text": {"body": "hello"}}]);
        deliver_all(
            &sink,
            one_thread(Some("16505551234"), Some(&context), &messages),
        )
        .await;
        deliver_all(
            &sink,
            change(
                "smb_message_echoes",
                &json!({
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "message_echoes": [{"from": "15550783881", "to": "16505551234",
                        "to_user_id": "US.13491208655302741918",
                        "id": "wamid.echo", "timestamp": "1739231024", "type": "text",
                        "text": {"body": "from the app"}}]
                }),
            ),
        )
        .await;
        let ids: Vec<_> = thread(store.as_ref(), "US.13491208655302741918")
            .await
            .into_iter()
            .map(|r| r.id.as_str().to_owned())
            .collect();
        assert_eq!(ids, ["wamid.out", "wamid.echo"]);
        assert!(thread(store.as_ref(), "16505551234").await.is_empty());
    }

    /// When the whole `history` value fell to `WebhookEvent::Unknown`: a
    /// thread whose `context` is malformed is skipped whole (guessing its
    /// customer could mix two), the other threads are recorded with their
    /// thread id as the conversation, and a value without `metadata` is
    /// acknowledged with nothing recorded.
    #[tokio::test]
    async fn the_recovered_history_path_skips_what_it_cannot_place() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        let events = change(
            "history",
            &json!({
                "messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "history": [{"threads": [
                    {"id": "16505551234", "context": {"user_id": 5},
                        "messages": [{"from": "16505551234", "id": "wamid.hidden",
                            "timestamp": "1739230901", "type": "text", "text": {"body": "?"}}]},
                    {"id": "12125557890",
                        "messages": [{"from": "15550783881", "id": "wamid.kept",
                            "timestamp": "1739230902", "type": "text", "text": {"body": "kept"}}]}
                ]}]
            }),
        );
        assert!(
            matches!(events[0], WebhookEvent::Unknown { .. }),
            "{:?}",
            events[0].kind()
        );
        deliver_all(&sink, events).await;
        assert!(thread(store.as_ref(), "16505551234").await.is_empty());
        let kept = thread(store.as_ref(), "12125557890").await;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].direction, Direction::Outbound);

        let events = change(
            "history",
            &json!({"messaging_product": "whatsapp", "history": [{"threads": [{"id": "1",
                "messages": [{"from": "1", "id": "wamid.x", "timestamp": "1", "type": "text",
                    "text": {"body": "x"}}]}]}]}),
        );
        assert!(matches!(events[0], WebhookEvent::Unknown { .. }));
        deliver_all(&sink, events).await;
        assert_eq!(
            store
                .conversations(&PNID.into(), None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// A revoke deletes only a message sent in its direction: a customer
    /// revokes what they sent, the business (an echo) what it sent.
    #[tokio::test]
    async fn a_revoke_only_deletes_a_message_of_its_direction() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            inbound("wamid.in", 1_760_000_000, None, Some("16505551234"), "hi"),
        )
        .await;
        let echo = |id: &str, ts: &str, content: &serde_json::Value| {
            let mut echo = json!({"from": "15550783881", "to": "16505551234", "id": id,
                "timestamp": ts});
            for (k, v) in content.as_object().unwrap() {
                echo[k] = v.clone();
            }
            change(
                "smb_message_echoes",
                &json!({
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "message_echoes": [echo]
                }),
            )
        };
        deliver_all(
            &sink,
            echo(
                "wamid.out",
                "1760000001",
                &json!({"type": "text", "text": {"body": "hello"}}),
            ),
        )
        .await;
        // The business "revokes" the customer's message, the customer the
        // business's: neither applies.
        deliver_all(
            &sink,
            echo(
                "wamid.r1",
                "1760000002",
                &json!({"type": "revoke", "revoke": {"original_message_id": "wamid.in"}}),
            ),
        )
        .await;
        deliver_all(
            &sink,
            payload(&json!({
                "messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "contacts": [{"profile": {"name": "Sheena"}, "wa_id": "16505551234"}],
                "messages": [{"from": "16505551234", "id": "wamid.r2", "timestamp": "1760000003",
                    "type": "revoke", "revoke": {"original_message_id": "wamid.out"}}]
            })),
        )
        .await;
        let statuses: Vec<_> = thread(store.as_ref(), "16505551234")
            .await
            .into_iter()
            .map(|r| (r.id.as_str().to_owned(), r.status))
            .collect();
        assert_eq!(
            statuses,
            [
                ("wamid.in".to_owned(), DeliveryStatus::Received),
                ("wamid.out".to_owned(), DeliveryStatus::Sent),
            ]
        );
        // Each in its own direction does.
        deliver_all(
            &sink,
            echo(
                "wamid.r3",
                "1760000004",
                &json!({"type": "revoke", "revoke": {"original_message_id": "wamid.out"}}),
            ),
        )
        .await;
        let rows = thread(store.as_ref(), "16505551234").await;
        assert_eq!(rows[1].status, DeliveryStatus::Deleted);
    }

    /// A revoke that arrives before its message (live, then the history
    /// chunk that carries the original) leaves a tombstone: the content the
    /// customer deleted is never stored.
    #[tokio::test]
    async fn a_revoke_before_its_message_keeps_the_content_out() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            one_message(&json!({"from": "16505551234", "id": "wamid.rev",
                "timestamp": "1749854575", "type": "revoke",
                "revoke": {"original_message_id": "wamid.a"}})),
        )
        .await;
        deliver_all(
            &sink,
            one_thread(
                Some("16505551234"),
                Some(&json!({"user_id": "US.1", "wa_id": "16505551234"})),
                &json!([{"from": "16505551234", "from_user_id": "US.1", "id": "wamid.a",
                    "timestamp": "1749854000", "type": "text",
                    "text": {"body": "my card number is 4111"}}]),
            ),
        )
        .await;
        let rows = thread(store.as_ref(), "US.1").await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            (
                rows[0].kind.as_str(),
                rows[0].text.as_deref(),
                rows[0].status
            ),
            (StoredMessage::REVOKED, None, DeliveryStatus::Deleted)
        );
        assert!(
            !rows[0].payload.to_string().contains("4111"),
            "{}",
            rows[0].payload
        );
        let key = ConversationKey::new(PNID, "US.1");
        assert_eq!(store.last_inbound_at(&key).await.unwrap(), None);
    }

    /// A phone with a wrong clock must not pin a conversation to the top of
    /// the inbox: synced device timestamps are bounded by the sink's clock
    /// (plus 5 minutes); the payload keeps Meta's.
    #[tokio::test]
    async fn synced_device_timestamps_are_bounded_by_the_clock() {
        let store = Arc::new(MemoryConversationStore::new());
        let clock = ManualClock::new(datetime!(2025-02-11 00:00 UTC));
        let sink = InboxSink::new(store.clone()).with_clock(Arc::new(clock.clone()));
        // 4102444800 is 2100-01-01.
        deliver_all(
            &sink,
            one_thread(
                Some("16505551234"),
                None,
                &json!([{"from": "16505551234", "id": "wamid.future", "timestamp": "4102444800",
                    "type": "text", "text": {"body": "from the future"}},
                    {"from": "15550783881", "id": "wamid.past", "timestamp": "1739230900",
                    "type": "text", "text": {"body": "on time"}}]),
            ),
        )
        .await;
        let rows = thread(store.as_ref(), "16505551234").await;
        assert_eq!(rows[0].timestamp.unix_timestamp(), 1_739_230_900);
        assert_eq!(
            rows[1].timestamp,
            datetime!(2025-02-11 00:05 UTC),
            "bounded at the clock plus the skew"
        );
        assert_eq!(rows[1].payload["timestamp"], "4102444800");
        // A synced revoke from the future leaves its tombstone at the bound.
        deliver_all(
            &sink,
            one_thread(
                Some("16505551234"),
                None,
                &json!([{"from": "16505551234", "id": "wamid.rev", "timestamp": "4102444800",
                    "type": "revoke", "revoke": {"original_message_id": "wamid.unknown"}}]),
            ),
        )
        .await;
        let tombstone = thread(store.as_ref(), "16505551234")
            .await
            .into_iter()
            .find(|r| r.id.as_str() == "wamid.unknown")
            .unwrap();
        assert_eq!(tombstone.timestamp, datetime!(2025-02-11 00:05 UTC));
        // A later live message is the conversation's latest again.
        clock.set(datetime!(2025-02-11 01:00 UTC));
        deliver_all(
            &sink,
            inbound(
                "wamid.live",
                1_739_235_600,
                None,
                Some("16505551234"),
                "now",
            ),
        )
        .await;
        let key = ConversationKey::new(PNID, "16505551234");
        assert_eq!(
            summary_of(store.as_ref(), &key)
                .await
                .unwrap()
                .last_text
                .as_deref(),
            Some("now")
        );
    }

    /// A store that counts its write calls.
    #[derive(Debug, Default)]
    struct CountingStore {
        inner: MemoryConversationStore,
        appends: std::sync::atomic::AtomicUsize,
        synced_calls: std::sync::atomic::AtomicUsize,
        /// `erase_all` calls (`erase` is its one-key case).
        erasures: std::sync::atomic::AtomicUsize,
        /// `identities` calls.
        reads: std::sync::atomic::AtomicUsize,
        /// Answer `append_synced` with one answer too few (a broken store).
        short: bool,
        /// Answer `thread_owner` with `None`: the read of a replica that
        /// ran before another replica's write landed (a race).
        stale_owner: bool,
    }

    #[async_trait]
    impl ConversationStore for CountingStore {
        async fn append(&self, m: StoredMessage) -> std::result::Result<bool, StorageError> {
            self.appends
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.append(m).await
        }
        async fn append_synced(
            &self,
            messages: Vec<StoredMessage>,
        ) -> std::result::Result<Vec<bool>, StorageError> {
            self.synced_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut answers = self.inner.append_synced(messages).await?;
            if self.short {
                answers.pop();
            }
            Ok(answers)
        }
        async fn fill_media_placeholder(
            &self,
            phone_number_id: &PhoneNumberId,
            id: &MessageId,
            kind: String,
            text: Option<String>,
            payload: serde_json::Value,
        ) -> std::result::Result<bool, StorageError> {
            self.inner
                .fill_media_placeholder(phone_number_id, id, kind, text, payload)
                .await
        }
        async fn revoke(
            &self,
            key: &ConversationKey,
            id: &MessageId,
            direction: Direction,
            at: OffsetDateTime,
        ) -> std::result::Result<bool, StorageError> {
            self.inner.revoke(key, id, direction, at).await
        }
        async fn update_status(
            &self,
            phone_number_id: &PhoneNumberId,
            id: &MessageId,
            status: DeliveryStatus,
            at: OffsetDateTime,
            error: Option<serde_json::Value>,
        ) -> std::result::Result<bool, StorageError> {
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
        async fn message(
            &self,
            phone_number_id: &PhoneNumberId,
            id: &MessageId,
        ) -> std::result::Result<Option<StoredMessage>, StorageError> {
            self.inner.message(phone_number_id, id).await
        }
        async fn record_window_event(
            &self,
            event: meta_whatsapp_core::store::WindowEvent,
        ) -> std::result::Result<bool, StorageError> {
            self.inner.record_window_event(event).await
        }
        async fn window_events(
            &self,
            key: &ConversationKey,
            before: Option<(OffsetDateTime, String)>,
            limit: usize,
        ) -> std::result::Result<Vec<meta_whatsapp_core::store::WindowEvent>, StorageError>
        {
            self.inner.window_events(key, before, limit).await
        }
        async fn set_thread_owner(
            &self,
            key: &ConversationKey,
            ownership: meta_whatsapp_core::store::ThreadOwnership,
        ) -> std::result::Result<bool, StorageError> {
            self.inner.set_thread_owner(key, ownership).await
        }
        async fn thread_owner(
            &self,
            key: &ConversationKey,
        ) -> std::result::Result<Option<meta_whatsapp_core::store::ThreadOwnership>, StorageError>
        {
            if self.stale_owner {
                return Ok(None);
            }
            self.inner.thread_owner(key).await
        }
        async fn put_contact(
            &self,
            contact: meta_whatsapp_core::store::StoredContact,
        ) -> std::result::Result<bool, StorageError> {
            self.inner.put_contact(contact).await
        }
        async fn remove_contact(
            &self,
            key: &ConversationKey,
            at: OffsetDateTime,
        ) -> std::result::Result<bool, StorageError> {
            self.inner.remove_contact(key, at).await
        }
        async fn contact(
            &self,
            key: &ConversationKey,
        ) -> std::result::Result<Option<meta_whatsapp_core::store::StoredContact>, StorageError>
        {
            self.inner.contact(key).await
        }
        async fn contacts(
            &self,
            phone_number_id: &PhoneNumberId,
            after: Option<String>,
            limit: usize,
        ) -> std::result::Result<Vec<meta_whatsapp_core::store::StoredContact>, StorageError>
        {
            self.inner.contacts(phone_number_id, after, limit).await
        }
        async fn link_identity(
            &self,
            link: meta_whatsapp_core::store::IdentityLink,
        ) -> std::result::Result<bool, StorageError> {
            self.inner.link_identity(link).await
        }
        async fn identity_links(
            &self,
            key: &ConversationKey,
        ) -> std::result::Result<Vec<meta_whatsapp_core::store::IdentityLink>, StorageError>
        {
            self.inner.identity_links(key).await
        }
        async fn identities(
            &self,
            key: &ConversationKey,
        ) -> std::result::Result<BTreeSet<String>, StorageError> {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.identities(key).await
        }
        async fn erase_all(
            &self,
            phone_number_id: &PhoneNumberId,
            contacts: &[String],
        ) -> std::result::Result<Erased, StorageError> {
            self.erasures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.erase_all(phone_number_id, contacts).await
        }
        async fn purge_before(
            &self,
            phone_number_id: Option<&PhoneNumberId>,
            cutoff: OffsetDateTime,
        ) -> std::result::Result<meta_whatsapp_core::store::Purged, StorageError> {
            self.inner.purge_before(phone_number_id, cutoff).await
        }
    }

    /// A history webhook can carry thousands of messages: a chunk is one
    /// store call, not one per message (security review L5: one round trip
    /// per item outlived the dedup lease, and Meta's retry ran a second
    /// pass concurrently), on the typed and on the recovered path; the
    /// media contents of a value are one call too.
    #[tokio::test]
    async fn a_history_chunk_is_one_store_call() {
        let store = Arc::new(CountingStore::default());
        let sink = InboxSink::new(store.clone());
        let messages: Vec<_> = (0..500)
            .map(|i| {
                json!({"from": if i % 2 == 0 { "16505551234" } else { "15550783881" },
                    "id": format!("wamid.{i}"), "timestamp": (1_739_230_000 + i).to_string(),
                    "type": "text", "text": {"body": format!("m{i}")}})
            })
            .collect();
        deliver_all(
            &sink,
            one_thread(Some("16505551234"), None, &json!(messages)),
        )
        .await;
        let calls = |s: &CountingStore| {
            (
                s.appends.load(std::sync::atomic::Ordering::Relaxed),
                s.synced_calls.load(std::sync::atomic::Ordering::Relaxed),
            )
        };
        assert_eq!(calls(&store), (0, 1));
        let key = ConversationKey::new(PNID, "16505551234");
        assert_eq!(
            store.inner.messages(&key, None, 1000).await.unwrap().len(),
            500
        );

        let mut two_chunks = messages[..10].to_vec();
        two_chunks.push(json!({"from": "16505551234", "timestamp": "1"}));
        deliver_all(
            &sink,
            change(
                "history",
                &json!({
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "history": [
                        {"threads": [{"id": "16505551234", "messages": two_chunks}]},
                        {"threads": [{"id": "12125557890", "messages": messages[10..20]}]}
                    ],
                    "messages": [{"from": "16505551234", "id": "wamid.m1", "timestamp": "1739231000",
                        "type": "image", "image": {"id": "1"}},
                        {"from": "16505551234", "id": "wamid.m2", "timestamp": "1739231001",
                        "type": "image", "image": {"id": "2"}}]
                }),
            ),
        )
        .await;
        assert_eq!(
            calls(&store),
            (0, 4),
            "two chunks and one batch of contents"
        );

        // A store that answers for fewer messages than it was given fails
        // the delivery (Meta redelivers) instead of mismatching the fills.
        let broken = Arc::new(CountingStore {
            short: true,
            ..CountingStore::default()
        });
        let sink = InboxSink::new(broken);
        for event in body(HISTORY_MEDIA) {
            assert!(sink.deliver(event).await.is_err());
        }
    }

    /// Media content items name their customer through the value's
    /// `contacts`: by BSUID, else by phone number in any format.
    #[tokio::test]
    async fn media_contents_find_their_customer_in_the_contacts() {
        let store = Arc::new(MemoryConversationStore::new());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            change(
                "history",
                &json!({
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "contacts": [
                        {"profile": {"name": "A"}, "wa_id": "16505551234", "user_id": "US.A"},
                        {"profile": {"name": "B"}, "wa_id": "12125557890", "user_id": "US.B"},
                        {"profile": {"name": "A?"}, "wa_id": "+1 650 555 1234", "user_id": "US.C"}
                    ],
                    "messages": [{"from": "+1 650-555-1234", "id": "wamid.a", "timestamp": "1739231000",
                        "type": "image", "image": {"id": "1"}}],
                    "message_echoes": [{"from": "15550783881", "to": "12125557890", "id": "wamid.b",
                        "timestamp": "1739231001", "type": "image", "image": {"id": "2"}}]
                }),
            ),
        )
        .await;
        assert_eq!(
            thread(store.as_ref(), "US.A").await.len(),
            1,
            "the first contact of a number wins"
        );
        assert_eq!(thread(store.as_ref(), "US.B").await.len(), 1);
    }

    /// Content in history and echoes is recorded exactly too.
    #[tokio::test]
    async fn a_nul_in_synced_or_echoed_content_is_recorded_exactly() {
        let store = Arc::new(PostgresRulesStore::default());
        let sink = InboxSink::new(store.clone());
        deliver_all(
            &sink,
            history_with(
                &json!({"from": "15550783881", "id": "wamid.n", "timestamp": "1739230950",
                "type": "text", "text": {"body": "code\u{0}42"}}),
            ),
        )
        .await;
        deliver_all(
            &sink,
            change(
                "smb_message_echoes",
                &json!({
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "message_echoes": [{"from": "15550783881", "to": "16505551234",
                        "id": "wamid.e", "timestamp": "1739231000", "type": "text",
                        "text": {"body": "see\u{0}you"}}]
                }),
            ),
        )
        .await;
        let rows = thread(&store.inner, "16505551234").await;
        let texts: Vec<_> = rows.iter().map(|r| r.text.as_deref().unwrap()).collect();
        assert_eq!(texts, ["before", "code\u{0}42", "after", "see\u{0}you"]);
        let bodies: Vec<_> = rows
            .iter()
            .map(|r| r.payload["text"]["body"].as_str().unwrap())
            .collect();
        assert_eq!(bodies, texts, "the payloads keep it too");
    }

    /// Storage failures still fail the delivery, so Meta redelivers the
    /// history instead of it being lost.
    #[tokio::test]
    async fn a_storage_failure_during_history_fails_the_delivery() {
        let store = Arc::new(PostgresRulesStore {
            down: true,
            ..PostgresRulesStore::default()
        });
        let sink = InboxSink::new(store);
        for event in body(HISTORY_THREADS)
            .into_iter()
            .chain(body(ECHO_TEXT))
            .chain(history_with(
                &json!({"from": "16505551234", "timestamp": "1"}),
            ))
        {
            let kind = event.kind();
            assert!(sink.deliver(event).await.is_err(), "{kind}");
        }
    }

    /// Two replicas, one second: one records a `control_passed`, the other
    /// a standby copy of that second whose read of the owner ran before the
    /// handover's write and whose write lands after it. The handover still
    /// wins, so the merchant can answer the customer who asked for them.
    #[tokio::test]
    async fn a_racing_standby_copy_never_overrides_a_handover_of_its_second() {
        let shared = MemoryConversationStore::new();
        let handover = change(
            "messaging_handovers",
            &json!({"messaging_product": "whatsapp", "sender": {"phone_number": "16505551234"},
                "recipient": {"phone_number_id": PNID, "display_phone_number": "15550783881"},
                "type": "control_passed", "timestamp": "1750101000",
                "control_passed": {"previous_owner_role": "ai_agent", "new_owner_role": "escalation"}}),
        );
        let standby = change(
            "standby",
            &json!({"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "standby": {"messages": [{"from": "16505551234", "id": "wamid.S",
                    "timestamp": "1750101000", "type": "text", "text": {"body": "a human, please"}}]}}),
        );
        deliver_all(&InboxSink::new(Arc::new(shared.clone())), handover).await;
        let replica = Arc::new(CountingStore {
            inner: shared.clone(),
            stale_owner: true,
            ..CountingStore::default()
        });
        deliver_all(&InboxSink::new(replica), standby).await;

        let clock = ManualClock::new(datetime!(2025-06-16 19:11:00 UTC));
        let t = ScriptedTransport::new();
        let inbox = inbox(&t, Arc::new(shared), &clock);
        let key = inbox.key("16505551234");
        let owner = inbox.thread_owner(&key).await.unwrap().unwrap();
        assert_eq!(
            (owner.owner, owner.role.as_deref()),
            (ThreadOwner::ThisApp, Some("escalation"))
        );
        assert!(
            inbox.window_is_open(&key).await.unwrap(),
            "the standby copy"
        );
        t.push_json(200, sent("wamid.R"));
        inbox
            .reply(&key, Text::new("Hi, I'm here").into())
            .await
            .unwrap();
        assert_eq!(t.remaining(), 0);
    }
}
