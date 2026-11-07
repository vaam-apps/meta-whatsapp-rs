//! [`PgWebhookStore`]: webhooks out on Postgres (roadmap M2c;
//! `wa_server_webhook_endpoints` and `wa_server_webhook_deliveries`,
//! migration 9; roadmap M2m: `wa_server_webhook_endpoint_tenants` and the
//! endpoints' `named_tenants` flag, migration 11).
//!
//! - **An endpoint is its tenant's**: every statement names the tenant;
//!   both tables reference the tenant `ON DELETE CASCADE`, and a delivery
//!   its endpoint, so a tenant's or an endpoint's deletion takes them. A
//!   creation counts the tenant's endpoints under a `FOR UPDATE` lock on
//!   the tenant's row, which serializes creations (and the tenant's
//!   deletion) with it.
//! - **A fan-out is once**: it locks the row holding the cursor, the
//!   endpoint's or, for an endpoint of named tenants, that tenant's row,
//!   while the cursor is the value it read (`FOR UPDATE`, then re-checked
//!   by Postgres after a wait), records the deliveries (`ON CONFLICT
//!   (endpoint_id, event_id) DO NOTHING`, each with the tenant of the
//!   stream) and moves the cursor in one transaction.
//! - **Named tenants** (roadmap M2m): an endpoint with `named_tenants`
//!   receives its rows' tenants and no other's, a cursor each; a tenant's
//!   deletion deletes its row and its deliveries (`ON DELETE CASCADE`),
//!   so the endpoint is never widened by it.
//! - **A claim is exclusive**: each tenant's oldest due delivery, locked
//!   `FOR UPDATE SKIP LOCKED`, is leased and stamped with the claim's
//!   token in one statement; a settlement names that token.
//! - Every instant compared is the caller's (the dispatcher's clock).

use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::postgres::PgRow;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::{self, PgPool, Row};
use meta_whatsapp_rs::core::error::StorageError;
use time::OffsetDateTime;

use meta_whatsapp_server_core::model::{Listing, PageRequest, TenantId};
use meta_whatsapp_server_core::store::{StoreResult, listing};
use meta_whatsapp_server_core::webhooks_out::store::{
    Attempt, ClaimRequest, Claimed, CreateOutcome, EndpointChange, NewDelivery, NewEndpoint,
    Outcome, PendingDeliveries, WebhookStore, reached_through_holds,
};
use meta_whatsapp_server_core::webhooks_out::{
    AttemptError, Delivery, DeliveryId, DeliveryStatus, DisabledReason, Endpoint, EndpointId,
    SealedSecret, TenantCursor,
};

/// Webhooks out on Postgres. Cheap to clone.
#[derive(Clone)]
pub struct PgWebhookStore {
    pool: PgPool,
}

impl std::fmt::Debug for PgWebhookStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgWebhookStore").finish_non_exhaustive()
    }
}

impl PgWebhookStore {
    /// The store on `pool`, migrated.
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

const ENDPOINT_COLUMNS: &str = "id, tenant_id, url, event_types, enabled, disabled_reason, \
     secret, previous_secret, previous_secret_until, cursor, failing_since, created_at, updated_at, \
     named_tenants";

const DELIVERY_COLUMNS: &str = "id, endpoint_id, tenant_id, event_id, sequence, event_type, \
     status, attempts, next_attempt_at, give_up_at, last_attempt_at, last_status, last_error, \
     last_duration_ms, created_at";

fn endpoint_row(row: &PgRow) -> StoreResult<Endpoint> {
    Ok(Endpoint {
        id: EndpointId::parse(&get::<String>(row, "id")?).ok_or_else(|| corrupt("id"))?,
        tenant: TenantId::parse(&get::<String>(row, "tenant_id")?)
            .ok_or_else(|| corrupt("tenant_id"))?,
        // Filled by `fill_tenants` for an endpoint of named tenants.
        tenants: get::<bool>(row, "named_tenants")?.then(Vec::new),
        url: get(row, "url")?,
        types: get(row, "event_types")?,
        enabled: get(row, "enabled")?,
        disabled_reason: get::<Option<String>>(row, "disabled_reason")?
            .as_deref()
            .map(DisabledReason::parse),
        secret: SealedSecret::from_stored(get::<String>(row, "secret")?),
        previous_secret: get::<Option<String>>(row, "previous_secret")?
            .map(SealedSecret::from_stored),
        previous_secret_until: get(row, "previous_secret_until")?,
        cursor: get(row, "cursor")?,
        failing_since: get(row, "failing_since")?,
        created_at: get(row, "created_at")?,
        updated_at: get(row, "updated_at")?,
    })
}

fn delivery_row(row: &PgRow) -> StoreResult<Delivery> {
    Ok(Delivery {
        id: DeliveryId::parse(&get::<String>(row, "id")?).ok_or_else(|| corrupt("id"))?,
        endpoint: EndpointId::parse(&get::<String>(row, "endpoint_id")?)
            .ok_or_else(|| corrupt("endpoint_id"))?,
        tenant: TenantId::parse(&get::<String>(row, "tenant_id")?)
            .ok_or_else(|| corrupt("tenant_id"))?,
        event_id: get(row, "event_id")?,
        sequence: get(row, "sequence")?,
        event_type: get(row, "event_type")?,
        status: DeliveryStatus::parse(&get::<String>(row, "status")?)
            .ok_or_else(|| corrupt("status"))?,
        attempts: u32::try_from(get::<i32>(row, "attempts")?).unwrap_or(0),
        next_attempt_at: get(row, "next_attempt_at")?,
        give_up_at: get(row, "give_up_at")?,
        last_attempt_at: get(row, "last_attempt_at")?,
        last_status: get::<Option<i32>>(row, "last_status")?.and_then(|s| u16::try_from(s).ok()),
        last_error: get::<Option<String>>(row, "last_error")?
            .as_deref()
            .map(AttemptError::parse),
        last_duration_ms: get::<Option<i32>>(row, "last_duration_ms")?
            .and_then(|d| u32::try_from(d).ok()),
        created_at: get(row, "created_at")?,
    })
}

/// The named tenants of those of `endpoints` that have them, read on
/// `executor` (the transaction that read the endpoints, or the pool).
async fn fill_tenants<'e, E>(executor: E, endpoints: &mut [Endpoint]) -> StoreResult<()>
where
    E: sqlx::PgExecutor<'e>,
{
    let ids: Vec<String> = endpoints
        .iter()
        .filter(|e| e.tenants.is_some())
        .map(|e| e.id.as_str().to_owned())
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let rows = sqlx::query(
        "SELECT endpoint_id, tenant_id, cursor FROM wa_server_webhook_endpoint_tenants \
         WHERE endpoint_id = ANY($1) ORDER BY endpoint_id, tenant_id",
    )
    .bind(&ids)
    .fetch_all(executor)
    .await
    .map_err(backend)?;
    for row in &rows {
        let endpoint: String = get(row, "endpoint_id")?;
        let tenant = TenantId::parse(&get::<String>(row, "tenant_id")?)
            .ok_or_else(|| corrupt("tenant_id"))?;
        let cursor: i64 = get(row, "cursor")?;
        if let Some(named) = endpoints
            .iter_mut()
            .find(|e| e.id.as_str() == endpoint)
            .and_then(|e| e.tenants.as_mut())
        {
            named.push(TenantCursor { tenant, cursor });
        }
    }
    Ok(())
}

/// `rows` as endpoints, their named tenants read on `executor`.
async fn endpoints_of<'e, E>(executor: E, rows: &[PgRow]) -> StoreResult<Vec<Endpoint>>
where
    E: sqlx::PgExecutor<'e>,
{
    let mut endpoints = rows
        .iter()
        .map(endpoint_row)
        .collect::<StoreResult<Vec<_>>>()?;
    fill_tenants(executor, &mut endpoints).await?;
    Ok(endpoints)
}

fn after(now: OffsetDateTime, by: Duration) -> OffsetDateTime {
    now.saturating_add(time::Duration::try_from(by).unwrap_or(time::Duration::MAX))
}

impl PgWebhookStore {
    async fn owned(&self, tenant: &TenantId, id: &EndpointId) -> StoreResult<bool> {
        Ok(sqlx::query(
            "SELECT 1 FROM wa_server_webhook_endpoints WHERE id = $1 AND tenant_id = $2",
        )
        .bind(id.as_str())
        .bind(tenant.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?
        .is_some())
    }
}

#[async_trait]
impl WebhookStore for PgWebhookStore {
    async fn create_endpoint(
        &self,
        endpoint: &NewEndpoint,
        max: usize,
    ) -> StoreResult<CreateOutcome> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let exists = sqlx::query("SELECT 1 FROM wa_server_tenants WHERE id = $1 FOR UPDATE")
            .bind(endpoint.tenant.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
        if exists.is_none() {
            return Ok(CreateOutcome::NoSuchTenant);
        }
        let held: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM wa_server_webhook_endpoints WHERE tenant_id = $1",
        )
        .bind(endpoint.tenant.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;
        if usize::try_from(held).unwrap_or(usize::MAX) >= max {
            return Ok(CreateOutcome::LimitReached);
        }
        if let Some(named) = &endpoint.tenants {
            // Shared locks: a named tenant is not deleted until this
            // commits (its deletion would wait), and one gone already is
            // refused, nothing created.
            let names: Vec<String> = named.iter().map(|t| t.tenant.as_str().to_owned()).collect();
            let found: Vec<String> =
                sqlx::query_scalar("SELECT id FROM wa_server_tenants WHERE id = ANY($1) FOR SHARE")
                    .bind(&names)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(backend)?;
            if found.len() != names.len() {
                return Ok(CreateOutcome::NamedTenantGone);
            }
        }
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO wa_server_webhook_endpoints \
             (id, tenant_id, url, event_types, enabled, secret, cursor, created_at, updated_at, \
              named_tenants) \
             VALUES ($1, $2, $3, $4, true, $5, $6, $7, $7, $8) RETURNING {ENDPOINT_COLUMNS}"
        )))
        .bind(endpoint.id.as_str())
        .bind(endpoint.tenant.as_str())
        .bind(&endpoint.url)
        .bind(&endpoint.types)
        .bind(endpoint.secret.as_stored())
        .bind(endpoint.cursor)
        .bind(endpoint.now)
        .bind(endpoint.tenants.is_some())
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;
        for named in endpoint.tenants.iter().flatten() {
            sqlx::query(
                "INSERT INTO wa_server_webhook_endpoint_tenants (endpoint_id, tenant_id, cursor) \
                 VALUES ($1, $2, $3)",
            )
            .bind(endpoint.id.as_str())
            .bind(named.tenant.as_str())
            .bind(named.cursor)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        let mut made = endpoint_row(&row)?;
        fill_tenants(&mut *tx, std::slice::from_mut(&mut made)).await?;
        tx.commit().await.map_err(backend)?;
        Ok(CreateOutcome::Created(Box::new(made)))
    }

    async fn endpoint(&self, tenant: &TenantId, id: &EndpointId) -> StoreResult<Option<Endpoint>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {ENDPOINT_COLUMNS} FROM wa_server_webhook_endpoints \
             WHERE id = $1 AND tenant_id = $2"
        )))
        .bind(id.as_str())
        .bind(tenant.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        Ok(endpoints_of(&self.pool, &rows).await?.pop())
    }

    async fn endpoints(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<Endpoint>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {ENDPOINT_COLUMNS} FROM wa_server_webhook_endpoints \
             WHERE tenant_id = $1 AND ($2::text IS NULL OR id > $2) ORDER BY id LIMIT $3"
        )))
        .bind(tenant.as_str())
        .bind(page.after.as_deref())
        .bind(i64::try_from(page.limit + 1).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let items = endpoints_of(&self.pool, &rows).await?;
        Ok(listing(items, page.limit, |e| e.id.as_str().to_owned()))
    }

    async fn update_endpoint(
        &self,
        tenant: &TenantId,
        id: &EndpointId,
        change: &EndpointChange,
        now: OffsetDateTime,
    ) -> StoreResult<Option<Endpoint>> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let has_set: Option<bool> = sqlx::query_scalar(
            "SELECT named_tenants FROM wa_server_webhook_endpoints \
             WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(id.as_str())
        .bind(tenant.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        let Some(has_set) = has_set else {
            return Ok(None);
        };
        // The set the caller was checked against must still be the
        // endpoint's (D342): read under the row's lock, so no other change
        // of the set slips in between.
        let current: Option<Vec<TenantCursor>> = if has_set {
            let rows: Vec<String> = sqlx::query_scalar(
                "SELECT tenant_id FROM wa_server_webhook_endpoint_tenants WHERE endpoint_id = $1",
            )
            .bind(id.as_str())
            .fetch_all(&mut *tx)
            .await
            .map_err(backend)?;
            Some(
                rows.iter()
                    .map(|t| {
                        TenantId::parse(t)
                            .map(|tenant| TenantCursor { tenant, cursor: 0 })
                            .ok_or_else(|| corrupt("tenant_id"))
                    })
                    .collect::<StoreResult<Vec<_>>>()?,
            )
        } else {
            None
        };
        if !reached_through_holds(current.as_deref(), change.reached_through.as_deref()) {
            return Ok(None);
        }
        if let (true, Some(set)) = (has_set, &change.tenants) {
            let names: Vec<String> = set.iter().map(|t| t.tenant.as_str().to_owned()).collect();
            // A tenant left out stops: its row first (a fan-out holding it
            // finishes before this goes on), then its pending deliveries.
            sqlx::query(
                "DELETE FROM wa_server_webhook_endpoint_tenants \
                 WHERE endpoint_id = $1 AND NOT (tenant_id = ANY($2))",
            )
            .bind(id.as_str())
            .bind(&names)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            sqlx::query(
                "DELETE FROM wa_server_webhook_deliveries \
                 WHERE endpoint_id = $1 AND status = 'pending' AND event_id IS NOT NULL \
                   AND NOT (tenant_id = ANY($2))",
            )
            .bind(id.as_str())
            .bind(&names)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            // A tenant added starts at the cursor given; one that does not
            // exist (deleted meanwhile) is left out, never created.
            for added in set {
                sqlx::query(
                    "INSERT INTO wa_server_webhook_endpoint_tenants (endpoint_id, tenant_id, cursor) \
                     SELECT $1, t.id, $3 FROM wa_server_tenants t WHERE t.id = $2 \
                     ON CONFLICT (endpoint_id, tenant_id) DO NOTHING",
                )
                .bind(id.as_str())
                .bind(added.tenant.as_str())
                .bind(added.cursor)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
        }
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE wa_server_webhook_endpoints SET \
               url = COALESCE($3, url), \
               event_types = COALESCE($4, event_types), \
               enabled = COALESCE($5, enabled), \
               disabled_reason = CASE WHEN $5 THEN NULL ELSE disabled_reason END, \
               failing_since = CASE WHEN $5 THEN NULL ELSE failing_since END, \
               updated_at = $6 \
             WHERE id = $1 AND tenant_id = $2 RETURNING {ENDPOINT_COLUMNS}"
        )))
        .bind(id.as_str())
        .bind(tenant.as_str())
        .bind(change.url.as_deref())
        .bind(change.types.as_deref())
        .bind(change.enabled)
        .bind(now)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        let updated = endpoints_of(&mut *tx, &rows).await?.pop();
        tx.commit().await.map_err(backend)?;
        Ok(updated)
    }

    async fn rotate_secret(
        &self,
        tenant: &TenantId,
        id: &EndpointId,
        secret: &SealedSecret,
        previous_until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> StoreResult<Option<Endpoint>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE wa_server_webhook_endpoints SET \
               previous_secret = secret, previous_secret_until = $4, secret = $3, updated_at = $5 \
             WHERE id = $1 AND tenant_id = $2 RETURNING {ENDPOINT_COLUMNS}"
        )))
        .bind(id.as_str())
        .bind(tenant.as_str())
        .bind(secret.as_stored())
        .bind(previous_until)
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        Ok(endpoints_of(&self.pool, &rows).await?.pop())
    }

    async fn delete_endpoint(&self, tenant: &TenantId, id: &EndpointId) -> StoreResult<bool> {
        let done =
            sqlx::query("DELETE FROM wa_server_webhook_endpoints WHERE id = $1 AND tenant_id = $2")
                .bind(id.as_str())
                .bind(tenant.as_str())
                .execute(&self.pool)
                .await
                .map_err(backend)?;
        Ok(done.rows_affected() > 0)
    }

    async fn active_endpoints(&self) -> StoreResult<Vec<Endpoint>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {ENDPOINT_COLUMNS} FROM wa_server_webhook_endpoints WHERE enabled ORDER BY id"
        )))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        endpoints_of(&self.pool, &rows).await
    }

    async fn fan_out(
        &self,
        endpoint: &EndpointId,
        tenant: &TenantId,
        from: i64,
        to: i64,
        deliveries: &[NewDelivery],
    ) -> StoreResult<bool> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        // The row holding the stream's cursor, locked while it is `from`:
        // the endpoint's own (an endpoint of its tenant's own events), or
        // the tenant's row of an endpoint of named tenants.
        let own = sqlx::query(
            "SELECT 1 FROM wa_server_webhook_endpoints \
             WHERE id = $1 AND tenant_id = $2 AND cursor = $3 AND NOT named_tenants FOR UPDATE",
        )
        .bind(endpoint.as_str())
        .bind(tenant.as_str())
        .bind(from)
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?
        .is_some();
        if !own {
            let named = sqlx::query(
                "SELECT 1 FROM wa_server_webhook_endpoint_tenants \
                 WHERE endpoint_id = $1 AND tenant_id = $2 AND cursor = $3 FOR UPDATE",
            )
            .bind(endpoint.as_str())
            .bind(tenant.as_str())
            .bind(from)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?
            .is_some();
            if !named {
                return Ok(false);
            }
        }
        for delivery in deliveries {
            sqlx::query(
                "INSERT INTO wa_server_webhook_deliveries \
                 (id, endpoint_id, tenant_id, event_id, sequence, event_type, status, \
                  next_attempt_at, give_up_at, created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7, $8, $7) \
                 ON CONFLICT (endpoint_id, event_id) DO NOTHING",
            )
            .bind(delivery.id.as_str())
            .bind(endpoint.as_str())
            .bind(tenant.as_str())
            .bind(delivery.event_id.as_deref())
            .bind(delivery.sequence)
            .bind(&delivery.event_type)
            .bind(delivery.created_at)
            .bind(delivery.give_up_at)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        if own {
            sqlx::query("UPDATE wa_server_webhook_endpoints SET cursor = $2 WHERE id = $1")
                .bind(endpoint.as_str())
                .bind(to)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        } else {
            sqlx::query(
                "UPDATE wa_server_webhook_endpoint_tenants SET cursor = $3 \
                 WHERE endpoint_id = $1 AND tenant_id = $2",
            )
            .bind(endpoint.as_str())
            .bind(tenant.as_str())
            .bind(to)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(true)
    }

    async fn insert_delivery(
        &self,
        tenant: &TenantId,
        endpoint: &EndpointId,
        delivery: &NewDelivery,
    ) -> StoreResult<Option<Delivery>> {
        // The tenant from the endpoint's own row: nothing for another's.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO wa_server_webhook_deliveries \
             (id, endpoint_id, tenant_id, event_id, sequence, event_type, status, \
              next_attempt_at, give_up_at, created_at) \
             SELECT $1, e.id, e.tenant_id, $4, $5, $6, 'pending', $7, $8, $7 \
             FROM wa_server_webhook_endpoints e WHERE e.id = $2 AND e.tenant_id = $3 \
             RETURNING {DELIVERY_COLUMNS}"
        )))
        .bind(delivery.id.as_str())
        .bind(endpoint.as_str())
        .bind(tenant.as_str())
        .bind(delivery.event_id.as_deref())
        .bind(delivery.sequence)
        .bind(&delivery.event_type)
        .bind(delivery.created_at)
        .bind(delivery.give_up_at)
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?
        .as_ref()
        .map(delivery_row)
        .transpose()
    }

    async fn claim(&self, request: &ClaimRequest) -> StoreResult<Vec<Claimed>> {
        let busy_endpoints: Vec<String> = request
            .busy_endpoints
            .iter()
            .map(|e| e.as_str().to_owned())
            .collect();
        let busy_tenants: Vec<String> = request
            .busy_tenants
            .iter()
            .map(|t| t.as_str().to_owned())
            .collect();
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "WITH picked AS ( \
               SELECT d.id FROM ( \
                 SELECT u.tenant_id FROM ( \
                   SELECT e.tenant_id FROM wa_server_webhook_endpoints e WHERE e.enabled \
                   UNION \
                   SELECT c.tenant_id FROM wa_server_webhook_endpoint_tenants c \
                   JOIN wa_server_webhook_endpoints e ON e.id = c.endpoint_id WHERE e.enabled \
                 ) u JOIN wa_server_tenants t ON t.id = u.tenant_id \
                 WHERE t.status = 'active' AND NOT (u.tenant_id = ANY($3)) \
               ) tn \
               CROSS JOIN LATERAL ( \
                 SELECT d.id, d.next_attempt_at FROM wa_server_webhook_deliveries d \
                 JOIN wa_server_webhook_endpoints e ON e.id = d.endpoint_id \
                 WHERE d.tenant_id = tn.tenant_id AND d.status = 'pending' \
                   AND d.next_attempt_at <= $1 \
                   AND (d.lease_until IS NULL OR d.lease_until <= $1) \
                   AND e.enabled AND NOT (d.endpoint_id = ANY($4)) \
                 ORDER BY d.next_attempt_at, d.id LIMIT 1 \
                 FOR UPDATE OF d SKIP LOCKED \
               ) d \
               ORDER BY d.next_attempt_at, d.id LIMIT $5 \
             ) \
             UPDATE wa_server_webhook_deliveries d SET lease_until = $2, claim = $6 \
             FROM picked WHERE d.id = picked.id \
             RETURNING {}",
            DELIVERY_COLUMNS
                .split(", ")
                .map(|c| format!("d.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        )))
        .bind(request.now)
        .bind(after(request.now, request.lease))
        .bind(&busy_tenants)
        .bind(&busy_endpoints)
        .bind(i64::try_from(request.limit).unwrap_or(i64::MAX))
        .bind(&request.token.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        let deliveries = rows
            .iter()
            .map(delivery_row)
            .collect::<StoreResult<Vec<_>>>()?;
        let ids: Vec<String> = deliveries
            .iter()
            .map(|d| d.endpoint.as_str().to_owned())
            .collect();
        let endpoint_rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {ENDPOINT_COLUMNS} FROM wa_server_webhook_endpoints WHERE id = ANY($1)"
        )))
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        let endpoints = endpoints_of(&mut *tx, &endpoint_rows).await?;
        tx.commit().await.map_err(backend)?;
        Ok(deliveries
            .into_iter()
            .filter_map(|delivery| {
                let endpoint = endpoints
                    .iter()
                    .find(|e| e.id == delivery.endpoint)?
                    .clone();
                Some(Claimed {
                    delivery,
                    endpoint,
                    claim: request.token.clone(),
                })
            })
            .collect())
    }

    async fn settle(&self, attempt: &Attempt, now: OffsetDateTime) -> StoreResult<bool> {
        let (status, next) = match attempt.outcome {
            Outcome::Delivered => (DeliveryStatus::Delivered, None),
            Outcome::Retry(at) => (DeliveryStatus::Pending, Some(at)),
            Outcome::Failed => (DeliveryStatus::Failed, None),
        };
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let endpoint: Option<String> = sqlx::query_scalar(
            "UPDATE wa_server_webhook_deliveries SET \
               claim = NULL, lease_until = NULL, \
               attempts = attempts + CASE WHEN $3 THEN 1 ELSE 0 END, \
               last_attempt_at = CASE WHEN $3 THEN $4 ELSE last_attempt_at END, \
               last_duration_ms = CASE WHEN $3 THEN $5 ELSE last_duration_ms END, \
               last_status = CASE WHEN $3 THEN $6 ELSE last_status END, \
               last_error = $7, status = $8, next_attempt_at = $9 \
             WHERE id = $1 AND claim = $2 AND status = 'pending' RETURNING endpoint_id",
        )
        .bind(attempt.delivery.as_str())
        .bind(&attempt.claim.0)
        .bind(attempt.counted)
        .bind(attempt.started_at)
        .bind(i32::try_from(attempt.duration_ms).unwrap_or(i32::MAX))
        .bind(attempt.status.map(i32::from))
        .bind(attempt.error.as_ref().map(AttemptError::as_str))
        .bind(status.as_str())
        .bind(next)
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        let Some(endpoint) = endpoint else {
            return Ok(false);
        };
        if attempt.counted {
            if attempt.outcome == Outcome::Delivered {
                sqlx::query(
                    "UPDATE wa_server_webhook_endpoints SET failing_since = NULL WHERE id = $1",
                )
                .bind(&endpoint)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            } else {
                sqlx::query(
                    "UPDATE wa_server_webhook_endpoints SET \
                       failing_since = COALESCE(failing_since, $2), \
                       disabled_reason = CASE WHEN enabled AND $3::timestamptz IS NOT NULL \
                         AND COALESCE(failing_since, $2) <= $3 THEN 'failing' \
                         ELSE disabled_reason END, \
                       updated_at = CASE WHEN enabled AND $3::timestamptz IS NOT NULL \
                         AND COALESCE(failing_since, $2) <= $3 THEN $4 ELSE updated_at END, \
                       enabled = CASE WHEN $3::timestamptz IS NOT NULL \
                         AND COALESCE(failing_since, $2) <= $3 THEN false ELSE enabled END \
                     WHERE id = $1",
                )
                .bind(&endpoint)
                .bind(attempt.started_at)
                .bind(attempt.disable_if_failing_since)
                .bind(now)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
        }
        tx.commit().await.map_err(backend)?;
        Ok(true)
    }

    async fn deliveries(
        &self,
        tenant: &TenantId,
        endpoint: &EndpointId,
        page: &PageRequest,
    ) -> StoreResult<Option<Listing<Delivery>>> {
        if !self.owned(tenant, endpoint).await? {
            return Ok(None);
        }
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {DELIVERY_COLUMNS} FROM wa_server_webhook_deliveries \
             WHERE endpoint_id = $1 AND ($2::text IS NULL OR id < $2) \
             ORDER BY id DESC LIMIT $3"
        )))
        .bind(endpoint.as_str())
        .bind(page.after.as_deref())
        .bind(i64::try_from(page.limit + 1).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let items = rows
            .iter()
            .map(delivery_row)
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(Some(listing(items, page.limit, |d| {
            d.id.as_str().to_owned()
        })))
    }

    async fn retry_delivery(
        &self,
        tenant: &TenantId,
        endpoint: &EndpointId,
        id: &DeliveryId,
        now: OffsetDateTime,
        give_up_at: OffsetDateTime,
    ) -> StoreResult<Option<Delivery>> {
        if !self.owned(tenant, endpoint).await? {
            return Ok(None);
        }
        sqlx::query(
            "UPDATE wa_server_webhook_deliveries SET status = 'pending', next_attempt_at = $3, \
               give_up_at = $4, lease_until = NULL, claim = NULL \
             WHERE id = $1 AND endpoint_id = $2 AND status <> 'pending'",
        )
        .bind(id.as_str())
        .bind(endpoint.as_str())
        .bind(now)
        .bind(give_up_at)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {DELIVERY_COLUMNS} FROM wa_server_webhook_deliveries \
             WHERE id = $1 AND endpoint_id = $2"
        )))
        .bind(id.as_str())
        .bind(endpoint.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?
        .as_ref()
        .map(delivery_row)
        .transpose()
    }

    async fn purge_deliveries(&self, before: OffsetDateTime) -> StoreResult<u64> {
        let done = sqlx::query(
            "DELETE FROM wa_server_webhook_deliveries \
             WHERE status <> 'pending' AND COALESCE(last_attempt_at, created_at) < $1",
        )
        .bind(before)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(done.rows_affected())
    }

    async fn pending(&self, now: OffsetDateTime) -> StoreResult<PendingDeliveries> {
        // The partial index on pending rows answers it alone.
        let row = sqlx::query(
            "SELECT count(*) AS pending, \
                    min(next_attempt_at) FILTER (WHERE next_attempt_at <= $1) AS oldest_due \
             FROM wa_server_webhook_deliveries WHERE status = 'pending'",
        )
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        let count: i64 = get(&row, "pending")?;
        let oldest_due: Option<OffsetDateTime> = get(&row, "oldest_due")?;
        Ok(PendingDeliveries {
            count: u64::try_from(count).unwrap_or(0),
            oldest_due,
        })
    }
}
