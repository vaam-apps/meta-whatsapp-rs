//! Injectable "now". Expiry, rate limits and the customer service window all
//! depend on time; tests pin it with [`ManualClock`].

use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use time::OffsetDateTime;

/// Source of the current time.
pub trait Clock: Send + Sync + fmt::Debug + 'static {
    /// Current UTC time.
    fn now(&self) -> OffsetDateTime;
}

/// Wall-clock time.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

impl<T: Clock + ?Sized> Clock for Arc<T> {
    fn now(&self) -> OffsetDateTime {
        (**self).now()
    }
}

/// A clock that only moves when told to.
#[derive(Debug, Clone)]
pub struct ManualClock(Arc<Mutex<OffsetDateTime>>);

impl ManualClock {
    /// Start at `at`.
    pub fn new(at: OffsetDateTime) -> Self {
        Self(Arc::new(Mutex::new(at)))
    }

    /// Move forward.
    pub fn advance(&self, by: Duration) {
        let mut t = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *t += by;
    }

    /// Move forward to `at`, or stay where it is when already there or
    /// later: never back. Atomic, so tasks sharing the clock that each
    /// wait until a deadline leave it at the latest one, not at the sum
    /// of their waits.
    pub fn advance_to(&self, at: OffsetDateTime) {
        let mut t = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if at > *t {
            *t = at;
        }
    }

    /// Jump to `at`.
    pub fn set(&self, at: OffsetDateTime) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = at;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> OffsetDateTime {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn manual_clock_advances() {
        let c = ManualClock::new(datetime!(2026-01-01 0:00 UTC));
        c.advance(Duration::from_secs(90));
        assert_eq!(c.now(), datetime!(2026-01-01 0:01:30 UTC));
    }

    #[test]
    fn manual_clock_advances_to_a_later_time_only() {
        let c = ManualClock::new(datetime!(2026-01-01 0:00 UTC));
        c.advance_to(datetime!(2026-01-01 0:00:50 UTC));
        c.advance_to(datetime!(2026-01-01 0:00:20 UTC));
        assert_eq!(c.now(), datetime!(2026-01-01 0:00:50 UTC));
        c.advance_to(datetime!(2026-01-01 0:01 UTC));
        assert_eq!(c.now(), datetime!(2026-01-01 0:01 UTC));
    }
}
