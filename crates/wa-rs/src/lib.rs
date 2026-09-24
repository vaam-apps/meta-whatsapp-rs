//! WhatsApp Business Platform for Rust: one dependency for the typed Graph
//! API client, webhooks, storage/transport/sink adapters and Typst-rendered
//! documents.
//!
//! This crate is a facade. Each workspace crate is re-exported under a short
//! name, the types almost every integration touches are in [`prelude`], and
//! [`inbox`] (the CMS chat glue) lives here because it needs all of them.
//!
//! | Module | Crate | What |
//! | --- | --- | --- |
//! | [`mod@client`] | `wa-client` | Graph API client, one module per endpoint family |
//! | [`webhooks`] | `wa-webhooks` | signature and verify-token checks, typed events, dedup, axum router + SSE |
//! | [`adapters`] | `wa-adapters` | reqwest transport; memory, Postgres, Redis stores; sinks |
//! | [`core`] | `wa-core` | error tree, ids, secrets, the ports (`HttpTransport`, `KvStore`, `ConversationStore`, `EventSink`, `Clock`) |
//! | `typst` (feature `typst`) | `wa-typst` | invoices, receipts, vouchers → PDF/PNG |
//! | [`inbox`] | here | webhook events → conversation history; window-checked replies with a merchant's token |
//!
//! # Which module for which product
//!
//! | You are building | Start with | Runnable example |
//! | --- | --- | --- |
//! | **E-commerce marketing**: templates, product messages, order documents | [`client::templates`], [`client::messages`], [`client::marketing`], [`client::signups`], [`client::commerce`], `typst` | `send_message`, `invoice_document` |
//! | **CMS in-app chat**: each merchant connects their own number and chats with their customers | [`client::embedded_signup`], [`webhooks`], [`inbox`] | `embedded_signup`, `cms_inbox` |
//! | **WhatsApp OTP login** | [`client::authentication`] ([`OtpService`](client::authentication::OtpService)) | `otp_login` |
//!
//! The examples live in `crates/wa-rs/examples/`; each file's header lists
//! the environment variables it reads and the command that runs it. The
//! snippets below are condensed from them (the README quotes them verbatim).
//!
//! ## Send a message
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use wa_rs::prelude::*;
//!
//! let client = wa_rs::client(std::env::var("WA_TOKEN")?)?;
//! let messages = client.messages(std::env::var("WA_PHONE_NUMBER_ID")?);
//! let to = Recipient::phone(std::env::var("WA_TO")?); // E.164, with `+`
//!
//! // Free-form: only inside the 24-hour customer service window.
//! messages.send(&OutboundMessage::text(to.clone(), "Your order has shipped.")).await?;
//! // A template: any time.
//! messages.send(&OutboundMessage::template(to, TemplateMessage::new("hello_world", "en_US"))).await?;
//! # Ok(()) }
//! ```
//!
//! ## CMS inbox
//!
//! Merchants connect their number with Embedded Signup (the business token
//! lands, encrypted, in a [`TokenVault`](client::embedded_signup::TokenVault));
//! customer messages arrive by webhook and are recorded per conversation and
//! streamed live; replies go out with that merchant's token.
//!
//! ```no_run
//! # #[cfg(all(feature = "memory", feature = "sinks", feature = "axum"))]
//! # fn demo(
//! #     app_secret: wa_rs::core::secret::AppSecret,
//! #     verify_token: wa_rs::core::secret::VerifyToken,
//! #     kv: std::sync::Arc<dyn wa_rs::core::store::KvStore>,
//! #     conversations: std::sync::Arc<dyn wa_rs::core::store::ConversationStore>,
//! # ) -> wa_rs::Result<wa_rs::webhooks::axum::Router> {
//! use std::sync::Arc;
//! use wa_rs::adapters::sink::{BroadcastSink, FanoutSink};
//! use wa_rs::prelude::*;
//!
//! let (live, _) = tokio::sync::broadcast::channel::<WebhookEvent>(256);
//! let sink = FanoutSink::new()
//!     .with(InboxSink::new(conversations.clone())) // history, per conversation
//!     .with(BroadcastSink::from_sender(live.clone())); // live, for SSE
//! let handler = WebhookHandler::builder(
//!     SignatureVerifier::new(vec![app_secret])?,
//!     verify_token,
//!     Arc::new(sink),
//! )
//! .dedup(DedupGuard::new(kv))
//! .build();
//! // The axum the router is built with, re-exported: no axum dependency of your own.
//! let router = wa_rs::webhooks::axum::Router::new().nest("/webhook", wa_rs::webhooks::router(Arc::new(handler)));
//! # let _ = live; Ok(router) }
//! ```
//!
//! Replying, with the merchant's token looked up by phone number:
//!
//! ```no_run
//! # async fn demo(
//! #     client: wa_rs::Client,
//! #     vault: wa_rs::client::embedded_signup::TokenVault,
//! #     conversations: std::sync::Arc<dyn wa_rs::core::store::ConversationStore>,
//! #     number: wa_rs::core::ids::PhoneNumberId,
//! # ) -> wa_rs::Result<()> {
//! use wa_rs::client::messages::Text;
//! use wa_rs::prelude::*;
//!
//! let Some(merchant) = vault.get_by_phone_number(&number).await? else {
//!     return Ok(()); // nobody connected this number
//! };
//! let inbox = Inbox::new(client.with_token(merchant.token), number, conversations);
//! let key = inbox.key("US.13491208655302741918"); // BSUID, or wa_id when there is none
//! inbox.reply(&key, Text::new("Yes, it also comes in navy.").into()).await?;
//! # Ok(()) }
//! ```
//!
//! ## OTP login
//!
//! ```no_run
//! # #[cfg(feature = "memory")]
//! # async fn demo(client: wa_rs::Client) -> anyhow::Result<()> {
//! use std::sync::Arc;
//! use wa_rs::adapters::store::MemoryKvStore;
//! use wa_rs::client::authentication::{
//!     IssueOutcome, OtpConfig, OtpPepper, OtpService, OtpTemplate, VerifyOutcome,
//! };
//! use wa_rs::core::clock::SystemClock;
//! use wa_rs::prelude::*;
//!
//! let otp = OtpService::new(
//!     client,
//!     std::env::var("WA_PHONE_NUMBER_ID")?,
//!     OtpTemplate::new("login_code", "en_US"), // an approved authentication template
//!     Arc::new(MemoryKvStore::new()),
//!     Arc::new(SystemClock),
//!     OtpPepper::new(std::env::var("WA_OTP_PEPPER")?)?, // >= 32 bytes, kept out of the store
//!     OtpConfig::default(),
//! )?;
//! let user = Recipient::phone("+16505551234"); // strict E.164, with `+`
//! if let IssueOutcome::Sent(challenge) = otp.issue(&user, "login").await? {
//!     println!("code sent, valid until {}", challenge.expires_at);
//! }
//! if otp.verify(&user, "login", "123456").await? == VerifyOutcome::Verified {
//!     println!("signed in");
//! }
//! # Ok(()) }
//! ```
//!
//! # Feature flags
//!
//! | Feature | Default | Adds |
//! | --- | --- | --- |
// `client` is both a module (the `wa-client` re-export) and a function (the
// shortcut, feature `reqwest`): link the function with `fn@`, and only when
// it exists, so `cargo doc --no-default-features` has no broken link.
#![cfg_attr(
    feature = "reqwest",
    doc = "| `reqwest` | yes | `adapters::http::ReqwestTransport` (rustls, HTTP/2) and the [`client()`](fn@client) / [`client_builder()`](fn@client_builder) shortcuts |"
)]
#![cfg_attr(
    not(feature = "reqwest"),
    doc = "| `reqwest` | yes | `adapters::http::ReqwestTransport` (rustls, HTTP/2) and the `client()` / `client_builder()` shortcuts |"
)]
//! | `memory` | yes | `adapters::store::{MemoryKvStore, MemoryConversationStore}`: tests, development, one instance |
//! | `sinks` | yes | `adapters::sink`: channel, broadcast, fan-out, filter, fn and tracing sinks |
//! | `postgres` | | `adapters::store::{PostgresKvStore, PostgresConversationStore}` and `adapters::store::postgres::migrate` (sqlx) |
//! | `redis` | | `adapters::store::RedisKvStore` |
//! | `axum` | | `webhooks::router` (`GET`/`POST` webhook endpoint) and `webhooks::sse` (live inbox stream) |
//! | `typst` | | the `typst` module: invoice, receipt and voucher templates → PDF/PNG |
//! | `flows-endpoint` | | `client::flows::endpoint`: WhatsApp Flows data-endpoint crypto (aws-lc-rs) |
//! | `testing` | | `core::testing::ScriptedTransport` for your own tests (enable in `[dev-dependencies]`) |
//! | `full` | | all of the above |
//!
//! What is implemented, and what is not, is in `docs/coverage.md`; the
//! design is `docs/architecture.md`.

pub mod inbox;
pub mod prelude;

pub use wa_adapters as adapters;
pub use wa_client as client;
pub use wa_core as core;
#[cfg(feature = "typst")]
pub use wa_typst as typst;
pub use wa_webhooks as webhooks;

pub use wa_client::{AppCredentials, Client, ClientBuilder, RetryPolicy};
pub use wa_core::{Error, ErrorKind, GraphApiError, Result};

/// A [`ClientBuilder`] with the production transport already set:
/// `adapters::http::ReqwestTransport` (rustls, HTTP/2, proxies from
/// `HTTPS_PROXY`/`NO_PROXY`). Everything else is the builder's default —
/// Graph API `v25.0`, the default [`RetryPolicy`], a 30 s timeout — and can
/// still be changed before `build()`.
///
/// Use it for a client without a default token, the multi-tenant case where
/// each call runs as a merchant with [`Client::with_token`]:
///
/// ```no_run
/// # fn demo() -> wa_rs::Result<()> {
/// let client = wa_rs::client_builder()?.build()?;
/// # let _ = client; Ok(()) }
/// ```
///
/// Each call creates a new connection pool: build one client at startup and
/// clone it (cloning is cheap and shares the pool). Fails only when the TLS
/// backend cannot be initialised.
#[cfg(feature = "reqwest")]
pub fn client_builder() -> Result<ClientBuilder> {
    let transport = wa_adapters::http::ReqwestTransport::new()?;
    Ok(Client::builder().transport(transport))
}

/// A production client authenticating with `token` (a system user or
/// business token): [`client_builder()`] plus the token.
///
/// ```no_run
/// # fn demo() -> anyhow::Result<()> {
/// let client = wa_rs::client(std::env::var("WA_TOKEN")?)?;
/// # let _ = client; Ok(()) }
/// ```
#[cfg(feature = "reqwest")]
pub fn client(token: impl Into<wa_core::secret::AccessToken>) -> Result<Client> {
    client_builder()?.access_token(token).build()
}

#[cfg(all(test, feature = "reqwest"))]
mod tests {
    use wa_core::config::ApiVersion;

    #[test]
    fn client_is_production_graph_with_the_token() {
        let client = super::client("EAAB-test-token").unwrap();
        assert_eq!(
            client
                .token()
                .map(wa_core::secret::AccessToken::expose_secret),
            Some("EAAB-test-token")
        );
        assert_eq!(
            client.endpoint().base().as_str(),
            "https://graph.facebook.com/"
        );
        assert_eq!(client.endpoint().version(), ApiVersion::DEFAULT);
    }

    #[test]
    fn client_builder_has_no_default_token() {
        let client = super::client_builder().unwrap().build().unwrap();
        assert!(client.token().is_none());
    }
}
