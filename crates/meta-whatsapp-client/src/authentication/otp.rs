//! The OTP service: one-time passcodes over an authentication template,
//! stored on a [`KvStore`] as keyed hashes only, verified with an atomic
//! attempt limit.
//!
//! Design (see `docs/architecture.md`, "Authentication"):
//!
//! - **Recipient**: an E.164 number *with its leading `+`*. Meta prepends the
//!   sending number's country calling code to a number without `+`
//!   (`messages/send-messages#whatsapp-user-phone-number-formats`), so the
//!   same digits reach different people with and without it. Keying both on
//!   their digits (as a "digits only" normalization would) lets a code that
//!   was delivered to `+<business cc><digits>` verify `+<digits>`, an account
//!   takeover for whoever owns the first number. So `+` is required, the key
//!   is the digits after it, and the message goes to exactly `+<digits>`.
//!   Recipients without a phone number are refused: see [`OtpService::issue`].
//! - **Generation**: `code_length` decimal digits from the OS CSPRNG
//!   (`getrandom`), one byte per digit with rejection sampling — bytes
//!   `250..=255` are discarded so every digit is exactly 1/10 likely.
//! - **Storage**: namespace `wa.otp`, key = hex HMAC-SHA256(pepper,
//!   `"wa.otp.key|" + scope + "|" + digits + "|" + purpose`). No phone
//!   number or code is ever stored or used as a key. The record holds the
//!   challenge id, HMAC-SHA256(pepper, `"wa.otp.code|" + key + "|" +
//!   challenge id + "|" + code`), the attempt count, the expiry and the
//!   send time. The two HMAC inputs carry different prefixes so a key can
//!   never be replayed as a code hash, and the code hash covers the key it
//!   is stored under: whoever can write the store but lacks the pepper
//!   cannot copy their own record over someone else's key (another number,
//!   purpose or namespace) and verify there with their own code. The issue
//!   log (`wa.otp.rate`, same key) holds send times only.
//! - **Scope**: the sending `phone_number_id` and the required
//!   [`OtpConfig::namespace`] (the tenant), length-prefixed (netstrings, so
//!   neither can be shifted into the other). Services that share a store
//!   and a pepper — several merchants of one integrator, on their own
//!   numbers or on one shared number — therefore never see each other's
//!   codes, cooldowns or issue limits: without it, a code merchant A sent
//!   verified at merchant B for the same phone number and purpose.
//! - **Rate limits**: see [`OtpConfig::resend_cooldown`] and
//!   [`OtpConfig::issue_limit`] for the brute-force arithmetic behind them.
//! - **Issue**: cooldown check, then a slot in the issue log
//!   (compare-and-swap), then the record, written with compare-and-swap
//!   against what was read (so two concurrent issues cannot both send), then
//!   the send. A send Meta provably did not accept (a 4xx, throttling, a
//!   local refusal) removes the record again; one that may have been
//!   accepted (unreadable 2xx, timeout, any non-throttling 5xx, any other
//!   status) keeps it, because the code may be on its way. The line is
//!   [`Error::may_have_been_sent`], the same one integrators use.
//! - **Verify**: counts the attempt with compare-and-swap *before*
//!   comparing, so concurrent guesses cannot exceed `max_attempts`;
//!   compares the HMACs in constant time (`subtle`); a match deletes the
//!   record (compare-and-swap again, so only one of two concurrent correct
//!   guesses wins).
//! - **Secrecy**: the code exists only in local variables and the request
//!   body. It is never logged, never put in an error, and every public type
//!   here has a `Debug` that cannot print it. The pepper is a
//!   [`SecretBytes`] (zeroized on drop, redacted `Debug`).
//!
//! Limitation: the code itself is not zeroized. It has to be serialized into
//! the request body, which the transport owns; wiping the local copies would
//! not remove it from memory.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use time::{OffsetDateTime, PrimitiveDateTime};
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
use meta_whatsapp_core::clock::Clock;
use meta_whatsapp_core::error::{ConfigError, CryptoError, StorageError, ValidationError};
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::secret::SecretBytes;
use meta_whatsapp_core::store::{Expiry, JsonStore, KvStore};
use meta_whatsapp_core::{Error, Result};

use super::otp_template_message;
use crate::Client;
use crate::messages::OutboundMessage;
use crate::templates::TemplateMessage;

const NAMESPACE: &str = "wa.otp";
const ISSUE_LOG_NAMESPACE: &str = "wa.otp.rate";
const KEY_DOMAIN: &[u8] = b"wa.otp.key";
const CODE_DOMAIN: &[u8] = b"wa.otp.code";
const MIN_PEPPER_BYTES: usize = 32;
/// ITU-T E.164: at most 15 digits, country code first (never `0`).
const E164_MAX_DIGITS: usize = 15;

/// Server-side secret keying every HMAC the service computes. At least 32
/// random bytes; keep it out of the database that holds the challenges, so
/// a dump of the store alone cannot be brute-forced (a 6-digit code has
/// only 10⁶ values). Rotating it invalidates outstanding challenges and
/// resets every issue log.
#[derive(Clone)]
pub struct OtpPepper(SecretBytes);

impl OtpPepper {
    /// Wrap the pepper. Rejects fewer than 32 bytes: HMAC-SHA256 keys
    /// shorter than the hash output weaken it (RFC 2104 §3).
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self> {
        Self::from_secret(SecretBytes::new(bytes))
    }

    /// Wrap a pepper that is already a [`SecretBytes`] (no extra copy).
    pub fn from_secret(secret: SecretBytes) -> Result<Self> {
        if secret.len() < MIN_PEPPER_BYTES {
            return Err(ConfigError::new(format!(
                "OTP pepper must be at least {MIN_PEPPER_BYTES} bytes"
            ))
            .into());
        }
        Ok(Self(secret))
    }
}

impl fmt::Debug for OtpPepper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OtpPepper([REDACTED])")
    }
}

/// The approved authentication template the service sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtpTemplate {
    /// Template name.
    pub name: String,
    /// Language code it was approved in.
    pub language: String,
}

impl OtpTemplate {
    /// Build one.
    pub fn new(name: impl Into<String>, language: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            language: language.into(),
        }
    }
}

/// At most `max_issues` codes per phone number and purpose in any rolling
/// `window` (a sliding window: the limit holds for every `window`-long span,
/// not per calendar hour, so there is no burst at a window boundary).
///
/// Every issue that passes the resend cooldown takes a slot before it sends,
/// including one whose send then fails or that loses a race to a concurrent
/// issue: counting attempts rather than deliveries is what keeps concurrent
/// callers from exceeding the limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssueLimit {
    /// Codes allowed per window (at least 1). The issue log stores one
    /// timestamp per code in the window.
    pub max_issues: u32,
    /// Window length (non-zero).
    pub window: Duration,
}

impl IssueLimit {
    /// 5 codes per rolling hour, the [`OtpConfig::new`] limit.
    pub const DEFAULT: Self = Self {
        max_issues: 5,
        window: Duration::from_hours(1),
    };
}

impl Default for IssueLimit {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Service settings. [`OtpConfig::new`] takes the one setting without a
/// default, the [namespace](Self::namespace), and sets the rest: 6 digits,
/// 10 minutes, 5 attempts, 30 s cooldown, at most 5 codes per rolling hour.
/// Change any of them with struct update syntax:
///
/// ```
/// use meta_whatsapp_client::authentication::OtpConfig;
///
/// let config = OtpConfig {
///     code_length: 8,
///     ..OtpConfig::new("tenant-42")
/// };
/// assert!(config.validate().is_ok());
/// ```
///
/// There is no `Default`: a namespace nobody chose would let every service
/// that forgot it share one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtpConfig {
    /// Digits per code, 4 to 8. (iOS keyboard suggestions pick up numeric
    /// codes of 3 to 8 digits, `…/keyboard-suggestions`.)
    pub code_length: u8,
    /// How long a code is valid. Keep it equal to the template's
    /// `code_expiration_minutes` (Meta's default when that is unset is 10
    /// minutes, which is also this default) so the message never promises
    /// a lifetime the service does not honour. Between 1 second and 90
    /// minutes, the range of `code_expiration_minutes`.
    pub ttl: Duration,
    /// Wrong guesses allowed per code (at least 1). The attempt that
    /// reaches the limit is still compared; the next one is refused.
    pub max_attempts: u32,
    /// Minimum time between two codes for the same phone number and
    /// purpose. It stops accidental resends; it is **not** what bounds
    /// brute force (see [`Self::issue_limit`]): a new code resets the
    /// attempt count, so 30 s alone allows 120 codes an hour.
    pub resend_cooldown: Duration,
    /// Cap on codes per phone number and purpose; on by default
    /// ([`IssueLimit::DEFAULT`], 5 per rolling hour).
    ///
    /// Why it is mandatory by default: a guesser gets `max_attempts` tries
    /// per code and a fresh code resets them, so what bounds an online
    /// brute force is the number of codes one number can be sent. At the
    /// defaults (6 digits, 5 attempts) each code falls with probability
    /// 5/10⁶:
    ///
    /// | | codes/hour | guesses/hour | success in a day | 50 % success after |
    /// | --- | --- | --- | --- | --- |
    /// | cooldown only (30 s) | 120 | 600 | ≈ 1.4 % | ≈ 48 days |
    /// | + 5 per hour (default) | 5 | 25 | ≈ 0.06 % | ≈ 3.2 years |
    ///
    /// The attacker picks the victim's number and triggers the sends, so
    /// the limit has to be per number, not per client or IP. It also caps
    /// the WhatsApp messages (billed to you) one number can be flooded
    /// with.
    ///
    /// `None` is the explicit opt-out: set it only when an equivalent
    /// per-number limit is enforced in front of this service.
    pub issue_limit: Option<IssueLimit>,
    /// Tenant (or app) this service issues codes for — the tenant id of
    /// your platform, for instance. Required: not blank, without leading or
    /// trailing whitespace, without control (`Cc`) or format (`Cf`: U+200B,
    /// U+FEFF, bidi controls, …) characters. A server-side
    /// constant (from your configuration or your tenant table), never a
    /// value taken from the request: a caller who picks the namespace picks
    /// whose codes, cooldowns and limits they get.
    ///
    /// A code, cooldown or issue limit of one namespace is invisible to
    /// every other, on top of the binding to the sending `phone_number_id`:
    /// two tenants that send from the same number (a platform's own
    /// number) can never verify or rate-limit each other's codes. Keep it
    /// stable: changing it invalidates outstanding codes and resets the
    /// issue limits.
    pub namespace: String,
}

impl OtpConfig {
    /// The default settings for the service of `namespace` (see
    /// [`Self::namespace`]; checked by [`Self::validate`]).
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            code_length: 6,
            ttl: Duration::from_mins(10),
            max_attempts: 5,
            resend_cooldown: Duration::from_secs(30),
            issue_limit: Some(IssueLimit::DEFAULT),
            namespace: namespace.into(),
        }
    }

    /// Check the settings; the error names the offending field.
    /// [`OtpService::new`] reports the same failure as
    /// [`Error::Config`].
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        let bad = |field: &str, reason: &str| Err(ValidationError::new(field, reason));
        if !(4..=8).contains(&self.code_length) {
            return bad("code_length", "must be 4 to 8");
        }
        if self.ttl.is_zero() || self.ttl > Duration::from_mins(90) {
            return bad("ttl", "must be between 1 second and 90 minutes");
        }
        if self.max_attempts == 0 {
            return bad("max_attempts", "must be at least 1");
        }
        if let Some(limit) = self.issue_limit
            && (limit.max_issues == 0 || limit.window.is_zero())
        {
            return bad("issue_limit", "needs max_issues >= 1 and a non-zero window");
        }
        if self.namespace.trim().is_empty() {
            return bad(
                "namespace",
                "must not be blank: name the tenant (or app) the service issues codes for",
            );
        }
        // `" shop"`, `"shop\u{200B}"` and `"shop"` would be three tenants
        // that print alike.
        if self.namespace.trim() != self.namespace
            || self
                .namespace
                .chars()
                .any(|c| c.is_control() || c.general_category() == GeneralCategory::Format)
        {
            return bad(
                "namespace",
                "must not have leading or trailing whitespace, control characters or \
                 format characters (U+200B, U+FEFF, bidi controls, …)",
            );
        }
        Ok(())
    }
}

/// A code that was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    /// Random challenge id (32 hex characters). Not a secret, not the code.
    pub id: String,
    /// When the code stops being accepted.
    pub expires_at: OffsetDateTime,
    /// The WhatsApp message that carried it (match delivery webhooks on it).
    pub message_id: MessageId,
}

/// Result of [`OtpService::issue`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum IssueOutcome {
    /// A new code was sent; any previous code for this phone number and
    /// purpose is no longer valid.
    Sent(Challenge),
    /// A code was sent less than `resend_cooldown` ago.
    CoolingDown {
        /// Time until a new code may be issued.
        retry_after: Duration,
    },
    /// The [`IssueLimit`] is used up for this phone number and purpose.
    RateLimited {
        /// Time until the oldest code in the window leaves it.
        retry_after: Duration,
    },
}

/// Result of [`OtpService::verify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum VerifyOutcome {
    /// The code matched; it has been consumed and will not match again.
    Verified,
    /// Wrong code; this attempt was counted.
    Invalid {
        /// Guesses left before [`VerifyOutcome::TooManyAttempts`].
        attempts_left: u32,
    },
    /// The code expired; issue a new one.
    Expired,
    /// `max_attempts` guesses were used; the code is dead even if correct.
    TooManyAttempts,
    /// No code is outstanding (never issued, already used, or long gone).
    NotFound,
}

#[derive(Serialize, Deserialize)]
struct StoredChallenge {
    /// Challenge id (hex).
    id: String,
    /// Hex HMAC of the code.
    mac: String,
    attempts: u32,
    /// UNIX milliseconds.
    expires_at: i64,
    /// UNIX milliseconds.
    sent_at: i64,
}

/// Send times of the codes issued in the current window.
#[derive(Serialize, Deserialize)]
struct IssueLog {
    /// UNIX milliseconds.
    issued_at: Vec<i64>,
}

fn to_ms(t: OffsetDateTime) -> i64 {
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

fn from_ms(ms: i64) -> OffsetDateTime {
    // Out-of-range values only come from a corrupt record; treating them
    // as the epoch makes the challenge expired, which fails closed.
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn duration_ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// `t + d`, saturating instead of panicking: `resend_cooldown` and the issue
/// window are caller-chosen and unbounded.
fn plus(t: OffsetDateTime, d: Duration) -> OffsetDateTime {
    time::Duration::try_from(d)
        .ok()
        .and_then(|d| t.checked_add(d))
        .unwrap_or_else(|| PrimitiveDateTime::MAX.assume_utc())
}

fn until(later: OffsetDateTime, now: OffsetDateTime) -> Duration {
    Duration::try_from(later - now).unwrap_or_default()
}

/// The only error the OS random number generator produces here.
fn rng_error(_: getrandom::Error) -> Error {
    CryptoError::Rng.into()
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(rng_error)?;
    Ok(buf)
}

/// Append digits drawn from uniformly random `bytes` to `code`, stopping
/// at `len`. 250 = 25 × 10: bytes `250..=255` are skipped, otherwise `0`–`5`
/// would be likelier than `6`–`9` (modulo bias).
fn push_digits(code: &mut String, len: usize, bytes: &[u8]) {
    for &b in bytes {
        if code.len() >= len {
            return;
        }
        if b < 250 {
            code.push(char::from(b'0' + b % 10));
        }
    }
}

/// `len` uniformly random decimal digits from the OS CSPRNG.
fn random_code(len: u8) -> Result<String> {
    let len = usize::from(len);
    let mut code = String::with_capacity(len);
    while code.len() < len {
        let mut buf = random_bytes::<32>()?;
        push_digits(&mut code, len, &buf);
        buf.fill(0);
    }
    Ok(code)
}

fn contention(what: &str) -> Error {
    StorageError::Backend(anyhow::anyhow!(
        "OTP {what}: the record kept changing under concurrent writers"
    ))
    .into()
}

/// A phone number reduced to what decides its WhatsApp destination: the
/// E.164 digits after the `+`.
struct Phone {
    digits: String,
}

impl Phone {
    /// The one guard on who can receive a code.
    ///
    /// A recipient without a phone number (a BSUID alone, a group, or any
    /// addressing mode added later) is refused here, before anything is
    /// stored or sent: Meta cannot deliver authentication templates to a
    /// BSUID (Graph error `131062`, `ErrorKind::RecipientNotSupported`,
    /// `business-scoped-user-ids`).
    fn of(recipient: &Recipient) -> Result<Self> {
        let refuse = |reason: &str| Err(ValidationError::new("recipient", reason).into());
        let (Recipient::Phone(raw) | Recipient::PhoneAndUser { phone: raw, .. }) = recipient else {
            return refuse(
                "OTP codes need a phone number: authentication templates cannot be sent to a \
                 BSUID alone or a group (Graph error 131062)",
            );
        };
        // Never echo `raw` in an error: it is personal data.
        let Some(rest) = raw.trim().strip_prefix('+') else {
            return refuse(
                "phone number must be E.164 with its leading `+`: without it Meta prepends \
                 the sending number's country code",
            );
        };
        let mut digits = String::with_capacity(rest.len());
        for c in rest.chars() {
            match c {
                '0'..='9' => digits.push(c),
                // The separators Meta accepts in `to`.
                ' ' | '-' | '(' | ')' => {}
                _ => {
                    return refuse(
                        "phone number may only contain digits, spaces, hyphens and \
                         parentheses after the `+`",
                    );
                }
            }
        }
        if digits.is_empty() || digits.starts_with('0') || digits.len() > E164_MAX_DIGITS {
            return refuse("phone number must be a country code (not 0) and at most 15 digits");
        }
        Ok(Self { digits })
    }

    fn e164(&self) -> String {
        format!("+{}", self.digits)
    }
}

/// Append `bytes` as a netstring (`<len>:<bytes>,`), a self-delimiting
/// encoding: no sequence of netstrings can be re-split differently.
fn netstring(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(bytes);
    out.push(b',');
}

/// What every store key of a service is bound to: its sending number and
/// its namespace, as two netstrings (prefix-free, so neither can be shifted
/// into the other). The same bytes a service with that namespace derived
/// when the namespace was optional, so its outstanding codes survived the
/// change.
fn scope(phone_number_id: &PhoneNumberId, namespace: &str) -> Vec<u8> {
    let mut out = Vec::new();
    netstring(&mut out, phone_number_id.as_str().as_bytes());
    netstring(&mut out, namespace.as_bytes());
    out
}

/// Issues and verifies one-time passcodes. Cheap to clone.
///
/// Every code, cooldown and issue limit is bound to this service's sending
/// `phone_number_id` (and [`OtpConfig::namespace`]): services that share a
/// store and a pepper cannot verify, cancel or rate-limit each other's
/// codes.
#[derive(Clone)]
pub struct OtpService {
    client: Client,
    phone_number_id: PhoneNumberId,
    /// [`scope`] of `phone_number_id` and the namespace, computed once.
    scope: Arc<[u8]>,
    template: OtpTemplate,
    challenges: JsonStore<StoredChallenge>,
    issue_log: JsonStore<IssueLog>,
    clock: Arc<dyn Clock>,
    pepper: OtpPepper,
    config: OtpConfig,
}

impl fmt::Debug for OtpService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OtpService")
            .field("phone_number_id", &self.phone_number_id)
            .field("template", &self.template)
            .field("config", &self.config)
            .field("pepper", &self.pepper)
            .finish_non_exhaustive()
    }
}

impl OtpService {
    /// Build a service that sends `template` from `phone_number_id`, keeps
    /// challenges in `store` (namespaces `wa.otp` and `wa.otp.rate`) and
    /// reads time from `clock` (use the same clock for the store in tests).
    ///
    /// Challenges are bound to `phone_number_id` and
    /// [`OtpConfig::namespace`]: one store and one pepper can serve any
    /// number of services.
    pub fn new(
        client: Client,
        phone_number_id: impl Into<PhoneNumberId>,
        template: OtpTemplate,
        store: Arc<dyn KvStore>,
        clock: Arc<dyn Clock>,
        pepper: OtpPepper,
        config: OtpConfig,
    ) -> Result<Self> {
        config
            .validate()
            .map_err(|e| ConfigError::new(format!("OtpConfig: {e}")))?;
        let phone_number_id = phone_number_id.into();
        // Fail now rather than on the first send: an id that is empty, `.`
        // or `..` cannot be a path segment.
        client
            .endpoint()
            .url_segments(&[phone_number_id.as_str(), "messages"])
            .map_err(|e| ConfigError::new(format!("OTP phone_number_id: {}", e.reason)))?;
        crate::templates::validate::name(&template.name, "template.name")?;
        crate::templates::not_empty(&template.language, "template.language")?;
        let scope = scope(&phone_number_id, &config.namespace).into();
        Ok(Self {
            client,
            phone_number_id,
            scope,
            template,
            challenges: JsonStore::new(Arc::clone(&store), NAMESPACE),
            issue_log: JsonStore::new(store, ISSUE_LOG_NAMESPACE),
            clock,
            pepper,
            config,
        })
    }

    /// The settings.
    pub fn config(&self) -> &OtpConfig {
        &self.config
    }

    fn mac(&self, domain: &[u8], parts: &[&[u8]]) -> Result<[u8; 32]> {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.pepper.0.expose_secret())
            .map_err(|_| CryptoError::InvalidKey("OTP pepper"))?;
        mac.update(domain);
        for part in parts {
            mac.update(b"|");
            mac.update(part);
        }
        Ok(mac.finalize().into_bytes().into())
    }

    /// The code hash of challenge `id` stored under `key`. The key is part
    /// of it: a record copied to another key never verifies there. The
    /// input is unambiguous about the key because `key` comes first and is
    /// always 64 hex characters this service computed: whatever `id` (at
    /// verify, `record.id`, read back from the store) and `code` (the
    /// user's input) contain, `|` included, two different keys never hash
    /// the same bytes.
    fn code_mac(&self, key: &str, id: &str, code: &str) -> Result<[u8; 32]> {
        self.mac(
            CODE_DOMAIN,
            &[key.as_bytes(), id.as_bytes(), code.as_bytes()],
        )
    }

    /// Store key for a phone number and purpose, bound to this service's
    /// scope. The scope is prefix-free and digits contain no `|`, so
    /// `scope|digits|purpose` is unambiguous.
    fn key(&self, phone: &Phone, purpose: &str) -> Result<String> {
        if purpose.is_empty() {
            return Err(ValidationError::new("purpose", "must not be empty").into());
        }
        Ok(hex::encode(self.mac(
            KEY_DOMAIN,
            &[&self.scope[..], phone.digits.as_bytes(), purpose.as_bytes()],
        )?))
    }

    fn cas_budget(&self) -> usize {
        usize::try_from(self.config.max_attempts)
            .unwrap_or(usize::MAX)
            .saturating_add(64)
    }

    /// Generate a code, store its hash, and send it to `recipient` with the
    /// authentication template. `purpose` separates independent flows for
    /// the same number (`"login"`, `"reset_password"`, …): a constant of
    /// your code, never a value from the request, or a caller could verify
    /// one flow with a code sent for another.
    ///
    /// `recipient` must carry an E.164 phone number with its leading `+`
    /// (spaces, hyphens and parentheses are ignored): that exact number is
    /// what the code is bound to and sent to. From a `wa_id`, prepend `+`.
    /// The BSUID of a [`Recipient::PhoneAndUser`] is not sent, as Meta
    /// ignores it when a phone number is present.
    ///
    /// A recipient without a phone number (a BSUID alone, a group) or with
    /// a number not in that form is refused before anything is stored or
    /// sent, with [`Error::Validation`] on field `recipient` (so
    /// [`Error::kind`] is `InvalidParameter`). For a BSUID this is the local
    /// form of Graph error `131062` (`ErrorKind::RecipientNotSupported`),
    /// which Meta would return for the same send: handle both the same way.
    ///
    /// Returns an error, with the code still verifiable, when the send may
    /// have been accepted (an unreadable 2xx, a timeout, a non-throttling
    /// 5xx); the cooldown then applies to a retry.
    pub async fn issue(&self, recipient: &Recipient, purpose: &str) -> Result<IssueOutcome> {
        let phone = Phone::of(recipient)?;
        let key = self.key(&phone, purpose)?;
        let now = self.clock.now();
        let mut existing = self.challenges.get(&key).await?;
        if let Some(wait) = self.cooling_down(existing.as_ref(), now) {
            return Ok(IssueOutcome::CoolingDown { retry_after: wait });
        }
        if let Some(limit) = self.config.issue_limit
            && let Some(retry_after) = self.reserve_issue(&key, limit, now).await?
        {
            return Ok(IssueOutcome::RateLimited { retry_after });
        }

        let id = hex::encode(random_bytes::<16>()?);
        let code = random_code(self.config.code_length)?;
        let expires_at = plus(now, self.config.ttl);
        let record = StoredChallenge {
            mac: hex::encode(self.code_mac(&key, &id, &code)?),
            id: id.clone(),
            attempts: 0,
            expires_at: to_ms(expires_at),
            sent_at: to_ms(now),
        };
        // Keep the record one extra TTL past expiry so a late guess gets
        // `Expired` instead of `NotFound`, and at least as long as the
        // cooldown it enforces.
        let keep_until =
            plus(expires_at, self.config.ttl).max(plus(now, self.config.resend_cooldown));

        let mut stored = false;
        for _ in 0..self.cas_budget() {
            let written = match &existing {
                Some((_, version, _)) => {
                    self.challenges
                        .compare_and_swap(&key, *version, Some(&record), Expiry::At(keep_until))
                        .await?
                }
                None => {
                    self.challenges
                        .put_if_absent(&key, &record, Expiry::At(keep_until))
                        .await?
                }
            };
            if written.is_some() {
                stored = true;
                break;
            }
            // Someone wrote in between: a concurrent issue (cooldown now
            // applies) or a verification attempt on the old code (retry).
            existing = self.challenges.get(&key).await?;
            if let Some(wait) = self.cooling_down(existing.as_ref(), now) {
                return Ok(IssueOutcome::CoolingDown { retry_after: wait });
            }
        }
        if !stored {
            return Err(contention("issue"));
        }

        let message = otp_template_message(&self.template.name, &self.template.language, &code);
        drop(code);
        match self.send(&phone, message).await {
            Ok(message_id) => {
                tracing::debug!(challenge = %id, "OTP challenge issued");
                Ok(IssueOutcome::Sent(Challenge {
                    id,
                    expires_at: from_ms(record.expires_at),
                    message_id,
                }))
            }
            Err(error) => {
                // Only a provable rejection removes the challenge. If the
                // send may have been accepted, the code may be on its way
                // and stays verifiable: dropping it would turn a code the
                // user receives into a dead one, and keeping a code nobody
                // received gives a guesser nothing (the attempt and the
                // issue slot are counted either way).
                if !error.may_have_been_sent()
                    && let Err(cleanup) = self.remove(&key, &id).await
                {
                    tracing::warn!(challenge = %id, error = %cleanup, "could not remove unsent OTP challenge");
                }
                Err(error)
            }
        }
    }

    fn cooling_down(
        &self,
        existing: Option<&(StoredChallenge, u64, Option<OffsetDateTime>)>,
        now: OffsetDateTime,
    ) -> Option<Duration> {
        let (record, _, _) = existing?;
        let ready_at = plus(from_ms(record.sent_at), self.config.resend_cooldown);
        (now < ready_at).then(|| until(ready_at, now))
    }

    /// Take a slot in the issue log; `Some(wait)` when the window is full.
    async fn reserve_issue(
        &self,
        key: &str,
        limit: IssueLimit,
        now: OffsetDateTime,
    ) -> Result<Option<Duration>> {
        let max = usize::try_from(limit.max_issues).unwrap_or(usize::MAX);
        let now_ms = to_ms(now);
        // An issue at `t` is in the window while `now - t < window`.
        let horizon = now_ms.saturating_sub(duration_ms(limit.window));
        for _ in 0..self.cas_budget() {
            let current = self.issue_log.get(key).await?;
            let (mut issued, version) = match current {
                Some((log, version, _)) => (log.issued_at, Some(version)),
                None => (Vec::new(), None),
            };
            issued.retain(|&t| t > horizon);
            issued.sort_unstable();
            if issued.len() >= max {
                // The window has room again once all but `max - 1` of these
                // have left it.
                let blocking = issued[issued.len() - max];
                return Ok(Some(until(plus(from_ms(blocking), limit.window), now)));
            }
            issued.push(now_ms);
            let log = IssueLog { issued_at: issued };
            let expiry = Expiry::At(plus(now, limit.window));
            let written = match version {
                Some(version) => {
                    self.issue_log
                        .compare_and_swap(key, version, Some(&log), expiry)
                        .await?
                }
                None => self.issue_log.put_if_absent(key, &log, expiry).await?,
            };
            if written.is_some() {
                return Ok(None);
            }
        }
        Err(contention("issue limit"))
    }

    /// Delete the challenge `id` under `key` if it is still the stored one.
    async fn remove(&self, key: &str, id: &str) -> Result<bool> {
        for _ in 0..self.cas_budget() {
            let Some((record, version, _)) = self.challenges.get(key).await? else {
                return Ok(false);
            };
            if record.id != id {
                return Ok(false);
            }
            if self
                .challenges
                .compare_and_swap(key, version, None, Expiry::Keep)
                .await?
                .is_some()
            {
                return Ok(true);
            }
        }
        Err(contention("removal"))
    }

    /// Send the code with [`Messages::send`](crate::messages::Messages::send):
    /// the authentication pages' send body, checked by
    /// [`OutboundMessage::validate`], and a decode error that never quotes
    /// the response (it names the recipient).
    async fn send(&self, phone: &Phone, template: TemplateMessage) -> Result<MessageId> {
        let message = OutboundMessage::template(Recipient::Phone(phone.e164()), template);
        let sent = self
            .client
            .messages(self.phone_number_id.clone())
            .send(&message)
            .await?;
        sent.messages
            .into_iter()
            .next()
            .map(|m| m.id)
            .ok_or_else(|| {
                crate::request::withheld_decode_error(
                    crate::messages::SEND_CONTEXT,
                    "no `messages[0].id` in the response",
                )
            })
    }

    /// Check `code` for `recipient` and `purpose`. Every call with an
    /// outstanding, unexpired code counts as an attempt, right or wrong.
    /// `code` is compared byte for byte: trim user input before calling.
    /// `recipient` is checked as in [`Self::issue`].
    pub async fn verify(
        &self,
        recipient: &Recipient,
        purpose: &str,
        code: &str,
    ) -> Result<VerifyOutcome> {
        let key = self.key(&Phone::of(recipient)?, purpose)?;
        for _ in 0..self.cas_budget() {
            let Some((mut record, version, _)) = self.challenges.get(&key).await? else {
                return Ok(VerifyOutcome::NotFound);
            };
            if self.clock.now() >= from_ms(record.expires_at) {
                return Ok(VerifyOutcome::Expired);
            }
            if record.attempts >= self.config.max_attempts {
                return Ok(VerifyOutcome::TooManyAttempts);
            }
            record.attempts += 1;
            // Count first: a guess that loses this race is re-read and
            // re-checked against the limit, never compared for free.
            let Some(counted) = self
                .challenges
                .compare_and_swap(&key, version, Some(&record), Expiry::Keep)
                .await?
            else {
                continue;
            };
            let expected = hex::decode(&record.mac).map_err(|_| {
                Error::from(StorageError::Backend(anyhow::anyhow!(
                    "OTP record `{NAMESPACE}/{key}` has a malformed hash"
                )))
            })?;
            let candidate = self.code_mac(&key, &record.id, code)?;
            if bool::from(candidate.as_slice().ct_eq(expected.as_slice())) {
                return self.consume(&key, &record.id, counted).await;
            }
            return Ok(VerifyOutcome::Invalid {
                attempts_left: self.config.max_attempts - record.attempts,
            });
        }
        Err(contention("verification"))
    }

    /// Delete the verified challenge. Exactly one of several concurrent
    /// correct guesses gets `Verified`; the others find it gone.
    async fn consume(&self, key: &str, id: &str, mut version: u64) -> Result<VerifyOutcome> {
        for _ in 0..self.cas_budget() {
            if self
                .challenges
                .compare_and_swap(key, version, None, Expiry::Keep)
                .await?
                .is_some()
            {
                tracing::debug!(challenge = %id, "OTP challenge verified");
                return Ok(VerifyOutcome::Verified);
            }
            match self.challenges.get(key).await? {
                Some((record, current, _)) if record.id == id => version = current,
                _ => return Ok(VerifyOutcome::NotFound),
            }
        }
        Err(contention("consumption"))
    }
}

#[cfg(test)]
mod tests;
