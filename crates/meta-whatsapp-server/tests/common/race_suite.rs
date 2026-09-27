//! The port contracts that hold only when a backend makes two calls
//! atomic (roadmap S2), raced for real: each round starts two tasks on a
//! multi-threaded runtime, one of them a little late by a round-dependent
//! jitter, so that the rounds meet at different points. Run on memory
//! (`tests/store.rs`) and live on Postgres (`tests/live_postgres.rs`).
//!
//! A race test proves what it saw, not every interleaving: the decisive
//! tests of each lock are the deterministic ones (the insert in flight
//! holding off an unbinding, `live_postgres.rs`). These check that no
//! interleaving the rounds hit breaks an invariant, and that none fails.

use std::sync::Arc;
use std::time::Duration;

use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_server::model::{BindOutcome, DeleteTenantOutcome, TenantId};
use meta_whatsapp_server::store::events::{EventQuery, GuardedBinding, NewEvent, RouteGuard};
use meta_whatsapp_server::store::{Outbox as EventStore, Store};
use time::OffsetDateTime;

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

/// When racer `racer` (0 or 1) of `round` starts: in a third of the
/// rounds both start at once; in the others one of them (racer 0, then
/// racer 1) starts 1, 2, 4 or 8 ms late, so that in some it starts once
/// the other is done.
pub async fn jitter(round: usize, racer: usize) {
    let late = match round % 3 {
        0 => return,
        1 => 0,
        _ => 1,
    };
    if racer == late {
        // The delay mixed from the round, so that no variant of the
        // rounds (`round % 2`, `round / 2 % 2`) always draws the same one.
        tokio::time::sleep(Duration::from_millis(1 << ((round / 3 + round / 2) % 4))).await;
    }
}

/// The ids of every event of `tenant_id`'s stream.
async fn polled(outbox: &dyn EventStore, tenant_id: &str) -> Vec<String> {
    outbox
        .page(&EventQuery {
            tenant: tenant(tenant_id),
            after: None,
            types: None,
            phone_number_id: None,
            limit: 1000,
            max_bytes: 64 * 1024 * 1024,
        })
        .await
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.id)
        .collect()
}

/// How the rounds of [`an_insert_racing_a_move_never_reaches_the_new_holder`]
/// ended: the row kept by the old holder (the insert first), or no
/// tenant's (the move first, or the tenant deleted after it); and how
/// many rounds the two calls overlapped in (neither had returned when the
/// other started), where either ending is right.
#[derive(Debug, Default)]
pub struct MoveRaces {
    /// Rows the old holder kept.
    pub kept: usize,
    /// Rows no tenant polls.
    pub nobodys: usize,
    /// Rounds whose insert and move overlapped.
    pub overlapped: usize,
}

/// An outbox insert routed to a tenant, raced against its binding moving:
/// the WABA (and its number) unbound and bound to another tenant; or
/// unbound, its tenant deleted, created again under the same id and bound
/// again, in a later second than Meta dated the event. Whatever the
/// order, the insert succeeds and the new holder never polls the row; an
/// insert that started once the move had returned is no tenant's; one that
/// returned before the move started is the old holder's (until the
/// deletion, when there is one). Guarded by the number, then by the WABA,
/// round by round. Decisive (with the deterministic tests): the insert's
/// re-check, and on Postgres its locks.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
pub async fn an_insert_racing_a_move_never_reaches_the_new_holder(
    store: Arc<dyn Store>,
    outbox: Arc<dyn EventStore>,
    rounds: usize,
) -> MoveRaces {
    let run = super::unique();
    let other = format!("race-h-{run}");
    store.create_tenant(&tenant(&other), "").await.unwrap();
    let ids = |round: usize| {
        (
            format!("race-g-{run}-{round}"),
            WabaId::new(format!("race-w-{run}-{round}")),
            PhoneNumberId::new(format!("race-p-{run}-{round}")),
        )
    };
    // Every binding first, then one wait into the next second: a binding
    // made again during a round begins in a later second than the event,
    // which is dated in its first binding's second.
    let mut began = Vec::with_capacity(rounds);
    for round in 0..rounds {
        let (holder, waba, pn) = ids(round);
        store.create_tenant(&tenant(&holder), "").await.unwrap();
        assert_eq!(
            store
                .bind_waba(&tenant(&holder), &waba, std::slice::from_ref(&pn))
                .await
                .unwrap(),
            BindOutcome::Bound
        );
        began.push(store.waba(&waba).await.unwrap().unwrap().attached_at);
    }
    let latest = began.iter().max().copied().unwrap();
    let next_second = OffsetDateTime::from_unix_timestamp(latest.unix_timestamp() + 1).unwrap();
    let wait = next_second - OffsetDateTime::now_utc() + time::Duration::milliseconds(50);
    if wait.is_positive() {
        tokio::time::sleep(wait.unsigned_abs()).await;
    }

    let mut races = MoveRaces::default();
    for (round, began_at) in began.into_iter().enumerate() {
        let (holder, waba, pn) = ids(round);
        let recreated = round % 2 == 1;
        let binding = if (round / 2) % 2 == 0 {
            GuardedBinding::Number {
                phone_number_id: pn.clone(),
                waba_id: waba.clone(),
            }
        } else {
            GuardedBinding::Waba(waba.clone())
        };
        let event = NewEvent {
            id: format!("evt_race_{run}_{round}"),
            dedup_key: Some(format!("race-{run}-{round}")),
            dedup_window: None,
            tenant: Some(tenant(&holder)),
            route_guard: Some(RouteGuard {
                binding,
                not_after: Some(began_at),
            }),
            phone_number_id: Some(pn.clone()),
            waba_id: Some(waba.clone()),
            event_type: "message_received".to_owned(),
            data: "{}".to_owned(),
        };
        let inserted = {
            let outbox = outbox.clone();
            let event = event.clone();
            tokio::spawn(async move {
                jitter(round, 0).await;
                let started = std::time::Instant::now();
                let inserted = outbox.insert(&event).await;
                (started, inserted, std::time::Instant::now())
            })
        };
        let moved = {
            let store = store.clone();
            let (holder, other, waba, pn) =
                (holder.clone(), other.clone(), waba.clone(), pn.clone());
            tokio::spawn(async move {
                jitter(round, 1).await;
                let started = std::time::Instant::now();
                assert!(store.unbind_waba(&waba).await.unwrap());
                let to = if recreated {
                    assert_eq!(
                        store.delete_tenant(&tenant(&holder)).await.unwrap(),
                        DeleteTenantOutcome::Deleted
                    );
                    store
                        .create_tenant(&tenant(&holder), "")
                        .await
                        .unwrap()
                        .unwrap();
                    holder
                } else {
                    other
                };
                assert_eq!(
                    store
                        .bind_waba(&tenant(&to), &waba, std::slice::from_ref(&pn))
                        .await
                        .unwrap(),
                    BindOutcome::Bound
                );
                (started, std::time::Instant::now())
            })
        };
        let (inserted, moved) = tokio::join!(inserted, moved);
        let (move_started, move_ended) = moved.unwrap();
        let (insert_started, sequence, insert_ended) = inserted.unwrap();
        let what = if recreated { "created again" } else { "moved" };
        let sequence = sequence.unwrap_or_else(|e| panic!("round {round} ({what}): {e}"));
        assert!(sequence.is_some(), "round {round}: deduplicated");
        let new_holder = if recreated { &holder } else { &other };
        assert!(
            !polled(outbox.as_ref(), new_holder)
                .await
                .contains(&event.id),
            "round {round} ({what}): the new holder {new_holder} polls the old holder's event"
        );
        let kept = !recreated && polled(outbox.as_ref(), &holder).await.contains(&event.id);
        if insert_started > move_ended {
            assert!(
                !kept,
                "round {round} ({what}): an insert that began after the move kept its tenant"
            );
        } else if move_started > insert_ended {
            assert!(
                kept || recreated,
                "round {round} ({what}): an insert that ended before the move lost its tenant"
            );
        } else {
            races.overlapped += 1;
        }
        if kept {
            races.kept += 1;
        } else {
            races.nobodys += 1;
        }
    }
    races
}

/// How the rounds of [`a_binding_racing_a_deletion_serializes`] ended.
#[derive(Debug, Default)]
pub struct BindRaces {
    /// Bound, the deletion refused (`HasWabas`).
    pub bound: usize,
    /// Deleted, the binding refused (`NoSuchTenant`).
    pub deleted: usize,
}

/// `bind_waba` and `delete_tenant` of one tenant, raced on two tasks:
/// every round ends in one order or the other, both calls succeed, and
/// the records agree (a WABA and its number bound to a tenant that exists,
/// or neither and no tenant). Never an error, never a binding to a
/// deleted tenant. Decisive (on Postgres): the tenant's row locks in both
/// calls (`FOR KEY SHARE` and `FOR UPDATE`).
pub async fn a_binding_racing_a_deletion_serializes(
    store: Arc<dyn Store>,
    rounds: usize,
) -> BindRaces {
    let run = super::unique();
    let mut races = BindRaces::default();
    for round in 0..rounds {
        let id = tenant(&format!("bd-{run}-{round}"));
        let waba = WabaId::new(format!("bd-w-{run}-{round}"));
        let pn = PhoneNumberId::new(format!("bd-p-{run}-{round}"));
        store.create_tenant(&id, "").await.unwrap().unwrap();
        let bound = {
            let (store, id, waba, pn) = (store.clone(), id.clone(), waba.clone(), pn.clone());
            tokio::spawn(async move {
                jitter(round, 0).await;
                store.bind_waba(&id, &waba, &[pn]).await
            })
        };
        let deleted = {
            let (store, id) = (store.clone(), id.clone());
            tokio::spawn(async move {
                jitter(round, 1).await;
                store.delete_tenant(&id).await
            })
        };
        let (bound, deleted) = tokio::join!(bound, deleted);
        let bound = bound
            .unwrap()
            .unwrap_or_else(|e| panic!("round {round}: bind_waba failed: {e}"));
        let deleted = deleted
            .unwrap()
            .unwrap_or_else(|e| panic!("round {round}: delete_tenant failed: {e}"));
        let exists = store.tenant(&id).await.unwrap().is_some();
        let binding = store.waba(&waba).await.unwrap();
        let number = store.number(&pn).await.unwrap();
        match (bound, deleted) {
            (BindOutcome::Bound, DeleteTenantOutcome::HasWabas) => {
                assert!(exists, "round {round}: bound, and the tenant is gone");
                assert_eq!(binding.map(|b| b.tenant_id), Some(id.clone()));
                assert_eq!(number.map(|n| n.tenant_id), Some(id.clone()));
                races.bound += 1;
                assert!(store.unbind_waba(&waba).await.unwrap());
                assert_eq!(
                    store.delete_tenant(&id).await.unwrap(),
                    DeleteTenantOutcome::Deleted
                );
            }
            (BindOutcome::NoSuchTenant, DeleteTenantOutcome::Deleted) => {
                assert!(!exists, "round {round}");
                assert!(binding.is_none(), "round {round}: a deleted tenant's WABA");
                assert!(number.is_none(), "round {round}: a deleted tenant's number");
                races.deleted += 1;
            }
            other => panic!("round {round}: {other:?}"),
        }
    }
    races
}
