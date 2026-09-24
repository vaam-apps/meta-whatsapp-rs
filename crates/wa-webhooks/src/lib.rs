//! WhatsApp webhooks: verify, parse, normalize, dedup, dispatch.
//!
//! ```text
//! POST body ─► size limit (3 MiB default)
//!           ─► X-Hub-Signature-256: HMAC-SHA256 of the raw bytes, any of N app secrets
//!           ─► WebhookPayload { object, entry[{ id, time?, changes[{ field, value }] }] }
//!           ─► Vec<WebhookEvent>, one per message / status / error / field change
//!           ─► per event: DedupGuard claim (optional; 60 s lease in a KvStore)
//!           ─► EventSink<WebhookEvent>
//!           ─► DedupGuard complete (marker kept 7 days + 1 h)
//! ```
//!
//! | Module | What |
//! | --- | --- |
//! | [`verify`] | `GET` subscription check (`hub.mode`, `hub.verify_token`, `hub.challenge`) |
//! | [`signature`] | `X-Hub-Signature-256` verification and signing |
//! | [`payload`] | the envelope and per-field dispatch ([`ChangeValue`]) |
//! | [`fields`] | typed values for every documented field |
//! | [`event`] | [`WebhookEvent`] and [`events`] |
//! | [`dedup`] | [`DedupGuard`]: claim, complete, release; replay notes |
//! | [`handler`] | [`WebhookHandler`], the framework-independent endpoint |
//! | `server` | axum `router` and `sse` helpers (feature `axum`) |
//!
//! Forward compatibility is the design constraint: Meta adds fields, message
//! types and enum values without notice, and an endpoint that fails on them
//! makes Meta retry for 7 days and then drop the delivery. So unknown fields
//! and message types parse into `Unknown { … raw }`, enums are open, and a
//! known value that does not match its documented shape is kept untyped with
//! the reason instead of failing the batch. Only a body that is not a
//! webhook envelope at all becomes [`WebhookEvent::Unparsed`], and even that
//! is acknowledged.
//!
//! Nothing derived from a body's content is logged (sizes, digests, field
//! names and redacted error text only): bodies carry personal data.
//!
//! Identity follows the business-scoped user id (BSUID) rules: `user_id` is
//! present in every messages webhook since April 2026, `wa_id` (the phone
//! number) may be absent. Events carry the matched [`fields::Contact`].
//!
//! Doc paths (relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`)
//! are named in each module.
//!
//! # Example
//!
//! An axum server that verifies deliveries and hands events to an inbox
//! (`--features axum`):
//!
//! ```no_run
//! # #[cfg(feature = "axum")]
//! # mod demo {
//! use std::sync::Arc;
//!
//! use async_trait::async_trait;
//! use wa_core::error::SinkError;
//! use wa_core::secret::{AppSecret, VerifyToken};
//! use wa_core::sink::EventSink;
//! use wa_webhooks::{SignatureVerifier, WebhookEvent, WebhookHandler, router};
//!
//! #[derive(Debug)]
//! struct Inbox;
//!
//! #[async_trait]
//! impl EventSink<WebhookEvent> for Inbox {
//!     async fn deliver(&self, event: WebhookEvent) -> Result<(), SinkError> {
//!         if let WebhookEvent::MessageReceived { contact, message, .. } = &event {
//!             // Key the conversation by BSUID; the phone number may be absent.
//!             let who = contact.as_ref().and_then(|c| c.user_id.clone());
//!             println!("{who:?} sent {:?}", message.message_type());
//!         }
//!         Ok(())
//!     }
//! }
//!
//! pub async fn serve() -> Result<(), Box<dyn std::error::Error>> {
//!     let handler = WebhookHandler::builder(
//!         // Several secrets while rotating the app secret.
//!         SignatureVerifier::new(vec![AppSecret::new(std::env::var("META_APP_SECRET")?)])?,
//!         VerifyToken::new(std::env::var("META_VERIFY_TOKEN")?),
//!         Arc::new(Inbox),
//!     )
//!     // In production, also: .dedup(DedupGuard::new(kv_store))
//!     .build();
//!
//!     let app = axum::Router::new().nest("/webhooks/whatsapp", router(Arc::new(handler)));
//!     let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
//!     axum::serve(listener, app).await?;
//!     Ok(())
//! }
//! # }
//! ```

pub mod dedup;
pub mod event;
pub mod fields;
pub mod handler;
pub(crate) mod open_enum;
pub mod payload;
pub(crate) mod redact;
pub(crate) mod serde_ext;
#[cfg(feature = "axum")]
pub mod server;
pub mod signature;
pub mod verify;

/// The `axum` the router is built against; mount [`router`] with this
/// re-export rather than pinning axum yourself.
#[cfg(feature = "axum")]
pub use axum;
pub use dedup::{
    Claim, ClaimTicket, DEDUP_NAMESPACE, DEFAULT_CLAIM_LEASE, DEFAULT_DEDUP_TTL, DedupGuard,
};
pub use event::{WebhookEvent, events};
pub use handler::{DEFAULT_MAX_BODY_BYTES, DeliveryReport, WebhookHandler, WebhookHandlerBuilder};
pub use payload::{Change, ChangeValue, Entry, WebhookPayload};
#[cfg(feature = "axum")]
pub use server::{router, sse};
pub use signature::{SIGNATURE_HEADER, SignatureVerifier, sign};
pub use verify::{VerificationQuery, verify_subscription};
