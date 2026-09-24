//! WhatsApp Business Account: details, phone numbers, webhook subscriptions (`subscribed_apps`, override callback), assigned users, client/owned WABAs, activities.
//!
//! Docs: `whatsapp-business-accounts`, `reference/whatsapp-business-account/*`, `reference/business/*`, `solution-providers/manage-webhooks`, `webhooks/override`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::WabaId;

use crate::Client;

/// Entry point, see [`Client::waba`].
#[derive(Debug, Clone)]
pub struct Waba {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`Waba`] API for `waba_id`.
    pub fn waba(&self, waba_id: impl Into<WabaId>) -> Waba {
        Waba {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

impl Waba {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
