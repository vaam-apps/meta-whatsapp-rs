//! Reference code for the `meta-whatsapp-rs-interactive-messages` skill: reply
//! buttons, lists, CTA URL buttons, location requests, Flows and media
//! carousels, and what their local validation refuses.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use meta_whatsapp_rs::client::messages::{
    CardAction, CardHeader, CtaUrl, FlowParameters, FlowRef, Header, ListMessage, ListRow,
    ListSection, MediaCard, MediaCarousel, Messages, OutboundMessage, QuickReply, ReplyButton,
    ReplyButtons,
};
use meta_whatsapp_rs::prelude::*;

/// Up to three buttons; the tap comes back as `InteractiveReply::ButtonReply`.
pub fn delivery_slot_buttons(to: Recipient) -> OutboundMessage {
    let buttons = ReplyButtons::new(
        "Keep your delivery slot tomorrow, 9–12?",
        [
            ReplyButton::new("slot-keep", "Keep"), // id (≤ 256) comes back; title ≤ 20
            ReplyButton::new("slot-change", "Change"),
        ],
    )
    .header(Header::text("Order 860198"))
    .footer("Example Boutique");
    OutboundMessage::new(to, buttons)
}

/// Up to ten rows across all sections; comes back as `ListReply`.
pub fn delivery_slot_list(to: Recipient) -> OutboundMessage {
    let list = ListMessage::new(
        "Pick a delivery slot",
        "Slots", // the button that opens the list, ≤ 20
        [
            ListSection::new(
                "Tomorrow",
                [ListRow::new("t9", "9–12").description("Morning")],
            ),
            ListSection::new("Friday", [ListRow::new("f14", "14–18")]),
        ],
    );
    OutboundMessage::new(to, list)
}

/// A link button, a location request, a Flow.
pub async fn send_cta_location_flow(
    messages: &Messages,
    to: Recipient,
) -> meta_whatsapp_rs::Result<()> {
    let track = CtaUrl::new(
        "Track your parcel",
        "Track",
        "https://shop.example/t/860198",
    )
    .header(Header::text("Order 860198"));
    messages
        .send(&OutboundMessage::new(to.clone(), track))
        .await?;
    let ask = OutboundMessage::location_request(to.clone(), "Where should we deliver?");
    messages.send(&ask).await?;
    let fitting = FlowParameters::new(FlowRef::Name("fitting_v1".into()), "Book")
        .token("session-42") // your correlation id, echoed in the Flow's reply
        .navigate("WELCOME", Some(serde_json::json!({"customer": "Alex"})));
    let flow = OutboundMessage::flow(to, "Book a fitting", fitting);
    messages.send(&flow).await?;
    Ok(())
}

/// Two to ten cards; every card uses the same kind and number of buttons.
pub fn autumn_carousel(to: Recipient) -> OutboundMessage {
    let card = |image: &str, sku: &str| {
        MediaCard::new(
            CardHeader::Image(format!("https://shop.example/img/{image}.jpg")),
            CardAction::QuickReplies(vec![QuickReply::new(format!("buy-{sku}"), "Buy")]),
        )
        .body("Linen, navy")
    };
    let carousel = MediaCarousel::new(
        "Autumn picks",
        [card("shirt", "SKU-1"), card("belt", "SKU-2")],
    );
    OutboundMessage::new(to, carousel)
}

/// Every limit is checked before sending: the error names the JSON path.
pub fn why_refused(to: Recipient) -> Option<String> {
    let buttons = ReplyButtons::new(
        "Too many",
        ["a", "b", "c", "d"].map(|id| ReplyButton::new(id, id)),
    );
    let message = OutboundMessage::new(to, buttons);
    message.validate().err().map(|e| e.field) // Some("interactive.action.buttons")
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use serde_json::json;

    use super::*;

    fn to() -> Recipient {
        Recipient::phone("+16505551234")
    }

    #[test]
    fn button_and_list_bodies() {
        let buttons = serde_json::to_value(delivery_slot_buttons(to())).unwrap();
        assert_eq!(buttons["interactive"]["type"], "button");
        assert_eq!(
            buttons["interactive"]["action"]["buttons"][0],
            json!({"type": "reply", "reply": {"id": "slot-keep", "title": "Keep"}})
        );
        let list = serde_json::to_value(delivery_slot_list(to())).unwrap();
        assert_eq!(list["interactive"]["type"], "list");
        assert_eq!(list["interactive"]["action"]["button"], "Slots");
        assert!(delivery_slot_buttons(to()).validate().is_ok());
        assert!(delivery_slot_list(to()).validate().is_ok());
        assert!(autumn_carousel(to()).validate().is_ok());
    }

    #[test]
    fn limits_are_refused_locally() {
        assert_eq!(
            why_refused(to()).as_deref(),
            Some("interactive.action.buttons")
        );
        let long_title = ReplyButtons::new("x", [ReplyButton::new("a", "a".repeat(21))]);
        let field = OutboundMessage::new(to(), long_title)
            .validate()
            .unwrap_err()
            .field;
        assert_eq!(field, "interactive.action.buttons[0].reply.title");
    }

    #[tokio::test]
    async fn cta_location_request_and_flow_go_out() {
        let transport = ScriptedTransport::new();
        for _ in 0..3 {
            transport.push_json(200, json!({"messages": [{"id": "wamid.OUT"}]}));
        }
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .build()
            .unwrap();
        send_cta_location_flow(&client.messages("106540352242922"), to())
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
                json!("cta_url"),
                json!("location_request_message"),
                json!("flow")
            ]
        );
        let flow = transport.last_request().unwrap().json().unwrap();
        assert_eq!(
            flow["interactive"]["action"]["parameters"]["flow_name"],
            "fitting_v1"
        );
        assert_eq!(
            flow["interactive"]["action"]["parameters"]["flow_token"],
            "session-42"
        );
        assert_eq!(transport.remaining(), 0);
    }
}
