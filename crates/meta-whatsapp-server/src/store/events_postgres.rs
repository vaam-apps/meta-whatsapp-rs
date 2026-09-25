//! [`PgEventStore`]: the event outbox on Postgres (`wa_server_events` and
//! `wa_server_event_streams`, migration 3).
//!
//! **One stream of sequences per tenant.** An insert draws its sequence by
//! incrementing its stream's row of `wa_server_event_streams` (the tenant's
//! id, or `''` for operator-only rows) and holds that row's lock until it
//! commits: a stream's inserts commit one at a time, in sequence order, so
//! a poll that saw sequence `n` of a tenant saw every event of the tenant
//! before it and never moves past one still in flight. Different tenants'
//! inserts do not wait for each other.
//!
//! **A tenant's events never outlive it.** `tenant_id` references the
//! tenant with `ON DELETE CASCADE`, and deleting a tenant also records its
//! stream purged through its last sequence (`PgStore::delete_tenant`), in
//! the deleting transaction: a tenant created later with the same id polls
//! none of them, and its sequences go on after them. And an insert routed
//! to a tenant keeps that tenant only while the binding it was routed by
//! still names it, read under a `FOR KEY SHARE` lock that holds off an
//! unbinding until the insert commits: an event routed just before its
//! tenant was unbound, deleted and created again under the same id is
//! recorded operator-only, never shown to the new tenant.
//!
//! **A page reads no more data than it answers.** The page's rows are
//! chosen from their sizes (`data_bytes`) first, then only those rows'
//! data is read.

use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::postgres::PgRow;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::{self, PgPool, Row};
use meta_whatsapp_rs::core::error::StorageError;
use time::OffsetDateTime;

use super::StoreResult;
use super::events::{EventPage, EventQuery, EventStore, NewEvent, OutboxBusy, StoredEvent};
use crate::model::TenantId;

/// The advisory lock of housekeeping (the outbox purge): one replica at a
/// time purges. The first eight bytes of
/// SHA-256(`meta-whatsapp-server/housekeeping`), as a big-endian `i64`.
/// Advisory locks are the database's, not a schema's: deployments sharing
/// one database purge in turn.
pub const HOUSEKEEPING_LOCK: i64 =
    i64::from_be_bytes([0x06, 0x62, 0x5b, 0xd9, 0x6d, 0x85, 0xd1, 0xcf]);

/// The outbox on Postgres. Cheap to clone.
#[derive(Clone)]
pub struct PgEventStore {
    pool: PgPool,
}

impl std::fmt::Debug for PgEventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgEventStore").finish_non_exhaustive()
    }
}

impl PgEventStore {
    /// The outbox on `pool`, whose database `migrate` has run on.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn backend(error: impl std::error::Error + Send + Sync + 'static) -> StorageError {
    StorageError::Backend(anyhow::Error::new(error))
}

/// A lock wait past `lock_timeout` (SQLSTATE 55P03) is [`OutboxBusy`].
fn busy_or_backend(error: sqlx::Error) -> StorageError {
    let lock_timeout = error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "55P03");
    if lock_timeout {
        StorageError::Backend(anyhow::Error::new(OutboxBusy))
    } else {
        backend(error)
    }
}

fn corrupt(column: &'static str) -> StorageError {
    StorageError::Backend(anyhow::anyhow!("unreadable value in column `{column}`"))
}

fn get<'r, T>(row: &'r PgRow, column: &'static str) -> StoreResult<T>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get(column).map_err(backend)
}

/// The event of a page row, `None` for the row of an empty page.
fn event_row(row: &PgRow) -> StoreResult<Option<StoredEvent>> {
    let Some(sequence) = get::<Option<i64>>(row, "sequence")? else {
        return Ok(None);
    };
    let tenant = get::<Option<String>>(row, "tenant_id")?
        .map(|t| TenantId::parse(&t).ok_or_else(|| corrupt("tenant_id")))
        .transpose()?;
    Ok(Some(StoredEvent {
        sequence,
        id: get::<Option<String>>(row, "id")?.ok_or_else(|| corrupt("id"))?,
        tenant,
        phone_number_id: get(row, "phone_number_id")?,
        waba_id: get(row, "waba_id")?,
        event_type: get::<Option<String>>(row, "event_type")?
            .ok_or_else(|| corrupt("event_type"))?,
        data: get::<Option<String>>(row, "data")?.ok_or_else(|| corrupt("data"))?,
        created_at: get::<Option<OffsetDateTime>>(row, "created_at")?
            .ok_or_else(|| corrupt("created_at"))?,
    }))
}

fn i64_of(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

#[async_trait]
impl EventStore for PgEventStore {
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>> {
        let data_bytes = i32::try_from(event.data.len())
            .map_err(|_| StorageError::Backend(anyhow::anyhow!("an event over 2 GiB")))?;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        // A connection of the pool the API shares waits at most this long
        // for a lock (security review M2).
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        // 1. The tenant, while the binding the event was routed by (its
        //    number, under the WABA it names; else its WABA) still names
        //    it: the same rule as `crate::events::owner`. Locked, so an
        //    unbinding waits for this commit.
        // 2. Nothing when the dedup key is stored (no sequence drawn).
        // 3. The stream's next sequence: its row stays locked until the
        //    commit, so the stream's inserts commit in order.
        let inserted: Option<(i64, Option<String>)> = sqlx::query_as(
            "WITH owner AS ( \
               SELECT CASE \
                 WHEN $3::text IS NULL THEN NULL \
                 WHEN $4::text IS NOT NULL THEN ( \
                   SELECT n.tenant_id FROM wa_server_numbers n \
                   WHERE n.phone_number_id = $4 AND n.tenant_id = $3 \
                     AND ($5::text IS NULL OR n.waba_id = $5) \
                   FOR KEY SHARE) \
                 ELSE ( \
                   SELECT w.tenant_id FROM wa_server_wabas w \
                   WHERE w.waba_id = $5 AND w.tenant_id = $3 \
                   FOR KEY SHARE) \
               END AS tenant_id \
             ), fresh AS ( \
               SELECT owner.tenant_id FROM owner \
               WHERE $2::text IS NULL \
                  OR NOT EXISTS (SELECT 1 FROM wa_server_events WHERE dedup_key = $2) \
             ), drawn AS ( \
               INSERT INTO wa_server_event_streams AS s (stream, last_sequence) \
               SELECT COALESCE(fresh.tenant_id, ''), 1 FROM fresh \
               ON CONFLICT (stream) DO UPDATE SET last_sequence = s.last_sequence + 1 \
               RETURNING s.last_sequence \
             ) \
             INSERT INTO wa_server_events \
             (tenant_id, sequence, id, dedup_key, phone_number_id, waba_id, event_type, data, \
              data_bytes) \
             SELECT fresh.tenant_id, drawn.last_sequence, $1, $2, $4, $5, $6, $7::json, $8 \
             FROM fresh, drawn \
             ON CONFLICT (dedup_key) DO NOTHING RETURNING sequence, tenant_id",
        )
        .bind(&event.id)
        .bind(event.dedup_key.as_deref())
        .bind(event.tenant.as_ref().map(TenantId::as_str))
        .bind(event.phone_number_id.as_deref())
        .bind(event.waba_id.as_deref())
        .bind(&event.event_type)
        .bind(&event.data)
        .bind(data_bytes)
        .fetch_optional(&mut *tx)
        .await
        .map_err(busy_or_backend)?;
        tx.commit().await.map_err(backend)?;
        let Some((sequence, tenant)) = inserted else {
            return Ok(None);
        };
        if event.tenant.is_some() && tenant.is_none() {
            tracing::warn!(
                sequence,
                event_type = %event.event_type,
                "an event's tenant lost the binding it was routed by while it was recorded: \
                 kept operator-only"
            );
        }
        Ok(Some(sequence))
    }

    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage> {
        // One statement, so the rows and the stream's bounds come from one
        // snapshot. The rows are chosen from their sizes; only theirs are
        // read in full.
        let rows = sqlx::query(
            "WITH bounds AS ( \
               SELECT COALESCE(max(purged_through), 0) AS purged_through, \
                      COALESCE(max(last_sequence), 0) AS high_water \
               FROM wa_server_event_streams WHERE stream = $1 \
             ), candidates AS ( \
               SELECT e.sequence, \
                      row_number() OVER w AS n, \
                      sum(e.data_bytes) OVER w AS running \
               FROM wa_server_events e, bounds b \
               WHERE e.stream = $1 AND e.sequence > COALESCE($2::bigint, b.purged_through) \
                 AND ($3::text[] IS NULL OR e.event_type = ANY($3)) \
                 AND ($4::text IS NULL OR e.phone_number_id = $4) \
               WINDOW w AS (ORDER BY e.sequence ROWS UNBOUNDED PRECEDING) \
               ORDER BY e.sequence LIMIT $5 + 1 \
             ), kept AS ( \
               SELECT sequence FROM candidates \
               WHERE n <= $5 AND (n = 1 OR running <= $6) \
             ) \
             SELECT b.purged_through, b.high_water, \
                    (SELECT count(*) FROM candidates) > (SELECT count(*) FROM kept) AS more, \
                    e.sequence, e.id, e.tenant_id, e.phone_number_id, e.waba_id, \
                    e.event_type, e.data::text AS data, e.created_at \
             FROM bounds b \
             LEFT JOIN kept k ON true \
             LEFT JOIN wa_server_events e ON e.stream = $1 AND e.sequence = k.sequence \
             ORDER BY e.sequence",
        )
        .bind(query.tenant.as_str())
        .bind(query.after)
        .bind(query.types.as_deref())
        .bind(query.phone_number_id.as_deref())
        .bind(i64_of(query.limit))
        .bind(i64_of(query.max_bytes))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let first = rows.first().ok_or_else(|| corrupt("purged_through"))?;
        let purged_through: i64 = get(first, "purged_through")?;
        let high_water: i64 = get(first, "high_water")?;
        let more: bool = get(first, "more")?;
        let mut events = Vec::with_capacity(rows.len());
        for row in &rows {
            if let Some(event) = event_row(row)? {
                events.push(event);
            }
        }
        Ok(EventPage {
            events,
            more,
            purged_through,
            high_water,
        })
    }

    async fn purge(&self, older_than: Duration) -> StoreResult<Option<u64>> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let ours: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(HOUSEKEEPING_LOCK)
            .fetch_one(&mut *tx)
            .await
            .map_err(backend)?;
        if !ours {
            tx.rollback().await.map_err(backend)?;
            return Ok(None);
        }
        let millis = i64::try_from(older_than.as_millis()).unwrap_or(i64::MAX);
        // Per stream, the prefix of sequences up to its newest event past
        // the cutoff: a clock step never leaves an older event behind a
        // purged one.
        let deleted: i64 = sqlx::query_scalar(
            "WITH cut AS ( \
               SELECT stream, max(sequence) AS through FROM wa_server_events \
               WHERE created_at < clock_timestamp() - $1::bigint * interval '1 millisecond' \
               GROUP BY stream \
             ), gone AS ( \
               DELETE FROM wa_server_events e USING cut \
               WHERE e.stream = cut.stream AND e.sequence <= cut.through \
               RETURNING 1 \
             ), marked AS ( \
               UPDATE wa_server_event_streams s \
               SET purged_through = GREATEST(s.purged_through, cut.through) \
               FROM cut WHERE s.stream = cut.stream \
               RETURNING 1 \
             ) \
             SELECT count(*) FROM gone",
        )
        .bind(millis)
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        Ok(Some(u64::try_from(deleted).unwrap_or_default()))
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    fn derived(name: &[u8]) -> i64 {
        let digest = Sha256::digest(name);
        let mut first = [0u8; 8];
        first.copy_from_slice(&digest[..8]);
        i64::from_be_bytes(first)
    }

    #[test]
    fn the_lock_key_is_derived_as_documented() {
        assert_eq!(
            HOUSEKEEPING_LOCK,
            derived(b"meta-whatsapp-server/housekeeping")
        );
        assert_ne!(HOUSEKEEPING_LOCK, super::super::MIGRATION_LOCK);
    }
}
