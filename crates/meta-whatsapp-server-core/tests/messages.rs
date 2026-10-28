//! Roadmap S5b's decisive case, in this crate alone: no HTTP layer and no
//! server crate. The core's message services run under its idempotency
//! engine over a memory backend of the core's own tests (`tests/memory`:
//! records, idempotency records that lease and expire keys, a vault on a
//! memory key/value store), with a Graph client on the library's
//! `ScriptedTransport`: M1.4's case, a scripted timeout reporting
//! `may_have_been_sent` and the same key replaying it with no second
//! request (`conformance::messages`, which the service's memory and
//! Postgres backends run too, in its `tests/store.rs` and
//! `tests/live_postgres.rs`). Every HTTP answer, the `Idempotent-Replayed`
//! header among them, is the service's `tests/messages.rs`.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

mod memory;

use meta_whatsapp_server_core::conformance;

/// Roadmap S5b, decisive: called with no HTTP layer, a scripted timeout
/// reports `may_have_been_sent`, and the same key replays it with no
/// second request; a `131047` releases its key; another authorizer's
/// caller is refused before the key is read.
#[tokio::test]
async fn a_timeout_may_have_been_sent_and_its_key_replays_it_without_a_second_request() {
    let backend = memory::Memory::default();
    conformance::messages::a_timeout_may_have_been_sent_and_its_key_replays_it_without_a_second_request(&backend).await;
}
