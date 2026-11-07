//! Webhooks out in memory (roadmap M2c): [`MemoryWebhookStore`], the
//! core's [`WebhookStore`] for development and tests. It keeps the port's
//! rules as the Postgres one does: an endpoint is its tenant's and goes
//! with it (deleted under the records' lock, as Postgres cascades), a
//! fan-out moves its endpoint's cursor only from the value it read, and a
//! claim takes a delivery for one claimer until its lease ends.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use meta_whatsapp_server_core::model::{Listing, PageRequest, TenantId, TenantStatus};
use meta_whatsapp_server_core::store::{StoreResult, listing};
use meta_whatsapp_server_core::webhooks_out::store::{
    Attempt, ClaimRequest, ClaimToken, Claimed, CreateOutcome, EndpointChange, NewDelivery,
    NewEndpoint, Outcome, PendingDeliveries, WebhookStore,
};
use meta_whatsapp_server_core::webhooks_out::{
    Delivery, DeliveryId, DeliveryStatus, DisabledReason, Endpoint, EndpointId, SealedSecret,
};
use time::OffsetDateTime;

use crate::memory::{State, lock_state};

/// A delivery and its claim, in memory.
#[derive(Clone)]
struct Row {
    delivery: Delivery,
    lease_until: Option<OffsetDateTime>,
    claim: Option<ClaimToken>,
}

#[derive(Default)]
struct WebhookState {
    endpoints: BTreeMap<String, Endpoint>,
    deliveries: BTreeMap<String, Row>,
}

/// The webhooks-out records in memory, always a [`crate::MemoryStore`]'s
/// ([`crate::MemoryStore::webhooks`]): it reads that store's tenants (an
/// endpoint of a missing tenant is refused, a suspended tenant's
/// deliveries are not claimed), and deleting a tenant there deletes its
/// endpoints and deliveries here.
pub struct MemoryWebhookStore {
    /// The records of the store it belongs to: locked before
    /// [`Self::state`].
    records: Arc<Mutex<State>>,
    state: Mutex<WebhookState>,
}

impl std::fmt::Debug for MemoryWebhookStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        f.debug_struct("MemoryWebhookStore")
            .field("endpoints", &state.endpoints.len())
            .field("deliveries", &state.deliveries.len())
            .finish()
    }
}

impl MemoryWebhookStore {
    /// The store over `records` (its [`crate::MemoryStore`]'s).
    pub(crate) fn of(records: Arc<Mutex<State>>) -> Self {
        Self {
            records,
            state: Mutex::new(WebhookState::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, WebhookState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Delete `tenant`'s endpoints and deliveries: called by its store's
    /// `delete_tenant`, under the records' lock.
    pub(crate) fn forget_tenant(&self, tenant: &TenantId) {
        let mut state = self.lock();
        state.endpoints.retain(|_, e| &e.tenant != tenant);
        state.deliveries.retain(|_, r| &r.delivery.tenant != tenant);
    }

    fn owned<'a>(
        state: &'a WebhookState,
        tenant: &TenantId,
        id: &EndpointId,
    ) -> Option<&'a Endpoint> {
        state
            .endpoints
            .get(id.as_str())
            .filter(|e| &e.tenant == tenant)
    }
}

fn new_row(endpoint: &Endpoint, delivery: &NewDelivery) -> Row {
    Row {
        delivery: Delivery {
            id: delivery.id.clone(),
            endpoint: endpoint.id.clone(),
            tenant: endpoint.tenant.clone(),
            event_id: delivery.event_id.clone(),
            sequence: delivery.sequence,
            event_type: delivery.event_type.clone(),
            status: DeliveryStatus::Pending,
            attempts: 0,
            next_attempt_at: Some(delivery.created_at),
            give_up_at: delivery.give_up_at,
            last_attempt_at: None,
            last_status: None,
            last_error: None,
            last_duration_ms: None,
            created_at: delivery.created_at,
        },
        lease_until: None,
        claim: None,
    }
}

fn lease_end(now: OffsetDateTime, lease: std::time::Duration) -> OffsetDateTime {
    now.saturating_add(time::Duration::try_from(lease).unwrap_or(time::Duration::MAX))
}

#[async_trait]
impl WebhookStore for MemoryWebhookStore {
    async fn create_endpoint(
        &self,
        endpoint: &NewEndpoint,
        max: usize,
    ) -> StoreResult<CreateOutcome> {
        let records = lock_state(&self.records);
        if !records.tenants.contains_key(endpoint.tenant.as_str()) {
            return Ok(CreateOutcome::NoSuchTenant);
        }
        let mut state = self.lock();
        let held = state
            .endpoints
            .values()
            .filter(|e| e.tenant == endpoint.tenant)
            .count();
        if held >= max {
            return Ok(CreateOutcome::LimitReached);
        }
        let made = Endpoint {
            id: endpoint.id.clone(),
            tenant: endpoint.tenant.clone(),
            url: endpoint.url.clone(),
            types: endpoint.types.clone(),
            enabled: true,
            disabled_reason: None,
            secret: endpoint.secret.clone(),
            previous_secret: None,
            previous_secret_until: None,
            cursor: endpoint.cursor,
            failing_since: None,
            created_at: endpoint.now,
            updated_at: endpoint.now,
        };
        state
            .endpoints
            .insert(endpoint.id.as_str().to_owned(), made.clone());
        Ok(CreateOutcome::Created(Box::new(made)))
    }

    async fn endpoint(&self, tenant: &TenantId, id: &EndpointId) -> StoreResult<Option<Endpoint>> {
        Ok(Self::owned(&self.lock(), tenant, id).cloned())
    }

    async fn endpoints(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<Endpoint>> {
        let state = self.lock();
        let items: Vec<Endpoint> = state
            .endpoints
            .iter()
            .filter(|(id, e)| {
                &e.tenant == tenant
                    && page
                        .after
                        .as_deref()
                        .is_none_or(|after| id.as_str() > after)
            })
            .take(page.limit + 1)
            .map(|(_, e)| e.clone())
            .collect();
        Ok(listing(items, page.limit, |e| e.id.as_str().to_owned()))
    }

    async fn update_endpoint(
        &self,
        tenant: &TenantId,
        id: &EndpointId,
        change: &EndpointChange,
        now: OffsetDateTime,
    ) -> StoreResult<Option<Endpoint>> {
        let mut state = self.lock();
        let Some(endpoint) = state
            .endpoints
            .get_mut(id.as_str())
            .filter(|e| &e.tenant == tenant)
        else {
            return Ok(None);
        };
        if let Some(url) = &change.url {
            endpoint.url.clone_from(url);
        }
        if let Some(types) = &change.types {
            endpoint.types.clone_from(types);
        }
        if let Some(enabled) = change.enabled {
            endpoint.enabled = enabled;
            if enabled {
                endpoint.disabled_reason = None;
                endpoint.failing_since = None;
            }
        }
        endpoint.updated_at = now;
        Ok(Some(endpoint.clone()))
    }

    async fn rotate_secret(
        &self,
        tenant: &TenantId,
        id: &EndpointId,
        secret: &SealedSecret,
        previous_until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> StoreResult<Option<Endpoint>> {
        let mut state = self.lock();
        let Some(endpoint) = state
            .endpoints
            .get_mut(id.as_str())
            .filter(|e| &e.tenant == tenant)
        else {
            return Ok(None);
        };
        endpoint.previous_secret = Some(std::mem::replace(&mut endpoint.secret, secret.clone()));
        endpoint.previous_secret_until = Some(previous_until);
        endpoint.updated_at = now;
        Ok(Some(endpoint.clone()))
    }

    async fn delete_endpoint(&self, tenant: &TenantId, id: &EndpointId) -> StoreResult<bool> {
        let mut state = self.lock();
        if Self::owned(&state, tenant, id).is_none() {
            return Ok(false);
        }
        state.endpoints.remove(id.as_str());
        state.deliveries.retain(|_, r| &r.delivery.endpoint != id);
        Ok(true)
    }

    async fn active_endpoints(&self) -> StoreResult<Vec<Endpoint>> {
        Ok(self
            .lock()
            .endpoints
            .values()
            .filter(|e| e.enabled)
            .cloned()
            .collect())
    }

    async fn fan_out(
        &self,
        endpoint: &EndpointId,
        from: i64,
        to: i64,
        deliveries: &[NewDelivery],
    ) -> StoreResult<bool> {
        let mut state = self.lock();
        let Some(found) = state
            .endpoints
            .get(endpoint.as_str())
            .filter(|e| e.cursor == from)
            .cloned()
        else {
            return Ok(false);
        };
        for delivery in deliveries {
            let duplicate = delivery.event_id.is_some()
                && state.deliveries.values().any(|r| {
                    &r.delivery.endpoint == endpoint && r.delivery.event_id == delivery.event_id
                });
            if !duplicate {
                state
                    .deliveries
                    .insert(delivery.id.as_str().to_owned(), new_row(&found, delivery));
            }
        }
        if let Some(e) = state.endpoints.get_mut(endpoint.as_str()) {
            e.cursor = to;
        }
        Ok(true)
    }

    async fn insert_delivery(
        &self,
        tenant: &TenantId,
        endpoint: &EndpointId,
        delivery: &NewDelivery,
    ) -> StoreResult<Option<Delivery>> {
        let mut state = self.lock();
        let Some(found) = Self::owned(&state, tenant, endpoint).cloned() else {
            return Ok(None);
        };
        let row = new_row(&found, delivery);
        let made = row.delivery.clone();
        state
            .deliveries
            .insert(delivery.id.as_str().to_owned(), row);
        Ok(Some(made))
    }

    async fn claim(&self, request: &ClaimRequest) -> StoreResult<Vec<Claimed>> {
        let records = lock_state(&self.records);
        let mut state = self.lock();
        let now = request.now;
        let active = |tenant: &TenantId| {
            records
                .tenants
                .get(tenant.as_str())
                .is_some_and(|t| t.status == TenantStatus::Active)
        };
        // Each tenant's oldest due delivery of an endpoint it may take.
        let mut picked: BTreeMap<String, (OffsetDateTime, String)> = BTreeMap::new();
        for (id, row) in &state.deliveries {
            let d = &row.delivery;
            let Some(due) = d.next_attempt_at else {
                continue;
            };
            let Some(endpoint) = state.endpoints.get(d.endpoint.as_str()) else {
                continue;
            };
            let eligible = d.status == DeliveryStatus::Pending
                && due <= now
                && row.lease_until.is_none_or(|until| until <= now)
                && endpoint.enabled
                && active(&d.tenant)
                && !request.busy_endpoints.contains(&d.endpoint)
                && !request.busy_tenants.contains(&d.tenant);
            if !eligible {
                continue;
            }
            let key = d.tenant.as_str().to_owned();
            let candidate = (due, id.clone());
            if picked.get(&key).is_none_or(|best| candidate < *best) {
                picked.insert(key, candidate);
            }
        }
        let mut chosen: Vec<(OffsetDateTime, String)> = picked.into_values().collect();
        chosen.sort();
        chosen.truncate(request.limit);
        let until = lease_end(now, request.lease);
        let mut claimed = Vec::with_capacity(chosen.len());
        for (_, id) in chosen {
            let Some(row) = state.deliveries.get_mut(&id) else {
                continue;
            };
            row.lease_until = Some(until);
            row.claim = Some(request.token.clone());
            let delivery = row.delivery.clone();
            if let Some(endpoint) = state.endpoints.get(delivery.endpoint.as_str()) {
                claimed.push(Claimed {
                    endpoint: endpoint.clone(),
                    delivery,
                    claim: request.token.clone(),
                });
            }
        }
        Ok(claimed)
    }

    async fn settle(&self, attempt: &Attempt, now: OffsetDateTime) -> StoreResult<bool> {
        let mut state = self.lock();
        let Some(row) = state.deliveries.get_mut(attempt.delivery.as_str()) else {
            return Ok(false);
        };
        if row.claim.as_ref() != Some(&attempt.claim)
            || row.delivery.status != DeliveryStatus::Pending
        {
            return Ok(false);
        }
        row.claim = None;
        row.lease_until = None;
        let d = &mut row.delivery;
        if attempt.counted {
            d.attempts = d.attempts.saturating_add(1);
            d.last_attempt_at = Some(attempt.started_at);
            d.last_duration_ms = Some(attempt.duration_ms);
            d.last_status = attempt.status;
        }
        d.last_error.clone_from(&attempt.error);
        match attempt.outcome {
            Outcome::Delivered => {
                d.status = DeliveryStatus::Delivered;
                d.next_attempt_at = None;
            }
            Outcome::Retry(at) => d.next_attempt_at = Some(at),
            Outcome::Failed => {
                d.status = DeliveryStatus::Failed;
                d.next_attempt_at = None;
            }
        }
        let endpoint_id = d.endpoint.clone();
        if !attempt.counted {
            return Ok(true);
        }
        if let Some(endpoint) = state.endpoints.get_mut(endpoint_id.as_str()) {
            if attempt.outcome == Outcome::Delivered {
                endpoint.failing_since = None;
            } else {
                let since = *endpoint.failing_since.get_or_insert(attempt.started_at);
                if let Some(cutoff) = attempt.disable_if_failing_since
                    && since <= cutoff
                    && endpoint.enabled
                {
                    endpoint.enabled = false;
                    endpoint.disabled_reason = Some(DisabledReason::Failing);
                    endpoint.updated_at = now;
                }
            }
        }
        Ok(true)
    }

    async fn deliveries(
        &self,
        tenant: &TenantId,
        endpoint: &EndpointId,
        page: &PageRequest,
    ) -> StoreResult<Option<Listing<Delivery>>> {
        let state = self.lock();
        if Self::owned(&state, tenant, endpoint).is_none() {
            return Ok(None);
        }
        let items: Vec<Delivery> = state
            .deliveries
            .iter()
            .rev()
            .filter(|(id, r)| {
                &r.delivery.endpoint == endpoint
                    && page
                        .after
                        .as_deref()
                        .is_none_or(|after| id.as_str() < after)
            })
            .take(page.limit + 1)
            .map(|(_, r)| r.delivery.clone())
            .collect();
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
        let mut state = self.lock();
        if Self::owned(&state, tenant, endpoint).is_none() {
            return Ok(None);
        }
        let Some(row) = state
            .deliveries
            .get_mut(id.as_str())
            .filter(|r| &r.delivery.endpoint == endpoint)
        else {
            return Ok(None);
        };
        if row.delivery.status != DeliveryStatus::Pending {
            row.delivery.status = DeliveryStatus::Pending;
            row.delivery.next_attempt_at = Some(now);
            row.delivery.give_up_at = give_up_at;
            row.lease_until = None;
            row.claim = None;
        }
        Ok(Some(row.delivery.clone()))
    }

    async fn purge_deliveries(&self, before: OffsetDateTime) -> StoreResult<u64> {
        let mut state = self.lock();
        let held = state.deliveries.len();
        state.deliveries.retain(|_, r| {
            r.delivery.status == DeliveryStatus::Pending
                || r.delivery.last_attempt_at.unwrap_or(r.delivery.created_at) >= before
        });
        Ok(u64::try_from(held - state.deliveries.len()).unwrap_or(u64::MAX))
    }

    async fn pending(&self, now: OffsetDateTime) -> StoreResult<PendingDeliveries> {
        let state = self.lock();
        let mut count = 0_u64;
        let mut oldest_due: Option<OffsetDateTime> = None;
        for row in state.deliveries.values() {
            let d = &row.delivery;
            if d.status != DeliveryStatus::Pending {
                continue;
            }
            count += 1;
            if let Some(due) = d.next_attempt_at.filter(|due| *due <= now) {
                oldest_due = Some(oldest_due.map_or(due, |best| best.min(due)));
            }
        }
        Ok(PendingDeliveries { count, oldest_due })
    }
}
