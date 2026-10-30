//! Roadmap S5d's decisive case, in this crate alone: no HTTP layer and no
//! server crate. The core's media services run over a memory backend of
//! the core's own tests (`tests/memory`), with a Graph client whose media
//! downloads arrive in the chunks the case gives
//! (`conformance::media`, which the service's memory and Postgres
//! backends run too, in its `tests/store.rs` and `tests/live_postgres.rs`).
//! Every HTTP answer is the service's `tests/media.rs`.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

mod memory;

use meta_whatsapp_server_core::conformance;

/// Roadmap S5d, decisive: a download through the core's stream ends
/// before its last chunk on a digest mismatch (M1b's rule, without axum);
/// it passes the file on while it arrives, one chunk held; the caps, the
/// digest, Meta's URL and the media id are checked as M1b checks them,
/// and a slot of the tenant's share is taken first.
#[tokio::test]
async fn a_download_through_the_cores_stream_ends_before_its_last_chunk_on_a_digest_mismatch() {
    let backend = memory::Memory::default();
    conformance::media::a_download_through_the_cores_stream_ends_before_its_last_chunk_on_a_digest_mismatch(
        &backend,
    )
    .await;
}

/// The upload, checked first, and the deletion, looked up first, send
/// Meta's documented requests with the number's token.
#[tokio::test]
async fn the_media_services_upload_and_delete_as_meta_documents() {
    let backend = memory::Memory::default();
    conformance::media::the_media_services_upload_and_delete_as_meta_documents(&backend).await;
}

/// A failed download never passes on an unverified last chunk (empty
/// chunks, a cut connection, one chunk, none, exactly `max_bytes`), gives
/// its slot back on every path, and a `190` from Meta's URL marks the
/// number.
#[tokio::test]
async fn a_failed_download_yields_no_unverified_end_and_frees_its_slot() {
    let backend = memory::Memory::default();
    conformance::media::a_failed_download_yields_no_unverified_end_and_frees_its_slot(&backend)
        .await;
}
