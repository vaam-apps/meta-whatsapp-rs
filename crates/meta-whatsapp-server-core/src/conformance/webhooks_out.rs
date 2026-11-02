//! Roadmap M2c: the bundle's [`WebhookStore`] and the core's webhooks-out
//! services and dispatcher over it, with a recording [`Sender`] and a
//! stub [`Resolver`] (no network). What it holds a backend to, besides
//! the port's own rules ([`crate::webhooks_out::store`]):
//!
//! - **An endpoint is its tenant's**, through every call; a tenant's
//!   limit is counted at the insert; deleting a tenant or an endpoint
//!   deletes its deliveries.
//! - **A fan-out is once**: a cursor moved meanwhile refuses it, two
//!   fan-outs racing record each event once, and two dispatchers (two
//!   replicas) racing send each event once.
//! - **A claim is exclusive** until its lease ends, one per tenant a
//!   claim, never a busy endpoint's or tenant's, a disabled endpoint's or
//!   a suspended tenant's; a settlement after another claim took the
//!   delivery is refused.
//! - **Acceptance test M2.4**, over the backend: the delivery signed the
//!   Standard Webhooks way, verified with the secret its creation showed;
//!   the retry schedule under a fake clock, its window ending in `failed`
//!   and the endpoint disabled, a manual retry delivering it; a
//!   destination refused at creation, and at delivery by a lookup that
//!   turned private on the second time (zero sends); two signatures during
//!   a rotation, one after its 24 hours; an event over 256 KiB delivered
//!   truncated and read whole by its tenant alone; each delivery its own
//!   tenant's event only.
//! - **Back-pressure**: an endpoint that never answers holds its own
//!   bound of attempts on a replica, and another endpoint's deliveries all
//!   go meanwhile.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{TokenVault, VaultKey, VaultKeys};
use meta_whatsapp_rs::core::clock::{Clock, ManualClock};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use time::OffsetDateTime;
use time::macros::datetime;

use super::outbox::{bind, row};
use crate::admin::mint;
use crate::authz::{Authorizer, Caller};
use crate::backend::Backend;
use crate::model::{DeleteTenantOutcome, KeyOwner, PageRequest, Scope, TenantId, TenantStatus};
use crate::outbox::NewEvent;
use crate::webhooks_out::destination::{DestinationPolicy, ResolveError, Resolver};
use crate::webhooks_out::dispatch::{
    DispatchSettings, Dispatcher, SendError, SendRequest, Sender, WebhooksOut,
};
use crate::webhooks_out::endpoints::{CreateEndpoint, WebhookEndpoints};
use crate::webhooks_out::schedule::{Jitter, RetrySchedule};
use crate::webhooks_out::seal::{DataKey, DataKeys};
use crate::webhooks_out::signature::{EndpointSecret, verify};
use crate::webhooks_out::store::{
    Attempt, ClaimRequest, ClaimToken, CreateOutcome, EndpointChange, NewDelivery, NewEndpoint,
    Outcome, WebhookStore,
};
use crate::webhooks_out::{
    AttemptError, DeliveryId, DeliveryStatus, DisabledReason, EndpointId, MAX_DELIVERED_DATA_BYTES,
    SealedSecret,
};

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    an_endpoint_is_its_tenants_own(backend).await;
    a_fan_out_moves_its_cursor_once(backend).await;
    a_claim_is_exclusive_until_its_lease_ends(backend).await;
    a_settlement_records_its_attempt(backend).await;
    deliveries_go_with_their_endpoint_and_tenant(backend).await;
    a_delivery_is_signed_and_carries_its_tenants_events_only(backend).await;
    every_tenant_event_type_is_delivered(backend).await;
    the_retry_schedule_under_a_fake_clock(backend).await;
    a_destination_is_refused_at_creation_and_at_delivery(backend).await;
    a_rotation_signs_twice_for_24_hours(backend).await;
    a_large_event_is_delivered_truncated(backend).await;
    two_dispatchers_send_each_event_once(backend).await;
    a_slow_endpoint_does_not_starve_another(backend).await;
}

/// The instant the fake clocks start at: whole seconds, which every
/// backend stores exactly.
const START: OffsetDateTime = datetime!(2026-11-02 10:00 UTC);

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

fn unique_tenant(prefix: &str) -> String {
    format!("{prefix}-{}", super::unique())
}

fn sealed(n: u8) -> SealedSecret {
    SealedSecret::from_stored(format!("v1.k1.sealed-{n}"))
}

fn new_endpoint(tenant_id: &str, cursor: i64) -> NewEndpoint {
    NewEndpoint {
        id: EndpointId::generate().unwrap(),
        tenant: tenant(tenant_id),
        url: "https://hooks.example.com/wa".to_owned(),
        types: vec!["message_received".to_owned()],
        secret: sealed(1),
        cursor,
        now: START,
    }
}

fn delivery(event: &str, sequence: i64, at: OffsetDateTime) -> NewDelivery {
    NewDelivery {
        id: DeliveryId::generate(at).unwrap(),
        event_id: Some(event.to_owned()),
        sequence: Some(sequence),
        event_type: "message_received".to_owned(),
        created_at: at,
        give_up_at: at + time::Duration::hours(72),
    }
}

fn claim(now: OffsetDateTime, limit: usize) -> ClaimRequest {
    ClaimRequest {
        token: ClaimToken(super::unique()),
        now,
        lease: Duration::from_secs(60),
        limit,
        busy_endpoints: Vec::new(),
        busy_tenants: Vec::new(),
    }
}

async fn created(
    store: &dyn WebhookStore,
    endpoint: &NewEndpoint,
) -> crate::webhooks_out::Endpoint {
    match store.create_endpoint(endpoint, 10).await.unwrap() {
        CreateOutcome::Created(made) => *made,
        other => panic!("not created: {other:?}"),
    }
}

/// Settle every delivery an earlier case left pending (as delivered), so
/// that a case's claims see its own deliveries only: the suite runs on one
/// backend.
async fn drain(store: &dyn WebhookStore) {
    let far = START + time::Duration::days(100_000);
    loop {
        let claimed = store.claim(&claim(far, 1000)).await.unwrap();
        if claimed.is_empty() {
            return;
        }
        for c in claimed {
            store
                .settle(
                    &Attempt {
                        delivery: c.delivery.id.clone(),
                        claim: c.claim.clone(),
                        outcome: Outcome::Delivered,
                        started_at: far,
                        duration_ms: 0,
                        status: Some(200),
                        error: None,
                        counted: false,
                        disable_if_failing_since: None,
                    },
                    far,
                )
                .await
                .unwrap();
        }
    }
}

/// `endpoint`'s one due delivery at `now`, claimed.
async fn take(
    store: &dyn WebhookStore,
    endpoint: &EndpointId,
    now: OffsetDateTime,
) -> crate::webhooks_out::store::Claimed {
    let mut claimed = store.claim(&claim(now, 1)).await.unwrap();
    claimed.retain(|c| &c.delivery.endpoint == endpoint);
    claimed.pop().expect("a due delivery")
}

/// The store's tenant rules: every call names the tenant, another's
/// endpoint answers as a missing one; the limit counted at the insert; a
/// missing tenant refused; a listing per tenant in byte order, paged.
pub async fn an_endpoint_is_its_tenants_own(backend: &dyn Backend) {
    let records = backend.records();
    let store = backend.webhooks();
    let (a, b) = (unique_tenant("wh-a"), unique_tenant("wh-b"));
    records.create_tenant(&tenant(&a), "").await.unwrap();
    records.create_tenant(&tenant(&b), "").await.unwrap();
    let mine = created(store.as_ref(), &new_endpoint(&a, 7)).await;
    assert_eq!(mine.tenant, tenant(&a));
    assert_eq!((mine.cursor, mine.enabled), (7, true));
    assert_eq!(mine.secret, sealed(1), "stored as given");
    let b_ = tenant(&b);
    assert_eq!(store.endpoint(&b_, &mine.id).await.unwrap(), None);
    assert!(
        store
            .endpoints(
                &b_,
                &PageRequest {
                    after: None,
                    limit: 10
                }
            )
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        store
            .update_endpoint(
                &b_,
                &mine.id,
                &EndpointChange {
                    enabled: Some(false),
                    ..EndpointChange::default()
                },
                START
            )
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .rotate_secret(&b_, &mine.id, &sealed(2), START, START)
            .await
            .unwrap(),
        None
    );
    assert!(!store.delete_endpoint(&b_, &mine.id).await.unwrap());
    assert_eq!(
        store
            .deliveries(
                &b_,
                &mine.id,
                &PageRequest {
                    after: None,
                    limit: 10
                }
            )
            .await
            .unwrap(),
        None
    );
    let ping = NewDelivery {
        event_id: None,
        sequence: None,
        event_type: "ping".to_owned(),
        ..delivery("x", 0, START)
    };
    assert_eq!(
        store.insert_delivery(&b_, &mine.id, &ping).await.unwrap(),
        None
    );
    assert!(
        store
            .endpoint(&tenant(&a), &mine.id)
            .await
            .unwrap()
            .unwrap()
            .enabled,
        "B changed nothing"
    );
    // The limit, per tenant, at the insert.
    let mut second = new_endpoint(&a, 0);
    assert!(matches!(
        store.create_endpoint(&second, 1).await.unwrap(),
        CreateOutcome::LimitReached
    ));
    second.tenant = tenant(&b);
    assert!(
        matches!(
            store.create_endpoint(&second, 1).await.unwrap(),
            CreateOutcome::Created(_)
        ),
        "B's own limit"
    );
    let nobody = new_endpoint(&unique_tenant("wh-nobody"), 0);
    assert_eq!(
        store.create_endpoint(&nobody, 10).await.unwrap(),
        CreateOutcome::NoSuchTenant
    );
    // A listing in byte order, paged.
    let mut ids = vec![mine.id.clone()];
    for _ in 0..3 {
        ids.push(created(store.as_ref(), &new_endpoint(&a, 0)).await.id);
    }
    ids.sort();
    let first = store
        .endpoints(
            &tenant(&a),
            &PageRequest {
                after: None,
                limit: 3,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        first.items.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
        ids[..3]
    );
    let rest = store
        .endpoints(
            &tenant(&a),
            &PageRequest {
                after: first.next_after.clone(),
                limit: 3,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        rest.items.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
        ids[3..]
    );
    assert_eq!(rest.next_after, None);
    // A change, a rotation.
    let changed = store
        .update_endpoint(
            &tenant(&a),
            &mine.id,
            &EndpointChange {
                url: Some("https://hooks.example.com/other".to_owned()),
                types: Some(vec!["status_updated".to_owned()]),
                enabled: Some(false),
            },
            START + time::Duration::seconds(1),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            changed.url.as_str(),
            changed.types.as_slice(),
            changed.enabled
        ),
        (
            "https://hooks.example.com/other",
            &["status_updated".to_owned()][..],
            false
        )
    );
    assert_eq!(changed.updated_at, START + time::Duration::seconds(1));
    let until = START + time::Duration::hours(24);
    let rotated = store
        .rotate_secret(&tenant(&a), &mine.id, &sealed(2), until, START)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            rotated.secret.clone(),
            rotated.previous_secret.clone(),
            rotated.previous_secret_until
        ),
        (sealed(2), Some(sealed(1)), Some(until))
    );
    let again = store
        .rotate_secret(&tenant(&a), &mine.id, &sealed(3), until, START)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (again.secret, again.previous_secret),
        (sealed(3), Some(sealed(2))),
        "the oldest dropped"
    );
    assert!(
        !store
            .active_endpoints()
            .await
            .unwrap()
            .iter()
            .any(|e| e.id == mine.id),
        "disabled: not active"
    );
}

/// A fan-out records its deliveries and moves the cursor only from the
/// value it read; an event recorded twice for one endpoint is one
/// delivery; two fan-outs racing from one cursor record it once.
pub async fn a_fan_out_moves_its_cursor_once(backend: &dyn Backend) {
    let records = backend.records();
    let store = backend.webhooks();
    let a = unique_tenant("wh-fan");
    records.create_tenant(&tenant(&a), "").await.unwrap();
    let endpoint = created(store.as_ref(), &new_endpoint(&a, 0)).await;
    let page = PageRequest {
        after: None,
        limit: 100,
    };
    assert!(
        !store
            .fan_out(&endpoint.id, 5, 6, &[delivery("evt_1", 1, START)])
            .await
            .unwrap(),
        "not its cursor"
    );
    assert!(
        store
            .deliveries(&tenant(&a), &endpoint.id, &page)
            .await
            .unwrap()
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        store
            .fan_out(
                &endpoint.id,
                0,
                2,
                &[delivery("evt_1", 1, START), delivery("evt_2", 2, START)]
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .fan_out(&endpoint.id, 2, 3, &[delivery("evt_2", 2, START)])
            .await
            .unwrap(),
        "a duplicate is skipped, the cursor moves"
    );
    let listed = store
        .deliveries(&tenant(&a), &endpoint.id, &page)
        .await
        .unwrap()
        .unwrap()
        .items;
    assert_eq!(listed.len(), 2);
    assert!(
        listed.iter().all(|d| d.tenant == tenant(&a)
            && d.status == DeliveryStatus::Pending
            && d.attempts == 0)
    );
    assert_eq!(
        store
            .endpoint(&tenant(&a), &endpoint.id)
            .await
            .unwrap()
            .unwrap()
            .cursor,
        3
    );
    // Two racing from one cursor: one wins.
    let (x, y) = ([delivery("evt_3", 3, START)], [delivery("evt_3", 3, START)]);
    let (one, two) = tokio::join!(
        store.fan_out(&endpoint.id, 3, 4, &x),
        store.fan_out(&endpoint.id, 3, 4, &y),
    );
    assert_eq!(
        u8::from(one.unwrap()) + u8::from(two.unwrap()),
        1,
        "exactly one fan-out moved the cursor"
    );
    let listed = store
        .deliveries(&tenant(&a), &endpoint.id, &page)
        .await
        .unwrap()
        .unwrap()
        .items;
    assert_eq!(
        listed
            .iter()
            .filter(|d| d.event_id.as_deref() == Some("evt_3"))
            .count(),
        1
    );
    // Newest first.
    let ids: Vec<_> = listed.iter().map(|d| d.id.clone()).collect();
    let mut sorted = ids.clone();
    sorted.sort_by(|x, y| y.cmp(x));
    assert_eq!(ids, sorted);
}

/// A claim takes each due delivery once until its lease ends, one per
/// tenant a claim, oldest due first; never a busy endpoint's or tenant's,
/// a disabled endpoint's, a suspended tenant's, nor one not due yet. A
/// settlement under a claim another one replaced is refused.
#[allow(clippy::many_single_char_names)] // the case's tenants and their endpoints
pub async fn a_claim_is_exclusive_until_its_lease_ends(backend: &dyn Backend) {
    let records = backend.records();
    let store = backend.webhooks();
    drain(store.as_ref()).await;
    let at = START + time::Duration::days(400);
    let (a, b) = (unique_tenant("wh-claim-a"), unique_tenant("wh-claim-b"));
    for t in [&a, &b] {
        records.create_tenant(&tenant(t), "").await.unwrap();
    }
    let ea1 = created(store.as_ref(), &new_endpoint(&a, 0)).await;
    let ea2 = created(store.as_ref(), &new_endpoint(&a, 0)).await;
    let eb = created(store.as_ref(), &new_endpoint(&b, 0)).await;
    let early = at - time::Duration::seconds(5);
    assert!(
        store
            .fan_out(
                &ea1.id,
                0,
                2,
                &[delivery("evt_a1", 1, early), delivery("evt_a2", 2, at)]
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .fan_out(
                &ea2.id,
                0,
                1,
                &[delivery("evt_a3", 3, early - time::Duration::seconds(1))]
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .fan_out(&eb.id, 0, 1, &[delivery("evt_b1", 1, at)])
            .await
            .unwrap()
    );
    // Not due before its time.
    let none = store
        .claim(&claim(early - time::Duration::seconds(2), 10))
        .await
        .unwrap();
    assert!(none.is_empty(), "{none:?}");
    // One per tenant: A's oldest (evt_a3, of its second endpoint), B's.
    let first = store.claim(&claim(at, 10)).await.unwrap();
    let mut events: Vec<_> = first
        .iter()
        .map(|c| c.delivery.event_id.clone().unwrap())
        .collect();
    events.sort();
    assert_eq!(events, ["evt_a3", "evt_b1"]);
    assert!(first.iter().all(|c| c.endpoint.id == c.delivery.endpoint && c.endpoint.tenant == c.delivery.tenant));
    // A second claimer at once: A's next (evt_a1), never one held.
    let second = store.claim(&claim(at, 10)).await.unwrap();
    assert_eq!(
        second
            .iter()
            .map(|c| c.delivery.event_id.clone().unwrap())
            .collect::<Vec<_>>(),
        ["evt_a1"]
    );
    // Busy endpoints and tenants are skipped; the limit holds.
    let mut busy = claim(at, 10);
    busy.busy_endpoints = vec![ea1.id.clone()];
    assert!(
        store.claim(&busy).await.unwrap().is_empty(),
        "evt_a2 is ea1's"
    );
    let mut busy = claim(at, 10);
    busy.busy_tenants = vec![tenant(&a)];
    assert!(store.claim(&busy).await.unwrap().is_empty());
    // A suspended tenant's and a disabled endpoint's are not claimed.
    records
        .update_tenant(&tenant(&a), None, Some(TenantStatus::Suspended))
        .await
        .unwrap();
    assert!(
        store.claim(&claim(at, 10)).await.unwrap().is_empty(),
        "suspended"
    );
    records
        .update_tenant(&tenant(&a), None, Some(TenantStatus::Active))
        .await
        .unwrap();
    store
        .update_endpoint(
            &tenant(&a),
            &ea1.id,
            &EndpointChange {
                enabled: Some(false),
                ..EndpointChange::default()
            },
            at,
        )
        .await
        .unwrap();
    assert!(
        store.claim(&claim(at, 10)).await.unwrap().is_empty(),
        "disabled"
    );
    store
        .update_endpoint(
            &tenant(&a),
            &ea1.id,
            &EndpointChange {
                enabled: Some(true),
                ..EndpointChange::default()
            },
            at,
        )
        .await
        .unwrap();
    let third = store.claim(&claim(at, 1)).await.unwrap();
    assert_eq!(
        third
            .iter()
            .map(|c| c.delivery.event_id.clone().unwrap())
            .collect::<Vec<_>>(),
        ["evt_a2"]
    );
    // The lease ends: claimed again, and the first claim's settlement is
    // refused.
    let later = at + time::Duration::seconds(61);
    let again = store.claim(&claim(later, 10)).await.unwrap();
    assert_eq!(again.len(), 2, "one per tenant again: {again:?}");
    let stale = first
        .iter()
        .find(|c| c.delivery.event_id.as_deref() == Some("evt_b1"))
        .unwrap();
    let attempt = |c: &crate::webhooks_out::store::Claimed, claim: ClaimToken| Attempt {
        delivery: c.delivery.id.clone(),
        claim,
        outcome: Outcome::Delivered,
        started_at: later,
        duration_ms: 5,
        status: Some(200),
        error: None,
        counted: true,
        disable_if_failing_since: None,
    };
    assert!(
        !store
            .settle(&attempt(stale, stale.claim.clone()), later)
            .await
            .unwrap(),
        "a replaced claim"
    );
    let fresh = again
        .iter()
        .find(|c| c.delivery.event_id.as_deref() == Some("evt_b1"))
        .unwrap();
    assert!(
        store
            .settle(&attempt(fresh, fresh.claim.clone()), later)
            .await
            .unwrap()
    );
    assert!(
        !store
            .settle(&attempt(fresh, fresh.claim.clone()), later)
            .await
            .unwrap(),
        "settled once"
    );
    // Racing claimers never share a delivery.
    let c = unique_tenant("wh-claim-c");
    records.create_tenant(&tenant(&c), "").await.unwrap();
    let mut ids = Vec::new();
    for _ in 0..6 {
        let e = created(store.as_ref(), &new_endpoint(&c, 0)).await;
        let n = super::unique();
        store
            .fan_out(&e.id, 0, 1, &[delivery(&format!("evt_{n}"), 1, at)])
            .await
            .unwrap();
        ids.push(e.id);
    }
    let far = at + time::Duration::days(1);
    let mut taken = Vec::new();
    for _ in 0..6 {
        let (cx, cy) = (claim(far, 10), claim(far, 10));
        let (x, y) = tokio::join!(store.claim(&cx), store.claim(&cy));
        for claimed in x.unwrap().into_iter().chain(y.unwrap()) {
            if claimed.delivery.tenant == tenant(&c) {
                taken.push(claimed.delivery.id);
            }
        }
    }
    let count = taken.len();
    taken.sort();
    taken.dedup();
    assert_eq!(
        (count, taken.len()),
        (6, 6),
        "each of C's six deliveries claimed once"
    );
}

/// A settlement: a retry counts the attempt and keeps it pending, due
/// at its time; a delivery ends the failure streak; a failure past the
/// window disables a failing endpoint (reason `failing`); an attempt that
/// never left (its event gone) counts nothing.
pub async fn a_settlement_records_its_attempt(backend: &dyn Backend) {
    let records = backend.records();
    let store = backend.webhooks();
    drain(store.as_ref()).await;
    let a = unique_tenant("wh-settle");
    records.create_tenant(&tenant(&a), "").await.unwrap();
    let endpoint = created(store.as_ref(), &new_endpoint(&a, 0)).await;
    let at = START + time::Duration::days(800);
    store
        .fan_out(
            &endpoint.id,
            0,
            3,
            &[
                delivery("evt_s1", 1, at),
                delivery("evt_s2", 2, at),
                delivery("evt_s3", 3, at),
            ],
        )
        .await
        .unwrap();
    let first = take(store.as_ref(), &endpoint.id, at).await;
    let next = at + time::Duration::seconds(10);
    assert!(
        store
            .settle(
                &Attempt {
                    delivery: first.delivery.id.clone(),
                    claim: first.claim.clone(),
                    outcome: Outcome::Retry(next),
                    started_at: at,
                    duration_ms: 12,
                    status: Some(503),
                    error: Some(AttemptError::Status),
                    counted: true,
                    disable_if_failing_since: None,
                },
                at,
            )
            .await
            .unwrap()
    );
    let page = PageRequest {
        after: None,
        limit: 10,
    };
    let listed = store
        .deliveries(&tenant(&a), &endpoint.id, &page)
        .await
        .unwrap()
        .unwrap()
        .items;
    let retried = listed.iter().find(|d| d.id == first.delivery.id).unwrap();
    assert_eq!(
        (
            retried.status,
            retried.attempts,
            retried.next_attempt_at,
            retried.last_status,
            retried.last_error.clone(),
            retried.last_duration_ms,
            retried.last_attempt_at
        ),
        (
            DeliveryStatus::Pending,
            1,
            Some(next),
            Some(503),
            Some(AttemptError::Status),
            Some(12),
            Some(at)
        )
    );
    let failing = store
        .endpoint(&tenant(&a), &endpoint.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failing.failing_since, Some(at));
    // A delivery ends the streak.
    let second = take(store.as_ref(), &endpoint.id, at).await;
    assert_ne!(
        second.delivery.id, first.delivery.id,
        "the retried one is not due"
    );
    store
        .settle(
            &Attempt {
                delivery: second.delivery.id.clone(),
                claim: second.claim.clone(),
                outcome: Outcome::Delivered,
                started_at: at,
                duration_ms: 3,
                status: Some(204),
                error: None,
                counted: true,
                disable_if_failing_since: None,
            },
            at,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .endpoint(&tenant(&a), &endpoint.id)
            .await
            .unwrap()
            .unwrap()
            .failing_since,
        None
    );
    // An attempt that never left counts nothing.
    let third = take(store.as_ref(), &endpoint.id, at).await;
    store
        .settle(
            &Attempt {
                delivery: third.delivery.id.clone(),
                claim: third.claim.clone(),
                outcome: Outcome::Failed,
                started_at: at,
                duration_ms: 0,
                status: None,
                error: Some(AttemptError::EventGone),
                counted: false,
                disable_if_failing_since: Some(at),
            },
            at,
        )
        .await
        .unwrap();
    let gone = store
        .deliveries(&tenant(&a), &endpoint.id, &page)
        .await
        .unwrap()
        .unwrap()
        .items
        .into_iter()
        .find(|d| d.id == third.delivery.id)
        .unwrap();
    assert_eq!(
        (gone.status, gone.attempts, gone.last_error),
        (DeliveryStatus::Failed, 0, Some(AttemptError::EventGone))
    );
    assert!(
        store
            .endpoint(&tenant(&a), &endpoint.id)
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
    // A failure past the window, failing since before its cutoff: off.
    let late = next + time::Duration::hours(80);
    let retry = take(store.as_ref(), &endpoint.id, late).await;
    assert_eq!(retry.delivery.id, first.delivery.id);
    store
        .settle(
            &Attempt {
                delivery: retry.delivery.id.clone(),
                claim: retry.claim.clone(),
                outcome: Outcome::Retry(late),
                started_at: late - time::Duration::hours(73),
                duration_ms: 1,
                status: None,
                error: Some(AttemptError::Timeout),
                counted: true,
                disable_if_failing_since: None,
            },
            late,
        )
        .await
        .unwrap();
    let retry = take(store.as_ref(), &endpoint.id, late).await;
    store
        .settle(
            &Attempt {
                delivery: retry.delivery.id.clone(),
                claim: retry.claim.clone(),
                outcome: Outcome::Failed,
                started_at: late,
                duration_ms: 1,
                status: None,
                error: Some(AttemptError::Connect),
                counted: true,
                disable_if_failing_since: Some(late - time::Duration::hours(72)),
            },
            late,
        )
        .await
        .unwrap();
    let off = store
        .endpoint(&tenant(&a), &endpoint.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (off.enabled, off.disabled_reason),
        (false, Some(DisabledReason::Failing))
    );
    // Turned on again: the reason and the streak cleared.
    let on = store
        .update_endpoint(
            &tenant(&a),
            &endpoint.id,
            &EndpointChange {
                enabled: Some(true),
                ..EndpointChange::default()
            },
            late,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (on.enabled, on.disabled_reason, on.failing_since),
        (true, None, None)
    );
    // A failed one retried by hand: pending, due now, a new window; a
    // pending one is left as it is.
    let window_end = late + time::Duration::hours(72);
    let manual = store
        .retry_delivery(
            &tenant(&a),
            &endpoint.id,
            &retry.delivery.id,
            late,
            window_end,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            manual.status,
            manual.next_attempt_at,
            manual.give_up_at,
            manual.attempts
        ),
        (DeliveryStatus::Pending, Some(late), window_end, 3)
    );
    let untouched = store
        .retry_delivery(
            &tenant(&a),
            &endpoint.id,
            &retry.delivery.id,
            late + time::Duration::hours(1),
            window_end + time::Duration::hours(1),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        untouched.next_attempt_at,
        Some(late),
        "a pending one is left"
    );
    assert_eq!(
        store
            .retry_delivery(
                &tenant(&a),
                &EndpointId::generate().unwrap(),
                &retry.delivery.id,
                late,
                window_end
            )
            .await
            .unwrap(),
        None,
        "another endpoint's id"
    );
}

/// Deleting an endpoint deletes its deliveries; deleting a tenant its
/// endpoints and theirs; the purge takes settled deliveries before its
/// cutoff only.
pub async fn deliveries_go_with_their_endpoint_and_tenant(backend: &dyn Backend) {
    let records = backend.records();
    let store = backend.webhooks();
    drain(store.as_ref()).await;
    let a = unique_tenant("wh-gone");
    records.create_tenant(&tenant(&a), "").await.unwrap();
    let kept = created(store.as_ref(), &new_endpoint(&a, 0)).await;
    let dropped = created(store.as_ref(), &new_endpoint(&a, 0)).await;
    let at = START + time::Duration::days(1200);
    store
        .fan_out(
            &kept.id,
            0,
            2,
            &[delivery("evt_k1", 1, at), delivery("evt_k2", 2, at)],
        )
        .await
        .unwrap();
    store
        .fan_out(&dropped.id, 0, 1, &[delivery("evt_d1", 1, at)])
        .await
        .unwrap();
    assert!(
        store
            .delete_endpoint(&tenant(&a), &dropped.id)
            .await
            .unwrap()
    );
    assert_eq!(
        store.endpoint(&tenant(&a), &dropped.id).await.unwrap(),
        None
    );
    assert!(
        !store
            .delete_endpoint(&tenant(&a), &dropped.id)
            .await
            .unwrap()
    );
    // Settle one, then purge: only the settled one before the cutoff goes.
    let c = &take(store.as_ref(), &kept.id, at).await;
    store
        .settle(
            &Attempt {
                delivery: c.delivery.id.clone(),
                claim: c.claim.clone(),
                outcome: Outcome::Delivered,
                started_at: at,
                duration_ms: 1,
                status: Some(200),
                error: None,
                counted: true,
                disable_if_failing_since: None,
            },
            at,
        )
        .await
        .unwrap();
    // Earlier cases' settled deliveries go; none of this case's.
    store.purge_deliveries(at).await.unwrap();
    let page = PageRequest {
        after: None,
        limit: 10,
    };
    assert_eq!(
        store
            .deliveries(&tenant(&a), &kept.id, &page)
            .await
            .unwrap()
            .unwrap()
            .items
            .len(),
        2,
        "nothing made before `at`"
    );
    assert!(
        store
            .purge_deliveries(at + time::Duration::seconds(1))
            .await
            .unwrap()
            >= 1
    );
    let left = store
        .deliveries(&tenant(&a), &kept.id, &page)
        .await
        .unwrap()
        .unwrap()
        .items;
    assert_eq!(left.len(), 1);
    assert_eq!(
        left[0].status,
        DeliveryStatus::Pending,
        "a pending one is never purged"
    );
    // The tenant's deletion takes the rest.
    assert_eq!(
        records.delete_tenant(&tenant(&a)).await.unwrap(),
        DeleteTenantOutcome::Deleted
    );
    records.create_tenant(&tenant(&a), "").await.unwrap();
    assert_eq!(
        store.endpoint(&tenant(&a), &kept.id).await.unwrap(),
        None,
        "gone with its tenant"
    );
    assert!(
        !store
            .active_endpoints()
            .await
            .unwrap()
            .iter()
            .any(|e| e.id == kept.id)
    );
    let all = claim(at + time::Duration::days(10), 1000);
    assert!(
        store
            .claim(&all)
            .await
            .unwrap()
            .iter()
            .all(|c| c.delivery.tenant != tenant(&a))
    );
}

// ─── The dispatcher, over the backend ────────────────────────────────────

/// What a [`Sender`] got, and what it answers: `status` (200 unless set),
/// or, for a host in `hold`, nothing until `release` (a receiver that
/// never answers).
#[derive(Default)]
pub struct Recorder {
    sent: Mutex<Vec<SendRequest>>,
    status: AtomicU16,
    hold: Mutex<Vec<String>>,
    held: AtomicUsize,
    release: tokio::sync::Notify,
}

impl Recorder {
    fn new() -> Arc<Self> {
        let recorder = Self::default();
        recorder.status.store(200, Ordering::SeqCst);
        Arc::new(recorder)
    }

    fn sent(&self) -> Vec<SendRequest> {
        self.sent.lock().unwrap().clone()
    }

    fn sent_to(&self, host: &str) -> Vec<SendRequest> {
        self.sent()
            .into_iter()
            .filter(|r| r.destination.authority == host)
            .collect()
    }
}

#[async_trait]
impl Sender for Recorder {
    async fn send(&self, request: SendRequest) -> Result<u16, SendError> {
        let held = self
            .hold
            .lock()
            .unwrap()
            .contains(&request.destination.authority);
        self.sent.lock().unwrap().push(request);
        if held {
            self.held.fetch_add(1, Ordering::SeqCst);
            self.release.notified().await;
            self.held.fetch_sub(1, Ordering::SeqCst);
            return Err(SendError::Timeout);
        }
        Ok(self.status.load(Ordering::SeqCst))
    }
}

/// A resolver of fixed answers per host; for a host with several, one
/// per lookup in turn, the last one kept (a host rebinding).
#[derive(Default)]
pub struct Stub {
    answers: Mutex<HashMap<String, Vec<Vec<IpAddr>>>>,
    lookups: AtomicUsize,
}

impl Stub {
    fn answer(&self, host: &str, answers: &[&[&str]]) {
        self.answers.lock().unwrap().insert(
            host.to_owned(),
            answers
                .iter()
                .map(|a| a.iter().map(|ip| ip.parse().unwrap()).collect())
                .collect(),
        );
    }
}

#[async_trait]
impl Resolver for Stub {
    async fn resolve(&self, host: &str, _: u16) -> Result<Vec<IpAddr>, ResolveError> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        let mut answers = self.answers.lock().unwrap();
        let list = answers.get_mut(host).ok_or(ResolveError)?;
        if list.len() > 1 {
            Ok(list.remove(0))
        } else {
            list.first().cloned().ok_or(ResolveError)
        }
    }
}

/// The jitter's draw, fixed: 0.5 is none.
struct Fixed(f64);

impl Jitter for Fixed {
    fn unit(&self) -> f64 {
        self.0
    }
}

/// A test deployment: a data key, the allow-list `hooks.example.com` and
/// `*.example.net`, the stub, the recorder, no jitter, a short poll.
struct Rig {
    authz: Authorizer,
    endpoints: WebhookEndpoints,
    dispatcher: Dispatcher,
    clock: ManualClock,
    sender: Arc<Recorder>,
    resolver: Arc<Stub>,
    out: WebhooksOut,
}

fn rig(backend: &dyn Backend, schedule: RetrySchedule, dispatch: DispatchSettings) -> Rig {
    let sender = Recorder::new();
    let resolver = Arc::new(Stub::default());
    for host in [
        "hooks.example.com",
        "a.example.net",
        "b.example.net",
        "slow.example.net",
        "fast.example.net",
    ] {
        resolver.answer(host, &[&["93.184.216.34"]]);
    }
    let out = WebhooksOut {
        keys: Arc::new(DataKeys::new(DataKey::generate("k1").unwrap())),
        policy: DestinationPolicy::parse("hooks.example.com, *.example.net", false).unwrap(),
        resolver: resolver.clone(),
        sender: sender.clone(),
        schedule,
        jitter: Arc::new(Fixed(0.5)),
        dispatch,
    };
    let clock = ManualClock::new(START);
    let vault = TokenVault::new(
        backend.kv(),
        VaultKeys::new(VaultKey::generate("test").unwrap()),
    )
    .unwrap();
    let client = Client::builder()
        .transport(ScriptedTransport::new())
        .build()
        .unwrap();
    Rig {
        authz: Authorizer::new(backend.records(), vault, client).unwrap(),
        endpoints: WebhookEndpoints::new(backend, Arc::new(clock.clone()), Some(out.clone())),
        dispatcher: Dispatcher::new(backend, Arc::new(clock.clone()), out.clone()),
        clock,
        sender,
        resolver,
        out,
    }
}

fn fast_settings() -> DispatchSettings {
    DispatchSettings {
        poll: Duration::from_millis(10),
        ..DispatchSettings::default()
    }
}

/// A caller of `tenant` (created) with scope `webhooks`, admitted by
/// `authz`.
async fn caller(backend: &dyn Backend, authz: &Authorizer, tenant_id: &str) -> Caller {
    let _ = backend
        .records()
        .create_tenant(&tenant(tenant_id), "")
        .await
        .unwrap();
    let key = mint(
        backend.records().as_ref(),
        KeyOwner::Tenant(tenant(tenant_id)),
        vec![Scope::Webhooks],
        String::new(),
        None,
    )
    .await
    .unwrap()
    .0;
    authz
        .tenant_caller(
            Some(&format!("Bearer {}", key.expose_key())),
            None,
            Scope::Webhooks,
        )
        .await
        .unwrap()
}

/// A message for `tenant_id` on a number of its own (bound first), in the
/// outbox.
async fn event_for(backend: &dyn Backend, tenant_id: &str, pn: &str) -> NewEvent {
    bind(backend.records().as_ref(), tenant_id, pn).await;
    let event = row(
        Some(tenant_id),
        "message_received",
        pn,
        Some(&super::unique()),
    );
    backend.outbox().insert(&event).await.unwrap().unwrap();
    event
}

/// One fan-out and one claim round, settled.
async fn round(rig: &Rig) -> usize {
    rig.dispatcher.fan_out().await.unwrap();
    rig.dispatcher.dispatch_due().await
}

fn create(url: &str, types: &[&str]) -> CreateEndpoint {
    CreateEndpoint {
        url: url.to_owned(),
        types: types.iter().map(|t| (*t).to_owned()).collect(),
    }
}

/// M2.4, the delivery itself: each endpoint gets its own tenant's events
/// only, of its types, from its creation on (not the event before it, not
/// another tenant's nor an operator-only row), `POST`ed to the address the
/// lookup answered; `webhook-id` is the event's id, the body its envelope,
/// the signature verifies with the secret the creation showed; a delivered
/// one is not sent again; a ping goes once.
pub async fn a_delivery_is_signed_and_carries_its_tenants_events_only(backend: &dyn Backend) {
    drain(backend.webhooks().as_ref()).await;
    let rig = rig(backend, RetrySchedule::default(), fast_settings());
    let (a, b) = (unique_tenant("wh-sign-a"), unique_tenant("wh-sign-b"));
    let (pa, pb) = (super::unique_digits(), super::unique_digits());
    let ca = caller(backend, &rig.authz, &a).await;
    let cb = caller(backend, &rig.authz, &b).await;
    let before = event_for(backend, &a, &pa).await;
    let (ea, secret_a) = rig
        .endpoints
        .open(&rig.authz, &ca)
        .unwrap()
        .create(create(
            "https://a.example.net/hook?t=1",
            &["message_received"],
        ))
        .await
        .unwrap();
    let (eb, secret_b) = rig
        .endpoints
        .open(&rig.authz, &cb)
        .unwrap()
        .create(create("https://b.example.net/hook", &["message_received"]))
        .await
        .unwrap();
    assert!(secret_a.expose().starts_with("whsec_"));
    assert_ne!(ea.secret.as_stored(), secret_a.expose(), "stored sealed");
    let a1 = event_for(backend, &a, &pa).await;
    let b1 = event_for(backend, &b, &pb).await;
    let mut status_only = row(
        Some(a.as_str()),
        "status_updated",
        &pa,
        Some(&super::unique()),
    );
    "{\"event\":\"status_updated\"}".clone_into(&mut status_only.data);
    backend
        .outbox()
        .insert(&status_only)
        .await
        .unwrap()
        .unwrap();
    let operator = row(None, "message_received", &pa, Some(&super::unique()));
    backend.outbox().insert(&operator).await.unwrap().unwrap();
    let a2 = event_for(backend, &a, &pa).await;
    assert_eq!(
        round(&rig).await,
        3,
        "one per tenant a claim, claims until none is due"
    );
    assert_eq!(round(&rig).await, 0);
    let to_a = rig.sender.sent_to("a.example.net");
    let to_b = rig.sender.sent_to("b.example.net");
    let ids = |sent: &[SendRequest]| {
        sent.iter()
            .map(|r| r.header("webhook-id").unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let mut got_a = ids(&to_a);
    got_a.sort();
    let mut want_a = vec![a1.id.clone(), a2.id.clone()];
    want_a.sort();
    assert_eq!(
        got_a, want_a,
        "A's events after its creation, of its type, nothing else"
    );
    assert!(!got_a.contains(&before.id));
    assert_eq!(ids(&to_b), std::slice::from_ref(&b1.id));
    for (sent, secret, tenant_id, endpoint) in
        [(&to_a, &secret_a, &a, &ea), (&to_b, &secret_b, &b, &eb)]
    {
        for request in sent {
            assert_eq!(
                request.address,
                "93.184.216.34:443".parse().unwrap(),
                "the lookup's address, pinned"
            );
            let id = request.header("webhook-id").unwrap();
            let ts = request.header("webhook-timestamp").unwrap();
            assert_eq!(
                ts,
                START.unix_timestamp().to_string(),
                "the attempt's second, on the service's clock"
            );
            let signatures = request.header("webhook-signature").unwrap();
            assert_eq!(signatures.split(' ').count(), 1);
            verify(
                secret,
                id,
                ts,
                signatures,
                &request.body,
                START.unix_timestamp(),
            )
            .unwrap();
            assert!(
                verify(
                    &EndpointSecret::generate().unwrap(),
                    id,
                    ts,
                    signatures,
                    &request.body,
                    START.unix_timestamp()
                )
                .is_err()
            );
            assert_eq!(request.header("content-type"), Some("application/json"));
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["id"], id);
            assert_eq!(body["tenant_id"], tenant_id.as_str());
            assert_eq!(body["type"], "message_received");
            assert_eq!(body["truncated"], false);
            assert_eq!(body["api_version"], "v1");
            assert!(body["data"].is_object());
            let _ = endpoint;
        }
    }
    assert_eq!(to_a[0].destination.path_and_query, "/hook?t=1");
    // Delivered: recorded, never sent again.
    let listed = rig
        .endpoints
        .open(&rig.authz, &ca)
        .unwrap()
        .deliveries(
            ea.id.as_str(),
            &PageRequest {
                after: None,
                limit: 10,
            },
        )
        .await
        .unwrap()
        .items;
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|d| d.status == DeliveryStatus::Delivered
        && d.attempts == 1
        && d.last_status == Some(200)));
    // B reads none of A's.
    let open_b = rig.endpoints.open(&rig.authz, &cb).unwrap();
    assert_eq!(
        open_b.get(ea.id.as_str()).await.unwrap_err().code(),
        "not_found"
    );
    assert_eq!(
        open_b
            .deliveries(
                ea.id.as_str(),
                &PageRequest {
                    after: None,
                    limit: 10
                }
            )
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        open_b
            .retry(ea.id.as_str(), listed[0].id.as_str())
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        open_b.test(ea.id.as_str()).await.unwrap_err().code(),
        "not_found"
    );
    assert_eq!(
        open_b
            .rotate_secret(ea.id.as_str())
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        open_b.delete(ea.id.as_str()).await.unwrap_err().code(),
        "not_found"
    );
    assert_eq!(
        open_b
            .update(
                ea.id.as_str(),
                EndpointChange {
                    url: Some("https://b.example.net/x".to_owned()),
                    ..EndpointChange::default()
                }
            )
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        open_b.get("whe_not-an-id").await.unwrap_err().code(),
        "not_found"
    );
    // A ping, once, signed; its body names the endpoint. A second later:
    // the newest delivery.
    rig.clock.advance(Duration::from_secs(1));
    let now = rig.clock.now().unix_timestamp();
    let ping = rig
        .endpoints
        .open(&rig.authz, &ca)
        .unwrap()
        .test(ea.id.as_str())
        .await
        .unwrap();
    assert_eq!(
        (ping.event_type.as_str(), ping.status),
        ("ping", DeliveryStatus::Pending)
    );
    rig.sender.status.store(500, Ordering::SeqCst);
    assert_eq!(round(&rig).await, 1);
    let pinged = rig.sender.sent_to("a.example.net").pop().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&pinged.body).unwrap();
    assert_eq!(
        (
            body["type"].as_str(),
            body["endpoint_id"].as_str(),
            body["id"].as_str()
        ),
        (Some("ping"), Some(ea.id.as_str()), Some(ping.id.as_str()))
    );
    verify(
        &secret_a,
        ping.id.as_str(),
        &now.to_string(),
        pinged.header("webhook-signature").unwrap(),
        &pinged.body,
        now,
    )
    .unwrap();
    assert_eq!(round(&rig).await, 0, "a ping is tried once");
    let listed = rig
        .endpoints
        .open(&rig.authz, &ca)
        .unwrap()
        .deliveries(
            ea.id.as_str(),
            &PageRequest {
                after: None,
                limit: 1,
            },
        )
        .await
        .unwrap()
        .items;
    assert_eq!(
        (
            listed[0].id.clone(),
            listed[0].status,
            listed[0].last_status
        ),
        (ping.id, DeliveryStatus::Failed, Some(500))
    );
    rig.sender.status.store(200, Ordering::SeqCst);
    // An unconfigured deployment opens nothing.
    let none = WebhookEndpoints::new(backend, Arc::new(rig.clock.clone()), None);
    assert_eq!(
        none.open(&rig.authz, &ca).unwrap_err().code(),
        "webhooks_not_configured"
    );
}

/// Every type a tenant receives may be asked for, and is delivered: the
/// types roadmap M2d reviewed and the service's own number events among
/// them (`TENANT_EVENT_TYPES`); a type not asked for is not.
pub async fn every_tenant_event_type_is_delivered(backend: &dyn Backend) {
    drain(backend.webhooks().as_ref()).await;
    let rig = rig(backend, RetrySchedule::default(), fast_settings());
    let a = unique_tenant("wh-types");
    let pn = super::unique_digits();
    let ca = caller(backend, &rig.authz, &a).await;
    let all: Vec<&str> = crate::events::TENANT_EVENT_TYPES
        .iter()
        .copied()
        .filter(|t| *t != "status_updated")
        .collect();
    rig.endpoints
        .open(&rig.authz, &ca)
        .unwrap()
        .create(create("https://hooks.example.com/all", &all))
        .await
        .unwrap();
    bind(backend.records().as_ref(), &a, &pn).await;
    let mut sent = Vec::new();
    for kind in crate::events::TENANT_EVENT_TYPES {
        let mut event = row(Some(a.as_str()), kind, &pn, Some(&super::unique()));
        event.data = format!("{{\"event\":\"{kind}\"}}");
        backend.outbox().insert(&event).await.unwrap().unwrap();
        if kind != "status_updated" {
            sent.push(event.id);
        }
    }
    while round(&rig).await > 0 {}
    let mut got: Vec<String> = rig
        .sender
        .sent()
        .iter()
        .map(|r| r.header("webhook-id").unwrap().to_owned())
        .collect();
    got.sort();
    sent.sort();
    assert_eq!(got, sent);
    for kind in [
        "standby_observed",
        "thread_control_changed",
        "user_action_reported",
        "number_connected",
        "number_disconnected",
        "number_reconnect_required",
    ] {
        assert!(
            rig.sender
                .sent()
                .iter()
                .any(
                    |r| serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["type"]
                        == kind
                ),
            "{kind}"
        );
    }
}

/// M2.4, the retry schedule under a fake clock: an endpoint answering
/// `500` is tried at 0, 10 s, 30 s, 70 s… (no jitter), never before its
/// time; past its window the delivery is `failed` and the endpoint, failing
/// the whole window, disabled; a manual retry after it is turned on again
/// delivers it. Decisive: the schedule's waits, the window, the claim's
/// due time.
pub async fn the_retry_schedule_under_a_fake_clock(backend: &dyn Backend) {
    drain(backend.webhooks().as_ref()).await;
    let schedule = RetrySchedule {
        window: Duration::from_secs(150),
        ..RetrySchedule::default()
    };
    let rig = rig(backend, schedule, fast_settings());
    let a = unique_tenant("wh-retry");
    let pn = super::unique_digits();
    let ca = caller(backend, &rig.authz, &a).await;
    let open = rig.endpoints.open(&rig.authz, &ca).unwrap();
    let (endpoint, _) = open
        .create(create(
            "https://hooks.example.com/wa",
            &["message_received"],
        ))
        .await
        .unwrap();
    event_for(backend, &a, &pn).await;
    rig.sender.status.store(500, Ordering::SeqCst);
    let mut attempts_at = Vec::new();
    // 0, 10, 30, 70, 150 s: each attempt exactly when due, none earlier.
    for wait in [0u64, 10, 20, 40, 80] {
        if wait > 0 {
            rig.clock.advance(Duration::from_secs(wait - 1));
            assert_eq!(round(&rig).await, 0, "not before its time ({wait} s)");
            rig.clock.advance(Duration::from_secs(1));
        }
        assert_eq!(round(&rig).await, 1, "due after {wait} s");
        attempts_at.push((rig.clock.now() - START).whole_seconds());
    }
    assert_eq!(attempts_at, [0, 10, 30, 70, 150]);
    let page = PageRequest {
        after: None,
        limit: 10,
    };
    let failed = open
        .deliveries(endpoint.id.as_str(), &page)
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();
    assert_eq!(
        (
            failed.status,
            failed.attempts,
            failed.next_attempt_at,
            failed.last_status
        ),
        (DeliveryStatus::Failed, 5, None, Some(500))
    );
    rig.clock.advance(Duration::from_hours(1));
    assert_eq!(round(&rig).await, 0, "failed: never tried again by itself");
    let off = open.get(endpoint.id.as_str()).await.unwrap();
    assert_eq!(
        (off.enabled, off.disabled_reason),
        (false, Some(DisabledReason::Failing)),
        "failing the whole window"
    );
    // Turned on, retried by hand: delivered.
    open.update(
        endpoint.id.as_str(),
        EndpointChange {
            enabled: Some(true),
            ..EndpointChange::default()
        },
    )
    .await
    .unwrap();
    rig.sender.status.store(204, Ordering::SeqCst);
    let retried = open
        .retry(endpoint.id.as_str(), failed.id.as_str())
        .await
        .unwrap();
    assert_eq!(
        (retried.status, retried.next_attempt_at),
        (DeliveryStatus::Pending, Some(rig.clock.now()))
    );
    assert_eq!(round(&rig).await, 1);
    let done = open
        .deliveries(endpoint.id.as_str(), &page)
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();
    assert_eq!(
        (done.status, done.attempts, done.last_status),
        (DeliveryStatus::Delivered, 6, Some(204))
    );
    assert_eq!(
        open.get(endpoint.id.as_str()).await.unwrap().failing_since,
        None
    );
    assert_eq!(rig.sender.sent().len(), 6);
    let _ = rig.out;
}

/// M2.4: a destination outside the allow-list is refused at creation
/// (not `https`, not listed, an address outside it, a host resolving to a
/// private address), and at delivery: the host's first lookup (at
/// creation) answered a public address, its second (at the attempt) a
/// private one, and nothing is sent; the attempt is recorded
/// `destination_refused` and retried, and refused again while the host
/// stays private. A change to an unlisted URL is refused too.
pub async fn a_destination_is_refused_at_creation_and_at_delivery(backend: &dyn Backend) {
    drain(backend.webhooks().as_ref()).await;
    let rig = rig(backend, RetrySchedule::default(), fast_settings());
    let a = unique_tenant("wh-ssrf");
    let pn = super::unique_digits();
    let ca = caller(backend, &rig.authz, &a).await;
    let open = rig.endpoints.open(&rig.authz, &ca).unwrap();
    rig.resolver.answer("private.example.net", &[&["10.0.0.7"]]);
    rig.resolver
        .answer("mapped.example.net", &[&["::ffff:127.0.0.1"]]);
    rig.resolver
        .answer("metadata.example.net", &[&["169.254.169.254"]]);
    let lookups = rig.resolver.lookups.load(Ordering::SeqCst);
    for url in [
        "http://a.example.net/hook",
        "https://evil.example.org/hook",
        "https://127.0.0.1/hook",
        "https://[::1]/hook",
        "https://user:pw@a.example.net/",
    ] {
        let refused = open
            .create(create(url, &["message_received"]))
            .await
            .unwrap_err();
        assert_eq!(
            (refused.code(), refused.field()),
            ("invalid_request", Some("url")),
            "{url}"
        );
    }
    assert_eq!(
        rig.resolver.lookups.load(Ordering::SeqCst),
        lookups,
        "refused by its text, before any lookup"
    );
    for url in [
        "https://private.example.net/hook",
        "https://mapped.example.net/hook",
        "https://metadata.example.net/hook",
        "https://unresolved.example.net/hook",
    ] {
        let refused = open
            .create(create(url, &["message_received"]))
            .await
            .unwrap_err();
        assert_eq!(refused.field(), Some("url"), "{url}");
    }
    // Rebinding: public at creation, private at delivery.
    rig.resolver
        .answer("rebind.example.net", &[&["93.184.216.34"], &["10.0.0.7"]]);
    let (endpoint, _) = open
        .create(create(
            "https://rebind.example.net/hook",
            &["message_received"],
        ))
        .await
        .unwrap();
    event_for(backend, &a, &pn).await;
    let sent = rig.sender.sent().len();
    assert_eq!(round(&rig).await, 1);
    assert_eq!(
        rig.sender.sent().len(),
        sent,
        "nothing sent to the private address"
    );
    let page = PageRequest {
        after: None,
        limit: 10,
    };
    let refused = open
        .deliveries(endpoint.id.as_str(), &page)
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();
    assert_eq!(
        (
            refused.status,
            refused.attempts,
            refused.last_error.clone(),
            refused.last_status
        ),
        (
            DeliveryStatus::Pending,
            1,
            Some(AttemptError::DestinationRefused),
            None
        )
    );
    rig.clock.advance(Duration::from_secs(10));
    assert_eq!(round(&rig).await, 1);
    assert_eq!(
        rig.sender.sent().len(),
        sent,
        "still private: still refused"
    );
    // A change to a URL off the list is refused; the endpoint keeps its own.
    let changed = open
        .update(
            endpoint.id.as_str(),
            EndpointChange {
                url: Some("https://evil.example.org/".to_owned()),
                ..EndpointChange::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(changed.field(), Some("url"));
    assert_eq!(
        open.get(endpoint.id.as_str()).await.unwrap().url,
        "https://rebind.example.net/hook"
    );
}

/// M2.4: two signatures during a rotation. The new secret, shown once by
/// the rotation, signs first, the old one second, each verifying on its
/// own; past the 24 hours, the new one alone.
pub async fn a_rotation_signs_twice_for_24_hours(backend: &dyn Backend) {
    drain(backend.webhooks().as_ref()).await;
    let rig = rig(backend, RetrySchedule::default(), fast_settings());
    let a = unique_tenant("wh-rotate");
    let pn = super::unique_digits();
    let ca = caller(backend, &rig.authz, &a).await;
    let open = rig.endpoints.open(&rig.authz, &ca).unwrap();
    let (endpoint, old) = open
        .create(create(
            "https://hooks.example.com/wa",
            &["message_received"],
        ))
        .await
        .unwrap();
    let (rotated, new) = open.rotate_secret(endpoint.id.as_str()).await.unwrap();
    assert_ne!(new.expose(), old.expose());
    assert_eq!(
        rotated.previous_secret_until,
        Some(START + time::Duration::hours(24))
    );
    let check = |request: &SendRequest, secret: &EndpointSecret| {
        verify(
            secret,
            request.header("webhook-id").unwrap(),
            request.header("webhook-timestamp").unwrap(),
            request.header("webhook-signature").unwrap(),
            &request.body,
            rig.clock.now().unix_timestamp(),
        )
    };
    event_for(backend, &a, &pn).await;
    assert_eq!(round(&rig).await, 1);
    let during = rig.sender.sent().pop().unwrap();
    let signatures: Vec<&str> = during
        .header("webhook-signature")
        .unwrap()
        .split(' ')
        .collect();
    assert_eq!(signatures.len(), 2, "both secrets sign");
    let id = during.header("webhook-id").unwrap();
    let ts: i64 = during.header("webhook-timestamp").unwrap().parse().unwrap();
    assert_eq!(signatures[0], new.sign(id, ts, &during.body));
    assert_eq!(signatures[1], old.sign(id, ts, &during.body));
    check(&during, &new).unwrap();
    check(&during, &old).unwrap();
    rig.clock.advance(Duration::from_hours(24));
    event_for(backend, &a, &pn).await;
    assert_eq!(round(&rig).await, 1);
    let after = rig.sender.sent().pop().unwrap();
    assert_eq!(
        after
            .header("webhook-signature")
            .unwrap()
            .split(' ')
            .count(),
        1
    );
    check(&after, &new).unwrap();
    assert!(
        check(&after, &old).is_err(),
        "the old secret no longer signs"
    );
}

/// M2.4: an event whose `data` is over 256 KiB is delivered without it
/// (`truncated: true`, `data: null`), signed over what is sent; the outbox
/// answers it whole to its tenant and to no other (what
/// `GET /v1/events/{id}` reads); one of exactly 256 KiB is delivered whole.
pub async fn a_large_event_is_delivered_truncated(backend: &dyn Backend) {
    drain(backend.webhooks().as_ref()).await;
    let rig = rig(backend, RetrySchedule::default(), fast_settings());
    let (a, b) = (unique_tenant("wh-big-a"), unique_tenant("wh-big-b"));
    let pn = super::unique_digits();
    let ca = caller(backend, &rig.authz, &a).await;
    let _ = caller(backend, &rig.authz, &b).await;
    let open = rig.endpoints.open(&rig.authz, &ca).unwrap();
    let (_, secret) = open
        .create(create(
            "https://hooks.example.com/wa",
            &["message_received"],
        ))
        .await
        .unwrap();
    bind(backend.records().as_ref(), &a, &pn).await;
    let sized = |n: usize| {
        let mut event = row(
            Some(a.as_str()),
            "message_received",
            &pn,
            Some(&super::unique()),
        );
        let head = "{\"event\":\"message_received\",\"pad\":\"";
        let tail = "\"}";
        event.data = format!("{head}{}{tail}", "x".repeat(n - head.len() - tail.len()));
        assert_eq!(event.data.len(), n);
        event
    };
    let big = sized(MAX_DELIVERED_DATA_BYTES + 1);
    let edge = sized(MAX_DELIVERED_DATA_BYTES);
    backend.outbox().insert(&big).await.unwrap().unwrap();
    backend.outbox().insert(&edge).await.unwrap().unwrap();
    assert_eq!(round(&rig).await, 2);
    let sent = rig.sender.sent();
    let of = |id: &str| {
        sent.iter()
            .find(|r| r.header("webhook-id") == Some(id))
            .unwrap()
    };
    let truncated = of(&big.id);
    let body: serde_json::Value = serde_json::from_slice(&truncated.body).unwrap();
    assert_eq!(
        (body["truncated"].as_bool(), body["data"].is_null()),
        (Some(true), true)
    );
    assert!(truncated.body.len() < 1024, "the data left out");
    verify(
        &secret,
        &big.id,
        truncated.header("webhook-timestamp").unwrap(),
        truncated.header("webhook-signature").unwrap(),
        &truncated.body,
        START.unix_timestamp(),
    )
    .unwrap();
    let whole: serde_json::Value = serde_json::from_slice(&of(&edge.id).body).unwrap();
    assert_eq!(
        (
            whole["truncated"].as_bool(),
            whole["data"]["event"].as_str()
        ),
        (Some(false), Some("message_received"))
    );
    // Fetchable whole by its tenant only.
    let fetched = backend
        .outbox()
        .get(&tenant(&a), &big.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.data, big.data);
    assert_eq!(
        backend.outbox().get(&tenant(&b), &big.id).await.unwrap(),
        None
    );
}

/// Two dispatchers on one backend (two replicas), fanning out and
/// claiming at once, send each event once.
pub async fn two_dispatchers_send_each_event_once(backend: &dyn Backend) {
    drain(backend.webhooks().as_ref()).await;
    let rig = rig(backend, RetrySchedule::default(), fast_settings());
    let other = Dispatcher::new(backend, Arc::new(rig.clock.clone()), rig.out.clone());
    let tenants: Vec<String> = (0..3).map(|_| unique_tenant("wh-twice")).collect();
    let mut events = Vec::new();
    for t in &tenants {
        let c = caller(backend, &rig.authz, t).await;
        rig.endpoints
            .open(&rig.authz, &c)
            .unwrap()
            .create(create(
                "https://hooks.example.com/wa",
                &["message_received"],
            ))
            .await
            .unwrap();
        let pn = super::unique_digits();
        for _ in 0..4 {
            events.push(event_for(backend, t, &pn).await.id);
        }
    }
    for _ in 0..8 {
        let (x, y) = tokio::join!(
            async {
                rig.dispatcher.fan_out().await.unwrap();
                rig.dispatcher.dispatch_due().await
            },
            async {
                other.fan_out().await.unwrap();
                other.dispatch_due().await
            }
        );
        let _ = (x, y);
    }
    let mut sent: Vec<String> = rig
        .sender
        .sent()
        .iter()
        .map(|r| r.header("webhook-id").unwrap().to_owned())
        .filter(|id| events.contains(id))
        .collect();
    sent.sort();
    events.sort();
    assert_eq!(sent, events, "each event sent once");
}

/// Back-pressure: an endpoint that never answers holds at most its bound
/// of attempts on the replica, and another tenant's endpoint gets all its
/// deliveries meanwhile, through the dispatcher's own loop.
pub async fn a_slow_endpoint_does_not_starve_another(backend: &dyn Backend) {
    drain(backend.webhooks().as_ref()).await;
    let dispatch = DispatchSettings {
        workers: 4,
        per_endpoint: 2,
        per_tenant: 3,
        poll: Duration::from_millis(10),
        ..DispatchSettings::default()
    };
    let rig = rig(backend, RetrySchedule::default(), dispatch);
    rig.sender
        .hold
        .lock()
        .unwrap()
        .push("slow.example.net".to_owned());
    let (slow, fast) = (unique_tenant("wh-slow"), unique_tenant("wh-fast"));
    let cs = caller(backend, &rig.authz, &slow).await;
    let cf = caller(backend, &rig.authz, &fast).await;
    rig.endpoints
        .open(&rig.authz, &cs)
        .unwrap()
        .create(create("https://slow.example.net/", &["message_received"]))
        .await
        .unwrap();
    rig.endpoints
        .open(&rig.authz, &cf)
        .unwrap()
        .create(create("https://fast.example.net/", &["message_received"]))
        .await
        .unwrap();
    let (ps, pf) = (super::unique_digits(), super::unique_digits());
    for _ in 0..10 {
        event_for(backend, &slow, &ps).await;
        event_for(backend, &fast, &pf).await;
    }
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(rig.dispatcher.clone().run(async {
        let _ = stopped.await;
    }));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while rig.sender.sent_to("fast.example.net").len() < 10
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        rig.sender.sent_to("fast.example.net").len(),
        10,
        "the fast endpoint got everything"
    );
    assert_eq!(
        rig.sender.held.load(Ordering::SeqCst),
        2,
        "the slow one holds its bound only"
    );
    assert_eq!(rig.sender.sent_to("slow.example.net").len(), 2);
    let _ = stop.send(());
    rig.sender.release.notify_waiters();
    // The held attempts end (as a timeout would), and the loop with them.
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            rig.sender.release.notify_waiters();
            if running.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        running.is_finished(),
        "the loop stops once its attempts end"
    );
}
