//! Live fan-out to any number of subscribers (inbox UIs over SSE or
//! WebSocket).

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::Stream;
use meta_whatsapp_core::error::SinkError;
use meta_whatsapp_core::sink::EventSink;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

/// Publishes every event to all current subscribers through a tokio
/// `broadcast` channel. Cheap to clone.
///
/// Each event is wrapped in one [`Arc`] when it is delivered, and every
/// subscriber receives a clone of that `Arc`: the same allocation, never a
/// copy of the event. Ten open inboxes cost ten reference counts, not ten
/// copies of a multi-megabyte history sync. The channel carries
/// `Arc<E>` for that reason, so [`from_sender`](Self::from_sender) takes a
/// `broadcast::Sender<Arc<E>>` and [`receiver`](Self::receiver) returns a
/// `broadcast::Receiver<Arc<E>>`. `E` need not be `Clone`; it must be
/// `Sync`, since subscribers on other threads read the one copy.
///
/// Delivery never fails and never waits: with no subscriber the event is
/// dropped (nobody is watching — not an error, so Meta is not asked to
/// redeliver), and a subscriber that falls more than `capacity` events
/// behind skips the oldest ones and sees a [`Lagged`] item instead. Live
/// views are best-effort by design; persist events with a
/// `ConversationStore` or a [`ChannelSink`](super::ChannelSink) when they
/// must not be lost.
pub struct BroadcastSink<E> {
    tx: broadcast::Sender<Arc<E>>,
}

impl<E: Send + Sync + 'static> BroadcastSink<E> {
    /// A sink whose subscribers each buffer up to `capacity` events
    /// (clamped to at least 1 instead of panicking like tokio on `0`).
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.clamp(1, usize::MAX / 2));
        Self { tx }
    }

    /// Wrap an existing sender, e.g. one whose receivers you hand to
    /// `meta_whatsapp_webhooks::sse`.
    pub fn from_sender(tx: broadcast::Sender<Arc<E>>) -> Self {
        Self { tx }
    }

    /// A raw tokio receiver, for APIs that take one (e.g. `meta_whatsapp_webhooks::sse`).
    /// Sees events delivered from now on, each as the `Arc` every other
    /// subscriber shares.
    pub fn receiver(&self) -> broadcast::Receiver<Arc<E>> {
        self.tx.subscribe()
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
impl<E: Send + Sync + 'static> EventSink<E> for BroadcastSink<E> {
    async fn deliver(&self, event: E) -> Result<(), SinkError> {
        // One allocation per event, shared by every subscriber: a receiver
        // clones what was sent, and cloning an `Arc` copies a pointer.
        // `send` only fails when there is no subscriber: nobody to tell.
        let _ = self.tx.send(Arc::new(event));
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
/// [`BroadcastSink::subscribe`], each as the [`Arc`] every subscriber
/// shares. Yields `Err(Lagged)` where events were skipped, then carries on
/// with the oldest event still buffered; ends when every sink handle is
/// dropped.
pub struct BroadcastSubscription<E> {
    inner: BroadcastStream<Arc<E>>,
}

impl<E> fmt::Debug for BroadcastSubscription<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BroadcastSubscription")
            .finish_non_exhaustive()
    }
}

impl<E: Send + Sync + 'static> Stream for BroadcastSubscription<E> {
    type Item = Result<Arc<E>, Lagged>;

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
                assert_eq!(s.next().await, Some(Ok(Arc::new(i))));
            }
        }
    }

    /// Roadmap L21b's decisive test: an event is allocated once, whatever
    /// the number of subscribers. A subscription that cloned the event
    /// (the behaviour before L21b, a copy per subscriber) gives each one
    /// its own allocation and fails `Arc::ptr_eq`.
    #[tokio::test]
    async fn ten_subscribers_share_one_allocation_per_event() {
        // Big enough that a copy would matter (a history sync is megabytes).
        #[derive(Debug, Clone)]
        struct Event(Vec<u8>);

        let sink = BroadcastSink::new(4);
        let mut subscriptions: Vec<_> = (0..10).map(|_| sink.subscribe()).collect();
        let mut receivers: Vec<_> = (0..10).map(|_| sink.receiver()).collect();
        assert_eq!(sink.receiver_count(), 20);
        sink.deliver(Event(vec![7; 64 * 1024])).await.unwrap();

        let mut received = Vec::new();
        for s in &mut subscriptions {
            received.push(s.next().await.unwrap().unwrap());
        }
        for r in &mut receivers {
            received.push(r.recv().await.unwrap());
        }
        assert_eq!(received.len(), 20);
        let first = &received[0];
        assert_eq!(first.0.len(), 64 * 1024);
        for (i, event) in received.iter().enumerate() {
            assert!(
                Arc::ptr_eq(first, event),
                "subscriber {i} got its own copy of the event, not the shared allocation"
            );
        }
    }

    #[tokio::test]
    async fn events_need_not_be_clone() {
        #[derive(Debug, PartialEq)]
        struct NotClone(u8);

        let sink = BroadcastSink::new(2);
        let mut s = sink.subscribe();
        sink.deliver(NotClone(1)).await.unwrap();
        assert_eq!(s.next().await, Some(Ok(Arc::new(NotClone(1)))));
    }

    #[tokio::test]
    async fn slow_subscriber_sees_lagged_then_resumes() {
        let sink = BroadcastSink::new(2);
        let mut s = sink.subscribe();
        for i in 0..5u8 {
            sink.deliver(i).await.unwrap();
        }
        assert_eq!(s.next().await, Some(Err(Lagged { skipped: 3 })));
        assert_eq!(s.next().await, Some(Ok(Arc::new(3))));
        assert_eq!(s.next().await, Some(Ok(Arc::new(4))));
    }

    #[tokio::test]
    async fn stream_ends_when_the_sink_is_dropped() {
        let sink = BroadcastSink::new(2);
        let mut s = sink.subscribe();
        sink.deliver(7u8).await.unwrap();
        drop(sink);
        assert_eq!(s.next().await, Some(Ok(Arc::new(7))));
        assert_eq!(s.next().await, None);
    }
}
