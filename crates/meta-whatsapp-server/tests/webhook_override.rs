//! A WABA's health status and a number's webhook override (roadmap M5c6;
//! design D440 to D444): `GET /v1/wabas/{waba_id}/health-status`, and `GET`,
//! `PUT`, `DELETE /v1/numbers/{pn}/webhook-override`.
//!
//! The decisive ones: another tenant's WABA is `404` with nothing asked of
//! Meta and the vault unread; the override's destination is refused as a
//! webhook endpoint's is (the same URLs, both routes, same answer); with the
//! setting off a set asks Meta nothing; clearing is always allowed. Meta's
//! examples are `support/health-status` and `webhooks/override`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use common::{Call, Harness};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{AllowedTenants, Scope, TenantId};
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::webhooks_out::destination::{
    DestinationPolicy, ResolveError, Resolver,
};
use meta_whatsapp_server_core::webhooks_out::dispatch::WebhooksOut;
use meta_whatsapp_server_core::webhooks_out::seal::{DataKey, DataKeys};
use meta_whatsapp_server_http::webhooks_out::HyperSender;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const OTHER_TENANT: &str = "merchant-43";
const OTHER_WABA: &str = "102290129340399";
const OTHER_PN: &str = "106540352242923";
const OTHER_TOKEN: &str = "EAAG-other-merchant-token";
/// `webhooks/override`'s example URL and token for a phone number.
const HOOK: &str = "https://my-phone-alternate-callback.com/webhook";
const VERIFY: &str = "myvoiceismypassport?";
const SENTINEL: &str = "META-TEXT-SENTINEL-7XK";

/// Names the allow-list lists, and the addresses the stub gives them.
struct Names;

#[async_trait]
impl Resolver for Names {
    async fn resolve(&self, host: &str, _: u16) -> Result<Vec<IpAddr>, ResolveError> {
        let answer: &[&str] = match host {
            "my-phone-alternate-callback.com" | "hooks.example.com" => &["93.184.216.34"],
            // A listed name that points inside.
            "inside.example.com" => &["10.0.0.5"],
            "metadata.example.com" => &["169.254.169.254"],
            // One public address and one private: every one must pass.
            "mixed.example.com" => &["93.184.216.34", "127.0.0.1"],
            _ => return Err(ResolveError),
        };
        Ok(answer.iter().map(|a| a.parse().unwrap()).collect())
    }
}

/// Webhooks out as a production deployment would have it: the data key, an
/// allow-list of names (no CIDR, no plain `http`), the stub lookup.
fn out() -> WebhooksOut {
    WebhooksOut::new(
        DataKeys::new(DataKey::generate("k1").unwrap()),
        DestinationPolicy::parse(
            "my-phone-alternate-callback.com, hooks.example.com, inside.example.com, \
             metadata.example.com, mixed.example.com, unresolved.example.com",
            false,
        )
        .unwrap(),
        Arc::new(Names),
        Arc::new(HyperSender::new().unwrap()),
    )
}

/// Two tenants with a WABA and a number each; the override `enabled` or
/// not, webhooks out `configured` or not.
async fn harness(enabled: bool, configured: bool) -> Harness {
    let h = Harness::with_settings(Settings {
        number_webhook_override: enabled,
        webhooks_out: configured.then(out),
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], OTHER_TOKEN)
        .await;
    h
}

fn url(pn: &str) -> String {
    format!("/v1/numbers/{pn}/webhook-override")
}

fn put(pn: &str, key: &str, hook: &str) -> Call {
    Call::new(Method::PUT, url(pn))
        .key(key)
        .json(&json!({"url": hook, "verify_token": VERIFY}))
}

fn delete(pn: &str, key: &str) -> Call {
    Call::new(Method::DELETE, url(pn)).key(key)
}

fn the_request(h: &Harness) -> (Method, String, Option<Value>) {
    let requests = h.graph.requests();
    let [asked] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(asked.bearer(), Some(TOKEN));
    (asked.method.clone(), asked.path().to_owned(), asked.json())
}

fn graph_error(details: &str) -> Value {
    json!({"error": {
        "message": SENTINEL, "type": "OAuthException", "code": 100,
        "error_data": {"messaging_product": "whatsapp", "details": details},
        "fbtrace_id": "A1b2C3"
    }})
}

// ─── A WABA's health status ──────────────────────────────────────────────

/// `support/health-status`'s shape for a WABA, with entities the tenant
/// may not see: another tenant's number, another's WABA, a type this version
/// does not know. The request is the library's (`?fields=health_status`,
/// the tenant's token); the answer keeps the tenant's WABA, its number, a
/// template, the business portfolio and the app, and drops the rest with
/// their errors. Decisive: each arm of `Waba::health_status`'s filter.
#[tokio::test]
async fn a_wabas_health_status_withholds_other_tenants_entities() {
    let h = harness(false, false).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    h.graph.push_json(
        200,
        json!({
            "health_status": {
                "can_send_message": "BLOCKED",
                "entities": [
                    {"entity_type": "WABA", "id": WABA, "can_send_message": "AVAILABLE"},
                    {"entity_type": "WABA", "id": OTHER_WABA, "can_send_message": "BLOCKED",
                     "errors": [{"error_code": 141_000, "error_description": SENTINEL,
                                 "possible_solution": "other tenant's"}]},
                    {"entity_type": "PHONE_NUMBER", "id": PN, "can_send_message": "AVAILABLE",
                     "can_receive_call_sip": "AVAILABLE"},
                    {"entity_type": "PHONE_NUMBER", "id": OTHER_PN, "can_send_message": "BLOCKED",
                     "errors": [{"error_code": 141_001, "error_description": SENTINEL}]},
                    {"entity_type": "PHONE_NUMBER", "id": "not a number id"},
                    {"entity_type": "MESSAGE_TEMPLATE", "id": "1234567890",
                     "can_send_message": "LIMITED", "additional_info": ["paused"]},
                    {"entity_type": "BUSINESS", "id": "2729063490586005",
                     "can_send_message": "AVAILABLE"},
                    {"entity_type": "APP", "id": "634974688087057",
                     "can_send_message": "AVAILABLE"},
                    {"entity_type": "SOMETHING_NEW", "id": "55555", "can_send_message": "BLOCKED"}
                ]
            },
            "id": WABA
        }),
    );
    let reply = h
        .call(Call::get(format!("/v1/wabas/{WABA}/health-status")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let (method, path, body) = the_request(&h);
    assert_eq!(
        (method, path.as_str(), body),
        (Method::GET, "/v25.0/102290129340398", None)
    );
    let asked = &h.graph.requests()[0];
    assert_eq!(asked.query("fields").as_deref(), Some("health_status"));
    assert_eq!(
        reply.json(),
        json!({
            "can_send_message": "BLOCKED",
            "entities": [
                {"entity_type": "WABA", "id": WABA, "can_send_message": "AVAILABLE",
                 "can_receive_call_sip": null, "additional_info": [], "errors": []},
                {"entity_type": "PHONE_NUMBER", "id": PN, "can_send_message": "AVAILABLE",
                 "can_receive_call_sip": "AVAILABLE", "additional_info": [], "errors": []},
                {"entity_type": "MESSAGE_TEMPLATE", "id": "1234567890",
                 "can_send_message": "LIMITED", "can_receive_call_sip": null,
                 "additional_info": ["paused"], "errors": []},
                {"entity_type": "BUSINESS", "id": "2729063490586005",
                 "can_send_message": "AVAILABLE", "can_receive_call_sip": null,
                 "additional_info": [], "errors": []},
                {"entity_type": "APP", "id": "634974688087057",
                 "can_send_message": "AVAILABLE", "can_receive_call_sip": null,
                 "additional_info": [], "errors": []}
            ]
        })
    );
    for withheld in [OTHER_WABA, OTHER_PN, SENTINEL, "55555"] {
        assert!(!reply.text.contains(withheld), "{withheld}: {}", reply.text);
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// M5.1 and M5.2, the roadmap's decisive test for M5c6: another tenant's
/// WABA's health status is `404`, nothing asked of Meta, the vault never
/// read; a platform key limited to the other tenant, and a tenant key
/// without the scope, are refused too.
#[tokio::test]
async fn another_tenants_wabas_health_status_is_not_found_and_nothing_is_asked() {
    let h = harness(false, false).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    let reads = h.kv.vault_reads();
    let reply = h
        .call(Call::get(format!("/v1/wabas/{OTHER_WABA}/health-status")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    assert_eq!(reply.json()["error"]["code"], "not_found");
    // A WABA that does not exist is the same answer.
    let missing = h
        .call(Call::get("/v1/wabas/999000999000/health-status").key(&key))
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    // Indistinguishable from the other tenant's (the request id apart).
    for body in [missing.json(), reply.json()] {
        assert_eq!(body["error"]["code"], "not_found");
        assert_eq!(body["error"]["message"], "Not found.");
    }
    // A platform key allowed only the other tenant, acting as ours.
    let platform = h
        .platform_key(
            AllowedTenants::Only(vec![TenantId::parse(OTHER_TENANT).unwrap()]),
            &[Scope::Numbers],
        )
        .await;
    let acting = h
        .call(
            Call::get(format!("/v1/wabas/{WABA}/health-status"))
                .key(&platform)
                .tenant(TENANT),
        )
        .await;
    assert_eq!(acting.status, StatusCode::FORBIDDEN, "{}", acting.text);
    // Without the scope.
    let events = h.tenant_key(TENANT, &[Scope::Events]).await;
    let denied = h
        .call(Call::get(format!("/v1/wabas/{WABA}/health-status")).key(&events))
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert!(h.graph.requests().is_empty(), "{:?}", h.graph.requests());
    assert_eq!(h.kv.vault_reads(), reads, "the vault was read");
}

/// Meta's refusal of the read is passed on with its code.
#[tokio::test]
async fn a_wabas_health_status_passes_metas_refusal_on() {
    let h = harness(false, false).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    h.graph.push_json(400, graph_error("nope"));
    let reply = h
        .call(Call::get(format!("/v1/wabas/{WABA}/health-status")).key(&key))
        .await;
    assert!(reply.status.is_client_error() || reply.status.is_server_error());
    assert_eq!(h.graph.remaining(), 0);
}

// ─── The override: destination ───────────────────────────────────────────

/// Destinations a webhook endpoint refuses (`422` on `url`) and the
/// override must too. Names and addresses are the allow-list's and the
/// stub's above.
const REFUSED: &[&str] = &[
    // A private address, loopback, link-local, the cloud metadata address,
    // IPv6 loopback, unspecified, none in the list.
    "https://10.0.0.1/hook",
    "https://127.0.0.1/hook",
    "https://169.254.169.254/latest/meta-data",
    "https://[::1]/hook",
    "https://0.0.0.0/hook",
    "https://2130706433/hook",
    // Plain http, where it is not allowed.
    "http://hooks.example.com/hook",
    // A listed name that resolves to a private address, to the metadata
    // address, or to a public and a private one.
    "https://inside.example.com/hook",
    "https://metadata.example.com/hook",
    "https://mixed.example.com/hook",
    // Credentials in the URL.
    "https://user:secret@hooks.example.com/hook",
    "https://user@hooks.example.com/hook",
    // A name that is not listed, or only looks like one that is.
    "https://evil.example/hook",
    "https://hooks.example.com.evil.example/hook",
    // A listed name that does not resolve.
    "https://unresolved.example.com/hook",
    // Not a URL.
    "not a url",
    "",
];

/// Decisive for M5c6: the override's destination is refused as a webhook
/// endpoint's is. The same URLs go to `POST /v1/webhook-endpoints` and to
/// `PUT /v1/numbers/{pn}/webhook-override` on one deployment: every one
/// refused by the first is refused by the second, `422` on `url`, with
/// nothing sent to Meta; and what the first admits, the second sends.
#[tokio::test]
async fn the_overrides_destination_is_refused_as_a_webhook_endpoints_is() {
    let h = harness(true, true).await;
    let key = h
        .tenant_key(TENANT, &[Scope::Numbers, Scope::Webhooks])
        .await;
    for hook in REFUSED {
        let endpoint = h
            .call(
                Call::new(Method::POST, "/v1/webhook-endpoints")
                    .key(&key)
                    .json(&json!({"url": hook, "types": ["message_received"]})),
            )
            .await;
        assert_eq!(
            (endpoint.status, endpoint.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("url")),
            "the endpoint route admitted {hook:?}: {}",
            endpoint.text
        );
        let reply = h.call(put(PN, &key, hook)).await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("url")),
            "{hook:?}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["code"], "invalid_request", "{hook:?}");
    }
    assert!(
        h.graph.requests().is_empty(),
        "something was sent to Meta: {:?}",
        h.graph.requests()
    );
    // What the endpoint route admits, the override sends.
    for hook in [HOOK, "https://hooks.example.com/wa?shop=1"] {
        let endpoint = h
            .call(
                Call::new(Method::POST, "/v1/webhook-endpoints")
                    .key(&key)
                    .json(&json!({"url": hook, "types": ["message_received"]})),
            )
            .await;
        assert_eq!(endpoint.status, StatusCode::CREATED, "{hook}");
        h.graph.push_json(200, json!({"success": true}));
        let reply = h.call(put(PN, &key, hook)).await;
        assert_eq!(reply.status, StatusCode::NO_CONTENT, "{hook}: {}", reply.text);
    }
    assert_eq!(h.graph.requests().len(), 2);
    assert_eq!(h.graph.remaining(), 0);
}

/// The shape Meta documents: 200 characters at most, a verify token that is
/// not empty, no other field. Nothing is sent.
#[tokio::test]
async fn a_url_too_long_or_a_token_missing_is_refused_before_anything_is_sent() {
    let h = harness(true, true).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    let long = format!("https://hooks.example.com/{}", "a".repeat(200));
    let reply = h.call(put(PN, &key, &long)).await;
    assert_eq!(reply.json()["error"]["field"], "url", "{}", reply.text);
    let reply = h
        .call(
            Call::new(Method::PUT, url(PN))
                .key(&key)
                .json(&json!({"url": HOOK, "verify_token": ""})),
        )
        .await;
    assert_eq!(
        (reply.status, reply.json()["error"]["field"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("verify_token"))
    );
    for body in [
        json!({"url": HOOK}),
        json!({"verify_token": VERIFY}),
        json!({"url": HOOK, "verify_token": VERIFY, "also": 1}),
    ] {
        let reply = h
            .call(Call::new(Method::PUT, url(PN)).key(&key).json(&body))
            .await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("body")),
            "{body}"
        );
    }
    assert!(h.graph.requests().is_empty());
}

// ─── The override: the setting, the request, clearing ───────────────────

/// Decisive: with the setting off (the default) a set is `403
/// feature_not_available`, whatever the key and however good the URL,
/// before the number is looked up (the vault unread) and with nothing sent
/// to Meta; the read and the clear still work.
#[tokio::test]
async fn with_the_setting_off_a_set_is_refused_and_nothing_is_sent() {
    let h = harness(false, true).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    let platform = h
        .platform_key(AllowedTenants::Only(vec![TenantId::parse(TENANT).unwrap()]), &[Scope::Numbers])
        .await;
    let reads = h.kv.vault_reads();
    let reply = h.call(put(PN, &key, HOOK)).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text);
    assert_eq!(reply.json()["error"]["code"], "feature_not_available");
    let acting = h
        .call(put(PN, &platform, HOOK).tenant(TENANT))
        .await;
    assert_eq!(acting.json()["error"]["code"], "feature_not_available");
    assert!(h.graph.requests().is_empty(), "{:?}", h.graph.requests());
    assert_eq!(h.kv.vault_reads(), reads, "the vault was read");
    // Nothing else is off: the read, and the clear.
    h.graph.push_json(200, json!({"webhook_configuration": {}, "id": PN}));
    assert_eq!(
        h.call(Call::get(url(PN)).key(&key)).await.status,
        StatusCode::OK
    );
    h.graph.push_json(200, json!({"success": true}));
    assert_eq!(h.call(delete(PN, &key)).await.status, StatusCode::NO_CONTENT);
    assert_eq!(h.graph.requests().len(), 2);
}

/// With the setting on and no webhooks out (no allow-list to check the URL
/// against), a set is `409 webhooks_not_configured` and nothing is sent: the
/// destination is never trusted unchecked.
#[tokio::test]
async fn without_webhooks_out_a_set_is_not_configured_and_nothing_is_sent() {
    let h = harness(true, false).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    let reply = h.call(put(PN, &key, HOOK)).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.text);
    assert_eq!(reply.json()["error"]["code"], "webhooks_not_configured");
    assert!(h.graph.requests().is_empty());
}

/// Meta's documented requests: the set is `POST /{pn}` with
/// `webhook_configuration` carrying the URL and the token (the page's phone
/// number example), the clear the same with an empty URL and no token; both
/// with the tenant's token. The token is in no answer.
#[tokio::test]
async fn the_set_and_the_clear_are_metas_documented_requests() {
    let h = harness(true, true).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(put(PN, &key, HOOK)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    assert!(reply.text.is_empty(), "{}", reply.text);
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(delete(PN, &key)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let requests = h.graph.requests();
    let [set, clear] = &requests[..] else {
        panic!("{requests:?}")
    };
    for asked in [set, clear] {
        assert_eq!(asked.bearer(), Some(TOKEN));
        assert_eq!(
            (asked.method.clone(), asked.path()),
            (Method::POST, "/v25.0/106540352242922")
        );
    }
    assert_eq!(
        set.json(),
        Some(json!({"webhook_configuration": {
            "override_callback_uri": HOOK, "verify_token": VERIFY
        }}))
    );
    assert_eq!(
        clear.json(),
        Some(json!({"webhook_configuration": {"override_callback_uri": ""}}))
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// Decisive: clearing is always allowed to the number's owner and to the
/// operator (a platform key acting as the tenant), whatever the setting,
/// with webhooks out missing, and for a key that never could set.
#[tokio::test]
async fn clearing_is_always_allowed() {
    for (enabled, configured) in [(false, false), (false, true), (true, false), (true, true)] {
        let h = harness(enabled, configured).await;
        let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
        let platform = h
            .platform_key(AllowedTenants::Only(vec![TenantId::parse(TENANT).unwrap()]), &[Scope::Numbers])
            .await;
        for call in [
            delete(PN, &key),
            delete(PN, &platform).tenant(TENANT),
        ] {
            h.graph.push_json(200, json!({"success": true}));
            let reply = h.call(call).await;
            assert_eq!(
                reply.status,
                StatusCode::NO_CONTENT,
                "setting {enabled}, webhooks out {configured}: {}",
                reply.text
            );
        }
        assert_eq!(h.graph.requests().len(), 2);
        for request in h.graph.requests() {
            assert_eq!(
                request.json(),
                Some(json!({"webhook_configuration": {"override_callback_uri": ""}}))
            );
        }
    }
}

/// `webhooks/override`'s "Get phone number alternate callback": the answer is
/// the number's own URL; the WABA's override and the app's callback URL,
/// which Meta's answer also carries (the latter the service's own), are not
/// answered. No override: `null`.
#[tokio::test]
async fn the_read_answers_the_numbers_own_url_only() {
    let h = harness(false, false).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    h.graph.push_json(
        200,
        json!({"webhook_configuration": {
            "phone_number": HOOK,
            "whatsapp_business_account": "https://my-waba-alternate-callback.com/webhook",
            "application": "https://my-production-callback.com/webhook"
        }, "id": PN}),
    );
    let reply = h.call(Call::get(url(PN)).key(&key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json(), json!({"url": HOOK}));
    for withheld in ["my-waba-alternate", "my-production-callback"] {
        assert!(!reply.text.contains(withheld), "{}", reply.text);
    }
    let (method, path, _) = the_request(&h);
    assert_eq!((method, path.as_str()), (Method::GET, "/v25.0/106540352242922"));
    assert_eq!(
        h.graph.requests()[0].query("fields").as_deref(),
        Some("webhook_configuration")
    );
    h.graph.push_json(
        200,
        json!({"webhook_configuration": {
            "application": "https://my-production-callback.com/webhook"
        }, "id": PN}),
    );
    let reply = h.call(Call::get(url(PN)).key(&key)).await;
    assert_eq!(reply.json(), json!({"url": null}), "{}", reply.text);
}

/// Meta refusing the set (it could not verify the URL, say) is passed on
/// without its texts (they may quote what was sent) and without the token;
/// a timeout says the call may have gone through.
#[tokio::test]
async fn a_failed_set_withholds_metas_texts_and_the_token() {
    let h = harness(true, true).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    h.graph.push_json(400, graph_error(&format!("{VERIFY} {SENTINEL}")));
    let reply = h.call(put(PN, &key, HOOK)).await;
    assert!(!reply.status.is_success(), "{}", reply.text);
    for secret in [SENTINEL, VERIFY] {
        assert!(!reply.text.contains(secret), "{}", reply.text);
    }
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    let reply = h.call(put(PN, &key, HOOK)).await;
    assert_eq!(reply.status, StatusCode::GATEWAY_TIMEOUT, "{}", reply.text);
    assert!(!reply.text.contains(VERIFY));
}

/// M5.1 and M5.2 for the three routes: another tenant's number is `404`,
/// nothing asked of Meta, the vault never read; a key without the scope is
/// `403`.
#[tokio::test]
async fn another_tenants_number_is_not_found_on_every_override_route() {
    let h = harness(true, true).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    let reads = h.kv.vault_reads();
    for call in [
        Call::get(url(OTHER_PN)).key(&key),
        put(OTHER_PN, &key, HOOK),
        delete(OTHER_PN, &key),
    ] {
        let reply = h.call(call).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    }
    let events = h.tenant_key(TENANT, &[Scope::Events]).await;
    for call in [
        Call::get(url(PN)).key(&events),
        put(PN, &events, HOOK),
        delete(PN, &events),
    ] {
        assert_eq!(h.call(call).await.status, StatusCode::FORBIDDEN);
    }
    assert!(h.graph.requests().is_empty());
    assert_eq!(h.kv.vault_reads(), reads, "the vault was read");
}
