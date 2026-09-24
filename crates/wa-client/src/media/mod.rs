//! Upload, retrieve URL, download (streaming), delete media; Resumable Upload API for template sample media (`header_handle`).
//!
//! Docs: `business-phone-numbers/media`, `reference/media/*`, `reference/whatsapp-business-phone-number/media-upload-api`, `templates/template-media`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::PhoneNumberId;

use crate::Client;

/// Entry point, see [`Client::media`].
#[derive(Debug, Clone)]
pub struct Media {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`Media`] API for `phone_number_id`.
    pub fn media(&self, phone_number_id: impl Into<PhoneNumberId>) -> Media {
        Media {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl Media {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
