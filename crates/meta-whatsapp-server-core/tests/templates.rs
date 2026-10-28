//! Roadmap S5c's decisive case, in this crate alone: no HTTP layer and no
//! server crate. The core's template services run over a memory backend
//! of the core's own tests (`tests/memory`: records, and a vault on a
//! memory key/value store whose reads the suite records), with a Graph
//! client on the library's `ScriptedTransport`: tenant B's caller on
//! tenant A's template id is not found, with zero vault reads
//! (`conformance::templates`, which the service's memory and Postgres
//! backends run too, in its `tests/store.rs` and `tests/live_postgres.rs`).
//! Every HTTP answer is the service's `tests/templates.rs`.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

mod memory;

use meta_whatsapp_server_core::conformance;

/// Roadmap S5c, decisive: called with no HTTP layer, tenant B's caller on
/// tenant A's template id is not found with zero vault reads, whether it
/// names A's WABA, is handed A's capability, or asks through its own
/// WABA (where nothing of A's is answered or deleted); another
/// authorizer's caller or capability is refused.
#[tokio::test]
async fn another_tenants_template_id_is_not_found_with_zero_vault_reads() {
    let backend = memory::Memory::default();
    conformance::templates::another_tenants_template_id_is_not_found_with_zero_vault_reads(
        &backend,
    )
    .await;
}

/// The template services send Meta's documented requests with the
/// WABA's token: the list and its cache, the creation (checked first)
/// and the deletions, each dropping the WABA's cached pages.
#[tokio::test]
async fn the_template_services_send_metas_documented_requests() {
    let backend = memory::Memory::default();
    conformance::templates::the_template_services_send_metas_documented_requests(&backend).await;
}
