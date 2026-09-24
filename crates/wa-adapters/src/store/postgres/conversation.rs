//! [`ConversationStore`] on Postgres.
//!
//! - `append` is a single statement: the message insert (`ON CONFLICT (id)
//!   DO NOTHING`) feeds the inbox-summary upsert through a data-modifying
//!   CTE, so a duplicate id changes nothing and the summary can never
//!   disagree with the history, without a client-side transaction.
//! - `update_status` applies [`DeliveryStatus::supersedes`] — the rule lives
//!   in `wa-core`, not re-encoded in SQL — as a compare-and-swap on the
//!   stored status: read it, decide in Rust, then `UPDATE … WHERE status =
//!   <what we read>`. A concurrent update makes the swap miss and we re-read.
//!   Every successful update strictly raises the status rank, so the loop is
//!   bounded by the number of ranks.

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
/// last `mark_read`), like the in-memory store. Timestamps are stored with
/// microsecond precision.
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

impl Sql {
    fn new(prefix: &TablePrefix) -> Self {
        let messages = prefix.table("messages");
        let conversations = prefix.table("conversations");
        // The summary's "newest message" is the max by (ts, id), the same
        // order the history pages in.
        let newer = "(EXCLUDED.last_message_at, EXCLUDED.last_message_id) \
                     > (c.last_message_at, c.last_message_id)";
        let arc = |s: String| -> Arc<str> { Arc::from(s) };
        Self {
            append: arc(format!(
                "WITH inserted AS ( \
                   INSERT INTO {messages} ({MESSAGE_COLUMNS}) \
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                   ON CONFLICT (id) DO NOTHING \
                   RETURNING id, phone_number_id, contact, direction, text, ts \
                 ) \
                 INSERT INTO {conversations} AS c \
                   (phone_number_id, contact, last_message_at, last_message_id, last_text, \
                    last_inbound_at, unread) \
                 SELECT phone_number_id, contact, ts, id, text, \
                   CASE WHEN direction = 'inbound' THEN ts END, \
                   CASE WHEN direction = 'inbound' THEN 1 ELSE 0 END \
                 FROM inserted \
                 ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
                   last_message_at = CASE WHEN {newer} THEN EXCLUDED.last_message_at ELSE c.last_message_at END, \
                   last_message_id = CASE WHEN {newer} THEN EXCLUDED.last_message_id ELSE c.last_message_id END, \
                   last_text = CASE WHEN {newer} THEN EXCLUDED.last_text ELSE c.last_text END, \
                   last_inbound_at = GREATEST(c.last_inbound_at, EXCLUDED.last_inbound_at), \
                   unread = c.unread + EXCLUDED.unread \
                 RETURNING 1 AS appended"
            )),
            status_get: arc(format!("SELECT status FROM {messages} WHERE id = $1")),
            status_swap: arc(format!(
                "UPDATE {messages} SET status = $3, status_at = $4, error = COALESCE($5, error) \
                 WHERE id = $1 AND status = $2"
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

fn parse_status(s: String, id: &str) -> Result<DeliveryStatus, StorageError> {
    serde_json::from_value(serde_json::Value::String(s)).map_err(|source| StorageError::Corrupt {
        key: format!("message {id} status"),
        source,
    })
}

fn message_from_row(row: &PgRow) -> Result<StoredMessage, StorageError> {
    let id: String = row.try_get("id").map_err(backend)?;
    let direction: String = row.try_get("direction").map_err(backend)?;
    let status: String = row.try_get("status").map_err(backend)?;
    let status = parse_status(status, &id)?;
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

#[async_trait]
impl ConversationStore for PostgresConversationStore {
    async fn append(&self, message: StoredMessage) -> Result<bool, StorageError> {
        let status = status_str(message.status)?;
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.append)))
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

    async fn update_status(
        &self,
        id: &MessageId,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<serde_json::Value>,
    ) -> Result<bool, StorageError> {
        let new = status_str(status)?;
        for _ in 0..MAX_STATUS_ROUNDS {
            let current: Option<String> =
                sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.status_get)))
                    .bind(id.as_str())
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(backend)?;
            let Some(current) = current else {
                return Ok(false); // a status for a message sent elsewhere
            };
            if !status.supersedes(parse_status(current.clone(), id.as_str())?) {
                return Ok(false);
            }
            let done = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.status_swap)))
                .bind(id.as_str())
                .bind(&current)
                .bind(&new)
                .bind(at)
                .bind(error.as_ref())
                .execute(&self.pool)
                .await
                .map_err(backend)?;
            if done.rows_affected() == 1 {
                return Ok(true);
            }
            // Someone else moved the status between our read and our swap;
            // decide again against what they wrote.
        }
        Err(StorageError::Backend(anyhow::anyhow!(
            "status of message {id} kept changing; gave up after {MAX_STATUS_ROUNDS} rounds"
        )))
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
            assert_eq!(parse_status(name, "m").unwrap(), s);
        }
        assert_eq!(status_str(S::Delivered).unwrap(), "delivered");
        assert!(matches!(
            parse_status("warning".to_owned(), "m"),
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
