//! The log half of `media.rs`: the media services' own lines (a stream
//! stopped, Meta's digest unusable, Meta's URL refused by the library's
//! host allow-list) are logged at `warn` under the target the service's
//! operators have always filtered on (`meta_whatsapp_server::api::media`),
//! which they kept when the media operations moved into the core (roadmap
//! S5d), and no line carries Meta's media URL (its handle and hash) or
//! the number's token. Alone in its binary, as `templates_logs.rs` is: the
//! capture is a thread's default subscriber, and `tracing` caches whether
//! a call site is enabled across threads.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

mod memory;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use meta_whatsapp_server_core::conformance;

/// One event: its level, its target, its message and every field, as
/// text.
type Event = (tracing::Level, String, String, String);

/// Every event logged while it is the thread's subscriber.
#[derive(Clone, Default)]
struct Logged(Arc<Mutex<Vec<Event>>>);

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
        #[derive(Default)]
        struct Fields {
            message: String,
            all: String,
        }
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.message = format!("{value:?}");
                }
                let _ = write!(self.all, " {}={value:?}", field.name());
            }
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.0.lock().unwrap().push((
            *event.metadata().level(),
            event.metadata().target().to_owned(),
            fields.message,
            fields.all,
        ));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// The decisive case's stops and refusals warn under the service's old
/// target, with the reason a stream stopped; nothing logged names Meta's
/// URL or the token. Decisive: each line's `target`.
#[tokio::test]
async fn the_media_lines_warn_under_the_services_target() {
    let backend = memory::Memory::default();
    let logged = Logged::default();
    let guard = tracing::subscriber::set_default(logged.clone());
    conformance::media::a_download_through_the_cores_stream_ends_before_its_last_chunk_on_a_digest_mismatch(
        &backend,
    )
    .await;
    drop(guard);
    let events = logged.0.lock().unwrap().clone();
    let ours: Vec<_> = events
        .iter()
        .filter(|(_, target, _, _)| target == "meta_whatsapp_server::api::media")
        .map(|(level, _, message, all)| (*level, message.as_str(), all.as_str()))
        .collect();
    let stopped = "a streamed media download stopped: connection aborted";
    let expected = [
        (
            stopped,
            " message=a streamed media download stopped: connection aborted \
             reason=the media failed its integrity check",
        ),
        (
            stopped,
            " message=a streamed media download stopped: connection aborted \
             reason=the media grew past max_bytes",
        ),
        (
            "Meta reported no usable SHA-256 for a media file",
            " message=Meta reported no usable SHA-256 for a media file",
        ),
        (
            "Meta's media URL was refused by the library's host allow-list",
            " message=Meta's media URL was refused by the library's host allow-list",
        ),
    ];
    assert_eq!(
        ours.iter().map(|(_, m, a)| (*m, *a)).collect::<Vec<_>>(),
        expected,
        "{events:?}"
    );
    assert!(
        ours.iter()
            .all(|(level, _, _)| *level == tracing::Level::WARN)
    );
    // The lines moved, none left under the core's own module path.
    assert!(
        !events
            .iter()
            .any(|(_, target, _, _)| target == "meta_whatsapp_server_core::media"),
        "{events:?}"
    );
    for (_, target, _, all) in &events {
        for secret in ["lookaside", "hash=abc", "evil.example", "EAAG-"] {
            assert!(!all.contains(secret), "{target}: {all}");
        }
    }

    // The failing downloads' case: each stop warns with its reason (a cut
    // connection's included), under the same target, and nothing logged
    // names Meta's URL, the other host or the token, Meta's `190` and its
    // details among them.
    let backend = memory::Memory::default();
    let logged = Logged::default();
    let guard = tracing::subscriber::set_default(logged.clone());
    conformance::media::a_failed_download_yields_no_unverified_end_and_frees_its_slot(&backend)
        .await;
    drop(guard);
    let events = logged.0.lock().unwrap().clone();
    let reasons: Vec<&str> = events
        .iter()
        .filter(|(_, target, message, _)| {
            target == "meta_whatsapp_server::api::media" && message == stopped
        })
        .map(|(level, _, _, all)| {
            assert_eq!(*level, tracing::Level::WARN, "{all}");
            all.rsplit("reason=").next().unwrap()
        })
        .collect();
    for reason in [
        "the media failed its integrity check",
        "the media download failed",
        "the media grew past max_bytes",
    ] {
        assert!(reasons.contains(&reason), "{reason}: {events:?}");
    }
    for (_, target, _, all) in &events {
        for secret in ["lookaside", "hash=abc", "evil.example", "EAAG-"] {
            assert!(!all.contains(secret), "{target}: {all}");
        }
    }
}
