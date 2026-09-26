//! The service's own records: tenants, API keys and the bindings of WABAs
//! and phone numbers to tenants (docs/design/server.md, sections 2.2 and
//! 3). Validation lives here, so every store and every route agrees on it.

use std::fmt;

use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use time::OffsetDateTime;

/// A tenant id: the integrator's own (e.g. the CMS merchant id), 1 to 64
/// characters of `[A-Za-z0-9._:-]`. Immutable: it is the OTP namespace,
/// so changing it would invalidate codes in flight.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TenantId(String);

/// Longest tenant id.
pub const MAX_TENANT_ID_LEN: usize = 64;

impl TenantId {
    /// Validate `id`; `None` when it is empty, too long or holds a
    /// character outside `[A-Za-z0-9._:-]`.
    pub fn parse(id: &str) -> Option<Self> {
        let valid = !id.is_empty()
            && id.len() <= MAX_TENANT_ID_LEN
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
        valid.then(|| Self(id.to_owned()))
    }

    /// The id.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TenantId({})", self.0)
    }
}

/// Whether a tenant may act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantStatus {
    /// Its keys work.
    Active,
    /// Its keys, and platform keys naming it, get `403 tenant_suspended`.
    Suspended,
}

impl TenantStatus {
    /// Stored and API name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suspended => "suspended",
        }
    }

    /// Parse the stored or API name.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "suspended" => Some(Self::Suspended),
            _ => None,
        }
    }
}

/// A tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tenant {
    /// Its id.
    pub id: TenantId,
    /// A label for operators.
    pub name: String,
    /// Active or suspended.
    pub status: TenantStatus,
    /// When it was created.
    pub created_at: OffsetDateTime,
    /// When it last changed.
    pub updated_at: OffsetDateTime,
}

/// Longest tenant or key name.
pub const MAX_NAME_CHARS: usize = 256;

/// What a key may do on tenant routes (docs/design/server.md, section 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Messages.
    Send,
    /// Media.
    Media,
    /// Templates.
    Templates,
    /// The inbox.
    Inbox,
    /// Events (poll and stream).
    Events,
    /// Webhook endpoints.
    Webhooks,
    /// Embedded Signup.
    Signup,
    /// OTP.
    Otp,
    /// WABAs, numbers and profiles.
    Numbers,
}

impl Scope {
    /// Every scope.
    pub const ALL: [Scope; 9] = [
        Self::Send,
        Self::Media,
        Self::Templates,
        Self::Inbox,
        Self::Events,
        Self::Webhooks,
        Self::Signup,
        Self::Otp,
        Self::Numbers,
    ];

    /// Stored and API name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Media => "media",
            Self::Templates => "templates",
            Self::Inbox => "inbox",
            Self::Events => "events",
            Self::Webhooks => "webhooks",
            Self::Signup => "signup",
            Self::Otp => "otp",
            Self::Numbers => "numbers",
        }
    }

    /// Parse the stored or API name.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scope| scope.as_str() == s)
    }
}

/// Who a key acts as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// Its own tenant.
    Tenant,
    /// The tenant named by `WA-Tenant`, within its allowed set.
    Platform,
    /// `/v1/admin` only.
    Admin,
}

impl KeyKind {
    /// Stored and API name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::Platform => "platform",
            Self::Admin => "admin",
        }
    }

    /// Parse the stored name.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tenant" => Some(Self::Tenant),
            "platform" => Some(Self::Platform),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }
}

/// The tenants a platform key may name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowedTenants {
    /// Any tenant (`"*"`).
    All,
    /// These.
    Only(Vec<TenantId>),
}

impl AllowedTenants {
    /// Whether `tenant` is allowed.
    pub fn allows(&self, tenant: &TenantId) -> bool {
        match self {
            Self::All => true,
            Self::Only(list) => list.contains(tenant),
        }
    }
}

/// Who a key belongs to and what it may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOwner {
    /// A tenant key.
    Tenant(TenantId),
    /// A platform key and the tenants it may name.
    Platform(AllowedTenants),
    /// An admin key.
    Admin,
}

impl KeyOwner {
    /// The kind of key.
    pub fn kind(&self) -> KeyKind {
        match self {
            Self::Tenant(_) => KeyKind::Tenant,
            Self::Platform(_) => KeyKind::Platform,
            Self::Admin => KeyKind::Admin,
        }
    }
}

/// An API key as stored: its id and the SHA-256 digest of its secret,
/// never the secret. `Debug` leaves the digest out.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKeyRecord {
    /// Public id (the part of the key after `wak_`, before the secret).
    pub key_id: String,
    /// SHA-256 of the secret.
    pub secret_sha256: [u8; 32],
    /// Whose key it is.
    pub owner: KeyOwner,
    /// Tenant-route scopes (empty for admin keys).
    pub scopes: Vec<Scope>,
    /// A label for operators.
    pub name: String,
    /// When it was minted.
    pub created_at: OffsetDateTime,
    /// When it stops working, if it does.
    pub expires_at: Option<OffsetDateTime>,
    /// When it was revoked.
    pub revoked_at: Option<OffsetDateTime>,
    /// When it was last used (updated at most once a minute).
    pub last_used_at: Option<OffsetDateTime>,
}

impl fmt::Debug for ApiKeyRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKeyRecord")
            .field("key_id", &self.key_id)
            .field("owner", &self.owner)
            .field("scopes", &self.scopes)
            .field("revoked", &self.revoked_at.is_some())
            .finish_non_exhaustive()
    }
}

impl ApiKeyRecord {
    /// Whether the key works at `now`: not revoked, not expired.
    pub fn is_usable(&self, now: OffsetDateTime) -> bool {
        self.revoked_at.is_none() && self.expires_at.is_none_or(|at| now < at)
    }
}

/// A new key, before it is stored.
#[derive(Clone, PartialEq, Eq)]
pub struct NewApiKey {
    /// Public id.
    pub key_id: String,
    /// SHA-256 of the secret.
    pub secret_sha256: [u8; 32],
    /// Whose key it is.
    pub owner: KeyOwner,
    /// Tenant-route scopes.
    pub scopes: Vec<Scope>,
    /// A label for operators.
    pub name: String,
    /// When it stops working.
    pub expires_at: Option<OffsetDateTime>,
}

impl fmt::Debug for NewApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewApiKey")
            .field("key_id", &self.key_id)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// Which keys a listing or a revocation addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyScope {
    /// The tenant keys of one tenant.
    Tenant(TenantId),
    /// Platform keys.
    Platform,
    /// Admin keys.
    Admin,
}

/// A WABA bound to a tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WabaBinding {
    /// The WABA.
    pub waba_id: WabaId,
    /// Its tenant.
    pub tenant_id: TenantId,
    /// Solution Partner credit allocation config id, when a line was
    /// shared.
    pub credit_allocation_id: Option<String>,
    /// When it was bound.
    pub attached_at: OffsetDateTime,
}

/// The connection status of a bound number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberStatus {
    /// Usable.
    Connected,
    /// A call with its token got Meta's `190` (or the token expired):
    /// reconnect (attach again, or onboard again).
    ReconnectRequired,
    /// Offboarded; history is kept.
    Disconnected,
}

impl NumberStatus {
    /// Stored and API name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::ReconnectRequired => "reconnect_required",
            Self::Disconnected => "disconnected",
        }
    }

    /// Parse the stored name.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "connected" => Some(Self::Connected),
            "reconnect_required" => Some(Self::ReconnectRequired),
            "disconnected" => Some(Self::Disconnected),
            _ => None,
        }
    }
}

/// A phone number bound to a tenant, through its WABA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberBinding {
    /// The number.
    pub phone_number_id: PhoneNumberId,
    /// Its WABA.
    pub waba_id: WabaId,
    /// Its tenant.
    pub tenant_id: TenantId,
    /// Its connection status.
    pub status: NumberStatus,
    /// When the status last changed.
    pub updated_at: OffsetDateTime,
}

/// A page request: at most `limit` items after the exclusive cursor
/// `after` (the last id of the previous page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    /// Exclusive lower bound, in id order.
    pub after: Option<String>,
    /// Page size, 1 to [`MAX_PAGE_SIZE`].
    pub limit: usize,
}

/// Largest page.
pub const MAX_PAGE_SIZE: usize = 100;

/// A page of items and whether more follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing<T> {
    /// The items, in id order.
    pub items: Vec<T>,
    /// The last id of this page when another page follows.
    pub next_after: Option<String>,
}

/// Whether a WABA and its numbers could be bound to a tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindOutcome {
    /// Bound (or already bound to that tenant, numbers refreshed).
    Bound,
    /// The WABA, or one of its numbers, belongs to another tenant: nothing
    /// changed (decision D4: refuse).
    OwnedByAnotherTenant,
}

/// Whether a tenant was deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteTenantOutcome {
    /// Deleted, with its keys.
    Deleted,
    /// No such tenant.
    NotFound,
    /// It still has WABAs: disconnect or unbind them first.
    HasWabas,
}

/// Longest `Idempotency-Key` accepted, in bytes.
pub const MAX_IDEMPOTENCY_KEY_LEN: usize = 255;

/// A caller's `Idempotency-Key` (docs/design/server.md, section 5.4): 1
/// to [`MAX_IDEMPOTENCY_KEY_LEN`] visible ASCII characters (`!` to `~`),
/// scoped to its tenant. Callers derive it from their own records
/// (`order:1234:shipped`). `Debug` shows its length only: it names the
/// caller's records.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Validate `key`; `None` when it is empty, too long, or holds a
    /// character outside `!` to `~` (a space or a control character could
    /// forge a log line; the key is never logged, but stays inert).
    pub fn parse(key: &str) -> Option<Self> {
        let valid = !key.is_empty()
            && key.len() <= MAX_IDEMPOTENCY_KEY_LEN
            && key.bytes().all(|b| b.is_ascii_graphic());
        valid.then(|| Self(key.to_owned()))
    }

    /// The key.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for IdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IdempotencyKey({} bytes)", self.0.len())
    }
}

/// What claiming an idempotency key found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyClaim {
    /// The key was free (or its record had expired): it is now
    /// `in_progress` for this request, which settles it with the claim id
    /// it gave.
    Claimed,
    /// Another request holds, or held, the key.
    Existing(IdempotencyRecord),
}

/// An idempotency record another request left. `Debug` shows its state
/// only: the fingerprint hashes the request's body, and a kept answer's
/// body is the caller's data.
#[derive(Clone, PartialEq, Eq)]
pub struct IdempotencyRecord {
    /// SHA-256 of the request that claimed it (method, path, body).
    pub fingerprint: [u8; 32],
    /// Its state.
    pub state: IdempotencyState,
}

impl fmt::Debug for IdempotencyRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdempotencyRecord")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// Where the request holding a key stands. `Debug` shows a kept answer's
/// length (`body_len`), never its bytes.
#[derive(Clone, PartialEq, Eq)]
pub enum IdempotencyState {
    /// Still running, or stopped without settling it.
    InProgress {
        /// Whether its lease ended: the request crashed or was cut, and
        /// its outcome is unknown.
        lease_expired: bool,
    },
    /// Done, and this is what it answered.
    Completed {
        /// HTTP status.
        status: u16,
        /// JSON body, byte for byte.
        body: Vec<u8>,
    },
}

impl fmt::Debug for IdempotencyState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InProgress { lease_expired } => f
                .debug_struct("InProgress")
                .field("lease_expired", lease_expired)
                .finish(),
            Self::Completed { status, body } => f
                .debug_struct("Completed")
                .field("status", status)
                .field("body_len", &body.len())
                .finish(),
        }
    }
}
