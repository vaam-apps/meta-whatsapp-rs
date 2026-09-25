//! Acceptance test M1.7, in process: the captured `tracing` output of
//! every operation of the committed document (admin, numbers, sends,
//! media, templates), at `TRACE` for every target (the library's
//! included), holds no secret, key, token, message text, phone number or
//! contact. `live_postgres.rs` repeats it on Postgres.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::Harness;
use common::capture::{Captured, check, exercise, subscriber};

#[tokio::test]
async fn every_route_logs_no_secret_key_message_text_phone_number_or_contact() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    let secrets = exercise(&h).await;
    check(&captured.text(), &secrets);
}
