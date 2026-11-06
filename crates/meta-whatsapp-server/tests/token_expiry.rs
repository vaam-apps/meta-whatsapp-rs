//! The token-expiry event over HTTP (roadmap M3e; the core's
//! `token_expiry`, whose rules over each backend are
//! `conformance::token_expiry`): the service's task tells a tenant its
//! stored token nears its expiry, once, and counts it
//! (`wa_server_token_expiry_events_total`). The same case runs on Postgres
//! in `live_postgres.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::Harness;

/// Decisive (roadmap M3e): see `common::token_expiry`.
#[tokio::test]
async fn a_token_inside_the_lead_is_told_to_its_tenant_once() {
    common::token_expiry::a_token_inside_the_lead_is_told_to_its_tenant_once(&Harness::new()).await;
}
