//! Deliver one event to several sinks.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_core::error::SinkError;
use meta_whatsapp_core::sink::EventSink;

/// Delivers each event to every inner sink, concurrently.
///
/// **Partial failure:** every sink is attempted even when another fails;
/// `deliver` then returns the first transient error in the order the sinks
/// were added, else the first permanent one
/// ([`SinkError::Permanent`]): the fan-out refuses an event permanently
/// only when every sink that failed did, so a webhook handler with a
/// dead-letter store never acknowledges an event a sink could still take
/// on a redelivery. Sinks that succeeded keep the event (nothing is rolled
/// back), and since an error makes Meta redeliver the batch (or a
/// dead letter is replayed to the whole fan-out), they will see it again —
/// make them idempotent, or put a dedup guard in front of the fan-out.
///
/// With no sinks, delivery succeeds and does nothing.
pub struct FanoutSink<E> {
    sinks: Vec<Arc<dyn EventSink<E>>>,
}

impl<E: Clone + Send + 'static> FanoutSink<E> {
    /// No sinks yet.
    pub fn new() -> Self {
        Self { sinks: Vec::new() }
    }

    /// Add a sink (builder style).
    #[must_use]
    pub fn with(mut self, sink: impl EventSink<E>) -> Self {
        self.push(sink);
        self
    }

    /// Add a sink.
    pub fn push(&mut self, sink: impl EventSink<E>) {
        self.sinks.push(Arc::new(sink));
    }

    /// Add an already shared sink.
    pub fn push_arc(&mut self, sink: Arc<dyn EventSink<E>>) {
        self.sinks.push(sink);
    }

    /// Number of sinks.
    pub fn len(&self) -> usize {
        self.sinks.len()
    }

    /// Whether there are no sinks.
    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }
}

impl<E: Clone + Send + 'static> Default for FanoutSink<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E> Clone for FanoutSink<E> {
    fn clone(&self) -> Self {
        Self {
            sinks: self.sinks.clone(),
        }
    }
}

impl<E> fmt::Debug for FanoutSink<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FanoutSink")
            .field("sinks", &self.sinks)
            .finish()
    }
}

#[async_trait]
impl<E: Clone + Send + 'static> EventSink<E> for FanoutSink<E> {
    async fn deliver(&self, event: E) -> Result<(), SinkError> {
        let Some((last, rest)) = self.sinks.split_last() else {
            return Ok(());
        };
        let mut deliveries = Vec::with_capacity(self.sinks.len());
        for sink in rest {
            deliveries.push(sink.deliver(event.clone()));
        }
        deliveries.push(last.deliver(event));
        // join_all keeps input order, so "first" means first added.
        let mut permanent = None;
        for result in futures::future::join_all(deliveries).await {
            match result {
                Ok(()) => {}
                Err(error) if error.is_permanent() => {
                    permanent.get_or_insert(error);
                }
                Err(transient) => return Err(transient),
            }
        }
        permanent.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Records what it receives; optionally fails with a fixed error.
    #[derive(Debug, Default)]
    struct Recorder {
        seen: Mutex<Vec<u32>>,
        fail: Option<fn() -> SinkError>,
    }

    #[async_trait]
    impl EventSink<u32> for Recorder {
        async fn deliver(&self, event: u32) -> Result<(), SinkError> {
            self.seen.lock().unwrap().push(event);
            self.fail.map_or(Ok(()), |f| Err(f()))
        }
    }

    fn recorder(fail: Option<fn() -> SinkError>) -> Arc<Recorder> {
        Arc::new(Recorder {
            seen: Mutex::default(),
            fail,
        })
    }

    #[tokio::test]
    async fn delivers_to_all() {
        let (a, b) = (recorder(None), recorder(None));
        let sink = FanoutSink::new().with(Arc::clone(&a)).with(Arc::clone(&b));
        assert_eq!(sink.len(), 2);
        sink.deliver(1).await.unwrap();
        sink.deliver(2).await.unwrap();
        assert_eq!(*a.seen.lock().unwrap(), [1, 2]);
        assert_eq!(*b.seen.lock().unwrap(), [1, 2]);
    }

    #[tokio::test]
    async fn attempts_all_and_returns_the_first_error_in_order() {
        let ok_before = recorder(None);
        let full = recorder(Some(|| SinkError::Full));
        let ok_between = recorder(None);
        let closed = recorder(Some(|| SinkError::Closed));
        let ok_after = recorder(None);
        let mut sink = FanoutSink::new();
        for s in [&ok_before, &full, &ok_between, &closed, &ok_after] {
            sink.push_arc(Arc::clone(s) as Arc<dyn EventSink<u32>>);
        }
        let err = sink.deliver(9).await.unwrap_err();
        assert!(
            matches!(err, SinkError::Full),
            "first failing sink wins: {err:?}"
        );
        for s in [&ok_before, &full, &ok_between, &closed, &ok_after] {
            assert_eq!(*s.seen.lock().unwrap(), [9], "every sink was attempted");
        }
    }

    fn permanent() -> SinkError {
        SinkError::permanent(std::io::Error::other("refused for good"))
    }

    /// A transient error wins over a permanent one, whatever their order:
    /// a dead-letter store must not acknowledge an event another sink
    /// could still take (roadmap L21a). Only when every failure is
    /// permanent is the fan-out's.
    #[tokio::test]
    async fn a_transient_error_wins_over_a_permanent_one() {
        let refused = recorder(Some(permanent));
        let full = recorder(Some(|| SinkError::Full));
        let ok = recorder(None);
        let mut sink = FanoutSink::new();
        for s in [&refused, &ok, &full] {
            sink.push_arc(Arc::clone(s) as Arc<dyn EventSink<u32>>);
        }
        let err = sink.deliver(1).await.unwrap_err();
        assert!(matches!(err, SinkError::Full), "{err:?}");

        let only_permanent = FanoutSink::new()
            .with(recorder(None))
            .with(recorder(Some(permanent)))
            .with(recorder(Some(permanent)));
        let err = only_permanent.deliver(2).await.unwrap_err();
        assert!(err.is_permanent(), "{err:?}");
    }

    #[tokio::test]
    async fn empty_fanout_is_a_no_op() {
        let sink = FanoutSink::<u32>::default();
        assert!(sink.is_empty());
        sink.deliver(1).await.unwrap();
    }
}
