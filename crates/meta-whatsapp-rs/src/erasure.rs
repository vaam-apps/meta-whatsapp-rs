//! Erase a person from every store the library keeps them in.
//!
//! A conversation store's erasure ([`ConversationStore::erase_all`], and
//! [`Inbox::erase_all`](crate::inbox::Inbox::erase_all) over it) deletes
//! what that store holds. The webhook dead letters
//! ([`DeadLetters`], roadmap L21a)
//! sit in a [`KvStore`](meta_whatsapp_core::store::KvStore), which has no
//! scan, so the conversation store cannot reach them: they keep their own
//! per-contact index and [`DeadLetters::erase`]. [`Eraser`] calls both, for
//! one person on one business number:
//!
//! ```
//! # #[cfg(not(feature = "memory"))]
//! # fn main() {}
//! # #[cfg(feature = "memory")]
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> meta_whatsapp_rs::Result<()> {
//! use std::sync::Arc;
//!
//! use meta_whatsapp_rs::adapters::store::{MemoryConversationStore, MemoryKvStore};
//! use meta_whatsapp_rs::core::ids::PhoneNumberId;
//! use meta_whatsapp_rs::erasure::Eraser;
//! use meta_whatsapp_rs::webhooks::DeadLetters;
//!
//! let kv = Arc::new(MemoryKvStore::new());
//! let eraser = Eraser::new(Arc::new(MemoryConversationStore::new()))
//!     // The same store the webhook handler dead-letters into.
//!     .with_dead_letters(DeadLetters::new(kv));
//! let number = PhoneNumberId::new("106540352242922")?;
//! // Every identity of the person on this number (`Inbox::identities`).
//! let ids = ["US.13491208655302741918".to_owned(), "16505551234".to_owned()];
//! let erased = eraser.erase_all(&number, &ids).await?;
//! assert!(erased.conversations.is_empty());
//! assert_eq!(erased.dead_letters, 0);
//! # Ok(()) }
//! ```
//!
//! Like [`ConversationStore::erase_all`], it trusts the number it is
//! given: call it behind your ownership check of the number. Everything
//! [`Inbox::erase_all`](crate::inbox::Inbox::erase_all)'s documentation
//! says it leaves (your own copies, logs, Meta's side) it leaves too.

use std::fmt;
use std::sync::Arc;

use meta_whatsapp_core::ids::PhoneNumberId;
use meta_whatsapp_core::store::{ConversationStore, Erased};
use meta_whatsapp_core::{Error, Result};
use meta_whatsapp_webhooks::DeadLetters;

/// Erases a person from the conversation store and, when given one, the
/// dead-letter store. See the [module docs](self).
#[derive(Clone)]
pub struct Eraser {
    conversations: Arc<dyn ConversationStore>,
    dead_letters: Option<DeadLetters>,
}

impl fmt::Debug for Eraser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Eraser")
            .field("dead_letters", &self.dead_letters.is_some())
            .finish_non_exhaustive()
    }
}

/// What [`Eraser::erase_all`] deleted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Erasure {
    /// What the conversation store deleted.
    pub conversations: Erased,
    /// Dead letters deleted (0 without a dead-letter store).
    pub dead_letters: u64,
}

impl Eraser {
    /// An eraser over `conversations`.
    pub fn new(conversations: Arc<dyn ConversationStore>) -> Self {
        Self {
            conversations,
            dead_letters: None,
        }
    }

    /// Also erase from `store`: the dead-letter store your webhook handler
    /// writes to (`WebhookHandlerBuilder::dead_letters`). Without it, the
    /// person's dead letters stay until they age out.
    #[must_use]
    pub fn with_dead_letters(mut self, store: DeadLetters) -> Self {
        self.dead_letters = Some(store);
        self
    }

    /// Erase one person on `phone_number_id`, known by `contacts` (their
    /// BSUIDs and phone numbers): [`ConversationStore::erase_all`], then
    /// [`DeadLetters::erase`] (which also reads the letters whose event
    /// named no business number).
    ///
    /// # Errors
    ///
    /// The failing store's error, in a step: `conversations` or
    /// `dead_letters` ([`Error::Step`]). Both erasures are idempotent:
    /// erase again to finish.
    pub async fn erase_all(
        &self,
        phone_number_id: &PhoneNumberId,
        contacts: &[String],
    ) -> Result<Erasure> {
        let conversations = self
            .conversations
            .erase_all(phone_number_id, contacts)
            .await
            .map_err(|e| Error::from(e).in_step("conversations"))?;
        let dead_letters = match &self.dead_letters {
            Some(store) => store
                .erase(phone_number_id, contacts)
                .await
                .map_err(|e| e.in_step("dead_letters"))?,
            None => 0,
        };
        Ok(Erasure {
            conversations,
            dead_letters,
        })
    }
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_adapters::store::{MemoryConversationStore, MemoryKvStore};
    use meta_whatsapp_core::error::SinkError;
    use meta_whatsapp_core::ids::MessageId;
    use meta_whatsapp_core::store::{ConversationKey, DeliveryStatus, Direction, StoredMessage};
    use meta_whatsapp_webhooks::{WebhookEvent, WebhookPayload};
    use serde_json::json;

    use super::*;

    const NUMBER: &str = "106540352242922";
    const BSUID: &str = "US.13491208655302741918";

    fn inbound(id: &str, from: &str, user_id: &str) -> WebhookEvent {
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398", "changes": [{
            "field": "messages",
            "value": {"messaging_product": "whatsapp",
                      "metadata": {"display_phone_number": "15550783881", "phone_number_id": NUMBER},
                      "contacts": [{"profile": {"name": "Sheena Nelson"}, "wa_id": from, "user_id": user_id}],
                      "messages": [{"from": from, "from_user_id": user_id, "id": id,
                                    "timestamp": "1749416383", "type": "text",
                                    "text": {"body": "Does it come in another color?"}}]}
        }]}]});
        WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events()
            .remove(0)
    }

    /// The decisive test of L21a's erasure: the facade's erasure of a
    /// contact removes their dead letters beside their conversation, and
    /// leaves someone else's. Without the dead-letter store's `erase` in
    /// it, the letters stay and this fails.
    #[tokio::test]
    async fn erasing_a_contact_removes_their_dead_letters() {
        let conversations = Arc::new(MemoryConversationStore::new());
        let dead_letters = DeadLetters::new(Arc::new(MemoryKvStore::new()));
        let number = PhoneNumberId::new(NUMBER).unwrap();
        let key = ConversationKey::new(number.clone(), BSUID);
        conversations
            .append(StoredMessage {
                id: MessageId::new("wamid.Z"),
                conversation: key.clone(),
                direction: Direction::Inbound,
                kind: "text".to_owned(),
                text: Some("hello".to_owned()),
                payload: json!({"text": {"body": "hello"}}),
                status: DeliveryStatus::Received,
                timestamp: time::OffsetDateTime::UNIX_EPOCH,
                status_at: None,
                error: None,
            })
            .await
            .unwrap();
        let refused = SinkError::permanent(std::io::Error::other("refused for good"));
        let theirs = dead_letters
            .record(&inbound("wamid.A", "16505551234", BSUID), &refused)
            .await
            .unwrap();
        let other = dead_letters
            .record(
                &inbound("wamid.B", "4915112345678", "US.99999999999999999999"),
                &refused,
            )
            .await
            .unwrap();

        let facade = Eraser::new(conversations.clone()).with_dead_letters(dead_letters.clone());
        let erased = facade
            .erase_all(&number, &[BSUID.to_owned(), "16505551234".to_owned()])
            .await
            .unwrap();
        assert_eq!(erased.dead_letters, 1);
        assert_eq!(erased.conversations.messages, 1);
        assert_eq!(dead_letters.get(theirs).await.unwrap(), None);
        assert!(dead_letters.get(other).await.unwrap().is_some());
        assert!(
            conversations
                .messages(&key, None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Without a dead-letter store it is the conversation store's erasure.
    #[tokio::test]
    async fn without_a_dead_letter_store_it_erases_conversations_only() {
        let conversations = Arc::new(MemoryConversationStore::new());
        let number = PhoneNumberId::new(NUMBER).unwrap();
        let erased = Eraser::new(conversations)
            .erase_all(&number, &[BSUID.to_owned()])
            .await
            .unwrap();
        assert_eq!(erased, Erasure::default());
    }
}
