//! Log events with `tracing`.

use std::borrow::Cow;
use std::fmt;

use async_trait::async_trait;
use meta_whatsapp_core::error::SinkError;
use meta_whatsapp_core::sink::EventSink;
use tracing::Level;

/// Emits one `tracing` event per delivered event, at a configurable level
/// (default `INFO`), and never fails.
///
/// By default only the event's type is logged: webhook events carry
/// customer phone numbers, names and message text, which do not belong in
/// logs unless you opt in with [`TracingSink::with_payload`] (which logs the
/// event's `Debug` output — `meta-whatsapp-rs` types keep secrets out of `Debug`, your
/// own event types must too).
#[derive(Debug, Clone)]
pub struct TracingSink {
    level: Level,
    payload: bool,
    name: Cow<'static, str>,
}

impl Default for TracingSink {
    fn default() -> Self {
        Self::new()
    }
}

impl TracingSink {
    /// `INFO`, no payload, named `"wa"`.
    pub fn new() -> Self {
        Self {
            level: Level::INFO,
            payload: false,
            name: Cow::Borrowed("wa"),
        }
    }

    /// Log at `level`.
    #[must_use]
    pub fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Include the event's `Debug` output as the `payload` field.
    #[must_use]
    pub fn with_payload(mut self, payload: bool) -> Self {
        self.payload = payload;
        self
    }

    /// Value of the `sink` field, to tell several tracing sinks apart.
    #[must_use]
    pub fn name(mut self, name: impl Into<Cow<'static, str>>) -> Self {
        self.name = name.into();
        self
    }
}

/// `tracing::event!` needs its level as a constant, hence one arm per level.
/// `Level` has exactly these five values; the last arm is `TRACE`.
macro_rules! event_at {
    ($level:expr, $($args:tt)+) => {
        match $level {
            Level::ERROR => tracing::event!(Level::ERROR, $($args)+),
            Level::WARN => tracing::event!(Level::WARN, $($args)+),
            Level::INFO => tracing::event!(Level::INFO, $($args)+),
            Level::DEBUG => tracing::event!(Level::DEBUG, $($args)+),
            _ => tracing::event!(Level::TRACE, $($args)+),
        }
    };
}

#[async_trait]
impl<E: fmt::Debug + Send + 'static> EventSink<E> for TracingSink {
    async fn deliver(&self, event: E) -> Result<(), SinkError> {
        let event_type = std::any::type_name::<E>();
        if self.payload {
            event_at!(self.level, sink = %self.name, event_type, payload = ?event, "event delivered");
        } else {
            event_at!(self.level, sink = %self.name, event_type, "event delivered");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    use super::*;

    /// `(level, [(field, value)])` of one captured event.
    type Captured = (Level, Vec<(String, String)>);

    /// Captures every event.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<Captured>>>);

    struct Fields(Vec<(String, String)>);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.0.push((field.name().to_owned(), format!("{value:?}")));
        }
    }

    impl Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut fields = Fields(Vec::new());
            event.record(&mut fields);
            self.0
                .lock()
                .unwrap()
                .push((*event.metadata().level(), fields.0));
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    #[derive(Debug)]
    struct Order {
        #[expect(dead_code, reason = "only read through Debug, which is the point")]
        customer_phone: &'static str,
    }

    async fn capture(sink: TracingSink) -> Vec<Captured> {
        let cap = Capture::default();
        let _guard = tracing::subscriber::set_default(cap.clone());
        sink.deliver(Order {
            customer_phone: "+15551234567",
        })
        .await
        .unwrap();
        cap.0.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn logs_type_but_no_payload_by_default() {
        let events = capture(TracingSink::new()).await;
        assert_eq!(events.len(), 1);
        let (level, fields) = &events[0];
        assert_eq!(*level, Level::INFO);
        let rendered = format!("{fields:?}");
        assert!(rendered.contains("Order"), "event type logged: {rendered}");
        assert!(rendered.contains("\"wa\""), "sink name logged: {rendered}");
        assert!(
            !rendered.contains("5551234567"),
            "payload must not be logged by default: {rendered}"
        );
        assert!(!fields.iter().any(|(k, _)| k == "payload"));
    }

    #[tokio::test]
    async fn payload_and_level_are_opt_in() {
        let sink = TracingSink::new()
            .level(Level::WARN)
            .with_payload(true)
            .name("audit");
        let events = capture(sink).await;
        assert_eq!(events.len(), 1);
        let (level, fields) = &events[0];
        assert_eq!(*level, Level::WARN);
        let rendered = format!("{fields:?}");
        assert!(rendered.contains("5551234567"), "{rendered}");
        assert!(rendered.contains("audit"), "{rendered}");
    }

    #[tokio::test]
    async fn every_level_is_honoured() {
        for level in [
            Level::TRACE,
            Level::DEBUG,
            Level::INFO,
            Level::WARN,
            Level::ERROR,
        ] {
            let events = capture(TracingSink::new().level(level)).await;
            assert_eq!(events[0].0, level);
        }
    }
}
