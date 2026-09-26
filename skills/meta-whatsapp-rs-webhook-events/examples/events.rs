//! Reference code for the `meta-whatsapp-rs-webhook-events` skill: turning each
//! `WebhookEvent` into what your application does, keyed by BSUID, with a
//! place for everything Meta adds later.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use meta_whatsapp_rs::client::common::QualityRating;
use meta_whatsapp_rs::core::ids::MessageId;
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::webhooks::fields::{
    HandoverType, InteractiveReply, MessageContent as Inbound, MessageStatus, TemplateQualityScore,
};

/// What the application does with one event.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// A customer wrote: key them by BSUID; `wa_id` may be absent.
    Message {
        number: PhoneNumberId,
        customer: String,
        text: Option<String>,
    },
    ButtonTapped {
        customer: String,
        id: String,
    },
    Delivered {
        id: MessageId,
        callback: Option<String>,
    },
    Failed {
        id: MessageId,
        kinds: Vec<ErrorKind>,
    },
    CustomerRenamed {
        previous: UserId,
        current: UserId,
    }, // re-key what you stored
    TemplateStatus {
        name: String,
        status: String,
    },
    /// Conversation Routing: a copy of a thread another responder owns.
    /// Record it; never reply.
    Observed {
        number: PhoneNumberId,
    },
    /// Conversation Routing: whether you own the thread with this user now.
    Ownership {
        number: PhoneNumberId,
        user: Option<String>,
        owner: bool,
    },
    /// A field or shape this meta-whatsapp-rs version does not type: log the name.
    Untyped(String),
    Ignore,
}

/// The customer's stable key: the BSUID, else the phone number.
fn customer(message: &meta_whatsapp_rs::webhooks::fields::InboundMessage) -> Option<String> {
    message
        .from_user_id
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| message.from.as_ref().map(ToString::to_string))
}

/// `WebhookEvent` is `#[non_exhaustive]`: the `_` arm is required.
pub fn action(event: &WebhookEvent) -> Action {
    match event {
        WebhookEvent::MessageReceived {
            phone_number_id,
            message,
            ..
        } => {
            let Some(customer) = customer(message) else {
                return Action::Ignore;
            };
            match &message.content {
                Inbound::Text(text) => Action::Message {
                    number: phone_number_id.clone(),
                    customer,
                    text: Some(text.body.clone()),
                },
                Inbound::Interactive(InteractiveReply::ButtonReply(button)) => {
                    Action::ButtonTapped {
                        customer,
                        id: button.id.clone(),
                    }
                }
                // Media, location, orders, reactions… and Invalid/Unknown
                // for types and shapes Meta adds: never fail on them.
                _ => Action::Message {
                    number: phone_number_id.clone(),
                    customer,
                    text: None,
                },
            }
        }
        WebhookEvent::StatusUpdated { status, .. } => match status.status {
            MessageStatus::Delivered | MessageStatus::Read => Action::Delivered {
                id: status.id.clone(),
                callback: status.biz_opaque_callback_data.clone(), // your OutboundMessage::callback_data
            },
            MessageStatus::Failed => Action::Failed {
                id: status.id.clone(),
                kinds: status
                    .errors
                    .iter()
                    .map(meta_whatsapp_rs::GraphApiError::kind)
                    .collect(), // 131049/131050 arrive here
            },
            _ => Action::Ignore, // sent, played, and values Meta adds (Other)
        },
        WebhookEvent::UserIdChanged { update, .. } => Action::CustomerRenamed {
            previous: update.user_id.previous.clone(),
            current: update.user_id.current.clone(),
        },
        WebhookEvent::TemplateStatusUpdated { update, .. } => Action::TemplateStatus {
            name: update.message_template_name.clone(),
            status: update.event.as_str().to_owned(),
        },
        WebhookEvent::ThreadControlChanged {
            phone_number_id,
            update,
            ..
        } => Action::Ownership {
            number: phone_number_id.clone(),
            // Meta may omit it: then key by the user you track for the thread.
            user: update
                .sender
                .as_ref()
                .and_then(|s| s.phone_number.as_ref())
                .map(ToString::to_string),
            // control_passed: reply to the user; control_taken: stop.
            owner: update.handover_type == HandoverType::ControlPassed,
        },
        // A copy of a thread another responder owns: record it, never reply.
        WebhookEvent::StandbyObserved {
            phone_number_id, ..
        } => Action::Observed {
            number: phone_number_id.clone(),
        },
        WebhookEvent::Unknown { field, .. } => Action::Untyped(field.clone()),
        WebhookEvent::Unparsed { .. } => Action::Untyped("(unparsed body)".into()), // alert on it
        _ => Action::Ignore,
    }
}

/// A `TemplateQualityUpdated` score as the Graph API's `QualityRating`
/// (templates and phone numbers read with the client): the same scale in
/// two types. Convert through the wire value: `QualityRating` parses it
/// case-insensitively, and `NA`, which only it names, is `Other("NA")` on
/// the webhook side.
pub fn quality(score: &TemplateQualityScore) -> QualityRating {
    match score.as_str().parse::<QualityRating>() {
        Ok(rating) => rating,
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn events(value: &Value) -> Vec<WebhookEvent> {
        meta_whatsapp_rs::webhooks::WebhookPayload::from_slice(value.to_string().as_bytes())
            .unwrap()
            .into_events()
    }

    fn messages_change(value: &Value) -> Value {
        json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": "messages", "value": value}]}]})
    }

    #[test]
    fn a_bsuid_only_text() {
        let body = messages_change(&json!({"messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": "106540352242922"},
            "contacts": [{"profile": {"name": "Sheena"}, "user_id": "US.13491208655302741918"}],
            "messages": [{"from_user_id": "US.13491208655302741918", "id": "wamid.1",
                "timestamp": "1749416383", "type": "text", "text": {"body": "Navy?"}}]}));
        assert_eq!(
            action(&events(&body)[0]),
            Action::Message {
                number: "106540352242922".into(),
                customer: "US.13491208655302741918".into(),
                text: Some("Navy?".into())
            }
        );
    }

    #[test]
    fn statuses_carry_callback_data_and_errors() {
        let body = messages_change(&json!({"messaging_product": "whatsapp",
        "metadata": {"display_phone_number": "15550783881", "phone_number_id": "106540352242922"},
        "statuses": [
            {"id": "wamid.A", "status": "delivered", "timestamp": "1751142888",
             "recipient_id": "16505551234", "biz_opaque_callback_data": "order:860198"},
            {"id": "wamid.B", "status": "failed", "timestamp": "1751142889",
             "recipient_id": "16505551234", "errors": [{"code": 131050, "title": "opted out"}]}
        ]}));
        let events = events(&body);
        assert_eq!(
            action(&events[0]),
            Action::Delivered {
                id: "wamid.A".into(),
                callback: Some("order:860198".into())
            }
        );
        assert_eq!(
            action(&events[1]),
            Action::Failed {
                id: "wamid.B".into(),
                kinds: vec![ErrorKind::MarketingOptedOut]
            }
        );
    }

    /// `message_template_quality_update`: the webhook's scores map onto the
    /// client's by wire value; `NA` and other spellings are `Other` on the
    /// webhook side (it matches exactly) and parse on the client side.
    #[test]
    fn template_quality_scores_map_by_wire_value() {
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "time": 1674864290, "changes": [{"field": "message_template_quality_update", "value": {
                "previous_quality_score": "GREEN", "new_quality_score": "YELLOW",
                "message_template_id": 806_312_974_732_579_u64,
                "message_template_name": "welcome_template",
                "message_template_language": "en-US"}}]}]});
        let WebhookEvent::TemplateQualityUpdated { update, .. } = &events(&body)[0] else {
            panic!("not a quality update")
        };
        assert_eq!(quality(&update.new_quality_score), QualityRating::Yellow);
        let previous = update.previous_quality_score.as_ref().unwrap();
        assert_eq!(quality(previous), QualityRating::Green);
        for (webhook, client) in [
            ("RED", QualityRating::Red),
            ("UNKNOWN", QualityRating::Unknown),
            ("NA", QualityRating::NotApplicable),
            ("green", QualityRating::Green),
            ("PURPLE", QualityRating::Other("PURPLE".into())),
        ] {
            assert_eq!(
                quality(&TemplateQualityScore::from(webhook)),
                client,
                "{webhook}"
            );
        }
        assert_eq!(
            TemplateQualityScore::from("NA"),
            TemplateQualityScore::Other("NA".into()),
            "the webhook documents no NA"
        );
        assert_eq!(
            TemplateQualityScore::from("green"),
            TemplateQualityScore::Other("green".into()),
            "and matches exactly"
        );
        assert_eq!(
            TemplateQualityScore::from(QualityRating::NotApplicable.as_str()),
            TemplateQualityScore::Other("NA".into())
        );
    }

    #[test]
    fn handovers_set_thread_ownership_and_standby_is_never_answered() {
        let handover = |kind: &str| {
            json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
                "changes": [{"field": "messaging_handovers", "value": {
                    "messaging_product": "whatsapp",
                    "sender": {"phone_number": "16505551234"},
                    "recipient": {"phone_number_id": "106540352242922",
                                  "display_phone_number": "15550783881"},
                    "type": kind, "timestamp": "1750101000",
                    kind: {"previous_owner_role": "ai_agent", "new_owner_role": "escalation"}}}]}]})
        };
        for (kind, owner) in [("control_passed", true), ("control_taken", false)] {
            assert_eq!(
                action(&events(&handover(kind))[0]),
                Action::Ownership {
                    number: "106540352242922".into(),
                    user: Some("16505551234".into()),
                    owner
                }
            );
        }
        let standby = json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": "standby", "value": {"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": "106540352242922"},
                "standby": {"messages": [{"from": "16505551234", "id": "wamid.S",
                    "timestamp": "1750101000", "type": "text", "text": {"body": "Hi"}}]}}}]}]});
        assert_eq!(
            action(&events(&standby)[0]),
            Action::Observed {
                number: "106540352242922".into()
            }
        );
    }

    #[test]
    fn new_fields_arrive_as_unknown() {
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "1",
            "changes": [{"field": "some_field_meta_adds_next_year", "value": {"x": 1}}]}]});
        assert_eq!(
            action(&events(&body)[0]),
            Action::Untyped("some_field_meta_adds_next_year".into())
        );
    }
}
