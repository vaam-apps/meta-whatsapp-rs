//! Acceptance test M1.7 in process (all but a send, which is M1b's): the
//! captured `tracing` output of admin and numbers calls, Meta's webhook
//! deliveries (signed, unsigned, forged) and polling their events, at
//! `TRACE` for every target (the library's included), holds no secret,
//! key, token, message text, phone number or contact.
//! `live_logs.rs` repeats it on Postgres.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::Harness;
use common::capture::{Captured, check, exercise, subscriber};

#[tokio::test]
async fn admin_and_numbers_calls_log_no_secret_key_or_phone_number() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    let secrets = exercise(&h).await;
    check(&captured.text(), &secrets);
}
