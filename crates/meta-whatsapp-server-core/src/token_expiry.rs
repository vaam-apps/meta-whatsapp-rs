//! The token-expiry event (roadmap M3e; `OPEN_QUESTIONS.md` #8; design D180 to
//! D183): ahead of a stored business token's lapse, the tenant that holds
//! its WABA is told, once, with `token_expiring` in its event outbox, and
//! a counter moves.
//!
//! | Type | Recorded when | `data` |
//! | --- | --- | --- |
//! | `token_expiring` | a round found the token stored for a bound WABA due to expire within the lead time (`WA_SERVER_TOKEN_EXPIRY_LEAD`, 7 days by default), and not yet expired | `{event, waba_id, expires_at}` |
//!
//! `expires_at` is RFC 3339, in UTC, to the second. **The data carries
//! ids and that time only**: no token, no part of one, no number, no
//! customer, nothing of Meta's text. The tenant answers by running
//! Embedded Signup again (the stored token is replaced; `409
//! reconnect_required` is what its calls answer once it lapses, as
//! before).
//!
//! **A round** ([`round`]) walks every bound WABA, as a vault rotation
//! does (the vault cannot list its records), and asks the library's
//! `TokenVault::token_expiry` for each (roadmap L11e): the record's clear
//! `expires_at`, so nothing is decrypted and nothing written. Every
//! [`TOKEN_EXPIRY_INTERVAL`] one replica runs it, under one
//! [`LeaderLock`](crate::store::LeaderLock) turn named
//! [`TOKEN_EXPIRY`]; the turn only spares the others the walk, **the
//! event does not depend on it**.
//!
//! **Once per token, per expiry, with no marker of its own.** The event's
//! outbox key ([`TokenExpiring::outbox_key`]) is its type, the WABA, its
//! tenant and the token's `expires_at`, and the outbox records a key once:
//! a second round, a second replica, a round that ran twice (a lease that
//! ended while it ran) record nothing more. A token stored again (the
//! merchant ran Embedded Signup again: a new `expires_at`) has a key of its
//! own, so it is told again when it nears its new expiry; a WABA that moved
//! to another tenant is told to its new holder (the tenant is in the key).
//! Because the key lives as long as the outbox row, the lead time must not
//! exceed the outbox's retention, or a purge would let a token still due
//! be told again: the service refuses a configuration where it does
//! ([`lead_fits_retention`]). Only a token still to expire is told: one
//! found already expired (the service was down, or the token was stored
//! expired) is not, since nothing after the lapse would keep the same
//! event from recurring once its row is purged.
//!
//! **Routed to the owning tenant only**, by the outbox's own check
//! ([`RouteGuard`]) over the binding the token was read under, as the
//! number events are ([`crate::number_events`]): a binding that moved
//! meanwhile gives the row to nobody (operator-only).

use std::future::Future;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::authz::Authorizer;
use crate::backend::Backend;
use crate::events::EventIdKey;
use crate::model::BindingEpoch;
use crate::observe::Observer;
use crate::outbox::{AfterMove, GuardedBinding, NewEvent, RouteGuard};
use crate::store::{TOKEN_EXPIRY, TOKEN_EXPIRY_LEASE};

/// The target of this module's log lines.
const LOG_TARGET: &str = "meta_whatsapp_server::token_expiry";

/// The event's type, in [`crate::events::TENANT_EVENT_TYPES`] too (its
/// last). No webhook of Meta's has it. Pending the owner's confirmation
/// before the first release.
pub const TOKEN_EXPIRING: &str = "token_expiring";

/// How often a round runs. A token is told when a round first meets it
/// within the lead time, so the tenant has the lead time less at most this
/// much.
pub const TOKEN_EXPIRY_INTERVAL: Duration = Duration::from_secs(600);

/// The default lead time (`WA_SERVER_TOKEN_EXPIRY_LEAD`): the library's
/// `DEFAULT_EXPIRY_LEAD`, 7 days (a business token generated to expire
/// lasts 60 days).
pub const DEFAULT_LEAD: Duration = meta_whatsapp_rs::client::embedded_signup::DEFAULT_EXPIRY_LEAD;

/// Whether `lead` fits the outbox's `retention`: no longer than it, so that
/// the row holding an event's key outlives the token's lapse (see the
/// [module](self)).
pub fn lead_fits_retention(lead: Duration, retention: Duration) -> bool {
    lead <= retention
}

/// What a walk of the bindings found: see `Authorizer::tokens_expiring`.
#[derive(Debug, Default)]
pub(crate) struct Walk {
    /// WABAs walked.
    pub(crate) wabas: usize,
    /// The tokens due, each under the binding it was read under.
    pub(crate) due: Vec<Due>,
    /// WABAs whose record could not be read.
    pub(crate) unreadable: usize,
}

/// A token due to expire.
#[derive(Debug)]
pub(crate) struct Due {
    pub(crate) epoch: BindingEpoch,
    pub(crate) expires_at: OffsetDateTime,
}

/// A stored token about to expire: what `token_expiring` is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenExpiring {
    /// The binding the token was read under: its WABA and tenant.
    pub epoch: BindingEpoch,
    /// When the token expires.
    pub expires_at: OffsetDateTime,
}

impl TokenExpiring {
    /// `expires_at` as the event states it: RFC 3339, UTC, to the second.
    fn expires_at_text(&self) -> String {
        self.expires_at
            .replace_nanosecond(0)
            .unwrap_or(self.expires_at)
            .to_offset(time::UtcOffset::UTC)
            .format(&Rfc3339)
            .unwrap_or_default()
    }

    /// Its `data`: the type (`event`, as every event's `data` is tagged),
    /// the WABA and the expiry. Ids and a time, never a token.
    pub fn data(&self) -> String {
        serde_json::json!({
            "event": TOKEN_EXPIRING,
            "waba_id": self.epoch.waba_id.as_str(),
            "expires_at": self.expires_at_text(),
        })
        .to_string()
    }

    /// The outbox's idempotency key: SHA-256 (hex) over a label of its
    /// own, the type, the WABA, its tenant and the expiry in Unix seconds,
    /// each followed by a zero byte. Stored (`wa_server_events.dedup_key`):
    /// pinned by a test. Not the binding's generation: the same token
    /// attached again to the same tenant is the same token, told once.
    pub fn outbox_key(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"meta-whatsapp-server/token-expiry-event/v1\0");
        for part in [
            TOKEN_EXPIRING,
            self.epoch.waba_id.as_str(),
            self.epoch.tenant_id.as_str(),
            &self.expires_at.unix_timestamp().to_string(),
        ] {
            hash.update(part.as_bytes());
            hash.update(b"\0");
        }
        hex::encode(hash.finalize())
    }

    /// The outbox row: for the binding's tenant, guarded by the WABA's
    /// binding, dated the second after that binding began (as the number
    /// events are). `None` when no id could be derived (it always can).
    pub fn row(&self, ids: &EventIdKey) -> Option<NewEvent> {
        let dedup_key = self.outbox_key();
        let id = ids.event_id(&dedup_key)?;
        Some(NewEvent {
            id,
            dedup_key: Some(dedup_key),
            dedup_window: None,
            tenant: Some(self.epoch.tenant_id.clone()),
            route_guard: Some(RouteGuard {
                binding: GuardedBinding::Waba(self.epoch.waba_id.clone()),
                not_after: Some(self.epoch.attached_at + time::Duration::SECOND),
                after_move: AfterMove::default(),
            }),
            phone_number_id: None,
            waba_id: Some(self.epoch.waba_id.clone()),
            event_type: TOKEN_EXPIRING.to_owned(),
            data: self.data(),
        })
    }
}

/// What a round did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Round {
    /// WABAs walked.
    pub wabas: usize,
    /// Tokens within the lead time.
    pub due: usize,
    /// Events recorded by this round (a token told by an earlier round, or
    /// another replica's, is not counted).
    pub recorded: usize,
    /// Records that could not be read, and events that could not be
    /// recorded (retried next round).
    pub failed: usize,
}

/// One round on `backend`, telling `lead` ahead, under one turn of its
/// [`LeaderLock`](crate::store::LeaderLock): `None` when another replica
/// has the turn, or the turn could not be taken (logged). Each event
/// recorded is told to `observer` (`recorded`), each failure too
/// (`failed`). A failure of the service's own storage ends the round, to
/// be retried at the next; what was recorded stays recorded, each once.
pub async fn round(
    authz: &Authorizer,
    backend: &dyn Backend,
    observer: &dyn Observer,
    lead: Duration,
) -> Option<Round> {
    let turn = match backend
        .leader_lock()
        .try_exclusive(TOKEN_EXPIRY, TOKEN_EXPIRY_LEASE)
        .await
    {
        Ok(Some(turn)) => turn,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(target: LOG_TARGET, error = %error, "taking the token-expiry turn failed");
            return None;
        }
    };
    let done = walk_and_record(authz, observer, lead).await;
    if let Err(error) = turn.release().await {
        tracing::warn!(target: LOG_TARGET, error = %error, "ending the token-expiry turn failed");
    }
    Some(done)
}

async fn walk_and_record(authz: &Authorizer, observer: &dyn Observer, lead: Duration) -> Round {
    let mut done = Round::default();
    let Some(events) = authz.number_events() else {
        // No recorder: the core's suites that build an `Authorizer` of
        // their own. The service always has one.
        return done;
    };
    let walk = match authz.tokens_expiring(lead).await {
        Ok(walk) => walk,
        Err(error) => {
            tracing::warn!(
                target: LOG_TARGET,
                kind = error.kind().as_str(),
                "walking the stored tokens failed"
            );
            observer.token_expiry("failed");
            done.failed += 1;
            return done;
        }
    };
    done.wabas = walk.wabas;
    done.due = walk.due.len();
    for _ in 0..walk.unreadable {
        observer.token_expiry("failed");
    }
    done.failed += walk.unreadable;
    for due in walk.due {
        let expiring = TokenExpiring {
            epoch: due.epoch,
            expires_at: due.expires_at,
        };
        let Some(row) = expiring.row(events.ids()) else {
            tracing::warn!(target: LOG_TARGET, "no id for a token-expiry event");
            continue;
        };
        match events.record_row(&row, &expiring.epoch.tenant_id).await {
            Ok(Some(_)) => {
                done.recorded += 1;
                observer.token_expiry("recorded");
            }
            // Told already.
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    error = %error,
                    "recording a token-expiry event failed (retried next round)"
                );
                observer.token_expiry("failed");
                done.failed += 1;
                break;
            }
        }
    }
    if done.recorded > 0 {
        tracing::info!(
            target: LOG_TARGET,
            recorded = done.recorded,
            "tokens near their expiry told to their tenants"
        );
    }
    done
}

/// Rounds on `backend`, one every `every` (the first at once) until `stop`
/// completes, telling `lead` ahead. A round already running when `stop`
/// completes finishes first. A zero `every` is taken as one millisecond.
pub async fn run(
    authz: Arc<Authorizer>,
    backend: Arc<dyn Backend>,
    observer: Arc<dyn Observer>,
    lead: Duration,
    every: Duration,
    stop: impl Future<Output = ()>,
) {
    let mut ticks = tokio::time::interval(every.max(Duration::from_millis(1)));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut stop = std::pin::pin!(stop);
    loop {
        let stopped = std::future::poll_fn(|cx| {
            if stop.as_mut().poll(cx).is_ready() {
                return Poll::Ready(true);
            }
            ticks.poll_tick(cx).map(|_| false)
        })
        .await;
        if stopped {
            return;
        }
        round(&authz, backend.as_ref(), observer.as_ref(), lead).await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a panic is the report
mod tests {
    use meta_whatsapp_rs::core::ids::WabaId;

    use super::*;
    use crate::model::TenantId;

    fn expiring() -> TokenExpiring {
        TokenExpiring {
            epoch: BindingEpoch {
                waba_id: WabaId::new("102290129340398").unwrap(),
                tenant_id: TenantId::parse("merchant-42").unwrap(),
                attached_at: time::macros::datetime!(2026-09-26 12:00:00.5 UTC),
                generation: 2,
            },
            expires_at: time::macros::datetime!(2026-11-05 12:00:00.75 UTC),
        }
    }

    /// The `data` is part of the v1 API: ids and the expiry, to the second,
    /// and nothing else. Decisive: each field's name and value.
    #[test]
    fn the_events_data_is_ids_and_the_expiry_only() {
        let data: serde_json::Value = serde_json::from_str(&expiring().data()).unwrap();
        assert_eq!(
            data,
            serde_json::json!({"event": "token_expiring", "waba_id": "102290129340398",
                "expires_at": "2026-11-05T12:00:00Z"})
        );
    }

    /// The key is stored and the id derived from it: computed otherwise
    /// after an upgrade, a token already told is told again. Known answer,
    /// computed apart from this code (the `w` of the label spelled `\x77`,
    /// so that a rename cannot rewrite it):
    //
    // python3 - <<'EOF'
    // import hashlib
    // parts = [b"token_expiring", b"102290129340398", b"merchant-42", b"1793880000"]
    // m = b"meta-\x77hatsapp-server/token-expiry-event/v1\x00" + b"".join(p + b"\x00" for p in parts)
    // print(hashlib.sha256(m).hexdigest())
    // EOF
    //
    // Decisive: the label, each separator, each part and its order; and
    // that the binding's generation and the sub-second part are not in it.
    #[test]
    fn the_key_is_pinned() {
        let base = expiring();
        assert_eq!(
            base.outbox_key(),
            "e2d2ef0029634a8e3df203230e64d770fac9181b0357256f4279bb2ed4bc4954"
        );
        let mut again = expiring();
        again.epoch.generation = 9;
        again.expires_at = time::macros::datetime!(2026-11-05 12:00:00.1 UTC);
        assert_eq!(base.outbox_key(), again.outbox_key());
        let mut refreshed = expiring();
        refreshed.expires_at += time::Duration::DAY;
        assert_ne!(base.outbox_key(), refreshed.outbox_key());
        let mut moved = expiring();
        moved.epoch.tenant_id = TenantId::parse("merchant-43").unwrap();
        assert_ne!(base.outbox_key(), moved.outbox_key());
    }

    /// The defaults fit each other, and a lead past the retention does not.
    #[test]
    fn the_lead_may_not_exceed_the_retention() {
        let day = Duration::from_hours(24);
        assert!(lead_fits_retention(day, day * 2));
        assert!(lead_fits_retention(day, day));
        assert!(!lead_fits_retention(day * 2, day));
        // One second past the retention is past it: the row holding the key
        // would be purged a second before the token lapses.
        assert!(!lead_fits_retention(day + Duration::from_secs(1), day));
        assert!(lead_fits_retention(
            DEFAULT_LEAD,
            crate::events::DEFAULT_OUTBOX_RETENTION
        ));
    }
}
