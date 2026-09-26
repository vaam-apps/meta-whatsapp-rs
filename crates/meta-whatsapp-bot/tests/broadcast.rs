//! The paced broadcast (roadmap B2): the rate never exceeded under a fake
//! clock, a send that may have gone out never resent, the pair rate limit
//! deferring one recipient only, the per-user marketing limit reported,
//! throughput slowing the pacer, a stop and a cancel ending the run, and a
//! bot's read receipts and replies paced through the same budget.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::pin::pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use common::{NUMBER, Recording, client, send_response, text_event};
use futures::future::{Either, select};
use meta_whatsapp_bot::{
    Backoff, Bot, Broadcast, BroadcastHandle, BroadcastPolicy, Command, Ctx, Ended, MarkRead,
    Outbound, Outcome, PacedOutbound, Pacer, Rate, RateLimiter, Timer, TokenBucket, Verdict,
};
use meta_whatsapp_client::messages::{OutboundMessage, SendResponse, Text};
use meta_whatsapp_client::templates::TemplateMessage;
use meta_whatsapp_core::clock::{Clock, ManualClock};
use meta_whatsapp_core::error::{StorageError, TransportError};
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::testing::ScriptedTransport;
use meta_whatsapp_core::{Error, ErrorKind, GraphApiError};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;

const T0: OffsetDateTime = datetime!(2026-09-26 12:00 UTC);

fn at(clock: &ManualClock) -> Duration {
    Duration::try_from(clock.now() - T0).unwrap()
}

fn ms(d: Duration) -> u128 {
    d.as_millis()
}

fn pacer(clock: &ManualClock, per_second: u32) -> Pacer {
    Pacer::new(TokenBucket::new(Rate::per_second(per_second).unwrap())).with_timer(clock.clone())
}

fn phone(i: usize) -> Recipient {
    Recipient::phone(format!("+1650555{i:04}"))
}

/// Who a recorded message went to.
fn to(message: &Value) -> String {
    message["to"]
        .as_str()
        .or_else(|| message["recipient"].as_str())
        .unwrap_or_default()
        .to_owned()
}

/// The most sends that started in one half-open second `[t, t + 1 s)`.
fn most_in_one_second(starts: &[Duration]) -> usize {
    starts
        .iter()
        .map(|t| {
            starts
                .iter()
                .filter(|s| **s >= *t && **s < *t + Duration::from_secs(1))
                .count()
        })
        .max()
        .unwrap_or(0)
}

/// Meta's documented error body (`support/error-codes`, "Example
/// response") with another code and its `details` from the same page.
fn meta_error(code: i64, details: &str) -> Value {
    json!({"error": {
        "message": format!("(#{code})"),
        "type": "OAuthException",
        "code": code,
        "error_data": {"messaging_product": "whatsapp", "details": details},
        "fbtrace_id": "Az8or2yhqkZfEZ-_4Qn_Bam"
    }})
}

/// A Graph error as the client reports it on a 400.
fn graph(code: i64) -> Error {
    let mut error = GraphApiError::new(code, format!("(#{code})"));
    error.http_status = Some(400);
    error.into()
}

/// What to answer a send to one recipient, in order.
type Script = Mutex<Vec<(String, Box<dyn FnOnce() -> Error + Send>)>>;

/// An `Outbound` that records when each send started (on the fake clock)
/// and to whom, fails the sends it is told to, and yields once per send so
/// the broadcast's concurrent senders interleave.
#[derive(Clone)]
struct Timed {
    clock: ManualClock,
    sends: Arc<Mutex<Vec<(Duration, String)>>>,
    reads: Arc<Mutex<Vec<(Duration, bool)>>>,
    failures: Arc<Script>,
    /// Cancelled after this many sends, when set.
    cancel_after: Arc<OnceLock<(usize, BroadcastHandle)>>,
}

impl std::fmt::Debug for Timed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Timed")
    }
}

impl Timed {
    fn new(clock: &ManualClock) -> Self {
        Self {
            clock: clock.clone(),
            sends: Arc::default(),
            reads: Arc::default(),
            failures: Arc::default(),
            cancel_after: Arc::default(),
        }
    }

    /// The next send to `recipient` fails with `error`.
    fn fail(&self, recipient: &Recipient, error: impl FnOnce() -> Error + Send + 'static) {
        let to = to(&serde_json::to_value(OutboundMessage::text(recipient.clone(), "x")).unwrap());
        self.failures.lock().unwrap().push((to, Box::new(error)));
    }

    fn sends(&self) -> Vec<(Duration, String)> {
        self.sends.lock().unwrap().clone()
    }
}

#[async_trait]
impl Outbound for Timed {
    async fn send(
        &self,
        _: &PhoneNumberId,
        message: &OutboundMessage,
    ) -> meta_whatsapp_core::Result<SendResponse> {
        message.validate()?;
        let who = to(&serde_json::to_value(message).unwrap());
        let count = {
            let mut sends = self.sends.lock().unwrap();
            sends.push((at(&self.clock), who.clone()));
            sends.len()
        };
        if let Some((after, handle)) = self.cancel_after.get()
            && count == *after
        {
            handle.cancel();
        }
        tokio::task::yield_now().await;
        let failure = {
            let mut failures = self.failures.lock().unwrap();
            let position = failures.iter().position(|(to, _)| *to == who);
            position.map(|p| failures.remove(p).1)
        };
        if let Some(error) = failure {
            return Err(error());
        }
        Ok(serde_json::from_value(send_response()).unwrap())
    }

    async fn mark_read(
        &self,
        _: &PhoneNumberId,
        _: &MessageId,
        typing_indicator: bool,
    ) -> meta_whatsapp_core::Result<()> {
        self.reads
            .lock()
            .unwrap()
            .push((at(&self.clock), typing_indicator));
        Ok(())
    }
}

/// The decisive test of B2: 200 sends at a configured 20 a second, under a
/// fake clock, never more than 20 in any one-second window, and not
/// slower than the rate either.
#[tokio::test]
async fn two_hundred_sends_at_twenty_a_second_never_exceed_twenty_in_any_second() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    let broadcast = Broadcast::builder(NUMBER)
        .to((0..200).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .build()
        .unwrap();
    let handle = broadcast.handle();
    assert_eq!(handle.progress().remaining(), 200);

    let report = broadcast.run().await;

    let starts: Vec<Duration> = outbound.sends().iter().map(|(t, _)| *t).collect();
    assert_eq!(starts.len(), 200);
    assert_eq!(most_in_one_second(&starts), 20);
    // Evenly spaced at 50 ms: the whole run takes 9.95 s.
    assert_eq!(ms(starts[0]), 0);
    assert_eq!(ms(starts[199]), 9950);
    assert!(
        starts
            .windows(2)
            .all(|w| w[1].checked_sub(w[0]) == Some(Duration::from_millis(50)))
    );
    // Each recipient once, in the order given.
    let order: Vec<String> = outbound.sends().into_iter().map(|(_, to)| to).collect();
    let expected: Vec<String> = (0..200).map(|i| format!("+1650555{i:04}")).collect();
    assert_eq!(order, expected);

    assert_eq!(report.ended, Ended::Completed);
    let progress = report.progress();
    assert_eq!(
        (progress.sent, progress.failed, progress.skipped),
        (200, 0, 0)
    );
    assert_eq!(handle.progress(), progress);
    assert!(report.recipients.iter().all(|r| r.attempts == 1));
    assert!(
        report
            .recipients
            .iter()
            .all(|r| matches!(r.outcome, Outcome::Sent(_)))
    );
    // A cancel after the end changes nothing.
    handle.cancel();
    assert!(!handle.is_cancelled());
}

/// A policy that asks to retry everything: the library's rule must hold
/// against it.
#[derive(Debug)]
struct RetryEverything;

impl BroadcastPolicy for RetryEverything {
    fn on_failure(&self, _: &Error, _: u32) -> Verdict {
        Verdict::RetryAfter(Duration::from_secs(1))
    }

    fn slows_down(&self, _: &Error) -> bool {
        false
    }
}

/// A timed-out send may have gone out: it is never resent, even when the
/// policy asks for it. Removing the `may_have_been_sent` check fails this.
#[tokio::test]
async fn a_timed_out_send_is_never_resent_whatever_the_policy() {
    let t = ScriptedTransport::new();
    t.push_json(200, send_response());
    t.push_error(|| TransportError::Timeout);
    t.push_json(200, send_response());
    let clock = ManualClock::new(T0);
    let recipients = [phone(1), phone(2), phone(3)];
    let report = Broadcast::builder(NUMBER)
        .to(recipients.clone())
        .content(Text::new("Your order has shipped"))
        .client(client(&t))
        .pacer(pacer(&clock, 20))
        .policy(RetryEverything)
        .build()
        .unwrap()
        .run()
        .await;

    let sent_to: Vec<String> = t
        .requests()
        .iter()
        .map(|r| to(&r.json().unwrap()))
        .collect();
    assert_eq!(sent_to, ["+16505550001", "+16505550002", "+16505550003"]);
    assert_eq!(t.remaining(), 0);
    let timed_out = &report.recipients[1];
    assert_eq!(timed_out.attempts, 1);
    let Outcome::Failed(error) = &timed_out.outcome else {
        panic!("{:?}", timed_out.outcome)
    };
    assert!(error.may_have_been_sent());
    assert!(matches!(error, Error::Transport(TransportError::Timeout)));
    assert!(matches!(report.recipients[0].outcome, Outcome::Sent(_)));
    assert!(matches!(report.recipients[2].outcome, Outcome::Sent(_)));
    assert_eq!(report.ended, Ended::Completed);
}

/// Nor is a refusal that cannot succeed later (`131049`: Meta says a
/// resend within 24 hours only fails again), whatever the policy.
#[tokio::test]
async fn a_refusal_that_is_not_retryable_is_not_resent_whatever_the_policy() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    outbound.fail(&phone(1), || graph(131049));
    let report = Broadcast::builder(NUMBER)
        .to([phone(1)])
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .policy(RetryEverything)
        .build()
        .unwrap()
        .run()
        .await;
    assert_eq!(outbound.sends().len(), 1);
    assert_eq!(report.recipients[0].attempts, 1);
    assert!(matches!(report.recipients[0].outcome, Outcome::Failed(_)));
}

/// The same policy does resend what provably did not go out: the rule is
/// "never when it may have been sent", not "never".
#[tokio::test]
async fn a_throttled_send_is_resent_under_the_same_policy() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        meta_error(130429, "Cloud API message throughput has been reached."),
    );
    t.push_json(200, send_response());
    let clock = ManualClock::new(T0);
    let report = Broadcast::builder(NUMBER)
        .to([phone(1)])
        .content(Text::new("Hello"))
        .client(client(&t))
        .pacer(pacer(&clock, 20))
        .policy(RetryEverything)
        .build()
        .unwrap()
        .run()
        .await;
    assert_eq!(t.requests().len(), 2);
    assert_eq!(t.remaining(), 0);
    assert_eq!(report.recipients[0].attempts, 2);
    assert!(matches!(report.recipients[0].outcome, Outcome::Sent(_)));
}

/// Through `BroadcastBuilder::client`, a client with its default
/// `RetryPolicy` (which replays a throttled send at once, outside the
/// pacer) does not replay: the broadcast retries, 1 s later on the pacer's
/// clock, and the attempts say so.
#[tokio::test]
async fn a_broadcast_retries_through_its_pacer_not_the_clients_replays() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        meta_error(130429, "Cloud API message throughput has been reached."),
    );
    t.push_json(200, send_response());
    let default_retries = meta_whatsapp_client::Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .build()
        .unwrap();
    assert_ne!(
        default_retries.retry_policy(),
        meta_whatsapp_client::RetryPolicy::NONE
    );
    let clock = ManualClock::new(T0);
    let report = Broadcast::builder(NUMBER)
        .to([phone(1)])
        .content(Text::new("Hello"))
        .client(default_retries)
        .pacer(pacer(&clock, 20))
        .build()
        .unwrap()
        .run()
        .await;
    assert_eq!(t.requests().len(), 2);
    assert_eq!(t.remaining(), 0);
    assert_eq!(report.recipients[0].attempts, 2);
    assert!(matches!(report.recipients[0].outcome, Outcome::Sent(_)));
    assert_eq!(ms(at(&clock)), 1000, "the retry waited for the backoff");
}

/// `131056`: that recipient waits (Meta's 4^0 = 1 s first) while the others
/// go on, then is sent; exact requests through the client.
#[tokio::test]
async fn a_pair_rate_limit_defers_only_that_recipient() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        meta_error(
            131056,
            "Too many messages sent from the sender phone number to the same recipient phone \
             number in a short period of time.",
        ),
    );
    t.push_json(200, send_response());
    t.push_json(200, send_response());
    t.push_json(200, send_response());
    let clock = ManualClock::new(T0);
    let outbound = TimedClient {
        clock: clock.clone(),
        inner: meta_whatsapp_bot::ClientOutbound::new(client(&t)),
        starts: Arc::default(),
    };
    let report = Broadcast::builder(NUMBER)
        .to([phone(1), phone(2), phone(3)])
        .content(Text::new("Your order has shipped"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .build()
        .unwrap()
        .run()
        .await;

    let requests = t.requests();
    assert_eq!(t.remaining(), 0);
    for request in &requests {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path(), format!("/v25.0/{NUMBER}/messages"));
        assert_eq!(request.bearer(), Some("TOKEN"));
    }
    assert_eq!(
        requests[0].json().unwrap(),
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505550001",
            "type": "text",
            "text": {"body": "Your order has shipped"}
        })
    );
    let sent_to: Vec<String> = requests.iter().map(|r| to(&r.json().unwrap())).collect();
    assert_eq!(
        sent_to,
        [
            "+16505550001",
            "+16505550002",
            "+16505550003",
            "+16505550001"
        ]
    );
    // The others at the pacer's 50 ms; the limited one 1 s after its
    // failure.
    let starts: Vec<u128> = outbound
        .starts
        .lock()
        .unwrap()
        .iter()
        .map(|d| ms(*d))
        .collect();
    assert_eq!(starts, [0, 50, 100, 1000]);

    assert_eq!(report.ended, Ended::Completed);
    assert_eq!(report.recipients[0].attempts, 2);
    assert!(
        report
            .recipients
            .iter()
            .all(|r| matches!(r.outcome, Outcome::Sent(_)))
    );
    assert_eq!(report.recipients[1].attempts, 1);
}

/// [`Outbound`] over the client, recording when each send started.
#[derive(Debug, Clone)]
struct TimedClient {
    clock: ManualClock,
    inner: meta_whatsapp_bot::ClientOutbound,
    starts: Arc<Mutex<Vec<Duration>>>,
}

#[async_trait]
impl Outbound for TimedClient {
    async fn send(
        &self,
        from: &PhoneNumberId,
        message: &OutboundMessage,
    ) -> meta_whatsapp_core::Result<SendResponse> {
        self.starts.lock().unwrap().push(at(&self.clock));
        self.inner.send(from, message).await
    }

    async fn mark_read(
        &self,
        from: &PhoneNumberId,
        message_id: &MessageId,
        typing_indicator: bool,
    ) -> meta_whatsapp_core::Result<()> {
        self.inner
            .mark_read(from, message_id, typing_indicator)
            .await
    }
}

/// A pair limit that keeps refusing is given up after `max_attempts`, on
/// Meta's 4^X schedule.
#[tokio::test]
async fn a_pair_limit_that_persists_gives_up_after_the_attempts() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    for _ in 0..3 {
        outbound.fail(&phone(1), || graph(131056));
    }
    let report = Broadcast::builder(NUMBER)
        .to([phone(1)])
        .content(Text::new("Hi"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .policy(Backoff::new().max_attempts(3))
        .build()
        .unwrap()
        .run()
        .await;
    let starts: Vec<u128> = outbound.sends().iter().map(|(t, _)| ms(*t)).collect();
    assert_eq!(starts, [0, 1000, 5000]);
    assert_eq!(report.recipients[0].attempts, 3);
    let Outcome::Failed(error) = &report.recipients[0].outcome else {
        panic!("{:?}", report.recipients[0].outcome)
    };
    assert_eq!(error.kind(), ErrorKind::PairRateLimited);
}

/// `131049`: reported for that recipient, never retried; the others are
/// sent.
#[tokio::test]
async fn the_per_user_marketing_limit_is_reported_and_not_retried() {
    let t = ScriptedTransport::new();
    t.push_json(200, send_response());
    t.push_json(
        400,
        meta_error(
            131049,
            "This message was not delivered to maintain healthy ecosystem engagement.",
        ),
    );
    t.push_json(200, send_response());
    let clock = ManualClock::new(T0);
    let report = Broadcast::builder(NUMBER)
        .to([phone(1), phone(2), phone(3)])
        .content(Text::new("Spring sale"))
        .client(client(&t))
        .pacer(pacer(&clock, 20))
        .build()
        .unwrap()
        .run()
        .await;
    assert_eq!(t.requests().len(), 3);
    assert_eq!(t.remaining(), 0);
    let limited = &report.recipients[1];
    assert_eq!(limited.attempts, 1);
    let Outcome::Failed(error) = &limited.outcome else {
        panic!("{:?}", limited.outcome)
    };
    assert_eq!(error.kind(), ErrorKind::EcosystemEngagementLimit);
    assert_eq!(error.graph().map(|g| g.code), Some(131049));
    assert!(!error.may_have_been_sent());
    let progress = report.progress();
    assert_eq!(
        (progress.sent, progress.failed, progress.remaining()),
        (2, 1, 0)
    );
    assert_eq!(report.ended, Ended::Completed);
}

/// `130429`: the number's rate halves (50 ms → 100 ms apart) and the
/// recipient is retried after the backoff.
#[tokio::test]
async fn a_throughput_error_slows_the_pacer_down_and_is_retried() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    outbound.fail(&phone(2), || graph(130429));
    let report = Broadcast::builder(NUMBER)
        .to((1..=5).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .concurrency(1)
        .build()
        .unwrap()
        .run()
        .await;
    let sends: Vec<(u128, String)> = outbound
        .sends()
        .into_iter()
        .map(|(t, to)| (ms(t), to))
        .collect();
    assert_eq!(
        sends,
        [
            (0, "+16505550001".to_owned()),
            (50, "+16505550002".to_owned()), // 130429
            (100, "+16505550003".to_owned()),
            (200, "+16505550004".to_owned()), // 10 a second from here
            (300, "+16505550005".to_owned()),
            (1050, "+16505550002".to_owned()), // 1 s after its failure
        ]
    );
    assert_eq!(report.recipients[1].attempts, 2);
    assert_eq!(report.progress().sent, 5);
}

/// `131048` (spam): reported, not retried, and the pacer slows down.
#[tokio::test]
async fn a_spam_limit_slows_the_pacer_and_is_not_retried() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    outbound.fail(&phone(1), || graph(131048));
    let report = Broadcast::builder(NUMBER)
        .to((1..=3).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .concurrency(1)
        .build()
        .unwrap()
        .run()
        .await;
    let starts: Vec<u128> = outbound.sends().iter().map(|(t, _)| ms(*t)).collect();
    assert_eq!(starts, [0, 50, 150]);
    assert_eq!(report.recipients[0].attempts, 1);
    assert!(matches!(report.recipients[0].outcome, Outcome::Failed(_)));
}

/// An expired token holds for every recipient: the run stops, the rest is
/// skipped.
#[tokio::test]
async fn an_account_wide_error_stops_the_run() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    outbound.fail(&phone(2), || graph(190));
    let broadcast = Broadcast::builder(NUMBER)
        .to((1..=5).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .concurrency(1)
        .build()
        .unwrap();
    let handle = broadcast.handle();
    let report = broadcast.run().await;
    assert_eq!(outbound.sends().len(), 2);
    assert_eq!(report.ended, Ended::Stopped { recipient: 1 });
    assert!(matches!(report.recipients[0].outcome, Outcome::Sent(_)));
    assert!(matches!(report.recipients[1].outcome, Outcome::Failed(_)));
    for line in &report.recipients[2..] {
        assert!(matches!(line.outcome, Outcome::Skipped));
        assert_eq!(line.attempts, 0);
    }
    let progress = handle.progress();
    assert_eq!(
        (
            progress.sent,
            progress.failed,
            progress.skipped,
            progress.remaining()
        ),
        (1, 1, 3, 0)
    );
    assert!(!handle.is_cancelled());
}

/// Cancelling stops further sends: none starts after the cancel.
#[tokio::test]
async fn cancelling_stops_further_sends() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    let broadcast = Broadcast::builder(NUMBER)
        .to((0..10).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .build()
        .unwrap();
    let handle = broadcast.handle();
    outbound.cancel_after.set((3, handle.clone())).unwrap();

    let report = broadcast.run().await;

    assert_eq!(outbound.sends().len(), 3);
    assert_eq!(report.ended, Ended::Cancelled);
    assert!(handle.is_cancelled());
    let progress = report.progress();
    assert_eq!((progress.sent, progress.skipped), (3, 7));
    assert_eq!(handle.progress(), progress);
    assert!(
        report.recipients[3..]
            .iter()
            .all(|r| matches!(r.outcome, Outcome::Skipped) && r.attempts == 0)
    );
}

/// A clock whose sleeps never end: only a cancel gets a waiting sender
/// out.
#[derive(Debug)]
struct Frozen(ManualClock);

impl Clock for Frozen {
    fn now(&self) -> OffsetDateTime {
        self.0.now()
    }
}

#[async_trait]
impl Timer for Frozen {
    async fn sleep(&self, _: Duration) {
        futures::future::pending::<()>().await;
    }
}

/// Cancel ends the wait for a slot at once, rather than after it.
#[tokio::test]
async fn cancelling_ends_a_wait_for_the_next_slot() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    let broadcast = Broadcast::builder(NUMBER)
        .to((0..3).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(
            Pacer::new(TokenBucket::new(Rate::per_second(1).unwrap()))
                .with_timer(Frozen(clock.clone())),
        )
        .build()
        .unwrap();
    let handle = broadcast.handle();
    let canceller = async move {
        tokio::task::yield_now().await;
        handle.cancel();
        // Give the run many chances to end before calling it stuck.
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
    };
    let report = match select(pin!(broadcast.run()), pin!(canceller)).await {
        Either::Left((report, _)) => report,
        Either::Right(((), _)) => panic!("the cancelled run kept waiting for its slot"),
    };
    assert_eq!(outbound.sends().len(), 1);
    assert_eq!(report.ended, Ended::Cancelled);
    assert_eq!(report.progress().skipped, 2);
}

/// A clock on which the broadcast is cancelled while a sender waits, and
/// whose wait then ends as usual: the cancel and the slot arrive together.
#[derive(Debug)]
struct CancelWhileWaiting {
    clock: ManualClock,
    handle: Arc<OnceLock<BroadcastHandle>>,
}

impl Clock for CancelWhileWaiting {
    fn now(&self) -> OffsetDateTime {
        self.clock.now()
    }
}

#[async_trait]
impl Timer for CancelWhileWaiting {
    async fn sleep(&self, duration: Duration) {
        if let Some(handle) = self.handle.get() {
            handle.cancel();
        }
        self.clock.advance(duration);
    }
}

/// A slot that comes after the cancel is not used: nothing starts once
/// the run is cancelled, even when the wait ended normally.
#[tokio::test]
async fn a_slot_reached_after_a_cancel_sends_nothing() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    let handle_slot = Arc::new(OnceLock::new());
    let broadcast = Broadcast::builder(NUMBER)
        .to((0..3).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(
            Pacer::new(TokenBucket::new(Rate::per_second(1).unwrap())).with_timer(
                CancelWhileWaiting {
                    clock: clock.clone(),
                    handle: Arc::clone(&handle_slot),
                },
            ),
        )
        .concurrency(1)
        .build()
        .unwrap();
    handle_slot.set(broadcast.handle()).unwrap();
    let report = broadcast.run().await;
    // The first send needs no wait; the second waited, and was cancelled
    // meanwhile.
    assert_eq!(outbound.sends().len(), 1);
    assert_eq!(report.ended, Ended::Cancelled);
    assert_eq!(report.progress().skipped, 2);
}

/// One message per recipient, with the recipient's own callback data (to
/// reconcile with the status webhooks), as Meta's template send shows.
#[tokio::test]
async fn compose_builds_each_recipients_message() {
    let t = ScriptedTransport::new();
    t.push_json(200, send_response());
    t.push_json(200, send_response());
    let clock = ManualClock::new(T0);
    let report = Broadcast::builder(NUMBER)
        .to([phone(1), Recipient::user("US.13491208655302741918")])
        .compose(|to: &Recipient| {
            let data = match to {
                Recipient::User(user) => format!("spring-sale:{user}"),
                _ => "spring-sale:phone".to_owned(),
            };
            Ok(
                OutboundMessage::template(to.clone(), TemplateMessage::new("spring_sale", "en_US"))
                    .callback_data(data),
            )
        })
        .client(client(&t))
        .pacer(pacer(&clock, 20))
        .build()
        .unwrap()
        .run()
        .await;
    assert_eq!(t.remaining(), 0);
    let bodies: Vec<Value> = t.requests().iter().map(|r| r.json().unwrap()).collect();
    assert_eq!(
        bodies,
        [
            json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "to": "+16505550001",
                "biz_opaque_callback_data": "spring-sale:phone",
                "type": "template",
                "template": {"name": "spring_sale", "language": {"code": "en_US"}}
            }),
            json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "recipient": "US.13491208655302741918",
                "biz_opaque_callback_data": "spring-sale:US.13491208655302741918",
                "type": "template",
                "template": {"name": "spring_sale", "language": {"code": "en_US"}}
            }),
        ]
    );
    assert_eq!(report.progress().sent, 2);
}

/// A composed message addressed to someone else fails that recipient
/// without a send; a compose error too.
#[tokio::test]
async fn a_message_composed_for_someone_else_is_not_sent() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    let report = Broadcast::builder(NUMBER)
        .to([phone(1), phone(2), phone(3)])
        .compose(|to: &Recipient| match to {
            Recipient::Phone(p) if p.ends_with('1') => Ok(OutboundMessage::text(phone(9), "x")),
            Recipient::Phone(p) if p.ends_with('2') => Err(
                meta_whatsapp_core::error::ValidationError::new("name", "unknown customer").into(),
            ),
            _ => Ok(OutboundMessage::text(to.clone(), "Hello")),
        })
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .build()
        .unwrap()
        .run()
        .await;
    let sent: Vec<String> = outbound.sends().into_iter().map(|(_, to)| to).collect();
    assert_eq!(sent, ["+16505550003"]);
    for line in &report.recipients[..2] {
        assert_eq!(line.attempts, 0);
        let Outcome::Failed(error) = &line.outcome else {
            panic!("{:?}", line.outcome)
        };
        assert_eq!(error.kind(), ErrorKind::InvalidParameter);
    }
}

#[tokio::test]
async fn build_refuses_missing_settings_and_an_empty_list_completes() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    let cases = [
        Broadcast::builder(NUMBER)
            .content(Text::new("x"))
            .pacer(pacer(&clock, 20))
            .build(),
        Broadcast::builder(NUMBER)
            .content(Text::new("x"))
            .outbound(outbound.clone())
            .build(),
        Broadcast::builder(NUMBER)
            .outbound(outbound.clone())
            .pacer(pacer(&clock, 20))
            .build(),
        Broadcast::builder(NUMBER)
            .content(Text::new("x"))
            .outbound(outbound.clone())
            .pacer(pacer(&clock, 20))
            .concurrency(0)
            .build(),
    ];
    for case in cases {
        assert!(matches!(case, Err(Error::Config(_))), "{case:?}");
    }
    let report = Broadcast::builder(NUMBER)
        .content(Text::new("x"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .build()
        .unwrap()
        .run()
        .await;
    assert!(report.recipients.is_empty());
    assert_eq!(report.ended, Ended::Completed);
    assert!(outbound.sends().is_empty());
}

/// The run can be spawned.
#[test]
fn the_run_is_send() {
    fn send<T: Send>(_: &T) {}
    let broadcast = Broadcast::builder(NUMBER)
        .content(Text::new("x"))
        .outbound(Recording::default())
        .pacer(Pacer::default())
        .build()
        .unwrap();
    let run = broadcast.run();
    send(&run);
}

/// A policy by code: the pair limit after 1 s, throughput after 10 s,
/// never a slow-down (so the times below are the pacer's 50 ms only).
#[derive(Debug)]
struct ByCode;

impl BroadcastPolicy for ByCode {
    fn on_failure(&self, error: &Error, _: u32) -> Verdict {
        match error.graph().map(|g| g.code) {
            Some(131056) => Verdict::RetryAfter(Duration::from_secs(1)),
            Some(130429) => Verdict::RetryAfter(Duration::from_secs(10)),
            _ => Verdict::Fail,
        }
    }

    fn slows_down(&self, _: &Error) -> bool {
        false
    }
}

/// Retries go in the order they fall due: one due in 1 s does not wait
/// behind one due in 10 s.
#[tokio::test]
async fn retries_go_in_the_order_they_fall_due() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    outbound.fail(&phone(1), || graph(130429)); // again at 10 s
    outbound.fail(&phone(2), || graph(131056)); // again 1 s after 50 ms
    let report = Broadcast::builder(NUMBER)
        .to([phone(1), phone(2)])
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .policy(ByCode)
        .concurrency(1)
        .build()
        .unwrap()
        .run()
        .await;
    let sends: Vec<(u128, String)> = outbound
        .sends()
        .into_iter()
        .map(|(t, to)| (ms(t), to))
        .collect();
    assert_eq!(
        sends,
        [
            (0, "+16505550001".to_owned()),
            (50, "+16505550002".to_owned()),
            (1050, "+16505550002".to_owned()),
            (10_000, "+16505550001".to_owned()),
        ]
    );
    assert_eq!(report.progress().sent, 2);
}

/// A retry that falls due is sent then, not after every recipient not
/// sent yet: here at the first slot after 1 s, in a run of 5 s.
#[tokio::test]
async fn a_due_retry_goes_before_the_recipients_not_sent_yet() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    outbound.fail(&phone(0), || graph(131056));
    let report = Broadcast::builder(NUMBER)
        .to((0..100).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .policy(ByCode)
        .concurrency(1)
        .build()
        .unwrap()
        .run()
        .await;
    let first: Vec<u128> = outbound
        .sends()
        .into_iter()
        .filter(|(_, to)| to == "+16505550000")
        .map(|(t, _)| ms(t))
        .collect();
    assert_eq!(first, [0, 1050]);
    assert_eq!(report.progress().sent, 100);
    assert_eq!(report.recipients[0].attempts, 2);
}

/// A rate limiter that cannot be reached (a shared one, down).
#[derive(Debug)]
struct Unreachable;

#[async_trait]
impl RateLimiter for Unreachable {
    async fn reserve(
        &self,
        _: &PhoneNumberId,
        _: OffsetDateTime,
    ) -> meta_whatsapp_core::Result<Duration> {
        Err(StorageError::Backend(anyhow::anyhow!("limiter unreachable")).into())
    }

    async fn slow_down(
        &self,
        _: &PhoneNumberId,
        _: OffsetDateTime,
    ) -> meta_whatsapp_core::Result<()> {
        Err(StorageError::Backend(anyhow::anyhow!("limiter unreachable")).into())
    }
}

/// A limiter that fails lets nothing through: the run stops at the first
/// recipient, before any send, and the rest are skipped.
#[tokio::test]
async fn a_failing_rate_limiter_stops_the_run_before_any_send() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    let report = Broadcast::builder(NUMBER)
        .to((0..3).map(phone))
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(Pacer::new(Unreachable).with_timer(clock.clone()))
        .concurrency(1)
        .build()
        .unwrap()
        .run()
        .await;
    assert!(outbound.sends().is_empty());
    assert_eq!(report.ended, Ended::Stopped { recipient: 0 });
    let Outcome::Failed(error) = &report.recipients[0].outcome else {
        panic!("{:?}", report.recipients[0].outcome)
    };
    assert!(matches!(error, Error::Storage(_)));
    assert_eq!(report.recipients[0].attempts, 0);
    assert!(
        report.recipients[1..]
            .iter()
            .all(|r| matches!(r.outcome, Outcome::Skipped))
    );
}

/// `PacedOutbound`: a limiter that fails fails the call, and nothing is
/// sent or marked read.
#[tokio::test]
async fn a_paced_outbound_sends_nothing_when_its_limiter_fails() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    let paced = PacedOutbound::new(
        outbound.clone(),
        Pacer::new(Unreachable).with_timer(clock.clone()),
    );
    let from = PhoneNumberId::new(NUMBER);
    let sent = paced
        .send(&from, &OutboundMessage::text(phone(1), "Hello"))
        .await;
    assert!(matches!(sent, Err(Error::Storage(_))), "{sent:?}");
    let read = paced
        .mark_read(&from, &MessageId::new("wamid.X"), true)
        .await;
    assert!(matches!(read, Err(Error::Storage(_))), "{read:?}");
    assert!(outbound.sends().is_empty());
    assert!(outbound.reads.lock().unwrap().is_empty());
}

const USER: &str = "US.13491208655302741918";
const GROUP: &str = "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD";

/// The list of `each_person_is_sent_once_by_default`: eight listings,
/// four people.
fn listed_twice() -> Vec<Recipient> {
    vec![
        Recipient::phone("+16505550001"),     // 0
        Recipient::phone("1 (650) 555-0001"), // 1: 0 again, by its digits
        Recipient::user(USER),                // 2
        Recipient::PhoneAndUser {
            phone: "+16505550002".into(),
            user: USER.into(),
        }, // 3: 2 again, by the user id
        Recipient::phone("16505550002"),      // 4: 2 again, the number 3 gave them
        Recipient::group(GROUP),              // 5
        Recipient::group(GROUP),              // 6: 5 again
        Recipient::phone("+16505550003"),     // 7
    ]
}

/// A person listed twice gets one message: a duplicate marketing message
/// is billed and harms the merchant. The repeats' lines name the first.
#[tokio::test]
async fn each_person_is_sent_once_by_default() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    let broadcast = Broadcast::builder(NUMBER)
        .to(listed_twice())
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .build()
        .unwrap();
    let handle = broadcast.handle();
    assert_eq!(handle.progress().duplicates, 4, "known from the start");
    assert_eq!(handle.progress().remaining(), 4);

    let report = broadcast.run().await;

    let sent: Vec<String> = outbound.sends().into_iter().map(|(_, to)| to).collect();
    assert_eq!(sent, ["+16505550001", USER, GROUP, "+16505550003"]);
    let firsts: Vec<(usize, Option<usize>)> = report
        .recipients
        .iter()
        .map(|line| {
            let of = match line.outcome {
                Outcome::Duplicate { of } => Some(of),
                Outcome::Sent(_) => None,
                ref other => panic!("{other:?}"),
            };
            (line.index, of)
        })
        .collect();
    assert_eq!(
        firsts,
        [
            (0, None),
            (1, Some(0)),
            (2, None),
            (3, Some(2)),
            (4, Some(2)),
            (5, None),
            (6, Some(5)),
            (7, None)
        ]
    );
    assert!(
        report
            .recipients
            .iter()
            .filter(|l| matches!(l.outcome, Outcome::Duplicate { .. }))
            .all(|l| l.attempts == 0)
    );
    let progress = report.progress();
    assert_eq!(
        (progress.sent, progress.duplicates, progress.remaining()),
        (4, 4, 0)
    );
    assert_eq!(handle.progress(), progress);
}

/// `dedupe(false)`: the list as given, one message per listing.
#[tokio::test]
async fn dedupe_off_sends_every_listing() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    let report = Broadcast::builder(NUMBER)
        .to(listed_twice())
        .content(Text::new("Spring sale"))
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 20))
        .dedupe(false)
        .build()
        .unwrap()
        .run()
        .await;
    assert_eq!(outbound.sends().len(), 8);
    assert_eq!(report.progress().sent, 8);
    assert_eq!(report.progress().duplicates, 0);
}

/// `BotBuilder::pacer`: the typing indicator (`MarkRead`) and the reply
/// both wait for a slot of the number, in one budget.
#[tokio::test]
async fn a_bots_read_receipts_and_replies_go_through_the_pacer() {
    let clock = ManualClock::new(T0);
    let outbound = Timed::new(&clock);
    let bot = Bot::builder()
        .outbound(outbound.clone())
        .pacer(pacer(&clock, 1))
        .middleware(MarkRead::with_typing_indicator())
        .command(Command::new("ping", |ctx: Ctx| async move {
            ctx.reply("pong").await?;
            Ok(())
        }))
        .build()
        .await
        .unwrap();

    bot.deliver(text_event("messages/text.json", "/ping"))
        .await
        .unwrap();

    let reads: Vec<(u128, bool)> = outbound
        .reads
        .lock()
        .unwrap()
        .iter()
        .map(|(t, typing)| (ms(*t), *typing))
        .collect();
    assert_eq!(reads, [(0, true)]);
    let replies: Vec<u128> = outbound.sends().iter().map(|(t, _)| ms(*t)).collect();
    assert_eq!(replies, [1000], "the reply waited one slot at 1 a second");
}
