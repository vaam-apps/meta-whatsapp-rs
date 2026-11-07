//! The dispatcher (design §2.4 and §4.4): on every replica, a loop that
//!
//! 1. **fans out** the outbox to each enabled endpoint: its tenant's
//!    events after its cursor, of its types, one delivery each, the cursor
//!    moved in the same step ([`WebhookStore::fan_out`]: once, whatever
//!    replicas race);
//! 2. **claims** due deliveries under a lease ([`WebhookStore::claim`]:
//!    one claimer each, `FOR UPDATE SKIP LOCKED` on Postgres; no leader),
//!    at most one per tenant a claim, none of an endpoint or a tenant at
//!    its bound of attempts in flight on this replica
//!    ([`DispatchSettings::per_endpoint`], [`DispatchSettings::per_tenant`],
//!    [`DispatchSettings::workers`]): a slow endpoint holds its own
//!    attempts, never the others';
//! 3. **attempts** each one: its event read from the outbox in its
//!    endpoint's tenant's stream, `data` left out past
//!    [`MAX_DELIVERED_DATA_BYTES`], the destination checked again on a
//!    fresh lookup and the checked address pinned ([`pin`]), signed by the
//!    endpoint's secrets (both during a rotation), `POST`ed through the
//!    [`Sender`] (no redirect, [`DELIVERY_TIMEOUT`],
//!    [`MAX_RESPONSE_BYTES`] of the answer), then settled: delivered on a
//!    2xx, else retried by the [`RetrySchedule`] until its window ends,
//!    then failed, an endpoint failing for the whole window disabled.
//!
//! It runs when woken by the backend's notifier (a tenant with an
//! endpoint gained an event), when an attempt ends, and every
//! [`DispatchSettings::poll`] (retries come due, another replica's
//! endpoints and pings). At least once: an attempt cut by a crash is
//! claimed again once its lease ends, and receivers deduplicate on
//! `webhook-id`, the event's id.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::core::clock::Clock;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::destination::{Destination, DestinationPolicy, Resolver, pin};
use super::schedule::{Jitter, RandomJitter, RetrySchedule};
use super::seal::DataKeys;
use super::signature::{
    EndpointSecret, WEBHOOK_ID, WEBHOOK_SIGNATURE, WEBHOOK_TIMESTAMP, signature_header,
};
use super::store::{
    Attempt, ClaimRequest, ClaimToken, Claimed, NewDelivery, Outcome, WebhookStore,
};
use super::{
    AttemptError, DELIVERY_TIMEOUT, Delivery, DeliveryId, Endpoint, EndpointId,
    MAX_DELIVERED_DATA_BYTES, MAX_RESPONSE_BYTES, PING,
};
use crate::backend::Backend;
use crate::model::TenantId;
use crate::notify::{EventNotifier, Subscription};
use crate::outbox::{HeaderQuery, Outbox, StoredEvent};

/// The envelope's version (the HTTP envelope's `api_version`).
pub const ENVELOPE_VERSION: &str = "v1";

/// The `User-Agent` of every attempt.
pub const USER_AGENT: &str = concat!("meta-whatsapp-server/", env!("CARGO_PKG_VERSION"));

/// What an attempt asks the [`Sender`]: `POST` the body, with the
/// headers, to the address the destination check pinned. `Debug` shows
/// neither the headers (the signatures) nor the body (a customer's
/// message).
#[derive(Clone)]
pub struct SendRequest {
    /// The URL, checked: its host for TLS (SNI and the certificate) and
    /// `Host`, its path and query for the request line.
    pub destination: Destination,
    /// Where to connect: this address and no other. A resolver of the
    /// sender's own would undo the check.
    pub address: SocketAddr,
    /// The headers, in order: `content-type`, `user-agent`, `webhook-id`,
    /// `webhook-timestamp`, `webhook-signature`.
    pub headers: Vec<(&'static str, String)>,
    /// The body, as signed.
    pub body: Vec<u8>,
    /// The whole attempt's time limit ([`DELIVERY_TIMEOUT`]).
    pub timeout: Duration,
    /// The most of the answer's body to read ([`MAX_RESPONSE_BYTES`]).
    pub max_response_bytes: usize,
}

impl fmt::Debug for SendRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendRequest")
            .field("destination", &self.destination)
            .field("address", &self.address)
            .field("body_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl SendRequest {
    /// The value of header `name`, if the request has it.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Why a send failed (nothing of the receiver's answer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// No answer within the time limit.
    #[error("timed out")]
    Timeout,
    /// The connection failed.
    #[error("the connection failed")]
    Connect,
    /// The TLS handshake failed.
    #[error("the TLS handshake failed")]
    Tls,
    /// The answer was not HTTP.
    #[error("the answer was not HTTP")]
    Protocol,
}

impl From<SendError> for AttemptError {
    fn from(error: SendError) -> Self {
        match error {
            SendError::Timeout => Self::Timeout,
            SendError::Connect => Self::Connect,
            SendError::Tls => Self::Tls,
            SendError::Protocol => Self::Protocol,
        }
    }
}

/// Sends an attempt: the HTTP crate's, over TCP and TLS
/// (`meta-whatsapp-server-http`); a recorder in tests. Every
/// implementation keeps four rules: it connects to
/// [`SendRequest::address`] only (never resolving the host itself), it
/// follows no redirect (a 3xx is the attempt's answer), it reads at most
/// [`SendRequest::max_response_bytes`] of the answer, and it gives up at
/// [`SendRequest::timeout`].
#[async_trait]
pub trait Sender: Send + Sync + 'static {
    /// Send `request`; the answer's status.
    ///
    /// # Errors
    ///
    /// [`SendError`].
    async fn send(&self, request: SendRequest) -> Result<u16, SendError>;
}

/// The dispatcher's bounds and timing: the service's defaults (the design
/// states none but the lease's existence), which tests shorten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchSettings {
    /// Attempts in flight at once on a replica.
    pub workers: usize,
    /// Attempts to one endpoint in flight at once on a replica.
    pub per_endpoint: usize,
    /// Attempts of one tenant in flight at once on a replica.
    pub per_tenant: usize,
    /// How long a claim holds a delivery: past it, another replica claims
    /// it again. Above [`DELIVERY_TIMEOUT`] and the lookup's limit.
    pub lease: Duration,
    /// How often the loop runs without a wake-up.
    pub poll: Duration,
    /// Events fanned out to one endpoint per round.
    pub fan_out_page: usize,
}

impl Default for DispatchSettings {
    fn default() -> Self {
        Self {
            workers: 32,
            per_endpoint: 4,
            per_tenant: 8,
            lease: Duration::from_secs(60),
            poll: Duration::from_secs(1),
            fan_out_page: 100,
        }
    }
}

/// The deployment's webhooks out: the data key, the allow-list, the
/// lookup and the sender, the schedule. Without one, every webhook
/// endpoint route is `409 webhooks_not_configured` and no dispatcher
/// runs. `Debug` shows no key.
#[derive(Clone)]
pub struct WebhooksOut {
    /// `WA_SERVER_DATA_KEY` (and the previous ones).
    pub keys: Arc<DataKeys>,
    /// `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS`, plain `http` in
    /// development only.
    pub policy: DestinationPolicy,
    /// The lookups, at creation and at every attempt.
    pub resolver: Arc<dyn Resolver>,
    /// The attempts.
    pub sender: Arc<dyn Sender>,
    /// `WA_SERVER_WEBHOOK_RETRY_WINDOW` and the design's waits.
    pub schedule: RetrySchedule,
    /// The jitter of the waits.
    pub jitter: Arc<dyn Jitter>,
    /// The dispatcher's bounds.
    pub dispatch: DispatchSettings,
}

impl fmt::Debug for WebhooksOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebhooksOut")
            .field("keys", &self.keys)
            .field("policy", &self.policy)
            .field("schedule", &self.schedule)
            .field("dispatch", &self.dispatch)
            .finish_non_exhaustive()
    }
}

impl WebhooksOut {
    /// `keys`, `policy`, `resolver` and `sender`, with the design's
    /// schedule, a random jitter and the default bounds.
    pub fn new(
        keys: DataKeys,
        policy: DestinationPolicy,
        resolver: Arc<dyn Resolver>,
        sender: Arc<dyn Sender>,
    ) -> Self {
        Self {
            keys: Arc::new(keys),
            policy,
            resolver,
            sender,
            schedule: RetrySchedule::default(),
            jitter: Arc::new(RandomJitter),
            dispatch: DispatchSettings::default(),
        }
    }
}

/// RFC 3339, UTC.
fn rfc3339(at: OffsetDateTime) -> String {
    at.to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// The body of an event's delivery: the envelope `GET /v1/events/{id}`
/// answers (the HTTP crate's `EventEnvelope`, field for field), `data`
/// left out (`null`, `truncated: true`) when it is over
/// [`MAX_DELIVERED_DATA_BYTES`] or not JSON.
pub fn event_body(tenant: &TenantId, event: &StoredEvent) -> Vec<u8> {
    #[derive(Serialize)]
    struct Envelope<'a> {
        id: &'a str,
        sequence: i64,
        #[serde(rename = "type")]
        event_type: &'a str,
        api_version: &'static str,
        tenant_id: &'a str,
        phone_number_id: Option<&'a str>,
        waba_id: Option<&'a str>,
        received_at: String,
        truncated: bool,
        data: Option<serde_json::Value>,
    }
    let data = (event.data.len() <= MAX_DELIVERED_DATA_BYTES)
        .then(|| serde_json::from_str(&event.data).ok())
        .flatten();
    serde_json::to_vec(&Envelope {
        id: &event.id,
        sequence: event.sequence,
        event_type: &event.event_type,
        api_version: ENVELOPE_VERSION,
        tenant_id: tenant.as_str(),
        phone_number_id: event
            .phone_number_id
            .as_ref()
            .map(meta_whatsapp_rs::core::ids::PhoneNumberId::as_str),
        waba_id: event
            .waba_id
            .as_ref()
            .map(meta_whatsapp_rs::core::ids::WabaId::as_str),
        received_at: rfc3339(event.created_at),
        truncated: data.is_none(),
        data,
    })
    .unwrap_or_default()
}

/// The body of a ping: its delivery's id, `type: ping`, the tenant, the
/// endpoint and when it was sent.
pub fn ping_body(delivery: &Delivery, now: OffsetDateTime) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "id": delivery.id.as_str(),
        "type": PING,
        "api_version": ENVELOPE_VERSION,
        "tenant_id": delivery.tenant.as_str(),
        "endpoint_id": delivery.endpoint.as_str(),
        "sent_at": rfc3339(now),
    }))
    .unwrap_or_default()
}

/// A random claim token; `None` without randomness.
fn claim_token() -> Option<ClaimToken> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(ClaimToken(hex::encode(bytes)))
}

/// What one attempt's task reports when it ends.
struct Ended {
    endpoint: EndpointId,
    tenant: TenantId,
}

/// The dispatcher of one replica, over a backend's webhook store, outbox
/// and notifier, on the service's clock.
#[derive(Clone)]
pub struct Dispatcher {
    store: Arc<dyn WebhookStore>,
    outbox: Arc<dyn Outbox>,
    notifier: Arc<dyn EventNotifier>,
    clock: Arc<dyn Clock>,
    out: WebhooksOut,
}

impl fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dispatcher")
            .field("out", &self.out)
            .finish_non_exhaustive()
    }
}

impl Dispatcher {
    /// The dispatcher on `backend`'s ports, reading "now" from `clock`.
    pub fn new(backend: &dyn Backend, clock: Arc<dyn Clock>, out: WebhooksOut) -> Self {
        Self {
            store: backend.webhooks(),
            outbox: backend.outbox(),
            notifier: backend.notifier(),
            clock,
            out,
        }
    }

    /// Run until `stop` completes; the attempts in flight then finish
    /// (each within its time limit) before it returns.
    pub async fn run(self, stop: impl Future<Output = ()>) {
        let mut stop = std::pin::pin!(stop);
        let mut tasks = tokio::task::JoinSet::new();
        let mut in_flight = InFlight::default();
        let mut subscriptions: HashMap<TenantId, Subscription> = HashMap::new();
        loop {
            match self.fan_out().await {
                Ok(tenants) => self.subscribe(&mut subscriptions, tenants).await,
                Err(error) => {
                    tracing::warn!(error = %error, "fanning events out to webhook endpoints failed");
                }
            }
            self.claim_into(&mut tasks, &mut in_flight).await;
            let woken = async {
                if subscriptions.is_empty() {
                    std::future::pending::<()>().await;
                } else {
                    futures::future::select_all(
                        subscriptions.values_mut().map(|s| Box::pin(s.woken())),
                    )
                    .await;
                }
            };
            tokio::select! {
                () = &mut stop => break,
                () = tokio::time::sleep(self.out.dispatch.poll) => {}
                Some(ended) = tasks.join_next(), if !tasks.is_empty() => {
                    if let Ok(ended) = ended {
                        in_flight.end(&ended);
                    }
                }
                () = woken => {}
            }
            while let Some(ended) = tasks.try_join_next() {
                if let Ok(ended) = ended {
                    in_flight.end(&ended);
                }
            }
        }
        while tasks.join_next().await.is_some() {}
    }

    /// Hold a subscription to each of `tenants` (those with an enabled
    /// endpoint), and none to another.
    async fn subscribe(
        &self,
        subscriptions: &mut HashMap<TenantId, Subscription>,
        tenants: Vec<TenantId>,
    ) {
        subscriptions.retain(|tenant, _| tenants.contains(tenant));
        for tenant in tenants {
            if subscriptions.contains_key(&tenant) {
                continue;
            }
            match self.notifier.subscribe(&tenant).await {
                Ok(subscription) => {
                    subscriptions.insert(tenant, subscription);
                }
                // The poll covers it.
                Err(error) => {
                    tracing::debug!(error = %error, "subscribing to a tenant's events failed");
                }
            }
        }
    }

    /// One fan-out round: each enabled endpoint's tenant's events after
    /// its cursor, of its types, up to [`DispatchSettings::fan_out_page`],
    /// recorded as deliveries, the cursor moved; read as headers, never their
    /// data (roadmap M2l). The tenants with an
    /// enabled endpoint.
    ///
    /// # Errors
    ///
    /// The library's `StorageError`, reading the endpoints. One
    /// endpoint's failure is logged and the others go on.
    pub async fn fan_out(&self) -> crate::store::StoreResult<Vec<TenantId>> {
        let endpoints = self.store.active_endpoints().await?;
        let mut tenants = Vec::new();
        for endpoint in endpoints {
            if !tenants.contains(&endpoint.tenant) {
                tenants.push(endpoint.tenant.clone());
            }
            if let Err(error) = self.fan_out_to(&endpoint).await {
                tracing::warn!(error = %error, endpoint = %endpoint.id, "fanning events out to a webhook endpoint failed");
            }
        }
        Ok(tenants)
    }

    async fn fan_out_to(&self, endpoint: &Endpoint) -> crate::store::StoreResult<()> {
        let page = self
            .outbox
            .headers_after(&HeaderQuery {
                tenant: endpoint.tenant.clone(),
                after: Some(endpoint.cursor),
                types: Some(endpoint.types.clone()),
                limit: self.out.dispatch.fan_out_page.max(1),
            })
            .await?;
        let to = match page.headers.last() {
            Some(last) if page.more => last.sequence,
            _ => page.high_water.max(endpoint.cursor),
        };
        if to == endpoint.cursor && page.headers.is_empty() {
            return Ok(());
        }
        let now = self.clock.now();
        let give_up_at = self.out.schedule.give_up_at(now);
        let mut deliveries = Vec::with_capacity(page.headers.len());
        for event in page.headers {
            // An outbox page is the tenant's own stream; checked again.
            if event.tenant.as_ref() != Some(&endpoint.tenant) {
                continue;
            }
            let Some(id) = DeliveryId::generate(now) else {
                return Err(meta_whatsapp_rs::core::error::StorageError::Backend(
                    anyhow::anyhow!("no randomness for a delivery id"),
                ));
            };
            deliveries.push(NewDelivery {
                id,
                event_id: Some(event.id),
                sequence: Some(event.sequence),
                event_type: event.event_type,
                created_at: now,
                give_up_at,
            });
        }
        // `false`: another replica moved the cursor first; its deliveries
        // stand.
        self.store
            .fan_out(&endpoint.id, endpoint.cursor, to, &deliveries)
            .await?;
        Ok(())
    }

    /// Claim what the bounds allow and start an attempt for each.
    async fn claim_into(&self, tasks: &mut tokio::task::JoinSet<Ended>, in_flight: &mut InFlight) {
        let settings = self.out.dispatch;
        // One delivery per tenant a claim: as many claims as a tenant may
        // hold in flight.
        for _ in 0..settings.per_tenant.max(1) {
            let free = settings.workers.saturating_sub(in_flight.total);
            if free == 0 {
                return;
            }
            let Some(request) = in_flight.request(self.clock.now(), &settings, free) else {
                return;
            };
            let claimed = match self.store.claim(&request).await {
                Ok(claimed) => claimed,
                Err(error) => {
                    tracing::warn!(error = %error, "claiming webhook deliveries failed");
                    return;
                }
            };
            if claimed.is_empty() {
                return;
            }
            for claimed in claimed {
                in_flight.start(&claimed.endpoint.id, &claimed.endpoint.tenant);
                let this = self.clone();
                tasks.spawn(async move {
                    let ended = Ended {
                        endpoint: claimed.endpoint.id.clone(),
                        tenant: claimed.endpoint.tenant.clone(),
                    };
                    this.attempt(claimed).await;
                    ended
                });
            }
        }
    }

    /// Claim what is due and attempt it, here, waiting for every attempt:
    /// one round of the loop's claims, for tests. How many were attempted.
    pub async fn dispatch_due(&self) -> usize {
        let mut tasks = tokio::task::JoinSet::new();
        let mut in_flight = InFlight::default();
        self.claim_into(&mut tasks, &mut in_flight).await;
        let mut n = 0;
        while tasks.join_next().await.is_some() {
            n += 1;
        }
        n
    }

    /// One attempt of `claimed`, settled.
    pub async fn attempt(&self, claimed: Claimed) {
        let Claimed {
            delivery,
            endpoint,
            claim,
        } = claimed;
        let started_at = self.clock.now();
        let started = tokio::time::Instant::now();
        let result = self.send(&delivery, &endpoint, started_at).await;
        let duration_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
        let now = self.clock.now();
        let (status, error, counted) = match result {
            Ok(status) if (200..300).contains(&status) => (Some(status), None, true),
            Ok(status) => (Some(status), Some(AttemptError::Status), true),
            Err(error) => {
                let counted = error != AttemptError::EventGone;
                (None, Some(error), counted)
            }
        };
        let outcome = match &error {
            None => Outcome::Delivered,
            Some(AttemptError::EventGone) => Outcome::Failed,
            // A ping is tried once: its answer is what the tenant asked.
            Some(_) if delivery.event_id.is_none() => Outcome::Failed,
            Some(_) => match self.out.schedule.next(
                delivery.attempts.saturating_add(1),
                now,
                delivery.give_up_at,
                self.out.jitter.unit(),
            ) {
                Some(at) => Outcome::Retry(at),
                None => Outcome::Failed,
            },
        };
        let disable_if_failing_since =
            (outcome == Outcome::Failed && counted && delivery.event_id.is_some())
                .then(|| {
                    time::Duration::try_from(self.out.schedule.window)
                        .ok()
                        .and_then(|window| now.checked_sub(window))
                })
                .flatten();
        let attempt = Attempt {
            delivery: delivery.id.clone(),
            claim,
            outcome,
            started_at,
            duration_ms,
            status,
            error: error.clone(),
            counted,
            disable_if_failing_since,
        };
        tracing::debug!(
            delivery = %delivery.id,
            endpoint = %endpoint.id,
            status,
            error = error.as_ref().map(AttemptError::as_str),
            "webhook delivery attempted"
        );
        match self.store.settle(&attempt, now).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::info!(delivery = %delivery.id, "a webhook delivery's claim ended before its attempt did");
            }
            Err(error) => {
                tracing::warn!(error = %error, delivery = %delivery.id, "recording a webhook delivery's attempt failed");
            }
        }
    }

    /// Build, check and send one attempt; the receiver's status.
    async fn send(
        &self,
        delivery: &Delivery,
        endpoint: &Endpoint,
        now: OffsetDateTime,
    ) -> Result<u16, AttemptError> {
        // A delivery carries its endpoint's tenant's event only.
        if delivery.tenant != endpoint.tenant || delivery.endpoint != endpoint.id {
            tracing::error!(delivery = %delivery.id, "a webhook delivery names another tenant than its endpoint");
            return Err(AttemptError::EventGone);
        }
        let (webhook_id, body) = match &delivery.event_id {
            Some(event_id) => match self.outbox.get(&endpoint.tenant, event_id).await {
                Ok(Some(event)) if event.tenant.as_ref() == Some(&endpoint.tenant) => {
                    let body = event_body(&endpoint.tenant, &event);
                    (event.id, body)
                }
                Ok(_) => return Err(AttemptError::EventGone),
                // The outbox did not answer: a failed attempt, retried.
                Err(_) => return Err(AttemptError::Other("storage".to_owned())),
            },
            None => (delivery.id.as_str().to_owned(), ping_body(delivery, now)),
        };
        let mut secrets: Vec<EndpointSecret> = Vec::new();
        for (i, sealed) in endpoint.signing_secrets(now).into_iter().enumerate() {
            match self.out.keys.open(&endpoint.id, sealed) {
                Some(secret) => secrets.push(secret),
                // Without the current secret nothing is signed.
                None if i == 0 => {
                    tracing::error!(endpoint = %endpoint.id, "a webhook endpoint's secret does not open under the data keys");
                    return Err(AttemptError::SecretUnavailable);
                }
                None => {}
            }
        }
        let pinned = pin(&self.out.policy, self.out.resolver.as_ref(), &endpoint.url)
            .await
            .map_err(AttemptError::from)?;
        let timestamp = self.clock.now().unix_timestamp();
        let signature = signature_header(&secrets, &webhook_id, timestamp, &body);
        let request = SendRequest {
            destination: pinned.destination,
            address: pinned.address,
            headers: vec![
                ("content-type", "application/json".to_owned()),
                ("user-agent", USER_AGENT.to_owned()),
                (WEBHOOK_ID, webhook_id),
                (WEBHOOK_TIMESTAMP, timestamp.to_string()),
                (WEBHOOK_SIGNATURE, signature),
            ],
            body,
            timeout: DELIVERY_TIMEOUT,
            max_response_bytes: MAX_RESPONSE_BYTES,
        };
        // The sender's own limit, and this one should it not keep it.
        match tokio::time::timeout(
            DELIVERY_TIMEOUT + Duration::from_secs(1),
            self.out.sender.send(request),
        )
        .await
        {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(error)) => Err(error.into()),
            Err(_) => Err(AttemptError::Timeout),
        }
    }
}

/// The attempts in flight on this replica, by endpoint and tenant.
#[derive(Default)]
struct InFlight {
    total: usize,
    endpoints: HashMap<EndpointId, usize>,
    tenants: HashMap<TenantId, usize>,
}

impl InFlight {
    fn start(&mut self, endpoint: &EndpointId, tenant: &TenantId) {
        self.total += 1;
        *self.endpoints.entry(endpoint.clone()).or_default() += 1;
        *self.tenants.entry(tenant.clone()).or_default() += 1;
    }

    fn end(&mut self, ended: &Ended) {
        self.total = self.total.saturating_sub(1);
        if let Some(n) = self.endpoints.get_mut(&ended.endpoint) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.endpoints.remove(&ended.endpoint);
            }
        }
        if let Some(n) = self.tenants.get_mut(&ended.tenant) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.tenants.remove(&ended.tenant);
            }
        }
    }

    /// The claim for `free` more, none of an endpoint or a tenant at its
    /// bound; `None` without randomness for its token.
    fn request(
        &self,
        now: OffsetDateTime,
        settings: &DispatchSettings,
        free: usize,
    ) -> Option<ClaimRequest> {
        Some(ClaimRequest {
            token: claim_token()?,
            now,
            lease: settings.lease,
            limit: free,
            busy_endpoints: self
                .endpoints
                .iter()
                .filter(|(_, n)| **n >= settings.per_endpoint)
                .map(|(e, _)| e.clone())
                .collect(),
            busy_tenants: self
                .tenants
                .iter()
                .filter(|(_, n)| **n >= settings.per_tenant)
                .map(|(t, _)| t.clone())
                .collect(),
        })
    }
}
