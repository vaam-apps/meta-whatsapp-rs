//! A number's registration and its two-step verification PIN
//! (docs/design/server.md, section 4.2, decision D6; roadmap M3d), as
//! services an API adapter calls, with the tenant's WABA token:
//!
//! | Operation | Graph call | Attempt budget |
//! | --- | --- | --- |
//! | [`Registration::request_code`] | `POST /{pn}/request_code` `{code_method, language}` | [`AttemptBudget::Verification`] |
//! | [`Registration::verify_code`] | `POST /{pn}/verify_code` `{code}` | [`AttemptBudget::Verification`] |
//! | [`Registration::register`] | `POST /{pn}/register` `{messaging_product, pin, data_localization_region?}` | [`AttemptBudget::Registration`] |
//! | [`Registration::deregister`] | `POST /{pn}/deregister` | [`AttemptBudget::Registration`] |
//! | [`Registration::set_two_step_pin`] | `POST /{pn}` `{pin}` | none |
//!
//! Pages: `business-phone-numbers/registration`,
//! `business-phone-numbers/two-step-verification`,
//! `reference/whatsapp-business-phone-number/{phone-number-verification-request-code-api,
//! verify-code-api, phone-number-registration, phone-number-deregister-api}`.
//!
//! **The PIN is typed per attempt and never stored (D6).** It arrives in
//! a request's body, is checked locally ([`pin`]: six ASCII digits, the
//! library's `TwoStepPin`, whose `Debug` is redacted and whose bytes are
//! zeroed on drop), and goes to Meta in that one request; no record, log,
//! error, audit event, idempotency record, event or metric holds it. So
//! the same holds for the verification code ([`verification_code`]). A
//! failed Graph call keeps Meta's code, subcode and trace id, and never
//! its texts: not in the answer (no `details`, as on the OTP and signup
//! routes) and not in any log (Meta's texts could quote the request's
//! parameters; [`crate::authz::OwnedNumber::failed_withholding_texts`]).
//!
//! **Attempts are counted per number** ([`RegistrationAttempts`], on the
//! bundle's `KvStore`, so every replica shares them and a restart keeps
//! them), after the authorization order made the number the caller's own
//! (another tenant never spends a number's budget: its call is `404`
//! before) and after the local checks (a malformed PIN spends nothing:
//! Meta never sees it), before the request to Meta. Meta allows 10
//! registration and deregistration requests per number in a moving
//! 72-hour window and locks the number for 72 hours on the 11th
//! (`133016`), and locks it after too many wrong PINs (`133008`, for a
//! time it does not document): the service's default,
//! [`AttemptLimits::default`], is 5 a fixed window of 72 hours, so a
//! moving window of 72 hours never holds more than 10 of the service's
//! (two fixed windows' worth), and a caller's retry loop cannot lock the
//! merchant's number. A refused attempt is `429 too_many_requests` with
//! `Retry-After` the rest of the window, and nothing is asked of Meta.
//!
//! **Deregistering is destructive** (Meta's page: the number is unusable
//! with the Cloud API until registered again, with its PIN; its history
//! and local storage are kept; a coexistence number cannot be
//! deregistered): [`Registration::deregister`] takes the number's id
//! again as a confirmation ([`Registration::deregister`]'s `confirm`) and
//! writes a tenant's audit event (`number_deregistered`), as a PIN change
//! does (`two_step_pin_set`). Neither changes the service's binding: the
//! number stays bound and connected (its token still works), and sends
//! from a deregistered number fail at Meta (`409 registration`).

use std::sync::Arc;
use std::time::Duration;

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::phone_numbers::{
    CodeMethod, DataLocalizationRegion, TwoStepPin, VerificationCode,
};
use meta_whatsapp_rs::core::clock::Clock;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::core::store::{Expiry, KvStore, StoreKey};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::audit::{Subject, tenant_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::error::ServiceError;
use crate::observe::Observer;

/// The `KvStore` namespace of the attempt counters
/// ([`RegistrationAttempts`]; a stable identifier, pending the owner's
/// confirmation: docs/architecture.md, § Stable identifiers). The key is
/// `<budget>:<phone_number_id>` ([`RegistrationAttempts::store_key`]); the
/// value `{"v":1,"since":<Unix seconds>,"count":<n>}`. **Ephemeral**:
/// renamed, every number's count starts again (one window at most).
pub const ATTEMPTS_NAMESPACE: &str = "wa.server.registration";

/// Most compare-and-swap rounds a charge takes before it answers `503
/// storage_unavailable` (other replicas charging the same number at once).
const MAX_ROUNDS: usize = 8;

// ─── The local checks ────────────────────────────────────────────────────

/// A two-step verification PIN from a request: six ASCII digits (the
/// library's [`TwoStepPin`]), else `422 invalid_request` on `pin`, whose
/// answer never quotes the value. `raw` is moved into the PIN, not
/// copied.
///
/// # Errors
///
/// Anything but six ASCII digits.
pub fn pin(raw: String) -> Result<TwoStepPin, ServiceError> {
    TwoStepPin::new(raw).map_err(|_| ServiceError::invalid("pin"))
}

/// A verification code from a request: the library's [`VerificationCode`]
/// (hyphens dropped, then digits only), else `422 invalid_request` on
/// `code`, whose answer never quotes the value.
///
/// # Errors
///
/// No digit, or anything but digits and hyphens.
pub fn verification_code(raw: String) -> Result<VerificationCode, ServiceError> {
    VerificationCode::new(raw).map_err(|_| ServiceError::invalid("code"))
}

/// A data localization region from a request: a two-letter uppercase ISO
/// 3166 code (Meta's list, or another one, which Meta then judges), else
/// `422 invalid_request` on `data_localization_region`.
///
/// # Errors
///
/// Anything but two uppercase ASCII letters.
pub fn region(raw: &str) -> Result<DataLocalizationRegion, ServiceError> {
    let region = DataLocalizationRegion::from_code(raw);
    region
        .validate()
        .map_err(|_| ServiceError::invalid("data_localization_region"))?;
    Ok(region)
}

/// A deregistration's confirmation: `confirm` must be the number's id
/// again, else `422 invalid_request` on `confirm` (the destructive call is
/// never made on a path alone). [`Registration::deregister`] checks it;
/// an adapter may check it first, before anything else is done.
///
/// # Errors
///
/// `confirm` is not `number`'s id.
pub fn confirmed(number: &PhoneNumberId, confirm: &str) -> Result<(), ServiceError> {
    if confirm == number.as_str() {
        Ok(())
    } else {
        Err(ServiceError::invalid("confirm"))
    }
}

/// What a verification code is requested with: how it is delivered and
/// the language of its message (Meta's `code_method` and `language`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRequest {
    /// SMS or voice call.
    pub method: CodeMethod,
    /// The message's locale, e.g. `en_US`: not blank (the library's rule;
    /// Meta's page names no other).
    pub language: String,
}

impl CodeRequest {
    /// `422 invalid_request` on `language` when it is blank.
    fn check(&self) -> Result<(), ServiceError> {
        if self.language.trim().is_empty() {
            return Err(ServiceError::invalid("language"));
        }
        Ok(())
    }
}

// ─── The attempt budgets ─────────────────────────────────────────────────

/// Which of a number's budgets an operation spends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttemptBudget {
    /// Registering and deregistering: Meta's 10 per 72 hours, shared by
    /// both, and its wrong-PIN lock.
    Registration,
    /// Requesting and checking a verification code (Meta's pages say
    /// failed attempts may block the number for a while, without a
    /// number).
    Verification,
}

impl AttemptBudget {
    /// Every budget.
    pub const ALL: [Self; 2] = [Self::Registration, Self::Verification];

    /// Its name, the first part of its store key (a stable identifier with
    /// [`ATTEMPTS_NAMESPACE`]).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registration => "registration",
            Self::Verification => "verification",
        }
    }
}

/// One budget's limit: at most `attempts` in a fixed `window`, which
/// starts at its first attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptLimit {
    /// Attempts allowed in a window (at least 1).
    pub attempts: u32,
    /// The window (at least a second).
    pub window: Duration,
}

/// Every budget's limit, per number, shared by every replica.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptLimits {
    /// The [`AttemptBudget::Registration`] budget
    /// (`WA_SERVER_REGISTRATION_ATTEMPTS`, `WA_SERVER_REGISTRATION_WINDOW`).
    pub registration: AttemptLimit,
    /// The [`AttemptBudget::Verification`] budget
    /// (`WA_SERVER_VERIFICATION_ATTEMPTS`, `WA_SERVER_VERIFICATION_WINDOW`).
    pub verification: AttemptLimit,
}

impl Default for AttemptLimits {
    /// Registration: 5 in 72 hours (half of Meta's 10, so two fixed
    /// windows back to back stay within Meta's moving window). Verification:
    /// 10 in an hour (the service's default: Meta's pages name none), a
    /// code requested and mistyped a few times.
    fn default() -> Self {
        Self {
            registration: AttemptLimit {
                attempts: 5,
                window: Duration::from_hours(72),
            },
            verification: AttemptLimit {
                attempts: 10,
                window: Duration::from_hours(1),
            },
        }
    }
}

impl AttemptLimits {
    /// The limit of `budget`.
    pub fn of(&self, budget: AttemptBudget) -> AttemptLimit {
        match budget {
            AttemptBudget::Registration => self.registration,
            AttemptBudget::Verification => self.verification,
        }
    }
}

/// A budget's record: the window's start and the attempts counted in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Counted {
    v: u8,
    since: i64,
    count: u32,
}

impl Counted {
    fn first(now: OffsetDateTime) -> Self {
        Self {
            v: 1,
            since: now.unix_timestamp(),
            count: 1,
        }
    }

    fn bytes(self) -> Vec<u8> {
        serde_json::to_vec(&self).unwrap_or_default()
    }
}

/// The attempt counters of every number, on a `KvStore` (a typed store,
/// as the token vault is): one record per number and budget, under
/// [`ATTEMPTS_NAMESPACE`], counted by compare-and-swap, so replicas sharing
/// the store share the budgets. Fails closed: a store that fails, or a
/// record that does not read, refuses the attempt (`503`) before Meta is
/// asked. Built by the service's state from its bundle's `KvStore`
/// (`AppState::from_backend`); only the [`Registration`] services charge
/// it.
pub struct RegistrationAttempts {
    kv: Arc<dyn KvStore>,
    limits: AttemptLimits,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for RegistrationAttempts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrationAttempts")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl RegistrationAttempts {
    /// Counters on `kv` with `limits`, windows read on `clock`.
    pub fn new(kv: Arc<dyn KvStore>, limits: AttemptLimits, clock: Arc<dyn Clock>) -> Self {
        Self { kv, limits, clock }
    }

    /// The limits.
    pub fn limits(&self) -> AttemptLimits {
        self.limits
    }

    /// The store key of `number`'s `budget`: `<budget>:<phone_number_id>`
    /// under [`ATTEMPTS_NAMESPACE`].
    pub fn store_key(budget: AttemptBudget, number: &PhoneNumberId) -> StoreKey {
        StoreKey::new(
            ATTEMPTS_NAMESPACE,
            format!("{}:{}", budget.as_str(), number.as_str()),
        )
    }

    /// Count one attempt of `budget` on `number`, or refuse it: the window
    /// holds its limit already (`429 too_many_requests`, `Retry-After` the
    /// rest of the window), or the store failed (`503`). A window past its
    /// end starts again with this attempt.
    pub(crate) async fn charge(
        &self,
        budget: AttemptBudget,
        number: &PhoneNumberId,
    ) -> Result<(), ServiceError> {
        let limit = self.limits.of(budget);
        let attempts = limit.attempts.max(1);
        let window = time::Duration::try_from(limit.window.max(Duration::from_secs(1)))
            .unwrap_or(time::Duration::MAX);
        let key = Self::store_key(budget, number);
        for _ in 0..MAX_ROUNDS {
            let now = self.clock.now();
            let Some(stored) = self.kv.get(&key).await? else {
                let first = Counted::first(now);
                let expiry = Expiry::At(now.saturating_add(window));
                if self
                    .kv
                    .put_if_absent(&key, first.bytes(), expiry)
                    .await?
                    .is_some()
                {
                    return Ok(());
                }
                continue;
            };
            let counted: Counted =
                serde_json::from_slice(&stored.value).map_err(|source| StorageError::Corrupt {
                    key: key.to_string(),
                    source,
                })?;
            let since = OffsetDateTime::from_unix_timestamp(counted.since).unwrap_or(now);
            let ends = since.saturating_add(window);
            let (next, expiry) = if now >= ends {
                (Counted::first(now), Expiry::At(now.saturating_add(window)))
            } else if counted.count >= attempts {
                let wait = Duration::try_from(ends - now).unwrap_or(Duration::ZERO);
                tracing::info!(
                    budget = budget.as_str(),
                    "a number's attempt budget is spent: refused before Meta"
                );
                return Err(ServiceError::too_many_requests(
                    crate::ratelimit::retry_after_secs(wait),
                ));
            } else {
                (
                    Counted {
                        count: counted.count + 1,
                        ..counted
                    },
                    Expiry::Keep,
                )
            };
            if self
                .kv
                .compare_and_swap(&key, stored.version, Some(next.bytes()), expiry)
                .await?
                .is_some()
            {
                return Ok(());
            }
        }
        Err(ServiceError::from(StorageError::Busy))
    }
}

// ─── The operations ──────────────────────────────────────────────────────

/// The registration operations on a number the caller's tenant owns: an
/// [`OwnedNumber`] its [`Authorizer`] made, and admits, with that number's
/// token (never another: the operator's, a partner's), the attempts
/// counted in [`RegistrationAttempts`]. Cheap: a few references.
pub struct Registration<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    number: &'a OwnedNumber,
    attempts: &'a RegistrationAttempts,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for Registration<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registration")
            .field("caller", self.caller)
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> Registration<'a> {
    /// The operations on `number` for `caller`, once `authz` admits both
    /// ([`Authorizer::admit`], [`Authorizer::admit_number`]) and `number`
    /// was made from a binding of `caller`'s tenant. Failed Graph calls are
    /// reported to nobody until [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `caller` or `number` another [`Authorizer`] made: `403 forbidden`.
    /// `number` another tenant's: `404 not_found`, the answer for a number
    /// that does not exist. Either way, before any attempt is counted or
    /// anything asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        number: &'a OwnedNumber,
        attempts: &'a RegistrationAttempts,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_number(number)?;
        if number.tenant() != caller.tenant() {
            tracing::warn!("refused the registration services on another tenant's number");
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
            caller,
            number,
            attempts,
            observer: &(),
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// The error of a failed Graph call: the number's (a `190` marks its
    /// WABA's numbers), counted when Meta answered with a Graph error,
    /// Meta's texts in no log and no answer.
    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self
            .number
            .failed_withholding_texts(self.authz, error)
            .await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    fn phone(&self) -> meta_whatsapp_rs::client::phone_numbers::PhoneNumber {
        self.number
            .client()
            .phone_number(self.number.phone_number_id().clone())
    }

    /// The tenant's audit event `action` on this number.
    fn audit(&self, action: &'static str) {
        tenant_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.number.waba_id().as_str()),
                phone_number_id: Some(self.number.phone_number_id().as_str()),
                ..Subject::default()
            },
        );
    }

    /// Have Meta send a verification code to the number, by SMS or voice
    /// call. Spends one [`AttemptBudget::Verification`] attempt.
    ///
    /// # Errors
    ///
    /// A blank `language`: `422 invalid_request`, nothing counted or
    /// asked. The budget spent: `429`. Meta's refusal or failure (an
    /// already verified number: `409 registration`), without its texts.
    pub async fn request_code(&self, request: &CodeRequest) -> Result<(), ServiceError> {
        request.check()?;
        self.attempts
            .charge(AttemptBudget::Verification, self.number.phone_number_id())
            .await?;
        match self
            .phone()
            .request_code(request.method, &request.language)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Submit the verification code the number received. Spends one
    /// [`AttemptBudget::Verification`] attempt.
    ///
    /// # Errors
    ///
    /// The budget spent: `429`. Meta's refusal (a wrong code) or failure,
    /// without its texts.
    pub async fn verify_code(&self, code: &VerificationCode) -> Result<(), ServiceError> {
        self.attempts
            .charge(AttemptBudget::Verification, self.number.phone_number_id())
            .await?;
        match self.phone().verify_code(code).await {
            Ok(()) => Ok(()),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Register the number for the Cloud API with `pin`: the number's
    /// two-step verification PIN, which becomes its PIN when it has none
    /// (Meta's page); with `region`, local storage in that country. Spends
    /// one [`AttemptBudget::Registration`] attempt. The PIN is in no record
    /// and no log: the request to Meta is its only copy.
    ///
    /// # Errors
    ///
    /// The budget spent: `429`. Meta's refusal (a wrong PIN: `409
    /// two_step_verification`; too many attempts, an unverified number:
    /// `409 registration`) or failure, without its texts.
    pub async fn register(
        &self,
        pin: &TwoStepPin,
        region: Option<&DataLocalizationRegion>,
    ) -> Result<(), ServiceError> {
        self.attempts
            .charge(AttemptBudget::Registration, self.number.phone_number_id())
            .await?;
        match self.phone().register(pin, region).await {
            Ok(()) => Ok(()),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Deregister the number: it can no longer send or receive through the
    /// Cloud API until registered again. `confirm` must be the number's id
    /// again (the destructive call is never made on a path alone). Spends
    /// one [`AttemptBudget::Registration`] attempt; audited
    /// (`number_deregistered`, with the key's public id) once Meta
    /// confirmed.
    ///
    /// # Errors
    ///
    /// `confirm` not the number's id: `422 invalid_request` on `confirm`,
    /// nothing counted or asked. The budget spent: `429`. Meta's refusal
    /// (a coexistence number, the 72-hour limit) or failure, without its
    /// texts.
    pub async fn deregister(&self, confirm: &str) -> Result<(), ServiceError> {
        confirmed(self.number.phone_number_id(), confirm)?;
        self.attempts
            .charge(AttemptBudget::Registration, self.number.phone_number_id())
            .await?;
        match self.phone().deregister().await {
            Ok(()) => {
                self.audit("number_deregistered");
                Ok(())
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Set the number's two-step verification PIN (there is no call to
    /// turn two-step verification off). Audited (`two_step_pin_set`) once
    /// Meta confirmed; the PIN is in no record and no log.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, without its texts.
    pub async fn set_two_step_pin(&self, pin: &TwoStepPin) -> Result<(), ServiceError> {
        match self.phone().set_two_step_pin(pin).await {
            Ok(()) => {
                self.audit("two_step_pin_set");
                Ok(())
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::adapters::store::MemoryKvStore;
    use meta_whatsapp_rs::core::clock::ManualClock;

    use super::*;

    fn pn(id: &str) -> PhoneNumberId {
        PhoneNumberId::new(id).unwrap()
    }

    fn attempts(limits: AttemptLimits) -> (RegistrationAttempts, ManualClock, Arc<MemoryKvStore>) {
        let kv = Arc::new(MemoryKvStore::new());
        let clock = ManualClock::new(OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap());
        (
            RegistrationAttempts::new(kv.clone(), limits, Arc::new(clock.clone())),
            clock,
            kv,
        )
    }

    /// The namespace, the key layout and the record are stable
    /// identifiers (pending the owner's confirmation): spelled out here,
    /// with the `w` escaped so that a rename cannot rewrite them.
    #[tokio::test]
    async fn the_counter_key_and_record_are_pinned() {
        assert_eq!(ATTEMPTS_NAMESPACE, "\x77a.server.registration");
        let key =
            RegistrationAttempts::store_key(AttemptBudget::Registration, &pn("106540352242922"));
        assert_eq!(key.namespace(), "\x77a.server.registration");
        assert_eq!(key.key(), "registration:106540352242922");
        assert_eq!(
            RegistrationAttempts::store_key(AttemptBudget::Verification, &pn("1")).key(),
            "verification:1"
        );
        let (counters, _, kv) = attempts(AttemptLimits::default());
        counters
            .charge(AttemptBudget::Registration, &pn("106540352242922"))
            .await
            .unwrap();
        let stored = kv.get(&key).await.unwrap().unwrap();
        assert_eq!(
            String::from_utf8(stored.value).unwrap(),
            r#"{"v":1,"since":1800000000,"count":1}"#
        );
        assert_eq!(
            stored.expires_at,
            OffsetDateTime::from_unix_timestamp(1_800_000_000 + 72 * 3600).ok()
        );
    }

    /// Meta's 10 per moving 72 hours: the default spends 5 per fixed
    /// window, so two windows back to back hold 10 at most.
    #[test]
    fn the_defaults_stay_within_metas_registration_limit() {
        let limits = AttemptLimits::default();
        assert_eq!(limits.registration.attempts, 5);
        assert_eq!(limits.registration.window, Duration::from_hours(72));
        assert!(limits.registration.attempts * 2 <= 10);
        assert_eq!(limits.verification.attempts, 10);
        assert_eq!(limits.verification.window, Duration::from_hours(1));
    }

    /// The limit, then `429` with the rest of the window; another number
    /// and another budget untouched; a new window after the end.
    /// Decisive: the count's comparison and the window's end.
    #[tokio::test]
    async fn a_numbers_budget_refuses_past_its_limit_until_the_window_ends() {
        let (counters, clock, _) = attempts(AttemptLimits {
            registration: AttemptLimit {
                attempts: 2,
                window: Duration::from_hours(1),
            },
            ..AttemptLimits::default()
        });
        let a = pn("106540352242922");
        for _ in 0..2 {
            counters
                .charge(AttemptBudget::Registration, &a)
                .await
                .unwrap();
        }
        clock.advance(Duration::from_mins(10));
        let refused = counters
            .charge(AttemptBudget::Registration, &a)
            .await
            .unwrap_err();
        assert_eq!(
            (refused.status(), refused.code()),
            (429, "too_many_requests")
        );
        assert_eq!(refused.retry_after(), Some(3000));
        assert!(refused.is_retryable());
        assert!(!refused.may_have_been_sent());
        // Another number, another budget: whole.
        counters
            .charge(AttemptBudget::Registration, &pn("106540352242923"))
            .await
            .unwrap();
        counters
            .charge(AttemptBudget::Verification, &a)
            .await
            .unwrap();
        // The window's end: a new one, with this attempt.
        clock.advance(Duration::from_mins(50));
        counters
            .charge(AttemptBudget::Registration, &a)
            .await
            .unwrap();
        counters
            .charge(AttemptBudget::Registration, &a)
            .await
            .unwrap();
        assert!(
            counters
                .charge(AttemptBudget::Registration, &a)
                .await
                .is_err()
        );
    }

    /// A record that does not read refuses (fail closed), and so does a
    /// store that fails.
    #[tokio::test]
    async fn a_record_that_does_not_read_refuses_the_attempt() {
        let (counters, _, kv) = attempts(AttemptLimits::default());
        let a = pn("106540352242922");
        kv.put(
            &RegistrationAttempts::store_key(AttemptBudget::Registration, &a),
            b"{\"v\":2}".to_vec(),
            Expiry::Never,
        )
        .await
        .unwrap();
        let refused = counters
            .charge(AttemptBudget::Registration, &a)
            .await
            .unwrap_err();
        assert_eq!(refused.status(), 503);
    }

    /// The local checks never quote the value they refuse.
    #[test]
    fn the_local_checks_name_the_field_only() {
        assert!(pin("581063".to_owned()).is_ok());
        for bad in ["58106", "5810634", "58106x", "", " 81063", "５８１０６３"] {
            let refused = pin(bad.to_owned()).unwrap_err();
            assert_eq!(refused.field(), Some("pin"), "{bad:?}");
            assert_eq!(refused.code(), "invalid_request");
            if !bad.is_empty() {
                assert!(!refused.message().contains(bad));
            }
        }
        assert_eq!(
            verification_code("123-830".to_owned())
                .unwrap()
                .expose_secret(),
            "123830"
        );
        assert_eq!(
            verification_code("12a4".to_owned()).unwrap_err().field(),
            Some("code")
        );
        assert_eq!(region("CH").unwrap(), DataLocalizationRegion::Ch);
        assert_eq!(
            region("ch").unwrap_err().field(),
            Some("data_localization_region")
        );
        let blank = CodeRequest {
            method: CodeMethod::Sms,
            language: " ".to_owned(),
        };
        assert_eq!(blank.check().unwrap_err().field(), Some("language"));
    }
}
