//! [`Pacer`]: at most so many sends a second from each business number.
//!
//! Meta's throughput (`throughput`): each registered business phone number
//! sends up to 80 messages a second by default ([`Rate::DEFAULT`]), up to
//! 1,000 once Meta upgrades it ([`Rate::HIGHER_THROUGHPUT`]), and a fixed 20
//! when it is also used in the WhatsApp Business app
//! ([`Rate::BUSINESS_APP`]). Throughput counts inbound and outbound
//! messages of every type; past it the API answers `130429`
//! (`ErrorKind::RateLimited`). Which rate a number has is yours to set:
//! Meta reports a number's level (`PhoneNumberInfo::throughput`) and a
//! `phone_number_quality_update` webhook when it rises
//! (`PhoneNumberQualityEvent::ThroughputUpgrade`), but documents no mapping
//! from a level to messages a second.
//!
//! The pacer is two decisions behind traits:
//!
//! | Decision | Trait | Default |
//! | --- | --- | --- |
//! | how many sends a number may start, and when | [`RateLimiter`] | [`TokenBucket`] (in this process) |
//! | what "now" is and how to wait | [`Timer`] (a `Clock` that can sleep) | `SystemClock` (Tokio's sleep) |
//!
//! **One budget per process.** [`TokenBucket`] counts the sends of this
//! process only: two replicas at 80 a second each send 160 a second from
//! one number. Give each replica its share of the rate
//! (`Rate::per_second(80 / replicas)`), or plug in a [`RateLimiter`] that
//! keeps its schedule in a store every replica shares (none ships: a
//! reservation is one atomic read-modify-write, see [`TokenBucket`]).
//!
//! What goes through the pacer is what calls it: a [`Broadcast`] reserves
//! a slot before each send (retries included); [`PacedOutbound`] (set on a
//! bot with `BotBuilder::pacer`) before each reply, refusal, read receipt
//! and typing indicator; and any other call, a group operation for one,
//! after [`Pacer::acquire`]. Meta documents no rate for group operations:
//! pacing them through the same budget is a choice, not a rule of Meta's.
//! The client's own calls are not paced by themselves.
//!
//! [`Broadcast`]: crate::Broadcast

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_client::messages::{OutboundMessage, SendResponse};
use meta_whatsapp_core::Result;
use meta_whatsapp_core::clock::{Clock, ManualClock, SystemClock};
use meta_whatsapp_core::error::ConfigError;
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use time::OffsetDateTime;

use crate::outbound::Outbound;

/// A business number's send rate, in messages a second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rate(u32);

impl Rate {
    /// 80 a second: every registered business number's throughput by
    /// default (`throughput`).
    pub const DEFAULT: Self = Self(80);

    /// 1,000 a second: a number Meta upgraded to higher throughput
    /// (`throughput`, "Higher throughput").
    pub const HIGHER_THROUGHPUT: Self = Self(1000);

    /// 20 a second: a number used in both the WhatsApp Business app and the
    /// Cloud API (`throughput`, "WhatsApp Business app phone numbers").
    pub const BUSINESS_APP: Self = Self(20);

    /// `messages` a second. Zero is a `ConfigError`.
    pub fn per_second(messages: u32) -> Result<Self> {
        if messages == 0 {
            return Err(ConfigError::new("a send rate of zero messages a second").into());
        }
        Ok(Self(messages))
    }

    /// Messages a second.
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The time between two sends at this rate divided by `divisor`,
    /// rounded up to the nanosecond, so that a second never holds one send
    /// more than the rate (at 3 a second, 333,333,334 ns).
    fn interval(self, divisor: u32) -> Duration {
        const NANOS_PER_SECOND: u64 = 1_000_000_000;
        Duration::from_nanos(
            NANOS_PER_SECOND
                .saturating_mul(u64::from(divisor.max(1)))
                .div_ceil(u64::from(self.0.max(1))),
        )
    }
}

impl Default for Rate {
    /// [`Rate::DEFAULT`].
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Decides when each send of a business number may start.
///
/// A reservation, not a permit: [`RateLimiter::reserve`] books the next
/// slot and says how long to wait for it, so the caller waits on its own
/// [`Timer`] (and can stop waiting when a broadcast is cancelled). The
/// default is [`TokenBucket`], in this process. A limiter shared by several
/// replicas (a Redis script, a row in a database) implements the same two
/// methods; it may take "now" from its own server and ignore `now`, as
/// long as the wait it returns is relative.
#[async_trait]
pub trait RateLimiter: Send + Sync + fmt::Debug + 'static {
    /// Book the next send of `from`, as of `now`, and say how long to wait
    /// before starting it (zero: at once). Every call books one send: a
    /// caller that then does not send leaves that slot unused.
    async fn reserve(&self, from: &PhoneNumberId, now: OffsetDateTime) -> Result<Duration>;

    /// Meta refused a send of `from` for going too fast (a broadcast calls
    /// it on the errors its `BroadcastPolicy::slows_down` names: by default
    /// the `RateLimited` codes, `130429` among them, and `131048`): send
    /// slower for a while.
    async fn slow_down(&self, from: &PhoneNumberId, now: OffsetDateTime) -> Result<()>;
}

#[async_trait]
impl<T: RateLimiter + ?Sized> RateLimiter for Arc<T> {
    async fn reserve(&self, from: &PhoneNumberId, now: OffsetDateTime) -> Result<Duration> {
        (**self).reserve(from, now).await
    }

    async fn slow_down(&self, from: &PhoneNumberId, now: OffsetDateTime) -> Result<()> {
        (**self).slow_down(from, now).await
    }
}

/// Slow-downs closer together than this count as one: every send in
/// flight when Meta starts refusing gets the same error.
const SLOW_DOWN_SPACING: Duration = Duration::from_secs(1);

/// One number's schedule.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    /// When the next send is due at the current rate (the generic cell
    /// rate algorithm's theoretical arrival time).
    due: OffsetDateTime,
    /// The latest `now` seen: an earlier one means the clock stepped back.
    seen: OffsetDateTime,
    /// The rate is divided by this (1, 2, 4, …) after slow-downs.
    divisor: u32,
    /// The last slow-down, or the last step back up since: the next step
    /// up is one recovery period after it.
    since: OffsetDateTime,
    /// The last slow-down that counted.
    slowed: Option<OffsetDateTime>,
}

impl Bucket {
    fn new(now: OffsetDateTime) -> Self {
        Self {
            due: now,
            seen: now,
            divisor: 1,
            since: now,
            slowed: None,
        }
    }
}

/// The default [`RateLimiter`]: a token bucket per business number, in this
/// process.
///
/// Each number sends at its [`Rate`] ([`TokenBucket::rate_for`], else the
/// bucket's own), evenly spaced: with the default burst of one, any
/// one-second window holds at most the rate's number of sends, whatever
/// Meta's window is. [`TokenBucket::burst`] lets up to that many go at
/// once after a quiet spell, and so up to `burst - 1` more than the rate
/// in some one-second window.
///
/// When Meta says a number goes too fast ([`RateLimiter::slow_down`]), its
/// rate is halved, at most once a second and never below one message a
/// second; each [`TokenBucket::recovery`] period without another slow-down
/// doubles it back, up to its configured rate.
///
/// The schedule survives a wall clock stepping back (the `Clock` is
/// wall time): it moves back with it instead of pausing the number for
/// the difference.
#[must_use]
pub struct TokenBucket {
    rate: Rate,
    rates: HashMap<PhoneNumberId, Rate>,
    burst: u32,
    recovery: Duration,
    buckets: Mutex<HashMap<PhoneNumberId, Bucket>>,
}

impl fmt::Debug for TokenBucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenBucket")
            .field("rate", &self.rate)
            .field("rates", &self.rates)
            .field("burst", &self.burst)
            .field("recovery", &self.recovery)
            .finish_non_exhaustive()
    }
}

impl Default for TokenBucket {
    /// [`Rate::DEFAULT`] for every number.
    fn default() -> Self {
        Self::new(Rate::DEFAULT)
    }
}

impl TokenBucket {
    /// Every number at `rate`, a burst of one, a 30-second recovery.
    pub fn new(rate: Rate) -> Self {
        Self {
            rate,
            rates: HashMap::new(),
            burst: 1,
            recovery: Duration::from_secs(30),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// `number` at `rate` instead of the bucket's own: a number Meta
    /// upgraded ([`Rate::HIGHER_THROUGHPUT`]), or one shared with the
    /// WhatsApp Business app ([`Rate::BUSINESS_APP`]).
    pub fn rate_for(mut self, number: impl Into<PhoneNumberId>, rate: Rate) -> Self {
        self.rates.insert(number.into(), rate);
        self
    }

    /// How many sends may start at once after a quiet spell (default 1;
    /// zero counts as one). Above one, a one-second window can hold up to
    /// `sends - 1` more than the rate.
    pub fn burst(mut self, sends: u32) -> Self {
        self.burst = sends.max(1);
        self
    }

    /// How long a slowed number waits, without another slow-down, before
    /// its rate doubles back (default 30 seconds; zero: slow-downs are
    /// ignored).
    pub fn recovery(mut self, period: Duration) -> Self {
        self.recovery = period;
        self
    }

    /// The configured rate of `number` (before any slow-down).
    pub fn rate_of(&self, number: &PhoneNumberId) -> Rate {
        self.rates.get(number).copied().unwrap_or(self.rate)
    }

    fn buckets(&self) -> MutexGuard<'_, HashMap<PhoneNumberId, Bucket>> {
        self.buckets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Follow the clock to `now`: back with it if it stepped back, and
    /// up by one doubling of the rate per quiet recovery period.
    fn catch_up(&self, bucket: &mut Bucket, now: OffsetDateTime) {
        if now < bucket.seen {
            let back = bucket.seen - now;
            bucket.due = bucket.due.saturating_sub(back);
            bucket.since = bucket.since.saturating_sub(back);
            bucket.slowed = bucket.slowed.map(|at| at.saturating_sub(back));
        }
        bucket.seen = now;
        if bucket.divisor == 1 {
            return;
        }
        if self.recovery.is_zero() {
            bucket.divisor = 1;
            return;
        }
        let steps = elapsed(bucket.since, now).as_nanos() / self.recovery.as_nanos();
        if steps > 0 {
            let steps = u32::try_from(steps).unwrap_or(u32::MAX).min(31);
            bucket.divisor = (bucket.divisor >> steps).max(1);
            bucket.since = later(bucket.since, self.recovery.saturating_mul(steps));
        }
    }
}

#[async_trait]
impl RateLimiter for TokenBucket {
    async fn reserve(&self, from: &PhoneNumberId, now: OffsetDateTime) -> Result<Duration> {
        let rate = self.rate_of(from);
        let mut buckets = self.buckets();
        let bucket = buckets
            .entry(from.clone())
            .or_insert_with(|| Bucket::new(now));
        self.catch_up(bucket, now);
        let interval = rate.interval(bucket.divisor);
        let due = bucket.due.max(now);
        let tolerance = interval.saturating_mul(self.burst - 1);
        let start = earlier(due, tolerance).max(now);
        bucket.due = later(due, interval);
        Ok(elapsed(now, start))
    }

    async fn slow_down(&self, from: &PhoneNumberId, now: OffsetDateTime) -> Result<()> {
        if self.recovery.is_zero() {
            return Ok(());
        }
        let rate = self.rate_of(from);
        let mut buckets = self.buckets();
        let bucket = buckets
            .entry(from.clone())
            .or_insert_with(|| Bucket::new(now));
        self.catch_up(bucket, now);
        if bucket
            .slowed
            .is_some_and(|at| elapsed(at, now) < SLOW_DOWN_SPACING)
        {
            return Ok(());
        }
        // Halve the rate, never below one message a second.
        if bucket.divisor.saturating_mul(2) <= rate.get() {
            bucket.divisor *= 2;
        }
        bucket.slowed = Some(now);
        bucket.since = now;
        tracing::debug!(
            divisor = bucket.divisor,
            "pacer slowed a business number down"
        );
        Ok(())
    }
}

/// `at + by`, saturating.
fn later(at: OffsetDateTime, by: Duration) -> OffsetDateTime {
    at.saturating_add(time::Duration::try_from(by).unwrap_or(time::Duration::MAX))
}

/// `at - by`, saturating.
fn earlier(at: OffsetDateTime, by: Duration) -> OffsetDateTime {
    at.saturating_sub(time::Duration::try_from(by).unwrap_or(time::Duration::MAX))
}

/// `to - from`, or zero when `to` is not after `from`.
pub(crate) fn elapsed(from: OffsetDateTime, to: OffsetDateTime) -> Duration {
    Duration::try_from(to - from).unwrap_or(Duration::ZERO)
}

/// `at + by`, saturating (for the broadcast's deferrals).
pub(crate) fn after(at: OffsetDateTime, by: Duration) -> OffsetDateTime {
    later(at, by)
}

/// A [`Clock`] that can wait: what the pacer reads "now" from and sleeps
/// on, so a test drives both with one fake clock.
///
/// Shipped for the core's two clocks: `SystemClock` sleeps with Tokio
/// (a Tokio runtime with its time driver must be running, as for the
/// client's retries), and `ManualClock` moves itself forward by the
/// duration, at once, so a test with a `ManualClock` runs a whole paced
/// broadcast without waiting and reads the times each send started.
#[async_trait]
pub trait Timer: Clock {
    /// Wait `duration`: afterwards, `now()` is at least that much later.
    async fn sleep(&self, duration: Duration);
}

#[async_trait]
impl Timer for SystemClock {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

#[async_trait]
impl Timer for ManualClock {
    /// Advances the clock by `duration`, without waiting.
    async fn sleep(&self, duration: Duration) {
        self.advance(duration);
    }
}

#[async_trait]
impl<T: Timer + ?Sized> Timer for Arc<T> {
    async fn sleep(&self, duration: Duration) {
        (**self).sleep(duration).await;
    }
}

/// Paces the sends of each business number: a [`RateLimiter`] (what may
/// start when) and a [`Timer`] (now, and the wait). See the
/// [module docs](self).
///
/// Cheap to clone; clones share the limiter. Share one per process across
/// every broadcast and bot of a number, or each gets a budget of its own.
#[derive(Clone)]
pub struct Pacer {
    limiter: Arc<dyn RateLimiter>,
    timer: Arc<dyn Timer>,
}

impl fmt::Debug for Pacer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pacer")
            .field("limiter", &self.limiter)
            .field("timer", &self.timer)
            .finish()
    }
}

impl Default for Pacer {
    /// A [`TokenBucket`] at [`Rate::DEFAULT`] for every number, on the
    /// system clock.
    fn default() -> Self {
        Self::new(TokenBucket::default())
    }
}

impl Pacer {
    /// Pace with `limiter`, on the system clock.
    pub fn new(limiter: impl RateLimiter) -> Self {
        Self::shared(Arc::new(limiter))
    }

    /// Pace with a limiter shared with other code.
    pub fn shared(limiter: Arc<dyn RateLimiter>) -> Self {
        Self {
            limiter,
            timer: Arc::new(SystemClock),
        }
    }

    /// Read the time from `timer` and wait on it (a `ManualClock` in tests).
    #[must_use]
    pub fn with_timer(mut self, timer: impl Timer) -> Self {
        self.timer = Arc::new(timer);
        self
    }

    /// The limiter.
    pub fn limiter(&self) -> &Arc<dyn RateLimiter> {
        &self.limiter
    }

    /// Wait for a slot of `from`: before a call you want counted in its
    /// budget, such as a group operation. Fails only when the limiter does
    /// (a shared one unreachable), before any wait.
    pub async fn acquire(&self, from: &PhoneNumberId) -> Result<()> {
        let wait = self.reserve(from).await?;
        self.sleep(wait).await;
        Ok(())
    }

    /// Tell the limiter Meta refused a send of `from` for going too fast.
    pub async fn slow_down(&self, from: &PhoneNumberId) -> Result<()> {
        self.limiter.slow_down(from, self.timer.now()).await
    }

    /// Book a slot of `from`: how long to wait for it.
    pub(crate) async fn reserve(&self, from: &PhoneNumberId) -> Result<Duration> {
        self.limiter.reserve(from, self.timer.now()).await
    }

    /// The timer's now.
    pub(crate) fn now(&self) -> OffsetDateTime {
        self.timer.now()
    }

    /// Wait on the timer (nothing for zero).
    pub(crate) async fn sleep(&self, duration: Duration) {
        if !duration.is_zero() {
            self.timer.sleep(duration).await;
        }
    }
}

/// An [`Outbound`] that waits for a slot of the business number before
/// every send and read receipt (typing indicators included), then hands
/// it to the outbound it wraps. `BotBuilder::pacer` puts a bot's outbound
/// in one, so `MarkRead`, the replies and the refusals share the number's
/// budget with its broadcasts.
///
/// A limiter that fails fails the call, before anything is sent.
///
/// One slot per call: over a [`crate::ClientOutbound`], the client's own
/// replays of a throttled request (its `RetryPolicy`) happen inside the
/// call, without a slot of their own. A client with `RetryPolicy::NONE`
/// (`Client::with_retry`) makes every request wait for one.
#[derive(Debug, Clone)]
pub struct PacedOutbound {
    inner: Arc<dyn Outbound>,
    pacer: Pacer,
}

impl PacedOutbound {
    /// Pace `inner` with `pacer`.
    pub fn new(inner: impl Outbound, pacer: Pacer) -> Self {
        Self::shared(Arc::new(inner), pacer)
    }

    /// Pace an outbound shared with other code.
    pub fn shared(inner: Arc<dyn Outbound>, pacer: Pacer) -> Self {
        Self { inner, pacer }
    }

    /// The pacer.
    pub fn pacer(&self) -> &Pacer {
        &self.pacer
    }
}

#[async_trait]
impl Outbound for PacedOutbound {
    async fn send(&self, from: &PhoneNumberId, message: &OutboundMessage) -> Result<SendResponse> {
        self.pacer.acquire(from).await?;
        self.inner.send(from, message).await
    }

    async fn mark_read(
        &self,
        from: &PhoneNumberId,
        message_id: &MessageId,
        typing_indicator: bool,
    ) -> Result<()> {
        self.pacer.acquire(from).await?;
        self.inner
            .mark_read(from, message_id, typing_indicator)
            .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use time::macros::datetime;

    const T0: OffsetDateTime = datetime!(2026-09-26 12:00 UTC);

    fn number() -> PhoneNumberId {
        PhoneNumberId::new("106540352242922")
    }

    /// Book `n` sends on `pacer`, each after waiting for its slot; the
    /// clock times they start.
    async fn starts(pacer: &Pacer, clock: &ManualClock, n: usize) -> Vec<Duration> {
        let mut out = Vec::new();
        for _ in 0..n {
            pacer.acquire(&number()).await.unwrap();
            out.push(elapsed(T0, clock.now()));
        }
        out
    }

    fn most_in_one_second(starts: &[Duration]) -> usize {
        starts
            .iter()
            .map(|t| {
                starts
                    .iter()
                    .filter(|s| **s >= *t && **s < *t + Duration::from_secs(1))
                    .count()
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn documented_rates_and_rounding() {
        assert_eq!(Rate::DEFAULT.get(), 80);
        assert_eq!(Rate::HIGHER_THROUGHPUT.get(), 1000);
        assert_eq!(Rate::BUSINESS_APP.get(), 20);
        assert_eq!(Rate::default(), Rate::DEFAULT);
        assert!(Rate::per_second(0).is_err());
        assert_eq!(
            Rate::per_second(80).unwrap().interval(1),
            Duration::from_micros(12_500)
        );
        // Rounded up: three sends at 3/s never fit in one second.
        assert_eq!(
            Rate::per_second(3).unwrap().interval(1),
            Duration::from_nanos(333_333_334)
        );
        assert_eq!(
            Rate::per_second(3).unwrap().interval(2),
            Duration::from_nanos(666_666_667)
        );
    }

    /// A rate that does not divide a second never lets one more in.
    #[tokio::test]
    async fn uneven_rates_never_exceed_the_window() {
        for per_second in [3, 7, 80] {
            let clock = ManualClock::new(T0);
            let pacer = Pacer::new(TokenBucket::new(Rate::per_second(per_second).unwrap()))
                .with_timer(clock.clone());
            let starts = starts(&pacer, &clock, 3 * per_second as usize).await;
            assert_eq!(
                most_in_one_second(&starts),
                per_second as usize,
                "{per_second}/s"
            );
        }
    }

    #[tokio::test]
    async fn a_burst_goes_at_once_then_the_rate_holds() {
        let clock = ManualClock::new(T0);
        let pacer = Pacer::new(TokenBucket::new(Rate::per_second(10).unwrap()).burst(5))
            .with_timer(clock.clone());
        let starts = starts(&pacer, &clock, 7).await;
        let ms: Vec<u128> = starts.iter().map(Duration::as_millis).collect();
        assert_eq!(ms, [0, 0, 0, 0, 0, 100, 200]);
        // Up to `burst - 1` more than the rate in one second.
        let starts = self::starts(&pacer, &clock, 30).await;
        assert!(most_in_one_second(&starts) <= 10 + 4);
    }

    #[tokio::test]
    async fn numbers_have_their_own_rates_and_budgets() {
        let clock = ManualClock::new(T0);
        let other = PhoneNumberId::new("2");
        let bucket = TokenBucket::new(Rate::per_second(10).unwrap())
            .rate_for(other.clone(), Rate::per_second(2).unwrap());
        assert_eq!(bucket.rate_of(&other).get(), 2);
        assert_eq!(bucket.rate_of(&number()).get(), 10);
        let now = clock.now();
        assert_eq!(
            bucket.reserve(&number(), now).await.unwrap(),
            Duration::ZERO
        );
        assert_eq!(
            bucket.reserve(&number(), now).await.unwrap(),
            Duration::from_millis(100)
        );
        // Another number is not behind the first one's queue.
        assert_eq!(bucket.reserve(&other, now).await.unwrap(), Duration::ZERO);
        assert_eq!(
            bucket.reserve(&other, now).await.unwrap(),
            Duration::from_millis(500)
        );
    }

    #[tokio::test]
    async fn a_slow_down_halves_the_rate_until_a_quiet_recovery() {
        let clock = ManualClock::new(T0);
        let bucket = Arc::new(TokenBucket::new(Rate::per_second(10).unwrap()));
        let pacer = Pacer::shared(bucket.clone()).with_timer(clock.clone());
        pacer.acquire(&number()).await.unwrap(); // t = 0
        pacer.slow_down(&number()).await.unwrap();
        // Every send in flight reports it: one halving a second at most.
        pacer.slow_down(&number()).await.unwrap();
        let start = clock.now();
        pacer.acquire(&number()).await.unwrap(); // due at 0.1 s
        pacer.acquire(&number()).await.unwrap(); // then 0.2 s apart
        assert_eq!(elapsed(start, clock.now()), Duration::from_millis(300));

        // 30 quiet seconds after the slow-down: back to 10/s.
        clock.set(later(T0, Duration::from_secs(40)));
        let start = clock.now();
        pacer.acquire(&number()).await.unwrap();
        pacer.acquire(&number()).await.unwrap();
        assert_eq!(elapsed(start, clock.now()), Duration::from_millis(100));
    }

    #[tokio::test]
    async fn slow_downs_stop_at_one_a_second() {
        let clock = ManualClock::new(T0);
        let bucket = TokenBucket::new(Rate::per_second(4).unwrap());
        for s in 0..5 {
            bucket
                .slow_down(&number(), later(T0, Duration::from_secs(s)))
                .await
                .unwrap();
        }
        let now = later(T0, Duration::from_secs(5));
        clock.set(now);
        bucket.reserve(&number(), now).await.unwrap();
        assert_eq!(
            bucket.reserve(&number(), now).await.unwrap(),
            Duration::from_secs(1)
        );
    }

    #[tokio::test]
    async fn a_zero_recovery_ignores_slow_downs() {
        let bucket = TokenBucket::new(Rate::per_second(10).unwrap()).recovery(Duration::ZERO);
        bucket.slow_down(&number(), T0).await.unwrap();
        bucket.reserve(&number(), T0).await.unwrap();
        assert_eq!(
            bucket.reserve(&number(), T0).await.unwrap(),
            Duration::from_millis(100)
        );
    }

    /// A wall clock stepping back an hour does not pause the number for an
    /// hour.
    #[tokio::test]
    async fn a_clock_stepping_back_moves_the_schedule_with_it() {
        let bucket = TokenBucket::new(Rate::per_second(10).unwrap());
        bucket.reserve(&number(), T0).await.unwrap();
        let back = earlier(T0, Duration::from_secs(3600));
        assert_eq!(
            bucket.reserve(&number(), back).await.unwrap(),
            Duration::from_millis(100)
        );
    }

    /// The wait before the second of two sends booked at `now`: the
    /// number's interval at its current rate.
    async fn interval_at(bucket: &TokenBucket, now: OffsetDateTime) -> Duration {
        bucket.reserve(&number(), now).await.unwrap();
        bucket.reserve(&number(), now).await.unwrap()
    }

    /// Each quiet recovery period doubles the rate once, and the time spent
    /// toward the next doubling is kept.
    #[tokio::test]
    async fn recovery_steps_count_every_quiet_period() {
        let s = |secs: u64| later(T0, Duration::from_secs(secs));
        // Three slow-downs: 8/s becomes 1/s.
        let bucket = TokenBucket::new(Rate::per_second(8).unwrap());
        for at in [0, 1, 2] {
            bucket.slow_down(&number(), s(at)).await.unwrap();
        }
        assert_eq!(interval_at(&bucket, s(3)).await, Duration::from_secs(1));
        // Three quiet periods later: three doublings, back to 8/s.
        assert_eq!(
            interval_at(&bucket, s(2 + 90)).await,
            Duration::from_millis(125)
        );

        // Two slow-downs (8/s → 2/s); 45 s after the last, one doubling
        // (4/s) and 15 s toward the next, which comes 15 s later.
        let bucket = TokenBucket::new(Rate::per_second(8).unwrap());
        for at in [0, 1] {
            bucket.slow_down(&number(), s(at)).await.unwrap();
        }
        assert_eq!(
            interval_at(&bucket, s(46)).await,
            Duration::from_millis(250)
        );
        assert_eq!(
            interval_at(&bucket, later(s(61), Duration::from_millis(500))).await,
            Duration::from_millis(125)
        );
    }

    /// A wall clock stepping back moves the recovery with it: an hour back
    /// does not keep a slowed number slow for an hour.
    #[tokio::test]
    async fn a_clock_stepping_back_moves_the_recovery_with_it() {
        let bucket = TokenBucket::new(Rate::per_second(8).unwrap());
        bucket.slow_down(&number(), T0).await.unwrap();
        let back = earlier(T0, Duration::from_secs(3600));
        bucket.reserve(&number(), back).await.unwrap();
        assert_eq!(
            interval_at(&bucket, later(back, Duration::from_secs(31))).await,
            Duration::from_millis(125)
        );
    }

    /// ... and the last slow-down with it: a throttle two seconds after the
    /// step back is not taken for one in the same second as the last.
    #[tokio::test]
    async fn a_clock_stepping_back_moves_the_last_slow_down_with_it() {
        let bucket = TokenBucket::new(Rate::per_second(8).unwrap());
        bucket.slow_down(&number(), T0).await.unwrap();
        let back = earlier(T0, Duration::from_secs(3600));
        bucket.reserve(&number(), back).await.unwrap();
        let two_later = later(back, Duration::from_secs(2));
        bucket.slow_down(&number(), two_later).await.unwrap();
        assert_eq!(
            interval_at(&bucket, two_later).await,
            Duration::from_millis(500)
        );
    }

    #[tokio::test]
    async fn a_burst_of_zero_counts_as_one() {
        let bucket = TokenBucket::new(Rate::per_second(10).unwrap()).burst(0);
        assert_eq!(bucket.reserve(&number(), T0).await.unwrap(), Duration::ZERO);
        assert_eq!(
            bucket.reserve(&number(), T0).await.unwrap(),
            Duration::from_millis(100)
        );
    }

    #[tokio::test]
    async fn a_manual_clock_sleeps_by_moving_forward() {
        let clock = ManualClock::new(T0);
        Timer::sleep(&clock, Duration::from_millis(1500)).await;
        assert_eq!(clock.now(), later(T0, Duration::from_millis(1500)));
    }
}
