//! Meta's webhook events, as the service records and serves them
//! (docs/design/server.md, sections 2.3 and 4.2): where an event goes
//! ([`route`]), the outbox row it becomes ([`outbox_row`]: its key and its
//! id), and polling the outbox ([`poll`]). Plain functions over the ports:
//! the webhook pipeline ([`crate::inbound`]) runs them for each event, and
//! the API adapters only translate.
//!
//! **Routing is an allow-list.** An event naming a business phone number
//! (or, untyped, whose raw `metadata` names one) belongs to the tenant that
//! number is bound to, and only when the event's WABA, if it names one, is
//! the number's WABA in the bindings; an event naming only a WABA belongs
//! to the WABA's tenant. And, for an event Meta dated ([`meta_time`]), only
//! when that WABA's binding began by the event's second, and, when the
//! WABA had a previous tenant, in a second before it, by the skew margin
//! more ([`MoveRules`]): a WABA moved from one tenant to another does not
//! bring the first one's dated events, which Meta retries for up to 7
//! days, to the second, and a first onboarding keeps the events of its
//! first second. An **undated** event (an error, a history chunk, a group
//! update without a date) has no date to compare: after a move from
//! another tenant it is operator-only until the window after the move has
//! passed (`undated_after_move`, design D36: [`UndatedAfterMove`]), since
//! the previous holder's undated events that Meta redelivers would
//! otherwise reach the new holder; for a WABA no other tenant held, and
//! with [`UndatedAfterMove::CurrentHolder`], it goes to whoever holds the
//! binding when it arrives. Roadmap S2b.
//! The outbox row carries that tenant only for the types in
//! [`TENANT_EVENT_TYPES`]; `unknown`, `unparsed`, `partner_solution_updated`
//! ([`OPERATOR_EVENT_TYPES`]), any type a later library adds, and every
//! event of a number or WABA no tenant holds (or held then) are
//! operator-only rows (no tenant: never polled), and the inbox records only
//! an owned event. The service's own events about its numbers are
//! [`crate::number_events`].
//!
//! **An event is recorded once.** The outbox inserts on the event's key
//! ([`outbox_key`]) at most once, so Meta's redeliveries are safe. The
//! events the library gives no dedup key (`error_reported`, `unparsed`)
//! are keyed by the signed body they came in and their position in it
//! ([`EventKey::Delivery`]), and deduplicated within
//! [`KEYLESS_DEDUP_WINDOW`] only: an identical body after it is recorded
//! again, as a new occurrence with an id of its own.

use std::time::Duration;

use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::core::secret::AppSecret;
use meta_whatsapp_rs::webhooks::WebhookEvent;
use meta_whatsapp_rs::webhooks::fields::StandbyItem;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::error::{ErrorCode, ServiceError};
use crate::model::TenantId;
use crate::outbox::{
    AfterMove, DedupWindow, EventQuery, GuardedBinding, NewEvent, Outbox, RouteGuard, StoredEvent,
};
use crate::store::{RecordStore, StoreResult};

/// How long an event the library gives no dedup key (`error_reported`,
/// `unparsed`) is deduplicated for: the same body and position within it
/// is Meta's redelivery; after it, a new occurrence, recorded again under
/// a new id. Meta documents its first retry as immediate, then fewer over
/// 7 days (`webhooks/create-webhook-endpoint`); that a redelivery is the
/// same bytes is an assumption Meta does not document. An outage longer
/// than this (the database answering `500` while Meta retries) records
/// the batch's keyless events twice, as new ids. The library itself never
/// deduplicates these events, since the same error legitimately recurs
/// and carries no date. Decision D23 of docs/design/server.md (section
/// 10): a coordinator's decision of 2026-09-25, reversible, for the owner
/// to confirm.
pub const KEYLESS_DEDUP_WINDOW: Duration = Duration::from_hours(1);

/// Default `WA_SERVER_OUTBOX_RETENTION`: 7 days, the design's proposal.
/// Retention is decision D10 of the design, still open: this default
/// stands until the owner decides.
pub const DEFAULT_OUTBOX_RETENTION: Duration = Duration::from_hours(7 * 24);

/// The default window of [`UndatedAfterMove::OperatorOnly`]: 7 days and an
/// hour, Meta's retry window (`webhooks/create-webhook-endpoint`) plus the
/// dedup lease's margin, the library's `DEFAULT_DEDUP_TTL` (design D36).
pub const DEFAULT_UNDATED_AFTER_MOVE_WINDOW: Duration = Duration::from_hours(7 * 24 + 1);

/// The default [`MoveRules::skew_margin`]: none beyond the strict second
/// (roadmap S2, L3), which already makes an event of a moved WABA's first
/// second operator-only.
pub const DEFAULT_MOVE_SKEW_MARGIN: Duration = Duration::ZERO;

/// Who receives an undated tenant-visible event (an error, a history
/// chunk, a group update without a date) of a WABA that moved to its
/// holder from another tenant: the setting `undated_after_move` (design
/// D36; the service's `WA_SERVER_UNDATED_AFTER_MOVE`). Such an event
/// carries no date to compare with the binding, and Meta redelivers a
/// failed delivery for up to 7 days, so within that time after a move it
/// may be the previous holder's. A WABA no other tenant held (a first
/// onboarding) is never concerned: its history sync reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndatedAfterMove {
    /// Operator-only until `window` has passed since the WABA was last
    /// unbound from another tenant (the default, with
    /// [`DEFAULT_UNDATED_AFTER_MOVE_WINDOW`]): fail closed. A moved WABA's
    /// new holder does not get its own undated events in that window
    /// either (its own history sync among them, when it onboarded within
    /// it); the operator sees them in the operator-only stream.
    OperatorOnly {
        /// How long after the move.
        window: Duration,
    },
    /// The holder when it arrives: the rule before roadmap S2b, under
    /// which the previous holder's redelivered undated events reach the
    /// new holder.
    CurrentHolder,
}

impl Default for UndatedAfterMove {
    fn default() -> Self {
        Self::OperatorOnly {
            window: DEFAULT_UNDATED_AFTER_MOVE_WINDOW,
        }
    }
}

/// The rules for the events of a WABA that moved between tenants (roadmap
/// S2b, design D36): settings of the webhook pipeline
/// (`crate::inbound::Inbound::with_move_rules`), applied by the routing
/// ([`owner`]) and handed to the outbox's check ([`RouteGuard::after_move`]).
/// Neither applies to a WABA no other tenant held. Swap them through the
/// service's settings (`WA_SERVER_UNDATED_AFTER_MOVE`,
/// `WA_SERVER_UNDATED_AFTER_MOVE_WINDOW`, `WA_SERVER_MOVE_SKEW_MARGIN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MoveRules {
    /// Undated events after a move ([`UndatedAfterMove`]).
    pub undated_after_move: UndatedAfterMove,
    /// How far past the second its binding began in an event of a moved
    /// WABA must be dated to reach the holder, for a clock of Meta's ahead
    /// of the store's (whole seconds, a fraction rounded up;
    /// [`DEFAULT_MOVE_SKEW_MARGIN`] by default).
    pub skew_margin: Duration,
}

impl MoveRules {
    /// The outbox's part of these rules for an event received at `now`
    /// (on the webhook pipeline's clock): the margin, and the instant a
    /// previous tenant must have left by for an undated event (`now` less
    /// the window, never before the Unix epoch: a database may not hold an
    /// earlier instant, and no move it recorded is older).
    pub fn after_move(&self, now: OffsetDateTime) -> AfterMove {
        AfterMove {
            skew_margin: self.skew_margin,
            undated_moved_by: match self.undated_after_move {
                UndatedAfterMove::OperatorOnly { window } => {
                    let window = time::Duration::try_from(window).unwrap_or(time::Duration::MAX);
                    Some(
                        now.saturating_sub(window)
                            .max(crate::model::UNRECORDED_MOVE),
                    )
                }
                UndatedAfterMove::CurrentHolder => None,
            },
        }
    }
}

/// When an event is routed, and by which rules ([`route`], [`owner`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Routing {
    /// When it was received, on the webhook pipeline's clock: the window
    /// after a move ([`UndatedAfterMove::OperatorOnly`]) is measured from it.
    pub now: OffsetDateTime,
    /// An event Meta dated before this is a replay (security review L3):
    /// the dedup lease's memory before [`Self::now`].
    pub not_before: OffsetDateTime,
    /// The rules after a move.
    pub moves: MoveRules,
}

/// How often a replica runs housekeeping (the outbox purge, the library's
/// expired key/value rows on Postgres, and expired idempotency records).
pub const HOUSEKEEPING_INTERVAL: Duration = Duration::from_secs(600);

/// The most `data` a page of `GET /v1/events` carries: the store stops
/// before the event that would pass it (the first event always comes) and
/// reads no data past it, and `next_after` continues from there. A history
/// sync event alone can approach 3 MiB.
pub const MAX_PAGE_DATA_BYTES: usize = 8 * 1024 * 1024;

/// The event types a tenant receives: the library's `WebhookEvent::kind`s
/// the service has reviewed and pinned (the service's
/// `tests/event_data.rs`), and the service's own number events
/// ([`crate::number_events::NUMBER_EVENT_TYPES`], the three before the
/// last) and the token-expiry event ([`crate::token_expiry::TOKEN_EXPIRING`],
/// the last), which no webhook carries. Every other type, today's `unknown`, `unparsed` and
/// `partner_solution_updated`, and any a later library adds, is
/// operator-only ([`OPERATOR_EVENT_TYPES`] lists today's). Roadmap M2d
/// (design decision D25) added Conversation Routing's standby copies and
/// handovers (`standby_observed`, `thread_control_changed`) and the
/// Marketing Messages API's clicks (`user_action_reported`), operator-only
/// before it, and the number events.
pub const TENANT_EVENT_TYPES: [&str; 35] = [
    "message_received",
    "status_updated",
    "error_reported",
    "message_echoed",
    "history_synced",
    "app_state_synced",
    "call_updated",
    "call_status_updated",
    "user_preference_changed",
    "user_id_changed",
    "automatic_event_detected",
    "group_updated",
    "flow_updated",
    "account_alert",
    "account_review_updated",
    "account_updated",
    "account_settings_updated",
    "business_capability_updated",
    "business_username_updated",
    "payment_configuration_updated",
    "phone_number_name_updated",
    "phone_number_quality_updated",
    "security_updated",
    "template_components_updated",
    "template_quality_updated",
    "template_status_updated",
    "template_category_updated",
    "template_category_misuse_detected",
    // Reviewed in roadmap M2d (design D25): a number's tenant receives
    // them, routed as every event naming a number is.
    "standby_observed",
    "thread_control_changed",
    "user_action_reported",
    // The service's own (`crate::number_events`).
    "number_connected",
    "number_disconnected",
    "number_reconnect_required",
    // The service's own, about a stored token (`crate::token_expiry`,
    // roadmap M3e).
    "token_expiring",
];

/// The library's event types that are operator-only whoever owns the
/// number: a field the library does not type (`unknown`: a raw body of any
/// shape), a signed body that is not a webhook (`unparsed`), and a business
/// portfolio's partner solution (`partner_solution_updated`: no WABA to
/// route it by). Moving one to [`TENANT_EVENT_TYPES`] later is additive;
/// the reverse is not. Until roadmap M2d, the types the library's webhook
/// conformance sweep (PR #17) added were here too (`standby_observed`,
/// `thread_control_changed`, `user_action_reported`): the owner decided on
/// 2026-09-26 that M2 makes them tenant-visible (design decision D25), and
/// M2d moved them.
pub const OPERATOR_EVENT_TYPES: [&str; 3] = ["unknown", "unparsed", "partner_solution_updated"];

/// Whether events of `kind` may reach a tenant.
pub fn tenant_visible(kind: &str) -> bool {
    TENANT_EVENT_TYPES.contains(&kind)
}

/// The key an event is recorded under, before the outbox scopes and
/// hashes it ([`outbox_key`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKey {
    /// The library's `WebhookEvent::dedup_key`.
    Library(String),
    /// An event the library gives no key (`error_reported`, `unparsed`):
    /// the SHA-256 of the signed body it came in and its position among
    /// that body's keyless events. A redelivery reproduces the key if it
    /// carries the same bytes (assumed: Meta documents the retries, not
    /// their bytes), and the event is recorded once within
    /// [`KEYLESS_DEDUP_WINDOW`]; after it, again, as a new occurrence.
    Delivery {
        /// SHA-256 of the signed body.
        body_sha256: [u8; 32],
        /// Its position among the body's keyless events, from 0.
        position: usize,
    },
}

/// Where an event goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The tenant holding the event's number (or, for an event naming no
    /// number, its WABA) since before the event: the inbox records the
    /// event.
    pub owner: Option<TenantId>,
    /// The outbox row's tenant: the owner, for a [`tenant_visible`] type;
    /// else `None`, an operator-only row.
    pub tenant: Option<TenantId>,
    /// Why the row is operator-only (a fixed set, for the log): `unowned`
    /// (no binding holds its number or WABA, or a stale one), `before_binding`
    /// (Meta dated it before its binding began, or, the WABA having had a
    /// previous tenant, in the second it began or within the skew margin
    /// after it: maybe a previous holder's), `after_move` (undated, within
    /// the window after the WABA moved from another tenant: roadmap S2b),
    /// `stale`
    /// (Meta dated it before the dedup lease's memory: a replay), or `type`
    /// (a type no tenant receives). `None` for a tenant's row.
    pub operator_only: Option<&'static str>,
    /// The binding the owner was found by, which the outbox checks again
    /// before it keeps the row's tenant ([`crate::outbox::Outbox::insert`]);
    /// `None` when nobody owns the event.
    pub guard: Option<RouteGuard>,
}

/// Who holds an event ([`owner`]): the tenant, and the binding it was
/// found by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// The tenant.
    pub tenant: TenantId,
    /// The binding, and the event's date, for the outbox's check.
    pub guard: RouteGuard,
}

/// When Meta says the event happened: the message's, status's, call's…
/// own time, else its entry's; a contact sync's, when the webhook was
/// triggered (`state_sync[].metadata.timestamp`). `None` for events with
/// no date of their own: history syncs (they carry the past on purpose), a
/// contact sync or a group update without a date, errors, bodies that are
/// not webhooks. An undated event reaches whoever holds its binding when it
/// arrives, except within the window after a move (see the
/// [module](self)).
pub fn meta_time(event: &WebhookEvent) -> Option<OffsetDateTime> {
    use WebhookEvent as E;
    match event {
        E::MessageReceived { message, .. } => Some(message.timestamp),
        E::AppStateSynced { item, .. } => item.metadata.as_ref().and_then(|m| m.timestamp),
        E::StatusUpdated { status, .. } => Some(status.timestamp),
        E::MessageEchoed { echo, .. } => Some(echo.timestamp),
        E::CallUpdated { call, .. } => Some(call.timestamp),
        E::CallStatusUpdated { status, .. } => Some(status.timestamp),
        E::UserPreferenceChanged { preference, .. } => Some(preference.timestamp),
        E::UserIdChanged { update, .. } => Some(update.timestamp),
        E::AutomaticEventDetected { detected, .. } => Some(detected.timestamp),
        E::GroupUpdated { update, .. } => update.timestamp,
        E::UserActionReported { action, .. } => Some(action.timestamp),
        E::ThreadControlChanged { update, .. } => Some(update.timestamp),
        E::StandbyObserved { item, .. } => match item.as_ref() {
            StandbyItem::Message(message) => Some(message.timestamp),
            StandbyItem::Echo(echo) => Some(echo.timestamp),
            StandbyItem::Status(status) => Some(status.timestamp),
            _ => None,
        },
        E::AccountSettingsUpdated { time, update, .. } => update.timestamp.or(*time),
        E::FlowUpdated { time, .. }
        | E::AccountAlert { time, .. }
        | E::AccountReviewUpdated { time, .. }
        | E::AccountUpdated { time, .. }
        | E::BusinessCapabilityUpdated { time, .. }
        | E::BusinessUsernameUpdated { time, .. }
        | E::PartnerSolutionUpdated { time, .. }
        | E::PaymentConfigurationUpdated { time, .. }
        | E::PhoneNumberNameUpdated { time, .. }
        | E::PhoneNumberQualityUpdated { time, .. }
        | E::SecurityUpdated { time, .. }
        | E::TemplateComponentsUpdated { time, .. }
        | E::TemplateQualityUpdated { time, .. }
        | E::TemplateStatusUpdated { time, .. }
        | E::TemplateCategoryUpdated { time, .. }
        | E::TemplateCategoryMisuseDetected { time, .. }
        | E::Unknown { time, .. } => *time,
        // History syncs import the past; errors and bodies that are not
        // webhooks carry no date. A type a later library adds has none
        // until listed here.
        _ => None,
    }
}

/// The business number an event is about: the one it names, or, for a
/// change the library did not type, the `metadata.phone_number_id` of its
/// raw value (the inbox still reads an untyped `history` change by it).
pub fn event_number(event: &WebhookEvent) -> Option<PhoneNumberId> {
    if let Some(pn) = event.phone_number_id() {
        return Some(pn.clone());
    }
    let WebhookEvent::Unknown { raw, .. } = event else {
        return None;
    };
    raw.get("metadata")?
        .get("phone_number_id")?
        .as_str()
        .filter(|pn| !pn.is_empty())
        .map(PhoneNumberId::new_unchecked)
}

/// The tenant holding `event`'s number or WABA, by the bindings, and the
/// binding it holds it by ([`Holder::guard`]); or why nobody does (see
/// [`Route::operator_only`]).
///
/// - Number first: the event's number (or an untyped change's
///   `metadata.phone_number_id`), bound under the WABA the event names;
///   else, naming no number, its WABA.
/// - Since before the event: Meta's date for it ([`meta_time`]) is no
///   earlier than the second the WABA's binding began in, and, when the
///   WABA had a previous tenant, in a later second, by the skew margin
///   more (the event's own second is too close to tell after a move:
///   [`RouteGuard::admits`]). A WABA unbound from one tenant and bound to
///   another does not bring the first one's dated events (Meta retries for
///   up to 7 days) to the second; a first onboarding keeps the events of
///   its first second (roadmap S2b).
/// - An undated event, after a move: past the window
///   ([`UndatedAfterMove::OperatorOnly`]) since the WABA was last unbound
///   from another tenant, else nobody's (`after_move`); with
///   [`UndatedAfterMove::CurrentHolder`], or for a WABA no other tenant
///   held, the current holder's.
///
/// # Errors
///
/// The store failing.
pub async fn owner(
    store: &dyn RecordStore,
    event: &WebhookEvent,
    at: &Routing,
) -> StoreResult<Result<Holder, &'static str>> {
    let dated = meta_time(event);
    // A replay: dated before what the dedup lease remembers.
    if dated.is_some_and(|dated| dated < at.not_before) {
        return Ok(Err("stale"));
    }
    let (binding, guarded) = if let Some(pn) = event_number(event) {
        let Some(number) = store.number(&pn).await? else {
            return Ok(Err("unowned"));
        };
        // A number bound under another WABA than the event names: the
        // bindings are stale, and neither tenant is certainly the owner.
        if event.waba_id().is_some_and(|waba| *waba != number.waba_id) {
            return Ok(Err("unowned"));
        }
        (
            store.waba(&number.waba_id).await?,
            GuardedBinding::Number {
                phone_number_id: pn,
                waba_id: number.waba_id,
            },
        )
    } else if let Some(waba) = event.waba_id() {
        (store.waba(waba).await?, GuardedBinding::Waba(waba.clone()))
    } else {
        return Ok(Err("unowned"));
    };
    let Some(binding) = binding else {
        return Ok(Err("unowned"));
    };
    let guard = RouteGuard {
        binding: guarded,
        not_after: dated,
        after_move: at.moves.after_move(at.now),
    };
    if !guard.admits(binding.attached_at, binding.previous_tenant_unbound_at) {
        return Ok(Err(if dated.is_some() {
            "before_binding"
        } else {
            "after_move"
        }));
    }
    Ok(Ok(Holder {
        tenant: binding.tenant_id,
        guard,
    }))
}

/// Where `event` goes: its owner, and the outbox row's tenant. An event
/// Meta dated before `at.not_before` is a replay (security review L3), no
/// tenant's; an undated one within the window after a move, too
/// ([`owner`]).
///
/// # Errors
///
/// The store failing.
pub async fn route(
    store: &dyn RecordStore,
    event: &WebhookEvent,
    at: &Routing,
) -> StoreResult<Route> {
    Ok(match owner(store, event, at).await? {
        Ok(Holder { tenant, guard }) if tenant_visible(event.kind()) => Route {
            owner: Some(tenant.clone()),
            tenant: Some(tenant),
            operator_only: None,
            guard: Some(guard),
        },
        Ok(Holder { tenant, guard }) => Route {
            owner: Some(tenant),
            tenant: None,
            operator_only: Some("type"),
            guard: Some(guard),
        },
        Err(reason) => Route {
            owner: None,
            tenant: None,
            operator_only: Some(reason),
            guard: None,
        },
    })
}

/// The JSON an event is stored and served as (`data` of the envelope): the
/// library's `WebhookEvent` serialization, pinned by the service's
/// `tests/event_data.rs`.
///
/// # Errors
///
/// Serialization failing (it does not for the library's events).
pub fn event_data(event: &WebhookEvent) -> Result<String, serde_json::Error> {
    serde_json::to_string(event)
}

/// The outbox's idempotency key of an event keyed `key`, about the
/// business number `phone_number_id` (when it names one), whose JSON is
/// `data`: SHA-256 (hex) over the number and the key, so two numbers never
/// share one (hashed: some dedup keys hold a group participant's phone
/// number). A keyless event's key also covers its `data`, so a library
/// that splits a body into other events never takes one for another.
pub fn outbox_key(key: &EventKey, phone_number_id: Option<&str>, data: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"meta-whatsapp-server/outbox-key/v1\0");
    hash.update(phone_number_id.unwrap_or_default().as_bytes());
    hash.update(b"\0");
    match key {
        EventKey::Library(key) => {
            hash.update(b"library\0");
            hash.update(key.as_bytes());
        }
        EventKey::Delivery {
            body_sha256,
            position,
        } => {
            hash.update(b"delivery\0");
            hash.update(body_sha256);
            // 8 bytes on every target (a `usize` is 4 on a 32-bit one): the
            // key is stored, and replicas must agree on it.
            hash.update(u64::try_from(*position).unwrap_or(u64::MAX).to_be_bytes());
            hash.update(Sha256::digest(data.as_bytes()));
        }
    }
    hex::encode(hash.finalize())
}

/// The key event ids are derived with: HMAC-SHA256 under the (first) app
/// secret of a fixed label, so every replica derives the same ids, and an
/// id tells nothing of the event it names. Another first app secret (a
/// rotation) is another key: an event recorded again after it (its row
/// purged) gets another id.
#[derive(Clone)]
pub struct EventIdKey([u8; 32]);

impl std::fmt::Debug for EventIdKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EventIdKey(<redacted>)")
    }
}

type HmacSha256 = hmac::Hmac<Sha256>;

/// HMAC-SHA256 of `message` under `key`. HMAC takes keys of any length, so
/// this is never `None`; it is handled as a failure rather than with a
/// fallback key.
fn hmac_sha256(key: &[u8], message: &[u8]) -> Option<[u8; 32]> {
    use hmac::Mac as _;
    let mut mac = <HmacSha256 as hmac::Mac>::new_from_slice(key).ok()?;
    mac.update(message);
    Some(mac.finalize().into_bytes().into())
}

impl EventIdKey {
    /// The key of the deployment whose Meta app has `app_secret`; `None`
    /// never happens (see `hmac_sha256`).
    pub fn from_app_secret(app_secret: &AppSecret) -> Option<Self> {
        hmac_sha256(
            app_secret.expose_secret().as_bytes(),
            b"meta-whatsapp-server/event-id/v1",
        )
        .map(Self)
    }

    /// The id of the event whose outbox key is `outbox_key`: `evt_` and 32
    /// hex digits. The same event gets the same id every time it is
    /// recorded under this key, a replay after its row was purged included
    /// (security review L3): receivers deduplicate on it. (An event the
    /// library gives no dedup key has an id per occurrence:
    /// [`KEYLESS_DEDUP_WINDOW`].)
    pub fn event_id(&self, outbox_key: &str) -> Option<String> {
        let mac = hmac_sha256(&self.0, outbox_key.as_bytes())?;
        Some(format!("evt_{}", hex::encode(&mac[..16])))
    }
}

/// Why [`outbox_row`] made no row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RowError {
    /// The event did not serialize: serde's category only (its message
    /// could quote content).
    #[error("an event did not serialize ({0:?})")]
    Serialization(serde_json::error::Category),
    /// No id could be derived.
    #[error("no event id")]
    NoId,
}

/// The outbox row of `event`, keyed `key`, routed by `route`, received at
/// `now` on the webhook pipeline's clock, its id derived with `ids`.
///
/// A library key names one event: its id, whenever it is recorded. A
/// keyless event is deduplicated within [`KEYLESS_DEDUP_WINDOW`] only,
/// and each occurrence recorded is an event of its own: its id covers when
/// it was received (two occurrences recorded are a window apart, on the
/// same clock, so their ids differ).
///
/// # Errors
///
/// The event not serializing, or no id.
pub fn outbox_row(
    event: &WebhookEvent,
    key: &EventKey,
    route: &Route,
    now: OffsetDateTime,
    ids: &EventIdKey,
) -> Result<NewEvent, RowError> {
    let data = event_data(event).map_err(|e| RowError::Serialization(e.classify()))?;
    let phone_number_id = event.phone_number_id().cloned();
    let dedup_key = outbox_key(
        key,
        phone_number_id.as_ref().map(PhoneNumberId::as_str),
        &data,
    );
    let (id_of, dedup_window) = match key {
        EventKey::Library(_) => (dedup_key.clone(), None),
        EventKey::Delivery { .. } => (
            format!("{dedup_key}@{}", now.unix_timestamp_nanos()),
            Some(DedupWindow {
                now,
                until: now + KEYLESS_DEDUP_WINDOW,
            }),
        ),
    };
    let id = ids.event_id(&id_of).ok_or(RowError::NoId)?;
    Ok(NewEvent {
        id,
        dedup_key: Some(dedup_key),
        dedup_window,
        tenant: route.tenant.clone(),
        // The binding the tenant was found by: the outbox keeps the tenant
        // only while it holds.
        route_guard: route.tenant.as_ref().and(route.guard.clone()),
        phone_number_id,
        waba_id: event.waba_id().cloned(),
        event_type: event.kind().to_owned(),
        data,
    })
}

/// A poll's events and the cursor to continue from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Polled {
    /// The events, in sequence order.
    pub events: Vec<StoredEvent>,
    /// Pass as `after` next time: the last event's sequence when more
    /// follow, else the newest sequence of the tenant's stream.
    pub next_after: i64,
}

/// A tenant's events after `query.after`, in its own stream of sequences
/// (docs/design/server.md, sections 2.3 and 4.2).
///
/// # Errors
///
/// `410 cursor_expired` for a cursor below what was purged (by retention,
/// or with a deleted tenant of the same id: events after it may be gone);
/// `422 invalid_request` on `after` for a cursor the tenant's stream never
/// reached (a restored or another database); `503 storage_unavailable`.
pub async fn poll(outbox: &dyn Outbox, query: &EventQuery) -> Result<Polled, ServiceError> {
    let page = outbox.page(query).await?;
    let start = match query.after {
        Some(after) if after < page.purged_through => {
            return Err(ServiceError::new(ErrorCode::CURSOR_EXPIRED));
        }
        Some(after) if after > page.high_water => return Err(ServiceError::invalid("after")),
        Some(after) => after,
        None => page.purged_through,
    };
    let next_after = match page.events.last() {
        Some(last) if page.more => last.sequence,
        _ => page.high_water.max(start),
    };
    Ok(Polled {
        events: page.events,
        next_after,
    })
}

/// Whether `id` has the shape of an event id: `evt_` and 32 lowercase hex
/// digits ([`EventIdKey::event_id`]).
pub fn is_event_id(id: &str) -> bool {
    id.strip_prefix("evt_").is_some_and(|hex| {
        hex.len() == 32 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// `tenant`'s event of id `id` (`GET /v1/events/{id}`, roadmap M2b): the
/// whole event, as polling answers it, however large.
///
/// # Errors
///
/// `404 not_found` for an id that is not an event's ([`is_event_id`],
/// before the outbox is read), for another tenant's event or an
/// operator-only row (the outbox looks the id up in `tenant`'s stream
/// only, so nothing of another tenant's is read), and for a purged or
/// unknown one, all alike; `503 storage_unavailable`.
pub async fn get_event(
    outbox: &dyn Outbox,
    tenant: &TenantId,
    id: &str,
) -> Result<StoredEvent, ServiceError> {
    if !is_event_id(id) {
        return Err(ServiceError::not_found());
    }
    outbox
        .get(tenant, id)
        .await?
        .filter(|event| event.tenant.as_ref() == Some(tenant))
        .ok_or_else(ServiceError::not_found)
}

/// Housekeeping's outbox purge: the events past `retention`, how many
/// went. It takes no lock: housekeeping runs it under its `LeaderLock`
/// turn, and two at once are safe ([`Outbox::purge`]).
///
/// # Errors
///
/// The store failing.
pub async fn purge_outbox(outbox: &dyn Outbox, retention: Duration) -> StoreResult<u64> {
    let purged = outbox.purge(retention).await?;
    if purged > 0 {
        tracing::info!(purged, "outbox events past retention purged");
    }
    Ok(purged)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An outbox no test may read: [`get_event`] answers an id of no
    /// event's shape before it asks the outbox.
    struct Untouched;

    #[async_trait::async_trait]
    impl Outbox for Untouched {
        async fn insert(&self, _: &NewEvent) -> StoreResult<Option<i64>> {
            panic!("the outbox was written")
        }

        async fn headers_after(
            &self,
            _: &crate::outbox::HeaderQuery,
        ) -> StoreResult<crate::outbox::HeaderPage> {
            panic!("the outbox was read")
        }

        async fn page(&self, _: &EventQuery) -> StoreResult<crate::outbox::EventPage> {
            panic!("the outbox was read")
        }

        async fn get(&self, _: &TenantId, _: &str) -> StoreResult<Option<StoredEvent>> {
            panic!("the outbox was read")
        }

        async fn purge(&self, _: Duration) -> StoreResult<u64> {
            panic!("the outbox was purged")
        }

        async fn erase_contacts(
            &self,
            _: &TenantId,
            _: &meta_whatsapp_rs::core::ids::PhoneNumberId,
            _: &[String],
        ) -> StoreResult<u64> {
            panic!("the outbox was erased from")
        }

        async fn received_media(
            &self,
            _: &meta_whatsapp_rs::core::ids::MediaId,
        ) -> StoreResult<Vec<crate::outbox::ReceivedMedia>> {
            panic!("the outbox was read")
        }
    }

    /// `GET /v1/events/{id}` with an id of no event's shape (the wrong
    /// prefix, length, case or digits; the route's other literal, `stream`)
    /// is `404 not_found` before the outbox is read. Decisive:
    /// `is_event_id` in `get_event`.
    #[tokio::test]
    async fn an_id_of_no_events_shape_is_not_looked_up() {
        let tenant = TenantId::parse("merchant-42").unwrap();
        for id in [
            "stream",
            "",
            "evt_",
            "evt_4042233518184f0e9822bebdbba028a",
            "evt_4042233518184f0e9822bebdbba028a8a",
            "evt_4042233518184F0E9822BEBDBBA028A8",
            "evt_4042233518184f0e9822bebdbba028ag",
            "EVT_4042233518184f0e9822bebdbba028a8",
            "4042233518184f0e9822bebdbba028a8",
        ] {
            let error = get_event(&Untouched, &tenant, id).await.unwrap_err();
            assert_eq!((error.status(), error.code()), (404, "not_found"), "{id:?}");
        }
        assert!(is_event_id("evt_4042233518184f0e9822bebdbba028a8"));
    }

    /// The outbox key is stored (`wa_server_events.dedup_key`): derived
    /// otherwise after an upgrade, an event Meta redelivers across it is
    /// recorded twice. Known answers (docs/architecture.md, "Stable
    /// identifiers"), computed apart from this code, the `w` of the domain
    /// spelled `\x77` so that a rename cannot rewrite it:
    //
    // python3 - <<'EOF'
    // import hashlib
    // d = b"meta-\x77hatsapp-server/outbox-key/v1\x00"
    // s = hashlib.sha256
    // pn = b"106540352242922"
    // print(s(d + pn + b"\x00library\x00wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBI3MTE5MjVBOTE3MDk5QUVFM0YA:delivered").hexdigest())
    // print(s(d + b"\x00library\x00template_status_updated:0f").hexdigest())
    // body = s(b'{"object":"whatsapp_business_account","entry":[]}').digest()
    // data = s(b'{"event":"error_reported"}').digest()
    // print(s(d + pn + b"\x00delivery\x00" + body + (1).to_bytes(8, "big") + data).hexdigest())
    // EOF
    //
    // Decisive: any byte of the domain, of a separator or of a tag, and
    // the position's width (8 bytes, big-endian).
    #[test]
    fn the_outbox_key_is_pinned() {
        let pn = Some("106540352242922");
        assert_eq!(
            outbox_key(
                &EventKey::Library(
                    "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBI3MTE5MjVBOTE3MDk5QUVFM0YA:delivered".into()
                ),
                pn,
                "not part of a library key",
            ),
            "8b03965281267f2e3d95bcc9993f3eaa82bca4a1926e46035d8f504f39bc493b"
        );
        assert_eq!(
            outbox_key(
                &EventKey::Library("template_status_updated:0f".into()),
                None,
                ""
            ),
            "d44da459bea55c133593a8c93602889cc078495337367f90afa235431601884f"
        );
        let body = br#"{"object":"whatsapp_business_account","entry":[]}"#;
        assert_eq!(
            outbox_key(
                &EventKey::Delivery {
                    body_sha256: Sha256::digest(body).into(),
                    position: 1,
                },
                pn,
                r#"{"event":"error_reported"}"#,
            ),
            "35b3b949670586faf869b7fd42f64f8778e66ebf0f6baa4bf8dd5742cc750239"
        );
    }

    /// Event ids are stored (`wa_server_events.id`) and receivers
    /// deduplicate on them: derived otherwise after an upgrade, an event
    /// recorded again gets another id. Known answers (`evt_` and the first
    /// 16 bytes of the HMAC, in hex), computed apart from this code:
    //
    // key=$(printf %b 'meta-\x77hatsapp-server/event-id/v1' \
    //   | openssl dgst -sha256 -hmac app-secret-for-tests -r | cut -c1-64)
    // printf %s 8b03965281267f2e3d95bcc9993f3eaa82bca4a1926e46035d8f504f39bc493b \
    //   | openssl dgst -sha256 -mac HMAC -macopt hexkey:$key -r | cut -c1-32
    //
    // Decisive: any byte of the label, the `evt_` prefix, the length.
    #[test]
    fn event_ids_are_pinned() {
        let ids = EventIdKey::from_app_secret(&AppSecret::new("app-secret-for-tests")).unwrap();
        assert_eq!(
            ids.0.as_slice(),
            hex::decode("25e756ec5c10294c3c94138600dd769f8b73af157b7aefac75fa33cf2d29ab78")
                .unwrap()
        );
        assert_eq!(
            ids.event_id("8b03965281267f2e3d95bcc9993f3eaa82bca4a1926e46035d8f504f39bc493b")
                .unwrap(),
            "evt_4042233518184f0e9822bebdbba028a8"
        );
    }

    /// Meta's date for an account settings update is the update's own
    /// `timestamp`, else its entry's `time` (Meta's example sends only the
    /// first; a batch can carry both): routing compares it with the WABA's
    /// binding. Decisive: the order of the two in `meta_time`, and the
    /// fallback.
    #[test]
    fn an_account_settings_update_is_dated_by_the_update_then_the_entry() {
        use meta_whatsapp_rs::webhooks::WebhookPayload;
        let dated = |body: &serde_json::Value| {
            let events = WebhookPayload::from_slice(body.to_string().as_bytes())
                .unwrap()
                .into_events();
            let [event] = events.as_slice() else {
                panic!("{events:?}")
            };
            assert_eq!(event.kind(), "account_settings_updated");
            meta_time(event).map(OffsetDateTime::unix_timestamp)
        };
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../meta-whatsapp-webhooks/tests/fixtures/fields/account_settings_update.json"
        ))
        .unwrap();
        assert_eq!(dated(&body), Some(1_671_644_824), "the update's own");
        body["entry"][0]["time"] = serde_json::json!(1_671_000_000);
        assert_eq!(dated(&body), Some(1_671_644_824), "before the entry's");
        body["entry"][0]["changes"][0]["value"]
            .as_object_mut()
            .unwrap()
            .remove("timestamp");
        assert_eq!(dated(&body), Some(1_671_000_000), "else the entry's");
    }

    /// Roadmap S2 (the security review's M2): a contact sync is dated when
    /// Meta triggered its webhook (`state_sync[].metadata.timestamp`, Meta's
    /// example's `1739321024`), so a previous holder's sync redelivered
    /// after a move is compared with the new binding like any dated event;
    /// without that date it stays undated. Decisive: the `AppStateSynced`
    /// arm of `meta_time`.
    #[test]
    fn a_contact_sync_is_dated_when_its_webhook_was_triggered() {
        use meta_whatsapp_rs::webhooks::WebhookPayload;
        let dated = |body: &serde_json::Value| {
            let events = WebhookPayload::from_slice(body.to_string().as_bytes())
                .unwrap()
                .into_events();
            let [event] = events.as_slice() else {
                panic!("{events:?}")
            };
            assert_eq!(event.kind(), "app_state_synced");
            meta_time(event).map(OffsetDateTime::unix_timestamp)
        };
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../meta-whatsapp-webhooks/tests/fixtures/fields/smb_app_state_sync.json"
        ))
        .unwrap();
        assert_eq!(dated(&body), Some(1_739_321_024));
        body["entry"][0]["changes"][0]["value"]["state_sync"][0]
            .as_object_mut()
            .unwrap()
            .remove("metadata");
        assert_eq!(dated(&body), None, "undated without it");
    }

    /// Roadmap L20c: an untyped change's `metadata.phone_number_id` is kept
    /// as Meta sent it, whatever its shape (`PhoneNumberId::new` would
    /// refuse this one), so the event is still routed by its number.
    /// Decisive: checking it in `event_number`.
    #[test]
    fn an_untyped_changes_number_is_kept_as_sent() {
        use meta_whatsapp_rs::webhooks::WebhookPayload;
        let body = serde_json::json!({"object": "whatsapp_business_account", "entry": [{
            "id": "102290129340398", "time": 1_739_321_024,
            "changes": [{"field": "a_field_meta_adds_later",
                         "value": {"metadata": {"phone_number_id": "PN-1/x"}}}]
        }]});
        let events = WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events();
        let [event] = events.as_slice() else {
            panic!("{events:?}")
        };
        assert!(matches!(event, WebhookEvent::Unknown { .. }), "{event:?}");
        assert_eq!(
            event_number(event).as_ref().map(PhoneNumberId::as_str),
            Some("PN-1/x")
        );
        assert!(PhoneNumberId::new("PN-1/x").is_err());
    }

    /// The two lists split the library's kinds: none is both.
    #[test]
    fn tenant_and_operator_types_are_disjoint() {
        for kind in OPERATOR_EVENT_TYPES {
            assert!(!tenant_visible(kind), "{kind}");
        }
        let mut all: Vec<&str> = TENANT_EVENT_TYPES.to_vec();
        all.extend(OPERATOR_EVENT_TYPES);
        let unique: std::collections::BTreeSet<&str> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len());
    }
}
