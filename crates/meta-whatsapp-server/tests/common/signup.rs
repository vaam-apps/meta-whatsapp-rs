//! Embedded Signup's test fixtures (roadmap M3a): the deployment in both
//! partner modes, the page's posts, and Meta's answers, as the library's
//! onboarding tests take them from Meta's pages (`embedded-signup/*`,
//! `access-tokens`, `solution-providers/*`, `webhooks/reference/account_update`).
#![allow(dead_code)] // each test binary uses a different subset

use meta_whatsapp_rs::client::AppCredentials;
use meta_whatsapp_rs::client::credit_lines::WabaCurrency;
use meta_whatsapp_rs::client::embedded_signup::SolutionPartner;
use meta_whatsapp_rs::core::ids::AppId;
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_rs::webhooks::axum::http::Method;
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::signup::SignupDeployment;
use serde_json::{Value, json};

use super::{APP_SECRET, Call, Harness, test_settings};

/// The Meta app (`embedded-signup/implementation`'s example app id).
pub const APP_ID: &str = "236484624622562";
/// The Facebook Login for Business configuration.
pub const CONFIG_ID: &str = "<CONFIGURATION_ID>";
/// The page's code: 30 seconds, single use.
pub const CODE: &str = "AQBhlXsctMxJYbwbrpybxlo9tLPGy";
/// The business token the exchange answers: in no answer, log or row.
pub const TOKEN: &str = "EAAAN6tcBzAUBOwtDtTfmZCJ9n3FHpSDcDTH86ekf89Xnn";
/// `embedded-signup/implementation`, session logging example values.
pub const WABA: &str = "524126980791429";
pub const PHONE: &str = "106540352242922";
/// A second number of the WABA, which Meta lists too.
pub const PHONE_2: &str = "106540352242923";
pub const BUSINESS: &str = "2729063490586005";
/// The merchant's own two-step verification PIN.
pub const PIN: &str = "581063";
/// The partner's system user, its token, its credit line, and the
/// allocation a share answers (`solution-providers/share-and-revoke-credit-lines`).
pub const SYSTEM_TOKEN: &str = "EAAG-the-partners-system-token";
pub const SYSTEM_USER: &str = "1972555232742222";
pub const LINE: &str = "1972385232742146";
pub const ALLOCATION: &str = "58501441721238";

/// A Tech Provider's signup.
pub fn tech_provider() -> SignupDeployment {
    SignupDeployment::new(
        AppCredentials::new(AppId::new(APP_ID).unwrap(), APP_SECRET),
        CONFIG_ID,
    )
}

/// A Solution Partner's signup: [`LINE`], shared with Meta's current
/// method, in USD by default.
pub fn solution_partner() -> SignupDeployment {
    tech_provider().solution_partner(
        SolutionPartner::new(
            AccessToken::new(SYSTEM_TOKEN),
            SYSTEM_USER.to_owned(),
            LINE.to_owned(),
        )
        .default_currency(WabaCurrency::Usd),
    )
}

/// The test settings with `deployment`.
pub fn settings(deployment: SignupDeployment) -> Settings {
    Settings {
        signup: Some(deployment),
        ..test_settings()
    }
}

/// A harness whose signup is `deployment`.
pub fn harness(deployment: SignupDeployment) -> Harness {
    Harness::with_settings(settings(deployment))
}

/// `POST /v1/signup/sessions` with `key`: the state.
pub async fn start(h: &Harness, key: &str) -> String {
    let started = h
        .call(
            Call::new(Method::POST, "/v1/signup/sessions")
                .key(key)
                .json(&json!({})),
        )
        .await;
    assert_eq!(started.status, 200, "{}", started.text);
    started.json()["state"].as_str().unwrap().to_owned()
}

/// What the page posts after a Cloud API flow, with the merchant's PIN.
pub fn finished(state: &str) -> Value {
    json!({
        "state": state,
        "code": CODE,
        "event": {
            "data": {"phone_number_id": PHONE, "waba_id": WABA, "business_id": BUSINESS},
            "type": "WA_EMBEDDED_SIGNUP",
            "event": "FINISH"
        },
        "pin": PIN
    })
}

/// `POST /v1/signup/complete` of `body` with `key`.
pub fn complete(key: &str, body: &Value) -> Call {
    Call::new(Method::POST, "/v1/signup/complete")
        .key(key)
        .json(body)
}

/// `POST /v1/signup/resume` of `body` with `key`.
pub fn resume(key: &str, body: &Value) -> Call {
    Call::new(Method::POST, "/v1/signup/resume")
        .key(key)
        .json(body)
}

/// Meta's answers up to and including `verify_assets`: the code exchange,
/// `debug_token` (`access-tokens`' shape, the WABA granted), the WABA's
/// owner (`owner_business_info`), its numbers (both of them).
pub fn script_until_verified(graph: &ScriptedTransport) {
    graph.push_json(200, json!({"access_token": TOKEN, "token_type": "bearer"}));
    graph.push_json(
        200,
        json!({"data": {
          "app_id": APP_ID, "type": "SYSTEM_USER", "application": "Jaspers", "is_valid": true,
          "expires_at": 0, "data_access_expires_at": 0,
          "scopes": ["whatsapp_business_management", "whatsapp_business_messaging"],
          "granular_scopes": [
            {"scope": "whatsapp_business_management", "target_ids": [WABA]},
            {"scope": "whatsapp_business_messaging", "target_ids": [WABA]}
          ],
          "user_id": "1"
        }}),
    );
    graph.push_json(
        200,
        json!({"owner_business_info": {"name": "Wind & Wool", "id": BUSINESS}, "id": WABA}),
    );
    graph.push_json(200, json!({"data": [{"id": PHONE}, {"id": PHONE_2}]}));
}

/// `{"success": true}`.
pub fn success() -> Value {
    json!({"success": true})
}

/// A Solution Partner's credit steps: the system user added, nothing
/// shared yet, then shared and attached.
pub fn script_share(graph: &ScriptedTransport) {
    graph.push_json(200, success()); // assigned_users
    graph.push_json(200, json!({"data": []})); // owning_credit_allocation_configs
    graph.push_json(
        200,
        json!({"allocation_config_id": ALLOCATION, "waba_id": WABA}),
    );
}

/// A revocation of [`ALLOCATION`]: Meta's record of it, its status (not
/// revoked), the `DELETE`, its status `DELETED`.
pub fn script_revocation(graph: &ScriptedTransport) {
    let business = json!({"name": "Wind & Wool", "id": BUSINESS});
    graph.push_json(
        200,
        json!({"id": ALLOCATION, "receiving_business": business}),
    );
    graph.push_json(200, json!({"receiving_business": business}));
    graph.push_json(200, success());
    graph.push_json(
        200,
        json!({"receiving_business": business, "request_status": "DELETED"}),
    );
}

/// A revocation finding [`ALLOCATION`] revoked already.
pub fn script_already_revoked(graph: &ScriptedTransport) {
    let business = json!({"name": "Wind & Wool", "id": BUSINESS});
    graph.push_json(
        200,
        json!({"id": ALLOCATION, "receiving_business": business}),
    );
    graph.push_json(
        200,
        json!({"receiving_business": business, "request_status": "DELETED"}),
    );
}

/// `debug_token` on the stored token, confirming the WABA left (the token
/// no longer valid).
pub fn script_confirmed(graph: &ScriptedTransport) {
    graph.push_json(200, json!({"data": {"is_valid": false}}));
}

/// An `account_update` webhook of `event` (Meta's examples,
/// `webhooks/reference/account_update`: the entry is the partner's
/// business portfolio, the WABA in `waba_info`).
pub fn account_update(event: &str, info: &Value) -> Vec<u8> {
    // Dated now: an event dated before its WABA's binding began is
    // operator-only (docs/design/server.md, section 2.3).
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    json!({"object": "whatsapp_business_account", "entry": [{
        "id": "2949482758682047", "time": now,
        "changes": [{"field": "account_update", "value": {"event": event, "waba_info": info}}]
    }]})
    .to_string()
    .into_bytes()
}

/// `PARTNER_APP_UNINSTALLED` of this deployment's app.
pub fn uninstalled() -> Vec<u8> {
    account_update(
        "PARTNER_APP_UNINSTALLED",
        &json!({"waba_id": WABA, "owner_business_id": BUSINESS, "partner_app_id": APP_ID}),
    )
}

/// `PARTNER_REMOVED` of the merchant's WABA.
pub fn removed() -> Vec<u8> {
    account_update(
        "PARTNER_REMOVED",
        &json!({"waba_id": WABA, "owner_business_id": BUSINESS}),
    )
}

// ─── Roadmap M3f: hosted signup, several WABAs, pre-verified numbers ────

/// A second WABA of the same business, and its number.
pub const WABA_2: &str = "524126980791430";
pub const PHONE_3: &str = "106540352242924";
/// The deployment's system token for hosted signup, and the
/// `appsecret_proof` of it under [`APP_SECRET`]
/// (`echo -n "$TOKEN" | openssl dgst -sha256 -hmac "$APP_SECRET"`).
pub const HOSTED_TOKEN: &str = "EAAG-hosted-system-token";
pub const HOSTED_PROOF: &str = "833fa583e46636ee6558857d1aa7d776530aee85868134033cb5c0b45685e624";
/// The deployment's own business portfolio (the pool's), its token, a
/// pre-verified number's id and the code it received.
pub const POOL_BUSINESS: &str = "506914307656634";
pub const POOL_TOKEN: &str = "EAAG-pool-system-token";
pub const PRE_VERIFIED: &str = "1972385232742999";
pub const PRE_VERIFIED_CODE: &str = "471986";

/// `PARTNER_ADDED` of `waba` and its owner `owner`
/// (`webhooks/reference/account_update`).
pub fn partner_added(waba: &str, owner: &str) -> Vec<u8> {
    account_update(
        "PARTNER_ADDED",
        &json!({"waba_id": waba, "owner_business_id": owner}),
    )
}

/// What the page posts after a flow that granted `waba_ids` (the first
/// the claimed WABA).
pub fn finished_with(state: &str, waba_ids: &[&str]) -> Value {
    json!({
        "state": state,
        "code": CODE,
        "event": {
            "data": {"phone_number_id": PHONE, "waba_id": waba_ids[0], "business_id": BUSINESS,
                     "waba_ids": waba_ids},
            "type": "WA_EMBEDDED_SIGNUP",
            "event": "FINISH"
        },
        "pin": PIN
    })
}

/// The code exchange and a `debug_token` granting [`WABA`] and [`WABA_2`].
pub fn script_two_grants(graph: &ScriptedTransport) {
    graph.push_json(200, json!({"access_token": TOKEN, "token_type": "bearer"}));
    graph.push_json(200, two_grants_debug());
}

/// `debug_token`'s answer granting [`WABA`] and [`WABA_2`].
pub fn two_grants_debug() -> Value {
    json!({"data": {
      "app_id": APP_ID, "type": "SYSTEM_USER", "is_valid": true,
      "expires_at": 0, "data_access_expires_at": 0,
      "scopes": ["whatsapp_business_management", "whatsapp_business_messaging"],
      "granular_scopes": [
        {"scope": "whatsapp_business_management", "target_ids": [WABA, WABA_2]},
        {"scope": "whatsapp_business_messaging", "target_ids": [WABA, WABA_2]}
      ],
      "user_id": "1"
    }})
}

/// A WABA's owner and its numbers.
pub fn script_waba(graph: &ScriptedTransport, waba: &str, numbers: &[&str]) {
    graph.push_json(
        200,
        json!({"owner_business_info": {"name": "Wind & Wool", "id": BUSINESS}, "id": waba}),
    );
    let data: Vec<Value> = numbers.iter().map(|n| json!({"id": n})).collect();
    graph.push_json(200, json!({ "data": data }));
}

/// `debug_token`'s answer granting [`WABA`] only.
pub fn one_grant_debug() -> Value {
    json!({"data": {
      "app_id": APP_ID, "type": "SYSTEM_USER", "is_valid": true,
      "expires_at": 0, "data_access_expires_at": 0,
      "scopes": ["whatsapp_business_management", "whatsapp_business_messaging"],
      "granular_scopes": [
        {"scope": "whatsapp_business_management", "target_ids": [WABA]},
        {"scope": "whatsapp_business_messaging", "target_ids": [WABA]}
      ],
      "user_id": "1"
    }})
}

/// `debug_token`'s answer granting exactly `wabas`.
pub fn debug_granting(wabas: &[&str]) -> Value {
    json!({"data": {
      "app_id": APP_ID, "type": "SYSTEM_USER", "is_valid": true,
      "expires_at": 0, "data_access_expires_at": 0,
      "scopes": ["whatsapp_business_management", "whatsapp_business_messaging"],
      "granular_scopes": [
        {"scope": "whatsapp_business_management", "target_ids": wabas},
        {"scope": "whatsapp_business_messaging", "target_ids": wabas}
      ],
      "user_id": "1"
    }})
}

/// A deployment with all of M3f on: every granted WABA bound, hosted
/// signup, and the pool of pre-verified numbers.
pub fn everything() -> SignupDeployment {
    tech_provider()
        .every_granted_waba(true)
        .hosted_signup(AccessToken::new(HOSTED_TOKEN))
        .pre_verified_pool(
            meta_whatsapp_rs::core::ids::BusinessId::new(POOL_BUSINESS).unwrap(),
            AccessToken::new(POOL_TOKEN),
        )
}
