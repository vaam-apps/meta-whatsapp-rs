//! Meta's webhook events, as the service records and serves them
//! (docs/design/server.md, sections 2.3 and 4.2): where an event goes
//! ([`route`]), the outbox row it becomes ([`outbox_row`]: its key and its
//! id), and polling the outbox ([`poll`]). Plain functions over the ports:
//! the webhook pipeline (`meta_whatsapp_server::events`) and the API
//! adapters only translate.
//!
//! **Routing is an allow-list.** An event naming a business phone number
//! (or, untyped, whose raw `metadata` names one) belongs to the tenant that
//! number is bound to, and only when the event's WABA, if it names one, is
//! the number's WABA in the bindings; an event naming only a WABA belongs
//! to the WABA's tenant. And only when Meta dated it no earlier than that
//! WABA's binding began ([`meta_time`]): a WABA moved from one tenant to
//! another does not bring the first one's retried events to the second.
//! The outbox row carries that tenant only for the types in
//! [`TENANT_EVENT_TYPES`]; `unknown`, `unparsed`, `partner_solution_updated`,
//! the types not yet reviewed for tenants ([`OPERATOR_EVENT_TYPES`]), any
//! type a later library adds, and every event of a number or WABA no
//! tenant holds (or held then) are operator-only rows (no tenant: never
//! polled), and the inbox records only an owned event.
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

use crate::error::ServiceError;
use crate::model::TenantId;
use crate::outbox::{DedupWindow, EventQuery, NewEvent, Outbox, StoredEvent};
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
/// `tests/event_data.rs`). Every other type, today's `unknown`,
/// `unparsed`, `partner_solution_updated` and the types not yet reviewed
/// for tenants, and any a later library adds, is operator-only
/// ([`OPERATOR_EVENT_TYPES`] lists today's).
pub const TENANT_EVENT_TYPES: [&str; 28] = [
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
];

/// The library's event types that are operator-only whoever owns the
/// number: a field the library does not type (`unknown`: a raw body of any
/// shape), a signed body that is not a webhook (`unparsed`), a business
/// portfolio's partner solution (`partner_solution_updated`: no WABA to
/// route it by), and, until the service reviews them for tenants, the
/// types the library's webhook conformance sweep (PR #17) added, which
/// were `unknown` before it: Conversation Routing's standby copies and
/// handovers (`standby_observed`, `thread_control_changed`) and the
/// Marketing Messages API's clicks (`user_action_reported`). Moving one to
/// [`TENANT_EVENT_TYPES`] later is additive; the reverse is not. The owner
/// decided on 2026-09-26 that M2 makes these three tenant-visible (design
/// decision D25).
pub const OPERATOR_EVENT_TYPES: [&str; 6] = [
    "unknown",
    "unparsed",
    "partner_solution_updated",
    "standby_observed",
    "thread_control_changed",
    "user_action_reported",
];

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
    /// (Meta dated it before its binding: a previous holder's), `stale`
    /// (Meta dated it before the dedup lease's memory: a replay), or `type`
    /// (a type no tenant receives). `None` for a tenant's row.
    pub operator_only: Option<&'static str>,
}

/// When Meta says the event happened: the message's, status's, call's…
/// own time, else its entry's. `None` for events with no date of their
/// own: history and contact syncs (they carry the past on purpose),
/// errors, bodies that are not webhooks.
pub fn meta_time(event: &WebhookEvent) -> Option<OffsetDateTime> {
    use WebhookEvent as E;
    match event {
        E::MessageReceived { message, .. } => Some(message.timestamp),
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
        // History and contact syncs import the past; errors and bodies
        // that are not webhooks carry no date. A type a later library adds
        // has none until listed here.
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
        .map(PhoneNumberId::new)
}

/// The tenant holding `event`'s number or WABA, by the bindings, and why
/// nobody does (see [`Route::operator_only`]).
///
/// - Number first: the event's number (or an untyped change's
///   `metadata.phone_number_id`), bound under the WABA the event names;
///   else, naming no number, its WABA.
/// - Since before the event: Meta's date for it ([`meta_time`]) is not
///   before the second the WABA's binding began. A WABA unbound from one
///   tenant and bound to another does not bring the first one's events
///   (Meta retries for up to 7 days) to the second.
///
/// # Errors
///
/// The store failing.
pub async fn owner(
    store: &dyn RecordStore,
    event: &WebhookEvent,
    not_before: OffsetDateTime,
) -> StoreResult<Result<TenantId, &'static str>> {
    // A replay: dated before what the dedup lease remembers.
    if meta_time(event).is_some_and(|at| at < not_before) {
        return Ok(Err("stale"));
    }
    let binding = if let Some(pn) = event_number(event) {
        let Some(number) = store.number(&pn).await? else {
            return Ok(Err("unowned"));
        };
        // A number bound under another WABA than the event names: the
        // bindings are stale, and neither tenant is certainly the owner.
        if event.waba_id().is_some_and(|waba| *waba != number.waba_id) {
            return Ok(Err("unowned"));
        }
        store.waba(&number.waba_id).await?
    } else if let Some(waba) = event.waba_id() {
        store.waba(waba).await?
    } else {
        None
    };
    let Some(binding) = binding else {
        return Ok(Err("unowned"));
    };
    if meta_time(event).is_some_and(|at| at.unix_timestamp() < binding.attached_at.unix_timestamp())
    {
        return Ok(Err("before_binding"));
    }
    Ok(Ok(binding.tenant_id))
}

/// Where `event` goes: its owner, and the outbox row's tenant. An event
/// Meta dated before `not_before` is a replay (security review L3), no
/// tenant's.
///
/// # Errors
///
/// The store failing.
pub async fn route(
    store: &dyn RecordStore,
    event: &WebhookEvent,
    not_before: OffsetDateTime,
) -> StoreResult<Route> {
    Ok(match owner(store, event, not_before).await? {
        Ok(owner) if tenant_visible(event.kind()) => Route {
            tenant: Some(owner.clone()),
            owner: Some(owner),
            operator_only: None,
        },
        Ok(owner) => Route {
            owner: Some(owner),
            tenant: None,
            operator_only: Some("type"),
        },
        Err(reason) => Route {
            owner: None,
            tenant: None,
            operator_only: Some(reason),
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
    let phone_number_id = event.phone_number_id().map(|pn| pn.as_str().to_owned());
    let dedup_key = outbox_key(key, phone_number_id.as_deref(), &data);
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
        meta_time: meta_time(event),
        tenant: route.tenant.clone(),
        phone_number_id,
        waba_id: event.waba_id().map(|waba| waba.as_str().to_owned()),
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
            return Err(ServiceError::new("cursor_expired"));
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

/// One round of housekeeping: purge the outbox past `retention`. `None`
/// when another replica holds the housekeeping lock.
///
/// # Errors
///
/// The store failing.
pub async fn purge_outbox(outbox: &dyn Outbox, retention: Duration) -> StoreResult<Option<u64>> {
    let purged = outbox.purge(retention).await?;
    if let Some(purged) = purged
        && purged > 0
    {
        tracing::info!(purged, "outbox events past retention purged");
    }
    Ok(purged)
}

#[cfg(test)]
mod tests {
    use super::*;

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
