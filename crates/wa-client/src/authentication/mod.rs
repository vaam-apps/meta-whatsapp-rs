//! Authentication templates (copy code, one-tap autofill, zero-tap), template previews, bulk upsert, and the OTP service (generate, store hashed, send, verify).
//!
//! Docs: `templates/authentication-templates/*`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::WabaId;

use crate::Client;

/// Entry point, see [`Client::authentication`].
#[derive(Debug, Clone)]
pub struct Authentication {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`Authentication`] API for `waba_id`.
    pub fn authentication(&self, waba_id: impl Into<WabaId>) -> Authentication {
        Authentication {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

impl Authentication {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
