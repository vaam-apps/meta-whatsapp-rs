//! When a failed attempt is tried again (design §4.4): "backoff with
//! jitter from 10 s, at most 1 h a step, for 72 h, then `failed`".
//!
//! After the `n`-th failed attempt the next one waits `10 s × 2^(n-1)`,
//! at most an hour, scaled by a jitter factor drawn in `[0.8, 1.2)` and
//! still at most an hour: 10 s, 20 s, 40 s, … 42 min 40 s, then an hour a
//! step. A delivery whose next attempt would fall after its
//! `give_up_at` (its creation, or its last manual retry, and the window,
//! `WA_SERVER_WEBHOOK_RETRY_WINDOW`) is `failed` instead. The jitter is a
//! [`Jitter`] of the dispatcher's, so a test draws it.

use std::time::Duration;

use time::OffsetDateTime;

use super::DEFAULT_RETRY_WINDOW;

/// The first retry's wait.
pub const FIRST_RETRY: Duration = Duration::from_secs(10);

/// The longest wait between two attempts.
pub const LONGEST_RETRY: Duration = Duration::from_hours(1);

/// How far the jitter moves a wait, either way: a fifth.
pub const JITTER: f64 = 0.2;

/// Where in its range a wait falls: a number in `[0, 1)`.
pub trait Jitter: Send + Sync + 'static {
    /// The next draw, in `[0, 1)`.
    fn unit(&self) -> f64;
}

/// Draws from the system's randomness (0.5, no jitter, without it).
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomJitter;

impl Jitter for RandomJitter {
    fn unit(&self) -> f64 {
        let mut bytes = [0u8; 4];
        if getrandom::fill(&mut bytes).is_err() {
            return 0.5;
        }
        f64::from(u32::from_le_bytes(bytes)) / (f64::from(u32::MAX) + 1.0)
    }
}

/// The schedule: the first wait, the longest, the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetrySchedule {
    /// The first retry's wait ([`FIRST_RETRY`]).
    pub first: Duration,
    /// The longest wait ([`LONGEST_RETRY`]).
    pub longest: Duration,
    /// How long a delivery is retried (`WA_SERVER_WEBHOOK_RETRY_WINDOW`).
    pub window: Duration,
}

impl Default for RetrySchedule {
    fn default() -> Self {
        Self {
            first: FIRST_RETRY,
            longest: LONGEST_RETRY,
            window: DEFAULT_RETRY_WINDOW,
        }
    }
}

impl RetrySchedule {
    /// The wait after the `attempts`-th failed attempt (1 for the first),
    /// at the jitter draw `unit` (in `[0, 1)`; 0.5 is no jitter).
    pub fn delay(&self, attempts: u32, unit: f64) -> Duration {
        let doublings = attempts.saturating_sub(1).min(32);
        let base = self
            .first
            .saturating_mul(1u32 << doublings.min(31))
            .min(self.longest);
        let factor = 1.0 - JITTER + 2.0 * JITTER * unit.clamp(0.0, 1.0);
        base.mul_f64(factor).min(self.longest)
    }

    /// When the attempt after the `attempts`-th failed one is due, from
    /// `now`; `None` when that is after `give_up_at`: the delivery fails.
    pub fn next(
        &self,
        attempts: u32,
        now: OffsetDateTime,
        give_up_at: OffsetDateTime,
        unit: f64,
    ) -> Option<OffsetDateTime> {
        let wait = time::Duration::try_from(self.delay(attempts, unit)).ok()?;
        let at = now.checked_add(wait)?;
        (at <= give_up_at).then_some(at)
    }

    /// The `give_up_at` of a delivery made, or retried by hand, at `now`.
    pub fn give_up_at(&self, now: OffsetDateTime) -> OffsetDateTime {
        time::Duration::try_from(self.window)
            .ok()
            .and_then(|window| now.checked_add(window))
            .unwrap_or(OffsetDateTime::new_utc(
                time::Date::MAX,
                time::Time::MIDNIGHT,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The design's schedule, without jitter (a draw of 0.5): 10 s
    /// doubling to an hour a step, an hour from the tenth wait on.
    /// Decisive: the doubling, its first wait, its cap.
    #[test]
    fn the_waits_double_from_10_s_to_an_hour() {
        let s = RetrySchedule::default();
        let waits: Vec<u64> = (1..=12).map(|n| s.delay(n, 0.5).as_secs()).collect();
        assert_eq!(
            waits,
            [10, 20, 40, 80, 160, 320, 640, 1280, 2560, 3600, 3600, 3600]
        );
        assert_eq!(s.delay(u32::MAX, 0.5), LONGEST_RETRY);
    }

    /// The jitter moves a wait by a fifth at most either way, and never
    /// past an hour. Decisive: the factor's bounds, the cap after it.
    #[test]
    fn the_jitter_is_a_fifth_either_way_and_never_past_an_hour() {
        let s = RetrySchedule::default();
        assert_eq!(s.delay(1, 0.0), Duration::from_secs(8));
        let top = s.delay(1, 0.999_999);
        assert!(top < Duration::from_secs(12) && top > Duration::from_millis(11_990));
        assert_eq!(s.delay(10, 0.0), Duration::from_mins(48));
        assert_eq!(s.delay(10, 0.999), LONGEST_RETRY);
        assert_eq!(s.delay(3, 0.25), Duration::from_secs(36));
        for n in 0..100 {
            let unit = RandomJitter.unit();
            assert!((0.0..1.0).contains(&unit));
            let d = s.delay(n % 14 + 1, unit);
            assert!(d <= LONGEST_RETRY);
        }
    }

    /// A delivery is retried while its next attempt falls within its
    /// window, and fails at the first that would not: over 72 hours
    /// without jitter, 80 attempts (9 doubling waits, 5,110 s, then 70 of an
    /// hour). Decisive: `next` refusing an
    /// attempt after `give_up_at`.
    #[test]
    fn a_delivery_is_retried_for_its_window_then_fails() {
        let s = RetrySchedule::default();
        let start = time::macros::datetime!(2026-11-02 10:00 UTC);
        let give_up = s.give_up_at(start);
        assert_eq!(give_up - start, time::Duration::hours(72));
        let mut at = start;
        let mut attempts = 0;
        while let Some(next) = s.next(attempts + 1, at, give_up, 0.5) {
            attempts += 1;
            at = next;
        }
        assert_eq!(attempts + 1, 80, "attempts made, the first included");
        assert!(at <= give_up);
        assert!(at + time::Duration::hours(1) > give_up);
    }
}
