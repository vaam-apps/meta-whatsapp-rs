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
//! | [`records`] | [`RecordStore`] and [`IdempotencyRecords`]: tenants, keys, bindings (decision D4), idempotency keys gone with their tenant, the referential rules (roadmap S2: `NoSuchTenant`, a binding and a deletion of one tenant serialize), byte order; a WABA's moves recorded by its tombstone, a deleted tenant counting as another, and the bind generation a refresh moves (roadmap S2b); a number's moves recorded by its own tombstone, its start kept by a refresh and a move within its tenant (roadmap M2h) |
//! | [`outbox`] | [`Outbox`]: each tenant's own stream, pages and their byte budget, dedup and its window, filters, purges per stream that take no lock |
//! | [`invariants`] | what crosses ports, which a backend made of loose stores loses: every accessor hands out the same data; deleting a tenant purges its stream; the routing checked again with each insert (the [`RouteGuard`] cases: a binding moved before the insert, begun after the event, of the event's own second after a move; since S2b a first onboarding's first second, the skew margin after a move only, an undated event within the window after a move); each tenant's inserts commit in sequence order |
//! | [`capabilities`] | through the core's [`Authorizer`]: a capability acts only on the binding and the vault record it was made from (roadmap S2, SR-L2, the `failed` race, M1), and not on a binding refreshed since (S2b, the bind generation) |
//! | [`races`] | the atomic contracts raced on two tasks: an insert against its binding moving, a binding against its tenant's deletion, another tenant's WABA taking a number against its release (roadmap M2h), each conditioned write against a re-attach |
//! | [`admin`] | the core's admin services over the bundle, with no HTTP layer (roadmap S5a): attaching a WABA another tenant holds is refused with nothing stored, subscribed or shared, whether its holder is seen before Meta is asked or only by the binding's own check |
//! | [`messages`] | the core's message services under its idempotency engine, over the bundle's records, vault and [`IdempotencyRecords`], with no HTTP layer (roadmap S5b, M1.4): a digits-only phone number refused before any request; a timeout `may_have_been_sent`, its key replaying it with no second request; a `131047` releasing its key; another authorizer's caller refused before the key is read |
//! | [`templates`] | the core's template services over the bundle's records and vault, with no HTTP layer (roadmap S5c): tenant B's caller on tenant A's template id is not found with zero vault reads (A's WABA not B's, A's capability refused to B, A's id not in B's WABA, nothing deleted); another authorizer's caller or capability refused; Meta's documented requests, the cache and its invalidation (by a creation or deletion Meta refuses too); a template id past the lookup's 5 pages not found, nothing deleted |
//! | [`media`] | the core's media services over the bundle's records and vault, with no HTTP layer (roadmap S5d): a download through the core's stream ends before its last chunk on a digest mismatch (M1b's rule), passes the file on while it arrives (one chunk held), and stops past `max_bytes`; a failed or oddly chunked download (empty chunks, a cut connection, one chunk, none) never yields its last chunk unverified, and gives its slot back on every path; a `190` from Meta's URL marks the number; a whole download answers no byte of a tampered file; the reported size, digest and URL checked before the download; another number's media id not found, nothing deleted; Meta's documented upload and deletion; only the order's own capability; a slot of the tenant's share first |
//! | [`numbers`] | the core's number services over the bundle's records and vault, with no HTTP layer (roadmap S5c): only the order's own capabilities; a number's details and profile, a change of it (an empty one only reads, a refused one sends nothing); a disconnection deleting nothing until Meta unsubscribed the app, and nothing for a WABA that moved |
//! | [`live`] | the bundle's [`EventNotifier`] and the core's live streams over it (roadmap M2b): the notifier wakes its tenant's subscriptions only, through any of the bundle's handles; a stream sends what the outbox holds after its cursor and resumes exactly after it (`Last-Event-ID`); only its tenant's events, never an `unknown` or `unparsed` one (M2.3, through the pipeline); a lost notification still delivers (the re-poll); a cursor past the newest sequence `422` and one below a purge `410` before the stream starts; streams bounded per tenant and replica (`429 too_many_streams`); a slow client dropped with `lagged`, its buffer within its budget; a shutdown ends every stream; an event read by id in its tenant's stream only; a number stream on the tenant's own number only; a stream behind a purge ends `lagged`. M2.5's load (`live::many_streams_at_a_steady_rate_hold_bounded_memory`) is a case of its own, not in `run` |
//! | [`inbox`] | the core's inbox services over the bundle's records, vault and inbox, the webhooks recorded by the core's pipeline, with no HTTP layer (roadmap M2a): M2.1 (an inbound message listed, read, answered within its window, Meta's documented request, recorded `accepted`, then `delivered` by its status; a free-form reply outside the window `409` with zero requests, a template not; pages by the store's cursor); the binding epoch (a moved number's history hidden from its new tenant and kept, its own shown, a first onboarding's synced history shown; a number migrated between two tenants' WABAs, neither moving, cut at its own start (M2h); a capability made before the move reads nothing); `control_taken` refusing a free-form reply `409 thread_owned_elsewhere` with zero requests, unless the number's ownership check is off; only the order's own number; housekeeping applying the inbox store's retention |
//! | [`webhooks`] | the core's webhook pipeline over the bundle's records, key/value store, inbox and outbox, with no listener (roadmap S5e, M1.2): routing is an allow-list (an unowned number's event, a stale binding's, a replay's are operator-only, whatever the payload says of a tenant; an event dated before its binding, or seen past the replay window, too; a number moved while its event records gives it to neither tenant); the signature checked before any store is touched or a turn waited for; at most `MAX_DELIVERIES_RECORDING` deliveries recording, another waiting with no lease claimed (its `503 busy` after the wait is `webhooks::a_delivery_with_no_turn_is_busy_after_the_wait`, on a paused clock, not in `run`); each event once, under its dedup lease; the answer only once every event is recorded, a failed or busy outbox refused and redelivered once; `unknown` and `unparsed` acknowledged as operator-only rows; after a move, undated events (a history chunk, an error, an undated group update) reaching no tenant within the window, the new holder under `CurrentHolder` or past it, a first onboarding always, and an event of the binding's first second a first onboarding's only (roadmap S2b, design D36); an address book change reaches its tenant and the inbox keeps no synced contact (before roadmap M3b, design D38) |
//!
//! Every case panics on a broken contract, with what broke: this is test
//! code.
//!
//! [`RecordStore`]: crate::store::RecordStore
//! [`IdempotencyRecords`]: crate::store::IdempotencyRecords
//! [`Outbox`]: crate::outbox::Outbox
//! [`EventNotifier`]: crate::notify::EventNotifier
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
pub mod inbox;
pub mod invariants;
pub mod live;
pub mod media;
pub mod messages;
pub mod numbers;
pub mod outbox;
pub mod races;
pub mod records;
pub mod templates;
pub mod webhooks;

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

    let backend = fresh().await;
    messages::run(backend.as_ref()).await;
    backend.close().await;

    let backend = fresh().await;
    templates::run(backend.as_ref()).await;
    backend.close().await;

    let backend = fresh().await;
    numbers::run(backend.as_ref()).await;
    backend.close().await;

    let backend = fresh().await;
    media::run(backend.as_ref()).await;
    backend.close().await;

    let backend = fresh().await;
    webhooks::run(backend.as_ref()).await;
    backend.close().await;

    let backend = fresh().await;
    live::run(backend.as_ref()).await;
    backend.close().await;

    let backend = fresh().await;
    inbox::run(backend.as_ref()).await;
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
