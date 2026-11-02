//! A memory backend of the core's own tests, with no server crate: the
//! ports the admin, messages, templates, numbers, media and webhooks
//! suites (`conformance::admin`, `conformance::messages`,
//! `conformance::templates`, `conformance::numbers`, `conformance::media`,
//! `conformance::webhooks`), and the capabilities suite's vault writes
//! (`conformance::capabilities`), reach. Its records keep decision D4
//! (`bind_waba` refuses a WABA another tenant holds) and condition an
//! unbind and a status change on the binding's epoch, its idempotency
//! records lease and expire keys on the process's monotonic clock, its
//! key/value store (the vault's, the webhook dedup leases') is
//! `support::Kv`, its outbox keeps a row's tenant only while the binding
//! it was routed by holds (checked under the records' lock), and its inbox
//! is the library's `MemoryConversationStore`. The ports no suite reaches
//! are not there.
#![allow(dead_code)] // each test binary uses a different subset

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use meta_whatsapp_rs::adapters::store::MemoryConversationStore;
use meta_whatsapp_rs::core::ids::{MediaId, PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_server_core::backend::{Backend, BackendKind};
use meta_whatsapp_server_core::model::{
    ApiKeyRecord, BindOutcome, BindingEpoch, DeleteTenantOutcome, IdempotencyClaim, IdempotencyKey,
    IdempotencyRecord, IdempotencyState, KeyScope, Listing, NewApiKey, NewSignupAttempt,
    NumberBinding, NumberStatus, NumberTombstone, OtpSettings, PageRequest, SignupAttempt, Tenant,
    TenantId, TenantStatus, VaultEntry, WabaBinding, WabaTombstone,
};
use meta_whatsapp_server_core::notify::{EventNotifier, LocalNotifier};
use meta_whatsapp_server_core::outbox::{
    EventPage, EventQuery, GuardedBinding, NewEvent, Outbox, RECEIVED_MEDIA_ROWS, ReceivedMedia,
    RouteGuard, StoredEvent,
};
use meta_whatsapp_server_core::store::{
    IdempotencyRecords, Janitor, LeaderLock, RecordStore, SchemaMigrator, StoreResult, listing,
};
use time::OffsetDateTime;

use crate::support;

#[derive(Default)]
pub struct State {
    tenants: BTreeMap<String, Tenant>,
    keys: BTreeMap<String, ApiKeyRecord>,
    wabas: BTreeMap<String, WabaBinding>,
    numbers: BTreeMap<String, NumberBinding>,
    /// Each WABA's last unbinding (roadmap S2b: the webhooks suite's moves).
    tombstones: BTreeMap<String, WabaTombstone>,
    /// Each number's last release (roadmap M2h: the inbox suite's moves).
    number_tombstones: BTreeMap<String, NumberTombstone>,
    /// Each tenant's OTP settings, stored (roadmap M3c: the otp suite).
    otp_settings: BTreeMap<String, serde_json::Value>,
    /// Signup attempts by WABA, gone with their binding (roadmap M3a: the
    /// signup suite).
    signup_attempts: BTreeMap<String, SignupAttempt>,
    /// The vault's records kept past a binding, by key (roadmap M3a).
    vault_entries: BTreeMap<String, VaultEntry>,
}

/// Release the bound numbers of `waba_id` that `keep` does not list, each
/// leaving its tombstone at `at`.
fn release(state: &mut State, waba_id: &WabaId, keep: &[PhoneNumberId], at: OffsetDateTime) {
    let gone: Vec<String> = state
        .numbers
        .iter()
        .filter(|(pn, n)| &n.waba_id == waba_id && !keep.iter().any(|k| k.as_str() == *pn))
        .map(|(pn, _)| pn.clone())
        .collect();
    for pn in gone {
        if let Some(binding) = state.numbers.remove(&pn) {
            state
                .number_tombstones
                .insert(pn, NumberTombstone::of(&binding, at));
        }
    }
}

/// Remove `waba_id`'s binding and its numbers, leaving its tombstone, in
/// one step; whether it was bound.
fn unbind(state: &mut State, waba_id: &WabaId) -> bool {
    let Some(binding) = state.wabas.remove(waba_id.as_str()) else {
        return false;
    };
    release(state, waba_id, &[], OffsetDateTime::now_utc());
    state.signup_attempts.remove(waba_id.as_str());
    state.tombstones.insert(
        waba_id.as_str().to_owned(),
        WabaTombstone::of(&binding, OffsetDateTime::now_utc()),
    );
    true
}

/// Records in memory, as far as the suites this backend runs reach them:
/// tenants, keys and bindings, with D4 kept by `bind_waba`.
#[derive(Default)]
pub struct Records(Mutex<State>);

impl Records {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }
}

#[async_trait]
impl RecordStore for Records {
    async fn ping(&self) -> StoreResult<()> {
        Ok(())
    }
    async fn create_tenant(&self, id: &TenantId, name: &str) -> StoreResult<Option<Tenant>> {
        let mut state = self.state();
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
        Ok(self.state().tenants.get(id.as_str()).cloned())
    }
    async fn tenants(&self, page: &PageRequest) -> StoreResult<Listing<Tenant>> {
        let items = self.state().tenants.values().cloned().collect();
        Ok(listing(items, page.limit, |t| t.id.as_str().to_owned()))
    }
    async fn update_tenant(
        &self,
        id: &TenantId,
        name: Option<&str>,
        status: Option<TenantStatus>,
    ) -> StoreResult<Option<Tenant>> {
        let mut state = self.state();
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
    async fn otp_settings(&self, id: &TenantId) -> StoreResult<Option<OtpSettings>> {
        Ok(self
            .state()
            .otp_settings
            .get(id.as_str())
            .map(|stored| OtpSettings::from_stored(stored).expect("stored by this backend")))
    }
    async fn set_otp_settings(
        &self,
        id: &TenantId,
        settings: Option<&OtpSettings>,
    ) -> StoreResult<bool> {
        let mut state = self.state();
        if !state.tenants.contains_key(id.as_str()) {
            return Ok(false);
        }
        match settings {
            Some(settings) => state
                .otp_settings
                .insert(id.as_str().to_owned(), settings.to_stored()),
            None => state.otp_settings.remove(id.as_str()),
        };
        Ok(true)
    }
    async fn delete_tenant(&self, _: &TenantId) -> StoreResult<DeleteTenantOutcome> {
        unimplemented!("not used by the suites this backend runs")
    }
    async fn insert_key(&self, key: &NewApiKey) -> StoreResult<Option<ApiKeyRecord>> {
        let mut state = self.state();
        if state.keys.contains_key(&key.key_id) {
            return Ok(None);
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
        Ok(self.state().keys.get(key_id).cloned())
    }
    async fn keys(&self, _: &KeyScope, _: &PageRequest) -> StoreResult<Listing<ApiKeyRecord>> {
        unimplemented!("not used by the suites this backend runs")
    }
    async fn revoke_key(&self, _: &KeyScope, _: &str) -> StoreResult<bool> {
        unimplemented!("not used by the suites this backend runs")
    }
    async fn touch_key(&self, _: &str) -> StoreResult<()> {
        Ok(())
    }
    async fn bind_waba(
        &self,
        tenant: &TenantId,
        waba_id: &WabaId,
        numbers: &[PhoneNumberId],
    ) -> StoreResult<BindOutcome> {
        let mut state = self.state();
        if !state.tenants.contains_key(tenant.as_str()) {
            return Ok(BindOutcome::NoSuchTenant);
        }
        // D4, atomically with the binding: the WABA, or any of its numbers,
        // held by another tenant.
        let held_elsewhere = state
            .wabas
            .get(waba_id.as_str())
            .is_some_and(|w| &w.tenant_id != tenant)
            || numbers.iter().any(|pn| {
                state
                    .numbers
                    .get(pn.as_str())
                    .is_some_and(|n| &n.tenant_id != tenant)
            });
        if held_elsewhere {
            return Ok(BindOutcome::OwnedByAnotherTenant);
        }
        let now = OffsetDateTime::now_utc();
        // A refresh keeps the binding's start and its previous tenant and
        // moves its generation; a new binding reads the WABA's tombstone
        // (roadmap S2b, the port's contract).
        let binding = match state.wabas.get(waba_id.as_str()) {
            Some(w) => WabaBinding {
                generation: w.generation + 1,
                ..w.clone()
            },
            None => WabaBinding {
                waba_id: waba_id.clone(),
                tenant_id: tenant.clone(),
                credit_allocation_id: None,
                attached_at: now,
                generation: 1,
                previous_tenant_unbound_at: state
                    .tombstones
                    .get(waba_id.as_str())
                    .and_then(|t| t.previous_for(tenant)),
            },
        };
        state.wabas.insert(waba_id.as_str().to_owned(), binding);
        // Exactly `numbers`: those the WABA had and the list lacks are
        // unbound (the port's contract).
        release(&mut state, waba_id, numbers, now);
        for pn in numbers {
            let bound = match state.numbers.get(pn.as_str()) {
                Some(held) => NumberBinding {
                    waba_id: waba_id.clone(),
                    status: NumberStatus::Connected,
                    updated_at: now,
                    ..held.clone()
                },
                None => NumberBinding::newly_bound(
                    pn.clone(),
                    waba_id.clone(),
                    tenant.clone(),
                    now,
                    state.number_tombstones.get(pn.as_str()),
                ),
            };
            state.numbers.insert(pn.as_str().to_owned(), bound);
        }
        Ok(BindOutcome::Bound)
    }
    async fn unbind_waba(&self, waba_id: &WabaId) -> StoreResult<bool> {
        // An admin's unbind of a WABA whose token was not opened
        // (`conformance::capabilities`' vault writes): the binding and its
        // numbers.
        Ok(unbind(&mut self.state(), waba_id))
    }
    async fn unbind_waba_if(&self, epoch: &BindingEpoch) -> StoreResult<bool> {
        // The numbers suite's disconnection: the binding and its numbers,
        // only while the binding is the one `epoch` read.
        let mut state = self.state();
        let held = state
            .wabas
            .get(epoch.waba_id.as_str())
            .is_some_and(|w| w.epoch() == *epoch);
        Ok(held && unbind(&mut state, &epoch.waba_id))
    }
    async fn waba(&self, waba_id: &WabaId) -> StoreResult<Option<WabaBinding>> {
        Ok(self.state().wabas.get(waba_id.as_str()).cloned())
    }
    async fn number(&self, pn: &PhoneNumberId) -> StoreResult<Option<NumberBinding>> {
        Ok(self.state().numbers.get(pn.as_str()).cloned())
    }
    async fn all_wabas(&self, page: &PageRequest) -> StoreResult<Listing<WabaBinding>> {
        // The vault's rotation walks them (`conformance::capabilities`).
        let items = self
            .state()
            .wabas
            .values()
            .filter(|w| {
                page.after
                    .as_deref()
                    .is_none_or(|after| w.waba_id.as_str() > after)
            })
            .cloned()
            .collect();
        Ok(listing(items, page.limit, |w| {
            w.waba_id.as_str().to_owned()
        }))
    }
    async fn wabas(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<WabaBinding>> {
        let items = self
            .state()
            .wabas
            .values()
            .filter(|w| &w.tenant_id == tenant)
            .cloned()
            .collect();
        Ok(listing(items, page.limit, |w| {
            w.waba_id.as_str().to_owned()
        }))
    }
    async fn waba_numbers(&self, waba_id: &WabaId) -> StoreResult<Vec<NumberBinding>> {
        Ok(self
            .state()
            .numbers
            .values()
            .filter(|n| &n.waba_id == waba_id)
            .cloned()
            .collect())
    }
    async fn numbers(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<NumberBinding>> {
        let items = self
            .state()
            .numbers
            .values()
            .filter(|n| &n.tenant_id == tenant)
            .cloned()
            .collect();
        Ok(listing(items, page.limit, |n| {
            n.phone_number_id.as_str().to_owned()
        }))
    }
    async fn set_waba_status(&self, waba_id: &WabaId, status: NumberStatus) -> StoreResult<()> {
        // An admin's `190` after an attach stored the token
        // (`conformance::capabilities`' vault writes).
        let now = OffsetDateTime::now_utc();
        for number in self.state().numbers.values_mut() {
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
        // A `190` marking the numbers of the binding a capability was made
        // from (the numbers and templates suites), only while it holds.
        let mut state = self.state();
        let held = state
            .wabas
            .get(epoch.waba_id.as_str())
            .is_some_and(|w| w.epoch() == *epoch);
        if !held {
            return Ok(false);
        }
        let now = OffsetDateTime::now_utc();
        for number in state.numbers.values_mut() {
            if number.waba_id == epoch.waba_id {
                number.status = status;
                number.updated_at = now;
            }
        }
        Ok(true)
    }
    async fn set_credit_allocation_if(
        &self,
        epoch: &BindingEpoch,
        allocation_config_id: &str,
    ) -> StoreResult<bool> {
        let mut state = self.state();
        match state.wabas.get_mut(epoch.waba_id.as_str()) {
            Some(binding) if binding.epoch() == *epoch => {
                binding.credit_allocation_id = Some(allocation_config_id.to_owned());
                Ok(true)
            }
            _ => Ok(false),
        }
    }
    async fn save_signup_attempt(&self, attempt: &NewSignupAttempt) -> StoreResult<bool> {
        let mut state = self.state();
        let bound = state
            .wabas
            .get(attempt.waba_id.as_str())
            .is_some_and(|w| w.tenant_id == attempt.tenant_id);
        if !bound {
            return Ok(false);
        }
        let now = OffsetDateTime::now_utc();
        let created_at = state
            .signup_attempts
            .get(attempt.waba_id.as_str())
            .filter(|kept| kept.tenant_id == attempt.tenant_id)
            .map_or(now, |kept| kept.created_at);
        state.signup_attempts.insert(
            attempt.waba_id.as_str().to_owned(),
            SignupAttempt {
                tenant_id: attempt.tenant_id.clone(),
                waba_id: attempt.waba_id.clone(),
                failed_step: attempt.failed_step.clone(),
                session: attempt.session.clone(),
                finish_kind: attempt.finish_kind.clone(),
                created_at,
                updated_at: now,
            },
        );
        Ok(true)
    }
    async fn signup_attempt(
        &self,
        tenant: &TenantId,
        waba_id: &WabaId,
    ) -> StoreResult<Option<SignupAttempt>> {
        Ok(self
            .state()
            .signup_attempts
            .get(waba_id.as_str())
            .filter(|a| &a.tenant_id == tenant)
            .cloned())
    }
    async fn signup_attempts(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<SignupAttempt>> {
        let items: Vec<SignupAttempt> = self
            .state()
            .signup_attempts
            .iter()
            .filter(|(id, a)| {
                &a.tenant_id == tenant
                    && page
                        .after
                        .as_deref()
                        .is_none_or(|after| id.as_str() > after)
            })
            .take(page.limit + 1)
            .map(|(_, a)| a.clone())
            .collect();
        Ok(listing(items, page.limit, |a| {
            a.waba_id.as_str().to_owned()
        }))
    }
    async fn delete_signup_attempt(
        &self,
        tenant: &TenantId,
        waba_id: &WabaId,
    ) -> StoreResult<bool> {
        let mut state = self.state();
        let theirs = state
            .signup_attempts
            .get(waba_id.as_str())
            .is_some_and(|a| &a.tenant_id == tenant);
        if theirs {
            state.signup_attempts.remove(waba_id.as_str());
        }
        Ok(theirs)
    }
    async fn record_vault_entry(&self, entry: &VaultEntry) -> StoreResult<()> {
        self.state()
            .vault_entries
            .entry(entry.key())
            .or_insert_with(|| entry.clone());
        Ok(())
    }
    async fn vault_entries(&self, page: &PageRequest) -> StoreResult<Listing<VaultEntry>> {
        let items: Vec<VaultEntry> = self
            .state()
            .vault_entries
            .iter()
            .filter(|(key, _)| {
                page.after
                    .as_deref()
                    .is_none_or(|after| key.as_str() > after)
            })
            .take(page.limit + 1)
            .map(|(_, e)| e.clone())
            .collect();
        Ok(listing(items, page.limit, VaultEntry::key))
    }
}

/// A key's record: the request's fingerprint, the claim holding it, its
/// state (`None` while in progress) and when its lease and its record end.
struct Kept {
    fingerprint: [u8; 32],
    claim: String,
    answer: Option<(u16, Vec<u8>)>,
    lease_until: Instant,
    expires_at: Instant,
}

/// Idempotency records in memory, by tenant and key: one lock, so a claim
/// is atomic.
#[derive(Default)]
pub struct Idempotency(Mutex<BTreeMap<(String, String), Kept>>);

impl Idempotency {
    fn id(tenant: &TenantId, key: &IdempotencyKey) -> (String, String) {
        (tenant.as_str().to_owned(), key.as_str().to_owned())
    }
}

#[async_trait]
impl IdempotencyRecords for Idempotency {
    async fn claim_idempotency_key(
        &self,
        tenant: &TenantId,
        key: &IdempotencyKey,
        fingerprint: &[u8; 32],
        claim: &str,
        lease: Duration,
        ttl: Duration,
    ) -> StoreResult<IdempotencyClaim> {
        let now = Instant::now();
        let mut kept = self.0.lock().unwrap();
        if let Some(record) = kept.get(&Self::id(tenant, key))
            && record.expires_at > now
        {
            let state = match &record.answer {
                Some((status, body)) => IdempotencyState::Completed {
                    status: *status,
                    body: body.clone(),
                },
                None => IdempotencyState::InProgress {
                    lease_expired: record.lease_until <= now,
                },
            };
            return Ok(IdempotencyClaim::Existing(IdempotencyRecord {
                fingerprint: record.fingerprint,
                state,
            }));
        }
        kept.insert(
            Self::id(tenant, key),
            Kept {
                fingerprint: *fingerprint,
                claim: claim.to_owned(),
                answer: None,
                lease_until: now + lease,
                expires_at: now + ttl,
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
        let mut kept = self.0.lock().unwrap();
        match kept.get_mut(&Self::id(tenant, key)) {
            Some(record) if record.claim == claim && record.answer.is_none() => {
                record.answer = Some((status, body.to_vec()));
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
        let mut kept = self.0.lock().unwrap();
        let id = Self::id(tenant, key);
        match kept.get(&id) {
            Some(record) if record.claim == claim && record.answer.is_none() => {
                kept.remove(&id);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn purge_idempotency_keys(&self) -> StoreResult<u64> {
        let now = Instant::now();
        let mut kept = self.0.lock().unwrap();
        let before = kept.len();
        kept.retain(|_, record| record.expires_at > now);
        Ok((before - kept.len()) as u64)
    }
}

/// A row as the outbox below stores it, with the dedup key it holds and
/// until when.
struct Row {
    dedup_key: Option<String>,
    dedup_until: Option<OffsetDateTime>,
    /// The media id it records as received (roadmap M2e).
    media_id: Option<MediaId>,
    event: StoredEvent,
}

/// The rows of every stream (by tenant, `""` the operator-only one) and
/// each stream's last sequence.
#[derive(Default)]
struct Streams {
    rows: Vec<Row>,
    last: BTreeMap<String, i64>,
}

/// An outbox in memory, as far as the webhooks suite reaches it: inserts
/// once per dedup key (a keyless event's within its window), the routing
/// checked again under the records' lock, and pages of a tenant's stream.
pub struct Events {
    records: Arc<Records>,
    streams: Mutex<Streams>,
}

impl Events {
    fn of(records: Arc<Records>) -> Self {
        Self {
            records,
            streams: Mutex::new(Streams::default()),
        }
    }
}

/// Whether `guard` still names `tenant` in `records`, and its WABA's
/// binding is one the event may reach (`RouteGuard::admits`).
fn holds(records: &State, tenant: &TenantId, guard: &RouteGuard) -> bool {
    let waba = guard.binding.waba_id();
    let number_holds = match &guard.binding {
        GuardedBinding::Number {
            phone_number_id,
            waba_id,
        } => records
            .numbers
            .get(phone_number_id.as_str())
            .is_some_and(|n| &n.tenant_id == tenant && &n.waba_id == waba_id),
        GuardedBinding::Waba(_) => true,
    };
    number_holds
        && records.wabas.get(waba.as_str()).is_some_and(|w| {
            &w.tenant_id == tenant && guard.admits(w.attached_at, w.previous_tenant_unbound_at)
        })
}

#[async_trait]
impl Outbox for Events {
    async fn insert(&self, event: &NewEvent) -> StoreResult<Option<i64>> {
        let records = self.records.state();
        let mut streams = self.streams.lock().unwrap();
        if let Some(key) = &event.dedup_key
            && let Some(held) = streams
                .rows
                .iter_mut()
                .find(|row| row.dedup_key.as_ref() == Some(key))
        {
            match event.dedup_window {
                Some(window) if held.dedup_until.is_some_and(|until| until <= window.now) => {
                    held.dedup_key = None;
                }
                _ => return Ok(None),
            }
        }
        let tenant = event.tenant.as_ref().filter(|tenant| {
            event
                .route_guard
                .as_ref()
                .is_some_and(|guard| holds(&records, tenant, guard))
        });
        let stream = tenant.map(|t| t.as_str().to_owned()).unwrap_or_default();
        let last = streams.last.entry(stream).or_default();
        *last += 1;
        let sequence = *last;
        streams.rows.push(Row {
            dedup_key: event.dedup_key.clone(),
            dedup_until: event.dedup_window.map(|window| window.until),
            media_id: event.received_media(),
            event: StoredEvent {
                sequence,
                id: event.id.clone(),
                tenant: tenant.cloned(),
                phone_number_id: event.phone_number_id.clone(),
                waba_id: event.waba_id.clone(),
                event_type: event.event_type.clone(),
                data: event.data.clone(),
                created_at: OffsetDateTime::now_utc(),
            },
        });
        Ok(Some(sequence))
    }

    async fn page(&self, query: &EventQuery) -> StoreResult<EventPage> {
        let streams = self.streams.lock().unwrap();
        let after = query.after.unwrap_or(0);
        let mut events: Vec<StoredEvent> = streams
            .rows
            .iter()
            .map(|row| &row.event)
            .filter(|event| event.tenant.as_ref() == Some(&query.tenant))
            .filter(|event| event.sequence > after)
            .filter(|event| {
                query
                    .types
                    .as_ref()
                    .is_none_or(|types| types.contains(&event.event_type))
            })
            .filter(|event| {
                query
                    .phone_number_id
                    .as_ref()
                    .is_none_or(|pn| event.phone_number_id.as_ref() == Some(pn))
            })
            .cloned()
            .collect();
        let more = events.len() > query.limit;
        events.truncate(query.limit);
        Ok(EventPage {
            events,
            more,
            purged_through: 0,
            high_water: streams
                .last
                .get(query.tenant.as_str())
                .copied()
                .unwrap_or_default(),
        })
    }

    async fn purge(&self, _: Duration) -> StoreResult<u64> {
        unimplemented!("not used by the suites this backend runs")
    }

    async fn received_media(&self, media_id: &MediaId) -> StoreResult<Vec<ReceivedMedia>> {
        let streams = self.streams.lock().unwrap();
        let mut found: Vec<ReceivedMedia> = streams
            .rows
            .iter()
            .filter(|row| row.media_id.as_ref() == Some(media_id))
            .map(|row| ReceivedMedia {
                tenant: row.event.tenant.clone(),
                phone_number_id: row.event.phone_number_id.clone(),
                received_at: row.event.created_at,
            })
            .collect();
        found.sort_by_key(|row| std::cmp::Reverse(row.received_at));
        found.truncate(RECEIVED_MEDIA_ROWS);
        Ok(found)
    }
}

/// A backend of the records, idempotency records and outbox above, a
/// memory key/value store (the vault's, the dedup leases') and the
/// library's memory conversation store (the inbox).
pub struct Memory {
    pub records: Arc<Records>,
    pub idempotency: Arc<Idempotency>,
    pub kv: Arc<support::Kv>,
    pub outbox: Arc<Events>,
    pub conversations: Arc<MemoryConversationStore>,
    pub notifier: Arc<LocalNotifier>,
}

impl Default for Memory {
    fn default() -> Self {
        let records = Arc::new(Records::default());
        Self {
            outbox: Arc::new(Events::of(records.clone())),
            records,
            idempotency: Arc::default(),
            kv: Arc::default(),
            conversations: Arc::new(MemoryConversationStore::new()),
            notifier: Arc::default(),
        }
    }
}

#[async_trait]
impl Backend for Memory {
    fn kind(&self) -> BackendKind {
        BackendKind::Other("core-tests-memory")
    }
    fn records(&self) -> Arc<dyn RecordStore> {
        self.records.clone()
    }
    fn idempotency(&self) -> Arc<dyn IdempotencyRecords> {
        self.idempotency.clone()
    }
    fn outbox(&self) -> Arc<dyn Outbox> {
        self.outbox.clone()
    }
    fn notifier(&self) -> Arc<dyn EventNotifier> {
        self.notifier.clone()
    }
    fn leader_lock(&self) -> Arc<dyn LeaderLock> {
        unimplemented!("not used by the suites this backend runs")
    }
    fn janitor(&self) -> Arc<dyn Janitor> {
        unimplemented!("not used by the suites this backend runs")
    }
    fn migrator(&self) -> Arc<dyn SchemaMigrator> {
        unimplemented!("not used by the suites this backend runs")
    }
    fn kv(&self) -> Arc<dyn KvStore> {
        self.kv.clone()
    }
    fn conversations(&self) -> Arc<dyn ConversationStore> {
        self.conversations.clone()
    }
    async fn close(&self) {}
}
