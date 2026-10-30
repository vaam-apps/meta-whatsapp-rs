//! Roadmap S5e's decisive case, in this crate alone: no listener, no HTTP
//! layer and no server crate. The core's webhook pipeline runs over a
//! memory backend of the core's own tests (`tests/memory`: records, a
//! key/value store for the dedup leases, an outbox that checks the routing
//! again, the library's memory inbox), fed Meta's documented payloads as
//! signed bytes (`conformance::webhooks`, which the service's memory and
//! Postgres backends run too, in its `tests/store.rs` and
//! `tests/live_postgres.rs`). Every HTTP answer is the service's
//! `tests/webhooks.rs`.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

mod memory;

use meta_whatsapp_server_core::conformance;

/// Roadmap S5e, decisive: M1.2's routing cases through the core's
/// pipeline, with no listener. An event reaches only the tenant the
/// records bind its number (under the WABA it names) or its WABA to; an
/// unowned number's event, a stale binding's and a replay's are
/// operator-only, whatever the payload says of a tenant.
#[tokio::test]
async fn routing_is_an_allow_list_with_no_listener() {
    let backend = memory::Memory::default();
    conformance::webhooks::routing_is_an_allow_list_with_no_listener(&backend).await;
}

/// An event dated before its binding, or before the replay window, is
/// operator-only; on time, its holder's.
#[tokio::test]
async fn an_event_is_routed_only_after_its_binding_and_within_the_replay_window() {
    let backend = memory::Memory::default();
    conformance::webhooks::an_event_is_routed_only_after_its_binding_and_within_the_replay_window(
        &backend,
    )
    .await;
}

/// A number moving while its event records gives the event to neither
/// tenant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_number_moved_while_its_event_records_reaches_neither_tenant() {
    let backend = memory::Memory::default();
    conformance::webhooks::a_number_moved_while_its_event_records_reaches_neither_tenant(&backend)
        .await;
}

/// With every turn to record held, a delivery waits, touching nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delivery_waits_for_a_turn_to_record() {
    let backend = memory::Memory::default();
    conformance::webhooks::a_delivery_waits_for_a_turn_to_record(&backend).await;
}

/// With every turn to record held, a delivery is `503 busy` after
/// `RECORDING_WAIT`, on a paused clock (no 10 s of real time).
#[tokio::test(start_paused = true)]
async fn a_delivery_with_no_turn_is_busy_after_the_wait() {
    let backend = memory::Memory::default();
    conformance::webhooks::a_delivery_with_no_turn_is_busy_after_the_wait(&backend).await;
}

/// The signature is checked against every app secret before any store is
/// touched or a turn to record waited for; 3 MiB is read, one byte more
/// refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delivery_is_verified_before_anything_is_stored() {
    let backend = memory::Memory::default();
    conformance::webhooks::a_delivery_is_verified_before_anything_is_stored(&backend).await;
}

/// Each event once, under its dedup lease; a held lease is `503` with
/// nothing recorded.
#[tokio::test]
async fn an_event_is_recorded_once_under_its_dedup_lease() {
    let backend = memory::Memory::default();
    conformance::webhooks::an_event_is_recorded_once_under_its_dedup_lease(&backend).await;
}

/// The answer only once every event is recorded; a failed or busy outbox
/// refused, and Meta's redelivery recorded once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_answer_comes_only_once_every_event_is_recorded() {
    let backend = memory::Memory::default();
    conformance::webhooks::the_answer_comes_only_once_every_event_is_recorded(&backend).await;
}

/// `unknown` and `unparsed` are acknowledged, operator-only.
#[tokio::test]
async fn unknown_and_unparsed_bodies_are_acknowledged_as_operator_only_rows() {
    let backend = memory::Memory::default();
    conformance::webhooks::unknown_and_unparsed_bodies_are_acknowledged_as_operator_only_rows(
        &backend,
    )
    .await;
}

/// The whole suite, on one backend, as `conformance::run` runs it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_webhook_suite_runs_on_one_backend() {
    let backend = memory::Memory::default();
    conformance::webhooks::run(&backend).await;
}
