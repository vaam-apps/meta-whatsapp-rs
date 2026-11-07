//! Roadmap M5h2's audit events and logs (docs/design/server.md, D433): the
//! partner Intent API's invitation is an admin audit event, by the admin
//! key's id and the business, with an outcome; the agreement's state read
//! and the client WABAs listing are reads and write none. A signer planted
//! in Meta's answers, the operator's token in the request's body and
//! Meta's texts are in no line. Its own test binary: the `tracing`
//! callsites' interest is cached per process, and a test in a binary of
//! many, run on another thread without the capture's subscriber, can
//! switch the events off.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::capture::{Captured, subscriber};
use common::signup::{POOL_BUSINESS, POOL_TOKEN, tech_provider};
use common::{Call, Harness};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::BusinessId;
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const BUSINESS: &str = "529216684107530";
const END_BUSINESS: &str = "2780023991704";
const BUSINESS_TOKEN: &str = "EAAB-the-operators-business-token-planted";
const SIGNER_NAME: &str = "Jane Planted-Signer";
const SIGNER_EMAIL: &str = "jane.planted@bsp.example";
const SENTINEL: &str = "SENTINEL-META-TEXT";

/// The invitation: answered, refused by Meta (nothing went through: no
/// event), cut by a timeout (it may have: `unknown`), refused by the
/// setting (no event). The reads write no audit event. No line holds the
/// planted signer, the token of the state read, the deployment's token or
/// Meta's sentinel. Decisive: `Partners::request_marketing_onboarding`'s
/// audit calls, and `MaxPriceAgreements::state` withholding the texts.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario: the calls, then every line
async fn the_invitation_is_audited_and_the_signer_is_in_no_line() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::with_settings(Settings {
        signup: Some(tech_provider().pre_verified_pool(
            BusinessId::new(POOL_BUSINESS).unwrap(),
            AccessToken::new(POOL_TOKEN),
        )),
        legal_acts: true,
        ..common::test_settings()
    });
    let admin = h.admin_key().await;
    let onboard = || {
        Call::new(
            Method::POST,
            format!("/v1/admin/partners/businesses/{END_BUSINESS}/marketing-onboarding"),
        )
        .key(&admin)
    };
    let state = |token: &str| {
        Call::new(
            Method::POST,
            format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement/state"),
        )
        .key(&admin)
        .json(&json!({ "token": token }))
    };

    h.graph.push_json(200, json!({"request_id": "893436979557695"}));
    assert_eq!(h.call(onboard()).await.status, StatusCode::OK);
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#1752041) {SENTINEL} {SIGNER_EMAIL}"),
                         "type": "OAuthException", "code": 1_752_041,
                         "error_data": {"details": SENTINEL},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h.call(onboard()).await;
    assert!(refused.status.is_client_error(), "{}", refused.text);
    h.graph.push_error(|| TransportError::Timeout);
    assert_eq!(h.call(onboard()).await.status, StatusCode::GATEWAY_TIMEOUT);

    // The reads, with the signer planted in Meta's answers.
    h.graph.push_json(
        200,
        json!({"status": "accepted", "signer_name": SIGNER_NAME, "signer_email": SIGNER_EMAIL}),
    );
    assert_eq!(
        h.call(state(BUSINESS_TOKEN)).await.json(),
        json!({"status": "accepted"})
    );
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#100) {SIGNER_NAME} {SIGNER_EMAIL} {SENTINEL}"),
                         "type": "OAuthException", "code": 100,
                         "error_data": {"details": format!("{SIGNER_NAME} {SIGNER_EMAIL}")},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    assert!(h.call(state(BUSINESS_TOKEN)).await.status.is_client_error());
    h.graph.push_bytes(
        502,
        "text/html",
        format!("<html>{SIGNER_NAME} {SIGNER_EMAIL} {BUSINESS_TOKEN}</html>"),
    );
    assert!(!h.call(state(BUSINESS_TOKEN)).await.status.is_success());
    // A body the parser rejects, with the token and the signer in it.
    let malformed = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement/state"),
            )
            .key(&admin)
            .json(&json!({"token": BUSINESS_TOKEN, "signer_email": SIGNER_EMAIL})),
        )
        .await;
    assert_eq!(malformed.status, StatusCode::UNPROCESSABLE_ENTITY);
    h.graph.push_json(
        200,
        json!({"data": [{"id": "46302397361990", "name": "San Andreas Roofing"}]}),
    );
    let listed = h
        .call(
            Call::new(
                Method::GET,
                "/v1/admin/partners/client-wabas?onboarding_status=ELIGIBLE",
            )
            .key(&admin),
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(h.graph.remaining(), 0);

    // The setting off: refused, no event.
    let off = Harness::with_settings(Settings {
        signup: Some(tech_provider().pre_verified_pool(
            BusinessId::new(POOL_BUSINESS).unwrap(),
            AccessToken::new(POOL_TOKEN),
        )),
        ..common::test_settings()
    });
    let off_admin = off.admin_key().await;
    let refusal = off
        .call(
            Call::new(
                Method::POST,
                format!("/v1/admin/partners/businesses/{END_BUSINESS}/marketing-onboarding"),
            )
            .key(&off_admin),
        )
        .await;
    assert_eq!(refusal.code(), "feature_not_available");
    assert!(off.graph.requests().is_empty());

    let logs = captured.text();
    let events: Vec<Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["target"] == "audit")
        .map(|e| e["fields"].clone())
        .collect();
    let invitations: Vec<&Value> = events
        .iter()
        .filter(|f| f["action"] == "marketing_onboarding_requested")
        .collect();
    let outcomes: Vec<&str> = invitations
        .iter()
        .map(|f| f["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(
        outcomes,
        ["answered", "unknown"],
        "a refusal Meta answered changed nothing; a timeout may have gone through"
    );
    for fields in &invitations {
        assert_eq!(fields["message"], "admin change", "{fields}");
        assert!(fields["admin_key_id"].is_string(), "{fields}");
        assert_eq!(fields["subject_id"], END_BUSINESS, "{fields}");
        for name in fields.as_object().unwrap().keys() {
            assert!(
                matches!(
                    name.as_str(),
                    "message" | "action" | "admin_key_id" | "subject_id" | "count" | "outcome"
                ),
                "an unexpected field `{name}`: {fields}"
            );
        }
    }
    // Nothing else of this item is an audit event: the reads change
    // nothing.
    for fields in &events {
        let action = fields["action"].as_str().unwrap_or_default();
        assert!(
            !action.contains("agreement") && !action.contains("client_waba"),
            "a read was audited: {fields}"
        );
    }
    for planted in [
        SIGNER_NAME,
        SIGNER_EMAIL,
        "Planted-Signer",
        "bsp.example",
        BUSINESS_TOKEN,
        POOL_TOKEN,
        SENTINEL,
        "San Andreas",
    ] {
        assert!(!logs.contains(planted), "a planted value was logged");
    }
}
