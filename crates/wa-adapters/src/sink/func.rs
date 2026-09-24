//! A sink from an async closure.

use std::fmt;
use std::future::Future;

use async_trait::async_trait;
use wa_core::error::SinkError;
use wa_core::sink::EventSink;

/// Calls an async closure for every event.
///
/// ```
/// # async fn demo() {
/// use wa_adapters::sink::FnSink;
/// use wa_core::error::SinkError;
/// use wa_core::sink::EventSink;
///
/// let sink = FnSink::new(|n: u32| async move {
///     if n == 0 {
///         return Err(SinkError::Delivery(anyhow::anyhow!("zero is not an order id")));
///     }
///     Ok(())
/// });
/// assert!(sink.deliver(7).await.is_ok());
/// assert!(sink.deliver(0).await.is_err());
/// # }
/// ```
#[derive(Clone)]
pub struct FnSink<F> {
    f: F,
}

impl<F> FnSink<F> {
    /// Wrap `f`. It receives each event by value and returns a future of
    /// the delivery result.
    pub fn new(f: F) -> Self {
        Self { f }
    }
}

impl<F> fmt::Debug for FnSink<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnSink")
            .field("fn", &std::any::type_name::<F>())
            .finish()
    }
}

#[async_trait]
impl<E, F, Fut> EventSink<E> for FnSink<F>
where
    E: Send + 'static,
    F: Fn(E) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), SinkError>> + Send,
{
    async fn deliver(&self, event: E) -> Result<(), SinkError> {
        (self.f)(event).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    #[tokio::test]
    async fn calls_the_closure_and_returns_its_result() {
        let total = Arc::new(AtomicU32::new(0));
        let sink = FnSink::new({
            let total = Arc::clone(&total);
            move |n: u32| {
                let total = Arc::clone(&total);
                async move {
                    if n == 13 {
                        return Err(SinkError::Full);
                    }
                    total.fetch_add(n, Ordering::SeqCst);
                    Ok(())
                }
            }
        });
        sink.deliver(2).await.unwrap();
        sink.deliver(5).await.unwrap();
        assert!(matches!(sink.deliver(13).await, Err(SinkError::Full)));
        assert_eq!(total.load(Ordering::SeqCst), 7);
    }
}
