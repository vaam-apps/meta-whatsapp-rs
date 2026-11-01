//! Acceptance test M3.5's log half (roadmap M3c): logs hold neither code
//! nor number. Alone in its binary, as `logs.rs` is: the capture is a
//! thread's default subscriber, and `tracing` caches whether a call site is
//! enabled across threads, so a test running beside it could leave the
//! capture's call sites disabled.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::capture::{Captured, subscriber};
use common::{Call, Harness, otp_deployment, test_settings};
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::webhooks::axum::http::Method;
use meta_whatsapp_server::model::{OtpSettings, Scope, TenantId};
use meta_whatsapp_server::state::Settings;
use serde_json::json;

const PLATFORM: &str = "platform";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-platform-token";
const PHONE: &str = "+16505551234";

/// M3.5: logs hold neither code nor number. Captured at `TRACE` for every
/// target (the library's, the service's), an issue, a wrong and a right
/// check, a cooldown and a send Meta refuses log neither the code nor the
/// recipient's number, with or without its `+` and country code.
#[tokio::test]
async fn m3_5_logs_hold_neither_code_nor_number() {
    let h = Harness::with_settings(Settings {
        otp: Some(otp_deployment(Some(PLATFORM))),
        ..test_settings()
    });
    h.tenant(PLATFORM).await;
    h.tenant("shop-a").await;
    h.connect(PLATFORM, WABA, &[PN], TOKEN).await;
    let mut settings = OtpSettings::new(PhoneNumberId::new(PN).unwrap(), "login_code", "en_US");
    // Eight digits: a code is looked for as a whole run of digits, which a
    // log's times and durations make of their own.
    settings.code_length = 8;
    h.store
        .set_otp_settings(&TenantId::parse(PLATFORM).unwrap(), Some(&settings))
        .await
        .unwrap();
    let key = h.tenant_key("shop-a", &[Scope::Otp]).await;
    let issue = || {
        Call::new(Method::POST, "/v1/otp/issue")
            .key(&key)
            .json(&json!({"phone": PHONE}))
    };
    let verify = |code: &str| {
        Call::new(Method::POST, "/v1/otp/verify")
            .key(&key)
            .json(&json!({"phone": PHONE, "code": code}))
    };
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    h.graph.push_json(
        200,
        json!({
            "messaging_product": "whatsapp",
            "contacts": [{"input": PHONE, "wa_id": "16505551234"}],
            "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
        }),
    );
    let sent = h.call(issue()).await;
    assert_eq!(sent.json()["status"], "sent", "{}", sent.text);
    let body = h.graph.last_request().unwrap().json().unwrap();
    let code = body["template"]["components"][0]["parameters"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(code.len(), 8);
    let wrong = if code == "00000000" {
        "11111111"
    } else {
        "00000000"
    };
    assert_eq!(h.call(verify(wrong)).await.json()["result"], "invalid");
    assert_eq!(h.call(issue()).await.json()["status"], "cooling_down");
    assert_eq!(h.call(verify(&code)).await.json()["result"], "verified");
    // A send Meta refuses: its error path logs too.
    h.clock.advance(std::time::Duration::from_secs(31));
    h.graph.push_json(
        400,
        json!({"error": {"message": "(#132001) Template name does not exist in the translation",
            "type": "OAuthException", "code": 132001, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h.call(issue()).await;
    assert_eq!(refused.code(), "template_not_found", "{}", refused.text);
    let logs = captured.text();
    assert!(
        logs.contains("/v1/otp/issue") && logs.contains("/v1/otp/verify"),
        "the capture saw the requests:\n{logs}"
    );
    assert!(logs.contains("graph request"), "and the library's events");
    assert!(
        !digit_runs(&logs).any(|run| run == code),
        "the code was logged:\n{logs}"
    );
    for number in [PHONE, "16505551234", "6505551234"] {
        assert!(!logs.contains(number), "{number} was logged:\n{logs}");
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// Every maximal run of ASCII digits in `text`.
fn digit_runs(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|run| !run.is_empty())
}
