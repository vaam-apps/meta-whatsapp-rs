//! One-time passcodes as services (docs/design/server.md, section 4.7;
//! roadmap M3c): a code sent with a tenant's authentication template, and
//! a typed code checked, both through the library's `OtpService`, which
//! keeps every rule of its own: codes from the OS random number generator,
//! stored only as keyed hashes in the backend's key/value store (`wa.otp`,
//! `wa.otp.rate`), compared in constant time, an attempt counted before
//! the comparison, a code's expiry, a resend cooldown and an issue limit
//! per recipient and purpose. This module adds nothing to them; it decides
//! **whose** codes they are and **which number** sends them:
//!
//! - **The namespace is the calling tenant's id**, always
//!   (`OtpConfig::namespace`): codes, cooldowns and issue limits never
//!   cross tenants, even when two tenants send from one number (the
//!   platform's). A tenant's code checked by another tenant is `not_found`,
//!   with nothing else said, and counts no attempt against it.
//! - **The sending number** (decision D8: per tenant, the platform's by
//!   default) comes from the tenant's own OTP settings
//!   ([`OtpSettings`], set by the operator,
//!   [`crate::admin::Admin::set_otp_settings`]), which may only name one of
//!   the tenant's own numbers; a tenant without settings borrows those of
//!   the deployment's default OTP tenant ([`OtpDeployment::default_tenant`],
//!   `WA_SERVER_OTP_DEFAULT_TENANT`), the platform's number. Either way the
//!   number is opened for the tenant whose settings name it, through the
//!   authorization order (steps 4 and 5: bound to that tenant, connected,
//!   its token read last); a number no longer that tenant's, or no settings
//!   at all, is `409 otp_not_configured`, before any code is made.
//! - **What a caller sends is never a setting**: the request carries a
//!   phone number (strict E.164 with its `+`; a BSUID is refused, Meta
//!   cannot deliver an authentication template to one, `131062`), a
//!   purpose (1 to 64 of `[A-Za-z0-9._:-]`, `login` when absent) and, to
//!   check, the code; never a namespace, a number or a template.
//! - **The code reaches no answer, log, error or event**: it lives in the
//!   library's call and the request body sent to Meta. A failed send keeps
//!   Meta's code, subcode and trace id, never its text or `details` (a
//!   template error could quote a parameter: the code); `Debug` of every
//!   type here redacts the phone number and the code.
//! - The rate limits per tenant are the core's buckets
//!   ([`crate::ratelimit`]: `otp_issue`, `otp_verify`); per recipient, the
//!   library's cooldown and issue limit, under the tenant's namespace: the
//!   service never turns the issue limit off.
//!
//! An issue takes an `Idempotency-Key` (design section 5.4), over a keyed
//! fingerprint ([`crate::idempotency::Fingerprint::issue_otp`]): the body
//! names a phone number, which design section 6 keeps only as keyed hashes.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, Mac};
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::authentication::{
    IssueLimit, IssueOutcome, OtpConfig, OtpPepper, OtpService, OtpTemplate, VerifyOutcome,
};
use meta_whatsapp_rs::core::clock::Clock;
use meta_whatsapp_rs::core::error::CryptoError;
use meta_whatsapp_rs::core::recipient::Recipient;
use meta_whatsapp_rs::core::secret::SecretBytes;
use meta_whatsapp_rs::core::store::KvStore;
use sha2::Sha256;
use time::OffsetDateTime;

use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::error::{ErrorCode, ServiceError};
use crate::messages::is_e164;
use crate::model::{OtpSettings, TenantId};
use crate::observe::Observer;

/// The purpose of a request that names none.
pub const DEFAULT_PURPOSE: &str = "login";

/// Longest purpose, in bytes.
pub const MAX_PURPOSE_LEN: usize = 64;

/// Longest code a check takes, in bytes: the library makes 4 to 8 digits,
/// and Meta's limit for an authentication template's parameter is 15
/// characters; anything longer never matches, and is refused before the
/// store is read.
pub const MAX_CODE_LEN: usize = 15;

/// Most codes an issue limit may allow per window: the library's issue log
/// keeps one time per code in the window.
pub const MAX_ISSUES: u32 = 1000;

/// HMAC label of the key the issue fingerprint is keyed with, derived from
/// the OTP pepper (roadmap M3c): a stored name of the idempotency records,
/// pending the owner's confirmation (docs/architecture.md, § Stable
/// identifiers). Changed, a repeat of an issue made before the change
/// within the records' TTL is `422 idempotency_key_reused`, never a second
/// code.
const FINGERPRINT_LABEL: &[u8] = b"meta-whatsapp-server/otp-fingerprint/v1";

/// The OTP pepper (`WA_OTP_PEPPER`, at least 32 bytes): the library's
/// `OtpPepper`, which keys every code's and every store key's HMAC, and the
/// key the service derives from it for an issue's idempotency fingerprint.
/// `Debug` shows neither. Rotating the pepper invalidates the codes in
/// flight and resets every issue limit.
#[derive(Clone)]
pub struct OtpKeys {
    pepper: OtpPepper,
    fingerprint: SecretBytes,
}

impl fmt::Debug for OtpKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OtpKeys([REDACTED])")
    }
}

impl OtpKeys {
    /// The keys of `pepper`.
    ///
    /// # Errors
    ///
    /// Fewer than 32 bytes (the library's `OtpPepper::new`): a
    /// configuration error.
    pub fn new(pepper: SecretBytes) -> Result<Self, Error> {
        let fingerprint = SecretBytes::new(hmac(pepper.expose_secret(), FINGERPRINT_LABEL)?);
        Ok(Self {
            pepper: OtpPepper::from_secret(pepper)?,
            fingerprint,
        })
    }

    /// A pepper of 32 random bytes for this process: the development
    /// service on memory storage, whose codes die with it anyway.
    ///
    /// # Errors
    ///
    /// The OS random number generator failed.
    pub fn throwaway() -> Result<Self, Error> {
        let mut bytes = vec![0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::from(CryptoError::Rng))?;
        Self::new(SecretBytes::new(bytes))
    }

    /// HMAC-SHA256 of `digest` under the fingerprint key.
    pub(crate) fn fingerprint(&self, digest: &[u8; 32]) -> [u8; 32] {
        // An HMAC takes a key of any length: this cannot fail.
        hmac(self.fingerprint.expose_secret(), digest).unwrap_or([0; 32])
    }
}

fn hmac(key: &[u8], message: &[u8]) -> Result<[u8; 32], Error> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|_| Error::from(CryptoError::InvalidKey("OTP pepper")))?;
    mac.update(message);
    Ok(mac.finalize().into_bytes().into())
}

/// What the deployment sets for OTP: the pepper, and the tenant whose
/// settings a tenant without its own borrows (decision D8's default, the
/// platform's number; `None`: a tenant without settings is `409
/// otp_not_configured`).
#[derive(Debug, Clone)]
pub struct OtpDeployment {
    keys: OtpKeys,
    default_tenant: Option<TenantId>,
}

impl OtpDeployment {
    /// `keys`, and `default_tenant`'s settings for the tenants without
    /// their own.
    pub fn new(keys: OtpKeys, default_tenant: Option<TenantId>) -> Self {
        Self {
            keys,
            default_tenant,
        }
    }

    /// The pepper's keys.
    pub fn keys(&self) -> &OtpKeys {
        &self.keys
    }

    /// The default OTP tenant (`WA_SERVER_OTP_DEFAULT_TENANT`).
    pub fn default_tenant(&self) -> Option<&TenantId> {
        self.default_tenant.as_ref()
    }
}

/// The OTP services of one service: the backend's key/value store the
/// library keeps challenges in, the service's clock, and the deployment's
/// settings (`None`: OTP is not set up, and every OTP request is `409
/// otp_not_configured`). Cheap to share.
pub struct OtpCodes {
    kv: Arc<dyn KvStore>,
    clock: Arc<dyn Clock>,
    deployment: Option<OtpDeployment>,
}

impl fmt::Debug for OtpCodes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OtpCodes")
            .field("deployment", &self.deployment)
            .finish_non_exhaustive()
    }
}

impl OtpCodes {
    /// Codes kept in `kv` (the backend's: `wa.otp`, `wa.otp.rate`), timed by
    /// `clock` (the service's one clock), under `deployment`.
    pub fn new(
        kv: Arc<dyn KvStore>,
        clock: Arc<dyn Clock>,
        deployment: Option<OtpDeployment>,
    ) -> Self {
        Self {
            kv,
            clock,
            deployment,
        }
    }

    /// The deployment's settings, if OTP is set up.
    pub fn deployment(&self) -> Option<&OtpDeployment> {
        self.deployment.as_ref()
    }

    /// The OTP operations for `caller`, once `authz` admits it.
    ///
    /// # Errors
    ///
    /// `caller` another [`Authorizer`] made: `403 forbidden`. OTP not set
    /// up for the deployment: `409 otp_not_configured`.
    pub fn open<'a>(
        &'a self,
        authz: &'a Authorizer,
        caller: &'a Caller,
    ) -> Result<Otp<'a>, ServiceError> {
        authz.admit(caller)?;
        let deployment = self.deployment.as_ref().ok_or_else(not_configured)?;
        Ok(Otp {
            codes: self,
            deployment,
            authz,
            caller,
            observer: &(),
        })
    }
}

fn not_configured() -> ServiceError {
    ServiceError::new(ErrorCode::OTP_NOT_CONFIGURED)
}

/// A code to send, as the request carried it. `Debug` shows no phone
/// number.
#[derive(Clone, PartialEq, Eq)]
pub struct IssueRequest {
    /// The recipient: E.164 with its `+`.
    pub phone: String,
    /// The flow the code is for (`login` when absent).
    pub purpose: Option<String>,
}

impl fmt::Debug for IssueRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssueRequest")
            .field("purpose", &self.purpose)
            .finish_non_exhaustive()
    }
}

/// A code to check, as the request carried it. `Debug` shows neither the
/// phone number nor the code.
#[derive(Clone, PartialEq, Eq)]
pub struct VerifyRequest {
    /// The recipient the code was sent to: E.164 with its `+`.
    pub phone: String,
    /// The flow the code is for (`login` when absent).
    pub purpose: Option<String>,
    /// The code as typed, compared byte for byte.
    pub code: String,
}

impl fmt::Debug for VerifyRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifyRequest")
            .field("purpose", &self.purpose)
            .finish_non_exhaustive()
    }
}

/// What an issue did (the library's `IssueOutcome`): never the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issued {
    /// A new code was sent; the recipient's previous one for the purpose
    /// no longer verifies.
    Sent {
        /// The challenge's random id (not a secret, not the code).
        challenge_id: String,
        /// When the code stops verifying.
        expires_at: OffsetDateTime,
    },
    /// A code was sent to the recipient for the purpose less than the
    /// resend cooldown ago: nothing sent.
    CoolingDown {
        /// Seconds until a new code may be sent, rounded up.
        retry_after_seconds: u64,
    },
    /// The recipient's issue limit for the purpose is used up: nothing
    /// sent.
    RateLimited {
        /// Seconds until the oldest code in the window leaves it, rounded
        /// up.
        retry_after_seconds: u64,
    },
}

/// What a check found (the library's `VerifyOutcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checked {
    /// The code matched, and is used up.
    Verified,
    /// Wrong code; the attempt was counted.
    Invalid {
        /// Attempts left on this code.
        attempts_left: u32,
    },
    /// The code expired: send a new one.
    Expired,
    /// The code's attempts are used up, even if this one was right.
    TooManyAttempts,
    /// No code is outstanding for the recipient and purpose, for this
    /// tenant: never sent, used, long gone, or another tenant's.
    NotFound,
}

impl Checked {
    /// The answer's name: `verified`, `invalid`, `expired`,
    /// `too_many_attempts` or `not_found`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Invalid { .. } => "invalid",
            Self::Expired => "expired",
            Self::TooManyAttempts => "too_many_attempts",
            Self::NotFound => "not_found",
        }
    }
}

impl Issued {
    /// The answer's name: `sent`, `cooling_down` or `rate_limited`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sent { .. } => "sent",
            Self::CoolingDown { .. } => "cooling_down",
            Self::RateLimited { .. } => "rate_limited",
        }
    }
}

/// The OTP operations for one admitted caller ([`OtpCodes::open`]).
pub struct Otp<'a> {
    codes: &'a OtpCodes,
    deployment: &'a OtpDeployment,
    authz: &'a Authorizer,
    caller: &'a Caller,
    observer: &'a dyn Observer,
}

impl fmt::Debug for Otp<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Otp")
            .field("caller", self.caller)
            .finish_non_exhaustive()
    }
}

impl<'a> Otp<'a> {
    /// Tell `observer` (the adapter's metrics) each outcome, and each
    /// failed Graph call's code.
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// Send a code to `request.phone` for `request.purpose`, from the
    /// caller's sending number with its template (see the
    /// [module](self)): checked locally first (the phone number, the
    /// purpose), then the settings and the sending number, then the
    /// library's cooldown and issue limit, then the send.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `phone` or `purpose`, before anything is
    /// read. `409 otp_not_configured` (no settings, or the sending number
    /// no longer its holder's); the sending number's `409
    /// number_not_connected` or `reconnect_required`. Meta's refusal or
    /// failure, its code and trace id kept, its text and `details` never
    /// (a `190` marks the sending number's WABA `reconnect_required`); a
    /// timeout or a 5xx `may_have_been_sent` (the code may be on its way,
    /// and stays verifiable).
    pub async fn issue(&self, request: IssueRequest) -> Result<Issued, ServiceError> {
        let phone = phone(request.phone)?;
        let purpose = purpose(request.purpose)?;
        let (holder, settings) = self.settings().await?;
        let number = self.sender(&holder, &settings).await?;
        let service = self.service(number.client().clone(), &settings)?;
        let issued = service.issue(&Recipient::Phone(phone), &purpose).await;
        let issued = match issued {
            Ok(IssueOutcome::Sent(challenge)) => Issued::Sent {
                challenge_id: challenge.id,
                expires_at: challenge.expires_at,
            },
            Ok(IssueOutcome::CoolingDown { retry_after }) => Issued::CoolingDown {
                retry_after_seconds: seconds(retry_after),
            },
            Ok(IssueOutcome::RateLimited { retry_after }) => Issued::RateLimited {
                retry_after_seconds: seconds(retry_after),
            },
            Err(error) => {
                self.observer.otp("issue", "failed");
                return Err(self.failed(&number, &error).await);
            }
        };
        self.observer.otp("issue", issued.as_str());
        Ok(issued)
    }

    /// Check `request.code` for `request.phone` and `request.purpose`:
    /// every check of an outstanding, unexpired code counts as an attempt,
    /// right or wrong (the library's). Asks Meta nothing, and reads no
    /// token.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `phone`, `purpose` or `code` (empty, or
    /// longer than [`MAX_CODE_LEN`]), before anything is read. `409
    /// otp_not_configured`. The store's failure.
    pub async fn verify(&self, request: VerifyRequest) -> Result<Checked, ServiceError> {
        let phone = phone(request.phone)?;
        let purpose = purpose(request.purpose)?;
        let code = request.code;
        if code.is_empty() || code.len() > MAX_CODE_LEN {
            return Err(ServiceError::invalid("code"));
        }
        let (_, settings) = self.settings().await?;
        // Checking a code sends nothing: the tokenless client, and no
        // token read.
        let service = self.service(self.authz.client().clone(), &settings)?;
        let checked = match service
            .verify(&Recipient::Phone(phone), &purpose, &code)
            .await
        {
            Ok(VerifyOutcome::Verified) => Checked::Verified,
            Ok(VerifyOutcome::Invalid { attempts_left }) => Checked::Invalid { attempts_left },
            Ok(VerifyOutcome::Expired) => Checked::Expired,
            Ok(VerifyOutcome::TooManyAttempts) => Checked::TooManyAttempts,
            Ok(VerifyOutcome::NotFound) => Checked::NotFound,
            Err(error) => {
                self.observer.otp("verify", "failed");
                return Err(ServiceError::from_library(&error));
            }
        };
        self.observer.otp("verify", checked.as_str());
        Ok(checked)
    }

    /// The settings the caller's codes follow, and the tenant they are
    /// (the holder of the sending number): the caller's own, else the
    /// deployment's default OTP tenant's, else `409 otp_not_configured`.
    async fn settings(&self) -> Result<(TenantId, OtpSettings), ServiceError> {
        let records = self.authz.records();
        let own = self.caller.tenant();
        if let Some(settings) = records.otp_settings(own).await? {
            return Ok((own.clone(), settings));
        }
        let default = self
            .deployment
            .default_tenant()
            .ok_or_else(not_configured)?;
        let settings = records
            .otp_settings(default)
            .await?
            .ok_or_else(not_configured)?;
        Ok((default.clone(), settings))
    }

    /// The sending number, opened for `holder` (the tenant whose settings
    /// name it): a number no longer `holder`'s is `409
    /// otp_not_configured`, the stale setting the operator fixes.
    async fn sender(
        &self,
        holder: &TenantId,
        settings: &OtpSettings,
    ) -> Result<OwnedNumber, ServiceError> {
        match self
            .authz
            .otp_sender(self.caller, holder, settings.phone_number_id.clone())
            .await
        {
            Err(error) if error.error_code() == ErrorCode::NOT_FOUND => {
                tracing::warn!(
                    holder = holder.as_str(),
                    "an OTP sending number is no longer its holder's: answered otp_not_configured"
                );
                Err(not_configured())
            }
            opened => opened,
        }
    }

    /// The library's service for `settings`, its namespace the caller's
    /// tenant (never the holder's: two tenants on one number keep apart).
    fn service(&self, client: Client, settings: &OtpSettings) -> Result<OtpService, ServiceError> {
        OtpService::new(
            client,
            settings.phone_number_id.clone(),
            OtpTemplate::new(
                settings.template_name.clone(),
                settings.template_language.clone(),
            ),
            self.codes.kv.clone(),
            self.codes.clock.clone(),
            self.deployment.keys.pepper.clone(),
            config(settings, self.caller.tenant()),
        )
        .map_err(|error| {
            tracing::warn!(
                kind = error.kind().as_str(),
                "stored OTP settings refused by the library"
            );
            ServiceError::internal()
        })
    }

    /// A failed send's error: a `190` marks the sending number's WABA, a
    /// Graph error's code is told to the observer; Meta's `details` are
    /// dropped (never [`ServiceError::with_details`]).
    async fn failed(&self, number: &OwnedNumber, error: &Error) -> ServiceError {
        if let Error::Validation(_) = error {
            // The library's own refusal of the recipient: nothing was sent.
            return ServiceError::invalid("phone");
        }
        let failed = number.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(failed.code());
        }
        failed
    }
}

/// The library's settings for `settings`, its namespace `tenant`.
pub fn config(settings: &OtpSettings, tenant: &TenantId) -> OtpConfig {
    OtpConfig {
        code_length: settings.code_length,
        ttl: Duration::from_secs(settings.ttl_seconds.into()),
        max_attempts: settings.max_attempts,
        resend_cooldown: Duration::from_secs(settings.resend_cooldown_seconds.into()),
        issue_limit: Some(IssueLimit {
            max_issues: settings.max_issues,
            window: Duration::from_secs(settings.issue_window_seconds.into()),
        }),
        namespace: tenant.as_str().to_owned(),
    }
}

/// Whether `settings` hold what the library accepts and what the service
/// allows, else `422 invalid_request` on the request's field: a template
/// name of Meta's form, a language code, the library's `OtpConfig` limits
/// and at most [`MAX_ISSUES`] codes per window. Whether the number is the
/// tenant's own is the admin service's check.
///
/// # Errors
///
/// `422 invalid_request` on `template.name`, `template.language`,
/// `code_length`, `ttl_seconds`, `max_attempts` or `issue_limit`.
pub fn check(settings: &OtpSettings) -> Result<(), ServiceError> {
    crate::templates::template_name("template.name", &settings.template_name)?;
    let language = &settings.template_language;
    if !(2..=15).contains(&language.len())
        || !language
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(ServiceError::invalid("template.language"));
    }
    if settings.max_issues > MAX_ISSUES {
        return Err(ServiceError::invalid("issue_limit"));
    }
    let placeholder = TenantId::parse("tenant").ok_or_else(ServiceError::internal)?;
    config(settings, &placeholder)
        .validate()
        .map_err(|refused| match refused.field.as_str() {
            "ttl" => ServiceError::invalid("ttl_seconds"),
            "code_length" => ServiceError::invalid("code_length"),
            "max_attempts" => ServiceError::invalid("max_attempts"),
            _ => ServiceError::invalid("issue_limit"),
        })
}

/// `phone` if it is strict E.164 with its `+`, else `422` on `phone`
/// (never quoting it). A BSUID, a `wa_id` without `+` and a number with
/// separators are refused: the library would accept separators, the
/// service's contract does not.
fn phone(phone: String) -> Result<String, ServiceError> {
    if is_e164(&phone) {
        Ok(phone)
    } else {
        Err(ServiceError::invalid("phone"))
    }
}

/// `purpose`, or [`DEFAULT_PURPOSE`]: 1 to [`MAX_PURPOSE_LEN`] of
/// `[A-Za-z0-9._:-]`, else `422` on `purpose`.
fn purpose(purpose: Option<String>) -> Result<String, ServiceError> {
    let purpose = purpose.unwrap_or_else(|| DEFAULT_PURPOSE.to_owned());
    let valid = !purpose.is_empty()
        && purpose.len() <= MAX_PURPOSE_LEN
        && purpose
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    if valid {
        Ok(purpose)
    } else {
        Err(ServiceError::invalid("purpose"))
    }
}

/// Whole seconds, rounded up.
fn seconds(wait: Duration) -> u64 {
    wait.as_secs() + u64::from(wait.subsec_nanos() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use meta_whatsapp_rs::core::ids::PhoneNumberId;

    fn settings() -> OtpSettings {
        OtpSettings::new(
            PhoneNumberId::new("106540352242922").unwrap(),
            "login_code",
            "en_US",
        )
    }

    /// The namespace is the tenant's id given, whatever the settings: the
    /// tenant the codes are for, never the holder of the number.
    #[test]
    fn the_config_is_the_settings_under_the_tenants_namespace() {
        let tenant = TenantId::parse("shop-a").unwrap();
        let config = config(&settings(), &tenant);
        assert_eq!(config.namespace, "shop-a");
        assert_eq!(
            config,
            OtpConfig {
                namespace: "shop-a".to_owned(),
                ..OtpConfig::new("x")
            },
            "the settings' defaults are the library's"
        );
        assert!(config.issue_limit.is_some(), "never off");
    }

    /// Each refused setting names its field.
    #[test]
    fn settings_are_checked_field_by_field() {
        type Change = fn(&mut OtpSettings);
        assert!(check(&settings()).is_ok());
        let cases: [(&str, Change); 8] = [
            ("template.name", |s| s.template_name = "Login Code".into()),
            ("template.language", |s| s.template_language = "e".into()),
            ("template.language", |s| {
                s.template_language = "en-US".into();
            }),
            ("code_length", |s| s.code_length = 3),
            ("ttl_seconds", |s| s.ttl_seconds = 5401),
            ("max_attempts", |s| s.max_attempts = 0),
            ("issue_limit", |s| s.max_issues = 0),
            ("issue_limit", |s| s.max_issues = MAX_ISSUES + 1),
        ];
        for (field, change) in cases {
            let mut s = settings();
            change(&mut s);
            assert_eq!(check(&s).unwrap_err().field(), Some(field), "{field}");
        }
        let mut s = settings();
        s.issue_window_seconds = 0;
        assert_eq!(check(&s).unwrap_err().field(), Some("issue_limit"));
    }

    /// Strict E.164 with `+`; a purpose of the tenant id's characters.
    #[test]
    fn the_phone_and_the_purpose_are_strict() {
        assert!(phone("+16505551234".into()).is_ok());
        for bad in [
            "16505551234",
            "+1 650 555 1234",
            "US.13491208655302741918",
            "+0123",
            "",
        ] {
            assert_eq!(
                phone(bad.into()).unwrap_err().field(),
                Some("phone"),
                "{bad}"
            );
        }
        assert_eq!(purpose(None).unwrap(), "login");
        assert_eq!(
            purpose(Some("reset_password".into())).unwrap(),
            "reset_password"
        );
        for bad in ["", "with space", "é", &"p".repeat(65)] {
            assert_eq!(
                purpose(Some(bad.to_owned())).unwrap_err().field(),
                Some("purpose"),
                "{bad}"
            );
        }
    }

    /// The keys' `Debug` shows nothing; two peppers key two fingerprints.
    #[test]
    fn the_keys_are_redacted_and_keyed() {
        let a = OtpKeys::new(SecretBytes::new(vec![7u8; 32])).unwrap();
        let b = OtpKeys::new(SecretBytes::new(vec![8u8; 32])).unwrap();
        assert_eq!(format!("{a:?}"), "OtpKeys([REDACTED])");
        assert_ne!(a.fingerprint(&[1; 32]), b.fingerprint(&[1; 32]));
        assert_eq!(a.fingerprint(&[1; 32]), a.fingerprint(&[1; 32]));
        assert!(
            OtpKeys::new(SecretBytes::new(vec![7u8; 31])).is_err(),
            "under 32 bytes"
        );
        let request = VerifyRequest {
            phone: "+16505551234".into(),
            purpose: None,
            code: "123456".into(),
        };
        let shown = format!("{request:?}");
        assert!(
            !shown.contains("123456") && !shown.contains("6505551234"),
            "{shown}"
        );
    }
}
