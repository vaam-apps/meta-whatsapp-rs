//! Send messages (text, media, location, contacts, interactive, reactions, templates, products), mark as read, typing indicators, Direct Send `category`.
//!
//! Docs: `messages/send-messages`, `messages/*`, `typing-indicators`, `link-previews`, `messages/contextual-replies`, `messages/mark-message-as-read`, `direct-send/*`, `catalogs/*-messages`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::PhoneNumberId;

use crate::Client;

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

impl Messages {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
