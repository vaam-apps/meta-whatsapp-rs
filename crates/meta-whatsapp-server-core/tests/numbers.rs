//! Roadmap S5c's number services, in this crate alone: no HTTP layer and
//! no server crate. The core's number and WABA services run over a memory
//! backend of the core's own tests (`tests/memory`), with a Graph client
//! on the library's `ScriptedTransport` (`conformance::numbers`, which the
//! service's memory and Postgres backends run too). Every HTTP answer is
//! the service's `tests/numbers.rs`.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

mod memory;

use meta_whatsapp_server_core::conformance;

/// The services open only on their own `Authorizer`'s capabilities; a
/// number's details and profile are read, and the profile changed, as
/// Meta documents it; a WABA is disconnected only once Meta unsubscribed
/// the app, and not at all once it moved.
#[tokio::test]
async fn the_number_services_read_change_and_disconnect_as_meta_documents() {
    let backend = memory::Memory::default();
    conformance::numbers::the_number_services_read_change_and_disconnect_as_meta_documents(
        &backend,
    )
    .await;
}

/// Roadmap M3d: the registration services open on the caller's own
/// number only (another tenant's is `404`, before any attempt is counted).
#[tokio::test]
async fn the_registration_services_open_on_the_callers_own_number_only() {
    let backend = memory::Memory::default();
    conformance::numbers::the_registration_services_open_on_the_callers_own_number_only(&backend)
        .await;
}

/// Roadmap M5c6: a WABA's health status is the caller's tenant's own, and
/// filtered; a number's webhook override is refused with the setting off,
/// without webhooks out, and at a destination a webhook endpoint would be
/// refused at, all before anything is sent; clearing is allowed in every
/// state. In the core alone, so the API adapter's own gate cannot cover for
/// the core's.
#[tokio::test]
async fn the_waba_health_and_the_webhook_override_services_gate_and_filter() {
    let backend = memory::Memory::default();
    conformance::webhook_override::the_waba_health_and_the_webhook_override_services_gate_and_filter(
        &backend,
    )
    .await;
}
