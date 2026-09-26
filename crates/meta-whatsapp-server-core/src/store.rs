//! The storage ports of the service's own records (docs/design/server.md,
//! section 2.2): [`RecordStore`] (tenants, keys, bindings),
//! [`IdempotencyRecords`] (section 5.4), and the housekeeping ones:
//! [`LeaderLock`], [`Janitor`] and [`SchemaMigrator`]. The event outbox
//! is [`crate::outbox::Outbox`]; a backend gives every one of them over
//! one database ([`crate::backend::Backend`]).
//!
//! The library's records (the token vault, OTP challenges, signup
//! sessions, webhook dedup, the inbox) stay in its own ports, `KvStore`
//! and `ConversationStore`. No port here names a database driver's type:
//! a backend maps its failures to the library's [`StorageError`].
//!
//! The ports are not independent of each other: a backend implements them
//! over one database, and deleting a tenant ([`RecordStore::delete_tenant`])
//! reaches its idempotency records and its event stream too.

use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};

use crate::model::{
    ApiKeyRecord, BindOutcome, DeleteTenantOutcome, IdempotencyClaim, IdempotencyKey, KeyScope,
    Listing, NewApiKey, NumberBinding, NumberStatus, PageRequest, Tenant, TenantId, TenantStatus,
    WabaBinding,
};

/// Result of a store call. Every failure is the library's
/// [`StorageError`], answered `503 storage_unavailable`.
pub type StoreResult<T> = Result<T, StorageError>;

/// The service's records: tenants, API keys, and the bindings of WABAs
/// and numbers to tenants. Implementations must be safe to share between
/// replicas (a shared database) or tasks (memory).
#[async_trait]
pub trait RecordStore: Send + Sync + 'static {
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

    /// Delete a tenant, its keys, its idempotency records and its events,
    /// unless it still has WABAs; its event stream records its events
    /// purged, so a tenant created later with the same id polls none of
    /// them and its sequences go on after them; platform keys stop
    /// allowing it (a tenant created later with the same id is another
    /// tenant for them).
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

    /// A WABA's numbers, in id order (at most the 1,000 attach binds).
    async fn waba_numbers(&self, waba_id: &WabaId) -> StoreResult<Vec<NumberBinding>>;

    /// A tenant's numbers, in id order.
    async fn numbers(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<NumberBinding>>;

    /// Set the status of every number of `waba_id`.
    async fn set_waba_status(&self, waba_id: &WabaId, status: NumberStatus) -> StoreResult<()>;
}

/// The records of `Idempotency-Key`s (docs/design/server.md, section
/// 5.4), scoped to their tenant: deleting the tenant deletes them. The
/// engine that claims, settles and replays them is
/// [`crate::idempotency`].
#[async_trait]
pub trait IdempotencyRecords: Send + Sync + 'static {
    /// Claim `key` for `tenant`: when no live record holds it (none, or
    /// one past its `ttl`), write one `in_progress` with `fingerprint`,
    /// the claim id `claim`, a lease ending after `lease` and an expiry
    /// after `ttl`, and answer [`IdempotencyClaim::Claimed`]; else answer
    /// the record found. Atomic: of two requests racing for a key, one
    /// claims it. Times are the store's clock (the database's).
    async fn claim_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        fingerprint: &[u8; 32],
        claim: &str,
        lease: Duration,
        ttl: Duration,
    ) -> StoreResult<IdempotencyClaim>;

    /// Record the answer of the request holding `key` under `claim`:
    /// `completed`, kept until its expiry. `false` when `claim` no longer
    /// holds it.
    async fn complete_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        claim: &str,
        status: u16,
        body: &[u8],
    ) -> StoreResult<bool>;

    /// Release `key`: delete its record, if `claim` holds it (the request
    /// proved nothing was sent). `false` when `claim` no longer holds it.
    async fn release_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        claim: &str,
    ) -> StoreResult<bool>;

    /// Delete the records past their expiry; how many went. One replica
    /// at a time: `0` when another one is purging (the delete is safe to
    /// repeat).
    async fn purge_idempotency_keys(&self) -> StoreResult<u64>;
}

/// The name of the lock housekeeping's purges run under
/// ([`LeaderLock::try_exclusive`]): one replica at a time purges, the
/// others skip that round (docs/design/server.md, section 2.4).
pub const HOUSEKEEPING: &str = "housekeeping";

/// Leader election for periodic work shared by the replicas of one
/// deployment: of the replicas asking for `name` at once, one gets the
/// turn, and the others skip the work this round. A backend backs it with
/// its database (Postgres: an advisory lock); memory, with the process.
#[async_trait]
pub trait LeaderLock: Send + Sync + 'static {
    /// The turn for `name`, or `None` when another holder has it. The turn
    /// ends when it is released ([`LeaderTurn::release`]) or dropped.
    async fn try_exclusive(&self, name: &str) -> StoreResult<Option<LeaderTurn>>;
}

/// A turn a [`LeaderLock`] gave: held until released, or dropped (a
/// holder cut before it released it, a task aborted at shutdown, still
/// ends it, as its backend can: Postgres rolls the transaction holding the
/// lock back).
pub struct LeaderTurn(Box<dyn Turn>);

impl std::fmt::Debug for LeaderTurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaderTurn").finish_non_exhaustive()
    }
}

impl LeaderTurn {
    /// Wrap a backend's turn.
    pub fn new(turn: impl Turn + 'static) -> Self {
        Self(Box::new(turn))
    }

    /// End the turn.
    pub async fn release(self) -> StoreResult<()> {
        self.0.release().await
    }
}

/// A backend's side of a [`LeaderTurn`]: what ends it. Dropping it
/// without [`Turn::release`] must end it too.
#[async_trait]
pub trait Turn: Send {
    /// End the turn.
    async fn release(self: Box<Self>) -> StoreResult<()>;
}

/// The expired rows a backend's stores leave and nothing else deletes:
/// on Postgres, the library's key/value rows (webhook dedup markers add
/// one per event). Reads already ignore them: this bounds the tables.
/// Housekeeping calls it under its [`LeaderLock`] turn.
#[async_trait]
pub trait Janitor: Send + Sync + 'static {
    /// Delete what expired; how many rows went.
    async fn purge_expired(&self) -> StoreResult<u64>;
}

/// Creates or upgrades a backend's schema: the library's stores' and the
/// service's, as one step. Idempotent, and safe to run from several
/// replicas at once.
#[async_trait]
pub trait SchemaMigrator: Send + Sync + 'static {
    /// Migrate.
    async fn migrate(&self) -> StoreResult<()>;
}

/// `items` (one more than asked, when there is more) as a page: at most
/// `limit` of them, and the last one's id when another page follows.
pub fn listing<T>(mut items: Vec<T>, limit: usize, id: impl Fn(&T) -> String) -> Listing<T> {
    let more = items.len() > limit;
    items.truncate(limit);
    let next_after = if more { items.last().map(id) } else { None };
    Listing { items, next_after }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_says_whether_another_page_follows() {
        let page = listing(vec![1, 2, 3], 2, ToString::to_string);
        assert_eq!(page.items, [1, 2]);
        assert_eq!(page.next_after.as_deref(), Some("2"));
        let last = listing(vec![1, 2], 2, ToString::to_string);
        assert_eq!(last.items, [1, 2]);
        assert_eq!(last.next_after, None);
    }
}
