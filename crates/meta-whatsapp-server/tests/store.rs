//! The store suites on memory (Postgres: `live_postgres.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use meta_whatsapp_server::store::{MemoryEventStore, MemoryStore};

#[tokio::test]
async fn the_memory_store_passes_the_suite() {
    common::store_suite::run(&MemoryStore::new()).await;
}

#[tokio::test]
async fn the_memory_event_store_passes_the_suite() {
    common::events_suite::run(&MemoryEventStore::new()).await;
}
