//! The core's conformance suites (`meta_whatsapp_server_core::conformance`)
//! on the memory backend (Postgres: the store-postgres crate's
//! `tests/live_postgres.rs`), and what they refuse: a backend whose `kv()`
//! is a new store on each call, and an outbox that ignores the route guard
//! or bends its date (S2's security review, L4 and L3), each refused by
//! `conformance::run` itself, not only by the case that catches it. (The
//! service's `tests/store.rs` until roadmap S7.)
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt as _;
use meta_whatsapp_rs::adapters::store::MemoryKvStore;
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_server_core::backend::{Backend, BackendKind};
use meta_whatsapp_server_core::conformance;
use meta_whatsapp_server_core::model::TenantId;
use meta_whatsapp_server_core::notify::{EventNotifier, Hub, LocalNotifier, Subscription, Wakeup};
use meta_whatsapp_server_core::outbox::{
    AfterMove, EventPage, EventQuery, GuardedBinding, NewEvent, Outbox, RouteGuard, StoredEvent,
};
use meta_whatsapp_server_core::store::{
    IdempotencyRecords, Janitor, LeaderLock, RecordStore, SchemaMigrator, StoreResult,
};
use meta_whatsapp_server_store_memory::MemoryBackend;

/// Roadmap S3: every suite, each on a memory backend of its own, raced on
/// four threads (`conformance::races`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_memory_backend_passes_the_conformance_suite() {
    let report =
        conformance::run(async || Arc::new(MemoryBackend::new()) as Arc<dyn Backend>).await;
    eprintln!("memory, the races: {report:?}");
}

/// A memory backend with one port swapped: what the suites must refuse.
struct Altered {
    inner: MemoryBackend,
    /// `kv()` builds a new store on each call (each call's writes lost to
    /// the next).
    fresh_kv: bool,
    /// The outbox, instead of the memory store's own.
    outbox: Option<Arc<dyn Outbox>>,
    /// `notifier()` builds a new notifier on each call (a subscription
    /// through one handle never hears a notification through another).
    fresh_notifier: bool,
    /// The notifier, instead of the backend's own.
    notifier: Option<Arc<dyn EventNotifier>>,
}

#[async_trait]
impl Backend for Altered {
    fn kind(&self) -> BackendKind {
        BackendKind::Other("altered")
    }
    fn records(&self) -> Arc<dyn RecordStore> {
        self.inner.records()
    }
    fn idempotency(&self) -> Arc<dyn IdempotencyRecords> {
        self.inner.idempotency()
    }
    fn outbox(&self) -> Arc<dyn Outbox> {
        self.outbox.clone().unwrap_or_else(|| self.inner.outbox())
    }
    fn notifier(&self) -> Arc<dyn EventNotifier> {
        if self.fresh_notifier {
            return Arc::new(LocalNotifier::new());
        }
        self.notifier
            .clone()
            .unwrap_or_else(|| self.inner.notifier())
    }
    fn leader_lock(&self) -> Arc<dyn LeaderLock> {
        self.inner.leader_lock()
    }
    fn janitor(&self) -> Arc<dyn Janitor> {
        self.inner.janitor()
    }
    fn migrator(&self) -> Arc<dyn SchemaMigrator> {
        self.inner.migrator()
    }
    fn kv(&self) -> Arc<dyn KvStore> {
        if self.fresh_kv {
            Arc::new(MemoryKvStore::new())
        } else {
            self.inner.kv()
        }
    }
    fn conversations(&self) -> Arc<dyn ConversationStore> {
        self.inner.conversations()
    }
    async fn close(&self) {}
}

/// The message `case` panicked with; `None` when it passed.
async fn failure(case: impl Future<Output = ()>) -> Option<String> {
    let payload = AssertUnwindSafe(case).catch_unwind().await.err()?;
    Some(
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_default(),
    )
}

/// `conformance::run` over the backends `fresh` makes: the message it
/// panicked with; `None` when it passed.
async fn whole_suite_failure<F>(fresh: F) -> Option<String>
where
    F: AsyncFn() -> Arc<dyn Backend>,
{
    failure(async {
        conformance::run(fresh).await;
    })
    .await
}

/// Roadmap S3's decisive case for the bundle: a `Backend` whose `kv()`
/// returns a new store on each call fails the same-data case, at the
/// read through the second handle, and so the whole suite.
#[tokio::test]
async fn a_backend_whose_kv_is_new_on_each_call_fails_the_same_data_case() {
    let backend = Altered {
        inner: MemoryBackend::new(),
        fresh_kv: true,
        outbox: None,
        fresh_notifier: false,
        notifier: None,
    };
    let failed =
        failure(conformance::invariants::every_accessor_hands_out_the_same_data(&backend)).await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("written through another kv() handle")),
        "{failed:?}"
    );
    let failed = whole_suite_failure(async || {
        Arc::new(Altered {
            inner: MemoryBackend::new(),
            fresh_kv: true,
            outbox: None,
            fresh_notifier: false,
            notifier: None,
        }) as Arc<dyn Backend>
    })
    .await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("written through another kv() handle")),
        "the whole suite: {failed:?}"
    );
    // The control: the same kind of backend, with its own kv(), passes.
    let backend = Altered {
        inner: MemoryBackend::new(),
        fresh_kv: false,
        outbox: None,
        fresh_notifier: false,
        notifier: None,
    };
    assert_eq!(
        failure(conformance::invariants::every_accessor_hands_out_the_same_data(&backend)).await,
        None
    );
}

/// An outbox that keeps every row's tenant, whatever its route guard says
/// (each tenant's stream from 1, no dedup, no purge): a third-party outbox
/// that ignores the guard, which fails open.
#[derive(Default)]
struct IgnoresTheGuard {
    rows: Mutex<Vec<StoredEvent>>,
}

#[async_trait]
impl Outbox for IgnoresTheGuard {
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>> {
        let mut rows = self.rows.lock().unwrap();
        let sequence =
            1 + i64::try_from(rows.iter().filter(|row| row.tenant == event.tenant).count())
                .unwrap();
        rows.push(StoredEvent {
            sequence,
            id: event.id.clone(),
            tenant: event.tenant.clone(),
            phone_number_id: event.phone_number_id.clone(),
            waba_id: event.waba_id.clone(),
            event_type: event.event_type.clone(),
            data: event.data.clone(),
            created_at: time::OffsetDateTime::now_utc(),
        });
        Ok(Some(sequence))
    }

    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage> {
        let rows = self.rows.lock().unwrap();
        let stream: Vec<&StoredEvent> = rows
            .iter()
            .filter(|row| row.tenant.as_ref() == Some(&query.tenant))
            .collect();
        let after = query.after.unwrap_or(0);
        let events: Vec<StoredEvent> = stream
            .iter()
            .filter(|row| row.sequence > after)
            .take(query.limit)
            .map(|row| (*row).clone())
            .collect();
        let high_water = stream.last().map_or(0, |row| row.sequence);
        Ok(EventPage {
            more: events.last().is_some_and(|last| last.sequence < high_water),
            events,
            purged_through: 0,
            high_water,
        })
    }

    async fn purge(&self, _older_than: Duration) -> StoreResult<u64> {
        Ok(0)
    }
}

/// S2's security review, L4, closed by roadmap S3: an outbox that ignores
/// the route guard (a tenant kept though its binding moved, began after
/// the event, or was never named) fails the guard's cases.
#[tokio::test]
async fn an_outbox_that_ignores_the_route_guard_fails_the_suite() {
    let backend = Altered {
        inner: MemoryBackend::new(),
        fresh_kv: false,
        outbox: Some(Arc::new(IgnoresTheGuard::default())),
        fresh_notifier: false,
        notifier: None,
    };
    let failed =
        failure(conformance::invariants::the_route_guard_is_checked_with_the_insert(&backend))
            .await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("only the rows whose guard held")),
        "{failed:?}"
    );
    let failed = failure(
        conformance::invariants::a_binding_begun_after_the_event_fails_the_number_guard(&backend),
    )
    .await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("a number's binding begun after the event")),
        "{failed:?}"
    );
    let failed = failure(
        conformance::invariants::a_binding_of_the_events_own_second_fails_the_guard(&backend),
    )
    .await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("only the rows dated after the binding's own second")),
        "{failed:?}"
    );
    // Roadmap S2b's guard cases: a first onboarding's second before, a
    // moved WABA's margin, an undated row within the window.
    let failed =
        failure(conformance::invariants::a_first_onboarding_keeps_its_first_second(&backend)).await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("a first onboarding's first second is its own")),
        "{failed:?}"
    );
    let failed =
        failure(conformance::invariants::the_skew_margin_applies_after_a_move_only(&backend)).await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("after a move: only the rows dated past the skew margin")),
        "{failed:?}"
    );
    let failed = failure(
        conformance::invariants::an_undated_event_after_a_move_waits_out_the_window(&backend),
    )
    .await;
    assert!(
        failed.as_deref().is_some_and(
            |m| m.contains("after a move: an undated row within the window kept its tenant")
        ),
        "{failed:?}"
    );
    // And the suite of what crosses ports, as a backend runs it: on the
    // guard, not on what this outbox lacks besides (it purges nothing).
    let backend = Altered {
        inner: MemoryBackend::new(),
        fresh_kv: false,
        outbox: Some(Arc::new(IgnoresTheGuard::default())),
        fresh_notifier: false,
        notifier: None,
    };
    let failed = failure(conformance::invariants::run(&backend)).await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("only the rows whose guard held")),
        "invariants::run: {failed:?}"
    );
}

/// The memory backend's own outbox, handed each event with its route
/// guard rewritten: an outbox that checks a guard, but not quite the one
/// it was given.
struct RewritesTheGuard {
    inner: Arc<dyn Outbox>,
    rewrite: Rewrite,
}

/// How [`RewritesTheGuard`] bends a guard.
type Rewrite = fn(RouteGuard) -> RouteGuard;

#[async_trait]
impl Outbox for RewritesTheGuard {
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>> {
        let event = NewEvent {
            route_guard: event.route_guard.clone().map(self.rewrite),
            ..event.clone()
        };
        self.inner.insert(&event).await
    }

    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage> {
        self.inner.page(query).await
    }

    async fn purge(&self, older_than: Duration) -> StoreResult<u64> {
        self.inner.purge(older_than).await
    }
}

/// Each `RouteGuard` case is in `conformance::run`, not only callable: an
/// outbox that drops every guard's date fails the guard's first case
/// (its row dated before the binding began), one that drops a number
/// guard's date fails the number guard's case, one that reads the date a
/// second late fails the binding's own second after a move (L3), one that
/// drops the skew margin fails the margin's case, and one that drops the
/// window after a move fails the undated case (roadmap S2b). Each passes
/// every suite before `invariants`, and each fails the whole suite.
/// Decisive: removing a guard case from `invariants::run`.
#[tokio::test]
async fn an_outbox_that_bends_the_guards_date_fails_the_whole_suite() {
    let bent: [(&str, Rewrite, &str); 5] = [
        (
            "every guard's date dropped",
            |guard| RouteGuard {
                not_after: None,
                ..guard
            },
            "only the rows whose guard held",
        ),
        (
            "a number guard's date dropped",
            |guard| {
                if matches!(guard.binding, GuardedBinding::Number { .. }) {
                    RouteGuard {
                        not_after: None,
                        ..guard
                    }
                } else {
                    guard
                }
            },
            "a number's binding begun after the event",
        ),
        (
            "the date read a second late",
            |guard| RouteGuard {
                not_after: guard.not_after.map(|at| at + time::Duration::SECOND),
                ..guard
            },
            "only the rows dated after the binding's own second",
        ),
        (
            "the skew margin dropped",
            |guard| RouteGuard {
                after_move: AfterMove {
                    skew_margin: Duration::ZERO,
                    ..guard.after_move
                },
                ..guard
            },
            "after a move: only the rows dated past the skew margin",
        ),
        (
            "the window after a move dropped",
            |guard| RouteGuard {
                after_move: AfterMove {
                    undated_moved_by: None,
                    ..guard.after_move
                },
                ..guard
            },
            "after a move: an undated row within the window kept its tenant",
        ),
    ];
    for (how, rewrite, expected) in bent {
        let failed = whole_suite_failure(async || {
            let inner = MemoryBackend::new();
            let outbox: Arc<dyn Outbox> = Arc::new(RewritesTheGuard {
                inner: inner.outbox(),
                rewrite,
            });
            Arc::new(Altered {
                inner,
                fresh_kv: false,
                outbox: Some(outbox),
                fresh_notifier: false,
                notifier: None,
            }) as Arc<dyn Backend>
        })
        .await;
        assert!(
            failed.as_deref().is_some_and(|m| m.contains(expected)),
            "{how}: {failed:?}"
        );
    }
}

/// A notifier that wakes every subscription on any notification: one
/// channel for every tenant.
#[derive(Default)]
struct WakesEveryone {
    hub: Hub,
}

#[async_trait]
impl EventNotifier for WakesEveryone {
    async fn notify(&self, _: &Wakeup) -> StoreResult<()> {
        self.hub.wake_all();
        Ok(())
    }

    async fn subscribe(&self, tenant: &TenantId) -> StoreResult<Subscription> {
        Ok(self.hub.subscribe(tenant))
    }
}

/// Roadmap M2b: the notifier case refuses a backend whose `notifier()` is
/// new on each call (a subscription through one handle never hears a
/// notification through another: nothing would wake a stream on the
/// webhook pipeline's notifications) and one whose notifier wakes every
/// tenant; each fails `conformance::run` too, and the backend's own
/// passes. Decisive: the case in `live::run`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_notifier_new_on_each_call_or_waking_every_tenant_fails_the_suite() {
    let fresh = || Altered {
        inner: MemoryBackend::new(),
        fresh_kv: false,
        outbox: None,
        fresh_notifier: true,
        notifier: None,
    };
    let failed =
        failure(conformance::live::the_notifier_wakes_its_tenants_subscriptions_only(&fresh()))
            .await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("subscription to a's stream was not woken")),
        "{failed:?}"
    );
    let everyone = || Altered {
        inner: MemoryBackend::new(),
        fresh_kv: false,
        outbox: None,
        fresh_notifier: false,
        notifier: Some(Arc::new(WakesEveryone::default())),
    };
    let failed =
        failure(conformance::live::the_notifier_wakes_its_tenants_subscriptions_only(&everyone()))
            .await;
    assert!(
        failed
            .as_deref()
            .is_some_and(|m| m.contains("woken by a's event")),
        "{failed:?}"
    );
    let failed = whole_suite_failure(async || Arc::new(fresh()) as Arc<dyn Backend>).await;
    assert!(
        failed.is_some(),
        "the whole suite passed a notifier new on each call"
    );
    let failed = whole_suite_failure(async || Arc::new(everyone()) as Arc<dyn Backend>).await;
    assert!(
        failed.is_some(),
        "the whole suite passed a notifier waking every tenant"
    );
    assert_eq!(
        failure(
            conformance::live::the_notifier_wakes_its_tenants_subscriptions_only(
                &MemoryBackend::new()
            )
        )
        .await,
        None,
        "the control"
    );
}

/// An outbox whose pages leave the tenant out: the asked tenant's page
/// with the other tenants' events after the same cursor mixed in.
struct IgnoresTheTenant {
    inner: Arc<dyn Outbox>,
    others: Vec<TenantId>,
}

#[async_trait]
impl Outbox for IgnoresTheTenant {
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>> {
        self.inner.insert(event).await
    }

    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage> {
        let mut page = self.inner.page(query).await?;
        for other in self.others.iter().filter(|other| **other != query.tenant) {
            let theirs = self
                .inner
                .page(&EventQuery {
                    tenant: other.clone(),
                    ..query.clone()
                })
                .await?;
            page.events.extend(theirs.events);
            page.high_water = page.high_water.max(theirs.high_water);
        }
        page.events.sort_by_key(|event| event.sequence);
        Ok(page)
    }

    async fn purge(&self, older_than: Duration) -> StoreResult<u64> {
        self.inner.purge(older_than).await
    }
}

/// Roadmap M2b's decisive case for the tenant filter: an outbox whose
/// pages leave the tenant out hands tenant B's stream tenant A's events,
/// and the live suite's tenant case fails on it; the backend's own
/// passes. Decisive: the tenant in every read of a stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_the_tenant_filter_on_the_stream_fails_the_tenant_case() {
    let inner = MemoryBackend::new();
    let backend = Altered {
        outbox: Some(Arc::new(IgnoresTheTenant {
            inner: inner.outbox(),
            others: ["live-tenant-a", "live-tenant-b"]
                .map(|t| TenantId::parse(t).unwrap())
                .to_vec(),
        })),
        inner,
        fresh_kv: false,
        fresh_notifier: false,
        notifier: None,
    };
    let failed = failure(conformance::live::a_stream_sends_only_its_tenants_events(
        &backend,
    ))
    .await;
    assert!(
        failed.is_some(),
        "a stream without its tenant filter passed"
    );
    assert_eq!(
        failure(conformance::live::a_stream_sends_only_its_tenants_events(
            &MemoryBackend::new()
        ))
        .await,
        None,
        "the control"
    );
}

/// M2.5 on memory: 200 open streams (ten tenants of 20, half of them never
/// read) at 100 events a second for 3 seconds: every read stream gets
/// every event of its tenant in order, the unread ones are dropped as
/// slow, and the data held for clients never passes each stream's budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_hundred_streams_at_a_hundred_events_a_second_hold_bounded_memory() {
    let backend = MemoryBackend::new();
    let load =
        conformance::live::many_streams_at_a_steady_rate_hold_bounded_memory(&backend, 200, 100, 3)
            .await;
    eprintln!("memory, M2.5: {load:?}");
    assert_eq!((load.streams, load.events), (200, 300));
    // Each event reaches its tenant's ten read streams.
    assert_eq!(load.delivered, load.events * 10);
    assert!(load.peak_buffered_bytes <= load.streams * load.budget_per_stream);
}
