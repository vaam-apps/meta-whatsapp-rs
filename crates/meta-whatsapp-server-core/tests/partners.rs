//! Roadmap M5j, at the core's own boundary: no HTTP layer. Each of the
//! seven legal or irreversible partner acts is `403 feature_not_available`
//! from the service itself while the legal acts are off, before the
//! deployment's partner credentials, the vault or Meta are asked, so that a
//! caller of the services other than the API adapter (whose extractor
//! answers the gate before a body is read) cannot take one. The HTTP
//! answers, the audit events and the logs are the service's
//! `tests/partners.rs` and `tests/logs.rs`.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

use std::sync::Arc;

use meta_whatsapp_rs::core::clock::SystemClock;
use meta_whatsapp_server_core::partners::{DocumentInput, VerificationRequest};
use meta_whatsapp_server_core::signup::Signups;
use support::Side;

/// Decisive: `gate_partner` in `Partners::open` for the four of the
/// operator, `gate_operation` in `WabaPartners` for the three of a tenant's
/// WABA. A deployment with no partner credentials would otherwise answer
/// `409 signup_not_configured` to the operator's, and Meta's refusal to the
/// tenant's; the gate is first.
#[tokio::test]
async fn each_legal_act_is_refused_by_the_service_while_the_setting_is_off() {
    let side = Side::new("vault-1", "EAAG-token").await;
    let signups = Signups::new(side.kv.clone(), Arc::new(SystemClock), None);
    let admin = side.admin().await;
    let partners = signups.partners(&side.authz, &admin, false).unwrap();
    let refused = |error: meta_whatsapp_server_core::ServiceError, what: &str| {
        assert_eq!(error.code(), "feature_not_available", "{what}");
    };
    refused(
        partners
            .submit_business_verification(
                &side.authz,
                VerificationRequest {
                    client_business_id: "2729063490586005".to_owned(),
                    documents: vec![DocumentInput {
                        file_name: "a.pdf".to_owned(),
                        content_base64: "JVBERi0=".to_owned(),
                    }],
                },
            )
            .await
            .unwrap_err(),
        "submit_business_verification",
    );
    refused(
        partners
            .accept_solution(&side.authz, "795033096057724")
            .await
            .unwrap_err(),
        "accept_solution",
    );
    refused(
        partners
            .reject_solution(&side.authz, "795033096057724")
            .await
            .unwrap_err(),
        "reject_solution",
    );
    refused(
        partners
            .accept_solution_deactivation(&side.authz, "795033096057724")
            .await
            .unwrap_err(),
        "accept_solution_deactivation",
    );
    // With the setting on, the same call reaches the next check: no partner
    // credentials here, so `409 signup_not_configured`.
    let on = signups.partners(&side.authz, &admin, true).unwrap();
    assert_eq!(
        on.accept_solution(&side.authz, "795033096057724")
            .await
            .unwrap_err()
            .code(),
        "signup_not_configured"
    );

    let caller = side.caller().await;
    let waba = side.waba().await;
    let tenant = signups
        .waba_partners(&side.authz, &caller, &waba, false)
        .unwrap();
    refused(
        tenant
            .create_solution_migration_intent("303610109049230")
            .await
            .unwrap_err(),
        "create_solution_migration_intent",
    );
    refused(
        tenant
            .create_billing_migration("INR", None)
            .await
            .unwrap_err(),
        "create_billing_migration",
    );
    refused(
        tenant
            .resume_billing_migration("mig_01HZYK3ABCDEF4567890")
            .await
            .unwrap_err(),
        "resume_billing_migration",
    );
}
