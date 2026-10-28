//! The log half of `templates.rs`: a template id not found in the first
//! pages of its name's templates is logged at `warn` under the target the
//! service's operators have always filtered on
//! (`meta_whatsapp_server::api::templates`), which the line kept when the
//! lookup moved into the core (roadmap S5c). Alone in its binary, as
//! `capabilities_logs.rs` is: the capture is a thread's default
//! subscriber, and `tracing` caches whether a call site is enabled across
//! threads.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

mod memory;

use std::sync::{Arc, Mutex};

use meta_whatsapp_server_core::conformance;

/// Every event logged while it is the thread's subscriber: its level, its
/// target and its message.
#[derive(Clone, Default)]
struct Logged(Arc<Mutex<Vec<(tracing::Level, String, String)>>>);

impl tracing::Subscriber for Logged {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0.lock().unwrap().push((
            *event.metadata().level(),
            event.metadata().target().to_owned(),
            message.0,
        ));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// The lookup past its pages warns once per lookup, under the service's
/// old target. Decisive: the line's `target`.
#[tokio::test]
async fn a_lookup_past_its_pages_warns_under_the_services_target() {
    let backend = memory::Memory::default();
    let logged = Logged::default();
    let guard = tracing::subscriber::set_default(logged.clone());
    conformance::templates::a_template_id_past_the_lookups_five_pages_is_not_found(&backend).await;
    drop(guard);
    let events = logged.0.lock().unwrap().clone();
    let warned: Vec<_> = events
        .iter()
        .filter(|(_, _, message)| message.contains("not found in the first pages"))
        .collect();
    assert_eq!(warned.len(), 2, "the get and the deletion: {events:?}");
    for (level, target, _) in warned {
        assert_eq!(*level, tracing::Level::WARN);
        assert_eq!(target, "meta_whatsapp_server::api::templates");
    }
}
