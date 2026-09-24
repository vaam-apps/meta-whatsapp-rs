//! Embedded Signup: onboard a business (a merchant of your platform) onto
//! the WhatsApp Business Platform from your own site, and end up holding a
//! business integration system user token scoped to their WABA.
//!
//! Docs: `embedded-signup/*` (v4 is current), `access-tokens`,
//! `solution-providers/*`, `embedded-signup/onboarding-business-app-users`
//! (coexistence).
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use crate::{AppCredentials, Client};

/// Entry point, see [`Client::embedded_signup`].
#[derive(Debug, Clone)]
pub struct EmbeddedSignup {
    client: Client,
    app: AppCredentials,
}

impl Client {
    /// Embedded Signup operations for the app identified by `app`.
    pub fn embedded_signup(&self, app: AppCredentials) -> EmbeddedSignup {
        EmbeddedSignup {
            client: self.clone(),
            app,
        }
    }
}

impl EmbeddedSignup {
    /// The app these operations act for.
    pub fn app(&self) -> &AppCredentials {
        &self.app
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
