//! The store suite on the memory store (Postgres: `live_postgres.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use meta_whatsapp_server::store::MemoryStore;

#[tokio::test]
async fn the_memory_store_passes_the_suite() {
    common::store_suite::run(&MemoryStore::new()).await;
}
