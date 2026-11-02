//! The OTP routes' decisive scenario (roadmap M3c), run on memory
//! (`otp.rs`) and on Postgres (`live_postgres.rs`): acceptance test M3.5's
//! namespace over HTTP, with the settings set through the admin route.

use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{AllowedTenants, Scope, TenantId};
use serde_json::{Value, json};

use super::{Call, Harness};

/// The default OTP tenant of the scenario's harness
/// (`otp_deployment(Some(PLATFORM))`).
pub const PLATFORM: &str = "platform";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-platform-token";
const PHONE: &str = "+16505551234";

fn accepted() -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": PHONE, "wa_id": "16505551234"}],
        "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
    })
}

/// M3.5, decisive (the namespace), on `h`, whose default OTP tenant is
/// [`PLATFORM`]: the platform's settings set by the admin on its own
/// number; shop A's code, sent from it, is `not_found` for shop B (its key,
/// and a platform key naming it) and for the platform itself, five times
/// each, then `verified` for A: no other tenant's check counted an
/// attempt. B's own code, from the same number to the same recipient, is
/// B's; and the outcomes on the way (`sent`, `invalid`, `cooling_down`,
/// `not_found` once used).
pub async fn a_code_verifies_at_no_other_tenant_on_the_same_number(h: &Harness) {
    for tenant in [PLATFORM, "shop-a", "shop-b"] {
        h.tenant(tenant).await;
    }
    h.connect(PLATFORM, WABA, &[PN], TOKEN).await;
    let admin = h.admin_key().await;
    let saved = h
        .call(
            Call::new(Method::PUT, format!("/v1/admin/tenants/{PLATFORM}/otp"))
                .key(&admin)
                .json(&json!({"phone_number_id": PN, "template": {"name": "login_code", "language": "en_US"}})),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);
    let a = h.tenant_key("shop-a", &[Scope::Otp]).await;
    let b = h.tenant_key("shop-b", &[Scope::Otp]).await;
    let platform = h.tenant_key(PLATFORM, &[Scope::Otp]).await;
    let as_b = h
        .platform_key(
            AllowedTenants::Only(vec![TenantId::parse("shop-b").unwrap()]),
            &[Scope::Otp],
        )
        .await;
    let issue = |key: &str| {
        Call::new(Method::POST, "/v1/otp/issue")
            .key(key)
            .json(&json!({"phone": PHONE}))
    };
    let verify = |key: &str, code: &str| {
        Call::new(Method::POST, "/v1/otp/verify")
            .key(key)
            .json(&json!({"phone": PHONE, "code": code}))
    };
    let code_sent = || {
        let request = h.graph.last_request().unwrap();
        assert_eq!(request.path(), format!("/v25.0/{PN}/messages"));
        assert_eq!(request.bearer(), Some(TOKEN));
        request.json().unwrap()["template"]["components"][0]["parameters"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    h.graph.push_json(200, accepted());
    let sent = h.call(issue(&a)).await;
    assert_eq!(sent.json()["status"], "sent", "{}", sent.text);
    let code = code_sent();
    for _ in 0..5 {
        for (key, tenant) in [(&b, None), (&as_b, Some("shop-b")), (&platform, None)] {
            let mut call = verify(key, &code);
            if let Some(tenant) = tenant {
                call = call.tenant(tenant);
            }
            let reply = h.call(call).await;
            assert_eq!(reply.json(), json!({"result": "not_found"}), "{tenant:?}");
        }
    }
    let wrong = if code == "000000" { "111111" } else { "000000" };
    assert_eq!(
        h.call(verify(&a, wrong)).await.json(),
        json!({"result": "invalid", "attempts_left": 4}),
        "one attempt counted: A's own"
    );
    assert_eq!(
        h.call(issue(&a)).await.json()["status"],
        "cooling_down",
        "A's cooldown"
    );
    // B's cooldown is its own: B sends to the same recipient at once.
    h.graph.push_json(200, accepted());
    assert_eq!(h.call(issue(&b)).await.json()["status"], "sent");
    let b_code = code_sent();
    assert_eq!(
        h.call(verify(&a, &code)).await.json(),
        json!({"result": "verified"})
    );
    assert_eq!(
        h.call(verify(&a, &code)).await.json(),
        json!({"result": "not_found"}),
        "used up"
    );
    assert_eq!(
        h.call(verify(&b, &b_code)).await.json(),
        json!({"result": "verified"})
    );
    assert_eq!(h.graph.remaining(), 0);
}
