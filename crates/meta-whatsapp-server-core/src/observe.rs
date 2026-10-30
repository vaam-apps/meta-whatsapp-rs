//! What an operation reports beside its answer, for the API adapter's
//! metrics: the core counts nothing itself, and names no metrics library.
//! The service's Prometheus registry (`meta_whatsapp_server::metrics`)
//! implements [`Observer`]; `()` observes nothing.

/// Told what an operation saw on its way, for the adapter's metrics.
pub trait Observer: Send + Sync {
    /// A Graph call failed, answered with the error code `code` (one of
    /// [`crate::error::CODES`]).
    fn graph_error(&self, code: &'static str);

    /// A request met an earlier request's idempotency key
    /// ([`crate::idempotency::run`]): `outcome` is
    /// [`crate::idempotency::Repeat::outcome`]'s (`reused`,
    /// `in_progress`, `outcome_unknown` or `replayed`).
    fn idempotency(&self, outcome: &'static str);

    /// The webhook pipeline ([`crate::inbound::Pipeline`]) recorded a new
    /// outbox row: `event_type` is the library's `WebhookEvent::kind` (a
    /// fixed set), `audience` `tenant` (the row's tenant polls it) or
    /// `operator` (an operator-only row).
    fn webhook_event(&self, event_type: &'static str, audience: &'static str);

    /// The webhook pipeline met `count` events already recorded (at least
    /// one), recognised at `stage`: `dedup` (the dedup lease had seen
    /// them: once per delivery that met any) or `outbox` (the outbox had
    /// it: once per event).
    fn webhook_duplicates(&self, stage: &'static str, count: u64);

    /// The webhook pipeline failed to record an event at `stage`
    /// (`routing`, `serialization`, `inbox`, `outbox`, or `outbox_busy`:
    /// the outbox reported contention, `StorageError::Busy`); Meta
    /// redelivers it.
    fn webhook_failure(&self, stage: &'static str);
}

/// Observes nothing.
impl Observer for () {
    fn graph_error(&self, _: &'static str) {}

    fn idempotency(&self, _: &'static str) {}

    fn webhook_event(&self, _: &'static str, _: &'static str) {}

    fn webhook_duplicates(&self, _: &'static str, _: u64) {}

    fn webhook_failure(&self, _: &'static str) {}
}
