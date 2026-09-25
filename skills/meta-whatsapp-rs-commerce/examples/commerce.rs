//! Reference code for the `meta-whatsapp-rs-commerce` skill: commerce settings,
//! product and catalog messages (inside the 24-hour window), catalog and
//! multi-product templates (any time), and carts coming back as `order`
//! webhooks.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use meta_whatsapp_rs::client::messages::{Messages, ProductSection};
use meta_whatsapp_rs::client::templates::{
    Button, MpmSection, TemplateCategory, TemplateComponent, TemplateDefinition,
};
use meta_whatsapp_rs::core::ids::CatalogId;
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::webhooks::fields::{MessageContent as Inbound, OrderContent};

/// Show the storefront icon and keep the cart on.
pub async fn open_shop(client: &Client, phone_number_id: PhoneNumberId) -> meta_whatsapp_rs::Result<()> {
    let commerce = client.commerce(phone_number_id);
    commerce.set_catalog_visible(true).await?; // hidden by default
    commerce.set_cart_enabled(true).await // Meta's default
}

/// Free-form product messages: only inside the customer service window.
pub async fn show_products(
    messages: &Messages,
    to: Recipient,
    catalog_id: CatalogId, // connected to the WABA in Commerce Manager
) -> meta_whatsapp_rs::Result<()> {
    let one = OutboundMessage::product(to.clone(), catalog_id.clone(), "SKU-1");
    messages.send(&one).await?;
    let picks = OutboundMessage::product_list(
        to.clone(),
        "Autumn picks", // header, required
        "Tap to see more",
        catalog_id,
        [
            ProductSection::new("Shirts", ["SKU-1", "SKU-2"]),
            ProductSection::new("Belts", ["SKU-9"]),
        ],
    ); // at most 30 products; titles required with several sections
    messages.send(&picks).await?;
    messages
        .send(&OutboundMessage::catalog(to, "Browse the whole shop"))
        .await?;
    Ok(())
}

/// A multi-product template, created once, sent any time.
pub fn mpm_template() -> TemplateDefinition {
    TemplateDefinition::new("autumn_picks", "en_US", TemplateCategory::Marketing)
        .component(TemplateComponent::header_text("Autumn picks"))
        .component(TemplateComponent::body_positional(
            "Hi {{1}}, picked for you.",
            ["Alex"],
        ))
        .component(TemplateComponent::buttons([Button::mpm("View items")]))
}

/// Its invocation: the products are chosen at send time.
pub fn mpm_invocation() -> TemplateMessage {
    TemplateMessage::new("autumn_picks", "en_US")
        .body([Parameter::text("Alex")])
        .mpm_button(0, "SKU-1", [MpmSection::new("Shirts", ["SKU-1", "SKU-2"])])
}

/// A cart from the customer: re-check price and stock on your side.
pub fn cart(event: &WebhookEvent) -> Option<(&MessageId, &OrderContent)> {
    let WebhookEvent::MessageReceived { message, .. } = event else {
        return None;
    };
    match &message.content {
        Inbound::Order(order) => Some((&message.id, order)), // idempotency key: the message id
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use meta_whatsapp_rs::core::testing::ScriptedTransport;

    use super::*;

    #[tokio::test]
    async fn product_messages_are_interactive() {
        let transport = ScriptedTransport::new();
        for _ in 0..3 {
            transport.push_json(200, json!({"messages": [{"id": "wamid.OUT"}]}));
        }
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .build()
            .unwrap();
        show_products(
            &client.messages("106540352242922"),
            Recipient::phone("+16505551234"),
            "194836987003835".into(),
        )
        .await
        .unwrap();
        let kinds: Vec<_> = transport
            .requests()
            .iter()
            .map(|r| r.json().unwrap()["interactive"]["type"].clone())
            .collect();
        assert_eq!(
            kinds,
            [
                json!("product"),
                json!("product_list"),
                json!("catalog_message")
            ]
        );
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn templates_validate() {
        assert!(mpm_template().validate().is_ok());
        assert!(mpm_invocation().validate().is_ok());
    }

    #[test]
    fn an_order_webhook_is_a_cart() {
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": "messages", "value": {"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": "106540352242922"},
                "contacts": [{"profile": {"name": "Sheena Nelson"}, "user_id": "US.1"}],
                "messages": [{"from_user_id": "US.1", "id": "wamid.ORDER", "timestamp": "1750096325",
                    "type": "order", "order": {"catalog_id": "194836987003835", "text": "Love these!",
                    "product_items": [{"product_retailer_id": "SKU-1", "quantity": 2,
                        "item_price": 30, "currency": "USD"}]}}]}}]}]});
        let events = meta_whatsapp_rs::webhooks::WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events();
        let (id, order) = cart(&events[0]).unwrap();
        assert_eq!(id.as_str(), "wamid.ORDER");
        assert_eq!(order.product_items[0].quantity, Some(2));
    }
}
