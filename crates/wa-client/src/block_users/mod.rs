//! Block, unblock and list blocked users.
//!
//! Docs: `block-users`, `reference/whatsapp-business-phone-number/block-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::PhoneNumberId;

use crate::Client;

/// Entry point, see [`Client::block_users`].
#[derive(Debug, Clone)]
pub struct BlockUsers {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`BlockUsers`] API for `phone_number_id`.
    pub fn block_users(&self, phone_number_id: impl Into<PhoneNumberId>) -> BlockUsers {
        BlockUsers {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl BlockUsers {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
