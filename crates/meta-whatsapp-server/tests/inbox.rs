//! The inbox routes over HTTP (scope `inbox`; roadmap M2a; the core's
//! `inbox` module, whose cases with no HTTP layer are
//! `conformance::inbox`): acceptance test M2.1 and the binding epoch on
//! memory (on Postgres: `live_postgres.rs`), the reply's idempotency key,
//! the thread owner's refusal and the per-number setting that turns it
//! off, the scope, pages and what a request may name.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::inbox::{TOKEN, accepted, read, reply};
use common::meta::{
    EXAMPLE_PN, EXAMPLE_WA_ID, EXAMPLE_WABA, EXAMPLE_WAMID, bytes, dated, now, sent_now, text,
};
use common::{Call, Harness, test_settings};
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::inbox::ReplyChecks;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server_core::inbox::ReplyChecksByNumber;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const PN: &str = EXAMPLE_PN;
const WABA: &str = EXAMPLE_WABA;

/// M2.1 on memory.
#[tokio::test]
async fn m2_1_an_inbound_message_is_listed_read_and_answered_within_its_window() {
    common::inbox::m2_1(&Harness::new()).await;
}

/// Decisive (roadmap M2a): a moved number's history is not shown to its
/// new tenant.
#[tokio::test]
async fn a_moved_numbers_history_is_hidden_from_its_new_tenant() {
    common::inbox::a_moved_numbers_history_is_hidden_from_its_new_tenant(&Harness::new()).await;
}

/// merchant-42 holds the number, its customer wrote; its `inbox` key.
async fn with_a_conversation(h: &Harness) -> String {
    h.tenant("merchant-42").await;
    h.connect("merchant-42", WABA, &[PN], TOKEN).await;
    let received = h.webhook(&bytes(&text(WABA, PN, EXAMPLE_WAMID))).await;
    assert_eq!(received.status, StatusCode::OK, "{}", received.text);
    // The service's clock past the message (dated the next second).
    h.clock
        .set(time::OffsetDateTime::now_utc() + time::Duration::seconds(2));
    h.tenant_key("merchant-42", &[Scope::Inbox]).await
}

/// A reply is a send (design section 5.4): the same key and body replay
/// the kept answer with `Idempotent-Replayed: true` and no second request;
/// another body under it is `422 idempotency_key_reused`; a refusal that
/// sent nothing (the window) releases its key. The fingerprint is the
/// operation's and its input's: the contact percent-encoded is the same
/// request.
#[tokio::test]
async fn a_reply_takes_an_idempotency_key() {
    let h = Harness::new();
    let key = with_a_conversation(&h).await;
    h.graph.push_json(200, accepted("wamid.REPLY-1"));
    let first = h
        .call(reply(
            &key,
            PN,
            EXAMPLE_WA_ID,
            "order:1:reply",
            "On its way",
        ))
        .await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.text);
    let again = h
        .call(reply(
            &key,
            PN,
            "%31%36%35%30%35%35%35%31%32%33%34",
            "order:1:reply",
            "On its way",
        ))
        .await;
    assert_eq!(
        (again.status, &again.text),
        (StatusCode::ACCEPTED, &first.text)
    );
    assert_eq!(
        again.headers.get("idempotent-replayed").unwrap(),
        "true",
        "replayed"
    );
    assert_eq!(h.graph.requests().len(), 1, "a second request was sent");
    let other = h
        .call(reply(
            &key,
            PN,
            EXAMPLE_WA_ID,
            "order:1:reply",
            "Another text",
        ))
        .await;
    assert_eq!(
        (other.status, other.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused")
    );
    // Another contact under the key is another request too.
    let elsewhere = h
        .call(reply(
            &key,
            PN,
            "16505550000",
            "order:1:reply",
            "On its way",
        ))
        .await;
    assert_eq!(elsewhere.code(), "idempotency_key_reused");
    // A refusal of the window releases its key: the same key goes once the
    // window opens.
    h.clock
        .set(time::OffsetDateTime::now_utc() + time::Duration::hours(25));
    let closed = h
        .call(reply(&key, PN, EXAMPLE_WA_ID, "order:2:reply", "Hello?"))
        .await;
    assert_eq!(closed.code(), "customer_service_window_closed");
    let reopened = dated(text(WABA, PN, "wamid.AGAIN"), now() + 25 * 3600);
    assert_eq!(h.webhook(&bytes(&reopened)).await.status, StatusCode::OK);
    h.clock.set(
        time::OffsetDateTime::now_utc() + time::Duration::hours(25) + time::Duration::seconds(2),
    );
    h.graph.push_json(200, accepted("wamid.REPLY-2"));
    let sent = h
        .call(reply(&key, PN, EXAMPLE_WA_ID, "order:2:reply", "Hello?"))
        .await;
    assert_eq!(sent.status, StatusCode::ACCEPTED, "{}", sent.text);
    assert_eq!(h.graph.requests().len(), 2);
    assert_eq!(h.graph.remaining(), 0);
}

/// Meta's `control_taken` example on the conversation.
fn control_taken(at: i64) -> Value {
    dated(
        json!({
            "object": "whatsapp_business_account",
            "entry": [{
                "id": WABA,
                "changes": [{
                    "field": "messaging_handovers",
                    "value": {
                        "messaging_product": "whatsapp",
                        "sender": {"phone_number": EXAMPLE_WA_ID},
                        "recipient": {"phone_number_id": PN, "display_phone_number": "15550783881"},
                        "type": "control_taken",
                        "timestamp": "1750101000",
                        "control_taken": {
                            "previous_owner_role": "customer_service",
                            "new_owner_role": "escalation",
                            "metadata": "Human agent stepping in"
                        }
                    }
                }]
            }]
        }),
        at,
    )
}

/// After `control_taken`, a free-form reply is `409
/// thread_owned_elsewhere` with zero requests (the error table's code, the
/// library's `ErrorKind::ThreadOwnedElsewhere`), and a template is not
/// refused; with `WA_SERVER_INBOX_REPLY_CHECKS` turning the number's
/// ownership check off (the escalation partner's number), the free-form
/// reply goes to Meta. Decisive: the ownership refusal's mapping, and the
/// setting reaching the number's inbox.
#[tokio::test]
async fn a_reply_while_another_app_owns_the_thread_is_409_unless_the_number_turns_the_check_off() {
    for (checks, refused) in [
        (None, true),
        (Some(ReplyChecks::ALL.thread_owner(false)), false),
    ] {
        let mut settings = test_settings();
        if let Some(checks) = checks {
            settings.inbox_reply_checks =
                ReplyChecksByNumber::new().with(PhoneNumberId::new(PN).unwrap(), checks);
        }
        let h = Harness::with_settings(settings);
        let key = with_a_conversation(&h).await;
        let taken = h.webhook(&bytes(&control_taken(sent_now() + 1))).await;
        assert_eq!(taken.status, StatusCode::OK, "{}", taken.text);
        h.clock
            .set(time::OffsetDateTime::now_utc() + time::Duration::seconds(3));
        if refused {
            let answer = h
                .call(reply(&key, PN, EXAMPLE_WA_ID, "k1", "Let me check"))
                .await;
            assert_eq!(
                (answer.status, answer.code().as_str()),
                (StatusCode::CONFLICT, "thread_owned_elsewhere"),
                "{}",
                answer.text
            );
            assert_eq!(answer.json()["error"]["may_have_been_sent"], false);
            assert!(h.graph.requests().is_empty(), "refused with zero requests");
            h.graph.push_json(200, accepted("wamid.TEMPLATE"));
            let template = h
                .call(
                    Call::new(
                        Method::POST,
                        format!("/v1/numbers/{PN}/conversations/{EXAMPLE_WA_ID}/messages"),
                    )
                    .key(&key)
                    .json(&json!({"type": "template", "template": {
                        "name": "order_update", "language": {"code": "en_US"}}})),
                )
                .await;
            assert_eq!(template.status, StatusCode::ACCEPTED, "{}", template.text);
        } else {
            h.graph.push_json(200, accepted("wamid.PARTNER"));
            let answer = h
                .call(reply(&key, PN, EXAMPLE_WA_ID, "k1", "Escalation here"))
                .await;
            assert_eq!(answer.status, StatusCode::ACCEPTED, "{}", answer.text);
        }
        assert_eq!(h.graph.remaining(), 0);
    }
}

/// The routes take the `inbox` scope (another key is `403 forbidden`, the
/// store unread), and another tenant's number is `404`.
#[tokio::test]
async fn the_routes_take_the_inbox_scope_on_the_tenants_own_number() {
    let h = Harness::new();
    with_a_conversation(&h).await;
    let other_scopes = h
        .tenant_key(
            "merchant-42",
            &[Scope::Send, Scope::Numbers, Scope::Events, Scope::Media],
        )
        .await;
    h.tenant("merchant-7").await;
    let stranger = h.tenant_key("merchant-7", &[Scope::Inbox]).await;
    for path in [
        format!("/v1/numbers/{PN}/conversations"),
        format!("/v1/numbers/{PN}/conversations/{EXAMPLE_WA_ID}/messages"),
    ] {
        let refused = h.call(Call::get(&path).key(&other_scopes)).await;
        assert_eq!(
            (refused.status, refused.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{path}"
        );
        let foreign = h.call(Call::get(&path).key(&stranger)).await;
        assert_eq!(
            (foreign.status, foreign.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{path}"
        );
    }
    let refused = h
        .call(reply(&other_scopes, PN, EXAMPLE_WA_ID, "k", "hi"))
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(h.graph.requests().is_empty());
}

/// Pages by an opaque cursor (`?limit=&cursor=`, `next_cursor`), as every
/// list of `/v1` but the events: no repeats, no gaps; a cursor the service
/// did not write and a limit out of range are `422` on their parameter; a
/// contact with a control character is `422 contact`; a body naming `to`
/// is `422 body` (the conversation names the recipient); a contact with
/// no conversation has an empty history.
#[tokio::test]
async fn pages_and_what_a_request_may_name() {
    let h = Harness::new();
    let key = with_a_conversation(&h).await;
    for (i, contact) in ["16505550001", "16505550002"].into_iter().enumerate() {
        let mut body = text(WABA, PN, &format!("wamid.PAGE-{i}"));
        body["entry"][0]["changes"][0]["value"]["messages"][0]["from"] = json!(contact);
        body["entry"][0]["changes"][0]["value"]["contacts"][0]["wa_id"] = json!(contact);
        assert_eq!(h.webhook(&bytes(&body)).await.status, StatusCode::OK);
    }
    let first = read(&h, &key, &format!("/v1/numbers/{PN}/conversations?limit=2")).await;
    assert_eq!(first["data"].as_array().unwrap().len(), 2);
    let cursor = first["next_cursor"].as_str().unwrap();
    let rest = read(
        &h,
        &key,
        &format!("/v1/numbers/{PN}/conversations?limit=2&cursor={cursor}"),
    )
    .await;
    assert_eq!(rest["next_cursor"], Value::Null);
    let mut contacts: Vec<String> = first["data"]
        .as_array()
        .unwrap()
        .iter()
        .chain(rest["data"].as_array().unwrap())
        .map(|c| c["contact"].as_str().unwrap().to_owned())
        .collect();
    contacts.sort();
    assert_eq!(contacts, ["16505550001", "16505550002", EXAMPLE_WA_ID]);
    for (query, field) in [
        ("limit=0", "limit"),
        ("limit=101", "limit"),
        ("cursor=zz", "cursor"),
        ("cursor=616263", "cursor"),
    ] {
        let refused = h
            .call(Call::get(format!("/v1/numbers/{PN}/conversations?{query}")).key(&key))
            .await;
        assert_eq!(
            (refused.status, refused.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{query}: {}",
            refused.text
        );
    }
    let control = h
        .call(Call::get(format!("/v1/numbers/{PN}/conversations/a%0Ab/messages")).key(&key))
        .await;
    assert_eq!(
        (control.status, control.json()["error"]["field"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("contact"))
    );
    let empty = read(
        &h,
        &key,
        &format!("/v1/numbers/{PN}/conversations/US.13491208655302741918/messages"),
    )
    .await;
    assert_eq!(empty, json!({"data": [], "next_cursor": null}));
    let with_to = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/numbers/{PN}/conversations/{EXAMPLE_WA_ID}/messages"),
            )
            .key(&key)
            .json(
                &json!({"to": {"phone": "+16505550000"}, "type": "text", "text": {"body": "hi"}}),
            ),
        )
        .await;
    assert_eq!(
        (with_to.status, with_to.json()["error"]["field"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("body"))
    );
    assert!(h.graph.requests().is_empty());
}
