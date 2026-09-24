//! Reference code for the `wa-rs-webhook-events` skill: turning each
//! `WebhookEvent` into what your application does, keyed by BSUID, with a
//! place for everything Meta adds later.
//!
//! wa-rs compiles this file and runs its tests in its own gate
//! (`crates/wa-rs/tests/skills.rs`).

use wa_rs::core::ids::MessageId;
use wa_rs::prelude::*;
use wa_rs::webhooks::fields::{InteractiveReply, MessageContent as Inbound, MessageStatus};

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
    /// A field or shape this wa-rs version does not type: log the name.
    Untyped(String),
    Ignore,
}

/// The customer's stable key: the BSUID, else the phone number.
fn customer(message: &wa_rs::webhooks::fields::InboundMessage) -> Option<String> {
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
                    .map(wa_rs::GraphApiError::kind)
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
        WebhookEvent::Unknown { field, .. } => Action::Untyped(field.clone()),
        WebhookEvent::Unparsed { .. } => Action::Untyped("(unparsed body)".into()), // alert on it
        _ => Action::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn events(value: &Value) -> Vec<WebhookEvent> {
        wa_rs::webhooks::WebhookPayload::from_slice(value.to_string().as_bytes())
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
