//! The executable definition of the ports' contracts (feature
//! `conformance`): what every [`Backend`] must do, run over the bundle, as
//! the library's `meta_whatsapp_adapters::store::conformance` is for a
//! `KvStore`. The service's memory and Postgres backends run it in its
//! tests; a backend of your own (a MongoDB one, say) runs it in
//! yours:
//!
//! ```ignore
//! use std::sync::Arc;
//! use meta_whatsapp_server_core::backend::Backend;
//!
//! #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
//! async fn my_backend_passes_the_conformance_suite() {
//!     meta_whatsapp_server_core::conformance::run(async || {
//!         Arc::new(MyBackend::connect_to_an_empty_database().await) as Arc<dyn Backend>
//!     })
//!     .await;
//! }
//! ```
//!
//! [`run`] asks `fresh` for one **empty** backend per suite (the suites
//! list every tenant and page every stream, so another suite's records
//! would show up), and closes each one ([`Backend::close`]) once its suite
//! is done. A multi-threaded runtime makes the [`races`] race for real;
//! on a current-thread one they interleave only at awaits.
//!
//! | Suite | What it holds a backend to |
//! | --- | --- |
//! | [`records`] | [`RecordStore`] and [`IdempotencyRecords`]: tenants, keys, bindings (decision D4), idempotency keys gone with their tenant, the referential rules (roadmap S2: `NoSuchTenant`, a binding and a deletion of one tenant serialize), byte order |
//! | [`outbox`] | [`Outbox`]: each tenant's own stream, pages and their byte budget, dedup and its window, filters, purges per stream that take no lock |
//! | [`invariants`] | what crosses ports, which a backend made of loose stores loses: every accessor hands out the same data; deleting a tenant purges its stream; the routing checked again with each insert (the [`RouteGuard`] cases: a binding moved before the insert, begun after the event, of the event's own second); each tenant's inserts commit in sequence order |
//! | [`capabilities`] | through the core's [`Authorizer`]: a capability acts only on the binding and the vault record it was made from (roadmap S2, SR-L2, the `failed` race, M1) |
//! | [`races`] | the atomic contracts raced on two tasks: an insert against its binding moving, a binding against its tenant's deletion, each conditioned write against a re-attach |
//! | [`admin`] | the core's admin services over the bundle, with no HTTP layer (roadmap S5a): attaching a WABA another tenant holds is refused with nothing stored, subscribed or shared, whether its holder is seen before Meta is asked or only by the binding's own check |
//!
//! Every case panics on a broken contract, with what broke: this is test
//! code.
//!
//! [`RecordStore`]: crate::store::RecordStore
//! [`IdempotencyRecords`]: crate::store::IdempotencyRecords
//! [`Outbox`]: crate::outbox::Outbox
//! [`RouteGuard`]: crate::outbox::RouteGuard
//! [`Authorizer`]: crate::authz::Authorizer
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::too_many_lines
)] // test code: a panic is the report; its scenarios read top to bottom

pub mod admin;
pub mod capabilities;
pub mod invariants;
pub mod outbox;
pub mod races;
pub mod records;

use std::sync::Arc;

use crate::backend::Backend;

/// Every suite, each on a backend of its own from `fresh` (an empty one
/// each call), closed once its suite is done. What the races saw.
pub async fn run<F>(fresh: F) -> races::Report
where
    F: AsyncFn() -> Arc<dyn Backend>,
{
    let backend = fresh().await;
    records::run(backend.as_ref()).await;
    backend.close().await;

    let backend = fresh().await;
    outbox::run(backend.as_ref()).await;
    backend.close().await;

    let backend = fresh().await;
    invariants::run(backend.as_ref()).await;
    backend.close().await;

    let backend = fresh().await;
    capabilities::run(backend.as_ref()).await;
    backend.close().await;

    let backend = fresh().await;
    let report = races::run(backend.as_ref()).await;
    backend.close().await;

    let backend = fresh().await;
    admin::run(backend.as_ref()).await;
    backend.close().await;
    report
}

/// A lowercase `[a-z0-9_]` token unique to this call: ids of their own for
/// the cases that run more than once on one backend.
pub(crate) fn unique() -> String {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    format!("{t:x}_{}_{n}", std::process::id())
}

/// [`unique`] as digits, for a numeric id (a WABA or phone number id is
/// digits only: `meta_whatsapp_rs::core::ids`): the time, the process id
/// and a counter as fixed-width decimals, 40 digits, so the caller may
/// append a few of its own within the 64 allowed.
pub(crate) fn unique_digits() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = time::OffsetDateTime::now_utc()
        .unix_timestamp_nanos()
        .unsigned_abs();
    format!("{t:020}{:010}{n:010}", std::process::id())
}
