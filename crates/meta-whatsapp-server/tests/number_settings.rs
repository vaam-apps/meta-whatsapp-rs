//! A number's settings (scope `numbers`, and `erasure` for the contact
//! book; docs/design/server.md, section 4.2; roadmap M5c3): its settings
//! and payload encryption, business username, Official Business Account,
//! compliance information, health status, conversational components and
//! contact book. Each route's exact request to Meta, the answer built from
//! Meta's documented examples, every bound checked before any request, the
//! legal acts off until the deployment enables them, and the design's M5.1
//! and M5.2 (another tenant's number is `404`, nothing asked of Meta, the
//! vault never read: `tests/authz.rs` iterates these routes too).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
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
/// `business-scoped-user-ids`' example BSUID.
const BSUID: &str = "US.13491208655302741918";

/// The harness, with the legal acts on or off, and a key for `numbers`
/// (the second) and one for `numbers` and `erasure` (the first).
async fn connected(legal_acts: bool) -> (Harness, String, String) {
    let h = Harness::with_settings(Settings {
        legal_acts,
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], OTHER_TOKEN)
        .await;
    let both = h
        .tenant_key(TENANT, &[Scope::Numbers, Scope::Erasure])
        .await;
    let numbers = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    (h, both, numbers)
}

fn put(path: &str, key: &str, body: &Value) -> Call {
    Call::new(Method::PUT, format!("/v1/numbers/{PN}/{path}"))
        .key(key)
        .json(body)
}

fn get(path: &str, key: &str) -> Call {
    Call::get(format!("/v1/numbers/{PN}/{path}")).key(key)
}

/// The one request Meta received: its method, path and JSON body, with
/// the tenant's token.
fn the_request(h: &Harness) -> (Method, String, Option<Value>) {
    let requests = h.graph.requests();
    let [asked] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(asked.bearer(), Some(TOKEN));
    (asked.method.clone(), asked.path().to_owned(), asked.json())
}

fn graph_error(code: i64, details: &str) -> Value {
    json!({"error": {
        "message": "(#100) Invalid parameter", "type": "OAuthException", "code": code,
        "error_data": {"messaging_product": "whatsapp", "details": details},
        "fbtrace_id": "A1b2C3"
    }})
}

// ─── Settings ────────────────────────────────────────────────────────────

/// `settings-api` and `local-storage`: the read answers local storage and
/// payload encryption's status and fingerprint, **never a key**: Meta's
/// `cloud_encryption_key` is in no part of the answer.
#[tokio::test]
async fn the_settings_are_read_without_key_material() {
    let (h, _, key) = connected(false).await;
    h.graph.push_json(
        200,
        json!({
            "storage_configuration": {
                "status": "IN_COUNTRY_STORAGE_ENABLED", "data_localization_region": "BR"
            },
            "payload_encryption": {
                "status": "enabled",
                "client_encryption_key_fingerprint": "FP-0123",
                "cloud_encryption_key": "CLOUD-KEY-SECRET-7XK"
            }
        }),
    );
    let reply = h.call(get("settings", &key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({
            "local_storage": {
                "status": "IN_COUNTRY_STORAGE_ENABLED", "data_localization_region": "BR"
            },
            "payload_encryption": {"status": "enabled", "client_encryption_key_fingerprint": "FP-0123"}
        })
    );
    assert!(!reply.text.contains("CLOUD-KEY-SECRET"), "{}", reply.text);
    let (method, path, body) = the_request(&h);
    assert_eq!(
        (method, path.as_str(), body),
        (Method::GET, "/v25.0/106540352242922/settings", None)
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// Each setting is one Graph call with the library's documented body, the
/// tenant's token, and the answer names the setting. Decisive: each arm of
/// `NumberSettings::change`.
#[tokio::test]
async fn each_setting_is_one_graph_call() {
    let (h, _, key) = connected(false).await;
    for (body, path, sent, name) in [
        (
            json!({"identity_key_check": true}),
            "/v25.0/106540352242922/settings",
            json!({"user_identity_change": {"enable_identity_key_check": true}}),
            "identity_key_check",
        ),
        (
            json!({"local_storage": {"enabled": true, "data_localization_region": "DE"}}),
            "/v25.0/106540352242922/settings",
            json!({"storage_configuration": {
                "status": "IN_COUNTRY_STORAGE_ENABLED", "data_localization_region": "DE"
            }}),
            "local_storage",
        ),
        (
            json!({"local_storage": {"enabled": false}}),
            "/v25.0/106540352242922/settings",
            json!({"storage_configuration": {"status": "IN_COUNTRY_STORAGE_DISABLED"}}),
            "local_storage",
        ),
        (
            json!({"search_visibility": "HIDDEN"}),
            "/v25.0/106540352242922",
            json!({"search_visibility": "HIDDEN"}),
            "search_visibility",
        ),
        (
            json!({"security_notifications": true}),
            "/v25.0/106540352242922",
            json!({"whatsapp_business_api_data": {"show_security_notifications": true}}),
            "security_notifications",
        ),
        (
            json!({"notify_user_change_number": false}),
            "/v25.0/106540352242922",
            json!({"whatsapp_business_api_data": {"notify_user_change_number": false}}),
            "notify_user_change_number",
        ),
    ] {
        h.graph.push_json(200, json!({"success": true}));
        let reply = h
            .call(
                Call::new(Method::PATCH, format!("/v1/numbers/{PN}/settings"))
                    .key(&key)
                    .json(&body),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{body}: {}", reply.text);
        assert_eq!(reply.json(), json!({"setting": name}), "{body}");
        let last = h.graph.last_request().unwrap();
        assert_eq!(
            (last.method.clone(), last.path(), last.json(), last.bearer()),
            (Method::POST, path, Some(sent), Some(TOKEN)),
            "{body}"
        );
    }
    assert_eq!(h.graph.requests().len(), 6);
    assert_eq!(h.graph.remaining(), 0);
}

/// A change names exactly one setting, valid, or nothing is asked of Meta.
#[tokio::test]
async fn a_settings_change_is_bounded_before_any_request() {
    let (h, _, key) = connected(false).await;
    for (body, field) in [
        (json!({}), "body"),
        (
            json!({"identity_key_check": true, "security_notifications": true}),
            "body",
        ),
        (json!({"identity_key_check": "yes"}), "body"),
        (json!({"calling": {"status": "ENABLED"}}), "body"),
        (json!({"search_visibility": "hidden"}), "search_visibility"),
        (json!({"local_storage": {"enabled": true}}), "local_storage"),
        (
            json!({"local_storage": {"enabled": true, "data_localization_region": "de"}}),
            "local_storage",
        ),
        (
            json!({"local_storage": {"enabled": true, "data_localization_region": "DEU"}}),
            "local_storage",
        ),
        (
            json!({"local_storage": {"enabled": false, "data_localization_region": "DE"}}),
            "local_storage",
        ),
    ] {
        let reply = h
            .call(
                Call::new(Method::PATCH, format!("/v1/numbers/{PN}/settings"))
                    .key(&key)
                    .json(&body),
            )
            .await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{body}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
    }
    assert!(h.graph.requests().is_empty());
}

/// Payload encryption on and off (`settings-api`): the key goes into the
/// body only, the answer is `204`, a private key or a non-key is `422`
/// without being quoted, Meta's texts are withheld and a timeout is not
/// replayed. Decisive: the exact bodies; `failed_withholding_texts`; the
/// library's `post_once`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn payload_encryption_keeps_the_key_out_of_everything_but_the_body() {
    let (h, _, key) = connected(false).await;
    let url = "payload-encryption";
    let path = format!("settings/{url}");
    h.graph.push_json(200, json!({"success": true}));
    let on = h
        .call(put(
            &path,
            &key,
            &json!({"client_encryption_key": common::SAMPLE_CLIENT_KEY}),
        ))
        .await;
    assert_eq!(on.status, StatusCode::NO_CONTENT, "{}", on.text);
    assert!(on.text.is_empty());
    let (method, graph_path, body) = the_request(&h);
    assert_eq!(
        (method, graph_path.as_str(), body),
        (
            Method::POST,
            "/v25.0/106540352242922/settings",
            Some(json!({"payload_encryption": {
                "status": "enabled", "client_encryption_key": common::SAMPLE_CLIENT_KEY
            }}))
        )
    );

    h.graph.push_json(200, json!({"success": true}));
    let off = h
        .call(Call::new(Method::DELETE, format!("/v1/numbers/{PN}/{path}")).key(&key))
        .await;
    assert_eq!(off.status, StatusCode::NO_CONTENT, "{}", off.text);
    assert_eq!(
        h.graph.last_request().unwrap().json(),
        Some(json!({"payload_encryption": {"status": "disabled"}}))
    );
    let sent = h.graph.requests().len();

    // A PEM private key, not base64, an empty value and an extra field:
    // `422`, nothing sent, the value never quoted.
    let private = base64_of(
        "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----",
    );
    for (body, field) in [
        (
            json!({"client_encryption_key": private}),
            "client_encryption_key",
        ),
        (
            json!({"client_encryption_key": "not base64 !!"}),
            "client_encryption_key",
        ),
        (
            json!({"client_encryption_key": ""}),
            "client_encryption_key",
        ),
        (json!({"client_encryption_key": 7}), "body"),
        (
            json!({"client_encryption_key": common::SAMPLE_CLIENT_KEY, "status": "enabled"}),
            "body",
        ),
    ] {
        let reply = h.call(put(&path, &key, &body)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{body}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
        assert!(!reply.text.contains(&private), "{}", reply.text);
        assert!(!reply.text.contains("not base64"), "{}", reply.text);
    }
    assert_eq!(h.graph.requests().len(), sent);

    // Meta's refusal, whose details quote the key: withheld.
    h.graph.push_json(
        400,
        graph_error(100, &format!("bad key {}", common::SAMPLE_CLIENT_KEY)),
    );
    let refused = h
        .call(put(
            &path,
            &key,
            &json!({"client_encryption_key": common::SAMPLE_CLIENT_KEY}),
        ))
        .await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.text
    );
    assert!(
        !refused.text.contains(common::SAMPLE_CLIENT_KEY),
        "{}",
        refused.text
    );
    assert!(refused.json()["error"]["graph"]["details"].is_null());

    // A cut call is not replayed: one request, `504`.
    let before = h.graph.requests().len();
    h.graph.push_error(|| TransportError::Timeout);
    let cut = h
        .call(put(
            &path,
            &key,
            &json!({"client_encryption_key": common::SAMPLE_CLIENT_KEY}),
        ))
        .await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    assert_eq!(h.graph.requests().len(), before + 1);
    assert_eq!(h.graph.remaining(), 0);
}

fn base64_of(text: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(text)
}

// ─── Username ────────────────────────────────────────────────────────────

/// `business-scoped-user-ids`, § Business usernames: adopt (with
/// `transfer_action`), read, suggestions flattened, delete.
#[tokio::test]
async fn a_username_is_adopted_read_suggested_and_deleted() {
    let (h, _, key) = connected(false).await;
    h.graph.push_json(200, json!({"status": "approved"}));
    let set = h
        .call(put(
            "username",
            &key,
            &json!({"username": "lucky_shrub", "force_transfer": true}),
        ))
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text);
    assert_eq!(
        set.json(),
        json!({"username": "lucky_shrub", "status": "approved"})
    );
    let (method, path, body) = the_request(&h);
    assert_eq!(
        (method, path.as_str(), body),
        (
            Method::POST,
            "/v25.0/106540352242922/username",
            Some(json!({"username": "lucky_shrub", "transfer_action": "force_transfer"}))
        )
    );

    h.graph.push_json(200, json!({"status": "reserved"}));
    let plain = h
        .call(put("username", &key, &json!({"username": "lucky.shrub"})))
        .await;
    assert_eq!(plain.json()["status"], "reserved");
    assert_eq!(
        h.graph.last_request().unwrap().json(),
        Some(json!({"username": "lucky.shrub"}))
    );

    h.graph.push_json(
        200,
        json!({"username": "lucky_shrub", "status": "approved"}),
    );
    let read = h.call(get("username", &key)).await;
    assert_eq!(
        read.json(),
        json!({"username": "lucky_shrub", "status": "approved"})
    );
    h.graph.push_json(200, json!({}));
    let none = h.call(get("username", &key)).await;
    assert_eq!(none.json(), json!({"username": null, "status": null}));

    h.graph.push_json(
        200,
        json!({"data": [{"username_suggestions": ["lucky_shrub", "luckyshrub_store"]}]}),
    );
    let suggestions = h.call(get("username/suggestions", &key)).await;
    assert_eq!(
        suggestions.json(),
        json!({"data": ["lucky_shrub", "luckyshrub_store"]})
    );
    assert_eq!(
        h.graph.last_request().unwrap().path(),
        "/v25.0/106540352242922/username_suggestions"
    );

    h.graph.push_json(200, json!({"success": true}));
    let deleted = h
        .call(Call::new(Method::DELETE, format!("/v1/numbers/{PN}/username")).key(&key))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.text);
    let last = h.graph.last_request().unwrap();
    assert_eq!(
        (last.method.clone(), last.path()),
        (Method::DELETE, "/v25.0/106540352242922/username")
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// Meta's documented format, checked before any request.
#[tokio::test]
async fn a_username_is_checked_before_any_request() {
    let (h, _, key) = connected(false).await;
    for bad in [
        "ab",
        &"a".repeat(36),
        "lucky shrub",
        "1234567",
        ".lucky",
        "lucky.",
        "lu..cky",
        "www.lucky",
        "lücky",
    ] {
        let reply = h
            .call(put("username", &key, &json!({"username": bad})))
            .await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{bad}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], "username", "{bad}");
    }
    let reply = h
        .call(put(
            "username",
            &key,
            &json!({"username": "lucky_shrub", "transfer": true}),
        ))
        .await;
    assert_eq!(reply.json()["error"]["field"], "body");
    assert!(h.graph.requests().is_empty());
}

// ─── Official Business Account ───────────────────────────────────────────

/// `official-business-accounts`, "Getting OBA status via API": the status.
#[tokio::test]
async fn the_official_business_account_status_is_read() {
    let (h, _, key) = connected(false).await;
    h.graph.push_json(
        200,
        json!({"official_business_account": {"oba_status": "NOT_STARTED"}, "id": PN}),
    );
    let reply = h.call(get("official-business-account", &key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"oba_status": "NOT_STARTED", "status_message": null})
    );
    let asked = h.graph.last_request().unwrap();
    assert_eq!(asked.path(), "/v25.0/106540352242922");
    assert_eq!(
        asked.query("fields").as_deref(),
        Some("official_business_account")
    );
}

/// Roadmap M5c3 (design D192): the application is not a legal act. With
/// the deployment's legal acts off it still reaches Meta: the library's
/// one `POST`, `202`, once per `Idempotency-Key`, and never replayed after
/// a timeout. Decisive: `is_legal_act` (the application is not in it) and
/// the idempotency class of `RequestOfficialBusinessAccount`.
#[tokio::test]
async fn the_application_is_not_a_legal_act_and_reaches_meta_with_them_off() {
    let post_as = |key: &str, reference: &str| {
        Call::new(
            Method::POST,
            format!("/v1/numbers/{PN}/official-business-account"),
        )
        .key(key)
        .header("idempotency-key", reference)
        .json(&common::sample_oba_application())
    };
    let (h, _, key) = connected(false).await;
    h.graph.push_json(
        200,
        json!({
            "success": true, "message": "Application submitted",
            "updated_status": {"oba_status": "PENDING", "status_message": "Submitted"},
            "tracking_id": "T-1"
        }),
    );
    let reply = h.call(post_as(&key, "oba-1")).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({
            "message": "Application submitted",
            "updated_status": {"oba_status": "PENDING", "status_message": "Submitted"},
            "tracking_id": "T-1"
        })
    );
    let (method, path, body) = the_request(&h);
    assert_eq!(
        (method, path.as_str(), body),
        (
            Method::POST,
            "/v25.0/106540352242922/official_business_account",
            Some(common::sample_oba_application())
        )
    );
    let again = h.call(post_as(&key, "oba-1")).await;
    assert_eq!(again.status, StatusCode::ACCEPTED);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(h.graph.requests().len(), 1, "no second application");

    // A cut call is not replayed, and the key keeps the outcome unknown.
    h.graph.push_error(|| TransportError::Timeout);
    let cut = h.call(post_as(&key, "oba-2")).await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    assert_eq!(h.graph.requests().len(), 2);
    assert_eq!(h.graph.remaining(), 0);
}

/// The application's bounds, before any request (Meta's: the two required
/// fields and 5 to 10 links; the service's: links `http` or `https`).
#[tokio::test]
async fn an_application_is_bounded_before_any_request() {
    let (h, _, key) = connected(false).await;
    let with = |field: &str, value: Value| {
        let mut body = common::sample_oba_application();
        body[field] = value;
        body
    };
    let links = |n: usize| {
        (0..n)
            .map(|i| format!("https://news.example/{i}"))
            .collect::<Vec<_>>()
    };
    for (body, field) in [
        (
            with("business_website_url", json!("javascript:alert(1)")),
            "business_website_url",
        ),
        (
            with(
                "business_website_url",
                json!("https://good.example@evil.example/"),
            ),
            "business_website_url",
        ),
        (
            with("primary_country_of_operation", json!("")),
            "primary_country_of_operation",
        ),
        (
            with("supporting_links", json!(links(4))),
            "supporting_links",
        ),
        (
            with("supporting_links", json!(links(11))),
            "supporting_links",
        ),
        (
            with(
                "supporting_links",
                json!([
                    "ftp://x.example",
                    "https://a.example",
                    "https://b.example",
                    "https://c.example",
                    "https://d.example"
                ]),
            ),
            "supporting_links",
        ),
        (
            with("additional_supporting_information", json!("x".repeat(1025))),
            "additional_supporting_information",
        ),
        (
            with("additional_supporting_information", json!("a\u{202E}b")),
            "additional_supporting_information",
        ),
        (with("tracking_id", json!("x")), "body"),
        (
            json!({"business_website_url": "https://luckyshrub.example"}),
            "body",
        ),
    ] {
        let reply = h
            .call(
                Call::new(
                    Method::POST,
                    format!("/v1/numbers/{PN}/official-business-account"),
                )
                .key(&key)
                .json(&body),
            )
            .await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{body}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
    }
    assert!(h.graph.requests().is_empty());
}

// ─── Compliance information ──────────────────────────────────────────────

/// `business-compliance-information-api`: the read asks for the page's
/// fields; the declaration is a legal act, off until enabled, then the
/// library's `POST` with `messaging_product`.
#[tokio::test]
async fn compliance_information_is_read_and_declared_when_the_legal_acts_are_on() {
    let (off, _, key) = connected(false).await;
    let refused = off
        .call(put(
            "compliance-info",
            &key,
            &common::sample_compliance_info(),
        ))
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::FORBIDDEN, "feature_not_available"),
        "{}",
        refused.text
    );
    assert!(off.graph.requests().is_empty());
    // A read is not a legal act.
    off.graph.push_json(
        200,
        json!({"data": [{
            "whatsapp_business_account_id": WABA, "messaging_product": "whatsapp",
            "entity_name": "Lucky Shrub Pvt Ltd", "entity_type": "Private Limited Company",
            "is_registered": true,
            "grievance_officer_details": {"name": "A. Rao", "email": "grievance@luckyshrub.example"},
            "customer_care_details": {"email": "care@luckyshrub.example", "mobile_number": "+919876543210"}
        }]}),
    );
    let read = off.call(get("compliance-info", &key)).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.text);
    assert_eq!(
        read.json(),
        json!({
            "entity_name": "Lucky Shrub Pvt Ltd", "entity_type": "Private Limited Company",
            "entity_type_custom": null, "is_registered": true,
            "grievance_officer_details": {
                "name": "A. Rao", "email": "grievance@luckyshrub.example",
                "mobile_number": null, "landline_number": null
            },
            "customer_care_details": {
                "email": "care@luckyshrub.example", "mobile_number": "+919876543210",
                "landline_number": null
            }
        })
    );
    assert_eq!(
        off.graph.last_request().unwrap().query("fields").as_deref(),
        Some(
            "entity_name,entity_type,entity_type_custom,is_registered,grievance_officer_details,customer_care_details"
        )
    );
    off.graph.push_json(200, json!({"data": []}));
    let empty = off.call(get("compliance-info", &key)).await;
    assert_eq!(empty.json()["entity_name"], Value::Null);

    let (h, _, key) = connected(true).await;
    h.graph.push_json(200, json!({"success": true}));
    let declared = h
        .call(put(
            "compliance-info",
            &key,
            &common::sample_compliance_info(),
        ))
        .await;
    assert_eq!(declared.status, StatusCode::NO_CONTENT, "{}", declared.text);
    let (method, path, body) = the_request(&h);
    assert_eq!(
        (method, path.as_str(), body),
        (
            Method::POST,
            "/v25.0/106540352242922/business_compliance_info",
            Some(json!({
                "messaging_product": "whatsapp",
                "entity_name": "Lucky Shrub Pvt Ltd",
                "entity_type": "PRIVATE_COMPANY",
                "grievance_officer_details": {
                    "name": "A. Rao", "email": "grievance@luckyshrub.example",
                    "mobile_number": "+919876543210"
                },
                "customer_care_details": {"email": "care@luckyshrub.example"}
            }))
        )
    );
}

/// The declaration's bounds (the page's rules, and the service's), before
/// any request.
#[tokio::test]
async fn a_declaration_is_bounded_before_any_request() {
    let (h, _, key) = connected(true).await;
    let with = |path: &[&str], value: Value| {
        let mut body = common::sample_compliance_info();
        let mut at = &mut body;
        for part in &path[..path.len() - 1] {
            at = &mut at[*part];
        }
        at[path[path.len() - 1]] = value;
        body
    };
    for (body, field) in [
        (with(&["entity_name"], json!("A")), "entity_name"),
        (
            with(&["entity_name"], json!("x".repeat(129))),
            "entity_name",
        ),
        (
            with(&["entity_name"], json!("Lucky\u{200B}Shrub")),
            "entity_name",
        ),
        (with(&["entity_type"], json!("COOPERATIVE")), "entity_type"),
        (with(&["entity_type"], json!("OTHER")), "entity_type_custom"),
        (
            with(&["entity_type_custom"], json!("a co-op")),
            "entity_type_custom",
        ),
        (with(&["is_registered"], json!(true)), "is_registered"),
        (
            with(&["grievance_officer_details", "email"], json!("nope")),
            "grievance_officer_details.email",
        ),
        (
            with(&["grievance_officer_details", "name"], json!("")),
            "grievance_officer_details.name",
        ),
        (
            with(
                &["customer_care_details", "email"],
                json!(format!("{}@x.example", "a".repeat(128))),
            ),
            "customer_care_details.email",
        ),
        (
            with(
                &["customer_care_details", "mobile_number"],
                json!("call me"),
            ),
            "customer_care_details.mobile_number",
        ),
        (with(&["customer_care_details", "pin"], json!("1")), "body"),
    ] {
        let reply = h.call(put("compliance-info", &key, &body)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{body}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
    }
    assert!(h.graph.requests().is_empty());
}

// ─── Health status ───────────────────────────────────────────────────────

/// `support/health-status`, "Example limited response".
#[tokio::test]
async fn the_health_status_is_read() {
    let (h, _, key) = connected(false).await;
    h.graph.push_json(
        200,
        json!({
          "health_status": {
            "can_send_message": "LIMITED",
            "entities": [
              {"entity_type": "PHONE_NUMBER", "id": PN, "can_send_message": "LIMITED",
               "can_receive_call_sip": "AVAILABLE",
               "additional_info": ["Your display name has not been approved yet."]},
              {"entity_type": "BUSINESS", "id": "506914307656634", "can_send_message": "BLOCKED",
               "errors": [{"error_code": 141002, "error_description": "Template not approved",
                           "possible_solution": "Wait for approval"}]}
            ]
          },
          "id": PN
        }),
    );
    let reply = h.call(get("health-status", &key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({
            "can_send_message": "LIMITED",
            "entities": [
                {"entity_type": "PHONE_NUMBER", "id": PN, "can_send_message": "LIMITED",
                 "can_receive_call_sip": "AVAILABLE",
                 "additional_info": ["Your display name has not been approved yet."],
                 "errors": []},
                {"entity_type": "BUSINESS", "id": "506914307656634", "can_send_message": "BLOCKED",
                 "can_receive_call_sip": null, "additional_info": [],
                 "errors": [{"error_code": 141002, "error_description": "Template not approved",
                             "possible_solution": "Wait for approval"}]}
            ]
        })
    );
    let asked = h.graph.last_request().unwrap();
    assert_eq!(asked.path(), "/v25.0/106540352242922");
    assert_eq!(asked.query("fields").as_deref(), Some("health_status"));
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Conversational components ───────────────────────────────────────────

/// `conversational-components`: read, set (the library's body), and the
/// documented limits before any request.
#[tokio::test]
async fn conversational_components_are_read_set_and_bounded() {
    let (h, _, key) = connected(false).await;
    h.graph.push_json(
        200,
        json!({"conversational_automation": {
            "enable_welcome_message": true, "prompts": ["Book a table"],
            "commands": [{"command_name": "book", "command_description": "Book a table"}]
        }, "id": PN}),
    );
    let read = h.call(get("conversational-components", &key)).await;
    assert_eq!(
        read.json(),
        json!({
            "enable_welcome_message": true, "prompts": ["Book a table"],
            "commands": [{"command_name": "book", "command_description": "Book a table"}]
        })
    );
    assert_eq!(
        h.graph.last_request().unwrap().query("fields").as_deref(),
        Some("conversational_automation")
    );
    h.graph.push_json(200, json!({"id": PN}));
    let none = h.call(get("conversational-components", &key)).await;
    assert_eq!(
        none.json(),
        json!({"enable_welcome_message": null, "prompts": [], "commands": []})
    );

    h.graph.push_json(200, json!({"success": true}));
    let body = json!({
        "enable_welcome_message": true, "prompts": ["Book a table"],
        "commands": [{"command_name": "book", "command_description": "Book a table"}]
    });
    let set = h.call(put("conversational-components", &key, &body)).await;
    assert_eq!(set.status, StatusCode::NO_CONTENT, "{}", set.text);
    let last = h.graph.last_request().unwrap();
    assert_eq!(
        (last.method.clone(), last.path(), last.json()),
        (
            Method::POST,
            "/v25.0/106540352242922/conversational_automation",
            Some(body)
        )
    );
    let calls = h.graph.requests().len();

    for (body, field) in [
        (json!({"prompts": ["a", "b", "c", "d", "e"]}), "prompts"),
        (json!({"prompts": ["x".repeat(81)]}), "prompts[0]"),
        (json!({"prompts": [""]}), "prompts[0]"),
        (json!({"prompts": ["a\u{200B}b"]}), "prompts[0]"),
        (
            json!({"commands": [{"command_name": "/book", "command_description": "d"}]}),
            "commands[0].command_name",
        ),
        (
            json!({"commands": [{"command_name": "x".repeat(33), "command_description": "d"}]}),
            "commands[0].command_name",
        ),
        (
            json!({"commands": [{"command_name": "a", "command_description": "d\u{202E}"}]}),
            "commands[0].command_description",
        ),
        (
            json!({"commands": [{"command_name": "a", "command_description": "d"}, {"command_name": "a", "command_description": "e"}]}),
            "commands",
        ),
        (json!({"welcome": true}), "body"),
    ] {
        let reply = h.call(put("conversational-components", &key, &body)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{body}: {}",
            reply.text
        );
        assert!(
            reply.json()["error"]["field"]
                .as_str()
                .unwrap()
                .starts_with(field),
            "{body}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), calls);
}

// ─── Contact book ────────────────────────────────────────────────────────

/// `business-scoped-user-ids`, § Contact book: an irreversible erasure at
/// Meta. Scope `erasure`, not gated by the legal acts setting (a
/// data-subject erasure the tenant performs explicitly, design D192), a
/// BSUID only, and the request is the library's `DELETE` with the BSUID as
/// a query parameter. Decisive: the scope of `delete_contact_book_entry`,
/// the BSUID check, `is_legal_act` (the erasure is not in it).
#[tokio::test]
async fn a_contact_book_entry_is_erased_only_by_a_key_with_the_scope() {
    let delete = |key: &str, id: &str| {
        Call::new(
            Method::DELETE,
            format!("/v1/numbers/{PN}/contact-book/{id}"),
        )
        .key(key)
    };
    // The legal acts are off: the erasure is not one of them.
    let (h, both, numbers) = connected(false).await;
    // A key without the scope `erasure`: forbidden.
    let no_scope = h.call(delete(&numbers, BSUID)).await;
    assert_eq!(
        (no_scope.status, no_scope.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden"),
        "{}",
        no_scope.text
    );
    assert!(h.graph.requests().is_empty());

    // Not a BSUID: a phone number, a parent BSUID, a path-ish value.
    for bad in [
        "16505551234",
        "US.ENT.11815799212886844830",
        "us.1",
        "US.",
        "US.12%26bsuid=x",
    ] {
        let reply = h.call(delete(&both, bad)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{bad}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], "bsuid", "{bad}");
        assert!(!reply.text.contains(bad), "{}", reply.text);
    }
    assert!(h.graph.requests().is_empty());

    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "success": true, "deleted": true}),
    );
    let erased = h.call(delete(&both, BSUID)).await;
    assert_eq!(erased.status, StatusCode::OK, "{}", erased.text);
    assert_eq!(erased.json(), json!({"deleted": true}));
    let asked = h.graph.last_request().unwrap();
    assert_eq!(
        (asked.method.clone(), asked.path(), asked.bearer()),
        (
            Method::DELETE,
            "/v25.0/106540352242922/contact_book",
            Some(TOKEN)
        )
    );
    assert_eq!(
        asked.query("messaging_product").as_deref(),
        Some("whatsapp")
    );
    assert_eq!(asked.query("bsuid").as_deref(), Some(BSUID));
    assert_eq!(asked.url.query_pairs().count(), 2);
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "success": true, "deleted": false}),
    );
    let none = h.call(delete(&both, BSUID)).await;
    assert_eq!(none.json(), json!({"deleted": false}));
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Meta's texts, and the other tenant ──────────────────────────────────

/// A sentinel in Meta's error on each legal act (which the document-wide
/// sentinel test cannot reach, as they are off there): its text is in no
/// response; the details a route keeps are Meta's own.
#[tokio::test]
async fn metas_texts_on_a_legal_act_stay_out_of_the_answer() {
    let (h, both, _) = connected(true).await;
    let calls = [
        Call::new(
            Method::POST,
            format!("/v1/numbers/{PN}/official-business-account"),
        )
        .key(&both)
        .json(&common::sample_oba_application()),
        put("compliance-info", &both, &common::sample_compliance_info()),
        Call::new(
            Method::DELETE,
            format!("/v1/numbers/{PN}/contact-book/{BSUID}"),
        )
        .key(&both),
    ];
    for call in calls {
        h.graph.push_bytes(
            502,
            "application/json",
            b"<html>SENTINEL-7f3a91 bad gateway</html>".to_vec(),
        );
        let reply = h.call(call).await;
        assert!(!reply.text.contains("SENTINEL-7f3a91"), "{}", reply.text);
        assert!(reply.status.is_server_error(), "{}", reply.text);
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// The design's M5.1 and M5.2 for the new routes: another tenant's number
/// is `404`, nothing asked of Meta, even with the legal acts on and the
/// scope held (`tests/authz.rs` iterates the document for the rest).
#[tokio::test]
async fn another_tenants_number_is_not_found_on_every_settings_route() {
    let (h, both, _) = connected(true).await;
    for (method, path, body) in [
        (Method::GET, "settings", None),
        (
            Method::PATCH,
            "settings",
            Some(json!({"search_visibility": "HIDDEN"})),
        ),
        (
            Method::PUT,
            "settings/payload-encryption",
            Some(json!({"client_encryption_key": common::SAMPLE_CLIENT_KEY})),
        ),
        (Method::DELETE, "settings/payload-encryption", None),
        (Method::GET, "username", None),
        (
            Method::PUT,
            "username",
            Some(json!({"username": "lucky_shrub"})),
        ),
        (Method::DELETE, "username", None),
        (Method::GET, "username/suggestions", None),
        (Method::GET, "official-business-account", None),
        (
            Method::POST,
            "official-business-account",
            Some(common::sample_oba_application()),
        ),
        (Method::GET, "compliance-info", None),
        (
            Method::PUT,
            "compliance-info",
            Some(common::sample_compliance_info()),
        ),
        (Method::GET, "health-status", None),
        (Method::GET, "conversational-components", None),
        (
            Method::PUT,
            "conversational-components",
            Some(json!({"prompts": ["Hi"]})),
        ),
        (Method::DELETE, "contact-book/US.13491208655302741918", None),
    ] {
        let mut call =
            Call::new(method.clone(), format!("/v1/numbers/{OTHER_PN}/{path}")).key(&both);
        if let Some(body) = &body {
            call = call.json(body);
        }
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{method} {path}: {}",
            reply.text
        );
    }
    assert!(h.graph.requests().is_empty());
}
