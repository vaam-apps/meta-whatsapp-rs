//! [`PgEventStore`]: the event outbox on Postgres (`wa_server_events`,
//! migration 3).
//!
//! Sequences come from an identity column, and a sequence is drawn inside
//! the inserting transaction: two replicas inserting at once could commit
//! them out of order, and a poll between the two commits would move its
//! cursor past the event still in flight, which it would then never read.
//! So every insert holds [`OUTBOX_LOCK`] (a transaction-level advisory
//! lock) from before it draws its sequence until it commits: inserts
//! commit one at a time, in sequence order. The price is that inserts do
//! not run in parallel across replicas (each holds the lock for one
//! statement and its commit). Advisory locks are the database's, not a
//! schema's: deployments sharing one database share the lock.
//!
//! **A tenant's events never outlive it.** `tenant_id` references the
//! tenant with `ON DELETE SET NULL`: deleting a tenant turns its events
//! into operator-only rows in the deleting transaction. And an insert
//! routed to a tenant keeps that tenant only while the binding it was
//! routed by still names it, read under a `FOR KEY SHARE` lock that holds
//! off an unbinding until the insert commits: an event routed just before
//! its tenant was unbound, deleted and created again under the same id is
//! recorded operator-only, never shown to the new tenant.

use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::postgres::PgRow;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::{self, PgPool, Row};
use meta_whatsapp_rs::core::error::StorageError;
use time::OffsetDateTime;

use super::StoreResult;
use super::events::{EventPage, EventQuery, EventStore, NewEvent, StoredEvent};
use crate::model::TenantId;

/// The advisory lock every outbox insert holds until it commits, so that
/// sequences commit in order. The first eight bytes of
/// SHA-256(`meta-whatsapp-server/outbox`), as a big-endian `i64`.
pub const OUTBOX_LOCK: i64 = i64::from_be_bytes([0xdd, 0x21, 0x75, 0x17, 0x76, 0xf6, 0xce, 0x88]);

/// The advisory lock of housekeeping (the outbox purge): one replica at a
/// time purges. The first eight bytes of
/// SHA-256(`meta-whatsapp-server/housekeeping`), as a big-endian `i64`.
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

#[async_trait]
impl EventStore for PgEventStore {
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        // Held until the commit: sequences commit in order (module docs).
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(OUTBOX_LOCK)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        // The tenant, while the binding the event was routed by (its
        // number, under the WABA it names; else its WABA) still names it:
        // the same rule as `crate::events::owner`. Locked, so an unbinding
        // waits for this commit.
        let inserted: Option<(i64, Option<String>)> = sqlx::query_as(
            "INSERT INTO wa_server_events \
             (id, dedup_key, tenant_id, phone_number_id, waba_id, event_type, data) \
             SELECT $1, $2, \
               CASE \
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
               END, \
               $4, $5, $6, $7::json \
             ON CONFLICT (dedup_key) DO NOTHING RETURNING sequence, tenant_id",
        )
        .bind(&event.id)
        .bind(event.dedup_key.as_deref())
        .bind(event.tenant.as_ref().map(TenantId::as_str))
        .bind(event.phone_number_id.as_deref())
        .bind(event.waba_id.as_deref())
        .bind(&event.event_type)
        .bind(&event.data)
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
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
        // One statement, so the rows and both bounds come from one
        // snapshot.
        let rows = sqlx::query(
            "WITH bounds AS ( \
               SELECT COALESCE((SELECT purged_through FROM wa_server_event_purges), 0) \
                        AS purged_through, \
                      COALESCE((SELECT max(sequence) FROM wa_server_events), 0) AS stored \
             ) \
             SELECT b.purged_through, GREATEST(b.stored, b.purged_through) AS high_water, \
                    e.sequence, e.id, e.tenant_id, e.phone_number_id, e.waba_id, \
                    e.event_type, e.data::text AS data, e.created_at \
             FROM bounds b LEFT JOIN LATERAL ( \
               SELECT * FROM wa_server_events \
               WHERE tenant_id = $1 AND sequence > COALESCE($2::bigint, b.purged_through) \
                 AND ($3::text[] IS NULL OR event_type = ANY($3)) \
                 AND ($4::text IS NULL OR phone_number_id = $4) \
               ORDER BY sequence LIMIT $5 \
             ) e ON true \
             ORDER BY e.sequence",
        )
        .bind(query.tenant.as_str())
        .bind(query.after)
        .bind(query.types.as_deref())
        .bind(query.phone_number_id.as_deref())
        .bind(i64::try_from(query.limit.saturating_add(1)).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let first = rows.first().ok_or_else(|| corrupt("purged_through"))?;
        let purged_through: i64 = get(first, "purged_through")?;
        let high_water: i64 = get(first, "high_water")?;
        let mut events = Vec::with_capacity(rows.len());
        for row in &rows {
            if let Some(event) = event_row(row)? {
                events.push(event);
            }
        }
        Ok(EventPage {
            events,
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
        // The prefix of sequences up to the newest event past the cutoff:
        // a clock step never leaves an older event behind a purged one.
        let through: Option<i64> = sqlx::query_scalar(
            "SELECT max(sequence) FROM wa_server_events \
             WHERE created_at < clock_timestamp() - $1::bigint * interval '1 millisecond'",
        )
        .bind(millis)
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;
        let Some(through) = through else {
            tx.rollback().await.map_err(backend)?;
            return Ok(Some(0));
        };
        let deleted = sqlx::query("DELETE FROM wa_server_events WHERE sequence <= $1")
            .bind(through)
            .execute(&mut *tx)
            .await
            .map_err(backend)?
            .rows_affected();
        sqlx::query(
            "INSERT INTO wa_server_event_purges (singleton, purged_through) VALUES (true, $1) \
             ON CONFLICT (singleton) DO UPDATE SET purged_through = \
             GREATEST(wa_server_event_purges.purged_through, EXCLUDED.purged_through), \
             purged_at = now()",
        )
        .bind(through)
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        Ok(Some(deleted))
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
    fn the_lock_keys_are_derived_as_documented() {
        assert_eq!(OUTBOX_LOCK, derived(b"meta-whatsapp-server/outbox"));
        assert_eq!(
            HOUSEKEEPING_LOCK,
            derived(b"meta-whatsapp-server/housekeeping")
        );
        assert_ne!(OUTBOX_LOCK, super::super::MIGRATION_LOCK);
    }
}
