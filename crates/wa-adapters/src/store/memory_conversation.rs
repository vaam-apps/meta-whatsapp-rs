//! In-process [`ConversationStore`]. Honours the full contract (dedup on id,
//! the [`DeliveryStatus::supersedes`] rule, `(timestamp, id)` ordering with
//! exclusive cursors, unread counting) within one process; history is lost
//! on restart. Use it for tests, development and demos.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use wa_core::error::StorageError;
use wa_core::ids::{MessageId, PhoneNumberId};
use wa_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, DeliveryStatus, Direction,
    StoredMessage,
};

/// Inbox row state. `last_message_id` breaks timestamp ties the same way the
/// history order does, so "latest message" is well defined when two
/// messages share a timestamp.
#[derive(Debug, Clone)]
struct Summary {
    last_message_at: OffsetDateTime,
    last_message_id: MessageId,
    last_text: Option<String>,
    last_inbound_at: Option<OffsetDateTime>,
    unread: u64,
}

#[derive(Debug, Default)]
struct State {
    messages: HashMap<MessageId, StoredMessage>,
    /// Per-conversation index in `(timestamp, id)` order; paging walks it
    /// backwards from the exclusive cursor.
    history: HashMap<ConversationKey, BTreeSet<(OffsetDateTime, MessageId)>>,
    conversations: HashMap<ConversationKey, Summary>,
}

/// In-memory conversation history.
///
/// Unread counts are by **arrival**: an inbound message appended after the
/// last [`ConversationStore::mark_read`] counts even if its timestamp is
/// older (a late webhook the merchant has not seen yet). The Postgres
/// adapter counts the same way.
#[derive(Clone, Default)]
pub struct MemoryConversationStore {
    state: Arc<Mutex<State>>,
}

impl fmt::Debug for MemoryConversationStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Counts only: a derived Debug would print every stored message
        // (customer text, payloads) into whatever log formats the store.
        let mut d = f.debug_struct("MemoryConversationStore");
        match self.state.try_lock() {
            Ok(st) => d
                .field("messages", &st.messages.len())
                .field("conversations", &st.conversations.len()),
            Err(_) => d.field("state", &format_args!("<locked>")),
        };
        d.finish()
    }
}

impl MemoryConversationStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ConversationStore for MemoryConversationStore {
    async fn append(&self, message: StoredMessage) -> Result<bool, StorageError> {
        let mut st = self.state.lock().await;
        if st.messages.contains_key(&message.id) {
            return Ok(false);
        }
        let key = message.conversation.clone();
        let at = message.timestamp;
        let inbound = message.direction == Direction::Inbound;
        st.history
            .entry(key.clone())
            .or_default()
            .insert((at, message.id.clone()));
        match st.conversations.get_mut(&key) {
            Some(s) => {
                if (at, &message.id) > (s.last_message_at, &s.last_message_id) {
                    s.last_message_at = at;
                    s.last_message_id = message.id.clone();
                    s.last_text.clone_from(&message.text);
                }
                if inbound {
                    s.last_inbound_at = Some(s.last_inbound_at.map_or(at, |t| t.max(at)));
                    s.unread += 1;
                }
            }
            None => {
                st.conversations.insert(
                    key,
                    Summary {
                        last_message_at: at,
                        last_message_id: message.id.clone(),
                        last_text: message.text.clone(),
                        last_inbound_at: inbound.then_some(at),
                        unread: u64::from(inbound),
                    },
                );
            }
        }
        st.messages.insert(message.id.clone(), message);
        Ok(true)
    }

    async fn update_status(
        &self,
        id: &MessageId,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<serde_json::Value>,
    ) -> Result<bool, StorageError> {
        let mut st = self.state.lock().await;
        let Some(message) = st.messages.get_mut(id) else {
            return Ok(false);
        };
        if !status.supersedes(message.status) {
            return Ok(false);
        }
        message.status = status;
        message.status_at = Some(at);
        // A status without an error object (a late `read` cannot supersede a
        // `failed`, but a `sent` after `accepted` can) keeps whatever error
        // was recorded; only a status that brings one replaces it.
        if error.is_some() {
            message.error = error;
        }
        Ok(true)
    }

    async fn messages(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, MessageId)>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        let st = self.state.lock().await;
        let Some(index) = st.history.get(key) else {
            return Ok(Vec::new());
        };
        let rows: Box<dyn Iterator<Item = &(OffsetDateTime, MessageId)>> = match before {
            Some(cursor) => Box::new(index.range(..cursor).rev()),
            None => Box::new(index.iter().rev()),
        };
        Ok(rows
            .take(limit)
            .filter_map(|(_, id)| st.messages.get(id).cloned())
            .collect())
    }

    async fn conversations(
        &self,
        phone_number_id: &PhoneNumberId,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>, StorageError> {
        let st = self.state.lock().await;
        let mut rows: Vec<(&ConversationKey, &Summary)> = st
            .conversations
            .iter()
            .filter(|(k, _)| &k.phone_number_id == phone_number_id)
            .filter(|(k, s)| {
                before.as_ref().is_none_or(|(at, contact)| {
                    (s.last_message_at, k.contact.as_str()) < (*at, contact.as_str())
                })
            })
            .collect();
        rows.sort_unstable_by(|(ka, sa), (kb, sb)| {
            (sb.last_message_at, &kb.contact).cmp(&(sa.last_message_at, &ka.contact))
        });
        Ok(rows
            .into_iter()
            .take(limit)
            .map(|(k, s)| ConversationSummary {
                key: k.clone(),
                last_message_at: s.last_message_at,
                last_inbound_at: s.last_inbound_at,
                last_text: s.last_text.clone(),
                unread: s.unread,
            })
            .collect())
    }

    async fn mark_read(&self, key: &ConversationKey) -> Result<(), StorageError> {
        let mut st = self.state.lock().await;
        if let Some(s) = st.conversations.get_mut(key) {
            s.unread = 0;
        }
        Ok(())
    }

    async fn last_inbound_at(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<OffsetDateTime>, StorageError> {
        let st = self.state.lock().await;
        Ok(st.conversations.get(key).and_then(|s| s.last_inbound_at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::conversation_conformance;

    #[tokio::test]
    async fn passes_conversation_conformance_suite() {
        conversation_conformance::run(&MemoryConversationStore::new()).await;
    }

    #[tokio::test]
    async fn debug_shows_counts_not_messages() {
        let store = MemoryConversationStore::new();
        store
            .append(StoredMessage {
                id: MessageId::new("wamid.1"),
                conversation: ConversationKey::new("pn", "US.1"),
                direction: Direction::Inbound,
                kind: "text".to_owned(),
                text: Some("my card number is 4111".to_owned()),
                payload: serde_json::json!({"text": {"body": "my card number is 4111"}}),
                status: DeliveryStatus::Received,
                timestamp: time::macros::datetime!(2026-09-24 12:00 UTC),
                status_at: None,
                error: None,
            })
            .await
            .unwrap();
        assert_eq!(
            format!("{store:?}"),
            "MemoryConversationStore { messages: 1, conversations: 1 }"
        );
    }
}
