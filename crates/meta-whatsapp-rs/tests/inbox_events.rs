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
    assert_eq!(refused.kind(), ErrorKind::ThreadOwnedElsewhere);
    assert_eq!(transport.requests().len(), 1, "refused with zero requests");
    let checked = inbox.check_reply(&key).await.unwrap_err();
    assert_eq!(
        checked.kind(),
        ErrorKind::ThreadOwnedElsewhere,
        "as reply decides"
    );

    // A template needs no ownership, and sending one changes no owner
    // (`conversation-routing/thread-lifecycle`).
    transport.push_json(200, accepted("wamid.TEMPLATE"));
    inbox
        .reply(&key, TemplateMessage::new("order_update", "en_US").into())
        .await
        .unwrap();
    assert_eq!(transport.requests().len(), 2);
    let refused = inbox
        .reply(&key, text("After the template"))
        .await
        .unwrap_err();
    assert!(is_thread_owned_elsewhere(&refused), "{refused}");
    assert_eq!(transport.requests().len(), 2);

    // The caller's explicit override of the local check.
    transport.push_json(200, accepted("wamid.OVERRIDE"));
    inbox
        .clone()
        .with_reply_checks(ReplyChecks::ALL.thread_owner(false))
        .reply(&key, text("Escalation partner here"))
        .await
        .unwrap();
    assert_sent_text(&transport, "Escalation partner here");
    assert_eq!(transport.remaining(), 0);
}

/// After a `user_id_update`, `Inbox::identities` of the new BSUID holds the
/// previous one, and the phone number (`wa_id`) the update carries.
async fn a_bsuid_change_links_the_two(store: Arc<dyn ConversationStore>) {
    let clock = ManualClock::new(at(1_750_030_073));
    let transport = ScriptedTransport::new();
    let inbox = inbox(&store, &transport, &clock);
    deliver(&store, USER_ID_UPDATE).await;
    deliver(&store, USER_ID_UPDATE).await; // Meta's retry
    let current = inbox.key("US.20837465019283746501");
    assert_eq!(
        inbox.identities(&current).await.unwrap(),
        BTreeSet::from([
            PHONE.to_owned(),
            BSUID.to_owned(),
            "US.20837465019283746501".to_owned()
        ])
    );
    let mut links: Vec<(String, OffsetDateTime)> = store
        .identity_links(&current)
        .await
        .unwrap()
        .into_iter()
        .map(|l| {
            assert_eq!(l.current, "US.20837465019283746501");
            assert_eq!(l.phone_number_id.as_str(), PNID, "the business number's");
            (l.previous, l.at)
        })
        .collect();
    links.sort();
    assert_eq!(
        links,
        [
            (PHONE.to_owned(), at(1_750_030_073)),
            (BSUID.to_owned(), at(1_750_030_073)),
        ]
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

/// `conversation-routing/thread-control`, `control_passed` at 1750101000,
/// with the `conversation_context` summary Meta sends an app that receives
/// no standby copies (`conversation-routing/conversation-context`).
const CONTROL_PASSED: &str = include_str!(
    "../../meta-whatsapp-webhooks/tests/fixtures/pages/conversation-routing.thread-control__control_passed.json"
);

/// An app that receives handovers without standby copies: the customer's
/// last message it saw is days old, then the thread is passed to it
/// (`control_passed`). The window it recorded is closed, Meta's is open
/// (a thread is passed only while active): the reply goes to Meta, until
/// 24 hours after the handover. Without the trust
/// (`ReplyChecks::trust_handover(false)`), it is refused locally.
async fn a_handover_lets_meta_decide_the_window(store: Arc<dyn ConversationStore>) {
    let clock = ManualClock::new(at(1_750_101_000 + 600));
    let transport = ScriptedTransport::new();
    let inbox = inbox(&store, &transport, &clock);
    let key = inbox.key(PHONE);
    deliver(&store, PHONE_TEXT).await; // 1749416383, a week before
    assert!(!inbox.window_is_open(&key).await.unwrap());
    let refused = inbox.reply(&key, text("Hello?")).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::CustomerServiceWindowClosed);

    deliver(&store, CONTROL_PASSED).await;
    assert!(
        !inbox.window_is_open(&key).await.unwrap(),
        "the recorded window is still closed"
    );
    inbox.check_reply(&key).await.unwrap();
    transport.push_json(200, accepted("wamid.HANDOVER"));
    inbox
        .reply(&key, text("Hi, I'm Ana from support"))
        .await
        .unwrap();
    assert_eq!(transport.requests().len(), 1);

    let strict = inbox
        .clone()
        .with_reply_checks(ReplyChecks::ALL.trust_handover(false));
    let refused = strict.reply(&key, text("Hi again")).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::CustomerServiceWindowClosed);
    assert_eq!(
        strict.check_reply(&key).await.unwrap_err().kind(),
        ErrorKind::CustomerServiceWindowClosed
    );
    assert_eq!(transport.requests().len(), 1, "refused with zero requests");

    // 24 hours after the handover, the recorded window decides again.
    clock.set(at(1_750_101_000 + 86_400));
    let refused = inbox.reply(&key, text("Still there?")).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::CustomerServiceWindowClosed);
    assert_eq!(transport.remaining(), 0);
}

/// After a message of the customer's newer than the handover, the
/// recorded window applies again: open from that message, closed 24 hours
/// after it, trust or no trust.
async fn after_the_customers_next_message_the_window_decides(store: Arc<dyn ConversationStore>) {
    let clock = ManualClock::new(at(1_750_101_000 + 600));
    let transport = ScriptedTransport::new();
    let inbox = inbox(&store, &transport, &clock);
    let key = inbox.key(PHONE);
    deliver(&store, PHONE_TEXT).await;
    deliver(&store, CONTROL_PASSED).await;
    let next = r#""timestamp": "1750101300""#;
    let body = PHONE_TEXT
        .replacen(r#""timestamp": "1749416383""#, next, 1)
        .replacen("wamid.", "wamid.NEXT.", 1);
    assert!(body.contains(next) && body.contains("wamid.NEXT."));
    deliver(&store, &body).await;
    assert_eq!(
        inbox.window(&key).await.unwrap().closes_at(),
        Some(at(1_750_101_300 + 86_400))
    );
    for checks in [ReplyChecks::ALL, ReplyChecks::ALL.trust_handover(false)] {
        let inbox = inbox.clone().with_reply_checks(checks);
        clock.set(at(1_750_101_300 + 86_400 - 1));
        inbox.check_reply(&key).await.unwrap();
        clock.set(at(1_750_101_300 + 86_400));
        assert_eq!(
            inbox.check_reply(&key).await.unwrap_err().kind(),
            ErrorKind::CustomerServiceWindowClosed,
            "{checks:?}"
        );
    }
    assert!(transport.requests().is_empty());
}

fn memory() -> Arc<dyn ConversationStore> {
    Arc::new(MemoryConversationStore::new())
}

#[tokio::test]
async fn memory_a_handover_lets_meta_decide_the_window() {
    a_handover_lets_meta_decide_the_window(memory()).await;
}

#[tokio::test]
async fn memory_after_the_customers_next_message_the_window_decides() {
    after_the_customers_next_message_the_window_decides(memory()).await;
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
    use meta_whatsapp_rs::inbox::RecordingSwitches;
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

    /// A picked-up business call is dated when the customer picked it up
    /// (`start_time`: "Only present when the call was picked up by the
    /// other party"), and a call status names the customer by its
    /// `recipient_user_id` when no `contacts` come with it.
    #[tokio::test]
    async fn a_business_call_is_dated_when_picked_up() {
        // A long call of the business's, picked up: dated when the
        // customer picked it up, not when it ended an hour later.
        let store = memory();
        deliver(
            &store,
            &fixture(
                "pages/calling.reference__call_terminate_webhook.json",
                &[("1671644824", "1671648424")], // its `timestamp`, not `start_time`
            ),
        )
        .await;
        let events = store
            .window_events(&ConversationKey::new("105615555715855", BSUID), None, 10)
            .await
            .unwrap();
        assert_eq!((events.len(), events[0].at), (1, at(1_671_644_824)));

        // A status without `contacts`: keyed by its `recipient_user_id`.
        let store = memory();
        let accepted = change(
            "calls",
            &json!({"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "statuses": [{"id": "wacid.ACCEPTED", "type": "call", "status": "ACCEPTED",
                    "timestamp": "1750030073", "recipient_id": PHONE, "recipient_user_id": BSUID}]}),
        );
        deliver(&store, &accepted).await;
        assert_eq!(events_of(&store, BSUID).await.len(), 1);
        assert!(events_of(&store, PHONE).await.is_empty());
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

    /// A handover's number with a `+` is the customer's `wa_id` all the
    /// same, as the key of a message (`from`) has none.
    #[tokio::test]
    async fn a_handover_number_with_a_plus_names_the_wa_id() {
        let store = memory();
        let taken = fixture(
            "pages/conversation-routing.thread-control__control_taken.json",
            &[(
                r#""phone_number": "16505551234""#,
                r#""phone_number": "+16505551234""#,
            )],
        );
        deliver(&store, &taken).await;
        assert_eq!(
            owner_of(&store, PHONE).await.unwrap().owner,
            ThreadOwner::AnotherApp
        );
        assert_eq!(owner_of(&store, "+16505551234").await, None);
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

        // A loop of three stops where it closes too: at its last new identity.
        let store = memory();
        for l in [
            link(PHONE, "US.A", 100),
            link("US.A", "US.B", 200),
            link("US.B", "US.C", 300),
            link("US.C", "US.A", 400),
        ] {
            store.link_identity(l).await.unwrap();
        }
        deliver(&store, CONTROL_TAKEN).await;
        for contact in [PHONE, "US.A", "US.B"] {
            assert_eq!(owner_of(&store, contact).await, None, "{contact}");
        }
        assert_eq!(
            owner_of(&store, "US.C").await.unwrap().owner,
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

        // Only a contact whose number it is counts, however recently
        // another one the person's links connect was synced: here the
        // previous identity, under the number it had then.
        let store = memory();
        store
            .put_contact(contact(BSUID, PHONE, BSUID, 100))
            .await
            .unwrap();
        store
            .put_contact(contact("US.OLD", "16505559876", "US.OLD", 200))
            .await
            .unwrap();
        store
            .link_identity(IdentityLink::new(PNID, "US.OLD", BSUID, at(150)))
            .await
            .unwrap();
        deliver(&store, CONTROL_TAKEN).await;
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

        // The handover's time counts as activity: a thread taken 23 hours
        // after the customer's last message known here is still another
        // app's two hours later.
        let store = memory();
        let key = inbox.key(PHONE);
        deliver(&store, &message("wamid.early", 1_750_101_000 - 23 * 3_600)).await;
        deliver(&store, CONTROL_TAKEN).await; // 1750101000
        clock.set(at(1_750_101_000 + 2 * 3_600));
        let late = super::inbox(&store, &transport, &clock);
        assert_eq!(
            late.thread_owner(&key).await.unwrap().unwrap().owner,
            ThreadOwner::AnotherApp
        );

        // Another app's thread goes idle 24 hours after the handover when
        // no later message is known; an idle thread is not refused.
        let store = memory();
        let only_owner = super::inbox(&store, &transport, &clock)
            .with_reply_checks(ReplyChecks::ALL.window(false));
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
        // A message of the handover's second: before the take, which it
        // does not undo (the take came after the message reached this app).
        let same_second = change(
            "messages",
            &json!({"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                "messages": [{"from": PHONE, "from_user_id": BSUID, "id": "wamid.SAME",
                    "timestamp": "1750101000", "type": "text", "text": {"body": "a human, please"}}]}),
        );
        deliver(&store, &same_second).await;
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
        // A template needs no ownership; this app's own message, the latest
        // of the conversation now, claims nothing either.
        transport.push_json(200, accepted("wamid.TEMPLATE"));
        inbox
            .reply(&key, TemplateMessage::new("call_back", "en_US").into())
            .await
            .unwrap();
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
        assert_eq!(transport.requests().len(), 1, "the template only");
        assert_eq!(transport.remaining(), 0);

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
        let unchecked = inbox.clone().with_reply_checks(ReplyChecks::NONE);
        assert_eq!(unchecked.reply_checks(), ReplyChecks::NONE);
        assert_eq!(inbox.reply_checks(), ReplyChecks::default());
        let refused = inbox.reply(&key, text("late")).await.unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::CustomerServiceWindowClosed);
        let only_owner = inbox
            .clone()
            .with_reply_checks(ReplyChecks::ALL.window(false));
        // The thread is idle by now: nothing refuses it.
        transport.push_json(200, accepted("wamid.LATE"));
        only_owner.reply(&key, text("late")).await.unwrap();
        assert_eq!(transport.requests().len(), 2);
        assert!(ReplyChecks::ALL.checks_window() && ReplyChecks::ALL.checks_thread_owner());
        assert!(!ReplyChecks::NONE.checks_window() && !ReplyChecks::NONE.checks_thread_owner());
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
            .with_reply_checks(ReplyChecks::ALL.window(false))
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

    /// The trust in a handover is for a handover to this app only: not
    /// after another app took the thread (with the ownership check off),
    /// not after this app released it, and not with the window check off
    /// (nothing to trust then: the reply goes anyway).
    #[tokio::test]
    async fn only_a_handover_to_this_app_is_trusted() {
        let clock = ManualClock::new(at(1_750_101_000 + 600));
        let transport = ScriptedTransport::new();
        let only_window = ReplyChecks::ALL.thread_owner(false);

        let store = memory();
        let inbox = inbox(&store, &transport, &clock).with_reply_checks(only_window);
        let key = inbox.key(PHONE);
        deliver(&store, PHONE_TEXT).await;
        deliver(&store, CONTROL_TAKEN).await;
        let refused = inbox.reply(&key, text("hi")).await.unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::CustomerServiceWindowClosed);

        let store = memory();
        let inbox = super::inbox(&store, &transport, &clock);
        deliver(&store, PHONE_TEXT).await;
        deliver(&store, CONTROL_PASSED).await;
        inbox.check_reply(&key).await.unwrap();
        clock.set(at(1_750_101_000 + 700));
        assert!(inbox.record_release(&key).await.unwrap());
        let refused = inbox.reply(&key, text("hi")).await.unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::CustomerServiceWindowClosed);

        // No inbound message ever recorded: the handover is trusted too.
        let store = memory();
        let inbox = super::inbox(&store, &transport, &clock);
        deliver(&store, CONTROL_PASSED).await;
        assert_eq!(inbox.window(&key).await.unwrap().closes_at(), None);
        inbox.check_reply(&key).await.unwrap();
        assert!(transport.requests().is_empty());
    }

    /// `check_reply` decides as `reply` does, sending nothing: the window,
    /// then the owner (checked first), and the switches.
    #[tokio::test]
    async fn check_reply_decides_as_reply_does() {
        let store = memory();
        let clock = ManualClock::new(at(1_750_030_073 + 60));
        let transport = ScriptedTransport::new();
        let inbox = inbox(&store, &transport, &clock);
        let key = inbox.key(BSUID);
        let kind = |r: Result<(), meta_whatsapp_rs::Error>| r.err().map(|e| e.kind());
        assert_eq!(
            kind(inbox.check_reply(&key).await),
            Some(ErrorKind::CustomerServiceWindowClosed)
        );
        deliver(&store, PERMISSION_REPLY).await; // 1750030073, links the phone
        assert_eq!(kind(inbox.check_reply(&key).await), None);
        deliver(&store, CONTROL_TAKEN).await; // 1750101000
        // The window (from 1750030073) is closed by now, and the thread is
        // not idle yet (24 hours after the handover): both refusals apply,
        // and the owner's comes first.
        clock.set(at(1_750_101_000 + 86_400 - 1));
        assert!(!inbox.window_is_open(&key).await.unwrap());
        assert_eq!(
            inbox.thread_owner(&key).await.unwrap().unwrap().owner,
            ThreadOwner::AnotherApp
        );
        assert_eq!(
            kind(inbox.check_reply(&key).await),
            Some(ErrorKind::ThreadOwnedElsewhere),
            "the owner first, though the window is closed too"
        );
        let only_window = inbox
            .clone()
            .with_reply_checks(ReplyChecks::ALL.thread_owner(false));
        assert_eq!(
            kind(only_window.check_reply(&key).await),
            Some(ErrorKind::CustomerServiceWindowClosed)
        );
        let unchecked = inbox.clone().with_reply_checks(ReplyChecks::NONE);
        assert_eq!(kind(unchecked.check_reply(&key).await), None);
        // A key of another number is refused as such, before any check.
        let foreign = inbox
            .check_reply(&ConversationKey::new("999", BSUID))
            .await
            .unwrap_err();
        assert!(
            matches!(&foreign, meta_whatsapp_rs::Error::Validation(v) if v.field == "conversation"),
            "{foreign}"
        );
        assert!(transport.requests().is_empty());
        assert!(ReplyChecks::ALL.trusts_handover());
        assert!(!ReplyChecks::ALL.trust_handover(false).trusts_handover());
        assert!(!ReplyChecks::NONE.trusts_handover());
    }

    /// This app's own `pass` and `take`, which no webhook reports to it.
    #[tokio::test]
    async fn record_thread_owner_records_this_apps_pass_and_take() {
        let store = memory();
        let clock = ManualClock::new(at(1_750_101_000));
        let transport = ScriptedTransport::new();
        let inbox = inbox(&store, &transport, &clock);
        let key = inbox.key(BSUID);
        deliver(&store, PERMISSION_REPLY).await;
        clock.set(at(1_750_030_073 + 60));
        assert!(
            inbox
                .record_thread_owner(&key, ThreadOwner::AnotherApp, Some("ai_agent".to_owned()))
                .await
                .unwrap()
        );
        assert_eq!(
            owner_of(&store, BSUID).await.unwrap(),
            ThreadOwnership {
                owner: ThreadOwner::AnotherApp,
                role: Some("ai_agent".to_owned()),
                app_id: None,
                since: at(1_750_030_073 + 60),
            }
        );
        let refused = inbox.reply(&key, text("hi")).await.unwrap_err();
        assert!(is_thread_owned_elsewhere(&refused), "{refused}");

        clock.set(at(1_750_030_073 + 120));
        inbox
            .record_thread_owner(&key, ThreadOwner::ThisApp, Some("escalation".to_owned()))
            .await
            .unwrap();
        transport.push_json(200, accepted("wamid.TAKEN"));
        inbox.reply(&key, text("I'm taking over")).await.unwrap();
        assert_eq!(transport.remaining(), 0);

        let foreign = ConversationKey::new("999", BSUID);
        assert!(
            inbox
                .record_thread_owner(&foreign, ThreadOwner::ThisApp, None)
                .await
                .is_err()
        );
        assert_eq!(
            store
                .thread_owner(&ConversationKey::new("999", BSUID))
                .await
                .unwrap(),
            None
        );
    }

    /// The idle timeout is a setting.
    #[tokio::test]
    async fn the_idle_timeout_is_a_setting() {
        let store = memory();
        let clock = ManualClock::new(at(1_750_101_000 + 3_599));
        let transport = ScriptedTransport::new();
        let hour = std::time::Duration::from_secs(3_600);
        let inbox = inbox(&store, &transport, &clock).with_thread_idle_after(hour);
        assert_eq!(inbox.thread_idle_after(), hour);
        assert_eq!(
            super::inbox(&store, &transport, &clock).thread_idle_after(),
            Inbox::THREAD_IDLE_AFTER
        );
        let key = inbox.key(PHONE);
        deliver(&store, CONTROL_TAKEN).await;
        assert_eq!(
            inbox.thread_owner(&key).await.unwrap().unwrap().owner,
            ThreadOwner::AnotherApp
        );
        clock.set(at(1_750_101_000 + 3_600));
        assert_eq!(
            inbox.thread_owner(&key).await.unwrap().unwrap(),
            ThreadOwnership {
                owner: ThreadOwner::Idle,
                role: None,
                app_id: None,
                since: at(1_750_101_000 + 3_600),
            }
        );
    }

    // ─── What the sink records ───────────────────────────────────────────

    /// What each switch of `RecordingSwitches` records, on a fresh store:
    /// `(call window events, standby window events and owner, handover
    /// owner, identity links, the inbound message)`. Each kind is checked
    /// on every path that records it: a call and a call status; a standby
    /// copy's window event, owner and link (identity links, not standby).
    async fn recorded(switches: RecordingSwitches) -> (bool, bool, bool, bool, bool) {
        let store = memory();
        let sink = InboxSink::new(store.clone()).with_recording(switches);
        assert_eq!(sink.recording(), switches);
        let standby = fixture(
            "pages/webhooks.reference.standby__inbound_message.json",
            &[(
                r#""from": "16505551234","#,
                r#""from": "16315553601", "from_user_id": "US.STANDBY","#,
            )],
        );
        let accepted = fixture(
            "pages/business-scoped-user-ids__business_initiated_calls_status_webhooks.json",
            &[
                ("RINGING", "ACCEPTED"),
                ("wacid.ABGGFjFVU2AfAgo6V-Hc5eCgK5Gh", "wacid.STATUS"),
            ],
        );
        // The handover first: no link leads its number elsewhere yet.
        for body in [
            CONTROL_TAKEN,
            USER_CALL,
            accepted.as_str(),
            standby.as_str(),
            USER_ID_UPDATE,
            PERMISSION_REPLY,
        ] {
            for event in WebhookPayload::from_slice(body.as_bytes())
                .unwrap()
                .into_events()
            {
                sink.deliver(event).await.unwrap();
            }
        }
        let calls = events_of(&store, BSUID).await.len();
        assert!(
            calls == 0 || calls == 2,
            "{calls}: one switch for both paths"
        );
        let standby_events = events_of(&store, "US.STANDBY").await.len();
        let standby_owner = owner_of(&store, "US.STANDBY").await;
        assert_eq!(standby_events == 1, standby_owner.is_some(), "one switch");
        let links = [
            links_of(&store, "US.20837465019283746501").await.len(),
            links_of(&store, BSUID).await.len(),
            links_of(&store, "US.STANDBY").await.len(),
        ];
        assert!(
            links == [0, 0, 0] || links == [2, 2, 1],
            "{links:?}: one switch for every link"
        );
        (
            calls == 2,
            standby_events == 1,
            owner_of(&store, PHONE)
                .await
                .is_some_and(|o| o.owner == ThreadOwner::AnotherApp),
            links == [2, 2, 1],
            store
                .messages(&ConversationKey::new(PNID, BSUID), None, 10)
                .await
                .unwrap()
                .len()
                == 1,
        )
    }

    /// Each switch off records nothing of its kind, and everything else
    /// still; messages are always recorded.
    #[tokio::test]
    async fn each_recording_switch_turns_off_its_kind_only() {
        let all = RecordingSwitches::ALL;
        assert_eq!(RecordingSwitches::default(), all);
        assert_eq!(recorded(all).await, (true, true, true, true, true));
        assert_eq!(
            recorded(all.calls(false)).await,
            (false, true, true, true, true)
        );
        assert_eq!(
            recorded(all.standby(false)).await,
            (true, false, true, true, true)
        );
        assert_eq!(
            recorded(all.handovers(false)).await,
            (true, true, false, true, true)
        );
        assert_eq!(
            recorded(all.identity_links(false)).await,
            (true, true, true, false, true)
        );
        assert_eq!(
            recorded(RecordingSwitches::NONE).await,
            (false, false, false, false, true)
        );
        // And each switch turned on records its kind alone.
        let none = RecordingSwitches::NONE;
        assert_eq!(
            recorded(none.calls(true)).await,
            (true, false, false, false, true)
        );
        assert_eq!(
            recorded(none.standby(true)).await,
            (false, true, false, false, true)
        );
        assert_eq!(
            recorded(none.handovers(true)).await,
            (false, false, true, false, true)
        );
        assert_eq!(
            recorded(none.identity_links(true)).await,
            (false, false, false, true, true)
        );
        assert_eq!(all.calls(false).calls(true), all);
        assert!(
            !none.records_calls()
                && !none.records_standby()
                && !none.records_handovers()
                && !none.records_identity_links()
        );
        assert!(
            all.records_calls()
                && all.records_standby()
                && all.records_handovers()
                && all.records_identity_links()
        );
    }

    /// The public rules are the sink's: a call, a call status and a
    /// handover's number map to the keys it records under.
    #[tokio::test]
    async fn the_public_rules_are_the_sinks() {
        use meta_whatsapp_rs::inbox::{call_key, call_status_key, call_window, handover_key};
        use meta_whatsapp_rs::webhooks::WebhookEvent;

        let events = WebhookPayload::from_slice(USER_CALL.as_bytes())
            .unwrap()
            .into_events();
        let WebhookEvent::CallUpdated {
            phone_number_id,
            display_phone_number,
            contact,
            call,
            ..
        } = &events[0]
        else {
            panic!("{:?}", events[0].kind())
        };
        assert_eq!(
            call_window(call),
            Some((WindowEventKind::CustomerCall, at(1_750_030_073)))
        );
        assert_eq!(
            call_key(
                phone_number_id,
                display_phone_number,
                contact.as_ref(),
                call
            ),
            Some(ConversationKey::new(PNID, BSUID))
        );
        let accepted = fixture(
            "pages/business-scoped-user-ids__business_initiated_calls_status_webhooks.json",
            &[("RINGING", "ACCEPTED")],
        );
        let events = WebhookPayload::from_slice(accepted.as_bytes())
            .unwrap()
            .into_events();
        let WebhookEvent::CallStatusUpdated {
            phone_number_id,
            display_phone_number,
            contact,
            status,
            ..
        } = &events[0]
        else {
            panic!("{:?}", events[0].kind())
        };
        assert_eq!(
            call_status_key(
                phone_number_id,
                display_phone_number,
                contact.as_ref(),
                status
            ),
            Some(ConversationKey::new(PNID, BSUID))
        );

        let store = memory();
        let number = meta_whatsapp_rs::core::ids::PhoneNumberId::new(PNID);
        assert_eq!(
            handover_key(store.as_ref(), &number, "+16505551234")
                .await
                .unwrap(),
            Some(ConversationKey::new(PNID, PHONE))
        );
        deliver(&store, PERMISSION_REPLY).await; // links the phone to the BSUID
        assert_eq!(
            handover_key(store.as_ref(), &number, PHONE).await.unwrap(),
            Some(ConversationKey::new(PNID, BSUID))
        );
        assert_eq!(
            handover_key(store.as_ref(), &number, " + ").await.unwrap(),
            None
        );
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
        // The same rules for the phone number a `user_id_update` carries.
        let with_phone = |phone: &str| {
            change(
                "user_id_update",
                &json!({"messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": PNID},
                    "user_id_update": [{"wa_id": phone,
                        "user_id": {"previous": "US.9", "current": BSUID},
                        "timestamp": "1750030073"}]}),
            )
        };
        for phone in ["", "  ", "1650\u{0}5551234", BSUID] {
            deliver(&store, &with_phone(phone)).await;
        }
        assert_eq!(
            links_of(&store, BSUID).await,
            [pair("US.9", BSUID)],
            "the BSUIDs only"
        );
        let store = memory();
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
    async fn live_postgres_a_handover_lets_meta_decide_the_window() {
        let Some(db) = TestDb::new().await else {
            return;
        };
        a_handover_lets_meta_decide_the_window(db.store.clone()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_postgres_after_the_customers_next_message_the_window_decides() {
        let Some(db) = TestDb::new().await else {
            return;
        };
        after_the_customers_next_message_the_window_decides(db.store.clone()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_postgres_a_call_permission_reply_does_not_claim_the_thread() {
        let Some(db) = TestDb::new().await else {
            return;
        };
        super::rules::call_permission_reply_scenario(db.store.clone()).await;
    }
}
