//! Roadmap M3c's decisive cases, in this crate alone: no HTTP layer and no
//! server crate. The core's OTP services over a memory backend of the
//! core's own tests (`tests/memory`: records that keep the tenants' OTP
//! settings, a vault and the library's codes on a memory key/value store),
//! sends to the library's `ScriptedTransport` (`conformance::otp`, which
//! the memory and Postgres backends run too, in their crates'
//! `tests/conformance.rs` and `tests/live_postgres.rs`). Every HTTP answer
//! is the service's `tests/otp.rs`.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

mod memory;

use meta_whatsapp_server_core::conformance;

/// M3.5, decisive: tenant A's code verifies at no other tenant on the same
/// number; a sending number no longer its holder's is not used.
#[tokio::test]
async fn a_code_verifies_at_no_other_tenant_on_the_same_number() {
    let backend = memory::Memory::default();
    conformance::otp::a_code_verifies_at_no_other_tenant_on_the_same_number(&backend).await;
}
