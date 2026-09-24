//! Template management (create, list, get, edit, delete, library, migrate, compare) and typed component builders for creating and for sending templates.
//!
//! Docs: `templates/*`, `reference/whatsapp-business-account/message-template-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::WabaId;

use crate::Client;

/// Entry point, see [`Client::templates`].
#[derive(Debug, Clone)]
pub struct Templates {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`Templates`] API for `waba_id`.
    pub fn templates(&self, waba_id: impl Into<WabaId>) -> Templates {
        Templates {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

impl Templates {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
