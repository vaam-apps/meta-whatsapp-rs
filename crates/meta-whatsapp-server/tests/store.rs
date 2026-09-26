//! The store suites on memory (Postgres: `live_postgres.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use meta_whatsapp_server::store::{MemoryBackend, MemoryStore};

#[tokio::test]
async fn the_memory_store_passes_the_suite() {
    common::store_suite::run(&MemoryStore::new()).await;
}

#[tokio::test]
async fn the_memory_event_store_passes_the_suite() {
    // The outbox of the store that holds its tenants and bindings.
    let store = MemoryStore::new();
    common::events_suite::run(store.outbox().as_ref(), &store).await;
}

#[tokio::test]
async fn the_memory_backend_hands_out_the_same_data_on_every_call() {
    common::backend_suite::run(&MemoryBackend::new()).await;
}
