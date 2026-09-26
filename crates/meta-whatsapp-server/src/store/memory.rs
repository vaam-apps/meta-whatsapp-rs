//! The memory backend ([`MemoryBackend`]): one process, emptied on
//! restart. Only for `WA_SERVER_ENV=development` and tests.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};

use async_trait::async_trait;
use meta_whatsapp_rs::adapters::store::{MemoryConversationStore, MemoryKvStore};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use time::{Duration, OffsetDateTime};

use super::{
    Backend, BackendKind, IdempotencyRecords, Janitor, LeaderLock, LeaderTurn, MemoryEventStore,
    Outbox, RecordStore, SchemaMigrator, StoreResult, Turn, listing,
};
use crate::model::{
    AllowedTenants, ApiKeyRecord, BindOutcome, BindingEpoch, DeleteTenantOutcome, IdempotencyClaim,
    IdempotencyKey, IdempotencyRecord, IdempotencyState, KeyOwner, KeyScope, Listing, NewApiKey,
    NumberBinding, NumberStatus, PageRequest, Tenant, TenantId, TenantStatus, WabaBinding,
};

/// In-memory [`RecordStore`] and [`IdempotencyRecords`]. Its event outbox is [`MemoryStore::outbox`]: one
/// process's database, so that deleting a tenant reaches its events as it
/// does on Postgres, and the outbox checks an insert's route against these
/// bindings. `Debug` shows how many records it holds, never what
/// they hold: idempotency keys name the caller's records, and a kept
/// answer's body is the caller's data.
pub struct MemoryStore {
    /// One lock over every record: a binding and a deletion of one tenant
    /// serialize on it, and the outbox's check holds it too.
    state: Arc<Mutex<State>>,
    outbox: Arc<MemoryEventStore>,
}

impl Default for MemoryStore {
    fn default() -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        Self {
            outbox: Arc::new(MemoryEventStore::of(state.clone())),
            state,
        }
    }
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("MemoryStore");
        // Not `lock`: a `{:?}` while this thread holds the lock would
        // deadlock.
        let state = match self.state.try_lock() {
            Ok(state) => Some(state),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        };
        if let Some(state) = state {
            let kept: usize = state
                .idempotency
                .values()
                .filter_map(|entry| entry.completed.as_ref())
                .map(|(_, body)| body.len())
                .sum();
            out.field("tenants", &state.tenants.len())
                .field("keys", &state.keys.len())
                .field("wabas", &state.wabas.len())
                .field("numbers", &state.numbers.len())
                .field("idempotency_records", &state.idempotency.len())
                .field("body_len", &kept);
        }
        out.finish_non_exhaustive()
    }
}

/// No `Debug`: [`MemoryStore`]'s shows counts only. Maps keyed by id, in
/// byte order (`String`'s `Ord`), which listings follow.
#[derive(Default)]
pub(super) struct State {
    tenants: BTreeMap<String, Tenant>,
    keys: BTreeMap<String, ApiKeyRecord>,
    pub(super) wabas: BTreeMap<String, WabaBinding>,
    pub(super) numbers: BTreeMap<String, NumberBinding>,
    /// Idempotency records by (tenant, key).
    idempotency: BTreeMap<(String, String), IdempotencyEntry>,
}

/// Lock `state`, a poisoned lock included (no invariant spans a panic).
pub(super) fn lock_state(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An idempotency record in memory. No `Debug`: its body is the caller's
/// data.
#[derive(Clone)]
struct IdempotencyEntry {
    fingerprint: [u8; 32],
    claim: String,
    completed: Option<(u16, Vec<u8>)>,
    lease_until: OffsetDateTime,
    expires_at: OffsetDateTime,
}

/// `now + by`, saturating.
fn after(now: OffsetDateTime, by: std::time::Duration) -> OffsetDateTime {
    now.saturating_add(Duration::try_from(by).unwrap_or(Duration::MAX))
}

fn idempotency_id(tenant: &TenantId, key: &IdempotencyKey) -> (String, String) {
    (tenant.as_str().to_owned(), key.as_str().to_owned())
}

impl MemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// The event outbox of this store: deleting a tenant here deletes its
    /// events there, and records its stream purged.
    pub fn outbox(&self) -> Arc<MemoryEventStore> {
        self.outbox.clone()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        lock_state(&self.state)
    }
}

fn page<T: Clone>(
    map: &BTreeMap<String, T>,
    page: &PageRequest,
    keep: impl Fn(&T) -> bool,
) -> Vec<(String, T)> {
    map.iter()
        .filter(|(id, _)| {
            page.after
                .as_deref()
                .is_none_or(|after| id.as_str() > after)
        })
        .filter(|(_, item)| keep(item))
        .take(page.limit + 1)
        .map(|(id, item)| (id.clone(), item.clone()))
        .collect()
}

fn in_scope(key: &ApiKeyRecord, scope: &KeyScope) -> bool {
    match (scope, &key.owner) {
        (KeyScope::Tenant(tenant), KeyOwner::Tenant(owner)) => tenant == owner,
        (KeyScope::Platform, KeyOwner::Platform(_)) | (KeyScope::Admin, KeyOwner::Admin) => true,
        _ => false,
    }
}

#[async_trait]
impl RecordStore for MemoryStore {
    async fn ping(&self) -> StoreResult<()> {
        Ok(())
    }

    async fn create_tenant(&self, id: &TenantId, name: &str) -> StoreResult<Option<Tenant>> {
        let mut state = self.lock();
        if state.tenants.contains_key(id.as_str()) {
            return Ok(None);
        }
        let now = OffsetDateTime::now_utc();
        let tenant = Tenant {
            id: id.clone(),
            name: name.to_owned(),
            status: TenantStatus::Active,
            created_at: now,
            updated_at: now,
        };
        state.tenants.insert(id.as_str().to_owned(), tenant.clone());
        Ok(Some(tenant))
    }

    async fn tenant(&self, id: &TenantId) -> StoreResult<Option<Tenant>> {
        Ok(self.lock().tenants.get(id.as_str()).cloned())
    }

    async fn tenants(&self, request: &PageRequest) -> StoreResult<Listing<Tenant>> {
        let items = page(&self.lock().tenants, request, |_| true);
        Ok(listing(
            items.into_iter().map(|(_, t)| t).collect(),
            request.limit,
            |t: &Tenant| t.id.as_str().to_owned(),
        ))
    }

    async fn update_tenant(
        &self,
        id: &TenantId,
        name: Option<&str>,
        status: Option<TenantStatus>,
    ) -> StoreResult<Option<Tenant>> {
        let mut state = self.lock();
        let Some(tenant) = state.tenants.get_mut(id.as_str()) else {
            return Ok(None);
        };
        if let Some(name) = name {
            name.clone_into(&mut tenant.name);
        }
        if let Some(status) = status {
            tenant.status = status;
        }
        tenant.updated_at = OffsetDateTime::now_utc();
        Ok(Some(tenant.clone()))
    }

    async fn delete_tenant(&self, id: &TenantId) -> StoreResult<DeleteTenantOutcome> {
        let mut state = self.lock();
        if !state.tenants.contains_key(id.as_str()) {
            return Ok(DeleteTenantOutcome::NotFound);
        }
        if state.wabas.values().any(|w| &w.tenant_id == id) {
            return Ok(DeleteTenantOutcome::HasWabas);
        }
        state.tenants.remove(id.as_str());
        state
            .keys
            .retain(|_, key| !matches!(&key.owner, KeyOwner::Tenant(t) if t == id));
        // A platform key allowed this tenant does not carry the allowance
        // over to a tenant created later with the same id.
        for key in state.keys.values_mut() {
            if let KeyOwner::Platform(AllowedTenants::Only(list)) = &mut key.owner {
                list.retain(|t| t != id);
            }
        }
        // Its idempotency records go too, as on Postgres (ON DELETE CASCADE).
        state
            .idempotency
            .retain(|(tenant, _), _| tenant != id.as_str());
        // Its events: under the store's lock, as Postgres does it in the
        // deleting transaction.
        self.outbox.forget_tenant(id);
        Ok(DeleteTenantOutcome::Deleted)
    }

    async fn insert_key(&self, key: &NewApiKey) -> StoreResult<Option<ApiKeyRecord>> {
        let mut state = self.lock();
        if state.keys.contains_key(&key.key_id) {
            return Ok(None);
        }
        if let KeyOwner::Tenant(tenant) = &key.owner
            && !state.tenants.contains_key(tenant.as_str())
        {
            return Err(meta_whatsapp_rs::core::error::StorageError::Backend(
                anyhow::anyhow!("no such tenant"),
            ));
        }
        let record = ApiKeyRecord {
            key_id: key.key_id.clone(),
            secret_sha256: key.secret_sha256,
            owner: key.owner.clone(),
            scopes: key.scopes.clone(),
            name: key.name.clone(),
            created_at: OffsetDateTime::now_utc(),
            expires_at: key.expires_at,
            revoked_at: None,
            last_used_at: None,
        };
        state.keys.insert(key.key_id.clone(), record.clone());
        Ok(Some(record))
    }

    async fn key(&self, key_id: &str) -> StoreResult<Option<ApiKeyRecord>> {
        Ok(self.lock().keys.get(key_id).cloned())
    }

    async fn keys(
        &self,
        scope: &KeyScope,
        request: &PageRequest,
    ) -> StoreResult<Listing<ApiKeyRecord>> {
        let items = page(&self.lock().keys, request, |k| in_scope(k, scope));
        Ok(listing(
            items.into_iter().map(|(_, k)| k).collect(),
            request.limit,
            |k: &ApiKeyRecord| k.key_id.clone(),
        ))
    }

    async fn revoke_key(&self, scope: &KeyScope, key_id: &str) -> StoreResult<bool> {
        let mut state = self.lock();
        match state.keys.get_mut(key_id) {
            Some(key) if in_scope(key, scope) => {
                key.revoked_at.get_or_insert_with(OffsetDateTime::now_utc);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn touch_key(&self, key_id: &str) -> StoreResult<()> {
        let now = OffsetDateTime::now_utc();
        if let Some(key) = self.lock().keys.get_mut(key_id)
            && key
                .last_used_at
                .is_none_or(|at| at < now - Duration::minutes(1))
        {
            key.last_used_at = Some(now);
        }
        Ok(())
    }

    async fn bind_waba(
        &self,
        tenant: &TenantId,
        waba_id: &WabaId,
        numbers: &[PhoneNumberId],
    ) -> StoreResult<BindOutcome> {
        let mut state = self.lock();
        // Under the lock `delete_tenant` takes: the tenant exists until
        // this binding is made.
        if !state.tenants.contains_key(tenant.as_str()) {
            return Ok(BindOutcome::NoSuchTenant);
        }
        if state
            .wabas
            .get(waba_id.as_str())
            .is_some_and(|w| &w.tenant_id != tenant)
            || numbers.iter().any(|n| {
                state
                    .numbers
                    .get(n.as_str())
                    .is_some_and(|b| &b.tenant_id != tenant)
            })
        {
            return Ok(BindOutcome::OwnedByAnotherTenant);
        }
        let now = OffsetDateTime::now_utc();
        state
            .wabas
            .entry(waba_id.as_str().to_owned())
            .or_insert_with(|| WabaBinding {
                waba_id: waba_id.clone(),
                tenant_id: tenant.clone(),
                credit_allocation_id: None,
                attached_at: now,
            });
        state
            .numbers
            .retain(|pn, b| &b.waba_id != waba_id || numbers.iter().any(|n| n.as_str() == pn));
        for number in numbers {
            state.numbers.insert(
                number.as_str().to_owned(),
                NumberBinding {
                    phone_number_id: number.clone(),
                    waba_id: waba_id.clone(),
                    tenant_id: tenant.clone(),
                    status: NumberStatus::Connected,
                    updated_at: now,
                },
            );
        }
        Ok(BindOutcome::Bound)
    }

    async fn unbind_waba(&self, waba_id: &WabaId) -> StoreResult<bool> {
        let mut state = self.lock();
        let removed = state.wabas.remove(waba_id.as_str()).is_some();
        state.numbers.retain(|_, b| &b.waba_id != waba_id);
        Ok(removed)
    }

    async fn unbind_waba_if(&self, epoch: &BindingEpoch) -> StoreResult<bool> {
        let mut state = self.lock();
        if !holds(&state, epoch) {
            return Ok(false);
        }
        state.wabas.remove(epoch.waba_id.as_str());
        state.numbers.retain(|_, b| b.waba_id != epoch.waba_id);
        Ok(true)
    }

    async fn waba(&self, waba_id: &WabaId) -> StoreResult<Option<WabaBinding>> {
        Ok(self.lock().wabas.get(waba_id.as_str()).cloned())
    }

    async fn number(&self, phone_number_id: &PhoneNumberId) -> StoreResult<Option<NumberBinding>> {
        Ok(self.lock().numbers.get(phone_number_id.as_str()).cloned())
    }

    async fn all_wabas(&self, request: &PageRequest) -> StoreResult<Listing<WabaBinding>> {
        let items = page(&self.lock().wabas, request, |_| true);
        Ok(listing(
            items.into_iter().map(|(_, w)| w).collect(),
            request.limit,
            |w: &WabaBinding| w.waba_id.as_str().to_owned(),
        ))
    }

    async fn wabas(
        &self,
        tenant: &TenantId,
        request: &PageRequest,
    ) -> StoreResult<Listing<WabaBinding>> {
        let items = page(&self.lock().wabas, request, |w| &w.tenant_id == tenant);
        Ok(listing(
            items.into_iter().map(|(_, w)| w).collect(),
            request.limit,
            |w: &WabaBinding| w.waba_id.as_str().to_owned(),
        ))
    }

    async fn waba_numbers(&self, waba_id: &WabaId) -> StoreResult<Vec<NumberBinding>> {
        Ok(self
            .lock()
            .numbers
            .values()
            .filter(|n| &n.waba_id == waba_id)
            .cloned()
            .collect())
    }

    async fn numbers(
        &self,
        tenant: &TenantId,
        request: &PageRequest,
    ) -> StoreResult<Listing<NumberBinding>> {
        let items = page(&self.lock().numbers, request, |n| &n.tenant_id == tenant);
        Ok(listing(
            items.into_iter().map(|(_, n)| n).collect(),
            request.limit,
            |n: &NumberBinding| n.phone_number_id.as_str().to_owned(),
        ))
    }

    async fn set_waba_status(&self, waba_id: &WabaId, status: NumberStatus) -> StoreResult<()> {
        let now = OffsetDateTime::now_utc();
        for number in self.lock().numbers.values_mut() {
            if &number.waba_id == waba_id {
                number.status = status;
                number.updated_at = now;
            }
        }
        Ok(())
    }

    async fn set_waba_status_if(
        &self,
        epoch: &BindingEpoch,
        status: NumberStatus,
    ) -> StoreResult<bool> {
        let now = OffsetDateTime::now_utc();
        let mut state = self.lock();
        if !holds(&state, epoch) {
            return Ok(false);
        }
        for number in state.numbers.values_mut() {
            if number.waba_id == epoch.waba_id {
                number.status = status;
                number.updated_at = now;
            }
        }
        Ok(true)
    }
}

/// Whether `epoch.waba_id` is still bound as `epoch` says: the same
/// tenant, since the same instant.
fn holds(state: &State, epoch: &BindingEpoch) -> bool {
    state
        .wabas
        .get(epoch.waba_id.as_str())
        .is_some_and(|w| w.tenant_id == epoch.tenant_id && w.attached_at == epoch.attached_at)
}

#[async_trait]
impl IdempotencyRecords for MemoryStore {
    async fn claim_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        fingerprint: &[u8; 32],
        claim: &str,
        lease: std::time::Duration,
        ttl: std::time::Duration,
    ) -> StoreResult<IdempotencyClaim> {
        let now = OffsetDateTime::now_utc();
        let mut state = self.lock();
        let id = idempotency_id(tenant, key);
        if let Some(entry) = state.idempotency.get(&id)
            && entry.expires_at > now
        {
            let state = match &entry.completed {
                Some((status, body)) => IdempotencyState::Completed {
                    status: *status,
                    body: body.clone(),
                },
                None => IdempotencyState::InProgress {
                    lease_expired: entry.lease_until <= now,
                },
            };
            return Ok(IdempotencyClaim::Existing(IdempotencyRecord {
                fingerprint: entry.fingerprint,
                state,
            }));
        }
        state.idempotency.insert(
            id,
            IdempotencyEntry {
                fingerprint: *fingerprint,
                claim: claim.to_owned(),
                completed: None,
                lease_until: after(now, lease),
                expires_at: after(now, ttl),
            },
        );
        Ok(IdempotencyClaim::Claimed)
    }

    async fn complete_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        claim: &str,
        status: u16,
        body: &[u8],
    ) -> StoreResult<bool> {
        let mut state = self.lock();
        match state.idempotency.get_mut(&idempotency_id(tenant, key)) {
            Some(entry) if entry.claim == claim => {
                entry.completed = Some((status, body.to_vec()));
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn release_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        claim: &str,
    ) -> StoreResult<bool> {
        let mut state = self.lock();
        let id = idempotency_id(tenant, key);
        if state
            .idempotency
            .get(&id)
            .is_some_and(|entry| entry.claim == claim)
        {
            state.idempotency.remove(&id);
            return Ok(true);
        }
        Ok(false)
    }

    async fn purge_idempotency_keys(&self) -> StoreResult<u64> {
        let now = OffsetDateTime::now_utc();
        let mut state = self.lock();
        let before = state.idempotency.len();
        state.idempotency.retain(|_, entry| entry.expires_at > now);
        Ok(u64::try_from(before - state.idempotency.len()).unwrap_or(u64::MAX))
    }
}

/// Leader election within one process: the memory backend's replicas are
/// the tasks of one process. A turn is a lease on the process's monotonic
/// clock: it ends at its release, its drop, or `lease` after it was
/// given, and a turn that ended never ends a later holder's. Cheap to
/// clone; clones share their turns.
#[derive(Debug, Default, Clone)]
pub struct MemoryLeaderLock {
    held: Arc<Mutex<Leases>>,
}

/// The turns held, by name: which grant holds each, and until when
/// (`None`: a lease too long to represent, never).
#[derive(Debug, Default)]
struct Leases {
    by_name: BTreeMap<String, (u64, Option<std::time::Instant>)>,
    granted: u64,
}

impl MemoryLeaderLock {
    /// No turn held.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl LeaderLock for MemoryLeaderLock {
    async fn try_exclusive(
        &self,
        name: &str,
        lease: std::time::Duration,
    ) -> StoreResult<Option<LeaderTurn>> {
        let now = std::time::Instant::now();
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if held
            .by_name
            .get(name)
            .is_some_and(|(_, until)| until.is_none_or(|until| now < until))
        {
            return Ok(None);
        }
        held.granted = held.granted.wrapping_add(1);
        let grant = held.granted;
        held.by_name
            .insert(name.to_owned(), (grant, now.checked_add(lease)));
        Ok(Some(LeaderTurn::new(MemoryTurn {
            held: self.held.clone(),
            name: name.to_owned(),
            grant,
        })))
    }
}

/// A turn of [`MemoryLeaderLock`]: its name goes when it is released or
/// dropped, unless its lease ended and another holder has it since.
struct MemoryTurn {
    held: Arc<Mutex<Leases>>,
    name: String,
    grant: u64,
}

impl Drop for MemoryTurn {
    fn drop(&mut self) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if held
            .by_name
            .get(&self.name)
            .is_some_and(|(grant, _)| *grant == self.grant)
        {
            held.by_name.remove(&self.name);
        }
    }
}

#[async_trait]
impl Turn for MemoryTurn {
    async fn release(self: Box<Self>) -> StoreResult<()> {
        drop(self);
        Ok(())
    }
}

/// What memory has no work for: no schema to migrate, and no sweep. The
/// library's `MemoryKvStore` does keep what expired (its reads ignore it)
/// until its own `purge_expired`, which the service does not call: memory
/// storage is one development process's, emptied on restart.
#[derive(Debug)]
struct Nothing;

#[async_trait]
impl Janitor for Nothing {
    async fn purge_expired(&self) -> StoreResult<u64> {
        Ok(0)
    }
}

#[async_trait]
impl SchemaMigrator for Nothing {
    async fn migrate(&self) -> StoreResult<()> {
        Ok(())
    }
}

/// Every port in one process's memory, emptied on restart
/// (`WA_SERVER_ENV=development` and tests): [`MemoryStore`] and its
/// outbox, the library's `MemoryKvStore` and `MemoryConversationStore`,
/// and a [`MemoryLeaderLock`].
#[derive(Debug, Clone)]
pub struct MemoryBackend {
    store: Arc<MemoryStore>,
    outbox: Arc<MemoryEventStore>,
    leader: Arc<MemoryLeaderLock>,
    kv: Arc<MemoryKvStore>,
    conversations: Arc<MemoryConversationStore>,
}

impl Default for MemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryBackend {
    /// Empty stores.
    pub fn new() -> Self {
        let store = MemoryStore::new();
        Self {
            outbox: store.outbox(),
            store: Arc::new(store),
            leader: Arc::new(MemoryLeaderLock::new()),
            kv: Arc::new(MemoryKvStore::new()),
            conversations: Arc::new(MemoryConversationStore::new()),
        }
    }
}

#[async_trait]
impl Backend for MemoryBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Memory
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

    fn leader_lock(&self) -> Arc<dyn LeaderLock> {
        self.leader.clone()
    }

    fn janitor(&self) -> Arc<dyn Janitor> {
        Arc::new(Nothing)
    }

    fn migrator(&self) -> Arc<dyn SchemaMigrator> {
        Arc::new(Nothing)
    }

    fn kv(&self) -> Arc<dyn KvStore> {
        self.kv.clone()
    }

    fn conversations(&self) -> Arc<dyn ConversationStore> {
        self.conversations.clone()
    }

    async fn close(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Debug` counts the records and the kept bodies' bytes, and prints
    /// none of them: not a body, not an idempotency key. Decisive: the
    /// hand-written `Debug` (a derived one prints both).
    #[tokio::test]
    async fn debug_shows_counts_never_a_body_or_a_key() {
        let store = MemoryStore::new();
        let tenant = TenantId::parse("tenant-a").unwrap();
        let key = IdempotencyKey::parse("order:SECRET-KEY-31:shipped").unwrap();
        let body = br#"{"messages":[{"id":"wamid.SECRET-BODY-7Q"}]}"#;
        let claimed = store
            .claim_idempotency_key(
                &tenant,
                &key,
                &[9; 32],
                "claim-1",
                std::time::Duration::from_secs(60),
                std::time::Duration::from_secs(600),
            )
            .await
            .unwrap();
        assert_eq!(claimed, IdempotencyClaim::Claimed);
        assert!(
            store
                .complete_idempotency_key(&tenant, &key, "claim-1", 200, body)
                .await
                .unwrap()
        );
        let debug = format!("{store:?}");
        assert!(debug.contains("idempotency_records: 1"), "{debug}");
        assert!(
            debug.contains(&format!("body_len: {}", body.len())),
            "{debug}"
        );
        let bytes = format!("{:?}", body.to_vec());
        for secret in [
            "SECRET-BODY",
            "SECRET-KEY",
            "claim-1",
            &bytes[1..bytes.len() - 1],
            "9, 9, 9",
        ] {
            assert!(!debug.contains(secret), "{debug} shows {secret}");
        }
        // Held by this thread: no deadlock, and still nothing shown.
        let held = store.lock();
        assert_eq!(format!("{store:?}"), "MemoryStore { .. }");
        drop(held);
    }

    /// One turn per name at a time; released or dropped, it is free again.
    /// Decisive: the name's removal on drop.
    #[tokio::test]
    async fn one_turn_per_name_until_released_or_dropped() {
        let lock = MemoryLeaderLock::new();
        let turn = lock
            .try_exclusive("housekeeping", LEASE)
            .await
            .unwrap()
            .unwrap();
        assert!(
            lock.try_exclusive("housekeeping", LEASE)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            lock.clone()
                .try_exclusive("housekeeping", LEASE)
                .await
                .unwrap()
                .is_none()
        );
        let other = lock.try_exclusive("elsewhere", LEASE).await.unwrap();
        assert!(other.is_some(), "each name its own");
        turn.release().await.unwrap();
        let again = lock.try_exclusive("housekeeping", LEASE).await.unwrap();
        assert!(again.is_some(), "released");
        drop(again);
        assert!(
            lock.try_exclusive("housekeeping", LEASE)
                .await
                .unwrap()
                .is_some(),
            "dropped"
        );
    }

    const LEASE: std::time::Duration = std::time::Duration::from_secs(60);

    /// A turn is a lease: past it, another holder gets the name even
    /// though the first never released it; and the first one's end (its
    /// release or drop) then leaves the second one's turn alone. Decisive:
    /// the lease's expiry in `try_exclusive`, and the grant compared on
    /// drop.
    #[tokio::test]
    async fn a_turn_ends_with_its_lease_and_never_ends_a_later_one() {
        let lock = MemoryLeaderLock::new();
        let short = std::time::Duration::from_millis(30);
        let stale = lock.try_exclusive("sweep", short).await.unwrap().unwrap();
        assert!(lock.try_exclusive("sweep", LEASE).await.unwrap().is_none());
        tokio::time::sleep(short * 2).await;
        let fresh = lock
            .try_exclusive("sweep", LEASE)
            .await
            .unwrap()
            .expect("the first lease ended");
        stale.release().await.unwrap();
        assert!(
            lock.try_exclusive("sweep", LEASE).await.unwrap().is_none(),
            "the stale turn's release ended the fresh one"
        );
        drop(fresh);
        assert!(lock.try_exclusive("sweep", LEASE).await.unwrap().is_some());
    }
}
