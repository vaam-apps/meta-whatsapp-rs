//! The inbox routes' scenarios (roadmap M2a), run on memory (`inbox.rs`)
//! and on Postgres (`live_postgres.rs`): acceptance test M2.1 over HTTP,
//! and the binding epoch (decision D10).

use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::store::ConversationKey;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use serde_json::{Value, json};

use super::meta::{
    EXAMPLE_PN, EXAMPLE_TEXT, EXAMPLE_WA_ID, EXAMPLE_WABA, EXAMPLE_WAMID, bytes, dated, delivered,
    now, text,
};
use super::{Call, Harness};

/// The merchant's token, in the vault.
pub const TOKEN: &str = "TOKEN-OF-MERCHANT-42";
/// Where a reply to [`EXAMPLE_WA_ID`] goes: `+` and the `wa_id`.
pub const TO: &str = "+16505551234";

/// `messages/text-messages`, "Example response", for message `wamid`.
pub fn accepted(wamid: &str) -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": TO, "wa_id": EXAMPLE_WA_ID}],
        "messages": [{"id": wamid}]
    })
}

/// `GET path` with `key`: `200`, its JSON.
pub async fn read(h: &Harness, key: &str, path: &str) -> Value {
    let reply = h.call(Call::get(path).key(key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.text);
    reply.json()
}

/// A text reply to `contact` on `pn`, under `idempotency_key`.
pub fn reply(key: &str, pn: &str, contact: &str, idempotency_key: &str, body: &str) -> Call {
    Call::new(
        Method::POST,
        format!("/v1/numbers/{pn}/conversations/{contact}/messages"),
    )
    .key(key)
    .header("idempotency-key", idempotency_key)
    .json(&json!({"type": "text", "text": {"body": body}, "callback_data": "order:1234"}))
}

/// M2.1 over HTTP (docs/design/server.md, section 9): Meta's text example
/// received by merchant-42's number, then
///
/// 1. the conversation list, then its history, every field answered;
/// 2. a reply within the window: `202` with Meta's message id, Meta's
///    documented request with the merchant's token, recorded `accepted`,
///    newest first;
/// 3. Meta's `delivered` status: the reply is `delivered`;
/// 4. 25 hours later, a free-form reply is `409
///    customer_service_window_closed` with zero requests.
#[allow(clippy::too_many_lines)] // a scenario: it reads top to bottom
pub async fn m2_1(h: &Harness) {
    let (waba, pn) = (EXAMPLE_WABA, EXAMPLE_PN);
    h.tenant("merchant-42").await;
    h.connect("merchant-42", waba, &[pn], TOKEN).await;
    let key = h.tenant_key("merchant-42", &[Scope::Inbox]).await;
    let received = h.webhook(&bytes(&text(waba, pn, EXAMPLE_WAMID))).await;
    assert_eq!(received.status, StatusCode::OK, "{}", received.text);

    // 1. Listed, then read.
    let listed = read(h, &key, &format!("/v1/numbers/{pn}/conversations")).await;
    let conversation = &listed["data"][0];
    assert_eq!(listed["data"].as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed["next_cursor"], Value::Null);
    assert_eq!(conversation["contact"], EXAMPLE_WA_ID);
    assert_eq!(conversation["last_text"], EXAMPLE_TEXT);
    assert_eq!(conversation["unread"], 1);
    let sent_at = conversation["last_inbound_at"].as_str().unwrap().to_owned();
    assert_eq!(conversation["last_message_at"], sent_at.as_str());
    let history_path = format!("/v1/numbers/{pn}/conversations/{EXAMPLE_WA_ID}/messages");
    let history = read(h, &key, &history_path).await;
    assert_eq!(
        history,
        json!({
            "data": [{
                "id": EXAMPLE_WAMID,
                "direction": "inbound",
                "kind": "text",
                "text": EXAMPLE_TEXT,
                "payload": history["data"][0]["payload"].clone(),
                "status": "received",
                "timestamp": sent_at,
                "status_at": null,
                "error": null
            }],
            "next_cursor": null
        })
    );
    assert_eq!(
        history["data"][0]["payload"]["text"]["body"], EXAMPLE_TEXT,
        "the webhook's message object"
    );
    assert!(h.graph.requests().is_empty(), "reading asks Meta nothing");

    // 2. A reply within the window (the service's clock past the
    //    customer's message, dated the next second).
    h.clock
        .set(time::OffsetDateTime::now_utc() + time::Duration::seconds(2));
    h.graph.push_json(200, accepted("wamid.M21-REPLY"));
    let replied = h
        .call(reply(
            &key,
            pn,
            EXAMPLE_WA_ID,
            "order:1234:reply",
            "Yes: blue and green.",
        ))
        .await;
    assert_eq!(replied.status, StatusCode::ACCEPTED, "{}", replied.text);
    assert_eq!(
        replied.json(),
        json!({"message_id": "wamid.M21-REPLY",
               "contacts": [{"input": TO, "wa_id": EXAMPLE_WA_ID, "user_id": null,
                             "parent_user_id": null}]})
    );
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(requests[0].path(), format!("/v25.0/{pn}/messages"));
    assert_eq!(requests[0].bearer(), Some(TOKEN), "the merchant's token");
    assert_eq!(
        requests[0].json().unwrap(),
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": TO,
            "type": "text",
            "text": {"body": "Yes: blue and green."},
            "biz_opaque_callback_data": "order:1234"
        })
    );
    assert_eq!(h.graph.remaining(), 0);
    let history = read(h, &key, &history_path).await;
    let ids: Vec<&str> = history["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["wamid.M21-REPLY", EXAMPLE_WAMID], "newest first");
    assert_eq!(history["data"][0]["direction"], "outbound");
    assert_eq!(
        history["data"][0]["status"], "accepted",
        "recorded accepted"
    );
    assert_eq!(history["data"][0]["text"], "Yes: blue and green.");

    // 3. Meta's status moves it to `delivered`.
    let status = h
        .webhook(&bytes(&delivered(waba, pn, "wamid.M21-REPLY")))
        .await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.text);
    let history = read(h, &key, &history_path).await;
    assert_eq!(history["data"][0]["id"], "wamid.M21-REPLY");
    assert_eq!(history["data"][0]["status"], "delivered");
    assert!(history["data"][0]["status_at"].is_string());

    // 4. Outside the window: refused before any request.
    h.clock
        .set(time::OffsetDateTime::now_utc() + time::Duration::hours(25));
    let refused = h
        .call(reply(
            &key,
            pn,
            EXAMPLE_WA_ID,
            "order:1234:late",
            "Still there?",
        ))
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "customer_service_window_closed"),
        "{}",
        refused.text
    );
    assert_eq!(refused.json()["error"]["may_have_been_sent"], false);
    assert_eq!(h.graph.requests().len(), 1, "refused with zero requests");
}

/// The binding epoch over HTTP (decision D10; the decisive case of roadmap
/// M2a): merchant-a's customer writes; the WABA moves to merchant-b, whose
/// key then lists no conversation and reads no history on the number,
/// while the inbox keeps merchant-a's message (hidden, never purged); the
/// customer writes again, and merchant-b reads that message alone.
pub async fn a_moved_numbers_history_is_hidden_from_its_new_tenant(h: &Harness) {
    let (waba, pn) = (EXAMPLE_WABA, EXAMPLE_PN);
    h.tenant("merchant-a").await;
    h.tenant("merchant-b").await;
    h.connect("merchant-a", waba, &[pn], "TOKEN-OF-A").await;
    let a_key = h.tenant_key("merchant-a", &[Scope::Inbox]).await;
    // In the binding's own second, which a first onboarding keeps.
    let mut before = dated(text(waba, pn, "wamid.EPOCH-A"), now());
    before["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"] =
        json!("My address is 1 Hacker Way");
    assert_eq!(h.webhook(&bytes(&before)).await.status, StatusCode::OK);
    let conversations = format!("/v1/numbers/{pn}/conversations");
    let history = format!("/v1/numbers/{pn}/conversations/{EXAMPLE_WA_ID}/messages");
    let seen = read(h, &a_key, &conversations).await;
    assert_eq!(seen["data"].as_array().unwrap().len(), 1, "{seen}");

    // The move.
    assert!(
        h.store
            .unbind_waba(&WabaId::new(waba).unwrap())
            .await
            .unwrap()
    );
    h.connect("merchant-b", waba, &[pn], "TOKEN-OF-B").await;
    let b_key = h.tenant_key("merchant-b", &[Scope::Inbox]).await;
    let listed = read(h, &b_key, &conversations).await;
    assert_eq!(
        listed,
        json!({"data": [], "next_cursor": null}),
        "the previous holder's conversation is shown to the new one"
    );
    let read_history = read(h, &b_key, &history).await;
    assert_eq!(
        read_history,
        json!({"data": [], "next_cursor": null}),
        "the previous holder's history is shown to the new one"
    );
    let key = ConversationKey::new(PhoneNumberId::new(pn).unwrap(), EXAMPLE_WA_ID);
    assert_eq!(
        h.conversations
            .messages(&key, None, 10)
            .await
            .unwrap()
            .len(),
        1,
        "hidden, never purged"
    );
    let gone = h.call(Call::get(&conversations).key(&a_key)).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND, "{}", gone.text);

    // The customer writes to merchant-b (after a move, an event of the
    // binding's own second reaches nobody: the next one).
    let mut after = dated(text(waba, pn, "wamid.EPOCH-B"), now() + 1);
    after["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"] = json!("Hello again");
    assert_eq!(h.webhook(&bytes(&after)).await.status, StatusCode::OK);
    let listed = read(h, &b_key, &conversations).await;
    assert_eq!(listed["data"].as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed["data"][0]["last_text"], "Hello again");
    let read_history = read(h, &b_key, &history).await;
    let ids: Vec<&str> = read_history["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["wamid.EPOCH-B"],
        "merchant-b's own message, not merchant-a's"
    );
    assert!(
        !read_history.to_string().contains("1 Hacker Way"),
        "{read_history}"
    );
}

/// Roadmap M2h over HTTP: Meta migrated merchant-a's number to
/// merchant-b's WABA, neither WABA moving (merchant-b's bound before,
/// merchant-a's still its own). merchant-b's key lists no conversation and
/// reads no history of merchant-a's on the number (kept in the inbox),
/// cannot answer in free form the window merchant-a's customer opened
/// (`409 customer_service_window_closed`, zero requests), and reads its own
/// once the customer writes under its WABA, its `unread` its own.
pub async fn a_number_migrated_between_two_tenants_wabas_hides_its_history(h: &Harness) {
    const WABA_B: &str = "102290129340399";
    let (waba_a, pn) = (EXAMPLE_WABA, EXAMPLE_PN);
    h.tenant("merchant-a").await;
    h.tenant("merchant-b").await;
    h.connect("merchant-b", WABA_B, &[], "TOKEN-OF-B").await;
    h.connect("merchant-a", waba_a, &[pn], "TOKEN-OF-A").await;
    let a_key = h.tenant_key("merchant-a", &[Scope::Inbox]).await;
    let mut before = dated(text(waba_a, pn, "wamid.NMOVE-A"), now());
    before["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"] =
        json!("My address is 1 Hacker Way");
    assert_eq!(h.webhook(&bytes(&before)).await.status, StatusCode::OK);
    let conversations = format!("/v1/numbers/{pn}/conversations");
    let history = format!("/v1/numbers/{pn}/conversations/{EXAMPLE_WA_ID}/messages");
    let seen = read(h, &a_key, &conversations).await;
    assert_eq!(seen["data"].as_array().unwrap().len(), 1, "{seen}");

    // The migration: merchant-a's WABA no longer lists the number,
    // merchant-b's does.
    let a = meta_whatsapp_server::model::TenantId::parse("merchant-a").unwrap();
    h.store
        .bind_waba(&a, &WabaId::new(waba_a).unwrap(), &[])
        .await
        .unwrap();
    h.connect("merchant-b", WABA_B, &[pn], "TOKEN-OF-B").await;
    let b_key = h.tenant_key("merchant-b", &[Scope::Inbox]).await;
    assert_eq!(
        read(h, &b_key, &conversations).await,
        json!({"data": [], "next_cursor": null}),
        "the previous holder's conversation is shown to the new one"
    );
    assert_eq!(
        read(h, &b_key, &history).await,
        json!({"data": [], "next_cursor": null}),
        "the previous holder's history is shown to the new one"
    );
    let key = ConversationKey::new(PhoneNumberId::new(pn).unwrap(), EXAMPLE_WA_ID);
    assert_eq!(
        h.conversations
            .messages(&key, None, 10)
            .await
            .unwrap()
            .len(),
        1,
        "hidden, never purged"
    );
    let gone = h.call(Call::get(&conversations).key(&a_key)).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND, "{}", gone.text);
    // merchant-a's customer opened a window on the number minutes ago:
    // not merchant-b's conversation to continue in free form, refused
    // before any request.
    let refused = h
        .call(reply(&b_key, pn, EXAMPLE_WA_ID, "nmove:1", "Hello from B"))
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "customer_service_window_closed"),
        "{}",
        refused.text
    );
    assert!(h.graph.requests().is_empty(), "refused with zero requests");

    let mut after = dated(text(WABA_B, pn, "wamid.NMOVE-B"), now() + 1);
    after["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"] = json!("Hello again");
    assert_eq!(h.webhook(&bytes(&after)).await.status, StatusCode::OK);
    let listed = read(h, &b_key, &conversations).await;
    assert_eq!(listed["data"].as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(
        listed["data"][0]["unread"], 1,
        "merchant-b's unread message, not merchant-a's: {listed}"
    );
    let read_history = read(h, &b_key, &history).await;
    let ids: Vec<&str> = read_history["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["wamid.NMOVE-B"],
        "merchant-b's own message, not merchant-a's"
    );
    assert!(
        !read_history.to_string().contains("1 Hacker Way"),
        "{read_history}"
    );
}
