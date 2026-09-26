//! `messages` field: every inbound type, statuses, errors, BSUID identities.
//! Fixtures are the examples of the `webhooks/reference/messages/*` pages.

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use meta_whatsapp_core::ErrorKind;
use meta_whatsapp_webhooks::fields::{
    CallPermissionResponse, CallPermissionSource, ContactShareOrigin, InboundMessage,
    InteractiveReply, MessageContent, MessageStatus, PricingCategory, PricingModel, PricingType,
    ReferralMediaType, ReferralSourceType, Status, SystemMessageType,
};
use meta_whatsapp_webhooks::{WebhookEvent, WebhookPayload};
use pretty_assertions::assert_eq;
use serde_json::json;
use time::OffsetDateTime;

/// The single inbound message of a fixture, with its matched contact.
fn only_message(
    name: &str,
) -> (
    Option<meta_whatsapp_webhooks::fields::Contact>,
    InboundMessage,
) {
    let mut events = common::events(name);
    assert_eq!(events.len(), 1, "{name}: {events:?}");
    match events.remove(0) {
        WebhookEvent::MessageReceived {
            waba_id,
            phone_number_id,
            display_phone_number,
            contact,
            message,
            conversation_context,
        } => {
            // No reference example carries Conversation Routing's summary.
            assert_eq!(conversation_context, None, "{name}");
            // The "Message business" example uses another WABA id.
            assert!(
                ["102290129340398", "419561257915477"].contains(&waba_id.as_str()),
                "{name}"
            );
            assert_eq!(phone_number_id.as_str(), "106540352242922", "{name}");
            assert_eq!(display_phone_number, "15550783881", "{name}");
            (contact, *message)
        }
        other => panic!("{name}: {other:?}"),
    }
}

fn statuses(name: &str) -> Vec<(Option<meta_whatsapp_webhooks::fields::Contact>, Status)> {
    common::events(name)
        .into_iter()
        .map(|e| match e {
            WebhookEvent::StatusUpdated {
                contact, status, ..
            } => (contact, *status),
            other => panic!("{name}: {other:?}"),
        })
        .collect()
}

fn ts(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).unwrap()
}

#[test]
fn text() {
    let (contact, m) = only_message("messages/text.json");
    let contact = contact.expect("contact matched by wa_id");
    assert_eq!(contact.name(), Some("Sheena Nelson"));
    assert_eq!(contact.wa_id.as_ref().unwrap().as_str(), "16505551234");
    assert_eq!(m.from.as_ref().unwrap().as_str(), "16505551234");
    assert_eq!(
        m.id.as_str(),
        "wamid.HBgLMTY1MDM4Nzk0MzkVAgASGBQzQTRBNjU5OUFFRTAzODEwMTQ0RgA="
    );
    assert_eq!(m.timestamp, ts(1749416383));
    let MessageContent::Text(text) = &m.content else {
        panic!("{m:?}")
    };
    assert_eq!(text.body, "Does it come in another color?");
    assert_eq!(common::events("messages/overview_text.json").len(), 1);
}

#[test]
fn text_from_message_business_button_has_referred_product() {
    let (_, m) = only_message("messages/text_message_business_button.json");
    let ctx = m.context.unwrap();
    assert_eq!(ctx.from.as_deref(), Some("15550783881"));
    assert_eq!(
        ctx.id.unwrap().as_str(),
        "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGA9wcm9kdWN0X2lucXVpcnkA"
    );
    let product = ctx.referred_product.unwrap();
    assert_eq!(product.catalog_id.as_str(), "194836987003835");
    assert_eq!(product.product_retailer_id, "di9ozbzfi4");
}

#[test]
fn text_from_click_to_whatsapp_ad_has_referral() {
    let (_, m) = only_message("messages/text_ctwa_referral.json");
    let r = m.referral.unwrap();
    assert_eq!(r.source_url.as_deref(), Some("https://fb.me/3cr4Wqqkv"));
    assert_eq!(r.source_id.as_deref(), Some("120226305854810726"));
    assert_eq!(r.source_type, Some(ReferralSourceType::Ad));
    assert_eq!(r.body.as_deref(), Some("Summer Succulents are here!"));
    assert_eq!(r.headline.as_deref(), Some("Chat with us"));
    assert_eq!(r.media_type, Some(ReferralMediaType::Image));
    assert!(r.image_url.is_some() && r.video_url.is_none());
    assert!(
        r.ctwa_clid
            .unwrap()
            .starts_with("Aff-n8ZTODiE79d22KtAwQKj9e")
    );
    assert_eq!(
        r.welcome_message.unwrap().text.as_deref(),
        Some("Hi there! Let us know how we can help!")
    );
}

#[test]
fn media_types() {
    let (_, m) = only_message("messages/image.json");
    let MessageContent::Image(img) = m.content else {
        panic!()
    };
    assert_eq!(img.caption.as_deref(), Some("Taj Mahal"));
    assert_eq!(img.mime_type.as_deref(), Some("image/jpeg"));
    assert_eq!(
        img.sha256.as_deref(),
        Some("SfInY0gGKTsJlUWbwxC1k+FAD0FZHvzwfpvO0zX0GUI=")
    );
    assert_eq!(img.id.unwrap().as_str(), "1003383421387256");
    assert!(img.url.unwrap().starts_with("https://lookaside.fbsbx.com/"));

    let (_, m) = only_message("messages/audio.json");
    let MessageContent::Audio(audio) = m.content else {
        panic!()
    };
    assert_eq!(audio.voice, Some(true));
    assert_eq!(audio.mime_type.as_deref(), Some("audio/ogg; codecs=opus"));
    assert_eq!(audio.id.unwrap().as_str(), "1908647269898587");

    let (_, m) = only_message("messages/video.json");
    let MessageContent::Video(video) = m.content else {
        panic!()
    };
    assert_eq!(video.caption.as_deref(), Some("Timelapse of growth"));
    assert_eq!(video.id.unwrap().as_str(), "731675419373506");

    let (_, m) = only_message("messages/document.json");
    let MessageContent::Document(doc) = m.content else {
        panic!()
    };
    assert_eq!(doc.filename.as_deref(), Some("receipt.pdf"));
    assert_eq!(doc.caption.as_deref(), Some("my receipt"));
    assert_eq!(doc.mime_type.as_deref(), Some("application/pdf"));

    let (_, m) = only_message("messages/sticker.json");
    let MessageContent::Sticker(sticker) = m.content else {
        panic!()
    };
    assert_eq!(sticker.animated, Some(true));
    assert_eq!(sticker.mime_type.as_deref(), Some("image/webp"));
}

#[test]
fn location() {
    let (_, m) = only_message("messages/location.json");
    let MessageContent::Location(loc) = m.content else {
        panic!()
    };
    assert!((loc.latitude - 37.44221496582).abs() < 1e-12);
    assert!((loc.longitude - -122.16165924072).abs() < 1e-12);
    assert_eq!(loc.name.as_deref(), Some("Philz Coffee"));
    assert_eq!(
        loc.address.as_deref(),
        Some("101 Forest Ave, Palo Alto, CA 94301")
    );
    assert_eq!(loc.url.as_deref(), Some("https://philzcoffee.com/"));
}

#[test]
fn contacts() {
    let (_, m) = only_message("messages/contacts.json");
    let MessageContent::Contacts(cards) = m.content else {
        panic!()
    };
    assert_eq!(cards.len(), 1);
    let card = &cards[0];
    let name = card.name.as_ref().unwrap();
    assert_eq!(name.formatted_name.as_deref(), Some("Barbara J. Johnson"));
    assert_eq!(name.first_name.as_deref(), Some("Barbara"));
    assert_eq!(name.last_name.as_deref(), Some("Johnson"));
    assert_eq!(
        card.org.as_ref().unwrap().company.as_deref(),
        Some("Social Tsunami")
    );
    assert_eq!(card.phones[0].phone.as_deref(), Some("+1 (415) 555-0829"));
    assert_eq!(
        card.phones[0].wa_id.as_ref().unwrap().as_str(),
        "14125550829"
    );
    assert_eq!(card.phones[0].phone_type.as_deref(), Some("MOBILE"));
    assert!(card.addresses.is_empty() && card.emails.is_empty() && card.urls.is_empty());
}

#[test]
fn interactive_list_and_button_replies() {
    let (_, m) = only_message("messages/interactive_list_reply.json");
    assert_eq!(
        m.context.unwrap().id.unwrap().as_str(),
        "wamid.HBgLMTQxMjU1NTA4MjkVAgASGBQzQUNCNjk5RDUwNUZGMUZEM0VBRAA="
    );
    let MessageContent::Interactive(InteractiveReply::ListReply(row)) = m.content else {
        panic!()
    };
    assert_eq!(row.id, "priority_express");
    assert_eq!(row.title, "Priority Mail Express");
    assert_eq!(row.description.as_deref(), Some("Next Day to 2 Days"));

    let (_, m) = only_message("messages/interactive_button_reply.json");
    let MessageContent::Interactive(InteractiveReply::ButtonReply(button)) = m.content else {
        panic!()
    };
    assert_eq!(button.id, "cancel-button");
    assert_eq!(button.title, "Cancel");
}

#[test]
fn interactive_nfm_replies_for_flows_and_addresses() {
    let (contact, m) = only_message("messages/interactive_nfm_reply_flow.json");
    assert_eq!(
        contact.unwrap().user_id.unwrap().as_str(),
        "US.13491208655302741918"
    );
    let MessageContent::Interactive(InteractiveReply::NfmReply(nfm)) = m.content else {
        panic!()
    };
    assert_eq!(nfm.name.as_deref(), Some("flow"));
    assert_eq!(nfm.body.as_deref(), Some("Sent"));
    let response: serde_json::Value = nfm.response().unwrap();
    assert_eq!(response["flow_token"], "order-7781");

    let (_, m) = only_message("messages/interactive_nfm_reply_address.json");
    let MessageContent::Interactive(InteractiveReply::NfmReply(nfm)) = m.content else {
        panic!()
    };
    assert_eq!(nfm.name.as_deref(), Some("address_message"));
    let response: serde_json::Value = nfm.response().unwrap();
    assert_eq!(response["values"]["in_pin_code"], "400063");
}

#[test]
fn interactive_call_permission_reply() {
    let (contact, m) = only_message("messages/interactive_call_permission_reply.json");
    assert_eq!(contact.unwrap().username(), Some("realsheenanelson"));
    assert_eq!(
        m.from_parent_user_id.unwrap().as_str(),
        "US.ENT.11815799212886844830"
    );
    let MessageContent::Interactive(InteractiveReply::CallPermissionReply(reply)) = m.content
    else {
        panic!()
    };
    assert_eq!(reply.response, CallPermissionResponse::Accept);
    assert_eq!(reply.is_permanent, Some(false));
    assert_eq!(reply.expiration_timestamp, Some(ts(1768550400)));
    assert_eq!(
        reply.response_source,
        Some(CallPermissionSource::UserAction)
    );
}

#[test]
fn button_order_reaction() {
    let (_, m) = only_message("messages/button.json");
    let MessageContent::Button(b) = m.content else {
        panic!()
    };
    assert_eq!(b.payload.as_deref(), Some("Unsubscribe"));
    assert_eq!(b.text.as_deref(), Some("Unsubscribe"));

    let (_, m) = only_message("messages/order.json");
    let MessageContent::Order(order) = m.content else {
        panic!()
    };
    assert_eq!(order.catalog_id.unwrap().as_str(), "194836987003835");
    assert_eq!(order.text.as_deref(), Some("Love these!"));
    assert_eq!(order.product_items.len(), 2);
    assert_eq!(order.product_items[0].product_retailer_id, "di9ozbzfi4");
    assert_eq!(order.product_items[0].quantity, Some(2));
    assert_eq!(order.product_items[0].item_price, Some(30.0));
    assert_eq!(order.product_items[0].currency.as_deref(), Some("USD"));

    let (_, m) = only_message("messages/reaction.json");
    let MessageContent::Reaction(r) = m.content else {
        panic!()
    };
    assert_eq!(r.emoji.as_deref(), Some("👍"));
    assert_eq!(
        r.message_id.as_str(),
        "wamid.HBgLMTQxMjU1NTA4MjkVAgASGBQzQUNCNjk5RDUwNUZGMUZEM0VBRAA="
    );
    let (_, m) = only_message("messages/reaction_removed.json");
    let MessageContent::Reaction(r) = m.content else {
        panic!()
    };
    assert_eq!(r.emoji, None);
}

#[test]
fn system_unsupported_edit_revoke() {
    let (contact, m) = only_message("messages/system.json");
    assert!(contact.is_none(), "system messages carry no contacts");
    let MessageContent::System(sys) = m.content else {
        panic!()
    };
    assert_eq!(sys.system_type, Some(SystemMessageType::UserChangedNumber));
    assert_eq!(sys.wa_id.unwrap().as_str(), "12195555358");
    assert!(sys.body.unwrap().contains("changed from 16505551234"));

    let (_, m) = only_message("messages/unsupported.json");
    let MessageContent::Unsupported(u) = &m.content else {
        panic!()
    };
    assert_eq!(u.unsupported_type.as_deref(), Some("edit"));
    assert_eq!(m.errors.len(), 1);
    assert_eq!(m.errors[0].code, 131051);
    assert_eq!(m.errors[0].kind(), ErrorKind::UnsupportedMessageType);
    assert_eq!(
        m.errors[0].details(),
        Some("Message type is currently not supported.")
    );

    let (_, m) = only_message("messages/edit.json");
    let MessageContent::Edit(edit) = m.content else {
        panic!()
    };
    assert_eq!(
        edit.original_message_id.as_str(),
        "wamid.HBgLMTQxMjU1NTA4MjkVAgASGBQzQUNCNjk5RDUwNUZGMUZEM0VBRAA="
    );
    assert_eq!(edit.message.context.unwrap().id.unwrap().as_str(), "M0");
    let MessageContent::Image(img) = *edit.message.content else {
        panic!()
    };
    assert_eq!(img.caption.as_deref(), Some("Updated image caption"));

    let (_, m) = only_message("messages/revoke.json");
    let MessageContent::Revoke(r) = m.content else {
        panic!()
    };
    assert_eq!(
        r.original_message_id.as_str(),
        "wamid.HBgLMTQxMjU1NTA4MjkVAgASGBQzQUNCNjk5RDUwNUZGMUZEM0VBRAA="
    );
}

#[test]
fn group_message_carries_group_id() {
    let (contact, m) = only_message("messages/group_text.json");
    assert_eq!(contact.unwrap().name(), Some("Tiago Mingo"));
    assert_eq!(
        m.group_id.unwrap().as_str(),
        "HBgLMTY1MDM4Nzk0MzkVAgASGBQzQTRBNjU5OUFFRTAzODEwMTQ0RgA"
    );
    assert!(matches!(m.content, MessageContent::Text(_)));
}

#[test]
fn statuses_with_pricing_and_conversation() {
    let [(contact, s)] = statuses("messages/status_sent.json").try_into().unwrap();
    assert!(contact.is_none(), "pre-BSUID example has no contacts");
    assert_eq!(s.status, MessageStatus::Sent);
    assert_eq!(s.recipient_id.as_deref(), Some("16505551234"));
    let conv = s.conversation.as_ref().unwrap();
    assert_eq!(conv.id.as_deref(), Some("72b14d6bd5407799e66f64d1b338e567"));
    assert_eq!(conv.expiration_timestamp, Some(ts(1750116480)));
    assert_eq!(
        conv.origin.as_ref().unwrap().origin_type,
        Some(PricingCategory::Marketing)
    );
    let pricing = s.effective_pricing().unwrap();
    assert_eq!(pricing.billable, Some(true));
    assert_eq!(pricing.pricing_model, Some(PricingModel::Pmp));
    assert_eq!(pricing.pricing_type, Some(PricingType::Regular));
    assert_eq!(pricing.category, Some(PricingCategory::Marketing));

    let [(_, s)] = statuses("messages/status_sent_v24.json")
        .try_into()
        .unwrap();
    assert!(s.conversation.is_none() && s.pricing.is_none());

    let [(_, s)] = statuses("messages/status_delivered_cbp.json")
        .try_into()
        .unwrap();
    assert_eq!(s.status, MessageStatus::Delivered);
    assert_eq!(s.pricing.unwrap().pricing_model, Some(PricingModel::Cbp));

    let [(contact, s)] = statuses("messages/status_free_customer_service.json")
        .try_into()
        .unwrap();
    assert_eq!(contact.unwrap().name(), Some("Sheena Nelson"));
    assert_eq!(s.biz_opaque_callback_data.as_deref(), Some("order-7781"));
    assert_eq!(
        s.recipient_identity_key_hash.as_deref(),
        Some("DF2lS5v2W6x=")
    );
    let pricing = s.pricing.unwrap();
    assert_eq!(pricing.pricing_type, Some(PricingType::FreeCustomerService));
    assert_eq!(pricing.category, Some(PricingCategory::Utility));
    assert_eq!(pricing.billable, Some(false));
}

#[test]
fn failed_status_carries_classified_error() {
    let [(_, s)] = statuses("messages/status_failed.json").try_into().unwrap();
    assert_eq!(s.status, MessageStatus::Failed);
    assert_eq!(s.errors.len(), 1);
    assert_eq!(s.errors[0].code, 131049);
    assert_eq!(s.errors[0].kind(), ErrorKind::EcosystemEngagementLimit);
    assert!(s.errors[0].href.is_some());
}

#[test]
fn group_statuses_keep_each_participant_and_both_pricing_placements() {
    let all = statuses("messages/group_statuses_aggregated.json");
    assert_eq!(all.len(), 3);
    let participants: Vec<_> = all
        .iter()
        .map(|(_, s)| s.recipient_participant_id.clone().unwrap())
        .collect();
    assert_eq!(participants, ["16505551234", "16505551235", "16505551236"]);
    for (_, s) in &all {
        assert_eq!(s.recipient_type.as_deref(), Some("group"));
        assert!(s.effective_pricing().is_some(), "{s:?}");
    }
    assert_eq!(
        all[0].1.effective_pricing().unwrap().category,
        Some(PricingCategory::GroupMarketing)
    );
    assert_eq!(
        all[2].1.effective_pricing().unwrap().category,
        Some(PricingCategory::GroupService)
    );
    // Same message id and status, different participants: distinct dedup keys.
    let keys: std::collections::BTreeSet<_> =
        common::events("messages/group_statuses_aggregated.json")
            .iter()
            .map(|e| e.dedup_key().unwrap())
            .collect();
    assert_eq!(keys.len(), 3, "{keys:?}");
}

#[test]
fn value_level_errors_become_error_events() {
    let events = common::events("messages/errors.json");
    let [
        WebhookEvent::ErrorReported {
            field,
            error,
            phone_number_id,
            ..
        },
    ] = events.as_slice()
    else {
        panic!("{events:?}")
    };
    assert_eq!(field, "messages");
    assert_eq!(phone_number_id.as_str(), "106540352242922");
    assert_eq!(error.code, 130429);
    assert_eq!(error.kind(), ErrorKind::RateLimited);
    assert_eq!(error.summary(), "Rate limit hit");
    assert_eq!(events[0].dedup_key(), None);
}

#[test]
fn bsuid_only_contact_without_wa_id_parses_and_matches() {
    let (contact, m) = only_message("bsuid/text_username_no_wa_id.json");
    assert!(m.from.is_none(), "phone number withheld");
    assert_eq!(
        m.from_user_id.as_ref().unwrap().as_str(),
        "US.13491208655302741918"
    );
    assert_eq!(
        m.from_parent_user_id.unwrap().as_str(),
        "US.ENT.11815799212886844830"
    );
    let contact = contact.expect("matched by BSUID");
    assert!(contact.wa_id.is_none());
    assert_eq!(contact.username(), Some("realsheenanelson"));
    assert_eq!(
        contact.parent_user_id.unwrap().as_str(),
        "US.ENT.11815799212886844830"
    );
}

#[test]
fn bsuid_statuses() {
    let [(contact, s)] = statuses("bsuid/status_delivered_parent_bsuid.json")
        .try_into()
        .unwrap();
    let contact = contact.unwrap();
    assert_eq!(contact.username(), Some("pablomorales"));
    assert_eq!(contact.wa_id.unwrap().as_str(), "16505551234");
    assert_eq!(
        s.recipient_user_id.unwrap().as_str(),
        "US.13491208655302741918"
    );
    assert_eq!(
        s.recipient_parent_user_id.unwrap().as_str(),
        "US.ENT.11815799212886844830"
    );

    let [(contact, s)] = statuses("bsuid/status_delivered_bsuid_only.json")
        .try_into()
        .unwrap();
    assert!(s.recipient_id.is_none());
    assert_eq!(
        contact.expect("matched by recipient_user_id").name(),
        Some("Pablo M.")
    );

    let [(contact, s)] = statuses("bsuid/status_failed_no_contacts.json")
        .try_into()
        .unwrap();
    assert!(contact.is_none());
    assert!(s.recipient_user_id.is_none());
    assert_eq!(s.errors[0].code, 131049);
}

#[test]
fn bsuid_shared_contact_card_and_changed_user_id() {
    let (contact, m) = only_message("bsuid/contacts_shared_vcard.json");
    assert_eq!(
        contact.unwrap().user_id.unwrap().as_str(),
        "US.13491208655302741918"
    );
    let MessageContent::Contacts(cards) = m.content else {
        panic!()
    };
    assert_eq!(cards[0].origin, Some(ContactShareOrigin::SharedDirectly));
    assert!(
        cards[0]
            .vcard
            .as_deref()
            .unwrap()
            .starts_with("BEGIN:VCARD")
    );
    assert_eq!(cards[1].origin, Some(ContactShareOrigin::ContactRequest));
    assert!(cards[1].vcard.is_none());

    let (_, m) = only_message("bsuid/system_user_changed_user_id.json");
    let MessageContent::System(sys) = m.content else {
        panic!()
    };
    assert_eq!(sys.system_type, Some(SystemMessageType::UserChangedUserId));
    assert_eq!(sys.user_id.unwrap().as_str(), "US.29847561203948576612");
    assert!(sys.wa_id.is_none());
}

#[test]
fn unknown_message_type_parses_to_unknown_and_keeps_its_payload() {
    let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "1", "changes": [{
        "field": "messages",
        "value": {
            "messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "1555", "phone_number_id": "42"},
            "contacts": [{"profile": {"name": "A"}, "user_id": "US.1"}],
            "messages": [{"from_user_id": "US.1", "id": "wamid.P", "timestamp": "1767168000",
                          "type": "poll_creation", "poll_creation": {"question": "Lunch?"}}]
        }
    }]}]});
    let events = WebhookPayload::from_slice(body.to_string().as_bytes())
        .unwrap()
        .into_events();
    let [
        WebhookEvent::MessageReceived {
            contact, message, ..
        },
    ] = events.as_slice()
    else {
        panic!("{events:?}")
    };
    assert_eq!(contact.as_ref().unwrap().name(), Some("A"));
    let MessageContent::Unknown { message_type, raw } = &message.content else {
        panic!("{message:?}")
    };
    assert_eq!(message_type.as_deref(), Some("poll_creation"));
    assert_eq!(raw["poll_creation"]["question"], "Lunch?");
    assert_eq!(events[0].dedup_key().as_deref(), Some("wamid.P"));
}

/// `groups/groups-messaging` shows an `unsupported` group message with no
/// `unsupported` object at all; the draft turned it into `Invalid`.
#[test]
fn a_type_without_its_object_is_typed_when_the_object_is_all_optional() {
    let m: InboundMessage = serde_json::from_value(json!({
        "from": "16505551234", "group_id": "Y2FwaV9ncm91cDoxNjUwNTU1MTIzNDoxMjAzNjM0MDQ2OTQyMzM4MjAZD",
        "id": "wamid.G", "timestamp": "1750030073",
        "errors": [{"code": 130501, "message": "Message type is not currently supported",
                    "title": "Unsupported message type",
                    "error_data": {"details": "Message type is not currently supported"}}],
        "type": "unsupported"
    }))
    .unwrap();
    assert_eq!(
        m.content,
        MessageContent::Unsupported(meta_whatsapp_webhooks::fields::UnsupportedContent::default())
    );
    assert_eq!(m.errors[0].code, 130501);
    assert!(m.group_id.is_some());

    // A payload with required properties still cannot come from nothing.
    let m: InboundMessage = serde_json::from_value(json!({
        "id": "wamid.T", "timestamp": "1750030073", "type": "text"
    }))
    .unwrap();
    assert!(
        matches!(&m.content, MessageContent::Invalid { message_type, .. } if message_type == "text"),
        "{:?}",
        m.content
    );
}

/// Authentication-template button replies carry `from_logical_id`
/// (`templates/authentication-templates/copy-code-button-authentication-templates`).
#[test]
fn from_logical_id_is_kept() {
    let m: InboundMessage = serde_json::from_value(json!({
        "context": {"from": "12345678", "id": "wamid.C"},
        "from": "12345678", "id": "wamid.B", "timestamp": "1753919111",
        "from_logical_id": "131063108133020",
        "type": "button", "button": {"payload": "DID_NOT_REQUEST_CODE", "text": "I didn't request a code"}
    }))
    .unwrap();
    assert_eq!(m.from_logical_id.as_deref(), Some("131063108133020"));
    assert!(matches!(m.content, MessageContent::Button(_)));
}

/// `groups/*` pages print error codes quoted; one quoted code must not
/// turn the whole change (and its other statuses) into `Unknown`.
#[test]
fn quoted_error_codes_do_not_untype_the_change() {
    let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "1", "changes": [{
        "field": "messages",
        "value": {"messaging_product": "whatsapp",
                  "metadata": {"display_phone_number": "15550783881", "phone_number_id": "106540352242922"},
                  "statuses": [
                      {"id": "wamid.A", "status": "failed", "timestamp": "1750030073",
                       "recipient_id": "Y2FwaV9ncm91cA", "recipient_type": "group",
                       "errors": [{"code": "131049", "title": "Not delivered"}]},
                      {"id": "wamid.B", "status": "read", "timestamp": "1750030073",
                       "recipient_id": "16505551234"}
                  ]}
    }]}]});
    let events = WebhookPayload::from_slice(body.to_string().as_bytes())
        .unwrap()
        .into_events();
    assert_eq!(events.len(), 2, "{events:?}");
    let WebhookEvent::StatusUpdated { status, .. } = &events[0] else {
        panic!("{events:?}")
    };
    assert_eq!(status.errors[0].code, 131049);
    assert_eq!(status.errors[0].kind(), ErrorKind::EcosystemEngagementLimit);
}

#[test]
fn unknown_status_value_is_kept() {
    let s: Status = serde_json::from_value(json!({
        "id": "wamid.X", "status": "deleted", "timestamp": "1"
    }))
    .unwrap();
    assert_eq!(s.status, MessageStatus::Other("deleted".into()));
}
