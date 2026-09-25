//! Reference code for the `meta-whatsapp-rs-send-messages` skill: free-form messages
//! (text, media, location, contacts, reactions), addressing, quoted
//! replies, callback data, read receipts and typing indicators.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use meta_whatsapp_rs::client::messages::{
    Contact, ContactPhone, Document, Image, Location, MediaSource, Messages, OutboundMessage,
};
use meta_whatsapp_rs::core::ids::MediaId;
use meta_whatsapp_rs::prelude::*;

/// Who to write to, from what a webhook gave you.
pub fn recipients() -> [Recipient; 4] {
    [
        Recipient::phone("+16505551234"), // E.164 WITH the `+`: `to`
        Recipient::user("US.13491208655302741918"), // BSUID from the webhook: `recipient`
        Recipient::PhoneAndUser {
            phone: "+16505551234".into(), // Meta uses the phone number
            user: "US.13491208655302741918".into(),
        },
        Recipient::group("Y2FwaV9ncm91cDox"), // Groups API
    ]
}

/// A webhook `wa_id` is digits only: add the `+` before sending to it.
pub fn from_wa_id(wa_id: &str) -> Recipient {
    Recipient::phone(format!("+{wa_id}"))
}

/// Text, as a quoted reply, tagged for the status webhooks.
pub async fn reply_text(
    client: &Client,
    phone_number_id: PhoneNumberId,
    to: Recipient,
    inbound: MessageId, // the customer's message you answer
) -> meta_whatsapp_rs::Result<Option<MessageId>> {
    let messages = client.messages(phone_number_id);
    let text = OutboundMessage::text(to, "Your order has shipped.")
        .reply_to(inbound) // shown as a quote
        .callback_data("order:860198"); // comes back as Status::biz_opaque_callback_data
    let sent = messages.send(&text).await?;
    Ok(sent.message_id().cloned()) // match status webhooks on this wamid
}

/// Media by id (uploaded: see `meta-whatsapp-rs-media`) or by public HTTPS link.
pub async fn send_media(
    messages: &Messages,
    to: Recipient,
    photo: MediaId,
) -> meta_whatsapp_rs::Result<SendResponse> {
    let image = Image::new(photo).caption("Your parcel, packed");
    messages
        .send(&OutboundMessage::new(to.clone(), image))
        .await?;
    let invoice = Document::new(MediaSource::link("https://shop.example/i/860198.pdf"))
        .filename("invoice-860198.pdf");
    messages.send(&OutboundMessage::new(to, invoice)).await
}

/// A pin, a contact card, a reaction.
pub async fn send_location_contact_reaction(
    messages: &Messages,
    to: Recipient,
    inbound: MessageId,
) -> meta_whatsapp_rs::Result<()> {
    let shop = Location::new(52.520008, 13.404954)
        .name("Example Boutique")
        .address("Musterstraße 1, 10115 Berlin");
    messages
        .send(&OutboundMessage::new(to.clone(), shop))
        .await?;
    let card =
        Contact::new("Example Boutique support").phone(ContactPhone::new("+4930123456", "WORK"));
    messages
        .send(&OutboundMessage::contacts(to.clone(), [card]))
        .await?;
    messages.react(to, inbound, "👍").await?; // "" removes the reaction
    Ok(())
}

/// Blue ticks for a received message; "typing…" only right before a reply.
pub async fn read_then_reply(
    messages: &Messages,
    to: Recipient,
    inbound: &MessageId,
) -> meta_whatsapp_rs::Result<()> {
    if messages
        .mark_read_with_typing_indicator(inbound)
        .await
        .is_err()
    {
        messages.mark_read(inbound).await?; // idempotent: the client may replay it
    }
    let answer = OutboundMessage::text(to, "Yes, it also comes in navy.");
    messages.send(&answer).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use serde_json::json;

    use super::*;

    fn client(transport: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn accepted() -> serde_json::Value {
        json!({"messaging_product": "whatsapp", "messages": [{"id": "wamid.OUT"}]})
    }

    #[tokio::test]
    async fn a_quoted_reply_to_a_bsuid_carries_context_and_callback_data() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, accepted());
        let id = reply_text(
            &client(&transport),
            "106540352242922".into(),
            Recipient::user("US.13491208655302741918"),
            "wamid.IN".into(),
        )
        .await
        .unwrap();
        assert_eq!(id.as_ref().map(MessageId::as_str), Some("wamid.OUT"));
        assert_eq!(
            transport.last_request().unwrap().json().unwrap(),
            json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "recipient": "US.13491208655302741918",
                "context": {"message_id": "wamid.IN"},
                "biz_opaque_callback_data": "order:860198",
                "type": "text",
                "text": {"body": "Your order has shipped."}
            })
        );
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn a_wa_id_gets_its_plus() {
        assert_eq!(from_wa_id("16505551234"), Recipient::phone("+16505551234"));
    }

    #[tokio::test]
    async fn media_location_contacts_and_reactions_are_sent() {
        let transport = ScriptedTransport::new();
        for _ in 0..5 {
            transport.push_json(200, accepted());
        }
        let messages = client(&transport).messages("106540352242922");
        let to = Recipient::phone("+16505551234");
        send_media(&messages, to.clone(), "1037543291543636".into())
            .await
            .unwrap();
        send_location_contact_reaction(&messages, to, "wamid.IN".into())
            .await
            .unwrap();
        let types: Vec<String> = transport
            .requests()
            .iter()
            .map(|r| r.json().unwrap()["type"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            types,
            ["image", "document", "location", "contacts", "reaction"]
        );
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn mark_read_falls_back_when_typing_fails() {
        let transport = ScriptedTransport::new();
        transport.push_json(500, json!({"error": {"message": "(#1) x", "code": 1}}));
        transport.push_json(200, json!({"success": true}));
        transport.push_json(200, accepted());
        let messages = client(&transport).messages("106540352242922");
        read_then_reply(
            &messages,
            Recipient::phone("+16505551234"),
            &"wamid.IN".into(),
        )
        .await
        .unwrap();
        let bodies: Vec<_> = transport
            .requests()
            .iter()
            .map(|r| r.json().unwrap())
            .collect();
        assert_eq!(bodies[0]["typing_indicator"], json!({"type": "text"}));
        assert_eq!(bodies[1]["status"], "read");
        assert!(bodies[1].get("typing_indicator").is_none());
        assert_eq!(transport.remaining(), 0);
    }
}
