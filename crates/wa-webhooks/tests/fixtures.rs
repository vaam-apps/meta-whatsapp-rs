//! Every fixture (copied from Meta's doc examples, placeholders filled) must
//! parse fully typed, flatten to events, and survive our own serialization.

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use pretty_assertions::assert_eq;
use wa_webhooks::fields::MessageContent;
use wa_webhooks::{ChangeValue, WebhookEvent, WebhookPayload};

fn content_is_typed(content: &MessageContent) -> bool {
    match content {
        MessageContent::Unknown { .. } | MessageContent::Invalid { .. } => false,
        MessageContent::Edit(edit) => content_is_typed(&edit.message.content),
        _ => true,
    }
}

#[test]
fn there_are_fixtures_for_every_area() {
    let all = common::all_fixtures();
    assert!(all.len() >= 90, "only {} fixtures", all.len());
}

#[test]
fn every_fixture_parses_into_typed_values_and_events() {
    for name in common::all_fixtures() {
        let payload = common::payload(&name);
        assert_eq!(payload.object, "whatsapp_business_account", "{name}");
        for entry in &payload.entry {
            for change in &entry.changes {
                assert!(
                    !change.value.is_unknown(),
                    "{name}: field `{}` fell back to Unknown: {:?}",
                    change.field,
                    change.parse_error
                );
            }
        }
        let events = payload.clone().into_events();
        assert!(!events.is_empty(), "{name}: no events");
        for event in &events {
            match event {
                WebhookEvent::Unknown { .. } | WebhookEvent::Unparsed { .. } => {
                    panic!("{name}: untyped event {event:?}")
                }
                WebhookEvent::MessageReceived { message, .. } => {
                    assert!(content_is_typed(&message.content), "{name}: {message:?}");
                }
                WebhookEvent::MessageEchoed { echo, .. } => {
                    assert!(content_is_typed(&echo.content), "{name}: {echo:?}");
                }
                _ => {}
            }
        }
    }
}

#[test]
fn payloads_and_events_round_trip_through_our_own_json() {
    for name in common::all_fixtures() {
        let payload = common::payload(&name);
        let json = serde_json::to_vec(&payload).unwrap();
        let back = WebhookPayload::from_slice(&json).unwrap();
        assert_eq!(back, payload, "{name}");

        for event in payload.into_events() {
            let json = serde_json::to_string(&event).unwrap();
            let back: WebhookEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(back, event, "{name}: {json}");
            let tag: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(tag["event"], event.kind(), "{name}");
        }
    }
}

#[test]
fn every_documented_field_has_a_fixture() {
    let mut seen = std::collections::BTreeSet::new();
    for name in common::all_fixtures() {
        for entry in common::payload(&name).entry {
            for change in entry.changes {
                seen.insert(change.field);
            }
        }
    }
    for field in [
        "account_alerts",
        "account_review_update",
        "account_settings_update",
        "account_update",
        "automatic_events",
        "business_capability_update",
        "business_username_updates",
        "calls",
        "flows",
        "group_lifecycle_update",
        "group_participants_update",
        "group_settings_update",
        "group_status_update",
        "history",
        "message_template_components_update",
        "message_template_quality_update",
        "message_template_status_update",
        "messages",
        "partner_solutions",
        "payment_configuration_update",
        "phone_number_name_update",
        "phone_number_quality_update",
        "security",
        "smb_app_state_sync",
        "smb_message_echoes",
        "template_category_update",
        "template_correct_category_detection",
        "user_id_update",
        "user_preferences",
    ] {
        assert!(seen.contains(field), "no fixture for `{field}`");
    }
}

#[test]
fn every_message_type_has_a_fixture() {
    let mut seen = std::collections::BTreeSet::new();
    for name in common::all_fixtures() {
        for event in common::events(&name) {
            if let WebhookEvent::MessageReceived { message, .. } = event {
                seen.insert(message.message_type().unwrap_or_default().to_owned());
            }
        }
    }
    for ty in [
        "audio",
        "button",
        "contacts",
        "document",
        "edit",
        "image",
        "interactive",
        "location",
        "order",
        "reaction",
        "revoke",
        "sticker",
        "system",
        "text",
        "unsupported",
        "video",
    ] {
        assert!(seen.contains(ty), "no fixture for message type `{ty}`");
    }
}

#[test]
fn change_value_variant_matches_field_for_group_fields() {
    let p = common::payload("fields/group_status_update.json");
    assert!(matches!(
        p.entry[0].changes[0].value,
        ChangeValue::GroupStatusUpdate(_)
    ));
}
