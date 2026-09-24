//! One business phone number: details, request/verify code, register/deregister, two-step verification PIN, settings, display name, conversational components (welcome message, commands, prompts).
//!
//! Docs: `business-phone-numbers/*`, `display-names`, `reference/whatsapp-business-phone-number/*`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::PhoneNumberId;

use crate::Client;

/// Entry point, see [`Client::phone_number`].
#[derive(Debug, Clone)]
pub struct PhoneNumber {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`PhoneNumber`] API for `phone_number_id`.
    pub fn phone_number(&self, phone_number_id: impl Into<PhoneNumberId>) -> PhoneNumber {
        PhoneNumber {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl PhoneNumber {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
