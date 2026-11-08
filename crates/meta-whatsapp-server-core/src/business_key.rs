//! A number's Flows business public key (roadmap M5d2, design D450 to
//! D453): the RSA public key WhatsApp encrypts a Flow data endpoint's
//! requests to, set per phone number at Meta, as services an API adapter
//! calls with the tenant's WABA token over the library's
//! `client::flows::BusinessEncryption`:
//!
//! | Operation | Graph call |
//! | --- | --- |
//! | [`BusinessKey::get`] | `GET /{pn}/whatsapp_business_encryption` |
//! | [`BusinessKey::set`] | `POST /{pn}/whatsapp_business_encryption` (urlencoded, as the guide's example) |
//!
//! **Checked before any request.** The key is parsed by the library's
//! [`Rsa2048PublicKey`]: one `PUBLIC KEY` PEM block, a 2048-bit RSA key
//! (what Meta's page asks for), never a private key, never quoted back in
//! an error. What is sent is the key in its canonical form.
//!
//! **The key must be the public half of the Flow key.** When the tenant
//! has set a Flow private key for the number's WABA
//! ([`crate::flow_key`]), the public key to set must be that key's public
//! half: otherwise every Flow of the number would be encrypted to a key
//! the data endpoint cannot open (`421`, whatever Meta's client retries).
//! A mismatch is `422 invalid_request` on `flow_key`, before any request,
//! unless the caller says it means it ([`BusinessKey::set`]'s
//! `allow_mismatch`). With no Flow key set (the integrating app may hold
//! its own, the deployment's key may serve) there is nothing to compare:
//! the key is sent, and the answer says so (`matches_flow_key: null`).
//!
//! **Audit.** `business_public_key_set`, with the tenant, the WABA, the
//! number and the key's SHA-256 ([`crate::audit::business_key_audit`]),
//! and `flow_key_override` (`true` when `allow_mismatch` let a key through
//! that is not the Flow key's public half);
//! when Meta answered, or when it may have gone through unanswered. Never
//! the key. The names are pending the owner's confirmation before the
//! first release.
//!
//! The services open on a capability their own [`Authorizer`] made, and
//! admit it first; a number of another tenant is `404` before anything is
//! read or sent.

use std::fmt;

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::flows::Rsa2048PublicKey;

use crate::audit::{Outcome, Subject, business_key_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::error::ServiceError;
use crate::observe::Observer;
use crate::request::wire;

/// The log target of this module's lines.
const LOG_TARGET: &str = "meta_whatsapp_server::api::business_key";

/// The audit action of [`BusinessKey::set`].
pub const BUSINESS_PUBLIC_KEY_SET: &str = "business_public_key_set";

/// Largest public key text accepted: the library's bound.
pub const MAX_PUBLIC_KEY_BYTES: usize = meta_whatsapp_rs::client::flows::MAX_PUBLIC_KEY_PEM_BYTES;

/// What [`BusinessKey::set`] stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessKeySet {
    /// The SHA-256 (hex) of the key sent, in canonical PEM: the digest
    /// [`crate::flow_key::FlowKeyInfo`] answers for the Flow key's public
    /// half.
    pub public_key_sha256: String,
    /// `Some(true)`: it is the public half of the tenant's Flow key for the
    /// WABA. `Some(false)`: it is not, and the caller allowed that. `None`:
    /// the tenant has no Flow key to compare with.
    pub matches_flow_key: Option<bool>,
}

/// What Meta holds for a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessKeyView {
    /// The key Meta stores, as it answered, or `None` when none is set.
    pub public_key: Option<String>,
    /// The SHA-256 (hex) of `public_key`'s canonical form, when it parses
    /// as a 2048-bit RSA public key.
    pub public_key_sha256: Option<String>,
    /// Meta's check of its signature over the key, as it wrote it
    /// (`VALID`, `MISMATCH`).
    pub signature_status: Option<String>,
    /// As [`BusinessKeySet::matches_flow_key`], for the key Meta holds:
    /// `None` when there is no key at Meta, none that parses, or no Flow
    /// key.
    pub matches_flow_key: Option<bool>,
}

/// The operations on the business public key of a number the caller's
/// tenant owns. Cheap: a few references.
pub struct BusinessKey<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    number: &'a OwnedNumber,
    observer: &'a dyn Observer,
}

impl fmt::Debug for BusinessKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BusinessKey")
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

/// `text` as the key to set: the library's [`Rsa2048PublicKey`].
///
/// # Errors
///
/// `422 invalid_request` on `business_public_key`, never quoting the text.
pub fn public_key(text: &str) -> Result<Rsa2048PublicKey, ServiceError> {
    Rsa2048PublicKey::from_pem(text).map_err(|_| ServiceError::invalid("business_public_key"))
}

impl<'a> BusinessKey<'a> {
    /// The operations on `number`, once `authz` admits it
    /// ([`Authorizer::admit_number`]) and `caller`.
    ///
    /// # Errors
    ///
    /// `number` or `caller` another [`Authorizer`] made: `403 forbidden`,
    /// and nothing is asked of Meta. A number of another tenant:
    /// `404 not_found`.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        number: &'a OwnedNumber,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_number(number)?;
        if number.tenant() != caller.tenant() {
            tracing::warn!(
                target: LOG_TARGET,
                "refused the business public key services on another tenant's number"
            );
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
            caller,
            number,
            observer: &(),
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    fn encryption(&self) -> meta_whatsapp_rs::client::flows::BusinessEncryption {
        self.number
            .client()
            .business_encryption(self.number.phone_number_id().clone())
    }

    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self.number.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api.with_details(error)
    }

    fn audit(&self, public_key_sha256: &str, flow_key_override: bool, outcome: Outcome) {
        business_key_audit(
            BUSINESS_PUBLIC_KEY_SET,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.number.waba_id().as_str()),
                phone_number_id: Some(self.number.phone_number_id().as_str()),
                ..Subject::default()
            },
            public_key_sha256,
            flow_key_override,
            outcome,
        );
    }

    /// Whether `key` is the public half of the tenant's Flow key for the
    /// number's WABA: `None` when the tenant has set none.
    async fn matches_flow_key(&self, key: &Rsa2048PublicKey) -> Result<Option<bool>, ServiceError> {
        let Some(private) = self
            .authz
            .read_flow_key_for_number(self.caller, self.number)
            .await?
        else {
            return Ok(None);
        };
        let held = private
            .public_key_pem()
            .ok()
            .and_then(|pem| Rsa2048PublicKey::from_pem(&pem).ok())
            // A stored key whose public half cannot be written is a
            // mismatch with everything, not a reason to skip the check.
            .is_some_and(|held| &held == key);
        Ok(Some(held))
    }

    /// The key Meta holds for the number, and whether it is the Flow key's
    /// public half.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`. A Flow key record
    /// that cannot be opened.
    pub async fn get(&self) -> Result<BusinessKeyView, ServiceError> {
        let stored = match self.encryption().get().await {
            Ok(stored) => stored,
            Err(error) => return Err(self.failed(&error).await),
        };
        let parsed = stored
            .business_public_key
            .as_deref()
            .and_then(|pem| Rsa2048PublicKey::from_pem(pem).ok());
        let matches_flow_key = match &parsed {
            Some(key) => self.matches_flow_key(key).await?,
            None => None,
        };
        Ok(BusinessKeyView {
            public_key_sha256: parsed.as_ref().map(Rsa2048PublicKey::sha256),
            public_key: stored.business_public_key,
            signature_status: wire(stored.business_public_key_signature_status.as_ref()),
            matches_flow_key,
        })
    }

    /// Set `key` (checked by [`public_key`]) as the number's business
    /// public key at Meta, replacing any. Audited
    /// ([`BUSINESS_PUBLIC_KEY_SET`]) when Meta answered, or may have.
    ///
    /// `allow_mismatch` lets a key that is not the Flow key's public half
    /// through (see the module).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `flow_key`: the tenant's Flow key for the
    /// WABA exists and `key` is not its public half, unless
    /// `allow_mismatch`; nothing is sent. Meta's refusal or failure, with
    /// its `details`.
    pub async fn set(
        &self,
        key: &Rsa2048PublicKey,
        allow_mismatch: bool,
    ) -> Result<BusinessKeySet, ServiceError> {
        let matches_flow_key = self.matches_flow_key(key).await?;
        if matches_flow_key == Some(false) && !allow_mismatch {
            tracing::info!(
                target: LOG_TARGET,
                "refused a business public key that is not the Flow key's public half"
            );
            return Err(ServiceError::invalid("flow_key"));
        }
        let digest = key.sha256();
        // The caller let a key through that is not the Flow key's public
        // half: a deliberate act, audited as such.
        let overridden = matches_flow_key == Some(false);
        match self.encryption().set_public_key(&key.to_pem()).await {
            Ok(()) => {
                self.audit(&digest, overridden, Outcome::Answered);
                Ok(BusinessKeySet {
                    public_key_sha256: digest,
                    matches_flow_key,
                })
            }
            Err(error) => {
                let failed = self.failed(&error).await;
                if failed.may_have_been_sent() {
                    self.audit(&digest, overridden, Outcome::Unanswered);
                }
                Err(failed)
            }
        }
    }
}
