//! Business profile of a phone number: about, address, description, email, vertical, websites, profile picture.
//!
//! Docs: `business-profiles`, `reference/whatsapp-business-phone-number/whatsapp-business-profile-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::PhoneNumberId;

use crate::Client;

/// Entry point, see [`Client::business_profile`].
#[derive(Debug, Clone)]
pub struct BusinessProfile {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`BusinessProfile`] API for `phone_number_id`.
    pub fn business_profile(&self, phone_number_id: impl Into<PhoneNumberId>) -> BusinessProfile {
        BusinessProfile {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl BusinessProfile {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
