//! Request bodies are compared with the example bodies on Meta's pages
//! (named above each test). Where a page only shows placeholders, the
//! sample values from its parameter table are substituted.

use std::time::Duration;

use http::Method;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use wa_core::error::TransportError;
use wa_core::ids::{MessageId, UserId};
use wa_core::recipient::Recipient;
use wa_core::testing::ScriptedTransport;
use wa_core::{Error, ErrorKind};

use super::*;
use crate::templates::TemplateMessage;
use crate::{Client, RetryPolicy};

const PHONE: &str = "+16505551234";
const BSUID: &str = "US.13491208655302741918";

fn phone() -> Recipient {
    Recipient::phone(PHONE)
}

fn client_with(t: &ScriptedTransport, retry: RetryPolicy) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(retry)
        .build()
        .unwrap()
}

fn client(t: &ScriptedTransport) -> Client {
    client_with(t, RetryPolicy::NONE)
}

/// Validate, serialize, compare.
///
/// Compares the bytes `send` would put on the wire, not `to_value`: a
/// `Value` silently keeps only the last of two equal keys, which would hide
/// a flattened field colliding with the envelope.
#[track_caller]
fn assert_wire(msg: &OutboundMessage, expected: &Value) {
    if let Err(e) = msg.validate() {
        panic!("documented example failed validation: {e}");
    }
    let wire = serde_json::to_vec(msg).unwrap();
    assert_no_duplicate_keys(&wire);
    assert_eq!(&serde_json::from_slice::<Value>(&wire).unwrap(), expected);
}

/// Re-serializing parsed JSON drops duplicate keys (and nothing else, for
/// compact output), so a shorter result means the input had some.
#[track_caller]
fn assert_no_duplicate_keys(wire: &[u8]) {
    let parsed: Value = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        serde_json::to_vec(&parsed).unwrap().len(),
        wire.len(),
        "duplicate JSON keys in {}",
        String::from_utf8_lossy(wire)
    );
}

/// The `field` of the validation error `msg` produces.
#[track_caller]
fn invalid(msg: &OutboundMessage) -> String {
    msg.validate()
        .expect_err("expected a validation error")
        .field
}

fn text_response() -> Value {
    // messages/text-messages, "Example response".
    json!({
      "messaging_product": "whatsapp",
      "contacts": [{"input": "+16505551234", "wa_id": "16505551234"}],
      "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
    })
}

// ─── Sending end to end ──────────────────────────────────────────────────

#[tokio::test]
async fn send_posts_the_body_to_the_messages_edge_with_bearer_auth() {
    let t = ScriptedTransport::new();
    t.push_json(200, text_response());
    let resp = client(&t)
        .messages("106540352242922")
        .send(&OutboundMessage::text_with_preview(
            phone(),
            "As requested, here's the link to our latest product: https://www.meta.com/quest/quest-3/",
        ))
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/106540352242922/messages");
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(req.url.query(), None);
    // messages/text-messages, "Example request".
    assert_eq!(
        req.json().unwrap(),
        json!({
          "messaging_product": "whatsapp",
          "recipient_type": "individual",
          "to": "+16505551234",
          "type": "text",
          "text": {
            "preview_url": true,
            "body": "As requested, here's the link to our latest product: https://www.meta.com/quest/quest-3/"
          }
        })
    );
    assert_eq!(resp.messaging_product, "whatsapp");
    assert_eq!(
        resp.message_id(),
        Some(&MessageId::new(
            "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"
        ))
    );
    assert_eq!(resp.contacts[0].input, PHONE);
    assert_eq!(
        resp.contacts[0].wa_id.as_ref().unwrap().as_str(),
        "16505551234"
    );
    assert_eq!(resp.contacts[0].user_id, None);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn invalid_messages_never_reach_the_transport() {
    let t = ScriptedTransport::new();
    let err = client(&t)
        .messages("1")
        .send(&OutboundMessage::text(phone(), ""))
        .await
        .unwrap_err();
    assert!(matches!(&err, Error::Validation(v) if v.field == "text.body"));
    assert!(t.requests().is_empty());
}

#[tokio::test]
async fn a_phone_number_id_cannot_redirect_the_request() {
    // An id smuggling `/`, `?` or `#` stays one percent-encoded segment:
    // `123/subscribed_apps` must not become `POST /123/subscribed_apps/…`.
    let t = ScriptedTransport::new();
    for id in ["123/subscribed_apps", "123?fields=x#y"] {
        t.push_json(200, text_response());
        t.push_json(200, json!({"success": true}));
        let m = client(&t).messages(id);
        m.send(&OutboundMessage::text(phone(), "hi")).await.unwrap();
        m.mark_read(&MessageId::new("wamid.A")).await.unwrap();
    }
    let paths: Vec<_> = t.requests().iter().map(|r| r.url.clone()).collect();
    for url in &paths[..2] {
        assert_eq!(url.path(), "/v25.0/123%2Fsubscribed_apps/messages");
        assert_eq!(url.query(), None);
    }
    for url in &paths[2..] {
        assert_eq!(url.path(), "/v25.0/123%3Ffields=x%23y/messages");
        assert_eq!((url.query(), url.fragment()), (None, None));
    }
    assert_eq!(t.remaining(), 0);

    // Segments URL normalization would drop or pop never leave the client.
    let t = ScriptedTransport::new();
    let c = client(&t);
    for bad in ["", ".", ".."] {
        let err = c
            .messages(bad)
            .send(&OutboundMessage::text(phone(), "hi"))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "path"),
            "{bad:?}: {err}"
        );
        let err = c
            .messages(bad)
            .mark_read_with_typing_indicator(&MessageId::new("wamid.A"))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "path"),
            "{bad:?}: {err}"
        );
    }
    assert!(t.requests().is_empty());
}

#[tokio::test]
async fn closed_service_window_maps_to_its_kind() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        json!({"error": {
            "message": "(#131047) Re-engagement message",
            "type": "OAuthException",
            "code": 131047,
            "error_data": {"messaging_product": "whatsapp", "details": "Message failed to send because more than 24 hours have passed since the customer last replied to this number."},
            "fbtrace_id": "A1b2"
        }}),
    );
    let err = client(&t)
        .messages("1")
        .send(&OutboundMessage::text(phone(), "hi"))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::CustomerServiceWindowClosed);
    assert_eq!(err.graph().unwrap().http_status, Some(400));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn a_send_that_times_out_is_not_replayed_even_with_retries_enabled() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    // Would be consumed by a (wrong) retry.
    t.push_json(200, text_response());
    let err = client_with(&t, RetryPolicy::default())
        .messages("1")
        .send(&OutboundMessage::text(phone(), "Your code is on its way"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(
        t.requests().len(),
        1,
        "a timed-out send must not be replayed"
    );
    assert_eq!(t.remaining(), 1);
}

#[tokio::test]
async fn a_throttled_send_is_replayed() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        json!({"error": {"message": "(#130429) Rate limit hit", "code": 130429}}),
    );
    t.push_json(200, text_response());
    let retry = RetryPolicy {
        max_retries: 1,
        base_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
    };
    client_with(&t, retry)
        .messages("1")
        .send(&OutboundMessage::text(phone(), "hi"))
        .await
        .unwrap();
    assert_eq!(t.requests().len(), 2);
    assert_eq!(t.remaining(), 0);
}

// ─── Read receipts, typing, react ────────────────────────────────────────

#[tokio::test]
async fn mark_read_matches_the_docs_and_is_idempotent() {
    let t = ScriptedTransport::new();
    t.push_bytes(502, "text/html", "<html>bad gateway</html>");
    t.push_json(200, json!({"success": true}));
    let retry = RetryPolicy {
        max_retries: 1,
        base_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
    };
    client_with(&t, retry)
        .messages("106540352242922")
        .mark_read(&MessageId::new(
            "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBJDQjZCMzlEQUE4OTJBMTE4RTUA",
        ))
        .await
        .unwrap();
    let reqs = t.requests();
    assert_eq!(reqs.len(), 2, "mark_read is replayed after a 502");
    assert_eq!(reqs[1].path(), "/v25.0/106540352242922/messages");
    // messages/mark-message-as-read, "Example request".
    assert_eq!(
        reqs[1].json().unwrap(),
        json!({
          "messaging_product": "whatsapp",
          "status": "read",
          "message_id": "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBJDQjZCMzlEQUE4OTJBMTE4RTUA"
        })
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn typing_indicator_matches_the_docs_and_is_not_replayed() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    let m = client(&t).messages("106540352242922");
    let id = MessageId::new("wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBJDQjZCMzlEQUE4OTJBMTE4RTUA");
    m.mark_read_with_typing_indicator(&id).await.unwrap();
    // typing-indicators, "Example request".
    assert_eq!(
        t.last_request().unwrap().json().unwrap(),
        json!({
          "messaging_product": "whatsapp",
          "status": "read",
          "message_id": "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBJDQjZCMzlEQUE4OTJBMTE4RTUA",
          "typing_indicator": {"type": "text"}
        })
    );

    let t = ScriptedTransport::new();
    t.push_bytes(502, "text/html", "<html>bad gateway</html>");
    t.push_json(200, json!({"success": true}));
    let retry = RetryPolicy {
        max_retries: 1,
        base_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
    };
    let err = client_with(&t, retry)
        .messages("1")
        .mark_read_with_typing_indicator(&id)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Http { status: 502, .. }));
    assert_eq!(t.requests().len(), 1);

    assert!(matches!(
        client(&t)
            .messages("1")
            .mark_read(&MessageId::new(""))
            .await,
        Err(Error::Validation(_))
    ));

    // A 200 that says `success: false` is not a success.
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": false}));
    t.push_json(200, json!({"success": false}));
    let m = client(&t).messages("1");
    assert!(m.mark_read(&id).await.is_err());
    assert!(m.mark_read_with_typing_indicator(&id).await.is_err());
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn react_sends_a_reaction() {
    let t = ScriptedTransport::new();
    t.push_json(200, text_response());
    client(&t)
        .messages("106540352242922")
        .react(
            phone(),
            "wamid.HBgLMTY0NjcwNDM1OTUVAgASGBQzQUZCMTY0MDc2MUYwNzBDNTY5MAA=",
            "\u{1F600}",
        )
        .await
        .unwrap();
    // messages/reaction-messages, "Example request" ("😀" is 😀).
    assert_eq!(
        t.last_request().unwrap().json().unwrap(),
        json!({
          "messaging_product": "whatsapp",
          "recipient_type": "individual",
          "to": "+16505551234",
          "type": "reaction",
          "reaction": {
            "message_id": "wamid.HBgLMTY0NjcwNDM1OTUVAgASGBQzQUZCMTY0MDc2MUYwNzBDNTY5MAA=",
            "emoji": "\u{1F600}"
          }
        })
    );
    assert_eq!(t.remaining(), 0);
}

// ─── Responses ───────────────────────────────────────────────────────────

#[test]
fn parses_bsuid_and_pacing_responses() {
    // business-scoped-user-ids, "Send message response", BSUID only.
    let r: SendResponse = serde_json::from_value(json!({
      "messaging_product": "whatsapp",
      "contacts": [{"input": "US.13491208655302741918", "user_id": "US.13491208655302741918"}],
      "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
    }))
    .unwrap();
    assert_eq!(r.contacts[0].wa_id, None);
    assert_eq!(r.contacts[0].user_id, Some(UserId::new(BSUID)));

    // Same page, Marketing Messages response with pacing status.
    let r: SendResponse = serde_json::from_value(json!({
      "messaging_product": "whatsapp",
      "contacts": [{"input": "US.13491208655302741918", "user_id": "US.13491208655302741918"}],
      "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA", "message_status": "accepted"}]
    }))
    .unwrap();
    assert_eq!(r.messages[0].message_status, Some(MessageStatus::Accepted));

    // calling/user-call-permissions: user_id and parent_user_id.
    let r: SendResponse = serde_json::from_value(json!({
      "messaging_product": "whatsapp",
      "contacts": [{"input": "+1-408-555-1234", "wa_id": "14085551234", "user_id": "US.1", "parent_user_id": "US.ENT.11815799212886844830"}],
      "messages": [{"id": "wamid.gBGGFlaCmZ9plHrf2Mh-o"}]
    }))
    .unwrap();
    assert_eq!(
        r.contacts[0].parent_user_id,
        Some(UserId::new("US.ENT.11815799212886844830"))
    );

    // Values Meta may add, and the other documented pacing values.
    let r: SendResponse = serde_json::from_value(json!({
      "messages": [
        {"id": "a", "message_status": "held_for_quality_assessment", "group_id": "Y2FwaV9ncm91cDox"},
        {"id": "b", "message_status": "paused"},
        {"id": "c", "message_status": "brand_new_status", "new_field": 1}
      ]
    }))
    .unwrap();
    assert_eq!(
        r.messages[0].message_status,
        Some(MessageStatus::HeldForQualityAssessment)
    );
    assert_eq!(
        r.messages[0].group_id.as_ref().unwrap().as_str(),
        "Y2FwaV9ncm91cDox"
    );
    assert_eq!(r.messages[1].message_status, Some(MessageStatus::Paused));
    assert_eq!(r.messages[2].message_status, Some(MessageStatus::Unknown));
}

#[test]
fn parses_the_legacy_flow_sample_response() {
    // flows/guides/sendingaflow "Sample Response": capitalised `Input`, no
    // messaging_product, an extra `meta` block. Must still parse.
    let r: SendResponse = serde_json::from_value(json!({
      "contacts": [{"Input": "+447385946746", "wa_id": "47385946746"}],
      "messages": [{"id": "gHTRETHRTHTRTH-av4Y"}],
      "meta": {"api_status": "stable", "version": "2.44.0.27"}
    }))
    .unwrap();
    assert_eq!(r.message_id().unwrap().as_str(), "gHTRETHRTHTRTH-av4Y");
    assert_eq!(r.contacts[0].input, "");

    // Every top-level field is optional in the reference schema.
    let r: SendResponse = serde_json::from_value(json!({})).unwrap();
    assert_eq!(r.message_id(), None);
    assert!(r.contacts.is_empty() && r.messaging_product.is_empty());
}

// ─── Every message type against its documented body ──────────────────────

#[test]
fn media_messages_match_the_docs() {
    // messages/image-messages.
    assert_wire(
        &OutboundMessage::new(
            phone(),
            Image::new(MediaSource::id("1479537139650973")).caption("The best succulent ever?"),
        ),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": PHONE, "type": "image",
                "image": {"id": "1479537139650973", "caption": "The best succulent ever?"}}),
    );
    assert_wire(
        &OutboundMessage::image_link(
            phone(),
            "https://www.luckyshrub.com/assets/succulents/aloe.png",
        ),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": PHONE, "type": "image",
                "image": {"link": "https://www.luckyshrub.com/assets/succulents/aloe.png"}}),
    );
    // messages/audio-messages.
    assert_wire(
        &OutboundMessage::new(
            phone(),
            Audio::new(MediaSource::id("1013859600285441")).voice(),
        ),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": PHONE, "type": "audio",
                "audio": {"id": "1013859600285441", "voice": true}}),
    );
    // messages/video-messages.
    assert_wire(
        &OutboundMessage::new(
            phone(),
            Video::new(MediaSource::id("1166846181421424")).caption("A succulent eclipse!"),
        ),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": PHONE, "type": "video",
                "video": {"id": "1166846181421424", "caption": "A succulent eclipse!"}}),
    );
    // messages/document-messages.
    assert_wire(
        &OutboundMessage::new(
            phone(),
            Document::new(MediaSource::id("1376223850470843"))
                .filename("order_abc123.pdf")
                .caption("Your order confirmation (PDF)"),
        ),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": PHONE, "type": "document",
                "document": {"id": "1376223850470843", "filename": "order_abc123.pdf", "caption": "Your order confirmation (PDF)"}}),
    );
    assert_eq!(
        OutboundMessage::document_id(phone(), "1376223850470843", "order_abc123.pdf").content,
        MessageContent::Document(
            Document::new(MediaSource::id("1376223850470843")).filename("order_abc123.pdf")
        )
    );
    // messages/sticker-messages.
    assert_wire(
        &OutboundMessage::sticker_id(phone(), "798882015472548"),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": PHONE, "type": "sticker",
                "sticker": {"id": "798882015472548"}}),
    );
    assert_eq!(
        serde_json::to_value(OutboundMessage::image_id(phone(), "1").content).unwrap(),
        json!({"type": "image", "image": {"id": "1"}})
    );
}

#[test]
fn location_and_reaction_match_the_docs() {
    // messages/location-messages: coordinates are strings on the wire.
    assert_wire(
        &OutboundMessage::new(
            phone(),
            Location::new(37.44216251868683, -122.16153582049394)
                .name("Philz Coffee")
                .address("101 Forest Ave, Palo Alto, CA 94301"),
        ),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": PHONE, "type": "location",
                "location": {"latitude": "37.44216251868683", "longitude": "-122.16153582049394",
                             "name": "Philz Coffee", "address": "101 Forest Ave, Palo Alto, CA 94301"}}),
    );
    // Removing a reaction: empty emoji passes validation.
    assert_wire(
        &OutboundMessage::reaction(phone(), "wamid.X", ""),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": PHONE, "type": "reaction",
                "reaction": {"message_id": "wamid.X", "emoji": ""}}),
    );
}

#[test]
fn contacts_match_the_docs() {
    // messages/contacts-messages, "Example request". The example omits
    // `recipient_type`, which the reference marks required; we always send it.
    let card = Contact {
        addresses: vec![
            ContactAddress {
                street: Some("1 Lucky Shrub Way".into()),
                city: Some("Menlo Park".into()),
                state: Some("CA".into()),
                zip: Some("94025".into()),
                country: Some("United States".into()),
                country_code: Some("US".into()),
                kind: Some("Office".into()),
            },
            ContactAddress {
                street: Some("1 Hacker Way".into()),
                city: Some("Menlo Park".into()),
                state: Some("CA".into()),
                zip: Some("94025".into()),
                country: Some("United States".into()),
                country_code: Some("US".into()),
                kind: Some("Pop-Up".into()),
            },
        ],
        birthday: Some("1999-01-23".into()),
        emails: vec![
            ContactEmail::new("bjohnson@luckyshrub.com", "Work"),
            ContactEmail::new("bjohnson@luckyshrubplants.com", "Work (old)"),
        ],
        name: ContactName {
            formatted_name: "Barbara J. Johnson".into(),
            first_name: Some("Barbara".into()),
            last_name: Some("Johnson".into()),
            middle_name: Some("Joana".into()),
            suffix: Some("Esq.".into()),
            prefix: Some("Dr.".into()),
        },
        org: Some(ContactOrg {
            company: Some("Lucky Shrub".into()),
            department: Some("Legal".into()),
            title: Some("Lead Counsel".into()),
        }),
        phones: vec![
            ContactPhone::new("+16505559999", "Landline"),
            ContactPhone::new("+19175559999", "Mobile").wa_id("19175559999"),
        ],
        urls: vec![
            ContactUrl::new("https://www.luckyshrub.com", "Company"),
            ContactUrl::new("https://www.facebook.com/luckyshrubplants", "Company (FB)"),
        ],
    };
    assert_wire(
        &OutboundMessage::contacts(phone(), [card]),
        &json!({
          "messaging_product": "whatsapp",
          "recipient_type": "individual",
          "to": "+16505551234",
          "type": "contacts",
          "contacts": [{
            "addresses": [
              {"street": "1 Lucky Shrub Way", "city": "Menlo Park", "state": "CA", "zip": "94025",
               "country": "United States", "country_code": "US", "type": "Office"},
              {"street": "1 Hacker Way", "city": "Menlo Park", "state": "CA", "zip": "94025",
               "country": "United States", "country_code": "US", "type": "Pop-Up"}
            ],
            "birthday": "1999-01-23",
            "emails": [
              {"email": "bjohnson@luckyshrub.com", "type": "Work"},
              {"email": "bjohnson@luckyshrubplants.com", "type": "Work (old)"}
            ],
            "name": {"formatted_name": "Barbara J. Johnson", "first_name": "Barbara", "last_name": "Johnson",
                     "middle_name": "Joana", "suffix": "Esq.", "prefix": "Dr."},
            "org": {"company": "Lucky Shrub", "department": "Legal", "title": "Lead Counsel"},
            "phones": [
              {"phone": "+16505559999", "type": "Landline"},
              {"phone": "+19175559999", "type": "Mobile", "wa_id": "19175559999"}
            ],
            "urls": [
              {"url": "https://www.luckyshrub.com", "type": "Company"},
              {"url": "https://www.facebook.com/luckyshrubplants", "type": "Company (FB)"}
            ]
          }]
        }),
    );
}

#[test]
fn reply_buttons_match_the_docs() {
    // messages/interactive-reply-buttons-messages, "Example request".
    let msg = OutboundMessage::new(
        phone(),
        ReplyButtons::new(
            "Hi Pablo! Your gardening workshop is scheduled for 9am tomorrow. Use the buttons if you need to reschedule. Thank you!",
            [ReplyButton::new("change-button", "Change"), ReplyButton::new("cancel-button", "Cancel")],
        )
        .header(Header::image_id("2762702990552401"))
        .footer("Lucky Shrub: Your gateway to succulents!™"),
    );
    assert_wire(
        &msg,
        &json!({
          "messaging_product": "whatsapp",
          "recipient_type": "individual",
          "to": "+16505551234",
          "type": "interactive",
          "interactive": {
            "type": "button",
            "header": {"type": "image", "image": {"id": "2762702990552401"}},
            "body": {"text": "Hi Pablo! Your gardening workshop is scheduled for 9am tomorrow. Use the buttons if you need to reschedule. Thank you!"},
            "footer": {"text": "Lucky Shrub: Your gateway to succulents!™"},
            "action": {"buttons": [
              {"type": "reply", "reply": {"id": "change-button", "title": "Change"}},
              {"type": "reply", "reply": {"id": "cancel-button", "title": "Cancel"}}
            ]}
          }
        }),
    );
    // Header shapes from the parameter table and direct-send/media-headers.
    let header = |h: Header| serde_json::to_value(h).unwrap();
    assert_eq!(
        header(Header::text("Workshop Details")),
        json!({"type": "text", "text": "Workshop Details"})
    );
    assert_eq!(
        header(Header::image_link(
            "https://www.luckyshrub.com/media/workshop-banner.png"
        )),
        json!({"type": "image", "image": {"link": "https://www.luckyshrub.com/media/workshop-banner.png"}})
    );
    assert_eq!(
        header(Header::video_id("1")),
        json!({"type": "video", "video": {"id": "1"}})
    );
    assert_eq!(
        header(Header::Document {
            source: MediaSource::id("1"),
            filename: Some("a.pdf".into())
        }),
        json!({"type": "document", "document": {"id": "1", "filename": "a.pdf"}})
    );
}

#[test]
fn list_matches_the_docs() {
    // messages/interactive-list-messages, "Example request".
    let msg = OutboundMessage::new(
        phone(),
        ListMessage::new(
            "Which shipping option do you prefer?",
            "Shipping Options",
            [
                ListSection::new(
                    "I want it ASAP!",
                    [
                        ListRow::new("priority_express", "Priority Mail Express")
                            .description("Next Day to 2 Days"),
                        ListRow::new("priority_mail", "Priority Mail").description("1–3 Days"),
                    ],
                ),
                ListSection::new(
                    "I can wait a bit",
                    [
                        ListRow::new("usps_ground_advantage", "USPS Ground Advantage")
                            .description("2–5 Days"),
                        ListRow::new("media_mail", "Media Mail").description("2–8 Days"),
                    ],
                ),
            ],
        )
        .header("Choose Shipping Option")
        .footer("Lucky Shrub: Your gateway to succulents™"),
    );
    assert_wire(
        &msg,
        &json!({
          "messaging_product": "whatsapp",
          "recipient_type": "individual",
          "to": "+16505551234",
          "type": "interactive",
          "interactive": {
            "type": "list",
            "header": {"type": "text", "text": "Choose Shipping Option"},
            "body": {"text": "Which shipping option do you prefer?"},
            "footer": {"text": "Lucky Shrub: Your gateway to succulents™"},
            "action": {
              "button": "Shipping Options",
              "sections": [
                {"title": "I want it ASAP!", "rows": [
                  {"id": "priority_express", "title": "Priority Mail Express", "description": "Next Day to 2 Days"},
                  {"id": "priority_mail", "title": "Priority Mail", "description": "1–3 Days"}
                ]},
                {"title": "I can wait a bit", "rows": [
                  {"id": "usps_ground_advantage", "title": "USPS Ground Advantage", "description": "2–5 Days"},
                  {"id": "media_mail", "title": "Media Mail", "description": "2–8 Days"}
                ]}
              ]
            }
          }
        }),
    );
}

#[test]
fn cta_url_and_location_request_match_the_docs() {
    // messages/interactive-cta-url-messages, "Example request".
    assert_wire(
        &OutboundMessage::new(
            phone(),
            CtaUrl::new(
                "Tap the button below to see available dates.",
                "See Dates",
                "https://www.luckyshrub.com?clickID=kqDGWd24Q5TRwoEQTICY7W1JKoXvaZOXWAS7h1P76s0R7Paec4",
            )
            .header(Header::image_link("https://www.luckyshrub.com/assets/lucky-shrub-banner-logo-v1.png"))
            .footer("Dates subject to change."),
        ),
        &json!({
          "messaging_product": "whatsapp",
          "recipient_type": "individual",
          "to": "+16505551234",
          "type": "interactive",
          "interactive": {
            "type": "cta_url",
            "header": {"type": "image", "image": {"link": "https://www.luckyshrub.com/assets/lucky-shrub-banner-logo-v1.png"}},
            "body": {"text": "Tap the button below to see available dates."},
            "action": {"name": "cta_url", "parameters": {
              "display_text": "See Dates",
              "url": "https://www.luckyshrub.com?clickID=kqDGWd24Q5TRwoEQTICY7W1JKoXvaZOXWAS7h1P76s0R7Paec4"
            }},
            "footer": {"text": "Dates subject to change."}
          }
        }),
    );
    // messages/location-request-messages, "Example request".
    assert_wire(
        &OutboundMessage::location_request(
            phone(),
            "Let's start with your pickup. You can either manually *enter an address* or *share your current location*.",
        ),
        &json!({
          "messaging_product": "whatsapp",
          "recipient_type": "individual",
          "type": "interactive",
          "to": "+16505551234",
          "interactive": {
            "type": "location_request_message",
            "body": {"text": "Let's start with your pickup. You can either manually *enter an address* or *share your current location*."},
            "action": {"name": "send_location"}
          }
        }),
    );
}

#[test]
fn flow_matches_the_docs() {
    // flows/guides/sendingaflow, "Cloud API Sample Request (with all
    // parameters)", `flow_name` variant.
    let params = FlowParameters::new(FlowRef::Name("appointment_booking_v1".into()), "Book!")
        .token("AQAAAAACS5FpgQ_cAAAAAD0QI3s.")
        .navigate(
            "<SCREEN_NAME>",
            Some(json!("{\"product_name\":\"name\",\"product_description\":\"description\",\"product_price\":100}")),
        );
    let msg = OutboundMessage::new(
        Recipient::phone("whatsapp-id"),
        FlowMessage::new("Flow message body", params)
            .header(Header::text("Flow message header"))
            .footer("Flow message footer"),
    );
    assert_wire(
        &msg,
        &json!({
          "recipient_type": "individual",
          "messaging_product": "whatsapp",
          "to": "whatsapp-id",
          "type": "interactive",
          "interactive": {
            "type": "flow",
            "header": {"type": "text", "text": "Flow message header"},
            "body": {"text": "Flow message body"},
            "footer": {"text": "Flow message footer"},
            "action": {
              "name": "flow",
              "parameters": {
                "flow_message_version": "3",
                "flow_token": "AQAAAAACS5FpgQ_cAAAAAD0QI3s.",
                "flow_name": "appointment_booking_v1",
                "flow_cta": "Book!",
                "flow_action": "navigate",
                "flow_action_payload": {
                  "screen": "<SCREEN_NAME>",
                  "data": "{\"product_name\":\"name\",\"product_description\":\"description\",\"product_price\":100}"
                }
              }
            }
          }
        }),
    );
    // `flow_id` variant, draft mode, data exchange.
    let p = FlowParameters::new(FlowRef::Id("123456".into()), "Go")
        .draft()
        .data_exchange();
    assert_eq!(
        serde_json::to_value(&p).unwrap(),
        json!({"flow_message_version": "3", "flow_id": "123456", "flow_cta": "Go", "mode": "draft", "flow_action": "data_exchange"})
    );
}

#[test]
fn commerce_messages_match_the_docs() {
    // catalogs/single-product-messages, step 1 (placeholders kept).
    assert_wire(
        &OutboundMessage::new(
            Recipient::phone("PHONE_NUMBER"),
            SingleProduct::new("CATALOG_ID", "ID_TEST_ITEM_1")
                .body("BODY_TEXT")
                .footer("FOOTER_TEXT"),
        ),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "to": "PHONE_NUMBER", "type": "interactive",
          "interactive": {"type": "product", "body": {"text": "BODY_TEXT"}, "footer": {"text": "FOOTER_TEXT"},
                          "action": {"catalog_id": "CATALOG_ID", "product_retailer_id": "ID_TEST_ITEM_1"}}
        }),
    );
    // catalogs/multi-product-messages, step 1 (two items per section).
    assert_wire(
        &OutboundMessage::new(
            Recipient::phone("PHONE_NUMBER"),
            ProductList::new(
                "HEADER_CONTENT",
                "BODY_CONTENT",
                "CATALOG_ID",
                [
                    ProductSection::new("SECTION_TITLE", ["PRODUCT-SKU", "PRODUCT-SKU"]),
                    ProductSection::new("SECTION_TITLE", ["PRODUCT-SKU", "PRODUCT-SKU"]),
                ],
            )
            .footer("FOOTER_CONTENT"),
        ),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "to": "PHONE_NUMBER", "type": "interactive",
          "interactive": {
            "type": "product_list",
            "header": {"type": "text", "text": "HEADER_CONTENT"},
            "body": {"text": "BODY_CONTENT"},
            "footer": {"text": "FOOTER_CONTENT"},
            "action": {"catalog_id": "CATALOG_ID", "sections": [
              {"title": "SECTION_TITLE", "product_items": [{"product_retailer_id": "PRODUCT-SKU"}, {"product_retailer_id": "PRODUCT-SKU"}]},
              {"title": "SECTION_TITLE", "product_items": [{"product_retailer_id": "PRODUCT-SKU"}, {"product_retailer_id": "PRODUCT-SKU"}]}
            ]}
          }
        }),
    );
    // catalogs/catalog-messages, "Example request".
    assert_wire(
        &OutboundMessage::new(
            phone(),
            CatalogMessage::new("Hello! Thanks for your interest. Ordering is easy. Just visit our catalog and add items to purchase.")
                .thumbnail("2lc20305pt")
                .footer("Best grocery deals on WhatsApp!"),
        ),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "to": "+16505551234", "type": "interactive",
          "interactive": {
            "type": "catalog_message",
            "body": {"text": "Hello! Thanks for your interest. Ordering is easy. Just visit our catalog and add items to purchase."},
            "action": {"name": "catalog_message", "parameters": {"thumbnail_product_retailer_id": "2lc20305pt"}},
            "footer": {"text": "Best grocery deals on WhatsApp!"}
          }
        }),
    );
    assert_eq!(
        serde_json::to_value(CatalogMessage::new("b")).unwrap(),
        json!({"body": {"text": "b"}, "action": {"name": "catalog_message"}})
    );
    // catalogs/interactive-product-carousel-messages, "Example request".
    assert_wire(
        &OutboundMessage::new(
            Recipient::phone("1234567890"),
            ProductCarousel::new(
                "Check out our featured products!",
                [
                    ProductCard::new("123456789", "abc123xyz"),
                    ProductCard::new("123456789", "def456uvw"),
                ],
            ),
        ),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "to": "1234567890", "type": "interactive",
          "interactive": {"type": "carousel", "body": {"text": "Check out our featured products!"}, "action": {"cards": [
            {"card_index": 0, "type": "product", "action": {"product_retailer_id": "abc123xyz", "catalog_id": "123456789"}},
            {"card_index": 1, "type": "product", "action": {"product_retailer_id": "def456uvw", "catalog_id": "123456789"}}
          ]}}
        }),
    );
}

fn carousel_card(name: &str, slug: &str, text: &str, action: CardAction) -> MediaCard {
    MediaCard::new(
        CardHeader::Image(format!("https://www.luckyshrub.com/assets/{slug}.jpeg")),
        action,
    )
    .body(format!("*{name}*\n\n{text}"))
}

const CAROUSEL_CARDS: [(&str, &str, &str); 3] = [
    (
        "Blue Echeveria",
        "blue-echeveria",
        "A rosette-shaped succulent with powdery blue leaves, perfect for brightening up any space.",
    ),
    (
        "Zebra Haworthia",
        "zebra-haworthia",
        "Striking white stripes on deep green leaves give this compact succulent a bold, modern look.",
    ),
    (
        "Panda Plant",
        "panda-plant",
        "Soft, fuzzy leaves with chocolate-brown edges—adorable and easy to care for.",
    ),
];

fn carousel_card_json(i: usize, action: &Value) -> Value {
    let (name, slug, text) = CAROUSEL_CARDS[i];
    json!({
      "card_index": i,
      "type": "cta_url",
      "header": {"type": "image", "image": {"link": format!("https://www.luckyshrub.com/assets/{slug}.jpeg")}},
      "body": {"text": format!("*{name}*\n\n{text}")},
      "action": action
    })
}

#[test]
fn media_carousel_matches_both_doc_examples() {
    // messages/interactive-media-carousel-messages, "URL buttons example".
    let cards = CAROUSEL_CARDS.iter().map(|(name, slug, text)| {
        carousel_card(
            name,
            slug,
            text,
            CardAction::Url {
                display_text: "Buy now".into(),
                url: format!("https://shop.luckyshrub.com/latest/{slug}"),
            },
        )
    });
    let expected_cards: Vec<Value> = (0..3)
        .map(|i| {
            carousel_card_json(
                i,
                &json!({"name": "cta_url", "parameters": {
                    "display_text": "Buy now",
                    "url": format!("https://shop.luckyshrub.com/latest/{}", CAROUSEL_CARDS[i].1)
                }}),
            )
        })
        .collect();
    let body = "Of course! Here are three of our latest arrivals, each under $25:";
    assert_wire(
        &OutboundMessage::new(
            Recipient::phone("16505551234"),
            MediaCarousel::new(body, cards),
        ),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "to": "16505551234", "type": "interactive",
          "interactive": {"type": "carousel", "body": {"text": body}, "action": {"cards": expected_cards}}
        }),
    );

    // "Quick-reply buttons example".
    let cards = CAROUSEL_CARDS.iter().map(|(name, slug, text)| {
        carousel_card(
            name,
            slug,
            text,
            CardAction::QuickReplies(vec![
                QuickReply::new(format!("learn-{slug}"), "Learn more"),
                QuickReply::new(format!("fav-{slug}"), "Add to favorites"),
            ]),
        )
    });
    let expected_cards: Vec<Value> = (0..3)
        .map(|i| {
            let slug = CAROUSEL_CARDS[i].1;
            carousel_card_json(i, &json!({"buttons": [
                {"type": "quick_reply", "quick_reply": {"id": format!("learn-{slug}"), "title": "Learn more"}},
                {"type": "quick_reply", "quick_reply": {"id": format!("fav-{slug}"), "title": "Add to favorites"}}
            ]}))
        })
        .collect();
    assert_wire(
        &OutboundMessage::new(
            Recipient::phone("16505551234"),
            MediaCarousel::new(body, cards),
        ),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "to": "16505551234", "type": "interactive",
          "interactive": {"type": "carousel", "body": {"text": body}, "action": {"cards": expected_cards}}
        }),
    );
}

#[test]
fn calling_messages_match_the_docs() {
    let both = || Recipient::PhoneAndUser {
        phone: "14085551234".into(),
        user: UserId::new(BSUID),
    };
    // calling/call-button-messages-deep-links, "Request body".
    assert_wire(
        &OutboundMessage::new(
            both(),
            VoiceCall::new("You can call us on WhatsApp now for faster service!").parameters(
                VoiceCallParameters {
                    display_text: Some("Call on WhatsApp".into()),
                    ttl_minutes: Some(100),
                    payload: Some("payload data".into()),
                },
            ),
        ),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "to": "14085551234", "recipient": BSUID,
          "type": "interactive",
          "interactive": {"type": "voice_call", "body": {"text": "You can call us on WhatsApp now for faster service!"},
                          "action": {"name": "voice_call", "parameters": {"display_text": "Call on WhatsApp", "ttl_minutes": 100, "payload": "payload data"}}}
        }),
    );
    // calling/user-call-permissions, "Send free form call permission request
    // message" (the `to` placeholder replaced by the table's sample value).
    assert_wire(
        &OutboundMessage::new(
            Recipient::PhoneAndUser {
                phone: "+17863476655".into(),
                user: UserId::new(BSUID),
            },
            CallPermissionRequest::new(
                "We would like to call you to help support your query on Order No: ON-12853.",
            ),
        ),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "to": "+17863476655", "recipient": BSUID,
          "type": "interactive",
          "interactive": {"type": "call_permission_request", "action": {"name": "call_permission_request"},
                          "body": {"text": "We would like to call you to help support your query on Order No: ON-12853."}}
        }),
    );
    assert_eq!(
        serde_json::to_value(CallPermissionRequest::default()).unwrap(),
        json!({"action": {"name": "call_permission_request"}})
    );
}

#[test]
fn address_messages_match_the_docs() {
    let body = "Thanks for your order! Tell us what address you'd like this order delivered to.";
    // messages/address-messages, saved addresses example.
    let mut msg = AddressMessage::new(body, "IN");
    msg.parameters.saved_addresses.push(SavedAddress {
        id: "address1".into(),
        value: [
            ("name", "<CUSTOMER_NAME>"),
            ("phone_number", "+91xxxxxxxxxx"),
            ("in_pin_code", "400063"),
            ("floor_number", "8"),
            ("building_name", ""),
            ("address", "Wing A, Cello Triumph,IB Patel Rd"),
            ("landmark_area", "Goregaon"),
            ("city", "Mumbai"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect(),
    });
    assert_wire(
        &OutboundMessage::new(Recipient::phone("91xxxxxxxxxx"), msg),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "to": "91xxxxxxxxxx", "type": "interactive",
          "interactive": {"type": "address_message", "body": {"text": body}, "action": {"name": "address_message", "parameters": {
            "country": "IN",
            "saved_addresses": [{"id": "address1", "value": {
              "name": "<CUSTOMER_NAME>", "phone_number": "+91xxxxxxxxxx", "in_pin_code": "400063", "floor_number": "8",
              "building_name": "", "address": "Wing A, Cello Triumph,IB Patel Rd", "landmark_area": "Goregaon", "city": "Mumbai"
            }}]
          }}}
        }),
    );
    // "Send an address message with validation errors".
    let mut msg = AddressMessage::new(body, "IN");
    for (k, v) in [
        ("name", "CUSTOMER_NAME"),
        ("phone_number", "+91xxxxxxxxxx"),
        ("in_pin_code", "666666"),
        ("address", "Some other location"),
        ("city", "Delhi"),
    ] {
        msg.parameters.values.insert(k.into(), v.into());
    }
    msg.parameters.validation_errors.insert(
        "in_pin_code".into(),
        "We could not locate this pin code.".into(),
    );
    assert_wire(
        &OutboundMessage::new(Recipient::phone("91xxxxxxxxxx"), msg),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "to": "91xxxxxxxxxx", "type": "interactive",
          "interactive": {"type": "address_message", "body": {"text": body}, "action": {"name": "address_message", "parameters": {
            "country": "IN",
            "values": {"name": "CUSTOMER_NAME", "phone_number": "+91xxxxxxxxxx", "in_pin_code": "666666",
                       "address": "Some other location", "city": "Delhi"},
            "validation_errors": {"in_pin_code": "We could not locate this pin code."}
          }}}
        }),
    );
}

#[test]
fn bsuid_template_and_contact_info_request_match_the_docs() {
    // business-scoped-user-ids, "Using templates": BSUID only, no `to`.
    assert_wire(
        &OutboundMessage::template(
            Recipient::user(BSUID),
            TemplateMessage::new("<TEMPLATE_NAME>", "<TEMPLATE_LANGUAGE>"),
        ),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "recipient": BSUID, "type": "template",
          "template": {"name": "<TEMPLATE_NAME>", "language": {"code": "<TEMPLATE_LANGUAGE>"}}
        }),
    );
    // Same page, "Using interactive messages".
    assert_wire(
        &OutboundMessage::new(
            Recipient::user(BSUID),
            RequestContactInfo::new("<BODY_TEXT>"),
        ),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "recipient": BSUID, "type": "interactive",
          "interactive": {"type": "request_contact_info", "body": {"text": "<BODY_TEXT>"}, "action": {"name": "request_contact_info"}}
        }),
    );
}

#[test]
fn envelope_features_match_the_docs() {
    // messages/contextual-replies, "Example request".
    assert_wire(
        &OutboundMessage::text(phone(), "You're welcome, Pablo!")
            .reply_to("wamid.HBgLMTY0NjcwNDM1OTUVAgASGBQzQTdCNTg5RjY1MEMyRjlGMjRGNgA="),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "individual", "to": "+16505551234",
          "context": {"message_id": "wamid.HBgLMTY0NjcwNDM1OTUVAgASGBQzQTdCNTg5RjY1MEMyRjlGMjRGNgA="},
          "type": "text", "text": {"body": "You're welcome, Pablo!"}
        }),
    );
    // groups/groups-messaging, "Example group message send".
    assert_wire(
        &OutboundMessage::text_with_preview(
            Recipient::group("Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD"),
            "This is another destination option: https://www.luckytravel.com/DDLmU5F1Pw",
        ),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "group",
          "to": "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD",
          "type": "text", "text": {"preview_url": true, "body": "This is another destination option: https://www.luckytravel.com/DDLmU5F1Pw"}
        }),
    );
    // groups/groups-messaging, pin request (sample values from its table).
    assert_wire(
        &OutboundMessage::pin(
            "Y2FwaV9ncm91cDoxOTUwNTU1MDA3OToxMjAzNjMzOTQzMjAdOTY0MTUZD",
            "wamid.HBgLM",
            4,
        ),
        &json!({
          "messaging_product": "whatsapp", "recipient_type": "group",
          "to": "Y2FwaV9ncm91cDoxOTUwNTU1MDA3OToxMjAzNjMzOTQzMjAdOTY0MTUZD",
          "type": "pin", "pin": {"type": "pin", "message_id": "wamid.HBgLM", "expiration_days": 4}
        }),
    );
    assert_wire(
        &OutboundMessage::unpin("G", "wamid.HBgLM"),
        &json!({"messaging_product": "whatsapp", "recipient_type": "group", "to": "G", "type": "pin",
                "pin": {"type": "unpin", "message_id": "wamid.HBgLM"}}),
    );
    // Callback data (webhooks/reference/messages/status sample value).
    assert_wire(
        &OutboundMessage::text(phone(), "hi").callback_data("1744434060"),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": PHONE, "type": "text",
                "text": {"body": "hi"}, "biz_opaque_callback_data": "1744434060"}),
    );
    // Raw escape hatch.
    assert_wire(
        &OutboundMessage::new(
            phone(),
            MessageContent::Raw {
                message_type: "order_details".into(),
                body: json!({"a": 1}),
            },
        ),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": PHONE,
                "type": "order_details", "order_details": {"a": 1}}),
    );
}

/// The doc examples set nearly every optional field, so on their own they
/// cannot tell "omitted when absent" from "sent as null / [] / {}". These
/// shapes leave each optional field out at least once.
#[test]
fn sparse_shapes_omit_what_is_absent() {
    let content = |m: OutboundMessage| {
        let wire = serde_json::to_vec(&m).unwrap();
        assert_no_duplicate_keys(&wire);
        let mut v: Value = serde_json::from_slice(&wire).unwrap();
        let kind = v["type"].as_str().unwrap().to_owned();
        v[kind].take()
    };
    // Contacts: a bare card, and one whose nested objects are all empty.
    let sparse = Contact {
        addresses: vec![ContactAddress::default()],
        birthday: None,
        emails: vec![ContactEmail {
            email: "a@example.com".into(),
            kind: None,
        }],
        name: ContactName::new("B"),
        org: Some(ContactOrg::default()),
        phones: vec![ContactPhone {
            phone: "+1".into(),
            kind: None,
            wa_id: None,
        }],
        urls: vec![ContactUrl {
            url: "https://x.example".into(),
            kind: None,
        }],
    };
    assert_eq!(
        content(OutboundMessage::contacts(
            phone(),
            [Contact::new("A"), sparse]
        )),
        json!([
            {"name": {"formatted_name": "A"}},
            {"addresses": [{}], "emails": [{"email": "a@example.com"}], "name": {"formatted_name": "B"},
             "org": {}, "phones": [{"phone": "+1"}], "urls": [{"url": "https://x.example"}]}
        ])
    );
    // Media without caption, file name or voice flag.
    assert_eq!(
        content(OutboundMessage::video_link(phone(), "https://x/v.mp4")),
        json!({"link": "https://x/v.mp4"})
    );
    assert_eq!(
        content(OutboundMessage::audio_id(phone(), "1")),
        json!({"id": "1"})
    );
    assert_eq!(
        content(OutboundMessage::new(
            phone(),
            Document::new(MediaSource::id("1"))
        )),
        json!({"id": "1"})
    );
    assert_eq!(
        content(OutboundMessage::location(phone(), 1.5, -2.0)),
        json!({"latitude": "1.5", "longitude": "-2"})
    );
    // Headers: document without file name; text with the reference's sub_text.
    let header = |h: Header| serde_json::to_value(h).unwrap();
    assert_eq!(
        header(Header::document_id("1")),
        json!({"type": "document", "document": {"id": "1"}})
    );
    assert_eq!(
        header(Header::Text {
            text: "T".into(),
            sub_text: Some("S".into())
        }),
        json!({"type": "text", "text": "T", "sub_text": "S"})
    );
    // A one-section list without section title or row description.
    assert_eq!(
        content(list(vec![ListSection {
            title: None,
            rows: vec![ListRow::new("r", "Row")],
        }])),
        json!({"type": "list", "body": {"text": "body"}, "action": {"button": "Options", "sections": [
            {"rows": [{"id": "r", "title": "Row"}]}
        ]}})
    );
    // Flow: bare parameters, and navigate without data.
    assert_eq!(
        serde_json::to_value(FlowParameters::new(FlowRef::Name("f".into()), "Go")).unwrap(),
        json!({"flow_message_version": "3", "flow_name": "f", "flow_cta": "Go"})
    );
    assert_eq!(
        serde_json::to_value(
            FlowParameters::new(FlowRef::Name("f".into()), "Go").navigate("S", None)
        )
        .unwrap(),
        json!({"flow_message_version": "3", "flow_name": "f", "flow_cta": "Go",
               "flow_action": "navigate", "flow_action_payload": {"screen": "S"}})
    );
    // Call button: no parameters, then one of three.
    assert_eq!(
        content(OutboundMessage::new(phone(), VoiceCall::new("b"))),
        json!({"type": "voice_call", "body": {"text": "b"}, "action": {"name": "voice_call"}})
    );
    assert_eq!(
        serde_json::to_value(VoiceCallParameters {
            ttl_minutes: Some(60),
            ..Default::default()
        })
        .unwrap(),
        json!({"ttl_minutes": 60})
    );
    // A one-section product list without section title.
    assert_eq!(
        content(OutboundMessage::product_list(
            phone(),
            "H",
            "B",
            "C",
            [ProductSection {
                title: None,
                product_retailer_ids: vec!["S".into()],
            }],
        ))["action"],
        json!({"catalog_id": "C", "sections": [{"product_items": [{"product_retailer_id": "S"}]}]})
    );
    // A carousel card without body.
    let card = MediaCard::new(
        CardHeader::Video("https://x/v.mp4".into()),
        CardAction::Url {
            display_text: "Go".into(),
            url: "https://x".into(),
        },
    );
    assert_eq!(
        content(OutboundMessage::new(
            phone(),
            MediaCarousel::new("b", vec![card; 2])
        ))["action"]["cards"][1],
        json!({"card_index": 1, "type": "cta_url",
               "header": {"type": "video", "video": {"link": "https://x/v.mp4"}},
               "action": {"name": "cta_url", "parameters": {"display_text": "Go", "url": "https://x"}}})
    );
}

#[test]
fn direct_send_matches_the_docs() {
    // direct-send/send-utility-and-authentication-messages.
    assert_wire(
        &OutboundMessage::text(
            Recipient::phone("<WHATSAPP_USER_PHONE_NUMBER>"),
            "<BODY_TEXT>",
        )
        .category(DirectSendCategory::Utility),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": "<WHATSAPP_USER_PHONE_NUMBER>",
                "type": "text", "text": {"body": "<BODY_TEXT>"}, "category": "utility"}),
    );
    // direct-send/configure-message-ttl.
    assert_wire(
        &OutboundMessage::text(
            Recipient::phone("<WHATSAPP_USER_PHONE_NUMBER>"),
            "<BODY_TEXT>",
        )
        .category(DirectSendCategory::Utility)
        .ttl_seconds(600),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": "<WHATSAPP_USER_PHONE_NUMBER>",
                "type": "text", "text": {"body": "<BODY_TEXT>"}, "category": "utility", "ttl_seconds": 600}),
    );
    // direct-send/business-named-templates.
    assert_wire(
        &OutboundMessage::text(
            Recipient::phone("<WHATSAPP_USER_PHONE_NUMBER>"),
            "Hi Jane, your order #12345 has been shipped and is expected to arrive on March 20.",
        )
        .category(DirectSendCategory::Utility)
        .direct_send_template_name("order_shipment_update"),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": "<WHATSAPP_USER_PHONE_NUMBER>",
                "type": "text", "text": {"body": "Hi Jane, your order #12345 has been shipped and is expected to arrive on March 20."},
                "category": "utility", "direct_send_config": {"template_name": "order_shipment_update"}}),
    );
    for (c, s) in [
        (DirectSendCategory::Authentication, "authentication"),
        (DirectSendCategory::Service, "service"),
        (DirectSendCategory::Other("marketing".into()), "marketing"),
    ] {
        assert_eq!(serde_json::to_value(&c).unwrap(), json!(s));
    }
}

// ─── Local validation: every limit, both sides of its boundary ───────────

/// `build(n)` must pass validation at `max` and fail on `field` at
/// `max + 1`. Kills off-by-one and deleted-guard mutations alike.
#[track_caller]
fn assert_limit(field: &str, max: usize, build: impl Fn(usize) -> OutboundMessage) {
    if let Err(e) = build(max).validate() {
        panic!("{field} at {max} must pass: {e}");
    }
    assert_eq!(invalid(&build(max + 1)), field, "{field} at {}", max + 1);
}

fn s(n: usize) -> String {
    "x".repeat(n)
}

fn buttons(n: usize) -> Vec<ReplyButton> {
    (0..n)
        .map(|i| ReplyButton::new(format!("b{i}"), format!("B{i}")))
        .collect()
}

#[test]
fn text_and_caption_limits() {
    assert_limit("text.body", 4096, |n| OutboundMessage::text(phone(), s(n)));
    assert_limit("text.body", 1024, |n| {
        OutboundMessage::text(phone(), s(n)).category(DirectSendCategory::Utility)
    });
    assert_limit("text.body", 1024, |n| {
        OutboundMessage::text(phone(), s(n)).category(DirectSendCategory::Authentication)
    });
    // `service` is the normal flow: the normal limit.
    assert_limit("text.body", 4096, |n| {
        OutboundMessage::text(phone(), s(n)).category(DirectSendCategory::Service)
    });
    assert_limit("image.caption", 1024, |n| {
        OutboundMessage::new(phone(), Image::new(MediaSource::id("1")).caption(s(n)))
    });
    assert_limit("video.caption", 1024, |n| {
        OutboundMessage::new(phone(), Video::new(MediaSource::id("1")).caption(s(n)))
    });
    assert_limit("document.caption", 1024, |n| {
        OutboundMessage::new(phone(), Document::new(MediaSource::id("1")).caption(s(n)))
    });
    assert_eq!(invalid(&OutboundMessage::image_id(phone(), "")), "image.id");
    assert_eq!(
        invalid(&OutboundMessage::audio_link(phone(), "")),
        "audio.link"
    );
    assert_eq!(
        invalid(&OutboundMessage::sticker_id(phone(), "")),
        "sticker.id"
    );
    assert_eq!(
        invalid(&OutboundMessage::video_link(phone(), "")),
        "video.link"
    );
    assert_eq!(
        invalid(&OutboundMessage::document_link(phone(), "", "a.pdf")),
        "document.link"
    );
    assert_eq!(
        invalid(&OutboundMessage::text(Recipient::phone(""), "hi")),
        "to"
    );
    assert_eq!(
        invalid(&OutboundMessage::text(Recipient::user(""), "hi")),
        "recipient"
    );
    assert_eq!(
        invalid(&OutboundMessage::text(Recipient::group(""), "hi")),
        "to"
    );
}

#[test]
fn reply_button_limits() {
    let rb = |b: Vec<ReplyButton>| OutboundMessage::reply_buttons(phone(), "body", b);
    assert_limit("interactive.action.buttons", 3, |n| rb(buttons(n)));
    assert_eq!(invalid(&rb(vec![])), "interactive.action.buttons");
    assert_limit("interactive.action.buttons[0].reply.title", 20, |n| {
        rb(vec![ReplyButton::new("id", s(n))])
    });
    assert_limit("interactive.action.buttons[0].reply.id", 256, |n| {
        rb(vec![ReplyButton::new(s(n), "t")])
    });
    assert_limit("interactive.body.text", 1024, |n| {
        OutboundMessage::reply_buttons(phone(), s(n), buttons(1))
    });
    assert_limit("interactive.footer.text", 60, |n| {
        OutboundMessage::new(phone(), ReplyButtons::new("b", buttons(1)).footer(s(n)))
    });
    assert_eq!(
        invalid(&rb(vec![
            ReplyButton::new("a", "Same"),
            ReplyButton::new("b", "Same")
        ])),
        "interactive.action.buttons[1].reply.title"
    );
    assert_eq!(
        invalid(&rb(vec![
            ReplyButton::new("a", "One"),
            ReplyButton::new("a", "Two")
        ])),
        "interactive.action.buttons[1].reply.id"
    );
    assert_eq!(
        invalid(&OutboundMessage::new(
            phone(),
            ReplyButtons::new("b", buttons(1)).header(Header::text(""))
        )),
        "interactive.header.text"
    );
    assert_eq!(
        invalid(&OutboundMessage::new(
            phone(),
            ReplyButtons::new("b", buttons(1)).header(Header::image_link(""))
        )),
        "interactive.header.image.link"
    );
}

fn rows(n: usize) -> Vec<ListRow> {
    (0..n)
        .map(|i| ListRow::new(format!("r{i}"), format!("Row {i}")))
        .collect()
}

fn list(sections: Vec<ListSection>) -> OutboundMessage {
    OutboundMessage::list(phone(), "body", "Options", sections)
}

#[test]
fn list_limits() {
    let one = |r: ListRow| list(vec![ListSection::new("S", [r])]);
    assert_limit("interactive.action.button", 20, |n| {
        OutboundMessage::list(phone(), "b", s(n), [ListSection::new("S", rows(1))])
    });
    assert_limit("interactive.body.text", 4096, |n| {
        OutboundMessage::list(phone(), s(n), "o", [ListSection::new("S", rows(1))])
    });
    assert_limit("interactive.header.text", 60, |n| {
        OutboundMessage::new(
            phone(),
            ListMessage::new("b", "o", [ListSection::new("S", rows(1))]).header(s(n)),
        )
    });
    assert_limit("interactive.footer.text", 60, |n| {
        OutboundMessage::new(
            phone(),
            ListMessage::new("b", "o", [ListSection::new("S", rows(1))]).footer(s(n)),
        )
    });
    assert_limit("interactive.action.sections", 10, |n| {
        list(
            (0..n)
                .map(|i| ListSection::new(format!("S{i}"), rows(1)))
                .take(n)
                .collect(),
        )
    });
    assert_eq!(invalid(&list(vec![])), "interactive.action.sections");
    // 10 rows across all sections combined.
    assert_limit("interactive.action.sections[].rows", 10, |n| {
        list(vec![
            ListSection::new("A", rows(n / 2)),
            ListSection::new("B", rows(n - n / 2)),
        ])
    });
    assert_eq!(
        invalid(&list(vec![ListSection::new("A", [])])),
        "interactive.action.sections[].rows"
    );
    assert_limit("interactive.action.sections[0].rows[0].id", 200, |n| {
        one(ListRow::new(s(n), "t"))
    });
    assert_limit("interactive.action.sections[0].rows[0].title", 24, |n| {
        one(ListRow::new("i", s(n)))
    });
    assert_limit(
        "interactive.action.sections[0].rows[0].description",
        72,
        |n| one(ListRow::new("i", "t").description(s(n))),
    );
    assert_limit("interactive.action.sections[0].title", 24, |n| {
        list(vec![ListSection::new(s(n), rows(1))])
    });
    // Title optional with one section, required with more.
    let untitled = |r| ListSection {
        title: None,
        rows: r,
    };
    assert!(list(vec![untitled(rows(1))]).validate().is_ok());
    assert_eq!(
        invalid(&list(vec![
            ListSection::new("A", rows(1)),
            untitled(rows(1))
        ])),
        "interactive.action.sections[1].title"
    );
}

#[test]
fn cta_location_request_and_flow_limits() {
    let cta = |b: String, d: String| OutboundMessage::cta_url(phone(), b, d, "https://x.example");
    assert_limit("interactive.body.text", 1024, |n| cta(s(n), "Go".into()));
    assert_limit("interactive.action.parameters.display_text", 20, |n| {
        cta("b".into(), s(n))
    });
    assert_limit("interactive.footer.text", 60, |n| {
        OutboundMessage::new(phone(), CtaUrl::new("b", "Go", "https://x").footer(s(n)))
    });
    assert_limit("interactive.header.text", 60, |n| {
        OutboundMessage::new(
            phone(),
            CtaUrl::new("b", "Go", "https://x").header(Header::text(s(n))),
        )
    });
    assert_eq!(
        invalid(&OutboundMessage::cta_url(phone(), "b", "Go", "")),
        "interactive.action.parameters.url"
    );
    assert_limit("interactive.body.text", 1024, |n| {
        OutboundMessage::location_request(phone(), s(n))
    });

    let flow = |p: FlowParameters| OutboundMessage::flow(phone(), "body", p);
    let named = || FlowParameters::new(FlowRef::Name("f".into()), "Open");
    assert!(flow(named()).validate().is_ok());
    assert_eq!(
        invalid(&flow(FlowParameters::new(FlowRef::Name("f".into()), ""))),
        "interactive.action.parameters.flow_cta"
    );
    assert_eq!(
        invalid(&flow(FlowParameters::new(FlowRef::Id("".into()), "Open"))),
        "interactive.action.parameters.flow_id"
    );
    for empty in [json!({}), json!(""), Value::Null] {
        assert_eq!(
            invalid(&flow(named().navigate("S", Some(empty)))),
            "interactive.action.parameters.flow_action_payload.data"
        );
    }
    assert_eq!(
        invalid(&flow(named().navigate("", None))),
        "interactive.action.parameters.flow_action_payload.screen"
    );
    assert_eq!(
        invalid(&OutboundMessage::flow(phone(), "", named())),
        "interactive.body.text"
    );
}

#[test]
fn commerce_limits() {
    let mpm = |sections: Vec<ProductSection>| {
        OutboundMessage::product_list(phone(), "H", "B", "C", sections)
    };
    let skus = |n: usize| (0..n).map(|i| format!("SKU{i}")).collect::<Vec<_>>();
    // Up to 30 products across sections.
    assert_limit("interactive.action.sections[].product_items", 30, |n| {
        mpm(vec![
            ProductSection::new("A", skus(n / 2)),
            ProductSection::new("B", skus(n - n / 2)),
        ])
    });
    assert_eq!(invalid(&mpm(vec![])), "interactive.action.sections");
    assert_eq!(
        invalid(&mpm(vec![ProductSection::new("A", skus(0))])),
        "interactive.action.sections[].product_items"
    );
    assert_limit("interactive.action.sections[0].title", 24, |n| {
        mpm(vec![ProductSection::new(s(n), skus(1))])
    });
    let untitled = ProductSection {
        title: None,
        product_retailer_ids: skus(1),
    };
    assert!(mpm(vec![untitled.clone()]).validate().is_ok());
    assert_eq!(
        invalid(&mpm(vec![ProductSection::new("A", skus(1)), untitled])),
        "interactive.action.sections[1].title"
    );
    assert_eq!(
        invalid(&OutboundMessage::product_list(
            phone(),
            "",
            "B",
            "C",
            [ProductSection::new("A", skus(1))]
        )),
        "interactive.header.text"
    );
    assert_eq!(
        invalid(&OutboundMessage::product(phone(), "C", "")),
        "interactive.action.product_retailer_id"
    );
    assert_eq!(
        invalid(&OutboundMessage::product(phone(), "", "S")),
        "interactive.action.catalog_id"
    );

    assert_limit("interactive.body.text", 1024, |n| {
        OutboundMessage::catalog(phone(), s(n))
    });
    assert_limit("interactive.footer.text", 60, |n| {
        OutboundMessage::new(phone(), CatalogMessage::new("b").footer(s(n)))
    });

    let cards = |n: usize| {
        (0..n)
            .map(|i| ProductCard::new("C", format!("P{i}")))
            .collect::<Vec<_>>()
    };
    let pc = |body: String, c: Vec<ProductCard>| {
        OutboundMessage::new(phone(), ProductCarousel::new(body, c))
    };
    assert_limit("interactive.action.cards", 10, |n| pc("b".into(), cards(n)));
    assert_eq!(
        invalid(&pc("b".into(), cards(1))),
        "interactive.action.cards"
    );
    assert_limit("interactive.body.text", 1024, |n| pc(s(n), cards(2)));
    assert_eq!(
        invalid(&pc(
            "b".into(),
            vec![ProductCard::new("C", "P0"), ProductCard::new("OTHER", "P1")]
        )),
        "interactive.action.cards[1].action.catalog_id"
    );
}

fn url_card() -> MediaCard {
    MediaCard::new(
        CardHeader::Image("https://x/a.jpeg".into()),
        CardAction::Url {
            display_text: "Buy".into(),
            url: "https://x".into(),
        },
    )
}

fn qr_card(n: usize) -> MediaCard {
    MediaCard::new(
        CardHeader::Video("https://x/a.mp4".into()),
        CardAction::QuickReplies(
            (0..n)
                .map(|i| QuickReply::new(format!("q{i}"), "Q"))
                .collect(),
        ),
    )
}

#[test]
fn media_carousel_limits() {
    let mc =
        |cards: Vec<MediaCard>| OutboundMessage::new(phone(), MediaCarousel::new("body", cards));
    assert_limit("interactive.action.cards", 10, |n| mc(vec![url_card(); n]));
    assert_eq!(invalid(&mc(vec![url_card()])), "interactive.action.cards");
    assert_limit("interactive.body.text", 1024, |n| {
        OutboundMessage::new(phone(), MediaCarousel::new(s(n), vec![url_card(); 2]))
    });
    assert_limit("interactive.action.cards[0].body.text", 160, |n| {
        mc(vec![url_card().body(s(n)), url_card()])
    });
    // Up to two line breaks.
    assert!(
        mc(vec![url_card().body("a\nb\nc"), url_card()])
            .validate()
            .is_ok()
    );
    assert_eq!(
        invalid(&mc(vec![url_card().body("a\nb\nc\nd"), url_card()])),
        "interactive.action.cards[0].body.text"
    );
    // Same button type and count on every card.
    assert_eq!(
        invalid(&mc(vec![url_card(), qr_card(1)])),
        "interactive.action.cards[1].action"
    );
    assert_eq!(
        invalid(&mc(vec![qr_card(2), qr_card(1)])),
        "interactive.action.cards[1].action"
    );
    assert!(mc(vec![qr_card(2), qr_card(2)]).validate().is_ok());
    assert_eq!(
        invalid(&mc(vec![qr_card(0), qr_card(0)])),
        "interactive.action.cards[0].action.buttons"
    );
    let titled = |t: String| {
        MediaCard::new(
            CardHeader::Image("https://x".into()),
            CardAction::QuickReplies(vec![QuickReply::new("q", t)]),
        )
    };
    assert_limit(
        "interactive.action.cards[0].action.buttons[0].quick_reply.title",
        20,
        |n| mc(vec![titled(s(n)), titled("t".into())]),
    );
    let ided = |id: String| {
        MediaCard::new(
            CardHeader::Image("https://x".into()),
            CardAction::QuickReplies(vec![QuickReply::new(id, "t")]),
        )
    };
    assert_limit(
        "interactive.action.cards[0].action.buttons[0].quick_reply.id",
        256,
        |n| mc(vec![ided(s(n)), ided("q".into())]),
    );
    let url = |d: String| {
        MediaCard::new(
            CardHeader::Image("https://x".into()),
            CardAction::Url {
                display_text: d,
                url: "https://x".into(),
            },
        )
    };
    assert_limit(
        "interactive.action.cards[0].action.parameters.display_text",
        20,
        |n| mc(vec![url(s(n)), url("d".into())]),
    );
    assert_eq!(
        invalid(&mc(vec![
            MediaCard::new(CardHeader::Image(String::new()), url_card().action),
            url_card()
        ])),
        "interactive.action.cards[0].header.image.link"
    );
}

#[test]
fn calling_limits() {
    let vc =
        |p: VoiceCallParameters| OutboundMessage::new(phone(), VoiceCall::new("b").parameters(p));
    assert_limit("interactive.action.parameters.display_text", 20, |n| {
        vc(VoiceCallParameters {
            display_text: Some(s(n)),
            ..Default::default()
        })
    });
    assert_limit("interactive.action.parameters.payload", 512, |n| {
        vc(VoiceCallParameters {
            payload: Some(s(n)),
            ..Default::default()
        })
    });
    assert_limit("interactive.action.parameters.ttl_minutes", 43200, |n| {
        vc(VoiceCallParameters {
            ttl_minutes: Some(u32::try_from(n).unwrap()),
            ..Default::default()
        })
    });
    let ttl = |t| {
        vc(VoiceCallParameters {
            ttl_minutes: Some(t),
            ..Default::default()
        })
    };
    assert!(ttl(1).validate().is_ok());
    assert_eq!(
        invalid(&ttl(0)),
        "interactive.action.parameters.ttl_minutes"
    );
    assert_eq!(
        invalid(&OutboundMessage::new(phone(), VoiceCall::new(""))),
        "interactive.body.text"
    );
    assert_eq!(
        invalid(&OutboundMessage::new(
            phone(),
            CallPermissionRequest {
                body: Some(String::new())
            }
        )),
        "interactive.body.text"
    );
    assert_eq!(
        invalid(&OutboundMessage::new(phone(), AddressMessage::new("b", ""))),
        "interactive.action.parameters.country"
    );
    assert_eq!(
        invalid(&OutboundMessage::new(phone(), RequestContactInfo::new(""))),
        "interactive.body.text"
    );
}

#[test]
fn contact_location_reaction_and_pin_limits() {
    assert_limit("contacts", 257, |n| {
        OutboundMessage::contacts(phone(), vec![Contact::new("A"); n])
    });
    assert_eq!(invalid(&OutboundMessage::contacts(phone(), [])), "contacts");
    assert_eq!(
        invalid(&OutboundMessage::contacts(phone(), [Contact::new("")])),
        "contacts[0].name.formatted_name"
    );
    for bad in [
        "1999-1-23",
        "1999-02-30",
        "23-01-1999",
        "19990123",
        "1999-01-23T00:00",
        "+1999-01-23",
        "01999-01-23",
        " 1999-01-23",
        "1999-01-23 ",
        "1999/01/23",
    ] {
        assert_eq!(
            invalid(&OutboundMessage::contacts(
                phone(),
                [Contact::new("A").birthday(bad)]
            )),
            "contacts[0].birthday",
            "{bad}"
        );
    }
    assert!(
        OutboundMessage::contacts(phone(), [Contact::new("A").birthday("2000-02-29")])
            .validate()
            .is_ok()
    );

    for (lat, lon, field) in [
        (90.000_001, 0.0, "location.latitude"),
        (-90.000_001, 0.0, "location.latitude"),
        (f64::NAN, 0.0, "location.latitude"),
        (0.0, 180.000_001, "location.longitude"),
        (0.0, f64::INFINITY, "location.longitude"),
    ] {
        assert_eq!(
            invalid(&OutboundMessage::location(phone(), lat, lon)),
            field
        );
    }
    assert!(
        OutboundMessage::location(phone(), -90.0, 180.0)
            .validate()
            .is_ok()
    );

    // messages/contextual-replies: a reaction cannot be a contextual reply.
    assert_eq!(
        invalid(&OutboundMessage::reaction(phone(), "wamid.A", "👍").reply_to("wamid.B")),
        "context"
    );
    assert_eq!(
        invalid(&OutboundMessage::reaction(phone(), "", "👍")),
        "reaction.message_id"
    );
    assert_eq!(
        invalid(&OutboundMessage::text(phone(), "hi").reply_to("")),
        "context.message_id"
    );

    assert!(OutboundMessage::pin("G", "wamid.A", 1).validate().is_ok());
    assert!(OutboundMessage::pin("G", "wamid.A", 30).validate().is_ok());
    assert_eq!(
        invalid(&OutboundMessage::pin("G", "wamid.A", 0)),
        "pin.expiration_days"
    );
    assert_eq!(
        invalid(&OutboundMessage::pin("G", "wamid.A", 31)),
        "pin.expiration_days"
    );
    assert_eq!(invalid(&OutboundMessage::pin("G", "", 3)), "pin.message_id");
    let mut to_user = OutboundMessage::pin("G", "wamid.A", 3);
    to_user.recipient = phone();
    assert_eq!(invalid(&to_user), "recipient_type");
}

#[test]
fn direct_send_limits() {
    let text = || OutboundMessage::text(phone(), "hi");
    let utility = || text().category(DirectSendCategory::Utility);
    let auth = || text().category(DirectSendCategory::Authentication);
    assert_limit("ttl_seconds", 43200, |n| {
        utility().ttl_seconds(u32::try_from(n).unwrap())
    });
    assert_limit("ttl_seconds", 900, |n| {
        auth().ttl_seconds(u32::try_from(n).unwrap())
    });
    assert!(utility().ttl_seconds(30).validate().is_ok());
    assert_eq!(invalid(&utility().ttl_seconds(29)), "ttl_seconds");
    assert_eq!(invalid(&auth().ttl_seconds(29)), "ttl_seconds");
    // TTL outside Direct Send is error 100 at Meta.
    assert_eq!(invalid(&text().ttl_seconds(600)), "ttl_seconds");
    assert_eq!(
        invalid(
            &text()
                .category(DirectSendCategory::Service)
                .ttl_seconds(600)
        ),
        "ttl_seconds"
    );
    assert!(
        text()
            .category(DirectSendCategory::Other("new".into()))
            .ttl_seconds(5)
            .validate()
            .is_ok()
    );

    // Authentication can't go to a BSUID-only recipient.
    let mut to_bsuid = auth();
    to_bsuid.recipient = Recipient::user(BSUID);
    assert_eq!(invalid(&to_bsuid), "recipient");
    to_bsuid.recipient = Recipient::PhoneAndUser {
        phone: PHONE.into(),
        user: UserId::new(BSUID),
    };
    assert!(to_bsuid.validate().is_ok());
    let mut utility_bsuid = utility();
    utility_bsuid.recipient = Recipient::user(BSUID);
    assert!(utility_bsuid.validate().is_ok());

    // Business-named templates: utility only, ^[a-z0-9_]+$, ≤ 512.
    assert_limit("direct_send_config.template_name", 512, |n| {
        utility().direct_send_template_name(s(n))
    });
    assert!(
        utility()
            .direct_send_template_name("order_update_2")
            .validate()
            .is_ok()
    );
    for bad in ["Order", "order-update", "order update", "ordér", ""] {
        assert_eq!(
            invalid(&utility().direct_send_template_name(bad)),
            "direct_send_config.template_name",
            "{bad}"
        );
    }
    assert_eq!(
        invalid(&auth().direct_send_template_name("x")),
        "direct_send_config"
    );
    assert_eq!(
        invalid(&text().direct_send_template_name("x")),
        "direct_send_config"
    );

    // direct-send/supported-features-and-limits: header 60 under Direct
    // Send; the reply-buttons page itself documents no header limit.
    let buttons_with_header = |n: usize| {
        OutboundMessage::new(
            phone(),
            ReplyButtons::new("b", buttons(1)).header(Header::text(s(n))),
        )
    };
    assert_limit("interactive.header.text", 60, |n| {
        buttons_with_header(n).category(DirectSendCategory::Utility)
    });
    assert!(buttons_with_header(61).validate().is_ok());
}

#[test]
fn raw_types_cannot_shadow_envelope_fields() {
    let raw = |t: &str| {
        OutboundMessage::new(
            phone(),
            MessageContent::Raw {
                message_type: t.into(),
                body: json!({}),
            },
        )
    };
    // Every key the envelope can emit, taken from a message that sets all
    // of them: a Raw type named like any of them would put it twice.
    let full = |t: &str| {
        let mut m = raw(t)
            .reply_to("wamid.A")
            .callback_data("cb")
            .category(DirectSendCategory::Utility)
            .ttl_seconds(60)
            .direct_send_template_name("n");
        m.recipient = Recipient::PhoneAndUser {
            phone: PHONE.into(),
            user: UserId::new(BSUID),
        };
        m
    };
    let envelope = serde_json::to_value(full("order_status")).unwrap();
    let keys: Vec<&String> = envelope
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| *k != "order_status")
        .collect();
    assert_eq!(keys.len(), 10, "{keys:?}");
    for reserved in keys {
        assert_eq!(invalid(&full(reserved)), "type", "{reserved}");
        assert_eq!(invalid(&raw(reserved)), "type", "{reserved}");
    }
    assert_eq!(invalid(&raw("")), "type");
    assert!(full("order_status").validate().is_ok());
    assert_no_duplicate_keys(&serde_json::to_vec(&full("order_status")).unwrap());
    // The detector itself: an unvalidated collision does duplicate a key.
    let collide = serde_json::to_vec(&raw("to")).unwrap();
    let parsed: Value = serde_json::from_slice(&collide).unwrap();
    assert!(serde_json::to_vec(&parsed).unwrap().len() < collide.len());
    assert_eq!(
        invalid(&OutboundMessage::template(
            phone(),
            TemplateMessage::new("", "en")
        )),
        "template.name"
    );
}
