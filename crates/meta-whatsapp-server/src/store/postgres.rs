//! [`PgStore`]: the service's tables on Postgres, and [`migrate`], which
//! creates or upgrades them together with the library's.
//!
//! Every query is a runtime query on the sqlx the facade re-exports
//! (`meta_whatsapp_rs::adapters::store::postgres::sqlx`), so building never
//! needs a database. Times come from the database server's `now()`, like
//! the library's stores.

use std::borrow::Cow;

use async_trait::async_trait;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::migrate::{
    Migration, MigrationType, Migrator,
};
use meta_whatsapp_rs::adapters::store::postgres::sqlx::postgres::PgRow;
use meta_whatsapp_rs::adapters::store::postgres::sqlx::{self, PgPool, Row, SqlSafeStr};
use meta_whatsapp_rs::adapters::store::postgres::{self as library};
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};

use super::{Store, StoreResult, listing};
use crate::model::{
    AllowedTenants, ApiKeyRecord, BindOutcome, DeleteTenantOutcome, KeyOwner, KeyScope, Listing,
    NewApiKey, NumberBinding, NumberStatus, PageRequest, Scope, Tenant, TenantId, TenantStatus,
    WabaBinding,
};

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
const MIGRATION_FILES: &[(i64, &str, &str)] = &[(
    1,
    "tenants keys bindings",
    include_str!("../../migrations/0001_tenants_keys_bindings.sql"),
)];

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

fn waba_row(row: &PgRow) -> StoreResult<WabaBinding> {
    Ok(WabaBinding {
        waba_id: WabaId::new(get::<String>(row, "waba_id")?),
        tenant_id: tenant_id(&get::<String>(row, "tenant_id")?, "tenant_id")?,
        credit_allocation_id: get(row, "credit_allocation_id")?,
        attached_at: get(row, "attached_at")?,
    })
}

fn number_row(row: &PgRow) -> StoreResult<NumberBinding> {
    Ok(NumberBinding {
        phone_number_id: PhoneNumberId::new(get::<String>(row, "phone_number_id")?),
        waba_id: WabaId::new(get::<String>(row, "waba_id")?),
        tenant_id: tenant_id(&get::<String>(row, "tenant_id")?, "tenant_id")?,
        status: NumberStatus::parse(&get::<String>(row, "status")?)
            .ok_or_else(|| corrupt("status"))?,
        updated_at: get(row, "updated_at")?,
    })
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
impl Store for PgStore {
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
        // Keys go with it (ON DELETE CASCADE).
        sqlx::query("DELETE FROM wa_server_tenants WHERE id = $1")
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
        // Insert, or lock the existing row and read its tenant: two tenants
        // racing for a new WABA serialize here, and the second sees the
        // first's binding.
        let owner: String = sqlx::query_scalar(
            "INSERT INTO wa_server_wabas (waba_id, tenant_id) VALUES ($1, $2) \
             ON CONFLICT (waba_id) DO UPDATE SET waba_id = EXCLUDED.waba_id \
             RETURNING tenant_id",
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
        let ids: Vec<&str> = numbers.iter().map(PhoneNumberId::as_str).collect();
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
            // conditional update leaves it, and nothing is returned.
            let bound = sqlx::query(
                "INSERT INTO wa_server_numbers (phone_number_id, waba_id, tenant_id, status) \
                 VALUES ($1, $2, $3, 'connected') \
                 ON CONFLICT (phone_number_id) DO UPDATE SET waba_id = EXCLUDED.waba_id, \
                 status = 'connected', updated_at = now() \
                 WHERE wa_server_numbers.tenant_id = EXCLUDED.tenant_id \
                 RETURNING phone_number_id",
            )
            .bind(*id)
            .bind(waba_id.as_str())
            .bind(tenant.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
            if bound.is_none() {
                tx.rollback().await.map_err(backend)?;
                return Ok(BindOutcome::OwnedByAnotherTenant);
            }
        }
        tx.commit().await.map_err(backend)?;
        Ok(BindOutcome::Bound)
    }

    async fn unbind_waba(&self, waba_id: &WabaId) -> StoreResult<bool> {
        // Its numbers go with it (ON DELETE CASCADE).
        let done = sqlx::query("DELETE FROM wa_server_wabas WHERE waba_id = $1")
            .bind(waba_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected() > 0)
    }

    async fn waba(&self, waba_id: &WabaId) -> StoreResult<Option<WabaBinding>> {
        let row = sqlx::query(
            "SELECT waba_id, tenant_id, credit_allocation_id, attached_at \
             FROM wa_server_wabas WHERE waba_id = $1",
        )
        .bind(waba_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        row.as_ref().map(waba_row).transpose()
    }

    async fn number(&self, phone_number_id: &PhoneNumberId) -> StoreResult<Option<NumberBinding>> {
        let row = sqlx::query(
            "SELECT phone_number_id, waba_id, tenant_id, status, updated_at \
             FROM wa_server_numbers WHERE phone_number_id = $1",
        )
        .bind(phone_number_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        row.as_ref().map(number_row).transpose()
    }

    async fn wabas(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<WabaBinding>> {
        let rows = sqlx::query(
            "SELECT waba_id, tenant_id, credit_allocation_id, attached_at FROM wa_server_wabas \
             WHERE tenant_id = $1 AND ($2::text IS NULL OR waba_id > $2) ORDER BY waba_id LIMIT $3",
        )
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

    async fn numbers(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<NumberBinding>> {
        let rows = sqlx::query(
            "SELECT phone_number_id, waba_id, tenant_id, status, updated_at FROM wa_server_numbers \
             WHERE tenant_id = $1 AND ($2::text IS NULL OR phone_number_id > $2) \
             ORDER BY phone_number_id LIMIT $3",
        )
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
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use sha2::{Digest, Sha256};

    use super::*;

    /// `(version, SHA-384 hex)` of every service migration, as sqlx records
    /// it. A new migration adds a line; an existing line never changes (an
    /// edited migration makes `migrate` refuse every database it ran on).
    const PINNED_CHECKSUMS: [(i64, &str); 1] = [(
        1,
        "4d1c5a2555461deec494d1d0f8a6be354e0f092115ee74abdc6e270670a441d3856222ca2ade7598a98e62dc80075a47",
    )];

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
        assert_eq!(MIGRATIONS_TABLE, "wa_server_sqlx_migrations");
    }

    #[test]
    fn the_lock_key_is_derived_as_documented() {
        let digest = Sha256::digest(b"meta-whatsapp-server/migrate");
        let mut first = [0u8; 8];
        first.copy_from_slice(&digest[..8]);
        assert_eq!(MIGRATION_LOCK, i64::from_be_bytes(first));
    }

    /// Why `sql` would contract the schema (what a replica of the previous
    /// release still reads, or writes without knowing a new rule), or
    /// `None` when it only expands.
    fn contraction(sql: &str) -> Option<String> {
        let upper = sql
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n")
            .to_uppercase();
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
        for statement in upper.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let statement = statement.split_whitespace().collect::<Vec<_>>().join(" ");
            if !(statement.starts_with("CREATE TABLE WA_SERVER_")
                || statement.starts_with("CREATE INDEX WA_SERVER_")
                || statement.starts_with("ALTER TABLE WA_SERVER_"))
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
        ] {
            assert!(contraction(sql).is_some(), "{sql}");
        }
        for sql in [
            "CREATE TABLE wa_server_x (id TEXT NOT NULL);",
            "CREATE INDEX wa_server_x_idx ON wa_server_x (id);",
            "ALTER TABLE wa_server_tenants ADD COLUMN plan TEXT;",
            "ALTER TABLE wa_server_tenants ADD COLUMN plan TEXT NOT NULL DEFAULT 'free';",
            "-- DROP nothing: a comment\nCREATE TABLE wa_server_y (id TEXT);",
        ] {
            assert_eq!(contraction(sql), None, "{sql}");
        }
    }
}
