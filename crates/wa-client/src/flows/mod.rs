//! WhatsApp Flows: create, update JSON, publish, deprecate, delete, metrics; business public key; Flow data endpoint request decryption / response encryption.
//!
//! Docs: `flows/*`, `reference/whatsapp-business-phone-number/business-encryption-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::WabaId;

use crate::Client;

/// Entry point, see [`Client::flows`].
#[derive(Debug, Clone)]
pub struct Flows {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`Flows`] API for `waba_id`.
    pub fn flows(&self, waba_id: impl Into<WabaId>) -> Flows {
        Flows {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

impl Flows {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
