//! Live fan-out to any number of subscribers (inbox UIs over SSE or
//! WebSocket).

use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use wa_core::error::SinkError;
use wa_core::sink::EventSink;

/// Publishes every event to all current subscribers through a tokio
/// `broadcast` channel. Cheap to clone.
///
/// Delivery never fails and never waits: with no subscriber the event is
/// dropped (nobody is watching — not an error, so Meta is not asked to
/// redeliver), and a subscriber that falls more than `capacity` events
/// behind skips the oldest ones and sees a [`Lagged`] item instead. Live
/// views are best-effort by design; persist events with a
/// `ConversationStore` or a [`ChannelSink`](super::ChannelSink) when they
/// must not be lost.
pub struct BroadcastSink<E> {
    tx: broadcast::Sender<E>,
}

impl<E: Clone + Send + 'static> BroadcastSink<E> {
    /// A sink whose subscribers each buffer up to `capacity` events
    /// (clamped to at least 1 instead of panicking like tokio on `0`).
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.clamp(1, usize::MAX / 2));
        Self { tx }
    }

    /// Wrap an existing sender.
    pub fn from_sender(tx: broadcast::Sender<E>) -> Self {
        Self { tx }
    }

    /// A new subscription, seeing events delivered from now on.
    pub fn subscribe(&self) -> BroadcastSubscription<E> {
        BroadcastSubscription {
            inner: BroadcastStream::new(self.tx.subscribe()),
        }
    }

    /// Current number of subscriptions.
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl<E> Clone for BroadcastSink<E> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<E> fmt::Debug for BroadcastSink<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BroadcastSink")
            .field("subscribers", &self.tx.receiver_count())
            .finish()
    }
}

#[async_trait]
impl<E: Clone + Send + 'static> EventSink<E> for BroadcastSink<E> {
    async fn deliver(&self, event: E) -> Result<(), SinkError> {
        // `send` only fails when there is no subscriber: nobody to tell.
        let _ = self.tx.send(event);
        Ok(())
    }
}

/// A subscriber fell behind and missed events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lagged {
    /// How many events were skipped.
    pub skipped: u64,
}

impl fmt::Display for Lagged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "subscriber lagged and missed {} events", self.skipped)
    }
}

impl std::error::Error for Lagged {}

/// A [`Stream`] of the events delivered to a [`BroadcastSink`] after
/// [`BroadcastSink::subscribe`]. Yields `Err(Lagged)` where events were
/// skipped, then carries on with the oldest event still buffered; ends when
/// every sink handle is dropped.
pub struct BroadcastSubscription<E> {
    inner: BroadcastStream<E>,
}

impl<E> fmt::Debug for BroadcastSubscription<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BroadcastSubscription")
            .finish_non_exhaustive()
    }
}

impl<E: Clone + Send + 'static> Stream for BroadcastSubscription<E> {
    type Item = Result<E, Lagged>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx).map(|item| {
            item.map(|r| r.map_err(|BroadcastStreamRecvError::Lagged(skipped)| Lagged { skipped }))
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;

    #[tokio::test]
    async fn no_subscriber_is_not_an_error() {
        let sink = BroadcastSink::new(4);
        assert_eq!(sink.receiver_count(), 0);
        sink.deliver(1u8).await.unwrap();
    }

    #[tokio::test]
    async fn every_subscriber_sees_every_later_event() {
        let sink = BroadcastSink::new(8);
        sink.deliver(0u8).await.unwrap(); // before anyone subscribed
        let mut a = sink.subscribe();
        let mut b = sink.subscribe();
        assert_eq!(sink.receiver_count(), 2);
        for i in 1..=3u8 {
            sink.deliver(i).await.unwrap();
        }
        for s in [&mut a, &mut b] {
            for i in 1..=3u8 {
                assert_eq!(s.next().await, Some(Ok(i)));
            }
        }
    }

    #[tokio::test]
    async fn slow_subscriber_sees_lagged_then_resumes() {
        let sink = BroadcastSink::new(2);
        let mut s = sink.subscribe();
        for i in 0..5u8 {
            sink.deliver(i).await.unwrap();
        }
        assert_eq!(s.next().await, Some(Err(Lagged { skipped: 3 })));
        assert_eq!(s.next().await, Some(Ok(3)));
        assert_eq!(s.next().await, Some(Ok(4)));
    }

    #[tokio::test]
    async fn stream_ends_when_the_sink_is_dropped() {
        let sink = BroadcastSink::new(2);
        let mut s = sink.subscribe();
        sink.deliver(7u8).await.unwrap();
        drop(sink);
        assert_eq!(s.next().await, Some(Ok(7)));
        assert_eq!(s.next().await, None);
    }
}
