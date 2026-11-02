//! The event outbox port (`wa_server_events` on Postgres,
//! docs/design/server.md, section 2.3): every webhook event the service
//! received, with the tenant it was routed to, or none (an operator-only
//! row, never shown to a tenant).
//!
//! **Each tenant has its own stream of sequences**, from 1, and the
//! operator-only rows one of their own: a tenant's cursor and its events'
//! sequences say nothing about other tenants' traffic.
//!
//! Every [`Outbox`] keeps two contracts, which a backend meets however
//! its database allows (the Postgres one with row locks, the memory one
//! with one lock over the records and the outbox):
//!
//! - **A stream's inserts commit in sequence order**, so a reader that
//!   sees sequence `n` of a tenant sees every event of that tenant before
//!   it, and `next_after` never skips one that commits later.
//! - **An insert checks the routing again** ([`RouteGuard`]): a row keeps
//!   its tenant only while, atomically with the insert, the binding the
//!   event was routed by still names that tenant and is one the event may
//!   reach: begun by the event's second (strictly before it, by a skew
//!   margin, when the WABA had a previous tenant), or, for an undated
//!   event after a move, once the window after it passed
//!   ([`RouteGuard::admits`]). Between the routing (a read) and the
//!   insert, a WABA can move to another tenant, or its tenant be deleted,
//!   created again under the same id and bound again; the row is then
//!   operator-only, never the new holder's.
//!
//! **What a row records as received** (roadmap M2e, `OPEN_QUESTIONS.md`
//! #43): a `message_received` event of an image, a video, an audio, a
//! document or a sticker, or an edit of a message into one, names the
//! media id its customer sent ([`NewEvent::received_media`]); a backend
//! keeps it beside the row, and [`Outbox::received_media`] finds the rows
//! by it. The core's media services read them: a media id recorded on the
//! caller's own number, routed to its tenant, is asked from Meta without
//! `phone_number_id`, and one recorded anywhere else is not asked at all
//! (`crate::media`).
//!
//! **What a row names of a customer** (roadmap M2f): the identifiers an
//! event names its customers by ([`NewEvent::contacts`], from
//! [`event_contacts`]), which a backend keeps beside the row at insert
//! (Postgres: `wa_server_events.contacts`, a GIN index), so that an
//! erasure finds a person's rows on a number without reading every row's
//! data ([`Outbox::erase_contacts`], called by [`crate::erasure`] only).
//! The column holds identifiers by design, as `data` does: it goes with
//! its row, by the outbox's retention, its tenant's deletion or an
//! erasure.

use std::collections::BTreeSet;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::core::ids::{MediaId, PhoneNumberId, WabaId};
use serde::Deserialize;
use time::OffsetDateTime;

use crate::model::TenantId;
use crate::store::StoreResult;

/// An event to append to the outbox.
#[derive(Clone, PartialEq, Eq)]
pub struct NewEvent {
    /// Its public id (`evt_…`).
    pub id: String,
    /// Its idempotency key (`crate::events::outbox_key`): a second insert
    /// with the same key is a no-op. The sink gives every event one; a row
    /// without one is never deduplicated.
    pub dedup_key: Option<String>,
    /// How long its row holds [`Self::dedup_key`]: `None`, for as long as
    /// it is stored (the library's dedup keys); for an event the library
    /// gives no dedup key (`error_reported`, `unparsed`), a window
    /// (`crate::events::KEYLESS_DEDUP_WINDOW`) on the webhook pipeline's
    /// clock.
    pub dedup_window: Option<DedupWindow>,
    /// The tenant it is routed to; `None` for an operator-only row. The
    /// row keeps it only through [`Self::route_guard`]: see
    /// [`Outbox::insert`].
    pub tenant: Option<TenantId>,
    /// The binding [`Self::tenant`] was routed by, which the insert checks
    /// again (`crate::events::outbox_row` sets it for every routed event).
    /// A row with a tenant and no guard is operator-only.
    pub route_guard: Option<RouteGuard>,
    /// The business phone number it is about, when it names one.
    pub phone_number_id: Option<PhoneNumberId>,
    /// Its WABA, when it names one.
    pub waba_id: Option<WabaId>,
    /// Its type: the library's `WebhookEvent::kind`.
    pub event_type: String,
    /// The event's JSON (the library's `WebhookEvent` serialization).
    pub data: String,
}

impl NewEvent {
    /// The media id this event records as received
    /// ([`received_media_id`]): what a backend keeps beside its row for
    /// [`Outbox::received_media`].
    pub fn received_media(&self) -> Option<MediaId> {
        received_media_id(&self.event_type, &self.data)
    }
}

/// The message types whose object carries a media id a customer sent
/// (`webhooks/reference/messages/image`, `…/video`, `…/audio`,
/// `…/document`, `…/sticker`: each `{type}.id`).
pub const RECEIVED_MEDIA_TYPES: [&str; 5] = ["image", "video", "audio", "document", "sticker"];

/// The media id an event of type `event_type`, its JSON `data` (the
/// library's `WebhookEvent` serialization), records as received
/// (roadmap M2e): for `message_received` only, its message's
/// `{type}.id` when its `type` is one of [`RECEIVED_MEDIA_TYPES`], or, for
/// an edit (`webhooks/reference/messages/edit`), the edited message's.
/// Only an id the media routes accept (`crate::request::graph_id`: digits,
/// no leading `0`). `None` for anything else, an event that does not read
/// as one included: a backend then records no media id for it, and the
/// media routes check that id with Meta's `phone_number_id`, as for any id
/// they never saw.
pub fn received_media_id(event_type: &str, data: &str) -> Option<MediaId> {
    #[derive(Deserialize)]
    struct Event {
        message: Option<Message>,
    }
    #[derive(Deserialize)]
    struct Message {
        #[serde(rename = "type")]
        kind: Option<String>,
        image: Option<Object>,
        video: Option<Object>,
        audio: Option<Object>,
        document: Option<Object>,
        sticker: Option<Object>,
        edit: Option<Edit>,
    }
    #[derive(Deserialize)]
    struct Edit {
        message: Option<Box<Message>>,
    }
    #[derive(Deserialize)]
    struct Object {
        id: Option<String>,
    }
    fn media_of(message: Message, nested: bool) -> Option<String> {
        let object = match message.kind.as_deref()? {
            "image" => message.image,
            "video" => message.video,
            "audio" => message.audio,
            "document" => message.document,
            "sticker" => message.sticker,
            // One level: an edit's message is never an edit itself.
            "edit" if !nested => return media_of(*message.edit?.message?, true),
            _ => None,
        };
        object?.id
    }
    if event_type != "message_received" {
        return None;
    }
    let event: Event = serde_json::from_str(data).ok()?;
    let id = media_of(event.message?, false)?;
    crate::request::graph_id("media_id", &id).ok()?;
    MediaId::new(id).ok()
}

/// Most rows [`Outbox::received_media`] answers for one media id: a media
/// id is one customer's file, in one message (and its edits).
pub const RECEIVED_MEDIA_ROWS: usize = 16;

/// A row of the outbox that recorded a media id as received
/// ([`Outbox::received_media`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedMedia {
    /// The row's tenant; `None` for an operator-only row (an event no
    /// binding gave a tenant: a number no tenant held, a move's window).
    pub tenant: Option<TenantId>,
    /// The business phone number the event names.
    pub phone_number_id: Option<PhoneNumberId>,
    /// When the service received it (the row's [`StoredEvent::created_at`]).
    pub received_at: OffsetDateTime,
}

impl NewEvent {
    /// The identifiers this event names its customers by
    /// ([`event_contacts`] of its data): what a backend keeps beside its
    /// row for [`Outbox::erase_contacts`].
    pub fn contacts(&self) -> Vec<String> {
        event_contacts(&self.data)
    }
}

/// The keys of an event's JSON whose values name a customer (Meta's
/// webhook references, as the library's `WebhookEvent` serializes them):
/// a phone number (`wa_id`, `from`, `to`, `input`, a thread control
/// event's `sender.phone_number`, an address book contact's
/// `phone_number`), a BSUID or a parent BSUID (`user_id`,
/// `parent_user_id`, and every key ending in `user_id`: `from_user_id`,
/// `to_parent_user_id`, `recipient_user_id`,
/// `recipient_participant_parent_user_id`, a `user_id_changed` event's
/// `user_id.previous` and `.current`), a status's recipient
/// (`recipient_id`, and any key ending in `_recipient_id` or
/// `participant_id`), and a group (`group_id`: the inbox keys a group's
/// conversation by it). A history chunk's thread names its customer by
/// the thread's `id` ([`event_contacts`]).
pub const CONTACT_KEYS: [&str; 7] = [
    "wa_id",
    "from",
    "to",
    "input",
    "phone_number",
    "recipient_id",
    "group_id",
];

/// Whether the values under `key` name a customer ([`CONTACT_KEYS`], and
/// the suffixes it lists).
fn names_a_contact(key: &str) -> bool {
    CONTACT_KEYS.contains(&key)
        || key.ends_with("user_id")
        || key.ends_with("_recipient_id")
        || key.ends_with("participant_id")
}

/// Every identifier an event's JSON `data` (the library's `WebhookEvent`
/// serialization) names a customer by: each string under a key of
/// [`CONTACT_KEYS`] (or its suffixes), at any depth (an object under one,
/// such as `user_id: {previous, current}`, gives every string in it), and
/// the `id` of each element of a `threads` array (a history chunk's
/// thread is its customer's phone number). Deduplicated, in byte order.
///
/// Left out: an empty value, the business's numbers (every value of a
/// `display_phone_number` in the event: an echo's `from`, a history
/// chunk's business messages; every row would name them), and
/// a value no erasure can name (longer than
/// [`crate::inbox::MAX_CONTACT_BYTES`], or holding a control character:
/// [`crate::inbox::conversation_key`] refuses both), which also keeps
/// every value storable (Postgres refuses `NUL` in `TEXT`, and an index
/// entry has a size limit). A body that is not JSON names nobody.
///
/// What it does not read, as the inbox's erasure does not
/// (`ConversationStore::erase_all`): a customer quoted in a message's
/// content (a shared contact card's formatted phone number, a text), and
/// a body the library could not parse (`unparsed`, whose raw body is one
/// string).
pub fn event_contacts(data: &str) -> Vec<String> {
    fn take(value: &serde_json::Value, found: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::String(s) => {
                found.insert(s.clone());
            }
            serde_json::Value::Array(items) => items.iter().for_each(|item| take(item, found)),
            serde_json::Value::Object(map) => map.values().for_each(|item| take(item, found)),
            _ => {}
        }
    }
    fn walk(value: &serde_json::Value, found: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, item) in map {
                    if names_a_contact(key) {
                        take(item, found);
                        continue;
                    }
                    // A history chunk's thread is its customer's: its `id`.
                    if key == "threads"
                        && let serde_json::Value::Array(threads) = item
                    {
                        for thread in threads {
                            if let Some(id) = thread.get("id").and_then(serde_json::Value::as_str) {
                                found.insert(id.to_owned());
                            }
                        }
                    }
                    walk(item, found);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|item| walk(item, found)),
            _ => {}
        }
    }
    let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
        return Vec::new();
    };
    let mut found = BTreeSet::new();
    walk(&event, &mut found);
    let business = business_numbers(&event);
    found
        .into_iter()
        .filter(|contact| {
            !contact.is_empty()
                && contact.len() <= crate::inbox::MAX_CONTACT_BYTES
                && !contact.chars().any(char::is_control)
                && !business.contains(contact)
        })
        .collect()
}

/// Every value of a `display_phone_number` key in `event`, at any depth:
/// the business's own numbers, never a customer's.
fn business_numbers(event: &serde_json::Value) -> BTreeSet<String> {
    fn walk(value: &serde_json::Value, found: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, item) in map {
                    if key == "display_phone_number"
                        && let Some(number) = item.as_str()
                    {
                        found.insert(number.to_owned());
                    }
                    walk(item, found);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|item| walk(item, found)),
            _ => {}
        }
    }
    let mut found = BTreeSet::new();
    walk(event, &mut found);
    found
}

impl std::fmt::Debug for NewEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `data` carries customers' messages and numbers.
        f.debug_struct("NewEvent")
            .field("id", &self.id)
            .field("tenant", &self.tenant)
            .field("route_guard", &self.route_guard)
            .field("event_type", &self.event_type)
            .field("data_bytes", &self.data.len())
            .finish_non_exhaustive()
    }
}

/// What an insert checks before it keeps a row's tenant: the binding the
/// event was routed by (`crate::events::owner`), typed, so that no backend
/// redoes the routing rules. [`Outbox::insert`] keeps the row's tenant
/// only if, atomically with the insert:
///
/// - [`Self::binding`] still names that tenant: for
///   [`GuardedBinding::Number`], the number is bound to the tenant under
///   that WABA; for [`GuardedBinding::Waba`], the WABA is bound to the
///   tenant;
/// - and that WABA's binding, as it is then (its `attached_at` and its
///   `previous_tenant_unbound_at`: [`crate::model::WabaBinding`]), is one
///   this event may reach ([`Self::admits`]):
///   - **a dated event** ([`Self::not_after`]): its binding began by the
///     event's second. Meta dates in whole seconds, so after a move an
///     event of the second the binding began in may be the previous
///     holder's, and a binding made after Meta dated the event is another
///     holder's, even under the same tenant id (the security review of
///     roadmap S2, L3). So, when the WABA had a previous tenant, the
///     binding must have begun in a second before the event's, by
///     [`AfterMove::skew_margin`] more (whole seconds); a first onboarding
///     (no previous tenant: roadmap S2b) keeps the events of its first
///     second (its `attached_at`'s second at most the event's), and no
///     margin applies to it.
///   - **an undated event** (an error, a history chunk, a group update
///     without a date): when the WABA had a previous tenant and
///     [`AfterMove::undated_moved_by`] is set (design D36's
///     `OperatorOnly`), only once that tenant left by then: within the
///     window after a move, the previous holder's undated events that
///     Meta redelivers (for up to 7 days) are operator-only. A first
///     onboarding's reach it, so its history sync does.
///
/// Otherwise the row is recorded operator-only (no tenant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteGuard {
    /// The binding the event was routed by.
    pub binding: GuardedBinding,
    /// When Meta dated the event (`crate::events::meta_time`); `None` for
    /// an undated event (an error, a history chunk, a group update without
    /// a date), which [`Self::after_move`]'s window decides after a move.
    pub not_after: Option<OffsetDateTime>,
    /// The rules for a binding whose WABA had a previous tenant.
    pub after_move: AfterMove,
}

/// What changes for an event of a WABA that moved between tenants
/// ([`RouteGuard`]): set by the routing from the service's settings
/// (`crate::events::MoveRules`), so that every backend applies the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AfterMove {
    /// A dated event reaches a binding with a previous tenant only when
    /// Meta dated it this much after the second the binding began in, and
    /// more (counted in whole seconds, a fraction rounded up: Meta dates in
    /// whole seconds). Zero: the second after the binding's.
    pub skew_margin: Duration,
    /// An undated event reaches a binding with a previous tenant only when
    /// that tenant left at or before this instant (the event's arrival,
    /// less the window: `UndatedAfterMove::OperatorOnly`); `None`: always
    /// (`UndatedAfterMove::CurrentHolder`, the rule before roadmap S2b).
    pub undated_moved_by: Option<OffsetDateTime>,
}

impl AfterMove {
    /// [`Self::skew_margin`] in whole seconds, a fraction rounded up.
    pub fn margin_seconds(&self) -> i64 {
        let whole = self
            .skew_margin
            .as_secs()
            .saturating_add(u64::from(self.skew_margin.subsec_nanos() > 0));
        i64::try_from(whole).unwrap_or(i64::MAX)
    }
}

/// For a dated event, the Unix second its binding must have begun before
/// (`attached_at < to_timestamp(s)`), depending on whether the WABA had a
/// previous tenant ([`RouteGuard::dated_bounds`]): a backend that compares
/// in its database's language binds these two and picks one by the
/// binding's `previous_tenant_unbound_at`, so that it redoes no rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatedBounds {
    /// No previous tenant (a first onboarding): the second after the
    /// event's, so a binding of the event's own second holds.
    pub first_onboarding: i64,
    /// A previous tenant: the event's second, less the skew margin, so a
    /// binding of the event's own second (or of the margin before it) does
    /// not hold.
    pub after_move: i64,
}

impl RouteGuard {
    /// The bounds of a dated event ([`DatedBounds`]); `None` for an undated
    /// one. [`DatedBounds::after_move`] is never before the Unix epoch: no
    /// binding began before it, so a margin reaching further back changes
    /// nothing, and a database may not hold an earlier instant (Postgres'
    /// `to_timestamp` refuses one millennia back, which would fail every
    /// dated insert).
    pub fn dated_bounds(&self) -> Option<DatedBounds> {
        self.not_after.map(|at| {
            let second = at.unix_timestamp();
            DatedBounds {
                first_onboarding: second.saturating_add(1),
                after_move: second
                    .saturating_sub(self.after_move.margin_seconds())
                    .max(crate::model::UNRECORDED_MOVE.unix_timestamp()),
            }
        })
    }

    /// Whether the guarded WABA's binding, which began at `attached_at`
    /// and whose WABA was last unbound from another tenant at
    /// `previous_tenant_unbound_at` (`None`: never), may receive this
    /// event: the rule of [`RouteGuard`], for a backend that compares in
    /// Rust, and the routing's (`crate::events::owner`). Its tenant is
    /// checked apart.
    pub fn admits(
        &self,
        attached_at: OffsetDateTime,
        previous_tenant_unbound_at: Option<OffsetDateTime>,
    ) -> bool {
        match (self.dated_bounds(), previous_tenant_unbound_at) {
            (Some(bounds), None) => attached_at.unix_timestamp() < bounds.first_onboarding,
            (Some(bounds), Some(_)) => attached_at.unix_timestamp() < bounds.after_move,
            (None, Some(left)) => self
                .after_move
                .undated_moved_by
                .is_none_or(|moved_by| left <= moved_by),
            (None, None) => true,
        }
    }
}

/// Which binding an event was routed by ([`RouteGuard::binding`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardedBinding {
    /// An event naming a business number: the number, bound under this
    /// WABA (the one it was bound under when the event was routed; the
    /// event's own WABA, when it names one).
    Number {
        /// The number.
        phone_number_id: PhoneNumberId,
        /// Its WABA.
        waba_id: WabaId,
    },
    /// An event naming only a WABA.
    Waba(WabaId),
}

impl GuardedBinding {
    /// The WABA whose binding's start [`RouteGuard::not_after`] is
    /// compared with.
    pub fn waba_id(&self) -> &WabaId {
        match self {
            Self::Number { waba_id, .. } | Self::Waba(waba_id) => waba_id,
        }
    }
}

/// A keyless event's dedup window, on the webhook pipeline's clock (both
/// ends: the stores never compare it with their own clock).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DedupWindow {
    /// When the event was received. A stored row holding the same key whose
    /// window ended at or before it is an earlier occurrence, not a
    /// redelivery: it gives the key up (keeping its row, sequence and id)
    /// and this event is recorded.
    pub now: OffsetDateTime,
    /// Until when this event's row holds the key: the same key before it
    /// is a redelivery, recorded nothing.
    pub until: OffsetDateTime,
}

/// An event as stored.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// Its position in its tenant's stream: increasing, never reused
    /// within one database's history (a point-in-time restore rolls the
    /// stream's `last_sequence` back, and later events draw its sequences
    /// again: docs/design/server.md, section 2.3).
    pub sequence: i64,
    /// Its public id.
    pub id: String,
    /// Its tenant; `None` for an operator-only row.
    pub tenant: Option<TenantId>,
    /// The business phone number, when it names one.
    pub phone_number_id: Option<PhoneNumberId>,
    /// Its WABA, when it names one.
    pub waba_id: Option<WabaId>,
    /// Its type.
    pub event_type: String,
    /// Its JSON, as written.
    pub data: String,
    /// When the service received it.
    pub created_at: OffsetDateTime,
}

impl std::fmt::Debug for StoredEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredEvent")
            .field("sequence", &self.sequence)
            .field("id", &self.id)
            .field("tenant", &self.tenant)
            .field("event_type", &self.event_type)
            .field("data_bytes", &self.data.len())
            .finish_non_exhaustive()
    }
}

/// Which of a tenant's events a poll asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventQuery {
    /// The tenant. Operator-only rows (no tenant) never match.
    pub tenant: TenantId,
    /// Events after this sequence of the tenant's stream; `None` starts at
    /// the oldest retained.
    pub after: Option<i64>,
    /// Only these types; `None` for every type.
    pub types: Option<Vec<String>>,
    /// Only this phone number's events.
    pub phone_number_id: Option<PhoneNumberId>,
    /// At most this many events.
    pub limit: usize,
    /// At most this many bytes of `data`, except that the first event
    /// always comes: the store stops before the event that would pass it,
    /// and reads no event's data past it.
    pub max_bytes: usize,
}

/// A poll's rows and the tenant's stream's bounds, read at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPage {
    /// The matching events after the cursor, in sequence order: at most
    /// `limit`, within `max_bytes` (the first always).
    pub events: Vec<StoredEvent>,
    /// Whether more matching events follow the last one.
    pub more: bool,
    /// Every event of the stream up to this sequence was purged, by
    /// retention or with a deleted tenant (0 before the first purge): a
    /// cursor below it may have missed some.
    pub purged_through: i64,
    /// The stream's highest sequence, stored or purged: every event up to
    /// it was visible to this read.
    pub high_water: i64,
}

/// The event outbox.
#[async_trait]
pub trait Outbox: Send + Sync + 'static {
    /// Append `event` to its tenant's stream (or the operator-only one),
    /// committed in the stream's sequence order; its sequence.
    ///
    /// **The routing, checked again.** A row keeps [`NewEvent::tenant`]
    /// only if [`NewEvent::route_guard`] holds atomically with the insert
    /// (see [`RouteGuard`] for what holds means): its guarded binding
    /// cannot move between the check and the commit, and deleting the
    /// binding (or its tenant) waits for the insert or comes before the
    /// check. Otherwise, a tenant without a guard included, the row is
    /// recorded operator-only (no tenant, in the operator-only stream).
    /// This is a requirement of every backend, not a Postgres property. A
    /// database that sees a conflict between two writes but not between a
    /// read and a write (MongoDB's snapshot transactions) meets it by
    /// writing the guarded binding's record in the insert's transaction, so
    /// that an unbinding conflicts with the insert.
    ///
    /// **Once.** `None` when an event with the same dedup key is stored
    /// (nothing is written, no sequence drawn), unless `event` has a
    /// [`NewEvent::dedup_window`] and the stored row's window ended at or
    /// before its `now`: that row gives the key up, and `event` is
    /// recorded.
    ///
    /// # Errors
    ///
    /// The library's `StorageError`; `StorageError::Busy` when the insert
    /// gave up waiting for another writer (another insert of the same
    /// stream, a binding changing) and wrote nothing: the webhook pipeline
    /// answers Meta `503` for it, and Meta retries.
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>>;

    /// A tenant's events after the query's cursor, and its stream's bounds,
    /// from one consistent read.
    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage>;

    /// The event of id `id` in `tenant`'s stream, if it is stored there
    /// (`GET /v1/events/{id}`, roadmap M2b): never another tenant's event
    /// nor an operator-only row, whatever its id, and none once purged.
    ///
    /// The default pages the tenant's stream from its oldest retained
    /// event until it finds the id: correct, and as slow as the stream is
    /// long. The in-tree backends answer it with one lookup by id (an
    /// index), and a backend of yours should too.
    ///
    /// # Errors
    ///
    /// The library's `StorageError`.
    async fn get(&self, tenant: &TenantId, id: &str) -> StoreResult<Option<StoredEvent>> {
        let mut after = None;
        loop {
            let page = self
                .page(&EventQuery {
                    tenant: tenant.clone(),
                    after,
                    types: None,
                    phone_number_id: None,
                    limit: 100,
                    max_bytes: crate::events::MAX_PAGE_DATA_BYTES,
                })
                .await?;
            let last = page.events.last().map(|event| event.sequence);
            if let Some(found) = page
                .events
                .into_iter()
                .find(|event| event.id == id && event.tenant.as_ref() == Some(tenant))
            {
                return Ok(Some(found));
            }
            match last {
                Some(last) if page.more => after = Some(last),
                _ => return Ok(None),
            }
        }
    }

    /// Delete the events received more than `older_than` ago, as a prefix
    /// of each stream, and record how far each went; how many went. It
    /// takes no lock of its own (housekeeping runs it under a
    /// `LeaderLock` turn) and is safe to run on two replicas at once: a
    /// stream's `purged_through` only moves forward, and a row two purges
    /// both chose is deleted once.
    async fn purge(&self, older_than: Duration) -> StoreResult<u64>;

    /// The rows that recorded `media_id` as received (roadmap M2e): each
    /// row whose [`NewEvent::received_media`] was `media_id` when it was
    /// inserted, operator-only rows included, newest first, at most
    /// [`RECEIVED_MEDIA_ROWS`]; none once purged, or gone with their
    /// tenant. A backend keeps the id beside the row at insert (Postgres:
    /// `wa_server_events.media_id`, indexed), so this is one lookup, never
    /// a scan of the rows' data.
    ///
    /// # Errors
    ///
    /// The library's `StorageError`.
    async fn received_media(&self, media_id: &MediaId) -> StoreResult<Vec<ReceivedMedia>>;

    /// Erase a customer's rows on one business number (roadmap M2f): delete
    /// every row of `tenant`'s stream and of the operator-only stream
    /// whose `phone_number_id` is `phone_number_id` and whose
    /// [`NewEvent::contacts`], kept beside it at insert, name one of
    /// `contacts`; how many went. Whole rows: one that also names others
    /// (a history chunk, a group update) goes with their part. Never a row
    /// of another tenant's stream, nor of another number; an empty
    /// `contacts`, or ids no row names, delete nothing; a row inserted
    /// before its backend kept contacts names nobody here. A backend keeps
    /// the contacts indexed (Postgres: `wa_server_events.contacts`, a GIN
    /// index), so this never reads the rows' data.
    ///
    /// A stream's sequences stay as they are (a poll skips the gap, and
    /// `purged_through` does not move); a deleted row's dedup key is free
    /// again, as after a purge: the webhook pipeline's own dedup markers
    /// (seven days and an hour) keep Meta's redeliveries out.
    ///
    /// It trusts its arguments: the core's erasure service
    /// ([`crate::erasure::Erasures`]) calls it for an owned number only.
    ///
    /// # Errors
    ///
    /// The library's `StorageError`.
    async fn erase_contacts(
        &self,
        tenant: &TenantId,
        phone_number_id: &PhoneNumberId,
        contacts: &[String],
    ) -> StoreResult<u64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every event of Meta's documented webhooks (the library's fixtures,
    /// `webhooks/reference/*`, as the service stores their events:
    /// `crate::events::event_data`), with its data.
    fn fixture_events() -> Vec<(String, String, String)> {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../meta-whatsapp-webhooks/tests/fixtures");
        let mut events = Vec::new();
        let mut dirs = vec![fixtures.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let body = std::fs::read(&path).unwrap();
                let Ok(payload) = meta_whatsapp_rs::webhooks::WebhookPayload::from_slice(&body)
                else {
                    continue;
                };
                let name = path.strip_prefix(&fixtures).unwrap();
                for event in payload.into_events() {
                    let data = crate::events::event_data(&event).unwrap();
                    events.push((
                        name.to_string_lossy().into_owned(),
                        event.kind().to_owned(),
                        data,
                    ));
                }
            }
        }
        events.sort();
        assert!(events.len() > 100, "the fixtures were not read");
        events
    }

    /// Roadmap M2f: no row names a customer that its `contacts` miss.
    /// Every identifier any of Meta's documented events names a customer
    /// by ([`event_contacts`] over every fixture) is, in every event whose
    /// data holds it as a JSON string, one of that event's contacts, but
    /// for the business's own display numbers (never a contact). Decisive: each key of
    /// [`CONTACT_KEYS`] and each suffix (the events using it escape), and
    /// the threads' `id`.
    #[test]
    fn every_identifier_an_event_holds_is_one_of_its_contacts() {
        let events = fixture_events();
        let mut universe = BTreeSet::new();
        for (_, _, data) in &events {
            universe.extend(event_contacts(data));
        }
        // Meta's examples' customers, by each kind of identifier.
        for id in [
            "16505551234",
            "US.13491208655302741918",
            "US.ENT.11815799212886844830",
        ] {
            assert!(universe.contains(id), "{id}: {universe:?}");
        }
        let mut missed = Vec::new();
        for (file, kind, data) in &events {
            let contacts = event_contacts(data);
            let business = serde_json::from_str::<serde_json::Value>(data)
                .map(|event| business_numbers(&event))
                .unwrap_or_default();
            for id in &universe {
                let quoted = serde_json::to_string(id).unwrap();
                if data.contains(&quoted) && !contacts.contains(id) && !business.contains(id) {
                    missed.push(format!("{file} ({kind}): {id}"));
                }
            }
        }
        assert_eq!(missed, Vec::<String>::new());
    }

    /// The contacts of a few of Meta's examples, exactly: a text from a
    /// phone number; a text from a BSUID with its parent; a BSUID change,
    /// both BSUIDs and both parents; a history chunk, by its threads and
    /// their messages' senders, the business's number left out; a status,
    /// by its recipient. Decisive: the business's number filter, the
    /// thread `id` rule, the subtree of a contact key.
    #[test]
    fn metas_examples_name_their_customers() {
        let events = fixture_events();
        let contacts_of = |file: &str| -> Vec<Vec<String>> {
            events
                .iter()
                .filter(|(f, _, _)| f == file)
                .map(|(_, _, data)| event_contacts(data))
                .collect()
        };
        assert_eq!(contacts_of("messages/text.json"), [["16505551234"]]);
        assert_eq!(
            contacts_of("bsuid/text_username_no_wa_id.json"),
            [["US.13491208655302741918", "US.ENT.11815799212886844830"]]
        );
        assert_eq!(
            contacts_of("fields/user_id_update.json"),
            [[
                "16505551234",
                "US.13491208655302741918",
                "US.29847561203948576612",
                "US.ENT.11815799212886844830",
                "US.ENT.22926810323997955941",
            ]]
        );
        assert_eq!(
            contacts_of("fields/history_threads.json"),
            [["12125557890", "16505551234"]]
        );
        assert_eq!(contacts_of("messages/status_sent.json"), [["16505551234"]]);
    }

    /// What names nobody: an empty value, one longer than an erasure may
    /// name, one with a control character (Postgres refuses `NUL` in
    /// `TEXT`), the event's display number, and a body that is not JSON.
    /// A contact key's object gives every string in it; numbers and
    /// booleans are no identifiers. Decisive: each filter of
    /// [`event_contacts`].
    #[test]
    fn only_storable_nameable_identifiers_are_contacts() {
        let long = "1".repeat(crate::inbox::MAX_CONTACT_BYTES + 1);
        let longest = "1".repeat(crate::inbox::MAX_CONTACT_BYTES);
        let data = serde_json::json!({
            "display_phone_number": "15550783881",
            "message": {
                "from": "15550783881",
                "to": "",
                "wa_id": long,
                "input": longest,
                "user_id": {"previous": "US.1", "current": "US.2", "n": 7, "b": true},
                "recipient_id": "a\u{0}b",
                "group_id": "120363044662341228",
                "text": {"body": "16505550000"}
            }
        })
        .to_string();
        let mut expected = vec![
            longest,
            "120363044662341228".to_owned(),
            "US.1".to_owned(),
            "US.2".to_owned(),
        ];
        expected.sort();
        assert_eq!(event_contacts(&data), expected);
        assert!(event_contacts("not json").is_empty());
        assert!(event_contacts("{}").is_empty());
    }

    /// The media id each of Meta's documented webhooks records as received
    /// (the library's fixtures, `webhooks/reference/*`, as the service
    /// stores their events: `crate::events::event_data`): the five media
    /// types' `{type}.id` and an edit's, nothing else. Decisive: the type
    /// match in `received_media_id`, and the edit's one level.
    #[test]
    fn metas_media_messages_record_their_media_id_and_nothing_else_does() {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../meta-whatsapp-webhooks/tests/fixtures");
        let mut found = Vec::new();
        let mut events = 0;
        let mut dirs = vec![fixtures.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let body = std::fs::read(&path).unwrap();
                let Ok(payload) = meta_whatsapp_rs::webhooks::WebhookPayload::from_slice(&body)
                else {
                    continue;
                };
                for event in payload.into_events() {
                    events += 1;
                    let data = crate::events::event_data(&event).unwrap();
                    if let Some(id) = received_media_id(event.kind(), &data) {
                        let name = path.strip_prefix(&fixtures).unwrap();
                        found.push((name.to_string_lossy().into_owned(), id.into_inner()));
                    }
                }
            }
        }
        found.sort();
        assert!(events > 100, "the fixtures were not read: {events} events");
        let expected = [
            ("messages/audio.json", "1908647269898587"),
            ("messages/document.json", "622684793477189"),
            ("messages/edit.json", "1234567890"),
            ("messages/image.json", "1003383421387256"),
            ("messages/sticker.json", "1908647269898587"),
            ("messages/video.json", "731675419373506"),
            // `business-scoped-user-ids`' edit example, and a voicemail
            // (`calling/call-settings`): an audio message.
            (
                "pages/business-scoped-user-ids__edit_messages_webhooks.json",
                "1234567890",
            ),
            (
                "pages/calling.call-settings__voicemail_webhook_payload.json",
                "1002764438271669",
            ),
        ]
        .map(|(f, id)| (f.to_owned(), id.to_owned()));
        assert_eq!(found, expected);
    }

    /// What is not a received media id: another event type, a type whose
    /// object is missing (the object of another type present instead), an
    /// id the media routes refuse (a leading `0`, not digits), an edit of
    /// an edit, a body that is not an event.
    #[test]
    fn only_a_media_message_of_its_own_type_records_a_routable_id() {
        let message =
            |message: &str| format!("{{\"event\":\"message_received\",\"message\":{message}}}");
        let image = message(r#"{"type":"image","image":{"id":"1003383421387256"}}"#);
        assert_eq!(
            received_media_id("message_received", &image).map(MediaId::into_inner),
            Some("1003383421387256".to_owned())
        );
        // Each media type reads its own object, and only its own: the
        // others' objects present, its own missing, record nothing.
        for kind in RECEIVED_MEDIA_TYPES {
            let own = message(&format!(
                r#"{{"type":"{kind}","{kind}":{{"id":"1003383421387256"}}}}"#
            ));
            assert_eq!(
                received_media_id("message_received", &own).map(MediaId::into_inner),
                Some("1003383421387256".to_owned()),
                "{kind}"
            );
            let others: Vec<String> = RECEIVED_MEDIA_TYPES
                .iter()
                .filter(|other| **other != kind)
                .map(|other| format!(r#""{other}":{{"id":"1003383421387256"}}"#))
                .collect();
            let theirs = message(&format!(r#"{{"type":"{kind}",{}}}"#, others.join(",")));
            assert_eq!(
                received_media_id("message_received", &theirs),
                None,
                "{kind}: another type's object"
            );
        }
        for (event_type, data, why) in [
            ("status_updated", image.clone(), "another event type"),
            (
                "message_received",
                message(r#"{"type":"image","video":{"id":"1003383421387256"}}"#),
                "the object of another type",
            ),
            (
                "message_received",
                message(
                    r#"{"type":"text","text":{"body":"hi"},"image":{"id":"1003383421387256"}}"#,
                ),
                "a text message",
            ),
            (
                "message_received",
                message(r#"{"type":"image","image":{"id":"01003383421387256"}}"#),
                "a leading 0",
            ),
            (
                "message_received",
                message(r#"{"type":"image","image":{"id":"../me"}}"#),
                "not digits",
            ),
            (
                "message_received",
                message(r#"{"type":"image","image":{"id":1003383421387256}}"#),
                "a number, not Meta's string",
            ),
            (
                "message_received",
                message(
                    r#"{"type":"edit","edit":{"message":{"type":"edit","edit":{"message":{"type":"image","image":{"id":"1003383421387256"}}}}}}"#,
                ),
                "an edit of an edit",
            ),
            ("message_received", "not json".to_owned(), "not JSON"),
            ("message_received", "{}".to_owned(), "no message"),
        ] {
            assert_eq!(received_media_id(event_type, &data), None, "{why}");
        }
    }

    fn guard(not_after: Option<OffsetDateTime>, after_move: AfterMove) -> RouteGuard {
        RouteGuard {
            binding: GuardedBinding::Waba(WabaId::new("102290129340398").unwrap()),
            not_after,
            after_move,
        }
    }

    /// After a move (the WABA had a previous tenant), a binding that began
    /// in a second before the event's is the event's; one that began in the
    /// event's own second (its first instant included) or later is not:
    /// Meta's dates are whole seconds, so an event of that second may be
    /// the previous holder's (the security review of roadmap S2, L3). A
    /// first onboarding's binding keeps the events of its own second
    /// (roadmap S2b), never those dated before it. Decisive: the strict
    /// comparison of seconds after a move, the relaxed one without.
    #[test]
    fn a_binding_began_in_a_second_before_the_events_after_a_move_only() {
        let t = time::macros::datetime!(2026-09-26 12:00:00.5 UTC);
        let at = |secs: f64| t + time::Duration::seconds_f64(secs);
        let moved = Some(t - time::Duration::days(30));
        let g = guard(Some(t), AfterMove::default());
        for previous in [moved, None] {
            assert!(g.admits(at(-10.0), previous), "{previous:?}");
            assert!(g.admits(at(-0.6), previous), "the second before's end");
            assert!(!g.admits(at(0.5), previous), "the next second");
            assert!(!g.admits(at(3600.0), previous), "an hour later");
        }
        assert!(
            !g.admits(at(-0.5), moved),
            "after a move: the event's second's first instant"
        );
        assert!(!g.admits(at(0.4), moved), "after a move: the same second");
        assert!(
            g.admits(at(-0.5), None),
            "a first onboarding: the event's second's first instant"
        );
        assert!(
            g.admits(at(0.4), None),
            "a first onboarding: the same second"
        );
    }

    /// The skew margin widens the strict rule after a move, in whole
    /// seconds (a fraction rounded up), and never applies to a first
    /// onboarding. Decisive: the margin in `dated_bounds`, and its
    /// application to the binding with a previous tenant only.
    #[test]
    fn the_skew_margin_applies_after_a_move_only() {
        let t = time::macros::datetime!(2026-09-26 12:00:00 UTC);
        let margin = |secs: f64| AfterMove {
            skew_margin: std::time::Duration::from_secs_f64(secs),
            undated_moved_by: None,
        };
        let moved = Some(t - time::Duration::days(30));
        let began = |secs: i64| t - time::Duration::seconds(secs);
        let g = guard(Some(t), margin(5.0));
        assert!(!g.admits(began(5), moved), "5 s before: within the margin");
        assert!(g.admits(began(6), moved), "6 s before: past it");
        assert!(g.admits(began(0), None), "a first onboarding: no margin");
        let g = guard(Some(t), margin(4.2));
        assert_eq!(g.after_move.margin_seconds(), 5, "rounded up");
        assert!(!g.admits(began(5), moved));
        assert!(g.admits(began(6), moved));
        assert_eq!(margin(0.0).margin_seconds(), 0);
        // A margin reaching before the epoch: the bound stays at it.
        let g = guard(
            Some(t),
            AfterMove {
                skew_margin: std::time::Duration::MAX,
                undated_moved_by: None,
            },
        );
        assert_eq!(g.dated_bounds().map(|b| b.after_move), Some(0));
        assert!(!g.admits(began(3600), moved));
        assert!(g.admits(began(0), None), "a first onboarding: no margin");
    }

    /// An undated event reaches a binding with a previous tenant only once
    /// that tenant left by `undated_moved_by` (`OperatorOnly`), always
    /// with no such instant (`CurrentHolder`), and a first onboarding's
    /// always. Its binding's start is not compared. Decisive: the window
    /// check in `admits`, and the first-onboarding exemption.
    #[test]
    fn an_undated_event_waits_out_the_window_after_a_move_only() {
        let t = time::macros::datetime!(2026-09-26 12:00:00 UTC);
        let left = t - time::Duration::days(3);
        let within = guard(
            None,
            AfterMove {
                undated_moved_by: Some(left - time::Duration::MICROSECOND),
                ..AfterMove::default()
            },
        );
        let at_its_end = guard(
            None,
            AfterMove {
                undated_moved_by: Some(left),
                ..AfterMove::default()
            },
        );
        let current_holder = guard(None, AfterMove::default());
        assert!(!within.admits(t, Some(left)), "within the window");
        assert!(at_its_end.admits(t, Some(left)), "the window's end");
        assert!(current_holder.admits(t, Some(left)), "CurrentHolder");
        assert!(within.admits(t, None), "a first onboarding");
        assert!(
            within.admits(t + time::Duration::days(400), None),
            "its binding's start is not compared"
        );
    }
}
