//! Roadmap L7's decisive tests: what `InboxSink` records besides messages
//! (the calls that reopen the customer service window, thread ownership
//! under Conversation Routing, the links between a customer's identities)
//! and what `Inbox` does with it. Each scenario runs on the memory store,
//! and live on Postgres (`live_postgres_*`: skipped unless
//! `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set; `META_WHATSAPP_RS_REQUIRE_LIVE=1`,
//! as in `just test-live`, turns the skip into a failure). Webhooks are
//! Meta's example payloads (`meta-whatsapp-webhooks/tests/fixtures`).

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "memory")]

use std::collections::BTreeSet;
use std::sync::Arc;

use meta_whatsapp_rs::adapters::store::MemoryConversationStore;
use meta_whatsapp_rs::client::messages::Text;
use meta_whatsapp_rs::core::clock::ManualClock;
use meta_whatsapp_rs::core::store::{ThreadOwner, WindowEventKind};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_rs::inbox::{ReplyChecks, is_thread_owned_elsewhere};
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::webhooks::WebhookPayload;
use pretty_assertions::assert_eq;
use serde_json::json;
use time::OffsetDateTime;

/// The business number, the customer's BSUID and phone number of Meta's
/// examples.
const PNID: &str = "106540352242922";
const BSUID: &str = "US.13491208655302741918";
const PHONE: &str = "16505551234";

/// `business-scoped-user-ids`, "User-initiated connected calls webhooks":
/// the customer calls, at 1750030073.
const USER_CALL: &str = include_str!(
    "../../meta-whatsapp-webhooks/tests/fixtures/pages/business-scoped-user-ids__user_initiated_connected_calls_webhooks.json"
);
/// `business-scoped-user-ids`, "Call permission request webhooks": the
/// customer's reply to a call permission request, at 1750030073, carrying
/// both `from` and `from_user_id`.
const PERMISSION_REPLY: &str = include_str!(
    "../../meta-whatsapp-webhooks/tests/fixtures/pages/business-scoped-user-ids__call_permission_request_webhooks.json"
);
/// `conversation-routing/thread-control`, `control_taken`, at 1750101000:
/// the customer is named by `sender.phone_number` only.
const CONTROL_TAKEN: &str = include_str!(
    "../../meta-whatsapp-webhooks/tests/fixtures/pages/conversation-routing.thread-control__control_taken.json"
);
/// `business-scoped-user-ids`, `user_id_update` webhooks: `US.1349…`
/// becomes `US.2083…`.
const USER_ID_UPDATE: &str = include_str!(
    "../../meta-whatsapp-webhooks/tests/fixtures/pages/business-scoped-user-ids__user_id_update_webhooks.json"
);
/// `webhooks/reference/messages/text`: a message from the phone number
/// alone, at 1749416383 (a thread from before BSUIDs).
const PHONE_TEXT: &str =
    include_str!("../../meta-whatsapp-webhooks/tests/fixtures/messages/text.json");

fn at(unix: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(unix).unwrap()
}

/// The `since` of the ownership a standby copy of `unix` records: 1 ms
/// before its second, so that a handover of that second is always later.
fn just_before(unix: i64) -> OffsetDateTime {
    at(unix) - time::Duration::milliseconds(1)
}

/// Deliver every event of a webhook body, as the handler would.
async fn deliver(store: &Arc<dyn ConversationStore>, body: &str) {
    let events = WebhookPayload::from_slice(body.as_bytes())
        .unwrap()
        .into_events();
    assert!(!events.is_empty(), "the fixture produced no events");
    let sink = InboxSink::new(store.clone());
    for event in events {
        sink.deliver(event).await.unwrap();
    }
}

/// The merchant's inbox on [`PNID`], replying through `transport`, at
/// `clock`'s time.
fn inbox(
    store: &Arc<dyn ConversationStore>,
    transport: &ScriptedTransport,
    clock: &ManualClock,
) -> Inbox {
    let client = Client::builder()
        .transport(transport.clone())
        .access_token("MERCHANT_TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap();
    Inbox::new(client, PNID, store.clone()).with_clock(Arc::new(clock.clone()))
}

/// The text reply of these tests.
fn text(body: &str) -> MessageContent {
    Text::new(body).into()
}

/// Meta's answer to a send.
fn accepted(id: &str) -> serde_json::Value {
    json!({"messaging_product": "whatsapp", "contacts": [{"input": BSUID, "user_id": BSUID}], "messages": [{"id": id}]})
}

/// The last request is the free-form reply `body` to the customer's BSUID,
/// with the merchant's token.
fn assert_sent_text(transport: &ScriptedTransport, body: &str) {
    let request = transport.last_request().unwrap();
    assert_eq!(request.path(), format!("/v25.0/{PNID}/messages"));
    assert_eq!(request.bearer(), Some("MERCHANT_TOKEN"));
    assert_eq!(
        request.json().unwrap(),
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "recipient": BSUID,
            "type": "text",
            "text": {"body": body}
        })
    );
}

/// After the customer's call, `Inbox::reply` sends a free-form reply it
/// refused before; the call is a window event, not a message.
async fn a_call_reopens_the_window(store: Arc<dyn ConversationStore>) {
    let clock = ManualClock::new(at(1_750_030_073 + 3_600));
    let transport = ScriptedTransport::new();
    let inbox = inbox(&store, &transport, &clock);
    let key = inbox.key(BSUID);

    let refused = inbox
        .reply(&key, text("Sorry we missed your call"))
        .await
        .unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::CustomerServiceWindowClosed);
    assert!(
        transport.requests().is_empty(),
        "refused before any request"
    );

    deliver(&store, USER_CALL).await;
    deliver(&store, USER_CALL).await; // Meta's retry
    let events = store.window_events(&key, None, 10).await.unwrap();
    assert_eq!(events.len(), 1, "one call, recorded once");
    assert_eq!(events[0].kind, WindowEventKind::CustomerCall);
    assert_eq!(events[0].id, "wacid.ABGGFjFVU2AfAgo6V-Hc5eCgK5Gh");
    assert_eq!(
        inbox.window(&key).await.unwrap().closes_at(),
        Some(at(1_750_030_073 + 86_400))
    );
    assert!(
        inbox.history(&key, None, 10).await.unwrap().is_empty(),
        "a call is not a message"
    );

    transport.push_json(200, accepted("wamid.CALLBACK"));
    inbox
        .reply(&key, text("Sorry we missed your call"))
        .await
        .unwrap();
    assert_sent_text(&transport, "Sorry we missed your call");
    assert_eq!(transport.remaining(), 0);
    let summary = inbox.conversations(None, 10).await.unwrap();
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].unread, 0, "the call is never unread");
    assert_eq!(summary[0].last_inbound_at, None, "nor an inbound message");
}

/// After `control_taken`, a reply is refused locally with zero requests;
/// the handover named the customer by phone number, and the phone-to-BSUID
/// link of their earlier message led it to their BSUID conversation. The
/// override lets the reply through; a template needs no ownership.
async fn control_taken_refuses_a_reply_locally(store: Arc<dyn ConversationStore>) {
    let clock = ManualClock::new(at(1_750_101_000 + 3_600));
    let transport = ScriptedTransport::new();
    let inbox = inbox(&store, &transport, &clock);
    let key = inbox.key(BSUID);
    deliver(&store, PERMISSION_REPLY).await;
    assert!(inbox.window_is_open(&key).await.unwrap());
    assert_eq!(
        inbox.thread_owner(&key).await.unwrap(),
        None,
        "no routing yet"
    );
    transport.push_json(200, accepted("wamid.BEFORE"));
    inbox.reply(&key, text("Before the take")).await.unwrap();
    assert_eq!(transport.requests().len(), 1);

    deliver(&store, CONTROL_TAKEN).await;
    let owner = inbox.thread_owner(&key).await.unwrap().unwrap();
    assert_eq!(owner.owner, ThreadOwner::AnotherApp);
    assert_eq!(owner.role.as_deref(), Some("escalation"));
    assert_eq!(owner.since, at(1_750_101_000));

    let refused = inbox.reply(&key, text("After the take")).await.unwrap_err();
    assert!(is_thread_owned_elsewhere(&refused), "{refused}");
    assert_eq!(transport.requests().len(), 1, "refused with zero requests");

    // A template needs no ownership.
    transport.push_json(200, accepted("wamid.TEMPLATE"));
    inbox
        .reply(&key, TemplateMessage::new("order_update", "en_US").into())
        .await
        .unwrap();
    assert_eq!(transport.requests().len(), 2);

    // The caller's explicit override of the local check.
    transport.push_json(200, accepted("wamid.OVERRIDE"));
    inbox
        .clone()
        .with_reply_checks(ReplyChecks::all().thread_owner(false))
        .reply(&key, text("Escalation partner here"))
        .await
        .unwrap();
    assert_sent_text(&transport, "Escalation partner here");
    assert_eq!(transport.remaining(), 0);
}

/// After a `user_id_update`, `Inbox::identities` of the new BSUID holds the
/// previous one.
async fn a_bsuid_change_links_the_two(store: Arc<dyn ConversationStore>) {
    let clock = ManualClock::new(at(1_750_030_073));
    let transport = ScriptedTransport::new();
    let inbox = inbox(&store, &transport, &clock);
    deliver(&store, USER_ID_UPDATE).await;
    deliver(&store, USER_ID_UPDATE).await; // Meta's retry
    let current = inbox.key("US.20837465019283746501");
    assert_eq!(
        inbox.identities(&current).await.unwrap(),
        BTreeSet::from([BSUID.to_owned(), "US.20837465019283746501".to_owned()])
    );
    let links = store.identity_links(&current).await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(
        (links[0].previous.as_str(), links[0].at),
        (BSUID, at(1_750_030_073))
    );
}

/// A phone-to-BSUID link recorded from an inbound message that carries both
/// makes an erasure over `Inbox::identities` reach the thread keyed by the
/// phone number from before BSUIDs.
async fn an_erasure_reaches_the_phone_keyed_thread(store: Arc<dyn ConversationStore>) {
    let clock = ManualClock::new(at(1_750_030_073));
    let transport = ScriptedTransport::new();
    let inbox = inbox(&store, &transport, &clock);
    deliver(&store, PHONE_TEXT).await; // keyed by the phone number
    deliver(&store, PERMISSION_REPLY).await; // keyed by the BSUID, carries both
    let (phone, bsuid) = (inbox.key(PHONE), inbox.key(BSUID));
    assert_eq!(inbox.history(&phone, None, 10).await.unwrap().len(), 1);
    assert_eq!(inbox.history(&bsuid, None, 10).await.unwrap().len(), 1);

    let ids: Vec<String> = inbox
        .identities(&bsuid)
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(ids, [PHONE, BSUID]);
    let erased = inbox.erase_all(&ids).await.unwrap();
    assert_eq!((erased.messages, erased.identity_links), (2, 1));
    assert!(inbox.history(&phone, None, 10).await.unwrap().is_empty());
    assert!(inbox.history(&bsuid, None, 10).await.unwrap().is_empty());
    assert!(inbox.conversations(None, 10).await.unwrap().is_empty());
}

/// `webhooks/reference/standby`, "Inbound message", at 1750101000: a copy
/// of the customer's message to another responder.
const STANDBY_MESSAGE: &str = include_str!(
    "../../meta-whatsapp-webhooks/tests/fixtures/pages/webhooks.reference.standby__inbound_message.json"
);

/// A customer's message seen in standby opens the window, is neither
/// history nor unread, and says another app owns the thread.
async fn a_standby_message_is_a_window_event(store: Arc<dyn ConversationStore>) {
    let clock = ManualClock::new(at(1_750_101_000 + 60));
    let transport = ScriptedTransport::new();
    let inbox = inbox(&store, &transport, &clock);
    let key = inbox.key(PHONE); // the copy carries no BSUID
    deliver(&store, STANDBY_MESSAGE).await;
    deliver(&store, STANDBY_MESSAGE).await; // Meta's retry
    let events = store.window_events(&key, None, 10).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, WindowEventKind::StandbyMessage);
    assert_eq!(
        events[0].id,
        "wamid.HBgLMTY1MDU1NTEyMzQVAgASGBQzRUIwQTk4NkNGMUZCMDZERTREMAA="
    );
    assert!(inbox.window_is_open(&key).await.unwrap());
    assert!(inbox.history(&key, None, 10).await.unwrap().is_empty());
    assert!(
        inbox.conversations(None, 10).await.unwrap().is_empty(),
        "no summary, no unread count"
    );
    let owner = inbox.thread_owner(&key).await.unwrap().unwrap();
    assert_eq!(
        (owner.owner, owner.since),
        (ThreadOwner::AnotherApp, just_before(1_750_101_000)),
        "that app owned the thread when the message reached it"
    );
    let refused = inbox.reply(&key, text("Hello")).await.unwrap_err();
    assert!(is_thread_owned_elsewhere(&refused), "{refused}");
    assert!(transport.requests().is_empty());
}

fn memory() -> Arc<dyn ConversationStore> {
    Arc::new(MemoryConversationStore::new())
}

#[tokio::test]
async fn memory_a_call_reopens_the_window() {
    a_call_reopens_the_window(memory()).await;
}

#[tokio::test]
async fn memory_a_standby_message_is_a_window_event() {
    a_standby_message_is_a_window_event(memory()).await;
}

#[tokio::test]
async fn memory_control_taken_refuses_a_reply_locally() {
    control_taken_refuses_a_reply_locally(memory()).await;
}

#[tokio::test]
async fn memory_a_bsuid_change_links_the_two() {
    a_bsuid_change_links_the_two(memory()).await;
}

#[tokio::test]
async fn memory_an_erasure_reaches_the_phone_keyed_thread() {
    an_erasure_reaches_the_phone_keyed_thread(memory()).await;
}

/// The rules one by one, on the memory store.
mod rules {
    use meta_whatsapp_rs::client::messages::{DirectSendCategory, OutboundMessage};
    use meta_whatsapp_rs::core::store::{
        IdentityLink, StoredContact, ThreadOwnership, WindowEvent,
    };
    use pretty_assertions::assert_eq;
    use serde_json::Value;

    use super::*;

    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../meta-whatsapp-webhooks/tests/fixtures/"
    );

    /// A fixture's body, with each `(from, to)` replaced once in order.
    fn fixture(path: &str, edits: &[(&str, &str)]) -> String {
        let mut body = std::fs::read_to_string(format!("{FIXTURES}{path}")).unwrap();
        for (from, to) in edits {
            assert!(body.contains(from), "{path} has no {from}");
            body = body.replacen(from, to, 1);
        }
        body
    }

    /// A webhook body of one change.
    fn change(field: &str, value: &Value) -> String {
        json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": field, "value": value}]}]})
        .to_string()
    }

    async fn events_of(store: &Arc<dyn ConversationStore>, contact: &str) -> Vec<WindowEvent> {
        store
            .window_events(&ConversationKey::new(PNID, contact), None, 10)
            .await
            .unwrap()
    }

    async fn every_event(store: &Arc<dyn ConversationStore>) -> usize {
        let mut n = 0;
        for contact in [BSUID, PHONE, "16315553602", "16315553601", "13175551399"] {
            n += events_of(store, contact).await.len();
        }
        n
    }

    async fn links_of(store: &Arc<dyn ConversationStore>, contact: &str) -> Vec<(String, String)> {
        store
            .identity_links(&ConversationKey::new(PNID, contact))
            .await
            .unwrap()
            .into_iter()
            .map(|l| (l.previous, l.current))
            .collect()
    }

    fn pair(previous: &str, current: &str) -> (String, String) {
        (previous.to_owned(), current.to_owned())
    }

    async fn owner_of(
        store: &Arc<dyn ConversationStore>,
        contact: &str,
    ) -> Option<ThreadOwnership> {
        store
            .thread_owner(&ConversationKey::new(PNID, contact))
            .await
            .unwrap()
    }

    // ─── Calls ───────────────────────────────────────────────────────────

    /// `calling/pricing`: the customer's call, answered or not, and the
    /// customer accepting the business's call; nothing else.
    #[tokio::test]
    async fn which_call_webhooks_open_the_window() {
        let store = memory();
        // The customer's call ended: at the time it was picked up.
        deliver(
            &store,
            &fixture(
                "pages/business-scoped-user-ids__user_initiated_terminated_calls_webhooks.json",
                &[],
            ),
        )
        .await;
        let events = events_of(&store, BSUID).await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            (events[0].kind.clone(), events[0].at),
            (WindowEventKind::CustomerCall, at(1_750_029_953))
        );

        // An unanswered call of the customer: at the terminate's time.
        let store = memory();
        let unanswered = change(
            "calls",
            &json!({"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "calls": [{"id": "wacid.MISSED", "from": PHONE, "from_user_id": BSUID,
                    "to": "15550783881", "event": "terminate", "direction": "USER_INITIATED",
                    "timestamp": "1750030100", "status": "FAILED"}]}),
        );
        deliver(&store, &unanswered).await;
        let events = events_of(&store, BSUID).await;
        assert_eq!(
            (events.len(), events[0].at, events[0].id.as_str()),
            (1, at(1_750_030_100), "wacid.MISSED")
        );

        // A SIP call the customer placed (`calling/sip`): its call_created.
        let store = memory();
        deliver(
            &store,
            &fixture(
                "fields/calls_created.json",
                &[("BUSINESS_INITIATED", "USER_INITIATED")],
            ),
        )
        .await;
        assert_eq!(
            events_of(&store, BSUID).await[0].kind,
            WindowEventKind::CustomerCall
        );

        // The customer accepted the business's call.
        let store = memory();
        let status =
            "pages/business-scoped-user-ids__business_initiated_calls_status_webhooks.json";
        deliver(&store, &fixture(status, &[])).await; // RINGING
        deliver(&store, &fixture(status, &[("RINGING", "REJECTED")])).await;
        assert_eq!(
            every_event(&store).await,
            0,
            "ringing and rejected open nothing"
        );
        deliver(&store, &fixture(status, &[("RINGING", "ACCEPTED")])).await;
        let events = events_of(&store, BSUID).await;
        assert_eq!(
            (events.len(), events[0].kind.clone(), events[0].at),
            (1, WindowEventKind::CallAccepted, at(1_750_030_073))
        );

        // A business-initiated call picked up: its terminate's start_time.
        let store = memory();
        deliver(
            &store,
            &fixture("pages/calling.reference__call_terminate_webhook.json", &[]),
        )
        .await;
        let events = store
            .window_events(&ConversationKey::new("105615555715855", BSUID), None, 10)
            .await
            .unwrap();
        assert_eq!(
            (events.len(), events[0].kind.clone(), events[0].at),
            (1, WindowEventKind::CallAccepted, at(1_671_644_824))
        );

        // Nothing else.
        let store = memory();
        for path in [
            // The business's call, before the customer answers.
            "pages/business-scoped-user-ids__business_initiated_connected_calls_webhooks.json",
            // The business's call, never picked up (no start_time).
            "pages/business-scoped-user-ids__business_initiated_terminated_calls_webhooks.json",
            // A connect without a direction.
            "pages/calling.user-initiated-calls__part_1_user_calls_business.json",
            "fields/calls_created.json",
            "fields/calls_recording_available.json",
            "fields/calls_transcription_available.json",
        ] {
            deliver(&store, &fixture(path, &[])).await;
        }
        assert_eq!(every_event(&store).await, 0);
        assert!(
            store
                .window_events(&ConversationKey::new("436666719526789", BSUID), None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Without a BSUID, a call is keyed by the customer's phone number:
    /// the contact's `wa_id`, else whichever of `from` and `to` is not the
    /// business number.
    #[tokio::test]
    async fn a_call_without_a_bsuid_is_keyed_by_the_customers_number() {
        let call = |id: &str, from: &str, to: &str| {
            change(
                "calls",
                &json!({"messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "calls": [{"id": id, "from": from, "to": to, "event": "connect",
                        "direction": "USER_INITIATED", "timestamp": "1750030073"}]}),
            )
        };
        let store = memory();
        deliver(&store, &call("wacid.1", PHONE, "15550783881")).await;
        deliver(&store, &call("wacid.2", "+15550783881", "+16505559876")).await;
        assert_eq!(events_of(&store, PHONE).await.len(), 1);
        assert_eq!(events_of(&store, "16505559876").await.len(), 1);
        assert!(events_of(&store, "15550783881").await.is_empty());
        // No usable id at all: acknowledged, not recorded.
        deliver(&store, &call("wacid.3", "15550783881", "")).await;
        deliver(&store, &call("wacid.\u{0}4", PHONE, "15550783881")).await;
        assert_eq!(events_of(&store, PHONE).await.len(), 1);
        assert!(events_of(&store, "").await.is_empty());
    }

    /// The window check reads window events.
    #[tokio::test]
    async fn the_window_opens_from_the_latest_of_messages_and_window_events() {
        let store = memory();
        let clock = ManualClock::new(at(1_750_030_073 + 3_600));
        let transport = ScriptedTransport::new();
        let inbox = inbox(&store, &transport, &clock);
        let key = inbox.key(BSUID);
        deliver(&store, PERMISSION_REPLY).await; // inbound at 1750030073
        let event = |id: &str, unix: i64| WindowEvent {
            conversation: key.clone(),
            kind: WindowEventKind::Other("future".to_owned()),
            id: id.to_owned(),
            at: at(unix),
        };
        store
            .record_window_event(event("old", 1_000))
            .await
            .unwrap();
        assert_eq!(
            inbox.window(&key).await.unwrap().closes_at(),
            Some(at(1_750_030_073 + 86_400)),
            "an older event changes nothing"
        );
        store
            .record_window_event(event("new", 1_750_040_000))
            .await
            .unwrap();
        assert_eq!(
            inbox.window(&key).await.unwrap().closes_at(),
            Some(at(1_750_040_000 + 86_400))
        );
        clock.set(at(1_750_040_000 + 86_400 - 1));
        assert!(inbox.window_is_open(&key).await.unwrap());
        clock.set(at(1_750_040_000 + 86_400));
        assert!(!inbox.window_is_open(&key).await.unwrap());
    }

    // ─── Standby ─────────────────────────────────────────────────────────

    /// Standby echoes and receipts, and a revoke, record nothing; a group's
    /// copy opens the group's window but records no owner.
    #[tokio::test]
    async fn only_a_customers_standby_message_counts() {
        let store = memory();
        for path in [
            "pages/webhooks.reference.standby__text_message_echo.json",
            "pages/webhooks.reference.standby__template_message_echo.json",
            "pages/webhooks.reference.standby__status_receipt.json",
        ] {
            deliver(&store, &fixture(path, &[])).await;
        }
        let revoke = change(
            "standby",
            &json!({"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "standby": {"messages": [{"from": PHONE, "id": "wamid.R", "timestamp": "1750101000",
                    "type": "revoke", "revoke": {"original_message_id": "wamid.X"}}]}}),
        );
        deliver(&store, &revoke).await;
        assert_eq!(every_event(&store).await, 0);
        assert_eq!(owner_of(&store, PHONE).await, None);

        let group = change(
            "standby",
            &json!({"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "standby": {"messages": [{"from": PHONE, "id": "wamid.G", "timestamp": "1750101000",
                    "group_id": "HBgGROUP", "type": "text", "text": {"body": "hi all"}}]}}),
        );
        deliver(&store, &group).await;
        assert_eq!(events_of(&store, "HBgGROUP").await.len(), 1);
        assert_eq!(owner_of(&store, "HBgGROUP").await, None);
    }

    /// A handover is the stronger signal: a standby copy of its second or
    /// earlier never overrides it; a later one does, keeping the role
    /// another app's handover named.
    #[tokio::test]
    async fn a_standby_copy_never_overrides_a_handover_of_its_second() {
        let store = memory();
        deliver(
            &store,
            &fixture(
                "pages/conversation-routing.thread-control__control_passed.json",
                &[],
            ),
        )
        .await;
        deliver(&store, STANDBY_MESSAGE).await; // same second, 1750101000
        assert_eq!(
            owner_of(&store, PHONE).await.unwrap().owner,
            ThreadOwner::ThisApp
        );

        let store = memory();
        deliver(&store, CONTROL_TAKEN).await; // 1750101000, role escalation
        deliver(
            &store,
            &fixture(
                "pages/webhooks.reference.standby__inbound_message.json",
                &[("1750101000", "1750101060")],
            ),
        )
        .await;
        let owner = owner_of(&store, PHONE).await.unwrap();
        assert_eq!(
            (owner.owner, owner.role.as_deref(), owner.since),
            (
                ThreadOwner::AnotherApp,
                Some("escalation"),
                just_before(1_750_101_060)
            )
        );

        // The other order: the copy first, then the handover of its second.
        let store = memory();
        deliver(&store, STANDBY_MESSAGE).await; // 1750101000
        deliver(
            &store,
            &fixture(
                "pages/conversation-routing.thread-control__control_passed.json",
                &[],
            ),
        )
        .await;
        assert_eq!(
            owner_of(&store, PHONE).await.unwrap(),
            ThreadOwnership {
                owner: ThreadOwner::ThisApp,
                role: Some("escalation".to_owned()),
                app_id: None,
                since: at(1_750_101_000),
            }
        );

        // A message on `messages` of a copy's second reached this app: it
        // owns the thread, and the merchant may answer.
        let store = memory();
        let clock = ManualClock::new(at(1_750_101_000 + 60));
        let transport = ScriptedTransport::new();
        let inbox = inbox(&store, &transport, &clock);
        deliver(&store, STANDBY_MESSAGE).await; // 1750101000
        deliver(
            &store,
            &fixture("messages/text.json", &[("1749416383", "1750101000")]),
        )
        .await;
        let owner = inbox
            .thread_owner(&inbox.key(PHONE))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (owner.owner, owner.since),
            (ThreadOwner::ThisApp, at(1_750_101_000))
        );

        // After control_passed to this app, a later copy: another app again,
        // without the role this app held.
        let store = memory();
        deliver(
            &store,
            &fixture(
                "pages/conversation-routing.thread-control__control_passed.json",
                &[],
            ),
        )
        .await;
        deliver(
            &store,
            &fixture(
                "pages/webhooks.reference.standby__inbound_message.json",
                &[("1750101000", "1750101060")],
            ),
        )
        .await;
        let owner = owner_of(&store, PHONE).await.unwrap();
        assert_eq!((owner.owner, owner.role), (ThreadOwner::AnotherApp, None));
    }

    // ─── Handovers ───────────────────────────────────────────────────────

    /// `control_passed` records this app, with the new owner's role; with
    /// no link and no contact, under the phone number.
    #[tokio::test]
    async fn control_passed_records_this_app_under_the_phone_number() {
        let store = memory();
        deliver(
            &store,
            &fixture(
                "pages/webhooks.reference.messaging-handovers__control_passed.json",
                &[],
            ),
        )
        .await;
        let owner = owner_of(&store, PHONE).await.unwrap();
        assert_eq!(
            owner,
            ThreadOwnership {
                owner: ThreadOwner::ThisApp,
                role: Some("escalation".to_owned()),
                app_id: None,
                since: at(1_750_101_000),
            }
        );
        let taken = change(
            "messaging_handovers",
            &json!({"messaging_product": "whatsapp", "sender": {"phone_number": PHONE},
                "recipient": {"phone_number_id": PNID, "display_phone_number": "15550783881"},
                "type": "control_taken", "timestamp": "1750101100",
                "control_taken": {"new_owner_role": "escalation", "new_owner_app_id": "42"}}),
        );
        deliver(&store, &taken).await;
        let owner = owner_of(&store, PHONE).await.unwrap();
        assert_eq!(
            (
                owner.owner,
                owner
                    .app_id
                    .as_ref()
                    .map(meta_whatsapp_rs::core::ids::AppId::as_str)
            ),
            (ThreadOwner::AnotherApp, Some("42"))
        );
    }

    /// A handover follows the identity links from its phone number: the
    /// latest link it is the `previous` of, then on from there.
    #[tokio::test]
    async fn a_handover_follows_the_links_to_the_current_bsuid() {
        let store = memory();
        let link = |previous: &str, current: &str, unix: i64| {
            IdentityLink::new(PNID, previous, current, at(unix))
        };
        for l in [
            link(PHONE, "US.1", 100),
            link(PHONE, "US.2", 200), // the latest from the phone number
            link("US.1", "US.9", 300),
            link("US.2", "US.3", 250), // then the BSUID changed
            link("US.3", "US.2", 260), // a loop stops where it closes
            link("US.0", PHONE, 400),  // not from the phone number
        ] {
            store.link_identity(l).await.unwrap();
        }
        deliver(&store, CONTROL_TAKEN).await;
        for contact in [PHONE, "US.1", "US.2", "US.9", "US.0"] {
            assert_eq!(owner_of(&store, contact).await, None, "{contact}");
        }
        assert_eq!(
            owner_of(&store, "US.3").await.unwrap().owner,
            ThreadOwner::AnotherApp
        );
    }

    /// With no link from the phone number, a synced address book contact
    /// with that number leads to its BSUID.
    #[tokio::test]
    async fn a_handover_finds_the_bsuid_of_a_synced_contact() {
        let store = memory();
        let contact = |key: &str, phone: &str, user: &str, unix: i64| StoredContact {
            key: ConversationKey::new(PNID, key),
            full_name: None,
            first_name: None,
            phone_number: Some(phone.to_owned()),
            user_id: Some(UserId::new(user)),
            parent_user_id: None,
            username: None,
            synced_at: at(unix),
        };
        // Two contacts with the number (an earlier BSUID's too): the latest synced.
        store
            .put_contact(contact(BSUID, PHONE, BSUID, 100))
            .await
            .unwrap();
        store
            .put_contact(contact("US.OLD", PHONE, "US.OLD", 50))
            .await
            .unwrap();
        deliver(&store, CONTROL_TAKEN).await;
        assert_eq!(owner_of(&store, PHONE).await, None);
        assert_eq!(owner_of(&store, "US.OLD").await, None);
        assert_eq!(
            owner_of(&store, BSUID).await.unwrap().owner,
            ThreadOwner::AnotherApp
        );
    }

    /// A handover without the customer's number, or of an unknown type,
    /// records nothing and does not fail the delivery.
    #[tokio::test]
    async fn a_handover_without_a_number_records_nothing() {
        let store = memory();
        let handover = |sender: Value, kind: &str| {
            let mut value = json!({"messaging_product": "whatsapp", "sender": sender,
                "recipient": {"phone_number_id": PNID, "display_phone_number": "15550783881"},
                "type": kind, "timestamp": "1750101000"});
            value[kind] = json!({});
            change("messaging_handovers", &value)
        };
        deliver(&store, &handover(json!({}), "control_taken")).await;
        deliver(
            &store,
            &handover(json!({"phone_number": ""}), "control_taken"),
        )
        .await;
        deliver(
            &store,
            &handover(json!({"phone_number": PHONE}), "control_shared"),
        )
        .await;
        assert_eq!(owner_of(&store, PHONE).await, None);
        assert_eq!(owner_of(&store, "").await, None);
    }

    // ─── Who owns the thread now ─────────────────────────────────────────

    /// A message on `messages` after the record: this app owns the thread
    /// (not one of the record's second); 24 hours after the latest
    /// activity: idle.
    #[tokio::test]
    async fn ownership_follows_messages_and_the_idle_timeout() {
        let store = memory();
        let clock = ManualClock::new(at(1_750_101_000 + 60));
        let transport = ScriptedTransport::new();
        let inbox = inbox(&store, &transport, &clock);
        let key = inbox.key(PHONE);
        deliver(&store, CONTROL_TAKEN).await;
        let message = |id: &str, unix: i64| {
            change(
                "messages",
                &json!({"messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "messages": [{"from": PHONE, "id": id, "timestamp": unix.to_string(),
                        "type": "text", "text": {"body": "hi"}}]}),
            )
        };
        deliver(&store, &message("wamid.same", 1_750_101_000)).await;
        assert_eq!(
            inbox.thread_owner(&key).await.unwrap().unwrap().owner,
            ThreadOwner::AnotherApp,
            "a message of the handover's second does not win"
        );
        deliver(&store, &message("wamid.after", 1_750_101_030)).await;
        let owner = inbox.thread_owner(&key).await.unwrap().unwrap();
        assert_eq!(
            (owner.owner, owner.role, owner.since),
            (ThreadOwner::ThisApp, None, at(1_750_101_030))
        );
        clock.set(at(1_750_101_030 + 86_400 - 1));
        assert_eq!(
            inbox.thread_owner(&key).await.unwrap().unwrap().owner,
            ThreadOwner::ThisApp
        );
        clock.set(at(1_750_101_030 + 86_400));
        assert_eq!(
            inbox.thread_owner(&key).await.unwrap().unwrap(),
            ThreadOwnership {
                owner: ThreadOwner::Idle,
                role: None,
                app_id: None,
                since: at(1_750_101_030 + 86_400),
            }
        );

        // Another app's thread goes idle 24 hours after the handover when
        // no later message is known; an idle thread is not refused.
        let store = memory();
        let only_owner = super::inbox(&store, &transport, &clock)
            .with_reply_checks(ReplyChecks::all().window(false));
        deliver(&store, CONTROL_TAKEN).await;
        clock.set(at(1_750_101_000 + 86_400 - 1));
        assert_eq!(
            only_owner.thread_owner(&key).await.unwrap().unwrap().owner,
            ThreadOwner::AnotherApp
        );
        let refused = only_owner.reply(&key, text("hi")).await.unwrap_err();
        assert!(is_thread_owned_elsewhere(&refused), "{refused}");
        clock.set(at(1_750_101_000 + 86_400));
        assert_eq!(
            only_owner.thread_owner(&key).await.unwrap().unwrap().owner,
            ThreadOwner::Idle
        );
        transport.push_json(200, accepted("wamid.T"));
        only_owner.reply(&key, text("hi")).await.unwrap();
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(transport.remaining(), 0);
    }

    /// `conversation-routing/calling-webhooks`: a customer's answer to a
    /// call permission request goes to the Incoming Call primary and the
    /// standby partners, on `messages`, and "does not change thread
    /// ownership": receiving one does not make this app the owner. A
    /// message routing delivered after it does.
    #[tokio::test]
    async fn a_call_permission_reply_does_not_claim_the_thread() {
        call_permission_reply_scenario(memory()).await;
    }

    /// [`a_call_permission_reply_does_not_claim_the_thread`], on `store`
    /// (the rule reads the stored payload back: live on Postgres too).
    pub(super) async fn call_permission_reply_scenario(store: Arc<dyn ConversationStore>) {
        let clock = ManualClock::new(at(1_750_101_000 + 600));
        let transport = ScriptedTransport::new();
        let inbox = inbox(&store, &transport, &clock);
        let key = inbox.key(BSUID);
        deliver(&store, PERMISSION_REPLY).await; // links the phone number to the BSUID
        deliver(&store, CONTROL_TAKEN).await; // 1750101000
        let answer = fixture(
            "pages/business-scoped-user-ids__call_permission_request_webhooks.json",
            &[
                (
                    "wamid.HBgLMTY1MDM4Nzk0MzkVAgASGBQzQUFERjg0NDEzNDdFODU3MUMxMAA=",
                    "wamid.PERMISSION.2",
                ),
                ("1750030073", "1750101300"),
            ],
        );
        deliver(&store, &answer).await;
        assert_eq!(
            store.last_inbound_at(&key).await.unwrap(),
            Some(at(1_750_101_300)),
            "recorded, and it opens the window"
        );
        let owner = inbox.thread_owner(&key).await.unwrap().unwrap();
        assert_eq!(
            (owner.owner, owner.since),
            (ThreadOwner::AnotherApp, at(1_750_101_000))
        );
        let refused = inbox
            .reply(&key, text("Calling you now"))
            .await
            .unwrap_err();
        assert!(is_thread_owned_elsewhere(&refused), "{refused}");
        assert!(transport.requests().is_empty());

        let message = change(
            "messages",
            &json!({"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "messages": [{"from": PHONE, "from_user_id": BSUID, "id": "wamid.BACK",
                    "timestamp": "1750101400", "type": "text", "text": {"body": "back to you"}}]}),
        );
        deliver(&store, &message).await;
        let owner = inbox.thread_owner(&key).await.unwrap().unwrap();
        assert_eq!(
            (owner.owner, owner.since),
            (ThreadOwner::ThisApp, at(1_750_101_400))
        );
    }

    /// This app's own `release`: idle from the inbox's clock, until the
    /// customer's next message; a key of another number is refused.
    #[tokio::test]
    async fn record_release_makes_the_thread_idle() {
        let store = memory();
        let clock = ManualClock::new(at(1_750_101_000 + 60));
        let transport = ScriptedTransport::new();
        let inbox = inbox(&store, &transport, &clock);
        let key = inbox.key(PHONE);
        deliver(
            &store,
            &fixture(
                "pages/webhooks.reference.messaging-handovers__control_passed.json",
                &[],
            ),
        )
        .await;
        assert!(inbox.record_release(&key).await.unwrap());
        assert_eq!(
            inbox.thread_owner(&key).await.unwrap().unwrap(),
            ThreadOwnership {
                owner: ThreadOwner::Idle,
                role: None,
                app_id: None,
                since: at(1_750_101_060),
            }
        );
        let foreign = ConversationKey::new("999", PHONE);
        assert!(inbox.record_release(&foreign).await.is_err());
        assert!(inbox.thread_owner(&foreign).await.is_err());
        assert_eq!(owner_of_number(&store, "999").await, None);
    }

    async fn owner_of_number(
        store: &Arc<dyn ConversationStore>,
        number: &str,
    ) -> Option<ThreadOwnership> {
        store
            .thread_owner(&ConversationKey::new(number, PHONE))
            .await
            .unwrap()
    }

    // ─── The local checks ────────────────────────────────────────────────

    /// Templates and Direct Send `utility` / `authentication` need no
    /// ownership; a `service` category does. `ReplyChecks` turns the window
    /// check off too.
    #[tokio::test]
    async fn what_the_ownership_check_exempts_and_the_window_override() {
        let store = memory();
        let clock = ManualClock::new(at(1_750_101_000 + 60));
        let transport = ScriptedTransport::new();
        let inbox = inbox(&store, &transport, &clock);
        let key = inbox.key(BSUID);
        deliver(&store, PERMISSION_REPLY).await; // window open, link phone → BSUID
        deliver(&store, CONTROL_TAKEN).await;
        let direct = |category| {
            OutboundMessage::new(inbox.recipient(&key), Text::new("Your code")).category(category)
        };
        transport.push_json(200, accepted("wamid.U"));
        inbox
            .send(&key, direct(DirectSendCategory::Utility))
            .await
            .unwrap();
        let refused = inbox
            .send(&key, direct(DirectSendCategory::Service))
            .await
            .unwrap_err();
        assert!(is_thread_owned_elsewhere(&refused), "{refused}");
        assert_eq!(transport.requests().len(), 1);

        // Two days later the window is closed: `window(false)` lets Meta decide.
        clock.set(at(1_750_030_073 + 2 * 86_400));
        let unchecked = inbox.clone().with_reply_checks(ReplyChecks::none());
        assert_eq!(unchecked.reply_checks(), ReplyChecks::none());
        assert_eq!(inbox.reply_checks(), ReplyChecks::default());
        let refused = inbox.reply(&key, text("late")).await.unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::CustomerServiceWindowClosed);
        let only_owner = inbox
            .clone()
            .with_reply_checks(ReplyChecks::all().window(false));
        // The thread is idle by now: nothing refuses it.
        transport.push_json(200, accepted("wamid.LATE"));
        only_owner.reply(&key, text("late")).await.unwrap();
        assert_eq!(transport.requests().len(), 2);
        assert!(ReplyChecks::all().checks_window() && ReplyChecks::all().checks_thread_owner());
        assert!(!ReplyChecks::none().checks_window() && !ReplyChecks::none().checks_thread_owner());
    }

    /// `direct-send/send-utility-and-authentication-messages`: a `service`
    /// category "follows the existing service-message flow; the message is
    /// dropped if the service window isn't open". `utility` and
    /// `authentication` are sent as templates, and a category this crate
    /// does not know is left to Meta (it answers `100` for one it does not
    /// know either): neither local check refuses them.
    #[tokio::test]
    async fn a_direct_send_service_message_needs_the_window_and_the_thread() {
        let store = memory();
        // The customer wrote at 1749416383: the window closed 17 s ago.
        let clock = ManualClock::new(at(1_749_416_383 + 86_400 + 17));
        let transport = ScriptedTransport::new();
        let inbox = inbox(&store, &transport, &clock);
        let key = inbox.key(PHONE);
        deliver(&store, PHONE_TEXT).await;
        let direct = |category| {
            OutboundMessage::new(inbox.recipient(&key), Text::new("Your order shipped"))
                .category(category)
        };
        let refused = inbox
            .send(&key, direct(DirectSendCategory::Service))
            .await
            .unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::CustomerServiceWindowClosed);
        assert!(
            transport.requests().is_empty(),
            "refused before any request"
        );

        let taken = fixture(
            "pages/conversation-routing.thread-control__control_taken.json",
            &[("1750101000", "1749500000")],
        );
        deliver(&store, &taken).await;
        let refused = inbox
            .clone()
            .with_reply_checks(ReplyChecks::all().window(false))
            .send(&key, direct(DirectSendCategory::Service))
            .await
            .unwrap_err();
        assert!(is_thread_owned_elsewhere(&refused), "{refused}");
        assert!(transport.requests().is_empty());

        // Another app owns the thread, outside the window.
        for (category, id) in [
            (DirectSendCategory::Utility, "wamid.U"),
            (DirectSendCategory::Authentication, "wamid.A"),
            (DirectSendCategory::Other("future".to_owned()), "wamid.F"),
        ] {
            transport.push_json(200, accepted(id));
            inbox.send(&key, direct(category)).await.unwrap();
        }
        assert_eq!(transport.requests().len(), 3);
        assert_eq!(transport.remaining(), 0);
    }

    // ─── Identity links ──────────────────────────────────────────────────

    /// A number change (`system`), old identity to new, and the new phone
    /// number to the new BSUID; a message carrying only one of `from` and
    /// `from_user_id` links nothing.
    #[tokio::test]
    async fn number_changes_are_linked() {
        let store = memory();
        deliver(&store, &fixture("messages/system.json", &[])).await;
        assert_eq!(links_of(&store, PHONE).await, [pair(PHONE, "12195555358")]);

        let store = memory();
        deliver(
            &store,
            &fixture("bsuid/system_user_changed_user_id.json", &[]),
        )
        .await;
        assert_eq!(
            links_of(&store, BSUID).await,
            [pair(BSUID, "US.29847561203948576612")]
        );

        let store = memory();
        deliver(
            &store,
            &fixture(
                "pages/business-scoped-user-ids__system_messages_webhooks.json",
                &[],
            ),
        )
        .await;
        assert_eq!(
            links_of(&store, "US.20837465019283746501").await,
            [
                pair(PHONE, "US.20837465019283746501"),
                pair("16505559876", "US.20837465019283746501"),
            ]
        );

        // Neither a message with one identity, nor another system type.
        let store = memory();
        deliver(&store, PHONE_TEXT).await;
        deliver(
            &store,
            &fixture(
                "messages/system.json",
                &[("user_changed_number", "customer_identity_changed")],
            ),
        )
        .await;
        deliver(&store, &fixture("bsuid/text_username_no_wa_id.json", &[])).await;
        assert!(links_of(&store, PHONE).await.is_empty());
        assert!(links_of(&store, BSUID).await.is_empty());
    }

    /// A group message links its sender's phone number to their BSUID; so
    /// does a standby copy.
    #[tokio::test]
    async fn group_and_standby_messages_link_their_sender() {
        let store = memory();
        deliver(
            &store,
            &fixture(
                "pages/business-scoped-user-ids__incoming_messages_webhooks.json",
                &[],
            ),
        )
        .await;
        assert_eq!(links_of(&store, PHONE).await, [pair(PHONE, BSUID)]);

        let store = memory();
        deliver(
            &store,
            &fixture(
                "pages/webhooks.reference.standby__inbound_message.json",
                &[(
                    r#""from": "16505551234","#,
                    r#""from": "16505551234", "from_user_id": "US.13491208655302741918","#,
                )],
            ),
        )
        .await;
        assert_eq!(links_of(&store, PHONE).await, [pair(PHONE, BSUID)]);
    }

    /// An empty value, one with U+0000, and a value linked to itself are
    /// never recorded.
    #[tokio::test]
    async fn unusable_values_are_never_linked() {
        let store = memory();
        let update = |previous: &str, current: &str| {
            change(
                "user_id_update",
                &json!({"messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "user_id_update": [{"user_id": {"previous": previous, "current": current},
                        "timestamp": "1750030073"}]}),
            )
        };
        for (previous, current) in [
            ("", BSUID),
            (BSUID, ""),
            ("   ", BSUID),
            (BSUID, "US.\u{0}2"),
            (BSUID, BSUID),
        ] {
            deliver(&store, &update(previous, current)).await;
        }
        let message = change(
            "messages",
            &json!({"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "messages": [{"from": "", "from_user_id": BSUID, "id": "wamid.E",
                    "timestamp": "1750030073", "type": "text", "text": {"body": "hi"}}]}),
        );
        deliver(&store, &message).await;
        assert!(links_of(&store, BSUID).await.is_empty());
        assert!(links_of(&store, "").await.is_empty());
    }
}

/// The same scenarios on Postgres, each in a schema of its own.
#[cfg(feature = "postgres")]
mod live {
    use std::str::FromStr;

    use meta_whatsapp_rs::adapters::store::PostgresConversationStore;
    use meta_whatsapp_rs::adapters::store::postgres::{self, sqlx};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    /// A schema of its own on the test database, dropped with it (when the
    /// test panics too), and the store on it.
    struct TestDb {
        store: Arc<dyn ConversationStore>,
        _cleanup: Cleanup,
    }

    impl TestDb {
        async fn new() -> Option<Self> {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let url = match std::env::var("META_WHATSAPP_RS_TEST_POSTGRES_URL") {
                Ok(url) if !url.trim().is_empty() => url,
                _ if std::env::var("META_WHATSAPP_RS_REQUIRE_LIVE").is_ok_and(|v| v == "1") => {
                    panic!(
                        "META_WHATSAPP_RS_TEST_POSTGRES_URL is not set, and \
                         META_WHATSAPP_RS_REQUIRE_LIVE=1 turns a skipped live test into a failure"
                    )
                }
                _ => {
                    eprintln!(
                        "skipping: META_WHATSAPP_RS_TEST_POSTGRES_URL is not set \
                         (META_WHATSAPP_RS_REQUIRE_LIVE=1 makes this a failure)"
                    );
                    return None;
                }
            };
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let t = OffsetDateTime::now_utc().unix_timestamp_nanos();
            let schema = format!("wa_test_{t:x}_{}_{n}", std::process::id());
            let cleanup = Cleanup {
                url: url.clone(),
                statement: format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
            };
            let admin = PgPoolOptions::new()
                .max_connections(1)
                .connect(&url)
                .await
                .expect("connect to META_WHATSAPP_RS_TEST_POSTGRES_URL");
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
                .execute(&admin)
                .await
                .unwrap();
            admin.close().await;
            let options = PgConnectOptions::from_str(&url)
                .unwrap()
                .options([("search_path", schema.as_str())]);
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .connect_with(options)
                .await
                .unwrap();
            postgres::migrate(&pool).await.unwrap();
            Some(Self {
                store: Arc::new(PostgresConversationStore::new(pool)),
                _cleanup: cleanup,
            })
        }
    }

    /// Runs `statement` when dropped, on a connection and a runtime of its
    /// own (the test's runtime may be the one unwinding), as the adapters'
    /// live tests do.
    struct Cleanup {
        url: String,
        statement: String,
    }

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let (url, statement) = (self.url.clone(), self.statement.clone());
            let result = std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                runtime.block_on(async {
                    use sqlx::{ConnectOptions, Connection};
                    let mut conn = PgConnectOptions::from_str(&url)
                        .map_err(|e| e.to_string())?
                        .options([("lock_timeout", "10s")])
                        .connect()
                        .await
                        .map_err(|e| e.to_string())?;
                    sqlx::query(sqlx::AssertSqlSafe(statement))
                        .execute(&mut conn)
                        .await
                        .map_err(|e| e.to_string())?;
                    conn.close().await.map_err(|e| e.to_string())
                })
            })
            .join()
            .unwrap_or_else(|_| Err("the cleanup thread panicked".to_owned()));
            if let Err(e) = result {
                if std::thread::panicking() {
                    eprintln!("cleanup `{}` failed: {e}", self.statement);
                } else {
                    panic!("cleanup `{}` failed: {e}", self.statement);
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_postgres_a_call_reopens_the_window() {
        let Some(db) = TestDb::new().await else {
            return;
        };
        a_call_reopens_the_window(db.store.clone()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_postgres_a_standby_message_is_a_window_event() {
        let Some(db) = TestDb::new().await else {
            return;
        };
        a_standby_message_is_a_window_event(db.store.clone()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_postgres_control_taken_refuses_a_reply_locally() {
        let Some(db) = TestDb::new().await else {
            return;
        };
        control_taken_refuses_a_reply_locally(db.store.clone()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_postgres_a_bsuid_change_links_the_two() {
        let Some(db) = TestDb::new().await else {
            return;
        };
        a_bsuid_change_links_the_two(db.store.clone()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_postgres_an_erasure_reaches_the_phone_keyed_thread() {
        let Some(db) = TestDb::new().await else {
            return;
        };
        an_erasure_reaches_the_phone_keyed_thread(db.store.clone()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_postgres_a_call_permission_reply_does_not_claim_the_thread() {
        let Some(db) = TestDb::new().await else {
            return;
        };
        super::rules::call_permission_reply_scenario(db.store.clone()).await;
    }
}
