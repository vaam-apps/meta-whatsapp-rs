//! Retry policy.
//!
//! Two questions decide a retry, and they are different questions:
//!
//! 1. *Could it succeed later?* — [`meta_whatsapp_core::Error::is_retryable`].
//! 2. *Is it safe to send again?* — idempotent requests (GET, DELETE) always
//!    are. A send (POST `/messages`) is only replayed when the error proves
//!    Meta rejected it before doing anything (throttling:
//!    [`meta_whatsapp_core::ErrorKind::is_rejected_before_processing`]). A timeout on a
//!    send is **never** replayed: the message may already be on its way, and
//!    a duplicate OTP or order confirmation is worse than a surfaced error.

use std::time::Duration;

use meta_whatsapp_core::Error;

/// Exponential backoff with full jitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after the first attempt. `0` disables retrying.
    pub max_retries: u32,
    /// Delay cap for the first retry; doubles each retry.
    pub base_delay: Duration,
    /// Upper bound for any single delay (including a server `Retry-After`).
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(8),
        }
    }
}

impl RetryPolicy {
    /// Never retry.
    pub const NONE: Self = Self {
        max_retries: 0,
        base_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
    };

    /// Whether attempt number `attempt` (0-based, the one that just failed
    /// with `error`) should be followed by another.
    pub fn should_retry(&self, attempt: u32, error: &Error, idempotent: bool) -> bool {
        if attempt >= self.max_retries || !error.is_retryable() {
            return false;
        }
        if idempotent {
            return true;
        }
        match error {
            Error::Api(e) => e.kind().is_rejected_before_processing(),
            Error::Http { status: 429, .. } => true,
            _ => false,
        }
    }

    /// Delay before retry number `attempt + 1`. `retry_after` (from the
    /// response) wins when present, capped at `max_delay`.
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(d) = retry_after {
            return d.min(self.max_delay);
        }
        let exp = self
            .base_delay
            .saturating_mul(2u32.saturating_pow(attempt))
            .min(self.max_delay);
        // Full jitter: uniform in [0, exp].
        let nanos = u64::try_from(exp.as_nanos()).unwrap_or(u64::MAX);
        if nanos == 0 {
            return Duration::ZERO;
        }
        let mut buf = [0u8; 8];
        if getrandom::fill(&mut buf).is_err() {
            return exp;
        }
        Duration::from_nanos(u64::from_le_bytes(buf) % (nanos + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meta_whatsapp_core::GraphApiError;
    use meta_whatsapp_core::error::TransportError;

    fn api(code: i64) -> Error {
        Error::from(GraphApiError::new(code, "x"))
    }

    #[test]
    fn sends_are_only_replayed_when_rejected_before_processing() {
        let p = RetryPolicy::default();
        // Throughput limit: safe to replay a send.
        assert!(p.should_retry(0, &api(130429), false));
        // Meta hiccup: retryable, but a send may have gone out.
        assert!(p.should_retry(0, &api(131000), true));
        assert!(!p.should_retry(0, &api(131000), false));
        // Timeout on a send: never replayed.
        let timeout = Error::Transport(TransportError::Timeout);
        assert!(p.should_retry(0, &timeout, true));
        assert!(!p.should_retry(0, &timeout, false));
        // Not retryable at all.
        assert!(!p.should_retry(0, &api(131050), true));
        // Budget exhausted.
        assert!(!p.should_retry(3, &api(130429), true));
    }

    #[test]
    fn delay_is_capped_and_honours_retry_after() {
        let p = RetryPolicy::default();
        for attempt in 0..10 {
            assert!(p.delay(attempt, None) <= p.max_delay);
        }
        assert_eq!(
            p.delay(0, Some(Duration::from_secs(60))),
            p.max_delay,
            "server Retry-After is capped"
        );
        assert_eq!(
            p.delay(0, Some(Duration::from_millis(5))),
            Duration::from_millis(5)
        );
        assert_eq!(RetryPolicy::NONE.delay(3, None), Duration::ZERO);
    }
}
