//! Where the service keeps its own records: [`Store`], with a Postgres
//! implementation ([`PgStore`]) for every deployment and a memory one
//! ([`MemoryStore`]) for `WA_SERVER_ENV=development` and tests.
//!
//! The library's records (the token vault, OTP challenges, signup
//! sessions, webhook dedup) stay in its `KvStore`; these are the tables
//! the design adds (`wa_server_*`, docs/design/server.md, section 2.2).

mod memory;
mod postgres;

pub use memory::MemoryStore;
pub use postgres::{MIGRATION_LOCK, MIGRATIONS_TABLE, PgStore, migrate, migrations};

use async_trait::async_trait;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};

use crate::model::{
    ApiKeyRecord, BindOutcome, DeleteTenantOutcome, KeyScope, Listing, NewApiKey, NumberBinding,
    NumberStatus, PageRequest, Tenant, TenantId, TenantStatus, WabaBinding,
};

/// Result of a store call. Every failure is the library's
/// [`StorageError`], answered `503 storage_unavailable`.
pub type StoreResult<T> = Result<T, StorageError>;

/// The service's records. Implementations must be safe to share between
/// replicas (Postgres) or tasks (memory).
#[async_trait]
pub trait Store: Send + Sync + 'static {
    /// Whether the store answers (`/readyz`).
    async fn ping(&self) -> StoreResult<()>;

    /// Create a tenant; `None` when the id is taken.
    async fn create_tenant(&self, id: &TenantId, name: &str) -> StoreResult<Option<Tenant>>;

    /// One tenant.
    async fn tenant(&self, id: &TenantId) -> StoreResult<Option<Tenant>>;

    /// Tenants in id order.
    async fn tenants(&self, page: &PageRequest) -> StoreResult<Listing<Tenant>>;

    /// Change a tenant's name and status (`None` leaves it). `None` when
    /// there is no such tenant.
    async fn update_tenant(
        &self,
        id: &TenantId,
        name: Option<&str>,
        status: Option<TenantStatus>,
    ) -> StoreResult<Option<Tenant>>;

    /// Delete a tenant and its keys, unless it still has WABAs.
    async fn delete_tenant(&self, id: &TenantId) -> StoreResult<DeleteTenantOutcome>;

    /// Store a new key; `None` when its id is taken (the caller draws a
    /// new one).
    async fn insert_key(&self, key: &NewApiKey) -> StoreResult<Option<ApiKeyRecord>>;

    /// The key with this id, revoked or not.
    async fn key(&self, key_id: &str) -> StoreResult<Option<ApiKeyRecord>>;

    /// The keys of `scope`, in id order, revoked ones included.
    async fn keys(
        &self,
        scope: &KeyScope,
        page: &PageRequest,
    ) -> StoreResult<Listing<ApiKeyRecord>>;

    /// Revoke the key `key_id` of `scope`. `false` when no such key belongs
    /// to `scope`. Revoking a revoked key keeps its first revocation time.
    async fn revoke_key(&self, scope: &KeyScope, key_id: &str) -> StoreResult<bool>;

    /// Record that the key was used, at most once a minute.
    async fn touch_key(&self, key_id: &str) -> StoreResult<()>;

    /// Bind `waba_id` and exactly `numbers` to `tenant`, in one step: when
    /// the WABA or a number is bound to another tenant, nothing changes
    /// (decision D4). Numbers the WABA had and `numbers` lacks are unbound;
    /// the others become `connected`.
    async fn bind_waba(
        &self,
        tenant: &TenantId,
        waba_id: &WabaId,
        numbers: &[PhoneNumberId],
    ) -> StoreResult<BindOutcome>;

    /// Remove the binding of `waba_id` and its numbers. `false` when it was
    /// not bound.
    async fn unbind_waba(&self, waba_id: &WabaId) -> StoreResult<bool>;

    /// The binding of a WABA.
    async fn waba(&self, waba_id: &WabaId) -> StoreResult<Option<WabaBinding>>;

    /// The binding of a phone number.
    async fn number(&self, phone_number_id: &PhoneNumberId) -> StoreResult<Option<NumberBinding>>;

    /// Every tenant's WABAs, in id order (the vault cannot list its
    /// records: key rotation walks these).
    async fn all_wabas(&self, page: &PageRequest) -> StoreResult<Listing<WabaBinding>>;

    /// A tenant's WABAs, in id order.
    async fn wabas(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<WabaBinding>>;

    /// A tenant's numbers, in id order.
    async fn numbers(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<NumberBinding>>;

    /// Set the status of every number of `waba_id`.
    async fn set_waba_status(&self, waba_id: &WabaId, status: NumberStatus) -> StoreResult<()>;
}

/// `items` (one more than asked, when there is more) as a page.
pub(crate) fn listing<T>(mut items: Vec<T>, limit: usize, id: impl Fn(&T) -> String) -> Listing<T> {
    let more = items.len() > limit;
    items.truncate(limit);
    let next_after = if more { items.last().map(id) } else { None };
    Listing { items, next_after }
}
