//! Marketing Messages API for WhatsApp (MM API): send marketing templates through `/marketing_messages`, onboarding status, TTL, click tracking.
//!
//! Docs: `marketing-messages/*`, `reference/whatsapp-business-phone-number/marketing-messages-api-for-whatsapp`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::PhoneNumberId;

use crate::Client;

/// Entry point, see [`Client::marketing`].
#[derive(Debug, Clone)]
pub struct Marketing {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`Marketing`] API for `phone_number_id`.
    pub fn marketing(&self, phone_number_id: impl Into<PhoneNumberId>) -> Marketing {
        Marketing {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl Marketing {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
