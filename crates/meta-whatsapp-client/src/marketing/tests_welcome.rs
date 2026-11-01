//! Click-to-WhatsApp welcome message sequences (roadmap L13), against the
//! examples of `ctwa/welcome-message-sequences` (fetched 2026-11-01).

use std::time::Duration;

use http::Method;
use meta_whatsapp_core::Error;
use meta_whatsapp_core::error::TransportError;
use meta_whatsapp_core::ids::{WabaId, WelcomeSequenceId};
use meta_whatsapp_core::testing::{RecordedBody, ScriptedTransport};
use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::{Client, RetryPolicy};

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

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

fn sequences(c: &Client) -> WelcomeSequences {
    c.welcome_sequences(WabaId::new("395394933592466").unwrap())
}

/// The guide's sample `welcome_message_sequence`.
fn doc_message() -> WelcomeMessage {
    WelcomeMessage::new()
        .text("This is a welcome message authored in a 3P tool")
        .autofill("Hello! Can I get more info on this!")
        .ice_breaker("Quick reply 1")
        .ice_breaker("Quick reply 2")
        .ice_breaker("Quick reply 3")
}

fn doc_message_json() -> serde_json::Value {
    json!({
        "text": "This is a welcome message authored in a 3P tool",
        "autofill_message": {"content": "Hello! Can I get more info on this!"},
        "ice_breakers": [
            {"title": "Quick reply 1"},
            {"title": "Quick reply 2"},
            {"title": "Quick reply 3"}
        ]
    })
}

fn text_field(req: &meta_whatsapp_core::testing::RecordedRequest, name: &str) -> Option<String> {
    let (filename, content_type, data) = req.multipart_field(name)?;
    assert_eq!(
        (filename, content_type),
        (None, None),
        "{name} is a text field"
    );
    Some(String::from_utf8(data.to_vec()).unwrap())
}

fn field_names(req: &meta_whatsapp_core::testing::RecordedRequest) -> Vec<String> {
    let RecordedBody::Multipart(parts) = &req.body else {
        panic!("expected multipart, got {:?}", req.body);
    };
    parts.iter().map(|(name, ..)| name.clone()).collect()
}

#[tokio::test]
async fn create_sends_the_documented_form_and_reads_the_id() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"sequence_id": "186473890"}));
    let created = sequences(&client(&t))
        .create("Driver sign-up", &doc_message())
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(
        req.path(),
        "/v25.0/395394933592466/welcome_message_sequences"
    );
    assert_eq!(req.url.query(), None);
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(field_names(&req), ["welcome_message_sequence", "name"]);
    assert_eq!(text_field(&req, "name").as_deref(), Some("Driver sign-up"));
    let sequence: serde_json::Value =
        serde_json::from_str(&text_field(&req, "welcome_message_sequence").unwrap()).unwrap();
    assert_eq!(sequence, doc_message_json());
    assert_eq!(created.sequence_id, WelcomeSequenceId::new("186473890"));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn create_is_not_replayed_after_a_timeout() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    let err = sequences(&retrying_client(&t))
        .create("Driver sign-up", &doc_message())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(t.requests().len(), 1);
}

#[tokio::test]
async fn update_sends_the_documented_form() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    t.push_json(200, json!({"success": true}));
    let c = client(&t);
    let s = sequences(&c);
    let id = WelcomeSequenceId::new("186473890");
    // "Change an existing sequence": the id and a new name.
    s.update(
        &id,
        &WelcomeSequenceUpdate::new().name("Driver sign-up updated name"),
    )
    .await
    .unwrap();
    let req = &t.requests()[0];
    assert_eq!(req.method, Method::POST);
    assert_eq!(
        req.path(),
        "/v25.0/395394933592466/welcome_message_sequences"
    );
    assert_eq!(field_names(req), ["sequence_id", "name"]);
    assert_eq!(text_field(req, "sequence_id").as_deref(), Some("186473890"));
    assert_eq!(
        text_field(req, "name").as_deref(),
        Some("Driver sign-up updated name")
    );

    s.update(&id, &WelcomeSequenceUpdate::new().message(doc_message()))
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(
        field_names(&req),
        ["sequence_id", "welcome_message_sequence"]
    );
    let sequence: serde_json::Value =
        serde_json::from_str(&text_field(&req, "welcome_message_sequence").unwrap()).unwrap();
    assert_eq!(sequence, doc_message_json());
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn update_is_replayed_and_requires_success() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    t.push_json(200, json!({"success": true}));
    sequences(&retrying_client(&t))
        .update(
            &WelcomeSequenceId::new("186473890"),
            &WelcomeSequenceUpdate::new().name("n"),
        )
        .await
        .unwrap();
    assert_eq!(t.requests().len(), 2);
    assert_eq!(t.remaining(), 0);

    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": false}));
    let err = sequences(&client(&t))
        .update(
            &WelcomeSequenceId::new("186473890"),
            &WelcomeSequenceUpdate::new().name("n"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Http { status: 200, .. }), "{err}");
}

#[tokio::test]
async fn list_reads_the_documented_bare_array_and_a_data_envelope() {
    let t = ScriptedTransport::new();
    // "Get a list of sequences" (the page elides entries with `...`).
    t.push_json(
        200,
        json!([
            {"sequence_id": "8716291", "name": "Driver Sign up", "welcome_message_sequence": "<JSON_OBJECT>", "is_used_in_ad": true},
            {"sequence_id": "4362", "name": "Basic Triage", "welcome_message_sequence": "<JSON_OBJECT>", "is_used_in_ad": false},
            {"sequence_id": "0139138", "name": "Appointment Schedule", "welcome_message_sequence": "<JSON_OBJECT>", "is_used_in_ad": true},
            {"sequence_id": "6987565", "name": "Car Leads", "welcome_message_sequence": "<JSON_OBJECT>", "is_used_in_ad": false}
        ]),
    );
    t.push_json(
        200,
        json!({"data": [{"sequence_id": "4362", "welcome_message_sequence": doc_message_json()}]}),
    );
    let c = client(&t);
    let s = sequences(&c);
    let list = s.list(&ListWelcomeSequences::new()).await.unwrap();
    let req = &t.requests()[0];
    assert_eq!(req.method, Method::GET);
    assert_eq!(
        req.path(),
        "/v25.0/395394933592466/welcome_message_sequences"
    );
    assert_eq!(req.url.query(), None);
    assert_eq!(req.bearer(), Some("TOKEN"));
    let ids: Vec<&str> = list.iter().map(|s| s.sequence_id.as_str()).collect();
    assert_eq!(ids, ["8716291", "4362", "0139138", "6987565"]);
    assert_eq!(list[0].name.as_deref(), Some("Driver Sign up"));
    assert_eq!(list[0].is_used_in_ad, Some(true));
    assert_eq!(list[3].is_used_in_ad, Some(false));
    // The guide's placeholder is no welcome message.
    assert_eq!(list[0].message(), None);

    let list = s.list(&ListWelcomeSequences::new().limit(5)).await.unwrap();
    assert_eq!(
        t.last_request().unwrap().query("limit").as_deref(),
        Some("5")
    );
    assert_eq!(list[0].message(), Some(doc_message()));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn get_passes_the_id_and_reads_the_documented_answer() {
    let t = ScriptedTransport::new();
    // "Get a specific sequence".
    t.push_json(
        200,
        json!([{"sequence_id": "6987565", "name": "Driver Sign up", "welcome_message_sequence": doc_message_json().to_string(), "is_used_in_ad": false}]),
    );
    t.push_json(200, json!([]));
    let c = client(&t);
    let s = sequences(&c);
    let found = s
        .get(&WelcomeSequenceId::new("6987565"))
        .await
        .unwrap()
        .unwrap();
    let req = &t.requests()[0];
    assert_eq!(req.method, Method::GET);
    assert_eq!(
        req.path(),
        "/v25.0/395394933592466/welcome_message_sequences"
    );
    assert_eq!(req.query("sequence_id").as_deref(), Some("6987565"));
    assert_eq!(found.name.as_deref(), Some("Driver Sign up"));
    // A message sent back as a JSON string is read too.
    assert_eq!(found.message(), Some(doc_message()));
    assert_eq!(s.get(&WelcomeSequenceId::new("1")).await.unwrap(), None);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn delete_passes_the_id_and_requires_success() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    sequences(&client(&t))
        .delete(&WelcomeSequenceId::new("1234567890"))
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::DELETE);
    assert_eq!(
        req.path(),
        "/v25.0/395394933592466/welcome_message_sequences"
    );
    assert_eq!(req.query("sequence_id").as_deref(), Some("1234567890"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(t.remaining(), 0);
}

/// An empty id would turn a get into the whole list and a delete into one
/// that names no sequence: refused before any request.
#[tokio::test]
async fn local_checks_come_before_any_request() {
    let t = ScriptedTransport::new();
    let c = client(&t);
    let s = sequences(&c);
    let blank = WelcomeSequenceId::new(" ");
    let field = |err: Error| match err {
        Error::Validation(v) => v.field,
        other => panic!("expected a validation error, got {other:?}"),
    };
    assert_eq!(
        field(s.create(" ", &doc_message()).await.unwrap_err()),
        "name"
    );
    assert_eq!(
        field(
            s.update(&blank, &WelcomeSequenceUpdate::new().name("n"))
                .await
                .unwrap_err()
        ),
        "sequence_id"
    );
    assert_eq!(
        field(
            s.update(&WelcomeSequenceId::new("1"), &WelcomeSequenceUpdate::new())
                .await
                .unwrap_err()
        ),
        "welcome_message_sequence"
    );
    assert_eq!(
        field(
            s.update(
                &WelcomeSequenceId::new("1"),
                &WelcomeSequenceUpdate::new().name("")
            )
            .await
            .unwrap_err()
        ),
        "name"
    );
    assert_eq!(field(s.get(&blank).await.unwrap_err()), "sequence_id");
    assert_eq!(field(s.delete(&blank).await.unwrap_err()), "sequence_id");
    assert!(t.requests().is_empty());
}

#[tokio::test]
async fn a_sequence_used_in_an_ad_is_meta_s_refusal() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        json!({"error": {
            "message": "Sequence used in an ad",
            "type": "OAuthException",
            "code": 4027012,
            "error_data": {"details": "The welcome message sequence is linked to an active ad and cannot be updated or deleted."}
        }}),
    );
    let err = sequences(&retrying_client(&t))
        .delete(&WelcomeSequenceId::new("8716291"))
        .await
        .unwrap_err();
    assert_eq!(err.graph().unwrap().code, 4_027_012);
    assert_eq!(t.requests().len(), 1);
}
