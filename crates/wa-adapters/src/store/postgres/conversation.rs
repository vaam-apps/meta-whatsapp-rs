//! [`ConversationStore`] on Postgres.
//!
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
//!   in `wa-core`, not re-encoded in SQL — as a compare-and-swap on the
//!   stored status: read it, decide in Rust, then `UPDATE … WHERE status =
//!   <what we read>`. A concurrent update makes the swap miss and we re-read.
//!   Every successful update strictly raises the status rank, so the loop is
//!   bounded by the number of ranks. Both statements match `id` *and*
//!   `phone_number_id`, so an event of one business number never changes a
//!   row of another.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::postgres::PgRow;
use sqlx::{AssertSqlSafe, PgPool, Row};
use time::OffsetDateTime;
use wa_core::error::StorageError;
use wa_core::ids::{MessageId, PhoneNumberId};
use wa_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, DeliveryStatus, Direction,
    StoredMessage,
};

use super::{TablePrefix, backend, to_i64};

/// More compare-and-swap rounds than there are status ranks: reaching it
/// means something outside this adapter keeps rewriting the row.
const MAX_STATUS_ROUNDS: usize = 16;

/// `ConversationStore` over a Postgres pool. Cheap to clone.
///
/// Unread counts are by **arrival** (inbound messages appended since the
/// last `mark_read`; synced history never counts), like the in-memory
/// store. Timestamps are stored with microsecond precision.
#[derive(Clone)]
pub struct PostgresConversationStore {
    pool: PgPool,
    prefix: TablePrefix,
    sql: Arc<Sql>,
}

impl fmt::Debug for PostgresConversationStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresConversationStore")
            .field("prefix", &self.prefix)
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
}

const MESSAGE_COLUMNS: &str =
    "id, phone_number_id, contact, direction, kind, text, payload, status, ts, status_at, error";

const SUMMARY_COLUMNS: &str =
    "phone_number_id, contact, last_message_at, last_inbound_at, last_text, unread";

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
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
           ON CONFLICT (id) DO NOTHING \
           RETURNING id, phone_number_id, contact, direction, text, ts \
         ) \
         INSERT INTO {conversations} AS c \
           (phone_number_id, contact, last_message_at, last_message_id, last_text, \
            last_inbound_at, unread) \
         SELECT phone_number_id, contact, ts, id, text, {inbound_at}, {unread} \
         FROM inserted \
         ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
           last_message_at = CASE WHEN {newer} THEN EXCLUDED.last_message_at ELSE c.last_message_at END, \
           last_message_id = CASE WHEN {newer} THEN EXCLUDED.last_message_id ELSE c.last_message_id END, \
           last_text = CASE WHEN {newer} THEN EXCLUDED.last_text ELSE c.last_text END, \
           last_inbound_at = GREATEST(c.last_inbound_at, EXCLUDED.last_inbound_at), \
           unread = c.unread + EXCLUDED.unread \
         RETURNING 1 AS appended"
    )
}

/// The `append_synced` statement: one row per element of the eleven
/// column arrays, the first of each id kept, no inbound effects.
fn append_synced_sql(messages: &str, conversations: &str) -> String {
    let newer = NEWER;
    format!(
        "WITH input AS ( \
           SELECT DISTINCT ON (id) * FROM UNNEST($1::text[], $2::text[], $3::text[], \
             $4::text[], $5::text[], $6::text[], $7::jsonb[], $8::text[], \
             $9::timestamptz[], $10::timestamptz[], $11::jsonb[]) \
             WITH ORDINALITY AS t({MESSAGE_COLUMNS}, n) \
           ORDER BY id, n \
         ), inserted AS ( \
           INSERT INTO {messages} ({MESSAGE_COLUMNS}) \
           SELECT {MESSAGE_COLUMNS} FROM input \
           ON CONFLICT (id) DO NOTHING \
           RETURNING id, phone_number_id, contact, text, ts \
         ), latest AS ( \
           SELECT DISTINCT ON (phone_number_id, contact) phone_number_id, contact, ts, id, text \
           FROM inserted \
           ORDER BY phone_number_id, contact, ts DESC, id COLLATE \"C\" DESC \
         ), summary AS ( \
           INSERT INTO {conversations} AS c \
             (phone_number_id, contact, last_message_at, last_message_id, last_text, \
              last_inbound_at, unread) \
           SELECT phone_number_id, contact, ts, id, text, NULL, 0 FROM latest \
           ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
             last_message_at = CASE WHEN {newer} THEN EXCLUDED.last_message_at ELSE c.last_message_at END, \
             last_message_id = CASE WHEN {newer} THEN EXCLUDED.last_message_id ELSE c.last_message_id END, \
             last_text = CASE WHEN {newer} THEN EXCLUDED.last_text ELSE c.last_text END \
         ) \
         SELECT id FROM inserted"
    )
}

impl Sql {
    fn new(prefix: &TablePrefix) -> Self {
        let messages = prefix.table("messages");
        let conversations = prefix.table("conversations");
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
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                 ON CONFLICT (id) DO NOTHING \
                 RETURNING 1 AS appended"
            )),
            // Data-modifying CTEs always run to completion, whether or not
            // the final SELECT reads them. `$7`: the `deleted` status, which
            // a placeholder is never filled in.
            fill_placeholder: arc(format!(
                "WITH filled AS ( \
                   UPDATE {messages} SET kind = $3, text = $4, payload = $5 \
                   WHERE id = $1 AND phone_number_id = $2 AND kind = $6 AND status <> $7 \
                   RETURNING id, phone_number_id, contact, text \
                 ), preview AS ( \
                   UPDATE {conversations} AS c SET last_text = f.text \
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
                "UPDATE {messages} SET status = $4, status_at = $5, error = COALESCE($6, error) \
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
        Self { pool, prefix, sql }
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
/// type serializes to (and a variant added to `wa-core` needs no change here).
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

fn message_from_row(row: &PgRow) -> Result<StoredMessage, StorageError> {
    let id: String = row.try_get("id").map_err(backend)?;
    let direction: String = row.try_get("direction").map_err(backend)?;
    let status: String = row.try_get("status").map_err(backend)?;
    let status = parse_status(status)?;
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    Ok(StoredMessage {
        conversation: ConversationKey::new(phone_number_id, contact),
        direction: parse_direction(&direction)?,
        kind: row.try_get("kind").map_err(backend)?,
        text: row.try_get("text").map_err(backend)?,
        payload: row.try_get("payload").map_err(backend)?,
        status,
        timestamp: row.try_get("ts").map_err(backend)?,
        status_at: row.try_get("status_at").map_err(backend)?,
        error: row.try_get("error").map_err(backend)?,
        id: MessageId::new(id),
    })
}

fn summary_from_row(row: &PgRow) -> Result<ConversationSummary, StorageError> {
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let unread: i64 = row.try_get("unread").map_err(backend)?;
    Ok(ConversationSummary {
        key: ConversationKey::new(phone_number_id, contact),
        last_message_at: row.try_get("last_message_at").map_err(backend)?,
        last_inbound_at: row.try_get("last_inbound_at").map_err(backend)?,
        last_text: row.try_get("last_text").map_err(backend)?,
        // Never negative: it only grows by 1 and resets to 0.
        unread: u64::try_from(unread).unwrap_or(0),
    })
}

impl PostgresConversationStore {
    /// Run an append statement (`sql`) for `message`.
    async fn insert(&self, sql: &Arc<str>, message: &StoredMessage) -> Result<bool, StorageError> {
        let status = status_str(message.status)?;
        let row = sqlx::query(AssertSqlSafe(Arc::clone(sql)))
            .bind(message.id.as_str())
            .bind(message.conversation.phone_number_id.as_str())
            .bind(message.conversation.contact.as_str())
            .bind(direction_str(message.direction))
            .bind(message.kind.as_str())
            .bind(message.text.as_deref())
            .bind(&message.payload)
            .bind(status)
            .bind(message.timestamp)
            .bind(message.status_at)
            .bind(message.error.as_ref())
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
                .bind(error)
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
            kinds.push(m.kind.as_str());
            texts.push(m.text.as_deref());
            payloads.push(&m.payload);
            statuses.push(status_str(m.status)?);
            timestamps.push(m.timestamp);
            status_ats.push(m.status_at);
            errors.push(m.error.as_ref());
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
            .bind(kind)
            .bind(text)
            .bind(payload)
            .bind(StoredMessage::MEDIA_PLACEHOLDER)
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
    fn directions_round_trip() {
        for d in [Direction::Inbound, Direction::Outbound] {
            assert_eq!(parse_direction(direction_str(d)).unwrap(), d);
        }
        assert!(parse_direction("sideways").is_err());
    }
}
