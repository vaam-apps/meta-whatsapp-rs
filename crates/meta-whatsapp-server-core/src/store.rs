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
//!
//! **What every backend guarantees**, whatever its database (Postgres gets
//! some of these from foreign keys and row locks; a MongoDB or a
//! CrateStack-models backend has to build them):
//!
//! - **Referential rules**: binding a WABA to a tenant that does not
//!   exist answers [`BindOutcome::NoSuchTenant`], and a binding and a
//!   deletion of one tenant serialize ([`RecordStore::bind_waba`],
//!   [`RecordStore::delete_tenant`]): never a binding to a deleted tenant.
//! - **The outbox checks the routing again**, atomically with the insert
//!   ([`crate::outbox::Outbox::insert`]).
//! - **One clock**: every time a port compares (an idempotency lease or
//!   expiry, a key's last use, a purge's cutoff) is read from one clock
//!   all replicas agree on: the database's where it has one (Postgres:
//!   `now()`), else the service's `Clock` (the library's port), whose skew
//!   between replicas stays below the shortest lease it measures
//!   ([`HOUSEKEEPING_LEASE`], the idempotency lease).
//! - **Contention is typed**: a backend that gives up waiting for another
//!   writer reports `StorageError::Busy`, never an opaque backend error.
//! - **Listings are in byte order** of their ids (Postgres: `COLLATE
//!   "C"`), never a locale's collation: a page's cursor is an id, and two
//!   backends must page the same records the same way.
//! - **Purges take no lock of their own**, return how many rows went, and
//!   are safe on two replicas at once; housekeeping runs them under one
//!   [`LeaderLock`] turn per round.

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

    /// Tenants in byte order of their ids.
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
    /// tenant for them). Serialized with [`Self::bind_waba`] on the same
    /// tenant: the check for WABAs and the deletion are one step that no
    /// binding of this tenant lands in between (on a database without
    /// foreign keys, both write the tenant's record, so that they
    /// conflict).
    async fn delete_tenant(&self, id: &TenantId) -> StoreResult<DeleteTenantOutcome>;

    /// Store a new key; `None` when its id is taken (the caller draws a
    /// new one).
    async fn insert_key(&self, key: &NewApiKey) -> StoreResult<Option<ApiKeyRecord>>;

    /// The key with this id, revoked or not.
    async fn key(&self, key_id: &str) -> StoreResult<Option<ApiKeyRecord>>;

    /// The keys of `scope`, in byte order of their ids, revoked ones
    /// included.
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
    /// `tenant` does not exist ([`BindOutcome::NoSuchTenant`]), or the WABA
    /// or a number is bound to another tenant
    /// ([`BindOutcome::OwnedByAnotherTenant`], decision D4), nothing
    /// changes. Numbers the WABA had and `numbers` lacks are unbound; the
    /// others become `connected`. A WABA already bound to `tenant` keeps
    /// its `attached_at`. Serialized with [`Self::delete_tenant`] of the
    /// same tenant: a binding lands before the deletion (which then finds
    /// the WABA) or after it (which then finds no tenant), never beside it.
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

    /// Every tenant's WABAs, in byte order of their ids (the vault cannot
    /// list its records: key rotation walks these).
    async fn all_wabas(&self, page: &PageRequest) -> StoreResult<Listing<WabaBinding>>;

    /// A tenant's WABAs, in byte order of their ids.
    async fn wabas(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<WabaBinding>>;

    /// A WABA's numbers, in byte order of their ids (at most the 1,000
    /// attach binds).
    async fn waba_numbers(&self, waba_id: &WabaId) -> StoreResult<Vec<NumberBinding>>;

    /// A tenant's numbers, in byte order of their ids.
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
    /// claims it. Times (the lease, the expiry, and whether either ended)
    /// are read from one clock all replicas agree on: the database's where
    /// it has one, else the service's `Clock`, with its skew between
    /// replicas below `lease` (see the [module](self)).
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

    /// Delete the records past their expiry; how many went. It takes no
    /// lock of its own (housekeeping runs it under a [`LeaderLock`] turn),
    /// and is safe on two replicas at once: a record two purges both chose
    /// goes once.
    async fn purge_idempotency_keys(&self) -> StoreResult<u64>;
}

/// The name of the lock a housekeeping round runs under
/// ([`LeaderLock::try_exclusive`]): one turn per round, for all its purges;
/// one replica at a time runs a round, the others skip it
/// (docs/design/server.md, section 2.4).
pub const HOUSEKEEPING: &str = "housekeeping";

/// How long a housekeeping turn is leased for: below the interval between
/// rounds (`crate::events::HOUSEKEEPING_INTERVAL`), so a replica that died
/// holding it does not cost the next round, and above the clock skew the
/// replicas' clock tolerates (see the [module](self)). A round that runs
/// longer may overlap the next replica's, which the purges tolerate.
pub const HOUSEKEEPING_LEASE: Duration = Duration::from_secs(300);

/// Leader election for periodic work shared by the replicas of one
/// deployment, as a **lease**: of the replicas asking for `name` at once,
/// one gets the turn, and the others skip the work this round. The turn
/// ends at its release, its drop, or its lease's expiry, whichever comes
/// first, so a backend can back it with a session lock (Postgres: a
/// transaction-scoped advisory lock, the transaction cut after the lease),
/// a leased record (MongoDB, Redis: a document or key with an expiry, the
/// database's clock), or the process (memory).
///
/// **Exclusion is best-effort**: past the lease (a holder paused, a clock
/// skewed), two replicas may hold one name. Work done under a turn must
/// tolerate overlapping with another replica's: housekeeping's purges are
/// safe to run twice at once.
#[async_trait]
pub trait LeaderLock: Send + Sync + 'static {
    /// The turn for `name`, leased for `lease`, or `None` when another
    /// holder has it. The turn ends when it is released
    /// ([`LeaderTurn::release`]), dropped, or when `lease` has passed.
    async fn try_exclusive(&self, name: &str, lease: Duration) -> StoreResult<Option<LeaderTurn>>;
}

/// A turn a [`LeaderLock`] gave: held until released, dropped (a holder
/// cut before it released it, a task aborted at shutdown), or its lease
/// expired. A backend whose lease cannot end synchronously on drop (a
/// leased record deleted over the network) lets the lease end it.
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
/// without [`Turn::release`] ends it too, or lets its lease end it.
#[async_trait]
pub trait Turn: Send {
    /// End the turn.
    async fn release(self: Box<Self>) -> StoreResult<()>;
}

/// The expired rows a backend's stores leave and nothing else deletes:
/// on Postgres, the library's key/value rows (webhook dedup markers add
/// one per event). Reads already ignore them: this bounds the tables.
/// Housekeeping calls it under its [`LeaderLock`] turn; like the other
/// purges, it takes no lock of its own and is safe twice at once.
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

/// `items` (one more than asked, when there is more, in byte order of
/// their ids) as a page: at most `limit` of them, and the last one's id
/// when another page follows.
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
