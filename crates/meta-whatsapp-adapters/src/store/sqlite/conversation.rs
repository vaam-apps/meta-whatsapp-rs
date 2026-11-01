//! [`ConversationStore`] on SQLite.
//!
//! Every write is one `BEGIN IMMEDIATE` transaction (see the
//! [module docs](super#concurrency)): it holds SQLite's write lock from its
//! first statement, so it reads, decides in Rust and writes as one step,
//! and the summary can never disagree with the history.
//!
//! - A message row is keyed by `(phone_number_id, id)`; the unique index on
//!   `id` alone (`wa_messages_id_idx`) keeps today's rule, an id once per
//!   store (`OPEN_QUESTIONS.md` #33). Inserts are `ON CONFLICT DO NOTHING`
//!   on either, so a duplicate changes nothing.
//! - `append` and `append_synced` insert the row, then upsert its
//!   conversation's summary (the latest by `(ts, id)`, the window and the
//!   unread count for a live inbound one), in the same transaction; a batch
//!   is one transaction.
//! - `update_status` and `revoke` read the stored status and apply
//!   [`DeliveryStatus::supersedes`] (the rule lives in `meta-whatsapp-core`)
//!   under the lock. Both match `id` *and* `phone_number_id`, as do
//!   `fill_media_placeholder` and the lookup.
//! - `erase_all` and `purge_before` delete in one transaction each: what
//!   an erasure reaches is the memory and Postgres stores' (the person's
//!   records, the synced contacts and links naming them, their group
//!   messages by [`StoredMessage::sender`], written with each message).
//! - `identities` walks the synced contacts and the links of one number in
//!   one read transaction, a snapshot of the store.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::ids::{AppId, MessageId, PhoneNumberId, UserId};
use meta_whatsapp_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, DeliveryStatus, Direction, Erased,
    ErasureMode, IdentityLink, Purged, Retention, StoredContact, StoredMessage, ThreadOwnership,
    WindowEvent, WindowEventKind,
};
use sqlx::sqlite::SqliteRow;
use sqlx::{AssertSqlSafe, Row, Sqlite, SqlitePool, Transaction};
use time::OffsetDateTime;

use super::{BEGIN_WRITE, TablePrefix, backend, from_micros, micros, to_i64};
use crate::store::codec::{
    direction_str, json_text, owner_str, parse_direction, parse_json, parse_owner, parse_status,
    status_str, utf8,
};

/// `ConversationStore` over an SQLite pool. Cheap to clone.
///
/// Unread counts are by **arrival** (inbound messages appended since the
/// last `mark_read`; synced history never counts), like the memory and
/// Postgres stores. Timestamps are stored with microsecond precision.
/// Retention is kept by default ([`with_retention`](Self::with_retention)
/// sets what [`ConversationStore::apply_retention`] purges); an erasure
/// redacts an erased person's group messages by default
/// ([`with_erasure_mode`](Self::with_erasure_mode) deletes them).
#[derive(Clone)]
pub struct SqliteConversationStore {
    pool: SqlitePool,
    prefix: TablePrefix,
    sql: Arc<Sql>,
    retention: Retention,
    erasure_mode: ErasureMode,
}

impl fmt::Debug for SqliteConversationStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteConversationStore")
            .field("prefix", &self.prefix)
            .field("retention", &self.retention)
            .field("erasure_mode", &self.erasure_mode)
            .finish_non_exhaustive()
    }
}

const MESSAGE_COLUMNS: &str = "id, phone_number_id, contact, direction, kind_utf8, text_utf8, \
     payload_json, status, ts, status_at, error_json";

const SUMMARY_COLUMNS: &str =
    "phone_number_id, contact, last_message_at, last_inbound_at, last_text_utf8, unread";

const WINDOW_EVENT_COLUMNS: &str = "phone_number_id, id, contact, kind, ts";

const OWNER_COLUMNS: &str = "owner, role, app_id, since";

const CONTACT_COLUMNS: &str = "phone_number_id, contact, full_name_utf8, first_name_utf8, \
     phone_number, user_id, parent_user_id, username_utf8, synced_at";

/// The statements, rendered once per store with its prefix.
struct Sql {
    insert: Arc<str>,
    summarize: Arc<str>,
    status_get: Arc<str>,
    status_set: Arc<str>,
    fill: Arc<str>,
    fill_preview: Arc<str>,
    messages: Arc<str>,
    messages_before: Arc<str>,
    conversations: Arc<str>,
    conversations_before: Arc<str>,
    mark_read: Arc<str>,
    last_inbound_at: Arc<str>,
    message: Arc<str>,
    window_event: Arc<str>,
    window_events: Arc<str>,
    window_events_before: Arc<str>,
    set_owner: Arc<str>,
    owner: Arc<str>,
    put_contact: Arc<str>,
    contact_state: Arc<str>,
    remove_contact: Arc<str>,
    contact: Arc<str>,
    contacts: Arc<str>,
    contacts_after: Arc<str>,
    link_identity: Arc<str>,
    identity_links: Arc<str>,
    naming_contacts: Arc<str>,
    erase_messages: Arc<str>,
    erase_conversation: Arc<str>,
    erase_window_events: Arc<str>,
    erase_owner: Arc<str>,
    erase_keyed_contact: Arc<str>,
    erase_naming_contacts: Arc<str>,
    erase_links: Arc<str>,
    sent_by: Arc<str>,
    is_latest: Arc<str>,
    redact: Arc<str>,
    clear_preview: Arc<str>,
    delete_message: Arc<str>,
    latest_remaining: Arc<str>,
    resummarize: Arc<str>,
    drop_summary: Arc<str>,
    purge: [Arc<str>; 5],
}

impl Sql {
    #[allow(clippy::too_many_lines)] // one statement per field
    fn new(prefix: &TablePrefix) -> Self {
        let messages = prefix.table("messages");
        let conversations = prefix.table("conversations");
        let window_events = prefix.table("window_events");
        let owners = prefix.table("thread_owners");
        let contacts = prefix.table("synced_contacts");
        let links = prefix.table("identity_links");
        let arc = |s: String| -> Arc<str> { Arc::from(s) };
        let newer = "(excluded.last_message_at, excluded.last_message_id) \
                     > (last_message_at, last_message_id)";
        // `?2` is the business number, or NULL for every number.
        let scope = "(?2 IS NULL OR phone_number_id = ?2)";
        Self {
            // A conflict on the key or on the id alone (today's rule):
            // nothing is written.
            insert: arc(format!(
                "INSERT INTO {messages} (id, phone_number_id, contact, direction, kind_utf8, \
                   text_utf8, payload_json, status, ts, status_at, error_json, sender) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                 ON CONFLICT DO NOTHING"
            )),
            // `?6` / `?7`: what the row adds to the window and the unread
            // count (NULL and 0 but for a live inbound message).
            summarize: arc(format!(
                "INSERT INTO {conversations} (phone_number_id, contact, last_message_at, \
                   last_message_id, last_text_utf8, last_inbound_at, unread) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
                   last_message_at = CASE WHEN {newer} THEN excluded.last_message_at \
                     ELSE last_message_at END, \
                   last_message_id = CASE WHEN {newer} THEN excluded.last_message_id \
                     ELSE last_message_id END, \
                   last_text_utf8 = CASE WHEN {newer} THEN excluded.last_text_utf8 \
                     ELSE last_text_utf8 END, \
                   last_inbound_at = CASE WHEN excluded.last_inbound_at IS NULL THEN last_inbound_at \
                     WHEN last_inbound_at IS NULL THEN excluded.last_inbound_at \
                     ELSE max(last_inbound_at, excluded.last_inbound_at) END, \
                   unread = unread + excluded.unread"
            )),
            status_get: arc(format!(
                "SELECT status, direction FROM {messages} WHERE phone_number_id = ?1 AND id = ?2"
            )),
            status_set: arc(format!(
                "UPDATE {messages} SET status = ?3, status_at = ?4, \
                   error_json = COALESCE(?5, error_json) \
                 WHERE phone_number_id = ?1 AND id = ?2"
            )),
            // `?6`: the placeholder kind, as bytes; `?7`: the `deleted`
            // status, which a placeholder is never filled in.
            fill: arc(format!(
                "UPDATE {messages} SET kind_utf8 = ?3, text_utf8 = ?4, payload_json = ?5 \
                 WHERE phone_number_id = ?1 AND id = ?2 AND kind_utf8 = ?6 AND status <> ?7 \
                 RETURNING contact"
            )),
            fill_preview: arc(format!(
                "UPDATE {conversations} SET last_text_utf8 = ?4 \
                 WHERE phone_number_id = ?1 AND contact = ?2 AND last_message_id = ?3"
            )),
            messages: arc(format!(
                "SELECT {MESSAGE_COLUMNS} FROM {messages} \
                 WHERE phone_number_id = ?1 AND contact = ?2 \
                 ORDER BY ts DESC, id DESC LIMIT ?3"
            )),
            messages_before: arc(format!(
                "SELECT {MESSAGE_COLUMNS} FROM {messages} \
                 WHERE phone_number_id = ?1 AND contact = ?2 AND (ts, id) < (?4, ?5) \
                 ORDER BY ts DESC, id DESC LIMIT ?3"
            )),
            conversations: arc(format!(
                "SELECT {SUMMARY_COLUMNS} FROM {conversations} \
                 WHERE phone_number_id = ?1 \
                 ORDER BY last_message_at DESC, contact DESC LIMIT ?2"
            )),
            conversations_before: arc(format!(
                "SELECT {SUMMARY_COLUMNS} FROM {conversations} \
                 WHERE phone_number_id = ?1 AND (last_message_at, contact) < (?3, ?4) \
                 ORDER BY last_message_at DESC, contact DESC LIMIT ?2"
            )),
            mark_read: arc(format!(
                "UPDATE {conversations} SET unread = 0 WHERE phone_number_id = ?1 AND contact = ?2"
            )),
            last_inbound_at: arc(format!(
                "SELECT last_inbound_at FROM {conversations} \
                 WHERE phone_number_id = ?1 AND contact = ?2"
            )),
            message: arc(format!(
                "SELECT {MESSAGE_COLUMNS} FROM {messages} WHERE phone_number_id = ?1 AND id = ?2"
            )),
            window_event: arc(format!(
                "INSERT INTO {window_events} ({WINDOW_EVENT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT DO NOTHING"
            )),
            window_events: arc(format!(
                "SELECT {WINDOW_EVENT_COLUMNS} FROM {window_events} \
                 WHERE phone_number_id = ?1 AND contact = ?2 \
                 ORDER BY ts DESC, id DESC LIMIT ?3"
            )),
            window_events_before: arc(format!(
                "SELECT {WINDOW_EVENT_COLUMNS} FROM {window_events} \
                 WHERE phone_number_id = ?1 AND contact = ?2 AND (ts, id) < (?4, ?5) \
                 ORDER BY ts DESC, id DESC LIMIT ?3"
            )),
            // The update applies only when the stored record is not later.
            set_owner: arc(format!(
                "INSERT INTO {owners} (phone_number_id, contact, {OWNER_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
                   owner = excluded.owner, role = excluded.role, app_id = excluded.app_id, \
                   since = excluded.since \
                 WHERE excluded.since >= since"
            )),
            owner: arc(format!(
                "SELECT {OWNER_COLUMNS} FROM {owners} WHERE phone_number_id = ?1 AND contact = ?2"
            )),
            // A removal counts as stored: an older `add` is refused.
            put_contact: arc(format!(
                "INSERT INTO {contacts} ({CONTACT_COLUMNS}, removed) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0) \
                 ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
                   full_name_utf8 = excluded.full_name_utf8, \
                   first_name_utf8 = excluded.first_name_utf8, \
                   phone_number = excluded.phone_number, user_id = excluded.user_id, \
                   parent_user_id = excluded.parent_user_id, \
                   username_utf8 = excluded.username_utf8, synced_at = excluded.synced_at, \
                   removed = 0 \
                 WHERE excluded.synced_at >= synced_at"
            )),
            contact_state: arc(format!(
                "SELECT synced_at, removed FROM {contacts} \
                 WHERE phone_number_id = ?1 AND contact = ?2"
            )),
            // The removal's key and time, nothing of the contact.
            remove_contact: arc(format!(
                "INSERT INTO {contacts} (phone_number_id, contact, synced_at, removed) \
                 VALUES (?1, ?2, ?3, 1) \
                 ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
                   full_name_utf8 = NULL, first_name_utf8 = NULL, phone_number = NULL, \
                   user_id = NULL, parent_user_id = NULL, username_utf8 = NULL, \
                   synced_at = excluded.synced_at, removed = 1"
            )),
            contact: arc(format!(
                "SELECT {CONTACT_COLUMNS} FROM {contacts} \
                 WHERE phone_number_id = ?1 AND contact = ?2 AND removed = 0"
            )),
            contacts: arc(format!(
                "SELECT {CONTACT_COLUMNS} FROM {contacts} \
                 WHERE phone_number_id = ?1 AND removed = 0 ORDER BY contact LIMIT ?2"
            )),
            contacts_after: arc(format!(
                "SELECT {CONTACT_COLUMNS} FROM {contacts} \
                 WHERE phone_number_id = ?1 AND contact > ?3 AND removed = 0 \
                 ORDER BY contact LIMIT ?2"
            )),
            link_identity: arc(format!(
                "INSERT INTO {links} (phone_number_id, previous, current, ts) \
                 VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING"
            )),
            identity_links: arc(format!(
                "SELECT phone_number_id, previous, current, ts FROM {links} \
                 WHERE phone_number_id = ?1 AND (previous = ?2 OR current = ?2) \
                 ORDER BY ts, previous, current"
            )),
            // The synced contacts (not kept removals) naming `?2` by any of
            // their four ids.
            naming_contacts: arc(format!(
                "SELECT contact, user_id, parent_user_id, phone_number FROM {contacts} \
                 WHERE phone_number_id = ?1 AND removed = 0 \
                   AND (contact = ?2 OR user_id = ?2 OR parent_user_id = ?2 OR phone_number = ?2)"
            )),
            erase_messages: arc(format!(
                "DELETE FROM {messages} WHERE phone_number_id = ?1 AND contact = ?2"
            )),
            erase_conversation: arc(format!(
                "DELETE FROM {conversations} WHERE phone_number_id = ?1 AND contact = ?2"
            )),
            erase_window_events: arc(format!(
                "DELETE FROM {window_events} WHERE phone_number_id = ?1 AND contact = ?2"
            )),
            erase_owner: arc(format!(
                "DELETE FROM {owners} WHERE phone_number_id = ?1 AND contact = ?2"
            )),
            erase_keyed_contact: arc(format!(
                "DELETE FROM {contacts} WHERE phone_number_id = ?1 AND contact = ?2"
            )),
            erase_naming_contacts: arc(format!(
                "DELETE FROM {contacts} WHERE phone_number_id = ?1 \
                   AND (user_id = ?2 OR parent_user_id = ?2 OR phone_number = ?2)"
            )),
            erase_links: arc(format!(
                "DELETE FROM {links} WHERE phone_number_id = ?1 AND (previous = ?2 OR current = ?2)"
            )),
            sent_by: arc(format!(
                "SELECT id, contact FROM {messages} WHERE phone_number_id = ?1 AND sender = ?2"
            )),
            is_latest: arc(format!(
                "SELECT EXISTS (SELECT 1 FROM {conversations} \
                   WHERE phone_number_id = ?1 AND contact = ?2 AND last_message_id = ?3)"
            )),
            // `?3`: `StoredMessage::ERASED`'s bytes.
            redact: arc(format!(
                "UPDATE {messages} SET kind_utf8 = ?3, text_utf8 = NULL, payload_json = '{{}}', \
                   error_json = NULL, sender = NULL \
                 WHERE phone_number_id = ?1 AND id = ?2"
            )),
            clear_preview: arc(format!(
                "UPDATE {conversations} SET last_text_utf8 = NULL \
                 WHERE phone_number_id = ?1 AND contact = ?2"
            )),
            delete_message: arc(format!(
                "DELETE FROM {messages} WHERE phone_number_id = ?1 AND id = ?2"
            )),
            // `?3`: `StoredMessage::REVOKED`'s bytes: a tombstone is never
            // the summary's.
            latest_remaining: arc(format!(
                "SELECT ts, id, text_utf8 FROM {messages} \
                 WHERE phone_number_id = ?1 AND contact = ?2 AND kind_utf8 <> ?3 \
                 ORDER BY ts DESC, id DESC LIMIT 1"
            )),
            resummarize: arc(format!(
                "UPDATE {conversations} SET last_message_at = ?3, last_message_id = ?4, \
                   last_text_utf8 = ?5 \
                 WHERE phone_number_id = ?1 AND contact = ?2"
            )),
            drop_summary: arc(format!(
                "DELETE FROM {conversations} WHERE phone_number_id = ?1 AND contact = ?2"
            )),
            purge: [
                arc(format!("DELETE FROM {messages} WHERE ts < ?1 AND {scope}")),
                arc(format!(
                    "DELETE FROM {conversations} WHERE last_message_at < ?1 AND {scope}"
                )),
                arc(format!(
                    "DELETE FROM {window_events} WHERE ts < ?1 AND {scope}"
                )),
                arc(format!("DELETE FROM {owners} WHERE since < ?1 AND {scope}")),
                arc(format!(
                    "DELETE FROM {contacts} WHERE removed = 1 AND synced_at < ?1 AND {scope}"
                )),
            ],
        }
    }
}

impl SqliteConversationStore {
    /// Store on `pool` with the default `wa_` tables. Run
    /// [`migrate`](super::migrate) first.
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_prefix(pool, TablePrefix::DEFAULT)
    }

    /// Store on `pool` using `prefix`'s tables. Run
    /// [`migrate_with_prefix`](super::migrate_with_prefix) with the same
    /// prefix first.
    pub fn with_prefix(pool: SqlitePool, prefix: TablePrefix) -> Self {
        let sql = Arc::new(Sql::new(&prefix));
        Self {
            pool,
            prefix,
            sql,
            retention: Retention::Keep,
            erasure_mode: ErasureMode::Redact,
        }
    }

    /// This store applying `retention` in
    /// [`ConversationStore::apply_retention`] (the default keeps
    /// everything).
    #[must_use]
    pub fn with_retention(mut self, retention: Retention) -> Self {
        self.retention = retention;
        self
    }

    /// This store redacting or deleting an erased person's messages in
    /// other conversations (a group's) as `mode` says, in
    /// [`ConversationStore::erase_all`] (the default redacts:
    /// [`ErasureMode::Redact`]).
    #[must_use]
    pub fn with_erasure_mode(mut self, mode: ErasureMode) -> Self {
        self.erasure_mode = mode;
        self
    }

    /// The table prefix in use.
    pub fn prefix(&self) -> &TablePrefix {
        &self.prefix
    }

    async fn begin(&self) -> Result<Transaction<'static, Sqlite>, StorageError> {
        self.pool.begin_with(BEGIN_WRITE).await.map_err(backend)
    }

    fn query(sql: &Arc<str>) -> sqlx::query::Query<'static, Sqlite, sqlx::sqlite::SqliteArguments> {
        sqlx::query(AssertSqlSafe(Arc::clone(sql)))
    }

    /// Insert `message`'s row in `tx`. Returns whether it was stored:
    /// `false` when its id is stored already (on any number: today's rule).
    async fn insert_row(
        &self,
        tx: &mut Transaction<'static, Sqlite>,
        message: &StoredMessage,
    ) -> Result<bool, StorageError> {
        let payload = json_text(&message.payload, "message payload")?;
        let error = message
            .error
            .as_ref()
            .map(|e| json_text(e, "message error"))
            .transpose()?;
        let done = Self::query(&self.sql.insert)
            .bind(message.id.as_str())
            .bind(message.conversation.phone_number_id.as_str())
            .bind(message.conversation.contact.as_str())
            .bind(direction_str(message.direction))
            .bind(message.kind.as_bytes())
            .bind(message.text.as_deref().map(str::as_bytes))
            .bind(payload)
            .bind(status_str(message.status)?)
            .bind(micros(message.timestamp))
            .bind(message.status_at.map(micros))
            .bind(error)
            .bind(message.sender())
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected() == 1)
    }

    /// Fold a stored `message` into its conversation's summary; a `live`
    /// inbound one moves the window and counts as unread.
    async fn summarize(
        &self,
        tx: &mut Transaction<'static, Sqlite>,
        message: &StoredMessage,
        live: bool,
    ) -> Result<(), StorageError> {
        let opens_window = live && message.direction == Direction::Inbound;
        Self::query(&self.sql.summarize)
            .bind(message.conversation.phone_number_id.as_str())
            .bind(message.conversation.contact.as_str())
            .bind(micros(message.timestamp))
            .bind(message.id.as_str())
            .bind(message.text.as_deref().map(str::as_bytes))
            .bind(opens_window.then(|| micros(message.timestamp)))
            .bind(i64::from(opens_window))
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
        Ok(())
    }

    /// Move message `id` of `phone_number_id` (in `direction` only, when
    /// given) to `status` if that supersedes the stored one, in `tx`.
    #[allow(clippy::too_many_arguments)]
    async fn set_status(
        &self,
        tx: &mut Transaction<'static, Sqlite>,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        direction: Option<Direction>,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<&serde_json::Value>,
    ) -> Result<bool, StorageError> {
        let row = Self::query(&self.sql.status_get)
            .bind(phone_number_id.as_str())
            .bind(id.as_str())
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
        let Some(row) = row else {
            return Ok(false);
        };
        let current: String = row.try_get("status").map_err(backend)?;
        let stored_direction: String = row.try_get("direction").map_err(backend)?;
        if direction.is_some_and(|d| direction_str(d) != stored_direction)
            || !status.supersedes(parse_status(current)?)
        {
            return Ok(false);
        }
        let error = error.map(|e| json_text(e, "message error")).transpose()?;
        Self::query(&self.sql.status_set)
            .bind(phone_number_id.as_str())
            .bind(id.as_str())
            .bind(status_str(status)?)
            .bind(micros(at))
            .bind(error)
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
        Ok(true)
    }

    /// Run `sql` with `(phone_number_id, id)` in `tx`; the rows it changed.
    async fn run2(
        tx: &mut Transaction<'static, Sqlite>,
        sql: &Arc<str>,
        phone_number_id: &PhoneNumberId,
        id: &str,
    ) -> Result<u64, StorageError> {
        let done = Self::query(sql)
            .bind(phone_number_id.as_str())
            .bind(id)
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected())
    }

    /// Make conversation `contact`'s summary follow its latest remaining
    /// message (never a tombstone), or drop it when none is left.
    async fn resummarize(
        &self,
        tx: &mut Transaction<'static, Sqlite>,
        phone_number_id: &PhoneNumberId,
        contact: &str,
    ) -> Result<(), StorageError> {
        let latest = Self::query(&self.sql.latest_remaining)
            .bind(phone_number_id.as_str())
            .bind(contact)
            .bind(StoredMessage::REVOKED.as_bytes())
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
        match latest {
            Some(row) => {
                let ts: i64 = row.try_get("ts").map_err(backend)?;
                let id: String = row.try_get("id").map_err(backend)?;
                let text: Option<Vec<u8>> = row.try_get("text_utf8").map_err(backend)?;
                Self::query(&self.sql.resummarize)
                    .bind(phone_number_id.as_str())
                    .bind(contact)
                    .bind(ts)
                    .bind(id)
                    .bind(text)
                    .execute(&mut **tx)
                    .await
                    .map_err(backend)?;
            }
            None => {
                Self::run2(tx, &self.sql.drop_summary, phone_number_id, contact).await?;
            }
        }
        Ok(())
    }

    /// The erased person's messages in conversations keyed by someone else
    /// (a group's): those sent by `sender`, redacted or deleted per the
    /// store's mode. Returns how many, and adds to `touched` the
    /// conversations whose latest message a deletion took.
    async fn erase_sent_by(
        &self,
        tx: &mut Transaction<'static, Sqlite>,
        phone_number_id: &PhoneNumberId,
        sender: &str,
        touched: &mut BTreeSet<String>,
    ) -> Result<u64, StorageError> {
        let rows = Self::query(&self.sql.sent_by)
            .bind(phone_number_id.as_str())
            .bind(sender)
            .fetch_all(&mut **tx)
            .await
            .map_err(backend)?;
        let mut n = 0;
        for row in rows {
            let id: String = row.try_get("id").map_err(backend)?;
            let contact: String = row.try_get("contact").map_err(backend)?;
            let latest: bool = Self::query(&self.sql.is_latest)
                .bind(phone_number_id.as_str())
                .bind(&contact)
                .bind(&id)
                .fetch_one(&mut **tx)
                .await
                .map_err(backend)?
                .try_get(0)
                .map_err(backend)?;
            if self.erasure_mode == ErasureMode::Delete {
                Self::run2(tx, &self.sql.delete_message, phone_number_id, &id).await?;
                if latest {
                    touched.insert(contact);
                }
            } else {
                // `Redact`, and any mode added later: never keep the content.
                Self::query(&self.sql.redact)
                    .bind(phone_number_id.as_str())
                    .bind(&id)
                    .bind(StoredMessage::ERASED.as_bytes())
                    .execute(&mut **tx)
                    .await
                    .map_err(backend)?;
                if latest {
                    Self::run2(tx, &self.sql.clear_preview, phone_number_id, &contact).await?;
                }
            }
            n += 1;
        }
        Ok(n)
    }
}

/// Refuse an identifier holding U+0000 (`what` names it, never its value):
/// SQLite keeps identifiers as `TEXT`, and Meta never assigns one with it.
fn identifier(what: &str, value: &str) -> Result<(), StorageError> {
    if value.contains('\0') {
        return Err(StorageError::Backend(anyhow::anyhow!(
            "{what} holds U+0000, which the SQLite store refuses in an identifier"
        )));
    }
    Ok(())
}

fn key_identifiers(key: &ConversationKey) -> Result<(), StorageError> {
    identifier("phone number id", key.phone_number_id.as_str())?;
    identifier("contact", &key.contact)
}

fn message_identifiers(message: &StoredMessage) -> Result<(), StorageError> {
    identifier("message id", message.id.as_str())?;
    key_identifiers(&message.conversation)
}

fn optional_utf8(row: &SqliteRow, column: &str) -> Result<Option<String>, StorageError> {
    let bytes: Option<Vec<u8>> = row.try_get(column).map_err(backend)?;
    bytes.map(|b| utf8(column, b)).transpose()
}

fn time_at(row: &SqliteRow, column: &str) -> Result<OffsetDateTime, StorageError> {
    let us: i64 = row.try_get(column).map_err(backend)?;
    from_micros(us, column)
}

fn optional_time_at(row: &SqliteRow, column: &str) -> Result<Option<OffsetDateTime>, StorageError> {
    let us: Option<i64> = row.try_get(column).map_err(backend)?;
    us.map(|us| from_micros(us, column)).transpose()
}

fn message_from_row(row: &SqliteRow) -> Result<StoredMessage, StorageError> {
    let id: String = row.try_get("id").map_err(backend)?;
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let direction: String = row.try_get("direction").map_err(backend)?;
    let kind: Vec<u8> = row.try_get("kind_utf8").map_err(backend)?;
    let payload: String = row.try_get("payload_json").map_err(backend)?;
    let status: String = row.try_get("status").map_err(backend)?;
    let error: Option<String> = row.try_get("error_json").map_err(backend)?;
    Ok(StoredMessage {
        conversation: ConversationKey::new(PhoneNumberId::new_unchecked(phone_number_id), contact),
        direction: parse_direction(&direction)?,
        kind: utf8("kind_utf8", kind)?,
        text: optional_utf8(row, "text_utf8")?,
        payload: parse_json(payload.as_bytes(), "payload_json")?,
        status: parse_status(status)?,
        timestamp: time_at(row, "ts")?,
        status_at: optional_time_at(row, "status_at")?,
        error: error
            .map(|e| parse_json(e.as_bytes(), "error_json"))
            .transpose()?,
        id: MessageId::new(id),
    })
}

fn summary_from_row(row: &SqliteRow) -> Result<ConversationSummary, StorageError> {
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let unread: i64 = row.try_get("unread").map_err(backend)?;
    Ok(ConversationSummary {
        key: ConversationKey::new(PhoneNumberId::new_unchecked(phone_number_id), contact),
        last_message_at: time_at(row, "last_message_at")?,
        last_inbound_at: optional_time_at(row, "last_inbound_at")?,
        last_text: optional_utf8(row, "last_text_utf8")?,
        // Never negative: it only grows by 1 and resets to 0.
        unread: u64::try_from(unread).unwrap_or(0),
    })
}

fn window_event_from_row(row: &SqliteRow) -> Result<WindowEvent, StorageError> {
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let kind: String = row.try_get("kind").map_err(backend)?;
    Ok(WindowEvent {
        conversation: ConversationKey::new(PhoneNumberId::new_unchecked(phone_number_id), contact),
        kind: WindowEventKind::from_name(&kind),
        id: row.try_get("id").map_err(backend)?,
        at: time_at(row, "ts")?,
    })
}

fn owner_from_row(row: &SqliteRow) -> Result<ThreadOwnership, StorageError> {
    let owner: String = row.try_get("owner").map_err(backend)?;
    let app_id: Option<String> = row.try_get("app_id").map_err(backend)?;
    Ok(ThreadOwnership {
        owner: parse_owner(owner)?,
        role: row.try_get("role").map_err(backend)?,
        app_id: app_id.map(AppId::new_unchecked),
        since: time_at(row, "since")?,
    })
}

fn contact_from_row(row: &SqliteRow) -> Result<StoredContact, StorageError> {
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let user_id: Option<String> = row.try_get("user_id").map_err(backend)?;
    let parent_user_id: Option<String> = row.try_get("parent_user_id").map_err(backend)?;
    Ok(StoredContact {
        key: ConversationKey::new(PhoneNumberId::new_unchecked(phone_number_id), contact),
        full_name: optional_utf8(row, "full_name_utf8")?,
        first_name: optional_utf8(row, "first_name_utf8")?,
        phone_number: row.try_get("phone_number").map_err(backend)?,
        user_id: user_id.map(UserId::new),
        parent_user_id: parent_user_id.map(UserId::new),
        username: optional_utf8(row, "username_utf8")?,
        synced_at: time_at(row, "synced_at")?,
    })
}

fn link_from_row(row: &SqliteRow) -> Result<IdentityLink, StorageError> {
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    Ok(IdentityLink {
        phone_number_id: PhoneNumberId::new_unchecked(phone_number_id),
        previous: row.try_get("previous").map_err(backend)?,
        current: row.try_get("current").map_err(backend)?,
        at: time_at(row, "ts")?,
    })
}

#[async_trait]
impl ConversationStore for SqliteConversationStore {
    async fn append(&self, message: StoredMessage) -> Result<bool, StorageError> {
        message_identifiers(&message)?;
        let mut tx = self.begin().await?;
        if !self.insert_row(&mut tx, &message).await? {
            return Ok(false);
        }
        self.summarize(&mut tx, &message, true).await?;
        tx.commit().await.map_err(backend)?;
        Ok(true)
    }

    async fn append_synced(&self, messages: Vec<StoredMessage>) -> Result<Vec<bool>, StorageError> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        for message in &messages {
            message_identifiers(message)?;
        }
        let mut tx = self.begin().await?;
        let mut inserted = Vec::with_capacity(messages.len());
        for message in &messages {
            let stored = self.insert_row(&mut tx, message).await?;
            if stored {
                self.summarize(&mut tx, message, false).await?;
            }
            inserted.push(stored);
        }
        tx.commit().await.map_err(backend)?;
        Ok(inserted)
    }

    async fn fill_media_placeholder(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        kind: String,
        text: Option<String>,
        payload: serde_json::Value,
    ) -> Result<bool, StorageError> {
        let payload = json_text(&payload, "message payload")?;
        let mut tx = self.begin().await?;
        let contact: Option<String> = sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.fill)))
            .bind(phone_number_id.as_str())
            .bind(id.as_str())
            .bind(kind.as_bytes())
            .bind(text.as_deref().map(str::as_bytes))
            .bind(payload)
            .bind(StoredMessage::MEDIA_PLACEHOLDER.as_bytes())
            .bind(status_str(DeliveryStatus::Deleted)?)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
        let Some(contact) = contact else {
            return Ok(false);
        };
        Self::query(&self.sql.fill_preview)
            .bind(phone_number_id.as_str())
            .bind(contact)
            .bind(id.as_str())
            .bind(text.as_deref().map(str::as_bytes))
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        Ok(true)
    }

    async fn revoke(
        &self,
        key: &ConversationKey,
        id: &MessageId,
        direction: Direction,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError> {
        let tombstone = StoredMessage::tombstone(key, id, direction, at);
        message_identifiers(&tombstone)?;
        let mut tx = self.begin().await?;
        // History only: a tombstone is never part of the summary. Its id
        // taken (on any number), the revoke applies to the stored message
        // of its own number and direction, if there is one.
        let changed = self.insert_row(&mut tx, &tombstone).await?
            || self
                .set_status(
                    &mut tx,
                    &key.phone_number_id,
                    id,
                    Some(direction),
                    DeliveryStatus::Deleted,
                    at,
                    None,
                )
                .await?;
        if changed {
            tx.commit().await.map_err(backend)?;
        }
        Ok(changed)
    }

    async fn update_status(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<serde_json::Value>,
    ) -> Result<bool, StorageError> {
        let mut tx = self.begin().await?;
        let changed = self
            .set_status(
                &mut tx,
                phone_number_id,
                id,
                None,
                status,
                at,
                error.as_ref(),
            )
            .await?;
        if changed {
            tx.commit().await.map_err(backend)?;
        }
        Ok(changed)
    }

    async fn messages(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, MessageId)>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = match &before {
            None => Self::query(&self.sql.messages),
            Some(_) => Self::query(&self.sql.messages_before),
        }
        .bind(key.phone_number_id.as_str())
        .bind(key.contact.as_str())
        .bind(to_i64(limit));
        let query = match &before {
            None => query,
            Some((at, id)) => query.bind(micros(*at)).bind(id.as_str()),
        };
        let rows = query.fetch_all(&self.pool).await.map_err(backend)?;
        rows.iter().map(message_from_row).collect()
    }

    async fn conversations(
        &self,
        phone_number_id: &PhoneNumberId,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = match &before {
            None => Self::query(&self.sql.conversations),
            Some(_) => Self::query(&self.sql.conversations_before),
        }
        .bind(phone_number_id.as_str())
        .bind(to_i64(limit));
        let query = match &before {
            None => query,
            Some((at, contact)) => query.bind(micros(*at)).bind(contact.as_str()),
        };
        let rows = query.fetch_all(&self.pool).await.map_err(backend)?;
        rows.iter().map(summary_from_row).collect()
    }

    async fn mark_read(&self, key: &ConversationKey) -> Result<(), StorageError> {
        Self::query(&self.sql.mark_read)
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }

    async fn last_inbound_at(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<OffsetDateTime>, StorageError> {
        let at: Option<Option<i64>> =
            sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.last_inbound_at)))
                .bind(key.phone_number_id.as_str())
                .bind(key.contact.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        at.flatten()
            .map(|us| from_micros(us, "last_inbound_at"))
            .transpose()
    }

    async fn message(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
    ) -> Result<Option<StoredMessage>, StorageError> {
        let row = Self::query(&self.sql.message)
            .bind(phone_number_id.as_str())
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(message_from_row).transpose()
    }

    async fn record_window_event(&self, event: WindowEvent) -> Result<bool, StorageError> {
        key_identifiers(&event.conversation)?;
        identifier("window event id", &event.id)?;
        let done = Self::query(&self.sql.window_event)
            .bind(event.conversation.phone_number_id.as_str())
            .bind(event.id.as_str())
            .bind(event.conversation.contact.as_str())
            .bind(event.kind.as_str())
            .bind(micros(event.at))
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected() == 1)
    }

    async fn window_events(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<WindowEvent>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = match &before {
            None => Self::query(&self.sql.window_events),
            Some(_) => Self::query(&self.sql.window_events_before),
        }
        .bind(key.phone_number_id.as_str())
        .bind(key.contact.as_str())
        .bind(to_i64(limit));
        let query = match &before {
            None => query,
            Some((at, id)) => query.bind(micros(*at)).bind(id.as_str()),
        };
        let rows = query.fetch_all(&self.pool).await.map_err(backend)?;
        rows.iter().map(window_event_from_row).collect()
    }

    async fn set_thread_owner(
        &self,
        key: &ConversationKey,
        ownership: ThreadOwnership,
    ) -> Result<bool, StorageError> {
        key_identifiers(key)?;
        let done = Self::query(&self.sql.set_owner)
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .bind(owner_str(ownership.owner)?)
            .bind(ownership.role)
            .bind(ownership.app_id.as_ref().map(AppId::as_str))
            .bind(micros(ownership.since))
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected() == 1)
    }

    async fn thread_owner(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<ThreadOwnership>, StorageError> {
        let row = Self::query(&self.sql.owner)
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(owner_from_row).transpose()
    }

    async fn put_contact(&self, contact: StoredContact) -> Result<bool, StorageError> {
        key_identifiers(&contact.key)?;
        for (what, value) in [
            ("phone number", contact.phone_number.as_deref()),
            ("user id", contact.user_id.as_ref().map(UserId::as_str)),
            (
                "parent user id",
                contact.parent_user_id.as_ref().map(UserId::as_str),
            ),
        ] {
            if let Some(value) = value {
                identifier(what, value)?;
            }
        }
        let done = Self::query(&self.sql.put_contact)
            .bind(contact.key.phone_number_id.as_str())
            .bind(contact.key.contact.as_str())
            .bind(contact.full_name.as_deref().map(str::as_bytes))
            .bind(contact.first_name.as_deref().map(str::as_bytes))
            .bind(contact.phone_number.as_deref())
            .bind(contact.user_id.as_ref().map(UserId::as_str))
            .bind(contact.parent_user_id.as_ref().map(UserId::as_str))
            .bind(contact.username.as_deref().map(str::as_bytes))
            .bind(micros(contact.synced_at))
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected() == 1)
    }

    async fn remove_contact(
        &self,
        key: &ConversationKey,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError> {
        key_identifiers(key)?;
        let mut tx = self.begin().await?;
        let stored = Self::query(&self.sql.contact_state)
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
        let removed = match stored {
            Some(row) => {
                let synced_at: i64 = row.try_get("synced_at").map_err(backend)?;
                if synced_at > micros(at) {
                    return Ok(false);
                }
                let was_removed: bool = row.try_get("removed").map_err(backend)?;
                !was_removed
            }
            None => false,
        };
        Self::query(&self.sql.remove_contact)
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .bind(micros(at))
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        Ok(removed)
    }

    async fn contact(&self, key: &ConversationKey) -> Result<Option<StoredContact>, StorageError> {
        let row = Self::query(&self.sql.contact)
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(contact_from_row).transpose()
    }

    async fn contacts(
        &self,
        phone_number_id: &PhoneNumberId,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<StoredContact>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = match &after {
            None => Self::query(&self.sql.contacts),
            Some(_) => Self::query(&self.sql.contacts_after),
        }
        .bind(phone_number_id.as_str())
        .bind(to_i64(limit));
        let query = match after {
            None => query,
            Some(contact) => query.bind(contact),
        };
        let rows = query.fetch_all(&self.pool).await.map_err(backend)?;
        rows.iter().map(contact_from_row).collect()
    }

    async fn link_identity(&self, link: IdentityLink) -> Result<bool, StorageError> {
        identifier("phone number id", link.phone_number_id.as_str())?;
        identifier("identity", &link.previous)?;
        identifier("identity", &link.current)?;
        let done = Self::query(&self.sql.link_identity)
            .bind(link.phone_number_id.as_str())
            .bind(link.previous)
            .bind(link.current)
            .bind(micros(link.at))
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected() == 1)
    }

    async fn identity_links(
        &self,
        key: &ConversationKey,
    ) -> Result<Vec<IdentityLink>, StorageError> {
        let rows = Self::query(&self.sql.identity_links)
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        rows.iter().map(link_from_row).collect()
    }

    async fn identities(&self, key: &ConversationKey) -> Result<BTreeSet<String>, StorageError> {
        // One read transaction: every step reads the same snapshot.
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let number = &key.phone_number_id;
        let mut found = BTreeSet::from([key.contact.clone()]);
        let mut todo = vec![key.contact.clone()];
        while let Some(id) = todo.pop() {
            if id.is_empty() {
                // An empty id names no one: never followed, nor found.
                continue;
            }
            let mut next: Vec<String> = Vec::new();
            let contacts = Self::query(&self.sql.naming_contacts)
                .bind(number.as_str())
                .bind(id.as_str())
                .fetch_all(&mut *tx)
                .await
                .map_err(backend)?;
            for row in contacts {
                for column in ["contact", "user_id", "parent_user_id", "phone_number"] {
                    let value: Option<String> = row.try_get(column).map_err(backend)?;
                    next.extend(value);
                }
            }
            let links = Self::query(&self.sql.identity_links)
                .bind(number.as_str())
                .bind(id.as_str())
                .fetch_all(&mut *tx)
                .await
                .map_err(backend)?;
            for row in &links {
                let link = link_from_row(row)?;
                next.push(link.previous);
                next.push(link.current);
            }
            for other in next {
                if !other.is_empty() && found.insert(other.clone()) {
                    todo.push(other);
                }
            }
        }
        tx.commit().await.map_err(backend)?;
        Ok(found)
    }

    async fn erase_all(
        &self,
        phone_number_id: &PhoneNumberId,
        contacts: &[String],
    ) -> Result<Erased, StorageError> {
        let ids: BTreeSet<&str> = contacts.iter().map(String::as_str).collect();
        let mut erased = Erased::default();
        if ids.is_empty() {
            return Ok(erased);
        }
        let mut tx = self.begin().await?;
        let sql = &self.sql;
        for id in &ids {
            erased.messages +=
                Self::run2(&mut tx, &sql.erase_messages, phone_number_id, id).await?;
            erased.conversations +=
                Self::run2(&mut tx, &sql.erase_conversation, phone_number_id, id).await?;
            erased.window_events +=
                Self::run2(&mut tx, &sql.erase_window_events, phone_number_id, id).await?;
            erased.thread_owners +=
                Self::run2(&mut tx, &sql.erase_owner, phone_number_id, id).await?;
            erased.contacts +=
                Self::run2(&mut tx, &sql.erase_keyed_contact, phone_number_id, id).await?;
        }
        // An empty id names no one: it matches what is keyed by it, never a
        // contact's other ids, a link's sides or a sender.
        let mut touched = BTreeSet::new();
        for id in ids.iter().filter(|id| !id.is_empty()) {
            erased.contacts +=
                Self::run2(&mut tx, &sql.erase_naming_contacts, phone_number_id, id).await?;
            erased.identity_links +=
                Self::run2(&mut tx, &sql.erase_links, phone_number_id, id).await?;
            // Their own conversations went above: what is left of theirs
            // is in conversations keyed by someone else.
            erased.group_messages += self
                .erase_sent_by(&mut tx, phone_number_id, id, &mut touched)
                .await?;
        }
        for contact in &touched {
            self.resummarize(&mut tx, phone_number_id, contact).await?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(erased)
    }

    fn erasure_mode(&self) -> ErasureMode {
        self.erasure_mode
    }

    async fn purge_before(
        &self,
        phone_number_id: Option<&PhoneNumberId>,
        cutoff: OffsetDateTime,
    ) -> Result<Purged, StorageError> {
        let mut tx = self.begin().await?;
        let mut counts = [0_u64; 5];
        for (sql, count) in self.sql.purge.iter().zip(&mut counts) {
            let done = Self::query(sql)
                .bind(micros(cutoff))
                .bind(phone_number_id.map(PhoneNumberId::as_str))
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            *count = done.rows_affected();
        }
        tx.commit().await.map_err(backend)?;
        let [
            messages,
            conversations,
            window_events,
            thread_owners,
            contact_removals,
        ] = counts;
        Ok(Purged {
            messages,
            conversations,
            window_events,
            thread_owners,
            contact_removals,
        })
    }

    fn retention(&self) -> Retention {
        self.retention
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::conversation_conformance;
    use crate::store::sqlite::tests_support::TempDb;
    use crate::store::sqlite::{memory, migrate, migrate_with_prefix, open};

    async fn store() -> SqliteConversationStore {
        let pool = memory().await.unwrap();
        migrate(&pool).await.unwrap();
        SqliteConversationStore::new(pool)
    }

    #[tokio::test]
    async fn passes_conversation_conformance_suite() {
        let store = store().await;
        assert_eq!(store.erasure_mode(), ErasureMode::Redact);
        conversation_conformance::run(&store).await;
    }

    /// The suite's retention case takes the other branch on a store that
    /// keeps a retention.
    #[tokio::test]
    async fn passes_conversation_conformance_suite_with_a_retention() {
        conversation_conformance::run(&store().await.with_retention(Retention::days(30))).await;
    }

    /// The suite's group erasure case takes the other branch on a store
    /// that deletes an erased person's group messages.
    #[tokio::test]
    async fn passes_conversation_conformance_suite_deleting_group_messages() {
        let store = store().await.with_erasure_mode(ErasureMode::Delete);
        assert_eq!(store.erasure_mode(), ErasureMode::Delete);
        conversation_conformance::run(&store).await;
    }

    /// A file shared by a pool of several connections, under another
    /// prefix: the suite's concurrency cases hold with writers taking the
    /// write lock in turn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn passes_conversation_conformance_suite_on_a_shared_file() {
        let file = TempDb::new("conversations");
        let pool = open(file.path()).await.unwrap();
        let prefix = TablePrefix::new("tenant_x_").unwrap();
        migrate_with_prefix(&pool, &prefix).await.unwrap();
        migrate_with_prefix(&pool, &prefix).await.unwrap(); // idempotent
        let store = SqliteConversationStore::with_prefix(pool.clone(), prefix.clone());
        assert_eq!(store.prefix(), &prefix);
        conversation_conformance::run(&store).await;
        let tables: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            tables.iter().all(|t| t.starts_with("tenant_x_")),
            "{tables:?}"
        );
    }

    /// Roadmap L8's decisive case, beyond the suite (which allows either
    /// answer): ids are stored once per store, so a second `append` of one
    /// id on another number returns `false` and stores nothing there.
    #[tokio::test]
    async fn a_second_append_of_one_id_on_another_number_is_refused() {
        conversation_conformance::ids_are_stored_once_per_store(&store().await).await;
    }

    /// Every `table.column` of the store's tables holding one of
    /// `needles`' bytes, as text or blob.
    async fn traces(store: &SqliteConversationStore, needles: &[String]) -> Vec<String> {
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND substr(name, 1, length(?1)) = ?1 AND name <> ?2",
        )
        .bind(store.prefix().as_str())
        .bind(store.prefix().table("sqlx_migrations"))
        .fetch_all(&store.pool)
        .await
        .unwrap();
        assert!(tables.len() >= 6, "{tables:?}");
        let mut found = Vec::new();
        for table in tables {
            let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info(?1)")
                .bind(&table)
                .fetch_all(&store.pool)
                .await
                .unwrap();
            for column in columns {
                for needle in needles {
                    let n: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
                        "SELECT count(*) FROM \"{table}\" \
                         WHERE instr(CAST(\"{column}\" AS BLOB), CAST(?1 AS BLOB)) > 0"
                    )))
                    .bind(needle.as_bytes())
                    .fetch_one(&store.pool)
                    .await
                    .unwrap();
                    if n > 0 {
                        found.push(format!("{table}.{column} holds {needle}"));
                    }
                }
            }
        }
        found
    }

    /// An erasure leaves nothing of the person in any table (a redacted
    /// group message's sender included), in both erasure modes; a purge
    /// leaves nothing of what it purged.
    #[tokio::test]
    async fn an_erasure_or_a_purge_leaves_no_trace_in_any_table() {
        for mode in [ErasureMode::Redact, ErasureMode::Delete] {
            let store = store().await.with_erasure_mode(mode);
            let needles = conversation_conformance::erase_one_person_of_every_record(&store).await;
            assert_eq!(
                traces(&store, &needles).await,
                Vec::<String>::new(),
                "{mode:?}"
            );
        }
        let store = store().await;
        let needles = conversation_conformance::purge_one_person_of_every_record(&store).await;
        assert_eq!(traces(&store, &needles).await, Vec::<String>::new());
    }

    /// Every write method refuses an identifier holding U+0000 and stores
    /// nothing, as on Postgres.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one case per write method and identifier
    async fn every_write_refuses_nul_in_an_identifier() {
        let store = store().await;
        let at = time::macros::datetime!(2026-09-24 12:00 UTC);
        let pn = PhoneNumberId::new("1").unwrap();
        let key = |c: &str| ConversationKey::new(pn.clone(), c);
        let refused = |r: Result<bool, StorageError>, what: &str| {
            assert!(matches!(r, Err(StorageError::Backend(_))), "{what}: {r:?}");
        };
        let event = |c: &str, id: &str| WindowEvent {
            conversation: key(c),
            kind: WindowEventKind::CustomerCall,
            id: id.to_owned(),
            at,
        };
        refused(
            store
                .record_window_event(event("US.\u{0}1", "wacid.1"))
                .await,
            "event contact",
        );
        refused(
            store.record_window_event(event("US.1", "wacid.\0")).await,
            "event id",
        );
        let owner = ThreadOwnership {
            owner: meta_whatsapp_core::store::ThreadOwner::ThisApp,
            role: None,
            app_id: None,
            since: at,
        };
        refused(
            store.set_thread_owner(&key("US.\u{0}1"), owner).await,
            "owner contact",
        );
        let contact = StoredContact {
            key: key("US.1"),
            full_name: Some("names keep \0".to_owned()),
            first_name: None,
            phone_number: None,
            user_id: None,
            parent_user_id: None,
            username: None,
            synced_at: at,
        };
        for (what, bad) in [
            (
                "contact key",
                StoredContact {
                    key: key("US.\u{0}1"),
                    ..contact.clone()
                },
            ),
            (
                "contact phone",
                StoredContact {
                    phone_number: Some("1\0".to_owned()),
                    ..contact.clone()
                },
            ),
            (
                "contact user id",
                StoredContact {
                    user_id: Some(UserId::new("US.\0")),
                    ..contact.clone()
                },
            ),
            (
                "contact parent user id",
                StoredContact {
                    parent_user_id: Some(UserId::new("US.ENT.\0")),
                    ..contact.clone()
                },
            ),
        ] {
            refused(store.put_contact(bad).await, what);
        }
        refused(
            store.remove_contact(&key("US.\u{0}1"), at).await,
            "removal key",
        );
        for (what, link) in [
            (
                "link previous",
                IdentityLink::new(pn.clone(), "1\0", "US.1", at),
            ),
            (
                "link current",
                IdentityLink::new(pn.clone(), "1", "US.\0", at),
            ),
        ] {
            refused(store.link_identity(link).await, what);
        }
        refused(
            store
                .revoke(
                    &key("US.1"),
                    &MessageId::new("wamid.\0"),
                    Direction::Inbound,
                    at,
                )
                .await,
            "revoke id",
        );
        let needles = ["US.", "wacid.", "wamid."].map(str::to_owned);
        assert_eq!(
            traces(&store, &needles).await,
            Vec::<String>::new(),
            "nothing stored"
        );
        // The content keeps it.
        assert!(store.put_contact(contact.clone()).await.unwrap());
        assert_eq!(store.contact(&contact.key).await.unwrap(), Some(contact));
    }

    #[tokio::test]
    async fn identifiers_with_nul_are_refused_and_content_keeps_it() {
        let store = store().await;
        let at = time::macros::datetime!(2026-09-24 12:00 UTC);
        let message = |id: &str, contact: &str| StoredMessage {
            id: MessageId::new(id),
            conversation: ConversationKey::new(PhoneNumberId::new("1").unwrap(), contact),
            direction: Direction::Inbound,
            kind: "text\0".to_owned(),
            text: Some("a\0b".to_owned()),
            payload: serde_json::json!({"a\0": "b\0"}),
            status: DeliveryStatus::Received,
            timestamp: at,
            status_at: None,
            error: None,
        };
        for bad in [message("wamid.\0", "US.1"), message("wamid.1", "US.\u{0}1")] {
            assert!(matches!(
                store.append(bad).await,
                Err(StorageError::Backend(_))
            ));
        }
        let good = message("wamid.1", "US.1");
        assert!(store.append(good.clone()).await.unwrap());
        assert_eq!(
            store
                .message(&good.conversation.phone_number_id, &good.id)
                .await
                .unwrap(),
            Some(good)
        );
    }

    #[tokio::test]
    async fn debug_shows_configuration_only() {
        assert_eq!(
            format!("{:?}", store().await),
            "SqliteConversationStore { prefix: TablePrefix(\"wa_\"), retention: Keep, \
             erasure_mode: Redact, .. }"
        );
    }
}
