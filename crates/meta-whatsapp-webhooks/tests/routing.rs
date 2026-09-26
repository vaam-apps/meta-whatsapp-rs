//! Conversation Routing: `messaging_handovers`, `standby`, and the
//! `conversation_context` of `messages`, from the examples of
//! `webhooks/reference/messaging-handovers`, `webhooks/reference/standby`,
//! `conversation-routing/thread-control` and
//! `conversation-routing/conversation-context` (placeholders filled).

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use meta_whatsapp_webhooks::fields::{
    ConversationContextType, HandoverAppRole, HandoverType, MessageContent, MessageStatus,
    MessagingHandoversValue, StandbyItem, ThreadRole,
};
use meta_whatsapp_webhooks::{ChangeValue, WebhookEvent, WebhookPayload};
use pretty_assertions::assert_eq;
use serde_json::json;
use time::OffsetDateTime;

const WABA: &str = "102290129340398";
const NUMBER_ID: &str = "106540352242922";
const USER: &str = "16505551234";

fn ts(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).unwrap()
}

fn handover(name: &str) -> MessagingHandoversValue {
    let mut events = common::events(name);
    assert_eq!(events.len(), 1, "{name}: {events:?}");
    let WebhookEvent::ThreadControlChanged {
        waba_id,
        phone_number_id,
        display_phone_number,
        handover,
    } = events.remove(0)
    else {
        panic!("{name}")
    };
    assert_eq!(waba_id.as_str(), WABA);
    assert_eq!(phone_number_id.as_str(), NUMBER_ID);
    assert_eq!(display_phone_number, "15550783881");
    assert_eq!(handover.messaging_product.as_deref(), Some("whatsapp"));
    assert_eq!(
        handover
            .sender
            .as_ref()
            .and_then(|s| s.phone_number.as_ref())
            .map(meta_whatsapp_core::ids::WaId::as_str),
        Some(USER)
    );
    assert_eq!(handover.timestamp, ts(1_750_101_000));
    *handover
}

#[test]
fn control_passed_from_the_reference() {
    let v = handover("pages/webhooks.reference.messaging-handovers__control_passed.json");
    assert_eq!(v.kind, HandoverType::ControlPassed);
    assert!(v.control_taken.is_none());
    let h = v.handover().unwrap();
    assert_eq!(v.control_passed.as_ref(), Some(h));
    assert_eq!(
        h.previous_owner_app_id
            .as_ref()
            .map(meta_whatsapp_core::ids::AppId::as_str),
        Some("1066355071287456")
    );
    assert_eq!(
        h.previous_owner_app_role,
        Some(HandoverAppRole::MetaBusinessAgent)
    );
    assert_eq!(h.previous_owner_role, Some(ThreadRole::AiAgent));
    assert_eq!(h.new_owner_app_id, None);
    assert_eq!(h.new_owner_role, Some(ThreadRole::Escalation));
    assert_eq!(
        h.metadata.as_deref(),
        Some("WhatsApp user requested human agent")
    );
    let context = h.conversation_context.as_ref().unwrap();
    assert_eq!(context.kind, ConversationContextType::Summary);
    assert_eq!(
        context.summary.as_ref().unwrap().text,
        "AI-generated summary string"
    );
}

#[test]
fn control_taken_from_the_reference_and_thread_control() {
    let v = handover("pages/webhooks.reference.messaging-handovers__control_taken.json");
    assert_eq!(v.kind, HandoverType::ControlTaken);
    assert!(v.control_passed.is_none());
    let h = v.handover().unwrap();
    assert_eq!(
        h.previous_owner_app_id
            .as_ref()
            .map(meta_whatsapp_core::ids::AppId::as_str),
        Some("1066355071287456")
    );
    // "`control_taken` carries no `previous_owner_app_role`."
    assert_eq!(h.previous_owner_app_role, None);
    assert_eq!(h.previous_owner_role, Some(ThreadRole::AiAgent));
    assert_eq!(h.new_owner_role, Some(ThreadRole::Escalation));
    assert_eq!(h.metadata.as_deref(), Some("Human agent stepping in"));
    assert_eq!(h.conversation_context, None);

    let v = handover("pages/conversation-routing.thread-control__control_taken.json");
    let h = v.handover().unwrap();
    assert_eq!(h.previous_owner_app_id, None);
    assert_eq!(h.previous_owner_role, Some(ThreadRole::CustomerService));
    assert_eq!(h.new_owner_role, Some(ThreadRole::Escalation));
}

#[test]
fn control_passed_without_app_ids() {
    for name in [
        "pages/conversation-routing.thread-control__control_passed.json",
        "pages/conversation-routing.conversation-context__control_passed.json",
    ] {
        let v = handover(name);
        assert_eq!(v.kind, HandoverType::ControlPassed, "{name}");
        let h = v.handover().unwrap();
        assert_eq!(h.previous_owner_app_id, None, "{name}");
        assert_eq!(h.previous_owner_app_role, None, "{name}");
        assert_eq!(h.previous_owner_role, Some(ThreadRole::AiAgent), "{name}");
        assert_eq!(h.new_owner_role, Some(ThreadRole::Escalation), "{name}");
        assert!(h.conversation_context.is_some(), "{name}");
    }
}

#[test]
fn every_role_identifier_is_known() {
    for (wire, role) in [
        ("customer_service", ThreadRole::CustomerService),
        ("marketing", ThreadRole::Marketing),
        ("utility", ThreadRole::Utility),
        ("ctwa", ThreadRole::Ctwa),
        ("ai_agent", ThreadRole::AiAgent),
        ("escalation", ThreadRole::Escalation),
    ] {
        assert_eq!(ThreadRole::from(wire), role);
        assert_eq!(role.as_str(), wire);
    }
}

/// The "Treat every field inside the notification object as optional"
/// best practice, and a notification type Meta adds later.
#[test]
fn a_bare_or_unknown_handover_still_parses() {
    let body = json!({"object": "whatsapp_business_account", "entry": [{"id": WABA, "changes": [{
        "field": "messaging_handovers",
        "value": {
            "messaging_product": "whatsapp",
            "recipient": {"phone_number_id": NUMBER_ID, "display_phone_number": "15550783881"},
            "type": "control_shared",
            "timestamp": "1750101000",
            "control_shared": {"new_owner_role": "escalation"}
        }
    }]}]});
    let events = WebhookPayload::from_slice(body.to_string().as_bytes())
        .unwrap()
        .into_events();
    let [WebhookEvent::ThreadControlChanged { handover, .. }] = events.as_slice() else {
        panic!()
    };
    assert_eq!(handover.kind, HandoverType::Other("control_shared".into()));
    assert!(handover.sender.is_none() && handover.handover().is_none());
}

fn standby(name: &str) -> (Option<String>, StandbyItem) {
    let mut events = common::events(name);
    assert_eq!(events.len(), 1, "{name}: {events:?}");
    let WebhookEvent::StandbyObserved {
        waba_id,
        phone_number_id,
        display_phone_number,
        contact,
        item,
    } = events.remove(0)
    else {
        panic!("{name}")
    };
    assert_eq!(waba_id.as_str(), WABA);
    assert_eq!(phone_number_id.as_str(), NUMBER_ID);
    assert_eq!(display_phone_number, "15550783881");
    (contact.and_then(|c| c.name().map(str::to_owned)), *item)
}

#[test]
fn standby_inbound_message_is_not_a_message_received() {
    let (name, item) = standby("pages/webhooks.reference.standby__inbound_message.json");
    assert_eq!(name.as_deref(), Some("Test User"));
    let StandbyItem::Message(m) = item else {
        panic!("{item:?}")
    };
    assert_eq!(
        m.from.as_ref().map(meta_whatsapp_core::ids::WaId::as_str),
        Some(USER)
    );
    assert!(m.id.as_str().starts_with("wamid."));
    assert_eq!(m.timestamp, ts(1_750_101_000));
    let MessageContent::Text(text) = &m.content else {
        panic!("{m:?}")
    };
    assert_eq!(text.body, "Test standby message");
}

#[test]
fn standby_echoes_keep_the_send_request() {
    let (_, item) = standby("pages/webhooks.reference.standby__text_message_echo.json");
    let StandbyItem::Echo(e) = item else {
        panic!("{item:?}")
    };
    assert_eq!(e.timestamp, ts(1_750_101_000));
    assert_eq!(e.to(), Some(USER));
    assert_eq!(e.message_type(), Some("text"));
    assert_eq!(
        e.message["text"]["body"],
        "Hello! Your order #12345 has shipped."
    );
    assert!(e.template.is_none() && e.flow.is_none());

    let (_, item) = standby("pages/webhooks.reference.standby__template_message_echo.json");
    let StandbyItem::Echo(e) = item else {
        panic!("{item:?}")
    };
    assert_eq!(e.message_type(), Some("template"));
    assert_eq!(e.message["template"]["name"], "summer_sale_2026");
    let template = e.template.as_ref().unwrap();
    assert_eq!(template["status"], "APPROVED");
    assert_eq!(
        template["components"][1]["text"],
        "Hi {{1}}, enjoy {{2}} off!"
    );

    let (_, item) = standby("pages/webhooks.reference.standby__interactive_flow_message_echo.json");
    let StandbyItem::Echo(e) = item else {
        panic!("{item:?}")
    };
    assert_eq!(e.message_type(), Some("interactive"));
    let flow = e.flow.as_ref().unwrap();
    assert_eq!(flow["status"], "PUBLISHED");
    assert_eq!(flow["categories"], json!(["APPOINTMENT_BOOKING"]));
}

#[test]
fn standby_status_receipt() {
    let (_, item) = standby("pages/webhooks.reference.standby__status_receipt.json");
    let StandbyItem::Status(s) = item else {
        panic!("{item:?}")
    };
    assert_eq!(s.status, MessageStatus::Delivered);
    assert_eq!(s.recipient_id.as_deref(), Some(USER));
    assert_eq!(s.timestamp, ts(1_750_101_000));
    let pricing = s.pricing.as_ref().unwrap();
    assert_eq!(pricing.billable, Some(true));
    assert_eq!(
        s.conversation.as_ref().unwrap().id.as_deref(),
        Some("b1946ac92492d2347c6235b4d2611184")
    );
}

#[test]
fn standby_dedup_keys_never_collide_with_the_owner_side() {
    let owner = common::events("messages/text.json").remove(0);
    let body = json!({"object": "whatsapp_business_account", "entry": [{"id": WABA, "changes": [{
        "field": "standby",
        "value": {"messaging_product": "whatsapp",
                  "metadata": {"display_phone_number": "15550783881", "phone_number_id": NUMBER_ID},
                  "standby": {"messages": [serde_json::to_value(match &owner {
                      WebhookEvent::MessageReceived { message, .. } => message,
                      other => panic!("{other:?}"),
                  }).unwrap()]}}
    }]}]});
    let copy = WebhookPayload::from_slice(body.to_string().as_bytes())
        .unwrap()
        .into_events()
        .remove(0);
    let (owner_key, copy_key) = (owner.dedup_key().unwrap(), copy.dedup_key().unwrap());
    assert_eq!(copy_key, format!("standby:{owner_key}"));
}

#[test]
fn an_empty_standby_is_kept_unknown() {
    // The page's "Common envelope" shows `"standby": {}`.
    let body = json!({"object": "whatsapp_business_account", "entry": [{"id": WABA, "changes": [{
        "field": "standby",
        "value": {"messaging_product": "whatsapp",
                  "metadata": {"display_phone_number": "15550783881", "phone_number_id": NUMBER_ID},
                  "standby": {}}
    }]}]});
    let p = WebhookPayload::from_slice(body.to_string().as_bytes()).unwrap();
    assert!(matches!(
        p.entry[0].changes[0].value,
        ChangeValue::Unknown(_)
    ));
    assert!(matches!(
        p.into_events().as_slice(),
        [WebhookEvent::Unknown { field, .. }] if field == "standby"
    ));
}

#[test]
fn conversation_context_rides_on_every_message_of_the_change() {
    let events =
        common::events("pages/conversation-routing.conversation-context__incoming_message.json");
    let [
        WebhookEvent::MessageReceived {
            conversation_context: Some(context),
            message,
            contact,
            ..
        },
    ] = events.as_slice()
    else {
        panic!("{events:?}")
    };
    assert_eq!(context.kind, ConversationContextType::Summary);
    assert_eq!(
        context.summary.as_ref().unwrap().text,
        "AI-generated summary string"
    );
    assert_eq!(contact.as_ref().unwrap().name(), Some("Sheena Nelson"));
    assert_eq!(message.message_type(), Some("text"));
}

/// Every standby copy's key is `standby:` plus the key the same item has
/// on the owner's side (`messages` statuses, `smb_message_echoes` echoes),
/// so a partner that is also subscribed to those fields never drops one
/// for the other.
#[test]
fn standby_echo_and_status_keys_are_the_owner_keys_prefixed() {
    let standby_of = |standby: serde_json::Value| {
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": WABA, "changes": [{
            "field": "standby",
            "value": {"messaging_product": "whatsapp",
                      "metadata": {"display_phone_number": "15550783881", "phone_number_id": NUMBER_ID},
                      "standby": standby}
        }]}]});
        let mut events = WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events();
        assert_eq!(events.len(), 1, "{events:?}");
        events.remove(0)
    };

    let owner = common::events("messages/group_statuses_aggregated.json").remove(0);
    let WebhookEvent::StatusUpdated { status, .. } = &owner else {
        panic!("{owner:?}")
    };
    let copy = standby_of(json!({"statuses": [serde_json::to_value(status).unwrap()]}));
    assert!(
        matches!(&copy, WebhookEvent::StandbyObserved { item, .. } if matches!(**item, StandbyItem::Status(_)))
    );
    assert_eq!(
        copy.dedup_key().unwrap(),
        format!("standby:{}", owner.dedup_key().unwrap())
    );

    let owner = common::events("fields/smb_message_echoes_text.json").remove(0);
    let WebhookEvent::MessageEchoed { echo, .. } = &owner else {
        panic!("{owner:?}")
    };
    let copy = standby_of(json!({"message_echoes": [{
        "id": echo.id.as_str(), "timestamp": "1750101000",
        "message": {"messaging_product": "whatsapp", "to": USER, "type": "text", "text": {"body": "x"}}
    }]}));
    assert_eq!(
        copy.dedup_key().unwrap(),
        format!("standby:{}", owner.dedup_key().unwrap())
    );
    // An echo and an inbound message never share a key, whatever their ids.
    let message = standby_of(json!({"messages": [{
        "from": USER, "id": echo.id.as_str(), "timestamp": "1750101000",
        "type": "text", "text": {"body": "x"}
    }]}));
    assert_ne!(copy.dedup_key(), message.dedup_key());
}

/// A standby item finds its user the way the `messages` field does: by
/// BSUID first, then by phone number, never by position.
#[test]
fn standby_items_find_their_contact_by_bsuid_then_phone() {
    let contacts = json!([
        {"profile": {"name": "First"}, "wa_id": "16505550000", "user_id": "US.1111"},
        {"profile": {"name": "Second"}, "user_id": "US.2222"},
        {"profile": {"name": "Third"}, "wa_id": "16505553333"}
    ]);
    let names = |standby: serde_json::Value| -> Vec<Option<String>> {
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": WABA, "changes": [{
            "field": "standby",
            "value": {"messaging_product": "whatsapp",
                      "metadata": {"display_phone_number": "15550783881", "phone_number_id": NUMBER_ID},
                      "standby": standby}
        }]}]});
        WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events()
            .iter()
            .map(|e| e.contact().and_then(|c| c.name().map(str::to_owned)))
            .collect()
    };
    let some = |s: &str| Some(s.to_owned());

    // Messages: `from_user_id` (no phone number), `from`, neither matching.
    let text = json!({"body": "x"});
    assert_eq!(
        names(json!({"contacts": contacts, "messages": [
            {"from_user_id": "US.2222", "id": "wamid.a", "timestamp": "1", "type": "text", "text": text},
            {"from": "+16505553333", "id": "wamid.b", "timestamp": "1", "type": "text", "text": text},
            {"from": "16505559999", "id": "wamid.c", "timestamp": "1", "type": "text", "text": text}
        ]})),
        [some("Second"), some("Third"), None]
    );

    // Statuses: `recipient_user_id`, a group participant's BSUID,
    // `recipient_id`, neither matching.
    assert_eq!(
        names(json!({"contacts": contacts, "statuses": [
            {"id": "wamid.a", "status": "delivered", "timestamp": "1", "recipient_user_id": "US.2222"},
            {"id": "wamid.b", "status": "read", "timestamp": "1", "recipient_id": "120363000000000000",
             "recipient_type": "group", "recipient_participant_user_id": "US.1111"},
            {"id": "wamid.c", "status": "sent", "timestamp": "1", "recipient_id": "16505553333"},
            {"id": "wamid.d", "status": "sent", "timestamp": "1", "recipient_id": "16505559999"}
        ]})),
        [some("Second"), some("First"), some("Third"), None]
    );
}
