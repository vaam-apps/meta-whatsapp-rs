//! Send messages (text, media, location, contacts, interactive, reactions,
//! templates, products, group pins), mark as read, typing indicators,
//! Direct Send `category`.
//!
//! Docs: `messages/send-messages`, `messages/*` (text, image, audio, video,
//! document, sticker, location, location-request, contacts, address,
//! reaction, interactive-reply-buttons, interactive-list,
//! interactive-cta-url, interactive-media-carousel, mark-message-as-read,
//! contextual-replies), `typing-indicators`, `direct-send/*`,
//! `catalogs/*-messages`, `flows/guides/sendingaflow`,
//! `calling/call-button-messages-deep-links`,
//! `calling/user-call-permissions`, `groups/groups-messaging`,
//! `business-scoped-user-ids`,
//! `reference/whatsapp-business-phone-number/message-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).
//!
//! # Sending
//!
//! Build an [`OutboundMessage`] and pass it to [`Messages::send`]. Every
//! documented hard limit (lengths, counts, formats) is checked locally
//! first, so a bad message fails with a [`ValidationError`] naming the JSON
//! path instead of a round trip. Sends are never replayed after a timeout
//! (the message may already be on its way); only throttling errors, which
//! prove Meta did not process the request, are retried.
//!
//! Outside the 24-hour customer service window only templates go through;
//! anything else fails with [`ErrorKind::CustomerServiceWindowClosed`]
//! (`131047`).
//!
//! [`ValidationError`]: wa_core::error::ValidationError
//! [`ErrorKind::CustomerServiceWindowClosed`]: wa_core::ErrorKind::CustomerServiceWindowClosed
//!
//! ```no_run
//! # async fn demo(client: wa_client::Client) -> wa_core::Result<()> {
//! use wa_client::messages::OutboundMessage;
//! use wa_client::templates::TemplateMessage;
//! use wa_core::recipient::Recipient;
//!
//! let messages = client.messages("106540352242922");
//!
//! // Text, as a reply to the user's message. Address users by BSUID when
//! // that is all a webhook gave you.
//! let to = Recipient::user("US.13491208655302741918");
//! let sent = messages
//!     .send(&OutboundMessage::text(to.clone(), "Your order has shipped.").reply_to("wamid.HBgL..."))
//!     .await?;
//! let wamid = sent.message_id(); // key for the status webhooks
//!
//! // A template (works outside the service window).
//! messages
//!     .send(&OutboundMessage::template(
//!         Recipient::phone("+16505551234"),
//!         TemplateMessage::new("order_confirmation", "en_US"),
//!     ))
//!     .await?;
//! # let _ = wamid; Ok(()) }
//! ```
//!
//! Reply buttons and lists:
//!
//! ```no_run
//! # async fn demo(client: wa_client::Client) -> wa_core::Result<()> {
//! use wa_client::messages::{Header, OutboundMessage, ReplyButton, ReplyButtons};
//! use wa_core::recipient::Recipient;
//!
//! let buttons = ReplyButtons::new(
//!     "Your workshop is at 9am tomorrow.",
//!     [ReplyButton::new("change", "Change"), ReplyButton::new("cancel", "Cancel")],
//! )
//! .header(Header::image_id("2762702990552401"))
//! .footer("Lucky Shrub");
//! client
//!     .messages("106540352242922")
//!     .send(&OutboundMessage::new(Recipient::phone("+16505551234"), buttons))
//!     .await?;
//! # Ok(()) }
//! ```
//!
//! A Flow, opened on its first screen with prefilled data:
//!
//! ```no_run
//! # async fn demo(client: wa_client::Client) -> wa_core::Result<()> {
//! use wa_client::messages::{FlowParameters, FlowRef, OutboundMessage};
//! use wa_core::recipient::Recipient;
//!
//! let params = FlowParameters::new(FlowRef::Name("appointment_booking_v1".into()), "Book!")
//!     .token("session-42")
//!     .navigate("WELCOME", Some(serde_json::json!({"customer": "Pablo"})));
//! client
//!     .messages("106540352242922")
//!     .send(&OutboundMessage::flow(Recipient::phone("+16505551234"), "Book a slot", params))
//!     .await?;
//! # Ok(()) }
//! ```
//!
//! A multi-product message from a catalog:
//!
//! ```no_run
//! # async fn demo(client: wa_client::Client) -> wa_core::Result<()> {
//! use wa_client::messages::{OutboundMessage, ProductSection};
//! use wa_core::recipient::Recipient;
//!
//! let msg = OutboundMessage::product_list(
//!     Recipient::phone("+16505551234"),
//!     "Our bestsellers",
//!     "Tap a product to see details.",
//!     "1234567890",
//!     [
//!         ProductSection::new("Succulents", ["SKU-ALOE", "SKU-ECHEVERIA"]),
//!         ProductSection::new("Pots", ["SKU-POT-S"]),
//!     ],
//! );
//! client.messages("106540352242922").send(&msg).await?;
//! # Ok(()) }
//! ```
//!
//! Sending an uploaded image and downloading received media are shown in
//! [`crate::media`].
//!
//! # Read receipts and typing indicators
//!
//! [`Messages::mark_read`] shows the blue ticks (and marks earlier
//! messages read too); [`Messages::mark_read_with_typing_indicator`] also
//! shows "typing…" until you reply or 25 seconds pass.

mod commerce;
mod contacts;
mod content;
mod interactive;
mod outbound;
mod response;
pub(crate) mod validate;

#[cfg(test)]
mod tests;

pub use commerce::{
    CatalogMessage, ProductCard, ProductCarousel, ProductList, ProductSection, SingleProduct,
};
pub use contacts::{
    Contact, ContactAddress, ContactEmail, ContactName, ContactOrg, ContactPhone, ContactUrl,
};
pub use content::{
    Audio, Document, Image, Location, MediaSource, Pin, Reaction, Sticker, Text, Video,
};
pub use interactive::{
    AddressMessage, AddressParameters, CallPermissionRequest, CardAction, CardHeader, CtaUrl,
    FlowAction, FlowActionPayload, FlowMessage, FlowMode, FlowParameters, FlowRef, Header,
    Interactive, ListMessage, ListRow, ListSection, LocationRequest, MediaCard, MediaCarousel,
    QuickReply, ReplyButton, ReplyButtons, RequestContactInfo, SavedAddress, VoiceCall,
    VoiceCallParameters,
};
pub use outbound::{
    Context, DirectSendCategory, DirectSendConfig, MessageContent, OutboundMessage,
};
pub use response::{MessageStatus, SendResponse, SentContact, SentMessage};

use serde::Serialize;
use wa_core::Result;
use wa_core::ids::{MessageId, PhoneNumberId};
use wa_core::recipient::Recipient;

use crate::{Client, GraphRequest};

/// Entry point, see [`Client::messages`].
#[derive(Debug, Clone)]
pub struct Messages {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`Messages`] API for `phone_number_id`.
    pub fn messages(&self, phone_number_id: impl Into<PhoneNumberId>) -> Messages {
        Messages {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

/// Body of a mark-as-read request (`messages/mark-message-as-read`,
/// `typing-indicators`).
#[derive(Serialize)]
struct MarkRead<'a> {
    messaging_product: &'static str,
    status: &'static str,
    message_id: &'a MessageId,
    #[serde(skip_serializing_if = "Option::is_none")]
    typing_indicator: Option<TypingIndicator>,
}

#[derive(Serialize)]
struct TypingIndicator {
    /// `text`, the only documented value.
    #[serde(rename = "type")]
    kind: &'static str,
}

impl Messages {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `POST /{phone_number_id}/messages`. The id is one path segment taken
    /// verbatim (`/`, `?`, `#` percent-encoded; empty, `.` and `..`
    /// rejected), so a crafted id cannot point the business token at a
    /// different Graph object.
    fn post_messages(&self) -> GraphRequest {
        self.client
            .post_at(&[self.phone_number_id.as_str(), "messages"])
    }

    /// Send `message` (`POST /{phone_number_id}/messages`).
    ///
    /// Validates it first ([`OutboundMessage::validate`]). Not idempotent:
    /// a timeout is surfaced, never replayed, because the message may have
    /// been sent.
    pub async fn send(&self, message: &OutboundMessage) -> Result<SendResponse> {
        message.validate()?;
        self.post_messages()
            .json(message)
            .context("send message response")
            .send()
            .await
    }

    /// Mark `message_id` (a received message, at most 30 days old) and every
    /// earlier message in the conversation as read.
    ///
    /// Marked idempotent: replaying it sets the same state again.
    pub async fn mark_read(&self, message_id: &MessageId) -> Result<()> {
        self.mark(message_id, None).await
    }

    /// [`mark_read`](Self::mark_read) and show a typing indicator until you
    /// reply or 25 seconds pass. Only use it when you are about to reply.
    ///
    /// Not replayed on transient errors (throttling excepted, which proves
    /// Meta did nothing). Replaying would not duplicate anything, but an
    /// indicator is only worth showing promptly: a retry that lands after
    /// backoff — typically when this call runs concurrently with composing
    /// the reply — can put "typing…" on screen *after* the reply arrived.
    /// On error, fall back to [`mark_read`](Self::mark_read), which is
    /// replayed.
    pub async fn mark_read_with_typing_indicator(&self, message_id: &MessageId) -> Result<()> {
        self.mark(message_id, Some(TypingIndicator { kind: "text" }))
            .await
    }

    async fn mark(&self, message_id: &MessageId, typing: Option<TypingIndicator>) -> Result<()> {
        validate::non_empty("message_id", message_id.as_str())?;
        let idempotent = typing.is_none();
        self.post_messages()
            .json(&MarkRead {
                messaging_product: "whatsapp",
                status: "read",
                message_id,
                typing_indicator: typing,
            })
            .idempotent(idempotent)
            .context("mark message as read response")
            .send_success()
            .await
    }

    /// React to `message_id` with `emoji`; an empty `emoji` removes the
    /// reaction. Shorthand for sending [`OutboundMessage::reaction`].
    pub async fn react(
        &self,
        recipient: impl Into<Recipient>,
        message_id: impl Into<MessageId>,
        emoji: impl Into<String>,
    ) -> Result<SendResponse> {
        self.send(&OutboundMessage::reaction(recipient, message_id, emoji))
            .await
    }
}
