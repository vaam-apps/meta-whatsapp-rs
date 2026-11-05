//! The names [`MongoKvStore`](super::MongoKvStore) and
//! [`MongoConversationStore`](super::MongoConversationStore) store under:
//! their collections, their fields and their indexes (roadmap S17). Once a
//! database holds records under them, renaming one strands that data, so
//! they are stable identifiers (docs/architecture.md, § Stable
//! identifiers), **pending the owner's confirmation before the first
//! release**. Each collection is named after the Postgres adapter's table,
//! and each field after its column where the column holds the same value
//! (`payload_json` and `error_json` hold JSON text there and here; `kind`,
//! `text`, `last_text` and a contact's names are BSON strings here, which
//! hold U+0000, where Postgres keeps their UTF-8 bytes in `*_utf8`).
//!
//! - [`KV`]: one document per key, `_id` the key ([`KV_NAMESPACE`],
//!   [`KV_KEY`]); a deleted key's document stays, without a value, until
//!   the TTL index [`INDEX_KV_PURGE`] removes it.
//! - [`MESSAGES`]: one document per message (tombstones included), `_id`
//!   its message id: ids are stored once per store (`OPEN_QUESTIONS.md`
//!   #33).
//! - [`CONVERSATIONS`]: one document per conversation summary.
//! - [`WINDOW_EVENTS`], [`THREAD_OWNERS`], [`SYNCED_CONTACTS`] and
//!   [`IDENTITY_LINKS`]: the records beside the history.
//!
//! The documents of the collections other than [`KV`] and [`MESSAGES`]
//! have a server-generated `ObjectId` as `_id`, which says nothing of
//! anyone; their key is a unique index.

/// The key/value records.
pub const KV: &str = "wa_kv";
/// The messages, tombstones included.
pub const MESSAGES: &str = "wa_messages";
/// The conversation summaries (the inbox rows).
pub const CONVERSATIONS: &str = "wa_conversations";
/// The window events.
pub const WINDOW_EVENTS: &str = "wa_window_events";
/// The thread ownership records.
pub const THREAD_OWNERS: &str = "wa_thread_owners";
/// The synced address book contacts, and the removals kept of them.
pub const SYNCED_CONTACTS: &str = "wa_synced_contacts";
/// The links between two identities of one person.
pub const IDENTITY_LINKS: &str = "wa_identity_links";

/// A key/value record's namespace, in its `_id`.
pub const KV_NAMESPACE: &str = "namespace";
/// A key/value record's key, in its `_id`.
pub const KV_KEY: &str = "key";
/// Its value, a BSON binary; absent once deleted (the document is then a
/// tombstone that keeps the version).
pub const KV_VALUE: &str = "value";
/// Its version, a 64-bit integer.
pub const KV_VERSION: &str = "version";
/// When it stops being visible (the server's clock); absent: never.
pub const KV_EXPIRES_AT: &str = "expires_at";
/// The claim of the `put_if_absent` that last wrote the record, an
/// `ObjectId` unique to the call: how the call learns its write took effect.
/// The next `put`, swap or delete removes it.
pub const KV_CLAIM: &str = "claim";
/// When the TTL index may remove the document: its expiry, or its
/// deletion, plus a grace period; absent for a live record that never
/// expires.
pub const KV_PURGE_AFTER: &str = "purge_after";

/// The business number a record belongs to (every collection but [`KV`]).
pub const PHONE_NUMBER_ID: &str = "phone_number_id";
/// The contact a record is keyed by (a conversation's).
pub const CONTACT: &str = "contact";

/// A message's direction: `inbound` or `outbound` (its serde name).
pub const MESSAGE_DIRECTION: &str = "direction";
/// A message's kind.
pub const MESSAGE_KIND: &str = "kind";
/// A message's text; absent when it has none.
pub const MESSAGE_TEXT: &str = "text";
/// A message's payload, its JSON text (a BSON string: a JSON object key
/// may hold U+0000, which a BSON field name cannot).
pub const MESSAGE_PAYLOAD: &str = "payload_json";
/// A message's status (its serde name).
pub const MESSAGE_STATUS: &str = "status";
/// A message's timestamp.
pub const MESSAGE_TS: &str = "ts";
/// When its status last changed; absent: never.
pub const MESSAGE_STATUS_AT: &str = "status_at";
/// A `failed` status's error, its JSON text; absent when none.
pub const MESSAGE_ERROR: &str = "error_json";
/// Who sent an inbound message (`StoredMessage::sender`, as it was
/// appended); absent when none, and once an erasure redacted it.
pub const MESSAGE_SENDER: &str = "sender";

/// The summary's latest message's timestamp.
pub const SUMMARY_LAST_MESSAGE_AT: &str = "last_message_at";
/// The summary's latest message's id (ties of the timestamp broken by it).
pub const SUMMARY_LAST_MESSAGE_ID: &str = "last_message_id";
/// The latest message's text; absent when it has none.
pub const SUMMARY_LAST_TEXT: &str = "last_text";
/// The latest live inbound message's timestamp; absent: none.
pub const SUMMARY_LAST_INBOUND_AT: &str = "last_inbound_at";
/// The unread count, a 64-bit integer.
pub const SUMMARY_UNREAD: &str = "unread";
/// How many appends wrote the summary, a 64-bit integer: every append
/// increments it, the write that makes an erasure of the conversation
/// conflict with an append in flight (MongoDB's snapshot transactions
/// conflict on writes only). No read returns it.
pub const SUMMARY_APPENDS: &str = "appends";

/// A window event's id (Meta's: the call's, the standby message's).
pub const WINDOW_EVENT_ID: &str = "id";
/// A window event's kind (`WindowEventKind::as_str`).
pub const WINDOW_EVENT_KIND: &str = "kind";
/// When it happened.
pub const WINDOW_EVENT_TS: &str = "ts";

/// Who holds the thread (`ThreadOwner`'s serde name).
pub const OWNER_OWNER: &str = "owner";
/// The holder's role; absent when unknown.
pub const OWNER_ROLE: &str = "role";
/// The holder's app; absent when unknown.
pub const OWNER_APP_ID: &str = "app_id";
/// Since when.
pub const OWNER_SINCE: &str = "since";

/// A synced contact's `full_name`; absent when none.
pub const CONTACT_FULL_NAME: &str = "full_name";
/// Its `first_name`; absent when none.
pub const CONTACT_FIRST_NAME: &str = "first_name";
/// Its `phone_number`; absent when none.
pub const CONTACT_PHONE_NUMBER: &str = "phone_number";
/// Its `user_id`; absent when none.
pub const CONTACT_USER_ID: &str = "user_id";
/// Its `parent_user_id`; absent when none.
pub const CONTACT_PARENT_USER_ID: &str = "parent_user_id";
/// Its `username`; absent when none.
pub const CONTACT_USERNAME: &str = "username";
/// When the address book changed (or the removal's time).
pub const CONTACT_SYNCED_AT: &str = "synced_at";
/// Whether the document is a kept removal (its key and time only).
pub const CONTACT_REMOVED: &str = "removed";

/// A link's earlier identity.
pub const LINK_PREVIOUS: &str = "previous";
/// The identity that replaced it.
pub const LINK_CURRENT: &str = "current";
/// When Meta reported it.
pub const LINK_TS: &str = "ts";

/// TTL index on [`KV_PURGE_AFTER`]: dead records (expired, deleted) go
/// once their grace period is over (the Postgres index's name).
pub const INDEX_KV_PURGE: &str = "wa_kv_purge_idx";
/// A conversation's history, newest first (the Postgres index's name).
pub const INDEX_MESSAGES_CONVERSATION: &str = "wa_messages_conversation_idx";
/// Purge by age (the Postgres index's name).
pub const INDEX_MESSAGES_TS: &str = "wa_messages_ts_idx";
/// What an erasure matches a person's group messages on: partial, the
/// messages holding a [`MESSAGE_SENDER`] only (the Postgres index's name).
pub const INDEX_MESSAGES_SENDER: &str = "wa_messages_sender_idx";
/// Unique: one summary per conversation.
pub const INDEX_CONVERSATIONS_KEY: &str = "wa_conversations_key_idx";
/// The conversation list, most recent first (the Postgres index's name).
pub const INDEX_CONVERSATIONS_RECENT: &str = "wa_conversations_recent_idx";
/// Purge by age (the Postgres index's name).
pub const INDEX_CONVERSATIONS_LAST: &str = "wa_conversations_last_idx";
/// Unique: one window event per business number and id.
pub const INDEX_WINDOW_EVENTS_KEY: &str = "wa_window_events_key_idx";
/// A conversation's window events, newest first (the Postgres index's
/// name).
pub const INDEX_WINDOW_EVENTS_CONVERSATION: &str = "wa_window_events_conv_idx";
/// Purge by age (the Postgres index's name).
pub const INDEX_WINDOW_EVENTS_TS: &str = "wa_window_events_ts_idx";
/// Unique: one ownership record per conversation.
pub const INDEX_THREAD_OWNERS_KEY: &str = "wa_thread_owners_key_idx";
/// Unique: one synced contact (or removal) per business number and
/// contact.
pub const INDEX_SYNCED_CONTACTS_KEY: &str = "wa_synced_contacts_key_idx";
/// Purge of kept removals by age: partial, the removals only (the Postgres
/// index's name).
pub const INDEX_CONTACT_REMOVALS: &str = "wa_contact_removals_idx";
/// Unique: one link per business number and its two identities.
pub const INDEX_IDENTITY_LINKS_KEY: &str = "wa_identity_links_key_idx";
/// A link found from its later identity (the Postgres index's name).
pub const INDEX_IDENTITY_LINKS_CURRENT: &str = "wa_identity_links_idx";

#[cfg(test)]
mod tests {
    /// The stored names, byte for byte (the `w` of the `wa_` prefix
    /// spelled `\x77`, as every pin of docs/architecture.md § Stable
    /// identifiers is, so that a search-and-replace cannot rewrite them
    /// along with the code). Decisive: any of them renamed.
    #[test]
    fn the_stored_names_are_pinned() {
        use super::*;
        let names = [
            (KV, "\x77a_kv"),
            (MESSAGES, "\x77a_messages"),
            (CONVERSATIONS, "\x77a_conversations"),
            (WINDOW_EVENTS, "\x77a_\x77indow_events"),
            (THREAD_OWNERS, "\x77a_thread_o\x77ners"),
            (SYNCED_CONTACTS, "\x77a_synced_contacts"),
            (IDENTITY_LINKS, "\x77a_identity_links"),
            (KV_NAMESPACE, "namespace"),
            (KV_KEY, "key"),
            (KV_VALUE, "value"),
            (KV_VERSION, "version"),
            (KV_EXPIRES_AT, "expires_at"),
            (KV_PURGE_AFTER, "purge_after"),
            (KV_CLAIM, "claim"),
            (PHONE_NUMBER_ID, "phone_number_id"),
            (CONTACT, "contact"),
            (MESSAGE_DIRECTION, "direction"),
            (MESSAGE_KIND, "kind"),
            (MESSAGE_TEXT, "text"),
            (MESSAGE_PAYLOAD, "payload_json"),
            (MESSAGE_STATUS, "status"),
            (MESSAGE_TS, "ts"),
            (MESSAGE_STATUS_AT, "status_at"),
            (MESSAGE_ERROR, "error_json"),
            (MESSAGE_SENDER, "sender"),
            (SUMMARY_LAST_MESSAGE_AT, "last_message_at"),
            (SUMMARY_LAST_MESSAGE_ID, "last_message_id"),
            (SUMMARY_LAST_TEXT, "last_text"),
            (SUMMARY_LAST_INBOUND_AT, "last_inbound_at"),
            (SUMMARY_UNREAD, "unread"),
            (SUMMARY_APPENDS, "appends"),
            (WINDOW_EVENT_ID, "id"),
            (WINDOW_EVENT_KIND, "kind"),
            (WINDOW_EVENT_TS, "ts"),
            (OWNER_OWNER, "o\x77ner"),
            (OWNER_ROLE, "role"),
            (OWNER_APP_ID, "app_id"),
            (OWNER_SINCE, "since"),
            (CONTACT_FULL_NAME, "full_name"),
            (CONTACT_FIRST_NAME, "first_name"),
            (CONTACT_PHONE_NUMBER, "phone_number"),
            (CONTACT_USER_ID, "user_id"),
            (CONTACT_PARENT_USER_ID, "parent_user_id"),
            (CONTACT_USERNAME, "username"),
            (CONTACT_SYNCED_AT, "synced_at"),
            (CONTACT_REMOVED, "removed"),
            (LINK_PREVIOUS, "previous"),
            (LINK_CURRENT, "current"),
            (LINK_TS, "ts"),
            (INDEX_KV_PURGE, "\x77a_kv_purge_idx"),
            (
                INDEX_MESSAGES_CONVERSATION,
                "\x77a_messages_conversation_idx",
            ),
            (INDEX_MESSAGES_TS, "\x77a_messages_ts_idx"),
            (INDEX_MESSAGES_SENDER, "\x77a_messages_sender_idx"),
            (INDEX_CONVERSATIONS_KEY, "\x77a_conversations_key_idx"),
            (INDEX_CONVERSATIONS_RECENT, "\x77a_conversations_recent_idx"),
            (INDEX_CONVERSATIONS_LAST, "\x77a_conversations_last_idx"),
            (INDEX_WINDOW_EVENTS_KEY, "\x77a_\x77indow_events_key_idx"),
            (
                INDEX_WINDOW_EVENTS_CONVERSATION,
                "\x77a_\x77indow_events_conv_idx",
            ),
            (INDEX_WINDOW_EVENTS_TS, "\x77a_\x77indow_events_ts_idx"),
            (INDEX_THREAD_OWNERS_KEY, "\x77a_thread_o\x77ners_key_idx"),
            (INDEX_SYNCED_CONTACTS_KEY, "\x77a_synced_contacts_key_idx"),
            (INDEX_CONTACT_REMOVALS, "\x77a_contact_removals_idx"),
            (INDEX_IDENTITY_LINKS_KEY, "\x77a_identity_links_key_idx"),
            (INDEX_IDENTITY_LINKS_CURRENT, "\x77a_identity_links_idx"),
        ];
        for (name, pinned) in names {
            assert_eq!(name, pinned);
        }
    }
}
