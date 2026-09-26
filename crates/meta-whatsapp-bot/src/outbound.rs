//! [`Outbound`]: how the bot sends. Every reply, refusal and read receipt
//! goes through it, so a test or a multi-tenant integration swaps it.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_client::Client;
use meta_whatsapp_client::messages::{OutboundMessage, SendResponse};
use meta_whatsapp_core::Result;
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};

/// Sends messages and read receipts for the bot.
///
/// The default, [`ClientOutbound`], calls the Cloud API with one
/// [`Client`]. Implement it yourself to pick a merchant's token per business
/// number (look it up in the token vault by `from`), to record replies in
/// the CMS inbox (`docs/guides/bots.md`), to queue sends, or to record them
/// in a test. One shared by several bots: `BotBuilder::shared_outbound`.
#[async_trait]
pub trait Outbound: Send + Sync + fmt::Debug + 'static {
    /// Send `message` from the business phone number `from`.
    async fn send(&self, from: &PhoneNumberId, message: &OutboundMessage) -> Result<SendResponse>;

    /// Mark `message_id` read on `from`, showing a typing indicator too when
    /// `typing_indicator` is set.
    async fn mark_read(
        &self,
        from: &PhoneNumberId,
        message_id: &MessageId,
        typing_indicator: bool,
    ) -> Result<()>;
}

#[async_trait]
impl<T: Outbound + ?Sized> Outbound for Arc<T> {
    async fn send(&self, from: &PhoneNumberId, message: &OutboundMessage) -> Result<SendResponse> {
        (**self).send(from, message).await
    }

    async fn mark_read(
        &self,
        from: &PhoneNumberId,
        message_id: &MessageId,
        typing_indicator: bool,
    ) -> Result<()> {
        (**self).mark_read(from, message_id, typing_indicator).await
    }
}

/// [`Outbound`] over the Cloud API: `client.messages(from).send(message)`,
/// `mark_read` and `mark_read_with_typing_indicator`. Every check and retry
/// rule of those calls applies (a timed-out send is never replayed).
#[derive(Debug, Clone)]
pub struct ClientOutbound {
    client: Client,
}

impl ClientOutbound {
    /// Send with `client` (and its token).
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// The client.
    pub fn client(&self) -> &Client {
        &self.client
    }
}

#[async_trait]
impl Outbound for ClientOutbound {
    async fn send(&self, from: &PhoneNumberId, message: &OutboundMessage) -> Result<SendResponse> {
        self.client.messages(from.clone()).send(message).await
    }

    async fn mark_read(
        &self,
        from: &PhoneNumberId,
        message_id: &MessageId,
        typing_indicator: bool,
    ) -> Result<()> {
        let messages = self.client.messages(from.clone());
        if typing_indicator {
            messages.mark_read_with_typing_indicator(message_id).await
        } else {
            messages.mark_read(message_id).await
        }
    }
}
