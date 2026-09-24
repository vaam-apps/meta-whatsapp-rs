//! The OTP service: one-time passcodes over an authentication template,
//! stored on a [`KvStore`] as keyed hashes only, verified with an atomic
//! attempt limit.
//!
//! Design (see `docs/architecture.md`, "Authentication"):
//!
//! - **Generation**: `code_length` decimal digits from the OS CSPRNG
//!   (`getrandom`), one byte per digit with rejection sampling — bytes
//!   `250..=255` are discarded so every digit is exactly 1/10 likely.
//! - **Storage**: namespace `wa.otp`, key = hex HMAC-SHA256(pepper,
//!   `"wa.otp.key|" + phone digits + "|" + purpose`). No phone number or
//!   code is ever stored or used as a key. The record holds the challenge
//!   id, HMAC-SHA256(pepper, `"wa.otp.code|" + challenge id + "|" + code`),
//!   the attempt count, the expiry and the send time. The two HMAC inputs
//!   carry different prefixes so a key can never be replayed as a code hash.
//! - **Issue**: refuses recipients without a phone number (Meta cannot
//!   deliver OTP buttons to a BSUID, `business-scoped-user-ids`), enforces
//!   the resend cooldown and the optional issue window, writes the record
//!   with compare-and-swap against what it read (so two concurrent issues
//!   cannot both send), then sends. A failed send removes the record again;
//!   a 2xx whose body cannot be read keeps it, because Meta accepted the
//!   message.
//! - **Verify**: counts the attempt with compare-and-swap *before*
//!   comparing, so concurrent guesses cannot exceed `max_attempts`;
//!   compares the HMACs in constant time (`subtle`); a match deletes the
//!   record (compare-and-swap again, so only one of two concurrent correct
//!   guesses wins).
//! - **Secrecy**: the code exists only in local variables and the request
//!   body. It is never logged, never put in an error, and every public type
//!   here has a `Debug` that cannot print it (the pepper's is redacted).
//!
//! Limitations: the code and pepper are ordinary heap memory, not zeroized
//! on drop (no zeroizing dependency is available to this crate).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use wa_core::clock::Clock;
use wa_core::error::{ConfigError, CryptoError, StorageError, ValidationError};
use wa_core::ids::{MessageId, PhoneNumberId};
use wa_core::recipient::Recipient;
use wa_core::store::{Expiry, JsonStore, KvStore};
use wa_core::{Error, Result};

use super::otp_template_message;
use crate::Client;
use crate::templates::TemplateMessage;

const NAMESPACE: &str = "wa.otp";
const WINDOW_NAMESPACE: &str = "wa.otp.rate";
const KEY_DOMAIN: &[u8] = b"wa.otp.key";
const CODE_DOMAIN: &[u8] = b"wa.otp.code";
const MIN_PEPPER_BYTES: usize = 32;

/// Server-side secret keying every HMAC the service computes. At least 32
/// random bytes; keep it out of the database that holds the challenges, so
/// a dump of the store alone cannot be brute-forced (a 6-digit code has
/// only 10⁶ values). Rotating it invalidates outstanding challenges.
#[derive(Clone)]
pub struct OtpPepper(Vec<u8>);

impl OtpPepper {
    /// Wrap the pepper. Rejects fewer than 32 bytes: HMAC-SHA256 keys
    /// shorter than the hash output weaken it (RFC 2104 §3).
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self> {
        let bytes = bytes.into();
        if bytes.len() < MIN_PEPPER_BYTES {
            return Err(ConfigError::new(format!(
                "OTP pepper must be at least {MIN_PEPPER_BYTES} bytes"
            ))
            .into());
        }
        Ok(Self(bytes))
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

/// At most `max_issues` codes per recipient and purpose in a fixed
/// `window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssueLimit {
    /// Codes allowed per window (at least 1).
    pub max_issues: u32,
    /// Window length (non-zero).
    pub window: Duration,
}

/// Service settings. [`OtpConfig::default`]: 6 digits, 10 minutes, 5
/// attempts, 30 s cooldown, no issue window.
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
    /// Minimum time between two codes for the same recipient and purpose.
    pub resend_cooldown: Duration,
    /// Optional cap on codes per recipient and purpose over a window. The
    /// cooldown alone allows one code per `resend_cooldown`; set this to
    /// bound message spend per phone number. Not set by default: the right
    /// numbers are a product decision.
    pub issue_limit: Option<IssueLimit>,
}

impl Default for OtpConfig {
    fn default() -> Self {
        Self {
            code_length: 6,
            ttl: Duration::from_mins(10),
            max_attempts: 5,
            resend_cooldown: Duration::from_secs(30),
            issue_limit: None,
        }
    }
}

impl OtpConfig {
    /// Check the settings.
    pub fn validate(&self) -> Result<()> {
        let bad = |msg: &str| Err(ConfigError::new(format!("OtpConfig: {msg}")).into());
        if !(4..=8).contains(&self.code_length) {
            return bad("code_length must be 4 to 8");
        }
        if self.ttl.is_zero() || self.ttl > Duration::from_mins(90) {
            return bad("ttl must be between 1 second and 90 minutes");
        }
        if self.max_attempts == 0 {
            return bad("max_attempts must be at least 1");
        }
        if let Some(limit) = self.issue_limit
            && (limit.max_issues == 0 || limit.window.is_zero())
        {
            return bad("issue_limit needs max_issues >= 1 and a non-zero window");
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
    /// A new code was sent; any previous code for this recipient and
    /// purpose is no longer valid.
    Sent(Challenge),
    /// A code was sent less than `resend_cooldown` ago.
    CoolingDown {
        /// Time until a new code may be issued.
        retry_after: Duration,
    },
    /// The [`IssueLimit`] window is full.
    RateLimited {
        /// Time until the window resets.
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

#[derive(Serialize, Deserialize)]
struct IssueWindow {
    /// UNIX milliseconds.
    started_at: i64,
    count: u32,
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

fn until(later: OffsetDateTime, now: OffsetDateTime) -> Duration {
    Duration::try_from(later - now).unwrap_or_default()
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf)
        .map_err(|e| Error::Other(anyhow::anyhow!("OS random number generator failed: {e}")))?;
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

/// Issues and verifies one-time passcodes. Cheap to clone.
#[derive(Clone)]
pub struct OtpService {
    client: Client,
    phone_number_id: PhoneNumberId,
    template: OtpTemplate,
    challenges: JsonStore<StoredChallenge>,
    windows: JsonStore<IssueWindow>,
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
    pub fn new(
        client: Client,
        phone_number_id: impl Into<PhoneNumberId>,
        template: OtpTemplate,
        store: Arc<dyn KvStore>,
        clock: Arc<dyn Clock>,
        pepper: OtpPepper,
        config: OtpConfig,
    ) -> Result<Self> {
        config.validate()?;
        crate::templates::validate::name(&template.name, "template.name")?;
        crate::templates::not_empty(&template.language, "template.language")?;
        Ok(Self {
            client,
            phone_number_id: phone_number_id.into(),
            template,
            challenges: JsonStore::new(Arc::clone(&store), NAMESPACE),
            windows: JsonStore::new(store, WINDOW_NAMESPACE),
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
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.pepper.0)
            .map_err(|_| CryptoError::InvalidKey("OTP pepper"))?;
        mac.update(domain);
        for part in parts {
            mac.update(b"|");
            mac.update(part);
        }
        Ok(mac.finalize().into_bytes().into())
    }

    /// Store key for a recipient and purpose. Phone digits contain no `|`,
    /// so `digits|purpose` is unambiguous.
    fn key(&self, recipient: &Recipient, purpose: &str) -> Result<String> {
        if !recipient.supports_otp_buttons() {
            return Err(ValidationError::new(
                "recipient",
                "authentication templates with OTP buttons need a phone number, not only a BSUID",
            )
            .into());
        }
        let (Recipient::Phone(phone) | Recipient::PhoneAndUser { phone, .. }) = recipient else {
            return Err(ValidationError::new("recipient", "needs a phone number").into());
        };
        let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return Err(ValidationError::new("recipient", "phone number has no digits").into());
        }
        if purpose.is_empty() {
            return Err(ValidationError::new("purpose", "must not be empty").into());
        }
        Ok(hex::encode(self.mac(
            KEY_DOMAIN,
            &[digits.as_bytes(), purpose.as_bytes()],
        )?))
    }

    fn cas_budget(&self) -> usize {
        usize::try_from(self.config.max_attempts)
            .unwrap_or(usize::MAX)
            .saturating_add(64)
    }

    /// Generate a code, store its hash, and send it to `recipient` with the
    /// authentication template. `purpose` separates independent flows for
    /// the same number (`"login"`, `"reset_password"`, …).
    ///
    /// Challenges are keyed on the phone number's digits only (`+`, spaces,
    /// dashes and brackets are ignored), so pass E.164 numbers consistently:
    /// `+44…` and `0044…` are different keys. The BSUID of a
    /// [`Recipient::PhoneAndUser`] is ignored, as Meta ignores it when a
    /// phone number is present.
    pub async fn issue(&self, recipient: &Recipient, purpose: &str) -> Result<IssueOutcome> {
        let key = self.key(recipient, purpose)?;
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
        let expires_ms = to_ms(now + self.config.ttl);
        let record = StoredChallenge {
            mac: hex::encode(self.mac(CODE_DOMAIN, &[id.as_bytes(), code.as_bytes()])?),
            id: id.clone(),
            attempts: 0,
            expires_at: expires_ms,
            sent_at: to_ms(now),
        };
        // Keep the record one extra TTL past expiry so a late guess gets
        // `Expired` instead of `NotFound`, and at least as long as the
        // cooldown it enforces.
        let keep_until =
            (now + self.config.ttl + self.config.ttl).max(now + self.config.resend_cooldown);

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
        match self.send(recipient, &message).await {
            Ok(message_id) => {
                tracing::debug!(challenge = %id, "OTP challenge issued");
                Ok(IssueOutcome::Sent(Challenge {
                    id,
                    expires_at: from_ms(expires_ms),
                    message_id,
                }))
            }
            Err(error) => {
                // A 2xx Meta accepted but we could not read: the code may be
                // on its way, keep it verifiable.
                if !matches!(error, Error::Decode { .. })
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
        let ready_at = from_ms(record.sent_at) + self.config.resend_cooldown;
        (now < ready_at).then(|| until(ready_at, now))
    }

    /// Take a slot in the issue window; `Some(wait)` when it is full.
    async fn reserve_issue(
        &self,
        key: &str,
        limit: IssueLimit,
        now: OffsetDateTime,
    ) -> Result<Option<Duration>> {
        let fresh = IssueWindow {
            started_at: to_ms(now),
            count: 1,
        };
        for _ in 0..self.cas_budget() {
            let done = match self.windows.get(key).await? {
                None => self
                    .windows
                    .put_if_absent(key, &fresh, Expiry::At(now + limit.window))
                    .await?
                    .is_some(),
                Some((window, version, _)) => {
                    let end = from_ms(window.started_at) + limit.window;
                    if now >= end {
                        self.windows
                            .compare_and_swap(
                                key,
                                version,
                                Some(&fresh),
                                Expiry::At(now + limit.window),
                            )
                            .await?
                            .is_some()
                    } else if window.count >= limit.max_issues {
                        return Ok(Some(until(end, now)));
                    } else {
                        let next = IssueWindow {
                            started_at: window.started_at,
                            count: window.count + 1,
                        };
                        self.windows
                            .compare_and_swap(key, version, Some(&next), Expiry::Keep)
                            .await?
                            .is_some()
                    }
                }
            };
            if done {
                return Ok(None);
            }
        }
        Err(contention("issue window"))
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

    async fn send(&self, recipient: &Recipient, template: &TemplateMessage) -> Result<MessageId> {
        // The send body of the authentication pages; built here because the
        // messages module is developed separately.
        #[derive(Serialize)]
        struct Body<'a> {
            messaging_product: &'static str,
            #[serde(flatten)]
            recipient: &'a Recipient,
            #[serde(rename = "type")]
            kind: &'static str,
            template: &'a TemplateMessage,
        }
        #[derive(Deserialize)]
        struct Sent {
            #[serde(default)]
            messages: Vec<SentMessage>,
        }
        #[derive(Deserialize)]
        struct SentMessage {
            id: MessageId,
        }
        const CONTEXT: &str = "send OTP message response";
        let sent: Sent = self
            .client
            .post(&format!("{}/messages", self.phone_number_id))
            .json(&Body {
                messaging_product: "whatsapp",
                recipient,
                kind: "template",
                template,
            })
            .context(CONTEXT)
            .send()
            .await?;
        sent.messages
            .into_iter()
            .next()
            .map(|m| m.id)
            .ok_or_else(|| {
                Error::decode(
                    CONTEXT,
                    <serde_json::Error as serde::de::Error>::custom(
                        "no `messages[0].id` in the response",
                    ),
                    b"",
                )
            })
    }

    /// Check `code` for `recipient` and `purpose`. Every call with an
    /// outstanding, unexpired code counts as an attempt, right or wrong.
    /// `code` is compared byte for byte: trim user input before calling.
    pub async fn verify(
        &self,
        recipient: &Recipient,
        purpose: &str,
        code: &str,
    ) -> Result<VerifyOutcome> {
        let key = self.key(recipient, purpose)?;
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
            let candidate = self.mac(CODE_DOMAIN, &[record.id.as_bytes(), code.as_bytes()])?;
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
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use http::Method;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use time::macros::datetime;
    use wa_adapters::store::MemoryKvStore;
    use wa_core::clock::ManualClock;
    use wa_core::error::TransportError;
    use wa_core::store::{StoreKey, Versioned};
    use wa_core::testing::{RecordedRequest, ScriptedTransport};

    use super::*;
    use crate::RetryPolicy;

    const PHONE: &str = "12015553931";
    const PEPPER: &[u8] = b"0123456789abcdef0123456789abcdef-test-pepper";

    /// A `MemoryKvStore` that records every written value and yields after
    /// every read and compare-and-swap, so concurrent callers interleave
    /// between their read and their write, and between counting an attempt
    /// and consuming the challenge (the windows a non-atomic implementation
    /// would lose in).
    #[derive(Debug)]
    struct RecordingKv {
        inner: MemoryKvStore,
        writes: Mutex<Vec<(StoreKey, Vec<u8>)>>,
    }

    impl RecordingKv {
        fn record(&self, key: &StoreKey, value: &[u8]) {
            self.writes
                .lock()
                .unwrap()
                .push((key.clone(), value.to_vec()));
        }
    }

    #[async_trait]
    impl KvStore for RecordingKv {
        async fn get(
            &self,
            key: &StoreKey,
        ) -> std::result::Result<Option<Versioned>, StorageError> {
            let v = self.inner.get(key).await;
            tokio::task::yield_now().await;
            v
        }
        async fn put(
            &self,
            key: &StoreKey,
            value: Vec<u8>,
            expiry: Expiry,
        ) -> std::result::Result<u64, StorageError> {
            self.record(key, &value);
            self.inner.put(key, value, expiry).await
        }
        async fn put_if_absent(
            &self,
            key: &StoreKey,
            value: Vec<u8>,
            expiry: Expiry,
        ) -> std::result::Result<Option<u64>, StorageError> {
            self.record(key, &value);
            self.inner.put_if_absent(key, value, expiry).await
        }
        async fn compare_and_swap(
            &self,
            key: &StoreKey,
            expected: u64,
            new: Option<Vec<u8>>,
            expiry: Expiry,
        ) -> std::result::Result<Option<u64>, StorageError> {
            if let Some(v) = &new {
                self.record(key, v);
            }
            let written = self
                .inner
                .compare_and_swap(key, expected, new, expiry)
                .await;
            // Also yield between a write and the caller's next step, so a
            // concurrent caller can act on the new version first.
            tokio::task::yield_now().await;
            written
        }
        async fn delete(&self, key: &StoreKey) -> std::result::Result<bool, StorageError> {
            self.inner.delete(key).await
        }
    }

    struct Fixture {
        otp: OtpService,
        transport: ScriptedTransport,
        clock: ManualClock,
        kv: Arc<RecordingKv>,
    }

    fn fixture(config: OtpConfig) -> Fixture {
        fixture_with(config, RetryPolicy::NONE)
    }

    fn fixture_with(config: OtpConfig, retry: RetryPolicy) -> Fixture {
        let transport = ScriptedTransport::new();
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .retry(retry)
            .build()
            .unwrap();
        let clock = ManualClock::new(datetime!(2026-09-24 12:00 UTC));
        let kv = Arc::new(RecordingKv {
            inner: MemoryKvStore::with_clock(Arc::new(clock.clone())),
            writes: Mutex::new(Vec::new()),
        });
        let otp = OtpService::new(
            client,
            "105954558954427",
            OtpTemplate::new("verification_code", "en_US"),
            kv.clone(),
            Arc::new(clock.clone()),
            OtpPepper::new(PEPPER).unwrap(),
            config,
        )
        .unwrap();
        Fixture {
            otp,
            transport,
            clock,
            kv,
        }
    }

    /// The send example response of the authentication pages.
    fn accept(t: &ScriptedTransport) {
        t.push_json(
            200,
            json!({
              "messaging_product": "whatsapp",
              "contacts": [{"input": PHONE, "wa_id": PHONE}],
              "messages": [{"id": "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBI4Qzc5QkNGNTc5NTMyMDU5QzEA"}]
            }),
        );
    }

    fn code_in(req: &RecordedRequest) -> String {
        let body = req.json().unwrap();
        body["template"]["components"][0]["parameters"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// `code` with its first digit changed.
    fn wrong(code: &str) -> String {
        let mut chars: Vec<char> = code.chars().collect();
        let d = chars[0].to_digit(10).unwrap();
        chars[0] = char::from_digit((d + 1) % 10, 10).unwrap();
        chars.into_iter().collect()
    }

    fn user() -> Recipient {
        Recipient::phone(PHONE)
    }

    async fn issue(f: &Fixture) -> (Challenge, String) {
        accept(&f.transport);
        let IssueOutcome::Sent(challenge) = f.otp.issue(&user(), "login").await.unwrap() else {
            panic!("expected a send")
        };
        let code = code_in(&f.transport.last_request().unwrap());
        (challenge, code)
    }

    async fn stored(f: &Fixture) -> Option<StoredChallenge> {
        let key = f.otp.key(&user(), "login").unwrap();
        f.otp.challenges.get(&key).await.unwrap().map(|(r, _, _)| r)
    }

    #[tokio::test]
    async fn issue_sends_the_documented_payload_then_verifies_once() {
        let f = fixture(OtpConfig::default());
        let (challenge, code) = issue(&f).await;

        let req = f.transport.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/105954558954427/messages");
        assert_eq!(req.bearer(), Some("TOKEN"));
        // copy-code/one-tap/zero-tap pages, send example request.
        assert_eq!(
            req.json(),
            Some(json!({
              "messaging_product": "whatsapp",
              "recipient_type": "individual",
              "to": PHONE,
              "type": "template",
              "template": {
                "name": "verification_code",
                "language": {"code": "en_US"},
                "components": [
                  {"type": "body", "parameters": [{"type": "text", "text": code}]},
                  {"type": "button", "sub_type": "url", "index": "0", "parameters": [{"type": "text", "text": code}]}
                ]
              }
            }))
        );
        assert_eq!(f.transport.remaining(), 0);
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(challenge.id.len(), 32);
        assert_eq!(challenge.expires_at, datetime!(2026-09-24 12:10 UTC));
        assert_eq!(
            challenge.message_id.as_str(),
            "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBI4Qzc5QkNGNTc5NTMyMDU5QzEA"
        );

        assert_eq!(
            f.otp.verify(&user(), "login", &code).await.unwrap(),
            VerifyOutcome::Verified
        );
        assert_eq!(
            f.otp.verify(&user(), "login", &code).await.unwrap(),
            VerifyOutcome::NotFound,
            "single use"
        );
        assert!(stored(&f).await.is_none());
    }

    #[tokio::test]
    async fn wrong_codes_count_down_then_lock_even_the_right_code() {
        let f = fixture(OtpConfig::default());
        let (_, code) = issue(&f).await;
        let bad = wrong(&code);
        for left in (0..5).rev() {
            assert_eq!(
                f.otp.verify(&user(), "login", &bad).await.unwrap(),
                VerifyOutcome::Invalid {
                    attempts_left: left
                }
            );
        }
        assert_eq!(
            f.otp.verify(&user(), "login", &code).await.unwrap(),
            VerifyOutcome::TooManyAttempts
        );
        assert_eq!(stored(&f).await.unwrap().attempts, 5);
    }

    #[tokio::test]
    async fn the_last_allowed_attempt_can_still_succeed() {
        let f = fixture(OtpConfig::default());
        let (_, code) = issue(&f).await;
        for _ in 0..4 {
            let _ = f.otp.verify(&user(), "login", &wrong(&code)).await.unwrap();
        }
        assert_eq!(
            f.otp.verify(&user(), "login", &code).await.unwrap(),
            VerifyOutcome::Verified
        );
    }

    #[tokio::test]
    async fn codes_expire_at_the_ttl() {
        let f = fixture(OtpConfig::default());
        let (_, code) = issue(&f).await;
        f.clock.advance(Duration::from_secs(10 * 60 - 1));
        assert_eq!(
            f.otp.verify(&user(), "login", &wrong(&code)).await.unwrap(),
            VerifyOutcome::Invalid { attempts_left: 4 }
        );
        f.clock.advance(Duration::from_secs(1));
        assert_eq!(
            f.otp.verify(&user(), "login", &code).await.unwrap(),
            VerifyOutcome::Expired
        );
        // Long after, the record is gone from the store.
        f.clock.advance(Duration::from_hours(1));
        assert_eq!(
            f.otp.verify(&user(), "login", &code).await.unwrap(),
            VerifyOutcome::NotFound
        );
    }

    #[tokio::test]
    async fn resend_cooldown_then_the_new_code_replaces_the_old() {
        let f = fixture(OtpConfig::default());
        let (_, old) = issue(&f).await;
        f.clock.advance(Duration::from_secs(10));
        assert_eq!(
            f.otp.issue(&user(), "login").await.unwrap(),
            IssueOutcome::CoolingDown {
                retry_after: Duration::from_secs(20)
            }
        );
        assert_eq!(
            f.transport.requests().len(),
            1,
            "nothing sent while cooling down"
        );
        f.clock.advance(Duration::from_secs(20));
        let (_, new) = issue(&f).await;
        assert_eq!(f.transport.requests().len(), 2);
        if old != new {
            assert!(matches!(
                f.otp.verify(&user(), "login", &old).await.unwrap(),
                VerifyOutcome::Invalid { .. }
            ));
        }
        assert_eq!(
            f.otp.verify(&user(), "login", &new).await.unwrap(),
            VerifyOutcome::Verified
        );
    }

    #[tokio::test]
    async fn a_failed_send_removes_the_challenge() {
        let f = fixture(OtpConfig::default());
        f.transport.push_json(
            400,
            json!({"error": {"message": "(#132001) Template name does not exist in the translation",
                              "type": "OAuthException", "code": 132001}}),
        );
        let err = f.otp.issue(&user(), "login").await.unwrap_err();
        assert_eq!(err.graph().map(|g| g.code), Some(132001));
        let code = code_in(&f.transport.last_request().unwrap());
        assert!(!err.to_string().contains(&code), "{err}");
        assert!(stored(&f).await.is_none());
        assert_eq!(
            f.otp.verify(&user(), "login", &code).await.unwrap(),
            VerifyOutcome::NotFound
        );
        // No cooldown left behind by a message that never went out.
        let _ = issue(&f).await;

        // Even with retries enabled, a timed-out send is not replayed (it
        // may have been delivered) and the challenge is dropped.
        let f = fixture_with(OtpConfig::default(), RetryPolicy::default());
        f.transport.push_error(|| TransportError::Timeout);
        assert!(f.otp.issue(&user(), "login").await.is_err());
        assert_eq!(f.transport.requests().len(), 1, "a send is never replayed");
        assert!(stored(&f).await.is_none());
    }

    #[tokio::test]
    async fn an_unreadable_2xx_keeps_the_challenge() {
        let f = fixture(OtpConfig::default());
        f.transport
            .push_json(200, json!({"messaging_product": "whatsapp"}));
        let err = f.otp.issue(&user(), "login").await.unwrap_err();
        assert!(matches!(err, Error::Decode { .. }), "{err}");
        let code = code_in(&f.transport.last_request().unwrap());
        assert_eq!(
            f.otp.verify(&user(), "login", &code).await.unwrap(),
            VerifyOutcome::Verified
        );
    }

    #[tokio::test]
    async fn recipients_without_a_phone_number_are_refused() {
        let f = fixture(OtpConfig::default());
        for r in [
            Recipient::user("US.13491208655302741918"),
            Recipient::group("Y2FwaV9ncm91cDox"),
        ] {
            let e = f.otp.issue(&r, "login").await.unwrap_err();
            assert!(
                matches!(e, Error::Validation(ref v) if v.field == "recipient"),
                "{e}"
            );
            let e = f.otp.verify(&r, "login", "123456").await.unwrap_err();
            assert!(matches!(e, Error::Validation(_)), "{e}");
        }
        assert!(f.transport.requests().is_empty());
        assert!(
            f.otp.issue(&user(), "").await.is_err(),
            "purpose is required"
        );
        // Phone + BSUID is fine, and keys on the phone digits only.
        let (_, code) = issue(&f).await;
        let both = Recipient::PhoneAndUser {
            phone: "+1 (201) 555-3931".into(),
            user: "US.1".into(),
        };
        assert_eq!(
            f.otp.verify(&both, "login", &code).await.unwrap(),
            VerifyOutcome::Verified
        );
    }

    #[tokio::test]
    async fn purposes_are_independent() {
        let f = fixture(OtpConfig::default());
        let (_, code) = issue(&f).await;
        assert_eq!(
            f.otp
                .verify(&user(), "reset_password", &code)
                .await
                .unwrap(),
            VerifyOutcome::NotFound
        );
        accept(&f.transport);
        assert!(matches!(
            f.otp.issue(&user(), "reset_password").await.unwrap(),
            IssueOutcome::Sent(_)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_wrong_guesses_never_exceed_max_attempts() {
        let f = fixture(OtpConfig::default());
        let (_, code) = issue(&f).await;
        let bad = wrong(&code);
        let tasks: Vec<_> = (0..20)
            .map(|_| {
                let otp = f.otp.clone();
                let bad = bad.clone();
                tokio::spawn(async move { otp.verify(&user(), "login", &bad).await.unwrap() })
            })
            .collect();
        let mut invalid = 0;
        let mut locked = 0;
        for t in tasks {
            match t.await.unwrap() {
                VerifyOutcome::Invalid { .. } => invalid += 1,
                VerifyOutcome::TooManyAttempts => locked += 1,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!((invalid, locked), (5, 15));
        assert_eq!(stored(&f).await.unwrap().attempts, 5);
        assert_eq!(
            f.otp.verify(&user(), "login", &code).await.unwrap(),
            VerifyOutcome::TooManyAttempts
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_issues_send_once() {
        let f = fixture(OtpConfig::default());
        accept(&f.transport);
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let otp = f.otp.clone();
                tokio::spawn(async move { otp.issue(&user(), "login").await.unwrap() })
            })
            .collect();
        let mut sent = 0;
        for t in tasks {
            match t.await.unwrap() {
                IssueOutcome::Sent(_) => sent += 1,
                IssueOutcome::CoolingDown { .. } => {}
                other @ IssueOutcome::RateLimited { .. } => panic!("{other:?}"),
            }
        }
        assert_eq!(sent, 1);
        assert_eq!(f.transport.requests().len(), 1);
        let code = code_in(&f.transport.last_request().unwrap());
        assert_eq!(
            f.otp.verify(&user(), "login", &code).await.unwrap(),
            VerifyOutcome::Verified
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_right_guesses_verify_exactly_once() {
        let f = fixture(OtpConfig {
            max_attempts: 50,
            ..OtpConfig::default()
        });
        let (_, code) = issue(&f).await;
        let tasks: Vec<_> = (0..10)
            .map(|_| {
                let otp = f.otp.clone();
                let code = code.clone();
                tokio::spawn(async move { otp.verify(&user(), "login", &code).await.unwrap() })
            })
            .collect();
        let mut verified = 0;
        for t in tasks {
            match t.await.unwrap() {
                VerifyOutcome::Verified => verified += 1,
                VerifyOutcome::NotFound => {}
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(verified, 1);
    }

    #[tokio::test]
    async fn the_store_never_sees_the_code_or_the_phone_number() {
        let f = fixture(OtpConfig {
            code_length: 8,
            ..OtpConfig::default()
        });
        let (_, code) = issue(&f).await;
        let _ = f.otp.verify(&user(), "login", &wrong(&code)).await.unwrap();
        let writes = f.kv.writes.lock().unwrap().clone();
        assert!(writes.len() >= 2, "{writes:?}");
        for (key, value) in &writes {
            assert_eq!(key.namespace(), NAMESPACE);
            assert!(!key.key().contains(PHONE), "{key}");
            assert_eq!(key.key().len(), 64, "hex HMAC-SHA256 key");
            let text = String::from_utf8(value.clone()).unwrap();
            assert!(
                !text.contains(&code),
                "stored value contains the code: {text}"
            );
            assert!(!text.contains(PHONE), "{text}");
        }
    }

    #[tokio::test]
    async fn debug_output_never_contains_the_code_or_the_pepper() {
        let f = fixture(OtpConfig::default());
        let (challenge, code) = issue(&f).await;
        let outcome = f.otp.verify(&user(), "login", &wrong(&code)).await.unwrap();
        let pepper = String::from_utf8_lossy(PEPPER).into_owned();
        for text in [
            format!("{:?}", f.otp),
            format!("{challenge:?}"),
            format!("{outcome:?}"),
            format!("{:?}", IssueOutcome::Sent(challenge.clone())),
            format!(
                "{:?}",
                otp_template_message("verification_code", "en_US", &code)
            ),
            format!("{:?}", OtpPepper::new(PEPPER).unwrap()),
        ] {
            assert!(!text.contains(&code), "{text}");
            assert!(!text.contains(&pepper), "{text}");
        }
    }

    #[tokio::test]
    async fn issue_window_limits_codes_per_recipient() {
        let f = fixture(OtpConfig {
            resend_cooldown: Duration::ZERO,
            issue_limit: Some(IssueLimit {
                max_issues: 2,
                window: Duration::from_secs(3600),
            }),
            ..OtpConfig::default()
        });
        let _ = issue(&f).await;
        f.clock.advance(Duration::from_secs(60));
        let _ = issue(&f).await;
        f.clock.advance(Duration::from_secs(60));
        assert_eq!(
            f.otp.issue(&user(), "login").await.unwrap(),
            IssueOutcome::RateLimited {
                retry_after: Duration::from_mins(58)
            }
        );
        assert_eq!(f.transport.requests().len(), 2);
        f.clock.advance(Duration::from_secs(3600));
        let _ = issue(&f).await;
    }

    #[test]
    fn rejection_sampling_skips_the_biased_bytes() {
        let mut code = String::new();
        push_digits(
            &mut code,
            6,
            &[250, 0, 255, 9, 251, 10, 249, 252, 253, 254, 123, 7, 8],
        );
        assert_eq!(code, "090937");
        let mut all = String::new();
        push_digits(&mut all, 256, &(0..=255).collect::<Vec<u8>>());
        assert_eq!(all.len(), 250, "exactly the 250 unbiased bytes are used");
        for d in '0'..='9' {
            assert_eq!(all.chars().filter(|c| *c == d).count(), 25, "digit {d}");
        }
    }

    #[test]
    fn random_codes_have_the_configured_length() {
        for len in 4..=8 {
            let code = random_code(len).unwrap();
            assert_eq!(code.len(), usize::from(len));
            assert!(code.bytes().all(|b| b.is_ascii_digit()));
        }
    }

    #[test]
    fn config_and_pepper_are_checked() {
        for bad in [
            OtpConfig {
                code_length: 3,
                ..OtpConfig::default()
            },
            OtpConfig {
                code_length: 9,
                ..OtpConfig::default()
            },
            OtpConfig {
                ttl: Duration::ZERO,
                ..OtpConfig::default()
            },
            OtpConfig {
                ttl: Duration::from_mins(91),
                ..OtpConfig::default()
            },
            OtpConfig {
                max_attempts: 0,
                ..OtpConfig::default()
            },
            OtpConfig {
                issue_limit: Some(IssueLimit {
                    max_issues: 0,
                    window: Duration::from_secs(1),
                }),
                ..OtpConfig::default()
            },
        ] {
            assert!(matches!(bad.validate(), Err(Error::Config(_))), "{bad:?}");
        }
        assert!(OtpConfig::default().validate().is_ok());
        assert!(OtpPepper::new(vec![7u8; 31]).is_err());
        assert!(OtpPepper::new(vec![7u8; 32]).is_ok());
    }
}
