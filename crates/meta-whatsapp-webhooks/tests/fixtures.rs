//! Every fixture (copied from Meta's doc examples, placeholders filled) must
//! parse fully typed, flatten to events, and survive our own serialization.

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use meta_whatsapp_webhooks::fields::MessageContent;
use meta_whatsapp_webhooks::{ChangeValue, WebhookEvent, WebhookPayload};
use pretty_assertions::assert_eq;
use serde_json::Value;

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

/// Every key in every fixture (i.e. every property Meta's examples show)
/// survives parse → serialize: nothing documented is silently dropped by a
/// struct that forgot the field. Values may be normalized (ids, timestamps
/// as strings), keys may not vanish.
#[test]
fn no_documented_property_is_dropped() {
    fn walk(orig: &Value, typed: &Value, path: &str, out: &mut Vec<String>) {
        match (orig, typed) {
            (Value::Object(a), Value::Object(b)) => {
                for (k, v) in a {
                    let p = format!("{path}.{k}");
                    match b.get(k) {
                        None if !v.is_null() => out.push(p),
                        None => {}
                        Some(w) => walk(v, w, &p, out),
                    }
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(a.len(), b.len(), "{path}");
                for (i, (v, w)) in a.iter().zip(b).enumerate() {
                    walk(v, w, &format!("{path}[{i}]"), out);
                }
            }
            _ => {}
        }
    }
    // Spellings accepted as aliases and written back under the canonical
    // name `recipient_participant_id` (see the `fields::messages` docs).
    let aliases = [".participant_recipient_id"];
    for name in common::all_fixtures() {
        let orig: Value = serde_json::from_slice(&common::fixture_bytes(&name)).unwrap();
        let typed = serde_json::to_value(common::payload(&name)).unwrap();
        let mut dropped = Vec::new();
        walk(&orig, &typed, "", &mut dropped);
        dropped.retain(|p| !aliases.iter().any(|alias| p.ends_with(alias)));
        assert!(dropped.is_empty(), "{name}: dropped {dropped:?}");
    }
}

/// Forward compatibility: Meta adds properties without notice. Inject an
/// unknown property into every object of every fixture; each must still
/// parse fully typed into the same kinds of events.
#[test]
fn unknown_properties_anywhere_are_ignored() {
    fn inject(v: &mut Value) {
        match v {
            Value::Object(map) => {
                for child in map.values_mut() {
                    inject(child);
                }
                map.insert(
                    "zz_added_by_meta_later".into(),
                    serde_json::json!({"nested": [1, "two", null]}),
                );
            }
            Value::Array(items) => items.iter_mut().for_each(inject),
            _ => {}
        }
    }
    let kinds = |p: WebhookPayload| -> Vec<&'static str> {
        p.into_events().iter().map(WebhookEvent::kind).collect()
    };
    for name in common::all_fixtures() {
        let mut v: Value = serde_json::from_slice(&common::fixture_bytes(&name)).unwrap();
        inject(&mut v);
        let payload = WebhookPayload::from_slice(&serde_json::to_vec(&v).unwrap())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        for entry in &payload.entry {
            for change in &entry.changes {
                assert!(
                    !change.value.is_unknown(),
                    "{name}: `{}` fell back: {:?}",
                    change.field,
                    change.parse_error
                );
            }
        }
        assert_eq!(
            kinds(payload.clone()),
            kinds(common::payload(&name)),
            "{name}"
        );
        for event in payload.into_events() {
            if let WebhookEvent::MessageReceived { message, .. } = &event {
                assert!(content_is_typed(&message.content), "{name}: {message:?}");
            }
        }
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
