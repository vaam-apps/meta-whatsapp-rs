//! Roadmap M2a's and M2g's decisive cases, in this crate alone: no HTTP layer and no
//! server crate. The core's inbox services over a memory backend of the
//! core's own tests (`tests/memory`: records that keep a WABA's moves, a
//! vault on a memory key/value store, the library's memory inbox), the
//! webhooks recorded by the core's pipeline, replies sent to the library's
//! `ScriptedTransport` (`conformance::inbox`, which the memory and
//! Postgres backends run too, in their crates' `tests/conformance.rs` and
//! `tests/live_postgres.rs`; housekeeping's retention case needs a leader
//! lock, which this backend has not, so it runs there only). Every HTTP
//! answer is the service's `tests/inbox.rs`.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

mod memory;

use meta_whatsapp_server_core::conformance;

/// M2.1 with no HTTP layer: an inbound message listed, read, answered
/// within its window and recorded `accepted`, then `delivered`; a
/// free-form reply outside the window refused with zero requests.
#[tokio::test]
async fn an_inbound_message_is_listed_read_and_answered_within_its_window() {
    let backend = memory::Memory::default();
    conformance::inbox::an_inbound_message_is_listed_read_and_answered_within_its_window(&backend)
        .await;
}

/// Decisive: the binding epoch. A moved number's history is hidden from
/// its new tenant (and kept); a first onboarding's synced history is not.
#[tokio::test]
async fn a_moved_numbers_history_is_hidden_from_its_new_tenant() {
    let backend = memory::Memory::default();
    conformance::inbox::a_moved_numbers_history_is_hidden_from_its_new_tenant(&backend).await;
}

/// `control_taken`: a free-form reply refused `409 thread_owned_elsewhere`
/// with zero requests, unless the number's ownership check is off.
#[tokio::test]
async fn a_reply_while_another_app_owns_the_thread_is_refused() {
    let backend = memory::Memory::default();
    conformance::inbox::a_reply_while_another_app_owns_the_thread_is_refused_unless_its_number_turns_the_check_off(&backend).await;
}

/// Only the order's own number opens the services.
#[tokio::test]
async fn the_services_open_on_their_own_number_only() {
    let backend = memory::Memory::default();
    conformance::inbox::the_services_open_on_their_own_number_only(&backend).await;
}

/// Decisive (roadmap M2h): a number migrated between two tenants' WABAs,
/// neither WABA moving, shows its new tenant none of the previous one's
/// history.
#[tokio::test]
async fn a_number_migrated_between_two_tenants_wabas_hides_its_history() {
    let backend = memory::Memory::default();
    conformance::inbox::a_number_migrated_between_two_tenants_wabas_hides_its_history(&backend)
        .await;
}

/// A capability made before its number left its binding (to another WABA
/// of its tenant, then to another tenant) reads nothing: `503`.
#[tokio::test]
async fn a_capability_made_before_its_number_left_reads_nothing() {
    let backend = memory::Memory::default();
    conformance::inbox::a_capability_made_before_its_number_left_reads_nothing(&backend).await;
}

/// Roadmap M2g: the window is what a free-form reply would meet (the
/// window, the thread's owner, the number's checks), and the list's
/// window is the same.
#[tokio::test]
async fn the_window_is_what_a_reply_would_meet() {
    let backend = memory::Memory::default();
    conformance::inbox::the_window_is_what_a_reply_would_meet(&backend).await;
}

/// Decisive (roadmap M2g): after marking a conversation read, `unread` is
/// 0, and Meta got one read receipt, for the latest inbound message only,
/// and only with `notify_customer`.
#[tokio::test]
async fn marking_a_conversation_read_resets_unread_and_receipts_the_latest_inbound_message() {
    let backend = memory::Memory::default();
    conformance::inbox::marking_a_conversation_read_resets_unread_and_receipts_the_latest_inbound_message(&backend).await;
}

/// Roadmap M2g: a message by id on its own number only.
#[tokio::test]
async fn a_message_is_found_by_id_on_its_own_number() {
    let backend = memory::Memory::default();
    conformance::inbox::a_message_is_found_by_id_on_its_own_number(&backend).await;
}

/// Decisive (roadmap M2g): a message of the number's previous holder is
/// `404` by id to the new holder; its window and its receipt never reach
/// the new holder either.
#[tokio::test]
async fn the_previous_holders_message_window_and_receipt_never_reach_the_new_holder() {
    let backend = memory::Memory::default();
    conformance::inbox::the_previous_holders_message_window_and_receipt_never_reach_the_new_holder(
        &backend,
    )
    .await;
}
