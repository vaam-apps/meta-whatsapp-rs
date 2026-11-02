//! The erasure route's scenarios (roadmap M2f), run on memory
//! (`erasure.rs`) and on Postgres (`live_postgres.rs`, which sweeps every
//! table of the schema after them).

use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::store::ConversationKey;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use serde_json::{Value, json};

use super::meta::{
    EXAMPLE_BSUID, EXAMPLE_PN, EXAMPLE_WA_ID, EXAMPLE_WABA, bytes, dated, delivered, fixture, now,
    text, with_ids,
};
use super::{Call, Harness, Reply};

/// The merchant's token, in the vault.
pub const TOKEN: &str = "TOKEN-OF-MERCHANT-42";
/// The customer's parent BSUID (`business-scoped-user-ids`).
pub const PARENT_BSUID: &str = "US.ENT.11815799212886844830";
/// The customer's username, in the BSUID example.
pub const USERNAME: &str = "realsheenanelson";
/// The customer's name, in Meta's examples.
pub const NAME: &str = "Sheena Nelson";
/// Another customer of the number, who stays.
pub const OTHER: &str = "12125557890";
/// The name of [`OTHER`].
pub const OTHER_NAME: &str = "Jordan Other";
/// The merchant's second number, and another tenant's WABA and number.
pub const SECOND_PN: &str = "106540352242923";
pub const THEIR_WABA: &str = "102290129340399";
pub const THEIR_PN: &str = "106540352242924";

/// What names the customer: their phone number, BSUID, parent BSUID,
/// username and name. No row, log line, journal entry or audit event may
/// hold one after the erasure (but on the numbers it did not erase on).
pub fn needles() -> Vec<&'static str> {
    vec![EXAMPLE_WA_ID, EXAMPLE_BSUID, PARENT_BSUID, USERNAME, NAME]
}

/// Meta's text example on `pn` of `waba`, message `message_id`, from
/// `customer` named `name`.
pub fn text_from(waba: &str, pn: &str, message_id: &str, customer: &str, name: &str) -> Value {
    let mut payload = text(waba, pn, message_id);
    let value = &mut payload["entry"][0]["changes"][0]["value"];
    value["contacts"][0]["wa_id"] = json!(customer);
    value["contacts"][0]["profile"]["name"] = json!(name);
    value["messages"][0]["from"] = json!(customer);
    payload
}

/// Meta's BSUID text example (`business-scoped-user-ids`) on `pn` of
/// `waba`, message `wamid`, with the customer's phone number shared as
/// well (`from`, the contact's `wa_id`): the inbox links the two.
pub fn linked(waba: &str, pn: &str, wamid: &str) -> Value {
    let mut payload = with_ids(fixture("bsuid/text_username_no_wa_id.json"), waba, pn);
    let value = &mut payload["entry"][0]["changes"][0]["value"];
    value["contacts"][0]["wa_id"] = json!(EXAMPLE_WA_ID);
    value["messages"][0]["from"] = json!(EXAMPLE_WA_ID);
    value["messages"][0]["id"] = json!(wamid);
    payload
}

/// Meta's standby example (`webhooks/reference/standby`) on `pn` of
/// `waba`: the customer's message while another app owns the thread, an
/// operator-only row of the outbox (`standby_observed`) naming them.
pub fn standby(waba: &str, pn: &str) -> Value {
    with_ids(
        fixture("pages/webhooks.reference.standby__inbound_message.json"),
        waba,
        pn,
    )
}

/// `POST /v1/numbers/{pn}/erasures` with `key`, confirming `confirm`.
pub fn erase(key: &str, pn: &str, confirm: &str, contacts: &[&str]) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{pn}/erasures"))
        .key(key)
        .json(&json!({"confirm": confirm, "contacts": contacts}))
}

/// Deliver `payload`, which Meta signed: `200`.
pub async fn deliver(h: &Harness, payload: &Value) {
    let received = h.webhook(&bytes(payload)).await;
    assert_eq!(received.status, StatusCode::OK, "{}", received.text);
}

/// merchant-42 with Meta's example number, its keys for the inbox, the
/// events and erasure; returns `(inbox and events key, erasure key)`.
pub async fn merchant(h: &Harness) -> (String, String) {
    h.tenant("merchant-42").await;
    h.connect("merchant-42", EXAMPLE_WABA, &[EXAMPLE_PN, SECOND_PN], TOKEN)
        .await;
    let reader = h
        .tenant_key("merchant-42", &[Scope::Inbox, Scope::Events])
        .await;
    let eraser = h.tenant_key("merchant-42", &[Scope::Erasure]).await;
    (reader, eraser)
}

/// The customer on Meta's example number: a text under their phone
/// number, a text under their BSUID with their phone number shared, a
/// status of a message sent to them; and [`OTHER`]'s text.
pub async fn the_customer_writes(h: &Harness) {
    let (waba, pn) = (EXAMPLE_WABA, EXAMPLE_PN);
    deliver(h, &text(waba, pn, "wamid.ERASE-1")).await;
    deliver(h, &linked(waba, pn, "wamid.ERASE-2")).await;
    deliver(h, &delivered(waba, pn, "wamid.ERASE-OUT")).await;
    deliver(h, &text_from(waba, pn, "wamid.ERASE-3", OTHER, OTHER_NAME)).await;
}

/// The tenant's events of `pn`, as `GET /v1/events` answers them.
pub async fn events_of(h: &Harness, key: &str, pn: &str) -> Vec<Value> {
    let reply = h
        .call(Call::get(format!("/v1/events?phone_number_id={pn}&limit=100")).key(key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    reply.json()["data"].as_array().unwrap().clone()
}

/// The contacts of `pn`'s conversations, as the list answers them.
pub async fn conversations_of(h: &Harness, key: &str, pn: &str) -> Vec<String> {
    let reply = h
        .call(Call::get(format!("/v1/numbers/{pn}/conversations")).key(key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let mut contacts: Vec<String> = reply.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["contact"].as_str().unwrap().to_owned())
        .collect();
    contacts.sort();
    contacts
}

/// How many of `events` name one of [`needles`] anywhere.
pub fn naming(events: &[Value]) -> usize {
    events
        .iter()
        .filter(|event| {
            let text = event.to_string();
            needles().iter().any(|needle| text.contains(needle))
        })
        .count()
}

/// The answer of a successful erasure, checked: `200`, its id, the number,
/// a time; nothing else, nothing of the customer.
pub fn erased(reply: &Reply, pn: &str) -> String {
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let answer = reply.json();
    let id = answer["erasure_id"].as_str().unwrap().to_owned();
    assert!(
        id.len() == 36 && id.starts_with("era_") && id[4..].bytes().all(|b| b.is_ascii_hexdigit()),
        "{answer}"
    );
    assert_eq!(answer["phone_number_id"], pn, "{answer}");
    assert!(answer["erased_at"].is_string(), "{answer}");
    assert_eq!(answer.as_object().unwrap().len(), 3, "{answer}");
    for needle in needles() {
        assert!(!reply.text.contains(needle), "{}", reply.text);
    }
    id
}

/// The decisive scenario (roadmap M2f): the customer writes under their
/// phone number and their BSUID (and once in standby, an operator-only
/// row of the outbox); the merchant erases them by their BSUID alone.
/// Afterwards the inbox lists only the other customer, none of the
/// tenant's events names them, Meta was asked nothing; a repeat answers
/// the same. `live_postgres.rs` then sweeps every table: no row a tenant
/// sees names them, and the operator-only row still does (design D89,
/// narrow by default).
pub async fn an_erasure_by_bsuid_leaves_nothing_of_the_customer(h: &Harness) {
    let (reader, eraser) = merchant(h).await;
    the_customer_writes(h).await;
    deliver(h, &standby(EXAMPLE_WABA, EXAMPLE_PN)).await;
    let pn = EXAMPLE_PN;
    assert_eq!(
        conversations_of(h, &reader, pn).await,
        [OTHER, EXAMPLE_WA_ID, EXAMPLE_BSUID],
        "both threads, before"
    );
    assert_eq!(naming(&events_of(h, &reader, pn).await), 3, "before");
    let reply = h.call(erase(&eraser, pn, pn, &[EXAMPLE_BSUID])).await;
    let first = erased(&reply, pn);
    assert!(
        h.graph.requests().is_empty(),
        "an erasure asks Meta nothing"
    );
    assert_eq!(conversations_of(h, &reader, pn).await, [OTHER]);
    let events = events_of(h, &reader, pn).await;
    assert_eq!(naming(&events), 0, "{events:?}");
    assert_eq!(
        events.len(),
        1,
        "the other customer's event stays: {events:?}"
    );
    for contact in [EXAMPLE_WA_ID, EXAMPLE_BSUID] {
        let history = h
            .call(
                Call::get(format!("/v1/numbers/{pn}/conversations/{contact}/messages"))
                    .key(&reader),
            )
            .await;
        assert_eq!(history.json()["data"], json!([]), "{contact}");
    }
    // A repeat: harmless, the same answer.
    let again = h.call(erase(&eraser, pn, pn, &[EXAMPLE_BSUID])).await;
    assert_ne!(erased(&again, pn), first);
    assert_eq!(conversations_of(h, &reader, pn).await, [OTHER]);
    assert_eq!(events_of(h, &reader, pn).await.len(), 1);
}

/// What an erasure does not reach: the customer on the merchant's other
/// number, and on another tenant's number; and a tenant's key cannot erase
/// on another tenant's number (`404`, nothing erased), nor a key without
/// the scope (`403`).
pub async fn an_erasure_stays_on_its_number_and_tenant(h: &Harness) {
    let (reader, eraser) = merchant(h).await;
    the_customer_writes(h).await;
    deliver(h, &text(EXAMPLE_WABA, SECOND_PN, "wamid.ERASE-4")).await;
    h.tenant("merchant-43").await;
    h.connect("merchant-43", THEIR_WABA, &[THEIR_PN], "TOKEN-OF-43")
        .await;
    deliver(h, &text(THEIR_WABA, THEIR_PN, "wamid.ERASE-5")).await;
    let theirs = h
        .tenant_key(
            "merchant-43",
            &[Scope::Inbox, Scope::Events, Scope::Erasure],
        )
        .await;

    // Another tenant's key, every scope it may hold: A's number does not
    // exist for it.
    let refused = h
        .call(erase(&theirs, EXAMPLE_PN, EXAMPLE_PN, &[EXAMPLE_WA_ID]))
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "{}",
        refused.text
    );
    // A key of A's without the scope.
    let forbidden = h
        .call(erase(&reader, EXAMPLE_PN, EXAMPLE_PN, &[EXAMPLE_WA_ID]))
        .await;
    assert_eq!(
        (forbidden.status, forbidden.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden"),
        "{}",
        forbidden.text
    );
    assert_eq!(
        conversations_of(h, &reader, EXAMPLE_PN).await,
        [OTHER, EXAMPLE_WA_ID, EXAMPLE_BSUID],
        "a refused erasure erased"
    );
    assert_eq!(naming(&events_of(h, &reader, EXAMPLE_PN).await), 3);

    // A's erasure on its first number.
    let reply = h
        .call(erase(&eraser, EXAMPLE_PN, EXAMPLE_PN, &[EXAMPLE_WA_ID]))
        .await;
    erased(&reply, EXAMPLE_PN);
    assert_eq!(conversations_of(h, &reader, EXAMPLE_PN).await, [OTHER]);
    assert_eq!(naming(&events_of(h, &reader, EXAMPLE_PN).await), 0);
    // Its second number keeps the customer: an erasure is per number.
    assert_eq!(
        conversations_of(h, &reader, SECOND_PN).await,
        [EXAMPLE_WA_ID]
    );
    assert_eq!(naming(&events_of(h, &reader, SECOND_PN).await), 1);
    // And so does the other tenant.
    assert_eq!(
        conversations_of(h, &theirs, THEIR_PN).await,
        [EXAMPLE_WA_ID]
    );
    assert_eq!(naming(&events_of(h, &theirs, THEIR_PN).await), 1);
    // Their own erasure on A's number is still `404`, after A's.
    let refused = h
        .call(erase(&theirs, EXAMPLE_PN, EXAMPLE_PN, &[OTHER]))
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.text);
    assert_eq!(conversations_of(h, &reader, EXAMPLE_PN).await, [OTHER]);
    assert!(h.graph.requests().is_empty(), "Meta was asked");
}

/// Design D90, narrow by default: on a number whose WABA moved from
/// merchant-a to merchant-b, merchant-b's erasure of the customer who
/// wrote to both is `409 erasure_crosses_binding` (the library's erasure
/// has no time bound, so it would reach merchant-a's hidden records), and
/// erases nothing: merchant-a's hidden message and merchant-b's own stay,
/// and so do both tenants' events of the number. The answer quotes no
/// contact.
pub async fn an_erasure_on_a_moved_number_erases_nothing(h: &Harness) {
    let (waba, pn) = (EXAMPLE_WABA, EXAMPLE_PN);
    h.tenant("merchant-a").await;
    h.tenant("merchant-b").await;
    h.connect("merchant-a", waba, &[pn], "TOKEN-OF-A").await;
    let a_key = h
        .tenant_key("merchant-a", &[Scope::Inbox, Scope::Events])
        .await;
    // In the binding's own second, which a first onboarding keeps.
    deliver(h, &dated(text(waba, pn, "wamid.ERASE-MOVED-A"), now())).await;
    assert_eq!(naming(&events_of(h, &a_key, pn).await), 1);
    assert!(
        h.store
            .unbind_waba(&WabaId::new(waba).unwrap())
            .await
            .unwrap()
    );
    h.connect("merchant-b", waba, &[pn], "TOKEN-OF-B").await;
    let b_key = h
        .tenant_key("merchant-b", &[Scope::Inbox, Scope::Events])
        .await;
    let b_eraser = h.tenant_key("merchant-b", &[Scope::Erasure]).await;
    // After a move, an event of the binding's own second reaches nobody.
    deliver(h, &dated(text(waba, pn, "wamid.ERASE-MOVED-B"), now() + 1)).await;
    let key = ConversationKey::new(PhoneNumberId::new(pn).unwrap(), EXAMPLE_WA_ID);
    let stored = || async {
        h.conversations
            .messages(&key, None, 10)
            .await
            .unwrap()
            .len()
    };
    assert_eq!(stored().await, 2, "merchant-a's hidden, merchant-b's own");
    assert_eq!(conversations_of(h, &b_key, pn).await, [EXAMPLE_WA_ID]);
    let reply = h.call(erase(&b_eraser, pn, pn, &[EXAMPLE_WA_ID])).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "erasure_crosses_binding"),
        "{}",
        reply.text
    );
    for needle in needles() {
        assert!(!reply.text.contains(needle), "{}", reply.text);
    }
    assert_eq!(stored().await, 2, "an erasure reached the previous holder");
    assert_eq!(conversations_of(h, &b_key, pn).await, [EXAMPLE_WA_ID]);
    assert_eq!(naming(&events_of(h, &b_key, pn).await), 1);
    assert_eq!(
        naming(&events_of(h, &a_key, pn).await),
        1,
        "merchant-a's own stream"
    );
    assert!(h.graph.requests().is_empty(), "Meta was asked");
}
