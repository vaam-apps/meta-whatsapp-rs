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
