//! The Postgres backend ([`PgBackend`]): [`PgStore`], the service's tables
//! on Postgres; [`migrate`] ([`PgMigrator`]), which creates or upgrades
//! them together with the library's; [`PgLeaderLock`] (advisory locks)
//! and [`PgJanitor`] (the library's expired key/value rows).
//!
//! Every query is a runtime query on the sqlx the facade re-exports
//! (`meta_whatsapp_rs::adapters::store::postgres::sqlx`), so building never
//! needs a database. Times come from the database server's `now()`, like
//! the library's stores.

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::migrate::{
    Migration, MigrationType, Migrator,
};
use meta_whatsapp_rs::adapters::store::postgres::sqlx::postgres::PgRow;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::{
    self, PgPool, Postgres, Row, SqlSafeStr, Transaction,
};
use meta_whatsapp_rs::adapters::store::postgres::{self as library};
use meta_whatsapp_rs::adapters::store::{PostgresConversationStore, PostgresKvStore};
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore, Retention};
use sha2::{Digest, Sha256};

use meta_whatsapp_server_core::backend::{Backend, BackendKind};
use meta_whatsapp_server_core::outbox::Outbox;
use meta_whatsapp_server_core::store::{
    IdempotencyRecords, Janitor, LeaderLock, LeaderTurn, RecordStore, SchemaMigrator, StoreResult,
    Turn, listing,
};

use crate::events::{PgEventStore, busy_or_backend};
use crate::notify::PgNotifier;
use crate::webhooks::PgWebhookStore;
use meta_whatsapp_server_core::model::{
    AllowedTenants, ApiKeyRecord, BindOutcome, BindingEpoch, DeleteTenantOutcome, IdempotencyClaim,
    IdempotencyKey, IdempotencyRecord, IdempotencyState, KeyOwner, KeyScope, Listing, NewApiKey,
    NewSignupAttempt, NumberBinding, NumberStatus, OtpSettings, PageRequest, Scope, SignupAttempt,
    Tenant, TenantId, TenantStatus, VaultEntry, WabaBinding,
};
use meta_whatsapp_server_core::notify::EventNotifier;
use meta_whatsapp_server_core::webhooks_out::store::WebhookStore;

/// The service's migration history table, apart from the library's
/// (`wa_sqlx_migrations`) and from any of the application's own.
pub const MIGRATIONS_TABLE: &str = "wa_server_sqlx_migrations";

/// The advisory lock the service holds while it migrates: the library's
/// migrations, then its own, as one step, so two replicas starting at once
/// never interleave them. sqlx takes its own lock inside each run; this one
/// spans both. The value is the first eight bytes of
/// SHA-256(`meta-whatsapp-server/migrate`), as a big-endian `i64`.
pub const MIGRATION_LOCK: i64 = 0x4609_2c1b_ffac_b625;

/// `(version, description, SQL)` of each service migration, in order.
/// Stable, byte for byte, once released: sqlx records each file's checksum.
const MIGRATION_FILES: &[(i64, &str, &str)] = &[
    (
        1,
        "tenants keys bindings",
        include_str!("../migrations/0001_tenants_keys_bindings.sql"),
    ),
    (
        2,
        "idempotency",
        include_str!("../migrations/0002_idempotency.sql"),
    ),
    (3, "events", include_str!("../migrations/0003_events.sql")),
    (
        4,
        "waba moves",
        include_str!("../migrations/0004_waba_moves.sql"),
    ),
    (
        5,
        "number moves",
        include_str!("../migrations/0005_number_moves.sql"),
    ),
    (6, "signup", include_str!("../migrations/0006_signup.sql")),
    (
        7,
        "received media",
        include_str!("../migrations/0007_received_media.sql"),
    ),
    (8, "erasure", include_str!("../migrations/0008_erasure.sql")),
    // Roadmap M2c, numbered after roadmap M2f's 8.
    (
        9,
        "webhooks out",
        include_str!("../migrations/0009_webhooks_out.sql"),
    ),
    // Roadmap M5j2: an operator's idempotency keys.
    (
        10,
        "admin idempotency",
        include_str!("../migrations/0010_admin_idempotency.sql"),
    ),
    // Roadmap M2m, after M5j2's 10.
    (
        11,
        "webhook endpoint tenants",
        include_str!("../migrations/0011_webhook_endpoint_tenants.sql"),
    ),
];

/// The advisory lock a housekeeping round runs under: the key of the
/// [`PgLeaderLock`] turn named [`HOUSEKEEPING`](meta_whatsapp_server_core::store::HOUSEKEEPING)
/// (docs/design/server.md, section 2.4: any replica, one at a time; the
/// others skip that round). The first eight bytes of
/// SHA-256(`meta-whatsapp-server/housekeeping`), as a big-endian `i64`.
/// The purges took it themselves before roadmap S2; a replica of that
/// release and one of this take the same key, so they still take turns
/// during a rolling deploy.
pub const HOUSEKEEPING_LOCK: i64 = 0x0662_5bd9_6d85_d1cf;

/// The advisory lock key of the service's lock `name`: the first eight
/// bytes of SHA-256(`meta-whatsapp-server/<name>`), as a big-endian `i64`.
/// [`HOUSEKEEPING_LOCK`] is `lock_key("housekeeping")`
/// ([`HOUSEKEEPING`](meta_whatsapp_server_core::store::HOUSEKEEPING)), [`MIGRATION_LOCK`] `lock_key("migrate")`.
pub fn lock_key(name: &str) -> i64 {
    let digest = Sha256::digest(format!("meta-whatsapp-server/{name}").as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    i64::from_be_bytes(first)
}

/// The service's migrations, as sqlx runs and records them.
pub fn migrations() -> Vec<Migration> {
    MIGRATION_FILES
        .iter()
        .map(|(version, description, sql)| {
            Migration::new(
                *version,
                Cow::Borrowed(*description),
                MigrationType::Simple,
                (*sql).into_sql_str(),
                false,
            )
        })
        .collect()
}

/// Create or upgrade the library's tables (default `wa_` prefix) and the
/// service's (`wa_server_*`), under [`MIGRATION_LOCK`]. Idempotent; safe
/// to run from several replicas at once. Needs a pool of at least two
/// connections (one holds the lock).
pub async fn migrate(pool: &PgPool) -> StoreResult<()> {
    let mut lock = pool.acquire().await.map_err(backend)?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *lock)
        .await
        .map_err(backend)?;
    let result = run_migrations(pool).await;
    let unlocked = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *lock)
        .await;
    if unlocked.is_err() {
        // A session lock outlives the statement: never hand this connection
        // back to the pool still holding it.
        let _ = lock.close().await;
    }
    result
}

async fn run_migrations(pool: &PgPool) -> StoreResult<()> {
    library::migrate(pool).await?;
    let mut migrator = Migrator::with_migrations(migrations());
    migrator.dangerous_set_table_name(MIGRATIONS_TABLE);
    migrator.run(pool).await.map_err(backend)
}

/// The service's records on Postgres. Cheap to clone.
#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl std::fmt::Debug for PgStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The pool's options could reveal connection settings.
        f.debug_struct("PgStore").finish_non_exhaustive()
    }
}

impl PgStore {
    /// A store on `pool`, whose database [`migrate`] has run on.
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

fn tenant_id(value: &str, column: &'static str) -> StoreResult<TenantId> {
    TenantId::parse(value).ok_or_else(|| corrupt(column))
}

fn tenant_row(row: &PgRow) -> StoreResult<Tenant> {
    Ok(Tenant {
        id: tenant_id(&get::<String>(row, "id")?, "id")?,
        name: get(row, "name")?,
        status: TenantStatus::parse(&get::<String>(row, "status")?)
            .ok_or_else(|| corrupt("status"))?,
        created_at: get(row, "created_at")?,
        updated_at: get(row, "updated_at")?,
    })
}

const KEY_COLUMNS: &str = "key_id, secret_sha256, kind, tenant_id, all_tenants, allowed_tenants, \
     scopes, name, created_at, expires_at, revoked_at, last_used_at";

fn key_row(row: &PgRow) -> StoreResult<ApiKeyRecord> {
    let digest: Vec<u8> = get(row, "secret_sha256")?;
    let secret_sha256: [u8; 32] = digest.try_into().map_err(|_| corrupt("secret_sha256"))?;
    let kind: String = get(row, "kind")?;
    let owner = match kind.as_str() {
        "tenant" => {
            let tenant: Option<String> = get(row, "tenant_id")?;
            KeyOwner::Tenant(tenant_id(
                &tenant.ok_or_else(|| corrupt("tenant_id"))?,
                "tenant_id",
            )?)
        }
        "platform" => {
            if get::<bool>(row, "all_tenants")? {
                KeyOwner::Platform(AllowedTenants::All)
            } else {
                let list: Vec<String> = get(row, "allowed_tenants")?;
                KeyOwner::Platform(AllowedTenants::Only(
                    list.iter()
                        .map(|t| tenant_id(t, "allowed_tenants"))
                        .collect::<StoreResult<_>>()?,
                ))
            }
        }
        "admin" => KeyOwner::Admin,
        _ => return Err(corrupt("kind")),
    };
    let scopes: Vec<String> = get(row, "scopes")?;
    Ok(ApiKeyRecord {
        key_id: get(row, "key_id")?,
        secret_sha256,
        owner,
        scopes: scopes
            .iter()
            .map(|s| Scope::parse(s).ok_or_else(|| corrupt("scopes")))
            .collect::<StoreResult<_>>()?,
        name: get(row, "name")?,
        created_at: get(row, "created_at")?,
        expires_at: get(row, "expires_at")?,
        revoked_at: get(row, "revoked_at")?,
        last_used_at: get(row, "last_used_at")?,
    })
}

/// The columns of a WABA's binding, as [`waba_row`] reads them.
const WABA_COLUMNS: &str = "waba_id, tenant_id, credit_allocation_id, attached_at, generation, \
     previous_tenant_unbound_at";

fn waba_row(row: &PgRow) -> StoreResult<WabaBinding> {
    Ok(WabaBinding {
        waba_id: WabaId::new_unchecked(get::<String>(row, "waba_id")?),
        tenant_id: tenant_id(&get::<String>(row, "tenant_id")?, "tenant_id")?,
        credit_allocation_id: get(row, "credit_allocation_id")?,
        attached_at: get(row, "attached_at")?,
        generation: get(row, "generation")?,
        previous_tenant_unbound_at: get(row, "previous_tenant_unbound_at")?,
    })
}

/// Delete the binding of the WABA `$1` matching `condition` (over its
/// columns, the WABA's `$1` and further parameters), its numbers with it
/// (ON DELETE CASCADE), and leave its tombstone in the same statement:
/// the tenant it was unbound from, now, and the binding's
/// `previous_tenant_unbound_at` (roadmap S2b). Each number's tombstone
/// (roadmap M2h) is left by migration 5's trigger,
/// `wa_server_numbers_released`, in the same statement: for every row
/// deleted from `wa_server_numbers` (this cascade, a binding that no
/// longer lists it, and the same deletions by a replica of the previous
/// release during a rolling deploy, which writes no tombstone of its own),
/// the tenant, now, and its `previous_tenant_released_at` replace the
/// tombstone the number had. Answers how many bindings went (0 or 1).
fn unbind_sql(condition: &str) -> String {
    format!(
        "WITH gone AS ( \
           DELETE FROM wa_server_wabas WHERE waba_id = $1 {condition} \
           RETURNING waba_id, tenant_id, previous_tenant_unbound_at \
         ), kept AS ( \
           INSERT INTO wa_server_waba_tombstones \
             (waba_id, tenant_id, unbound_at, previous_tenant_unbound_at) \
           SELECT waba_id, tenant_id, now(), previous_tenant_unbound_at FROM gone \
           ON CONFLICT (waba_id) DO UPDATE SET tenant_id = EXCLUDED.tenant_id, \
             unbound_at = EXCLUDED.unbound_at, \
             previous_tenant_unbound_at = EXCLUDED.previous_tenant_unbound_at \
           RETURNING 1 \
         ) \
         SELECT count(*) FROM gone"
    )
}

/// The columns of a number's binding, as [`number_row`] reads them, from
/// `wa_server_numbers n`: a row written before migration 5 has no
/// `attached_at` of its own, and reads its WABA's (when the binding that
/// lists it began).
const NUMBER_COLUMNS: &str = "n.phone_number_id, n.waba_id, n.tenant_id, n.status, n.updated_at, \
     COALESCE(n.attached_at, (SELECT w.attached_at FROM wa_server_wabas w \
       WHERE w.waba_id = n.waba_id)) AS attached_at, \
     n.previous_tenant_released_at";

fn number_row(row: &PgRow) -> StoreResult<NumberBinding> {
    Ok(NumberBinding {
        phone_number_id: PhoneNumberId::new_unchecked(get::<String>(row, "phone_number_id")?),
        waba_id: WabaId::new_unchecked(get::<String>(row, "waba_id")?),
        tenant_id: tenant_id(&get::<String>(row, "tenant_id")?, "tenant_id")?,
        status: NumberStatus::parse(&get::<String>(row, "status")?)
            .ok_or_else(|| corrupt("status"))?,
        updated_at: get(row, "updated_at")?,
        attached_at: get(row, "attached_at")?,
        previous_tenant_released_at: get(row, "previous_tenant_released_at")?,
    })
}

/// The columns of a signup attempt, as [`attempt_row`] reads them.
const ATTEMPT_COLUMNS: &str =
    "waba_id, tenant_id, failed_step, session, finish_kind, created_at, updated_at";

fn attempt_row(row: &PgRow) -> StoreResult<SignupAttempt> {
    Ok(SignupAttempt {
        waba_id: WabaId::new_unchecked(get::<String>(row, "waba_id")?),
        tenant_id: tenant_id(&get::<String>(row, "tenant_id")?, "tenant_id")?,
        failed_step: get(row, "failed_step")?,
        session: get(row, "session")?,
        finish_kind: get(row, "finish_kind")?,
        created_at: get(row, "created_at")?,
        updated_at: get(row, "updated_at")?,
    })
}

/// A vault entry's row (`wa_server_vault_records`): a kind or an id this
/// release did not write is a storage failure.
fn vault_entry_row(row: &PgRow) -> StoreResult<VaultEntry> {
    let kind: String = get(row, "kind")?;
    let id: String = get(row, "id")?;
    VaultEntry::parse(&kind, &id).ok_or_else(|| corrupt("kind"))
}

/// `LIMIT` for a page: one more than asked, to know whether more follow.
fn fetch_limit(page: &PageRequest) -> i64 {
    i64::try_from(page.limit.saturating_add(1)).unwrap_or(i64::MAX)
}

/// The `kind` column and owner fields of a key scope.
fn scope_filter(scope: &KeyScope) -> (&'static str, Option<&str>) {
    match scope {
        KeyScope::Tenant(tenant) => ("tenant", Some(tenant.as_str())),
        KeyScope::Platform => ("platform", None),
        KeyScope::Admin => ("admin", None),
    }
}

#[async_trait]
impl RecordStore for PgStore {
    async fn ping(&self) -> StoreResult<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(backend)
            .map(|_| ())
    }

    async fn create_tenant(&self, id: &TenantId, name: &str) -> StoreResult<Option<Tenant>> {
        let row = sqlx::query(
            "INSERT INTO wa_server_tenants (id, name) VALUES ($1, $2) \
             ON CONFLICT (id) DO NOTHING \
             RETURNING id, name, status, created_at, updated_at",
        )
        .bind(id.as_str())
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        row.as_ref().map(tenant_row).transpose()
    }

    async fn tenant(&self, id: &TenantId) -> StoreResult<Option<Tenant>> {
        let row = sqlx::query(
            "SELECT id, name, status, created_at, updated_at FROM wa_server_tenants WHERE id = $1",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        row.as_ref().map(tenant_row).transpose()
    }

    async fn tenants(&self, page: &PageRequest) -> StoreResult<Listing<Tenant>> {
        let rows = sqlx::query(
            "SELECT id, name, status, created_at, updated_at FROM wa_server_tenants \
             WHERE $1::text IS NULL OR id > $1 ORDER BY id LIMIT $2",
        )
        .bind(page.after.as_deref())
        .bind(fetch_limit(page))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let items = rows
            .iter()
            .map(tenant_row)
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(listing(items, page.limit, |t| t.id.as_str().to_owned()))
    }

    async fn update_tenant(
        &self,
        id: &TenantId,
        name: Option<&str>,
        status: Option<TenantStatus>,
    ) -> StoreResult<Option<Tenant>> {
        let row = sqlx::query(
            "UPDATE wa_server_tenants SET name = COALESCE($2, name), \
             status = COALESCE($3, status), updated_at = now() WHERE id = $1 \
             RETURNING id, name, status, created_at, updated_at",
        )
        .bind(id.as_str())
        .bind(name)
        .bind(status.map(TenantStatus::as_str))
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        row.as_ref().map(tenant_row).transpose()
    }

    async fn otp_settings(&self, id: &TenantId) -> StoreResult<Option<OtpSettings>> {
        // The key `otp` of the tenant's `settings` (roadmap M3c): SQL NULL
        // when it has none, or there is no such tenant.
        let row =
            sqlx::query("SELECT settings -> 'otp' AS otp FROM wa_server_tenants WHERE id = $1")
                .bind(id.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        let Some(row) = row else {
            return Ok(None);
        };
        match get::<Option<serde_json::Value>>(&row, "otp")? {
            None => Ok(None),
            Some(stored) => OtpSettings::from_stored(&stored)
                .map(Some)
                .ok_or_else(|| corrupt("settings")),
        }
    }

    async fn set_otp_settings(
        &self,
        id: &TenantId,
        settings: Option<&OtpSettings>,
    ) -> StoreResult<bool> {
        let done =
            match settings {
                Some(settings) => {
                    sqlx::query(
                        "UPDATE wa_server_tenants SET settings = jsonb_set(settings, '{otp}', $2), \
                     updated_at = now() WHERE id = $1",
                    )
                    .bind(id.as_str())
                    .bind(settings.to_stored())
                    .execute(&self.pool)
                    .await
                }
                None => sqlx::query(
                    "UPDATE wa_server_tenants SET settings = settings - 'otp', updated_at = now() \
                     WHERE id = $1",
                )
                .bind(id.as_str())
                .execute(&self.pool)
                .await,
            }
            .map_err(backend)?;
        Ok(done.rows_affected() == 1)
    }

    async fn delete_tenant(&self, id: &TenantId) -> StoreResult<DeleteTenantOutcome> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let exists = sqlx::query("SELECT 1 FROM wa_server_tenants WHERE id = $1 FOR UPDATE")
            .bind(id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
        if exists.is_none() {
            return Ok(DeleteTenantOutcome::NotFound);
        }
        let has_wabas = sqlx::query("SELECT 1 FROM wa_server_wabas WHERE tenant_id = $1 LIMIT 1")
            .bind(id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
        if has_wabas.is_some() {
            return Ok(DeleteTenantOutcome::HasWabas);
        }
        // Keys, idempotency records and events go with it (ON DELETE
        // CASCADE), and the tombstones of the WABAs it was unbound from
        // forget it (ON DELETE SET NULL: a tenant created again under its
        // id is another tenant to them, roadmap S2b); its event stream
        // records them purged, so a tenant created later with the same id
        // goes on after them and an old cursor is `410 cursor_expired`; and
        // a platform key allowed it does not carry the allowance over to
        // that tenant.
        sqlx::query("DELETE FROM wa_server_tenants WHERE id = $1")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        sqlx::query(
            "UPDATE wa_server_api_keys SET allowed_tenants = array_remove(allowed_tenants, $1) \
             WHERE $1 = ANY(allowed_tenants)",
        )
        .bind(id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        sqlx::query(
            "UPDATE wa_server_event_streams SET purged_through = last_sequence WHERE stream = $1",
        )
        .bind(id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        Ok(DeleteTenantOutcome::Deleted)
    }

    async fn insert_key(&self, key: &NewApiKey) -> StoreResult<Option<ApiKeyRecord>> {
        let (tenant, all, allowed): (Option<&str>, bool, Vec<&str>) = match &key.owner {
            KeyOwner::Tenant(t) => (Some(t.as_str()), false, Vec::new()),
            KeyOwner::Platform(AllowedTenants::All) => (None, true, Vec::new()),
            KeyOwner::Platform(AllowedTenants::Only(list)) => {
                (None, false, list.iter().map(TenantId::as_str).collect())
            }
            KeyOwner::Admin => (None, false, Vec::new()),
        };
        let scopes: Vec<&str> = key.scopes.iter().map(|s| s.as_str()).collect();
        let sql = format!(
            "INSERT INTO wa_server_api_keys \
             (key_id, secret_sha256, kind, tenant_id, all_tenants, allowed_tenants, scopes, name, \
              expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (key_id) DO NOTHING RETURNING {KEY_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(&key.key_id)
            .bind(key.secret_sha256.as_slice())
            .bind(key.owner.kind().as_str())
            .bind(tenant)
            .bind(all)
            .bind(&allowed)
            .bind(&scopes)
            .bind(&key.name)
            .bind(key.expires_at)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(key_row).transpose()
    }

    async fn key(&self, key_id: &str) -> StoreResult<Option<ApiKeyRecord>> {
        let sql = format!("SELECT {KEY_COLUMNS} FROM wa_server_api_keys WHERE key_id = $1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(key_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(key_row).transpose()
    }

    async fn keys(
        &self,
        scope: &KeyScope,
        page: &PageRequest,
    ) -> StoreResult<Listing<ApiKeyRecord>> {
        let (kind, tenant) = scope_filter(scope);
        let sql = format!(
            "SELECT {KEY_COLUMNS} FROM wa_server_api_keys \
             WHERE kind = $1 AND ($2::text IS NULL OR tenant_id = $2) \
             AND ($3::text IS NULL OR key_id > $3) ORDER BY key_id LIMIT $4"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(kind)
            .bind(tenant)
            .bind(page.after.as_deref())
            .bind(fetch_limit(page))
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        let items = rows.iter().map(key_row).collect::<StoreResult<Vec<_>>>()?;
        Ok(listing(items, page.limit, |k| k.key_id.clone()))
    }

    async fn revoke_key(&self, scope: &KeyScope, key_id: &str) -> StoreResult<bool> {
        let (kind, tenant) = scope_filter(scope);
        let done = sqlx::query(
            "UPDATE wa_server_api_keys SET revoked_at = COALESCE(revoked_at, now()) \
             WHERE key_id = $1 AND kind = $2 AND ($3::text IS NULL OR tenant_id = $3)",
        )
        .bind(key_id)
        .bind(kind)
        .bind(tenant)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(done.rows_affected() > 0)
    }

    async fn touch_key(&self, key_id: &str) -> StoreResult<()> {
        sqlx::query(
            "UPDATE wa_server_api_keys SET last_used_at = now() WHERE key_id = $1 \
             AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
        )
        .bind(key_id)
        .execute(&self.pool)
        .await
        .map_err(backend)
        .map(|_| ())
    }

    async fn bind_waba(
        &self,
        tenant: &TenantId,
        waba_id: &WabaId,
        numbers: &[PhoneNumberId],
    ) -> StoreResult<BindOutcome> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        // The tenant, locked for the binding: `delete_tenant` locks the row
        // `FOR UPDATE`, so the two serialize, and a tenant deleted first is
        // `NoSuchTenant` here rather than a foreign key's error.
        let exists = sqlx::query("SELECT 1 FROM wa_server_tenants WHERE id = $1 FOR KEY SHARE")
            .bind(tenant.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(busy_or_backend)?;
        if exists.is_none() {
            tx.rollback().await.map_err(backend)?;
            return Ok(BindOutcome::NoSuchTenant);
        }
        // Insert, or lock the existing row, move its generation (a refresh:
        // rolled back below when the row is another tenant's) and read its
        // tenant: two tenants racing for a new WABA serialize here, and the
        // second sees the first's binding.
        let (owner, generation): (String, i64) = sqlx::query_as(
            "INSERT INTO wa_server_wabas (waba_id, tenant_id, previous_tenant_unbound_at) \
             VALUES ($1, $2, NULL) \
             ON CONFLICT (waba_id) DO UPDATE SET generation = wa_server_wabas.generation + 1 \
             RETURNING tenant_id, generation",
        )
        .bind(waba_id.as_str())
        .bind(tenant.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;
        if owner != tenant.as_str() {
            tx.rollback().await.map_err(backend)?;
            return Ok(BindOutcome::OwnedByAnotherTenant);
        }
        if generation == 1 {
            // A new binding: when the WABA was last unbound from another
            // tenant, from its tombstone. A statement of its own, after the
            // insert: an unbinding the insert waited for (the row it
            // conflicted with, deleted meanwhile) committed before this
            // statement's snapshot, so its tombstone is seen. And when it
            // begins: not before now, nor before that unbinding. The
            // insert's default, `now()`, is this transaction's start, which
            // may precede an unbinding that committed while it ran, so an
            // event Meta dated while the previous tenant still held the
            // WABA would be in a second this binding began before (the
            // race suite's `a_binding_racing_an_unbind_records_the_move`).
            sqlx::query(
                "UPDATE wa_server_wabas w SET previous_tenant_unbound_at = ( \
                   SELECT CASE WHEN t.tenant_id IS NOT DISTINCT FROM w.tenant_id \
                               THEN t.previous_tenant_unbound_at ELSE t.unbound_at END \
                   FROM wa_server_waba_tombstones t WHERE t.waba_id = w.waba_id), \
                 attached_at = GREATEST(w.attached_at, clock_timestamp(), ( \
                   SELECT t.unbound_at FROM wa_server_waba_tombstones t \
                   WHERE t.waba_id = w.waba_id)) \
                 WHERE w.waba_id = $1",
            )
            .bind(waba_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        let ids: Vec<&str> = numbers.iter().map(PhoneNumberId::as_str).collect();
        // The numbers the WABA had and the list lacks are released, each
        // leaving its tombstone in the same statement (migration 5's trigger).
        sqlx::query(
            "DELETE FROM wa_server_numbers WHERE waba_id = $1 AND NOT (phone_number_id = ANY($2))",
        )
        .bind(waba_id.as_str())
        .bind(&ids)
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        for id in &ids {
            // A number bound to another tenant's WABA stays theirs: the
            // conditional update leaves it, and nothing is returned. One
            // already this tenant's keeps its start and its previous
            // tenant (a row from before migration 5, whose start is its
            // WABA's, keeps that one: written down before the row moves
            // to another WABA); the returned flag tells a new row (no
            // updater).
            let bound: Option<(bool,)> = sqlx::query_as(
                "INSERT INTO wa_server_numbers (phone_number_id, waba_id, tenant_id, status, \
                   attached_at, previous_tenant_released_at) \
                 VALUES ($1, $2, $3, 'connected', now(), NULL) \
                 ON CONFLICT (phone_number_id) DO UPDATE SET waba_id = EXCLUDED.waba_id, \
                 status = 'connected', updated_at = now(), \
                 attached_at = COALESCE(wa_server_numbers.attached_at, \
                   (SELECT w.attached_at FROM wa_server_wabas w \
                    WHERE w.waba_id = wa_server_numbers.waba_id)) \
                 WHERE wa_server_numbers.tenant_id = EXCLUDED.tenant_id \
                 RETURNING (xmax::text = '0')",
            )
            .bind(*id)
            .bind(waba_id.as_str())
            .bind(tenant.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
            let Some((inserted,)) = bound else {
                tx.rollback().await.map_err(backend)?;
                return Ok(BindOutcome::OwnedByAnotherTenant);
            };
            if inserted {
                // A new number binding: when another tenant last released
                // it, from its tombstone, and when it begins: not before
                // now, nor before that release. A statement of its own,
                // after the insert, as for the WABA above: a release the
                // insert waited for (the row it conflicted with, deleted
                // meanwhile) committed before this statement's snapshot,
                // so its tombstone is seen, and the insert's `now()` (this
                // transaction's start) may precede it.
                sqlx::query(
                    "UPDATE wa_server_numbers n SET previous_tenant_released_at = ( \
                       SELECT CASE WHEN t.tenant_id IS NOT DISTINCT FROM n.tenant_id \
                                   THEN t.previous_tenant_released_at ELSE t.released_at END \
                       FROM wa_server_number_tombstones t \
                       WHERE t.phone_number_id = n.phone_number_id), \
                     attached_at = GREATEST(n.attached_at, clock_timestamp(), ( \
                       SELECT t.released_at FROM wa_server_number_tombstones t \
                       WHERE t.phone_number_id = n.phone_number_id)) \
                     WHERE n.phone_number_id = $1",
                )
                .bind(*id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
        }
        tx.commit().await.map_err(backend)?;
        Ok(BindOutcome::Bound)
    }

    async fn unbind_waba(&self, waba_id: &WabaId) -> StoreResult<bool> {
        // Its numbers go with it (ON DELETE CASCADE), its tombstone and
        // theirs (migration 5's trigger) stay.
        let gone: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(unbind_sql("")))
            .bind(waba_id.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(backend)?;
        Ok(gone > 0)
    }

    async fn unbind_waba_if(&self, epoch: &BindingEpoch) -> StoreResult<bool> {
        // One statement: the row is deleted only as `epoch` read it (its
        // numbers with it, ON DELETE CASCADE, and its tombstone and theirs
        // left); a
        // binding made since has another `attached_at`, one refreshed since
        // another `generation`.
        let gone: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(unbind_sql(
            "AND tenant_id = $2 AND attached_at = $3 AND generation = $4",
        )))
        .bind(epoch.waba_id.as_str())
        .bind(epoch.tenant_id.as_str())
        .bind(epoch.attached_at)
        .bind(epoch.generation)
        .fetch_one(&self.pool)
        .await
        .map_err(busy_or_backend)?;
        Ok(gone > 0)
    }

    async fn waba(&self, waba_id: &WabaId) -> StoreResult<Option<WabaBinding>> {
        let sql = format!("SELECT {WABA_COLUMNS} FROM wa_server_wabas WHERE waba_id = $1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(waba_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(waba_row).transpose()
    }

    async fn number(&self, phone_number_id: &PhoneNumberId) -> StoreResult<Option<NumberBinding>> {
        let sql = format!(
            "SELECT {NUMBER_COLUMNS} FROM wa_server_numbers n WHERE n.phone_number_id = $1"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(phone_number_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(number_row).transpose()
    }

    async fn all_wabas(&self, page: &PageRequest) -> StoreResult<Listing<WabaBinding>> {
        let sql = format!(
            "SELECT {WABA_COLUMNS} FROM wa_server_wabas \
             WHERE ($1::text IS NULL OR waba_id > $1) ORDER BY waba_id LIMIT $2"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(page.after.as_deref())
            .bind(fetch_limit(page))
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        let items = rows.iter().map(waba_row).collect::<StoreResult<Vec<_>>>()?;
        Ok(listing(items, page.limit, |w| {
            w.waba_id.as_str().to_owned()
        }))
    }

    async fn wabas(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<WabaBinding>> {
        let sql = format!(
            "SELECT {WABA_COLUMNS} FROM wa_server_wabas \
             WHERE tenant_id = $1 AND ($2::text IS NULL OR waba_id > $2) ORDER BY waba_id LIMIT $3"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(tenant.as_str())
            .bind(page.after.as_deref())
            .bind(fetch_limit(page))
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        let items = rows.iter().map(waba_row).collect::<StoreResult<Vec<_>>>()?;
        Ok(listing(items, page.limit, |w| {
            w.waba_id.as_str().to_owned()
        }))
    }

    async fn waba_numbers(&self, waba_id: &WabaId) -> StoreResult<Vec<NumberBinding>> {
        let sql = format!(
            "SELECT {NUMBER_COLUMNS} FROM wa_server_numbers n \
             WHERE n.waba_id = $1 ORDER BY n.phone_number_id LIMIT 1001"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(waba_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        rows.iter().map(number_row).collect()
    }

    async fn numbers(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<NumberBinding>> {
        let sql = format!(
            "SELECT {NUMBER_COLUMNS} FROM wa_server_numbers n \
             WHERE n.tenant_id = $1 AND ($2::text IS NULL OR n.phone_number_id > $2) \
             ORDER BY n.phone_number_id LIMIT $3"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(tenant.as_str())
            .bind(page.after.as_deref())
            .bind(fetch_limit(page))
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        let items = rows
            .iter()
            .map(number_row)
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(listing(items, page.limit, |n| {
            n.phone_number_id.as_str().to_owned()
        }))
    }

    async fn set_waba_status(&self, waba_id: &WabaId, status: NumberStatus) -> StoreResult<()> {
        sqlx::query(
            "UPDATE wa_server_numbers SET status = $2, updated_at = now() WHERE waba_id = $1",
        )
        .bind(waba_id.as_str())
        .bind(status.as_str())
        .execute(&self.pool)
        .await
        .map_err(backend)
        .map(|_| ())
    }

    async fn set_waba_status_if(
        &self,
        epoch: &BindingEpoch,
        status: NumberStatus,
    ) -> StoreResult<bool> {
        // The WABA's row, as `epoch` read it, locked `FOR KEY SHARE`: an
        // unbinding (the row's delete, and its numbers') waits for this
        // update, and one committed before it leaves no row to find. Only
        // then its numbers.
        let held: bool = sqlx::query_scalar(
            "WITH held AS ( \
               SELECT waba_id FROM wa_server_wabas \
               WHERE waba_id = $1 AND tenant_id = $2 AND attached_at = $3 AND generation = $5 \
               FOR KEY SHARE \
             ), marked AS ( \
               UPDATE wa_server_numbers n SET status = $4, updated_at = now() \
               FROM held WHERE n.waba_id = held.waba_id \
               RETURNING 1 \
             ) \
             SELECT EXISTS (SELECT 1 FROM held)",
        )
        .bind(epoch.waba_id.as_str())
        .bind(epoch.tenant_id.as_str())
        .bind(epoch.attached_at)
        .bind(status.as_str())
        .bind(epoch.generation)
        .fetch_one(&self.pool)
        .await
        .map_err(busy_or_backend)?;
        Ok(held)
    }

    async fn set_credit_allocation_if(
        &self,
        epoch: &BindingEpoch,
        allocation_config_id: &str,
    ) -> StoreResult<bool> {
        // One statement: the row as `epoch` read it, or nothing.
        let done = sqlx::query(
            "UPDATE wa_server_wabas SET credit_allocation_id = $5 \
             WHERE waba_id = $1 AND tenant_id = $2 AND attached_at = $3 AND generation = $4",
        )
        .bind(epoch.waba_id.as_str())
        .bind(epoch.tenant_id.as_str())
        .bind(epoch.attached_at)
        .bind(epoch.generation)
        .bind(allocation_config_id)
        .execute(&self.pool)
        .await
        .map_err(busy_or_backend)?;
        Ok(done.rows_affected() == 1)
    }

    async fn save_signup_attempt(&self, attempt: &NewSignupAttempt) -> StoreResult<bool> {
        // Written only from the binding's row, held by `attempt`'s tenant,
        // locked `FOR KEY SHARE` (an unbind waits, and one committed first
        // leaves nothing to write from); a replacement of the same tenant's
        // attempt keeps when it began.
        let saved: Option<i32> = sqlx::query_scalar(
            "WITH held AS ( \
               SELECT waba_id, tenant_id FROM wa_server_wabas \
               WHERE waba_id = $1 AND tenant_id = $2 FOR KEY SHARE \
             ) \
             INSERT INTO wa_server_signup_attempts \
               (waba_id, tenant_id, failed_step, session, finish_kind) \
             SELECT waba_id, tenant_id, $3, $4, $5 FROM held \
             ON CONFLICT (waba_id) DO UPDATE SET \
               created_at = CASE WHEN wa_server_signup_attempts.tenant_id = EXCLUDED.tenant_id \
                 THEN wa_server_signup_attempts.created_at ELSE now() END, \
               tenant_id = EXCLUDED.tenant_id, failed_step = EXCLUDED.failed_step, \
               session = EXCLUDED.session, finish_kind = EXCLUDED.finish_kind, \
               updated_at = now() \
             RETURNING 1",
        )
        .bind(attempt.waba_id.as_str())
        .bind(attempt.tenant_id.as_str())
        .bind(&attempt.failed_step)
        .bind(&attempt.session)
        .bind(attempt.finish_kind.as_deref())
        .fetch_optional(&self.pool)
        .await
        .map_err(busy_or_backend)?;
        Ok(saved.is_some())
    }

    async fn signup_attempt(
        &self,
        tenant: &TenantId,
        waba_id: &WabaId,
    ) -> StoreResult<Option<SignupAttempt>> {
        let sql = format!(
            "SELECT {ATTEMPT_COLUMNS} FROM wa_server_signup_attempts \
             WHERE waba_id = $1 AND tenant_id = $2"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(waba_id.as_str())
            .bind(tenant.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(attempt_row).transpose()
    }

    async fn signup_attempts(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<SignupAttempt>> {
        let sql = format!(
            "SELECT {ATTEMPT_COLUMNS} FROM wa_server_signup_attempts \
             WHERE tenant_id = $1 AND ($2::text IS NULL OR waba_id > $2) \
             ORDER BY waba_id LIMIT $3"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(tenant.as_str())
            .bind(page.after.as_deref())
            .bind(fetch_limit(page))
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        let items = rows
            .iter()
            .map(attempt_row)
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(listing(items, page.limit, |a| {
            a.waba_id.as_str().to_owned()
        }))
    }

    async fn delete_signup_attempt(
        &self,
        tenant: &TenantId,
        waba_id: &WabaId,
    ) -> StoreResult<bool> {
        let done = sqlx::query(
            "DELETE FROM wa_server_signup_attempts WHERE waba_id = $1 AND tenant_id = $2",
        )
        .bind(waba_id.as_str())
        .bind(tenant.as_str())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(done.rows_affected() == 1)
    }

    async fn record_vault_entry(&self, entry: &VaultEntry) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO wa_server_vault_records (kind, id) VALUES ($1, $2) \
             ON CONFLICT (kind, id) DO NOTHING",
        )
        .bind(entry.kind())
        .bind(entry.id())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn vault_entries(&self, page: &PageRequest) -> StoreResult<Listing<VaultEntry>> {
        // The cursor is a key, `<kind>/<id>`: its kind and id, compared as
        // a pair (byte order of the key, as '/' sorts before every digit).
        let (kind, id) = match page.after.as_deref() {
            None => (None, None),
            Some(after) => {
                let (kind, id) = after.split_once('/').unwrap_or((after, ""));
                (Some(kind), Some(id))
            }
        };
        let rows = sqlx::query(
            "SELECT kind, id FROM wa_server_vault_records \
             WHERE $1::text IS NULL OR (kind, id) > ($1, $2) \
             ORDER BY kind, id LIMIT $3",
        )
        .bind(kind)
        .bind(id)
        .bind(fetch_limit(page))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let items = rows
            .iter()
            .map(vault_entry_row)
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(listing(items, page.limit, VaultEntry::key))
    }
}

#[async_trait]
impl IdempotencyRecords for PgStore {
    async fn claim_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        fingerprint: &[u8; 32],
        claim: &str,
        lease: std::time::Duration,
        ttl: std::time::Duration,
    ) -> StoreResult<IdempotencyClaim> {
        // A released key has no row; an expired one is replaced. Between
        // the insert that found a live row and the read of it, the row may
        // be released: then claim again (a few times: each round, another
        // request made progress).
        let table = idempotency_table(tenant);
        for _ in 0..3 {
            let insert = "INSERT INTO wa_server_idempotency (tenant_id, idempotency_key, request_sha256,                  claim, state, lease_until, expires_at)                  VALUES ($1, $2, $3, $4, 'in_progress', now() + make_interval(secs => $5),                  now() + make_interval(secs => $6))                  ON CONFLICT (tenant_id, idempotency_key) DO UPDATE SET                  request_sha256 = EXCLUDED.request_sha256, claim = EXCLUDED.claim,                  state = 'in_progress', lease_until = EXCLUDED.lease_until,                  response_status = NULL, response_body = NULL, created_at = now(),                  expires_at = EXCLUDED.expires_at                  WHERE wa_server_idempotency.expires_at <= now()                  RETURNING claim"
                .replace("wa_server_idempotency", table);
            let claimed = sqlx::query(sqlx::AssertSqlSafe(insert))
                .bind(tenant.as_str())
                .bind(key.as_str())
                .bind(fingerprint.as_slice())
                .bind(claim)
                .bind(lease.as_secs_f64())
                .bind(ttl.as_secs_f64())
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
            if claimed.is_some() {
                return Ok(IdempotencyClaim::Claimed);
            }
            let select = "SELECT request_sha256, state, lease_until <= now() AS lease_expired,                  response_status, response_body FROM wa_server_idempotency                  WHERE tenant_id = $1 AND idempotency_key = $2 AND expires_at > now()"
                .replace("wa_server_idempotency", table);
            let row = sqlx::query(sqlx::AssertSqlSafe(select))
                .bind(tenant.as_str())
                .bind(key.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
            let Some(row) = row else { continue };
            let digest: Vec<u8> = get(&row, "request_sha256")?;
            let fingerprint: [u8; 32] = digest.try_into().map_err(|_| corrupt("request_sha256"))?;
            let state = match get::<String>(&row, "state")?.as_str() {
                "completed" => IdempotencyState::Completed {
                    status: get::<Option<i32>>(&row, "response_status")?
                        .and_then(|s| u16::try_from(s).ok())
                        .ok_or_else(|| corrupt("response_status"))?,
                    body: get::<Option<Vec<u8>>>(&row, "response_body")?
                        .ok_or_else(|| corrupt("response_body"))?,
                },
                "in_progress" => IdempotencyState::InProgress {
                    lease_expired: get(&row, "lease_expired")?,
                },
                _ => return Err(corrupt("state")),
            };
            return Ok(IdempotencyClaim::Existing(IdempotencyRecord {
                fingerprint,
                state,
            }));
        }
        Err(StorageError::Backend(anyhow::anyhow!(
            "an idempotency key kept changing under concurrent requests"
        )))
    }

    async fn complete_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        claim: &str,
        status: u16,
        body: &[u8],
    ) -> StoreResult<bool> {
        let update = "UPDATE wa_server_idempotency SET state = 'completed', response_status = $4,              response_body = $5 WHERE tenant_id = $1 AND idempotency_key = $2 AND claim = $3"
            .replace("wa_server_idempotency", idempotency_table(tenant));
        let done = sqlx::query(sqlx::AssertSqlSafe(update))
            .bind(tenant.as_str())
            .bind(key.as_str())
            .bind(claim)
            .bind(i32::from(status))
            .bind(body)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected() > 0)
    }

    async fn release_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        claim: &str,
    ) -> StoreResult<bool> {
        let delete = "DELETE FROM wa_server_idempotency              WHERE tenant_id = $1 AND idempotency_key = $2 AND claim = $3"
            .replace("wa_server_idempotency", idempotency_table(tenant));
        let done = sqlx::query(sqlx::AssertSqlSafe(delete))
            .bind(tenant.as_str())
            .bind(key.as_str())
            .bind(claim)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected() > 0)
    }

    async fn purge_idempotency_keys(&self) -> StoreResult<u64> {
        // No lock of its own (housekeeping holds its turn): two at once
        // delete a record once.
        let mut purged = 0;
        for table in [IDEMPOTENCY_TABLE, ADMIN_IDEMPOTENCY_TABLE] {
            let done = sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE expires_at <= now()"
            )))
            .execute(&self.pool)
            .await
            .map_err(busy_or_backend)?;
            purged += done.rows_affected();
        }
        Ok(purged)
    }
}

/// The tenants' idempotency records (migration 2).
const IDEMPOTENCY_TABLE: &str = "wa_server_idempotency";

/// An operator's idempotency records (roadmap M5j2, migration 10): the
/// same columns, no foreign key to the tenants, since an admin key's
/// scope (`admin/<key id>`) has no tenant row.
const ADMIN_IDEMPOTENCY_TABLE: &str = "wa_server_admin_idempotency";

/// The table a scope's records live in: an admin key's namespace
/// (`admin/...`, which no tenant id can spell) in its own, else the
/// tenants'.
fn idempotency_table(scope: &TenantId) -> &'static str {
    if scope.as_str().starts_with("admin/") {
        ADMIN_IDEMPOTENCY_TABLE
    } else {
        IDEMPOTENCY_TABLE
    }
}

/// Leader election on Postgres: a transaction-scoped advisory lock
/// ([`lock_key`] of the name), held by an open transaction until the turn
/// is released (committed), dropped (rolled back), or its lease ends: the
/// transaction sets `idle_in_transaction_session_timeout` to the lease, so
/// the server ends the session, and the lock with it, once the turn has
/// sat that long (the work under a turn runs on other connections, so the
/// turn's own is idle from the start). The `housekeeping` turn is
/// [`HOUSEKEEPING_LOCK`]. Advisory locks are the database's, not a
/// schema's: deployments sharing one database take turns. Cheap to clone.
#[derive(Clone)]
pub struct PgLeaderLock {
    pool: PgPool,
}

impl std::fmt::Debug for PgLeaderLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgLeaderLock").finish_non_exhaustive()
    }
}

impl PgLeaderLock {
    /// Turns on `pool`'s database.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl LeaderLock for PgLeaderLock {
    async fn try_exclusive(
        &self,
        name: &str,
        lease: std::time::Duration,
    ) -> StoreResult<Option<LeaderTurn>> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let ours: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(lock_key(name))
            .fetch_one(&mut *tx)
            .await
            .map_err(backend)?;
        if !ours {
            tx.rollback().await.map_err(backend)?;
            return Ok(None);
        }
        // The lease: once the turn's transaction has sat idle this long,
        // the server ends its session, which releases the lock. At least a
        // millisecond (0 turns the timeout off), in whole milliseconds.
        let millis = i64::try_from(lease.as_millis())
            .unwrap_or(i64::from(i32::MAX))
            .clamp(1, i64::from(i32::MAX));
        sqlx::query("SELECT set_config('idle_in_transaction_session_timeout', $1, true)")
            .bind(format!("{millis}ms"))
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        Ok(Some(LeaderTurn::new(PgTurn { tx })))
    }
}

/// A turn of [`PgLeaderLock`]: the transaction holding the lock. Dropped
/// without a release, sqlx rolls it back, which releases the lock; past its
/// lease, the server ended it, and releasing it reports that.
struct PgTurn {
    tx: Transaction<'static, Postgres>,
}

#[async_trait]
impl Turn for PgTurn {
    async fn release(self: Box<Self>) -> StoreResult<()> {
        self.tx.commit().await.map_err(backend)
    }
}

/// The library's expired key/value rows on Postgres
/// (`PostgresKvStore::purge_expired`: webhook dedup markers add one per
/// event).
#[derive(Debug, Clone)]
pub struct PgJanitor {
    kv: PostgresKvStore,
}

impl PgJanitor {
    /// The janitor of `pool`'s key/value store (the default `wa_` prefix).
    pub fn new(pool: PgPool) -> Self {
        Self {
            kv: PostgresKvStore::new(pool),
        }
    }
}

#[async_trait]
impl Janitor for PgJanitor {
    async fn purge_expired(&self) -> StoreResult<u64> {
        self.kv.purge_expired().await
    }
}

/// [`migrate`], as a port.
#[derive(Clone)]
pub struct PgMigrator {
    pool: PgPool,
}

impl std::fmt::Debug for PgMigrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgMigrator").finish_non_exhaustive()
    }
}

impl PgMigrator {
    /// Migrations on `pool`, of at least two connections (one holds the
    /// lock).
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SchemaMigrator for PgMigrator {
    async fn migrate(&self) -> StoreResult<()> {
        migrate(&self.pool).await
    }
}

/// Every port on one Postgres database, through one pool: [`PgStore`],
/// [`PgEventStore`], [`PgNotifier`] (its `LISTEN` session a connection of
/// its own), [`PgLeaderLock`], [`PgJanitor`], [`PgMigrator`], and
/// the library's `PostgresKvStore` and `PostgresConversationStore` (the
/// default `wa_` prefix). Cheap to clone.
#[derive(Clone)]
pub struct PgBackend {
    pool: PgPool,
    store: Arc<PgStore>,
    outbox: Arc<PgEventStore>,
    webhooks: Arc<PgWebhookStore>,
    notifier: Arc<PgNotifier>,
    kv: Arc<PostgresKvStore>,
    conversations: Arc<PostgresConversationStore>,
}

impl std::fmt::Debug for PgBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The pool's options could reveal connection settings.
        f.debug_struct("PgBackend").finish_non_exhaustive()
    }
}

impl PgBackend {
    /// The ports on `pool`. Migrate it before serving
    /// ([`Backend::migrator`]).
    pub fn new(pool: PgPool) -> Self {
        Self {
            store: Arc::new(PgStore::new(pool.clone())),
            outbox: Arc::new(PgEventStore::new(pool.clone())),
            webhooks: Arc::new(PgWebhookStore::new(pool.clone())),
            notifier: Arc::new(PgNotifier::new(pool.clone())),
            kv: Arc::new(PostgresKvStore::new(pool.clone())),
            conversations: Arc::new(PostgresConversationStore::new(pool.clone())),
            pool,
        }
    }

    /// Its inbox applying `retention` (`PostgresConversationStore::with_retention`:
    /// what housekeeping's `ConversationStore::apply_retention` purges; the
    /// service's `WA_SERVER_INBOX_RETENTION`, roadmap M2a). The same tables:
    /// nothing is lost.
    #[must_use]
    pub fn with_inbox_retention(mut self, retention: Retention) -> Self {
        self.conversations =
            Arc::new(PostgresConversationStore::new(self.pool.clone()).with_retention(retention));
        self
    }
}

#[async_trait]
impl Backend for PgBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Postgres
    }

    fn records(&self) -> Arc<dyn RecordStore> {
        self.store.clone()
    }

    fn idempotency(&self) -> Arc<dyn IdempotencyRecords> {
        self.store.clone()
    }

    fn outbox(&self) -> Arc<dyn Outbox> {
        self.outbox.clone()
    }

    fn notifier(&self) -> Arc<dyn EventNotifier> {
        self.notifier.clone()
    }

    fn webhooks(&self) -> Arc<dyn WebhookStore> {
        self.webhooks.clone()
    }

    fn leader_lock(&self) -> Arc<dyn LeaderLock> {
        Arc::new(PgLeaderLock::new(self.pool.clone()))
    }

    fn janitor(&self) -> Arc<dyn Janitor> {
        Arc::new(PgJanitor::new(self.pool.clone()))
    }

    fn migrator(&self) -> Arc<dyn SchemaMigrator> {
        Arc::new(PgMigrator::new(self.pool.clone()))
    }

    fn kv(&self) -> Arc<dyn KvStore> {
        self.kv.clone()
    }

    fn conversations(&self) -> Arc<dyn ConversationStore> {
        self.conversations.clone()
    }

    async fn close(&self) {
        self.notifier.close().await;
        self.pool.close().await;
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use sha2::{Digest, Sha256};

    use super::*;

    /// `(version, SHA-384 hex)` of every service migration, as sqlx records
    /// it. A new migration adds a line; an existing line never changes (an
    /// edited migration makes `migrate` refuse every database it ran on).
    const PINNED_CHECKSUMS: [(i64, &str); 11] = [
        (
            1,
            "4d1c5a2555461deec494d1d0f8a6be354e0f092115ee74abdc6e270670a441d3856222ca2ade7598a98e62dc80075a47",
        ),
        (
            2,
            "07f54d21a6d32d8a87420469f6f5e35d3b93f47ce7a5aec34ed526ca13d7b7fb08b9858065b74c1d522b3a88b82f24b2",
        ),
        (
            3,
            "ca0cec491b00af0a22afff8e45913f1f4f1e59d08250804c1eb1f85306d33ae577c050208ad6a0c67ab8e3066296a1cd",
        ),
        (
            4,
            "4d58bbd6f98c7f98f52cad4856ac4220e2cc299a5860597681e2452b129956a5b49def8ab02a152087b5f976a35767ec",
        ),
        (
            5,
            "4757164b56bd779d41bfafaeb41aa393f921a93e269895d242ba4ec0b90e2f59b745c9407bc9f325a5885676ff137a6a",
        ),
        (
            6,
            "4d5988da61c5955cb42e9827a24ad4bf0ca4d6aaecba8bdde18ad9c49cc9c2308070ac8f8ca275bbe5b72a0c1606a8e5",
        ),
        (
            7,
            "aafa19ce1aa066caa0464aeac5ecc436acb994873108f0d51b002bae0155e0a9f8b8e47208fecb15d2420471dd555702",
        ),
        (
            8,
            "d748a179ba921d88a958978e4c476c78014d815694d145bc3d293213c6a40de2d6b0669c4b5daf356af7cc56213dd8aa",
        ),
        (
            9,
            "0d605fa295b385137f0d6fab20501615e8621c011abadb6dfc8a16608938232351950318861db7ad5fa7ee9094650a89",
        ),
        (
            10,
            "805d46eed073988cf8ff26bdc87cd2d18f5b529c117f5ea099888484814d019acac59315453a0561b2577dcc569df020",
        ),
        (
            11,
            "c9c2c040c3e545dede9e96a6fd78ef0b8522e81403461a3dc38f9037d39f74df34f8ac649c1ad839dc9dbc1c8a975fa4",
        ),
    ];

    #[test]
    fn the_migrations_and_their_checksums_are_pinned() {
        let recorded: Vec<(i64, String)> = migrations()
            .iter()
            .map(|m| {
                let hex = m.checksum.iter().fold(String::new(), |mut hex, b| {
                    write!(hex, "{b:02x}").unwrap();
                    hex
                });
                (m.version, hex)
            })
            .collect();
        assert_eq!(
            recorded,
            PINNED_CHECKSUMS.map(|(v, h)| (v, h.to_owned())).to_vec()
        );
        // sqlx records each description too, beside its version.
        let descriptions: Vec<String> = migrations()
            .iter()
            .map(|m| m.description.to_string())
            .collect();
        assert_eq!(
            descriptions,
            [
                "tenants keys bindings",
                "idempotency",
                "events",
                "waba moves",
                "number moves",
                "signup",
                "received media",
                "erasure",
                "webhooks out",
                "admin idempotency",
                "webhook endpoint tenants"
            ]
        );
        // `\x77` is `w`: spelled so that a search-and-replace of the table
        // prefix cannot rewrite this pin along with the code.
        assert_eq!(MIGRATIONS_TABLE, "\x77a_server_sqlx_migrations");
    }

    #[test]
    fn the_lock_key_is_derived_as_documented() {
        let digest = Sha256::digest(b"meta-whatsapp-server/migrate");
        let mut first = [0u8; 8];
        first.copy_from_slice(&digest[..8]);
        assert_eq!(MIGRATION_LOCK, i64::from_be_bytes(first));
        let digest = Sha256::digest(b"meta-whatsapp-server/housekeeping");
        first.copy_from_slice(&digest[..8]);
        assert_eq!(HOUSEKEEPING_LOCK, i64::from_be_bytes(first));
    }

    /// The leader lock's keys are the same derivation: its `housekeeping`
    /// turn is the lock the purges take. Decisive: the prefix of the name.
    #[test]
    fn the_leader_locks_keys_are_the_services_locks() {
        assert_eq!(
            lock_key(meta_whatsapp_server_core::store::HOUSEKEEPING),
            HOUSEKEEPING_LOCK
        );
        assert_eq!(lock_key("migrate"), MIGRATION_LOCK);
    }

    /// Why `sql` would contract the schema (what a replica of the previous
    /// release still reads, or writes without knowing a new rule), or
    /// `None` when it only expands. A function's body (between `$$`) is
    /// searched like the rest, an upsert's `DO UPDATE` aside (it writes the
    /// row its own insert names, in a table of the service's), and is not
    /// a statement of the migration; a trigger may only run `AFTER` the
    /// row is written (a `BEFORE` one could rewrite or refuse what the
    /// previous release writes).
    fn contraction(sql: &str) -> Option<String> {
        let upper = sql
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n")
            .to_uppercase()
            .replace("DO UPDATE SET", "DO UPSERT");
        for forbidden in [
            "DROP ",
            "RENAME ",
            "TRUNCATE",
            "DELETE FROM",
            "UPDATE ",
            " TYPE ",
            "SET NOT NULL",
            // A new constraint refuses the writes of the previous release.
            "ADD CONSTRAINT",
        ] {
            if upper.contains(forbidden) {
                return Some(format!("`{}`", forbidden.trim()));
            }
        }
        // The statements, each function's body left out (its `;` are its
        // own): the text outside the `$$` pairs.
        let outside: String = upper
            .split("$$")
            .enumerate()
            .filter(|(i, _)| i % 2 == 0)
            .map(|(_, part)| part)
            .collect::<Vec<_>>()
            .join(" BODY ");
        for statement in outside.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let statement = statement.split_whitespace().collect::<Vec<_>>().join(" ");
            let after_trigger = statement.starts_with("CREATE TRIGGER WA_SERVER_")
                && statement.split(' ').nth(3) == Some("AFTER");
            if !(statement.starts_with("CREATE TABLE WA_SERVER_")
                || statement.starts_with("CREATE INDEX WA_SERVER_")
                || statement.starts_with("ALTER TABLE WA_SERVER_")
                || statement.starts_with("CREATE FUNCTION WA_SERVER_")
                || after_trigger)
            {
                return Some(format!(
                    "`{statement}` is not an expansion of a wa_server_ table"
                ));
            }
            // The previous release inserts rows without the new column.
            if statement.starts_with("ALTER TABLE")
                && statement.contains("NOT NULL")
                && !statement.contains("DEFAULT")
            {
                return Some(format!(
                    "`{statement}`: a NOT NULL column without a default"
                ));
            }
        }
        None
    }

    /// Expand-then-contract: a migration adds; it never drops, renames,
    /// retypes or rewrites what a replica of the previous release still
    /// reads, nor adds a rule its writes break. A contracting step is a
    /// migration of a later release, whose name says so (`…_contract.sql`)
    /// and which this test then allows.
    #[test]
    fn migrations_only_expand() {
        for (version, description, sql) in MIGRATION_FILES {
            if description.ends_with("contract") {
                continue;
            }
            assert_eq!(
                contraction(sql),
                None,
                "migration {version} ({description}) contracts"
            );
        }
    }

    /// The check's own test: what it must refuse, and allow.
    #[test]
    fn the_expand_only_check_rejects_contractions() {
        for sql in [
            "DROP TABLE wa_server_tenants;",
            "ALTER TABLE wa_server_tenants RENAME COLUMN name TO label;",
            "ALTER TABLE wa_server_tenants ALTER COLUMN name TYPE INT;",
            "ALTER TABLE wa_server_tenants ALTER COLUMN name SET NOT NULL;",
            "ALTER TABLE wa_server_tenants ADD CONSTRAINT name_len CHECK (length(name) < 9);",
            "ALTER TABLE wa_server_tenants ADD COLUMN plan TEXT NOT NULL;",
            "ALTER TABLE wa_server_tenants\n  ADD COLUMN plan TEXT\n  NOT NULL;",
            "UPDATE wa_server_tenants SET name = '';",
            "CREATE TABLE wa_other (id TEXT);",
            "CREATE TRIGGER wa_server_x BEFORE INSERT ON wa_server_tenants \
             FOR EACH ROW EXECUTE FUNCTION wa_server_f();",
            "CREATE TRIGGER other_x AFTER DELETE ON wa_server_tenants \
             FOR EACH ROW EXECUTE FUNCTION wa_server_f();",
            "CREATE FUNCTION wa_server_f() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN DELETE FROM wa_server_tenants; RETURN OLD; END; $$;",
            "CREATE FUNCTION wa_server_f() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN UPDATE wa_server_tenants SET name = ''; RETURN OLD; END; $$;",
        ] {
            assert!(contraction(sql).is_some(), "{sql}");
        }
        for sql in [
            "CREATE TABLE wa_server_x (id TEXT NOT NULL);",
            "CREATE INDEX wa_server_x_idx ON wa_server_x (id);",
            "ALTER TABLE wa_server_tenants ADD COLUMN plan TEXT;",
            "ALTER TABLE wa_server_tenants ADD COLUMN plan TEXT NOT NULL DEFAULT 'free';",
            "-- DROP nothing: a comment\nCREATE TABLE wa_server_y (id TEXT);",
            "CREATE FUNCTION wa_server_f() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN INSERT INTO wa_server_x (id) VALUES (OLD.id) \
             ON CONFLICT (id) DO UPDATE SET id = EXCLUDED.id; RETURN OLD; END; $$;\n\
             CREATE TRIGGER wa_server_t AFTER DELETE ON wa_server_y \
             FOR EACH ROW EXECUTE FUNCTION wa_server_f();",
        ] {
            assert_eq!(contraction(sql), None, "{sql}");
        }
    }
}
