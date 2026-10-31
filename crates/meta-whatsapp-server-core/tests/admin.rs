//! Roadmap S5a's decisive case, in this crate alone: no HTTP layer and no
//! server crate. The core's admin services run over a memory backend of
//! the core's own tests (`tests/memory`: its records keep decision D4,
//! `bind_waba` refusing a WABA another tenant holds) and a vault on a
//! memory key/value store, with a Graph client on the library's
//! `ScriptedTransport`: attaching a WABA another tenant holds is refused
//! with nothing stored, subscribed or shared (`conformance::admin`, which
//! the service's memory and Postgres backends run too, in its
//! `tests/store.rs` and `tests/live_postgres.rs`). The rest of the admin
//! services, and every HTTP answer, are the service's `tests/admin.rs`.
//! And roadmap S6's vault writes, behind an admitted admin
//! (`conformance::capabilities`).
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

mod memory;

use meta_whatsapp_server_core::conformance;

/// Roadmap S5a, decisive: called with no HTTP layer, attaching a WABA
/// another tenant holds is refused with nothing stored, subscribed or
/// shared, whether the holder is seen before Meta is asked or only by the
/// binding's own check.
#[tokio::test]
async fn attaching_a_waba_another_tenant_holds_stores_subscribes_and_shares_nothing() {
    let backend = memory::Memory::default();
    conformance::admin::a_waba_another_tenant_holds_is_refused_with_nothing_stored(&backend).await;
}

/// Roadmap S6: the vault's admin writes take an admin the `Authorizer`
/// admits, each checking it again behind `Admin::new`'s admission
/// (`conformance::capabilities`, which the service's memory and Postgres
/// backends run too).
#[tokio::test]
async fn the_vaults_admin_writes_take_an_admitted_admin() {
    let backend = memory::Memory::default();
    conformance::capabilities::an_admins_vault_writes_take_an_admitted_admin(&backend).await;
}
