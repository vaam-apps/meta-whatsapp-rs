//! [`KvRateLimiter`]: one budget per business number, shared by every
//! replica through a `KvStore`.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::BuildHasher;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_core::clock::SystemClock;
use meta_whatsapp_core::error::{ConfigError, StorageError};
use meta_whatsapp_core::ids::PhoneNumberId;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey};
use meta_whatsapp_core::{Error, Result};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{RateLimiter, Reservation, SlotRequest, Timer, earlier, elapsed, later};
use crate::pacer::Rate;

/// Store namespace of [`KvRateLimiter`]'s schedules (`wa.<module>.<purpose>`,
/// like every namespace of the library: `docs/architecture.md` § Stable
/// identifiers). **Ephemeral**: changing it forgets each number's schedule,
/// slow-down and raised rate, so for one schedule's length replicas of two
/// releases book from two budgets; nothing else is stranded.
pub const PACER_NAMESPACE: &str = "wa.bot.pacer";

/// The first byte of a version 1 record.
const RECORD_V1: u8 = 1;

/// A version 1 record's length: the version byte, then `due` (8), the
/// raised rate (4), the divisor (4), `since` (8), `slowed` (8), the last id
/// (8) and the last booking's base (8).
const RECORD_V1_LEN: usize = 49;

/// An absent instant in a record (`slowed`, the last booking's base).
const ABSENT: i64 = i64::MIN;

/// One number's schedule, as stored: the generic cell rate algorithm's
/// state, the slow-down's and the rate an upgrade raised it to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Record {
    /// When the next send is due (the theoretical arrival time), on the
    /// replicas' clocks.
    due: OffsetDateTime,
    /// The rate a throughput upgrade raised the number to, shared by every
    /// replica ([`KvRateLimiter::raise_rate`]); never lowered.
    raised: Option<Rate>,
    /// The rate is divided by this after slow-downs (1 when none).
    divisor: u32,
    /// The last slow-down, or the last step back up since.
    since: OffsetDateTime,
    /// The last slow-down that counted.
    slowed: Option<OffsetDateTime>,
    /// The id of the last slot booked (any replica's).
    last_id: u64,
    /// The schedule before the last slot booked, while it can be given
    /// back ([`RateLimiter::release`]).
    last_base: Option<OffsetDateTime>,
}

impl Record {
    fn new(now: OffsetDateTime) -> Self {
        Self {
            due: now,
            raised: None,
            divisor: 1,
            since: now,
            slowed: None,
            last_id: 0,
            last_base: None,
        }
    }

    /// Version 1: the version byte, then big-endian fields; instants as
    /// nanoseconds since the Unix epoch (`i64`, saturating: until 2262),
    /// an absent one as `i64::MIN`, an absent rate as zero.
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(RECORD_V1_LEN);
        out.push(RECORD_V1);
        out.extend_from_slice(&nanos(self.due).to_be_bytes());
        out.extend_from_slice(&self.raised.map_or(0, Rate::get).to_be_bytes());
        out.extend_from_slice(&self.divisor.to_be_bytes());
        out.extend_from_slice(&nanos(self.since).to_be_bytes());
        out.extend_from_slice(&self.slowed.map_or(ABSENT, nanos).to_be_bytes());
        out.extend_from_slice(&self.last_id.to_be_bytes());
        out.extend_from_slice(&self.last_base.map_or(ABSENT, nanos).to_be_bytes());
        out
    }

    /// A stored record, or why it cannot be read (another version, a
    /// record not of this crate's).
    fn decode(bytes: &[u8]) -> std::result::Result<Self, String> {
        match bytes.first() {
            Some(&RECORD_V1) if bytes.len() == RECORD_V1_LEN => {}
            Some(&RECORD_V1) => return Err(format!("version 1 of {} bytes", bytes.len())),
            Some(version) => return Err(format!("version {version}, not one this release reads")),
            None => return Err("empty".to_owned()),
        }
        let mut fields = Fields(&bytes[1..]);
        let due = instant(fields.i64())?.ok_or("no due time")?;
        let raised = match fields.u32() {
            0 => None,
            rate => Some(Rate(rate)),
        };
        let divisor = fields.u32();
        if divisor == 0 {
            return Err("a divisor of zero".to_owned());
        }
        let since = instant(fields.i64())?.ok_or("no recovery time")?;
        let slowed = instant(fields.i64())?;
        let last_id = fields.u64();
        let last_base = instant(fields.i64())?;
        Ok(Self {
            due,
            raised,
            divisor,
            since,
            slowed,
            last_id,
            last_base,
        })
    }
}

/// Reads a record's fixed-width fields in order (the length is checked
/// before).
struct Fields<'a>(&'a [u8]);

impl Fields<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        if let Some((head, rest)) = self.0.split_at_checked(N) {
            out.copy_from_slice(head);
            self.0 = rest;
        }
        out
    }

    fn i64(&mut self) -> i64 {
        i64::from_be_bytes(self.take())
    }

    fn u64(&mut self) -> u64 {
        u64::from_be_bytes(self.take())
    }

    fn u32(&mut self) -> u32 {
        u32::from_be_bytes(self.take())
    }
}

/// Nanoseconds since the Unix epoch, saturating, never [`ABSENT`].
fn nanos(at: OffsetDateTime) -> i64 {
    i64::try_from(at.unix_timestamp_nanos())
        .unwrap_or(if at.unix_timestamp() < 0 {
            i64::MIN
        } else {
            i64::MAX
        })
        .max(ABSENT + 1)
}

fn instant(nanos: i64) -> std::result::Result<Option<OffsetDateTime>, String> {
    if nanos == ABSENT {
        return Ok(None);
    }
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos))
        .map(Some)
        .map_err(|e| e.to_string())
}

/// What a change of the record does.
enum Change<R> {
    /// Write the changed record, then answer.
    Write(R),
    /// Answer without writing (nothing changed).
    Keep(R),
}

/// A [`RateLimiter`] whose schedules are records in a [`KvStore`], so that
/// every replica on the same store books each number's sends from one
/// budget: the generic cell rate algorithm of [`super::TokenBucket`], its
/// state (when the next send is due, the slow-down, a raised rate) kept per
/// number under [`PACER_NAMESPACE`] and changed only by `compare_and_swap`
/// on the version read. Opt-in: a [`super::Pacer`] paces with a
/// [`super::TokenBucket`] in this process unless given one of these.
///
/// ```no_run
/// # fn demo(kv: std::sync::Arc<dyn meta_whatsapp_core::store::KvStore>) {
/// use meta_whatsapp_bot::{KvRateLimiter, Pacer, Rate};
///
/// // On every replica, over the same store (Redis, Postgres).
/// let pacer = Pacer::new(KvRateLimiter::new(kv, Rate::DEFAULT));
/// # let _ = pacer; }
/// ```
///
/// **Configure every replica alike.** The rates ([`Self::rate_for`],
/// [`Self::set_rate`]) and the settings (burst, slow-downs, recovery) are
/// each replica's own and the schedule is shared: a replica configured
/// slower books slower slots in the same schedule. Only a raised rate
/// ([`Self::raise_rate`], what [`crate::ThroughputUpgrades`] calls) is in
/// the record, for every replica: a number's rate is the higher of the
/// configured one and the raised one, and the raised one is never lowered.
/// A number Meta slows down for one replica ([`RateLimiter::slow_down`]) is
/// slowed down for all.
///
/// **Clocks.** Each replica books against the shared schedule by its own
/// clock and waits on its own: the schedule is absolute, the waits it
/// returns are relative. This assumes the replicas' clocks are synchronized
/// (NTP). Two replicas whose clocks differ by `skew` may start two sends up
/// to `skew` closer than the rate spaces them, so a one-second window can
/// hold up to `⌈skew × rate⌉` sends more than the rate (at 80 a second, one
/// more for each 12.5 ms of skew); a store round trip spent between reading
/// the time and writing the record counts the same way. Pace below Meta's
/// rate by that headroom. The schedule never moves back: a replica whose
/// clock is behind waits longer (never sends early), and one whose clock
/// stepped forward books slots that every replica then waits for (delete
/// the record, [`Self::store_key`], to drop them).
///
/// **Contention.** A booking reads the record and swaps it on the version
/// read; when another replica wrote in between, it reads again, after a
/// backoff (1 ms doubled each time up to 64 ms, each with a random half of
/// it taken off) and at most [`Self::contention`] times (default 8): then
/// `StorageError::Busy`. One key takes a read and a swap per booking, so it
/// sustains about one booking per two store round trips: at 1,000 a second,
/// the store must answer well within half a millisecond.
///
/// **Failures fail closed.** A store that fails, contention past the bound
/// or a record this release cannot read is an error of
/// [`RateLimiter::reserve`]: nothing is booked and nothing may be sent (a
/// [`crate::Broadcast`] stops, `BroadcastEnd::LimiterFailed`; a
/// [`super::PacedOutbound`] fails the call before sending). It never falls
/// back to a budget of its own, which would let every replica send the
/// whole rate.
///
/// **Expiry.** A record expires [`Self::idle_expiry`] (default 24 hours)
/// after its last slot, forgetting the number's schedule, slow-down and
/// raised rate. Redis drops it itself; with the memory or Postgres store,
/// call its `purge_expired` now and then (as for the other typed stores).
///
/// A slot given back ([`RateLimiter::release`]) is reused only when no
/// replica booked after it; otherwise it stays unused.
#[must_use]
pub struct KvRateLimiter {
    kv: Arc<dyn KvStore>,
    timer: Arc<dyn Timer>,
    rate: Rate,
    rates: RwLock<HashMap<PhoneNumberId, Rate>>,
    burst: u32,
    adaptive: bool,
    factor: u32,
    spacing: Duration,
    recovery: Duration,
    idle: Duration,
    attempts: u32,
    backoff: Duration,
}

impl fmt::Debug for KvRateLimiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KvRateLimiter")
            .field("rate", &self.rate)
            .field("rates", &*self.rates())
            .field("burst", &self.burst)
            .field("adaptive", &self.adaptive)
            .field("slow_down_factor", &self.factor)
            .field("slow_down_spacing", &self.spacing)
            .field("recovery", &self.recovery)
            .field("idle_expiry", &self.idle)
            .field("attempts", &self.attempts)
            .field("backoff", &self.backoff)
            .field("kv", &self.kv)
            .field("timer", &self.timer)
            .finish()
    }
}

impl KvRateLimiter {
    /// Every number at `rate`, its schedule in `kv`: a burst of one,
    /// slow-downs on (halving, at most once a second, back up every 30
    /// quiet seconds), records expiring 24 hours after their last slot, 8
    /// attempts under contention, on the system clock.
    pub fn new(kv: Arc<dyn KvStore>, rate: Rate) -> Self {
        Self {
            kv,
            timer: Arc::new(SystemClock),
            rate,
            rates: RwLock::default(),
            burst: 1,
            adaptive: true,
            factor: 2,
            spacing: Duration::from_secs(1),
            recovery: Duration::from_secs(30),
            idle: Duration::from_hours(24),
            attempts: 8,
            backoff: Duration::from_millis(1),
        }
    }

    /// Read the time from `timer`, and wait on it between attempts under
    /// contention (a `ManualClock` in tests): the pacer's timer.
    pub fn with_timer(mut self, timer: impl Timer) -> Self {
        self.timer = Arc::new(timer);
        self
    }

    /// `number` at `rate` instead of the limiter's own, on this replica
    /// (configure every replica alike).
    pub fn rate_for(self, number: impl Into<PhoneNumberId>, rate: Rate) -> Self {
        self.set_rate(number, rate);
        self
    }

    /// Set `number`'s configured rate on this replica while in use (behind
    /// an `Arc`): a merchant's number onboarded while running. A rate
    /// raised in the store ([`Self::raise_rate`]) still wins when higher.
    pub fn set_rate(&self, number: impl Into<PhoneNumberId>, rate: Rate) {
        self.rates
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(number.into(), rate);
    }

    /// How many sends may start at once after a quiet spell (default 1),
    /// as [`super::TokenBucket::burst`]. Zero is a `ConfigError`.
    pub fn burst(mut self, sends: u32) -> Result<Self> {
        if sends == 0 {
            return Err(ConfigError::new("a burst of zero sends").into());
        }
        self.burst = sends;
        Ok(self)
    }

    /// Whether Meta's throttling slows a number down (default `true`), as
    /// [`super::TokenBucket::adaptive`].
    pub fn adaptive(mut self, on: bool) -> Self {
        self.adaptive = on;
        self
    }

    /// What a slow-down divides the rate by (default 2), as
    /// [`super::TokenBucket::slow_down_factor`]. Below two is a
    /// `ConfigError`.
    pub fn slow_down_factor(mut self, factor: u32) -> Result<Self> {
        if factor < 2 {
            return Err(ConfigError::new(
                "a slow-down factor below two (to keep the rate: `KvRateLimiter::adaptive(false)`)",
            )
            .into());
        }
        self.factor = factor;
        Ok(self)
    }

    /// Slow-downs closer together than this count as one (default 1 s), as
    /// [`super::TokenBucket::slow_down_spacing`]: of every replica.
    pub fn slow_down_spacing(mut self, spacing: Duration) -> Self {
        self.spacing = spacing;
        self
    }

    /// How long a slowed number waits, without another slow-down, before
    /// its rate is multiplied back (default 30 seconds), as
    /// [`super::TokenBucket::recovery`]. Zero is a `ConfigError`.
    pub fn recovery(mut self, period: Duration) -> Result<Self> {
        if period.is_zero() {
            return Err(ConfigError::new(
                "a recovery period of zero (to ignore slow-downs: `KvRateLimiter::adaptive(false)`)",
            )
            .into());
        }
        self.recovery = period;
        Ok(self)
    }

    /// How long a record outlives its last slot (default 24 hours): then
    /// the number's schedule, slow-down and raised rate are forgotten.
    /// Zero is a `ConfigError`.
    pub fn idle_expiry(mut self, idle: Duration) -> Result<Self> {
        if idle.is_zero() {
            return Err(ConfigError::new("a pacer record expiring at its last slot").into());
        }
        self.idle = idle;
        Ok(self)
    }

    /// Under contention, read and swap the record at most `attempts` times
    /// (default 8), waiting `base_delay` (default 1 ms) after the first
    /// conflict, doubled after each up to 64 times it, each wait with a
    /// random part of up to half of it taken off; then fail with
    /// `StorageError::Busy`. Zero attempts or a zero delay (a busy spin) is
    /// a `ConfigError`.
    pub fn contention(mut self, attempts: u32, base_delay: Duration) -> Result<Self> {
        if attempts == 0 {
            return Err(ConfigError::new("zero attempts to book a slot").into());
        }
        if base_delay.is_zero() {
            return Err(ConfigError::new(
                "no wait between attempts to book a slot under contention",
            )
            .into());
        }
        self.attempts = attempts;
        self.backoff = base_delay;
        Ok(self)
    }

    /// The store key of `number`'s record: SHA-256 (hex) of the
    /// length-prefixed phone number id (`<len>:<id>,`, as the cooldowns'
    /// keys), in [`PACER_NAMESPACE`]. Delete it to forget the number's
    /// schedule.
    pub fn store_key(number: &PhoneNumberId) -> StoreKey {
        let mut hasher = Sha256::new();
        hasher.update(format!("{}:", number.as_str().len()));
        hasher.update(number.as_str());
        hasher.update(",");
        StoreKey::new(PACER_NAMESPACE, hex::encode(hasher.finalize()))
    }

    /// `number`'s rate before any slow-down: the higher of this replica's
    /// configured one and the one raised in the store. Fails when the
    /// store does.
    pub async fn rate_of(&self, number: &PhoneNumberId) -> Result<Rate> {
        let configured = self.configured(number);
        let Some(stored) = self.kv.get(&Self::store_key(number)).await? else {
            return Ok(configured);
        };
        let record = Record::decode(&stored.value).map_err(|why| unreadable(&why))?;
        Ok(effective(configured, record.raised))
    }

    /// Raise `number`'s rate to `rate` in the store, for every replica,
    /// unless it is already at or above it: checked on the record read and
    /// written by `compare_and_swap` on its version, so a rate raised
    /// higher meanwhile (by another replica) is never lowered. Whether it
    /// rose. Fails when the store does (nothing raised).
    pub async fn raise_rate(&self, number: &PhoneNumberId, rate: Rate) -> Result<bool> {
        let configured = self.configured(number);
        self.update(number, self.timer.now(), |record, _| {
            if effective(configured, record.raised).get() >= rate.get() {
                return Change::Keep(false);
            }
            record.raised = Some(rate);
            Change::Write(true)
        })
        .await
    }

    fn rates(&self) -> std::sync::RwLockReadGuard<'_, HashMap<PhoneNumberId, Rate>> {
        self.rates.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn configured(&self, number: &PhoneNumberId) -> Rate {
        self.rates().get(number).copied().unwrap_or(self.rate)
    }

    /// Up by one step of the rate per quiet recovery period since the
    /// last slow-down (as the token bucket's, without its step back: a
    /// shared schedule never moves back).
    fn recover(&self, record: &mut Record, now: OffsetDateTime) {
        if record.divisor == 1 {
            return;
        }
        let steps = elapsed(record.since, now).as_nanos() / self.recovery.as_nanos().max(1);
        if steps > 0 {
            let steps = u32::try_from(steps).unwrap_or(u32::MAX);
            for _ in 0..steps.min(32) {
                record.divisor = (record.divisor / self.factor).max(1);
                if record.divisor == 1 {
                    break;
                }
            }
            record.since = later(record.since, self.recovery.saturating_mul(steps));
        }
    }

    /// The wait after the `attempt`-th conflict (from 0): the base delay
    /// doubled `attempt` times (at most 64 times it), less a random part of
    /// up to half of it.
    fn backoff(&self, attempt: u32) -> Duration {
        let full = self.backoff.saturating_mul(1 << attempt.min(6));
        let half = u64::try_from(full.as_nanos() / 2).unwrap_or(u64::MAX);
        let off = RandomState::new().hash_one(attempt) % half.saturating_add(1);
        full.saturating_sub(Duration::from_nanos(off))
    }

    /// Read `number`'s record (a new one when there is none), `change` it
    /// as of the attempt's now, and write it back if it changed: created
    /// with `put_if_absent`, replaced with `compare_and_swap` on the
    /// version read. Another writer in between: again, after a backoff, at
    /// most `attempts` times, then `StorageError::Busy`. A store error or
    /// an unreadable record: that error, nothing written.
    async fn update<R: Send>(
        &self,
        number: &PhoneNumberId,
        first: OffsetDateTime,
        mut change: impl FnMut(&mut Record, OffsetDateTime) -> Change<R> + Send,
    ) -> Result<R> {
        let key = Self::store_key(number);
        for attempt in 0..self.attempts {
            if attempt > 0 {
                self.timer.sleep(self.backoff(attempt - 1)).await;
            }
            // The first attempt books as of the request; a later one as of
            // the time it reads the record again.
            let now = if attempt == 0 {
                first
            } else {
                first.max(self.timer.now())
            };
            let stored = self.kv.get(&key).await?;
            let mut record = match &stored {
                Some(stored) => Record::decode(&stored.value).map_err(|why| unreadable(&why))?,
                None => Record::new(now),
            };
            let answer = match change(&mut record, now) {
                Change::Keep(answer) => return Ok(answer),
                Change::Write(answer) => answer,
            };
            let expiry = Expiry::After(self.idle.saturating_add(elapsed(now, record.due)));
            let written = match stored {
                Some(stored) => {
                    self.kv
                        .compare_and_swap(&key, stored.version, Some(record.encode()), expiry)
                        .await?
                }
                None => self.kv.put_if_absent(&key, record.encode(), expiry).await?,
            };
            if written.is_some() {
                return Ok(answer);
            }
        }
        tracing::warn!(
            attempts = self.attempts,
            "pacer gave up booking: the shared schedule kept changing"
        );
        Err(StorageError::Busy.into())
    }
}

/// The higher of a configured rate and a raised one.
fn effective(configured: Rate, raised: Option<Rate>) -> Rate {
    raised
        .filter(|raised| raised.get() > configured.get())
        .unwrap_or(configured)
}

fn unreadable(why: &str) -> Error {
    StorageError::Backend(anyhow::anyhow!("unreadable pacer record: {why}")).into()
}

#[async_trait]
impl RateLimiter for KvRateLimiter {
    async fn reserve(&self, request: &SlotRequest) -> Result<Reservation> {
        let configured = self.configured(&request.from);
        let cost = request.cost.get();
        let (start, id) = self
            .update(&request.from, request.now, |record, now| {
                self.recover(record, now);
                let interval = effective(configured, record.raised).interval(record.divisor);
                let base = record.due.max(now);
                let tolerance = interval.saturating_mul(self.burst - 1);
                let start = earlier(base, tolerance).max(now);
                record.due = later(base, interval.saturating_mul(cost));
                record.last_id = record.last_id.wrapping_add(1).max(1);
                record.last_base = Some(base);
                Change::Write((start, record.last_id))
            })
            .await?;
        // Relative to the request: the pacer waits from its own now.
        Ok(Reservation::new(elapsed(request.now, start)).with_id(id))
    }

    async fn release(&self, request: &SlotRequest, reservation: &Reservation) -> Result<()> {
        if reservation.id == 0 {
            return Ok(());
        }
        self.update(&request.from, self.timer.now(), |record, _| {
            match record.last_base {
                // Still the last slot booked, by any replica: unbook it.
                Some(base) if record.last_id == reservation.id => {
                    record.due = base;
                    record.last_base = None;
                    Change::Write(())
                }
                _ => Change::Keep(()),
            }
        })
        .await
    }

    async fn slow_down(&self, from: &PhoneNumberId, now: OffsetDateTime) -> Result<()> {
        if !self.adaptive {
            return Ok(());
        }
        let configured = self.configured(from);
        self.update(from, now, |record, now| {
            self.recover(record, now);
            if record
                .slowed
                .is_some_and(|at| elapsed(at, now) < self.spacing)
            {
                return Change::Keep(());
            }
            let rate = effective(configured, record.raised);
            // Divide the rate, never below one message a second.
            if record.divisor.saturating_mul(self.factor) <= rate.get() {
                record.divisor *= self.factor;
            }
            record.slowed = Some(now);
            record.since = now;
            Change::Write(())
        })
        .await?;
        tracing::debug!("pacer slowed a business number down, for every replica");
        Ok(())
    }

    async fn raise_rate(&self, from: &PhoneNumberId, rate: Rate) -> Result<bool> {
        KvRateLimiter::raise_rate(self, from, rate).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use time::macros::datetime;

    /// The namespace and the key derivation are stable identifiers
    /// (`docs/architecture.md` § Stable identifiers): the digest is a known
    /// answer, `printf '15:106540352242922,' | sha256sum`, computed apart
    /// from this code.
    #[test]
    fn the_pacer_key_is_pinned_and_hashed() {
        assert_eq!(PACER_NAMESPACE, "\x77a.bot.pacer");
        let key = KvRateLimiter::store_key(&PhoneNumberId::new("106540352242922").unwrap());
        assert_eq!(key.namespace(), "\x77a.bot.pacer");
        assert_eq!(
            key.key(),
            "8777fb4b5245226544b16947fefcf1e61054edbb812adddee4cf86d44af1d1f9"
        );
        assert!(!key.to_string().contains("106540352242922"));
    }

    /// Version 1's bytes, field by field: a record stored by one release
    /// is read by the next.
    #[test]
    fn the_pacer_record_is_pinned() {
        let record = Record {
            due: datetime!(2026-09-26 12:00:00.5 UTC),
            raised: Some(Rate(1000)),
            divisor: 4,
            since: datetime!(2026-09-26 12:00 UTC),
            slowed: None,
            last_id: 7,
            last_base: Some(datetime!(2026-09-26 12:00:00.25 UTC)),
        };
        let bytes: [u8; RECORD_V1_LEN] = [
            1, // version
            0x18, 0xd8, 0xdd, 0x24, 0x73, 0x22, 0xe5, 0x00, // due
            0, 0, 0x03, 0xe8, // raised: 1000
            0, 0, 0, 4, // divisor
            0x18, 0xd8, 0xdd, 0x24, 0x55, 0x55, 0x80, 0x00, // since
            0x80, 0, 0, 0, 0, 0, 0, 0, // slowed: absent
            0, 0, 0, 0, 0, 0, 0, 7, // last id
            0x18, 0xd8, 0xdd, 0x24, 0x64, 0x3c, 0x32, 0x80, // last base
        ];
        assert_eq!(record.encode(), bytes);
        assert_eq!(Record::decode(&bytes), Ok(record));
        let fresh = Record::new(datetime!(2026-09-26 12:00 UTC));
        assert_eq!(Record::decode(&fresh.encode()), Ok(fresh));
    }

    #[test]
    fn records_this_release_cannot_read_are_refused() {
        let good = Record::new(datetime!(2026-09-26 12:00 UTC)).encode();
        let mut newer = good.clone();
        newer[0] = 2;
        assert!(Record::decode(&newer).unwrap_err().contains("version 2"));
        assert!(Record::decode(&good[..48]).is_err());
        assert!(Record::decode(&[]).is_err());
        assert!(Record::decode(b"1").is_err());
        let mut zero_divisor = good;
        zero_divisor[13..17].copy_from_slice(&0u32.to_be_bytes());
        assert!(Record::decode(&zero_divisor).is_err());
    }

    #[test]
    fn meaningless_zeros_are_config_errors() {
        let kv: Arc<dyn KvStore> = Arc::new(Unused);
        let limiter = || KvRateLimiter::new(kv.clone(), Rate::DEFAULT);
        assert!(matches!(limiter().burst(0), Err(Error::Config(_))));
        assert!(matches!(
            limiter().slow_down_factor(1),
            Err(Error::Config(_))
        ));
        assert!(matches!(
            limiter().recovery(Duration::ZERO),
            Err(Error::Config(_))
        ));
        assert!(matches!(
            limiter().idle_expiry(Duration::ZERO),
            Err(Error::Config(_))
        ));
        assert!(matches!(
            limiter().contention(0, Duration::from_millis(1)),
            Err(Error::Config(_))
        ));
        assert!(matches!(
            limiter().contention(3, Duration::ZERO),
            Err(Error::Config(_))
        ));
        assert!(limiter().contention(1, Duration::from_nanos(1)).is_ok());
    }

    /// Each wait is between half and all of the base delay doubled once per
    /// earlier conflict, up to 64 times it.
    #[test]
    fn the_backoff_doubles_with_jitter_up_to_a_cap() {
        let kv: Arc<dyn KvStore> = Arc::new(Unused);
        let limiter = KvRateLimiter::new(kv, Rate::DEFAULT);
        for attempt in 0..10 {
            let full = Duration::from_millis(1 << attempt.min(6));
            for _ in 0..20 {
                let wait = limiter.backoff(attempt);
                assert!(wait <= full && wait >= full / 2, "{attempt}: {wait:?}");
            }
        }
    }

    #[derive(Debug)]
    struct Unused;

    #[async_trait]
    impl KvStore for Unused {
        async fn get(
            &self,
            _: &StoreKey,
        ) -> std::result::Result<Option<meta_whatsapp_core::store::Versioned>, StorageError>
        {
            unreachable!()
        }
        async fn put(
            &self,
            _: &StoreKey,
            _: Vec<u8>,
            _: Expiry,
        ) -> std::result::Result<u64, StorageError> {
            unreachable!()
        }
        async fn put_if_absent(
            &self,
            _: &StoreKey,
            _: Vec<u8>,
            _: Expiry,
        ) -> std::result::Result<Option<u64>, StorageError> {
            unreachable!()
        }
        async fn compare_and_swap(
            &self,
            _: &StoreKey,
            _: u64,
            _: Option<Vec<u8>>,
            _: Expiry,
        ) -> std::result::Result<Option<u64>, StorageError> {
            unreachable!()
        }
        async fn delete(&self, _: &StoreKey) -> std::result::Result<bool, StorageError> {
            unreachable!()
        }
    }
}
