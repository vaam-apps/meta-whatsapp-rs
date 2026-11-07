//! Design D192, D231, D250: **every legal act is sent exactly once**. The
//! library's `RetryPolicy` replays what it deems safe (an `idempotent`
//! request after a timeout or a `5xx`; anything after a `429`), which is
//! right for a library user and wrong for an act that cannot be undone, so
//! the service sends every act gated by `is_legal_act` and
//! `is_legal_admin_act` through `number_settings::legal_client` (a client
//! with `RetryPolicy::NONE`).
//!
//! The walk derives its list from the two predicates over every
//! operation, so an operation added to the gate is covered (and fails
//! here until it has a call): with the setting on and a client that
//! retries, Meta answers the first request with a timeout or a `5xx` and
//! would answer the second well; exactly one request is made, the second
//! answer is never consumed, and the reply says the act may have been
//! sent. Decisive: route any gated operation through `authz.client()`
//! instead of `legal_client`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::time::Duration;

use common::signup::{POOL_BUSINESS, POOL_TOKEN, tech_provider};
use common::{Call, Harness};
use meta_whatsapp_rs::RetryPolicy;
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::BusinessId;
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::number_settings::{is_legal_act, is_legal_admin_act};
use meta_whatsapp_server_core::operation::{AdminOperation, Operation};
use serde_json::json;

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const SOLUTION: &str = "795033096057724";
const CUSTOMER: &str = "2729063490586005";
const DESTINATION_SOLUTION: &str = "303610109049230";
const MIGRATION: &str = "mig_01HZYK3ABCDEF4567890";
const BUSINESS: &str = "529216684107530";

/// A client that would replay anything it may, at once.
const RETRYING: RetryPolicy = RetryPolicy {
    max_retries: 3,
    base_delay: Duration::ZERO,
    max_delay: Duration::ZERO,
};

struct Fixture {
    h: Harness,
    key: String,
    admin: String,
}

async fn fixture() -> Fixture {
    let h = Harness::with_retry(
        Settings {
            signup: Some(tech_provider().pre_verified_pool(
                BusinessId::new(POOL_BUSINESS).unwrap(),
                AccessToken::new(POOL_TOKEN),
            )),
            legal_acts: true,
            ..common::test_settings()
        },
        RETRYING,
    );
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h
        .tenant_key(TENANT, &[Scope::Numbers, Scope::Partners, Scope::Analytics])
        .await;
    let admin = h.admin_key().await;
    Fixture { h, key, admin }
}

/// One legal act of either kind.
#[derive(Clone, Copy, Debug)]
enum Act {
    Tenant(Operation),
    Admin(AdminOperation),
}

/// Every legal act, derived from the two predicates.
fn every_legal_act() -> Vec<Act> {
    let tenant = Operation::ALL
        .into_iter()
        .filter(|operation| is_legal_act(*operation))
        .map(Act::Tenant);
    let admin = AdminOperation::ALL
        .into_iter()
        .filter(|operation| is_legal_admin_act(*operation))
        .map(Act::Admin);
    tenant.chain(admin).collect()
}

/// The call that takes `act`, and the number of requests that must precede
/// it (a set-up the act needs, answered well).
fn call_of(act: Act, f: &Fixture) -> (Call, usize) {
    let tenant = |method, path: String| Call::new(method, path).key(&f.key);
    let admin = |path: String| Call::new(Method::POST, path).key(&f.admin);
    let solution = |tail: &str| format!("/v1/admin/partners/solutions/{SOLUTION}{tail}");
    match act {
        Act::Tenant(Operation::SetComplianceInfo) => (
            tenant(Method::PUT, format!("/v1/numbers/{PN}/compliance-info"))
                .json(&common::sample_compliance_info()),
            0,
        ),
        Act::Tenant(Operation::CreateSolutionMigrationIntent) => (
            tenant(
                Method::POST,
                format!("/v1/wabas/{WABA}/solution-migration-intents"),
            )
            .json(&json!({"solution_id": DESTINATION_SOLUTION})),
            0,
        ),
        Act::Tenant(Operation::CreateBillingMigration) => (
            tenant(Method::POST, format!("/v1/wabas/{WABA}/billing-migrations"))
                .header("idempotency-key", "walk-1")
                .json(&json!({"currency": "INR"})),
            0,
        ),
        Act::Tenant(Operation::EnableTemplateInsights) => (
            tenant(
                Method::POST,
                format!("/v1/wabas/{WABA}/analytics/template-insights"),
            ),
            0,
        ),
        // The migration must be the WABA's own: one is started first.
        Act::Tenant(Operation::ResumeBillingMigration) => (
            tenant(
                Method::POST,
                format!("/v1/wabas/{WABA}/billing-migrations/{MIGRATION}/resume"),
            ),
            1,
        ),
        Act::Admin(AdminOperation::SignMaxPriceAgreement) => (
            admin(format!(
                "/v1/admin/businesses/{BUSINESS}/max-price-agreement"
            ))
            .json(&json!({
                "signer_name": "Jane Doe", "signer_email": "jane.doe@example.com",
                "token": "EAAB-the-operators-business-token"})),
            0,
        ),
        Act::Admin(AdminOperation::SubmitBusinessVerification) => (
            admin("/v1/admin/partners/business-verifications".to_owned()).json(&json!({
                "client_business_id": CUSTOMER,
                "documents": [{"file_name": "statement.pdf",
                               "content_base64": "JVBERi0xLjcKJeLjz9MKMSAwIG9iago="}]
            })),
            0,
        ),
        Act::Admin(AdminOperation::AcceptPartnerSolution) => (admin(solution("/accept")), 0),
        Act::Admin(AdminOperation::RejectPartnerSolution) => (admin(solution("/reject")), 0),
        Act::Admin(AdminOperation::AcceptPartnerSolutionDeactivation) => {
            (admin(solution("/deactivation-request/accept")), 0)
        }
        // Roadmap M5h2, design D431: the partner Intent API's invitation.
        Act::Admin(AdminOperation::RequestMarketingOnboarding) => (
            admin(format!(
                "/v1/admin/partners/businesses/{BUSINESS}/marketing-onboarding"
            )),
            0,
        ),
        other => panic!("{other:?} is a legal act with no call in this walk: add one"),
    }
}

/// The set-up request of an act that needs one: the migration the resume
/// names.
fn set_up(h: &Harness) {
    h.graph.push_json(
        200,
        json!({"migration_id": MIGRATION, "migration_status": "INITIATED"}),
    );
}

#[tokio::test]
async fn the_walk_covers_the_gated_operations() {
    // The walk's list is the gate's: the five tenant acts and the six of
    // the operator today.
    let acts = every_legal_act();
    assert_eq!(acts.len(), 11, "{acts:?}");
}

#[tokio::test]
async fn every_legal_act_reaches_meta_once_whatever_the_client_would_retry() {
    for act in every_legal_act() {
        for failure in ["timeout", "500", "429"] {
            let f = fixture().await;
            let (call, before) = call_of(act, &f);
            if before > 0 {
                set_up(&f.h);
                let started =
                    f.h.call(
                        Call::new(Method::POST, format!("/v1/wabas/{WABA}/billing-migrations"))
                            .key(&f.key)
                            .header("idempotency-key", "walk-setup")
                            .json(&json!({"currency": "INR"})),
                    )
                    .await;
                assert_eq!(
                    started.status,
                    StatusCode::CREATED,
                    "{act:?}: {}",
                    started.text
                );
            }
            let sent_before = f.h.graph.requests().len();
            assert_eq!(sent_before, before, "{act:?}");
            if failure == "timeout" {
                f.h.graph.push_error(|| TransportError::Timeout);
            } else if failure == "429" {
                // The one answer that lets the library replay a request
                // that is not marked idempotent.
                f.h.graph.push_json(
                    429,
                    json!({"error": {"message": "(#4) Application request limit reached",
                                     "type": "OAuthException", "code": 4,
                                     "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
                );
            } else {
                f.h.graph.push_json(
                    500,
                    json!({"error": {"message": "An unknown error occurred",
                                     "type": "OAuthException", "code": 2,
                                     "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
                );
            }
            // Meta would have answered the replay well.
            f.h.graph.push_json(200, json!({"success": true}));
            let reply = f.h.call(call).await;
            let what = format!("{act:?} after a {failure}: {}", reply.text);
            assert_eq!(
                f.h.graph.requests().len(),
                sent_before + 1,
                "{what}: the act was sent more than once"
            );
            assert_eq!(
                f.h.graph.remaining(),
                1,
                "{what}: the second answer was used"
            );
            if failure == "429" {
                assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS, "{what}");
            } else {
                assert!(
                    matches!(
                        reply.status,
                        StatusCode::GATEWAY_TIMEOUT | StatusCode::BAD_GATEWAY
                    ),
                    "{what}: {}",
                    reply.status
                );
                assert_eq!(reply.json()["error"]["may_have_been_sent"], true, "{what}");
            }
        }
    }
}
