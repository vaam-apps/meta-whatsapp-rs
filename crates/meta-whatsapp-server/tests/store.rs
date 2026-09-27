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

/// Roadmap S2 (SR-L2, the `failed` race) on memory.
#[tokio::test]
async fn capabilities_act_only_on_what_they_were_made_from() {
    common::capability_suite::run(
        std::sync::Arc::new(MemoryStore::new()),
        std::sync::Arc::new(meta_whatsapp_rs::adapters::store::MemoryKvStore::new()),
    )
    .await;
}

/// Roadmap S2's atomic contracts on memory, raced on four threads
/// (`common::race_suite`, `common::capability_suite::racing_a_reattach`):
/// an insert against its binding moving, a binding against its tenant's
/// deletion, and each conditioned write against a re-attach.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_the_atomic_contracts_breaks_none() {
    use common::capability_suite::{Act, racing_a_reattach};
    use common::race_suite::{
        a_binding_racing_a_deletion_serializes,
        an_insert_racing_a_move_never_reaches_the_new_holder,
    };
    use std::sync::Arc;
    let store = Arc::new(MemoryStore::new());
    let moves =
        an_insert_racing_a_move_never_reaches_the_new_holder(store.clone(), store.outbox(), 200)
            .await;
    eprintln!("memory, insert against a move: {moves:?}");
    let binds = a_binding_racing_a_deletion_serializes(store.clone(), 200).await;
    eprintln!("memory, bind against delete: {binds:?}");
    let kv = Arc::new(meta_whatsapp_rs::adapters::store::MemoryKvStore::new());
    for act in [Act::Forget, Act::NumberFailed, Act::WabaFailed] {
        racing_a_reattach(store.clone(), kv.clone(), act, 60).await;
    }
}
