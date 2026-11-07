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
//! - **A platform key's endpoint of several tenants** (roadmap M2m): it
//!   receives the named tenants' events and no other's, each delivery its
//!   own tenant's event; `tenants` is `422` from a tenant key and checked
//!   against the key's set (`403`); only a key that may name them all
//!   reaches it; a change of the set keeps, adds and drops by tenant; a
//!   deleted tenant leaves it (never widened, a suspended one waits).
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
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use time::OffsetDateTime;
use time::macros::datetime;

use super::outbox::{bind, row, waba_of};
use crate::admin::mint;
use crate::authz::{Authorizer, Caller};
use crate::backend::Backend;
use crate::model::{
    AllowedTenants, DeleteTenantOutcome, KeyOwner, PageRequest, Scope, TenantId, TenantStatus,
};
use crate::outbox::NewEvent;
use crate::webhooks_out::destination::{DestinationPolicy, ResolveError, Resolver};
use crate::webhooks_out::dispatch::{
    DispatchSettings, Dispatcher, SendError, SendRequest, Sender, WebhooksOut,
};
use crate::webhooks_out::endpoints::{CreateEndpoint, UpdateEndpoint, WebhookEndpoints};
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
    the_pending_backlog_is_counted(backend).await;
    deliveries_go_with_their_endpoint_and_tenant(backend).await;
    a_delivery_is_signed_and_carries_its_tenants_events_only(backend).await;
    every_tenant_event_type_is_delivered(backend).await;
    the_retry_schedule_under_a_fake_clock(backend).await;
    a_destination_is_refused_at_creation_and_at_delivery(backend).await;
    a_rotation_signs_twice_for_24_hours(backend).await;
    a_large_event_is_delivered_truncated(backend).await;
    two_dispatchers_send_each_event_once(backend).await;
    a_slow_endpoint_does_not_starve_another(backend).await;
    a_platform_endpoint_receives_its_named_tenants_only(backend).await;
    tenants_are_a_platform_keys_and_checked(backend).await;
    a_platform_endpoint_is_reached_by_a_key_naming_all_its_tenants(backend).await;
    a_platform_endpoint_changes_its_tenants(backend).await;
    a_deleted_or_suspended_named_tenant_does_not_widen_the_endpoint(backend).await;
    a_platform_endpoint_store_keeps_cursors_and_applies_only_to_the_set_checked(backend).await;
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
        tenants: None,
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
                tenants: None,
                reached_through: None,
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
            .fan_out(
                &endpoint.id,
                &endpoint.tenant,
                5,
                6,
                &[delivery("evt_1", 1, START)]
            )
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
                &endpoint.tenant,
                0,
                2,
                &[delivery("evt_1", 1, START), delivery("evt_2", 2, START)]
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .fan_out(
                &endpoint.id,
                &endpoint.tenant,
                2,
                3,
                &[delivery("evt_2", 2, START)]
            )
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
        store.fan_out(&endpoint.id, &endpoint.tenant, 3, 4, &x),
        store.fan_out(&endpoint.id, &endpoint.tenant, 3, 4, &y),
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
                &ea1.tenant,
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
                &ea2.tenant,
                0,
                1,
                &[delivery("evt_a3", 3, early - time::Duration::seconds(1))]
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .fan_out(&eb.id, &eb.tenant, 0, 1, &[delivery("evt_b1", 1, at)])
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
            .fan_out(
                &e.id,
                &e.tenant,
                0,
                1,
                &[delivery(&format!("evt_{n}"), 1, at)],
            )
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
            &endpoint.tenant,
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
    // The tenant's other endpoint: its id names none of this one's
    // deliveries, and the failed one stays failed (the retry below finds
    // it so, due at `late`, not at this call's instant).
    let sibling = created(store.as_ref(), &new_endpoint(&a, 0)).await;
    assert_eq!(
        store
            .retry_delivery(
                &tenant(&a),
                &sibling.id,
                &retry.delivery.id,
                late - time::Duration::minutes(5),
                window_end - time::Duration::minutes(5)
            )
            .await
            .unwrap(),
        None,
        "a sibling endpoint's id"
    );
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

/// The backlog counts (roadmap M2k): [`WebhookStore::pending`] counts every
/// pending delivery, claimed or not, and reports the earliest
/// `next_attempt_at` due by the instant asked, never a retry still waiting
/// for its backoff nor a delivery that is settled; it falls as deliveries
/// are delivered or fail, and with the endpoint that held them.
pub async fn the_pending_backlog_is_counted(backend: &dyn Backend) {
    let records = backend.records();
    let store = backend.webhooks();
    drain(store.as_ref()).await;
    let a = unique_tenant("wh-backlog");
    records.create_tenant(&tenant(&a), "").await.unwrap();
    let endpoint = created(store.as_ref(), &new_endpoint(&a, 0)).await;
    let at = START + time::Duration::days(900);
    let secs = time::Duration::seconds;
    let empty = store.pending(at).await.unwrap();
    assert_eq!((empty.count, empty.oldest_due), (0, None));
    store
        .fan_out(
            &endpoint.id,
            &endpoint.tenant,
            0,
            3,
            &[
                delivery("evt_b1", 1, at),
                delivery("evt_b2", 2, at + secs(10)),
                delivery("evt_b3", 3, at + secs(20)),
            ],
        )
        .await
        .unwrap();
    // Every one counts; only those due by the instant asked date it.
    let before = store.pending(at - secs(1)).await.unwrap();
    assert_eq!((before.count, before.oldest_due), (3, None));
    let first = store.pending(at + secs(15)).await.unwrap();
    assert_eq!((first.count, first.oldest_due), (3, Some(at)));
    // A suspended tenant's and a disabled endpoint's deliveries wait, and
    // the backlog is what waits: they count, and they age.
    records
        .update_tenant(&tenant(&a), None, Some(TenantStatus::Suspended))
        .await
        .unwrap();
    let suspended = store.pending(at + secs(15)).await.unwrap();
    assert_eq!((suspended.count, suspended.oldest_due), (3, Some(at)));
    records
        .update_tenant(&tenant(&a), None, Some(TenantStatus::Active))
        .await
        .unwrap();
    let toggle = |enabled| EndpointChange {
        enabled: Some(enabled),
        ..EndpointChange::default()
    };
    store
        .update_endpoint(&tenant(&a), &endpoint.id, &toggle(false), at)
        .await
        .unwrap();
    let disabled = store.pending(at + secs(15)).await.unwrap();
    assert_eq!((disabled.count, disabled.oldest_due), (3, Some(at)));
    store
        .update_endpoint(&tenant(&a), &endpoint.id, &toggle(true), at)
        .await
        .unwrap();
    // Claimed, still pending: it counts until it is settled.
    let one = take(store.as_ref(), &endpoint.id, at + secs(15)).await;
    assert_eq!(store.pending(at + secs(15)).await.unwrap().count, 3);
    let settle = |outcome| Attempt {
        delivery: one.delivery.id.clone(),
        claim: one.claim.clone(),
        outcome,
        started_at: at + secs(15),
        duration_ms: 1,
        status: Some(200),
        error: None,
        counted: true,
        disable_if_failing_since: None,
    };
    // A retry still waiting is pending but not due: the oldest due one is
    // the next delivery's, not the oldest record's.
    assert!(
        store
            .settle(&settle(Outcome::Retry(at + secs(500))), at + secs(15))
            .await
            .unwrap()
    );
    let retrying = store.pending(at + secs(15)).await.unwrap();
    assert_eq!(
        (retrying.count, retrying.oldest_due),
        (3, Some(at + secs(10)))
    );
    // Delivered, then failed: each leaves the backlog.
    let two = take(store.as_ref(), &endpoint.id, at + secs(15)).await;
    assert_eq!(two.delivery.event_id.as_deref(), Some("evt_b2"));
    let mut done = Attempt {
        delivery: two.delivery.id.clone(),
        claim: two.claim.clone(),
        ..settle(Outcome::Delivered)
    };
    assert!(store.settle(&done, at + secs(15)).await.unwrap());
    let delivered = store.pending(at + secs(15)).await.unwrap();
    assert_eq!((delivered.count, delivered.oldest_due), (2, None));
    let three = take(store.as_ref(), &endpoint.id, at + secs(25)).await;
    done.delivery = three.delivery.id.clone();
    done.claim = three.claim.clone();
    done.outcome = Outcome::Failed;
    assert!(store.settle(&done, at + secs(25)).await.unwrap());
    let failed = store.pending(at + secs(25)).await.unwrap();
    assert_eq!((failed.count, failed.oldest_due), (1, None));
    // The retrying one comes due.
    let later = store.pending(at + secs(500)).await.unwrap();
    assert_eq!((later.count, later.oldest_due), (1, Some(at + secs(500))));
    // A manual retry makes a settled one pending again.
    assert!(
        store
            .retry_delivery(
                &tenant(&a),
                &endpoint.id,
                &two.delivery.id,
                at + secs(600),
                at + time::Duration::hours(72),
            )
            .await
            .unwrap()
            .is_some()
    );
    let retried = store.pending(at + secs(600)).await.unwrap();
    assert_eq!(
        (retried.count, retried.oldest_due),
        (2, Some(at + secs(500)))
    );
    // And the endpoint's deletion takes its deliveries from the backlog.
    assert!(
        store
            .delete_endpoint(&tenant(&a), &endpoint.id)
            .await
            .unwrap()
    );
    let gone = store.pending(at + secs(600)).await.unwrap();
    assert_eq!((gone.count, gone.oldest_due), (0, None));
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
            &kept.tenant,
            0,
            2,
            &[delivery("evt_k1", 1, at), delivery("evt_k2", 2, at)],
        )
        .await
        .unwrap();
    store
        .fan_out(
            &dropped.id,
            &dropped.tenant,
            0,
            1,
            &[delivery("evt_d1", 1, at)],
        )
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
    // Its retention runs from its settlement, not its creation: made at
    // `at`, settled 40 days later, it stays 30 days after that.
    let late = at + time::Duration::days(40);
    let c = &take(store.as_ref(), &kept.id, late).await;
    store
        .settle(
            &Attempt {
                delivery: c.delivery.id.clone(),
                claim: c.claim.clone(),
                outcome: Outcome::Failed,
                started_at: late,
                duration_ms: 1,
                status: Some(500),
                error: Some(AttemptError::Status),
                counted: true,
                disable_if_failing_since: None,
            },
            late,
        )
        .await
        .unwrap();
    store
        .purge_deliveries(at + time::Duration::days(31))
        .await
        .unwrap();
    assert_eq!(
        store
            .deliveries(&tenant(&a), &kept.id, &page)
            .await
            .unwrap()
            .unwrap()
            .items
            .len(),
        1,
        "settled 9 days before the cutoff's 31: kept"
    );
    store
        .purge_deliveries(late + time::Duration::seconds(1))
        .await
        .unwrap();
    assert!(
        store
            .deliveries(&tenant(&a), &kept.id, &page)
            .await
            .unwrap()
            .unwrap()
            .items
            .is_empty(),
        "settled before the cutoff: purged"
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
        tenants: None,
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
    // Nor through an endpoint of its own: A's delivery id under B's
    // endpoint names nothing, and A's delivery is left as it is.
    assert_eq!(
        open_b
            .retry(eb.id.as_str(), listed[0].id.as_str())
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        backend
            .webhooks()
            .retry_delivery(
                &tenant(&b),
                &eb.id,
                &listed[0].id,
                START,
                START + time::Duration::hours(72)
            )
            .await
            .unwrap(),
        None,
        "the store, below the service"
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
                UpdateEndpoint {
                    url: Some("https://b.example.net/x".to_owned()),
                    ..UpdateEndpoint::default()
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
        (ping.id.clone(), DeliveryStatus::Failed, Some(500))
    );
    // A ping retried by hand gets a new window, and is still tried once:
    // one more attempt, then failed, never pending for the window.
    let retried = rig
        .endpoints
        .open(&rig.authz, &ca)
        .unwrap()
        .retry(ea.id.as_str(), ping.id.as_str())
        .await
        .unwrap();
    assert_eq!(retried.status, DeliveryStatus::Pending);
    assert!(retried.give_up_at > rig.clock.now());
    assert_eq!(round(&rig).await, 1);
    rig.clock.advance(Duration::from_secs(3600));
    assert_eq!(round(&rig).await, 0, "a retried ping is tried once too");
    let again = rig
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
        (again[0].id.clone(), again[0].status, again[0].attempts),
        (ping.id, DeliveryStatus::Failed, 2)
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
        "token_expiring",
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
        UpdateEndpoint {
            enabled: Some(true),
            ..UpdateEndpoint::default()
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
            UpdateEndpoint {
                url: Some("https://evil.example.org/".to_owned()),
                ..UpdateEndpoint::default()
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

// ─── Roadmap M2m: the tenants of a platform key's endpoint ────────────────

/// A caller with scope `webhooks` acting as `acting` (created) through a
/// platform key that may name `allowed`.
async fn platform_caller(
    backend: &dyn Backend,
    authz: &Authorizer,
    acting: &str,
    allowed: AllowedTenants,
) -> Caller {
    let _ = backend
        .records()
        .create_tenant(&tenant(acting), "")
        .await
        .unwrap();
    let key = mint(
        backend.records().as_ref(),
        KeyOwner::Platform(allowed),
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
            Some(acting.as_bytes()),
            Scope::Webhooks,
        )
        .await
        .unwrap()
}

fn only(tenants: &[&str]) -> AllowedTenants {
    AllowedTenants::Only(tenants.iter().map(|t| tenant(t)).collect())
}

fn naming(url: &str, tenants: &[&str]) -> CreateEndpoint {
    CreateEndpoint {
        tenants: Some(tenants.iter().map(|t| (*t).to_owned()).collect()),
        ..create(url, &["message_received"])
    }
}

fn page(limit: usize) -> PageRequest {
    PageRequest { after: None, limit }
}

/// The `webhook-id`s a host got, sorted.
fn got(rig: &Rig, host: &str) -> Vec<String> {
    let mut ids: Vec<String> = rig
        .sender
        .sent_to(host)
        .iter()
        .map(|r| r.header("webhook-id").unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

fn sorted(events: &[&NewEvent]) -> Vec<String> {
    let mut ids: Vec<String> = events.iter().map(|e| e.id.clone()).collect();
    ids.sort();
    ids
}

/// M2m, the decisive case: a platform key's endpoint naming A and B
/// receives A's and B's events and none of C's (nor the events of the
/// tenant it was made as, which it did not name, nor those before its
/// creation); each delivery is its own tenant's event, read in that
/// tenant's stream; a store fan-out for a tenant the endpoint does not
/// name records nothing.
pub async fn a_platform_endpoint_receives_its_named_tenants_only(backend: &dyn Backend) {
    drain(backend.webhooks().as_ref()).await;
    let rig = rig(backend, RetrySchedule::default(), fast_settings());
    let (x, a, b, c) = (
        unique_tenant("wh-pl-x"),
        unique_tenant("wh-pl-a"),
        unique_tenant("wh-pl-b"),
        unique_tenant("wh-pl-c"),
    );
    let digits: Vec<String> = (0..4).map(|_| super::unique_digits()).collect();
    for t in [&a, &b, &c] {
        let _ = backend
            .records()
            .create_tenant(&tenant(t), "")
            .await
            .unwrap();
    }
    let platform = platform_caller(backend, &rig.authz, &x, only(&[&x, &a, &b])).await;
    let before = event_for(backend, &a, &digits[0]).await;
    let (endpoint, secret) = rig
        .endpoints
        .open(&rig.authz, &platform)
        .unwrap()
        .create(naming(
            "https://a.example.net/hook",
            // A repeated tenant is one.
            &[&a, &b, &a],
        ))
        .await
        .unwrap();
    assert!(secret.expose().starts_with("whsec_"));
    assert_eq!(endpoint.tenant, tenant(&x), "the WA-Tenant's endpoint");
    let named: Vec<&TenantId> = endpoint
        .tenants
        .as_ref()
        .unwrap()
        .iter()
        .map(|t| &t.tenant)
        .collect();
    assert_eq!(named, [&tenant(&a), &tenant(&b)]);
    let read = rig
        .endpoints
        .open(&rig.authz, &platform)
        .unwrap()
        .get(endpoint.id.as_str())
        .await
        .unwrap();
    assert_eq!(read.tenants, endpoint.tenants, "stored with its tenants");
    let a1 = event_for(backend, &a, &digits[0]).await;
    let b1 = event_for(backend, &b, &digits[1]).await;
    let _c1 = event_for(backend, &c, &digits[2]).await;
    let _x1 = event_for(backend, &x, &digits[3]).await;
    let a2 = event_for(backend, &a, &digits[0]).await;
    assert_eq!(round(&rig).await, 3, "A's two and B's one");
    assert_eq!(round(&rig).await, 0);
    assert_eq!(
        got(&rig, "a.example.net"),
        sorted(&[&a1, &b1, &a2]),
        "A's and B's events: not C's, not the owner's, not before the creation ({})",
        before.id
    );
    for request in rig.sender.sent_to("a.example.net") {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let id = request.header("webhook-id").unwrap();
        let tenant_id = if id == b1.id { &b } else { &a };
        assert_eq!(body["id"], id);
        assert_eq!(
            body["tenant_id"],
            tenant_id.as_str(),
            "each delivery its own tenant's event"
        );
        assert!(body["data"].is_object());
        verify(
            &secret,
            id,
            request.header("webhook-timestamp").unwrap(),
            request.header("webhook-signature").unwrap(),
            &request.body,
            START.unix_timestamp(),
        )
        .unwrap();
    }
    // The store: a tenant the endpoint does not name has no stream in it,
    // however the cursor reads; a named one moves on its own cursor only.
    let store = backend.webhooks();
    for other in [&c, &x] {
        assert!(
            !store
                .fan_out(
                    &endpoint.id,
                    &tenant(other),
                    0,
                    1,
                    &[delivery("evt_other", 1, START)]
                )
                .await
                .unwrap(),
            "not a tenant of the endpoint"
        );
    }
    let ids: Vec<String> = rig
        .endpoints
        .open(&rig.authz, &platform)
        .unwrap()
        .deliveries(endpoint.id.as_str(), &page(10))
        .await
        .unwrap()
        .items
        .iter()
        .map(|d| d.tenant.to_string())
        .collect();
    assert_eq!(ids.len(), 3, "nothing recorded for C or the owner");
    assert!(!ids.contains(&c) && !ids.contains(&x));
    let live = store
        .active_endpoints()
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.id == endpoint.id)
        .unwrap();
    let cursors: Vec<(String, bool)> = live
        .tenants
        .unwrap()
        .iter()
        .map(|t| (t.tenant.to_string(), t.cursor > 0))
        .collect();
    assert_eq!(cursors, [(a.clone(), true), (b.clone(), true)]);
    assert_eq!(live.cursor, 0, "the endpoint's own cursor stays unused");
}

/// M2m: `tenants` is a platform key's. From a tenant key it is `422` on
/// `tenants`; from a platform key, an empty list, more than ten, or an id
/// of no tenant's shape is `422` too; a tenant outside the key's set and
/// one that does not exist answer alike, `403`; none of them makes an
/// endpoint.
pub async fn tenants_are_a_platform_keys_and_checked(backend: &dyn Backend) {
    drain(backend.webhooks().as_ref()).await;
    let rig = rig(backend, RetrySchedule::default(), fast_settings());
    let (x, a, c) = (
        unique_tenant("wh-tv-x"),
        unique_tenant("wh-tv-a"),
        unique_tenant("wh-tv-c"),
    );
    for t in [&a, &c] {
        let _ = backend
            .records()
            .create_tenant(&tenant(t), "")
            .await
            .unwrap();
    }
    let tenant_key = caller(backend, &rig.authz, &x).await;
    let narrow = platform_caller(backend, &rig.authz, &x, only(&[&x, &a])).await;
    let wide = platform_caller(backend, &rig.authz, &x, AllowedTenants::All).await;
    let url = "https://a.example.net/hook";
    let refused = |caller: &Caller, request: CreateEndpoint| {
        let rig = &rig;
        let caller = caller.clone();
        async move {
            let error = rig
                .endpoints
                .open(&rig.authz, &caller)
                .unwrap()
                .create(request)
                .await
                .unwrap_err();
            (error.code().to_owned(), error.field().map(str::to_owned))
        }
    };
    let invalid = ("invalid_request".to_owned(), Some("tenants".to_owned()));
    let forbidden = ("forbidden".to_owned(), None);
    assert_eq!(
        refused(&tenant_key, naming(url, &[&x])).await,
        invalid,
        "a tenant key may not name tenants, not even its own"
    );
    assert_eq!(refused(&wide, naming(url, &[])).await, invalid, "none");
    let eleven: Vec<String> = (0..11).map(|n| format!("t{n}")).collect();
    let eleven: Vec<&str> = eleven.iter().map(String::as_str).collect();
    assert_eq!(refused(&wide, naming(url, &eleven)).await, invalid, "11");
    assert_eq!(
        refused(&wide, naming(url, &["not a tenant!"])).await,
        invalid
    );
    assert_eq!(
        refused(&narrow, naming(url, &[&a, &c])).await,
        forbidden,
        "C is outside the key's set"
    );
    assert_eq!(
        refused(&wide, naming(url, &[&a, "no-such-tenant-m2m"])).await,
        forbidden,
        "a tenant that does not exist answers as one outside the set"
    );
    assert_eq!(
        refused(&narrow, naming(url, &["no-such-tenant-m2m"])).await,
        forbidden
    );
    for caller in [&tenant_key, &narrow, &wide] {
        assert!(
            rig.endpoints
                .open(&rig.authz, caller)
                .unwrap()
                .list(&page(10))
                .await
                .unwrap()
                .items
                .is_empty(),
            "none was made"
        );
    }
    // Ten tenants are the most, each of which the key may name.
    let ten: Vec<String> = (0..10)
        .map(|n| unique_tenant(&format!("wh-tv{n}")))
        .collect();
    for t in &ten {
        let _ = backend
            .records()
            .create_tenant(&tenant(t), "")
            .await
            .unwrap();
    }
    let ten: Vec<&str> = ten.iter().map(String::as_str).collect();
    let (made, _) = rig
        .endpoints
        .open(&rig.authz, &wide)
        .unwrap()
        .create(naming(url, &ten))
        .await
        .unwrap();
    assert_eq!(made.tenants.unwrap().len(), 10);
}

/// M2m: an endpoint of several tenants is its `WA-Tenant`'s, reached only
/// through a platform key that may name every tenant it receives. The
/// owner's tenant key, and a platform key of a narrower set, find it
/// missing everywhere (to change its URL is to read those tenants'
/// events); the tenant's own endpoints stay the tenant key's.
pub async fn a_platform_endpoint_is_reached_by_a_key_naming_all_its_tenants(backend: &dyn Backend) {
    drain(backend.webhooks().as_ref()).await;
    let rig = rig(backend, RetrySchedule::default(), fast_settings());
    let (x, a, b) = (
        unique_tenant("wh-pr-x"),
        unique_tenant("wh-pr-a"),
        unique_tenant("wh-pr-b"),
    );
    for t in [&a, &b] {
        let _ = backend
            .records()
            .create_tenant(&tenant(t), "")
            .await
            .unwrap();
    }
    let tenant_key = caller(backend, &rig.authz, &x).await;
    let narrow = platform_caller(backend, &rig.authz, &x, only(&[&x, &a])).await;
    let wide = platform_caller(backend, &rig.authz, &x, only(&[&x, &a, &b])).await;
    let (shared, _) = rig
        .endpoints
        .open(&rig.authz, &wide)
        .unwrap()
        .create(naming("https://a.example.net/hook", &[&a, &b]))
        .await
        .unwrap();
    let (own, _) = rig
        .endpoints
        .open(&rig.authz, &tenant_key)
        .unwrap()
        .create(create("https://b.example.net/hook", &["message_received"]))
        .await
        .unwrap();
    assert_eq!(own.tenants, None);
    let listed = |caller: &Caller| {
        let rig = &rig;
        let caller = caller.clone();
        async move {
            rig.endpoints
                .open(&rig.authz, &caller)
                .unwrap()
                .list(&page(10))
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|e| e.id)
                .collect::<Vec<_>>()
        }
    };
    let mut both = vec![shared.id.clone(), own.id.clone()];
    both.sort();
    assert_eq!(listed(&wide).await, both);
    assert_eq!(listed(&tenant_key).await, std::slice::from_ref(&own.id));
    assert_eq!(listed(&narrow).await, std::slice::from_ref(&own.id));
    let id = shared.id.as_str();
    for hidden in [&tenant_key, &narrow] {
        let open = rig.endpoints.open(&rig.authz, hidden).unwrap();
        let not_found = |code: String| assert_eq!(code, "not_found");
        not_found(open.get(id).await.unwrap_err().code().to_owned());
        not_found(
            open.update(
                id,
                UpdateEndpoint {
                    url: Some("https://b.example.net/steal".to_owned()),
                    ..UpdateEndpoint::default()
                },
            )
            .await
            .unwrap_err()
            .code()
            .to_owned(),
        );
        not_found(open.delete(id).await.unwrap_err().code().to_owned());
        not_found(open.rotate_secret(id).await.unwrap_err().code().to_owned());
        not_found(open.test(id).await.unwrap_err().code().to_owned());
        not_found(
            open.deliveries(id, &page(10))
                .await
                .unwrap_err()
                .code()
                .to_owned(),
        );
        not_found(
            open.retry(id, DeliveryId::generate(START).unwrap().as_str())
                .await
                .unwrap_err()
                .code()
                .to_owned(),
        );
    }
    // Untouched, and the key that may name them all reaches it.
    let open = rig.endpoints.open(&rig.authz, &wide).unwrap();
    assert_eq!(open.get(id).await.unwrap().url, shared.url);
    open.test(id).await.unwrap();
    open.rotate_secret(id).await.unwrap();
    // An endpoint of its tenant's own events never takes a set, and one
    // of several tenants never loses it.
    let own_open = rig.endpoints.open(&rig.authz, &wide).unwrap();
    let error = own_open
        .update(
            own.id.as_str(),
            UpdateEndpoint {
                tenants: Some(vec![a.clone()]),
                ..UpdateEndpoint::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        (error.code(), error.field()),
        ("invalid_request", Some("tenants"))
    );
    let error = open
        .update(
            id,
            UpdateEndpoint {
                tenants: Some(Vec::new()),
                ..UpdateEndpoint::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        (error.code(), error.field()),
        ("invalid_request", Some("tenants"))
    );
    open.delete(id).await.unwrap();
}

/// M2m: a change of `tenants` replaces the set. A tenant kept keeps its
/// place in its stream (an event not yet fanned out is still delivered);
/// one added receives from the change on; one left out stops at once, its
/// pending deliveries deleted; the key must be able to name each.
pub async fn a_platform_endpoint_changes_its_tenants(backend: &dyn Backend) {
    drain(backend.webhooks().as_ref()).await;
    let rig = rig(backend, RetrySchedule::default(), fast_settings());
    let (x, a, b, c) = (
        unique_tenant("wh-pu-x"),
        unique_tenant("wh-pu-a"),
        unique_tenant("wh-pu-b"),
        unique_tenant("wh-pu-c"),
    );
    let digits: Vec<String> = (0..3).map(|_| super::unique_digits()).collect();
    for t in [&a, &b, &c] {
        let _ = backend
            .records()
            .create_tenant(&tenant(t), "")
            .await
            .unwrap();
    }
    let wide = platform_caller(backend, &rig.authz, &x, AllowedTenants::All).await;
    let narrow = platform_caller(backend, &rig.authz, &x, only(&[&x, &a, &b])).await;
    let open = rig.endpoints.open(&rig.authz, &wide).unwrap();
    let (endpoint, _) = open
        .create(naming("https://a.example.net/hook", &[&a, &b]))
        .await
        .unwrap();
    // A's event is fanned out (pending), B's is not yet; C's precedes the
    // change.
    let a1 = event_for(backend, &a, &digits[0]).await;
    rig.dispatcher.fan_out().await.unwrap();
    let b1 = event_for(backend, &b, &digits[1]).await;
    let _c0 = event_for(backend, &c, &digits[2]).await;
    let id = endpoint.id.as_str();
    let error = rig
        .endpoints
        .open(&rig.authz, &narrow)
        .unwrap()
        .update(
            id,
            UpdateEndpoint {
                tenants: Some(vec![b.clone(), c.clone()]),
                ..UpdateEndpoint::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        "forbidden",
        "C is outside the narrow key's set"
    );
    let changed = open
        .update(
            id,
            UpdateEndpoint {
                tenants: Some(vec![b.clone(), c.clone(), b.clone()]),
                ..UpdateEndpoint::default()
            },
        )
        .await
        .unwrap();
    let names: Vec<String> = changed
        .tenants
        .unwrap()
        .iter()
        .map(|t| t.tenant.to_string())
        .collect();
    assert_eq!(names, [b.clone(), c.clone()]);
    let c1 = event_for(backend, &c, &digits[2]).await;
    let a2 = event_for(backend, &a, &digits[0]).await;
    assert_eq!(
        round(&rig).await,
        2,
        "B's kept place, and C from the change"
    );
    assert_eq!(
        got(&rig, "a.example.net"),
        sorted(&[&b1, &c1]),
        "not A's (pending or later), not C's before the change ({}, {})",
        a1.id,
        a2.id
    );
    let listed = open.deliveries(id, &page(10)).await.unwrap().items;
    assert_eq!(listed.len(), 2, "A's pending delivery went with A");
}

/// M2m: a named tenant that is deleted leaves the endpoint, with its
/// deliveries, and a tenant created later under the same id does not
/// rejoin it; one that is suspended has its deliveries wait; when every
/// named tenant is gone the endpoint receives nothing, and not the events
/// of the tenant it was made as.
pub async fn a_deleted_or_suspended_named_tenant_does_not_widen_the_endpoint(
    backend: &dyn Backend,
) {
    drain(backend.webhooks().as_ref()).await;
    let rig = rig(backend, RetrySchedule::default(), fast_settings());
    let (x, a, b) = (
        unique_tenant("wh-pd-x"),
        unique_tenant("wh-pd-a"),
        unique_tenant("wh-pd-b"),
    );
    let digits: Vec<String> = (0..3).map(|_| super::unique_digits()).collect();
    for t in [&a, &b] {
        let _ = backend
            .records()
            .create_tenant(&tenant(t), "")
            .await
            .unwrap();
    }
    let wide = platform_caller(backend, &rig.authz, &x, AllowedTenants::All).await;
    let open = rig.endpoints.open(&rig.authz, &wide).unwrap();
    let (endpoint, _) = open
        .create(naming("https://a.example.net/hook", &[&a, &b]))
        .await
        .unwrap();
    let a1 = event_for(backend, &a, &digits[0]).await;
    let b1 = event_for(backend, &b, &digits[1]).await;
    assert_eq!(round(&rig).await, 2);
    // A suspended tenant's deliveries wait, and go when it is active.
    let records = backend.records();
    records
        .update_tenant(&tenant(&b), None, Some(TenantStatus::Suspended))
        .await
        .unwrap();
    let b2 = event_for(backend, &b, &digits[1]).await;
    assert_eq!(round(&rig).await, 0, "suspended: waits");
    records
        .update_tenant(&tenant(&b), None, Some(TenantStatus::Active))
        .await
        .unwrap();
    assert_eq!(round(&rig).await, 1);
    // A is deleted: it leaves the endpoint, and its pending delivery.
    let a2 = event_for(backend, &a, &digits[0]).await;
    rig.dispatcher.fan_out().await.unwrap();
    // (A tenant with a WABA is not deleted: the operator unbinds it first.)
    let unbind = |pn: String| async move {
        assert!(
            backend
                .records()
                .unbind_waba(&WabaId::new(waba_of(&pn)).unwrap())
                .await
                .unwrap()
        );
    };
    unbind(digits[0].clone()).await;
    assert_eq!(
        records.delete_tenant(&tenant(&a)).await.unwrap(),
        DeleteTenantOutcome::Deleted
    );
    let left = open.get(endpoint.id.as_str()).await.unwrap();
    let names: Vec<String> = left
        .tenants
        .unwrap()
        .iter()
        .map(|t| t.tenant.to_string())
        .collect();
    assert_eq!(names, std::slice::from_ref(&b), "B only: never widened");
    assert_eq!(round(&rig).await, 0, "A's pending delivery went with it");
    // The same id again is a new tenant, not a named one.
    records.create_tenant(&tenant(&a), "").await.unwrap();
    let a3 = event_for(backend, &a, &super::unique_digits()).await;
    let b3 = event_for(backend, &b, &digits[1]).await;
    assert_eq!(round(&rig).await, 1, "B's only");
    assert_eq!(
        got(&rig, "a.example.net"),
        sorted(&[&a1, &b1, &b2, &b3]),
        "{} and {} never ({})",
        a2.id,
        a3.id,
        left.id
    );
    // All gone: nothing, and not the owner's events either.
    unbind(digits[1].clone()).await;
    assert_eq!(
        records.delete_tenant(&tenant(&b)).await.unwrap(),
        DeleteTenantOutcome::Deleted
    );
    let left = open.get(endpoint.id.as_str()).await.unwrap();
    assert_eq!(left.tenants, Some(Vec::new()), "still an endpoint of none");
    let x1 = event_for(backend, &x, &digits[2]).await;
    assert_eq!(round(&rig).await, 0);
    assert!(!got(&rig, "a.example.net").contains(&x1.id));
}

/// M2m at the store (D342, D344, D347): a change applies only while the
/// endpoint's set is the one the caller was checked against (a set moved
/// meanwhile answers as a missing endpoint, the URL untouched); a tenant
/// kept keeps its cursor and its pending delivery, one dropped loses its
/// pending delivery, one added starts at the cursor given; a fan-out is
/// for a tenant the endpoint receives, from that tenant's cursor; a named
/// tenant that does not exist refuses the creation.
pub async fn a_platform_endpoint_store_keeps_cursors_and_applies_only_to_the_set_checked(
    backend: &dyn Backend,
) {
    use crate::webhooks_out::TenantCursor;
    let store = backend.webhooks();
    drain(store.as_ref()).await;
    let (x, a, b, c, gone) = (
        unique_tenant("wh-ps-x"),
        unique_tenant("wh-ps-a"),
        unique_tenant("wh-ps-b"),
        unique_tenant("wh-ps-c"),
        unique_tenant("wh-ps-g"),
    );
    for t in [&x, &a, &b, &c] {
        let _ = backend
            .records()
            .create_tenant(&tenant(t), "")
            .await
            .unwrap();
    }
    let cursor = |t: &str, n: i64| TenantCursor {
        tenant: tenant(t),
        cursor: n,
    };
    // A named tenant that does not exist: nothing is created.
    let mut refused = new_endpoint(&x, 0);
    refused.tenants = Some(vec![cursor(&a, 1), cursor(&gone, 1)]);
    assert!(matches!(
        store.create_endpoint(&refused, 10).await.unwrap(),
        CreateOutcome::NamedTenantGone
    ));
    assert!(
        store
            .endpoint(&tenant(&x), &refused.id)
            .await
            .unwrap()
            .is_none()
    );
    let mut named = new_endpoint(&x, 0);
    named.tenants = Some(vec![cursor(&a, 10), cursor(&b, 20)]);
    let endpoint = created(store.as_ref(), &named).await;
    let own = created(store.as_ref(), &new_endpoint(&x, 0)).await;
    let at = START;
    // A fan-out names a tenant of the endpoint and that tenant's cursor.
    assert!(
        !store
            .fan_out(&endpoint.id, &tenant(&c), 10, 11, &[])
            .await
            .unwrap(),
        "C is not one it receives, though 10 is A's cursor"
    );
    assert!(
        !store
            .fan_out(&endpoint.id, &tenant(&b), 10, 11, &[])
            .await
            .unwrap(),
        "10 is A's cursor, not B's"
    );
    assert!(
        !store
            .fan_out(&endpoint.id, &tenant(&x), 0, 1, &[])
            .await
            .unwrap(),
        "the owner is not named: its endpoint cursor is not a stream of this one"
    );
    assert!(
        store
            .fan_out(
                &endpoint.id,
                &tenant(&a),
                10,
                11,
                &[delivery("evt-ps-a1", 11, at)]
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .fan_out(
                &endpoint.id,
                &tenant(&b),
                20,
                21,
                &[delivery("evt-ps-b1", 21, at)]
            )
            .await
            .unwrap()
    );
    let pending = |tenants: Vec<crate::webhooks_out::Delivery>| {
        let mut ids: Vec<String> = tenants
            .iter()
            .map(|d| format!("{}:{}", d.tenant, d.event_id.clone().unwrap_or_default()))
            .collect();
        ids.sort();
        ids
    };
    let listed = |store: std::sync::Arc<dyn WebhookStore>, id: EndpointId| {
        let owner = tenant(&x);
        async move {
            store
                .deliveries(&owner, &id, &page(50))
                .await
                .unwrap()
                .unwrap()
                .items
        }
    };
    let change =
        |through: Option<Vec<&str>>, url: &str, set: Option<Vec<TenantCursor>>| EndpointChange {
            url: Some(url.to_owned()),
            tenants: set,
            reached_through: through.map(|t| t.into_iter().map(tenant).collect()),
            ..EndpointChange::default()
        };
    let now = START;
    // The set moved meanwhile, or the caller was checked against another:
    // nothing changes.
    for stale in [
        None,
        Some(vec![a.as_str()]),
        Some(vec![a.as_str(), b.as_str(), c.as_str()]),
        Some(vec![a.as_str(), c.as_str()]),
    ] {
        assert!(
            store
                .update_endpoint(
                    &tenant(&x),
                    &endpoint.id,
                    &change(stale.clone(), "https://evil.example.net/hook", None),
                    now
                )
                .await
                .unwrap()
                .is_none(),
            "{stale:?}"
        );
    }
    assert!(
        store
            .update_endpoint(
                &tenant(&x),
                &own.id,
                &change(
                    Some(vec![a.as_str()]),
                    "https://evil.example.net/hook",
                    None
                ),
                now
            )
            .await
            .unwrap()
            .is_none(),
        "an endpoint without a set is not one with it"
    );
    let untouched = store
        .endpoint(&tenant(&x), &endpoint.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(untouched.url, "https://hooks.example.com/wa");
    assert_eq!(
        store
            .endpoint(&tenant(&x), &own.id)
            .await
            .unwrap()
            .unwrap()
            .url,
        "https://hooks.example.com/wa"
    );
    // The set as checked (in another order): applied. A kept (its cursor
    // as it is, whatever the change gives), B dropped, C added at 5.
    let changed = store
        .update_endpoint(
            &tenant(&x),
            &endpoint.id,
            &change(
                Some(vec![b.as_str(), a.as_str()]),
                "https://ok.example.net/hook",
                Some(vec![cursor(&a, 999), cursor(&c, 5)]),
            ),
            now,
        )
        .await
        .unwrap()
        .expect("the set is the one checked");
    assert_eq!(changed.url, "https://ok.example.net/hook");
    let mut set: Vec<(String, i64)> = changed
        .tenants
        .unwrap()
        .iter()
        .map(|t| (t.tenant.to_string(), t.cursor))
        .collect();
    set.sort();
    let mut want = vec![(a.clone(), 11), (c.clone(), 5)];
    want.sort();
    assert_eq!(set, want, "A keeps its cursor, C starts at the one given");
    assert_eq!(
        pending(listed(store.clone(), endpoint.id.clone()).await),
        [format!("{a}:evt-ps-a1")],
        "B's pending delivery went with B; A's stays"
    );
    // A tenant deleted meanwhile is left out of a change, never added.
    let with_gone = store
        .update_endpoint(
            &tenant(&x),
            &endpoint.id,
            &change(
                Some(vec![a.as_str(), c.as_str()]),
                "https://ok.example.net/hook",
                Some(vec![cursor(&a, 0), cursor(&gone, 0)]),
            ),
            now,
        )
        .await
        .unwrap()
        .unwrap();
    let names: Vec<String> = with_gone
        .tenants
        .unwrap()
        .iter()
        .map(|t| t.tenant.to_string())
        .collect();
    assert_eq!(names, [a.clone()], "{gone} does not exist");
    // And the endpoint without a set takes its own change.
    let own_changed = store
        .update_endpoint(
            &tenant(&x),
            &own.id,
            &change(None, "https://ok.example.net/own", None),
            now,
        )
        .await
        .unwrap()
        .expect("no set expected, none held");
    assert_eq!(own_changed.url, "https://ok.example.net/own");
}
