//! Typed client for Meta's WhatsApp Business Platform.
//!
//! ```no_run
//! # async fn demo(transport: impl meta_whatsapp_core::transport::HttpTransport) -> meta_whatsapp_core::Result<()> {
//! use meta_whatsapp_client::Client;
//!
//! let client = Client::builder()
//!     .transport(transport)
//!     .access_token("EAAJB...")
//!     .build()?;
//! // client.messages("<PHONE_NUMBER_ID>").send(...).await?;
//! # Ok(()) }
//! ```
//!
//! Layout: [`Client`] owns the transport, endpoint, retry policy and token;
//! each endpoint family is a module with an accessor on [`Client`]; all of
//! them build [`GraphRequest`]s, which own auth, retries and error decoding.
//! [`common`] is the exception: no endpoints, only the types several
//! families share (each re-exports the ones it uses).

mod client;
mod request;
mod retry;

pub mod analytics;
pub mod authentication;
pub mod block_users;
pub mod business_profile;
pub mod calling;
pub mod commerce;
pub mod common;
pub mod credit_lines;
pub mod embedded_signup;
pub mod flows;
pub mod groups;
pub mod marketing;
pub mod media;
pub mod messages;
pub mod phone_numbers;
pub mod qr_codes;
pub mod signups;
pub mod templates;
pub mod waba;

pub use client::{Client, ClientBuilder, DEFAULT_TIMEOUT};
pub use request::GraphRequest;
pub use retry::RetryPolicy;
pub use meta_whatsapp_core::{Error, ErrorKind, GraphApiError, Result};

use meta_whatsapp_core::ids::AppId;
use meta_whatsapp_core::secret::AppSecret;

/// A Meta app's id and secret. Needed to exchange Embedded Signup codes, to
/// build app tokens (`debug_token`, subscriptions) and to verify webhooks.
#[derive(Debug, Clone)]
pub struct AppCredentials {
    /// App id.
    pub app_id: AppId,
    /// App secret.
    pub app_secret: AppSecret,
}

impl AppCredentials {
    /// Build credentials.
    pub fn new(app_id: impl Into<AppId>, app_secret: impl Into<AppSecret>) -> Self {
        Self {
            app_id: app_id.into(),
            app_secret: app_secret.into(),
        }
    }

    /// `app_id|app_secret` app access token.
    pub fn app_token(&self) -> meta_whatsapp_core::secret::AccessToken {
        meta_whatsapp_core::secret::AccessToken::app_token(&self.app_id, &self.app_secret)
    }
}
