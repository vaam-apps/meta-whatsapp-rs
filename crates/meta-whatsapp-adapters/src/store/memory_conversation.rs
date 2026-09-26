//! In-process [`ConversationStore`]. Honours the full contract (dedup on id,
//! the [`DeliveryStatus::supersedes`] rule, `(timestamp, id)` ordering with
//! exclusive cursors, unread counting, synced history that opens no window,
//! media placeholders filled once and never after a revoke, tombstones kept
//! out of the summary, window events, thread ownership and synced contacts,
//! erasure and purge by age, each in one step under the store's lock)
//! within one process; everything is lost on restart. Use it for tests,
//! development and demos.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::ops::Bound;
use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, DeliveryStatus, Direction, Erased,
    Purged, Retention, StoredContact, StoredMessage, ThreadOwnership, WindowEvent,
};
use time::OffsetDateTime;
use tokio::sync::Mutex;

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
    /// Window events by `(business number, id)`, their identity.
    window_events: HashMap<(PhoneNumberId, String), WindowEvent>,
    /// Per-conversation index of the window events in `(at, id)` order.
    window_index: HashMap<ConversationKey, BTreeSet<(OffsetDateTime, String)>>,
    owners: HashMap<ConversationKey, ThreadOwnership>,
    /// By `(business number, contact)`: listing walks one number's range.
    contacts: BTreeMap<(PhoneNumberId, String), Synced>,
}

/// What the address book keeps under a key: the contact, or its removal
/// (when, and nothing else), kept so that an older sync arriving after it
/// is refused.
#[derive(Debug, Clone)]
enum Synced {
    Contact(StoredContact),
    Removed(OffsetDateTime),
}

impl Synced {
    fn at(&self) -> OffsetDateTime {
        match self {
            Self::Contact(c) => c.synced_at,
            Self::Removed(at) => *at,
        }
    }

    fn contact(&self) -> Option<&StoredContact> {
        match self {
            Self::Contact(c) => Some(c),
            Self::Removed(_) => None,
        }
    }
}

/// In-memory conversation history.
///
/// Unread counts are by **arrival**: an inbound message appended after the
/// last [`ConversationStore::mark_read`] counts even if its timestamp is
/// older (a late webhook the merchant has not seen yet); synced history
/// ([`ConversationStore::append_synced`]) never counts. The Postgres
/// adapter counts the same way.
#[derive(Clone, Default)]
pub struct MemoryConversationStore {
    state: Arc<Mutex<State>>,
    retention: Retention,
}

impl fmt::Debug for MemoryConversationStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Counts only: a derived Debug would print every stored message
        // (customer text, payloads) into whatever log formats the store.
        let mut d = f.debug_struct("MemoryConversationStore");
        match self.state.try_lock() {
            Ok(st) => d
                .field("messages", &st.messages.len())
                .field("conversations", &st.conversations.len())
                .field("window_events", &st.window_events.len())
                .field("thread_owners", &st.owners.len())
                .field("contacts", &st.contacts.len()),
            Err(_) => d.field("state", &format_args!("<locked>")),
        };
        d.field("retention", &self.retention).finish()
    }
}

impl MemoryConversationStore {
    /// Empty store, keeping everything ([`Retention::Keep`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// The same store (clones share their state) applying `retention` in
    /// [`ConversationStore::apply_retention`].
    #[must_use]
    pub fn with_retention(mut self, retention: Retention) -> Self {
        self.retention = retention;
        self
    }

    /// Insert `message` unless its id is known. `live` messages (not synced
    /// history) move the window and count as unread when inbound.
    async fn insert(&self, message: StoredMessage, live: bool) -> Result<bool, StorageError> {
        let mut st = self.state.lock().await;
        Ok(st.insert(message, live))
    }
}

impl State {
    /// Store `message` in the history unless its id is known, without
    /// touching the summary. Returns whether it was stored.
    fn insert_row(&mut self, message: StoredMessage) -> bool {
        if self.messages.contains_key(&message.id) {
            return false;
        }
        self.history
            .entry(message.conversation.clone())
            .or_default()
            .insert((message.timestamp, message.id.clone()));
        self.messages.insert(message.id.clone(), message);
        true
    }

    /// Delete message `id` and its history index entry. Returns whether it
    /// was stored.
    fn remove_message(&mut self, id: &MessageId) -> bool {
        let Some(message) = self.messages.remove(id) else {
            return false;
        };
        if let Some(index) = self.history.get_mut(&message.conversation) {
            index.remove(&(message.timestamp, message.id));
            if index.is_empty() {
                self.history.remove(&message.conversation);
            }
        }
        true
    }

    /// Delete the window event of `number` and `id`, and its index entry.
    fn remove_window_event(&mut self, number: &PhoneNumberId, id: &str) -> bool {
        let Some(event) = self.window_events.remove(&(number.clone(), id.to_owned())) else {
            return false;
        };
        if let Some(index) = self.window_index.get_mut(&event.conversation) {
            index.remove(&(event.at, event.id));
            if index.is_empty() {
                self.window_index.remove(&event.conversation);
            }
        }
        true
    }

    /// See [`MemoryConversationStore::insert`].
    fn insert(&mut self, message: StoredMessage, live: bool) -> bool {
        let st = self;
        let key = message.conversation.clone();
        let at = message.timestamp;
        let id = message.id.clone();
        let text = message.text.clone();
        let opens_window = live && message.direction == Direction::Inbound;
        if !st.insert_row(message) {
            return false;
        }
        match st.conversations.get_mut(&key) {
            Some(s) => {
                if (at, &id) > (s.last_message_at, &s.last_message_id) {
                    s.last_message_at = at;
                    s.last_message_id = id;
                    s.last_text = text;
                }
                if opens_window {
                    s.last_inbound_at = Some(s.last_inbound_at.map_or(at, |t| t.max(at)));
                    s.unread += 1;
                }
            }
            None => {
                st.conversations.insert(
                    key,
                    Summary {
                        last_message_at: at,
                        last_message_id: id,
                        last_text: text,
                        last_inbound_at: opens_window.then_some(at),
                        unread: u64::from(opens_window),
                    },
                );
            }
        }
        true
    }
}

#[async_trait]
impl ConversationStore for MemoryConversationStore {
    async fn append(&self, message: StoredMessage) -> Result<bool, StorageError> {
        self.insert(message, true).await
    }

    async fn append_synced(&self, messages: Vec<StoredMessage>) -> Result<Vec<bool>, StorageError> {
        let mut st = self.state.lock().await;
        Ok(messages.into_iter().map(|m| st.insert(m, false)).collect())
    }

    async fn revoke(
        &self,
        key: &ConversationKey,
        id: &MessageId,
        direction: Direction,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError> {
        let mut st = self.state.lock().await;
        if let Some(message) = st.messages.get_mut(id) {
            if message.conversation.phone_number_id != key.phone_number_id
                || message.direction != direction
                || !DeliveryStatus::Deleted.supersedes(message.status)
            {
                return Ok(false);
            }
            message.status = DeliveryStatus::Deleted;
            message.status_at = Some(at);
            return Ok(true);
        }
        // History only: a tombstone is never part of the summary.
        Ok(st.insert_row(StoredMessage::tombstone(key, id, direction, at)))
    }

    async fn fill_media_placeholder(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        kind: String,
        text: Option<String>,
        payload: serde_json::Value,
    ) -> Result<bool, StorageError> {
        let mut guard = self.state.lock().await;
        let st = &mut *guard;
        // Never a revoked one: the content is what its sender deleted.
        let Some(message) = st.messages.get_mut(id).filter(|m| {
            &m.conversation.phone_number_id == phone_number_id
                && m.kind == StoredMessage::MEDIA_PLACEHOLDER
                && m.status != DeliveryStatus::Deleted
        }) else {
            return Ok(false);
        };
        message.kind = kind;
        message.text = text;
        message.payload = payload;
        if let Some(s) = st.conversations.get_mut(&message.conversation)
            && &s.last_message_id == id
        {
            s.last_text.clone_from(&message.text);
        }
        Ok(true)
    }

    async fn update_status(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<serde_json::Value>,
    ) -> Result<bool, StorageError> {
        let mut st = self.state.lock().await;
        let Some(message) = st
            .messages
            .get_mut(id)
            .filter(|m| &m.conversation.phone_number_id == phone_number_id)
        else {
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

    async fn message(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
    ) -> Result<Option<StoredMessage>, StorageError> {
        let st = self.state.lock().await;
        Ok(st
            .messages
            .get(id)
            .filter(|m| &m.conversation.phone_number_id == phone_number_id)
            .cloned())
    }

    async fn record_window_event(&self, event: WindowEvent) -> Result<bool, StorageError> {
        let mut st = self.state.lock().await;
        let identity = (event.conversation.phone_number_id.clone(), event.id.clone());
        if st.window_events.contains_key(&identity) {
            return Ok(false);
        }
        st.window_index
            .entry(event.conversation.clone())
            .or_default()
            .insert((event.at, event.id.clone()));
        st.window_events.insert(identity, event);
        Ok(true)
    }

    async fn window_events(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<WindowEvent>, StorageError> {
        let st = self.state.lock().await;
        let Some(index) = st.window_index.get(key) else {
            return Ok(Vec::new());
        };
        let rows: Box<dyn Iterator<Item = &(OffsetDateTime, String)>> = match before {
            Some(cursor) => Box::new(index.range(..cursor).rev()),
            None => Box::new(index.iter().rev()),
        };
        Ok(rows
            .take(limit)
            .filter_map(|(_, id)| {
                st.window_events
                    .get(&(key.phone_number_id.clone(), id.clone()))
                    .cloned()
            })
            .collect())
    }

    async fn set_thread_owner(
        &self,
        key: &ConversationKey,
        ownership: ThreadOwnership,
    ) -> Result<bool, StorageError> {
        let mut st = self.state.lock().await;
        if st
            .owners
            .get(key)
            .is_some_and(|stored| ownership.since < stored.since)
        {
            return Ok(false);
        }
        st.owners.insert(key.clone(), ownership);
        Ok(true)
    }

    async fn thread_owner(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<ThreadOwnership>, StorageError> {
        Ok(self.state.lock().await.owners.get(key).cloned())
    }

    async fn put_contact(&self, contact: StoredContact) -> Result<bool, StorageError> {
        let mut st = self.state.lock().await;
        let key = (
            contact.key.phone_number_id.clone(),
            contact.key.contact.clone(),
        );
        if st
            .contacts
            .get(&key)
            .is_some_and(|stored| contact.synced_at < stored.at())
        {
            return Ok(false);
        }
        st.contacts.insert(key, Synced::Contact(contact));
        Ok(true)
    }

    async fn remove_contact(
        &self,
        key: &ConversationKey,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError> {
        let mut st = self.state.lock().await;
        let key = (key.phone_number_id.clone(), key.contact.clone());
        let removed = match st.contacts.get(&key) {
            Some(stored) if stored.at() > at => return Ok(false),
            Some(Synced::Contact(_)) => true,
            Some(Synced::Removed(_)) | None => false,
        };
        st.contacts.insert(key, Synced::Removed(at));
        Ok(removed)
    }

    async fn contact(&self, key: &ConversationKey) -> Result<Option<StoredContact>, StorageError> {
        let st = self.state.lock().await;
        Ok(st
            .contacts
            .get(&(key.phone_number_id.clone(), key.contact.clone()))
            .and_then(Synced::contact)
            .cloned())
    }

    async fn contacts(
        &self,
        phone_number_id: &PhoneNumberId,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<StoredContact>, StorageError> {
        let st = self.state.lock().await;
        let start = match after {
            Some(contact) => Bound::Excluded((phone_number_id.clone(), contact)),
            None => Bound::Included((phone_number_id.clone(), String::new())),
        };
        Ok(st
            .contacts
            .range((start, Bound::Unbounded))
            .take_while(|((number, _), _)| number == phone_number_id)
            .filter_map(|(_, synced)| synced.contact())
            .take(limit)
            .cloned()
            .collect())
    }

    async fn erase(&self, key: &ConversationKey) -> Result<Erased, StorageError> {
        let mut guard = self.state.lock().await;
        let st = &mut *guard;
        let mut erased = Erased::default();
        // Every row stored under the key, tombstones included, is in its
        // history index.
        for (_, id) in st.history.remove(key).unwrap_or_default() {
            if st.messages.remove(&id).is_some() {
                erased.messages += 1;
            }
        }
        erased.conversations = u64::from(st.conversations.remove(key).is_some());
        for (_, id) in st.window_index.remove(key).unwrap_or_default() {
            if st
                .window_events
                .remove(&(key.phone_number_id.clone(), id))
                .is_some()
            {
                erased.window_events += 1;
            }
        }
        erased.thread_owners = u64::from(st.owners.remove(key).is_some());
        let before = st.contacts.len();
        st.contacts.retain(|(number, contact), synced| {
            number != &key.phone_number_id
                || !(contact == &key.contact
                    || synced.contact().is_some_and(|stored| {
                        [&stored.user_id, &stored.parent_user_id]
                            .into_iter()
                            .flatten()
                            .any(|id| id.as_str() == key.contact)
                            || stored.phone_number.as_deref() == Some(key.contact.as_str())
                    }))
        });
        erased.contacts = (before - st.contacts.len()) as u64;
        Ok(erased)
    }

    async fn purge_before(
        &self,
        phone_number_id: Option<&PhoneNumberId>,
        cutoff: OffsetDateTime,
    ) -> Result<Purged, StorageError> {
        let mut guard = self.state.lock().await;
        let st = &mut *guard;
        let in_scope =
            |key: &ConversationKey| phone_number_id.is_none_or(|n| n == &key.phone_number_id);
        let mut purged = Purged::default();
        let old: Vec<MessageId> = st
            .messages
            .values()
            .filter(|m| in_scope(&m.conversation) && m.timestamp < cutoff)
            .map(|m| m.id.clone())
            .collect();
        for id in old {
            if st.remove_message(&id) {
                purged.messages += 1;
            }
        }
        // Its latest message is purged exactly when the summary's time is
        // before the cutoff: the summary holds that message's preview.
        let before = st.conversations.len();
        st.conversations
            .retain(|key, s| !(in_scope(key) && s.last_message_at < cutoff));
        purged.conversations = (before - st.conversations.len()) as u64;
        let old: Vec<(PhoneNumberId, String)> = st
            .window_events
            .iter()
            .filter(|(_, e)| in_scope(&e.conversation) && e.at < cutoff)
            .map(|(identity, _)| identity.clone())
            .collect();
        for (number, id) in old {
            if st.remove_window_event(&number, &id) {
                purged.window_events += 1;
            }
        }
        let before = st.owners.len();
        st.owners
            .retain(|key, o| !(in_scope(key) && o.since < cutoff));
        purged.thread_owners = (before - st.owners.len()) as u64;
        let before = st.contacts.len();
        st.contacts.retain(|(number, _), synced| {
            !(phone_number_id.is_none_or(|n| n == number)
                && matches!(synced, Synced::Removed(at) if *at < cutoff))
        });
        purged.contact_removals = (before - st.contacts.len()) as u64;
        Ok(purged)
    }

    fn retention(&self) -> Retention {
        self.retention
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

    /// The suite's retention case takes the other branch on a store that
    /// keeps a retention.
    #[tokio::test]
    async fn passes_conversation_conformance_suite_with_a_retention() {
        conversation_conformance::run(
            &MemoryConversationStore::new().with_retention(Retention::days(30)),
        )
        .await;
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
            "MemoryConversationStore { messages: 1, conversations: 1, window_events: 0, \
             thread_owners: 0, contacts: 0, retention: Keep }"
        );
    }

    /// The retention set on the store is what `apply_retention` applies:
    /// kept by default, purged past the cutoff when set, in every clone.
    #[tokio::test]
    async fn apply_retention_applies_the_configured_retention() {
        let now = time::macros::datetime!(2026-09-24 12:00 UTC);
        let message = |id: &str, at: OffsetDateTime| StoredMessage {
            id: MessageId::new(id),
            conversation: ConversationKey::new("pn", "US.1"),
            direction: Direction::Inbound,
            kind: "text".to_owned(),
            text: Some(id.to_owned()),
            payload: serde_json::json!({}),
            status: DeliveryStatus::Received,
            timestamp: at,
            status_at: None,
            error: None,
        };
        let keep = MemoryConversationStore::new();
        let day = time::Duration::days(1);
        keep.append(message("old", now - 3 * day)).await.unwrap();
        keep.append(message("new", now - day)).await.unwrap();
        assert_eq!(keep.retention(), Retention::Keep);
        assert!(keep.apply_retention(now).await.unwrap().is_empty());
        assert_eq!(
            keep.messages(&ConversationKey::new("pn", "US.1"), None, 10)
                .await
                .unwrap()
                .len(),
            2
        );

        let two_days = keep.clone().with_retention(Retention::days(2));
        assert_eq!(two_days.retention(), Retention::days(2));
        assert_eq!(
            two_days.apply_retention(now).await.unwrap(),
            Purged {
                messages: 1,
                ..Purged::default()
            }
        );
        let left = keep
            .messages(&ConversationKey::new("pn", "US.1"), None, 10)
            .await
            .unwrap();
        assert_eq!(
            left.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["new"],
            "clones share the state"
        );
    }
}
