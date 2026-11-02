//! Roadmap M3a's cases that need no HTTP layer and no server crate: the
//! core's signup services and the records they need, over a memory
//! backend of the core's own tests (`tests/memory`), Graph scripted with
//! the library's `ScriptedTransport` (`conformance::signup`, which the
//! memory and Postgres backends run too, in their crates'
//! `tests/conformance.rs` and `tests/live_postgres.rs`). Every HTTP answer
//! is the service's `tests/signup.rs`. The records' own contract
//! (`conformance::signup::the_records_keep_attempts_and_vault_entries`)
//! is the real backends' to meet: this test backend deletes no tenant.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

mod memory;

use meta_whatsapp_server_core::conformance;

/// M3.4, decisive: D4 before any credit call, through the library's
/// post-verification gate.
#[tokio::test]
async fn m3_4_d4_is_enforced_before_any_credit_call() {
    let backend = memory::Memory::default();
    conformance::signup::m3_4_d4_is_enforced_before_any_credit_call(&backend).await;
}

/// A key rotation reaches the ledger and the marker an offboarded WABA
/// left.
#[tokio::test]
async fn a_rotation_reaches_the_records_kept_past_a_binding() {
    let backend = memory::Memory::default();
    conformance::signup::a_rotation_reaches_the_records_kept_past_a_binding(&backend).await;
}
