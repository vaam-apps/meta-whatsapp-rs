//! Roadmap B2a: a `phone_number_quality_update` saying `THROUGHPUT_UPGRADE`
//! (Meta's documented example, `fields/phone_number_quality_update.json`,
//! parsed by the webhooks crate) raises that number's rate in the pacer's
//! bucket to `Rate::HIGHER_THROUGHPUT`, only when the bot was told to
//! follow upgrades; nothing else changes a rate. Timed on a `ManualClock`:
//! 80 a second is 12.5 ms between slots, 1,000 a second is 1 ms.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{NUMBER, Recording, event, events_of, fixture_json};
use futures::executor::block_on;
use meta_whatsapp_bot::throughput::QUALITY_UPDATE_KIND;
use meta_whatsapp_bot::{
    Bot, Ctx, KnownNumbers, Middleware, Next, NumberDirectory, Pacer, PropagateErrors, Rate,
    RateLimiter, SlotRequest, ThroughputUpgrades, TokenBucket,
};
use meta_whatsapp_core::clock::{Clock, ManualClock};
use meta_whatsapp_core::error::{SinkError, StorageError};
use meta_whatsapp_core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::{Error, Result};
use meta_whatsapp_webhooks::WebhookEvent;
use meta_whatsapp_webhooks::fields::PhoneNumberQualityEvent;
use pretty_assertions::assert_eq;
use serde_json::json;
use time::OffsetDateTime;
use time::macros::datetime;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

const T0: OffsetDateTime = datetime!(2026-09-26 12:00 UTC);
/// Meta's example: the WABA (`entry.id`) and the display number of
/// `fields/phone_number_quality_update.json`.
const WABA: &str = "102290129340398";
const DISPLAY: &str = "15550783881";
const FIXTURE: &str = "fields/phone_number_quality_update.json";

fn id(raw: &str) -> PhoneNumberId {
    PhoneNumberId::new(raw).unwrap()
}

fn waba(raw: &str) -> WabaId {
    WabaId::new(raw).unwrap()
}

/// The fixture's number, registered with a formatted display number (as
/// the phone numbers API may return it): compared by its digits.
fn ours() -> KnownNumbers {
    KnownNumbers::new()
        .number(waba(WABA), "+1 555-078-3881", id(NUMBER))
        .unwrap()
}

/// The fixture with its `event` replaced.
fn quality_update(value: &str) -> WebhookEvent {
    let mut json = fixture_json(FIXTURE);
    json["entry"][0]["changes"][0]["value"]["event"] = json!(value);
    let mut events = events_of(&json);
    assert_eq!(events.len(), 1);
    events.remove(0)
}

/// A bot whose pacer paces with `bucket` on `clock`, following upgrades
/// with `upgrades` when given; and its recording outbound.
async fn bot(
    bucket: &Arc<TokenBucket>,
    clock: &ManualClock,
    upgrades: Option<ThroughputUpgrades>,
) -> (Bot, Pacer, Recording) {
    let pacer = Pacer::shared(bucket.clone()).with_timer(clock.clone());
    let recording = Recording::default();
    let mut builder = Bot::builder()
        .outbound(recording.clone())
        .pacer(pacer.clone());
    if let Some(upgrades) = upgrades {
        builder = builder.follow_throughput_upgrades(upgrades);
    }
    (builder.build().await.unwrap(), pacer, recording)
}

/// The start of each of `n` slots of `number`, from `T0`, in microseconds.
async fn starts(pacer: &Pacer, clock: &ManualClock, number: &str, n: usize) -> Vec<u128> {
    let mut out = Vec::new();
    for _ in 0..n {
        pacer.acquire(&id(number)).await.unwrap();
        out.push(Duration::try_from(clock.now() - T0).unwrap().as_micros());
    }
    out
}

/// The gaps between consecutive starts.
fn gaps(starts: &[u128]) -> Vec<u128> {
    starts.windows(2).map(|w| w[1] - w[0]).collect()
}

/// The decisive test: the fixture, delivered to a bot that follows
/// upgrades, moves its number's sends from 12.5 ms to 1 ms apart (the slot
/// booked before the upgrade keeps its 12.5 ms).
#[tokio::test]
async fn the_upgrade_fixture_moves_the_numbers_sends_from_12_5_ms_to_1_ms_apart() {
    let fixture = event(FIXTURE);
    let WebhookEvent::PhoneNumberQualityUpdated {
        waba_id, update, ..
    } = &fixture
    else {
        panic!("{fixture:?}")
    };
    assert_eq!(fixture.kind(), QUALITY_UPDATE_KIND);
    assert_eq!(waba_id.as_str(), WABA);
    assert_eq!(update.display_phone_number, DISPLAY);
    assert_eq!(update.event, PhoneNumberQualityEvent::ThroughputUpgrade);

    let clock = ManualClock::new(T0);
    let bucket = Arc::new(TokenBucket::default());
    let upgrades = ThroughputUpgrades::new(bucket.clone(), ours());
    let (bot, pacer, recording) = bot(&bucket, &clock, Some(upgrades)).await;

    let before = starts(&pacer, &clock, NUMBER, 3).await;
    assert_eq!(before, [0, 12_500, 25_000]);

    bot.deliver(fixture).await.unwrap();

    let after = starts(&pacer, &clock, NUMBER, 4).await;
    assert_eq!(gaps(&after), [1_000; 3]);
    assert_eq!(after, [37_500, 38_500, 39_500, 40_500]);
    assert_eq!(bucket.rate_of(&id(NUMBER)), Rate::HIGHER_THROUGHPUT);
    // A quality update sends nothing.
    assert!(recording.sent().is_empty());
    assert!(recording.reads.lock().unwrap().is_empty());
}

/// Off by default: a bot with the same pacer that was not told to follow
/// upgrades leaves the number at 80 a second.
#[tokio::test]
async fn without_following_upgrades_the_fixture_changes_no_rate() {
    let clock = ManualClock::new(T0);
    let bucket = Arc::new(TokenBucket::default());
    let (bot, pacer, _) = bot(&bucket, &clock, None).await;

    bot.deliver(event(FIXTURE)).await.unwrap();

    assert_eq!(bucket.rate_of(&id(NUMBER)), Rate::DEFAULT);
    let starts = starts(&pacer, &clock, NUMBER, 4).await;
    assert_eq!(gaps(&starts), [12_500; 3]);
}

/// Only the upgraded number: another number of the same WABA, and one of
/// another WABA with the same display number, keep 80 a second.
#[tokio::test]
async fn other_numbers_keep_their_rate() {
    const SAME_WABA: &str = "106540352242923";
    const OTHER_WABA: &str = "106540352242924";
    let numbers = ours()
        .number(waba(WABA), "15550000000", id(SAME_WABA))
        .unwrap()
        .number(waba("102290129340399"), DISPLAY, id(OTHER_WABA))
        .unwrap();
    let clock = ManualClock::new(T0);
    let bucket = Arc::new(TokenBucket::default());
    let (bot, pacer, _) = bot(
        &bucket,
        &clock,
        Some(ThroughputUpgrades::new(bucket.clone(), numbers)),
    )
    .await;
    // Every number has sent before the upgrade.
    for number in [NUMBER, SAME_WABA, OTHER_WABA] {
        pacer.acquire(&id(number)).await.unwrap();
    }

    bot.deliver(event(FIXTURE)).await.unwrap();

    assert_eq!(bucket.rate_of(&id(NUMBER)), Rate::HIGHER_THROUGHPUT);
    for other in [SAME_WABA, OTHER_WABA] {
        assert_eq!(bucket.rate_of(&id(other)), Rate::DEFAULT, "{other}");
        let starts = starts(&pacer, &clock, other, 3).await;
        assert_eq!(gaps(&starts), [12_500; 2], "{other}");
    }
}

/// `ONBOARDING`, `FLAGGED` (Meta's `solution-providers/manage-webhooks`
/// example, its number registered here) and a value no page lists change
/// no rate: only `THROUGHPUT_UPGRADE` does.
#[tokio::test]
async fn only_a_throughput_upgrade_changes_a_rate() {
    let flagged = event("pages/solution-providers.manage-webhooks__quality_update_received.json");
    let numbers = ours()
        .number(waba("495709166956424"), "124545784358810", id("2"))
        .unwrap();
    let bucket = Arc::new(TokenBucket::default());
    let upgrades = ThroughputUpgrades::new(bucket.clone(), numbers);

    let onboarding = quality_update("ONBOARDING");
    let unknown = quality_update("A_VALUE_NO_PAGE_LISTS");
    for (event, expected) in [
        (&onboarding, PhoneNumberQualityEvent::Onboarding),
        (&flagged, PhoneNumberQualityEvent::Flagged),
        (
            &unknown,
            PhoneNumberQualityEvent::Other("A_VALUE_NO_PAGE_LISTS".to_owned()),
        ),
    ] {
        let WebhookEvent::PhoneNumberQualityUpdated { update, .. } = event else {
            panic!("{event:?}")
        };
        assert_eq!(update.event, expected);
        assert_eq!(upgrades.observe(event).await.unwrap(), None, "{expected:?}");
    }
    assert_eq!(bucket.rate_of(&id(NUMBER)), Rate::DEFAULT);
    assert_eq!(bucket.rate_of(&id("2")), Rate::DEFAULT);

    // A message event is not a quality update either.
    let text = event("messages/text.json");
    assert_eq!(upgrades.observe(&text).await.unwrap(), None);
    assert_eq!(bucket.rate_of(&id(NUMBER)), Rate::DEFAULT);
}

/// A number the directory does not know is not guessed: no rate changes,
/// and the event is not an error (Meta would redeliver it for 7 days).
#[tokio::test]
async fn an_upgrade_of_an_unknown_number_changes_nothing() {
    let bucket = Arc::new(TokenBucket::default());
    // The fixture's display number, under another WABA.
    let numbers = KnownNumbers::new()
        .number(waba("102290129340399"), DISPLAY, id(NUMBER))
        .unwrap();
    let upgrades = ThroughputUpgrades::new(bucket.clone(), numbers);
    assert_eq!(upgrades.observe(&event(FIXTURE)).await.unwrap(), None);
    let empty = ThroughputUpgrades::new(bucket.clone(), KnownNumbers::new());
    empty.deliver(event(FIXTURE)).await.unwrap();
    assert_eq!(bucket.rate_of(&id(NUMBER)), Rate::DEFAULT);
}

/// Hearing the upgrade twice is harmless, and an upgrade never lowers a
/// rate set higher by hand.
#[tokio::test]
async fn an_upgrade_is_idempotent_and_never_lowers_a_rate() {
    let bucket = Arc::new(TokenBucket::default());
    let upgrades = ThroughputUpgrades::new(bucket.clone(), ours());
    assert_eq!(
        upgrades.observe(&event(FIXTURE)).await.unwrap(),
        Some(id(NUMBER))
    );
    assert_eq!(upgrades.observe(&event(FIXTURE)).await.unwrap(), None);
    assert_eq!(bucket.rate_of(&id(NUMBER)), Rate::HIGHER_THROUGHPUT);

    let faster = Rate::per_second(2000).unwrap();
    let bucket = Arc::new(TokenBucket::default().rate_for(id(NUMBER), faster));
    let upgrades = ThroughputUpgrades::new(bucket.clone(), ours());
    assert_eq!(upgrades.observe(&event(FIXTURE)).await.unwrap(), None);
    assert_eq!(bucket.rate_of(&id(NUMBER)), faster);
}

/// The rate an upgrade sets is a setting: 800 a second leaves headroom
/// for inbound messages (1.25 ms between slots).
#[tokio::test]
async fn the_upgrade_rate_is_a_setting() {
    let clock = ManualClock::new(T0);
    let bucket = Arc::new(TokenBucket::default());
    let pacer = Pacer::shared(bucket.clone()).with_timer(clock.clone());
    let upgrades =
        ThroughputUpgrades::new(bucket.clone(), ours()).rate(Rate::per_second(800).unwrap());
    // Without a bot: delivered as an `EventSink`.
    upgrades.deliver(event(FIXTURE)).await.unwrap();
    assert_eq!(bucket.rate_of(&id(NUMBER)).get(), 800);
    let starts = starts(&pacer, &clock, NUMBER, 3).await;
    assert_eq!(gaps(&starts), [1_250; 2]);
}

/// A directory that cannot answer.
#[derive(Debug)]
struct Unreachable;

#[async_trait]
impl NumberDirectory for Unreachable {
    async fn phone_number_id(&self, _: &WabaId, _: &str) -> Result<Option<PhoneNumberId>> {
        Err(StorageError::Backend(anyhow::anyhow!("directory down")).into())
    }
}

/// A directory that fails changes no rate: the error is `observe`'s and,
/// as a sink, a delivery failure (Meta redelivers).
#[tokio::test]
async fn a_failed_directory_changes_no_rate() {
    let bucket = Arc::new(TokenBucket::default());
    let upgrades = ThroughputUpgrades::new(bucket.clone(), Unreachable);
    assert!(matches!(
        upgrades.observe(&event(FIXTURE)).await,
        Err(Error::Storage(_))
    ));
    assert!(matches!(
        upgrades.deliver(event(FIXTURE)).await,
        Err(SinkError::Delivery(_))
    ));
    assert_eq!(bucket.rate_of(&id(NUMBER)), Rate::DEFAULT);
}

/// Display numbers compare by their digits; one without a digit is
/// refused when registered. A number added to a shared directory while
/// running is followed.
#[tokio::test]
async fn known_numbers_compare_display_numbers_by_their_digits() {
    let numbers = Arc::new(KnownNumbers::new());
    for blank in ["", " ", "+-()"] {
        assert!(matches!(
            numbers.insert(waba(WABA), blank, id(NUMBER)),
            Err(Error::Config(_))
        ));
    }
    assert_eq!(
        numbers.phone_number_id(&waba(WABA), DISPLAY).await.unwrap(),
        None
    );
    numbers
        .insert(waba(WABA), "1 (555) 078-3881", id(NUMBER))
        .unwrap();
    for shape in [DISPLAY, "+1 555-078-3881", "1 555 078 3881"] {
        assert_eq!(
            numbers.phone_number_id(&waba(WABA), shape).await.unwrap(),
            Some(id(NUMBER)),
            "{shape}"
        );
    }
    // Another number, and the same one without its country code (digits
    // are compared whole, never as a suffix).
    for other in ["15550783882", "555-078-3881", "5550783881"] {
        assert_eq!(
            numbers.phone_number_id(&waba(WABA), other).await.unwrap(),
            None,
            "{other}"
        );
    }

    let bucket = Arc::new(TokenBucket::default());
    let upgrades = ThroughputUpgrades::shared(bucket.clone(), numbers);
    assert_eq!(
        upgrades.observe(&event(FIXTURE)).await.unwrap(),
        Some(id(NUMBER))
    );
}

/// Stops every event that is not a received message, as a middleware
/// ignoring "everything but messages" would.
#[derive(Debug)]
struct MessagesOnly;

#[async_trait]
impl Middleware for MessagesOnly {
    async fn handle(&self, ctx: Ctx, next: Next<'_>) -> Result<()> {
        if ctx.message().is_some() {
            next.run(ctx).await
        } else {
            Ok(())
        }
    }
}

/// The listener runs after the middleware (decided, AGENTS.md
/// § Decisions): one that stops the event stops the upgrade, and the
/// documented way around it, the upgrades delivered to as a sink next to
/// the bot, raises the rate.
#[tokio::test]
async fn a_middleware_that_stops_the_event_stops_the_upgrade() {
    let clock = ManualClock::new(T0);
    let bucket = Arc::new(TokenBucket::default());
    let upgrades = ThroughputUpgrades::new(bucket.clone(), ours());
    let pacer = Pacer::shared(bucket.clone()).with_timer(clock.clone());
    let bot = Bot::builder()
        .outbound(Recording::default())
        .pacer(pacer)
        .middleware(MessagesOnly)
        .follow_throughput_upgrades(upgrades.clone())
        .build()
        .await
        .unwrap();

    bot.deliver(event(FIXTURE)).await.unwrap();
    assert_eq!(bucket.rate_of(&id(NUMBER)), Rate::DEFAULT);

    upgrades.deliver(event(FIXTURE)).await.unwrap();
    assert_eq!(bucket.rate_of(&id(NUMBER)), Rate::HIGHER_THROUGHPUT);
}

/// Through the bot, a failed directory is the `ErrorHandler`'s: the
/// default acknowledges it (the upgrade is lost, as documented),
/// `PropagateErrors` fails the delivery so Meta redelivers.
#[tokio::test]
async fn through_the_bot_a_failed_directory_is_the_error_handlers() {
    let bucket = Arc::new(TokenBucket::default());
    let acknowledged = Bot::builder()
        .outbound(Recording::default())
        .follow_throughput_upgrades(ThroughputUpgrades::new(bucket.clone(), Unreachable))
        .build()
        .await
        .unwrap();
    acknowledged.deliver(event(FIXTURE)).await.unwrap();

    let propagated = Bot::builder()
        .outbound(Recording::default())
        .errors(PropagateErrors)
        .follow_throughput_upgrades(ThroughputUpgrades::new(bucket.clone(), Unreachable))
        .build()
        .await
        .unwrap();
    assert!(matches!(
        propagated.deliver(event(FIXTURE)).await,
        Err(SinkError::Delivery(_))
    ));
    assert_eq!(bucket.rate_of(&id(NUMBER)), Rate::DEFAULT);
}

/// Two threads book the number's slots while the upgrade lands: each
/// booking sees the old rate or the new one, never a mix, and once one
/// is at the new rate every later one is.
#[test]
fn bookings_racing_the_upgrade_see_one_rate_then_the_other() {
    const AFTER: usize = 50;
    let bucket = Arc::new(TokenBucket::default());
    let upgrades = ThroughputUpgrades::new(bucket.clone(), ours());
    let fixture = event(FIXTURE);
    let waits = Mutex::new(Vec::new());
    let upgraded = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                let mut after = 0;
                while after < AFTER {
                    let seen = upgraded.load(Ordering::SeqCst);
                    let slot = block_on(bucket.reserve(&SlotRequest::new(id(NUMBER), T0))).unwrap();
                    waits.lock().unwrap().push(slot.wait);
                    if seen {
                        after += 1;
                    }
                }
            });
        }
        scope.spawn(|| {
            while waits.lock().unwrap().len() < 20 {
                std::thread::yield_now();
            }
            assert_eq!(block_on(upgrades.observe(&fixture)).unwrap(), Some(id(NUMBER)));
            upgraded.store(true, Ordering::SeqCst);
        });
    });
    let mut waits = waits.into_inner().unwrap();
    waits.sort();
    let gaps: Vec<Duration> = waits.windows(2).map(|w| w[1] - w[0]).collect();
    let old = Duration::from_micros(12_500);
    let new = Duration::from_millis(1);
    assert!(gaps.iter().all(|g| *g == old || *g == new), "{gaps:?}");
    let first_new = gaps.iter().position(|g| *g == new).unwrap();
    assert!(first_new >= 19, "{gaps:?}");
    assert!(gaps[first_new..].iter().all(|g| *g == new), "{gaps:?}");
    assert!(gaps.len() - first_new >= 2 * AFTER - 1, "{gaps:?}");
}

/// Every log line as `LEVEL field=value …`.
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

/// Neither the logs (an upgrade followed, one already followed, a number
/// not known, another value) nor the `Debug` of a directory or of the
/// upgrades name the WABA or the display number, in any shape; the logs
/// name no phone number id.
#[test]
fn the_logs_and_debug_name_no_number() {
    const PRIVATE: [&str; 4] = [WABA, DISPLAY, "555-078-3881", "5550783881"];
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let upgrades = ThroughputUpgrades::new(Arc::new(TokenBucket::default()), ours());
    let unknown = ThroughputUpgrades::new(Arc::new(TokenBucket::default()), KnownNumbers::new());
    block_on(async {
        assert!(upgrades.observe(&event(FIXTURE)).await.unwrap().is_some());
        assert!(upgrades.observe(&event(FIXTURE)).await.unwrap().is_none());
        assert!(unknown.observe(&event(FIXTURE)).await.unwrap().is_none());
        assert!(
            upgrades
                .observe(&quality_update("ONBOARDING"))
                .await
                .unwrap()
                .is_none()
        );
    });
    let lines = capture.0.lock().unwrap().clone();
    for level in ["INFO", "WARN", "DEBUG"] {
        assert!(lines.iter().any(|l| l.starts_with(level)), "{lines:?}");
    }
    // The logs carry no phone number id either (the bucket's `Debug`,
    // inside the upgrades', lists its numbers' rates by id).
    for line in &lines {
        assert!(!line.contains(NUMBER), "{line}");
    }
    let debug = format!("{:?} {:?}", ours(), upgrades);
    for text in lines.iter().chain([&debug]) {
        for private in PRIVATE {
            assert!(!text.contains(private), "{private} in {text}");
        }
    }
}
