//! Roadmap M5e: a number's calling through the service (scope `calls`;
//! docs/design/server.md, section 4.2): the calling settings, a user's call
//! permission, the call a business places, the actions on a user's call and
//! the call analytics.
//!
//! Each test sends Meta's documented request and answer (`calling/*`,
//! `reference/whatsapp-business-phone-number/calling-api`, `analytics`):
//! the exact requests (method, path, query, the tenant's token, the JSON
//! body), `remaining() == 0`. The design's M5.2: **a call id the number
//! does not hold is `404` with zero vault reads**, decided by the service's
//! records of the number's calls before the token is opened (D210). M5.1
//! (another tenant's number, zero vault reads) is `tests/authz.rs`, which
//! iterates every route of the document, these included; the logs, the
//! audit events and the secrets (SDP, SIP password) are `tests/logs.rs`'s
//! capture.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::meta::{call_connected, call_status};
use common::{Call, Harness, SAMPLE_SDP};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const OTHER_TENANT: &str = "merchant-43";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const PN_2: &str = "106540352242923";
const OTHER_WABA: &str = "102290129340399";
const OTHER_PN: &str = "106540352242924";
const TOKEN: &str = "EAAG-merchant-token";
const OTHER_TOKEN: &str = "EAAG-other-token";

/// `calling/user-initiated-calls`, the connect webhook's call.
const CALL: &str = "wacid.ABGGFjFVU2AfAgo6V-Hc5eCgK5Gh";
const OTHER_CALL: &str = "wacid.HBgLMTIxODU1NTI4MjgVAgARGCAyODRQIAFRoA";
const THIRD_CALL: &str = "wacid.ABGGFjFVU2AfAgo6V";

/// Two tenants with a WABA each (the first with two numbers), the first
/// tenant's key with the scopes `calls` and `analytics`.
async fn connected() -> (Harness, String) {
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN, PN_2], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], OTHER_TOKEN)
        .await;
    let key = h
        .tenant_key(TENANT, &[Scope::Calls, Scope::Analytics])
        .await;
    (h, key)
}

fn on(method: Method, path: &str, key: &str) -> Call {
    Call::new(method, path).key(key)
}

fn graph_error(code: i64, details: &str) -> Value {
    json!({"error": {"message": details, "type": "OAuthException", "code": code,
                     "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn",
                     "error_data": {"messaging_product": "whatsapp", "details": details}}})
}

/// Deliver Meta's connect event, which makes `call` the number's.
async fn hold(h: &Harness, pn: &str, call: &str) {
    let waba = if pn == OTHER_PN { OTHER_WABA } else { WABA };
    let reply = h.webhook(&call_connected(waba, pn, call)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
}

fn success() -> Value {
    json!({"messaging_product": "whatsapp", "success": true})
}

// ─── The settings ────────────────────────────────────────────────────────

/// calling/call-settings, "Get calling settings": the request is Meta's
/// (`GET /{pn}/settings`, **no `include_sip_credentials`**), the answer the
/// `calling` object without the SIP password (Meta sent one) and with the
/// restrictions; the voicemail announcement's media id, a number on the
/// wire, is a string. A change is Meta's `calling` object, exactly what was
/// set, in a `POST /{pn}/settings`, answered `204`. Decisive: `settings(false)`
/// in `Calls::settings`, and the password dropped in the core and absent
/// from the view.
#[tokio::test]
#[allow(clippy::too_many_lines)] // Meta's example, then a change
async fn the_settings_are_read_without_the_sip_password_and_changed_as_metas_object() {
    let (h, key) = connected().await;
    h.graph.push_json(
        200,
        json!({
            "calling": {
                "status": "ENABLED",
                "call_icon_visibility": "DEFAULT",
                "callback_permission_status": "ENABLED",
                "call_hours": {
                    "status": "ENABLED",
                    "timezone_id": "America/Manaus",
                    "weekly_operating_hours": [
                        {"day_of_week": "MONDAY", "open_time": "0400", "close_time": "1020"},
                        {"day_of_week": "TUESDAY", "open_time": "0108", "close_time": "1020"}],
                    "holiday_schedule": [
                        {"date": "2026-01-01", "start_time": "0000", "end_time": "2359"}]
                },
                "sip": {"status": "ENABLED", "servers": [
                    {"hostname": "sip.example.com", "port": 5061,
                     "sip_user_password": "SIP-PASSWORD-SENTINEL-9Q"}]},
                "audio": {"additional_codecs": ["PCMA", "PCMU"]},
                "voicemail": {"status": "ENABLED", "triggers": ["REJECT", "TIMEOUT"],
                    "audio": {"default": {"announcement_media_id": 1_037_543_291_543_636_u64,
                                          "timeout_seconds": 20}}},
                "restrictions": {"restrictions_list": [
                    {"type": "RESTRICTED_BUSINESS_INITIATED_CALLING",
                     "reason": "temporarily disabled", "expiration": 1_754_072_386}]}
            },
            "other_settings": {"ignored": true}
        }),
    );
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}/call-settings")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert!(
        !reply.text.contains("SIP-PASSWORD-SENTINEL"),
        "{}",
        reply.text
    );
    assert!(!reply.text.contains("password"), "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({
            "status": "ENABLED",
            "call_icon_visibility": "DEFAULT",
            "callback_permission_status": "ENABLED",
            "call_hours": {
                "status": "ENABLED",
                "timezone_id": "America/Manaus",
                "weekly_operating_hours": [
                    {"day_of_week": "MONDAY", "open_time": "0400", "close_time": "1020"},
                    {"day_of_week": "TUESDAY", "open_time": "0108", "close_time": "1020"}],
                "holiday_schedule": [
                    {"date": "2026-01-01", "start_time": "0000", "end_time": "2359"}]
            },
            "sip": {"status": "ENABLED", "servers": [{"hostname": "sip.example.com", "port": 5061}]},
            "audio": {"additional_codecs": ["PCMA", "PCMU"]},
            "voicemail": {"status": "ENABLED", "triggers": ["REJECT", "TIMEOUT"],
                "audio": {"default": {"announcement_media_id": "1037543291543636",
                                      "timeout_seconds": 20}}},
            "restrictions": [{"type": "RESTRICTED_BUSINESS_INITIATED_CALLING",
                              "reason": "temporarily disabled",
                              "expiration": "2025-08-01T18:19:46Z"}]
        })
    );
    let requests = h.graph.requests();
    let [read] = &requests[..] else {
        panic!("one request: {requests:?}")
    };
    assert_eq!(read.method, Method::GET);
    assert_eq!(read.path(), format!("/v25.0/{PN}/settings"));
    assert_eq!(read.url.query(), None, "the SIP credentials are not asked");
    assert_eq!(read.bearer(), Some(TOKEN));

    // A number whose calling was never configured: Meta sends no `calling`.
    h.graph
        .push_json(200, json!({"messaging_product": "whatsapp"}));
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}/call-settings")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json(), json!({"restrictions": []}));

    // A change: the fields set, as Meta's object.
    h.graph.push_json(200, json!({"success": true}));
    let change = json!({
        "status": "ENABLED",
        "call_hours": {
            "status": "ENABLED",
            "timezone_id": "America/Manaus",
            "weekly_operating_hours": [
                {"day_of_week": "MONDAY", "open_time": "0900", "close_time": "1700"}],
            "holiday_schedule": [{"date": "2026-12-25", "start_time": "0000", "end_time": "2359"}]
        },
        "sip": {"status": "ENABLED", "servers": [{"hostname": "sip.example.com",
            "request_uri_user_params": {"tenant": "acme"}}]},
        "audio": {"additional_codecs": ["PCMA"]}
    });
    let reply = h
        .call(
            on(
                Method::PATCH,
                &format!("/v1/numbers/{PN}/call-settings"),
                &key,
            )
            .json(&change),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let sent = h.graph.last_request().unwrap();
    assert_eq!(sent.method, Method::POST);
    assert_eq!(sent.path(), format!("/v25.0/{PN}/settings"));
    assert_eq!(sent.url.query(), None);
    assert_eq!(sent.bearer(), Some(TOKEN));
    assert_eq!(sent.json().unwrap(), json!({"calling": change}));
    assert_eq!(h.graph.remaining(), 0);
}

/// A settings change is refused before any request: nothing to change, a
/// key the object does not have, a value the pages do not list, and what
/// the library checks (the call hours' windows, the SIP server, the
/// voicemail). Decisive: `settings_change` and the library's checks, each
/// field named.
#[tokio::test]
async fn a_settings_change_is_refused_before_any_request() {
    let (h, key) = connected().await;
    let hours = |weekly: Value| {
        json!({"call_hours": {"status": "ENABLED", "timezone_id": "America/Manaus",
                              "weekly_operating_hours": weekly}})
    };
    let window = |day: &str, open: &str, close: &str| json!({"day_of_week": day, "open_time": open, "close_time": close});
    for (body, field) in [
        (json!({}), "update"),
        (json!({"restrictions": {}}), "body"),
        (json!({"status": "ON"}), "status"),
        (
            json!({"call_icon_visibility": "HIDE"}),
            "call_icon_visibility",
        ),
        (
            json!({"audio": {"additional_codecs": ["OPUS"]}}),
            "audio.additional_codecs",
        ),
        (json!({"sip": {"status": "MAYBE"}}), "sip.status"),
        (
            json!({"voicemail": {"status": "ENABLED", "triggers": ["BUSY"]}}),
            "voicemail.triggers",
        ),
        (
            hours(json!([window("FUNDAY", "0900", "1700")])),
            "call_hours.weekly_operating_hours",
        ),
        // The library's checks.
        (
            hours(json!([])),
            "calling.call_hours.weekly_operating_hours",
        ),
        (
            hours(json!([window("MONDAY", "1700", "0900")])),
            "calling.call_hours.weekly_operating_hours[0]",
        ),
        (
            hours(json!([
                window("MONDAY", "0900", "1200"),
                window("MONDAY", "1100", "1300")
            ])),
            "calling.call_hours.weekly_operating_hours[0]",
        ),
        (
            json!({"sip": {"servers": [{"hostname": "a.example"}, {"hostname": "b.example"}]}}),
            "calling.sip.servers",
        ),
        (
            json!({"voicemail": {"status": "ENABLED", "triggers": ["REJECT"]}}),
            "calling.voicemail.audio.default.announcement_media_id",
        ),
    ] {
        let reply = h
            .call(
                on(
                    Method::PATCH,
                    &format!("/v1/numbers/{PN}/call-settings"),
                    &key,
                )
                .json(&body),
            )
            .await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{body}: {}",
            reply.text
        );
    }
    // Not even a body.
    let reply = h
        .call(
            on(
                Method::PATCH,
                &format!("/v1/numbers/{PN}/call-settings"),
                &key,
            )
            .json(&json!([1])),
        )
        .await;
    assert_eq!(reply.code(), "invalid_request", "{}", reply.text);
    assert!(h.graph.requests().is_empty(), "nothing reached Meta");
    assert_eq!(h.graph.remaining(), 0);
}

/// A voicemail announcement is the number's own media (design D211): the
/// change first looks it up with `phone_number_id={pn}` (`GET /{media_id}`),
/// and only that media is set. Meta refusing the id (another number's, none)
/// or answering for another node is `404 not_found`, without Meta's code,
/// and the settings are not changed: the lookup is the only request.
/// Decisive: the lookup in `Calls::update_settings` (delete it and the
/// POST goes out).
#[tokio::test]
async fn a_voicemail_announcement_must_be_the_numbers_own_media() {
    let (h, key) = connected().await;
    let voicemail = |media: &str| {
        json!({"voicemail": {"status": "ENABLED", "triggers": ["REJECT"],
            "audio": {"default": {"announcement_media_id": media}}}})
    };
    let path = format!("/v1/numbers/{PN}/call-settings");
    // The number's: looked up, then set (a number on the wire, as Meta
    // documents it).
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "url": "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1037543291543636",
               "mime_type": "audio/ogg", "sha256": "abc", "file_size": 1000, "id": "1037543291543636"}),
    );
    h.graph.push_json(200, json!({"success": true}));
    let reply = h
        .call(on(Method::PATCH, &path, &key).json(&voicemail("1037543291543636")))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let requests = h.graph.requests();
    let [lookup, change] = &requests[..] else {
        panic!("two requests: {requests:?}")
    };
    assert_eq!(lookup.method, Method::GET);
    assert_eq!(lookup.path(), "/v25.0/1037543291543636");
    assert_eq!(
        lookup.url.query(),
        Some(format!("phone_number_id={PN}").as_str())
    );
    assert_eq!(lookup.bearer(), Some(TOKEN));
    assert_eq!(change.method, Method::POST);
    assert_eq!(change.path(), format!("/v25.0/{PN}/settings"));
    assert_eq!(
        change.json().unwrap(),
        json!({"calling": {"voicemail": {"status": "ENABLED", "triggers": ["REJECT"],
            "audio": {"default": {"announcement_media_id": 1_037_543_291_543_636_u64}}}}})
    );

    // Meta refuses the id for this number: 404, the lookup alone sent.
    let before = h.graph.requests().len();
    h.graph
        .push_json(400, graph_error(100, "Unsupported get request"));
    let reply = h
        .call(on(Method::PATCH, &path, &key).json(&voicemail("999999999999")))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "{}",
        reply.text
    );
    assert!(reply.json()["error"]["graph"].is_null(), "{}", reply.text);
    assert_eq!(h.graph.requests().len(), before + 1, "the lookup only");
    // Meta answers for another node: 404, nothing set.
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "url": "https://lookaside.fbsbx.com/x",
               "mime_type": "audio/ogg", "sha256": "abc", "file_size": 1, "id": "5555"}),
    );
    let reply = h
        .call(on(Method::PATCH, &path, &key).json(&voicemail("999999999999")))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    assert_eq!(h.graph.requests().len(), before + 2, "the lookup only");
    // Not an id: refused with no request at all.
    for bad in ["x", "0123", "12 3", ""] {
        let reply = h
            .call(on(Method::PATCH, &path, &key).json(&voicemail(bad)))
            .await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("voicemail.audio.default.announcement_media_id")
            ),
            "{bad}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), before + 2);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Permissions ─────────────────────────────────────────────────────────

/// calling/user-call-permissions, "Get current call permission state": a
/// phone number is `user_wa_id`, a BSUID `recipient`; the answer is Meta's
/// (a temporary permission, the actions and their limits). A request that
/// names nobody, or a bad number or id, is refused with no request.
/// Decisive: the choice of query key in the library, `callee`'s checks.
#[tokio::test]
async fn a_users_permission_is_read_by_phone_or_bsuid() {
    let (h, key) = connected().await;
    let answer = json!({
        "messaging_product": "whatsapp",
        "permission": {"status": "temporary", "expiration_time": 1_745_343_479},
        "actions": [
            {"action_name": "send_call_permission_request", "can_perform_action": true,
             "limits": [{"time_period": "PT24H", "max_allowed": 1, "current_usage": 0},
                        {"time_period": "P7D", "max_allowed": 2, "current_usage": 1}]},
            {"action_name": "start_call", "can_perform_action": false,
             "limits": [{"time_period": "PT24H", "max_allowed": 5, "current_usage": 5,
                         "limit_expiration_time": 1_745_622_600}]}
        ]
    });
    h.graph.push_json(200, answer);
    let reply = h
        .call(
            Call::get(format!(
                "/v1/numbers/{PN}/call-permissions?phone=%2B13057765456"
            ))
            .key(&key),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({
            "status": "temporary",
            "expiration_time": "2025-04-22T17:37:59Z",
            "actions": [
                {"action_name": "send_call_permission_request", "can_perform_action": true,
                 "limits": [
                    {"time_period": "PT24H", "max_allowed": 1, "current_usage": 0,
                     "limit_expiration_time": null},
                    {"time_period": "P7D", "max_allowed": 2, "current_usage": 1,
                     "limit_expiration_time": null}]},
                {"action_name": "start_call", "can_perform_action": false,
                 "limits": [{"time_period": "PT24H", "max_allowed": 5, "current_usage": 5,
                             "limit_expiration_time": "2025-04-25T23:10:00Z"}]}
            ]
        })
    );
    let read = h.graph.last_request().unwrap();
    assert_eq!(read.method, Method::GET);
    assert_eq!(read.path(), format!("/v25.0/{PN}/call_permissions"));
    assert_eq!(read.url.query(), Some("user_wa_id=%2B13057765456"));
    assert_eq!(read.bearer(), Some(TOKEN));

    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "permission": {"status": "permanent"}}),
    );
    let reply = h
        .call(
            Call::get(format!(
                "/v1/numbers/{PN}/call-permissions?user_id=US.13491208655302741918"
            ))
            .key(&key),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"status": "permanent", "expiration_time": null, "actions": []})
    );
    let read = h.graph.last_request().unwrap();
    assert_eq!(read.url.query(), Some("recipient=US.13491208655302741918"));

    // Refused before any request.
    let before = h.graph.requests().len();
    for (query, field) in [
        ("", "user"),
        ("phone=16505551234", "phone"),
        ("user_id=us.1", "user_id"),
        ("user_id=1234567890", "user_id"),
        ("who=%2B16505551234", "query"),
    ] {
        let reply = h
            .call(Call::get(format!("/v1/numbers/{PN}/call-permissions?{query}")).key(&key))
            .await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{query}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), before, "nothing reached Meta");
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Placing a call ──────────────────────────────────────────────────────

fn connect_call(key: &str, idempotency: &str, body: &Value) -> Call {
    on(Method::POST, &format!("/v1/numbers/{PN}/calls"), key)
        .header("idempotency-key", idempotency)
        .json(body)
}

/// calling/business-initiated-calls, "Initiate call", with
/// calling/call-recording's `recording` object: Meta's request (the
/// tenant's token, the callee, the session, the options), `201` with the
/// call's id; the same key and body replays that answer without a second
/// ring, another body under the key is refused. The call is the number's
/// from then on (no event needed), and only this number's; the record
/// holds neither the call's id nor the callee. Nothing is recorded or
/// transcribed unless the request says so. Decisive: the key of
/// `Fingerprint::connect_call`, `CallRecords::record` in `Calls::connect`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the request, the replay, the record
async fn a_call_is_placed_once_per_key_and_becomes_the_numbers() {
    let (h, key) = connected().await;
    let body = json!({
        "to": {"phone": "+14085551234", "user_id": "US.13491208655302741918"},
        "sdp_offer": SAMPLE_SDP,
        "biz_opaque_callback_data": "0fS5cePMok",
        "recording": {"status": "ENABLED", "purpose": "quality assurance",
                      "announcement_language": "en_US"}
    });
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "calls": [{"id": CALL}]}),
    );
    let reply = h.call(connect_call(&key, "call-1", &body)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(reply.json(), json!({"call_id": CALL}));
    let requests = h.graph.requests();
    let [placed] = &requests[..] else {
        panic!("one request: {requests:?}")
    };
    assert_eq!(placed.method, Method::POST);
    assert_eq!(placed.path(), format!("/v25.0/{PN}/calls"));
    assert_eq!(placed.url.query(), None);
    assert_eq!(placed.bearer(), Some(TOKEN));
    assert_eq!(
        placed.json().unwrap(),
        json!({
            "messaging_product": "whatsapp",
            "to": "+14085551234",
            "recipient": "US.13491208655302741918",
            "action": "connect",
            "session": {"sdp_type": "offer", "sdp": SAMPLE_SDP},
            "biz_opaque_callback_data": "0fS5cePMok",
            "recording": {"status": "ENABLED", "purpose": "quality assurance",
                          "announcement_language": "en_US"}
        }),
        "no transcription key: it was not asked for"
    );
    // The same key and body: no second ring.
    let replayed = h.call(connect_call(&key, "call-1", &body)).await;
    assert_eq!(replayed.status, StatusCode::CREATED, "{}", replayed.text);
    assert_eq!(replayed.json(), reply.json());
    assert_eq!(h.graph.requests().len(), 1, "no second call");
    // Another body under the key: refused.
    let mut other = body.clone();
    other["biz_opaque_callback_data"] = json!("another");
    let reused = h.call(connect_call(&key, "call-1", &other)).await;
    assert_eq!(reused.code(), "idempotency_key_reused", "{}", reused.text);

    // The call is the number's: its actions find it, no event needed.
    h.graph.push_json(200, success());
    let ended = h
        .call(on(
            Method::POST,
            &format!("/v1/numbers/{PN}/calls/{CALL}/terminate"),
            &key,
        ))
        .await;
    assert_eq!(ended.status, StatusCode::NO_CONTENT, "{}", ended.text);
    // Only this number's.
    let reads = h.kv.vault_reads();
    let elsewhere = h
        .call(on(
            Method::POST,
            &format!("/v1/numbers/{PN_2}/calls/{CALL}/terminate"),
            &key,
        ))
        .await;
    assert_eq!(
        (elsewhere.status, elsewhere.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );
    assert_eq!(h.kv.vault_reads(), reads);
    // What is stored holds neither the call's id nor the callee.
    let mut records = 0;
    for (stored, value) in h.kv.writes() {
        if stored.namespace() == meta_whatsapp_server_core::calls::NAMESPACE {
            records += 1;
            assert!(!stored.key().contains("ABGG"), "{stored:?}");
            let value = String::from_utf8(value).unwrap();
            assert!(
                !value.contains("ABGG") && !value.contains("4085551234"),
                "{value}"
            );
        }
    }
    assert_eq!(records, 1);

    // Both options, and no options at all.
    let both = json!({
        "to": {"user_id": "US.ENT.11815799212886844830"},
        "sdp_offer": SAMPLE_SDP,
        "recording": {"status": "DISABLED"},
        "transcription": {"status": "ENABLED", "purpose": "notes", "announcement_language": "es"}
    });
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "calls": [{"id": OTHER_CALL}]}),
    );
    let reply = h.call(connect_call(&key, "call-2", &both)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(
        h.graph.last_request().unwrap().json().unwrap(),
        json!({
            "messaging_product": "whatsapp",
            "recipient": "US.ENT.11815799212886844830",
            "action": "connect",
            "session": {"sdp_type": "offer", "sdp": SAMPLE_SDP},
            "recording": {"status": "DISABLED"},
            "transcription": {"status": "ENABLED", "purpose": "notes", "announcement_language": "es"}
        })
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// A call is refused before any request when its request is wrong: the
/// callee, the SDP, the opaque data, the recording or transcription
/// (a status Meta does not list, a purpose missing or too long); and a
/// refused call takes no key. Decisive: `Calls::connect`'s checks and the
/// library's, each field named, and the key left for the corrected call.
#[tokio::test]
async fn a_call_is_refused_before_any_request_when_its_request_is_wrong() {
    let (h, key) = connected().await;
    let ok = || json!({"to": {"phone": "+14085551234"}, "sdp_offer": SAMPLE_SDP});
    let with = |name: &str, value: Value| {
        let mut body = ok();
        body[name] = value;
        body
    };
    for (body, field) in [
        (json!({"sdp_offer": SAMPLE_SDP}), "body"),
        (json!({"to": {}, "sdp_offer": SAMPLE_SDP}), "to"),
        (
            json!({"to": {"phone": "14085551234"}, "sdp_offer": SAMPLE_SDP}),
            "to.phone",
        ),
        (
            json!({"to": {"user_id": "nobody"}, "sdp_offer": SAMPLE_SDP}),
            "to.user_id",
        ),
        (with("sdp_offer", json!("  \r\n")), "sdp_offer"),
        (
            with("sdp_offer", json!("a".repeat(32 * 1024 + 1))),
            "sdp_offer",
        ),
        (
            with("biz_opaque_callback_data", json!("x".repeat(513))),
            "biz_opaque_callback_data",
        ),
        (
            with("recording", json!({"status": "ON"})),
            "recording.status",
        ),
        (
            with("recording", json!({"status": "ENABLED"})),
            "recording.purpose",
        ),
        (
            with(
                "transcription",
                json!({"status": "ENABLED", "purpose": "p"}),
            ),
            "transcription.announcement_language",
        ),
        (
            with(
                "transcription",
                json!({"status": "ENABLED", "purpose": "p".repeat(251),
                                        "announcement_language": "en_US"}),
            ),
            "transcription.purpose",
        ),
        (with("extra", json!(1)), "body"),
    ] {
        let reply = h.call(connect_call(&key, "call-bad", &body)).await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{field}: {}",
            reply.text
        );
        assert!(!reply.text.contains("ice-pwd"), "{}", reply.text);
    }
    assert!(h.graph.requests().is_empty(), "nothing reached Meta");
    // The refused requests took no key: the corrected one uses it.
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "calls": [{"id": CALL}]}),
    );
    let reply = h.call(connect_call(&key, "call-bad", &ok())).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// A placement that timed out may have rung the user: `504`,
/// `may_have_been_sent`, kept under its key (the repeat is the same answer
/// byte for byte, never a second ring), and the call is not the number's
/// (nobody knows its id: its events will make it so). Decisive: no
/// replay, in the library and in the idempotency run.
#[tokio::test]
async fn a_timed_out_call_is_not_replayed_and_its_key_replays_the_timeout() {
    let (h, key) = connected().await;
    let body = json!({"to": {"phone": "+14085551234"}, "sdp_offer": SAMPLE_SDP});
    h.graph.push_error(|| TransportError::Timeout);
    let first = h.call(connect_call(&key, "call-t", &body)).await;
    assert_eq!(first.status, StatusCode::GATEWAY_TIMEOUT, "{}", first.text);
    assert_eq!(first.json()["error"]["may_have_been_sent"], true);
    let again = h.call(connect_call(&key, "call-t", &body)).await;
    assert_eq!(again.text, first.text, "byte for byte");
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(h.graph.requests().len(), 1, "one request, not two");
    assert_eq!(h.graph.remaining(), 0);
}

// ─── The actions on a user's call ────────────────────────────────────────

/// calling/user-initiated-calls: pre-accept, accept (with a recording and
/// a transcription the caller asked for), reject and terminate each send
/// Meta's request for the call the event reported (the tenant's token, the
/// call's id, the session when there is one) and answer `204`. Without
/// `recording` and `transcription` neither key is sent: the service
/// decides nothing about them. Decisive: `Calls::held` and each action's
/// body.
#[tokio::test]
#[allow(clippy::too_many_lines)] // four actions, each with its request
async fn a_users_call_is_answered_by_its_actions() {
    let (h, key) = connected().await;
    hold(&h, PN, CALL).await;
    let base = format!("/v1/numbers/{PN}/calls/{CALL}");
    let sent = |h: &Harness| h.graph.last_request().unwrap();
    let check = |request: &meta_whatsapp_rs::core::testing::RecordedRequest, body: &Value| {
        assert_eq!(request.method, Method::POST);
        assert_eq!(request.path(), format!("/v25.0/{PN}/calls"));
        assert_eq!(request.url.query(), None);
        assert_eq!(request.bearer(), Some(TOKEN));
        assert_eq!(request.json().as_ref(), Some(body));
    };

    h.graph.push_json(200, success());
    let reply = h
        .call(
            on(Method::POST, &format!("{base}/pre-accept"), &key)
                .json(&json!({"sdp_answer": SAMPLE_SDP})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    assert!(reply.text.is_empty());
    check(
        &sent(&h),
        &json!({"messaging_product": "whatsapp", "call_id": CALL, "action": "pre_accept",
                "session": {"sdp_type": "answer", "sdp": SAMPLE_SDP}}),
    );

    h.graph.push_json(200, success());
    let reply = h
        .call(
            on(Method::POST, &format!("{base}/accept"), &key)
                .json(&json!({"sdp_answer": SAMPLE_SDP})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    check(
        &sent(&h),
        &json!({"messaging_product": "whatsapp", "call_id": CALL, "action": "accept",
                "session": {"sdp_type": "answer", "sdp": SAMPLE_SDP}}),
    );

    h.graph.push_json(200, success());
    let reply = h
        .call(
            on(Method::POST, &format!("{base}/accept"), &key).json(&json!({
                "sdp_answer": SAMPLE_SDP,
                "biz_opaque_callback_data": "random_string",
                "recording": {"status": "ENABLED", "purpose": "quality assurance",
                              "announcement_language": "en_US"},
                "transcription": {"status": "ENABLED", "purpose": "quality assurance",
                                  "announcement_language": "en_US"}
            })),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    check(
        &sent(&h),
        &json!({"messaging_product": "whatsapp", "call_id": CALL, "action": "accept",
                "session": {"sdp_type": "answer", "sdp": SAMPLE_SDP},
                "biz_opaque_callback_data": "random_string",
                "recording": {"status": "ENABLED", "purpose": "quality assurance",
                              "announcement_language": "en_US"},
                "transcription": {"status": "ENABLED", "purpose": "quality assurance",
                                  "announcement_language": "en_US"}}),
    );

    h.graph.push_json(200, success());
    let reply = h
        .call(on(Method::POST, &format!("{base}/reject"), &key))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    check(
        &sent(&h),
        &json!({"messaging_product": "whatsapp", "call_id": CALL, "action": "reject"}),
    );

    h.graph.push_json(200, success());
    let reply = h
        .call(on(Method::POST, &format!("{base}/terminate"), &key))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    check(
        &sent(&h),
        &json!({"messaging_product": "whatsapp", "call_id": CALL, "action": "terminate"}),
    );

    // `success: false` is Meta not doing it: an error, not a 204.
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "success": false}),
    );
    let reply = h
        .call(on(Method::POST, &format!("{base}/terminate"), &key))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY, "{}", reply.text);

    // Refused before any request.
    let before = h.graph.requests().len();
    for (action, body, field) in [
        ("pre-accept", json!({}), "body"),
        ("pre-accept", json!({"sdp_answer": " "}), "sdp_answer"),
        (
            "accept",
            json!({"sdp_answer": SAMPLE_SDP, "recording": {"status": "ENABLED"}}),
            "recording.purpose",
        ),
        (
            "accept",
            json!({"sdp_answer": SAMPLE_SDP, "biz_opaque_callback_data": "x".repeat(513)}),
            "biz_opaque_callback_data",
        ),
        (
            "accept",
            json!({"sdp_answer": SAMPLE_SDP, "stray": 1}),
            "body",
        ),
    ] {
        let reply = h
            .call(on(Method::POST, &format!("{base}/{action}"), &key).json(&body))
            .await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{action} {body}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), before, "nothing reached Meta");
    assert_eq!(h.graph.remaining(), 0);
}

/// A call action is never repeated: a timeout is `504` with one request
/// (the library marks the call non-idempotent), and Meta's refusal for a
/// missing permission (`138006`) is answered by its code and the service's
/// kind, never Meta's text. Decisive: no retry of a call action.
#[tokio::test]
async fn a_call_action_that_timed_out_is_not_repeated_and_metas_text_is_not_answered() {
    let (h, key) = connected().await;
    hold(&h, PN, CALL).await;
    h.graph.push_error(|| TransportError::Timeout);
    let reply = h
        .call(on(
            Method::POST,
            &format!("/v1/numbers/{PN}/calls/{CALL}/terminate"),
            &key,
        ))
        .await;
    assert_eq!(reply.status, StatusCode::GATEWAY_TIMEOUT, "{}", reply.text);
    assert_eq!(h.graph.requests().len(), 1);

    h.graph.push_json(
        400,
        graph_error(
            138_006,
            "No approved call permission from SECRET-SDP-LINE-9Q",
        ),
    );
    let reply = h
        .call(connect_call(
            &key,
            "call-denied",
            &json!({"to": {"phone": "+14085551234"}, "sdp_offer": SAMPLE_SDP}),
        ))
        .await;
    assert!(reply.status.is_client_error() || reply.status.is_server_error());
    assert_eq!(
        reply.json()["error"]["graph"]["code"],
        138_006,
        "{}",
        reply.text
    );
    assert!(!reply.text.contains("SECRET-SDP-LINE"), "{}", reply.text);
    assert!(reply.json()["error"]["graph"]["details"].is_null());
    assert_eq!(h.graph.remaining(), 0);
}

// ─── M5.2: a call the number does not hold ───────────────────────────────

/// The design's M5.2 (D210): a call id the number does not hold is `404
/// not_found` with **zero vault reads** and nothing asked of Meta, on every
/// route that names one: another tenant's call, another number's of the
/// same tenant, an id never recorded, an id that cannot be a call's. The
/// same id on the number that holds it is found. Decisive:
/// `CallRecords::owned` before `Authorizer::owned_number` in the
/// `OwnedCall` extractor (put the vault read first and every count below
/// moves).
#[tokio::test]
async fn a_call_the_number_does_not_hold_is_not_found_with_zero_vault_reads() {
    let (h, key) = connected().await;
    // A call of the second tenant's, and one of this tenant's other number.
    hold(&h, OTHER_PN, CALL).await;
    hold(&h, PN_2, OTHER_CALL).await;
    let routes = [
        ("pre-accept", Some(json!({"sdp_answer": SAMPLE_SDP}))),
        ("accept", Some(json!({"sdp_answer": SAMPLE_SDP}))),
        ("reject", None),
        ("terminate", None),
    ];
    let mut checked = 0;
    for call in [CALL, OTHER_CALL, "NEVER-RECORDED", "a%20b", "%C3%A9"] {
        for (action, body) in &routes {
            let mut request = on(
                Method::POST,
                &format!("/v1/numbers/{PN}/calls/{call}/{action}"),
                &key,
            );
            if let Some(body) = body {
                request = request.json(body);
            }
            let reads = h.kv.vault_reads();
            let reply = h.call(request).await;
            assert_eq!(
                (reply.status, reply.code().as_str()),
                (StatusCode::NOT_FOUND, "not_found"),
                "{action} {call}: {}",
                reply.text
            );
            assert_eq!(h.kv.vault_reads(), reads, "{action} {call} read the vault");
            checked += 1;
        }
    }
    assert_eq!(checked, 5 * 4);
    assert!(h.graph.requests().is_empty(), "Meta was never asked");
    // The tenant's own call, on the number that holds it: found, and now
    // the vault is read, once.
    let reads = h.kv.vault_reads();
    h.graph.push_json(200, success());
    let reply = h
        .call(on(
            Method::POST,
            &format!("/v1/numbers/{PN_2}/calls/{OTHER_CALL}/reject"),
            &key,
        ))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    assert!(
        h.kv.vault_reads() > reads,
        "the token is opened after the call"
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// What makes a call the number's: Meta's connect event of a user's call,
/// and a business-initiated call's status event, each for the tenant's own
/// number; not an event of another tenant's number, and nothing else. The
/// record is under the tenant and the number it was reported for.
/// Decisive: `CallRecords::on_event`'s two arms.
#[tokio::test]
async fn call_events_make_the_calls_the_numbers() {
    let (h, key) = connected().await;
    let terminate = |pn: &str, call: &str| {
        on(
            Method::POST,
            &format!("/v1/numbers/{pn}/calls/{call}/terminate"),
            &key,
        )
    };
    // Not yet.
    let reply = h.call(terminate(PN, CALL)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    // Another tenant's number reports it: it is theirs.
    hold(&h, OTHER_PN, CALL).await;
    let reply = h.call(terminate(PN, CALL)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    // A business-initiated call's status on this tenant's number.
    let reply = h.webhook(&call_status(WABA, PN, OTHER_CALL)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    h.graph.push_json(200, success());
    let reply = h.call(terminate(PN, OTHER_CALL)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    // The user's call on this tenant's number.
    hold(&h, PN, THIRD_CALL).await;
    h.graph.push_json(200, success());
    let reply = h.call(terminate(PN, THIRD_CALL)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    // ...and not on its other number.
    let reply = h.call(terminate(PN_2, THIRD_CALL)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    // Still not the tenant's: the call the other tenant's number reported.
    let reply = h.call(terminate(PN, CALL)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    assert_eq!(h.graph.remaining(), 0);
    // The events reach the tenant as ever (they are not changed): the
    // outbox row of the connect event is the tenant's.
    let events = h
        .call(Call::get("/v1/events").key(&h.tenant_key(TENANT, &[Scope::Events]).await))
        .await;
    assert_eq!(events.status, StatusCode::OK, "{}", events.text);
    let kinds: Vec<String> = events.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_owned())
        .collect();
    assert!(kinds.contains(&"call_updated".to_owned()), "{kinds:?}");
    assert!(
        kinds.contains(&"call_status_updated".to_owned()),
        "{kinds:?}"
    );
}

// ─── Scope ───────────────────────────────────────────────────────────────

/// The calling routes need the scope `calls` (and the call analytics
/// `analytics`): a key without it is `403 forbidden` before anything is
/// read. Decisive: `Operation::scope` for each route.
#[tokio::test]
async fn the_calling_routes_need_their_scope() {
    let (h, _) = connected().await;
    hold(&h, PN, CALL).await;
    // Every scope but `calls`: none stands in for it.
    let others: Vec<Scope> = Scope::ALL
        .into_iter()
        .filter(|scope| *scope != Scope::Calls)
        .collect();
    let send = h.tenant_key(TENANT, &others).await;
    let calls_only = h.tenant_key(TENANT, &[Scope::Calls]).await;
    let reads = h.kv.vault_reads();
    for (method, path, body) in [
        (Method::GET, format!("/v1/numbers/{PN}/call-settings"), None),
        (
            Method::PATCH,
            format!("/v1/numbers/{PN}/call-settings"),
            Some(json!({"status": "ENABLED"})),
        ),
        (
            Method::GET,
            format!("/v1/numbers/{PN}/call-permissions?phone=%2B16505551234"),
            None,
        ),
        (
            Method::POST,
            format!("/v1/numbers/{PN}/calls"),
            Some(json!({"to": {"phone": "+16505551234"}, "sdp_offer": SAMPLE_SDP})),
        ),
        (
            Method::POST,
            format!("/v1/numbers/{PN}/calls/{CALL}/reject"),
            None,
        ),
    ] {
        let mut request = on(method.clone(), &path, &send);
        if let Some(body) = &body {
            request = request.json(body);
        }
        let reply = h.call(request).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{method} {path}: {}",
            reply.text
        );
    }
    // The analytics are not the calls' scope.
    let range = {
        let now = time::OffsetDateTime::now_utc();
        let at = |days: i64| {
            (now - time::Duration::days(days))
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap()
                .replace('+', "%2B")
        };
        format!("start={}&end={}&granularity=DAILY", at(2), at(1))
    };
    let reply = h
        .call(Call::get(format!("/v1/wabas/{WABA}/analytics/calls?{range}")).key(&calls_only))
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text);
    assert_eq!(h.kv.vault_reads(), reads);
    assert!(h.graph.requests().is_empty());
}

// ─── Call analytics ──────────────────────────────────────────────────────

/// analytics, "Call analytics": the field expansion Meta documents
/// (`call_analytics.start(…).end(…).granularity(DAILY).directions([…])…`),
/// the WABA's own node, the answer's points. A filter that is not Meta's
/// value, or a range out of bounds, is refused with no request.
/// Decisive: `call_query` and the library's expansion.
#[tokio::test]
#[allow(clippy::too_many_lines)] // Meta's example, the no-data answer, each refusal
async fn call_analytics_are_asked_as_metas_expansion_and_bounded() {
    let (h, key) = connected().await;
    let now = time::OffsetDateTime::now_utc();
    let rfc = |at: time::OffsetDateTime| {
        at.format(&time::format_description::well_known::Rfc3339)
            .unwrap()
            .replace('+', "%2B")
    };
    let (start, end) = (now - time::Duration::days(3), now - time::Duration::days(1));
    h.graph.push_json(
        200,
        json!({"call_analytics": {"granularity": "DAILY", "directions": "USER_INITIATED",
            "data_points": [
                {"start": 1_765_958_400, "end": 1_766_044_800, "cost": 0.47795, "count": 35,
                 "average_duration": 106},
                {"start": 1_760_943_600, "end": 1_761_030_000, "cost": 0, "count": 20,
                 "average_duration": 103, "direction": "USER_INITIATED", "country": "US",
                 "phone_number": "16505550111"}]},
            "id": WABA}),
    );
    let reply = h
        .call(
            Call::get(format!(
                "/v1/wabas/{WABA}/analytics/calls?start={}&end={}&granularity=DAILY\
                 &directions=USER_INITIATED&dimensions=direction,country&metric_types=COUNT,AVERAGE_DURATION\
                 &phone_numbers=16505550111&country_codes=US",
                rfc(start),
                rfc(end)
            ))
            .key(&key),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"data_points": [
            {"start": "2025-12-17T08:00:00Z", "end": "2025-12-18T08:00:00Z", "count": 35,
             "cost": 0.47795, "average_duration": 106.0, "phone_number": null, "country": null,
             "direction": null},
            {"start": "2025-10-20T07:00:00Z", "end": "2025-10-21T07:00:00Z", "count": 20,
             "cost": 0.0, "average_duration": 103.0, "phone_number": "16505550111",
             "country": "US", "direction": "USER_INITIATED"}]})
    );
    let asked = h.graph.last_request().unwrap();
    assert_eq!(asked.method, Method::GET);
    assert_eq!(asked.path(), format!("/v25.0/{WABA}"));
    assert_eq!(asked.bearer(), Some(TOKEN));
    let fields = asked
        .url
        .query_pairs()
        .find(|(k, _)| k == "fields")
        .map(|(_, v)| v.into_owned())
        .unwrap();
    assert_eq!(
        fields,
        format!(
            "call_analytics.start({}).end({}).granularity(DAILY).phone_numbers([\"16505550111\"]).country_codes([\"US\"]).directions([\"USER_INITIATED\"]).dimensions([\"direction\",\"country\"]).metric_types([\"COUNT\",\"AVERAGE_DURATION\"])",
            start.unix_timestamp(),
            end.unix_timestamp()
        )
    );

    // No data: no points.
    h.graph.push_json(200, json!({"id": WABA}));
    let reply = h
        .call(
            Call::get(format!(
                "/v1/wabas/{WABA}/analytics/calls?start={}&end={}&granularity=MONTHLY",
                rfc(start),
                rfc(end)
            ))
            .key(&key),
        )
        .await;
    assert_eq!(reply.json(), json!({"data_points": []}));

    // Refused before any request.
    let before = h.graph.requests().len();
    let query = |extra: &str| {
        format!(
            "/v1/wabas/{WABA}/analytics/calls?start={}&end={}&granularity=DAILY{extra}",
            rfc(start),
            rfc(end)
        )
    };
    for (path, field) in [
        (query("&directions=BOTH"), "directions[0]"),
        (query("&dimensions=direction,DIRECTION"), "dimensions[1]"),
        (query("&metric_types=SUM"), "metric_types[0]"),
        (query("&phone_numbers=1)"), "phone_numbers[0]"),
        (query("&country_codes=USA"), "country_codes[0]"),
        (
            format!(
                "/v1/wabas/{WABA}/analytics/calls?start={}&end={}&granularity=DAY",
                rfc(start),
                rfc(end)
            ),
            "granularity",
        ),
        (
            format!(
                "/v1/wabas/{WABA}/analytics/calls?start={}&end={}&granularity=DAILY",
                rfc(now - time::Duration::days(400)),
                rfc(end)
            ),
            "start",
        ),
        (
            format!(
                "/v1/wabas/{WABA}/analytics/calls?start={}&end={}&granularity=HALF_HOUR",
                rfc(now - time::Duration::days(40)),
                rfc(end)
            ),
            "end",
        ),
    ] {
        let reply = h.call(Call::get(path.clone()).key(&key)).await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{path}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), before, "nothing reached Meta");
    // Another tenant's WABA: not found, Meta not asked.
    let reply = h
        .call(
            Call::get(format!(
                "/v1/wabas/{OTHER_WABA}/analytics/calls?start={}&end={}&granularity=DAILY",
                rfc(start),
                rfc(end)
            ))
            .key(&key),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    assert_eq!(h.graph.requests().len(), before);
    assert_eq!(h.graph.remaining(), 0);
}
