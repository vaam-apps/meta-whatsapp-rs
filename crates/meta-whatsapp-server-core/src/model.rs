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
    /// Payments: a WABA's payment configurations, a payment's lookup and
    /// its refunds (roadmap P2). The payment messages themselves are
    /// sends (`send`).
    Payments,
}

impl Scope {
    /// Every scope.
    pub const ALL: [Scope; 10] = [
        Self::Send,
        Self::Media,
        Self::Templates,
        Self::Inbox,
        Self::Events,
        Self::Webhooks,
        Self::Signup,
        Self::Otp,
        Self::Numbers,
        Self::Payments,
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
            Self::Payments => "payments",
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
    /// When it was bound: when this binding began. An attach of the WABA to
    /// the tenant that holds it (a refresh) keeps it.
    pub attached_at: OffsetDateTime,
    /// How many attaches this binding has had: 1 when it began, one more on
    /// every attach of the WABA to the tenant holding it (a refresh: the
    /// operator's reconnect, a token rotation), which keeps
    /// [`Self::attached_at`]. A binding made after an unbind starts again at
    /// 1 (it begins later). Part of the [`BindingEpoch`] (roadmap S2b).
    pub generation: i64,
    /// When the WABA was last unbound from a tenant other than this
    /// binding's: the record of its last move (roadmap S2b, design D36),
    /// read from its [`WabaTombstone`] when this binding began and kept by a
    /// refresh. `None`: no other tenant held it before (a first
    /// onboarding, or the tenant's own WABA attached again after an unbind
    /// of its own). A tenant deleted since counts as another tenant, even
    /// one created again under its id. A binding made before the store
    /// recorded moves (on Postgres, before migration 4) reads
    /// [`UNRECORDED_MOVE`]: a move may have come before it, long ago.
    pub previous_tenant_unbound_at: Option<OffsetDateTime>,
}

/// What [`WabaBinding::previous_tenant_unbound_at`] (and, since roadmap
/// M2h, [`NumberBinding::previous_tenant_released_at`]) reads for a binding
/// made before the store recorded moves (Postgres: its rows from before
/// migration 4, and those a replica of an earlier release writes during a
/// rolling deploy): the Unix epoch, as "a previous tenant may have held
/// the WABA, unbound before the records began". So such a binding keeps
/// the rules of before roadmap S2b: an event of its own second stays
/// operator-only (the rule of a WABA with a previous tenant), and its
/// undated events reach it (a move, if any, is past every window).
pub const UNRECORDED_MOVE: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

impl WabaBinding {
    /// This binding's epoch: what a write conditioned on it compares.
    pub fn epoch(&self) -> BindingEpoch {
        BindingEpoch {
            waba_id: self.waba_id.clone(),
            tenant_id: self.tenant_id.clone(),
            attached_at: self.attached_at,
            generation: self.generation,
        }
    }
}

/// The record a WABA's last unbinding leaves (roadmap S2b): which tenant
/// held it (`None` once that tenant was deleted: any tenant, one created
/// again under its id included, is another), when it was unbound, and when
/// it had last been unbound from a tenant other than that one. A store
/// keeps one per WABA ever unbound (Postgres: `wa_server_waba_tombstones`),
/// written atomically with every unbind
/// ([`RecordStore::unbind_waba`](crate::store::RecordStore::unbind_waba),
/// [`RecordStore::unbind_waba_if`](crate::store::RecordStore::unbind_waba_if)),
/// and reads it when it binds the WABA anew ([`Self::previous_for`]). The
/// memory backends apply these rules as written here, the Postgres one in
/// its SQL; the conformance suite holds both to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WabaTombstone {
    /// The tenant it was unbound from; `None` once that tenant was deleted.
    pub tenant_id: Option<TenantId>,
    /// When it was unbound.
    pub unbound_at: OffsetDateTime,
    /// The unbound binding's [`WabaBinding::previous_tenant_unbound_at`].
    pub previous_tenant_unbound_at: Option<OffsetDateTime>,
}

impl WabaTombstone {
    /// The record `binding`'s unbinding at `at` leaves.
    pub fn of(binding: &WabaBinding, at: OffsetDateTime) -> Self {
        Self {
            tenant_id: Some(binding.tenant_id.clone()),
            unbound_at: at,
            previous_tenant_unbound_at: binding.previous_tenant_unbound_at,
        }
    }

    /// When the WABA was last unbound from a tenant other than `tenant`,
    /// for a new binding of it to `tenant`: this unbinding, when it was
    /// another tenant's (or a deleted tenant's); else the one before it,
    /// which the unbound binding carried.
    pub fn previous_for(&self, tenant: &TenantId) -> Option<OffsetDateTime> {
        if self.tenant_id.as_ref() == Some(tenant) {
            self.previous_tenant_unbound_at
        } else {
            Some(self.unbound_at)
        }
    }

    /// `tenant` was deleted: from now on, any tenant is another.
    pub fn forget_tenant(&mut self, tenant: &TenantId) {
        if self.tenant_id.as_ref() == Some(tenant) {
            self.tenant_id = None;
        }
    }
}

/// A WABA's binding as it was read (by a capability, when it was made):
/// which tenant held the WABA, since when, after how many attaches. A
/// write conditioned on it (`RecordStore::unbind_waba_if`,
/// `RecordStore::set_waba_status_if`) does nothing once the WABA was
/// unbound since, and bound again or not, to the same tenant or another (a
/// binding made later begins later), nor once the binding was refreshed
/// for its own tenant (`RecordStore::bind_waba` on a WABA the tenant
/// holds moves [`Self::generation`], roadmap S2b): a capability made
/// before a refresh is told apart from one made after it. `attached_at` is
/// compared as the store gave it (a backend returns what it stored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingEpoch {
    /// The WABA.
    pub waba_id: WabaId,
    /// The tenant that held it.
    pub tenant_id: TenantId,
    /// When that binding began.
    pub attached_at: OffsetDateTime,
    /// Its [`WabaBinding::generation`] then.
    pub generation: i64,
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
    /// When the number came under its tenant: when a binding of the tenant
    /// first listed it (roadmap M2h). A refresh of its WABA, and a move to
    /// another WABA of the same tenant while it is bound, keep it; listed
    /// again after its tenant released it, it begins again. Never before
    /// its last release ([`NumberTombstone::released_at`]). A number bound
    /// before the store recorded numbers' moves (on Postgres, before
    /// migration 5) reads its WABA's [`WabaBinding::attached_at`].
    pub attached_at: OffsetDateTime,
    /// When the number was last released by a tenant other than its
    /// current one: the record of its last move (roadmap M2h, the
    /// number's counterpart of [`WabaBinding::previous_tenant_unbound_at`]),
    /// read from its [`NumberTombstone`] when it came under its tenant and
    /// kept while it stays there. `None`: no other tenant held it. A
    /// tenant deleted since counts as another tenant. A number bound before
    /// the store recorded numbers' moves reads [`UNRECORDED_MOVE`].
    pub previous_tenant_released_at: Option<OffsetDateTime>,
}

impl NumberBinding {
    /// `phone_number_id`, newly bound to `tenant` under `waba_id` at
    /// `now` (it was not `tenant`'s a moment before), after its last
    /// release `released` (`None`: never released): connected, its
    /// [`Self::attached_at`] `now` but never before that release, and its
    /// previous tenant read from the release ([`NumberTombstone::previous_for`]).
    /// The memory backends apply this as written; the Postgres one in its
    /// SQL; the conformance suite holds both to it.
    pub fn newly_bound(
        phone_number_id: PhoneNumberId,
        waba_id: WabaId,
        tenant: TenantId,
        now: OffsetDateTime,
        released: Option<&NumberTombstone>,
    ) -> Self {
        Self {
            previous_tenant_released_at: released.and_then(|t| t.previous_for(&tenant)),
            attached_at: released.map_or(now, |t| now.max(t.released_at)),
            phone_number_id,
            waba_id,
            tenant_id: tenant,
            status: NumberStatus::Connected,
            updated_at: now,
        }
    }
}

/// The record a number's last release leaves (roadmap M2h, the number's
/// counterpart of [`WabaTombstone`]): which tenant held it (`None` once
/// that tenant was deleted), when it was released (its WABA unbound, or a
/// binding that no longer lists it), and when it had last been released
/// by a tenant other than that one. A store keeps one per number ever
/// released (Postgres: `wa_server_number_tombstones`), written atomically
/// with the release, and reads it when a binding takes the number anew
/// ([`NumberBinding::newly_bound`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberTombstone {
    /// The tenant that released it; `None` once that tenant was deleted.
    pub tenant_id: Option<TenantId>,
    /// When it was released.
    pub released_at: OffsetDateTime,
    /// The released binding's [`NumberBinding::previous_tenant_released_at`].
    pub previous_tenant_released_at: Option<OffsetDateTime>,
}

impl NumberTombstone {
    /// The record `binding`'s release at `at` leaves.
    pub fn of(binding: &NumberBinding, at: OffsetDateTime) -> Self {
        Self {
            tenant_id: Some(binding.tenant_id.clone()),
            released_at: at,
            previous_tenant_released_at: binding.previous_tenant_released_at,
        }
    }

    /// When the number was last released by a tenant other than `tenant`,
    /// for a binding of it to `tenant`: this release, when it was another
    /// tenant's (or a deleted tenant's); else the one before it.
    pub fn previous_for(&self, tenant: &TenantId) -> Option<OffsetDateTime> {
        if self.tenant_id.as_ref() == Some(tenant) {
            self.previous_tenant_released_at
        } else {
            Some(self.released_at)
        }
    }

    /// `tenant` was deleted: from now on, any tenant is another.
    pub fn forget_tenant(&mut self, tenant: &TenantId) {
        if self.tenant_id.as_ref() == Some(tenant) {
            self.tenant_id = None;
        }
    }
}

/// A page request: at most `limit` items after the exclusive cursor
/// `after` (the last id of the previous page), in byte order of the ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    /// Exclusive lower bound, in byte order of the ids.
    pub after: Option<String>,
    /// Page size, 1 to [`MAX_PAGE_SIZE`].
    pub limit: usize,
}

/// Largest page.
pub const MAX_PAGE_SIZE: usize = 100;

/// A page of items and whether more follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing<T> {
    /// The items, in byte order of their ids.
    pub items: Vec<T>,
    /// The last id of this page when another page follows.
    pub next_after: Option<String>,
}

/// Whether a WABA and its numbers could be bound to a tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindOutcome {
    /// Bound (or already bound to that tenant: numbers refreshed, the
    /// binding's generation moved).
    Bound,
    /// The WABA, or one of its numbers, belongs to another tenant: nothing
    /// changed (decision D4: refuse).
    OwnedByAnotherTenant,
    /// No such tenant (never created, or deleted before the binding):
    /// nothing changed.
    NoSuchTenant,
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
    /// SHA-256 of the request that claimed it: its operation and its
    /// canonical input (`idempotency::Fingerprint`; a record claimed
    /// before roadmap S6 holds that of its method, path and body).
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
