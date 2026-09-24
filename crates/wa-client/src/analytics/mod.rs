//! WABA analytics: messaging, pricing, template and call analytics.
//!
//! Docs: `analytics`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::WabaId;

use crate::Client;

/// Entry point, see [`Client::analytics`].
#[derive(Debug, Clone)]
pub struct Analytics {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`Analytics`] API for `waba_id`.
    pub fn analytics(&self, waba_id: impl Into<WabaId>) -> Analytics {
        Analytics {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

impl Analytics {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
