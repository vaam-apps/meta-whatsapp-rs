//! Reference code for the `meta-whatsapp-rs-send-templates` skill: filling an approved
//! template's placeholders when sending it (`TemplateMessage`, `Parameter`).
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use meta_whatsapp_rs::client::messages::Messages;
use meta_whatsapp_rs::client::templates::{CarouselCardParameters, SendComponent};
use meta_whatsapp_rs::core::ids::MediaId;
use meta_whatsapp_rs::prelude::*;

/// Positional template `Hi {{1}}, order {{2}} has shipped.` with a document
/// header and a URL button whose URL ends in `{{1}}`.
pub fn order_update(invoice: MediaId) -> TemplateMessage {
    TemplateMessage::new("order_update", "en_US") // the language it was APPROVED in
        .header(Parameter::document_id(
            invoice,
            Some("invoice-860198.pdf".into()),
        ))
        .body([Parameter::text("Jessica"), Parameter::text("860198")]) // placeholder order
        .url_button(0, "860198") // index = the button's position in the template
}

/// Named template `Hi {{first_name}}, your total is {{total}}.`
pub fn welcome() -> TemplateMessage {
    TemplateMessage::new("welcome", "en_US").body([
        Parameter::named("first_name", "Jessica"),
        Parameter::named("total", "€134.80"),
    ])
}

/// A coupon (copy-code button), a quick reply payload and a limited-time offer.
pub fn autumn_offer(expires_at_ms: i64) -> TemplateMessage {
    TemplateMessage::new("autumn_offer", "en_US")
        .body([Parameter::named("first_name", "Jessica")])
        .limited_time_offer(expires_at_ms) // UNIX milliseconds
        .copy_code_button(0, "AUTUMN20") // at most 20 characters
        .quick_reply_button(1, "stop-promotions") // comes back in the Button webhook
}

/// A media card carousel: one entry per card, in card order.
pub fn carousel(images: [MediaId; 2]) -> TemplateMessage {
    let [first, second] = images;
    TemplateMessage::new("autumn_carousel", "en_US")
        .body([Parameter::text("Jessica")])
        .carousel([
            CarouselCardParameters::new(0, [SendComponent::header(Parameter::image_id(first))]),
            CarouselCardParameters::new(1, [SendComponent::header(Parameter::image_id(second))]),
        ])
}

/// Send it; a template reaches the customer outside the 24-hour window.
pub async fn send(
    messages: &Messages,
    to: Recipient,
    template: TemplateMessage,
) -> meta_whatsapp_rs::Result<SendResponse> {
    template.validate()?; // `send` does this too; the error names the field
    messages
        .send(&OutboundMessage::template(to, template))
        .await
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use serde_json::json;

    use super::*;

    #[test]
    fn positional_template_body() {
        let value = serde_json::to_value(order_update("1037543291543636".into())).unwrap();
        assert_eq!(
            value,
            json!({
                "name": "order_update",
                "language": {"code": "en_US"},
                "components": [
                    {"type": "header", "parameters": [{"type": "document",
                        "document": {"id": "1037543291543636", "filename": "invoice-860198.pdf"}}]},
                    {"type": "body", "parameters": [
                        {"type": "text", "text": "Jessica"},
                        {"type": "text", "text": "860198"}]},
                    {"type": "button", "sub_type": "url", "index": "0",
                        "parameters": [{"type": "text", "text": "860198"}]}
                ]
            })
        );
    }

    #[test]
    fn named_parameters_carry_their_names() {
        let value = serde_json::to_value(welcome()).unwrap();
        assert_eq!(
            value["components"][0]["parameters"][0],
            json!({"type": "text", "parameter_name": "first_name", "text": "Jessica"})
        );
    }

    #[test]
    fn send_time_limits() {
        assert!(autumn_offer(1_790_000_000_000).validate().is_ok());
        let long =
            TemplateMessage::new("autumn_offer", "en_US").copy_code_button(0, "X".repeat(21));
        assert!(long.validate().is_err());
        assert!(carousel(["1".into(), "2".into()]).validate().is_ok());
    }

    #[tokio::test]
    async fn sends_as_a_template_message() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"messages": [{"id": "wamid.OUT"}]}));
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .build()
            .unwrap();
        send(
            &client.messages("106540352242922"),
            Recipient::phone("+16505551234"),
            welcome(),
        )
        .await
        .unwrap();
        let body = transport.last_request().unwrap().json().unwrap();
        assert_eq!(body["type"], "template");
        assert_eq!(body["template"]["name"], "welcome");
        assert_eq!(transport.remaining(), 0);
    }
}
