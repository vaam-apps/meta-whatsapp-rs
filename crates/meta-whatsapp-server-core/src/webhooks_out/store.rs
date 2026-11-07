//! The webhooks-out storage port ([`WebhookStore`];
//! `wa_server_webhook_endpoints` and `wa_server_webhook_deliveries` on
//! Postgres): endpoints, each with its cursor in its tenant's stream, and
//! their deliveries.
//!
//! What every backend guarantees, which the conformance suite checks
//! (`conformance::webhooks_out`, feature `conformance`):
//!
//! - **An endpoint is its tenant's**: every read and write names the
//!   tenant, and another tenant's endpoint answers as a missing one; a
//!   delivery takes its tenant from its endpoint's row, never from the
//!   caller. Deleting a tenant deletes its endpoints, and an endpoint's
//!   deletion its deliveries. A tenant has at most the limit it is given
//!   ([`CreateOutcome::LimitReached`]), counted atomically with the insert.
//! - **A fan-out is once** ([`WebhookStore::fan_out`]): it moves the
//!   cursor of one of the endpoint's tenants' streams from the value it
//!   read to the next, atomically with recording the deliveries (each
//!   with that tenant), and does neither when the cursor moved meanwhile
//!   (another replica's fan-out) or the tenant is not one the endpoint
//!   receives; an event has one delivery per endpoint, whatever races.
//! - **An endpoint's tenants** (roadmap M2m): one made with several
//!   receives those tenants' events and no other's, each in a cursor of
//!   its own; deleting one of them deletes its deliveries and takes it
//!   out of the endpoint, which is never widened by that (nor does a
//!   tenant created later under the same id join it).
//! - **A claim is exclusive** ([`WebhookStore::claim`]): a delivery is
//!   claimed by one claimer at a time, until its lease ends or its attempt
//!   is settled; a claim never takes two deliveries of one endpoint, nor
//!   one of an endpoint or a tenant the claimer says is busy, nor one of a
//!   disabled endpoint or a suspended tenant. A settlement names its
//!   claim, and is refused once another claim took the delivery (its
//!   lease ended).
//! - **One clock**: what is due is decided by the instant the caller
//!   passes (the dispatcher's, the service's `Clock`), never the
//!   database's own, so the retry schedule runs on one clock, which a test
//!   controls (design D107; replicas' clocks must agree within the lease).

use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;

use super::{AttemptError, Delivery, DeliveryId, Endpoint, EndpointId, SealedSecret, TenantCursor};
use crate::model::{Listing, PageRequest, TenantId};
use crate::store::StoreResult;

/// An endpoint to create. `Debug` omits its URL (its path or query may
/// carry a token of the receiver's) and its secret.
#[derive(Clone)]
pub struct NewEndpoint {
    /// Its id.
    pub id: EndpointId,
    /// Its tenant.
    pub tenant: TenantId,
    /// Its URL, checked.
    pub url: String,
    /// Its event types.
    pub types: Vec<String>,
    /// Its secret, sealed.
    pub secret: SealedSecret,
    /// Its cursor: the tenant's newest sequence at its creation, so it
    /// receives what comes after. Unused when [`Self::tenants`] is set.
    pub cursor: i64,
    /// The tenants whose events it receives instead of its own tenant's
    /// (roadmap M2m), each at its newest sequence at the creation; `None`
    /// for an endpoint of its own tenant's events. Already checked: the
    /// store records what it is given.
    pub tenants: Option<Vec<TenantCursor>>,
    /// When it is made.
    pub now: OffsetDateTime,
}

impl std::fmt::Debug for NewEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewEndpoint")
            .field("id", &self.id)
            .field("tenant", &self.tenant)
            .field("types", &self.types.len())
            .field("cursor", &self.cursor)
            .field("tenants", &self.tenants.as_ref().map(Vec::len))
            .finish_non_exhaustive()
    }
}

/// What a creation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateOutcome {
    /// Made.
    Created(Box<Endpoint>),
    /// No such tenant (deleted meanwhile).
    NoSuchTenant,
    /// A tenant it names does not exist (deleted meanwhile, roadmap M2m):
    /// nothing is created.
    NamedTenantGone,
    /// The tenant has its limit of endpoints already.
    LimitReached,
}

/// A change of an endpoint (`PATCH`): `None` leaves a field. `Debug`
/// says whether the URL changes, never what to.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EndpointChange {
    /// A new URL, checked.
    pub url: Option<String>,
    /// New event types.
    pub types: Option<Vec<String>>,
    /// On or off. Turning it on clears the service's reason and the
    /// failure streak.
    pub enabled: Option<bool>,
    /// The tenants an endpoint made with several receives (roadmap M2m),
    /// replacing its set: a tenant already in it keeps its cursor (the one
    /// given is ignored), a new one starts at the cursor given, and a
    /// tenant left out stops at once, its pending deliveries deleted
    /// (settled ones stay as the log). Ignored for an endpoint without a
    /// set: an endpoint never gains or loses its set.
    pub tenants: Option<Vec<TenantCursor>>,
    /// The set of tenants the caller's key was checked against before
    /// asking (roadmap M2m, D342): the change applies only while the
    /// endpoint's set is still exactly this (as a set, whatever the
    /// order), `None` meaning an endpoint without one; otherwise the
    /// store changes nothing and answers `None`, as for a missing
    /// endpoint. It closes the window between the reach check and the
    /// write, in which another key could have added a tenant the first
    /// key may not name. The default, `None`, is the safe one: it
    /// never applies to an endpoint that has a set.
    pub reached_through: Option<Vec<TenantId>>,
}

impl std::fmt::Debug for EndpointChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EndpointChange")
            .field("url", &self.url.as_ref().map(|_| ".."))
            .field("types", &self.types.as_ref().map(Vec::len))
            .field("enabled", &self.enabled)
            .field("tenants", &self.tenants.as_ref().map(Vec::len))
            .field(
                "reached_through",
                &self.reached_through.as_ref().map(Vec::len),
            )
            .finish()
    }
}

/// Whether an endpoint's current set of named tenants is the one
/// `reached_through` says the caller was checked against
/// ([`EndpointChange::reached_through`]): the same tenants, order apart.
pub fn reached_through_holds(
    current: Option<&[TenantCursor]>,
    through: Option<&[TenantId]>,
) -> bool {
    match (current, through) {
        (None, None) => true,
        (Some(current), Some(through)) => {
            current.len() == through.len()
                && current.iter().all(|c| through.contains(&c.tenant))
                && through
                    .iter()
                    .all(|t| current.iter().any(|c| &c.tenant == t))
        }
        _ => false,
    }
}

/// A delivery to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDelivery {
    /// Its id.
    pub id: DeliveryId,
    /// Its event's id; `None` for a ping.
    pub event_id: Option<String>,
    /// Its event's sequence; `None` for a ping.
    pub sequence: Option<i64>,
    /// Its event's type, or [`super::PING`].
    pub event_type: String,
    /// When it was made: its first attempt is due then.
    pub created_at: OffsetDateTime,
    /// When its retries end.
    pub give_up_at: OffsetDateTime,
}

/// What a claim asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRequest {
    /// The claim's token: what each delivery it takes records, and its
    /// settlement names.
    pub token: ClaimToken,
    /// The claimer's "now": deliveries due by then, whose lease ended by
    /// then.
    pub now: OffsetDateTime,
    /// How long the claim holds them.
    pub lease: Duration,
    /// At most this many, one per endpoint.
    pub limit: usize,
    /// None of these endpoints' (the claimer has enough of theirs in
    /// flight).
    pub busy_endpoints: Vec<EndpointId>,
    /// None of these tenants' (likewise).
    pub busy_tenants: Vec<TenantId>,
}

/// A claim's token: what its settlement names. `Debug` shows nothing.
#[derive(Clone, PartialEq, Eq)]
pub struct ClaimToken(pub String);

impl std::fmt::Debug for ClaimToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClaimToken(..)")
    }
}

/// A claimed delivery, its endpoint as the claim read it, and its claim.
#[derive(Debug, Clone)]
pub struct Claimed {
    /// The delivery.
    pub delivery: Delivery,
    /// Its endpoint.
    pub endpoint: Endpoint,
    /// The claim.
    pub claim: ClaimToken,
}

/// Where an attempt leaves its delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Delivered.
    Delivered,
    /// Pending, its next attempt due at this instant.
    Retry(OffsetDateTime),
    /// Failed.
    Failed,
}

/// An attempt, settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// The delivery.
    pub delivery: DeliveryId,
    /// Its claim.
    pub claim: ClaimToken,
    /// Where it leaves the delivery.
    pub outcome: Outcome,
    /// When it started.
    pub started_at: OffsetDateTime,
    /// How long it took, in milliseconds.
    pub duration_ms: u32,
    /// The receiver's status, when it answered.
    pub status: Option<u16>,
    /// Why it failed, if it did.
    pub error: Option<AttemptError>,
    /// Whether it counts as one: `false` for an attempt that never left
    /// (its event gone), which neither counts nor touches the endpoint's
    /// failure streak.
    pub counted: bool,
    /// When [`Outcome::Failed`], the endpoint is disabled (reason
    /// `failing`) if it has been failing since this instant or before:
    /// the attempt's end less the retry window (design §4.4: "an endpoint
    /// failing the whole window is disabled"). `None`: never.
    pub disable_if_failing_since: Option<OffsetDateTime>,
}

/// The backlog of webhook deliveries (`wa_server_webhooks_out_*` metrics,
/// roadmap M2k): every pending one, whatever its endpoint or tenant. No
/// id, URL or tenant: it is a count and an instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingDeliveries {
    /// The deliveries still pending (not delivered, not failed): due or
    /// waiting for a retry, claimed or not, of an enabled endpoint or not.
    pub count: u64,
    /// The earliest `next_attempt_at` among the pending ones due by the
    /// instant asked (`next_attempt_at <= now`); `None` when none is due.
    /// A retry waiting for its backoff is not due, so it never ages the
    /// backlog.
    pub oldest_due: Option<OffsetDateTime>,
}

/// The webhooks-out records. Implementations must be safe to share
/// between replicas (a shared database) or tasks (memory).
#[async_trait]
pub trait WebhookStore: Send + Sync + 'static {
    /// Create `endpoint`, unless its tenant has `max` already (counted
    /// atomically with the insert) or does not exist.
    async fn create_endpoint(
        &self,
        endpoint: &NewEndpoint,
        max: usize,
    ) -> StoreResult<CreateOutcome>;

    /// `tenant`'s endpoint `id`; `None` for another tenant's.
    async fn endpoint(&self, tenant: &TenantId, id: &EndpointId) -> StoreResult<Option<Endpoint>>;

    /// `tenant`'s endpoints, in byte order of their ids.
    async fn endpoints(
        &self,
        tenant: &TenantId,
        page: &PageRequest,
    ) -> StoreResult<Listing<Endpoint>>;

    /// Change `tenant`'s endpoint `id` at `now`; `None` for another
    /// tenant's.
    async fn update_endpoint(
        &self,
        tenant: &TenantId,
        id: &EndpointId,
        change: &EndpointChange,
        now: OffsetDateTime,
    ) -> StoreResult<Option<Endpoint>>;

    /// Rotate `tenant`'s endpoint `id`'s secret at `now`: `secret` becomes
    /// the current one, the current one the previous one until
    /// `previous_until` (an older previous one is dropped). `None` for
    /// another tenant's.
    async fn rotate_secret(
        &self,
        tenant: &TenantId,
        id: &EndpointId,
        secret: &SealedSecret,
        previous_until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> StoreResult<Option<Endpoint>>;

    /// Delete `tenant`'s endpoint `id` and its deliveries; `false` for
    /// another tenant's.
    async fn delete_endpoint(&self, tenant: &TenantId, id: &EndpointId) -> StoreResult<bool>;

    /// Every enabled endpoint, every tenant's, in byte order of their
    /// ids: what the dispatcher fans out to.
    async fn active_endpoints(&self) -> StoreResult<Vec<Endpoint>>;

    /// Record `deliveries` for `endpoint`, each of `tenant`'s stream (its
    /// tenant: the endpoint's own, or one of those it names), and move
    /// that stream's cursor from `from` to `to`, atomically; `false`,
    /// nothing done, when the cursor is not `from` any more, the endpoint
    /// is gone or `tenant` is not one it receives. A delivery of an event
    /// the endpoint has one for already is skipped.
    async fn fan_out(
        &self,
        endpoint: &EndpointId,
        tenant: &TenantId,
        from: i64,
        to: i64,
        deliveries: &[NewDelivery],
    ) -> StoreResult<bool>;

    /// Record `delivery` (a ping) for `tenant`'s endpoint `endpoint`;
    /// `None` for another tenant's.
    async fn insert_delivery(
        &self,
        tenant: &TenantId,
        endpoint: &EndpointId,
        delivery: &NewDelivery,
    ) -> StoreResult<Option<Delivery>>;

    /// Claim deliveries due by `request.now` (see the module's rules), the
    /// oldest due first, at most one per endpoint.
    async fn claim(&self, request: &ClaimRequest) -> StoreResult<Vec<Claimed>>;

    /// Settle `attempt`; `false` when its claim no longer holds the
    /// delivery (it was claimed again after its lease, or is gone).
    async fn settle(&self, attempt: &Attempt, now: OffsetDateTime) -> StoreResult<bool>;

    /// `tenant`'s endpoint `endpoint`'s deliveries, newest first (ids in
    /// descending byte order; `page.after` is an exclusive upper bound);
    /// `None` for another tenant's endpoint.
    async fn deliveries(
        &self,
        tenant: &TenantId,
        endpoint: &EndpointId,
        page: &PageRequest,
    ) -> StoreResult<Option<Listing<Delivery>>>;

    /// Make `tenant`'s endpoint's delivery `id` pending again, due at
    /// `now`, its retries ending at `give_up_at` (a failed or delivered
    /// one; a pending one is left as it is); `None` for another tenant's,
    /// another endpoint's or a missing one.
    async fn retry_delivery(
        &self,
        tenant: &TenantId,
        endpoint: &EndpointId,
        id: &DeliveryId,
        now: OffsetDateTime,
        give_up_at: OffsetDateTime,
    ) -> StoreResult<Option<Delivery>>;

    /// Delete the settled deliveries (delivered or failed) settled before
    /// `before`: their last attempt started before it, or, never
    /// attempted, they were made before it (a delivery made long ago and
    /// retried by hand yesterday stays). How many went. No lock of its
    /// own: housekeeping runs it.
    async fn purge_deliveries(&self, before: OffsetDateTime) -> StoreResult<u64>;

    /// The pending deliveries, every tenant's, and the earliest due by
    /// `now` ([`PendingDeliveries`]): what the backlog metrics read. A
    /// read of the pending set only (on Postgres, the partial index
    /// `wa_server_webhook_deliveries_due_idx`); the caller bounds its time.
    async fn pending(&self, now: OffsetDateTime) -> StoreResult<PendingDeliveries>;
}
