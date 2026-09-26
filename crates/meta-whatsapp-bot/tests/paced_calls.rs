//! Parity row 92: the client's group operations (`PacedGroups`,
//! `PacedGroup`) and typing indicators (`PacedOutbound` over the client)
//! through a pacer. Each waits for a slot of its business number, here 1 a
//! second on a fake clock so the slots are 1 s apart; the requests are the
//! client's, unchanged; a limiter that fails fails the call before any
//! request; a `PacedOutbound` retries by the client's rule, each retry
//! after a slot.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use async_trait::async_trait;
use common::{NUMBER, client};
use meta_whatsapp_bot::{
    ClientOutbound, Outbound, PacedGroups, PacedOutbound, Pacer, Rate, RateLimiter, Reservation,
    SlotRequest, TokenBucket,
};
use meta_whatsapp_client::RetryPolicy;
use meta_whatsapp_client::groups::{
    CreateGroup, GroupField, GroupSettingsUpdate, ListGroups, ListJoinRequests,
};
use meta_whatsapp_client::messages::OutboundMessage;
use meta_whatsapp_core::Error;
use meta_whatsapp_core::clock::{Clock, ManualClock};
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::ids::{GroupId, MessageId, PhoneNumberId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::testing::ScriptedTransport;
use pretty_assertions::assert_eq;
use serde_json::json;
use time::OffsetDateTime;
use time::macros::datetime;

const T0: OffsetDateTime = datetime!(2026-09-26 12:00 UTC);
const GROUP: &str = "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD";

fn elapsed(clock: &ManualClock) -> Duration {
    Duration::try_from(clock.now() - T0).unwrap()
}

fn one_a_second(clock: &ManualClock) -> Pacer {
    Pacer::new(TokenBucket::new(Rate::per_second(1).unwrap())).with_timer(clock.clone())
}

/// Group operations of one number take its slots one after another; the
/// requests are the client's (Meta's documented shapes, as the client's
/// own tests assert them).
#[tokio::test]
async fn group_operations_wait_for_the_numbers_slots() {
    let t = ScriptedTransport::new();
    // groups-management-api: 200 is {messaging_product, request_id}.
    t.push_json(
        200,
        json!({"messaging_product": "whatsapp", "request_id": "req-1"}),
    );
    t.push_json(200, json!({}));
    // groups/groups-messaging "Pin and unpin group message" response.
    t.push_json(
        200,
        json!({
            "messaging_product": "whatsapp",
            "contacts": [{"input": GROUP, "wa_id": GROUP}],
            "messages": [{"id": "wamid.HBgLM..."}]
        }),
    );
    let clock = ManualClock::new(T0);
    let groups = PacedGroups::new(client(&t).groups(NUMBER), one_a_second(&clock));

    let created = groups
        .create(&CreateGroup::new("Watch Enthusiasts"))
        .await
        .unwrap();
    assert_eq!(created.request_id.as_deref(), Some("req-1"));
    assert_eq!(elapsed(&clock), Duration::ZERO);

    let group = groups.group(GROUP);
    assert_eq!(group.number(), &PhoneNumberId::new(NUMBER));
    let added = group
        .add_participants(&[Recipient::phone("+7669992245")])
        .await
        .unwrap();
    assert!(!added.partial);
    assert_eq!(elapsed(&clock), Duration::from_secs(1));

    groups
        .pin_message(&GroupId::new(GROUP), &MessageId::new("wamid.HBgLM..."), 4)
        .await
        .unwrap();
    assert_eq!(elapsed(&clock), Duration::from_secs(2));

    assert_eq!(t.remaining(), 0);
    let requests = t.requests();
    let shapes: Vec<(String, String, serde_json::Value)> = requests
        .iter()
        .map(|r| (r.method.to_string(), r.path().to_owned(), r.json().unwrap()))
        .collect();
    assert_eq!(
        shapes,
        [
            (
                "POST".to_owned(),
                format!("/v25.0/{NUMBER}/groups"),
                json!({"messaging_product": "whatsapp", "subject": "Watch Enthusiasts"})
            ),
            (
                "POST".to_owned(),
                format!("/v25.0/{GROUP}/participants"),
                json!({"messaging_product": "whatsapp", "participants": [{"user": "+7669992245"}]})
            ),
            (
                "POST".to_owned(),
                format!("/v25.0/{NUMBER}/messages"),
                json!({
                    "messaging_product": "whatsapp",
                    "recipient_type": "group",
                    "to": GROUP,
                    "type": "pin",
                    "pin": {"type": "pin", "message_id": "wamid.HBgLM...", "expiration_days": 4}
                })
            ),
        ]
    );
    assert!(requests.iter().all(|r| r.bearer() == Some("TOKEN")));
}

/// A rate limiter that cannot be reached.
#[derive(Debug)]
struct Unreachable;

#[async_trait]
impl RateLimiter for Unreachable {
    async fn reserve(&self, _: &SlotRequest) -> meta_whatsapp_core::Result<Reservation> {
        Err(StorageError::Backend(anyhow::anyhow!("limiter unreachable")).into())
    }

    async fn slow_down(
        &self,
        _: &PhoneNumberId,
        _: OffsetDateTime,
    ) -> meta_whatsapp_core::Result<()> {
        Err(StorageError::Backend(anyhow::anyhow!("limiter unreachable")).into())
    }
}

fn refused<T>(result: &meta_whatsapp_core::Result<T>) -> bool {
    matches!(result, Err(Error::Storage(_)))
}

/// Every wrapped operation takes its slot before its request: with a
/// limiter that fails, each one fails and nothing reaches Meta.
#[tokio::test]
async fn every_group_operation_waits_for_its_slot_before_its_request() {
    let t = ScriptedTransport::new();
    let groups = PacedGroups::new(client(&t).groups(NUMBER), Pacer::new(Unreachable));
    let group = groups.group(GROUP);
    let (id, message) = (GroupId::new(GROUP), MessageId::new("wamid.HBgLM..."));
    let users = [Recipient::phone("+7669992245")];
    let outcomes = [
        refused(&groups.create(&CreateGroup::new("Watch Enthusiasts")).await),
        refused(&groups.list(&ListGroups::default()).await),
        refused(&groups.pin_message(&id, &message, 4).await),
        refused(&groups.unpin_message(&id, &message).await),
        refused(&group.info(&[GroupField::Subject]).await),
        refused(&group.update(&GroupSettingsUpdate::default()).await),
        refused(&group.set_picture(vec![0xFF, 0xD8, 0xFF, 0xE0]).await),
        refused(&group.delete().await),
        refused(&group.invite_link().await),
        refused(&group.reset_invite_link().await),
        refused(&group.delete_invite_link().await),
        refused(&group.join_requests(&ListJoinRequests::default()).await),
        refused(&group.approve_join_requests(&["1"]).await),
        refused(&group.reject_join_requests(&["1"]).await),
        refused(&group.add_participants(&users).await),
        refused(&group.remove_participants(&users).await),
    ];
    assert_eq!(outcomes, [true; 16]);
    assert!(t.requests().is_empty());
}

/// A typing indicator (a read receipt with one) through `PacedOutbound`
/// over the client: each waits for a slot of the number, and the request
/// is `typing-indicators`' own.
#[tokio::test]
async fn typing_indicators_wait_for_the_numbers_slots() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    t.push_json(200, json!({"success": true}));
    let clock = ManualClock::new(T0);
    let outbound = PacedOutbound::new(ClientOutbound::new(client(&t)), one_a_second(&clock));
    let from = PhoneNumberId::new(NUMBER);

    outbound
        .mark_read(&from, &MessageId::new("wamid.A"), true)
        .await
        .unwrap();
    assert_eq!(elapsed(&clock), Duration::ZERO);
    outbound
        .mark_read(&from, &MessageId::new("wamid.B"), true)
        .await
        .unwrap();
    assert_eq!(elapsed(&clock), Duration::from_secs(1));

    assert_eq!(t.remaining(), 0);
    let last = t.last_request().unwrap();
    assert_eq!(last.method, "POST");
    assert_eq!(last.path(), format!("/v25.0/{NUMBER}/messages"));
    assert_eq!(
        last.json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "status": "read",
            "message_id": "wamid.B",
            "typing_indicator": {"type": "text"}
        }))
    );
}

fn server_error() -> serde_json::Value {
    json!({"error": {"message": "(#131000) Something went wrong", "type": "OAuthException", "code": 131000}})
}

/// `PacedOutbound` over a client that does not retry, retrying with a
/// policy of its own (what `BotBuilder::pacer` builds from a client):
/// the client's rule, call by call. A send answered with a 5xx may have
/// gone out, so it is not retried (`Error::may_resend`); a plain read
/// receipt is idempotent and is; one with a typing indicator is not. Each
/// retry waits its delay and a slot of its own.
#[tokio::test]
async fn a_paced_outbound_retries_what_the_client_would_and_nothing_else() {
    let t = ScriptedTransport::new();
    let clock = ManualClock::new(T0);
    let retry = RetryPolicy {
        max_retries: 3,
        base_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
    };
    let outbound =
        PacedOutbound::new(ClientOutbound::new(client(&t)), one_a_second(&clock)).retry(retry);
    let from = PhoneNumberId::new(NUMBER);

    t.push_json(500, server_error());
    let message = OutboundMessage::text(Recipient::phone("+16505551234"), "Your order shipped");
    let error = outbound.send(&from, &message).await.unwrap_err();
    assert!(error.may_have_been_sent());
    assert_eq!(
        t.requests().len(),
        1,
        "a send that may be out is not resent"
    );

    t.push_json(500, server_error());
    t.push_json(200, json!({"success": true}));
    outbound
        .mark_read(&from, &MessageId::new("wamid.A"), false)
        .await
        .unwrap();
    assert_eq!(t.requests().len(), 3, "a read receipt is retried");

    t.push_json(500, server_error());
    outbound
        .mark_read(&from, &MessageId::new("wamid.B"), true)
        .await
        .unwrap_err();
    assert_eq!(t.requests().len(), 4, "a typing indicator is not");

    // Each call and retry took a slot, one a second.
    assert_eq!(elapsed(&clock), Duration::from_secs(3));
    assert_eq!(t.remaining(), 0);
}
