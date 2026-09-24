//! The types almost every integration names, for `use wa_rs::prelude::*;`.
//!
//! Deliberately short: the client and its errors, ids and addressing,
//! secrets, the message and template builders you send most, the webhook
//! pieces, the storage and sink ports (traits, so their methods are in
//! scope on concrete adapters), and the inbox. Everything else is one path
//! away under [`crate::client`](mod@crate::client), [`crate::webhooks`],
//! [`crate::adapters`] and [`crate::core`].
//!
//! `Result` is not included, so a glob import never shadows the standard
//! library's; use [`crate::Result`] explicitly.

pub use crate::inbox::{Inbox, InboxSink};
pub use wa_client::messages::{MessageContent, OutboundMessage, SendResponse};
pub use wa_client::templates::{Parameter, TemplateMessage};
pub use wa_client::{AppCredentials, Client, ClientBuilder, RetryPolicy};
pub use wa_core::ids::{MessageId, PhoneNumberId, UserId, WabaId};
pub use wa_core::recipient::Recipient;
pub use wa_core::secret::{AccessToken, AppSecret, VerifyToken};
pub use wa_core::sink::EventSink;
pub use wa_core::store::{ConversationKey, ConversationStore, KvStore};
pub use wa_core::{Error, ErrorKind};
pub use wa_webhooks::{DedupGuard, SignatureVerifier, WebhookEvent, WebhookHandler};
