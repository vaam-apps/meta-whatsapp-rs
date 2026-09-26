//! The paced broadcast under real concurrency, over long runs: Tokio's
//! paused clock drives the pacer, so the concurrent senders sleep at once
//! and time moves only when every one of them waits, as it does for real
//! (a send takes no time on a `ManualClock`, so slow sends cannot overlap
//! on it).
//! Every one-second window of 10,000 sends is checked, with slow sends, a
//! burst, a slow-down and its recovery, and a wall clock stepping back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{NUMBER, send_response};
use meta_whatsapp_bot::{
    Broadcast, BroadcastEnd, Outbound, Pacer, Rate, RateLimiter, Reservation, SendOutcome,
    SlotRequest, Timer, TokenBucket,
};
use meta_whatsapp_client::messages::{OutboundMessage, SendResponse, Text};
use meta_whatsapp_core::clock::Clock;
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::{Error, GraphApiError};
use time::OffsetDateTime;
use time::macros::datetime;
use tokio::time::Instant;

const T0: OffsetDateTime = datetime!(2026-09-26 12:00 UTC);

/// Tokio's clock as the pacer's `Timer`, plus an offset a test moves to
/// step the wall clock back (Tokio's own clock never steps back).
#[derive(Debug, Clone)]
struct TokioClock {
    start: Instant,
    offset_ms: Arc<AtomicI64>,
}

impl TokioClock {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            offset_ms: Arc::default(),
        }
    }

    /// Time since the test started, on Tokio's monotonic clock: what the
    /// sends are measured on, whatever the wall clock does.
    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    fn step_back(&self, by: Duration) {
        let ms = i64::try_from(by.as_millis()).unwrap();
        self.offset_ms.fetch_sub(ms, Ordering::SeqCst);
    }
}

impl Clock for TokioClock {
    fn now(&self) -> OffsetDateTime {
        T0 + self.start.elapsed()
            + time::Duration::milliseconds(self.offset_ms.load(Ordering::SeqCst))
    }
}

#[async_trait]
impl Timer for TokioClock {
    async fn sleep_until(&self, deadline: OffsetDateTime) {
        let wait = Duration::try_from(deadline - self.now()).unwrap_or_default();
        tokio::time::sleep(wait).await;
    }

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Decides which sends fail: `(start, recipient)` in, an error out.
type Failures = Arc<dyn Fn(Duration, &str) -> Option<Error> + Send + Sync>;

/// An `Outbound` whose sends take `latency` each, recording when each
/// started (on Tokio's clock) and to whom.
#[derive(Clone)]
struct Slow {
    clock: TokioClock,
    latency: Duration,
    starts: Arc<Mutex<Vec<(Duration, String)>>>,
    /// When each failure was answered.
    failed_at: Arc<Mutex<Vec<Duration>>>,
    fail: Failures,
}

impl std::fmt::Debug for Slow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Slow")
    }
}

impl Slow {
    fn new(clock: &TokioClock, latency: Duration) -> Self {
        Self {
            clock: clock.clone(),
            latency,
            starts: Arc::default(),
            failed_at: Arc::default(),
            fail: Arc::new(|_, _| None),
        }
    }

    fn failing(
        mut self,
        fail: impl Fn(Duration, &str) -> Option<Error> + Send + Sync + 'static,
    ) -> Self {
        self.fail = Arc::new(fail);
        self
    }

    /// Send start times, in order.
    fn starts(&self) -> Vec<Duration> {
        let mut starts: Vec<Duration> = self
            .starts
            .lock()
            .unwrap()
            .iter()
            .map(|(t, _)| *t)
            .collect();
        starts.sort();
        starts
    }

    /// `(start, recipient)` of every send, in order.
    fn sends(&self) -> Vec<(Duration, String)> {
        let mut sends = self.starts.lock().unwrap().clone();
        sends.sort();
        sends
    }

    fn recipients(&self) -> Vec<String> {
        self.starts
            .lock()
            .unwrap()
            .iter()
            .map(|(_, to)| to.clone())
            .collect()
    }
}

#[async_trait]
impl Outbound for Slow {
    async fn send(
        &self,
        _: &PhoneNumberId,
        message: &OutboundMessage,
    ) -> meta_whatsapp_core::Result<SendResponse> {
        let start = self.clock.elapsed();
        let who = serde_json::to_value(message).unwrap()["to"]
            .as_str()
            .unwrap()
            .to_owned();
        self.starts.lock().unwrap().push((start, who.clone()));
        tokio::time::sleep(self.latency).await;
        if let Some(error) = (self.fail)(start, &who) {
            self.failed_at.lock().unwrap().push(self.clock.elapsed());
            return Err(error);
        }
        Ok(serde_json::from_value(send_response()).unwrap())
    }

    async fn mark_read(
        &self,
        _: &PhoneNumberId,
        _: &MessageId,
        _: bool,
    ) -> meta_whatsapp_core::Result<()> {
        Ok(())
    }
}

fn phone(i: usize) -> Recipient {
    Recipient::phone(format!("+1650{i:07}"))
}

/// The most sends starting in one half-open second `[t, t + 1 s)`, over
/// the windows starting at a send in `from..to` (the busiest window
/// always starts at a send). `starts` is sorted.
fn busiest_second(starts: &[Duration], from: Duration, to: Duration) -> usize {
    let mut most = 0;
    let mut end = 0;
    for (i, start) in starts.iter().enumerate() {
        while end < starts.len() && starts[end] < *start + Duration::from_secs(1) {
            end += 1;
        }
        if (from..to).contains(start) {
            most = most.max(end - i);
        }
    }
    most
}

fn all(starts: &[Duration]) -> usize {
    busiest_second(starts, Duration::ZERO, Duration::MAX)
}

/// A Graph error as the client reports it on a 400.
fn graph(code: i64, message: &str) -> Error {
    let mut error = GraphApiError::new(code, message);
    error.http_status = Some(400);
    error.into()
}

fn throttled() -> Error {
    graph(130429, "(#130429) Rate limit hit")
}

/// 10,000 sends: no one-second window ever holds more than the rate (plus
/// `burst - 1`), and the rate is reached, because the concurrent senders
/// cover the sends' latency. Removing the concurrency, or pacing wrong,
/// fails this.
#[tokio::test(start_paused = true)]
async fn ten_thousand_sends_never_exceed_the_rate_in_any_second() {
    const N: usize = 10_000;
    // (rate, burst, concurrency, a send's latency)
    let cases: [(u32, u32, usize, u64); 4] = [
        (80, 1, 32, 200),    // Meta's default, the broadcast's default concurrency
        (7, 1, 4, 300),      // a rate that does not divide a second
        (1000, 1, 400, 150), // higher throughput
        (80, 10, 32, 200),   // a burst of 10: up to 9 more in a window
    ];
    for (rate, burst, concurrency, latency) in cases {
        let clock = TokioClock::new();
        let outbound = Slow::new(&clock, Duration::from_millis(latency));
        let pacer = Pacer::new(
            TokenBucket::new(Rate::per_second(rate).unwrap())
                .burst(burst)
                .unwrap(),
        )
        .with_timer(clock.clone());
        let report = Broadcast::builder(NUMBER)
            .to((0..N).map(phone))
            .content(Text::new("Spring sale"))
            .outbound(outbound.clone())
            .pacer(pacer)
            .concurrency(concurrency)
            .build()
            .unwrap()
            .run()
            .await;

        let case = format!("{rate}/s, burst {burst}, {concurrency} at once, {latency} ms");
        assert_eq!(report.ended, BroadcastEnd::Completed, "{case}");
        assert_eq!(report.progress().sent, N, "{case}");
        let starts = outbound.starts();
        assert_eq!(starts.len(), N, "{case}");
        let unique: HashSet<String> = outbound.recipients().into_iter().collect();
        assert_eq!(unique.len(), N, "{case}: each recipient once");
        let most = rate as usize + burst as usize - 1;
        assert_eq!(all(&starts), most, "{case}: the busiest second");
        // Not slower than the rate either: the last send starts about
        // (N - burst) / rate seconds in.
        let last = starts[N - 1].as_secs_f64();
        let expected = f64::from(u32::try_from(N).unwrap() - burst) / f64::from(rate);
        assert!(
            (expected..expected + 1.0).contains(&last),
            "{case}: last send at {last} s, expected about {expected} s"
        );
    }
}

/// A throttle (`130429`) under load halves the rate once, however many
/// sends in flight report it; 30 quiet seconds later the full rate is back.
#[tokio::test(start_paused = true)]
async fn a_slow_down_holds_then_recovers_under_load() {
    const N: usize = 5_000;
    let clock = TokioClock::new();
    let throttled_once: Arc<Mutex<HashSet<String>>> = Arc::default();
    let outbound = Slow::new(&clock, Duration::from_millis(200)).failing({
        let throttled_once = Arc::clone(&throttled_once);
        move |start, to| {
            let window = Duration::from_secs(5)..Duration::from_millis(5300);
            (window.contains(&start) && throttled_once.lock().unwrap().insert(to.to_owned()))
                .then(throttled)
        }
    });
    let pacer = Pacer::new(TokenBucket::new(Rate::DEFAULT)).with_timer(clock.clone());
    let report = Broadcast::builder(NUMBER)
        .to((0..N).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer)
        .build()
        .unwrap()
        .run()
        .await;

    assert_eq!(report.progress().sent, N);
    let throttled = throttled_once.lock().unwrap().len();
    assert!((20..=25).contains(&throttled), "{throttled} throttled");
    let starts = outbound.starts();
    assert_eq!(starts.len(), N + throttled, "each throttled send once more");
    let slowed = outbound.failed_at.lock().unwrap()[0];
    let secs = |s: f64| slowed + Duration::from_secs_f64(s);
    assert_eq!(busiest_second(&starts, Duration::ZERO, slowed), 80);
    // The sends booked before the slow-down (one per sender) go at the
    // full rate; from then on, 40 a second, one halving only.
    assert_eq!(busiest_second(&starts, secs(0.5), secs(29.0)), 40);
    // Back to 80 a second after 30 quiet seconds.
    assert_eq!(busiest_second(&starts, secs(31.0), Duration::MAX), 80);
    assert_eq!(all(&starts), 80);
}

/// The wall clock stepping back an hour mid-run neither pauses the run for
/// the hour nor lets a burst through: on Tokio's monotonic clock, the
/// sends keep the rate.
#[tokio::test(start_paused = true)]
async fn a_wall_clock_stepping_back_neither_pauses_nor_bursts() {
    const N: usize = 400;
    let clock = TokioClock::new();
    let outbound = Slow::new(&clock, Duration::from_millis(100));
    let pacer =
        Pacer::new(TokenBucket::new(Rate::per_second(20).unwrap())).with_timer(clock.clone());
    let stepper = {
        let clock = clock.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            clock.step_back(Duration::from_secs(3600));
        }
    };
    let run = Broadcast::builder(NUMBER)
        .to((0..N).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer)
        .concurrency(8)
        .build()
        .unwrap()
        .run();
    let (report, ()) = tokio::join!(run, stepper);

    assert_eq!(report.progress().sent, N);
    let starts = outbound.starts();
    assert_eq!(all(&starts), 20);
    let last = starts[N - 1].as_secs_f64();
    assert!(last < 21.0, "the last send at {last} s: the run paused");
    assert!(
        matches!(report.recipients[N - 1].outcome, SendOutcome::Sent(_)),
        "{:?}",
        report.recipients[N - 1].outcome
    );
}

/// Meta's throughput upgrade takes a number off for up to a minute, the
/// API answering `131057` meanwhile (`throughput`); a large broadcast can
/// trigger it. No recipient is lost to that minute: each is retried until
/// it is over, and the pacer slows down rather than trying the whole list
/// against a number that cannot send.
#[tokio::test(start_paused = true)]
async fn a_minute_of_maintenance_loses_no_recipient() {
    const N: usize = 400;
    let clock = TokioClock::new();
    let outbound = Slow::new(&clock, Duration::from_millis(200)).failing(|start, _| {
        (start < Duration::from_secs(60))
            .then(|| graph(131_057, "(#131057) Business Account is in maintenance mode"))
    });
    let report = Broadcast::builder(NUMBER)
        .to((0..N).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(Pacer::new(TokenBucket::new(Rate::DEFAULT)).with_timer(clock.clone()))
        .build()
        .unwrap()
        .run()
        .await;
    let progress = report.progress();
    assert_eq!((progress.sent, progress.failed), (N, 0));
    let refused = outbound.failed_at.lock().unwrap().len();
    let first_minute = outbound
        .starts()
        .iter()
        .filter(|s| **s < Duration::from_secs(60))
        .count();
    assert_eq!(refused, first_minute);
    // Slowing down, it tries fewer sends in that minute than the list
    // holds; at the full rate it would try all 400 in 5 s, then each again
    // 20 and 40 s later.
    assert!(
        refused < N,
        "{refused} sends tried against the number in maintenance"
    );
}

/// A retry pending when the wall clock steps back an hour goes when it
/// was due, not an hour later.
#[tokio::test(start_paused = true)]
async fn a_retry_pending_when_the_wall_clock_steps_back_goes_when_due() {
    const N: usize = 400;
    let clock = TokioClock::new();
    let limited = Arc::new(Mutex::new(false));
    let outbound = Slow::new(&clock, Duration::from_millis(100)).failing(move |start, _| {
        let mut once = limited.lock().unwrap();
        (start >= Duration::from_millis(4500) && !*once).then(|| {
            *once = true;
            graph(131_056, "(#131056) Pair rate limit hit")
        })
    });
    let stepper = {
        let clock = clock.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            clock.step_back(Duration::from_secs(3600));
        }
    };
    let run = Broadcast::builder(NUMBER)
        .to((0..N).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(
            Pacer::new(TokenBucket::new(Rate::per_second(20).unwrap())).with_timer(clock.clone()),
        )
        .concurrency(8)
        .build()
        .unwrap()
        .run();
    let (report, ()) = tokio::join!(run, stepper);

    assert_eq!(report.progress().sent, N);
    let sends = outbound.sends();
    let limited: Vec<Duration> = {
        let first = &sends
            .iter()
            .find(|(t, _)| *t >= Duration::from_millis(4500))
            .unwrap()
            .1;
        sends
            .iter()
            .filter(|(_, to)| to == first)
            .map(|(t, _)| *t)
            .collect()
    };
    assert_eq!(limited.len(), 2);
    // Pair-limited at 4.5 s, answered at 4.6 s, due again 1 s later, then
    // behind the slots the 8 senders booked (8 at 50 ms): not an hour on.
    let again = limited[1].as_secs_f64();
    assert!(
        (5.6..6.1).contains(&again),
        "the retry went at {again} s, not when due"
    );
    assert!(sends[N].0 < Duration::from_secs(21));
}

/// A `TokenBucket` that counts the slots handed back to it.
#[derive(Debug)]
struct Counting {
    bucket: TokenBucket,
    releases: AtomicUsize,
}

#[async_trait]
impl RateLimiter for Counting {
    async fn reserve(&self, request: &SlotRequest) -> meta_whatsapp_core::Result<Reservation> {
        self.bucket.reserve(request).await
    }

    async fn release(
        &self,
        request: &SlotRequest,
        reservation: &Reservation,
    ) -> meta_whatsapp_core::Result<()> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        self.bucket.release(request, reservation).await
    }

    async fn slow_down(
        &self,
        from: &PhoneNumberId,
        now: OffsetDateTime,
    ) -> meta_whatsapp_core::Result<()> {
        self.bucket.slow_down(from, now).await
    }
}

/// The slots a cancel hands back never put two sends in one slot: a
/// cancel racing 32 senders (at a slot's very time for half the seeds),
/// while three handlers of a bot take slots of the same number before,
/// during and after it, over 64 seeds. Every start, the broadcast's and the bot's, is at
/// least one interval (20 ms at 50 a second) after the one before.
/// Giving back a slot with a live one after it fails this.
#[tokio::test(start_paused = true)]
async fn a_cancel_racing_the_senders_never_puts_two_sends_in_one_slot() {
    const N: usize = 300;
    const INTERVAL: Duration = Duration::from_millis(20);
    let mut releases = 0;
    let mut cancelled = 0;
    for seed in 0..64_u64 {
        let clock = TokioClock::new();
        let limiter = Arc::new(Counting {
            bucket: TokenBucket::new(Rate::per_second(50).unwrap()).adaptive(false),
            releases: AtomicUsize::new(0),
        });
        let pacer = Pacer::shared(limiter.clone()).with_timer(clock.clone());
        let outbound = Slow::new(&clock, Duration::from_millis(5 + seed * 7 % 60));
        let broadcast = Broadcast::builder(NUMBER)
            .to((0..N).map(phone))
            .content(Text::new("Spring sale"))
            .outbound(outbound.clone())
            .pacer(pacer.clone())
            .concurrency(32)
            .build()
            .unwrap();
        let handle = broadcast.handle();
        let cancel_at = if seed % 2 == 0 {
            INTERVAL * u32::try_from(seed * 13 % 150).unwrap()
        } else {
            Duration::from_millis(1 + seed * 37 % 3000)
        };
        let canceller = async move {
            tokio::time::sleep(cancel_at).await;
            handle.cancel();
        };
        // Three handlers of a bot on the number, each taking 20 slots: one
        // may book while another's slot is still ahead.
        let bot_starts: Arc<Mutex<Vec<Duration>>> = Arc::default();
        let bot = futures::future::join_all((0..3_u64).map(|task| {
            let (pacer, clock, starts) = (pacer.clone(), clock.clone(), Arc::clone(&bot_starts));
            async move {
                let from = PhoneNumberId::new(NUMBER);
                for i in 0..20_u64 {
                    pacer.acquire(&from).await.unwrap();
                    starts.lock().unwrap().push(clock.elapsed());
                    let pause = (i * seed + task) % 7 * 30;
                    tokio::time::sleep(Duration::from_millis(pause)).await;
                }
            }
        }));
        let (report, (), _) = tokio::join!(broadcast.run(), canceller, bot);

        let sent = outbound.starts();
        let progress = report.progress();
        assert_eq!(progress.sent, sent.len(), "seed {seed}");
        assert_eq!(progress.sent + progress.skipped, N, "seed {seed}");
        if report.ended == BroadcastEnd::Cancelled {
            cancelled += 1;
        }
        let mut all = sent;
        all.extend(bot_starts.lock().unwrap().iter().copied());
        all.sort();
        assert_eq!(all.len(), progress.sent + 60, "seed {seed}");
        for pair in all.windows(2) {
            assert!(
                pair[1] - pair[0] >= INTERVAL,
                "seed {seed}: two starts {:?} apart, at {:?} and {:?}",
                pair[1] - pair[0],
                pair[0],
                pair[1]
            );
        }
        releases += limiter.releases.load(Ordering::SeqCst);
    }
    // Not vacuous: the runs were cancelled with senders waiting, whose
    // slots went back.
    assert_eq!(cancelled, 64);
    assert!(releases > 64, "{releases} slots given back");
}

/// `TokenBucket::set_rate` while 32 senders wait for their slots: the
/// slots booked before keep the old spacing, the ones booked after take
/// the new rate, and no two starts are closer than the new interval.
/// Removing the change fails this.
#[tokio::test(start_paused = true)]
async fn a_rate_set_while_thirty_two_senders_wait_takes_effect() {
    const N: usize = 1_000;
    let clock = TokioClock::new();
    let outbound = Slow::new(&clock, Duration::from_millis(200));
    let bucket = Arc::new(TokenBucket::new(Rate::per_second(20).unwrap()).adaptive(false));
    let pacer = Pacer::shared(bucket.clone()).with_timer(clock.clone());
    let upgrade = {
        let bucket = Arc::clone(&bucket);
        async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            bucket.set_rate(NUMBER, Rate::per_second(100).unwrap());
        }
    };
    let run = Broadcast::builder(NUMBER)
        .to((0..N).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer)
        .concurrency(32)
        .build()
        .unwrap()
        .run();
    let (report, ()) = tokio::join!(run, upgrade);

    assert_eq!(report.progress().sent, N);
    let starts = outbound.starts();
    let gaps = |from: Duration, to: Duration| -> Vec<Duration> {
        starts
            .windows(2)
            .filter(|w| w[0] >= from && w[1] < to)
            .map(|w| w[1] - w[0])
            .collect()
    };
    // 20 a second until the change: 50 ms apart.
    assert!(
        gaps(Duration::ZERO, Duration::from_secs(5))
            .iter()
            .all(|g| *g == Duration::from_millis(50))
    );
    assert_eq!(
        busiest_second(&starts, Duration::ZERO, Duration::from_secs(4)),
        20
    );
    // The slots booked before it (one per sender, 1.6 s) are kept; then
    // 100 a second, 10 ms apart, never closer.
    let after = gaps(Duration::from_millis(6_700), Duration::MAX);
    assert!(!after.is_empty());
    assert!(
        after.iter().all(|g| *g == Duration::from_millis(10)),
        "{after:?}"
    );
    assert_eq!(
        busiest_second(&starts, Duration::from_millis(6_700), Duration::MAX),
        100
    );
    assert!(
        starts
            .windows(2)
            .all(|w| w[1] - w[0] >= Duration::from_millis(10))
    );
}
