//! QR codes and short links with prefilled messages.
//!
//! Docs: `qr-codes`, `reference/whatsapp-business-phone-number/whatsapp-business-qr-code-*`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::PhoneNumberId;

use crate::Client;

/// Entry point, see [`Client::qr_codes`].
#[derive(Debug, Clone)]
pub struct QrCodes {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`QrCodes`] API for `phone_number_id`.
    pub fn qr_codes(&self, phone_number_id: impl Into<PhoneNumberId>) -> QrCodes {
        QrCodes {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl QrCodes {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
