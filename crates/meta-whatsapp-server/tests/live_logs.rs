//! Acceptance test M1.7's log capture on a real Postgres: the capture of
//! `logs.rs` (every route of the committed document, sends included, and
//! Meta's webhooks) holds there too.
//! Skipped unless `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set; `just
//! test-live` sets `META_WHATSAPP_RS_REQUIRE_LIVE=1`, which turns the skip
//! into a failure.
//!
//! Alone in its binary, as `logs.rs` is: the capture is a thread's default
//! subscriber, and `tracing` caches whether a call site is enabled across
//! threads, so a test running beside it can leave the capture's call sites
//! disabled (the capture then sees no request at all).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::capture::{Captured, check, exercise, subscriber};
use common::{Harness, Stores, TestDb};
use meta_whatsapp_server::store::migrate;

/// M1.7 on Postgres: sqlx's own events join the capture, and still no
/// secret, key, message text, phone number or contact is logged.
#[tokio::test]
async fn live_postgres_logs_hold_no_secret_key_message_or_contact() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.pool(10).await;
    migrate(&pool).await.unwrap();
    let h = Harness::with(Stores::postgres(&pool));
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let secrets = exercise(&h).await;
    let logs = captured.text();
    check(&logs, &secrets);
    // The Postgres store's secret digests are bound parameters, never SQL
    // text.
    assert!(!logs.contains("secret_sha256 = '"));
}
