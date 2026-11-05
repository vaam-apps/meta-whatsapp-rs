//! Webhooks out (docs/design/server.md, section 4.4; roadmap M2c): a
//! tenant's events pushed to URLs of its own, signed the
//! [Standard Webhooks](https://www.standardwebhooks.com/) way, retried with
//! backoff, to destinations an allow-list admits only.
//!
//! | Module | What |
//! | --- | --- |
//! | [`signature`] | Standard Webhooks: `whsec_` secrets, `webhook-id`, `webhook-timestamp`, `webhook-signature` (`v1,<base64 HMAC-SHA256>` over `id.timestamp.body`, several during a rotation), and their verification |
//! | [`seal`] | the secrets sealed at rest under `WA_SERVER_DATA_KEY` (AES-256-GCM, bound to their endpoint) |
//! | [`destination`] | the allow-list (`WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS`): `https` only outside development, the host or every address listed, private, loopback, link-local and metadata addresses refused unless a listed CIDR holds them, checked at creation and again, on a fresh lookup, at every attempt; the [`destination::Resolver`] port |
//! | [`schedule`] | the retry schedule: 10 s doubling to 1 h a step, with jitter, for 72 h |
//! | [`store`] | the [`store::WebhookStore`] port: endpoints, their cursors, deliveries, claims and their leases |
//! | [`endpoints`] | the routes' services: create (the secret shown once), list, read, change, delete, rotate the secret, send a ping, list deliveries, retry one |
//! | [`dispatch`] | the dispatcher: the outbox fanned out to each endpoint's deliveries, claimed under a lease (one replica sends each), signed and sent through the [`dispatch::Sender`] port to the address the check pinned, bounded per endpoint, tenant and replica |
//!
//! **The outbox stays the source of truth.** An endpoint is a cursor in
//! its tenant's stream, as a live stream is ([`crate::live`]): the
//! dispatcher reads the outbox after it ([`crate::outbox::Outbox::page`],
//! the endpoint's types as the filter), records one delivery per event
//! (once: an event has one delivery per endpoint), and moves the cursor in
//! the same step. A delivery names its event; its body is built from the
//! outbox at each attempt, in the endpoint's tenant's stream only
//! ([`crate::outbox::Outbox::get`]), so no delivery carries another
//! tenant's event, and none survives its event's purge.
//!
//! **What is not built** (the design's §4.4 names them): `tenants` on an
//! endpoint made with a platform key (an endpoint is its creator's tenant's,
//! the `WA-Tenant` it acted as; roadmap M2m), a data key rotation walk (a
//! previous key still opens what it sealed; M2i), and a connection pool to
//! the receivers (each attempt opens its own connection to the address it
//! checked; M2j).

use std::fmt;
use std::time::Duration;

use time::OffsetDateTime;

use crate::model::TenantId;

pub mod destination;
pub mod dispatch;
pub mod endpoints;
pub mod schedule;
pub mod seal;
pub mod signature;
pub mod store;

/// How long an attempt may take, from its connection to the answer's
/// head (design §4.4: 10 s).
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// The most of a receiver's answer read (design §4.4: 1 KiB); the rest is
/// never read.
pub const MAX_RESPONSE_BYTES: usize = 1024;

/// An event whose `data` is larger than this is delivered without it
/// (`truncated: true`), for `GET /v1/events/{id}` to answer whole
/// (design §4.4: 256 KiB, a history sync).
pub const MAX_DELIVERED_DATA_BYTES: usize = 256 * 1024;

/// How long the previous secret signs too after a rotation (design §4.4:
/// 24 h).
pub const SECRET_OVERLAP: Duration = Duration::from_hours(24);

/// Default `WA_SERVER_WEBHOOK_RETRY_WINDOW` (design §7.2): how long a
/// delivery is retried before it is `failed`.
pub const DEFAULT_RETRY_WINDOW: Duration = Duration::from_hours(72);

/// How many endpoints one tenant may have (the service's default: the
/// design states none).
pub const MAX_ENDPOINTS_PER_TENANT: usize = 10;

/// The longest URL an endpoint takes.
pub const MAX_URL_LEN: usize = 2048;

/// How long the delivery records of a settled delivery (delivered or
/// failed) are kept before housekeeping purges them (design D10's
/// proposal for delivery logs: 30 days).
pub const DELIVERY_RETENTION: Duration = Duration::from_hours(30 * 24);

/// An endpoint's id: `whe_` and 32 lowercase hex digits, random.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EndpointId(String);

/// A delivery's id: `whd_`, 12 hex digits of the milliseconds it was made
/// at and 20 random ones, so that their byte order is their order of
/// creation (a delivery list is newest first).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeliveryId(String);

/// `prefix` and `hex` lowercase hex digits.
fn shaped(id: &str, prefix: &str, hex: usize) -> bool {
    id.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == hex
            && rest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// `n` random bytes, as lowercase hex; `None` when the system has no
/// randomness to give.
fn random_hex(n: usize) -> Option<String> {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).ok()?;
    Some(hex::encode(bytes))
}

impl EndpointId {
    /// A new random id; `None` without randomness.
    pub fn generate() -> Option<Self> {
        random_hex(16).map(|hex| Self(format!("whe_{hex}")))
    }

    /// `id`, if it has an endpoint id's shape.
    pub fn parse(id: &str) -> Option<Self> {
        shaped(id, "whe_", 32).then(|| Self(id.to_owned()))
    }

    /// The id.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl DeliveryId {
    /// A new id made at `now`; `None` without randomness.
    pub fn generate(now: OffsetDateTime) -> Option<Self> {
        let millis = u64::try_from(now.unix_timestamp_nanos() / 1_000_000).unwrap_or(0);
        // 48 bits of milliseconds: until the year 10889.
        let millis = millis & 0xffff_ffff_ffff;
        random_hex(10).map(|hex| Self(format!("whd_{millis:012x}{hex}")))
    }

    /// `id`, if it has a delivery id's shape.
    pub fn parse(id: &str) -> Option<Self> {
        shaped(id, "whd_", 32).then(|| Self(id.to_owned()))
    }

    /// The id.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EndpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for EndpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EndpointId({})", self.0)
    }
}

impl fmt::Display for DeliveryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for DeliveryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeliveryId({})", self.0)
    }
}

/// A signing secret as stored: sealed under the data key
/// ([`seal::DataKeys::seal`]). Opaque; `Debug` shows nothing of it.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedSecret(String);

impl SealedSecret {
    /// A stored value, as a backend read it.
    pub fn from_stored(stored: impl Into<String>) -> Self {
        Self(stored.into())
    }

    /// The value to store.
    pub fn as_stored(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SealedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SealedSecret(..)")
    }
}

/// Why the service turned an endpoint off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisabledReason {
    /// Its deliveries failed for the whole retry window (design §4.4).
    Failing,
    /// A reason a later release stored.
    Other(String),
}

impl DisabledReason {
    /// Its stored and API name.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Failing => "failing",
            Self::Other(other) => other,
        }
    }

    /// The reason stored as `stored`.
    pub fn parse(stored: &str) -> Self {
        match stored {
            "failing" => Self::Failing,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// A webhook endpoint (`wa_server_webhook_endpoints` on Postgres).
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Its id.
    pub id: EndpointId,
    /// The tenant whose events it receives, and who alone reads it.
    pub tenant: TenantId,
    /// Where the events go: checked against the allow-list at creation,
    /// at each change, and at every attempt.
    pub url: String,
    /// The event types it receives (`KnownEventType`s).
    pub types: Vec<String>,
    /// Whether deliveries are made to it.
    pub enabled: bool,
    /// Why the service disabled it, if it did.
    pub disabled_reason: Option<DisabledReason>,
    /// The signing secret, sealed.
    pub secret: SealedSecret,
    /// The secret before the last rotation, which signs too until
    /// [`Self::previous_secret_until`].
    pub previous_secret: Option<SealedSecret>,
    /// Until when [`Self::previous_secret`] signs.
    pub previous_secret_until: Option<OffsetDateTime>,
    /// The last sequence of its tenant's stream fanned out to it: the
    /// events after it are delivered next.
    pub cursor: i64,
    /// Since when its attempts have failed without a success between.
    pub failing_since: Option<OffsetDateTime>,
    /// When it was created.
    pub created_at: OffsetDateTime,
    /// When it last changed.
    pub updated_at: OffsetDateTime,
}

impl Endpoint {
    /// The secrets that sign an attempt at `now`: the current one, then
    /// the previous one while it overlaps.
    pub fn signing_secrets(&self, now: OffsetDateTime) -> Vec<&SealedSecret> {
        let mut secrets = vec![&self.secret];
        if let (Some(previous), Some(until)) = (&self.previous_secret, self.previous_secret_until)
            && now < until
        {
            secrets.push(previous);
        }
        secrets
    }
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The URL may carry a token of the receiver's in its path or query.
        f.debug_struct("Endpoint")
            .field("id", &self.id)
            .field("tenant", &self.tenant)
            .field("types", &self.types.len())
            .field("enabled", &self.enabled)
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
}

/// Where a delivery stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    /// Waiting for its next attempt (or in one).
    Pending,
    /// A 2xx answered an attempt.
    Delivered,
    /// Its retry window ended (or its event is gone) without a 2xx; a
    /// retry through the API makes it pending again.
    Failed,
}

impl DeliveryStatus {
    /// Its stored and API name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
        }
    }

    /// The status stored as `stored`; `None` for another.
    pub fn parse(stored: &str) -> Option<Self> {
        match stored {
            "pending" => Some(Self::Pending),
            "delivered" => Some(Self::Delivered),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Why an attempt failed, as a delivery records it (never a message of
/// the receiver's, nor its body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptError {
    /// No answer within [`DELIVERY_TIMEOUT`].
    Timeout,
    /// The connection to the checked address failed.
    Connect,
    /// The TLS handshake failed (an untrusted or mismatched certificate).
    Tls,
    /// The receiver answered something other than 2xx (a redirect
    /// included: never followed); the status is the attempt's.
    Status,
    /// The answer was not HTTP.
    Protocol,
    /// The destination is not allowed (anymore): the URL, or an address
    /// its host resolved to, is outside the allow-list.
    DestinationRefused,
    /// The host did not resolve.
    ResolveFailed,
    /// The signing secret could not be opened (its data key is not
    /// configured).
    SecretUnavailable,
    /// The event is no longer in the outbox (purged, or its tenant gone).
    EventGone,
    /// A reason a later release stored.
    Other(String),
}

impl AttemptError {
    /// Its stored and API name.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Timeout => "timeout",
            Self::Connect => "connect",
            Self::Tls => "tls",
            Self::Status => "status",
            Self::Protocol => "protocol",
            Self::DestinationRefused => "destination_refused",
            Self::ResolveFailed => "resolve_failed",
            Self::SecretUnavailable => "secret_unavailable",
            Self::EventGone => "event_gone",
            Self::Other(other) => other,
        }
    }

    /// The error stored as `stored`.
    pub fn parse(stored: &str) -> Self {
        match stored {
            "timeout" => Self::Timeout,
            "connect" => Self::Connect,
            "tls" => Self::Tls,
            "status" => Self::Status,
            "protocol" => Self::Protocol,
            "destination_refused" => Self::DestinationRefused,
            "resolve_failed" => Self::ResolveFailed,
            "secret_unavailable" => Self::SecretUnavailable,
            "event_gone" => Self::EventGone,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// The type of a test delivery (`POST /v1/webhook-endpoints/{id}/test`):
/// no event, its body names the endpoint.
pub const PING: &str = "ping";

/// A delivery: one event (or a ping) to one endpoint, and its attempts so
/// far (`wa_server_webhook_deliveries` on Postgres). No body: it is built
/// from the outbox at each attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// Its id.
    pub id: DeliveryId,
    /// Its endpoint.
    pub endpoint: EndpointId,
    /// Its endpoint's tenant, whose stream its event is read from.
    pub tenant: TenantId,
    /// Its event's id (`evt_…`); `None` for a ping.
    pub event_id: Option<String>,
    /// Its event's sequence in the tenant's stream; `None` for a ping.
    pub sequence: Option<i64>,
    /// Its event's type, or [`PING`].
    pub event_type: String,
    /// Where it stands.
    pub status: DeliveryStatus,
    /// Attempts made.
    pub attempts: u32,
    /// When the next attempt is due, while pending.
    pub next_attempt_at: Option<OffsetDateTime>,
    /// After this instant, a failed attempt fails the delivery.
    pub give_up_at: OffsetDateTime,
    /// The last attempt's start.
    pub last_attempt_at: Option<OffsetDateTime>,
    /// The last attempt's HTTP status, when the receiver answered.
    pub last_status: Option<u16>,
    /// Why the last attempt failed, if it did.
    pub last_error: Option<AttemptError>,
    /// How long the last attempt took, in milliseconds.
    pub last_duration_ms: Option<u32>,
    /// When it was made.
    pub created_at: OffsetDateTime,
}
