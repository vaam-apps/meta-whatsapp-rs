//! The webhook endpoint routes as services (design §4.2, scope
//! `webhooks`; roadmap M2c), opened for an admitted caller only
//! ([`WebhookEndpoints::open`]), on the caller's tenant: another tenant's
//! endpoint or delivery is `404 not_found`, the answer for a missing one,
//! before anything of it is read or written.
//!
//! | Service | Route |
//! | --- | --- |
//! | [`OpenEndpoints::create`] | `POST /v1/webhook-endpoints`: the URL checked against the allow-list (its host looked up, every address checked), the types `KnownEventType`s; a secret made, shown once, sealed; the cursor at the tenant's newest event |
//! | [`OpenEndpoints::list`], [`OpenEndpoints::get`] | `GET /v1/webhook-endpoints[/{id}]` |
//! | [`OpenEndpoints::update`] | `PATCH /v1/webhook-endpoints/{id}`: URL (checked again), types, enabled |
//! | [`OpenEndpoints::delete`] | `DELETE /v1/webhook-endpoints/{id}`, its deliveries with it |
//! | [`OpenEndpoints::rotate_secret`] | `POST …/{id}/rotate-secret`: a new secret, shown once; the old one signs too for 24 h |
//! | [`OpenEndpoints::test`] | `POST …/{id}/test`: a `ping` delivery, sent by the dispatcher once |
//! | [`OpenEndpoints::deliveries`] | `GET …/{id}/deliveries`: attempts, newest first, no bodies |
//! | [`OpenEndpoints::retry`] | `POST …/{id}/deliveries/{delivery_id}/retry`: pending again, due now, a new window |

use std::sync::Arc;

use meta_whatsapp_rs::core::clock::Clock;

use super::destination::pin;
use super::dispatch::WebhooksOut;
use super::signature::EndpointSecret;
use super::store::{CreateOutcome, EndpointChange, NewDelivery, NewEndpoint, WebhookStore};
use super::{
    Delivery, DeliveryId, Endpoint, EndpointId, MAX_ENDPOINTS_PER_TENANT, PING, SECRET_OVERLAP,
};
use crate::authz::{Authorizer, Caller};
use crate::backend::Backend;
use crate::error::{ErrorCode, ServiceError};
use crate::events::TENANT_EVENT_TYPES;
use crate::model::{Listing, PageRequest, TenantId};
use crate::outbox::{EventQuery, Outbox};

/// The most types an endpoint lists: every type a tenant receives, each
/// once.
pub const MAX_TYPES: usize = TENANT_EVENT_TYPES.len();

/// The webhook endpoint services, on a backend's webhook store and
/// outbox, for the deployment's webhooks out (`None`: every route is
/// `409 webhooks_not_configured`).
pub struct WebhookEndpoints {
    store: Arc<dyn WebhookStore>,
    outbox: Arc<dyn Outbox>,
    clock: Arc<dyn Clock>,
    out: Option<WebhooksOut>,
}

impl std::fmt::Debug for WebhookEndpoints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookEndpoints")
            .field("out", &self.out)
            .finish_non_exhaustive()
    }
}

/// The webhooks-out backlog ([`WebhookEndpoints::backlog`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backlog {
    /// The deliveries pending.
    pub pending: u64,
    /// Whole seconds since the oldest pending delivery that is due fell
    /// due; `0` when none is due.
    pub oldest_due_age_seconds: u64,
}

/// The services opened for one admitted caller, on its tenant.
#[derive(Debug)]
pub struct OpenEndpoints<'a> {
    endpoints: &'a WebhookEndpoints,
    out: &'a WebhooksOut,
    tenant: &'a TenantId,
}

/// What a creation asks: the URL and the types. `Debug` omits the URL.
#[derive(Clone, PartialEq, Eq)]
pub struct CreateEndpoint {
    /// Where to deliver.
    pub url: String,
    /// The `KnownEventType`s to deliver.
    pub types: Vec<String>,
}

impl std::fmt::Debug for CreateEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateEndpoint")
            .field("types", &self.types)
            .finish_non_exhaustive()
    }
}

/// An endpoint id from a path: one of another shape names nothing.
fn endpoint_id(id: &str) -> Result<EndpointId, ServiceError> {
    EndpointId::parse(id).ok_or_else(ServiceError::not_found)
}

/// `types`, checked: 1 to [`MAX_TYPES`] types a tenant receives, each
/// once (in the order given), else `422 invalid_request` on `types`.
pub fn checked_types(types: &[String]) -> Result<Vec<String>, ServiceError> {
    let mut checked: Vec<String> = Vec::new();
    for kind in types {
        if !TENANT_EVENT_TYPES.contains(&kind.as_str()) {
            return Err(ServiceError::invalid("types"));
        }
        if !checked.contains(kind) {
            checked.push(kind.clone());
        }
    }
    if checked.is_empty() || checked.len() > MAX_TYPES {
        return Err(ServiceError::invalid("types"));
    }
    Ok(checked)
}

impl WebhookEndpoints {
    /// The services on `backend`'s webhook store and outbox, reading "now"
    /// from `clock` (the service's), for `out`.
    pub fn new(backend: &dyn Backend, clock: Arc<dyn Clock>, out: Option<WebhooksOut>) -> Self {
        Self {
            store: backend.webhooks(),
            outbox: backend.outbox(),
            clock,
            out,
        }
    }

    /// The backlog of deliveries, every tenant's, for the
    /// `wa_server_webhooks_out_*` metrics (roadmap M2k); `None` without a
    /// deployment (no dispatcher runs, so nothing waits). Counts only: it
    /// names no endpoint, URL or tenant, so it needs no admitted caller.
    ///
    /// # Errors
    ///
    /// The store's.
    pub async fn backlog(&self) -> crate::store::StoreResult<Option<Backlog>> {
        if self.out.is_none() {
            return Ok(None);
        }
        let now = self.clock.now();
        let pending = self.store.pending(now).await?;
        let age = pending.oldest_due.map_or(0, |due| {
            u64::try_from((now - due).whole_seconds()).unwrap_or(0)
        });
        Ok(Some(Backlog {
            pending: pending.count,
            oldest_due_age_seconds: age,
        }))
    }

    /// The deployment's webhooks out, if set up.
    pub fn deployment(&self) -> Option<&WebhooksOut> {
        self.out.as_ref()
    }

    /// The services for `caller`, once `authz` admits it (a caller another
    /// authorizer made opens nothing), on its tenant.
    ///
    /// # Errors
    ///
    /// `403` for a caller of another authorizer; `409
    /// webhooks_not_configured` without a deployment.
    pub fn open<'a>(
        &'a self,
        authz: &Authorizer,
        caller: &'a Caller,
    ) -> Result<OpenEndpoints<'a>, ServiceError> {
        authz.admit(caller)?;
        let out = self
            .out
            .as_ref()
            .ok_or_else(|| ServiceError::new(ErrorCode::WEBHOOKS_NOT_CONFIGURED))?;
        Ok(OpenEndpoints {
            endpoints: self,
            out,
            tenant: caller.tenant(),
        })
    }
}

impl OpenEndpoints<'_> {
    fn store(&self) -> &dyn WebhookStore {
        self.endpoints.store.as_ref()
    }

    /// `url`, admitted: by its text, then its host looked up and every
    /// address checked (the attempts check again, each on its own
    /// lookup). `422 invalid_request` on `url` otherwise.
    async fn checked_url(&self, url: &str) -> Result<String, ServiceError> {
        pin(&self.out.policy, self.out.resolver.as_ref(), url)
            .await
            .map_err(|_| ServiceError::invalid("url"))?;
        Ok(url.to_owned())
    }

    /// Create an endpoint; it and its secret, which no later answer shows.
    ///
    /// # Errors
    ///
    /// `422` on `url` or `types`; `409 webhook_endpoint_limit_reached`;
    /// `503` from the store.
    pub async fn create(
        &self,
        request: CreateEndpoint,
    ) -> Result<(Endpoint, EndpointSecret), ServiceError> {
        let types = checked_types(&request.types)?;
        let url = self.checked_url(&request.url).await?;
        let id = EndpointId::generate().ok_or_else(ServiceError::internal)?;
        let secret = EndpointSecret::generate().ok_or_else(ServiceError::internal)?;
        let sealed = self
            .out
            .keys
            .seal(&id, &secret)
            .ok_or_else(ServiceError::internal)?;
        // From the tenant's newest event on: what comes after it.
        let cursor = self
            .endpoints
            .outbox
            .page(&EventQuery {
                tenant: self.tenant.clone(),
                after: Some(i64::MAX),
                types: None,
                phone_number_id: None,
                limit: 1,
                max_bytes: 1,
            })
            .await?
            .high_water;
        let created = self
            .store()
            .create_endpoint(
                &NewEndpoint {
                    id,
                    tenant: self.tenant.clone(),
                    url,
                    types,
                    secret: sealed,
                    cursor,
                    now: self.endpoints.clock.now(),
                },
                MAX_ENDPOINTS_PER_TENANT,
            )
            .await?;
        match created {
            CreateOutcome::Created(endpoint) => {
                tracing::info!(target: "audit", tenant = %self.tenant, endpoint = %endpoint.id, "webhook_endpoint_created");
                Ok((*endpoint, secret))
            }
            CreateOutcome::LimitReached => {
                Err(ServiceError::new(ErrorCode::WEBHOOK_ENDPOINT_LIMIT_REACHED))
            }
            CreateOutcome::NoSuchTenant => Err(ServiceError::not_found()),
        }
    }

    /// The tenant's endpoints.
    ///
    /// # Errors
    ///
    /// `503` from the store.
    pub async fn list(&self, page: &PageRequest) -> Result<Listing<Endpoint>, ServiceError> {
        Ok(self.store().endpoints(self.tenant, page).await?)
    }

    /// One of the tenant's endpoints.
    ///
    /// # Errors
    ///
    /// `404` for another tenant's, a missing one or an id of no endpoint's
    /// shape.
    pub async fn get(&self, id: &str) -> Result<Endpoint, ServiceError> {
        let id = endpoint_id(id)?;
        self.store()
            .endpoint(self.tenant, &id)
            .await?
            .ok_or_else(ServiceError::not_found)
    }

    /// Change one of the tenant's endpoints: its URL (checked again), its
    /// types, whether it is enabled.
    ///
    /// # Errors
    ///
    /// `404` as [`Self::get`], before the change is checked; `422` on
    /// `url` or `types`.
    pub async fn update(&self, id: &str, change: EndpointChange) -> Result<Endpoint, ServiceError> {
        let id = endpoint_id(id)?;
        if self.store().endpoint(self.tenant, &id).await?.is_none() {
            return Err(ServiceError::not_found());
        }
        let types = change.types.as_deref().map(checked_types).transpose()?;
        let url = match change.url.as_deref() {
            Some(url) => Some(self.checked_url(url).await?),
            None => None,
        };
        let change = EndpointChange {
            url,
            types,
            enabled: change.enabled,
        };
        let updated = self
            .store()
            .update_endpoint(self.tenant, &id, &change, self.endpoints.clock.now())
            .await?
            .ok_or_else(ServiceError::not_found)?;
        tracing::info!(target: "audit", tenant = %self.tenant, endpoint = %updated.id, "webhook_endpoint_updated");
        Ok(updated)
    }

    /// Delete one of the tenant's endpoints, and its deliveries.
    ///
    /// # Errors
    ///
    /// `404` as [`Self::get`].
    pub async fn delete(&self, id: &str) -> Result<(), ServiceError> {
        let id = endpoint_id(id)?;
        if !self.store().delete_endpoint(self.tenant, &id).await? {
            return Err(ServiceError::not_found());
        }
        tracing::info!(target: "audit", tenant = %self.tenant, endpoint = %id, "webhook_endpoint_deleted");
        Ok(())
    }

    /// A new secret for one of the tenant's endpoints, shown once; the
    /// current one signs too for [`SECRET_OVERLAP`].
    ///
    /// # Errors
    ///
    /// `404` as [`Self::get`].
    pub async fn rotate_secret(
        &self,
        id: &str,
    ) -> Result<(Endpoint, EndpointSecret), ServiceError> {
        let id = endpoint_id(id)?;
        if self.store().endpoint(self.tenant, &id).await?.is_none() {
            return Err(ServiceError::not_found());
        }
        let secret = EndpointSecret::generate().ok_or_else(ServiceError::internal)?;
        let sealed = self
            .out
            .keys
            .seal(&id, &secret)
            .ok_or_else(ServiceError::internal)?;
        let now = self.endpoints.clock.now();
        let until = now
            .checked_add(time::Duration::try_from(SECRET_OVERLAP).unwrap_or(time::Duration::DAY))
            .unwrap_or(now);
        let rotated = self
            .store()
            .rotate_secret(self.tenant, &id, &sealed, until, now)
            .await?
            .ok_or_else(ServiceError::not_found)?;
        tracing::info!(target: "audit", tenant = %self.tenant, endpoint = %id, "webhook_secret_rotated");
        Ok((rotated, secret))
    }

    /// A ping to one of the tenant's endpoints: a delivery due now, which
    /// the dispatcher attempts once.
    ///
    /// # Errors
    ///
    /// `404` as [`Self::get`].
    pub async fn test(&self, id: &str) -> Result<Delivery, ServiceError> {
        let id = endpoint_id(id)?;
        let now = self.endpoints.clock.now();
        let delivery = NewDelivery {
            id: DeliveryId::generate(now).ok_or_else(ServiceError::internal)?,
            event_id: None,
            sequence: None,
            event_type: PING.to_owned(),
            created_at: now,
            give_up_at: now,
        };
        self.store()
            .insert_delivery(self.tenant, &id, &delivery)
            .await?
            .ok_or_else(ServiceError::not_found)
    }

    /// One of the tenant's endpoints' deliveries, newest first.
    ///
    /// # Errors
    ///
    /// `404` as [`Self::get`].
    pub async fn deliveries(
        &self,
        id: &str,
        page: &PageRequest,
    ) -> Result<Listing<Delivery>, ServiceError> {
        let id = endpoint_id(id)?;
        self.store()
            .deliveries(self.tenant, &id, page)
            .await?
            .ok_or_else(ServiceError::not_found)
    }

    /// Deliver one of the tenant's endpoints' deliveries again: pending,
    /// due now, its retries for a new window.
    ///
    /// # Errors
    ///
    /// `404` for another tenant's, another endpoint's, a missing one or an
    /// id of no delivery's shape.
    pub async fn retry(&self, id: &str, delivery_id: &str) -> Result<Delivery, ServiceError> {
        let id = endpoint_id(id)?;
        let delivery_id = DeliveryId::parse(delivery_id).ok_or_else(ServiceError::not_found)?;
        let now = self.endpoints.clock.now();
        let give_up_at = self.out.schedule.give_up_at(now);
        self.store()
            .retry_delivery(self.tenant, &id, &delivery_id, now, give_up_at)
            .await?
            .ok_or_else(ServiceError::not_found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Types are `KnownEventType`s, at least one, each once (all of them
    /// at most); anything a tenant
    /// never receives is refused on `types`.
    #[test]
    fn types_are_known_event_types() {
        let types = |t: &[&str]| t.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            checked_types(&types(&[
                "message_received",
                "status_updated",
                "message_received"
            ]))
            .unwrap(),
            types(&["message_received", "status_updated"])
        );
        for bad in [
            types(&[]),
            types(&["unknown"]),
            types(&["unparsed"]),
            types(&["partner_solution_updated"]),
            types(&["ping"]),
        ] {
            assert_eq!(
                checked_types(&bad).unwrap_err().field(),
                Some("types"),
                "{bad:?}"
            );
        }
    }
}
