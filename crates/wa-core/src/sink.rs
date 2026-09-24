//! The event sink port: where webhook events (or any other events) go.
//!
//! The webhook handler parses and verifies a delivery, then hands each event
//! to one [`EventSink`]. Compose sinks rather than writing one big one —
//! `wa_adapters::sink` ships a channel sink (worker queue), a broadcast sink
//! (live fan-out to SSE/WebSocket subscribers), a fan-out combinator, a
//! closure sink and a tracing sink.
//!
//! A sink should be fast: Meta expects a `200` promptly and redelivers on
//! failure. Queue slow work behind a channel sink instead of doing it inline.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::SinkError;

/// Accepts events of type `E`.
#[async_trait]
pub trait EventSink<E>: Send + Sync + fmt::Debug + 'static
where
    E: Send + 'static,
{
    /// Take one event. An error makes the webhook handler answer non-`200`,
    /// so Meta redelivers the whole batch — make sinks idempotent or put a
    /// dedup guard in front of them.
    async fn deliver(&self, event: E) -> Result<(), SinkError>;
}

#[async_trait]
impl<E, T> EventSink<E> for Arc<T>
where
    E: Send + 'static,
    T: EventSink<E> + ?Sized,
{
    async fn deliver(&self, event: E) -> Result<(), SinkError> {
        (**self).deliver(event).await
    }
}
