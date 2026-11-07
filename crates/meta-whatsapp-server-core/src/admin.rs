//! The admin operations (docs/design/server.md, sections 3.4 and 4.2;
//! decision D4), as services an API adapter calls: tenants, their keys,
//! platform keys, attaching the platform's own WABAs, reading and
//! unbinding a binding, rotating the vault's key. Each takes domain
//! inputs (ids and labels as the request carried them, so that every
//! adapter validates them alike, in the same order, on the same fields),
//! and answers a domain value or a [`ServiceError`], which the adapter
//! renders (the axum one: the section 5.1 body).
//!
//! Every operation is [`Admin`]'s, which only an [`AdminCaller`] its own
//! [`Authorizer`] made opens ([`Admin::new`]): the records it writes and
//! the vault it stores in are that [`Authorizer`]'s, so a forger's
//! capability, or records of one's own, reach nothing through it. The
//! operator's changes are audited here ([`crate::audit`]), each once it
//! took effect, and a failed Graph call is reported to the adapter's
//! [`Observer`] (its metrics).
//!
//! What needs no capability is the command line's too, run by an operator
//! on the database directly: [`mint`] (the first admin key, platform
//! keys), [`allowed_tenants`] and [`is_valid_name`].

use std::collections::BTreeSet;

use futures::{StreamExt, TryStreamExt};
use meta_whatsapp_rs::client::embedded_signup::{StoredBusinessToken, steps};
use meta_whatsapp_rs::client::waba::PhoneNumbersQuery;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::{Error, ErrorKind};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::audit::{Subject, audit};
use crate::authz::{AdminCaller, Authorizer, OwnedWaba, VaultRotation};
use crate::error::{ErrorCode, ServiceError};
use crate::keys::MintedKey;
use crate::model::{
    AllowedTenants, ApiKeyRecord, BindOutcome, DeleteTenantOutcome, KeyOwner, KeyScope, Listing,
    MAX_NAME_CHARS, MAX_PAGE_SIZE, NewApiKey, NumberBinding, OtpSettings, PageRequest, Scope,
    Tenant, TenantId, TenantStatus, WabaBinding,
};
use crate::number_events::{DisconnectReason, NumberEvent};
use crate::observe::Observer;
use crate::signup::Signups;
use crate::store::RecordStore;
use crate::webhooks_out::endpoints::WebhookEndpoints;
use crate::webhooks_out::reseal::ResealReport;

/// Most numbers an attach binds for one WABA (Meta allows far fewer): past
/// it the listing is refused as `upstream` rather than followed for ever.
pub const MAX_WABA_NUMBERS: usize = 1000;

/// Rounds a tenant's deletion takes before it gives up: a WABA attached
/// while one runs is disconnected by the next.
const DELETE_ROUNDS: usize = 5;

// ─── Inputs and answers ──────────────────────────────────────────────────

/// A key to mint, as the request carried it.
#[derive(Debug, Clone, Default)]
pub struct KeyRequest {
    /// Its scopes: at least one (duplicates are dropped).
    pub scopes: Vec<Scope>,
    /// A label for operators, at most [`MAX_NAME_CHARS`] characters.
    pub name: Option<String>,
    /// When it stops working: RFC 3339, in the future.
    pub expires_at: Option<String>,
}

/// The tenants a platform key may name, as the request carried them.
#[derive(Debug, Clone)]
pub enum TenantsInput {
    /// A text: only `"*"` (any tenant) is one.
    All(String),
    /// These tenant ids: at least one (they need not exist yet).
    Only(Vec<String>),
}

/// A key just minted: the whole key, to show once, and its record.
#[derive(Debug)]
pub struct Minted {
    /// The key, `wak_<key_id>_<secret>`. Only its digest is stored.
    pub key: MintedKey,
    /// Its record.
    pub record: ApiKeyRecord,
}

/// A WABA just attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    /// The WABA.
    pub waba_id: WabaId,
    /// Its tenant.
    pub tenant_id: TenantId,
    /// Every number Meta listed on it, now bound to the tenant.
    pub phone_number_ids: Vec<PhoneNumberId>,
}

/// A bound WABA, as the operator sees it: which tenant holds it, and its
/// numbers.
#[derive(Debug, Clone)]
pub struct HeldWaba {
    /// The binding.
    pub binding: WabaBinding,
    /// Its numbers and their connection status.
    pub numbers: Vec<NumberBinding>,
}

// ─── Validation ──────────────────────────────────────────────────────────

/// A tenant id from a request's `field`: malformed, `422 invalid_request`
/// on it.
fn tenant_id(field: &'static str, id: &str) -> Result<TenantId, ServiceError> {
    TenantId::parse(id).ok_or_else(|| ServiceError::invalid(field))
}

/// A tenant id naming a resource (a path's): a malformed one names no
/// tenant, `404 not_found`.
fn named_tenant(id: &str) -> Result<TenantId, ServiceError> {
    TenantId::parse(id).ok_or_else(ServiceError::not_found)
}

/// Whether `name` may label a tenant or a key: at most
/// [`MAX_NAME_CHARS`] characters, none of them a control character.
pub fn is_valid_name(name: &str) -> bool {
    name.chars().count() <= MAX_NAME_CHARS && !name.chars().any(char::is_control)
}

/// A label, else `422 invalid_request` on `name`.
fn name(value: Option<String>) -> Result<Option<String>, ServiceError> {
    match value {
        Some(name) if !is_valid_name(&name) => Err(ServiceError::invalid("name")),
        other => Ok(other),
    }
}

/// At least one scope, each once, in order; else `422 invalid_request` on
/// `scopes`.
fn scopes(scopes: Vec<Scope>) -> Result<Vec<Scope>, ServiceError> {
    let set: BTreeSet<Scope> = scopes.into_iter().collect();
    if set.is_empty() {
        return Err(ServiceError::invalid("scopes"));
    }
    Ok(set.into_iter().collect())
}

/// An expiry: RFC 3339, in the future; else `422 invalid_request` on
/// `expires_at`.
fn expiry(value: Option<String>) -> Result<Option<OffsetDateTime>, ServiceError> {
    let Some(value) = value else { return Ok(None) };
    let at =
        OffsetDateTime::parse(&value, &Rfc3339).map_err(|_| ServiceError::invalid("expires_at"))?;
    if at <= OffsetDateTime::now_utc() {
        return Err(ServiceError::invalid("expires_at"));
    }
    Ok(Some(at))
}

/// Validate a platform key's tenants: `"*"`, or a non-empty list of tenant
/// ids (which need not exist yet); else `422 invalid_request` on
/// `tenants`.
///
/// # Errors
///
/// Another text than `"*"`, an empty list, or a malformed id in it.
pub fn allowed_tenants(spec: TenantsInput) -> Result<AllowedTenants, ServiceError> {
    match spec {
        TenantsInput::All(star) if star == "*" => Ok(AllowedTenants::All),
        TenantsInput::All(_) => Err(ServiceError::invalid("tenants")),
        TenantsInput::Only(list) => {
            let set = list
                .iter()
                .map(|t| TenantId::parse(t).ok_or_else(|| ServiceError::invalid("tenants")))
                .collect::<Result<BTreeSet<_>, _>>()?;
            if set.is_empty() {
                return Err(ServiceError::invalid("tenants"));
            }
            Ok(AllowedTenants::Only(set.into_iter().collect()))
        }
    }
}

/// A WABA id from a request: Meta's digits ([`WabaId::new`]: 1 to
/// [`MAX_NUMERIC_ID_LEN`](meta_whatsapp_rs::core::ids::MAX_NUMERIC_ID_LEN)
/// ASCII digits), not starting with `0` (D4 compares the exact string, so
/// `0102…` and `102…` must not both pass); else `422 invalid_request` on
/// `waba_id`.
fn requested_waba_id(id: &str) -> Result<WabaId, ServiceError> {
    if id.starts_with('0') {
        return Err(ServiceError::invalid(WabaId::FIELD));
    }
    WabaId::new(id).map_err(|_| ServiceError::invalid(WabaId::FIELD))
}

/// A WABA id from a path: only a numeric one can be bound, so any other
/// is `404 not_found`, before anything is read.
fn path_waba_id(id: &str) -> Result<WabaId, ServiceError> {
    WabaId::new(id).map_err(|_| ServiceError::not_found())
}

// ─── Minting ─────────────────────────────────────────────────────────────

/// Why a key could not be minted.
#[derive(Debug, thiserror::Error)]
pub enum MintError {
    /// The operating system's random number generator failed.
    #[error("the random number generator failed")]
    Random,
    /// Three drawn key ids were all taken.
    #[error("no free key id after three draws")]
    Collision,
    /// The store failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
}

impl From<MintError> for ServiceError {
    fn from(error: MintError) -> Self {
        match error {
            MintError::Random | MintError::Collision => ServiceError::internal(),
            MintError::Storage(storage) => storage.into(),
        }
    }
}

/// Mint and store a key for `owner`, with no capability: what the admin
/// operations ([`Admin::mint_tenant_key`], [`Admin::mint_platform_key`])
/// do once they validated their request, and what the command line does
/// on the database directly (the first admin key). Returns the key (to
/// show once) and its record.
///
/// # Errors
///
/// The random number generator or the store failed, or three drawn key
/// ids were all taken.
pub async fn mint(
    store: &dyn RecordStore,
    owner: KeyOwner,
    scopes: Vec<Scope>,
    name: String,
    expires_at: Option<OffsetDateTime>,
) -> Result<(MintedKey, ApiKeyRecord), MintError> {
    // 96 random bits: a collision is not expected, but never fatal.
    for _ in 0..3 {
        let minted = MintedKey::generate().map_err(|_| MintError::Random)?;
        let new = NewApiKey {
            key_id: minted.key_id().to_owned(),
            secret_sha256: minted.digest(),
            owner: owner.clone(),
            scopes: scopes.clone(),
            name: name.clone(),
            expires_at,
        };
        if let Some(record) = store.insert_key(&new).await? {
            return Ok((minted, record));
        }
    }
    Err(MintError::Collision)
}

// ─── The operations ──────────────────────────────────────────────────────

/// The admin operations, for one admin request: an [`AdminCaller`] its
/// [`Authorizer`] made, over that [`Authorizer`]'s records, vault and
/// tokenless Graph client. Cheap: a few references.
pub struct Admin<'a> {
    authz: &'a Authorizer,
    caller: &'a AdminCaller,
    observer: &'a dyn Observer,
    /// The deployment's signup, whose Solution Partner mode revokes the
    /// credit line of a deleted tenant's WABAs (decision D75).
    signups: Option<&'a Signups>,
}

impl std::fmt::Debug for Admin<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Admin")
            .field("caller", self.caller)
            .finish_non_exhaustive()
    }
}

impl<'a> Admin<'a> {
    /// The operations for `admin`, on `authz`'s records and vault, once
    /// `authz` admits `admin` ([`Authorizer::admit_admin`]); failed Graph
    /// calls are reported to nobody until [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `admin` another [`Authorizer`] made: `403 forbidden`, logged at
    /// `warn`, and nothing is read or written.
    pub fn new(authz: &'a Authorizer, admin: &'a AdminCaller) -> Result<Self, ServiceError> {
        authz.admit_admin(admin)?;
        Ok(Self {
            authz,
            caller: admin,
            observer: &(),
            signups: None,
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// Delete tenants as `signups`' deployment does (decision D75): in
    /// Solution Partner mode each WABA's credit line is revoked before its
    /// token and bindings are deleted ([`Signups::offboard`]). The unbind
    /// revokes nothing ([`Self::unbind_waba`]).
    #[must_use]
    pub fn offboarded_by(mut self, signups: &'a Signups) -> Self {
        self.signups = Some(signups);
        self
    }

    fn records(&self) -> &'a dyn RecordStore {
        self.authz.records()
    }

    /// Log the operator's change.
    fn audit(&self, action: &'static str, subject: Subject<'_>) {
        audit(action, self.caller.key_id(), &subject);
    }

    /// `api`, the error of a failed Graph call (`error`): reported to the
    /// observer when Meta answered with a Graph error.
    fn counted(&self, api: ServiceError, error: &Error) -> ServiceError {
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    // ─── Tenants ─────────────────────────────────────────────────────────

    /// Create the tenant `id` (the integrator's id), labelled `name`.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `id` (malformed) or `name`; `409
    /// tenant_exists`; the store's failure.
    pub async fn create_tenant(
        &self,
        id: &str,
        name: Option<String>,
    ) -> Result<Tenant, ServiceError> {
        let id = tenant_id("id", id)?;
        let name = self::name(name)?.unwrap_or_default();
        match self.records().create_tenant(&id, &name).await? {
            Some(tenant) => {
                self.audit("tenant_created", tenant_subject(&tenant.id));
                Ok(tenant)
            }
            // Taken (docs/design/server.md, section 5.2).
            None => Err(ServiceError::new(ErrorCode::TENANT_EXISTS)),
        }
    }

    /// A page of tenants, in id order.
    ///
    /// # Errors
    ///
    /// The store's failure.
    pub async fn tenants(&self, page: &PageRequest) -> Result<Listing<Tenant>, ServiceError> {
        Ok(self.records().tenants(page).await?)
    }

    /// The tenant `id`.
    ///
    /// # Errors
    ///
    /// `404 not_found` (a malformed id names none); the store's failure.
    pub async fn tenant(&self, id: &str) -> Result<Tenant, ServiceError> {
        let id = named_tenant(id)?;
        self.records()
            .tenant(&id)
            .await?
            .ok_or_else(ServiceError::not_found)
    }

    /// The OTP settings of the tenant `id` (roadmap M3c, decision D8).
    ///
    /// # Errors
    ///
    /// `404 not_found`: no such tenant, or it has no settings of its own
    /// (it borrows the default OTP tenant's, if the deployment names one).
    pub async fn otp_settings(&self, id: &str) -> Result<OtpSettings, ServiceError> {
        let id = named_tenant(id)?;
        self.records()
            .otp_settings(&id)
            .await?
            .ok_or_else(ServiceError::not_found)
    }

    /// Set the OTP settings of the tenant `id` (roadmap M3c, decision D8):
    /// the number its codes are sent from, which must be one of the
    /// tenant's own numbers, the approved authentication template and its
    /// language, and the library's limits (unset ones are the library's
    /// defaults). Replaces the tenant's settings whole; audited
    /// (`otp_settings_set`). Codes in flight sent from another number stop
    /// verifying: a code is bound to its sending number.
    ///
    /// # Errors
    ///
    /// `404 not_found` (no such tenant), before the body's fields. `422
    /// invalid_request` on `phone_number_id` (not an id, or not one of the
    /// tenant's numbers), `template.name`, `template.language`,
    /// `code_length`, `ttl_seconds`, `max_attempts`,
    /// `resend_cooldown_seconds` or `issue_limit`; the store's failure.
    pub async fn set_otp_settings(
        &self,
        id: &str,
        input: OtpSettingsInput,
    ) -> Result<OtpSettings, ServiceError> {
        let id = named_tenant(id)?;
        if self.records().tenant(&id).await?.is_none() {
            return Err(ServiceError::not_found());
        }
        let settings = input.settings()?;
        // Decision D8: a tenant's codes go out from one of its own numbers
        // (or, with no settings, from the default OTP tenant's own).
        let held = self
            .records()
            .number(&settings.phone_number_id)
            .await?
            .is_some_and(|number| number.tenant_id == id);
        if !held {
            return Err(ServiceError::invalid("phone_number_id"));
        }
        if !self
            .records()
            .set_otp_settings(&id, Some(&settings))
            .await?
        {
            return Err(ServiceError::not_found());
        }
        self.audit("otp_settings_set", tenant_subject(&id));
        Ok(settings)
    }

    /// Clear the OTP settings of the tenant `id`: from now on it borrows the
    /// default OTP tenant's, if the deployment names one, else its OTP
    /// requests are `409 otp_not_configured`. Audited
    /// (`otp_settings_cleared`).
    ///
    /// # Errors
    ///
    /// `404 not_found`: no such tenant. The store's failure.
    pub async fn clear_otp_settings(&self, id: &str) -> Result<(), ServiceError> {
        let id = named_tenant(id)?;
        if !self.records().set_otp_settings(&id, None).await? {
            return Err(ServiceError::not_found());
        }
        self.audit("otp_settings_cleared", tenant_subject(&id));
        Ok(())
    }

    /// Rename (`name`), suspend or reactivate (`status`) the tenant `id`.
    ///
    /// # Errors
    ///
    /// `404 not_found`; `422 invalid_request` on `name`; the store's
    /// failure.
    pub async fn update_tenant(
        &self,
        id: &str,
        name: Option<String>,
        status: Option<TenantStatus>,
    ) -> Result<Tenant, ServiceError> {
        let id = named_tenant(id)?;
        let name = self::name(name)?;
        let tenant = self
            .records()
            .update_tenant(&id, name.as_deref(), status)
            .await?
            .ok_or_else(ServiceError::not_found)?;
        let action = match status {
            Some(TenantStatus::Suspended) => "tenant_suspended",
            Some(TenantStatus::Active) => "tenant_activated",
            None => "tenant_renamed",
        };
        self.audit(action, tenant_subject(&id));
        Ok(tenant)
    }

    /// Delete the tenant `id`: disconnect every WABA it has (unsubscribe
    /// the app with its token, then delete the token and the bindings, as
    /// a tenant's own disconnection does; in Solution Partner mode
    /// ([`Self::offboarded_by`], decision D75), the WABA's credit line
    /// revoked between the two, audited `credit_line_revoked`, and a
    /// revocation that fails deletes nothing and is the answer, so a
    /// deleted tenant's WABAs never stay funded by the partner), then
    /// delete it and its keys
    /// (its events and idempotency records go with it, in the backend).
    /// Stops at the first WABA that cannot be disconnected; the tenant
    /// stays. A WABA attached while this runs is disconnected by the next
    /// round, and one that moved since its round listed it is skipped,
    /// never answered for.
    ///
    /// # Errors
    ///
    /// `404 not_found`; a WABA's failure (no usable token, Meta's refusal
    /// or failure to unsubscribe, with Meta's `details`; a revocation's,
    /// `step: revoke_credit_line`); `500 internal`, retryable, when every
    /// round found WABAs; the store's failure.
    pub async fn delete_tenant(&self, id: &str) -> Result<(), ServiceError> {
        let id = named_tenant(id)?;
        for _ in 0..DELETE_ROUNDS {
            if self.records().tenant(&id).await?.is_none() {
                return Err(ServiceError::not_found());
            }
            let wabas = self
                .records()
                .wabas(
                    &id,
                    &PageRequest {
                        after: None,
                        limit: MAX_PAGE_SIZE,
                    },
                )
                .await?;
            for binding in &wabas.items {
                // The listing may be stale (each WABA before this one cost
                // a call to Meta): a WABA unbound since, and maybe bound to
                // another tenant, is skipped, never answered for; the next
                // round lists what the tenant holds now.
                let owned = match self.authz.waba_for_admin(self.caller, binding).await {
                    Ok(owned) => owned,
                    Err(error) if self.still_listed(binding).await? => return Err(error),
                    Err(_) => continue,
                };
                if !owned.still_bound(self.authz).await? {
                    continue;
                }
                let unsubscribed = owned
                    .client()
                    .waba(owned.waba_id().clone())
                    .unsubscribe_app()
                    .await;
                if let Err(error) = unsubscribed {
                    let api = owned.failed(self.authz, &error).await;
                    return Err(self.counted(api, &error).with_details(&error));
                }
                let subject = Subject {
                    tenant: Some(id.as_str()),
                    waba_id: Some(binding.waba_id.as_str()),
                    ..Subject::default()
                };
                if let Some(signups) = self.signups {
                    // Revoked, then the token and the bindings deleted
                    // (decision D75); nothing deleted when it fails.
                    match signups.offboard(self.authz, &owned, self.observer).await {
                        Ok(true) => {
                            self.audit("credit_line_revoked", subject);
                            self.audit("waba_disconnected", subject);
                            continue;
                        }
                        Ok(false) => {}
                        // Moved meanwhile: the next round sees it as it is.
                        Err(error)
                            if error.error_code() == ErrorCode::STORAGE_UNAVAILABLE
                                && !self.still_listed(binding).await? =>
                        {
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                }
                if !owned.forget(self.authz).await? {
                    // Attached again (or its token rotated) meanwhile: the
                    // next round sees it as it is now.
                    continue;
                }
                self.audit("waba_disconnected", subject);
            }
            match self.records().delete_tenant(&id).await? {
                DeleteTenantOutcome::Deleted => {
                    self.audit("tenant_deleted", tenant_subject(&id));
                    return Ok(());
                }
                DeleteTenantOutcome::NotFound => return Err(ServiceError::not_found()),
                DeleteTenantOutcome::HasWabas => {}
            }
        }
        tracing::warn!("tenant deletion kept finding WABAs; giving up");
        Err(ServiceError::internal().retryable(true))
    }

    /// Whether `binding`, a listing's entry, is still its WABA's binding:
    /// read again when a tenant's deletion could not act on it, to tell a
    /// WABA that moved (skipped) from one that failed (answered).
    async fn still_listed(&self, binding: &WabaBinding) -> Result<bool, ServiceError> {
        Ok(self
            .records()
            .waba(&binding.waba_id)
            .await?
            .is_some_and(|now| now.epoch() == binding.epoch()))
    }

    // ─── Keys ────────────────────────────────────────────────────────────

    /// Mint a key for the tenant `tenant`.
    ///
    /// # Errors
    ///
    /// `404 not_found` (no such tenant); `422 invalid_request` on
    /// `scopes`, `name` or `expires_at`; `500 internal` when no key could
    /// be drawn; the store's failure.
    pub async fn mint_tenant_key(
        &self,
        tenant: &str,
        request: KeyRequest,
    ) -> Result<Minted, ServiceError> {
        let id = named_tenant(tenant)?;
        let scopes = scopes(request.scopes)?;
        let name = name(request.name)?.unwrap_or_default();
        let expires_at = expiry(request.expires_at)?;
        if self.records().tenant(&id).await?.is_none() {
            return Err(ServiceError::not_found());
        }
        let (key, record) = mint(
            self.records(),
            KeyOwner::Tenant(id.clone()),
            scopes,
            name,
            expires_at,
        )
        .await?;
        self.audit(
            "tenant_key_minted",
            Subject {
                tenant: Some(id.as_str()),
                key_id: Some(key.key_id()),
                ..Subject::default()
            },
        );
        Ok(Minted { key, record })
    }

    /// A page of the tenant `tenant`'s keys, revoked ones included.
    ///
    /// # Errors
    ///
    /// `404 not_found` (no such tenant); the store's failure.
    pub async fn tenant_keys(
        &self,
        tenant: &str,
        page: &PageRequest,
    ) -> Result<Listing<ApiKeyRecord>, ServiceError> {
        let id = named_tenant(tenant)?;
        if self.records().tenant(&id).await?.is_none() {
            return Err(ServiceError::not_found());
        }
        Ok(self.records().keys(&KeyScope::Tenant(id), page).await?)
    }

    /// Revoke the tenant `tenant`'s key `key_id` (already revoked: done).
    /// Effective at once, on every replica: keys are not cached.
    ///
    /// # Errors
    ///
    /// `404 not_found` (no such key for this tenant); the store's failure.
    pub async fn revoke_tenant_key(&self, tenant: &str, key_id: &str) -> Result<(), ServiceError> {
        let id = named_tenant(tenant)?;
        if !self
            .records()
            .revoke_key(&KeyScope::Tenant(id.clone()), key_id)
            .await?
        {
            return Err(ServiceError::not_found());
        }
        self.audit(
            "tenant_key_revoked",
            Subject {
                tenant: Some(id.as_str()),
                key_id: Some(key_id),
                ..Subject::default()
            },
        );
        Ok(())
    }

    /// Mint a platform key for `tenants`.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `tenants`, `scopes`, `name` or
    /// `expires_at`; `500 internal` when no key could be drawn; the
    /// store's failure.
    pub async fn mint_platform_key(
        &self,
        tenants: TenantsInput,
        request: KeyRequest,
    ) -> Result<Minted, ServiceError> {
        let allowed = allowed_tenants(tenants)?;
        let scopes = scopes(request.scopes)?;
        let name = name(request.name)?.unwrap_or_default();
        let expires_at = expiry(request.expires_at)?;
        let (key, record) = mint(
            self.records(),
            KeyOwner::Platform(allowed),
            scopes,
            name,
            expires_at,
        )
        .await?;
        self.audit("platform_key_minted", key_subject(key.key_id()));
        Ok(Minted { key, record })
    }

    /// A page of platform keys, revoked ones included.
    ///
    /// # Errors
    ///
    /// The store's failure.
    pub async fn platform_keys(
        &self,
        page: &PageRequest,
    ) -> Result<Listing<ApiKeyRecord>, ServiceError> {
        Ok(self.records().keys(&KeyScope::Platform, page).await?)
    }

    /// Revoke the platform key `key_id` (already revoked: done).
    ///
    /// # Errors
    ///
    /// `404 not_found`; the store's failure.
    pub async fn revoke_platform_key(&self, key_id: &str) -> Result<(), ServiceError> {
        if !self
            .records()
            .revoke_key(&KeyScope::Platform, key_id)
            .await?
        {
            return Err(ServiceError::not_found());
        }
        self.audit("platform_key_revoked", key_subject(key_id));
        Ok(())
    }

    // ─── WABAs ───────────────────────────────────────────────────────────

    /// Attach one of the platform's own WABAs, `waba_id`, to the tenant
    /// `tenant`, with `token` (a system user's, with access to it). In
    /// this order, which decision D4 and the port rule of [`crate::store`]
    /// fix:
    ///
    /// 1. D4 on the claimed id, before Meta is asked anything: a WABA
    ///    another tenant holds is `409 waba_owned_by_another_tenant`.
    /// 2. Every number Meta lists on the WABA, with `token` (so no id is
    ///    bound on the caller's word), at most [`MAX_WABA_NUMBERS`] (past
    ///    it, `502 upstream`). Meta refusing `token` here is the request's
    ///    to fix: `422 invalid_request` on `token`, nothing bound or
    ///    stored.
    /// 3. Bind the WABA and those numbers to the tenant, D4 again,
    ///    atomically with the binding (another tenant's since step 1: `409
    ///    waba_owned_by_another_tenant`, nothing stored). Binding before
    ///    storing: a store first would overwrite the holder's token before
    ///    D4 refused.
    /// 4. Store the token (`Authorizer::store_token`: binds, stores,
    ///    then confirms; moved meanwhile, its own write taken back and
    ///    `503`, retryable). Audited (`waba_attached`) once stored. Then
    ///    `number_connected` for each of its numbers, for the binding's
    ///    generation (roadmap M2d, [`crate::number_events`]): not recorded
    ///    (the outbox failing), `503`, the WABA staying attached; a repeat
    ///    is a new generation, which records them again.
    /// 5. Subscribe the app to the WABA's webhooks with the token. Its
    ///    failure carries `step: subscribe_app` and `resumable: true`: the
    ///    WABA stays attached, and repeating the call finishes it. Meta
    ///    refusing the token now is a stored token's `190` (`409
    ///    reconnect_required`, the WABA's numbers marked so).
    ///
    /// Meta's errors keep their `details`.
    ///
    /// # Errors
    ///
    /// As the steps say; `404 not_found` (no such tenant, or deleted
    /// during the attach); `422 invalid_request` on `waba_id` (not Meta's
    /// digits) or `token` (blank); the store's failure.
    pub async fn attach_waba(
        &self,
        tenant: &str,
        waba_id: &str,
        token: AccessToken,
    ) -> Result<Attached, ServiceError> {
        let tenant = named_tenant(tenant)?;
        let waba_id = requested_waba_id(waba_id)?;
        if token.expose_secret().trim().is_empty() {
            return Err(ServiceError::invalid("token"));
        }
        if self.records().tenant(&tenant).await?.is_none() {
            return Err(ServiceError::not_found());
        }
        // D4 on the claimed id, before Meta is asked anything.
        if self
            .records()
            .waba(&waba_id)
            .await?
            .is_some_and(|w| w.tenant_id != tenant)
        {
            return Err(ServiceError::new(ErrorCode::WABA_OWNED_BY_ANOTHER_TENANT));
        }
        let waba = self
            .authz
            .client()
            .with_token(token.clone())
            .waba(waba_id.clone());
        let meta_failed = |error: &Error| {
            let api = ServiceError::from_library(error).with_details(error);
            self.observer.graph_error(api.code());
            // The token is the request's: Meta refusing it is the caller's
            // input to fix, not a stored token to reconnect.
            if error.kind() == ErrorKind::Authentication {
                api.as_invalid("token")
            } else {
                api
            }
        };
        // Every number Meta lists on the WABA, with the given token; a
        // listing that never ends (a cursor per page, from a stub or a
        // fault) stops.
        let numbers: Vec<PhoneNumberId> = waba
            .phone_numbers_stream(&PhoneNumbersQuery::new())
            .take(MAX_WABA_NUMBERS + 1)
            .map_ok(|n| n.id)
            .try_collect()
            .await
            .map_err(|error| meta_failed(&error))?;
        if numbers.len() > MAX_WABA_NUMBERS {
            tracing::warn!(
                max = MAX_WABA_NUMBERS,
                "Meta listed more numbers on a WABA than attach binds"
            );
            return Err(ServiceError::new(ErrorCode::UPSTREAM));
        }
        // D4 again, atomically with the binding. Binding before storing: a
        // store first would overwrite the owner's token before D4 refused.
        match self
            .records()
            .bind_waba(&tenant, &waba_id, &numbers)
            .await?
        {
            BindOutcome::Bound => {}
            BindOutcome::OwnedByAnotherTenant => {
                return Err(ServiceError::new(ErrorCode::WABA_OWNED_BY_ANOTHER_TENANT));
            }
            // Deleted since it was read above.
            BindOutcome::NoSuchTenant => return Err(ServiceError::not_found()),
        }
        let record =
            StoredBusinessToken::new(waba_id.clone(), token).phone_number_ids(numbers.clone());
        // Binds, stores, then confirms (the port rule): a WABA moved
        // between the binding and the store is `503`, the token this
        // stored taken back.
        self.authz
            .store_token(self.caller, &tenant, &record)
            .await?;
        self.audit(
            "waba_attached",
            Subject {
                tenant: Some(tenant.as_str()),
                waba_id: Some(waba_id.as_str()),
                ..Subject::default()
            },
        );
        // The binding as the token's store confirmed it: its numbers are
        // connected, for this generation (roadmap M2d).
        if let Some(binding) = self
            .records()
            .waba(&waba_id)
            .await?
            .filter(|binding| binding.tenant_id == tenant)
        {
            self.authz
                .record_number_event(&binding.epoch(), NumberEvent::Connected)
                .await?;
        }
        // Subscribe the app to the WABA's webhooks, as onboarding does
        // after storing the token: a WABA the app is not subscribed to
        // delivers no webhook, and disconnecting unsubscribes it
        // (docs/design/server.md, section 3.4), so attaching again must
        // subscribe again. Safe to repeat: after a refusal the WABA stays
        // attached, and repeating the attach finishes it, which the answer
        // says (`step`, `resumable`). The token is stored by now: Meta
        // refusing it is a stored token's `190`, `409 reconnect_required`
        // with the WABA's numbers marked so, as on every route, not the
        // `422` of a token refused before anything was bound.
        if let Err(error) = waba.subscribe_app(None).await {
            let error = error.in_step(steps::SUBSCRIBE_APP);
            let api = self
                .authz
                .graph_failed_for_admin(self.caller, &waba_id, &error)
                .await;
            return Err(self
                .counted(api, &error)
                .with_details(&error)
                .resumable(true));
        }
        Ok(Attached {
            waba_id,
            tenant_id: tenant,
            phone_number_ids: numbers,
        })
    }

    /// Which tenant holds the WABA `waba_id`, and its numbers: what an
    /// operator checks before an unbind (decision D4).
    ///
    /// # Errors
    ///
    /// `404 not_found` (not bound, or not a numeric id: refused before
    /// anything is read); the store's failure.
    pub async fn waba(&self, waba_id: &str) -> Result<HeldWaba, ServiceError> {
        let waba_id = path_waba_id(waba_id)?;
        let binding = self
            .records()
            .waba(&waba_id)
            .await?
            .ok_or_else(ServiceError::not_found)?;
        let numbers = self.records().waba_numbers(&waba_id).await?;
        Ok(HeldWaba { binding, numbers })
    }

    /// Unbind the WABA `waba_id` (decision D4's admin unbind, which lets
    /// another tenant attach it; also the way to free a WABA whose token
    /// no longer works): [`Self::unbind`] on its binding, audited
    /// (`waba_unbound`). It revokes no Solution Partner credit line, unlike
    /// a tenant's deletion (decision D75): the operator may be moving the
    /// WABA to another tenant. An operator who means to end its funding
    /// revokes explicitly (the library's
    /// `EmbeddedSignup::revoke_credit_line`).
    ///
    /// # Errors
    ///
    /// `404 not_found` (not bound, or not a numeric id: refused before
    /// anything is read); as [`Self::unbind`].
    pub async fn unbind_waba(&self, waba_id: &str) -> Result<(), ServiceError> {
        let binding = self
            .records()
            .waba(&path_waba_id(waba_id)?)
            .await?
            .ok_or_else(ServiceError::not_found)?;
        self.unbind(&binding).await?;
        self.audit(
            "waba_unbound",
            Subject {
                tenant: Some(binding.tenant_id.as_str()),
                waba_id: Some(binding.waba_id.as_str()),
                ..Subject::default()
            },
        );
        Ok(())
    }

    /// Unbind a WABA as the operator, from its `binding` as read: with its
    /// stored token, if one is usable, unsubscribe the app, at best (Meta
    /// refusing does not stop it); then record `number_disconnected`
    /// (`reason`: `operator_unbound`) for each of its numbers, while the
    /// binding still holds them (roadmap M2d); then delete the token, then
    /// the bindings, as a disconnection does. A token no tenant can reach
    /// serves nothing and widens what a database and vault key compromise
    /// exposes. Not audited: [`Self::unbind_waba`] is.
    ///
    /// # Errors
    ///
    /// `503 storage_unavailable` (retryable) when the WABA moved since
    /// `binding` was read, or its token or binding while it was being
    /// forgotten (a repeat acts on what is there now), or the store
    /// failed.
    pub async fn unbind(&self, binding: &WabaBinding) -> Result<(), ServiceError> {
        match self.authz.waba_for_admin(self.caller, binding).await {
            Ok(owned) => self.unsubscribe_and_forget(owned).await,
            // Storage down: nothing can be deleted either.
            Err(error) if error.error_code() == ErrorCode::STORAGE_UNAVAILABLE => Err(error),
            // No token, an expired one, or one that no longer decrypts.
            Err(unusable) => {
                self.authz
                    .record_number_event(
                        &binding.epoch(),
                        NumberEvent::Disconnected(DisconnectReason::OperatorUnbound),
                    )
                    .await?;
                self.authz
                    .forget_for_admin(self.caller, &binding.waba_id)
                    .await?;
                // Only once the admin was accepted and the WABA unbound: a
                // refused admin unbinds nothing.
                tracing::info!(
                    code = unusable.code(),
                    "unbound a WABA without a usable token: the app stays subscribed"
                );
                Ok(())
            }
        }
    }

    /// [`Self::unbind`] for an opened WABA.
    async fn unsubscribe_and_forget(&self, owned: OwnedWaba) -> Result<(), ServiceError> {
        // Moved since it was made: its token is not the new holder's to
        // unsubscribe with. A repeat reads it anew.
        if !owned.still_bound(self.authz).await? {
            return Err(ServiceError::from(StorageError::Busy));
        }
        let unsubscribed = owned
            .client()
            .waba(owned.waba_id().clone())
            .unsubscribe_app()
            .await;
        if let Err(error) = unsubscribed {
            let api = self.counted(owned.failed(self.authz, &error).await, &error);
            tracing::warn!(
                code = api.code(),
                "unbinding a WABA Meta did not unsubscribe the app from"
            );
        }
        self.authz
            .record_number_event(
                owned.epoch(),
                NumberEvent::Disconnected(DisconnectReason::OperatorUnbound),
            )
            .await?;
        owned.forget_or_busy(self.authz).await
    }

    // ─── Vault ───────────────────────────────────────────────────────────

    /// Re-encrypt every bound WABA's token (and its credit ledger) under
    /// the active vault key (`Authorizer::rotate_vault`), audited
    /// (`vault_rotated`). The old key can go once `failed` is empty.
    ///
    /// # Errors
    ///
    /// The service's own storage failed (a record that fails is reported
    /// in `failed`, and the walk goes on).
    pub async fn rotate_vault(&self) -> Result<VaultRotation, ServiceError> {
        let report = self.authz.rotate_vault(self.caller).await?;
        self.audit("vault_rotated", Subject::default());
        Ok(report)
    }

    /// Re-seal every webhook endpoint's secret and previous secret under
    /// the active data key (`WA_SERVER_DATA_KEY`), audited
    /// (`webhook_secrets_resealed`; roadmap M2i). The old key can go from
    /// `WA_SERVER_DATA_PREVIOUS_KEYS` once the report is
    /// [`ResealReport::complete`].
    ///
    /// # Errors
    ///
    /// `409 webhooks_not_configured` without a data key; the service's
    /// own storage failing (an endpoint that fails is reported in
    /// `failed`, and the walk goes on).
    pub async fn reseal_webhook_secrets(
        &self,
        endpoints: &WebhookEndpoints,
    ) -> Result<ResealReport, ServiceError> {
        let report = endpoints.reseal_secrets().await?;
        self.audit("webhook_secrets_resealed", Subject::default());
        Ok(report)
    }
}

/// A tenant's OTP settings as the request carried them (roadmap M3c):
/// every field named as the API names it; an unset limit is the library's
/// default ([`OtpSettings::new`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OtpSettingsInput {
    /// The sending number's id: one of the tenant's own.
    pub phone_number_id: String,
    /// The approved authentication template's name.
    pub template_name: String,
    /// Its language.
    pub template_language: String,
    /// Digits per code, 4 to 8.
    pub code_length: Option<u32>,
    /// How long a code is valid, 1 to 5400 seconds.
    pub ttl_seconds: Option<u32>,
    /// Wrong guesses allowed per code.
    pub max_attempts: Option<u32>,
    /// Seconds between two codes for one recipient and purpose.
    pub resend_cooldown_seconds: Option<u32>,
    /// Codes allowed per recipient and purpose in the window.
    pub max_issues: Option<u32>,
    /// That window, in seconds.
    pub issue_window_seconds: Option<u32>,
}

impl OtpSettingsInput {
    /// The settings, checked ([`crate::otp::check`]).
    fn settings(self) -> Result<OtpSettings, ServiceError> {
        let phone_number_id: PhoneNumberId =
            crate::request::typed_graph_id("phone_number_id", &self.phone_number_id)?;
        let mut settings =
            OtpSettings::new(phone_number_id, self.template_name, self.template_language);
        if let Some(length) = self.code_length {
            settings.code_length =
                u8::try_from(length).map_err(|_| ServiceError::invalid("code_length"))?;
        }
        if let Some(ttl) = self.ttl_seconds {
            settings.ttl_seconds = ttl;
        }
        if let Some(attempts) = self.max_attempts {
            settings.max_attempts = attempts;
        }
        if let Some(cooldown) = self.resend_cooldown_seconds {
            settings.resend_cooldown_seconds = cooldown;
        }
        if let Some(issues) = self.max_issues {
            settings.max_issues = issues;
        }
        if let Some(window) = self.issue_window_seconds {
            settings.issue_window_seconds = window;
        }
        crate::otp::check(&settings)?;
        Ok(settings)
    }
}

fn tenant_subject(tenant: &TenantId) -> Subject<'_> {
    Subject {
        tenant: Some(tenant.as_str()),
        ..Subject::default()
    }
}

fn key_subject(key_id: &str) -> Subject<'_> {
    Subject {
        key_id: Some(key_id),
        ..Subject::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WABA id from a request is 1 to 64 digits, not starting with `0`.
    /// Decisive: each clause, the length cap included.
    #[test]
    fn a_waba_id_is_metas_digits() {
        assert_eq!(requested_waba_id("1").unwrap().as_str(), "1");
        assert!(requested_waba_id(&"9".repeat(64)).is_ok());
        for bad in [
            String::new(),
            "9".repeat(65),
            "0102290129340398".to_owned(),
            "12a".to_owned(),
            " 1".to_owned(),
        ] {
            let refused = requested_waba_id(&bad).unwrap_err();
            assert_eq!(refused.field(), Some("waba_id"), "{bad:?}");
        }
    }

    /// A platform key's tenants: `"*"` or a non-empty list of valid ids.
    #[test]
    fn allowed_tenants_are_a_star_or_a_list() {
        assert_eq!(
            allowed_tenants(TenantsInput::All("*".into())).unwrap(),
            AllowedTenants::All
        );
        assert_eq!(
            allowed_tenants(TenantsInput::Only(vec!["b".into(), "a".into(), "b".into()])).unwrap(),
            AllowedTenants::Only(vec![
                TenantId::parse("a").unwrap(),
                TenantId::parse("b").unwrap()
            ])
        );
        for bad in [
            TenantsInput::All("all".into()),
            TenantsInput::Only(Vec::new()),
            TenantsInput::Only(vec!["a b".into()]),
        ] {
            assert_eq!(allowed_tenants(bad).unwrap_err().field(), Some("tenants"));
        }
    }

    /// Names, scopes and expiries are refused on their own fields.
    #[test]
    fn key_inputs_are_refused_on_their_fields() {
        assert!(is_valid_name(&"é".repeat(MAX_NAME_CHARS)));
        assert!(!is_valid_name(&"é".repeat(MAX_NAME_CHARS + 1)));
        assert!(!is_valid_name("a\tb"));
        assert_eq!(name(Some("a\nb".into())).unwrap_err().field(), Some("name"));
        assert_eq!(scopes(Vec::new()).unwrap_err().field(), Some("scopes"));
        assert_eq!(
            scopes(vec![Scope::Send, Scope::Media, Scope::Send]).unwrap(),
            [Scope::Send, Scope::Media]
        );
        for bad in ["2001-01-01T00:00:00Z", "tomorrow"] {
            assert_eq!(
                expiry(Some(bad.into())).unwrap_err().field(),
                Some("expires_at"),
                "{bad}"
            );
        }
        assert!(
            expiry(Some("2999-01-01T00:00:00Z".into()))
                .unwrap()
                .is_some()
        );
    }
}
