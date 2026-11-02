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
    // Roadmap M2g: the list's window, open for 24 hours from the message.
    assert_eq!(conversation["window"]["open"], true, "{listed}");
    let sent_at = conversation["last_inbound_at"].as_str().unwrap().to_owned();
    let closes =
        time::OffsetDateTime::parse(&sent_at, &time::format_description::well_known::Rfc3339)
            .unwrap()
            + time::Duration::hours(24);
    assert_eq!(
        conversation["window"]["closes_at"],
        closes
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    );
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

/// `POST …/{contact}/read` with `key`, `body` (none: an empty body) and
/// `idempotency_key` (none: no header).
pub fn mark_read(
    key: &str,
    pn: &str,
    contact: &str,
    body: Option<&Value>,
    idempotency_key: Option<&str>,
) -> Call {
    let mut call = Call::new(
        Method::POST,
        format!("/v1/numbers/{pn}/conversations/{contact}/read"),
    )
    .key(key);
    if let Some(body) = body {
        call = call.json(body);
    }
    if let Some(idempotency_key) = idempotency_key {
        call = call.header("idempotency-key", idempotency_key);
    }
    call
}

/// Roadmap M2g over HTTP, decisive: merchant-a's customer writes; the WABA
/// moves to merchant-b, whose key gets `404` for merchant-a's message by
/// id (the body of an id the number never had), a window neither open nor
/// dated, and a `204` for marking the conversation read with
/// `notify_customer` that sent Meta nothing; once the customer writes to
/// merchant-b, its message is found, its window open, and its receipt sent
/// with merchant-b's own token.
#[allow(clippy::too_many_lines)] // a scenario: it reads top to bottom
pub async fn the_previous_holders_message_window_and_receipt_never_reach_the_new_holder(
    h: &Harness,
) {
    let (waba, pn) = (EXAMPLE_WABA, EXAMPLE_PN);
    h.tenant("merchant-a").await;
    h.tenant("merchant-b").await;
    h.connect("merchant-a", waba, &[pn], "TOKEN-OF-A").await;
    let a_key = h.tenant_key("merchant-a", &[Scope::Inbox]).await;
    let mut before = dated(text(waba, pn, "wamid.M2G-A"), now());
    before["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"] =
        json!("My address is 1 Hacker Way");
    assert_eq!(h.webhook(&bytes(&before)).await.status, StatusCode::OK);
    let by_id = |id: &str| format!("/v1/numbers/{pn}/messages/{id}");
    let window = format!("/v1/numbers/{pn}/conversations/{EXAMPLE_WA_ID}/window");
    let theirs = read(h, &a_key, &by_id("wamid.M2G-A")).await;
    assert_eq!(theirs["contact"], EXAMPLE_WA_ID);
    assert_eq!(theirs["message"]["id"], "wamid.M2G-A");
    assert_eq!(read(h, &a_key, &window).await["open"], true, "A's window");

    // The move.
    assert!(
        h.store
            .unbind_waba(&WabaId::new(waba).unwrap())
            .await
            .unwrap()
    );
    h.connect("merchant-b", waba, &[pn], "TOKEN-OF-B").await;
    let b_key = h.tenant_key("merchant-b", &[Scope::Inbox]).await;
    let hidden = h.call(Call::get(by_id("wamid.M2G-A")).key(&b_key)).await;
    let missing = h.call(Call::get(by_id("wamid.NEVER")).key(&b_key)).await;
    assert_eq!(
        (hidden.status, hidden.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "the previous holder's message, shown to the new one: {}",
        hidden.text
    );
    let (mut hidden, mut missing) = (hidden.json(), missing.json());
    hidden["error"]["request_id"] = Value::Null;
    missing["error"]["request_id"] = Value::Null;
    assert_eq!(hidden, missing, "a hidden message looks like a missing one");
    assert!(!hidden.to_string().contains("Hacker Way"));
    assert_eq!(
        read(h, &b_key, &window).await,
        json!({"open": false, "closes_at": null}),
        "the previous holder's customer's window, shown to the new one"
    );
    let marked = h
        .call(mark_read(
            &b_key,
            pn,
            EXAMPLE_WA_ID,
            Some(&json!({"notify_customer": true})),
            Some("m2g:read:1"),
        ))
        .await;
    assert_eq!(marked.status, StatusCode::NO_CONTENT, "{}", marked.text);
    assert!(
        h.graph.requests().is_empty(),
        "a receipt for the previous holder's message: {:?}",
        h.graph.requests()
    );

    // The customer writes to merchant-b (after a move, an event of the
    // binding's own second reaches nobody: the next one).
    let at = now() + 1;
    let after = dated(text(waba, pn, "wamid.M2G-B"), at);
    assert_eq!(h.webhook(&bytes(&after)).await.status, StatusCode::OK);
    let ours = read(h, &b_key, &by_id("wamid.M2G-B")).await;
    assert_eq!(ours["message"]["direction"], "inbound");
    let shown = read(h, &b_key, &window).await;
    assert_eq!(shown["open"], true, "{shown}");
    let closes = time::OffsetDateTime::from_unix_timestamp(at + 24 * 3600)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    assert_eq!(shown["closes_at"], closes.as_str());
    let listed = read(h, &b_key, &format!("/v1/numbers/{pn}/conversations")).await;
    assert_eq!(listed["data"][0]["window"], shown, "the list's window");
    h.graph.push_json(200, json!({"success": true}));
    let marked = h
        .call(mark_read(
            &b_key,
            pn,
            EXAMPLE_WA_ID,
            Some(&json!({"notify_customer": true})),
            Some("m2g:read:2"),
        ))
        .await;
    assert_eq!(marked.status, StatusCode::NO_CONTENT, "{}", marked.text);
    let [receipt] = h.graph.requests().try_into().unwrap();
    assert_eq!(receipt.bearer(), Some("TOKEN-OF-B"), "merchant-b's token");
    assert_eq!(receipt.json().unwrap()["message_id"], "wamid.M2G-B");
    assert_eq!(h.graph.remaining(), 0);
}

/// Roadmap M2g over HTTP, decisive: the read route. Without a body, the
/// unread count is reset and Meta asked nothing; with `notify_customer`
/// and an `Idempotency-Key`, `unread` is 0 and Meta got one read receipt,
/// Meta's documented request with the merchant's token, for the latest
/// inbound message only (not the merchant's reply recorded after it); the
/// same key and body replay the `204` (`Idempotent-Replayed`) with no
/// second receipt, another body under it is `422 idempotency_key_reused`;
/// a body with an unknown field is `422 body`, with nothing reset.
#[allow(clippy::too_many_lines)] // a scenario: it reads top to bottom
pub async fn the_read_route_resets_unread_and_receipts_the_latest_inbound_message(h: &Harness) {
    let (waba, pn) = (EXAMPLE_WABA, EXAMPLE_PN);
    h.tenant("merchant-42").await;
    h.connect("merchant-42", waba, &[pn], TOKEN).await;
    let key = h.tenant_key("merchant-42", &[Scope::Inbox]).await;
    let at = now();
    for (i, wamid) in ["wamid.READ-1", "wamid.READ-2"].into_iter().enumerate() {
        let body = dated(text(waba, pn, wamid), at + i64::try_from(i).unwrap());
        assert_eq!(h.webhook(&bytes(&body)).await.status, StatusCode::OK);
    }
    h.clock
        .set(time::OffsetDateTime::now_utc() + time::Duration::seconds(5));
    let conversations = format!("/v1/numbers/{pn}/conversations");
    let unread = async || read(h, &key, &conversations).await["data"][0]["unread"].clone();
    assert_eq!(unread().await, 2);
    h.graph.push_json(200, accepted("wamid.READ-REPLY"));
    let replied = h
        .call(reply(&key, pn, EXAMPLE_WA_ID, "read:reply", "Checking"))
        .await;
    assert_eq!(replied.status, StatusCode::ACCEPTED, "{}", replied.text);

    // No body: reset, nothing asked.
    let plain = h.call(mark_read(&key, pn, EXAMPLE_WA_ID, None, None)).await;
    assert_eq!(plain.status, StatusCode::NO_CONTENT, "{}", plain.text);
    assert!(plain.text.is_empty());
    assert!(plain.headers.get("content-type").is_none());
    assert_eq!(unread().await, 0);
    assert_eq!(h.graph.requests().len(), 1, "no receipt asked");

    // A new message, then a body the route does not take.
    let later = dated(text(waba, pn, "wamid.READ-3"), at + 2);
    assert_eq!(h.webhook(&bytes(&later)).await.status, StatusCode::OK);
    assert_eq!(unread().await, 1);
    let unknown = h
        .call(mark_read(
            &key,
            pn,
            EXAMPLE_WA_ID,
            Some(&json!({"notify_customer": true, "typing_indicator": true})),
            None,
        ))
        .await;
    assert_eq!(
        (unknown.status, unknown.json()["error"]["field"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("body"))
    );
    assert_eq!(unread().await, 1, "nothing reset");

    // With notify_customer, under a key.
    let notify = json!({"notify_customer": true});
    h.graph.push_json(200, json!({"success": true}));
    let first = h
        .call(mark_read(
            &key,
            pn,
            EXAMPLE_WA_ID,
            Some(&notify),
            Some("ticket-881:read"),
        ))
        .await;
    assert_eq!(first.status, StatusCode::NO_CONTENT, "{}", first.text);
    assert_eq!(unread().await, 0);
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 2, "one read receipt");
    assert_eq!(requests[1].method, Method::POST);
    assert_eq!(requests[1].path(), format!("/v25.0/{pn}/messages"));
    assert_eq!(requests[1].bearer(), Some(TOKEN), "the merchant's token");
    assert_eq!(
        requests[1].json().unwrap(),
        json!({
            "messaging_product": "whatsapp",
            "status": "read",
            "message_id": "wamid.READ-3"
        }),
        "the latest inbound message only"
    );
    // The same request, the contact percent-encoded: replayed.
    let again = h
        .call(mark_read(
            &key,
            pn,
            "%31%36%35%30%35%35%35%31%32%33%34",
            Some(&notify),
            Some("ticket-881:read"),
        ))
        .await;
    assert_eq!(again.status, StatusCode::NO_CONTENT, "{}", again.text);
    assert_eq!(again.headers.get("idempotent-replayed").unwrap(), "true");
    assert!(again.headers.get("content-type").is_none());
    assert_eq!(h.graph.requests().len(), 2, "a second receipt was sent");
    let other = h
        .call(mark_read(
            &key,
            pn,
            EXAMPLE_WA_ID,
            None,
            Some("ticket-881:read"),
        ))
        .await;
    assert_eq!(
        (other.status, other.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused")
    );
    assert_eq!(h.graph.remaining(), 0);
}
