//! Durable scheduling (roadmap B3): two runners on one store send a due job
//! once, a job scheduled before a restart is sent after it, a resumed
//! broadcast never resends a recipient recorded as sent, a `131049` status
//! webhook makes a re-run within 24 hours skip that recipient, a crashed
//! runner's lease holds the job until it expires, a timed-out send is never
//! resent, retries back off and count, and a cancel stops the next send.
//! Time is a `ManualClock` shared by the store, the jobs and the pacer;
//! concurrent runners are interleaved by a gate in the store, not by
//! thread timing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{HashMap, VecDeque};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{NUMBER, client, events_of, fixture_json, send_response};
use futures::future::{BoxFuture, Either, select};
use meta_whatsapp_adapters::store::MemoryKvStore;
use meta_whatsapp_bot::jobs::{JOBS_NAMESPACE, JOBS_SENT_NAMESPACE};
use meta_whatsapp_bot::{
    Backoff, Bot, Job, JobEnd, JobId, JobRunner, JobState, JobSummary, KvJobs, KvRateLimiter,
    Listen, Outbound, Pacer, Rate, ScheduledJob, TokenBucket,
};
use meta_whatsapp_client::messages::{OutboundMessage, SendResponse};
use meta_whatsapp_core::clock::{Clock, ManualClock};
use meta_whatsapp_core::error::{StorageError, TransportError};
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId, UserId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_core::testing::ScriptedTransport;
use meta_whatsapp_core::{Error, GraphApiError};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use tokio::sync::{Barrier, Notify};

const T0: OffsetDateTime = datetime!(2026-11-01 12:00 UTC);

fn number() -> PhoneNumberId {
    PhoneNumberId::new(NUMBER).unwrap()
}

fn id(s: &str) -> JobId {
    JobId::new(s).unwrap()
}

fn phone(i: usize) -> Recipient {
    Recipient::phone(format!("+1650555{i:04}"))
}

/// A recipient known by BSUID only (no phone number).
fn user(i: usize) -> Recipient {
    Recipient::user(UserId::new(format!("US.1349120865530274{i:04}")))
}

/// Who a message went to: `to` (phone) or `recipient` (BSUID).
fn to(message: &Value) -> String {
    message["to"]
        .as_str()
        .or_else(|| message["recipient"].as_str())
        .unwrap_or_default()
        .to_owned()
}

fn label(recipient: &Recipient) -> String {
    to(&serde_json::to_value(OutboundMessage::text(recipient.clone(), "x")).unwrap())
}

fn labels(recipients: &[Recipient]) -> Vec<String> {
    recipients.iter().map(label).collect()
}

/// A text message of the job's `text` (the composer's signature returns a `Result`).
#[allow(clippy::unnecessary_wraps)]
fn text(job: &ScheduledJob, recipient: &Recipient) -> meta_whatsapp_core::Result<OutboundMessage> {
    Ok(OutboundMessage::text(
        recipient.clone(),
        job.payload["text"].as_str().unwrap_or("hello"),
    ))
}

/// A store, the jobs on it, and the clock all of them read.
struct World {
    clock: ManualClock,
    kv: Arc<MemoryKvStore>,
}

impl World {
    fn new() -> Self {
        let clock = ManualClock::new(T0);
        let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
        Self { clock, kv }
    }

    fn jobs(&self) -> KvJobs {
        self.jobs_on(self.kv.clone())
    }

    fn jobs_on(&self, kv: Arc<dyn KvStore>) -> KvJobs {
        KvJobs::new(kv, Arc::new(self.clock.clone()))
    }

    fn pacer(&self) -> Pacer {
        Pacer::new(TokenBucket::new(Rate::per_second(80).unwrap()).adaptive(false))
            .with_timer(self.clock.clone())
    }

    fn runner(&self, jobs: KvJobs, outbound: &Fake, runner_id: &str) -> JobRunner {
        JobRunner::builder(jobs)
            .outbound(outbound.clone())
            .pacer(self.pacer())
            .compose(text)
            .concurrency(1)
            .runner_id(runner_id)
            .build()
            .unwrap()
    }

    fn advance(&self, by: Duration) {
        self.clock.advance(by);
    }
}

type Failure = Box<dyn FnOnce() -> Error + Send>;

/// Which send cancels which job.
type CancelOn = (usize, KvJobs, JobId);

/// Which send never returns, and who is told it started.
type HangOn = (usize, Arc<Notify>);

/// The failures to answer, by recipient, in order.
type Failures = HashMap<String, VecDeque<Failure>>;

/// An `Outbound` that records every send it is handed (in order, by
/// recipient), answers Meta's example response with a message id of its
/// own per send, fails the sends it is told to, and can hang forever on
/// one send (a crash mid-send) or cancel a job during one.
#[derive(Clone, Default)]
struct Fake {
    attempts: Arc<Mutex<Vec<String>>>,
    ids: Arc<Mutex<HashMap<String, Vec<String>>>>,
    failures: Arc<Mutex<Failures>>,
    hang_on: Arc<Mutex<Option<HangOn>>>,
    cancel_on: Arc<Mutex<Option<CancelOn>>>,
}

impl std::fmt::Debug for Fake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Fake")
    }
}

impl Fake {
    fn attempts(&self) -> Vec<String> {
        self.attempts.lock().unwrap().clone()
    }

    fn fail(&self, recipient: &Recipient, error: impl FnOnce() -> Error + Send + 'static) {
        self.failures
            .lock()
            .unwrap()
            .entry(label(recipient))
            .or_default()
            .push_back(Box::new(error));
    }

    /// The id of the last message sent to `recipient`.
    fn message_id(&self, recipient: &Recipient) -> String {
        self.ids.lock().unwrap()[&label(recipient)]
            .last()
            .unwrap()
            .clone()
    }

    /// The `n`-th send (1-based) never returns; `notify` is told when it
    /// starts.
    fn hang_on(&self, n: usize) -> Arc<Notify> {
        let notify = Arc::new(Notify::new());
        *self.hang_on.lock().unwrap() = Some((n, notify.clone()));
        notify
    }

    /// The `n`-th send cancels the job `id` before it answers.
    fn cancel_on(&self, n: usize, jobs: KvJobs, id: JobId) {
        *self.cancel_on.lock().unwrap() = Some((n, jobs, id));
    }
}

#[async_trait]
impl Outbound for Fake {
    async fn send(
        &self,
        _from: &PhoneNumberId,
        message: &OutboundMessage,
    ) -> meta_whatsapp_core::Result<SendResponse> {
        message.validate()?;
        let to = to(&serde_json::to_value(message).unwrap());
        let n = {
            let mut attempts = self.attempts.lock().unwrap();
            attempts.push(to.clone());
            attempts.len()
        };
        let hang = self.hang_on.lock().unwrap().clone();
        if let Some((at, notify)) = hang
            && at == n
        {
            notify.notify_one();
            futures::future::pending::<()>().await;
        }
        let cancel = self.cancel_on.lock().unwrap().take();
        if let Some((at, jobs, id)) = cancel {
            if at == n {
                assert!(jobs.cancel(&id).await.unwrap());
            } else {
                *self.cancel_on.lock().unwrap() = Some((at, jobs, id));
            }
        }
        let failure = self
            .failures
            .lock()
            .unwrap()
            .get_mut(&to)
            .and_then(VecDeque::pop_front);
        if let Some(failure) = failure {
            return Err(failure());
        }
        let message_id = format!("wamid.job.{n}");
        self.ids
            .lock()
            .unwrap()
            .entry(to.clone())
            .or_default()
            .push(message_id.clone());
        // Meta's example response (`messages/text-messages`), with this
        // send's own id.
        Ok(serde_json::from_value(json!({
            "messaging_product": "whatsapp",
            "contacts": [{"input": to, "wa_id": to.trim_start_matches('+')}],
            "messages": [{"id": message_id}]
        }))
        .unwrap())
    }

    async fn mark_read(
        &self,
        _: &PhoneNumberId,
        _: &MessageId,
        _: bool,
    ) -> meta_whatsapp_core::Result<()> {
        Ok(())
    }
}

/// A Graph error as the client reports it on a 400.
fn graph(code: i64) -> Error {
    let mut error = GraphApiError::new(code, format!("(#{code})"));
    error.http_status = Some(400);
    error.into()
}

/// A `KvStore` over the memory store that interleaves runners
/// deterministically: once armed, the first two reads of a job record
/// each wait for the other, so both runners read the same version before
/// either claims; and a hook can run once just before the cursor is first
/// moved forward.
#[derive(Debug)]
struct Gate {
    inner: Arc<MemoryKvStore>,
    barrier: Arc<Barrier>,
    waits: AtomicUsize,
    armed: AtomicBool,
    before_cursor_moves: Mutex<Option<Hook>>,
    /// Refuse to create recipients' markers.
    fail_markers: AtomicBool,
    /// Refuse every read.
    fail_reads: AtomicBool,
    /// Refuse every read once the cursor has moved forward (a runner that
    /// fails, or crashes, right after moving it).
    fail_reads_after_cursor_moves: AtomicBool,
    /// Runs on every read, after it (the clock moving while a runner
    /// reads, another runner ticking meanwhile).
    on_get: Mutex<Option<KeyHook>>,
    /// Runs on every `put_if_absent`, before it.
    on_put_if_absent: Mutex<Option<KeyHook>>,
    /// Reads made.
    gets: AtomicUsize,
}

/// What a [`KeyHook`] runs.
type KeyFn = dyn Fn(&StoreKey) -> BoxFuture<'static, ()> + Send + Sync;

/// A hook run with the key of each call.
#[derive(Clone)]
struct KeyHook(Arc<KeyFn>);

impl std::fmt::Debug for KeyHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyHook")
    }
}

struct Hook(Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>);

impl std::fmt::Debug for Hook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hook")
    }
}

impl Gate {
    fn new(inner: Arc<MemoryKvStore>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            barrier: Arc::new(Barrier::new(2)),
            waits: AtomicUsize::new(0),
            armed: AtomicBool::new(false),
            before_cursor_moves: Mutex::new(None),
            fail_markers: AtomicBool::new(false),
            fail_reads: AtomicBool::new(false),
            fail_reads_after_cursor_moves: AtomicBool::new(false),
            on_get: Mutex::new(None),
            on_put_if_absent: Mutex::new(None),
            gets: AtomicUsize::new(0),
        })
    }

    fn on_get(&self, hook: impl Fn(&StoreKey) -> BoxFuture<'static, ()> + Send + Sync + 'static) {
        *self.on_get.lock().unwrap() = Some(KeyHook(Arc::new(hook)));
    }

    fn on_put_if_absent(
        &self,
        hook: impl Fn(&StoreKey) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) {
        *self.on_put_if_absent.lock().unwrap() = Some(KeyHook(Arc::new(hook)));
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl KvStore for Gate {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(StorageError::Backend(anyhow::anyhow!("down")));
        }
        self.gets.fetch_add(1, Ordering::SeqCst);
        // Read first, then wait: both runners hold the same version when
        // either goes on to claim.
        let read = self.inner.get(key).await;
        let hook = self.on_get.lock().unwrap().clone();
        if let Some(KeyHook(hook)) = hook {
            hook(key).await;
        }
        if key.namespace() == JOBS_NAMESPACE
            && self.armed.load(Ordering::SeqCst)
            && self.waits.fetch_add(1, Ordering::SeqCst) < 2
        {
            self.barrier.wait().await;
        }
        read
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.inner.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        if self.fail_markers.load(Ordering::SeqCst) && key.namespace() == JOBS_SENT_NAMESPACE {
            return Err(StorageError::Backend(anyhow::anyhow!("down")));
        }
        let hook = self.on_put_if_absent.lock().unwrap().clone();
        if let Some(KeyHook(hook)) = hook {
            hook(key).await;
        }
        self.inner.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        let cursor = key.key() == "cursor";
        if cursor {
            let hook = self.before_cursor_moves.lock().unwrap().take();
            if let Some(Hook(hook)) = hook {
                hook().await;
            }
        }
        let swapped = self
            .inner
            .compare_and_swap(key, expected, new, expiry)
            .await;
        if cursor
            && matches!(swapped, Ok(Some(_)))
            && self.fail_reads_after_cursor_moves.load(Ordering::SeqCst)
        {
            self.fail_reads.store(true, Ordering::SeqCst);
        }
        swapped
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.inner.delete(key).await
    }
}

fn summary_of(end: &JobEnd) -> &JobSummary {
    match end {
        JobEnd::Done(summary) | JobEnd::Stopped(summary) | JobEnd::Retrying { summary, .. } => {
            summary
        }
        other => panic!("no summary: {other:?}"),
    }
}

// ─── Decisive: two runners, one store ────────────────────────────────────

/// Two runners read the same due job at the same version: only one claim
/// succeeds, and the job is sent once. Replacing the claim's
/// compare-and-swap by a plain put fails this (both runners claim and run
/// it: two runs); the send itself is guarded a second time by the
/// recipient's marker.
#[tokio::test]
async fn two_runners_on_one_store_send_a_due_job_once() {
    let world = World::new();
    let gate = Gate::new(world.kv.clone());
    let jobs = world.jobs_on(gate.clone());
    jobs.schedule(
        Job::new(id("reminder-1"), number(), T0)
            .to([user(1)])
            .payload(json!({"text": "Your table is booked for 8 pm"})),
    )
    .await
    .unwrap();
    let outbound = Fake::default();
    let a = world.runner(jobs.clone(), &outbound, "replica-a");
    let b = world.runner(jobs.clone(), &outbound, "replica-b");

    gate.arm();
    let (ta, tb) = tokio::join!(a.tick(), b.tick());
    let (ta, tb) = (ta.unwrap(), tb.unwrap());

    assert_eq!(
        ta.runs.len() + tb.runs.len(),
        1,
        "exactly one runner claims the job: {ta:?} {tb:?}"
    );
    assert_eq!(outbound.attempts(), labels(&[user(1)]));
    let run = ta.runs.first().or(tb.runs.first()).unwrap();
    assert_eq!(summary_of(&run.ended).sent, 1);
    assert_eq!(
        jobs.get(&id("reminder-1")).await.unwrap().unwrap().state,
        JobState::Done
    );
    // Nothing is left for anyone.
    assert!(a.tick().await.unwrap().runs.is_empty());
    assert!(b.tick().await.unwrap().runs.is_empty());
    assert_eq!(outbound.attempts().len(), 1);
}

// ─── Decisive: a restart ─────────────────────────────────────────────────

/// Scheduled for later, then the process goes away; a new runner (new
/// `KvJobs`, new id) on the same store sends it once it is due, and not
/// before.
#[tokio::test]
async fn a_job_scheduled_before_a_restart_is_sent_by_a_new_runner() {
    let world = World::new();
    let outbound = Fake::default();
    {
        let jobs = world.jobs();
        jobs.schedule(
            Job::new(id("restock"), number(), T0 + Duration::from_secs(3600))
                .to([phone(1)])
                .payload(json!({"text": "Back in stock"})),
        )
        .await
        .unwrap();
        let before = world.runner(jobs, &outbound, "before-restart");
        assert!(before.tick().await.unwrap().runs.is_empty());
    }
    let jobs = world.jobs();
    let after = world.runner(jobs.clone(), &outbound, "after-restart");
    world.advance(Duration::from_mins(30));
    assert!(after.tick().await.unwrap().runs.is_empty(), "not due yet");
    assert!(outbound.attempts().is_empty());

    world.advance(Duration::from_mins(30));
    let report = after.tick().await.unwrap();
    assert_eq!(report.runs.len(), 1);
    assert_eq!(report.runs[0].id, id("restock"));
    assert!(matches!(report.runs[0].ended, JobEnd::Done(_)));
    assert_eq!(outbound.attempts(), labels(&[phone(1)]));
    let job = jobs.get(&id("restock")).await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Done);
    assert_eq!(job.summary.unwrap().sent, 1);
}

// ─── Decisive: resume after a crash mid-broadcast ────────────────────────

/// Five recipients; the runner crashes during the third send (its future
/// dropped while the request is in flight). Returns the world, the jobs
/// and the outbound.
async fn crash_mid_broadcast() -> (World, KvJobs, Fake, Vec<Recipient>) {
    let world = World::new();
    let jobs = world.jobs();
    let recipients = vec![phone(1), user(2), phone(3), user(4), phone(5)];
    jobs.schedule(
        Job::new(id("spring-sale"), number(), T0)
            .to(recipients.clone())
            .payload(json!({"text": "Spring sale: 20% off"})),
    )
    .await
    .unwrap();
    let outbound = Fake::default();
    let crashed = outbound.hang_on(3);
    let a = world.runner(jobs.clone(), &outbound, "replica-a");
    {
        let tick = pin!(a.tick());
        let crash = pin!(crashed.notified());
        let Either::Right(_) = select(tick, crash).await else {
            panic!("the tick ended before the crash");
        };
    }
    drop(a);
    assert_eq!(outbound.attempts(), labels(&recipients[..3]));
    (world, jobs, outbound, recipients)
}

/// The runner that resumes the job skips the recipients recorded as sent
/// and the one whose send was in flight (its outcome unknown), and sends
/// the rest. Removing the per-recipient marker fails this (the first three
/// are sent again).
#[tokio::test]
async fn a_resumed_broadcast_never_resends_a_recipient_recorded_as_sent() {
    let (world, jobs, outbound, recipients) = crash_mid_broadcast().await;
    world.advance(Duration::from_secs(121));
    let b = world.runner(jobs.clone(), &outbound, "replica-b");
    let report = b.tick().await.unwrap();

    assert_eq!(report.runs.len(), 1);
    let summary = summary_of(&report.runs[0].ended);
    assert_eq!(summary.sent, 4);
    assert_eq!(summary.unknown, [2], "the send in flight is never resent");
    assert_eq!(summary.unsent, 0);
    let mut expected = labels(&recipients[..3]);
    expected.extend(labels(&recipients[3..]));
    assert_eq!(outbound.attempts(), expected);
    assert_eq!(
        jobs.get(&id("spring-sale")).await.unwrap().unwrap().state,
        JobState::Done
    );
}

/// Until the crashed runner's lease expires, nobody else claims the job;
/// after, one does. Ignoring the lease fails this (the job is resumed
/// at once).
#[tokio::test]
async fn a_crashed_runners_lease_holds_the_job_until_it_expires() {
    let (world, jobs, outbound, _) = crash_mid_broadcast().await;
    let b = world.runner(jobs.clone(), &outbound, "replica-b");
    world.advance(Duration::from_secs(119));
    assert!(b.tick().await.unwrap().runs.is_empty());
    assert_eq!(outbound.attempts().len(), 3);
    assert_eq!(
        jobs.get(&id("spring-sale")).await.unwrap().unwrap().state,
        JobState::Claimed
    );
    world.advance(Duration::from_secs(2));
    assert_eq!(b.tick().await.unwrap().runs.len(), 1);
    assert_eq!(outbound.attempts().len(), 5);
}

// ─── Decisive: 131049 from a status webhook ──────────────────────────────

/// Meta's failed-status fixture (`131049`, "healthy ecosystem
/// engagement") for the message `message_id`.
fn marketing_limited(message_id: &str) -> meta_whatsapp_webhooks::WebhookEvent {
    let mut json = fixture_json("messages/status_failed.json");
    json["entry"][0]["changes"][0]["value"]["statuses"][0]["id"] = json!(message_id);
    let mut events = events_of(&json);
    assert_eq!(events.len(), 1);
    events.remove(0)
}

/// The job keeps each message's id; a `131049` status for one marks its
/// recipient, and a re-run within 24 hours skips them and sends everyone
/// else. After 24 hours a re-run sends to them again. Removing the
/// limited check fails this.
#[tokio::test]
async fn a_131049_status_makes_a_rerun_within_24_hours_skip_that_recipient() {
    let world = World::new();
    let jobs = world.jobs();
    let recipients = vec![user(1), phone(2), user(3)];
    jobs.schedule(
        Job::new(id("weekly-deals"), number(), T0)
            .to(recipients.clone())
            .payload(json!({"text": "This week's deals"})),
    )
    .await
    .unwrap();
    let outbound = Fake::default();
    let runner = world.runner(jobs.clone(), &outbound, "replica-a");
    let first = runner.tick().await.unwrap();
    assert_eq!(summary_of(&first.runs[0].ended).sent, 3);

    // Meta accepted all three; then a failed status with 131049 for the
    // second, delivered to the feedback sink.
    let limited = outbound.message_id(&recipients[1]);
    jobs.feedback()
        .deliver(marketing_limited(&limited))
        .await
        .unwrap();
    // A status for a message no job sent is not recorded.
    assert!(
        !jobs
            .observe(&marketing_limited("wamid.not-ours"))
            .await
            .unwrap()
    );

    world.advance(Duration::from_secs(3600));
    assert!(
        jobs.rerun(&id("weekly-deals"), world.clock.now())
            .await
            .unwrap()
    );
    let second = runner.tick().await.unwrap();
    assert_eq!(second.runs.len(), 1);
    assert_eq!(second.runs[0].run, 2);
    let summary = summary_of(&second.runs[0].ended);
    assert_eq!(summary.limited, [1]);
    assert_eq!(summary.sent, 2);
    let mut expected = labels(&recipients);
    expected.extend(labels(&[recipients[0].clone(), recipients[2].clone()]));
    assert_eq!(outbound.attempts(), expected);

    // Two minutes short of 24 hours after the status, still limited.
    world.advance(Duration::from_mins(22 * 60 + 58));
    assert!(
        jobs.rerun(&id("weekly-deals"), world.clock.now())
            .await
            .unwrap()
    );
    let third = runner.tick().await.unwrap();
    assert_eq!(summary_of(&third.runs[0].ended).limited, [1]);
    assert_eq!(outbound.attempts().len(), 7);

    // 24 hours after the status, the limit is over.
    world.advance(Duration::from_mins(3));
    assert!(
        jobs.rerun(&id("weekly-deals"), world.clock.now())
            .await
            .unwrap()
    );
    let fourth = runner.tick().await.unwrap();
    let summary = summary_of(&fourth.runs[0].ended);
    assert!(summary.limited.is_empty());
    assert_eq!(summary.sent, 3);
    assert_eq!(outbound.attempts().len(), 10);
}

/// The feedback as a bot's listener of `status_updated`: the bot hears
/// the status webhook, and the re-run skips that recipient.
#[tokio::test]
async fn the_feedback_works_as_a_bot_listener() {
    let world = World::new();
    let jobs = world.jobs();
    jobs.schedule(Job::new(id("j"), number(), T0).to([user(1), user(2)]))
        .await
        .unwrap();
    let outbound = Fake::default();
    let runner = world.runner(jobs.clone(), &outbound, "r");
    runner.tick().await.unwrap();
    let bot = Bot::builder()
        .outbound(common::Recording::default())
        .listen(Listen::event("status_updated"), jobs.feedback())
        .build()
        .await
        .unwrap();
    bot.deliver(marketing_limited(&outbound.message_id(&user(2))))
        .await
        .unwrap();
    assert!(jobs.rerun(&id("j"), world.clock.now()).await.unwrap());
    let report = runner.tick().await.unwrap();
    assert_eq!(summary_of(&report.runs[0].ended).limited, [1]);
    assert_eq!(outbound.attempts(), labels(&[user(1), user(2), user(1)]));
}

/// Only a status carrying `131049` is recorded: a `delivered` status, or a
/// failure with another code, is not.
#[tokio::test]
async fn only_a_131049_status_marks_a_recipient() {
    let world = World::new();
    let jobs = world.jobs();
    jobs.schedule(Job::new(id("j"), number(), T0).to([phone(1)]))
        .await
        .unwrap();
    let outbound = Fake::default();
    world
        .runner(jobs.clone(), &outbound, "r")
        .tick()
        .await
        .unwrap();
    let sent = outbound.message_id(&phone(1));
    let mut other = fixture_json("messages/status_failed.json");
    let status = &mut other["entry"][0]["changes"][0]["value"]["statuses"][0];
    status["id"] = json!(sent);
    status["errors"][0]["code"] = json!(131026);
    assert!(!jobs.observe(&events_of(&other)[0]).await.unwrap());
    let mut delivered = fixture_json("messages/status_failed.json");
    let status = &mut delivered["entry"][0]["changes"][0]["value"]["statuses"][0];
    status["id"] = json!(sent);
    status["status"] = json!("delivered");
    status.as_object_mut().unwrap().remove("errors");
    assert!(!jobs.observe(&events_of(&delivered)[0]).await.unwrap());
    assert!(jobs.observe(&marketing_limited(&sent)).await.unwrap());
}

/// A `131049` the send itself returns marks the recipient too.
#[tokio::test]
async fn a_131049_refusal_marks_the_recipient_for_the_rerun() {
    let world = World::new();
    let jobs = world.jobs();
    jobs.schedule(Job::new(id("j"), number(), T0).to([phone(1), phone(2)]))
        .await
        .unwrap();
    let outbound = Fake::default();
    outbound.fail(&phone(1), || graph(131049));
    let runner = world.runner(jobs.clone(), &outbound, "r");
    let first = runner.tick().await.unwrap();
    let summary = summary_of(&first.runs[0].ended);
    assert_eq!((summary.sent, summary.failed), (1, 1));
    assert!(jobs.rerun(&id("j"), world.clock.now()).await.unwrap());
    let second = runner.tick().await.unwrap();
    assert_eq!(summary_of(&second.runs[0].ended).limited, [0]);
    assert_eq!(outbound.attempts(), labels(&[phone(1), phone(2), phone(2)]));
}

// ─── At most once, retries, cancel ───────────────────────────────────────

/// A timed-out send may have gone out: its marker says unknown and it is
/// never sent again, not even by the job's retry (which a throttled
/// recipient causes here). The retry backs off and is counted.
#[tokio::test]
async fn a_timed_out_send_is_marked_unknown_and_never_resent() {
    let world = World::new();
    let jobs = world.jobs();
    let recipients = vec![phone(1), phone(2), phone(3)];
    jobs.schedule(Job::new(id("j"), number(), T0).to(recipients.clone()))
        .await
        .unwrap();
    let outbound = Fake::default();
    outbound.fail(&phone(1), || TransportError::Timeout.into());
    outbound.fail(&phone(2), || graph(130429));
    let runner = JobRunner::builder(jobs.clone())
        .outbound(outbound.clone())
        .pacer(world.pacer())
        .compose(text)
        .concurrency(1)
        // One send per recipient and pass: the throttled one waits for the
        // job's retry.
        .policy(Backoff::new().max_attempts(1).unwrap())
        .build()
        .unwrap();

    let first = runner.tick().await.unwrap();
    let JobEnd::Retrying { at, summary } = &first.runs[0].ended else {
        panic!("{:?}", first.runs[0].ended);
    };
    assert_eq!(summary.unknown, [0]);
    assert_eq!((summary.sent, summary.unsent), (1, 1));
    let job = jobs.get(&id("j")).await.unwrap().unwrap();
    assert_eq!((job.state, job.retries), (JobState::Pending, 1));
    assert_eq!(job.due_at, *at);
    let first_retry = *at - world.clock.now();
    assert!(
        first_retry >= time::Duration::seconds(29) && first_retry <= time::Duration::seconds(30),
        "{first_retry}"
    );

    assert!(runner.tick().await.unwrap().runs.is_empty(), "not due yet");
    world.clock.set(*at);
    let second = runner.tick().await.unwrap();
    let JobEnd::Done(summary) = &second.runs[0].ended else {
        panic!("{:?}", second.runs[0].ended);
    };
    assert_eq!(summary.sent, 2);
    assert_eq!(summary.unknown, [0]);
    assert_eq!(
        outbound.attempts(),
        labels(&[phone(1), phone(2), phone(3), phone(2)])
    );
    let job = jobs.get(&id("j")).await.unwrap().unwrap();
    assert_eq!(job.summary.unwrap().unknown, [0]);
}

/// Every pass throttled: the job is retried with a doubling delay, then
/// stopped after its last retry, its recipient provably unsent.
#[tokio::test]
async fn retries_back_off_then_the_job_stops() {
    let world = World::new();
    let jobs = world.jobs();
    jobs.schedule(Job::new(id("j"), number(), T0).to([phone(1)]))
        .await
        .unwrap();
    let outbound = Fake::default();
    for _ in 0..3 {
        outbound.fail(&phone(1), || graph(130429));
    }
    let runner = JobRunner::builder(jobs.clone())
        .outbound(outbound.clone())
        .pacer(world.pacer())
        .compose(text)
        .policy(Backoff::new().max_attempts(1).unwrap())
        .retries(2)
        .retry_delay(Duration::from_secs(10), Duration::from_secs(15))
        .build()
        .unwrap();
    let mut delays = Vec::new();
    loop {
        let before = world.clock.now();
        let report = runner.tick().await.unwrap();
        match &report.runs[0].ended {
            JobEnd::Retrying { at, .. } => {
                delays.push((*at - before).whole_seconds());
                world.clock.set(*at);
            }
            JobEnd::Stopped(summary) => {
                assert_eq!(summary.unsent, 1);
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(delays, [10, 15], "10 s, then doubled up to the longest");
    let job = jobs.get(&id("j")).await.unwrap().unwrap();
    assert_eq!((job.state, job.retries), (JobState::Stopped, 2));
    assert_eq!(outbound.attempts().len(), 3);
}

/// Cancelled before it is due: nothing is sent, ever.
#[tokio::test]
async fn a_job_cancelled_before_it_is_due_is_never_sent() {
    let world = World::new();
    let jobs = world.jobs();
    jobs.schedule(Job::new(id("j"), number(), T0 + Duration::from_secs(60)).to([phone(1)]))
        .await
        .unwrap();
    assert!(jobs.cancel(&id("j")).await.unwrap());
    assert!(!jobs.cancel(&id("j")).await.unwrap(), "once");
    world.advance(Duration::from_secs(120));
    let outbound = Fake::default();
    assert!(
        world
            .runner(jobs.clone(), &outbound, "r")
            .tick()
            .await
            .unwrap()
            .runs
            .is_empty()
    );
    assert!(outbound.attempts().is_empty());
    assert_eq!(
        jobs.get(&id("j")).await.unwrap().unwrap().state,
        JobState::Cancelled
    );
}

/// Cancelled while it runs (during its second send): the send in flight
/// finishes, and the next is never made.
#[tokio::test]
async fn a_job_cancelled_while_it_runs_stops_before_its_next_send() {
    let world = World::new();
    let jobs = world.jobs();
    let recipients = vec![phone(1), phone(2), phone(3), phone(4)];
    jobs.schedule(Job::new(id("j"), number(), T0).to(recipients.clone()))
        .await
        .unwrap();
    let outbound = Fake::default();
    outbound.cancel_on(2, jobs.clone(), id("j"));
    let runner = world.runner(jobs.clone(), &outbound, "r");
    let report = runner.tick().await.unwrap();
    assert!(matches!(report.runs[0].ended, JobEnd::Cancelled));
    assert_eq!(outbound.attempts(), labels(&recipients[..2]));
    assert!(runner.tick().await.unwrap().runs.is_empty());
    assert_eq!(
        jobs.get(&id("j")).await.unwrap().unwrap().state,
        JobState::Cancelled
    );
}

// ─── The store ───────────────────────────────────────────────────────────

/// Fail closed: a store that cannot write a recipient's marker gets no
/// send for them (the job is retried later), and a store that cannot be
/// read gets no claim at all.
#[tokio::test]
async fn a_store_that_fails_sends_nothing() {
    let world = World::new();
    let gate = Gate::new(world.kv.clone());
    let jobs = world.jobs_on(gate.clone());
    jobs.schedule(Job::new(id("j"), number(), T0).to([phone(1)]))
        .await
        .unwrap();
    gate.fail_markers.store(true, Ordering::SeqCst);
    let outbound = Fake::default();
    let runner = world.runner(jobs.clone(), &outbound, "r");
    let report = runner.tick().await.unwrap();
    let JobEnd::Retrying { summary, .. } = &report.runs[0].ended else {
        panic!("{:?}", report.runs[0].ended);
    };
    assert_eq!(summary.unsent, 1);
    assert!(outbound.attempts().is_empty());

    gate.fail_reads.store(true, Ordering::SeqCst);
    world.advance(Duration::from_secs(60));
    assert!(runner.tick().await.is_err());
    assert!(outbound.attempts().is_empty());
}

/// More jobs in one minute than a page holds: the bucket continues on a
/// second page, and every job is run once.
#[tokio::test]
async fn a_full_bucket_continues_on_its_next_page() {
    let world = World::new();
    let jobs = world.jobs();
    let count = meta_whatsapp_bot::jobs::BUCKET_PAGE_CAPACITY + 1;
    for i in 0..count {
        jobs.schedule(Job::new(id(&format!("j{i}")), number(), T0).to([phone(i)]))
            .await
            .unwrap();
    }
    let outbound = Fake::default();
    let report = world
        .runner(jobs.clone(), &outbound, "r")
        .tick()
        .await
        .unwrap();
    assert_eq!(report.runs.len(), count);
    assert_eq!(outbound.attempts().len(), count);
}

/// A job indexed into a minute while a runner moves the cursor past it
/// (its writer read the cursor before it moved, so did not move it back):
/// the runner reads the minute again, moves the cursor back, and the next
/// tick runs it. Without that second read the job is never found.
#[tokio::test]
async fn a_job_indexed_while_the_cursor_moves_is_not_lost() {
    let world = World::new();
    let gate = Gate::new(world.kv.clone());
    let jobs = world.jobs_on(gate.clone());
    jobs.schedule(Job::new(id("first"), number(), T0).to([phone(1)]))
        .await
        .unwrap();
    let late = world.jobs();
    *gate.before_cursor_moves.lock().unwrap() = Some(Hook(Box::new(move || {
        Box::pin(async move {
            late.schedule(Job::new(id("late"), number(), T0).to([phone(2)]))
                .await
                .unwrap();
        })
    })));
    let outbound = Fake::default();
    let runner = world.runner(jobs.clone(), &outbound, "r");
    let first = runner.tick().await.unwrap();
    assert_eq!(first.runs.len(), 1);
    let second = runner.tick().await.unwrap();
    assert_eq!(second.runs.len(), 1);
    assert_eq!(second.runs[0].id, id("late"));
    assert_eq!(outbound.attempts(), labels(&[phone(1), phone(2)]));
}

/// A job re-run into the very minute it ran in, while a runner moves the
/// cursor past that minute: its id is in the minute's page already, and
/// the page is written again anyway, so the runner sees the change and
/// moves the cursor back. Without that write, the re-run is never found.
#[tokio::test]
async fn a_rerun_into_the_minute_it_ran_in_is_not_lost() {
    let world = World::new();
    let gate = Gate::new(world.kv.clone());
    let jobs = world.jobs_on(gate.clone());
    jobs.schedule(Job::new(id("j"), number(), T0).to([phone(1)]))
        .await
        .unwrap();
    let outbound = Fake::default();
    let runner = world.runner(jobs.clone(), &outbound, "r");
    // The first tick runs the job, then (moving the cursor) meets a re-run
    // made just before.
    let late = world.jobs();
    let clock = world.clock.clone();
    *gate.before_cursor_moves.lock().unwrap() = Some(Hook(Box::new(move || {
        Box::pin(async move {
            assert!(late.rerun(&id("j"), clock.now()).await.unwrap());
        })
    })));
    let first = runner.tick().await.unwrap();
    assert_eq!(first.runs.len(), 1);
    let second = runner.tick().await.unwrap();
    assert_eq!(second.runs.len(), 1, "the re-run is found");
    assert_eq!(second.runs[0].run, 2);
    assert_eq!(outbound.attempts(), labels(&[phone(1), phone(1)]));
}

/// Scheduling is idempotent for the same job (a retried call) and refuses
/// another under the same id; each person once.
#[tokio::test]
async fn scheduling_is_idempotent_and_keeps_each_person_once() {
    let world = World::new();
    let jobs = world.jobs();
    let job = Job::new(id("j"), number(), T0)
        .to([phone(1), Recipient::phone("16505550001"), user(2)])
        .payload(json!({"text": "hi"}));
    jobs.schedule(job.clone()).await.unwrap();
    jobs.schedule(job).await.unwrap();
    let other = Job::new(id("j"), number(), T0).to([phone(9)]);
    assert!(matches!(
        jobs.schedule(other).await,
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        jobs.schedule(Job::new(id("empty"), number(), T0)).await,
        Err(Error::Validation(_))
    ));
    let stored = jobs.get(&id("j")).await.unwrap().unwrap();
    assert_eq!(stored.recipients, [phone(1), user(2)]);
    let outbound = Fake::default();
    let report = world
        .runner(jobs.clone(), &outbound, "r")
        .tick()
        .await
        .unwrap();
    assert_eq!(report.runs.len(), 1);
    assert_eq!(outbound.attempts(), labels(&[phone(1), user(2)]));
}

/// Through the client: the request is Meta's send, exactly, with the
/// composer's message.
#[tokio::test]
async fn a_scheduled_message_is_sent_as_the_exact_request() {
    let world = World::new();
    let jobs = world.jobs();
    jobs.schedule(
        Job::new(id("order-1042-shipped"), number(), T0)
            .to([Recipient::phone("+16505551234")])
            .payload(json!({"text": "Your order 1042 has shipped"})),
    )
    .await
    .unwrap();
    let t = ScriptedTransport::new();
    t.push_json(200, send_response());
    let runner = JobRunner::builder(jobs.clone())
        .client(client(&t))
        .pacer(world.pacer())
        .compose(text)
        .build()
        .unwrap();
    let report = runner.tick().await.unwrap();
    assert!(matches!(report.runs[0].ended, JobEnd::Done(_)));
    let requests = t.requests();
    assert_eq!(t.remaining(), 0);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path(), format!("/v25.0/{NUMBER}/messages"));
    assert_eq!(requests[0].bearer(), Some("TOKEN"));
    assert_eq!(
        requests[0].json().unwrap(),
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505551234",
            "type": "text",
            "text": {"body": "Your order 1042 has shipped"}
        })
    );
}

/// The runner paces through any `Pacer`: over a `KvRateLimiter` on the
/// same store, every replica's jobs book from one budget per number (here
/// 20 a second: three sends span 100 ms).
#[tokio::test]
async fn jobs_pace_through_a_pacer_shared_by_replicas() {
    let world = World::new();
    let jobs = world.jobs();
    jobs.schedule(Job::new(id("j"), number(), T0).to([phone(1), phone(2), phone(3)]))
        .await
        .unwrap();
    let outbound = Fake::default();
    let shared = Pacer::new(KvRateLimiter::new(
        world.kv.clone(),
        Rate::per_second(20).unwrap(),
    ))
    .with_timer(world.clock.clone());
    let runner = JobRunner::builder(jobs.clone())
        .outbound(outbound.clone())
        .pacer(shared)
        .compose(text)
        .concurrency(1)
        .build()
        .unwrap();
    let report = runner.tick().await.unwrap();
    assert!(matches!(report.runs[0].ended, JobEnd::Done(_)));
    assert_eq!(outbound.attempts().len(), 3);
    assert!(world.clock.now() - T0 >= time::Duration::milliseconds(100));
}

/// The builder refuses what cannot run.
#[test]
fn the_runner_needs_its_parts() {
    let world = World::new();
    let outbound = Fake::default();
    let base = || JobRunner::builder(world.jobs());
    assert!(base().pacer(world.pacer()).compose(text).build().is_err());
    assert!(
        base()
            .outbound(outbound.clone())
            .compose(text)
            .build()
            .is_err()
    );
    assert!(
        base()
            .outbound(outbound.clone())
            .pacer(world.pacer())
            .build()
            .is_err()
    );
    assert!(
        base()
            .outbound(outbound.clone())
            .pacer(world.pacer())
            .compose(text)
            .lease(Duration::ZERO)
            .build()
            .is_err()
    );
    let a = base()
        .outbound(outbound.clone())
        .pacer(world.pacer())
        .compose(text)
        .build()
        .unwrap();
    let b = base()
        .outbound(outbound)
        .pacer(world.pacer())
        .compose(text)
        .build()
        .unwrap();
    assert_ne!(a.id(), b.id(), "two runners of one process have two ids");
}

// ─── Two runners, crash points, the due index (the sabotage review) ──────

/// A hook that runs `then` once, the first time a call's key is in
/// `namespace`, after moving the clock forward by `advance`.
fn once_in(
    namespace: &'static str,
    clock: &ManualClock,
    advance: Duration,
    then: impl Fn() -> BoxFuture<'static, ()> + Send + Sync + 'static,
) -> impl Fn(&StoreKey) -> BoxFuture<'static, ()> + Send + Sync + 'static {
    let fired = Arc::new(AtomicBool::new(false));
    let clock = clock.clone();
    let then = Arc::new(then);
    move |key: &StoreKey| {
        let fire = key.namespace() == namespace && !fired.swap(true, Ordering::SeqCst);
        let (clock, then) = (clock.clone(), then.clone());
        Box::pin(async move {
            if fire {
                clock.advance(advance);
                then().await;
            }
        })
    }
}

/// The reports of the ticks a hook ran.
type Reports = Arc<Mutex<Vec<meta_whatsapp_bot::TickReport>>>;

/// A hook's tick of `runner`, its report kept in `reports`.
fn tick_into(runner: &Arc<JobRunner>, reports: &Reports) -> BoxFuture<'static, ()> {
    let (runner, reports) = (runner.clone(), reports.clone());
    Box::pin(async move {
        let report = runner.tick().await.unwrap();
        reports.lock().unwrap().push(report);
    })
}

/// A runner paused between its check and the recipient's marker (a GC
/// pause, a slow store) for longer than its lease, while another runner
/// claims the job and sends to that recipient: the marker, created by
/// whoever comes first, stops the second send, and the first runner
/// learns it lost the job. Creating the marker with a plain `put` fails
/// this (the recipient is sent twice).
#[tokio::test]
async fn a_runner_whose_lease_expired_mid_send_never_sends_after_the_new_claim() {
    let world = World::new();
    let gate = Gate::new(world.kv.clone());
    let jobs = world.jobs_on(gate.clone());
    jobs.schedule(Job::new(id("j"), number(), T0).to([user(1)]))
        .await
        .unwrap();
    let outbound = Fake::default();
    let a = world.runner(jobs.clone(), &outbound, "replica-a");
    let b = Arc::new(world.runner(world.jobs(), &outbound, "replica-b"));
    let reports = Reports::default();
    let (runner, kept) = (b.clone(), reports.clone());
    gate.on_put_if_absent(once_in(
        JOBS_SENT_NAMESPACE,
        &world.clock,
        Duration::from_secs(121),
        move || tick_into(&runner, &kept),
    ));

    let ta = a.tick().await.unwrap();
    let tb = reports.lock().unwrap().pop().unwrap();
    assert_eq!(outbound.attempts(), labels(&[user(1)]), "sent once, by b");
    assert!(matches!(tb.runs[0].ended, JobEnd::Done(_)), "{tb:?}");
    assert!(matches!(ta.runs[0].ended, JobEnd::LeaseLost), "{ta:?}");
    assert_eq!(
        jobs.get(&id("j"))
            .await
            .unwrap()
            .unwrap()
            .summary
            .unwrap()
            .sent,
        1
    );
}

/// A cancel and a re-run land between a runner's check and its marker;
/// the re-run's runner sends to that recipient. The first runner, whose
/// marker is its old run's (so no marker stops it), checks the job again
/// with its marker in place, finds it no longer its own, and never sends.
/// Without that second check the recipient is sent twice.
#[tokio::test]
async fn a_send_checked_before_a_cancel_and_a_rerun_is_not_made() {
    let world = World::new();
    let gate = Gate::new(world.kv.clone());
    let jobs = world.jobs_on(gate.clone());
    jobs.schedule(Job::new(id("j"), number(), T0).to([user(1)]))
        .await
        .unwrap();
    let outbound = Fake::default();
    let a = world.runner(jobs.clone(), &outbound, "replica-a");
    let b = Arc::new(world.runner(world.jobs(), &outbound, "replica-b"));
    let reports = Reports::default();
    let (runner, kept, plain, clock) = (
        b.clone(),
        reports.clone(),
        world.jobs(),
        world.clock.clone(),
    );
    gate.on_put_if_absent(once_in(
        JOBS_SENT_NAMESPACE,
        &world.clock,
        Duration::ZERO,
        move || {
            let (plain, clock, tick) = (plain.clone(), clock.clone(), tick_into(&runner, &kept));
            Box::pin(async move {
                assert!(plain.cancel(&id("j")).await.unwrap());
                assert!(plain.rerun(&id("j"), clock.now()).await.unwrap());
                tick.await;
            })
        },
    ));

    let ta = a.tick().await.unwrap();
    let tb = reports.lock().unwrap().pop().unwrap();
    assert_eq!(
        outbound.attempts(),
        labels(&[user(1)]),
        "sent once, by run 2"
    );
    assert_eq!(tb.runs[0].run, 2);
    assert!(matches!(tb.runs[0].ended, JobEnd::Done(_)), "{tb:?}");
    assert!(matches!(ta.runs[0].ended, JobEnd::LeaseLost), "{ta:?}");
}

/// The runner moves the cursor past a minute, then fails before reading
/// that minute again (a store error; a crash is the same), while a job was
/// indexed into it by a writer that read the cursor before it moved (so
/// did not move it back). The next tick reads that minute again first,
/// against the page versions the cursor keeps, and finds the job. Without
/// them the cursor stays past the job, which is never sent.
#[tokio::test]
async fn a_runner_failing_right_after_moving_the_cursor_loses_no_job() {
    let world = World::new();
    let gate = Gate::new(world.kv.clone());
    let jobs = world.jobs_on(gate.clone());
    jobs.schedule(Job::new(id("first"), number(), T0).to([phone(1)]))
        .await
        .unwrap();
    let late = world.jobs();
    *gate.before_cursor_moves.lock().unwrap() = Some(Hook(Box::new(move || {
        Box::pin(async move {
            late.schedule(Job::new(id("late"), number(), T0).to([phone(2)]))
                .await
                .unwrap();
        })
    })));
    gate.fail_reads_after_cursor_moves
        .store(true, Ordering::SeqCst);
    let outbound = Fake::default();
    let runner = world.runner(jobs.clone(), &outbound, "r");
    assert!(runner.tick().await.is_err());
    assert_eq!(outbound.attempts(), labels(&[phone(1)]));

    gate.fail_reads_after_cursor_moves
        .store(false, Ordering::SeqCst);
    gate.fail_reads.store(false, Ordering::SeqCst);
    let second = runner.tick().await.unwrap();
    assert_eq!(second.runs.len(), 1, "the late job is found");
    assert_eq!(second.runs[0].id, id("late"));
    assert_eq!(outbound.attempts(), labels(&[phone(1), phone(2)]));
}

/// A job due long ago ("now or earlier" is allowed: a month, here) keeps
/// its due time but is indexed under the current minute, so the next tick
/// runs it without reading every minute since. Indexing it under its own
/// minute fails this: the cursor moves back a month, and the tick reads
/// over 40 000 minutes before it gets to anything else.
#[tokio::test]
async fn a_job_due_long_ago_is_run_without_reading_every_minute_since() {
    let world = World::new();
    let gate = Gate::new(world.kv.clone());
    let jobs = world.jobs_on(gate.clone());
    let long_ago = T0 - Duration::from_hours(30 * 24);
    jobs.schedule(Job::new(id("j"), number(), long_ago).to([phone(1)]))
        .await
        .unwrap();
    assert_eq!(
        jobs.get(&id("j")).await.unwrap().unwrap().due_at,
        long_ago,
        "the due time is kept as given"
    );
    let outbound = Fake::default();
    let runner = world.runner(jobs.clone(), &outbound, "r");
    let before = gate.gets.load(Ordering::SeqCst);
    let report = runner.tick().await.unwrap();
    let reads = gate.gets.load(Ordering::SeqCst) - before;
    assert_eq!(report.runs.len(), 1);
    assert!(matches!(report.runs[0].ended, JobEnd::Done(_)));
    assert_eq!(outbound.attempts(), labels(&[phone(1)]));
    assert!(reads < 50, "{reads} reads for one job");
}

/// The key of a recipient's marker, derived here independently of the
/// crate (the pinned derivation: SHA-256 hex of `<len>:<part>,` parts).
fn marker_key_of(job: &str, run: u32, recipient: &str) -> StoreKey {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    for part in [job, &run.to_string(), recipient] {
        hasher.update(format!("{}:{part},", part.len()));
    }
    StoreKey::new(JOBS_SENT_NAMESPACE, hex::encode(hasher.finalize()))
}

/// An unreadable marker (a later release's, or damaged) is something
/// written for that recipient: an outcome unknown, never resent. The job
/// settles, and the jobs after it run. Failing on it instead stops every
/// tick at that job, for good, and every later job waits behind it.
#[tokio::test]
async fn an_unreadable_marker_is_an_outcome_unknown_not_a_stuck_runner() {
    let world = World::new();
    let jobs = world.jobs();
    jobs.schedule(Job::new(id("j"), number(), T0).to([phone(1), phone(2)]))
        .await
        .unwrap();
    jobs.schedule(Job::new(id("k"), number(), T0).to([phone(3)]))
        .await
        .unwrap();
    world
        .kv
        .put(
            &marker_key_of("j", 1, "phone:16505550001"),
            br#"{"v":2,"state":"superseded"}"#.to_vec(),
            Expiry::Never,
        )
        .await
        .unwrap();
    let outbound = Fake::default();
    let report = world
        .runner(jobs.clone(), &outbound, "r")
        .tick()
        .await
        .unwrap();
    assert_eq!(report.runs.len(), 2, "{report:?}");
    let summary = summary_of(&report.runs[0].ended);
    assert_eq!((summary.sent, summary.unknown.clone()), (1, vec![0]));
    assert!(matches!(report.runs[1].ended, JobEnd::Done(_)));
    assert_eq!(outbound.attempts(), labels(&[phone(2), phone(3)]));
}

/// A job of many recipients on a slow store (a second a read): reading
/// their markers takes longer than the lease, which the runner renews as
/// it reads, so another runner ticking meanwhile leaves the job alone.
/// Without that renewal the second runner takes the job over and sends to
/// everyone while the first is still reading (the first's pass wasted;
/// with more replicas, the job can be taken over and over).
#[tokio::test]
async fn a_long_marker_scan_keeps_the_lease() {
    let world = World::new();
    let gate = Gate::new(world.kv.clone());
    let jobs = world.jobs_on(gate.clone());
    let recipients: Vec<Recipient> = (0..200).map(phone).collect();
    jobs.schedule(Job::new(id("j"), number(), T0).to(recipients.clone()))
        .await
        .unwrap();
    let outbound = Fake::default();
    let a = world.runner(jobs.clone(), &outbound, "replica-a");
    let b = Arc::new(world.runner(world.jobs(), &outbound, "replica-b"));
    let reports = Reports::default();
    let reads = Arc::new(AtomicUsize::new(0));
    let (clock, runner, kept) = (world.clock.clone(), b.clone(), reports.clone());
    gate.on_get(move |key| {
        let n = (key.namespace() == JOBS_SENT_NAMESPACE)
            .then(|| reads.fetch_add(1, Ordering::SeqCst) + 1);
        if n.is_some() {
            clock.advance(Duration::from_secs(1));
        }
        if n == Some(150) {
            tick_into(&runner, &kept)
        } else {
            Box::pin(async {})
        }
    });

    let ta = a.tick().await.unwrap();
    let tb = reports.lock().unwrap().pop().unwrap();
    assert!(tb.runs.is_empty(), "b took the job over: {tb:?}");
    assert_eq!(summary_of(&ta.runs[0].ended).sent, 200);
    assert_eq!(outbound.attempts(), labels(&recipients));
}

/// Sends that take long (50 s each, here): the runner renews its lease
/// before a send once half of it is gone, so another runner ticking past
/// the first lease's end leaves the job alone. Without the renewal, the
/// second runner takes it over mid-broadcast.
#[tokio::test]
async fn a_runner_renews_its_lease_between_sends() {
    let world = World::new();
    let gate = Gate::new(world.kv.clone());
    let jobs = world.jobs_on(gate.clone());
    let recipients: Vec<Recipient> = (1..=4).map(phone).collect();
    jobs.schedule(Job::new(id("j"), number(), T0).to(recipients.clone()))
        .await
        .unwrap();
    let outbound = Fake::default();
    let a = world.runner(jobs.clone(), &outbound, "replica-a");
    let b = Arc::new(world.runner(world.jobs(), &outbound, "replica-b"));
    let reports = Reports::default();
    let markers = Arc::new(AtomicUsize::new(0));
    let (clock, runner, kept) = (world.clock.clone(), b.clone(), reports.clone());
    gate.on_put_if_absent(move |key| {
        if key.namespace() != JOBS_SENT_NAMESPACE {
            return Box::pin(async {});
        }
        clock.advance(Duration::from_secs(50));
        // The fourth send starts 200 s after the claim, past its first
        // lease (120 s), within the one renewed at the third (100 s).
        if markers.fetch_add(1, Ordering::SeqCst) + 1 == 4 {
            tick_into(&runner, &kept)
        } else {
            Box::pin(async {})
        }
    });

    let ta = a.tick().await.unwrap();
    let tb = reports.lock().unwrap().pop().unwrap();
    assert!(tb.runs.is_empty(), "b took the job over: {tb:?}");
    assert_eq!(summary_of(&ta.runs[0].ended).sent, 4);
    assert_eq!(outbound.attempts(), labels(&recipients));
}

/// A cancel racing a claim, both reading the job at one version. The
/// cancel's compare-and-swap first: the claim fails, nothing is sent. The
/// claim's first: the cancel, its compare-and-swap failing on the version
/// it read, reads the job again and cancels it while the runner is
/// creating its first marker; the runner's check with the marker in place
/// sees it, and nothing is sent. Either way the job ends cancelled.
#[tokio::test]
async fn a_cancel_racing_a_claim_sends_nothing() {
    for claim_first in [true, false] {
        let world = World::new();
        let gate = Gate::new(world.kv.clone());
        let jobs = world.jobs_on(gate.clone());
        jobs.schedule(Job::new(id("j"), number(), T0).to([phone(1)]))
            .await
            .unwrap();
        let outbound = Fake::default();
        let runner = world.runner(jobs.clone(), &outbound, "r");
        // The runner's first marker waits for the cancel to be done.
        let done = Arc::new(Notify::new());
        let waited = done.clone();
        gate.on_put_if_absent(once_in(
            JOBS_SENT_NAMESPACE,
            &world.clock,
            Duration::ZERO,
            move || {
                let waited = waited.clone();
                Box::pin(async move { waited.notified().await })
            },
        ));
        gate.arm();
        let j = id("j");
        let cancel = async {
            let cancelled = jobs.cancel(&j).await;
            done.notify_one();
            cancelled
        };
        // The one polled first waits at the gate; the second goes on first.
        let (cancelled, report) = if claim_first {
            tokio::join!(cancel, runner.tick())
        } else {
            let (report, cancelled) = tokio::join!(runner.tick(), cancel);
            (cancelled, report)
        };
        assert!(cancelled.unwrap(), "claim first: {claim_first}");
        let report = report.unwrap();
        if claim_first {
            assert!(
                matches!(report.runs[..], [ref run] if run.ended == JobEnd::Cancelled),
                "{report:?}"
            );
        } else {
            assert!(report.runs.is_empty(), "{report:?}");
        }
        assert!(outbound.attempts().is_empty(), "claim first: {claim_first}");
        assert_eq!(
            jobs.get(&id("j")).await.unwrap().unwrap().state,
            JobState::Cancelled
        );
    }
}

/// The `131049` skip is the job's: another job to the same person, in the
/// same 24 hours, sends to them.
#[tokio::test]
async fn a_131049_skip_is_scoped_to_its_job() {
    let world = World::new();
    let jobs = world.jobs();
    jobs.schedule(Job::new(id("weekly"), number(), T0).to([user(1)]))
        .await
        .unwrap();
    let outbound = Fake::default();
    let runner = world.runner(jobs.clone(), &outbound, "r");
    runner.tick().await.unwrap();
    assert!(
        jobs.observe(&marketing_limited(&outbound.message_id(&user(1))))
            .await
            .unwrap()
    );
    jobs.schedule(Job::new(id("order-1042"), number(), world.clock.now()).to([user(1)]))
        .await
        .unwrap();
    assert!(jobs.rerun(&id("weekly"), world.clock.now()).await.unwrap());
    let report = runner.tick().await.unwrap();
    let by_id: HashMap<_, _> = report
        .runs
        .iter()
        .map(|run| (run.id.as_str().to_owned(), summary_of(&run.ended).clone()))
        .collect();
    assert_eq!(by_id["weekly"].limited, [0]);
    assert_eq!(by_id["order-1042"].sent, 1);
    assert!(by_id["order-1042"].limited.is_empty());
    assert_eq!(outbound.attempts(), labels(&[user(1), user(1)]));
}
