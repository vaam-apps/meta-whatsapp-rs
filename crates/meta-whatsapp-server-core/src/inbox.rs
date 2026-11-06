//! The inbox (docs/design/server.md, section 4.2, "Inbox"; roadmaps M2a and
//! M2g), as services an API adapter calls, on a number the caller's tenant
//! owns: its conversations, newest activity first ([`Conversations::list`],
//! and with each one's window, [`Conversations::list_with_windows`]); one
//! conversation's history, newest first ([`Conversations::history`]); its
//! 24-hour window as a reply would meet it ([`Conversations::window`]); a
//! message of the number by id ([`Conversations::message`]); marking a
//! conversation read, and on request its latest inbound message read on
//! Meta ([`Conversations::mark_read`]); and a reply to the conversation's
//! contact ([`Conversations::reply`]) through the library's inbox
//! (`meta_whatsapp_rs::inbox::Inbox::send`, which `Inbox::reply` is: the
//! same checks before any request).
//!
//! The inbox is the bundle's `ConversationStore`, the one the webhook
//! pipeline records into ([`crate::inbound`]): a message is there once its
//! webhook is answered, and a reply is recorded `accepted` there until its
//! status webhooks move it on.
//!
//! **The binding epoch** (decision D10: hidden, never purged). The store is
//! keyed by business number, not by tenant, so a number whose WABA moved
//! from another tenant keeps the previous holder's history. Its holder
//! reads only what is dated from its binding's start on
//! ([`crate::authz::history_from`]: a message's `timestamp`, a
//! conversation's `last_message_at`); a conversation whose latest message
//! is older is not listed, and a listed one whose customer last wrote
//! before the binding shows no `last_inbound_at` and no unread count. A
//! WABA no other tenant held shows all of it (a coexistence onboarding's
//! history, synced from before its binding, included). A listed
//! conversation's `unread` counts only the customer's messages dated from
//! the cut on (at most [`UNREAD_SCAN`] of its latest messages are read to
//! count them, only on a number with a cut). A reply follows the same cut
//! (see "The reply's checks" below), and so do the window (a window the
//! customer opened before the cut shows no `closes_at`), a message by id
//! (one dated before the cut is `404 not_found`, as a missing one) and the
//! read receipt (never for a message dated before the cut). A page, a
//! window, a message or a read receipt is answered (or sent) only if the
//! number's binding did not move while it was read
//! ([`crate::authz::OwnedNumber`]'s epoch and binding: `503
//! storage_unavailable`, retryable, otherwise).
//!
//! **Pages** take a limit (1 to [`MAX_PAGE_SIZE`]) and an exclusive
//! cursor: the store's own (`(last_message_at, contact)` of a page's last
//! conversation, `(timestamp, id)` of its last message), written as a
//! string ([`Listing::next_after`]) that an adapter wraps opaquely (the
//! HTTP one: `next_cursor`, as every list of `/v1` but `GET /v1/events`):
//! no repeats, no gaps, also when times collide.
//!
//! **The reply's checks** are the library's [`ReplyChecks`], per number
//! ([`ReplyChecksByNumber`], a setting of the deployment: every check on a
//! number it does not list). A free-form reply outside the window is `409
//! customer_service_window_closed`, and one while another app owns the
//! thread `409 thread_owned_elsewhere`, before any request; a template is
//! exempt from both. On a number with a cut, a free-form reply needs a
//! window opened from the cut on (the customer's message, a call or a
//! standby message dated from it, or, with the handover trusted, a
//! handover to this app recorded since): a window the previous holder's
//! customer opened is not the new holder's conversation to continue, and
//! answering it would tell the new holder that the customer wrote to the
//! previous one. Otherwise `409 customer_service_window_closed`, before
//! the library's checks and any request; a template goes (a new
//! conversation, the number's to start). To swap: drop the calls of
//! `cut_refuses_free_form`, [`Conversations::reply`]'s and the window's
//! (both, so that they keep agreeing). The window ([`ReplyWindow::open`])
//! is those checks run with nothing sent, in the same order, so it never
//! disagrees with a free-form reply's refusal.
//!
//! **The read receipt** ([`Conversations::mark_read`] with
//! `notify_customer`) goes for the conversation's latest inbound message
//! only (Meta marks the earlier ones of a one-to-one conversation read with
//! it, `messages/mark-message-as-read`), with the number's token, before
//! the unread count is reset: a receipt Meta refuses resets nothing. None
//! goes when that message is dated before the cut, is older than Meta's
//! 30 days ([`READ_RECEIPT_MAX_AGE`]), or is not among the conversation's
//! latest [`UNREAD_SCAN`] messages.

use std::collections::BTreeMap;
use std::sync::Arc;

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::messages::{DirectSendCategory, MessageContent, OutboundMessage};
use meta_whatsapp_rs::core::clock::Clock;
use meta_whatsapp_rs::core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_rs::core::store::{
    ConversationKey, ConversationStore, ConversationSummary, CustomerServiceWindow, Direction,
    StoredMessage, ThreadOwner,
};
use meta_whatsapp_rs::inbox::{Inbox, ReplyChecks};
use time::OffsetDateTime;

use crate::authz::{Authorizer, OwnedNumber};
use crate::error::{ErrorCode, ServiceError};
use crate::messages::{self, Accepted, ReplyMessage};
use crate::model::{Listing, MAX_PAGE_SIZE, PageRequest};
use crate::observe::Observer;

/// Longest conversation contact accepted, in bytes: a BSUID, a `wa_id` or
/// a group id is far shorter.
pub const MAX_CONTACT_BYTES: usize = 256;

/// On a number with a cut, how many of a listed conversation's latest
/// messages are read, at most, to count its unread messages dated from
/// the cut on: past them, the count is what was found (fewer, never a
/// previous holder's).
pub const UNREAD_SCAN: usize = 1000;

/// Longest message id accepted, in bytes: a `wamid.…` is far shorter.
pub const MAX_MESSAGE_ID_BYTES: usize = 256;

/// How old a received message may be for a read receipt: Meta's "within
/// 30 days of receipt" (`messages/mark-message-as-read`). An older latest
/// message gets none ([`Conversations::mark_read`]), rather than Meta's
/// refusal.
pub const READ_RECEIPT_MAX_AGE: time::Duration = time::Duration::days(30);

/// The conversation of `contact` on `number`: `contact` as the request
/// named it (a BSUID, a `wa_id`, a group id), 1 to [`MAX_CONTACT_BYTES`]
/// bytes, not blank and without a control character.
///
/// # Errors
///
/// `422 invalid_request` on `contact`, never quoting it.
pub fn conversation_key(
    number: &PhoneNumberId,
    contact: &str,
) -> Result<ConversationKey, ServiceError> {
    let valid = !contact.trim().is_empty()
        && contact.len() <= MAX_CONTACT_BYTES
        && !contact.chars().any(char::is_control);
    if valid {
        Ok(ConversationKey::new(number.clone(), contact))
    } else {
        Err(ServiceError::invalid("contact"))
    }
}

/// A message id as a request named it (`wamid.…`): 1 to
/// [`MAX_MESSAGE_ID_BYTES`] bytes, not blank and without a control
/// character.
///
/// # Errors
///
/// `422 invalid_request` on `message_id`, never quoting it.
pub fn message_id(id: &str) -> Result<MessageId, ServiceError> {
    let valid = !id.trim().is_empty()
        && id.len() <= MAX_MESSAGE_ID_BYTES
        && !id.chars().any(char::is_control);
    if valid {
        Ok(MessageId::new(id))
    } else {
        Err(ServiceError::invalid("message_id"))
    }
}

/// A conversation's 24-hour window as its holder reads it
/// ([`Conversations::window`]): whether a free-form reply would go to Meta
/// now, beside the window the customer opened, as recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyWindow {
    /// Whether a free-form reply would be sent to Meta now: the checks
    /// [`Conversations::reply`] makes (the cut's, then the library's
    /// `Inbox::check_reply`: the thread's owner, then the window, a
    /// trusted handover letting Meta decide), with the number's
    /// [`ReplyChecks`] and the service's clock. `false`: send a template.
    /// Meta still decides what it is sent.
    pub open: bool,
    /// When the window the customer opened closes, or closed, as the inbox
    /// recorded it (24 hours after the latest of their last message, a
    /// call that reopens it and a message of theirs seen in standby), from
    /// the binding's start on; `None` when they opened none since. It may
    /// be past while [`Self::open`] is true (a handover the checks trust,
    /// or the window check off), and future while it is false (another
    /// app owns the thread).
    pub closes_at: Option<OffsetDateTime>,
}

/// A conversation of a page of [`Conversations::list_with_windows`]: its
/// summary, as [`Conversations::list`] shows it, and its window.
#[derive(Debug, Clone, PartialEq)]
pub struct ListedConversation {
    /// The conversation, under the cut.
    pub summary: ConversationSummary,
    /// Its window ([`Conversations::window`]'s answer).
    pub window: ReplyWindow,
}

/// A page position: a time and an id, written `<Unix nanoseconds>:<id>`.
fn position(at: OffsetDateTime, id: &str) -> String {
    format!("{}:{id}", at.unix_timestamp_nanos())
}

/// The position `cursor` wrote ([`position`]).
///
/// # Errors
///
/// `422 invalid_request` on `cursor`.
fn read_position(cursor: &str) -> Result<(OffsetDateTime, String), ServiceError> {
    let invalid = || ServiceError::invalid("cursor");
    let (nanos, id) = cursor.split_once(':').ok_or_else(invalid)?;
    let at = nanos
        .parse::<i128>()
        .ok()
        .and_then(|nanos| OffsetDateTime::from_unix_timestamp_nanos(nanos).ok())
        .ok_or_else(invalid)?;
    if id.is_empty() {
        return Err(invalid());
    }
    Ok((at, id.to_owned()))
}

/// Whether a record dated `at` is the holder's to read, under the cut
/// `from` ([`crate::authz::history_from`]): dated at or after it. The
/// media services cut received media ids with it too (roadmap M2e,
/// `crate::media::Received::among`).
pub(crate) fn visible(from: Option<OffsetDateTime>, at: OffsetDateTime) -> bool {
    from.is_none_or(|from| at >= from)
}

/// `summary` as its holder reads it under the cut `from`: a customer who
/// last wrote before it shows no `last_inbound_at`, and no unread count
/// (every unread message is inbound, so none is newer).
fn masked(mut summary: ConversationSummary, from: Option<OffsetDateTime>) -> ConversationSummary {
    if summary.last_inbound_at.is_some_and(|at| !visible(from, at)) {
        summary.last_inbound_at = None;
        summary.unread = 0;
    }
    summary
}

/// The library's [`ReplyChecks`] per business number: a deployment's
/// setting (the service's `WA_SERVER_INBOX_REPLY_CHECKS`), every check on
/// ([`ReplyChecks::ALL`]) for a number it does not list. Turn the
/// ownership check off on the number of a tenant that is the account's
/// designated escalation partner (its service message takes the thread),
/// and the handover trust off on one that receives standby copies (its
/// window is known): `OPEN_QUESTIONS.md` #32, #44.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplyChecksByNumber {
    numbers: BTreeMap<PhoneNumberId, ReplyChecks>,
}

/// The names of the checks, as [`ReplyChecksByNumber::parse`] reads them.
pub const REPLY_CHECK_NAMES: [&str; 3] = ["window", "trust_handover", "thread_owner"];

/// Why a value of [`ReplyChecksByNumber::parse`] was refused; never quotes
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidReplyChecks(pub &'static str);

impl ReplyChecksByNumber {
    /// Every check on every number.
    pub fn new() -> Self {
        Self::default()
    }

    /// `checks` on `number`.
    #[must_use]
    pub fn with(mut self, number: PhoneNumberId, checks: ReplyChecks) -> Self {
        self.numbers.insert(number, checks);
        self
    }

    /// The checks of `number`: its own, else every one.
    pub fn of(&self, number: &PhoneNumberId) -> ReplyChecks {
        self.numbers
            .get(number)
            .copied()
            .unwrap_or(ReplyChecks::ALL)
    }

    /// The numbers listed, with their checks, in id order.
    pub fn iter(&self) -> impl Iterator<Item = (&PhoneNumberId, ReplyChecks)> {
        self.numbers
            .iter()
            .map(|(number, checks)| (number, *checks))
    }

    /// Whether no number is listed.
    pub fn is_empty(&self) -> bool {
        self.numbers.is_empty()
    }

    /// Read `value`: comma-separated `<phone_number_id>:<checks>` entries,
    /// `<checks>` being `all`, `none`, or the checks left on joined by `+`
    /// (`window`, `trust_handover`, `thread_owner`, each once):
    /// `106540352242922:window+trust_handover` turns the ownership check
    /// off on that number. Spaces around a part are ignored; an empty value
    /// lists nothing.
    ///
    /// # Errors
    ///
    /// A malformed entry, a phone number id that is not one, a number
    /// listed twice, an unknown or repeated check, or `trust_handover`
    /// without `window` (it only applies while the window is checked).
    pub fn parse(value: &str) -> Result<Self, InvalidReplyChecks> {
        let mut parsed = Self::new();
        for entry in value.split(',').map(str::trim) {
            if entry.is_empty() {
                continue;
            }
            let (number, checks) = entry.split_once(':').ok_or(InvalidReplyChecks(
                "expected <phone_number_id>:<checks>, comma-separated",
            ))?;
            let number = PhoneNumberId::new(number.trim())
                .map_err(|_| InvalidReplyChecks("expected a phone number id (digits)"))?;
            let checks = checks_named(checks.trim())?;
            if parsed.numbers.insert(number, checks).is_some() {
                return Err(InvalidReplyChecks("a phone number id appears twice"));
            }
        }
        Ok(parsed)
    }
}

/// The checks `names` turns on: `all`, `none`, or names joined by `+`.
fn checks_named(names: &str) -> Result<ReplyChecks, InvalidReplyChecks> {
    match names {
        "all" => return Ok(ReplyChecks::ALL),
        "none" => return Ok(ReplyChecks::NONE),
        _ => {}
    }
    let mut on = [false; 3];
    for name in names.split('+').map(str::trim) {
        let i = REPLY_CHECK_NAMES
            .iter()
            .position(|known| *known == name)
            .ok_or(InvalidReplyChecks(
                "expected all, none, or window, trust_handover, thread_owner joined by +",
            ))?;
        if on[i] {
            return Err(InvalidReplyChecks("a check appears twice"));
        }
        on[i] = true;
    }
    let [window, trust_handover, thread_owner] = on;
    if trust_handover && !window {
        return Err(InvalidReplyChecks(
            "trust_handover applies only with window",
        ));
    }
    Ok(ReplyChecks::NONE
        .window(window)
        .trust_handover(trust_handover)
        .thread_owner(thread_owner))
}

/// The inbox operations on a number the caller's tenant owns: an
/// [`OwnedNumber`] its [`Authorizer`] made, and admits (ownership and the
/// vault already checked: steps 4 and 5 of [`crate::authz`]), over the
/// bundle's inbox, replying with that number's token. A failed Graph call
/// is told to the [`Observer`] (the adapter's metrics), and a `190` marks
/// the WABA's numbers `reconnect_required` ([`OwnedNumber::failed`]).
pub struct Conversations<'a> {
    authz: &'a Authorizer,
    number: &'a OwnedNumber,
    inbox: Inbox,
    /// The inbox's store, read for what the library's inbox does not cut.
    store: Arc<dyn ConversationStore>,
    /// The service's clock, the inbox's own: a read receipt's age.
    clock: Arc<dyn Clock>,
    observer: &'a dyn Observer,
    /// [`Self::naming_the_account`].
    names_account: bool,
}

impl std::fmt::Debug for Conversations<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conversations")
            .field("number", self.number)
            .field("checks", &self.inbox.reply_checks())
            .finish_non_exhaustive()
    }
}

impl<'a> Conversations<'a> {
    /// The operations on `number`, once `authz` admits it
    /// ([`Authorizer::admit_number`]), over `store` (the bundle's
    /// `ConversationStore`, which the webhook pipeline records into),
    /// replying with the number's own `checks` (its entry of
    /// [`ReplyChecksByNumber`]) and reading "now" from `clock` (the
    /// service's one). Failed Graph calls are reported to nobody until
    /// [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `number` another [`Authorizer`] made: `403 forbidden`, logged at
    /// `warn`, and nothing is read or sent.
    pub fn new(
        authz: &'a Authorizer,
        number: &'a OwnedNumber,
        store: Arc<dyn ConversationStore>,
        checks: ReplyChecks,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ServiceError> {
        authz.admit_number(number)?;
        let inbox = Inbox::new(
            number.client().clone(),
            number.phone_number_id().clone(),
            Arc::clone(&store),
        )
        .with_clock(Arc::clone(&clock))
        .with_reply_checks(checks);
        Ok(Self {
            authz,
            number,
            inbox,
            store,
            clock,
            observer: &(),
            names_account: false,
        })
    }

    /// Whether a reply names the number's binding's WABA id as its
    /// `messaging_account_id`, as [`messages::Messages::naming_the_account`]
    /// does for a send (roadmap M5c5).
    #[must_use]
    pub fn naming_the_account(mut self, enabled: bool) -> Self {
        self.names_account = enabled;
        self
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// The checks a reply makes on this number.
    pub fn reply_checks(&self) -> ReplyChecks {
        self.inbox.reply_checks()
    }

    /// From when the number's history is its holder's to read
    /// ([`crate::authz::history_from`]); `None`, all of it.
    pub fn history_from(&self) -> Option<OffsetDateTime> {
        self.number.history_from()
    }

    /// The number's conversations, most recent activity first, from the
    /// binding's start on (the [module](self)'s binding epoch), at most
    /// `page.limit` of them after `page.after` (a previous page's
    /// [`Listing::next_after`]).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `cursor` (a cursor this did not write) or
    /// `limit`; the store failing, or the binding moving while the page was
    /// read: `503 storage_unavailable`.
    pub async fn list(
        &self,
        page: &PageRequest,
    ) -> Result<Listing<ConversationSummary>, ServiceError> {
        let listing = self.summaries(page).await?;
        // Answered only if the binding held throughout the reads.
        self.number.still_held(self.authz).await?;
        Ok(listing)
    }

    /// [`Self::list`], each conversation with its window
    /// ([`Self::window`]'s answer, read for each: a few reads of the store
    /// per conversation), so that a list and the window route never
    /// disagree (roadmap M2g).
    ///
    /// # Errors
    ///
    /// As [`Self::list`].
    pub async fn list_with_windows(
        &self,
        page: &PageRequest,
    ) -> Result<Listing<ListedConversation>, ServiceError> {
        let Listing { items, next_after } = self.summaries(page).await?;
        let mut listed = Vec::with_capacity(items.len());
        for summary in items {
            let window = self.reply_window(&summary.key).await?;
            listed.push(ListedConversation { summary, window });
        }
        // Answered only if the binding held throughout the reads.
        self.number.still_held(self.authz).await?;
        Ok(Listing {
            items: listed,
            next_after,
        })
    }

    /// The page [`Self::list`] answers, before the binding is read again.
    async fn summaries(
        &self,
        page: &PageRequest,
    ) -> Result<Listing<ConversationSummary>, ServiceError> {
        let limit = limit(page)?;
        let before = page.after.as_deref().map(read_position).transpose()?;
        let rows = self
            .inbox
            .conversations(before, limit.saturating_add(1))
            .await?;
        let from = self.number.history_from();
        let mut items: Vec<ConversationSummary> = rows
            .into_iter()
            .take_while(|summary| visible(from, summary.last_message_at))
            .collect();
        let more = items.len() > limit;
        items.truncate(limit);
        let next_after = more
            .then(|| items.last())
            .flatten()
            .map(|last| position(last.last_message_at, &last.key.contact));
        let mut shown = Vec::with_capacity(items.len());
        for summary in items {
            let mut summary = masked(summary, from);
            if let Some(cut) = from
                && summary.unread > 0
            {
                summary.unread = self.unread_since(&summary.key, cut, summary.unread).await?;
            }
            shown.push(summary);
        }
        Ok(Listing {
            items: shown,
            next_after,
        })
    }

    /// The unread messages of `key` dated from `cut` on: the customer's
    /// messages among its latest ones, newest first, down to the cut, at
    /// most `unread` (the store's count, by arrival since the last read)
    /// and reading at most [`UNREAD_SCAN`] messages. The store counts the
    /// previous holder's unread messages too.
    async fn unread_since(
        &self,
        key: &ConversationKey,
        cut: OffsetDateTime,
        unread: u64,
    ) -> Result<u64, ServiceError> {
        let mut counted = 0_u64;
        let mut read = 0_usize;
        let mut before = None;
        while counted < unread && read < UNREAD_SCAN {
            let page = self
                .inbox
                .history(key, before.take(), MAX_PAGE_SIZE.min(UNREAD_SCAN - read))
                .await?;
            let full = page.len() == MAX_PAGE_SIZE.min(UNREAD_SCAN - read);
            read = read.saturating_add(page.len());
            for message in &page {
                if !visible(Some(cut), message.timestamp) {
                    return Ok(counted.min(unread));
                }
                if message.direction == Direction::Inbound && message.kind != StoredMessage::REVOKED
                {
                    counted = counted.saturating_add(1);
                }
            }
            match page.last() {
                Some(last) if full => before = Some((last.timestamp, last.id.clone())),
                _ => break,
            }
        }
        Ok(counted.min(unread))
    }

    /// The history of `contact`'s conversation, newest first, from the
    /// binding's start on, at most `page.limit` messages after
    /// `page.after`.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `contact` ([`conversation_key`]), `cursor`
    /// or `limit`; `503 storage_unavailable` as for [`Self::list`].
    pub async fn history(
        &self,
        contact: &str,
        page: &PageRequest,
    ) -> Result<Listing<StoredMessage>, ServiceError> {
        let key = conversation_key(self.number.phone_number_id(), contact)?;
        let limit = limit(page)?;
        let before = page
            .after
            .as_deref()
            .map(read_position)
            .transpose()?
            .map(|(at, id)| (at, MessageId::new(id)));
        let rows = self
            .inbox
            .history(&key, before, limit.saturating_add(1))
            .await?;
        self.number.still_held(self.authz).await?;
        let from = self.number.history_from();
        let mut items: Vec<StoredMessage> = rows
            .into_iter()
            .take_while(|message| visible(from, message.timestamp))
            .collect();
        let more = items.len() > limit;
        items.truncate(limit);
        let next_after = more
            .then(|| items.last())
            .flatten()
            .map(|last| position(last.timestamp, last.id.as_str()));
        Ok(Listing { items, next_after })
    }

    /// Reply `request` to `contact`'s conversation with the number's token,
    /// addressed to the conversation's contact (the library's
    /// `Inbox::recipient`), through the library's inbox: refused before
    /// any request when the number's [`ReplyChecks`] say so (a free-form
    /// reply outside the 24-hour window, which counts the calls and
    /// standby messages that reopen it; a service message while another
    /// app owns the thread; templates exempt, and the Direct Send
    /// categories Meta sends as templates), else sent and recorded
    /// `accepted`. A Flow or a catalog the reply names by id is checked
    /// to be the number's WABA's once those checks pass (roadmap M5a),
    /// so a reply they refuse asks Meta nothing. Never sends twice (the
    /// client's `RetryPolicy` never replays a send that timed out).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `contact` or on the request's field at
    /// fault ([`messages::reply_message`]); `409 thread_owned_elsewhere` or
    /// `409 customer_service_window_closed` (also, on a number with a cut,
    /// for a window not opened since it), nothing sent; `404 not_found`
    /// for a named Flow or catalog of another WABA, nothing sent; Meta's refusal
    /// or failure, with its `details` (`504 timeout`, `may_have_been_sent`,
    /// …); `502 upstream` when Meta accepted without a message id.
    pub async fn reply(
        &self,
        contact: &str,
        request: ReplyMessage,
    ) -> Result<Accepted, ServiceError> {
        let key = conversation_key(self.number.phone_number_id(), contact)?;
        let message = messages::reply_message(request, self.inbox.recipient(&key))?;
        if !sent_as_template(&message)
            && let Some(cut) = self.window_cut()
        {
            let opened = self.window_opened_at(&key).await?;
            if self.cut_refuses_free_form(&key, cut, opened).await? {
                return Err(ServiceError::new(ErrorCode::CUSTOMER_SERVICE_WINDOW_CLOSED));
            }
        }
        // A Flow or a catalog named by id is the number's WABA's (roadmap
        // M5a, the design's M5.2), asked of Meta once the reply's local
        // checks pass: a reply they refuse asks Meta nothing.
        if messages::names_an_object(&message) {
            if !sent_as_template(&message)
                && let Err(error) = self.inbox.check_reply(&key).await
            {
                return Err(self.failed(&error).await);
            }
            messages::admit_named_object(self.authz, self.number, self.observer, &message).await?;
        }
        let message = messages::billed_to_binding(&message, self.number, self.names_account);
        match self.inbox.send(&key, message).await {
            Ok(response) => Accepted::from_response(response),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// When the customer last opened `key`'s window, as recorded: the
    /// latest of their last message ([`ConversationStore::last_inbound_at`])
    /// and the conversation's latest window event (a call, a standby
    /// message), as the library's `Inbox::window` reads them.
    async fn window_opened_at(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<OffsetDateTime>, ServiceError> {
        let last = self.store.last_inbound_at(key).await?;
        let event = self
            .store
            .window_events(key, None, 1)
            .await?
            .into_iter()
            .next()
            .map(|event| event.at);
        Ok(last.max(event))
    }

    /// The cut a free-form message must meet a window opened since: the
    /// number's ([`crate::authz::history_from`]), when it has one and its
    /// window is checked; `None` otherwise, and nothing is read.
    fn window_cut(&self) -> Option<OffsetDateTime> {
        self.number
            .history_from()
            .filter(|_| self.inbox.reply_checks().checks_window())
    }

    /// Whether, under the cut `cut` ([`Self::window_cut`]), a free-form
    /// message to `key` (what the library's window check covers: not a
    /// template, nor a Direct Send category Meta sends as one) meets a
    /// window not opened from the cut on, and is refused `409
    /// customer_service_window_closed` (the [module](self)'s "The reply's
    /// checks"). Opened from the cut on: `opened` (the latest of the
    /// customer's last message and the latest window event,
    /// [`Self::window_opened_at`]) dated from it, or, when the number
    /// trusts a handover, a handover to this app recorded since (the
    /// library's checks then decide).
    async fn cut_refuses_free_form(
        &self,
        key: &ConversationKey,
        cut: OffsetDateTime,
        opened: Option<OffsetDateTime>,
    ) -> Result<bool, ServiceError> {
        if opened.is_some_and(|at| visible(Some(cut), at)) {
            return Ok(false);
        }
        if self.inbox.reply_checks().trusts_handover()
            && self.store.thread_owner(key).await?.is_some_and(|owner| {
                owner.owner == ThreadOwner::ThisApp && visible(Some(cut), owner.since)
            })
        {
            return Ok(false);
        }
        Ok(true)
    }

    /// The 24-hour window of `contact`'s conversation as a free-form reply
    /// would meet it now ([`ReplyWindow`]): `open` when [`Self::reply`]
    /// would send one to Meta (the same checks, in the same order, nothing
    /// sent), and the window the customer opened from the binding's start
    /// on. Asks Meta nothing.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `contact`; the store failing, or the
    /// binding moving while it was read: `503 storage_unavailable`.
    pub async fn window(&self, contact: &str) -> Result<ReplyWindow, ServiceError> {
        let key = conversation_key(self.number.phone_number_id(), contact)?;
        let window = self.reply_window(&key).await?;
        self.number.still_held(self.authz).await?;
        Ok(window)
    }

    /// [`Self::window`] of `key`, before the binding is read again.
    async fn reply_window(&self, key: &ConversationKey) -> Result<ReplyWindow, ServiceError> {
        let from = self.number.history_from();
        let opened = self.window_opened_at(key).await?;
        // A window opened before the cut is the previous holder's: not
        // even its time is shown.
        let closes_at =
            CustomerServiceWindow::from_last_inbound(opened.filter(|at| visible(from, *at)))
                .closes_at();
        if let Some(cut) = self.window_cut()
            && self.cut_refuses_free_form(key, cut, opened).await?
        {
            return Ok(ReplyWindow {
                open: false,
                closes_at,
            });
        }
        let open = match self.inbox.check_reply(key).await {
            Ok(()) => true,
            Err(Error::Validation(refusal))
                if refusal.is_customer_service_window_closed()
                    || refusal.is_thread_owned_elsewhere() =>
            {
                false
            }
            Err(error) => return Err(error.into()),
        };
        Ok(ReplyWindow { open, closes_at })
    }

    /// Message `message_id` of the number, as the inbox recorded it (the
    /// store's lookup by id on the business number,
    /// [`ConversationStore::message`]: never another number's), if it is
    /// dated from the binding's start on. Asks Meta nothing.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `message_id` ([`message_id`]); `404
    /// not_found` for an id the number has no message of, or one dated
    /// before the cut (a previous holder's message looks like a missing
    /// one); `503 storage_unavailable` as for [`Self::window`].
    pub async fn message(&self, message_id: &str) -> Result<StoredMessage, ServiceError> {
        let id = self::message_id(message_id)?;
        let found = self
            .store
            .message(self.number.phone_number_id(), &id)
            .await?;
        self.number.still_held(self.authz).await?;
        let from = self.number.history_from();
        found
            .filter(|message| visible(from, message.timestamp))
            .ok_or_else(ServiceError::not_found)
    }

    /// The merchant read `contact`'s conversation: its unread count reset
    /// ([`ConversationStore::mark_read`]) and, with `notify_customer`, its
    /// latest inbound message marked read on Meta first, with the number's
    /// token (`messages/mark-message-as-read`: one request; the blue ticks
    /// the customer sees). No receipt goes without `notify_customer`, nor
    /// when the latest inbound message is dated before the cut, older than
    /// [`READ_RECEIPT_MAX_AGE`] or not among the conversation's latest
    /// [`UNREAD_SCAN`] messages; the count is reset all the same. A receipt
    /// Meta refuses resets nothing. Replaying it sets the same state again.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `contact`; Meta's refusal or failure of the
    /// receipt, with its `details`; `503 storage_unavailable` as for
    /// [`Self::window`] (checked before the receipt, and again after it,
    /// before the reset: a number that moved while Meta answered resets
    /// nothing).
    pub async fn mark_read(
        &self,
        contact: &str,
        notify_customer: bool,
    ) -> Result<(), ServiceError> {
        let key = conversation_key(self.number.phone_number_id(), contact)?;
        let latest = if notify_customer {
            self.latest_inbound(&key).await?
        } else {
            None
        };
        self.number.still_held(self.authz).await?;
        if let Some(id) = latest {
            let marked = self
                .number
                .client()
                .messages(self.number.phone_number_id().clone())
                .mark_read(&id)
                .await;
            if let Err(error) = marked {
                return Err(self.failed(&error).await);
            }
            // The receipt took a round trip to Meta: the number may have
            // moved meanwhile, and its conversation of `contact` be the
            // next holder's to reset, not this one's.
            self.number.still_held(self.authz).await?;
        }
        self.inbox.mark_read(&key).await?;
        Ok(())
    }

    /// The id of `key`'s latest inbound message a read receipt may name:
    /// the customer's newest among the conversation's latest
    /// [`UNREAD_SCAN`] messages (a revoke's tombstone skipped), if it is
    /// dated from the cut on and at most [`READ_RECEIPT_MAX_AGE`] old by
    /// the service's clock.
    async fn latest_inbound(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<MessageId>, ServiceError> {
        let from = self.number.history_from();
        let oldest = self.clock.now().saturating_sub(READ_RECEIPT_MAX_AGE);
        let mut read = 0_usize;
        let mut before = None;
        while read < UNREAD_SCAN {
            let want = MAX_PAGE_SIZE.min(UNREAD_SCAN - read);
            let page = self.inbox.history(key, before.take(), want).await?;
            read = read.saturating_add(page.len());
            for message in &page {
                if !visible(from, message.timestamp) || message.timestamp < oldest {
                    return Ok(None);
                }
                if message.direction == Direction::Inbound && message.kind != StoredMessage::REVOKED
                {
                    return Ok(Some(message.id.clone()));
                }
            }
            match page.last() {
                Some(last) if page.len() == want => {
                    before = Some((last.timestamp, last.id.clone()));
                }
                _ => break,
            }
        }
        Ok(None)
    }

    /// The error of a failed reply: the number's `failed` (a `190` marks
    /// its WABA's numbers), counted when Meta answered with a Graph error,
    /// with Meta's `details`.
    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self.number.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api.with_details(error)
    }
}

/// Whether `message` is exempt from the window: a template, or a Direct
/// Send category Meta sends as one (what the library's window check
/// exempts).
fn sent_as_template(message: &OutboundMessage) -> bool {
    matches!(message.content, MessageContent::Template(_))
        || matches!(
            message.category,
            Some(
                DirectSendCategory::Utility
                    | DirectSendCategory::Authentication
                    | DirectSendCategory::Other(_)
            )
        )
}

/// `page.limit`, 1 to [`MAX_PAGE_SIZE`].
fn limit(page: &PageRequest) -> Result<usize, ServiceError> {
    if (1..=MAX_PAGE_SIZE).contains(&page.limit) {
        Ok(page.limit)
    } else {
        Err(ServiceError::invalid("limit"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn pn() -> PhoneNumberId {
        PhoneNumberId::new("106540352242922").unwrap()
    }

    /// A position reads back as written, a time with nanoseconds and an id
    /// holding the separator included; anything else is `cursor`.
    #[test]
    fn a_position_reads_back_as_written() {
        let at = datetime!(2026-09-24 10:00:01.123_456_789 UTC);
        for id in ["US.13491208655302741918", "wamid.HBgL:x==", "16505551234"] {
            assert_eq!(
                read_position(&position(at, id)).unwrap(),
                (at, id.to_owned())
            );
        }
        let before_1970 = datetime!(1969-12-31 23:59:59 UTC);
        assert_eq!(
            read_position(&position(before_1970, "a")).unwrap().0,
            before_1970
        );
        for bad in [
            "",
            "123",
            "x:abc",
            "123:",
            ":abc",
            "99999999999999999999999999999:a",
        ] {
            assert_eq!(
                read_position(bad).unwrap_err().field(),
                Some("cursor"),
                "{bad:?}"
            );
        }
    }

    /// A contact is 1 to 256 bytes, not blank, without a control
    /// character; the refusal names `contact`.
    #[test]
    fn a_contact_is_checked_before_the_store_is_read() {
        for good in [
            "US.13491208655302741918",
            "16505551234",
            "120363025246125486@g.us",
        ] {
            assert_eq!(conversation_key(&pn(), good).unwrap().contact, good);
        }
        let long = "1".repeat(MAX_CONTACT_BYTES + 1);
        for bad in ["", " ", "a\nb", "a\u{0}b", long.as_str()] {
            assert_eq!(
                conversation_key(&pn(), bad).unwrap_err().field(),
                Some("contact"),
                "{bad:?}"
            );
        }
        assert!(conversation_key(&pn(), &"1".repeat(MAX_CONTACT_BYTES)).is_ok());
    }

    /// The cut hides what is dated before it, keeps its own instant, and
    /// hides nothing without one; a summary whose customer last wrote
    /// before it loses that time and its unread count.
    #[test]
    fn the_cut_hides_what_is_older_and_masks_a_summary() {
        let cut = datetime!(2026-09-24 10:00:00 UTC);
        assert!(visible(None, datetime!(2000-01-01 0:00 UTC)));
        assert!(visible(Some(cut), cut));
        assert!(!visible(Some(cut), cut - time::Duration::nanoseconds(1)));
        let summary = |inbound: Option<OffsetDateTime>| ConversationSummary {
            key: ConversationKey::new(pn(), "US.1"),
            last_message_at: cut + time::Duration::hours(1),
            last_inbound_at: inbound,
            last_text: Some("hi".to_owned()),
            unread: 3,
        };
        let before = masked(summary(Some(cut - time::Duration::hours(1))), Some(cut));
        assert_eq!((before.last_inbound_at, before.unread), (None, 0));
        assert_eq!(before.last_text.as_deref(), Some("hi"));
        let after = masked(summary(Some(cut)), Some(cut));
        assert_eq!((after.last_inbound_at, after.unread), (Some(cut), 3));
        let first = masked(summary(Some(cut - time::Duration::days(9))), None);
        assert_eq!(first.unread, 3);
    }

    /// The setting's grammar: entries, `all`, `none`, names joined by `+`;
    /// every refusal, never quoting the value.
    #[test]
    fn the_reply_checks_setting_reads_each_number() {
        let other = PhoneNumberId::new("106540352242923").unwrap();
        let parsed = ReplyChecksByNumber::parse(
            " 106540352242922 : window+trust_handover , 106540352242923:none,",
        )
        .unwrap();
        assert_eq!(parsed.of(&pn()), ReplyChecks::ALL.thread_owner(false));
        assert_eq!(parsed.of(&other), ReplyChecks::NONE);
        let unlisted = PhoneNumberId::new("1").unwrap();
        assert_eq!(parsed.of(&unlisted), ReplyChecks::ALL);
        assert_eq!(parsed.iter().count(), 2);
        assert!(ReplyChecksByNumber::parse("").unwrap().is_empty());
        assert_eq!(
            ReplyChecksByNumber::parse("1:all").unwrap().of(&unlisted),
            ReplyChecks::ALL
        );
        assert_eq!(
            ReplyChecksByNumber::parse("1:window+thread_owner")
                .unwrap()
                .of(&unlisted),
            ReplyChecks::ALL.trust_handover(false)
        );
        for bad in [
            "106540352242922",
            "+1650:all",
            "abc:all",
            "1:all,1:none",
            "1:windows",
            "1:window+window",
            "1:trust_handover",
            "1:trust_handover+thread_owner",
            "1:",
        ] {
            let refused = ReplyChecksByNumber::parse(bad).unwrap_err();
            assert!(!refused.to_string().contains(bad), "{bad:?}");
        }
    }
}
