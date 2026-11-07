//! The service's own records: tenants, API keys and the bindings of WABAs
//! and phone numbers to tenants (docs/design/server.md, sections 2.2 and
//! 3). Validation lives here, so every store and every route agrees on it.

use std::fmt;

use meta_whatsapp_rs::core::ids::{BusinessId, PhoneNumberId, WabaId};
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

    /// The namespace of an admin key's idempotency records (roadmap
    /// M5j2, design D290): `admin/<key id>`. The `/` is outside the
    /// charset of [`Self::parse`], so no tenant can be named so and an
    /// admin key's record can never be a tenant's, or the other way
    /// round. Crate-private: only [`crate::idempotency::run_admin`], for
    /// an [`crate::authz::AdminCaller`] the authorizer made, names one.
    pub(crate) fn admin_scope(key_id: &str) -> Self {
        Self(format!("admin/{key_id}"))
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
    /// WhatsApp Flows' management (roadmap M5d, design D132): a WABA's
    /// Flows created, read, changed, previewed, published, deprecated and
    /// deleted. A scope of its own: a key that edits templates does not
    /// manage Flows (a stored name, pending the owner's confirmation:
    /// docs/architecture.md, § Stable identifiers).
    Flows,
    /// Erasing a customer on a number (roadmap M2f): irreversible, so a
    /// scope of its own, which `inbox` does not grant (a stored name,
    /// pending the owner's confirmation: docs/architecture.md, § Stable
    /// identifiers).
    Erasure,
    /// Commerce (roadmap M5g): a number's commerce settings (the cart,
    /// the catalog's visibility) and the reads of the catalog a WABA
    /// connects (a stored name, pending the owner's confirmation:
    /// docs/architecture.md, § Stable identifiers).
    Commerce,
    /// QR codes and short links that open a chat with a prefilled
    /// message (roadmap M5g; a stored name, pending the owner's
    /// confirmation).
    QrCodes,
    /// A WABA's analytics (roadmap M5g; a stored name, pending the
    /// owner's confirmation).
    Analytics,
    /// The people a number talks to (roadmap M5g): blocking and
    /// unblocking them, and the block list. Blocking acts on a person,
    /// so a scope of its own, which `send` does not grant (a stored name,
    /// pending the owner's confirmation).
    Contacts,
    /// Groups: a number's groups, their settings, invite links,
    /// participants, join requests and analytics (roadmap M5f; a stored
    /// name, pending the owner's confirmation before the first release).
    Groups,
    /// Marketing Messages API and Click-to-WhatsApp (roadmap M5h): a
    /// number's marketing sends, a WABA's MM API onboarding state, the
    /// Cloud API marketing switch, max price (duplicating a template at
    /// another price, a template's max price, reach estimates) and
    /// welcome message sequences. A scope of its own: marketing sends and
    /// max price spend money, and a key that sends transactional messages
    /// or edits templates does not hold it (a stored name, pending the
    /// owner's confirmation before the first release).
    Marketing,
    /// Conversation routing (roadmap M5l): the routing roles of a number
    /// and the thread control actions (pass, take, release). A thread
    /// action hands a customer's conversation to another responder, so a
    /// scope of its own, which `send` and `inbox` do not grant (a stored
    /// name, pending the owner's confirmation before the first release).
    Routing,
    /// Calling (roadmap M5e): a number's calling settings, a user's call
    /// permission, the call a business places and the actions on a call
    /// (pre-accept, accept, reject, terminate). Calls carry SDP and may be
    /// recorded, so a scope of its own, which `send` does not grant (a
    /// stored name, pending the owner's confirmation before the first
    /// release).
    Calls,
    /// Partner acts on a tenant's own WABA (roadmap M5j): the read of its
    /// business verification status, a migration intent to a
    /// Multi-Partner Solution, and a billing currency or payment method
    /// migration. The last two are legal or irreversible acts, so a scope
    /// of its own, which `numbers` does not grant (a stored name, pending
    /// the owner's confirmation before the first release).
    Partners,
}

impl Scope {
    /// Every scope.
    pub const ALL: [Scope; 21] = [
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
        Self::Flows,
        Self::Erasure,
        Self::Commerce,
        Self::QrCodes,
        Self::Analytics,
        Self::Contacts,
        Self::Groups,
        Self::Marketing,
        Self::Routing,
        Self::Calls,
        Self::Partners,
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
            Self::Flows => "flows",
            Self::Erasure => "erasure",
            Self::Commerce => "commerce",
            Self::QrCodes => "qr_codes",
            Self::Analytics => "analytics",
            Self::Contacts => "contacts",
            Self::Groups => "groups",
            Self::Marketing => "marketing",
            Self::Routing => "routing",
            Self::Calls => "calls",
            Self::Partners => "partners",
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

/// A signup attempt to keep (roadmap M3a): an Embedded Signup onboarding
/// that stopped after its token was stored, which the tenant resumes
/// (`POST /v1/signup/resume`). What [`RecordStore::save_signup_attempt`](crate::store::RecordStore::save_signup_attempt)
/// writes; the store adds the times. **Never the code or the PIN**: the
/// session info is the browser's ids (the library's `SessionInfo`,
/// serialized), checked against what onboarding stored when the attempt
/// is resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSignupAttempt {
    /// The tenant whose attempt it is: the WABA's binding's.
    pub tenant_id: TenantId,
    /// The verified WABA.
    pub waba_id: WabaId,
    /// The step it stopped at (the library's `steps`, e.g.
    /// `register_phone`).
    pub failed_step: String,
    /// The session info, as the library serializes it (`SessionInfo`).
    pub session: serde_json::Value,
    /// Which completion the flow reported (the `event` string, e.g.
    /// `FINISH` or `FINISH_WHATSAPP_BUSINESS_APP_ONBOARDING`), when known:
    /// a coexistence attempt resumes its sync steps.
    pub finish_kind: Option<String>,
}

/// A kept signup attempt, as a store reads it back (roadmap M3a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignupAttempt {
    /// The tenant whose attempt it is.
    pub tenant_id: TenantId,
    /// The verified WABA: one attempt per WABA.
    pub waba_id: WabaId,
    /// The step it last stopped at.
    pub failed_step: String,
    /// The session info ([`NewSignupAttempt::session`]).
    pub session: serde_json::Value,
    /// [`NewSignupAttempt::finish_kind`].
    pub finish_kind: Option<String>,
    /// When it was first kept.
    pub created_at: OffsetDateTime,
    /// When it last stopped.
    pub updated_at: OffsetDateTime,
}

/// A record the token vault keeps past a WABA's binding, which a key
/// rotation must reach though no binding names it any more (roadmap M3a,
/// design section 4.2): the vault cannot list its records. Solution Partner
/// mode writes them, in the vault's own store, under the vault's keys.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VaultEntry {
    /// A WABA whose credit ledger may exist (the library's
    /// `credit/<WABA_ID>`), which outlives its token: the library's
    /// `TokenVault::rotate` reseals it, and the revocation marker of the
    /// business it names.
    Waba(WabaId),
    /// A business whose revocation marker may exist (the library's
    /// `revoked/<BUSINESS_ID>`): `TokenVault::rotate_business` reseals it.
    Business(BusinessId),
}

impl VaultEntry {
    /// Its stored kind: `waba` or `business`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Waba(_) => "waba",
            Self::Business(_) => "business",
        }
    }

    /// Its id.
    pub fn id(&self) -> &str {
        match self {
            Self::Waba(id) => id.as_str(),
            Self::Business(id) => id.as_str(),
        }
    }

    /// Its key in a listing: `<kind>/<id>`, which the listing is in byte
    /// order of, and its cursor names.
    pub fn key(&self) -> String {
        format!("{}/{}", self.kind(), self.id())
    }

    /// The entry of `kind` and `id` as a store keeps them; `None` for a
    /// kind or an id this release did not write (a backend answers that
    /// as a storage failure).
    pub fn parse(kind: &str, id: &str) -> Option<Self> {
        match kind {
            "waba" => WabaId::new(id).ok().map(Self::Waba),
            "business" => BusinessId::new(id).ok().map(Self::Business),
            _ => None,
        }
    }
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

/// A tenant's OTP settings (docs/design/server.md, section 4.7; roadmap
/// M3c, decision D8): the number its codes are sent from, which must be
/// one of the tenant's own numbers, the approved authentication template
/// and its language, and the library's `OtpConfig` but for the namespace,
/// which is always the issuing tenant's id, never a setting. A tenant
/// without settings borrows those of the deployment's default OTP tenant
/// (`WA_SERVER_OTP_DEFAULT_TENANT`: the platform's number, D8's default),
/// if one is set.
///
/// Stored in the tenant's record (Postgres: `wa_server_tenants.settings`,
/// under the key `otp`), in the layout [`Self::to_stored`] writes and
/// [`Self::from_stored`] reads (version 1, its field names pinned by
/// `the_stored_otp_settings_are_pinned`): a stored name, pending the
/// owner's confirmation (docs/architecture.md, § Stable identifiers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtpSettings {
    /// The number codes are sent from: one of the tenant's own.
    pub phone_number_id: PhoneNumberId,
    /// The approved authentication template's name.
    pub template_name: String,
    /// The language it was approved in.
    pub template_language: String,
    /// Digits per code, 4 to 8 (the library's `OtpConfig::code_length`).
    pub code_length: u8,
    /// How long a code is valid, 1 second to 90 minutes: keep it equal to
    /// the template's `code_expiration_minutes` (`OtpConfig::ttl`).
    pub ttl_seconds: u32,
    /// Wrong guesses allowed per code, at least 1
    /// (`OtpConfig::max_attempts`).
    pub max_attempts: u32,
    /// Seconds between two codes for one phone number and purpose
    /// (`OtpConfig::resend_cooldown`).
    pub resend_cooldown_seconds: u32,
    /// Codes allowed per phone number and purpose in any rolling window, at
    /// least 1 (`IssueLimit::max_issues`). The service never turns the
    /// library's issue limit off: it is what bounds an online brute force.
    pub max_issues: u32,
    /// That window, in seconds, at least 1 (`IssueLimit::window`).
    pub issue_window_seconds: u32,
}

/// The stored layout of [`OtpSettings`], version 1.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredOtpSettings {
    v: u8,
    phone_number_id: String,
    template: StoredOtpTemplate,
    code_length: u8,
    ttl_seconds: u32,
    max_attempts: u32,
    resend_cooldown_seconds: u32,
    issue_limit: StoredIssueLimit,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredOtpTemplate {
    name: String,
    language: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredIssueLimit {
    max_issues: u32,
    window_seconds: u32,
}

impl OtpSettings {
    /// The library's defaults (`OtpConfig::new`: 6 digits, 10 minutes, 5
    /// attempts, a 30-second cooldown, 5 codes per rolling hour) for
    /// `template_name` in `template_language`, sent from `phone_number_id`.
    pub fn new(
        phone_number_id: PhoneNumberId,
        template_name: impl Into<String>,
        template_language: impl Into<String>,
    ) -> Self {
        Self {
            phone_number_id,
            template_name: template_name.into(),
            template_language: template_language.into(),
            code_length: 6,
            ttl_seconds: 600,
            max_attempts: 5,
            resend_cooldown_seconds: 30,
            max_issues: 5,
            issue_window_seconds: 3600,
        }
    }

    /// The stored form: a JSON object, version 1.
    pub fn to_stored(&self) -> serde_json::Value {
        let stored = StoredOtpSettings {
            v: 1,
            phone_number_id: self.phone_number_id.as_str().to_owned(),
            template: StoredOtpTemplate {
                name: self.template_name.clone(),
                language: self.template_language.clone(),
            },
            code_length: self.code_length,
            ttl_seconds: self.ttl_seconds,
            max_attempts: self.max_attempts,
            resend_cooldown_seconds: self.resend_cooldown_seconds,
            issue_limit: StoredIssueLimit {
                max_issues: self.max_issues,
                window_seconds: self.issue_window_seconds,
            },
        };
        serde_json::to_value(stored).unwrap_or(serde_json::Value::Null)
    }

    /// Read the stored form back; `None` for anything [`Self::to_stored`]
    /// did not write (another version, a field missing, an id that is not
    /// one): a backend answers that as a storage failure, never as no
    /// settings.
    pub fn from_stored(value: &serde_json::Value) -> Option<Self> {
        let stored: StoredOtpSettings = serde::Deserialize::deserialize(value).ok()?;
        if stored.v != 1 {
            return None;
        }
        Some(Self {
            phone_number_id: PhoneNumberId::new(stored.phone_number_id).ok()?,
            template_name: stored.template.name,
            template_language: stored.template.language,
            code_length: stored.code_length,
            ttl_seconds: stored.ttl_seconds,
            max_attempts: stored.max_attempts,
            resend_cooldown_seconds: stored.resend_cooldown_seconds,
            max_issues: stored.issue_limit.max_issues,
            issue_window_seconds: stored.issue_limit.window_seconds,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kinds of the vault's records kept past a binding (roadmap M3a,
    /// `wa_server_vault_records.kind`) and the listing's key: stored names,
    /// pending the owner's confirmation (docs/architecture.md, § Stable
    /// identifiers). Renamed, the kept entries stop reading (a storage
    /// failure), and a rotation reaches none of them.
    #[test]
    fn the_vault_entry_kinds_are_pinned() {
        let waba = VaultEntry::Waba(WabaId::new("524126980791429").unwrap());
        let business = VaultEntry::Business(BusinessId::new("2729063490586005").unwrap());
        assert_eq!(
            (waba.kind(), waba.key()),
            ("\x77aba", "\x77aba/524126980791429".to_owned())
        );
        assert_eq!(
            (business.kind(), business.key()),
            ("business", "business/2729063490586005".to_owned())
        );
        assert_eq!(VaultEntry::parse("\x77aba", "524126980791429"), Some(waba));
        assert_eq!(
            VaultEntry::parse("business", "2729063490586005"),
            Some(business)
        );
        assert_eq!(VaultEntry::parse("token", "1"), None, "another kind");
        assert_eq!(VaultEntry::parse("\x77aba", "../1"), None, "not an id");
    }

    /// An admin key's idempotency scope (roadmap M5j2, D290) is a name no
    /// tenant id can be: `TenantId::parse` never returns it, whatever the
    /// key's id, and it is `admin/` and the id, a stored name pinned here
    /// (docs/architecture.md, § Stable identifiers). Decisive: the `/`.
    #[test]
    fn the_admin_scope_is_a_name_no_tenant_can_be() {
        for id in ["abc", "merchant-42", "", "wak_0123456789abcdefg"] {
            let scope = TenantId::admin_scope(id);
            assert_eq!(scope.as_str(), format!("admin/{id}"));
            assert_eq!(TenantId::parse(scope.as_str()), None, "{id}");
        }
    }

    /// The stored layout of a tenant's OTP settings (roadmap M3c): its
    /// version and field names are a stored name, pending the owner's
    /// confirmation (docs/architecture.md, § Stable identifiers). Renamed,
    /// every tenant's settings stop reading (a storage failure), so the
    /// literal is spelled out here, not derived.
    #[test]
    fn the_stored_otp_settings_are_pinned() {
        let settings = OtpSettings::new(
            PhoneNumberId::new("106540352242922").unwrap(),
            "login_code",
            "en_US",
        );
        let stored = settings.to_stored();
        assert_eq!(
            stored,
            serde_json::json!({
                "v": 1,
                "phone_number_id": "106540352242922",
                "template": {"name": "login_code", "language": "en_US"},
                "code_length": 6,
                "ttl_seconds": 600,
                "max_attempts": 5,
                "resend_cooldown_seconds": 30,
                "issue_limit": {"max_issues": 5, "window_seconds": 3600}
            })
        );
        assert_eq!(OtpSettings::from_stored(&stored), Some(settings));
        let mut other = stored.clone();
        other["v"] = 2.into();
        assert_eq!(OtpSettings::from_stored(&other), None, "another version");
        let mut bad = stored;
        bad["phone_number_id"] = "../1".into();
        assert_eq!(OtpSettings::from_stored(&bad), None, "not an id");
        assert_eq!(OtpSettings::from_stored(&serde_json::json!({})), None);
    }
}
