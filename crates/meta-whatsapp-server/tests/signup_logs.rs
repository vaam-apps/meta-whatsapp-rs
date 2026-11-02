//! Roadmap M3a's log half: the merchant's token, the partner's system
//! token, the PIN, the code and the attempt's state are in no log line, at
//! any level, for any target, through a Solution Partner's whole signup
//! (a completion stopped at the registration, the merchant's
//! `PARTNER_REMOVED`, a resume, the operator's re-share, a disconnection,
//! an uninstall); nor are Meta's texts on a failed signup step (the signup
//! routes withhold them, as the OTP routes do). Alone in its binary, as
//! `logs.rs` is: the capture is a thread's default subscriber, and
//! `tracing` caches whether a call site is enabled across threads.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::Call;
use common::capture::{Captured, subscriber};
use common::signup::{
    PHONE, SYSTEM_TOKEN, TOKEN, WABA, complete, harness, removed, resume, script_already_revoked,
    script_confirmed, script_revocation, script_share, script_until_verified, solution_partner,
    start, success, uninstalled,
};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use serde_json::json;

const CODE: &str = "AQB-SENTINEL-signup-code";
const PIN: &str = "835164";
const NEW_PIN: &str = "472905";

/// Every sentinel is in no line. Decisive: the signup routes' errors built
/// with `ServiceError::from_library_withholding_texts` (Meta's text, which
/// could quote a value, logged at `debug` elsewhere), and no `Debug` of a
/// request holding the PIN or the code.
#[tokio::test]
async fn a_partners_signup_logs_no_token_pin_code_or_state() {
    let h = harness(solution_partner());
    h.tenant("merchant-a").await;
    let key = h
        .tenant_key("merchant-a", &[Scope::Signup, Scope::Numbers])
        .await;
    let admin = h.admin_key().await;
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));

    let state = start(&h, &key).await;
    script_until_verified(&h.graph);
    h.graph.push_json(200, success()); // subscribed_apps
    script_share(&h.graph);
    h.graph.push_json(
        400,
        json!({"error": {"message": "(#133005) Two step verification PIN Mismatch SENTINEL-message",
            "type": "OAuthException", "code": 133005, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn",
            "error_user_title": "SENTINEL-user-title", "error_user_msg": "SENTINEL-user-message",
            "error_data": {"messaging_product": "whatsapp", "details": "SENTINEL-details"}}}),
    );
    let body = json!({
        "state": state, "code": CODE, "pin": PIN,
        "event": {"type": "WA_EMBEDDED_SIGNUP", "event": "FINISH",
            "data": {"waba_id": WABA, "phone_number_id": PHONE}}
    });
    let stopped = h.call(complete(&key, &body)).await;
    assert_eq!(
        stopped.json()["error"]["step"],
        "register_phone",
        "{}",
        stopped.text
    );
    // Removed, then the resume refused at the line, then the operator's
    // re-share, then the disconnection.
    script_confirmed(&h.graph);
    script_revocation(&h.graph);
    assert_eq!(h.webhook(&removed()).await.status, StatusCode::OK);
    h.graph.push_json(200, success());
    h.graph.push_json(200, success());
    let refused = h
        .call(resume(&key, &json!({"waba_id": WABA, "pin": NEW_PIN})))
        .await;
    assert_eq!(refused.json()["error"]["step"], "share_credit_line");
    h.graph.push_json(200, success());
    h.graph.push_json(200, success());
    script_already_revoked(&h.graph);
    h.graph.push_json(
        200,
        json!({"allocation_config_id": "58501441721239", "waba_id": WABA}),
    );
    let reshared = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/admin/wabas/{WABA}/credit-line/reshare"),
            )
            .key(&admin),
        )
        .await;
    assert_eq!(reshared.status, StatusCode::OK, "{}", reshared.text);
    // A second partner signup of the same WABA, refused at the line once
    // more? Not needed: the uninstall offboards it (revoked first).
    script_confirmed(&h.graph);
    let business = json!({"name": "Wind & Wool", "id": common::signup::BUSINESS});
    h.graph.push_json(
        200,
        json!({"id": "58501441721239", "receiving_business": business}),
    );
    h.graph
        .push_json(200, json!({"receiving_business": business}));
    h.graph.push_json(200, success());
    h.graph.push_json(
        200,
        json!({"receiving_business": business, "request_status": "DELETED"}),
    );
    assert_eq!(h.webhook(&uninstalled()).await.status, StatusCode::OK);
    assert_eq!(h.graph.remaining(), 0);

    let logs = captured.text();
    assert!(
        logs.contains("/v1/signup/complete") && logs.contains("/v1/signup/resume"),
        "the capture saw the requests:\n{logs}"
    );
    assert!(logs.contains("graph request"), "and the library's events");
    assert!(
        logs.contains("AXsgnV2Cm3ZMGF3dF_cfYIn"),
        "Meta's trace id is logged"
    );
    assert!(
        logs.contains("credit_line_reshared") && logs.contains("waba_offboarded"),
        "the operator's re-share and the webhook's offboarding are audited"
    );
    for secret in [
        TOKEN,
        SYSTEM_TOKEN,
        CODE,
        PIN,
        NEW_PIN,
        state.as_str(),
        "SENTINEL",
    ] {
        assert!(!logs.contains(secret), "{secret} was logged:\n{logs}");
    }
}
