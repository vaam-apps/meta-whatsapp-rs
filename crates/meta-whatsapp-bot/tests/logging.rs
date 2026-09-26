//! What the bot logs about an event: kinds, types, durations and error
//! kinds, never the message's content, the sender, their number or the
//! error's text. Captured with a minimal in-test `tracing` subscriber.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use meta_whatsapp_bot::{Bot, Command, Ctx, Logging};
use meta_whatsapp_core::Error;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

use common::{Recording, text_event};

/// Every event as `LEVEL field=value …`.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<String>>>);

struct Line(String);

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, " {}={value:?}", field.name());
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
        let mut line = Line(event.metadata().level().to_string());
        event.record(&mut line);
        self.0.lock().unwrap().push(line.0);
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

/// What must never appear: the texts, the sender's name, number and
/// BSUID, the message id, and the handler's error text.
const PRIVATE: [&str; 7] = [
    "top secret order",
    "Sheena",
    "16505551234",
    "US.13491208655302741918",
    "wamid.",
    "a handler's own failure",
    "realsheenanelson",
];

#[test]
fn the_logs_carry_no_content_and_no_identity() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let bot = Bot::builder()
            .outbound(Recording::default())
            .middleware(Logging)
            .command(Command::new("fail", |_ctx: Ctx| async move {
                Err(Error::Other(anyhow::anyhow!("a handler's own failure")))
            }))
            .command(Command::new("order", |ctx: Ctx| async move {
                ctx.reply("your top secret order shipped").await?;
                Ok(())
            }))
            .build()
            .await
            .unwrap();
        bot.handle(text_event("messages/text.json", "/fail top secret order"))
            .await
            .unwrap();
        bot.handle(text_event("messages/text.json", "/order top secret order"))
            .await
            .unwrap();
        bot.handle(text_event(
            "bsuid/text_username_no_wa_id.json",
            "top secret order",
        ))
        .await
        .unwrap();
    });
    let lines = capture.0.lock().unwrap().join("\n");
    // The events were logged…
    assert!(lines.contains("event=\"message_received\""), "{lines}");
    assert!(lines.contains("message_type=\"text\""), "{lines}");
    assert!(lines.contains("bot event failed"), "{lines}");
    assert!(lines.contains("command=\"fail\""), "{lines}");
    assert!(lines.contains("error_kind=Unknown"), "{lines}");
    // …without anything personal.
    for private in PRIVATE {
        assert!(!lines.contains(private), "{private:?} was logged:\n{lines}");
    }
}
