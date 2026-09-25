//! Acceptance test M1.7, the M1a part in process: the captured `tracing`
//! output of admin and numbers calls, at `TRACE` for every target (the
//! library's included), holds no secret, key, token or phone number.
//! `live_postgres.rs` repeats it on Postgres.
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
