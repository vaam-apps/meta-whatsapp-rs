//! Pass only matching events to an inner sink.

use std::fmt;

use async_trait::async_trait;
use meta_whatsapp_core::error::SinkError;
use meta_whatsapp_core::sink::EventSink;

/// Delivers an event to the inner sink when `predicate` returns `true`;
/// otherwise drops it and reports success (a filtered-out event is handled,
/// not failed, so it must not make Meta redeliver).
///
/// ```
/// # async fn demo() {
/// use meta_whatsapp_adapters::sink::{FilterSink, channel};
/// use meta_whatsapp_core::sink::EventSink;
///
/// let (worker, mut rx) = channel::<u32>(8);
/// let evens = FilterSink::new(worker, |n: &u32| n % 2 == 0);
/// evens.deliver(1).await.unwrap();
/// evens.deliver(2).await.unwrap();
/// assert_eq!(rx.recv().await, Some(2));
/// # }
/// ```
#[derive(Clone)]
pub struct FilterSink<S, F> {
    inner: S,
    predicate: F,
}

impl<S, F> FilterSink<S, F> {
    /// Filter `inner` with `predicate`.
    pub fn new(inner: S, predicate: F) -> Self {
        Self { inner, predicate }
    }

    /// The inner sink.
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S: fmt::Debug, F> fmt::Debug for FilterSink<S, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FilterSink")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl<E, S, F> EventSink<E> for FilterSink<S, F>
where
    E: Send + 'static,
    S: EventSink<E>,
    F: Fn(&E) -> bool + Send + Sync + 'static,
{
    async fn deliver(&self, event: E) -> Result<(), SinkError> {
        if (self.predicate)(&event) {
            self.inner.deliver(event).await
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::{ChannelMode, channel};

    #[tokio::test]
    async fn passes_matching_and_drops_the_rest() {
        let (inner, mut rx) = channel::<u32>(8);
        let sink = FilterSink::new(inner, |n: &u32| *n > 10);
        for n in [1, 20, 3, 40] {
            sink.deliver(n).await.unwrap();
        }
        drop(sink);
        let mut got = Vec::new();
        while let Some(n) = rx.recv().await {
            got.push(n);
        }
        assert_eq!(got, [20, 40]);
    }

    #[tokio::test]
    async fn inner_errors_propagate_but_filtered_events_never_fail() {
        let (inner, rx) = channel::<u32>(1);
        drop(rx);
        let sink = FilterSink::new(inner.with_mode(ChannelMode::TryOrFail), |n: &u32| *n > 10);
        sink.deliver(1).await.unwrap();
        assert!(matches!(sink.deliver(11).await, Err(SinkError::Closed)));
        assert!(format!("{sink:?}").starts_with("FilterSink { inner: ChannelSink"));
    }
}
