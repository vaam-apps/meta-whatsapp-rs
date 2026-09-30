//! The log half of `capabilities.rs`: a capability another `Authorizer`
//! made is refused with a `warn` naming the operation, and nothing the
//! forged capability or the vault holds. Alone in its binary, as the
//! service's `logs.rs` and `forged_logs.rs` are: the capture is a thread's
//! default subscriber, and `tracing` caches whether a call site is enabled
//! across threads, so a test running beside it can leave the capture's
//! call sites disabled (the capture then sees nothing).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

use support::*;

/// Every event logged while it is the thread's subscriber: its level and
/// its fields, as text.
#[derive(Clone, Default)]
struct Logged(Arc<Mutex<Vec<(tracing::Level, String)>>>);

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
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                let _ = write!(self.0, "{}={value:?} ", field.name());
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.0
            .lock()
            .unwrap()
            .push((*event.metadata().level(), fields.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// A refusal is logged at `warn` with the operation's name, and nothing
/// the forged capability or the vault holds.
#[tokio::test]
async fn a_refusal_is_logged_at_warn_without_secrets() {
    let (service, forger) = service_and_forger().await;
    let admin = forger.admin().await;
    let logged = Logged::default();
    let guard = tracing::subscriber::set_default(logged.clone());
    let error = service
        .authz
        .store_token(&admin, &victim(), &stored(FORGED_TOKEN))
        .await
        .unwrap_err();
    drop(guard);
    assert_forbidden(&error, "store_token");
    let events = logged.0.lock().unwrap().clone();
    let refusals: Vec<_> = events
        .iter()
        .filter(|(_, fields)| fields.contains("another authorizer"))
        .collect();
    assert_eq!(refusals.len(), 1, "{events:?}");
    let (level, fields) = refusals[0];
    assert_eq!(*level, tracing::Level::WARN);
    assert!(fields.contains("operation=\"store_token\""), "{fields}");
    let secret = forger.admin_bearer.trim_start_matches("Bearer ");
    for (_, fields) in &events {
        for text in [VICTIM_TOKEN, FORGED_TOKEN, secret, admin.key_id()] {
            assert!(!fields.contains(text), "{fields} names {text}");
        }
    }
}
