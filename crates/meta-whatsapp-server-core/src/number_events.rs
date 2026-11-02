//! The service's own events about a tenant's numbers (docs/design/server.md,
//! sections 2.3 and 3.4; roadmap M2d): no webhook of Meta's carries them,
//! the service records them in the event outbox when it changes what a
//! number's tenant can do with it, and the tenant polls and streams them as
//! it does Meta's events ([`crate::events::TENANT_EVENT_TYPES`] lists
//! them).
//!
//! | Type | Recorded when | `data` |
//! | --- | --- | --- |
//! | `number_connected` | an admin attach or an Embedded Signup (completed or resumed) bound the number to the tenant and stored its token (a first attach, or one again: a reconnection) | `{event, phone_number_id, waba_id}` |
//! | `number_disconnected` | the number is about to leave its tenant: the tenant's disconnect, once Meta unsubscribed the app (`reason`: `tenant_disconnected`), the operator's unbind (`operator_unbound`), or Meta's `account_update` offboarding, confirmed with Graph (`partner_app_uninstalled`, `account_deleted`) | `{event, phone_number_id, waba_id, reason}` |
//! | `number_reconnect_required` | Meta refused the number's stored token (`190`), and the service marked it `reconnect_required` | `{event, phone_number_id, waba_id}` |
//!
//! **Each reaches its number's tenant only**, by the outbox's own check
//! ([`RouteGuard`]): the row keeps its tenant only while, atomically with
//! the insert, the number is still bound to that tenant under that WABA,
//! and that WABA's binding began before the event's date. The event is
//! dated by the binding it is about, not by a clock: the second after the
//! binding's start ([`BindingEpoch::attached_at`]), so that its own binding
//! admits it (after a move too: the same-second rule protects a WABA's new
//! holder from the previous holder's redelivered events, and this event
//! is the binding's own), and no binding begun in a later second after a
//! move does (a deleted tenant counts as another, so a tenant created again
//! under its id is one; without a previous tenant, a binding of the
//! event's own second, one after its binding's, still holds, which only
//! the same tenant binding its own WABA again can meet). So an event of a
//! binding that moved to another tenant meanwhile, or that was unbound, its
//! tenant deleted, created again under the same id and bound again in a
//! later second, is operator-only. That is why
//! `number_disconnected` is recorded before the binding goes: after it, no
//! binding would hold. The window after a move for undated events (design
//! D36) does not apply: the event is dated.
//!
//! **Once per binding generation.** An event's key is its type, the
//! number, and the binding's epoch (its WABA, tenant, start and
//! generation: [`BindingEpoch`]); the outbox records a key once. A
//! repeated refusal of the same token records one `number_reconnect_required`;
//! an attach again (a new generation) records `number_connected` again.
//!
//! `data` names the business's own ids only: never a customer, a token,
//! nor Meta's error text.

use std::sync::Arc;

use meta_whatsapp_rs::core::ids::PhoneNumberId;
use sha2::{Digest, Sha256};

use crate::events::EventIdKey;
use crate::model::BindingEpoch;
use crate::notify::{EventNotifier, Wakeup};
use crate::outbox::{AfterMove, GuardedBinding, NewEvent, Outbox, RouteGuard};
use crate::store::StoreResult;

/// The target of this module's log lines.
const LOG_TARGET: &str = "meta_whatsapp_server::number_events";

/// The service's own event types, in [`crate::events::TENANT_EVENT_TYPES`]
/// too (its last three). No webhook of Meta's has them.
pub const NUMBER_EVENT_TYPES: [&str; 3] = [
    "number_connected",
    "number_disconnected",
    "number_reconnect_required",
];

/// What happened to a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberEvent {
    /// Bound to its tenant with a stored token: `number_connected`.
    Connected,
    /// About to leave its tenant: `number_disconnected`.
    Disconnected(DisconnectReason),
    /// Meta refused its stored token: `number_reconnect_required`.
    ReconnectRequired,
}

/// Why a number left its tenant (`number_disconnected`'s `reason`). New
/// reasons may come within `v1` (Meta's `account_update` offboarding, for
/// one): a client defaults on an unknown one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    /// The tenant disconnected its WABA (`DELETE /v1/wabas/{waba_id}`):
    /// `tenant_disconnected`.
    TenantDisconnected,
    /// The operator unbound the WABA
    /// (`DELETE /v1/admin/wabas/{waba_id}/binding`): `operator_unbound`.
    OperatorUnbound,
    /// Meta's `account_update` `PARTNER_APP_UNINSTALLED`, for this
    /// deployment's app, confirmed with Graph (roadmap M3a's
    /// `signup::AccountUpdates`): `partner_app_uninstalled`.
    PartnerAppUninstalled,
    /// Meta's `account_update` `ACCOUNT_DELETED`, confirmed with Graph:
    /// `account_deleted`.
    AccountDeleted,
}

impl DisconnectReason {
    /// Its name in `data`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TenantDisconnected => "tenant_disconnected",
            Self::OperatorUnbound => "operator_unbound",
            Self::PartnerAppUninstalled => "partner_app_uninstalled",
            Self::AccountDeleted => "account_deleted",
        }
    }
}

impl NumberEvent {
    /// Its type: one of [`NUMBER_EVENT_TYPES`].
    pub fn kind(self) -> &'static str {
        match self {
            Self::Connected => "number_connected",
            Self::Disconnected(_) => "number_disconnected",
            Self::ReconnectRequired => "number_reconnect_required",
        }
    }

    /// Its `data`: the type (`event`, as every event's `data` is tagged),
    /// the number and its WABA, and for `number_disconnected` its
    /// `reason`. The business's ids only.
    pub fn data(self, phone_number_id: &PhoneNumberId, epoch: &BindingEpoch) -> String {
        let mut data = serde_json::json!({
            "event": self.kind(),
            "phone_number_id": phone_number_id.as_str(),
            "waba_id": epoch.waba_id.as_str(),
        });
        if let (Self::Disconnected(reason), Some(map)) = (self, data.as_object_mut()) {
            map.insert("reason".to_owned(), reason.as_str().into());
        }
        data.to_string()
    }

    /// The outbox's idempotency key of this event about `phone_number_id`
    /// under `epoch`: SHA-256 (hex) over a label of its own (never an
    /// outbox key of Meta's events: `crate::events::outbox_key`'s label
    /// differs), the type, the number, and the epoch (WABA, tenant, the
    /// binding's start in Unix nanoseconds, generation), each followed by a
    /// zero byte. Stored (`wa_server_events.dedup_key`): pinned by a test.
    pub fn outbox_key(self, phone_number_id: &PhoneNumberId, epoch: &BindingEpoch) -> String {
        let mut hash = Sha256::new();
        hash.update(b"meta-whatsapp-server/number-event/v1\0");
        for part in [
            self.kind(),
            phone_number_id.as_str(),
            epoch.waba_id.as_str(),
            epoch.tenant_id.as_str(),
        ] {
            hash.update(part.as_bytes());
            hash.update(b"\0");
        }
        hash.update(
            epoch
                .attached_at
                .unix_timestamp_nanos()
                .to_string()
                .as_bytes(),
        );
        hash.update(b"\0");
        hash.update(epoch.generation.to_string().as_bytes());
        hex::encode(hash.finalize())
    }
}

/// Records the service's number events in the outbox and wakes the
/// tenant's live streams, as the webhook pipeline does for Meta's. Built
/// over the backend the service runs on (the webhook pipeline's
/// `Inbound::number_events`), with the pipeline's event id key. Cheap to
/// clone.
#[derive(Clone)]
pub struct NumberEvents {
    outbox: Arc<dyn Outbox>,
    notifier: Arc<dyn EventNotifier>,
    ids: EventIdKey,
}

impl std::fmt::Debug for NumberEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NumberEvents").finish_non_exhaustive()
    }
}

impl NumberEvents {
    /// Record into `outbox`, waking with `notifier`, ids derived with
    /// `ids`.
    pub fn new(outbox: Arc<dyn Outbox>, notifier: Arc<dyn EventNotifier>, ids: EventIdKey) -> Self {
        Self {
            outbox,
            notifier,
            ids,
        }
    }

    /// The outbox row of `event` about `phone_number_id`, under `epoch`
    /// (see the [module](self)): for `epoch`'s tenant, guarded by the
    /// number's binding under the epoch's WABA, dated the second after that
    /// binding began. `None` when no id could be derived (it always can:
    /// `EventIdKey::event_id`).
    pub fn row(
        &self,
        event: NumberEvent,
        phone_number_id: &PhoneNumberId,
        epoch: &BindingEpoch,
    ) -> Option<NewEvent> {
        let dedup_key = event.outbox_key(phone_number_id, epoch);
        let id = self.ids.event_id(&dedup_key)?;
        Some(NewEvent {
            id,
            dedup_key: Some(dedup_key),
            dedup_window: None,
            tenant: Some(epoch.tenant_id.clone()),
            route_guard: Some(RouteGuard {
                binding: GuardedBinding::Number {
                    phone_number_id: phone_number_id.clone(),
                    waba_id: epoch.waba_id.clone(),
                },
                not_after: Some(epoch.attached_at + time::Duration::SECOND),
                after_move: AfterMove::default(),
            }),
            phone_number_id: Some(phone_number_id.clone()),
            waba_id: Some(epoch.waba_id.clone()),
            event_type: event.kind().to_owned(),
            data: event.data(phone_number_id, epoch),
        })
    }

    /// Record `event` for each of `numbers` under `epoch`, in order, and
    /// wake the tenant's streams for each row recorded; how many were
    /// recorded (an event already recorded for the epoch is not again). A
    /// row the outbox kept operator-only (the binding moved meanwhile)
    /// counts too, and wakes nobody's stream: its sequence is the
    /// operator-only stream's.
    ///
    /// # Errors
    ///
    /// The outbox failing (`StorageError::Busy` on contention): the rows
    /// before it stay recorded, and a repeat records the rest (each once).
    pub async fn record(
        &self,
        event: NumberEvent,
        epoch: &BindingEpoch,
        numbers: &[PhoneNumberId],
    ) -> StoreResult<usize> {
        let mut recorded = 0;
        for pn in numbers {
            let Some(row) = self.row(event, pn, epoch) else {
                tracing::warn!(target: LOG_TARGET, "no id for a number event");
                continue;
            };
            let Some(sequence) = self.outbox.insert(&row).await? else {
                continue;
            };
            recorded += 1;
            tracing::debug!(
                target: LOG_TARGET,
                event_type = event.kind(),
                sequence,
                tenant = epoch.tenant_id.as_str(),
                "number event recorded"
            );
            let wakeup = Wakeup {
                tenant: epoch.tenant_id.clone(),
                sequence,
            };
            if let Err(error) = self.notifier.notify(&wakeup).await {
                tracing::warn!(
                    target: LOG_TARGET,
                    event_type = event.kind(),
                    error = %error,
                    "the live streams were not notified of a number event (their re-poll will find it)"
                );
            }
        }
        Ok(recorded)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a panic is the report
mod tests {
    use meta_whatsapp_rs::core::ids::WabaId;
    use meta_whatsapp_rs::core::secret::AppSecret;

    use super::*;
    use crate::model::TenantId;

    fn epoch() -> BindingEpoch {
        BindingEpoch {
            waba_id: WabaId::new("102290129340398").unwrap(),
            tenant_id: TenantId::parse("merchant-42").unwrap(),
            attached_at: time::macros::datetime!(2026-09-26 12:00:00.5 UTC),
            generation: 2,
        }
    }

    fn pn() -> PhoneNumberId {
        PhoneNumberId::new("106540352242922").unwrap()
    }

    /// An outbox no unit test writes to.
    struct Unused;

    #[async_trait::async_trait]
    impl Outbox for Unused {
        async fn insert(&self, _: &NewEvent) -> StoreResult<Option<i64>> {
            panic!("the outbox was written")
        }

        async fn page(
            &self,
            _: &crate::outbox::EventQuery,
        ) -> StoreResult<crate::outbox::EventPage> {
            panic!("the outbox was read")
        }

        async fn purge(&self, _: std::time::Duration) -> StoreResult<u64> {
            panic!("the outbox was purged")
        }

        async fn received_media(
            &self,
            _: &meta_whatsapp_rs::core::ids::MediaId,
        ) -> StoreResult<Vec<crate::outbox::ReceivedMedia>> {
            panic!("the outbox was read")
        }
    }

    /// The `data` of each type, exactly: what a tenant switches on, part
    /// of the v1 API (a change of it is an API change, as a changed
    /// snapshot of Meta's events is: the service's `tests/event_data.rs`).
    /// Decisive: each field's name and value, and `reason` on
    /// `number_disconnected` only.
    #[test]
    fn each_number_events_data_is_pinned() {
        let data = |event: NumberEvent| -> serde_json::Value {
            serde_json::from_str(&event.data(&pn(), &epoch())).unwrap()
        };
        assert_eq!(
            data(NumberEvent::Connected),
            serde_json::json!({"event": "number_connected",
                "phone_number_id": "106540352242922", "waba_id": "102290129340398"})
        );
        assert_eq!(
            data(NumberEvent::Disconnected(
                DisconnectReason::TenantDisconnected
            )),
            serde_json::json!({"event": "number_disconnected",
                "phone_number_id": "106540352242922", "waba_id": "102290129340398",
                "reason": "tenant_disconnected"})
        );
        assert_eq!(
            data(NumberEvent::Disconnected(DisconnectReason::OperatorUnbound))["reason"],
            "operator_unbound"
        );
        for (reason, name) in [
            (
                DisconnectReason::PartnerAppUninstalled,
                "partner_app_uninstalled",
            ),
            (DisconnectReason::AccountDeleted, "account_deleted"),
        ] {
            assert_eq!(data(NumberEvent::Disconnected(reason))["reason"], name);
        }
        assert_eq!(
            data(NumberEvent::ReconnectRequired),
            serde_json::json!({"event": "number_reconnect_required",
                "phone_number_id": "106540352242922", "waba_id": "102290129340398"})
        );
        let kinds = [
            NumberEvent::Connected,
            NumberEvent::Disconnected(DisconnectReason::OperatorUnbound),
            NumberEvent::ReconnectRequired,
        ]
        .map(NumberEvent::kind);
        assert_eq!(kinds, NUMBER_EVENT_TYPES);
        for kind in NUMBER_EVENT_TYPES {
            assert!(crate::events::tenant_visible(kind), "{kind}");
        }
        assert_eq!(
            crate::events::TENANT_EVENT_TYPES[crate::events::TENANT_EVENT_TYPES.len() - 3..],
            NUMBER_EVENT_TYPES
        );
    }

    /// The key is stored (`wa_server_events.dedup_key`) and the id derived
    /// from it: computed otherwise after an upgrade, an event of the same
    /// binding is recorded twice. Known answer, computed apart from this
    /// code (the `w` of the label spelled `\x77`, so that a rename cannot
    /// rewrite it):
    //
    // python3 - <<'EOF'
    // import hashlib
    // parts = [b"number_connected", b"106540352242922", b"102290129340398", b"merchant-42"]
    // m = b"meta-\x77hatsapp-server/number-event/v1\x00" + b"".join(p + b"\x00" for p in parts)
    // m += b"1790424000500000000\x002"
    // print(hashlib.sha256(m).hexdigest())
    // EOF
    //
    // Decisive: the label, each separator, each part and its order.
    #[test]
    fn the_number_event_key_is_pinned() {
        assert_eq!(
            NumberEvent::Connected.outbox_key(&pn(), &epoch()),
            "9b697a317e36aab7afcdd74e5678ad57c2e2796c3ba4d5f898a2aed21f1d473f"
        );
        let other = |epoch: BindingEpoch| NumberEvent::Connected.outbox_key(&pn(), &epoch);
        let base = other(epoch());
        assert_ne!(
            base,
            other(BindingEpoch {
                generation: 3,
                ..epoch()
            })
        );
        assert_ne!(
            base,
            other(BindingEpoch {
                attached_at: epoch().attached_at + time::Duration::MICROSECOND,
                ..epoch()
            })
        );
        assert_ne!(
            base,
            NumberEvent::ReconnectRequired.outbox_key(&pn(), &epoch())
        );
        // Both disconnections are one event of the binding.
        assert_eq!(
            NumberEvent::Disconnected(DisconnectReason::TenantDisconnected)
                .outbox_key(&pn(), &epoch()),
            NumberEvent::Disconnected(DisconnectReason::OperatorUnbound)
                .outbox_key(&pn(), &epoch())
        );
    }

    /// The row: the epoch's tenant, guarded by the number under the epoch's
    /// WABA, dated the second after its binding began; no window after a
    /// move, no margin. Its own binding admits it, after a move too; a
    /// binding begun a second later does not. Decisive: the guard's binding
    /// and its date.
    #[test]
    fn a_number_events_row_is_guarded_by_its_numbers_binding() {
        let ids = EventIdKey::from_app_secret(&AppSecret::new("app-secret-for-tests")).unwrap();
        let events = NumberEvents::new(
            Arc::new(Unused),
            Arc::new(crate::notify::LocalNotifier::default()),
            ids.clone(),
        );
        let row = events.row(NumberEvent::Connected, &pn(), &epoch()).unwrap();
        assert_eq!(row.tenant, Some(epoch().tenant_id));
        assert_eq!(row.event_type, "number_connected");
        assert_eq!(row.phone_number_id, Some(pn()));
        assert_eq!(row.waba_id, Some(epoch().waba_id));
        assert_eq!(row.dedup_window, None);
        assert_eq!(
            Some(row.id.clone()),
            ids.event_id(row.dedup_key.as_deref().unwrap())
        );
        let guard = row.route_guard.unwrap();
        assert_eq!(
            guard.binding,
            GuardedBinding::Number {
                phone_number_id: pn(),
                waba_id: epoch().waba_id,
            }
        );
        assert_eq!(
            guard.not_after,
            Some(epoch().attached_at + time::Duration::SECOND)
        );
        assert_eq!(guard.after_move, AfterMove::default());
        // Its own binding admits it, after a move too. A binding begun a
        // second later after a move (a tenant created again, bound again:
        // a deleted tenant counts as another) does not; without a previous
        // tenant (the same tenant's WABA bound again by itself), the event's
        // own second is the binding's, as for a first onboarding, and only
        // a binding begun two seconds later is refused.
        let moved = Some(epoch().attached_at - time::Duration::days(3));
        let later = |s: i64| epoch().attached_at + time::Duration::seconds(s);
        assert!(guard.admits(epoch().attached_at, moved));
        assert!(!guard.admits(later(1), moved));
        assert!(guard.admits(epoch().attached_at, None));
        assert!(guard.admits(later(1), None));
        assert!(!guard.admits(later(2), None));
    }
}
