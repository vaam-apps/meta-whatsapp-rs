//! Commerce settings (cart, catalog visibility) and catalog connection to a WABA. Product *messages* live in `messages`.
//!
//! Docs: `catalogs/*`, `reference/whatsapp-business-phone-number/commerce-settings-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::PhoneNumberId;

use crate::Client;

/// Entry point, see [`Client::commerce`].
#[derive(Debug, Clone)]
pub struct Commerce {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`Commerce`] API for `phone_number_id`.
    pub fn commerce(&self, phone_number_id: impl Into<PhoneNumberId>) -> Commerce {
        Commerce {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl Commerce {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
