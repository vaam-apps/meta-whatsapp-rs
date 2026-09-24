//! In-App Signup API: opt-in deep links (`wa.me/<phone>/signup/<id>`) for marketing subscriptions.
//!
//! Docs: `in-app-signup`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use wa_core::ids::WabaId;

use crate::Client;

/// Entry point, see [`Client::signups`].
#[derive(Debug, Clone)]
pub struct Signups {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`Signups`] API for `waba_id`.
    pub fn signups(&self, waba_id: impl Into<WabaId>) -> Signups {
        Signups {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

impl Signups {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
