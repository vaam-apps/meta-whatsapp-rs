//! [`Pacer`]: at most so many sends a second from each business number.
//!
//! Meta's throughput (`throughput`): each registered business phone number
//! sends up to 80 messages a second by default ([`Rate::DEFAULT`]), up to
//! 1,000 once Meta upgrades it ([`Rate::HIGHER_THROUGHPUT`]), and a fixed 20
//! when it is also used in the WhatsApp Business app
//! ([`Rate::BUSINESS_APP`]); past it the API answers `130429`
//! (`ErrorKind::RateLimited`). Which rate a number has is yours to set, and
//! to change while running ([`TokenBucket::set_rate`]): Meta reports a
//! number's level (`PhoneNumberInfo::throughput`) and a
//! `phone_number_quality_update` webhook when it rises
//! (`PhoneNumberQualityEvent::ThroughputUpgrade`), but documents no mapping
//! from a level to messages a second.
//!
//! **Leave headroom for inbound messages.** Meta's throughput is
//! "inclusive of inbound and outbound messages and all message types"
//! (`throughput`), and a pacer sees only what this process sends: a
//! campaign that draws replies shares the number's rate with them. Pace
//! below the rate by the inbound traffic you expect (at 80 a second and
//! one reply for every four messages, `Rate::per_second(64)`).
//!
//! The pacer is three decisions behind traits:
//!
//! | Decision | Trait | Default |
//! | --- | --- | --- |
//! | how many sends a number may start, and when | [`RateLimiter`] | [`TokenBucket`] (in this process) |
//! | which errors mean the number sends too fast | [`SlowDownRule`] | [`ThrottlingErrors`] |
//! | what "now" is and how to wait | [`Timer`] (a `Clock` that can sleep) | `SystemClock` (Tokio's sleep) |
//!
//! **One budget per process.** [`TokenBucket`] counts the sends of this
//! process only: two replicas at 80 a second each send 160 a second from
//! one number. Give each replica its share of the rate
//! (`Rate::per_second(80 / replicas)`), or plug in a [`RateLimiter`] that
//! keeps its schedule in a store every replica shares (none ships yet:
//! roadmap B2b; a reservation is one atomic read-modify-write, see
//! [`TokenBucket`]).
//!
//! What goes through the pacer is what calls it: a [`Broadcast`] reserves
//! a slot before each send (retries included); [`PacedOutbound`] (set on a
//! bot with `BotBuilder::pacer`) before each reply, refusal, read receipt
//! and typing indicator; [`PacedGroups`] and [`PacedGroup`] before each of
//! the client's group operations; and any other call after
//! [`Pacer::acquire`] (in a command handler, the bot's pacer is
//! `Ctx::pacer`). Meta documents no rate for group operations: pacing them
//! through the same budget is a choice, not a rule of Meta's. Each call
//! costs one slot ([`SlotRequest::cost`]): Meta documents no lower cost for
//! a read receipt or a typing indicator. The client's own calls are not
//! paced by themselves, nor are its own retries: whatever retries under a
//! pacer must not retry inside the call ([`PacedOutbound`]).
//!
//! **Zero.** A setting where zero means nothing is refused with a
//! `ConfigError` when given, before anything runs: `Rate::per_second(0)`,
//! [`TokenBucket::burst`], [`TokenBucket::recovery`],
//! [`TokenBucket::slow_down_factor`] (below two), `Backoff::max_attempts`
//! and a broadcast's concurrency (at `BroadcastBuilder::build`). Where
//! zero means something, the setter says what:
//! [`TokenBucket::slow_down_spacing`], `Backoff::base_delay` and
//! `Backoff::max_delay`.
//!
//! [`Broadcast`]: crate::Broadcast
//! [`PacedGroups`]: crate::PacedGroups
//! [`PacedGroup`]: crate::PacedGroup

use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_client::RetryPolicy;
use meta_whatsapp_client::messages::{OutboundMessage, SendResponse};
use meta_whatsapp_core::clock::{Clock, ManualClock, SystemClock};
use meta_whatsapp_core::error::ConfigError;
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_core::{Error, ErrorKind, Result};
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
    ///
    /// At this rate a broadcast's concurrency is what caps it: it sends
    /// about `concurrency ÷ a send's duration` a second (the default 32 at
    /// 400 ms a send: 80). Raise `BroadcastBuilder::concurrency` to about
    /// `1,000 × a send's duration` (400 at 400 ms) to reach it.
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

    /// The time between two sends at this rate divided by `divisor` (never
    /// below one a second), rounded up to the nanosecond, so that a second
    /// never holds one send more than the rate (at 3 a second,
    /// 333,333,334 ns).
    fn interval(self, divisor: u32) -> Duration {
        const NANOS_PER_SECOND: u64 = 1_000_000_000;
        let rate = self.0.max(1);
        Duration::from_nanos(
            NANOS_PER_SECOND
                .saturating_mul(u64::from(divisor.clamp(1, rate)))
                .div_ceil(u64::from(rate)),
        )
    }
}

impl Default for Rate {
    /// [`Rate::DEFAULT`].
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A request for a slot of a business number's budget
/// ([`RateLimiter::reserve`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SlotRequest {
    /// The business number whose budget the call takes.
    pub from: PhoneNumberId,
    /// When the request is made, on the pacer's [`Timer`]. A limiter
    /// shared by replicas may read its own server's time instead, as long
    /// as the wait it returns is relative.
    pub now: OffsetDateTime,
    /// How many sends of the budget the call takes: one for anything this
    /// crate paces (a message, a read receipt, a typing indicator, a group
    /// operation), since Meta counts "all message types" and documents no
    /// lower cost for any of them.
    pub cost: NonZeroU32,
}

impl SlotRequest {
    /// A request of `from`'s budget at `now`, costing one send.
    pub fn new(from: impl Into<PhoneNumberId>, now: OffsetDateTime) -> Self {
        Self {
            from: from.into(),
            now,
            cost: NonZeroU32::MIN,
        }
    }

    /// This request, costing `cost` sends.
    #[must_use]
    pub fn with_cost(mut self, cost: NonZeroU32) -> Self {
        self.cost = cost;
        self
    }
}

/// A slot a [`RateLimiter`] booked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Reservation {
    /// How long to wait, from the request's `now`, before starting the
    /// call (zero: at once).
    pub wait: Duration,
    /// The limiter's own name for the slot, handed back to it by
    /// [`RateLimiter::release`] (zero from a limiter that gives nothing
    /// back).
    pub id: u64,
}

impl Reservation {
    /// A slot `wait` away, with no id.
    pub const fn new(wait: Duration) -> Self {
        Self { wait, id: 0 }
    }

    /// This reservation, named `id` for [`RateLimiter::release`].
    #[must_use]
    pub const fn with_id(mut self, id: u64) -> Self {
        self.id = id;
        self
    }
}

/// Decides when each send of a business number may start.
///
/// A reservation, not a permit: [`RateLimiter::reserve`] books the next
/// slot and says how long to wait for it, so the caller waits on its own
/// [`Timer`] (and can stop waiting when a broadcast is cancelled, handing
/// the slot back with [`RateLimiter::release`]). The default is
/// [`TokenBucket`], in this process. A limiter shared by several replicas
/// (a Redis script, a row in a database) implements the same methods.
#[async_trait]
pub trait RateLimiter: Send + Sync + fmt::Debug + 'static {
    /// Book the next slot of `request.from` for `request.cost` sends, as of
    /// `request.now`, and say how long to wait before starting. Every call
    /// books: a caller that then does not send hands the slot back with
    /// [`Self::release`], or leaves it unused.
    async fn reserve(&self, request: &SlotRequest) -> Result<Reservation>;

    /// The caller of `request` will not use `reservation` (a broadcast
    /// cancelled while it waited): give it back if the limiter can do so
    /// without letting any one-second window hold more than the rate. The
    /// default gives nothing back.
    async fn release(&self, request: &SlotRequest, reservation: &Reservation) -> Result<()> {
        let _ = (request, reservation);
        Ok(())
    }

    /// Meta refused a send of `from` for going too fast (the errors the
    /// pacer's [`SlowDownRule`] names: by default the `RateLimited` codes,
    /// `130429` among them, `131048` and `131057`): send slower for a
    /// while.
    async fn slow_down(&self, from: &PhoneNumberId, now: OffsetDateTime) -> Result<()>;
}

#[async_trait]
impl<T: RateLimiter + ?Sized> RateLimiter for Arc<T> {
    async fn reserve(&self, request: &SlotRequest) -> Result<Reservation> {
        (**self).reserve(request).await
    }

    async fn release(&self, request: &SlotRequest, reservation: &Reservation) -> Result<()> {
        (**self).release(request, reservation).await
    }

    async fn slow_down(&self, from: &PhoneNumberId, now: OffsetDateTime) -> Result<()> {
        (**self).slow_down(from, now).await
    }
}

/// Which errors mean a number sends too fast, so its pacer slows down
/// ([`Pacer::on_error`]): a broadcast asks after each failed send, and a
/// [`PacedOutbound`] after each failed reply or read receipt.
pub trait SlowDownRule: Send + Sync + fmt::Debug + 'static {
    /// Whether `error`, from a call of a business number, means it sends
    /// too fast.
    fn slows_down(&self, error: &Error) -> bool;
}

impl<T: SlowDownRule + ?Sized> SlowDownRule for Arc<T> {
    fn slows_down(&self, error: &Error) -> bool {
        (**self).slows_down(error)
    }
}

/// The default [`SlowDownRule`], read from the error's kind or code, never
/// its text: `RateLimited` (`130429`, `80007`, `4`, an HTTP 429),
/// `SpamRateLimited` (`131048`) and `131057`, the account in maintenance
/// (Meta's throughput upgrade). Not the pair rate limit (`131056`: one
/// recipient, not the number) nor another outage.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThrottlingErrors;

/// `131057`: "Business Account is in maintenance mode", which Meta's
/// throughput upgrade causes for up to a minute (`throughput`).
pub(crate) const MAINTENANCE: i64 = 131_057;

/// Whether `error` is Meta's `131057`.
pub(crate) fn in_maintenance(error: &Error) -> bool {
    error.graph().is_some_and(|g| g.code == MAINTENANCE)
}

impl SlowDownRule for ThrottlingErrors {
    fn slows_down(&self, error: &Error) -> bool {
        matches!(
            error.kind(),
            ErrorKind::RateLimited | ErrorKind::SpamRateLimited
        ) || in_maintenance(error)
    }
}

/// A slot booked and not started yet, which [`RateLimiter::release`] can
/// give back.
#[derive(Debug, Clone, Copy)]
struct Booked {
    id: u64,
    /// The schedule before it.
    base: OffsetDateTime,
    /// The schedule after it.
    end: OffsetDateTime,
    released: bool,
}

/// One number's schedule.
#[derive(Debug, Clone)]
struct Bucket {
    /// When the next send is due at the current rate (the generic cell
    /// rate algorithm's theoretical arrival time).
    due: OffsetDateTime,
    /// The latest `now` seen: an earlier one means the clock stepped back.
    seen: OffsetDateTime,
    /// The rate is divided by this (1, 2, 4, … with the default factor)
    /// after slow-downs.
    divisor: u32,
    /// The last slow-down, or the last step back up since: the next step
    /// up is one recovery period after it.
    since: OffsetDateTime,
    /// The last slow-down that counted.
    slowed: Option<OffsetDateTime>,
    /// The id of the last slot booked.
    last_id: u64,
    /// The slots booked that have not started yet, oldest first.
    booked: Vec<Booked>,
}

impl Bucket {
    fn new(now: OffsetDateTime) -> Self {
        Self {
            due: now,
            seen: now,
            divisor: 1,
            since: now,
            slowed: None,
            last_id: 0,
            booked: Vec::new(),
        }
    }
}

/// The rates set per number, and each number's schedule.
#[derive(Debug, Default)]
struct State {
    rates: HashMap<PhoneNumberId, Rate>,
    buckets: HashMap<PhoneNumberId, Bucket>,
}

/// The default [`RateLimiter`]: a token bucket per business number, in this
/// process.
///
/// Each number sends at its [`Rate`] ([`TokenBucket::rate_for`] when
/// building, [`TokenBucket::set_rate`] while running, else the bucket's
/// own), evenly spaced: with the default burst of one, any one-second
/// window holds at most the rate's number of sends, whatever Meta's window
/// is. [`TokenBucket::burst`] lets up to that many go at once after a quiet
/// spell, and so up to `burst - 1` more than the rate in some one-second
/// window.
///
/// When Meta says a number goes too fast ([`RateLimiter::slow_down`]), its
/// rate is divided by [`TokenBucket::slow_down_factor`] (default 2), at
/// most once per [`TokenBucket::slow_down_spacing`] (default 1 s) and
/// never below one message a second; each [`TokenBucket::recovery`] period
/// (default 30 s) without another slow-down multiplies it back by the
/// factor, up to its configured rate. [`TokenBucket::adaptive`] with
/// `false` turns slow-downs off.
///
/// A slot given back ([`RateLimiter::release`]) is reused when nothing was
/// booked after it (or everything booked after it was given back too);
/// otherwise reusing it could put two sends in one slot, so it stays
/// unused.
///
/// The schedule survives a wall clock stepping back (the `Clock` is
/// wall time): it moves back with it instead of pausing the number for
/// the difference. Share one across tasks behind an `Arc`
/// (`Pacer::shared`) to keep a handle for [`TokenBucket::set_rate`].
#[must_use]
pub struct TokenBucket {
    rate: Rate,
    burst: u32,
    recovery: Duration,
    adaptive: bool,
    spacing: Duration,
    factor: u32,
    state: Mutex<State>,
}

impl fmt::Debug for TokenBucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenBucket")
            .field("rate", &self.rate)
            .field("rates", &self.state().rates)
            .field("burst", &self.burst)
            .field("adaptive", &self.adaptive)
            .field("slow_down_factor", &self.factor)
            .field("slow_down_spacing", &self.spacing)
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
    /// Every number at `rate`, a burst of one, slow-downs on (halving, at
    /// most once a second, back up every 30 quiet seconds).
    pub fn new(rate: Rate) -> Self {
        Self {
            rate,
            burst: 1,
            recovery: Duration::from_secs(30),
            adaptive: true,
            spacing: Duration::from_secs(1),
            factor: 2,
            state: Mutex::new(State::default()),
        }
    }

    /// `number` at `rate` instead of the bucket's own: a number Meta
    /// upgraded ([`Rate::HIGHER_THROUGHPUT`]), or one shared with the
    /// WhatsApp Business app ([`Rate::BUSINESS_APP`]).
    pub fn rate_for(self, number: impl Into<PhoneNumberId>, rate: Rate) -> Self {
        self.set_rate(number, rate);
        self
    }

    /// Set `number`'s rate while the bucket is in use (behind an `Arc`):
    /// the number's next slot is booked at it. For a number Meta upgrades
    /// (a `phone_number_quality_update` with
    /// `PhoneNumberQualityEvent::ThroughputUpgrade`), or a merchant's
    /// number onboarded while running.
    pub fn set_rate(&self, number: impl Into<PhoneNumberId>, rate: Rate) {
        self.state().rates.insert(number.into(), rate);
    }

    /// How many sends may start at once after a quiet spell (default 1).
    /// Above one, a one-second window can hold up to `sends - 1` more than
    /// the rate. Zero is a `ConfigError`.
    pub fn burst(mut self, sends: u32) -> Result<Self> {
        if sends == 0 {
            return Err(ConfigError::new("a burst of zero sends").into());
        }
        self.burst = sends;
        Ok(self)
    }

    /// Whether Meta's throttling slows a number down (default `true`):
    /// with `false`, [`RateLimiter::slow_down`] changes nothing and every
    /// number keeps its rate.
    pub fn adaptive(mut self, on: bool) -> Self {
        self.adaptive = on;
        self
    }

    /// What a slow-down divides the rate by (default 2), and a quiet
    /// recovery period multiplies it back by. Below two is a
    /// `ConfigError`: to keep the rate, turn slow-downs off
    /// ([`Self::adaptive`]).
    pub fn slow_down_factor(mut self, factor: u32) -> Result<Self> {
        if factor < 2 {
            return Err(ConfigError::new(
                "a slow-down factor below two (to keep the rate: `TokenBucket::adaptive(false)`)",
            )
            .into());
        }
        self.factor = factor;
        Ok(self)
    }

    /// Slow-downs closer together than this count as one (default 1 s):
    /// every send in flight when Meta starts refusing gets the same error,
    /// and should not divide the rate once each. Size it to a send's
    /// duration. Zero: every slow-down counts.
    pub fn slow_down_spacing(mut self, spacing: Duration) -> Self {
        self.spacing = spacing;
        self
    }

    /// How long a slowed number waits, without another slow-down, before
    /// its rate is multiplied back by the factor (default 30 seconds).
    /// Zero is a `ConfigError`: to ignore slow-downs, turn them off
    /// ([`Self::adaptive`]).
    pub fn recovery(mut self, period: Duration) -> Result<Self> {
        if period.is_zero() {
            return Err(ConfigError::new(
                "a recovery period of zero (to ignore slow-downs: `TokenBucket::adaptive(false)`)",
            )
            .into());
        }
        self.recovery = period;
        Ok(self)
    }

    /// The configured rate of `number` (before any slow-down).
    pub fn rate_of(&self, number: &PhoneNumberId) -> Rate {
        Self::rate_in(&self.state(), number, self.rate)
    }

    fn rate_in(state: &State, number: &PhoneNumberId, default: Rate) -> Rate {
        state.rates.get(number).copied().unwrap_or(default)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Follow the clock to `now`: back with it if it stepped back, and
    /// up by one step of the rate per quiet recovery period.
    fn catch_up(&self, bucket: &mut Bucket, now: OffsetDateTime) {
        if now < bucket.seen {
            let back = bucket.seen - now;
            bucket.due = bucket.due.saturating_sub(back);
            bucket.since = bucket.since.saturating_sub(back);
            bucket.slowed = bucket.slowed.map(|at| at.saturating_sub(back));
            // The slots booked before the step are not given back.
            bucket.booked.clear();
        }
        bucket.seen = now;
        // Slots whose time has passed are spent or lost: giving them back
        // would change nothing.
        let past = bucket
            .booked
            .iter()
            .position(|b| b.end > now)
            .unwrap_or(bucket.booked.len());
        bucket.booked.drain(..past);
        if bucket.divisor == 1 {
            return;
        }
        let steps = elapsed(bucket.since, now).as_nanos() / self.recovery.as_nanos().max(1);
        if steps > 0 {
            let steps = u32::try_from(steps).unwrap_or(u32::MAX);
            for _ in 0..steps.min(32) {
                bucket.divisor = (bucket.divisor / self.factor).max(1);
                if bucket.divisor == 1 {
                    break;
                }
            }
            bucket.since = later(bucket.since, self.recovery.saturating_mul(steps));
        }
    }
}

#[async_trait]
impl RateLimiter for TokenBucket {
    async fn reserve(&self, request: &SlotRequest) -> Result<Reservation> {
        let now = request.now;
        let mut state = self.state();
        let rate = Self::rate_in(&state, &request.from, self.rate);
        let bucket = state
            .buckets
            .entry(request.from.clone())
            .or_insert_with(|| Bucket::new(now));
        self.catch_up(bucket, now);
        let interval = rate.interval(bucket.divisor);
        let base = bucket.due.max(now);
        let tolerance = interval.saturating_mul(self.burst - 1);
        let start = earlier(base, tolerance).max(now);
        bucket.due = later(base, interval.saturating_mul(request.cost.get()));
        bucket.last_id += 1;
        let id = bucket.last_id;
        bucket.booked.push(Booked {
            id,
            base,
            end: bucket.due,
            released: false,
        });
        Ok(Reservation::new(elapsed(now, start)).with_id(id))
    }

    async fn release(&self, request: &SlotRequest, reservation: &Reservation) -> Result<()> {
        let mut state = self.state();
        let Some(bucket) = state.buckets.get_mut(&request.from) else {
            return Ok(());
        };
        if let Some(booked) = bucket.booked.iter_mut().find(|b| b.id == reservation.id) {
            booked.released = true;
        }
        // Unbook from the end: a slot with a live one after it stays
        // booked until that one is given back too.
        while let Some(last) = bucket.booked.last() {
            if !last.released || last.end != bucket.due {
                break;
            }
            bucket.due = last.base;
            bucket.booked.pop();
        }
        Ok(())
    }

    async fn slow_down(&self, from: &PhoneNumberId, now: OffsetDateTime) -> Result<()> {
        if !self.adaptive {
            return Ok(());
        }
        let mut state = self.state();
        let rate = Self::rate_in(&state, from, self.rate);
        let bucket = state
            .buckets
            .entry(from.clone())
            .or_insert_with(|| Bucket::new(now));
        self.catch_up(bucket, now);
        if bucket
            .slowed
            .is_some_and(|at| elapsed(at, now) < self.spacing)
        {
            return Ok(());
        }
        // Divide the rate, never below one message a second.
        if bucket.divisor.saturating_mul(self.factor) <= rate.get() {
            bucket.divisor *= self.factor;
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
/// The pacer knows when each slot starts, so it waits until a deadline
/// ([`Timer::sleep_until`]); [`Timer::sleep`] is defined on top of it.
/// Shipped for the core's two clocks: `SystemClock` sleeps with Tokio (a
/// Tokio runtime with its time driver must be running, as for the
/// client's retries), and `ManualClock` moves itself forward to the
/// deadline, at once and never back, so a test with a `ManualClock` runs a
/// whole paced broadcast without waiting and reads the times each send
/// started, exactly, however many senders share it. A send takes no time
/// on it: to test slow sends overlapping, implement `Timer` over Tokio's
/// paused clock.
#[async_trait]
pub trait Timer: Clock {
    /// Wait until `deadline` (at once when it has passed): afterwards,
    /// `now()` is at least `deadline`.
    async fn sleep_until(&self, deadline: OffsetDateTime);

    /// Wait `duration`: [`Self::sleep_until`] now plus `duration`.
    async fn sleep(&self, duration: Duration) {
        self.sleep_until(later(self.now(), duration)).await;
    }
}

#[async_trait]
impl Timer for SystemClock {
    async fn sleep_until(&self, deadline: OffsetDateTime) {
        tokio::time::sleep(elapsed(self.now(), deadline)).await;
    }

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

#[async_trait]
impl Timer for ManualClock {
    /// Moves the clock forward to `deadline` (never back), without
    /// waiting.
    async fn sleep_until(&self, deadline: OffsetDateTime) {
        self.advance_to(deadline);
    }
}

#[async_trait]
impl<T: Timer + ?Sized> Timer for Arc<T> {
    async fn sleep_until(&self, deadline: OffsetDateTime) {
        (**self).sleep_until(deadline).await;
    }

    async fn sleep(&self, duration: Duration) {
        (**self).sleep(duration).await;
    }
}

/// Paces the sends of each business number: a [`RateLimiter`] (what may
/// start when), a [`SlowDownRule`] (which errors slow a number down) and a
/// [`Timer`] (now, and the wait). See the [module docs](self).
///
/// Cheap to clone; clones share the limiter. Share one per process across
/// every broadcast and bot of a number, or each gets a budget of its own.
#[derive(Clone)]
pub struct Pacer {
    limiter: Arc<dyn RateLimiter>,
    timer: Arc<dyn Timer>,
    rule: Arc<dyn SlowDownRule>,
}

impl fmt::Debug for Pacer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pacer")
            .field("limiter", &self.limiter)
            .field("timer", &self.timer)
            .field("slow_down_rule", &self.rule)
            .finish()
    }
}

impl Default for Pacer {
    /// A [`TokenBucket`] at [`Rate::DEFAULT`] for every number, on the
    /// system clock, slowing down on [`ThrottlingErrors`].
    fn default() -> Self {
        Self::new(TokenBucket::default())
    }
}

impl Pacer {
    /// Pace with `limiter`, on the system clock, slowing down on
    /// [`ThrottlingErrors`].
    pub fn new(limiter: impl RateLimiter) -> Self {
        Self::shared(Arc::new(limiter))
    }

    /// Pace with a limiter shared with other code (an
    /// `Arc<TokenBucket>` you keep, to [`TokenBucket::set_rate`] while
    /// running).
    pub fn shared(limiter: Arc<dyn RateLimiter>) -> Self {
        Self {
            limiter,
            timer: Arc::new(SystemClock),
            rule: Arc::new(ThrottlingErrors),
        }
    }

    /// Read the time from `timer` and wait on it (a `ManualClock` in tests).
    #[must_use]
    pub fn with_timer(mut self, timer: impl Timer) -> Self {
        self.timer = Arc::new(timer);
        self
    }

    /// Slow a number down on the errors `rule` names, instead of
    /// [`ThrottlingErrors`].
    #[must_use]
    pub fn with_slow_down_rule(mut self, rule: impl SlowDownRule) -> Self {
        self.rule = Arc::new(rule);
        self
    }

    /// The limiter.
    pub fn limiter(&self) -> &Arc<dyn RateLimiter> {
        &self.limiter
    }

    /// Wait for a slot of `from`: before a call you want counted in its
    /// budget (the group operations have [`crate::PacedGroups`]). Fails
    /// only when the limiter does (a shared one unreachable), before any
    /// wait. Dropping the future while it waits leaves the slot unused.
    pub async fn acquire(&self, from: &PhoneNumberId) -> Result<()> {
        let (request, reservation) = self.reserve(from).await?;
        if !reservation.wait.is_zero() {
            self.timer
                .sleep_until(later(request.now, reservation.wait))
                .await;
        }
        Ok(())
    }

    /// Tell the limiter Meta refused a send of `from` for going too fast.
    pub async fn slow_down(&self, from: &PhoneNumberId) -> Result<()> {
        self.limiter.slow_down(from, self.timer.now()).await
    }

    /// Whether `error` means a number sends too fast, by the pacer's
    /// [`SlowDownRule`].
    pub fn slows_down(&self, error: &Error) -> bool {
        self.rule.slows_down(error)
    }

    /// Meta answered a call of `from` with `error`: slow the number down
    /// when the pacer's [`SlowDownRule`] says the error means too fast.
    /// Whether it did; an error only when the limiter failed.
    pub async fn on_error(&self, from: &PhoneNumberId, error: &Error) -> Result<bool> {
        if !self.rule.slows_down(error) {
            return Ok(false);
        }
        self.slow_down(from).await?;
        Ok(true)
    }

    /// Book a slot of `from`, as of now.
    pub(crate) async fn reserve(&self, from: &PhoneNumberId) -> Result<(SlotRequest, Reservation)> {
        let request = SlotRequest::new(from.clone(), self.timer.now());
        let reservation = self.limiter.reserve(&request).await?;
        Ok((request, reservation))
    }

    /// Hand back a slot that will not be used; a limiter that fails to
    /// take it back only loses the slot.
    pub(crate) async fn release(&self, request: &SlotRequest, reservation: &Reservation) {
        if let Err(error) = self.limiter.release(request, reservation).await {
            tracing::warn!(
                error_kind = ?error.kind(),
                "pacer could not give a slot back"
            );
        }
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

    /// Wait on the timer until `deadline`.
    pub(crate) async fn sleep_until(&self, deadline: OffsetDateTime) {
        self.timer.sleep_until(deadline).await;
    }
}

/// An [`Outbound`] that waits for a slot of the business number before
/// every send and read receipt (typing indicators included), then hands
/// it to the outbound it wraps. `BotBuilder::pacer` puts a bot's outbound
/// in one, so `MarkRead`, the replies and the refusals share the number's
/// budget with its broadcasts.
///
/// A call Meta refuses for going too fast slows the number down (the
/// pacer's [`SlowDownRule`]). A limiter that fails fails the call, before
/// anything is sent.
///
/// **Retries.** The outbound it wraps must not retry inside a call: each
/// of those retries would go out without a slot. Over a client, give the
/// client `RetryPolicy::NONE` (`Client::with_retry`) and this
/// [`PacedOutbound::retry`] the policy instead: each retry then waits its
/// delay on the pacer's timer and takes a slot of its own, and a send is
/// retried only when `Error::may_resend` holds (a read receipt without a
/// typing indicator, idempotent, on any retryable error).
/// `BotBuilder::pacer` does this for a bot built with `BotBuilder::client`.
/// An outbound of your own must also be truthful about
/// `Error::may_have_been_sent`.
#[derive(Debug, Clone)]
pub struct PacedOutbound {
    inner: Arc<dyn Outbound>,
    pacer: Pacer,
    retry: RetryPolicy,
}

impl PacedOutbound {
    /// Pace `inner` with `pacer`, retrying nothing.
    pub fn new(inner: impl Outbound, pacer: Pacer) -> Self {
        Self::shared(Arc::new(inner), pacer)
    }

    /// Pace an outbound shared with other code, retrying nothing.
    pub fn shared(inner: Arc<dyn Outbound>, pacer: Pacer) -> Self {
        Self {
            inner,
            pacer,
            retry: RetryPolicy::NONE,
        }
    }

    /// Retry failed calls with `retry`, each retry after its delay (on the
    /// pacer's timer) and a slot of its own (default
    /// `RetryPolicy::NONE`).
    #[must_use]
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// The pacer.
    pub fn pacer(&self) -> &Pacer {
        &self.pacer
    }

    /// After a failed call: slow the number down if the error says so, and
    /// whether (and after how long) to try `attempt + 1`.
    async fn after_failure(
        &self,
        from: &PhoneNumberId,
        error: &Error,
        attempt: u32,
        idempotent: bool,
    ) -> Option<Duration> {
        if let Err(e) = self.pacer.on_error(from, error).await {
            tracing::warn!(error_kind = ?e.kind(), "paced outbound could not slow the pacer down");
        }
        self.retry
            .should_retry(attempt, error, idempotent)
            .then(|| self.retry.delay(attempt, None))
    }
}

#[async_trait]
impl Outbound for PacedOutbound {
    async fn send(&self, from: &PhoneNumberId, message: &OutboundMessage) -> Result<SendResponse> {
        let mut attempt = 0;
        loop {
            self.pacer.acquire(from).await?;
            let error = match self.inner.send(from, message).await {
                Ok(response) => return Ok(response),
                Err(error) => error,
            };
            match self.after_failure(from, &error, attempt, false).await {
                Some(delay) => self.pacer.sleep(delay).await,
                None => return Err(error),
            }
            attempt += 1;
        }
    }

    async fn mark_read(
        &self,
        from: &PhoneNumberId,
        message_id: &MessageId,
        typing_indicator: bool,
    ) -> Result<()> {
        // As the client marks them: a plain read receipt sets a state again
        // when replayed; one with a typing indicator shows it again.
        let idempotent = !typing_indicator;
        let mut attempt = 0;
        loop {
            self.pacer.acquire(from).await?;
            let error = match self
                .inner
                .mark_read(from, message_id, typing_indicator)
                .await
            {
                Ok(()) => return Ok(()),
                Err(error) => error,
            };
            match self.after_failure(from, &error, attempt, idempotent).await {
                Some(delay) => self.pacer.sleep(delay).await,
                None => return Err(error),
            }
            attempt += 1;
        }
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

    fn at(now: OffsetDateTime) -> SlotRequest {
        SlotRequest::new(number(), now)
    }

    /// The wait before a slot of `number()` booked at `now`.
    async fn wait(bucket: &TokenBucket, now: OffsetDateTime) -> Duration {
        bucket.reserve(&at(now)).await.unwrap().wait
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
        // Never below one a second, whatever the divisor.
        assert_eq!(
            Rate::per_second(3).unwrap().interval(64),
            Duration::from_secs(1)
        );
    }

    /// Zero, where it means nothing, is refused when given.
    #[test]
    fn meaningless_zeros_are_config_errors() {
        let bucket = || TokenBucket::new(Rate::DEFAULT);
        assert!(matches!(bucket().burst(0), Err(Error::Config(_))));
        assert!(matches!(
            bucket().recovery(Duration::ZERO),
            Err(Error::Config(_))
        ));
        assert!(matches!(
            bucket().slow_down_factor(0),
            Err(Error::Config(_))
        ));
        assert!(matches!(
            bucket().slow_down_factor(1),
            Err(Error::Config(_))
        ));
        assert!(bucket().burst(1).is_ok());
        assert!(bucket().slow_down_factor(2).is_ok());
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
        let pacer = Pacer::new(
            TokenBucket::new(Rate::per_second(10).unwrap())
                .burst(5)
                .unwrap(),
        )
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
        let other = PhoneNumberId::new("2");
        let bucket = TokenBucket::new(Rate::per_second(10).unwrap())
            .rate_for(other.clone(), Rate::per_second(2).unwrap());
        assert_eq!(bucket.rate_of(&other).get(), 2);
        assert_eq!(bucket.rate_of(&number()).get(), 10);
        assert_eq!(wait(&bucket, T0).await, Duration::ZERO);
        assert_eq!(wait(&bucket, T0).await, Duration::from_millis(100));
        // Another number is not behind the first one's queue.
        let other_at = SlotRequest::new(other, T0);
        assert_eq!(
            bucket.reserve(&other_at).await.unwrap().wait,
            Duration::ZERO
        );
        assert_eq!(
            bucket.reserve(&other_at).await.unwrap().wait,
            Duration::from_millis(500)
        );
    }

    /// `set_rate` on a bucket in use: the next slots are booked at the new
    /// rate, down or up.
    #[tokio::test]
    async fn a_rate_set_while_running_takes_effect() {
        let bucket = Arc::new(TokenBucket::new(Rate::per_second(10).unwrap()));
        let clock = ManualClock::new(T0);
        let pacer = Pacer::shared(bucket.clone()).with_timer(clock.clone());
        let before = starts(&pacer, &clock, 2).await;
        assert_eq!(
            before[1].checked_sub(before[0]),
            Some(Duration::from_millis(100))
        );

        bucket.set_rate(number(), Rate::per_second(1000).unwrap());
        assert_eq!(bucket.rate_of(&number()).get(), 1000);
        let upgraded = starts(&pacer, &clock, 4).await;
        // The slot booked at 10/s first, then 1 ms apart.
        let gaps: Vec<Duration> = upgraded
            .windows(2)
            .filter_map(|w| w[1].checked_sub(w[0]))
            .collect();
        assert_eq!(gaps, [Duration::from_millis(1); 3]);

        bucket.set_rate(number(), Rate::per_second(4).unwrap());
        let slower = starts(&pacer, &clock, 3).await;
        let gaps: Vec<Duration> = slower
            .windows(2)
            .filter_map(|w| w[1].checked_sub(w[0]))
            .collect();
        assert_eq!(gaps, [Duration::from_millis(250); 2]);
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

    /// The factor and the spacing are settings: a factor of 4, and no
    /// spacing (every slow-down counts).
    #[tokio::test]
    async fn the_slow_down_factor_and_spacing_are_settings() {
        let bucket = TokenBucket::new(Rate::per_second(64).unwrap())
            .slow_down_factor(4)
            .unwrap()
            .slow_down_spacing(Duration::ZERO);
        bucket.slow_down(&number(), T0).await.unwrap();
        bucket.slow_down(&number(), T0).await.unwrap(); // 64 → 16 → 4
        assert_eq!(interval_at(&bucket, T0).await, Duration::from_millis(250));
        // One quiet recovery period: × 4.
        let recovered = later(T0, Duration::from_secs(31));
        assert_eq!(
            interval_at(&bucket, recovered).await,
            Duration::from_micros(62_500)
        );
    }

    #[tokio::test]
    async fn slow_downs_stop_at_one_a_second() {
        let bucket = TokenBucket::new(Rate::per_second(4).unwrap());
        for s in 0..5 {
            bucket
                .slow_down(&number(), later(T0, Duration::from_secs(s)))
                .await
                .unwrap();
        }
        let now = later(T0, Duration::from_secs(5));
        assert_eq!(interval_at(&bucket, now).await, Duration::from_secs(1));
    }

    #[tokio::test]
    async fn adaptive_off_ignores_slow_downs() {
        let bucket = TokenBucket::new(Rate::per_second(10).unwrap()).adaptive(false);
        bucket.slow_down(&number(), T0).await.unwrap();
        assert_eq!(interval_at(&bucket, T0).await, Duration::from_millis(100));
    }

    /// A wall clock stepping back an hour does not pause the number for an
    /// hour.
    #[tokio::test]
    async fn a_clock_stepping_back_moves_the_schedule_with_it() {
        let bucket = TokenBucket::new(Rate::per_second(10).unwrap());
        wait(&bucket, T0).await;
        let back = earlier(T0, Duration::from_secs(3600));
        assert_eq!(wait(&bucket, back).await, Duration::from_millis(100));
    }

    /// The wait before the second of two sends booked at `now`: the
    /// number's interval at its current rate.
    async fn interval_at(bucket: &TokenBucket, now: OffsetDateTime) -> Duration {
        wait(bucket, now).await;
        wait(bucket, now).await
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
        wait(&bucket, back).await;
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
        wait(&bucket, back).await;
        let two_later = later(back, Duration::from_secs(2));
        bucket.slow_down(&number(), two_later).await.unwrap();
        assert_eq!(
            interval_at(&bucket, two_later).await,
            Duration::from_millis(500)
        );
    }

    /// Slots given back are reused, in any order, when nothing live was
    /// booked after them; a slot with a live one after it stays booked.
    #[tokio::test]
    async fn slots_given_back_are_reused_only_from_the_end() {
        async fn book(bucket: &TokenBucket) -> Reservation {
            bucket.reserve(&at(T0)).await.unwrap()
        }
        let bucket = TokenBucket::new(Rate::per_second(10).unwrap());
        let first = book(&bucket).await; // 0 ms
        let second = book(&bucket).await; // 100 ms
        let third = book(&bucket).await; // 200 ms
        assert_eq!(third.wait, Duration::from_millis(200));
        // The second given back first: the third, live, is after it.
        bucket.release(&at(T0), &second).await.unwrap();
        assert_eq!(
            bucket.state().buckets[&number()].due,
            later(T0, Duration::from_millis(300))
        );
        // The third too: both are free again, in the order booked.
        bucket.release(&at(T0), &third).await.unwrap();
        assert_eq!(wait(&bucket, T0).await, Duration::from_millis(100));
        // A live slot booked after one given back keeps it booked.
        let fifth = book(&bucket).await; // 200 ms
        let sixth = book(&bucket).await; // 300 ms
        bucket.release(&at(T0), &fifth).await.unwrap();
        assert_eq!(wait(&bucket, T0).await, Duration::from_millis(400));
        let _ = (first, sixth);
        // An unknown id changes nothing.
        bucket
            .release(&at(T0), &Reservation::new(Duration::ZERO).with_id(999))
            .await
            .unwrap();
        assert_eq!(wait(&bucket, T0).await, Duration::from_millis(500));
    }

    #[tokio::test]
    async fn a_request_can_cost_several_sends() {
        let bucket = TokenBucket::new(Rate::per_second(10).unwrap());
        let three = at(T0).with_cost(NonZeroU32::new(3).unwrap());
        assert_eq!(bucket.reserve(&three).await.unwrap().wait, Duration::ZERO);
        assert_eq!(wait(&bucket, T0).await, Duration::from_millis(300));
    }

    /// A shared `ManualClock` waited on until each deadline stays exact:
    /// three sleepers until 50, 100 and 20 ms leave it at 100 ms.
    #[tokio::test]
    async fn a_manual_clock_sleeps_until_the_latest_deadline() {
        let clock = ManualClock::new(T0);
        let ms = |n| later(T0, Duration::from_millis(n));
        futures::join!(
            Timer::sleep_until(&clock, ms(50)),
            Timer::sleep_until(&clock, ms(100)),
            Timer::sleep_until(&clock, ms(20)),
        );
        assert_eq!(clock.now(), ms(100));
        Timer::sleep(&clock, Duration::from_millis(1500)).await;
        assert_eq!(clock.now(), ms(1600));
    }

    #[test]
    fn throttling_errors_slow_down_and_others_do_not() {
        let api = |code: i64| {
            let mut e = meta_whatsapp_core::GraphApiError::new(code, "x");
            e.http_status = Some(400);
            Error::from(e)
        };
        let rule = ThrottlingErrors;
        for code in [130_429, 131_048, 131_057, 80_007, 4] {
            assert!(rule.slows_down(&api(code)), "{code}");
        }
        for code in [131_056, 131_049, 131_000, 190] {
            assert!(!rule.slows_down(&api(code)), "{code}");
        }
        assert!(rule.slows_down(&Error::Http {
            status: 429,
            body_snippet: String::new()
        }));
    }
}
