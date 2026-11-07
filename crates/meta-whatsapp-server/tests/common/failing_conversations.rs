//! A `ConversationStore` over the memory one whose thread owner writes
//! can be made to fail (roadmap M5l2, D393), every other call delegated.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use meta_whatsapp_rs::adapters::store::MemoryConversationStore;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_rs::core::store::{
    ConversationKey, ConversationStore, ConversationSummary, DeliveryStatus, Direction, Erased,
    IdentityLink, Purged, StoredContact, StoredMessage, ThreadOwnership, WindowEvent,
};
use time::OffsetDateTime;

type Result<T> = std::result::Result<T, StorageError>;

/// Memory, but `set_thread_owner` fails while [`Self::fail_owner_writes`]
/// is on.
#[derive(Debug, Default)]
pub struct FailingOwnerWrites {
    pub inner: MemoryConversationStore,
    failing: AtomicBool,
}

impl FailingOwnerWrites {
    pub fn fail_owner_writes(&self, on: bool) {
        self.failing.store(on, Ordering::SeqCst);
    }
}

#[async_trait]
impl ConversationStore for FailingOwnerWrites {
    async fn append(&self, m: StoredMessage) -> Result<bool> {
        self.inner.append(m).await
    }
    async fn append_synced(&self, messages: Vec<StoredMessage>) -> Result<Vec<bool>> {
        self.inner.append_synced(messages).await
    }
    async fn fill_media_placeholder(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        kind: String,
        text: Option<String>,
        payload: serde_json::Value,
    ) -> Result<bool> {
        self.inner
            .fill_media_placeholder(phone_number_id, id, kind, text, payload)
            .await
    }
    async fn revoke(
        &self,
        key: &ConversationKey,
        id: &MessageId,
        direction: Direction,
        at: OffsetDateTime,
    ) -> Result<bool> {
        self.inner.revoke(key, id, direction, at).await
    }
    async fn update_status(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<serde_json::Value>,
    ) -> Result<bool> {
        self.inner
            .update_status(phone_number_id, id, status, at, error)
            .await
    }
    async fn messages(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, MessageId)>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        self.inner.messages(key, before, limit).await
    }
    async fn conversations(
        &self,
        phone_number_id: &PhoneNumberId,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>> {
        self.inner
            .conversations(phone_number_id, before, limit)
            .await
    }
    async fn mark_read(&self, key: &ConversationKey) -> Result<()> {
        self.inner.mark_read(key).await
    }
    async fn last_inbound_at(&self, key: &ConversationKey) -> Result<Option<OffsetDateTime>> {
        self.inner.last_inbound_at(key).await
    }
    async fn message(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
    ) -> Result<Option<StoredMessage>> {
        self.inner.message(phone_number_id, id).await
    }
    async fn record_window_event(&self, event: WindowEvent) -> Result<bool> {
        self.inner.record_window_event(event).await
    }
    async fn window_events(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<WindowEvent>> {
        self.inner.window_events(key, before, limit).await
    }
    async fn set_thread_owner(
        &self,
        key: &ConversationKey,
        ownership: ThreadOwnership,
    ) -> Result<bool> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(StorageError::Backend(anyhow::anyhow!(
                "the store is down"
            )));
        }
        self.inner.set_thread_owner(key, ownership).await
    }
    async fn thread_owner(&self, key: &ConversationKey) -> Result<Option<ThreadOwnership>> {
        self.inner.thread_owner(key).await
    }
    async fn put_contact(&self, contact: StoredContact) -> Result<bool> {
        self.inner.put_contact(contact).await
    }
    async fn remove_contact(&self, key: &ConversationKey, at: OffsetDateTime) -> Result<bool> {
        self.inner.remove_contact(key, at).await
    }
    async fn contact(&self, key: &ConversationKey) -> Result<Option<StoredContact>> {
        self.inner.contact(key).await
    }
    async fn contacts(
        &self,
        phone_number_id: &PhoneNumberId,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<StoredContact>> {
        self.inner.contacts(phone_number_id, after, limit).await
    }
    async fn link_identity(&self, link: IdentityLink) -> Result<bool> {
        self.inner.link_identity(link).await
    }
    async fn identity_links(&self, key: &ConversationKey) -> Result<Vec<IdentityLink>> {
        self.inner.identity_links(key).await
    }
    async fn identities(&self, key: &ConversationKey) -> Result<BTreeSet<String>> {
        self.inner.identities(key).await
    }
    async fn erase_all(
        &self,
        phone_number_id: &PhoneNumberId,
        contacts: &[String],
    ) -> Result<Erased> {
        self.inner.erase_all(phone_number_id, contacts).await
    }
    async fn purge_before(
        &self,
        phone_number_id: Option<&PhoneNumberId>,
        cutoff: OffsetDateTime,
    ) -> Result<Purged> {
        self.inner.purge_before(phone_number_id, cutoff).await
    }
    async fn trim_conversations(
        &self,
        phone_number_id: Option<&PhoneNumberId>,
        keep_last: usize,
    ) -> Result<Purged> {
        self.inner.trim_conversations(phone_number_id, keep_last).await
    }
}
