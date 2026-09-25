use std::collections::BTreeMap;
use std::time::Duration;

use http::Method;
use serde_json::json;
use time::macros::date;
use meta_whatsapp_core::error::TransportError;
use meta_whatsapp_core::ids::MediaId;
use meta_whatsapp_core::testing::ScriptedTransport;
use meta_whatsapp_core::{Error, ErrorKind};

use super::settings::*;
use super::*;
use crate::RetryPolicy;

const PHONE: &str = "106540352242922";
const CALL: &str = "wacid.ABGGFjFVU2AfAgo6V-Hc5eCgK5Gh";
const SDP: &str = "v=0\r\no=- 7669997803033704573 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n";

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

fn validation_field(err: &Error) -> &str {
    match err {
        Error::Validation(v) => &v.field,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

fn t(h: u8, m: u8) -> ClockTime {
    ClockTime::new(h, m).unwrap()
}

fn assert_calls_post(ts: &ScriptedTransport, body: &serde_json::Value) {
    let req = ts.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{PHONE}/calls"));
    assert_eq!(req.url.query(), None);
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(req.json().as_ref(), Some(body));
}

// ── Call actions ────────────────────────────────────────────────────────

#[tokio::test]
async fn connect_matches_docs_example_with_bsuid() {
    let ts = ScriptedTransport::new();
    // calling/reference "Initiate call" success response.
    ts.push_json(
        200,
        json!({"messaging_product": "whatsapp", "calls": [{"id": "wacid.ABGGFjFVU2AfAgo6V"}]}),
    );
    let started = client(&ts)
        .calling(PHONE)
        .connect(&ConnectCall {
            to: Recipient::PhoneAndUser {
                phone: "14085551234".into(),
                user: "US.13491208655302741918".into(),
            },
            sdp_offer: SDP.into(),
            biz_opaque_callback_data: Some("0fS5cePMok".into()),
        })
        .await
        .unwrap();
    assert_calls_post(
        &ts,
        &json!({
            "messaging_product": "whatsapp",
            "to": "14085551234",
            "recipient": "US.13491208655302741918",
            "action": "connect",
            "session": {"sdp_type": "offer", "sdp": SDP},
            "biz_opaque_callback_data": "0fS5cePMok"
        }),
    );
    assert_eq!(
        started.call_id(),
        Some(&CallId::new("wacid.ABGGFjFVU2AfAgo6V"))
    );
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn connect_by_phone_or_bsuid_only() {
    let ts = ScriptedTransport::new();
    ts.push_json(200, json!({"calls": [{"id": "wacid.1"}]}));
    ts.push_json(200, json!({"calls": [{"id": "wacid.2"}]}));
    let calling = client(&ts).calling(PHONE);
    calling
        .connect(&ConnectCall::new(Recipient::phone("17863476655"), SDP))
        .await
        .unwrap();
    assert_calls_post(
        &ts,
        &json!({"messaging_product": "whatsapp", "to": "17863476655", "action": "connect", "session": {"sdp_type": "offer", "sdp": SDP}}),
    );
    calling
        .connect(&ConnectCall::new(
            UserId::new("US.ENT.11815799212886844830"),
            SDP,
        ))
        .await
        .unwrap();
    assert_calls_post(
        &ts,
        &json!({"messaging_product": "whatsapp", "recipient": "US.ENT.11815799212886844830", "action": "connect", "session": {"sdp_type": "offer", "sdp": SDP}}),
    );
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn connect_is_never_replayed() {
    let ts = ScriptedTransport::new();
    ts.push_error(|| TransportError::Timeout);
    let c = Client::builder()
        .transport(ts.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy {
            max_retries: 2,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        })
        .build()
        .unwrap();
    let err = c
        .calling(PHONE)
        .connect(&ConnectCall::new(Recipient::phone("1"), SDP))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(ts.requests().len(), 1, "a replay would ring the user twice");
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn pre_accept_matches_docs_example() {
    let ts = ScriptedTransport::new();
    ts.push_json(
        200,
        json!({"messaging_product": "whatsapp", "success": true}),
    );
    client(&ts)
        .calling(PHONE)
        .pre_accept(&CallId::new(CALL), SDP)
        .await
        .unwrap();
    assert_calls_post(
        &ts,
        &json!({"messaging_product": "whatsapp", "call_id": CALL, "action": "pre_accept", "session": {"sdp_type": "answer", "sdp": SDP}}),
    );
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn accept_matches_docs_example() {
    let ts = ScriptedTransport::new();
    ts.push_json(
        200,
        json!({"messaging_product": "whatsapp", "success": true}),
    );
    ts.push_json(
        200,
        json!({"messaging_product": "whatsapp", "success": true}),
    );
    let calling = client(&ts).calling(PHONE);
    calling
        .accept(&CallId::new(CALL), SDP, Some("random_string"))
        .await
        .unwrap();
    assert_calls_post(
        &ts,
        &json!({"messaging_product": "whatsapp", "call_id": CALL, "action": "accept", "session": {"sdp_type": "answer", "sdp": SDP}, "biz_opaque_callback_data": "random_string"}),
    );
    calling.accept(&CallId::new(CALL), SDP, None).await.unwrap();
    assert_calls_post(
        &ts,
        &json!({"messaging_product": "whatsapp", "call_id": CALL, "action": "accept", "session": {"sdp_type": "answer", "sdp": SDP}}),
    );
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn reject_and_terminate_match_docs_examples() {
    let ts = ScriptedTransport::new();
    ts.push_json(
        200,
        json!({"messaging_product": "whatsapp", "success": true}),
    );
    ts.push_json(
        200,
        json!({"messaging_product": "whatsapp", "success": true}),
    );
    let calling = client(&ts).calling(PHONE);
    calling.reject(&CallId::new(CALL)).await.unwrap();
    assert_calls_post(
        &ts,
        &json!({"messaging_product": "whatsapp", "call_id": CALL, "action": "reject"}),
    );
    calling.terminate(&CallId::new(CALL)).await.unwrap();
    assert_calls_post(
        &ts,
        &json!({"messaging_product": "whatsapp", "call_id": CALL, "action": "terminate"}),
    );
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn success_false_on_an_action_is_an_error() {
    let ts = ScriptedTransport::new();
    ts.push_json(
        200,
        json!({"messaging_product": "whatsapp", "success": false}),
    );
    let err = client(&ts)
        .calling(PHONE)
        .terminate(&CallId::new(CALL))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Http { status: 200, .. }), "{err:?}");
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn missing_permission_error_is_decoded() {
    let ts = ScriptedTransport::new();
    // calling-api: 138006 = no call permission from the user.
    ts.push_json(
        400,
        json!({"error": {"message": "(#138006) No approved call permission from the recipient", "type": "OAuthException", "code": 138006}}),
    );
    let err = client(&ts)
        .calling(PHONE)
        .connect(&ConnectCall::new(Recipient::phone("1"), SDP))
        .await
        .unwrap_err();
    assert_eq!(err.graph().map(|g| g.code), Some(138006));
    assert_eq!(err.kind(), ErrorKind::Unknown);
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn call_inputs_are_validated() {
    let ts = ScriptedTransport::new();
    let calling = client(&ts).calling(PHONE);
    let err = calling
        .connect(&ConnectCall::new(Recipient::group("Y2FwaV9ncm91cDox"), SDP))
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "to");
    let err = calling
        .connect(&ConnectCall::new(Recipient::phone("1"), " "))
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "session.sdp");
    let err = calling
        .connect(&ConnectCall {
            biz_opaque_callback_data: Some("x".repeat(513)),
            ..ConnectCall::new(Recipient::phone("1"), SDP)
        })
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "biz_opaque_callback_data");
    let err = calling
        .accept(&CallId::new(CALL), SDP, Some(&"x".repeat(513)))
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "biz_opaque_callback_data");
    let err = calling
        .pre_accept(&CallId::new(CALL), "")
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "session.sdp");
    for bad in [
        calling.reject(&CallId::new("")).await,
        calling.terminate(&CallId::new(" ")).await,
    ] {
        assert_eq!(validation_field(&bad.unwrap_err()), "call_id");
    }
    let err = calling
        .permissions(&Recipient::group("G"))
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "user");
    assert!(ts.requests().is_empty());

    // 512 characters exactly is fine.
    ts.push_json(200, json!({"success": true}));
    calling
        .accept(&CallId::new(CALL), SDP, Some(&"é".repeat(512)))
        .await
        .unwrap();
    assert_eq!(ts.remaining(), 0);
}

// ── Permissions ─────────────────────────────────────────────────────────

#[tokio::test]
async fn permissions_match_docs_example() {
    let ts = ScriptedTransport::new();
    // calling/user-call-permissions response (trailing commas removed).
    ts.push_json(
        200,
        json!({
            "messaging_product": "whatsapp",
            "permission": {"status": "temporary", "expiration_time": 1745343479},
            "actions": [
                {
                    "action_name": "send_call_permission_request",
                    "can_perform_action": true,
                    "limits": [
                        {"time_period": "PT24H", "max_allowed": 1, "current_usage": 0},
                        {"time_period": "P7D", "max_allowed": 2, "current_usage": 1}
                    ]
                },
                {
                    "action_name": "start_call",
                    "can_perform_action": false,
                    "limits": [{"time_period": "PT24H", "max_allowed": 5, "current_usage": 5, "limit_expiration_time": 1745622600}]
                }
            ]
        }),
    );
    let p = client(&ts)
        .calling(PHONE)
        .permissions(&Recipient::phone("13057765456"))
        .await
        .unwrap();
    let req = ts.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), format!("/v25.0/{PHONE}/call_permissions"));
    assert_eq!(req.url.query(), Some("user_wa_id=13057765456"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(p.permission.status, CallPermissionStatus::Temporary);
    assert_eq!(
        p.permission
            .expiration_time
            .map(OffsetDateTime::unix_timestamp),
        Some(1745343479)
    );
    assert_eq!(
        p.actions[0].action_name,
        CallPermissionActionName::SendCallPermissionRequest
    );
    assert!(p.actions[0].can_perform_action);
    assert_eq!(p.actions[0].limits[1].time_period, "P7D");
    assert_eq!(p.actions[0].limits[1].current_usage, 1);
    assert_eq!(p.actions[0].limits[0].limit_expiration_time, None);
    assert_eq!(
        p.actions[1].action_name,
        CallPermissionActionName::StartCall
    );
    assert!(!p.actions[1].can_perform_action);
    assert_eq!(
        p.actions[1].limits[0]
            .limit_expiration_time
            .map(OffsetDateTime::unix_timestamp),
        Some(1745622600)
    );
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn permissions_by_bsuid_and_permanent_status() {
    let ts = ScriptedTransport::new();
    ts.push_json(
        200,
        json!({"messaging_product": "whatsapp", "permission": {"status": "permanent"}}),
    );
    ts.push_json(
        200,
        json!({"permission": {"status": "no_permission", "expiration": 1}}),
    );
    let calling = client(&ts).calling(PHONE);
    let p = calling
        .permissions(&Recipient::user("US.13491208655302741918"))
        .await
        .unwrap();
    assert_eq!(
        ts.last_request().unwrap().url.query(),
        Some("recipient=US.13491208655302741918")
    );
    assert_eq!(p.permission.status, CallPermissionStatus::Permanent);
    assert_eq!(p.permission.expiration_time, None);
    assert!(p.actions.is_empty());

    let p = calling
        .permissions(&Recipient::PhoneAndUser {
            phone: "+13057765456".into(),
            user: "US.1".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        ts.last_request().unwrap().url.query(),
        Some("user_wa_id=%2B13057765456")
    );
    assert_eq!(p.permission.status, CallPermissionStatus::NoPermission);
    assert_eq!(
        p.permission
            .expiration_time
            .map(OffsetDateTime::unix_timestamp),
        Some(1),
        "the table's `expiration` name is read too"
    );
    assert_eq!(ts.remaining(), 0);
}

// ── Settings ────────────────────────────────────────────────────────────

fn docs_settings_json() -> serde_json::Value {
    // calling/call-settings "Get phone number calling settings" response
    // (with the SIP password variant and a restriction), placeholders
    // filled in.
    json!({
        "calling": {
            "status": "ENABLED",
            "call_icon_visibility": "DEFAULT",
            "callback_permission_status": "ENABLED",
            "call_hours": {
                "status": "ENABLED",
                "timezone_id": "[REDACTED]",
                "weekly_operating_hours": [
                    {"day_of_week": "MONDAY", "open_time": "0400", "close_time": "1020"},
                    {"day_of_week": "TUESDAY", "open_time": "0108", "close_time": "1020"}
                ],
                "holiday_schedule": [{"date": "2026-01-01", "start_time": "0000", "end_time": "2359"}]
            },
            "sip": {
                "status": "ENABLED",
                "servers": [{"app_id": 1234567890, "hostname": "sip.example.com", "sip_user_password": "s3cr3t-digest"}]
            },
            "audio": {"additional_codecs": ["PCMA", "PCMU"]},
            "voicemail": {
                "status": "ENABLED",
                "triggers": ["REJECT", "TIMEOUT"],
                "audio": {"default": {"announcement_media_id": 938884519013664_u64, "timeout_seconds": 20}}
            },
            "restrictions": {"restrictions_list": [{
                "type": "RESTRICTED_BUSINESS_INITIATED_CALLING",
                "reason": "Business initiated calling capability has been temporarily disabled for this phone number due to high negative feedback from users.",
                "expiration": 1754072386
            }]}
        },
        "storage_configuration": {"status": "DEFAULT"}
    })
}

#[tokio::test]
async fn settings_parse_docs_example() {
    let ts = ScriptedTransport::new();
    ts.push_json(200, docs_settings_json());
    let s = client(&ts)
        .calling(PHONE)
        .settings(true)
        .await
        .unwrap()
        .unwrap();
    let req = ts.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), format!("/v25.0/{PHONE}/settings"));
    assert_eq!(req.url.query(), Some("include_sip_credentials=true"));
    assert_eq!(req.bearer(), Some("TOKEN"));

    assert_eq!(s.status, Some(FeatureStatus::Enabled));
    assert_eq!(s.call_icon_visibility, Some(CallIconVisibility::Default));
    let hours = s.call_hours.as_ref().unwrap();
    assert_eq!(
        hours.weekly_operating_hours[1].day_of_week,
        DayOfWeek::Tuesday
    );
    assert_eq!(hours.weekly_operating_hours[1].open_time, t(1, 8));
    assert_eq!(hours.weekly_operating_hours[1].close_time, t(10, 20));
    let holiday = &hours.holiday_schedule.as_ref().unwrap()[0];
    assert_eq!(holiday.date, date!(2026 - 01 - 01));
    assert_eq!(holiday.end_time, t(23, 59));
    let server = &s.sip.as_ref().unwrap().servers.as_ref().unwrap()[0];
    assert_eq!(server.hostname, "sip.example.com");
    assert_eq!(server.app_id.as_deref(), Some("1234567890"));
    let password = server.sip_user_password.as_ref().unwrap();
    assert_eq!(password.expose_secret(), "s3cr3t-digest");
    assert!(
        !format!("{s:?}").contains("s3cr3t-digest"),
        "Debug redacts the SIP password"
    );
    assert_eq!(
        s.audio.as_ref().unwrap().additional_codecs,
        Some(vec![AudioCodec::Pcma, AudioCodec::Pcmu])
    );
    let vm = s.voicemail.as_ref().unwrap();
    assert_eq!(
        vm.triggers,
        [VoicemailTrigger::Reject, VoicemailTrigger::Timeout]
    );
    let audio = &vm.audio.as_ref().unwrap().default;
    assert_eq!(
        audio.announcement_media_id,
        Some(MediaId::new("938884519013664"))
    );
    assert_eq!(audio.timeout_seconds, Some(20));
    let restriction = &s.restrictions.as_ref().unwrap().restrictions_list[0];
    assert_eq!(
        restriction.kind,
        RestrictionType::RestrictedBusinessInitiatedCalling
    );
    assert_eq!(
        restriction.expiration.map(OffsetDateTime::unix_timestamp),
        Some(1754072386)
    );
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn settings_without_credentials_or_calling_object() {
    let ts = ScriptedTransport::new();
    ts.push_json(200, json!({"storage_configuration": {"status": "DEFAULT"}}));
    let s = client(&ts).calling(PHONE).settings(false).await.unwrap();
    assert_eq!(ts.last_request().unwrap().url.query(), None);
    assert_eq!(s, None);
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn read_then_write_round_trips_without_response_only_data() {
    let ts = ScriptedTransport::new();
    ts.push_json(200, docs_settings_json());
    ts.push_json(200, json!({"success": true}));
    let calling = client(&ts).calling(PHONE);
    let settings = calling.settings(true).await.unwrap().unwrap();
    calling.update_settings(&settings).await.unwrap();
    let mut expected = docs_settings_json()["calling"].clone();
    // Never sent back: restrictions, the SIP password, the app id.
    expected.as_object_mut().unwrap().remove("restrictions");
    expected["sip"]["servers"] = json!([{"hostname": "sip.example.com"}]);
    let req = ts.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{PHONE}/settings"));
    assert_eq!(req.json(), Some(json!({"calling": expected})));
    assert_eq!(ts.remaining(), 0);
}

/// The settings of the calling/call-settings request body, placeholders
/// filled in.
fn docs_update() -> CallingSettings {
    let mut params = BTreeMap::new();
    params.insert("KEY1".to_owned(), "VALUE1".to_owned());
    params.insert("KEY2".to_owned(), "VALUE2".to_owned());
    CallingSettings {
        status: Some(FeatureStatus::Enabled),
        call_icon_visibility: Some(CallIconVisibility::Default),
        call_icons: Some(CallIcons {
            restrict_to_user_countries: Some(vec!["US".into(), "BR".into()]),
        }),
        call_hours: Some(CallHours {
            status: FeatureStatus::Enabled,
            timezone_id: "America/Manaus".into(),
            weekly_operating_hours: vec![
                WeeklyHours {
                    day_of_week: DayOfWeek::Monday,
                    open_time: t(4, 0),
                    close_time: t(10, 20),
                },
                WeeklyHours {
                    day_of_week: DayOfWeek::Tuesday,
                    open_time: t(1, 8),
                    close_time: t(10, 20),
                },
            ],
            holiday_schedule: Some(vec![HolidayHours {
                date: date!(2026 - 01 - 01),
                start_time: t(0, 0),
                end_time: t(23, 59),
            }]),
        }),
        callback_permission_status: Some(FeatureStatus::Enabled),
        sip: Some(SipSettings {
            status: Some(FeatureStatus::Enabled),
            webhook_delivery: None,
            servers: Some(vec![SipServer {
                hostname: "sip.example.com".into(),
                port: Some(5061),
                request_uri_user_params: params,
                ..SipServer::default()
            }]),
        }),
        audio: Some(AudioSettings {
            additional_codecs: Some(vec![AudioCodec::Pcma, AudioCodec::Pcmu]),
        }),
        voicemail: Some(Voicemail {
            status: FeatureStatus::Enabled,
            triggers: vec![VoicemailTrigger::Reject, VoicemailTrigger::Timeout],
            audio: Some(VoicemailAudio {
                default: VoicemailAudioConfig {
                    announcement_media_id: Some(MediaId::new("938884519013664")),
                    timeout_seconds: Some(20),
                },
            }),
        }),
        restrictions: None,
    }
}

#[tokio::test]
async fn update_settings_matches_docs_request_body() {
    let ts = ScriptedTransport::new();
    ts.push_error(|| TransportError::Timeout);
    ts.push_json(200, json!({"success": true}));
    let c = Client::builder()
        .transport(ts.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy {
            max_retries: 1,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        })
        .build()
        .unwrap();
    c.calling(PHONE)
        .update_settings(&docs_update())
        .await
        .unwrap();
    let reqs = ts.requests();
    assert_eq!(reqs.len(), 2, "settings updates are idempotent");
    let req = &reqs[1];
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{PHONE}/settings"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    // calling/call-settings request body.
    assert_eq!(
        req.json(),
        Some(json!({"calling": {
            "status": "ENABLED",
            "call_icon_visibility": "DEFAULT",
            "call_icons": {"restrict_to_user_countries": ["US", "BR"]},
            "call_hours": {
                "status": "ENABLED",
                "timezone_id": "America/Manaus",
                "weekly_operating_hours": [
                    {"day_of_week": "MONDAY", "open_time": "0400", "close_time": "1020"},
                    {"day_of_week": "TUESDAY", "open_time": "0108", "close_time": "1020"}
                ],
                "holiday_schedule": [{"date": "2026-01-01", "start_time": "0000", "end_time": "2359"}]
            },
            "callback_permission_status": "ENABLED",
            "sip": {
                "status": "ENABLED",
                "servers": [{"hostname": "sip.example.com", "port": 5061, "request_uri_user_params": {"KEY1": "VALUE1", "KEY2": "VALUE2"}}]
            },
            "audio": {"additional_codecs": ["PCMA", "PCMU"]},
            "voicemail": {
                "status": "ENABLED",
                "triggers": ["REJECT", "TIMEOUT"],
                "audio": {"default": {"announcement_media_id": 938884519013664_u64, "timeout_seconds": 20}}
            }
        }}))
    );
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn empty_arrays_are_sent_when_they_mean_clear() {
    let ts = ScriptedTransport::new();
    ts.push_json(200, json!({"success": true}));
    // calling/sip "Reset SIP password": disable and delete the server.
    client(&ts)
        .calling(PHONE)
        .update_settings(&CallingSettings {
            status: Some(FeatureStatus::Disabled),
            sip: Some(SipSettings {
                status: Some(FeatureStatus::Disabled),
                servers: Some(vec![]),
                ..SipSettings::default()
            }),
            call_icons: Some(CallIcons {
                restrict_to_user_countries: Some(vec![]),
            }),
            audio: Some(AudioSettings {
                additional_codecs: Some(vec![]),
            }),
            ..CallingSettings::default()
        })
        .await
        .unwrap();
    assert_eq!(
        ts.last_request().unwrap().json(),
        Some(json!({"calling": {
            "status": "DISABLED",
            "call_icons": {"restrict_to_user_countries": []},
            "sip": {"status": "DISABLED", "servers": []},
            "audio": {"additional_codecs": []}
        }}))
    );
    assert_eq!(ts.remaining(), 0);
}

fn hours(weekly: Vec<WeeklyHours>, holidays: Option<Vec<HolidayHours>>) -> CallingSettings {
    CallingSettings {
        call_hours: Some(CallHours {
            status: FeatureStatus::Enabled,
            timezone_id: "Asia/Singapore".into(),
            weekly_operating_hours: weekly,
            holiday_schedule: holidays,
        }),
        ..CallingSettings::default()
    }
}

fn monday(open: ClockTime, close: ClockTime) -> WeeklyHours {
    WeeklyHours {
        day_of_week: DayOfWeek::Monday,
        open_time: open,
        close_time: close,
    }
}

fn new_year(start: ClockTime, end: ClockTime) -> HolidayHours {
    HolidayHours {
        date: date!(2026 - 01 - 01),
        start_time: start,
        end_time: end,
    }
}

#[tokio::test]
async fn call_hours_limits_are_enforced() {
    let ts = ScriptedTransport::new();
    let calling = client(&ts).calling(PHONE);
    let weekly = "calling.call_hours.weekly_operating_hours";
    let holiday = "calling.call_hours.holiday_schedule";
    let cases: Vec<(CallingSettings, String)> = vec![
        (hours(vec![], None), weekly.into()),
        (
            hours(vec![monday(t(10, 0), t(9, 0))], None),
            format!("{weekly}[0]"),
        ),
        (
            hours(vec![monday(t(9, 0), t(9, 0))], None),
            format!("{weekly}[0]"),
        ),
        (
            hours(
                vec![
                    monday(t(8, 0), t(9, 0)),
                    monday(t(10, 0), t(11, 0)),
                    monday(t(12, 0), t(13, 0)),
                ],
                None,
            ),
            weekly.into(),
        ),
        (
            hours(
                vec![monday(t(8, 0), t(12, 0)), monday(t(11, 0), t(13, 0))],
                None,
            ),
            format!("{weekly}[0]"),
        ),
        (
            hours(
                vec![monday(t(8, 0), t(9, 0))],
                Some(vec![new_year(t(10, 0), t(9, 0))]),
            ),
            format!("{holiday}[0]"),
        ),
        (
            hours(
                vec![monday(t(8, 0), t(9, 0))],
                Some(vec![
                    new_year(t(8, 0), t(12, 0)),
                    new_year(t(11, 0), t(13, 0)),
                ]),
            ),
            format!("{holiday}[0]"),
        ),
        (
            hours(
                vec![monday(t(8, 0), t(9, 0))],
                Some(
                    (1..=21)
                        .map(|d| HolidayHours {
                            date: time::Date::from_calendar_date(2026, time::Month::March, d)
                                .unwrap(),
                            start_time: t(0, 0),
                            end_time: t(1, 0),
                        })
                        .collect(),
                ),
            ),
            holiday.into(),
        ),
    ];
    for (settings, field) in cases {
        let err = calling.update_settings(&settings).await.unwrap_err();
        assert_eq!(validation_field(&err), field);
    }
    assert!(ts.requests().is_empty());

    // Allowed: two touching windows per day, another day's window at the
    // same time, twenty holidays.
    ts.push_json(200, json!({"success": true}));
    let ok = hours(
        vec![
            monday(t(8, 0), t(12, 0)),
            monday(t(12, 0), t(18, 0)),
            WeeklyHours {
                day_of_week: DayOfWeek::Tuesday,
                open_time: t(8, 0),
                close_time: t(12, 0),
            },
        ],
        Some(
            (1..=20)
                .map(|d| HolidayHours {
                    date: time::Date::from_calendar_date(2026, time::Month::March, d).unwrap(),
                    start_time: t(0, 0),
                    end_time: t(1, 0),
                })
                .collect(),
        ),
    );
    calling.update_settings(&ok).await.unwrap();
    assert_eq!(ts.remaining(), 0);
}

#[tokio::test]
async fn sip_limits_are_enforced() {
    let ts = ScriptedTransport::new();
    let calling = client(&ts).calling(PHONE);
    let server = |host: &str| SipServer {
        hostname: host.into(),
        ..SipServer::default()
    };
    let sip = |servers| CallingSettings {
        sip: Some(SipSettings {
            servers: Some(servers),
            ..SipSettings::default()
        }),
        ..CallingSettings::default()
    };
    let err = calling
        .update_settings(&sip(vec![server("a.example.com"), server("b.example.com")]))
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "calling.sip.servers");
    let err = calling
        .update_settings(&sip(vec![server(" ")]))
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "calling.sip.servers[0].hostname");
    for (k, v) in [
        ("k".repeat(129), "v".to_owned()),
        ("k".to_owned(), "v".repeat(129)),
    ] {
        let mut params = BTreeMap::new();
        params.insert(k, v);
        let err = calling
            .update_settings(&sip(vec![SipServer {
                request_uri_user_params: params,
                ..server("sip.example.com")
            }]))
            .await
            .unwrap_err();
        assert_eq!(
            validation_field(&err),
            "calling.sip.servers[0].request_uri_user_params"
        );
    }
    assert!(ts.requests().is_empty());
}

#[tokio::test]
async fn voicemail_requirements_are_enforced() {
    use VoicemailTrigger::{Reject, Timeout};
    let ts = ScriptedTransport::new();
    let calling = client(&ts).calling(PHONE);
    let vm = |triggers: Vec<VoicemailTrigger>, media: Option<&str>, timeout: Option<u8>| {
        CallingSettings {
            voicemail: Some(Voicemail {
                status: FeatureStatus::Enabled,
                triggers,
                audio: Some(VoicemailAudio {
                    default: VoicemailAudioConfig {
                        announcement_media_id: media.map(MediaId::new),
                        timeout_seconds: timeout,
                    },
                }),
            }),
            ..CallingSettings::default()
        }
    };
    for (settings, field) in [
        (vm(vec![], Some("1"), None), "calling.voicemail.triggers"),
        (
            vm(vec![Reject], None, None),
            "calling.voicemail.audio.default.announcement_media_id",
        ),
        (
            vm(vec![Timeout], Some("1"), None),
            "calling.voicemail.audio.default.timeout_seconds",
        ),
        (
            vm(vec![Timeout], Some("1"), Some(31)),
            "calling.voicemail.audio.default.timeout_seconds",
        ),
    ] {
        let err = calling.update_settings(&settings).await.unwrap_err();
        assert_eq!(validation_field(&err), field);
    }
    let no_audio = CallingSettings {
        voicemail: Some(Voicemail {
            status: FeatureStatus::Enabled,
            triggers: vec![Reject],
            audio: None,
        }),
        ..CallingSettings::default()
    };
    let err = calling.update_settings(&no_audio).await.unwrap_err();
    assert_eq!(
        validation_field(&err),
        "calling.voicemail.audio.default.announcement_media_id"
    );
    assert!(ts.requests().is_empty());

    // Disabled voicemail needs nothing else; 0 and 30 s are in range.
    ts.push_json(200, json!({"success": true}));
    ts.push_json(200, json!({"success": true}));
    ts.push_json(200, json!({"success": true}));
    calling
        .update_settings(&CallingSettings {
            voicemail: Some(Voicemail {
                status: FeatureStatus::Disabled,
                triggers: vec![],
                audio: None,
            }),
            ..CallingSettings::default()
        })
        .await
        .unwrap();
    assert_eq!(
        ts.last_request().unwrap().json(),
        Some(json!({"calling": {"voicemail": {"status": "DISABLED"}}}))
    );
    calling
        .update_settings(&vm(vec![Timeout], Some("1"), Some(0)))
        .await
        .unwrap();
    calling
        .update_settings(&vm(vec![Timeout], Some("1"), Some(30)))
        .await
        .unwrap();
    assert_eq!(ts.remaining(), 0);
}

#[test]
fn clock_time_reads_every_documented_spelling() {
    let parsed: Vec<ClockTime> =
        serde_json::from_value(json!(["0400", "04:00", 400, "2359"])).unwrap();
    assert_eq!(parsed, [t(4, 0), t(4, 0), t(4, 0), t(23, 59)]);
    for bad in [
        json!("2400"),
        json!("1260"),
        json!("400"),
        json!("noon"),
        json!(99999),
    ] {
        assert!(
            serde_json::from_value::<ClockTime>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    assert_eq!(serde_json::to_value(t(1, 8)).unwrap(), json!("0108"));
    assert!(ClockTime::new(24, 0).is_err());
    assert!(ClockTime::new(0, 60).is_err());
}

#[test]
fn unknown_enum_values_round_trip() {
    let s: CallingSettings = serde_json::from_value(
        json!({"status": "PAUSED", "call_icon_visibility": "SOMETIMES", "audio": {"additional_codecs": ["G722"]}}),
    )
    .unwrap();
    assert_eq!(s.status, Some(FeatureStatus::Other("PAUSED".into())));
    assert_eq!(
        serde_json::to_value(&s).unwrap(),
        json!({"status": "PAUSED", "call_icon_visibility": "SOMETIMES", "audio": {"additional_codecs": ["G722"]}})
    );
}
