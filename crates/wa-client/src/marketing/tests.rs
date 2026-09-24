//! Request/response tests against the examples in `marketing-messages/*`,
//! the MM API reference, and `support/error-codes`.

use std::time::Duration;

use http::Method;
use pretty_assertions::assert_eq;
use serde_json::json;
use wa_core::error::TransportError;
use wa_core::recipient::Recipient;
use wa_core::testing::ScriptedTransport;
use wa_core::{Error, ErrorKind};

use super::*;
use crate::templates::TemplateMessage;
use crate::{Client, RetryPolicy};

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

/// Retries enabled: a test using it proves a call is *not* replayed when a
/// second scripted response would be needed.
fn retrying_client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy {
            max_retries: 3,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        })
        .build()
        .unwrap()
}

fn doc_phone_response() -> serde_json::Value {
    // "Example — send to phone number".
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": "+16505551234", "wa_id": "16505551234"}],
        "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
    })
}

#[tokio::test]
async fn sends_the_documented_minimal_body() {
    let t = ScriptedTransport::new();
    t.push_json(200, doc_phone_response());
    let sent = client(&t)
        .marketing("106540352242922")
        .send(
            &Recipient::phone("+16505551234"),
            &TemplateMessage::new("seasonal_sale_promo", "en"),
            &MarketingOptions::new(),
        )
        .await
        .unwrap();

    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/106540352242922/marketing_messages");
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505551234",
            "type": "template",
            "template": {"name": "seasonal_sale_promo", "language": {"code": "en"}}
        }))
    );
    assert_eq!(sent.messaging_product, "whatsapp");
    assert_eq!(sent.contacts[0].input, "+16505551234");
    assert_eq!(
        sent.contacts[0]
            .wa_id
            .as_ref()
            .map(wa_core::ids::WaId::as_str),
        Some("16505551234")
    );
    assert_eq!(sent.contacts[0].user_id, None);
    assert_eq!(
        sent.message_id().map(wa_core::ids::MessageId::as_str),
        Some("wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA")
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn sends_every_option_under_its_documented_name() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"messaging_product": "whatsapp", "contacts": [], "messages": [{"id": "wamid.1", "message_status": "held_for_quality_assessment"}]}),
    );
    let sent = client(&t)
        .marketing("P")
        .send(
            &Recipient::phone("+16505551234"),
            &TemplateMessage::new("seasonal_sale_promo", "en"),
            &MarketingOptions::new()
                .strict()
                .message_activity_sharing(false)
                .bid_multiplier(1.5),
        )
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505551234",
            "type": "template",
            "template": {"name": "seasonal_sale_promo", "language": {"code": "en"}},
            "product_policy": "STRICT",
            "message_activity_sharing": false,
            "bid_spec": {"per_message_bid_multiplier": 1.5}
        }))
    );
    assert_eq!(
        sent.messages[0].message_status,
        Some(MessageStatus::HeldForQualityAssessment)
    );

    t.push_json(200, doc_phone_response());
    client(&t)
        .marketing("P")
        .send(
            &Recipient::phone("1"),
            &TemplateMessage::new("t", "en"),
            &MarketingOptions::new().product_policy(ProductPolicy::CloudApiFallback),
        )
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().json().unwrap()["product_policy"],
        json!("CLOUD_API_FALLBACK")
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn sends_by_bsuid_and_parses_the_documented_response() {
    let t = ScriptedTransport::new();
    // "Example — send to BSUID".
    t.push_json(
        200,
        json!({
            "messaging_product": "whatsapp",
            "contacts": [{"input": "US.13491208655302741918", "user_id": "US.13491208655302741918"}],
            "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
        }),
    );
    let sent = client(&t)
        .marketing("P")
        .send(
            &Recipient::user("US.13491208655302741918"),
            &TemplateMessage::new("t", "en_US"),
            &MarketingOptions::default(),
        )
        .await
        .unwrap();
    let body = t.last_request().unwrap().json().unwrap();
    assert_eq!(body["recipient"], json!("US.13491208655302741918"));
    assert_eq!(body.get("to"), None);
    assert_eq!(sent.contacts[0].wa_id, None);
    assert_eq!(
        sent.contacts[0]
            .user_id
            .as_ref()
            .map(wa_core::ids::UserId::as_str),
        Some("US.13491208655302741918")
    );

    // The guide's "Sending to a BSUID" request: both identifiers, `to` wins.
    t.push_json(200, doc_phone_response());
    client(&t)
        .marketing("P")
        .send(
            &Recipient::PhoneAndUser {
                phone: "+16505551234".into(),
                user: "US.13491208655302741918".into(),
            },
            &TemplateMessage::new("t", "en_US"),
            &MarketingOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505551234",
            "recipient": "US.13491208655302741918",
            "type": "template",
            "template": {"name": "t", "language": {"code": "en_US"}}
        }))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn local_validation_happens_before_any_request() {
    let t = ScriptedTransport::new();
    let m = client(&t).marketing("P");
    let to = Recipient::phone("1");
    let err = m
        .send(
            &to,
            &TemplateMessage::new(" ", "en"),
            &MarketingOptions::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(ref v) if v.field == "template.name"));
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let err = m
            .send(
                &to,
                &TemplateMessage::new("t", "en"),
                &MarketingOptions::new().bid_multiplier(bad),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Validation(ref v) if v.field == "bid_spec.per_message_bid_multiplier"),
            "{bad}"
        );
    }
    assert!(t.requests().is_empty());
}

/// Like `Messages::send`: the response names the recipient, so an
/// unreadable one is reported without its body or serde's quoted values.
#[tokio::test]
async fn an_unreadable_send_response_never_quotes_the_recipient() {
    let t = ScriptedTransport::new();
    t.push_bytes(
        200,
        "application/json",
        r#"{"contacts":["+16505551234"],"messages":[{"id":"wamid.1"}]}"#,
    );
    let err = client(&t)
        .marketing("P")
        .send(
            &Recipient::phone("+16505551234"),
            &TemplateMessage::new("t", "en"),
            &MarketingOptions::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err}");
    assert!(!format!("{err} {err:?}").contains("6505551234"), "{err:?}");
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn sends_are_never_replayed_after_a_timeout() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    let err = retrying_client(&t)
        .marketing("P")
        .send(
            &Recipient::phone("1"),
            &TemplateMessage::new("t", "en"),
            &MarketingOptions::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(t.requests().len(), 1);
}

/// The MM API error table in `support/error-codes`, plus the send-page
/// errors, mapped through the existing `ErrorKind` classification.
#[tokio::test]
async fn mm_api_errors_map_to_their_kinds() {
    let cases = [
        (
            400,
            100,
            "(#100) Invalid parameter",
            "Message must be a template message.",
            ErrorKind::InvalidParameter,
        ),
        (
            400,
            131055,
            "(#131055) Method not allowed",
            "Only marketing template messages are supported",
            ErrorKind::MarketingNotAllowed,
        ),
        (
            400,
            134100,
            "(#134100) Only marketing messages supported",
            "You're only able to send marketing messages on this API.",
            ErrorKind::MarketingNotAllowed,
        ),
        (
            400,
            131063,
            "(#131063) Marketing templates disabled for Cloud API",
            "Your template is categorized as Marketing, but marketing templates are currently disabled for your Cloud API configuration.",
            ErrorKind::MarketingNotAllowed,
        ),
        (
            400,
            134101,
            "(#134101) Your template is still syncing",
            "When you send a message from a template, the template syncing process can take up to 10 minutes to complete. Wait a few minutes, and then try sending your message again.",
            ErrorKind::TemplateSyncing,
        ),
        (
            500,
            134102,
            "(#134102) Template unavailable for use",
            "Please check your eligibility status to ensure you are onboarded or contact Meta's customer support.",
            ErrorKind::TemplateUnavailable,
        ),
        (
            400,
            132018,
            "(#132018) Template validation error",
            "There's an issue with the parameters in your template.",
            ErrorKind::TemplateParameterMismatch,
        ),
    ];
    for (status, code, message, details, kind) in cases {
        let t = ScriptedTransport::new();
        t.push_json(
            status,
            json!({"error": {
                "message": message,
                "type": "OAuthException",
                "code": code,
                "error_data": {"messaging_product": "whatsapp", "details": details},
                "fbtrace_id": "Ak6nxJSySLEJz32Ps-QiZ1t"
            }}),
        );
        let err = retrying_client(&t)
            .marketing("P")
            .send(
                &Recipient::phone("1"),
                &TemplateMessage::new("t", "en"),
                &MarketingOptions::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), kind, "code {code}");
        let graph = err.graph().unwrap();
        assert_eq!(graph.http_status, Some(status));
        assert_eq!(graph.details(), Some(details));
        assert_eq!(t.requests().len(), 1, "code {code} is not replayed");
    }
}

#[tokio::test]
async fn waba_onboarding_status_parses_the_documented_example() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"marketing_messages_onboarding_status": "ELIGIBLE", "id": "25002526842541"}),
    );
    t.push_json(
        200,
        json!({"marketing_messages_onboarding_status": "PENDING_REVIEW", "id": "1"}),
    );
    t.push_json(200, json!({"id": "1"}));
    let account = client(&t).marketing_account("25002526842541");
    assert_eq!(
        account.onboarding_status().await.unwrap(),
        Some(OnboardingStatus::Eligible)
    );
    let req = &t.requests()[0];
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), "/v25.0/25002526842541");
    assert_eq!(
        req.query("fields").as_deref(),
        Some("marketing_messages_onboarding_status")
    );
    assert_eq!(
        account.onboarding_status().await.unwrap(),
        Some(OnboardingStatus::Unknown("PENDING_REVIEW".into()))
    );
    assert_eq!(account.onboarding_status().await.unwrap(), None);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn owner_business_info_parses_the_documented_example() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"owner_business_info": {
            "name": "WhatsApp PaidSend Testing",
            "id": "<BM_ID>",
            "marketing_messages_onboarding_status": {"status": "REQUEST_SENT", "time": "2025-08-13"}
        }}),
    );
    let info = client(&t)
        .marketing_account("69843579834234")
        .owner_business_info()
        .await
        .unwrap()
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.path(), "/v25.0/69843579834234");
    assert_eq!(req.query("fields").as_deref(), Some("owner_business_info"));
    assert_eq!(info.name.as_deref(), Some("WhatsApp PaidSend Testing"));
    let status = info.marketing_messages_onboarding_status.unwrap();
    assert_eq!(status.status, TermsStatus::RequestSent);
    assert_eq!(status.time.as_deref(), Some("2025-08-13"));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn cloud_api_marketing_switch_round_trips() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"id": "102290129340398"}));
    t.push_json(
        200,
        json!({"disable_marketing_messages_on_cloud_api": true, "id": "102290129340398"}),
    );
    let account = client(&t).marketing_account("102290129340398");
    account
        .set_cloud_api_marketing_disabled(true)
        .await
        .unwrap();
    let set = &t.requests()[0];
    assert_eq!(set.method, Method::POST);
    assert_eq!(set.path(), "/v25.0/102290129340398");
    assert_eq!(
        set.json(),
        Some(json!({"disable_marketing_messages_on_cloud_api": true}))
    );
    assert_eq!(
        account.cloud_api_marketing_disabled().await.unwrap(),
        Some(true)
    );
    let get = t.last_request().unwrap();
    assert_eq!(get.method, Method::GET);
    assert_eq!(
        get.query("fields").as_deref(),
        Some("disable_marketing_messages_on_cloud_api")
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn cloud_api_marketing_switch_is_replayed_and_rejects_success_false() {
    // Setting the same value twice is harmless, so a timeout is retried.
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    t.push_json(200, json!({"id": "102290129340398"}));
    retrying_client(&t)
        .marketing_account("102290129340398")
        .set_cloud_api_marketing_disabled(false)
        .await
        .unwrap();
    assert_eq!(t.requests().len(), 2);
    assert_eq!(
        t.last_request().unwrap().json(),
        Some(json!({"disable_marketing_messages_on_cloud_api": false}))
    );
    assert_eq!(t.remaining(), 0);

    // A Graph node update may answer {"success": ...}; false is not "done".
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    t.push_json(200, json!({"success": false}));
    let account = client(&t).marketing_account("W");
    account
        .set_cloud_api_marketing_disabled(true)
        .await
        .unwrap();
    let err = account
        .set_cloud_api_marketing_disabled(true)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Http { status: 200, .. }), "{err}");
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn business_onboarding_status_parses_the_documented_example() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"marketing_messages_onboarding_status": {"status": "TERM_OF_SERVICE_SIGNED", "time": "2025-10-07"}}),
    );
    let status = client(&t)
        .marketing_business("52002526842524351")
        .onboarding_status()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.status, TermsStatus::TermsSigned);
    let req = t.last_request().unwrap();
    assert_eq!(req.path(), "/v25.0/52002526842524351");
    assert_eq!(
        req.query("fields").as_deref(),
        Some("marketing_messages_onboarding_status")
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn lists_eligible_client_wabas_with_the_documented_filter() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"data": [{
            "id": "46302397361990",
            "name": "San Andreas Roofing",
            "timezone_id": "1",
            "message_template_namespace": "93d3e793_8a4f_49c4_b903_fd72aac80f71"
        }]}),
    );
    let page = client(&t)
        .marketing_business("19502398688333")
        .client_wabas_with_status(&[OnboardingStatus::Eligible], None)
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(
        req.path(),
        "/v25.0/19502398688333/client_whatsapp_business_accounts"
    );
    let filtering: serde_json::Value =
        serde_json::from_str(&req.query("filtering").unwrap()).unwrap();
    assert_eq!(
        filtering,
        json!([{"field": "marketing_messages_onboarding_status", "operator": "IN", "value": ["ELIGIBLE"]}])
    );
    assert_eq!(page.data[0].id.as_str(), "46302397361990");
    assert_eq!(page.data[0].name.as_deref(), Some("San Andreas Roofing"));
    assert_eq!(req.query("after"), None);

    t.push_json(200, json!({"data": []}));
    client(&t)
        .marketing_business("19502398688333")
        .client_wabas_with_status(
            &[OnboardingStatus::Eligible, OnboardingStatus::Onboarded],
            Some("QVFI..."),
        )
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.query("after").as_deref(), Some("QVFI..."));
    let filtering: serde_json::Value =
        serde_json::from_str(&req.query("filtering").unwrap()).unwrap();
    assert_eq!(filtering[0]["value"], json!(["ELIGIBLE", "ONBOARDED"]));
    assert_eq!(t.remaining(), 0);
}

/// Conventions review #7: the one list here without a `…_stream()`.
#[tokio::test]
async fn client_wabas_with_status_stream_follows_cursors_with_the_filter() {
    use futures::StreamExt;
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({
            "data": [{"id": "46302397361990", "name": "San Andreas Roofing"}],
            "paging": {"cursors": {"after": "QVFI1"}, "next": "https://graph.facebook.com/x"}
        }),
    );
    t.push_json(200, json!({"data": [{"id": "46302397361991"}]}));
    let ids: Vec<String> = client(&t)
        .marketing_business("19502398688333")
        .client_wabas_with_status_stream(&[OnboardingStatus::Eligible])
        .map(|w| w.unwrap().id.as_str().to_owned())
        .collect()
        .await;
    assert_eq!(ids, ["46302397361990", "46302397361991"]);
    let reqs = t.requests();
    assert_eq!(reqs.len(), 2);
    for (req, after) in reqs.iter().zip([None, Some("QVFI1")]) {
        assert_eq!(req.method, Method::GET);
        assert_eq!(
            req.path(),
            "/v25.0/19502398688333/client_whatsapp_business_accounts"
        );
        assert_eq!(req.bearer(), Some("TOKEN"));
        let filtering: serde_json::Value =
            serde_json::from_str(&req.query("filtering").unwrap()).unwrap();
        assert_eq!(
            filtering,
            json!([{"field": "marketing_messages_onboarding_status", "operator": "IN", "value": ["ELIGIBLE"]}])
        );
        assert_eq!(req.query("after").as_deref(), after);
    }
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn intent_api_posts_and_returns_the_request_id() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"request_id": "893436979557695"}));
    t.push_json(200, json!({"request_id": "893436979557696"}));
    let business = client(&t).marketing_business("END_BUSINESS_ID");
    let r = business.request_onboarding(None).await.unwrap();
    assert_eq!(r.request_id, "893436979557695");
    let req = &t.requests()[0];
    assert_eq!(req.method, Method::POST);
    assert_eq!(
        req.path(),
        "/v25.0/END_BUSINESS_ID/onboard_partners_to_mm_lite"
    );
    assert_eq!(req.url.query(), None);
    assert_eq!(req.bearer(), Some("TOKEN"));

    business
        .request_onboarding(Some("MULTI-PARTNER_SOLUTION_ID"))
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().query("solution_id").as_deref(),
        Some("MULTI-PARTNER_SOLUTION_ID")
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn intent_api_is_not_replayed_after_a_timeout() {
    // The reference says a pending request's id is returned again, the guide
    // says a second call fails as already sent. With the docs disagreeing, a
    // lost response is surfaced rather than replayed.
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    let err = retrying_client(&t)
        .marketing_business("B")
        .request_onboarding(None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(t.requests().len(), 1);
}

#[tokio::test]
async fn intent_api_duplicate_request_is_classified() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        json!({"error": {
            "message": "(#1752041) Duplicate Request",
            "type": "OAuthException",
            "code": 1752041,
            "error_data": {"details": "Duplicate Request is thrown when a client has already been invited to onboard by any partner."}
        }}),
    );
    let err = client(&t)
        .marketing_business("B")
        .request_onboarding(None)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::DuplicateOnboarding);
    assert!(!err.is_retryable());
    assert_eq!(t.remaining(), 0);
}
