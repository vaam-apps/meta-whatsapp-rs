//! The types almost every integration names, for `use meta_whatsapp_rs::prelude::*;`.
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
pub use meta_whatsapp_client::messages::{MessageContent, OutboundMessage, SendResponse};
pub use meta_whatsapp_client::templates::{Parameter, TemplateMessage};
pub use meta_whatsapp_client::{AppCredentials, Client, ClientBuilder, RetryPolicy};
pub use meta_whatsapp_core::ids::{MessageId, PhoneNumberId, UserId, WabaId};
pub use meta_whatsapp_core::recipient::Recipient;
pub use meta_whatsapp_core::secret::{AccessToken, AppSecret, VerifyToken};
pub use meta_whatsapp_core::sink::EventSink;
pub use meta_whatsapp_core::store::{ConversationKey, ConversationStore, KvStore};
pub use meta_whatsapp_core::{Error, ErrorKind};
pub use meta_whatsapp_webhooks::{DedupGuard, SignatureVerifier, WebhookEvent, WebhookHandler};
