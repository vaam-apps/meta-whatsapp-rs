//! Requests and responses from `conversation-routing/thread-control`.

use std::time::Duration;

use http::Method;
use meta_whatsapp_core::error::TransportError;
use meta_whatsapp_core::testing::{RecordedBody, ScriptedTransport};
use meta_whatsapp_core::{Error, ErrorKind};
use serde_json::{Value, json};

use super::*;
use crate::RetryPolicy;

const PHONE: &str = "106540352242922";
/// The page's `<WHATSAPP_USER_PHONE_NUMBER>`, filled in.
const USER: &str = "16505551234";
const BSUID: &str = "US.13491208655302741918";

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

fn api(t: &ScriptedTransport) -> ThreadControl {
    client(t).thread_control(PhoneNumberId::new(PHONE).unwrap())
}

/// The page's response example: the same for all three actions.
fn push_docs_response(t: &ScriptedTransport) {
    t.push_json(
        200,
        json!({"messaging_product": "whatsapp", "request_id": "<REQUEST_ID>"}),
    );
}

fn docs_response() -> ThreadControlResponse {
    ThreadControlResponse {
        messaging_product: Some("whatsapp".into()),
        request_id: Some("<REQUEST_ID>".into()),
    }
}

/// One `POST /v25.0/{PHONE}/thread_control` with the token and `body`.
fn assert_thread_control_post(t: &ScriptedTransport, body: &Value) {
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{PHONE}/thread_control"));
    assert_eq!(req.url.query(), None);
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert!(
        matches!(&req.body, RecordedBody::Bytes { content_type, .. } if content_type == "application/json"),
        "{:?}",
        req.body
    );
    assert_eq!(req.json().as_ref(), Some(body));
}

fn validation_field(err: &Error) -> &str {
    match err {
        Error::Validation(v) => &v.field,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

// ─── The page's request examples ─────────────────────────────────────────

#[tokio::test]
async fn untargeted_pass_matches_docs_example() {
    let t = ScriptedTransport::new();
    push_docs_response(&t);
    let resp = api(&t)
        .pass(&Recipient::phone(USER), &Pass::to_escalation_partner())
        .await
        .unwrap();
    assert_thread_control_post(
        &t,
        &json!({"messaging_product": "whatsapp", "to": USER, "action": "pass"}),
    );
    assert_eq!(resp, docs_response());
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn targeted_pass_matches_docs_example() {
    let t = ScriptedTransport::new();
    push_docs_response(&t);
    let resp = api(&t)
        .pass(&Recipient::phone(USER), &Pass::to_role(TargetRole::AiAgent))
        .await
        .unwrap();
    assert_thread_control_post(
        &t,
        &json!({
            "messaging_product": "whatsapp",
            "to": USER,
            "action": "pass",
            "control_pass": {"target_role": "ai_agent"}
        }),
    );
    assert_eq!(resp, docs_response());
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn release_matches_docs_example() {
    let t = ScriptedTransport::new();
    push_docs_response(&t);
    let resp = api(&t).release(&Recipient::phone(USER)).await.unwrap();
    assert_thread_control_post(
        &t,
        &json!({"messaging_product": "whatsapp", "to": USER, "action": "release"}),
    );
    assert_eq!(resp, docs_response());
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn take_matches_docs_example() {
    let t = ScriptedTransport::new();
    push_docs_response(&t);
    let resp = api(&t).take(&Recipient::phone(USER), None).await.unwrap();
    assert_thread_control_post(
        &t,
        &json!({"messaging_product": "whatsapp", "to": USER, "action": "take"}),
    );
    assert_eq!(resp, docs_response());
    assert_eq!(t.remaining(), 0);
}

// ─── Metadata: the handover examples' strings ────────────────────────────

/// The `control_passed` example's `metadata` is what the pass sent.
#[tokio::test]
async fn pass_metadata_is_sent_verbatim() {
    let t = ScriptedTransport::new();
    push_docs_response(&t);
    api(&t)
        .pass(
            &Recipient::phone(USER),
            &Pass::to_escalation_partner().metadata("WhatsApp user requested human agent"),
        )
        .await
        .unwrap();
    assert_thread_control_post(
        &t,
        &json!({
            "messaging_product": "whatsapp",
            "to": USER,
            "action": "pass",
            "metadata": "WhatsApp user requested human agent"
        }),
    );
    assert_eq!(t.remaining(), 0);
}

/// Target and metadata together.
#[tokio::test]
async fn targeted_pass_with_metadata() {
    let t = ScriptedTransport::new();
    push_docs_response(&t);
    api(&t)
        .pass(
            &Recipient::phone(USER),
            &Pass::to_role(TargetRole::CustomerService).metadata("order #1234"),
        )
        .await
        .unwrap();
    assert_thread_control_post(
        &t,
        &json!({
            "messaging_product": "whatsapp",
            "to": USER,
            "action": "pass",
            "control_pass": {"target_role": "customer_service"},
            "metadata": "order #1234"
        }),
    );
    assert_eq!(t.remaining(), 0);
}

/// The `control_taken` example's `metadata` is what the take sent.
#[tokio::test]
async fn take_metadata_is_sent_verbatim() {
    let t = ScriptedTransport::new();
    push_docs_response(&t);
    api(&t)
        .take(&Recipient::phone(USER), Some("Human agent stepping in"))
        .await
        .unwrap();
    assert_thread_control_post(
        &t,
        &json!({
            "messaging_product": "whatsapp",
            "to": USER,
            "action": "take",
            "metadata": "Human agent stepping in"
        }),
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn metadata_is_at_most_2000_characters() {
    let t = ScriptedTransport::new();
    let user = Recipient::phone(USER);
    let too_long = "a".repeat(METADATA_MAX_CHARS + 1);
    let err = api(&t)
        .pass(&user, &Pass::to_escalation_partner().metadata(&too_long))
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "metadata");
    let err = api(&t).take(&user, Some(&too_long)).await.unwrap_err();
    assert_eq!(validation_field(&err), "metadata");
    assert!(t.requests().is_empty(), "refused before any request");

    // 2,000 characters pass, counted as characters, not bytes: 2,000
    // two-byte characters are 4,000 bytes.
    let at_most = "é".repeat(METADATA_MAX_CHARS);
    assert_eq!(at_most.len(), 2 * METADATA_MAX_CHARS);
    push_docs_response(&t);
    push_docs_response(&t);
    api(&t)
        .pass(&user, &Pass::to_escalation_partner().metadata(&at_most))
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().json().unwrap()["metadata"],
        json!(at_most)
    );
    api(&t).take(&user, Some(&at_most)).await.unwrap();
    assert_eq!(t.requests().len(), 2);
    assert_eq!(t.remaining(), 0);
}

// ─── Identifying the WhatsApp user ───────────────────────────────────────

/// A BSUID goes in `recipient`, and `to` is absent, on every action.
#[tokio::test]
async fn a_bsuid_is_sent_as_recipient() {
    let t = ScriptedTransport::new();
    let user = Recipient::user(BSUID);
    let api = api(&t);
    for _ in 0..3 {
        push_docs_response(&t);
    }
    api.pass(&user, &Pass::to_escalation_partner())
        .await
        .unwrap();
    assert_thread_control_post(
        &t,
        &json!({"messaging_product": "whatsapp", "recipient": BSUID, "action": "pass"}),
    );
    api.take(&user, None).await.unwrap();
    assert_thread_control_post(
        &t,
        &json!({"messaging_product": "whatsapp", "recipient": BSUID, "action": "take"}),
    );
    api.release(&user).await.unwrap();
    assert_thread_control_post(
        &t,
        &json!({"messaging_product": "whatsapp", "recipient": BSUID, "action": "release"}),
    );
    assert_eq!(t.remaining(), 0);
}

/// Meta rejects both identifiers, or neither (`(#100) Provide only one of
/// to or recipient, not both.`, `(#100) Exactly one of to or recipient is
/// required.`); so is a group, which is not a WhatsApp user.
#[tokio::test]
async fn exactly_one_user_identifier_is_required() {
    let t = ScriptedTransport::new();
    let api = api(&t);
    let cases = [
        (
            Recipient::PhoneAndUser {
                phone: USER.into(),
                user: BSUID.into(),
            },
            "recipient",
        ),
        (Recipient::group("Y2FwaV9ncm91cDox"), "to"),
        (Recipient::phone(""), "to"),
        (Recipient::phone("  "), "to"),
        (Recipient::user(""), "recipient"),
    ];
    for (user, field) in cases {
        let err = api
            .pass(&user, &Pass::to_escalation_partner())
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), field, "pass {user:?}");
        let err = api.take(&user, None).await.unwrap_err();
        assert_eq!(validation_field(&err), field, "take {user:?}");
        let err = api.release(&user).await.unwrap_err();
        assert_eq!(validation_field(&err), field, "release {user:?}");
        assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    }
    assert!(t.requests().is_empty(), "refused before any request");
}

// ─── Target roles ────────────────────────────────────────────────────────

/// The six values of the page's `target_role` table, and a later one sent
/// verbatim.
#[tokio::test]
async fn every_target_role_has_its_wire_name() {
    let roles = [
        (TargetRole::AiAgent, "ai_agent"),
        (TargetRole::Ctwa, "ctwa"),
        (TargetRole::CustomerService, "customer_service"),
        (TargetRole::Escalation, "escalation"),
        (TargetRole::Marketing, "marketing"),
        (TargetRole::Utility, "utility"),
        (TargetRole::Other("future_role".into()), "future_role"),
    ];
    let t = ScriptedTransport::new();
    for (role, wire) in roles {
        assert_eq!(role.as_str(), wire);
        assert_eq!(role.to_string(), wire);
        assert_eq!(serde_json::to_value(&role).unwrap(), json!(wire));
        assert_eq!(
            serde_json::from_value::<TargetRole>(json!(wire)).unwrap(),
            role
        );
        push_docs_response(&t);
        api(&t)
            .pass(&Recipient::phone(USER), &Pass::to_role(role))
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().json().unwrap()["control_pass"],
            json!({"target_role": wire})
        );
    }
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn an_empty_target_role_is_refused() {
    let t = ScriptedTransport::new();
    for empty in ["", " "] {
        let err = api(&t)
            .pass(
                &Recipient::phone(USER),
                &Pass::to_role(TargetRole::Other(empty.into())),
            )
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "control_pass.target_role");
    }
    assert!(t.requests().is_empty());
}

#[test]
fn the_default_pass_is_untargeted() {
    assert_eq!(Pass::default(), Pass::to_escalation_partner());
    assert_eq!(Pass::default().target_role, None);
    assert_eq!(Pass::default().metadata, None);
}

// ─── Responses and errors ────────────────────────────────────────────────

/// A response with a field Meta adds later, or without `request_id`,
/// still parses.
#[tokio::test]
async fn responses_tolerate_new_and_missing_fields() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"messaging_product": "whatsapp", "request_id": "R1", "new_field": {"x": 1}}),
    );
    t.push_json(200, json!({}));
    let api = api(&t);
    let resp = api.release(&Recipient::phone(USER)).await.unwrap();
    assert_eq!(resp.request_id.as_deref(), Some("R1"));
    let resp = api.release(&Recipient::phone(USER)).await.unwrap();
    assert_eq!(resp.messaging_product, None);
    assert_eq!(resp.request_id, None);
    assert_eq!(t.remaining(), 0);
}

/// A row of the page's error table: an account not enabled for `pass`.
#[tokio::test]
async fn pass_not_supported_is_an_invalid_parameter() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        json!({"error": {
            "message": "(#100) Pass action is not supported.",
            "type": "OAuthException",
            "code": 100,
            "fbtrace_id": "<FBTRACE_ID>"
        }}),
    );
    let err = api(&t)
        .pass(&Recipient::phone(USER), &Pass::to_escalation_partner())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    let graph = err.graph().unwrap();
    assert_eq!(graph.code, 100);
    assert_eq!(graph.fbtrace_id.as_deref(), Some("<FBTRACE_ID>"));
    assert_eq!(t.remaining(), 0);
}

/// `2494191`: `take` from a caller that is not the escalation partner.
/// Its code reaches the caller; `ErrorKind` has no class for it yet.
#[tokio::test]
async fn take_by_a_non_escalation_partner_keeps_its_code() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        json!({"error": {
            "message": "You are not permitted to take thread control for this phone number.",
            "type": "OAuthException",
            "code": 2_494_191
        }}),
    );
    let err = api(&t)
        .take(&Recipient::phone(USER), None)
        .await
        .unwrap_err();
    assert_eq!(err.graph().map(|g| g.code), Some(2_494_191));
    assert_eq!(t.remaining(), 0);
}

/// None of the three is replayed after a timeout: Meta may have acted.
#[tokio::test]
async fn no_action_is_replayed_after_a_timeout() {
    let ts = ScriptedTransport::new();
    let api = Client::builder()
        .transport(ts.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy {
            max_retries: 2,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        })
        .build()
        .unwrap()
        .thread_control(PhoneNumberId::new(PHONE).unwrap());
    let user = Recipient::phone(USER);
    for action in ["pass", "take", "release"] {
        ts.push_error(|| TransportError::Timeout);
        // A replay would get this answer instead of the timeout.
        push_docs_response(&ts);
        let before = ts.requests().len();
        let result = match action {
            "pass" => api.pass(&user, &Pass::to_escalation_partner()).await,
            "take" => api.take(&user, None).await,
            _ => api.release(&user).await,
        };
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::Transport(TransportError::Timeout)),
            "{action}: {err:?}"
        );
        assert_eq!(ts.requests().len(), before + 1, "{action} was replayed");
        assert_eq!(ts.remaining(), 1);
        // Consume the unused answer so the next action starts clean.
        api.release(&user).await.unwrap();
    }
    assert_eq!(ts.remaining(), 0);
}

/// The phone number id stays one path segment, whatever it contains.
#[tokio::test]
async fn the_phone_number_id_stays_one_segment() {
    let t = ScriptedTransport::new();
    push_docs_response(&t);
    client(&t)
        .thread_control(PhoneNumberId::new_unchecked("123/messages"))
        .release(&Recipient::phone(USER))
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().path(),
        "/v25.0/123%2Fmessages/thread_control"
    );
    assert_eq!(t.remaining(), 0);
}

#[test]
fn accessors_return_what_was_given() {
    let t = ScriptedTransport::new();
    let api = api(&t);
    assert_eq!(api.id().as_str(), PHONE);
    assert!(api.client().token().is_some());
}
