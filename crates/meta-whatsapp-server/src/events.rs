//! Meta's webhooks into the inbox and the event outbox, and polling the
//! outbox (docs/design/server.md, sections 2.3 and 4.2): what
//! `POST /webhooks/meta` needs of an HTTP body (its places among the
//! deliveries read at once, how long it may take, the refusals' log), over
//! the core's webhook pipeline. The pipeline itself (the signature before
//! anything is stored, the library's `WebhookHandler`, each event's dedup
//! lease, routing by the records, the inbox, then the outbox) is the
//! core's [`meta_whatsapp_server_core::inbound`] since roadmap S5e, and
//! what an event becomes (its route, its outbox row, key and id) and
//! polling are [`meta_whatsapp_server_core::events`]; both are re-exported
//! here. The HTTP handlers (`api::webhooks`, `api::events`) only translate.
//!
//! ```text
//! POST /webhooks/meta ─► api::webhooks: a well-formed X-Hub-Signature-256 header
//!                        (401 before the body), a place (64; 503), the body
//!                        (3 MiB: 413; 15 s: 408) ─► the core's Pipeline::deliver:
//!                        the signature against every app secret (401 before
//!                        parsing or any store), a turn (4; 503 after 10 s),
//!                        WebhookHandler (the library): parse, per event:
//!                        DedupGuard lease (the backend's KvStore, 503 while
//!                        another request holds it) ─► the core's sink:
//!                          1. route: the tenant owning the number, else the WABA,
//!                             since before the event
//!                          2. inbox (InboxSink), when a tenant owns it
//!                          3. outbox row: the tenant, or none (operator-only)
//! GET /v1/events ─► poll: the caller's tenant's rows after a sequence of its own
//! ```
//!
//! **Routing is an allow-list** and **Meta's retries are safe**: see
//! [`meta_whatsapp_server_core::inbound`] and
//! [`meta_whatsapp_server_core::events`], whose rules these are. The
//! pipeline's log lines keep this module's path as their target
//! (`meta_whatsapp_server::events`), with the same fields.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use meta_whatsapp_rs::core::secret::VerifyToken;
use meta_whatsapp_rs::webhooks::axum::body::Bytes;
use meta_whatsapp_server_core::backend::Backend;
pub use meta_whatsapp_server_core::events::{
    DEFAULT_OUTBOX_RETENTION, EventIdKey, EventKey, HOUSEKEEPING_INTERVAL, KEYLESS_DEDUP_WINDOW,
    MAX_PAGE_DATA_BYTES, OPERATOR_EVENT_TYPES, Polled, Route, RowError, TENANT_EVENT_TYPES,
    event_data, event_number, meta_time, outbox_key, outbox_row, owner, poll, purge_outbox, route,
    tenant_visible,
};
use meta_whatsapp_server_core::inbound::{Delivered, Pipeline, Refused};
pub use meta_whatsapp_server_core::inbound::{
    Inbound, MAX_DELIVERIES_RECORDING, MAX_WEBHOOK_BODY_BYTES, RECORDING_WAIT,
};
use meta_whatsapp_server_core::outbox::Outbox;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::metrics::Metrics;

/// Meta's deliveries a replica reads and records at once: past it, a
/// delivery is answered `503` before its body is read (Meta retries). The
/// bodies read at once stay under 64 × 3 MiB, whoever sends them: the
/// signature can only be checked once a body is read (security review
/// M1).
pub const MAX_DELIVERIES_IN_FLIGHT: usize = 64;

/// How long a delivery's body may take to arrive: past it, `408` (Meta
/// retries), so slow bodies cannot hold the deliveries' places.
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(15);

/// The webhook pipeline and the outbox, as the routes use them: the
/// core's [`Pipeline`], and what the listener needs of a body.
pub(crate) struct Events {
    pipeline: Pipeline,
    /// Places for deliveries being read and recorded
    /// ([`MAX_DELIVERIES_IN_FLIGHT`]).
    in_flight: Arc<Semaphore>,
    rejections: RejectionLog,
}

/// What a refused delivery is refused for, as the rejection log counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rejection {
    /// No well-formed signature header: `401` before the body.
    Unsigned = 0,
    /// A signature no app secret produced: `401`.
    Forged = 1,
    /// Over 3 MiB: `413`.
    TooLarge = 2,
    /// The body took too long: `408`.
    SlowBody = 3,
    /// No place or turn: `503`.
    Busy = 4,
    /// The body broke off before it all arrived (a reset connection).
    Broken = 5,
}

/// Warnings about refused deliveries, at most one a minute for each
/// [`Rejection`], saying how many were not written since: anyone who
/// reaches the public listener can have requests refused, and each must
/// not cost a log line (security review L5). The metric
/// (`wa_server_webhook_deliveries_total`) counts every one.
#[derive(Debug)]
pub(crate) struct RejectionLog {
    started: std::time::Instant,
    /// Per rejection: the millisecond (since `started`) from which the next
    /// line may be written, and the refusals not written since the last.
    slots: [(AtomicU64, AtomicU64); 6],
}

impl RejectionLog {
    /// Between two lines about the same rejection.
    pub(crate) const EVERY: Duration = Duration::from_secs(60);

    fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
            slots: Default::default(),
        }
    }

    /// Whether to write a line about `rejection` now: `Some(n)` with the
    /// `n` refusals not written since the last line, else `None` (this one
    /// is counted for the next line).
    pub(crate) fn admit(&self, rejection: Rejection) -> Option<u64> {
        let (next, suppressed) = &self.slots[rejection as usize];
        let now = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let every = u64::try_from(Self::EVERY.as_millis()).unwrap_or(u64::MAX);
        let due = next.load(Ordering::Relaxed);
        if now >= due
            && next
                .compare_exchange(
                    due,
                    now.saturating_add(every),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
        {
            return Some(suppressed.swap(0, Ordering::Relaxed));
        }
        suppressed.fetch_add(1, Ordering::Relaxed);
        None
    }
}

impl Events {
    /// The pipeline `inbound` describes, on `backend`'s stores alone
    /// (the core's [`Pipeline::new`]), counted by `metrics`.
    pub(crate) fn new(inbound: Inbound, backend: &dyn Backend, metrics: Metrics) -> Self {
        Self {
            pipeline: Pipeline::new(inbound, backend, Arc::new(metrics)),
            in_flight: Arc::new(Semaphore::new(MAX_DELIVERIES_IN_FLIGHT)),
            rejections: RejectionLog::new(),
        }
    }

    /// The log of refused deliveries.
    pub(crate) fn rejections(&self) -> &RejectionLog {
        &self.rejections
    }

    /// A place for one delivery, or `None` when
    /// [`MAX_DELIVERIES_IN_FLIGHT`] are being read or recorded: hold it
    /// while reading the body and recording it.
    pub(crate) fn admit(&self) -> Option<OwnedSemaphorePermit> {
        self.in_flight.clone().try_acquire_owned().ok()
    }

    /// One of Meta's deliveries, through the core's pipeline: the
    /// signature, a turn to record, the library's `WebhookHandler` (size,
    /// parsing, the dedup lease), the service's sink.
    ///
    /// # Errors
    ///
    /// The pipeline's [`Refused`].
    pub(crate) async fn deliver(
        &self,
        signature: Option<&str>,
        body: Bytes,
    ) -> Result<Delivered, Refused> {
        self.pipeline.deliver(signature, body).await
    }

    /// The outbox.
    pub(crate) fn outbox(&self) -> &dyn Outbox {
        self.pipeline.outbox()
    }

    /// Meta's webhook verify token.
    pub(crate) fn verify_token(&self) -> &VerifyToken {
        self.pipeline.verify_token()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One line a minute per rejection, counting the ones left out.
    #[test]
    fn the_rejection_log_writes_a_line_a_minute_per_rejection() {
        let log = RejectionLog::new();
        assert_eq!(log.admit(Rejection::Unsigned), Some(0));
        for _ in 0..5 {
            assert_eq!(log.admit(Rejection::Unsigned), None);
        }
        assert_eq!(log.admit(Rejection::Forged), Some(0), "each its own");
        // A minute later: the next line says how many were left out.
        log.slots[Rejection::Unsigned as usize]
            .0
            .store(0, Ordering::Relaxed);
        assert_eq!(log.admit(Rejection::Unsigned), Some(5));
        assert_eq!(log.admit(Rejection::Unsigned), None);
    }

    /// The numbers docs/design/server.md (sections 2.3 and 6) and the guide
    /// state. The behaviour tests time themselves by these constants (a
    /// test stepping `KEYLESS_DEDUP_WINDOW` passes whatever the window
    /// is), so only this pins their values. Decisive: each value.
    #[test]
    fn the_pipelines_numbers_are_the_designs() {
        assert_eq!(MAX_WEBHOOK_BODY_BYTES, 3 * 1024 * 1024, "3 MiB");
        assert_eq!(MAX_DELIVERIES_IN_FLIGHT, 64);
        assert_eq!(BODY_READ_TIMEOUT, Duration::from_secs(15));
        assert_eq!(MAX_DELIVERIES_RECORDING, 4);
        assert_eq!(RECORDING_WAIT, Duration::from_secs(10));
        assert_eq!(KEYLESS_DEDUP_WINDOW, Duration::from_hours(1), "an hour");
        assert_eq!(RejectionLog::EVERY, Duration::from_mins(1), "a minute");
        assert_eq!(DEFAULT_OUTBOX_RETENTION, Duration::from_hours(7 * 24));
        assert_eq!(HOUSEKEEPING_INTERVAL, Duration::from_mins(10));
        assert_eq!(MAX_PAGE_DATA_BYTES, 8 * 1024 * 1024, "8 MiB");
    }
}
