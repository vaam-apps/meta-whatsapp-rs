//! Bounded channel to a worker task.

use std::fmt;

use async_trait::async_trait;
use tokio::sync::{Semaphore, mpsc};
use meta_whatsapp_core::error::SinkError;
use meta_whatsapp_core::sink::EventSink;

/// What [`ChannelSink`] does when the channel is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelMode {
    /// Wait for room (backpressure). The webhook request waits with it, so
    /// a stalled worker eventually makes Meta time out and redeliver.
    #[default]
    Wait,
    /// Fail immediately with [`SinkError::Full`]; the webhook answers
    /// non-`200` and Meta redelivers later.
    TryOrFail,
}

/// Sends events into a bounded tokio `mpsc` channel, for a worker to
/// process off the webhook's request path. Cheap to clone.
pub struct ChannelSink<E> {
    tx: mpsc::Sender<E>,
    mode: ChannelMode,
}

/// A [`ChannelSink`] in [`ChannelMode::Wait`] and its receiver.
///
/// `capacity` is clamped to `1..=` tokio's maximum instead of panicking like
/// `tokio::sync::mpsc::channel` does on `0`.
pub fn channel<E>(capacity: usize) -> (ChannelSink<E>, mpsc::Receiver<E>) {
    let (tx, rx) = mpsc::channel(capacity.clamp(1, Semaphore::MAX_PERMITS));
    (ChannelSink::new(tx), rx)
}

impl<E> ChannelSink<E> {
    /// Wrap an existing sender, in [`ChannelMode::Wait`].
    pub fn new(tx: mpsc::Sender<E>) -> Self {
        Self {
            tx,
            mode: ChannelMode::Wait,
        }
    }

    /// Set what happens when the channel is full.
    #[must_use]
    pub fn with_mode(mut self, mode: ChannelMode) -> Self {
        self.mode = mode;
        self
    }

    /// The mode in use.
    pub fn mode(&self) -> ChannelMode {
        self.mode
    }

    /// Whether the receiver is gone (every delivery would fail).
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

impl<E> Clone for ChannelSink<E> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            mode: self.mode,
        }
    }
}

impl<E> fmt::Debug for ChannelSink<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelSink")
            .field("mode", &self.mode)
            .field("capacity", &self.tx.max_capacity())
            .field("available", &self.tx.capacity())
            .field("closed", &self.tx.is_closed())
            .finish()
    }
}

#[async_trait]
impl<E: Send + 'static> EventSink<E> for ChannelSink<E> {
    async fn deliver(&self, event: E) -> Result<(), SinkError> {
        match self.mode {
            ChannelMode::Wait => self.tx.send(event).await.map_err(|_| SinkError::Closed),
            ChannelMode::TryOrFail => self.tx.try_send(event).map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => SinkError::Full,
                mpsc::error::TrySendError::Closed(_) => SinkError::Closed,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn delivers_in_order() {
        let (sink, mut rx) = channel(4);
        for i in 0..3 {
            sink.deliver(i).await.unwrap();
        }
        assert_eq!(
            [
                rx.recv().await.unwrap(),
                rx.recv().await.unwrap(),
                rx.recv().await.unwrap()
            ],
            [0, 1, 2]
        );
    }

    #[tokio::test]
    async fn try_or_fail_reports_full_then_recovers() {
        let (sink, mut rx) = channel(1);
        let sink = sink.with_mode(ChannelMode::TryOrFail);
        sink.deliver(1).await.unwrap();
        // Bounded: a TryOrFail that waited would hang here forever.
        let full = tokio::time::timeout(Duration::from_secs(5), sink.deliver(2))
            .await
            .expect("TryOrFail never waits for room");
        assert!(matches!(full, Err(SinkError::Full)));
        assert_eq!(rx.recv().await, Some(1));
        sink.deliver(3).await.unwrap();
        assert_eq!(rx.recv().await, Some(3));
    }

    #[tokio::test(start_paused = true)]
    async fn wait_mode_applies_backpressure_until_room() {
        let (sink, mut rx) = channel(1);
        sink.deliver(1).await.unwrap();
        let blocked = tokio::time::timeout(Duration::from_secs(5), sink.deliver(2)).await;
        assert!(blocked.is_err(), "a full channel makes Wait wait");
        let pending = tokio::spawn({
            let sink = sink.clone();
            async move { sink.deliver(3).await }
        });
        assert_eq!(rx.recv().await, Some(1));
        pending.await.unwrap().unwrap();
        assert_eq!(rx.recv().await, Some(3));
    }

    #[tokio::test]
    async fn dropped_receiver_is_closed_in_both_modes() {
        let (sink, rx) = channel::<u8>(4);
        drop(rx);
        assert!(sink.is_closed());
        assert!(matches!(sink.deliver(1).await, Err(SinkError::Closed)));
        let sink = sink.with_mode(ChannelMode::TryOrFail);
        assert!(matches!(sink.deliver(1).await, Err(SinkError::Closed)));
    }

    #[tokio::test]
    async fn zero_capacity_is_clamped_instead_of_panicking() {
        let (sink, mut rx) = channel(0);
        sink.deliver("x").await.unwrap();
        assert_eq!(rx.recv().await, Some("x"));
        assert!(format!("{sink:?}").contains("capacity: 1"));
    }
}
