//! A tenant's Flow private key (roadmap M5d2, design D370 to D374): the
//! business RSA key the Flows data endpoint decrypts a WABA's requests
//! with ([`crate::flows_endpoint`]), set by the tenant that owns the WABA,
//! as services an API adapter calls. Nothing is asked of Meta.
//!
//! | Operation | Effect |
//! | --- | --- |
//! | [`FlowKey::set`] | the key sealed in the token vault (`wa.token`, `flow-key/<waba_id>`), owned by the caller's tenant; replaces any (a rotation) |
//! | [`FlowKey::get`] | whether a key is set, and its **public** half and fingerprint |
//! | [`FlowKey::delete`] | the record removed |
//!
//! **The private key never comes back.** It arrives in one request's body
//! ([`private_key`]: PEM text, an unencrypted 2048-bit RSA key, PKCS#8 or
//! PKCS#1, what Meta's encryption page asks for), becomes the library's
//! `SecretBytes` (zeroed on drop, `Debug` redacted), is parsed by the
//! library's `FlowEndpointKey` before anything is stored, and is sealed
//! by the vault under the active vault key. No answer, error, log, audit
//! event, idempotency record, event or metric holds it, or anything
//! derived from it but its public half: what the tenant sets as the
//! business public key of each number a Flow is sent from (parity row
//! 66).
//!
//! **The key is the tenant's.** The vault record names the tenant that
//! set it (`Authorizer::store_flow_key`): another tenant
//! reads none, and a WABA that changed hands never serves its previous
//! holder's key. The data endpoint reads the tenant of the request's WABA
//! first and that tenant's key only
//! ([`crate::flows_endpoint::VaultFlowKeys`]).
//!
//! **Rotating.** Replace the key with `set`, then set the new public key
//! on each number at Meta: until both are done, requests encrypted to the
//! old public key are answered `421` (Meta's client then downloads the
//! public key again and retries, `implementingyourflowendpoint`). The
//! service keeps one key per WABA and no overlap.
//!
//! **Audit.** `flow_key_set` and `flow_key_deleted`, with the tenant, the
//! WABA and the key's public id ([`crate::audit::tenant_audit`]); no key
//! material, no fingerprint. Pending the owner's confirmation before the
//! first release.

use meta_whatsapp_rs::client::flows::endpoint::FlowEndpointKey;
use meta_whatsapp_rs::core::secret::SecretBytes;
use sha2::{Digest, Sha256};

use crate::audit::{Subject, tenant_audit};
use crate::authz::{Authorizer, Caller, OwnedWaba};
use crate::error::ServiceError;

/// Largest private key text accepted: a 2048-bit RSA key in PEM is about
/// 1.7 KiB (PKCS#8) and the bound only keeps a body a key.
pub const MAX_PRIVATE_KEY_BYTES: usize = 16 * 1024;

/// The public half of a Flow key: what the tenant sets as each number's
/// business public key, and a fingerprint to tell two keys apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowKeyInfo {
    /// The public key as PEM (`-----BEGIN PUBLIC KEY-----`).
    pub public_key: String,
    /// The SHA-256 of [`Self::public_key`]'s text, in lowercase hex.
    pub public_key_sha256: String,
}

impl FlowKeyInfo {
    fn of(key: &FlowEndpointKey) -> Result<Self, ServiceError> {
        let public_key = key
            .public_key_pem()
            .map_err(|_| ServiceError::invalid("private_key"))?;
        Ok(Self {
            public_key_sha256: hex::encode(Sha256::digest(public_key.as_bytes())),
            public_key,
        })
    }
}

/// Check `text` as a Flow private key and hold it as a secret: PEM, an
/// unencrypted 2048-bit RSA private key (PKCS#8 or PKCS#1), at most
/// [`MAX_PRIVATE_KEY_BYTES`]. The key's public half comes with it, for
/// the request's idempotency fingerprint and its answer.
///
/// # Errors
///
/// Anything else, a password-protected key included: `422 invalid_request`
/// on `private_key`, never quoting the text.
pub fn private_key(text: String) -> Result<(SecretBytes, FlowKeyInfo), ServiceError> {
    // Moved into the secret type whatever the outcome, so that the
    // request's copy is wiped on drop (a refused key too).
    let secret = SecretBytes::new(text.into_bytes());
    let text = std::str::from_utf8(secret.expose_secret())
        .map_err(|_| ServiceError::invalid("private_key"))?;
    if text.len() > MAX_PRIVATE_KEY_BYTES {
        return Err(ServiceError::invalid("private_key"));
    }
    let key = FlowEndpointKey::from_pem(text).map_err(|_| ServiceError::invalid("private_key"))?;
    let info = FlowKeyInfo::of(&key)?;
    Ok((secret, info))
}

/// The Flow key operations on a WABA the caller's tenant owns: an
/// [`OwnedWaba`] its [`Authorizer`] made, and admits. Cheap: a few
/// references.
pub struct FlowKey<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    waba: &'a OwnedWaba,
}

impl std::fmt::Debug for FlowKey<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowKey")
            .field("caller", self.caller)
            .field("waba", self.waba)
            .finish_non_exhaustive()
    }
}

impl<'a> FlowKey<'a> {
    /// The operations on `waba` for `caller`, once `authz` admits both
    /// and `waba` was made from a binding of `caller`'s tenant.
    ///
    /// # Errors
    ///
    /// `caller` or `waba` another [`Authorizer`] made: `403 forbidden`.
    /// `waba` another tenant's: `404 not_found`. Either way before
    /// anything is read or stored.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        waba: &'a OwnedWaba,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_waba(waba)?;
        if waba.tenant() != caller.tenant() {
            tracing::warn!("refused the Flow key services on another tenant's WABA");
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
            caller,
            waba,
        })
    }

    fn audit(&self, action: &'static str) {
        tenant_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.waba.waba_id().as_str()),
                ..Subject::default()
            },
        );
    }

    /// Set `key` (checked by [`private_key`]) as the WABA's Flow key,
    /// replacing any: the public half of what is stored. Audited
    /// (`flow_key_set`) once stored.
    ///
    /// # Errors
    ///
    /// The vault failing.
    pub async fn set(&self, key: &SecretBytes) -> Result<FlowKeyInfo, ServiceError> {
        self.authz
            .store_flow_key(self.caller, self.waba, key)
            .await?;
        self.audit("flow_key_set");
        // Read back: what the data endpoint will use is what is answered.
        self.get()
            .await?
            .ok_or_else(|| ServiceError::from(meta_whatsapp_rs::core::error::StorageError::Busy))
    }

    /// The WABA's Flow key, as its public half, or `None` when the
    /// caller's tenant has set none.
    ///
    /// # Errors
    ///
    /// A record that cannot be opened (sealed under a vault key no longer
    /// configured): the vault's error. The vault failing.
    pub async fn get(&self) -> Result<Option<FlowKeyInfo>, ServiceError> {
        match self.authz.read_flow_key(self.caller, self.waba).await? {
            Some(key) => FlowKeyInfo::of(&key).map(Some),
            None => Ok(None),
        }
    }

    /// Delete the WABA's Flow key: whether there was one. Audited
    /// (`flow_key_deleted`) when there was. The data endpoint then uses
    /// the deployment's key if the deployment allows it, else answers
    /// `404`.
    ///
    /// # Errors
    ///
    /// The vault failing.
    pub async fn delete(&self) -> Result<bool, ServiceError> {
        let removed = self.authz.delete_flow_key(self.caller, self.waba).await?;
        if removed {
            self.audit("flow_key_deleted");
        }
        Ok(removed)
    }
}
