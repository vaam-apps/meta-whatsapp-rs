//! Calling API signalling: call settings, call permissions, business-initiated calls (connect, pre-accept, accept, reject, terminate).
//!
//! Docs: `calling/*`, `reference/whatsapp-business-phone-number/calling-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::PhoneNumberId;

use crate::Client;

/// Entry point, see [`Client::calling`].
#[derive(Debug, Clone)]
pub struct Calling {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`Calling`] API for `phone_number_id`.
    pub fn calling(&self, phone_number_id: impl Into<PhoneNumberId>) -> Calling {
        Calling {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl Calling {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
