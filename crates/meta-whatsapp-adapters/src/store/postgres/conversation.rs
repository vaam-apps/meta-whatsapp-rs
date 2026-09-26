//! [`ConversationStore`] on Postgres.
//!
//! - Content keeps U+0000: `kind`, `text` and the summary's preview are
//!   `BYTEA` (their UTF-8 bytes), `payload` and `error` are `json` bound as
//!   JSON text (see the [module docs](super#content-keeps-u0000)). Ids,
//!   contacts and phone number ids are `TEXT COLLATE "C"`, and they alone
//!   (with the timestamps) order and match rows; the only comparison on
//!   content is `fill_media_placeholder`'s, of `kind_utf8` with the
//!   placeholder's bytes.
//! - `append` is a single statement: the message insert (`ON CONFLICT (id)
//!   DO NOTHING`) feeds the inbox-summary upsert through a data-modifying
//!   CTE, so a duplicate id changes nothing and the summary can never
//!   disagree with the history, without a client-side transaction.
//! - `append_synced` is one statement per batch: the messages travel as
//!   one array per column (`UNNEST`), duplicates within the batch are
//!   dropped (first wins), the insert is `ON CONFLICT (id) DO NOTHING`, and
//!   each touched conversation's summary is upserted once from its newest
//!   inserted row, without the inbound effects (`last_inbound_at`,
//!   `unread`). Nothing about a message's origin is stored: the summary is
//!   maintained when a row is written, so synced history needs no column.
//! - `revoke` first tries to store the tombstone (a message insert alone,
//!   `ON CONFLICT (id) DO NOTHING`: a tombstone is never part of the
//!   summary, and the primary key serializes it against a concurrent append
//!   of the message), and only when the id is taken runs the status
//!   compare-and-swap below, restricted to the revoke's direction. A
//!   separate statement, so it sees a row a concurrent append committed.
//! - `fill_media_placeholder` is one statement too: the content update of
//!   a row whose `kind` is still the placeholder and whose status is not
//!   `deleted` (a revoked placeholder never gets its content), and the
//!   preview of its conversation when it is the latest message. Two
//!   concurrent fills cannot both apply: the second finds the row no longer
//!   a placeholder, and a concurrent revoke either comes first (the fill
//!   finds it deleted) or marks the filled row deleted.
//! - `update_status` (and `revoke`) applies [`DeliveryStatus::supersedes`] — the rule lives
//!   in `meta-whatsapp-core`, not re-encoded in SQL — as a compare-and-swap on the
//!   stored status: read it, decide in Rust, then `UPDATE … WHERE status =
//!   <what we read>`. A concurrent update makes the swap miss and we re-read.
//!   Every successful update strictly raises the status rank, so the loop is
//!   bounded by the number of ranks. Both statements match `id` *and*
//!   `phone_number_id`, so an event of one business number never changes a
//!   row of another.
//! - `message` (the lookup) matches `id` *and* `phone_number_id` too, so it
//!   finds a message whether ids are unique per store (the primary key
//!   today) or per number (`OPEN_QUESTIONS.md` #33), and never another
//!   number's.
//! - Window events, thread owners and synced contacts live in tables of
//!   their own (migration `0004`), keyed by business number and contact
//!   (a window event by business number and id). `set_thread_owner` and
//!   `put_contact` are one upsert each whose update only applies when the
//!   stored record is not later (`ON CONFLICT … DO UPDATE … WHERE`), so
//!   the rule holds under concurrency without a read.
//! - `erase` is one statement: five `DELETE`s in data-modifying CTEs, over
//!   one snapshot, so it deletes every record of the key committed before
//!   it started, or nothing. A message a concurrent append commits while
//!   it runs may survive it (as one recorded just after it would), and its
//!   summary with it or not.
//! - `purge_before` is one statement the same way; `wa_messages(ts)` and
//!   `wa_conversations(last_message_at)` are indexed for it (migration
//!   `0004`).

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::ids::{AppId, MessageId, PhoneNumberId, UserId};
use meta_whatsapp_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, DeliveryStatus, Direction, Erased,
    Purged, Retention, StoredContact, StoredMessage, ThreadOwner, ThreadOwnership, WindowEvent,
    WindowEventKind,
};
use sqlx::postgres::PgRow;
use sqlx::{AssertSqlSafe, PgPool, Row};
use time::OffsetDateTime;

use super::{TablePrefix, backend, to_i64};

/// More compare-and-swap rounds than there are status ranks: reaching it
/// means something outside this adapter keeps rewriting the row.
const MAX_STATUS_ROUNDS: usize = 16;

/// `ConversationStore` over a Postgres pool. Cheap to clone.
///
/// Unread counts are by **arrival** (inbound messages appended since the
/// last `mark_read`; synced history never counts), like the in-memory
/// store. Timestamps are stored with microsecond precision.
///
/// Retention is kept by default; [`with_retention`](Self::with_retention)
/// sets what [`ConversationStore::apply_retention`] purges. Nothing purges
/// on its own: schedule `apply_retention` (one replica at a time is
/// enough; concurrent runs are safe).
#[derive(Clone)]
pub struct PostgresConversationStore {
    pool: PgPool,
    prefix: TablePrefix,
    sql: Arc<Sql>,
    retention: Retention,
}

impl fmt::Debug for PostgresConversationStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresConversationStore")
            .field("prefix", &self.prefix)
            .field("retention", &self.retention)
            .finish_non_exhaustive()
    }
}

struct Sql {
    append: Arc<str>,
    append_synced: Arc<str>,
    tombstone: Arc<str>,
    fill_placeholder: Arc<str>,
    status_get: Arc<str>,
    status_swap: Arc<str>,
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
    remove_contact: Arc<str>,
    contact: Arc<str>,
    contacts: Arc<str>,
    contacts_after: Arc<str>,
    erase: Arc<str>,
    purge_all: Arc<str>,
    purge_number: Arc<str>,
}

const MESSAGE_COLUMNS: &str = "id, phone_number_id, contact, direction, kind_utf8, text_utf8, \
     payload_json, status, ts, status_at, error_json";

const SUMMARY_COLUMNS: &str =
    "phone_number_id, contact, last_message_at, last_inbound_at, last_text_utf8, unread";

const WINDOW_EVENT_COLUMNS: &str = "phone_number_id, id, contact, kind, ts";

const OWNER_COLUMNS: &str = "owner, role, app_id, since";

const CONTACT_COLUMNS: &str = "phone_number_id, contact, full_name_utf8, first_name_utf8, \
     phone_number, user_id, parent_user_id, username_utf8, synced_at";

/// The eleven values of a message row, in [`MESSAGE_COLUMNS`] order. The
/// content travels as bytes (`kind_utf8`, `text_utf8`: `BYTEA`) and as
/// JSON text cast to `json` (`payload_json`, `error_json`): neither Postgres
/// `text` nor `jsonb` can hold U+0000, and content keeps it (see the
/// [module docs](super#content-keeps-u0000)).
const MESSAGE_VALUES: &str = "$1, $2, $3, $4, $5, $6, $7::json, $8, $9, $10, $11::json";

/// The summary's "newest message" is the max by `(ts, id)`, the same order
/// the history pages in.
const NEWER: &str = "(EXCLUDED.last_message_at, EXCLUDED.last_message_id) \
                     > (c.last_message_at, c.last_message_id)";

/// The `append` statement. `inbound_at` and `unread` are the SQL
/// expressions an inserted row contributes to its conversation's
/// `last_inbound_at` and `unread`.
fn append_sql(messages: &str, conversations: &str, inbound_at: &str, unread: &str) -> String {
    let newer = NEWER;
    format!(
        "WITH inserted AS ( \
           INSERT INTO {messages} ({MESSAGE_COLUMNS}) \
           VALUES ({MESSAGE_VALUES}) \
           ON CONFLICT (id) DO NOTHING \
           RETURNING id, phone_number_id, contact, direction, text_utf8, ts \
         ) \
         INSERT INTO {conversations} AS c \
           (phone_number_id, contact, last_message_at, last_message_id, last_text_utf8, \
            last_inbound_at, unread) \
         SELECT phone_number_id, contact, ts, id, text_utf8, {inbound_at}, {unread} \
         FROM inserted \
         ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
           last_message_at = CASE WHEN {newer} THEN EXCLUDED.last_message_at ELSE c.last_message_at END, \
           last_message_id = CASE WHEN {newer} THEN EXCLUDED.last_message_id ELSE c.last_message_id END, \
           last_text_utf8 = CASE WHEN {newer} THEN EXCLUDED.last_text_utf8 ELSE c.last_text_utf8 END, \
           last_inbound_at = GREATEST(c.last_inbound_at, EXCLUDED.last_inbound_at), \
           unread = c.unread + EXCLUDED.unread \
         RETURNING 1 AS appended"
    )
}

/// The `purge_before` statement. `scope` restricts every `DELETE` to one
/// business number (`$2`), or is empty.
fn purge_sql(prefix: &TablePrefix, scope: &str) -> String {
    let messages = prefix.table("messages");
    let conversations = prefix.table("conversations");
    let window_events = prefix.table("window_events");
    let owners = prefix.table("thread_owners");
    format!(
        "WITH m AS (DELETE FROM {messages} WHERE ts < $1{scope} RETURNING 1), \
         c AS (DELETE FROM {conversations} WHERE last_message_at < $1{scope} RETURNING 1), \
         w AS (DELETE FROM {window_events} WHERE ts < $1{scope} RETURNING 1), \
         o AS (DELETE FROM {owners} WHERE since < $1{scope} RETURNING 1) \
         SELECT (SELECT count(*) FROM m), (SELECT count(*) FROM c), \
           (SELECT count(*) FROM w), (SELECT count(*) FROM o)"
    )
}

/// The `append_synced` statement: one row per element of the eleven
/// column arrays, the first of each id kept, no inbound effects.
fn append_synced_sql(messages: &str, conversations: &str) -> String {
    let newer = NEWER;
    format!(
        "WITH input AS ( \
           SELECT DISTINCT ON (id) * FROM UNNEST($1::text[], $2::text[], $3::text[], \
             $4::text[], $5::bytea[], $6::bytea[], $7::json[], $8::text[], \
             $9::timestamptz[], $10::timestamptz[], $11::json[]) \
             WITH ORDINALITY AS t({MESSAGE_COLUMNS}, n) \
           ORDER BY id, n \
         ), inserted AS ( \
           INSERT INTO {messages} ({MESSAGE_COLUMNS}) \
           SELECT {MESSAGE_COLUMNS} FROM input \
           ON CONFLICT (id) DO NOTHING \
           RETURNING id, phone_number_id, contact, text_utf8, ts \
         ), latest AS ( \
           SELECT DISTINCT ON (phone_number_id, contact) phone_number_id, contact, ts, id, text_utf8 \
           FROM inserted \
           ORDER BY phone_number_id, contact, ts DESC, id COLLATE \"C\" DESC \
         ), summary AS ( \
           INSERT INTO {conversations} AS c \
             (phone_number_id, contact, last_message_at, last_message_id, last_text_utf8, \
              last_inbound_at, unread) \
           SELECT phone_number_id, contact, ts, id, text_utf8, NULL, 0 FROM latest \
           ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
             last_message_at = CASE WHEN {newer} THEN EXCLUDED.last_message_at ELSE c.last_message_at END, \
             last_message_id = CASE WHEN {newer} THEN EXCLUDED.last_message_id ELSE c.last_message_id END, \
             last_text_utf8 = CASE WHEN {newer} THEN EXCLUDED.last_text_utf8 ELSE c.last_text_utf8 END \
         ) \
         SELECT id FROM inserted"
    )
}

impl Sql {
    #[allow(clippy::too_many_lines)] // one statement per field
    fn new(prefix: &TablePrefix) -> Self {
        let messages = prefix.table("messages");
        let conversations = prefix.table("conversations");
        let window_events = prefix.table("window_events");
        let owners = prefix.table("thread_owners");
        let contacts = prefix.table("synced_contacts");
        let arc = |s: String| -> Arc<str> { Arc::from(s) };
        Self {
            append: arc(append_sql(
                &messages,
                &conversations,
                "CASE WHEN direction = 'inbound' THEN ts END",
                "CASE WHEN direction = 'inbound' THEN 1 ELSE 0 END",
            )),
            append_synced: arc(append_synced_sql(&messages, &conversations)),
            // The history row alone: a tombstone never touches (nor creates)
            // its conversation's summary.
            tombstone: arc(format!(
                "INSERT INTO {messages} ({MESSAGE_COLUMNS}) \
                 VALUES ({MESSAGE_VALUES}) \
                 ON CONFLICT (id) DO NOTHING \
                 RETURNING 1 AS appended"
            )),
            // Data-modifying CTEs always run to completion, whether or not
            // the final SELECT reads them. `$6`: the placeholder kind, as
            // bytes (`kind_utf8` is compared byte for byte). `$7`: the
            // `deleted` status, which a placeholder is never filled in.
            fill_placeholder: arc(format!(
                "WITH filled AS ( \
                   UPDATE {messages} SET kind_utf8 = $3, text_utf8 = $4, payload_json = $5::json \
                   WHERE id = $1 AND phone_number_id = $2 AND kind_utf8 = $6 AND status <> $7 \
                   RETURNING id, phone_number_id, contact, text_utf8 \
                 ), preview AS ( \
                   UPDATE {conversations} AS c SET last_text_utf8 = f.text_utf8 \
                   FROM filled f \
                   WHERE c.phone_number_id = f.phone_number_id AND c.contact = f.contact \
                     AND c.last_message_id = f.id \
                 ) \
                 SELECT count(*) FROM filled"
            )),
            // `$3` (`$7`): the direction a revoke is restricted to, NULL for
            // a status update.
            status_get: arc(format!(
                "SELECT status FROM {messages} \
                 WHERE id = $1 AND phone_number_id = $2 AND ($3::text IS NULL OR direction = $3)"
            )),
            status_swap: arc(format!(
                "UPDATE {messages} SET status = $4, status_at = $5, \
                   error_json = COALESCE($6::json, error_json) \
                 WHERE id = $1 AND phone_number_id = $2 AND status = $3 \
                   AND ($7::text IS NULL OR direction = $7)"
            )),
            messages: arc(format!(
                "SELECT {MESSAGE_COLUMNS} FROM {messages} \
                 WHERE phone_number_id = $1 AND contact = $2 \
                 ORDER BY ts DESC, id DESC LIMIT $3"
            )),
            messages_before: arc(format!(
                "SELECT {MESSAGE_COLUMNS} FROM {messages} \
                 WHERE phone_number_id = $1 AND contact = $2 AND (ts, id) < ($4, $5) \
                 ORDER BY ts DESC, id DESC LIMIT $3"
            )),
            conversations: arc(format!(
                "SELECT {SUMMARY_COLUMNS} FROM {conversations} \
                 WHERE phone_number_id = $1 \
                 ORDER BY last_message_at DESC, contact DESC LIMIT $2"
            )),
            conversations_before: arc(format!(
                "SELECT {SUMMARY_COLUMNS} FROM {conversations} \
                 WHERE phone_number_id = $1 AND (last_message_at, contact) < ($3, $4) \
                 ORDER BY last_message_at DESC, contact DESC LIMIT $2"
            )),
            mark_read: arc(format!(
                "UPDATE {conversations} SET unread = 0 WHERE phone_number_id = $1 AND contact = $2"
            )),
            last_inbound_at: arc(format!(
                "SELECT last_inbound_at FROM {conversations} \
                 WHERE phone_number_id = $1 AND contact = $2"
            )),
            message: arc(format!(
                "SELECT {MESSAGE_COLUMNS} FROM {messages} WHERE id = $1 AND phone_number_id = $2"
            )),
            window_event: arc(format!(
                "INSERT INTO {window_events} ({WINDOW_EVENT_COLUMNS}) VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (phone_number_id, id) DO NOTHING \
                 RETURNING 1 AS recorded"
            )),
            window_events: arc(format!(
                "SELECT {WINDOW_EVENT_COLUMNS} FROM {window_events} \
                 WHERE phone_number_id = $1 AND contact = $2 \
                 ORDER BY ts DESC, id DESC LIMIT $3"
            )),
            window_events_before: arc(format!(
                "SELECT {WINDOW_EVENT_COLUMNS} FROM {window_events} \
                 WHERE phone_number_id = $1 AND contact = $2 AND (ts, id) < ($4, $5) \
                 ORDER BY ts DESC, id DESC LIMIT $3"
            )),
            // The update applies only when the stored record is not later.
            set_owner: arc(format!(
                "INSERT INTO {owners} AS o (phone_number_id, contact, {OWNER_COLUMNS}) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
                   owner = EXCLUDED.owner, role = EXCLUDED.role, app_id = EXCLUDED.app_id, \
                   since = EXCLUDED.since \
                 WHERE EXCLUDED.since >= o.since \
                 RETURNING 1 AS stored"
            )),
            owner: arc(format!(
                "SELECT {OWNER_COLUMNS} FROM {owners} WHERE phone_number_id = $1 AND contact = $2"
            )),
            put_contact: arc(format!(
                "INSERT INTO {contacts} AS p ({CONTACT_COLUMNS}) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
                 ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
                   full_name_utf8 = EXCLUDED.full_name_utf8, \
                   first_name_utf8 = EXCLUDED.first_name_utf8, \
                   phone_number = EXCLUDED.phone_number, user_id = EXCLUDED.user_id, \
                   parent_user_id = EXCLUDED.parent_user_id, \
                   username_utf8 = EXCLUDED.username_utf8, synced_at = EXCLUDED.synced_at \
                 WHERE EXCLUDED.synced_at >= p.synced_at \
                 RETURNING 1 AS stored"
            )),
            remove_contact: arc(format!(
                "DELETE FROM {contacts} \
                 WHERE phone_number_id = $1 AND contact = $2 AND synced_at <= $3"
            )),
            contact: arc(format!(
                "SELECT {CONTACT_COLUMNS} FROM {contacts} \
                 WHERE phone_number_id = $1 AND contact = $2"
            )),
            contacts: arc(format!(
                "SELECT {CONTACT_COLUMNS} FROM {contacts} \
                 WHERE phone_number_id = $1 ORDER BY contact LIMIT $2"
            )),
            contacts_after: arc(format!(
                "SELECT {CONTACT_COLUMNS} FROM {contacts} \
                 WHERE phone_number_id = $1 AND contact > $3 ORDER BY contact LIMIT $2"
            )),
            // One snapshot for the five tables: all of it, or nothing.
            erase: arc(format!(
                "WITH m AS (DELETE FROM {messages} \
                   WHERE phone_number_id = $1 AND contact = $2 RETURNING 1), \
                 c AS (DELETE FROM {conversations} \
                   WHERE phone_number_id = $1 AND contact = $2 RETURNING 1), \
                 w AS (DELETE FROM {window_events} \
                   WHERE phone_number_id = $1 AND contact = $2 RETURNING 1), \
                 o AS (DELETE FROM {owners} \
                   WHERE phone_number_id = $1 AND contact = $2 RETURNING 1), \
                 p AS (DELETE FROM {contacts} \
                   WHERE phone_number_id = $1 \
                     AND $2 IN (contact, user_id, parent_user_id, phone_number) RETURNING 1) \
                 SELECT (SELECT count(*) FROM m), (SELECT count(*) FROM c), \
                   (SELECT count(*) FROM w), (SELECT count(*) FROM o), (SELECT count(*) FROM p)"
            )),
            purge_all: arc(purge_sql(prefix, "")),
            purge_number: arc(purge_sql(prefix, " AND phone_number_id = $2")),
        }
    }
}

impl PostgresConversationStore {
    /// Store on `pool` with the default `wa_` tables. Run
    /// [`migrate`](super::migrate) first.
    pub fn new(pool: PgPool) -> Self {
        Self::with_prefix(pool, TablePrefix::DEFAULT)
    }

    /// Store on `pool` using `prefix`'s tables. Run
    /// [`migrate_with_prefix`](super::migrate_with_prefix) with the same
    /// prefix first.
    pub fn with_prefix(pool: PgPool, prefix: TablePrefix) -> Self {
        let sql = Arc::new(Sql::new(&prefix));
        Self {
            pool,
            prefix,
            sql,
            retention: Retention::Keep,
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

    /// The table prefix in use.
    pub fn prefix(&self) -> &TablePrefix {
        &self.prefix
    }
}

fn direction_str(d: Direction) -> &'static str {
    match d {
        Direction::Inbound => "inbound",
        Direction::Outbound => "outbound",
    }
}

fn parse_direction(s: &str) -> Result<Direction, StorageError> {
    match s {
        "inbound" => Ok(Direction::Inbound),
        "outbound" => Ok(Direction::Outbound),
        other => Err(StorageError::Backend(anyhow::anyhow!(
            "unknown direction `{other}` in the messages table"
        ))),
    }
}

/// `DeliveryStatus` ↔ its serde name, so the column holds exactly what the
/// type serializes to (and a variant added to `meta-whatsapp-core` needs no change here).
fn status_str(status: DeliveryStatus) -> Result<String, StorageError> {
    match serde_json::to_value(status) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        Ok(other) => Err(StorageError::Backend(anyhow::anyhow!(
            "DeliveryStatus serialized to {other}, expected a string"
        ))),
        Err(source) => Err(StorageError::Corrupt {
            key: "status".to_owned(),
            source,
        }),
    }
}

/// A stored status. The error names no message id (see `set_status`).
fn parse_status(s: String) -> Result<DeliveryStatus, StorageError> {
    serde_json::from_value(serde_json::Value::String(s)).map_err(|source| StorageError::Corrupt {
        key: "message status".to_owned(),
        source,
    })
}

/// `ThreadOwner` ↔ its serde name, as for [`status_str`].
fn owner_str(owner: ThreadOwner) -> Result<String, StorageError> {
    match serde_json::to_value(owner) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        Ok(other) => Err(StorageError::Backend(anyhow::anyhow!(
            "ThreadOwner serialized to {other}, expected a string"
        ))),
        Err(source) => Err(StorageError::Corrupt {
            key: "thread owner".to_owned(),
            source,
        }),
    }
}

/// A stored thread owner. The error names no conversation.
fn parse_owner(s: String) -> Result<ThreadOwner, StorageError> {
    serde_json::from_value(serde_json::Value::String(s)).map_err(|source| StorageError::Corrupt {
        key: "thread owner".to_owned(),
        source,
    })
}

/// Content as the text a `json` column stores. `serde_json` writes U+0000
/// as the escape `\u0000`, which `json` keeps as written (`jsonb` refuses
/// it). `what` names the field in an error, never the message.
fn json_text(value: &serde_json::Value, what: &str) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|source| StorageError::Corrupt {
        key: what.to_owned(),
        source,
    })
}

/// A `BYTEA` content column back to its text. Our writes are always UTF-8;
/// bytes that are not (a row edited by hand) are
/// [`StorageError::Corrupt`], never replaced. The error names the column,
/// never the content.
fn utf8(column: &str, bytes: Vec<u8>) -> Result<String, StorageError> {
    String::from_utf8(bytes).map_err(|e| StorageError::Corrupt {
        key: column.to_owned(),
        source: serde::de::Error::custom(format_args!(
            "not UTF-8 (valid up to byte {})",
            e.utf8_error().valid_up_to()
        )),
    })
}

fn message_from_row(row: &PgRow) -> Result<StoredMessage, StorageError> {
    let id: String = row.try_get("id").map_err(backend)?;
    let direction: String = row.try_get("direction").map_err(backend)?;
    let status: String = row.try_get("status").map_err(backend)?;
    let status = parse_status(status)?;
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let kind: Vec<u8> = row.try_get("kind_utf8").map_err(backend)?;
    let text: Option<Vec<u8>> = row.try_get("text_utf8").map_err(backend)?;
    Ok(StoredMessage {
        conversation: ConversationKey::new(phone_number_id, contact),
        direction: parse_direction(&direction)?,
        kind: utf8("kind_utf8", kind)?,
        text: text.map(|t| utf8("text_utf8", t)).transpose()?,
        payload: row.try_get("payload_json").map_err(backend)?,
        status,
        timestamp: row.try_get("ts").map_err(backend)?,
        status_at: row.try_get("status_at").map_err(backend)?,
        error: row.try_get("error_json").map_err(backend)?,
        id: MessageId::new(id),
    })
}

fn window_event_from_row(row: &PgRow) -> Result<WindowEvent, StorageError> {
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let kind: String = row.try_get("kind").map_err(backend)?;
    Ok(WindowEvent {
        conversation: ConversationKey::new(phone_number_id, contact),
        kind: WindowEventKind::from_name(&kind),
        id: row.try_get("id").map_err(backend)?,
        at: row.try_get("ts").map_err(backend)?,
    })
}

fn owner_from_row(row: &PgRow) -> Result<ThreadOwnership, StorageError> {
    let owner: String = row.try_get("owner").map_err(backend)?;
    let app_id: Option<String> = row.try_get("app_id").map_err(backend)?;
    Ok(ThreadOwnership {
        owner: parse_owner(owner)?,
        role: row.try_get("role").map_err(backend)?,
        app_id: app_id.map(AppId::new),
        since: row.try_get("since").map_err(backend)?,
    })
}

/// An optional content column (`*_utf8`) back to its text.
fn optional_utf8(row: &PgRow, column: &str) -> Result<Option<String>, StorageError> {
    let bytes: Option<Vec<u8>> = row.try_get(column).map_err(backend)?;
    bytes.map(|b| utf8(column, b)).transpose()
}

fn contact_from_row(row: &PgRow) -> Result<StoredContact, StorageError> {
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let user_id: Option<String> = row.try_get("user_id").map_err(backend)?;
    let parent_user_id: Option<String> = row.try_get("parent_user_id").map_err(backend)?;
    Ok(StoredContact {
        key: ConversationKey::new(phone_number_id, contact),
        full_name: optional_utf8(row, "full_name_utf8")?,
        first_name: optional_utf8(row, "first_name_utf8")?,
        phone_number: row.try_get("phone_number").map_err(backend)?,
        user_id: user_id.map(UserId::new),
        parent_user_id: parent_user_id.map(UserId::new),
        username: optional_utf8(row, "username_utf8")?,
        synced_at: row.try_get("synced_at").map_err(backend)?,
    })
}

/// A `count(*)` column as a `u64`: never negative.
fn count(row: &PgRow, index: usize) -> Result<u64, StorageError> {
    let n: i64 = row.try_get(index).map_err(backend)?;
    Ok(u64::try_from(n).unwrap_or(0))
}

fn summary_from_row(row: &PgRow) -> Result<ConversationSummary, StorageError> {
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let unread: i64 = row.try_get("unread").map_err(backend)?;
    let last_text: Option<Vec<u8>> = row.try_get("last_text_utf8").map_err(backend)?;
    Ok(ConversationSummary {
        key: ConversationKey::new(phone_number_id, contact),
        last_message_at: row.try_get("last_message_at").map_err(backend)?,
        last_inbound_at: row.try_get("last_inbound_at").map_err(backend)?,
        last_text: last_text.map(|t| utf8("last_text_utf8", t)).transpose()?,
        // Never negative: it only grows by 1 and resets to 0.
        unread: u64::try_from(unread).unwrap_or(0),
    })
}

impl PostgresConversationStore {
    /// Run an append statement (`sql`) for `message`.
    async fn insert(&self, sql: &Arc<str>, message: &StoredMessage) -> Result<bool, StorageError> {
        let status = status_str(message.status)?;
        let payload = json_text(&message.payload, "message payload")?;
        let error = message
            .error
            .as_ref()
            .map(|e| json_text(e, "message error"))
            .transpose()?;
        let row = sqlx::query(AssertSqlSafe(Arc::clone(sql)))
            .bind(message.id.as_str())
            .bind(message.conversation.phone_number_id.as_str())
            .bind(message.conversation.contact.as_str())
            .bind(direction_str(message.direction))
            .bind(message.kind.as_bytes())
            .bind(message.text.as_deref().map(str::as_bytes))
            .bind(payload)
            .bind(status)
            .bind(message.timestamp)
            .bind(message.status_at)
            .bind(error)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        Ok(row.is_some())
    }

    /// Move message `id` of `phone_number_id` (in `direction` only, when
    /// given) to `status` if that supersedes the stored one: see the module
    /// docs.
    async fn set_status(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        direction: Option<Direction>,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<&serde_json::Value>,
    ) -> Result<bool, StorageError> {
        let new = status_str(status)?;
        let error = error.map(|e| json_text(e, "message error")).transpose()?;
        let direction = direction.map(direction_str);
        for _ in 0..MAX_STATUS_ROUNDS {
            let current: Option<String> =
                sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.status_get)))
                    .bind(id.as_str())
                    .bind(phone_number_id.as_str())
                    .bind(direction)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(backend)?;
            let Some(current) = current else {
                // A status for a message sent elsewhere, for another
                // business number's message, or a revoke of the other
                // direction.
                return Ok(false);
            };
            if !status.supersedes(parse_status(current.clone())?) {
                return Ok(false);
            }
            let done = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.status_swap)))
                .bind(id.as_str())
                .bind(phone_number_id.as_str())
                .bind(&current)
                .bind(&new)
                .bind(at)
                .bind(error.as_deref())
                .bind(direction)
                .execute(&self.pool)
                .await
                .map_err(backend)?;
            if done.rows_affected() == 1 {
                return Ok(true);
            }
            // Someone else moved the status between our read and our swap;
            // decide again against what they wrote.
        }
        // No message id: it is a Meta id derived from the customer's phone
        // number, and this error is logged.
        Err(StorageError::Backend(anyhow::anyhow!(
            "a message status kept changing; gave up after {MAX_STATUS_ROUNDS} rounds"
        )))
    }
}

#[async_trait]
impl ConversationStore for PostgresConversationStore {
    async fn append(&self, message: StoredMessage) -> Result<bool, StorageError> {
        self.insert(&self.sql.append, &message).await
    }

    async fn append_synced(&self, messages: Vec<StoredMessage>) -> Result<Vec<bool>, StorageError> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        let n = messages.len();
        let mut ids = Vec::with_capacity(n);
        let mut numbers = Vec::with_capacity(n);
        let mut contacts = Vec::with_capacity(n);
        let mut directions = Vec::with_capacity(n);
        let mut kinds = Vec::with_capacity(n);
        let mut texts = Vec::with_capacity(n);
        let mut payloads = Vec::with_capacity(n);
        let mut statuses = Vec::with_capacity(n);
        let mut timestamps = Vec::with_capacity(n);
        let mut status_ats = Vec::with_capacity(n);
        let mut errors = Vec::with_capacity(n);
        for m in &messages {
            ids.push(m.id.as_str());
            numbers.push(m.conversation.phone_number_id.as_str());
            contacts.push(m.conversation.contact.as_str());
            directions.push(direction_str(m.direction));
            kinds.push(m.kind.as_bytes());
            texts.push(m.text.as_deref().map(str::as_bytes));
            payloads.push(json_text(&m.payload, "message payload")?);
            statuses.push(status_str(m.status)?);
            timestamps.push(m.timestamp);
            status_ats.push(m.status_at);
            errors.push(
                m.error
                    .as_ref()
                    .map(|e| json_text(e, "message error"))
                    .transpose()?,
            );
        }
        let inserted: Vec<String> =
            sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.append_synced)))
                .bind(&ids)
                .bind(&numbers)
                .bind(&contacts)
                .bind(&directions)
                .bind(&kinds)
                .bind(&texts)
                .bind(&payloads)
                .bind(&statuses)
                .bind(&timestamps)
                .bind(&status_ats)
                .bind(&errors)
                .fetch_all(&self.pool)
                .await
                .map_err(backend)?;
        // The first occurrence of each inserted id is the one inserted.
        let mut inserted: std::collections::HashSet<String> = inserted.into_iter().collect();
        Ok(messages
            .iter()
            .map(|m| inserted.remove(m.id.as_str()))
            .collect())
    }

    async fn revoke(
        &self,
        key: &ConversationKey,
        id: &MessageId,
        direction: Direction,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError> {
        let tombstone = StoredMessage::tombstone(key, id, direction, at);
        if self.insert(&self.sql.tombstone, &tombstone).await? {
            return Ok(true);
        }
        self.set_status(
            &key.phone_number_id,
            id,
            Some(direction),
            DeliveryStatus::Deleted,
            at,
            None,
        )
        .await
    }

    async fn fill_media_placeholder(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        kind: String,
        text: Option<String>,
        payload: serde_json::Value,
    ) -> Result<bool, StorageError> {
        let filled: i64 = sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.fill_placeholder)))
            .bind(id.as_str())
            .bind(phone_number_id.as_str())
            .bind(kind.into_bytes())
            .bind(text.map(String::into_bytes))
            .bind(json_text(&payload, "message payload")?)
            .bind(StoredMessage::MEDIA_PLACEHOLDER.as_bytes())
            .bind(status_str(DeliveryStatus::Deleted)?)
            .fetch_one(&self.pool)
            .await
            .map_err(backend)?;
        Ok(filled > 0)
    }

    async fn update_status(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<serde_json::Value>,
    ) -> Result<bool, StorageError> {
        self.set_status(phone_number_id, id, None, status, at, error.as_ref())
            .await
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
            None => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.messages))),
            Some(_) => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.messages_before))),
        }
        .bind(key.phone_number_id.as_str())
        .bind(key.contact.as_str())
        .bind(to_i64(limit));
        let query = match &before {
            None => query,
            Some((at, id)) => query.bind(*at).bind(id.as_str()),
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
            None => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.conversations))),
            Some(_) => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.conversations_before))),
        }
        .bind(phone_number_id.as_str())
        .bind(to_i64(limit));
        let query = match &before {
            None => query,
            Some((at, contact)) => query.bind(*at).bind(contact.as_str()),
        };
        let rows = query.fetch_all(&self.pool).await.map_err(backend)?;
        rows.iter().map(summary_from_row).collect()
    }

    async fn mark_read(&self, key: &ConversationKey) -> Result<(), StorageError> {
        sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.mark_read)))
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
        let at: Option<Option<OffsetDateTime>> =
            sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.last_inbound_at)))
                .bind(key.phone_number_id.as_str())
                .bind(key.contact.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        Ok(at.flatten())
    }

    async fn message(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
    ) -> Result<Option<StoredMessage>, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.message)))
            .bind(id.as_str())
            .bind(phone_number_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(message_from_row).transpose()
    }

    async fn record_window_event(&self, event: WindowEvent) -> Result<bool, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.window_event)))
            .bind(event.conversation.phone_number_id.as_str())
            .bind(event.id.as_str())
            .bind(event.conversation.contact.as_str())
            .bind(event.kind.as_str())
            .bind(event.at)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        Ok(row.is_some())
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
            None => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.window_events))),
            Some(_) => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.window_events_before))),
        }
        .bind(key.phone_number_id.as_str())
        .bind(key.contact.as_str())
        .bind(to_i64(limit));
        let query = match &before {
            None => query,
            Some((at, id)) => query.bind(*at).bind(id.as_str()),
        };
        let rows = query.fetch_all(&self.pool).await.map_err(backend)?;
        rows.iter().map(window_event_from_row).collect()
    }

    async fn set_thread_owner(
        &self,
        key: &ConversationKey,
        ownership: ThreadOwnership,
    ) -> Result<bool, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.set_owner)))
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .bind(owner_str(ownership.owner)?)
            .bind(ownership.role.as_deref())
            .bind(ownership.app_id.as_ref().map(AppId::as_str))
            .bind(ownership.since)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        Ok(row.is_some())
    }

    async fn thread_owner(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<ThreadOwnership>, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.owner)))
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(owner_from_row).transpose()
    }

    async fn put_contact(&self, contact: StoredContact) -> Result<bool, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.put_contact)))
            .bind(contact.key.phone_number_id.as_str())
            .bind(contact.key.contact.as_str())
            .bind(contact.full_name.as_deref().map(str::as_bytes))
            .bind(contact.first_name.as_deref().map(str::as_bytes))
            .bind(contact.phone_number.as_deref())
            .bind(contact.user_id.as_ref().map(UserId::as_str))
            .bind(contact.parent_user_id.as_ref().map(UserId::as_str))
            .bind(contact.username.as_deref().map(str::as_bytes))
            .bind(contact.synced_at)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        Ok(row.is_some())
    }

    async fn remove_contact(
        &self,
        key: &ConversationKey,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError> {
        let done = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.remove_contact)))
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .bind(at)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected() > 0)
    }

    async fn contact(&self, key: &ConversationKey) -> Result<Option<StoredContact>, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.contact)))
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
            None => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.contacts))),
            Some(_) => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.contacts_after))),
        }
        .bind(phone_number_id.as_str())
        .bind(to_i64(limit));
        let query = match &after {
            None => query,
            Some(contact) => query.bind(contact.as_str()),
        };
        let rows = query.fetch_all(&self.pool).await.map_err(backend)?;
        rows.iter().map(contact_from_row).collect()
    }

    async fn erase(&self, key: &ConversationKey) -> Result<Erased, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.erase)))
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(backend)?;
        Ok(Erased {
            messages: count(&row, 0)?,
            conversations: count(&row, 1)?,
            window_events: count(&row, 2)?,
            thread_owners: count(&row, 3)?,
            contacts: count(&row, 4)?,
        })
    }

    async fn purge_before(
        &self,
        phone_number_id: Option<&PhoneNumberId>,
        cutoff: OffsetDateTime,
    ) -> Result<Purged, StorageError> {
        let query = match phone_number_id {
            None => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.purge_all))).bind(cutoff),
            Some(number) => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.purge_number)))
                .bind(cutoff)
                .bind(number.as_str()),
        };
        let row = query.fetch_one(&self.pool).await.map_err(backend)?;
        Ok(Purged {
            messages: count(&row, 0)?,
            conversations: count(&row, 1)?,
            window_events: count(&row, 2)?,
            thread_owners: count(&row, 3)?,
        })
    }

    fn retention(&self) -> Retention {
        self.retention
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_names_round_trip_through_serde() {
        use DeliveryStatus as S;
        for s in [
            S::Received,
            S::Accepted,
            S::Sent,
            S::Delivered,
            S::Read,
            S::Played,
            S::Failed,
            S::Deleted,
        ] {
            let name = status_str(s).unwrap();
            assert_eq!(parse_status(name).unwrap(), s);
        }
        assert_eq!(status_str(S::Delivered).unwrap(), "delivered");
        assert!(matches!(
            parse_status("warning".to_owned()),
            Err(StorageError::Corrupt { .. })
        ));
    }

    #[test]
    fn owner_names_round_trip_through_serde() {
        for owner in [
            ThreadOwner::ThisApp,
            ThreadOwner::AnotherApp,
            ThreadOwner::Idle,
        ] {
            assert_eq!(parse_owner(owner_str(owner).unwrap()).unwrap(), owner);
        }
        assert!(matches!(
            parse_owner("somebody".to_owned()),
            Err(StorageError::Corrupt { .. })
        ));
    }

    #[test]
    fn directions_round_trip() {
        for d in [Direction::Inbound, Direction::Outbound] {
            assert_eq!(parse_direction(direction_str(d)).unwrap(), d);
        }
        assert!(parse_direction("sideways").is_err());
    }
}
