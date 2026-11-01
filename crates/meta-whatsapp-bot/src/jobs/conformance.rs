#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::panic,
    clippy::too_many_lines
)] // test helper: panics are the report
//! The executable [`JobStore`] contract: the job suite every store runs.
//!
//! ```ignore
//! meta_whatsapp_bot::jobs::conformance::run(&|clock| {
//!     Arc::new(MyJobs::new(/* empty, reading time from */ Arc::new(clock)))
//!         as Arc<dyn JobStore>
//! })
//! .await;
//! ```
//!
//! Each check makes a store of its own with the function given: **empty**,
//! and reading time from the `ManualClock` it is handed (a `KvJobs` on a
//! `MemoryKvStore::with_clock` of that clock, a `MemoryJobs::new` of it).
//! A store that reads time elsewhere (a database's `now()`) cannot be
//! moved through a lease's expiry here.
//!
//! What it checks is the contract on [`JobStore`]: runners racing one job
//! and many (interleaved at every call they make to the store, so a claim
//! or a marker that is not atomic across an `.await` inside the store
//! sends twice), a runner crashing mid-send and the lease that holds its
//! job, the "sending" marker read as an outcome unknown, the second check
//! of the run with the marker in place, cancels racing claims, lease
//! renewal, retries and their backoff, re-runs, and the `131049` skip;
//! then each method on its own (markers exclusive and their runner's, a
//! claim checked on every write). Runners are this crate's [`JobRunner`],
//! sending through a recording `Outbound`; a decorator around the store
//! interleaves them, or runs another runner at a chosen call.
//!
//! What it cannot check: atomicity between two threads inside one call
//! (a store split across a lock without an `.await` between; run your own
//! stress test on a multi-threaded runtime, as this crate's tests do), and
//! durability across a process (a restart here is a new runner on the
//! same store value).

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::{BoxFuture, Either, join_all, select};
use meta_whatsapp_client::messages::{OutboundMessage, SendResponse};
use meta_whatsapp_core::clock::{Clock, ManualClock};
use meta_whatsapp_core::error::TransportError;
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId, UserId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::{Error, GraphApiError, Result};
use meta_whatsapp_webhooks::{WebhookEvent, WebhookPayload};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use tokio::sync::Notify;

use super::{
    ClaimHandler, Holding, Job, JobClaim, JobEnd, JobHalt, JobId, JobRun, JobRunner,
    JobRunnerBuilder, JobState, JobStore, JobSummary, RecipientMark, ScheduledJob, SendRecord,
    TickReport,
};
use crate::broadcast::Backoff;
use crate::outbound::Outbound;
use crate::pacer::{Pacer, Rate, TokenBucket};

/// Makes the store one check runs on: empty, reading time from the clock
/// it is handed.
pub type NewStore = dyn Fn(ManualClock) -> Arc<dyn JobStore> + Sync;

/// Run the suite, each check on a store of `new_store`'s.
///
/// # Panics
///
/// On any contract violation: this is a test helper.
pub async fn run(new_store: &NewStore) {
    two_runners_racing_one_job_send_it_once(new_store).await;
    runners_racing_many_jobs_send_each_recipient_once(new_store).await;
    a_job_held_by_a_runner_is_not_claimed_by_another(new_store).await;
    a_job_scheduled_for_later_is_sent_by_a_new_runner_when_due(new_store).await;
    a_resumed_broadcast_never_resends_a_recipient_recorded_as_sent(new_store).await;
    a_crashed_runners_lease_holds_the_job_until_it_expires(new_store).await;
    a_131049_status_makes_a_rerun_within_24_hours_skip_that_recipient(new_store).await;
    only_a_131049_status_marks_a_recipient(new_store).await;
    a_131049_refusal_marks_the_recipient_for_the_rerun(new_store).await;
    a_131049_skip_is_scoped_to_its_job(new_store).await;
    a_timed_out_send_is_marked_unknown_and_never_resent(new_store).await;
    retries_back_off_then_the_job_stops(new_store).await;
    a_job_retried_within_a_tick_is_claimed_once_a_tick(new_store).await;
    a_job_cancelled_before_it_is_due_is_never_sent(new_store).await;
    a_job_cancelled_while_it_runs_stops_before_its_next_send(new_store).await;
    a_cancel_racing_a_claim_sends_nothing(new_store).await;
    a_runner_whose_lease_expired_mid_send_never_sends_after_the_new_claim(new_store).await;
    a_send_checked_before_a_cancel_and_a_rerun_is_not_made(new_store).await;
    a_long_marker_scan_keeps_the_lease(new_store).await;
    a_runner_renews_its_lease_between_sends(new_store).await;
    scheduling_is_idempotent_and_keeps_each_person_once(new_store).await;
    a_job_due_long_ago_is_run_at_once(new_store).await;
    markers_are_exclusive_and_their_runners(new_store).await;
    a_claim_is_checked_on_every_write(new_store).await;
}

// ─── The world: a clock, a store, a recording outbound ───────────────────

const T0: OffsetDateTime = datetime!(2026-11-01 12:00 UTC);

/// The business number of Meta's webhook examples.
const NUMBER: &str = "106540352242922";

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

/// A text message of the job's `text`.
#[allow(clippy::unnecessary_wraps)]
fn text(job: &ScheduledJob, recipient: &Recipient) -> Result<OutboundMessage> {
    Ok(OutboundMessage::text(
        recipient.clone(),
        job.payload["text"].as_str().unwrap_or("hello"),
    ))
}

fn summary_of(end: &JobEnd) -> &JobSummary {
    match end {
        JobEnd::Done(summary) | JobEnd::Stopped(summary) | JobEnd::Retrying { summary, .. } => {
            summary
        }
        other => panic!("no summary: {other:?}"),
    }
}

/// A Graph error as the client reports it on a 400.
fn graph(code: i64) -> Error {
    let mut error = GraphApiError::new(code, format!("(#{code})"));
    error.http_status = Some(400);
    error.into()
}

/// A status webhook for `message_id` (Meta's failed-status example,
/// `webhooks/reference/messages/status`), failed with `code`, or as
/// `status` without errors when `code` is `None`.
fn status_event(message_id: &str, status: &str, code: Option<i64>) -> WebhookEvent {
    let mut entry = json!({
        "id": message_id,
        "status": status,
        "timestamp": "1751142888",
        "recipient_id": "16505551234"
    });
    if let Some(code) = code {
        entry["errors"] = json!([{
            "code": code,
            "title": "This message was not delivered to maintain healthy ecosystem engagement.",
            "message": "This message was not delivered to maintain healthy ecosystem engagement.",
            "error_data": {
                "details": "In order to maintain a healthy ecosystem engagement, the message failed to be delivered."
            },
            "href": "/documentation/business-messaging/whatsapp/support/error-codes"
        }]);
    }
    let payload = json!({
        "object": "whatsapp_business_account",
        "entry": [{
            "id": "102290129340398",
            "changes": [{
                "value": {
                    "messaging_product": "whatsapp",
                    "metadata": {
                        "display_phone_number": "15550783881",
                        "phone_number_id": NUMBER
                    },
                    "statuses": [entry]
                },
                "field": "messages"
            }]
        }]
    });
    let mut events = WebhookPayload::from_slice(payload.to_string().as_bytes())
        .unwrap()
        .into_events();
    assert_eq!(events.len(), 1);
    events.remove(0)
}

/// A `131049` status for `message_id`.
fn marketing_limited(message_id: &str) -> WebhookEvent {
    status_event(message_id, "failed", Some(131049))
}

/// A store and the clock it reads.
struct World {
    clock: ManualClock,
    jobs: Arc<dyn JobStore>,
}

impl World {
    fn new(new_store: &NewStore) -> Self {
        let clock = ManualClock::new(T0);
        let jobs = new_store(clock.clone());
        Self { clock, jobs }
    }

    fn pacer(&self) -> Pacer {
        Pacer::new(TokenBucket::new(Rate::per_second(80).unwrap()).adaptive(false))
            .with_timer(self.clock.clone())
    }

    fn builder(&self, jobs: Arc<dyn JobStore>, outbound: &Fake) -> JobRunnerBuilder {
        JobRunner::builder_shared(jobs)
            .outbound(outbound.clone())
            .pacer(self.pacer())
            .compose(text)
            .concurrency(1)
    }

    fn runner(&self, outbound: &Fake, runner_id: &str) -> JobRunner {
        self.runner_on(self.jobs.clone(), outbound, runner_id)
    }

    fn runner_on(&self, jobs: Arc<dyn JobStore>, outbound: &Fake, runner_id: &str) -> JobRunner {
        self.builder(jobs, outbound)
            .runner_id(runner_id)
            .build()
            .unwrap()
    }

    fn advance(&self, by: Duration) {
        self.clock.advance(by);
    }

    fn now(&self) -> OffsetDateTime {
        self.clock.now()
    }

    async fn schedule(&self, job: Job) {
        self.jobs.schedule(job).await.unwrap();
    }

    async fn state(&self, job: &str) -> JobState {
        self.jobs.get(&id(job)).await.unwrap().unwrap().state
    }
}

type Failure = Box<dyn FnOnce() -> Error + Send>;

/// Which send never returns, and who is told it started.
type HangOn = (usize, Arc<Notify>);

/// What to run during a send: the send's number (1-based) and the hook.
type DuringSend = (usize, Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>);

/// An `Outbound` that records every send it is handed (in order, by
/// recipient), answers Meta's example response with a message id of its
/// own per send, fails the sends it is told to, can hang forever on one
/// send (a crash mid-send), and can run a hook during one.
#[derive(Clone, Default)]
struct Fake {
    attempts: Arc<Mutex<Vec<String>>>,
    ids: Arc<Mutex<HashMap<String, Vec<String>>>>,
    failures: Arc<Mutex<HashMap<String, VecDeque<Failure>>>>,
    hang_on: Arc<Mutex<Option<HangOn>>>,
    during: Arc<Mutex<Option<DuringSend>>>,
}

impl fmt::Debug for Fake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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

    /// The `n`-th send never returns; the `Notify` is told when it
    /// starts.
    fn hang_on(&self, n: usize) -> Arc<Notify> {
        let notify = Arc::new(Notify::new());
        *self.hang_on.lock().unwrap() = Some((n, notify.clone()));
        notify
    }

    /// The `n`-th send runs `hook` before it answers.
    fn during(&self, n: usize, hook: impl FnOnce() -> BoxFuture<'static, ()> + Send + 'static) {
        *self.during.lock().unwrap() = Some((n, Box::new(hook)));
    }
}

#[async_trait]
impl Outbound for Fake {
    async fn send(&self, _from: &PhoneNumberId, message: &OutboundMessage) -> Result<SendResponse> {
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
        let hook = {
            let mut during = self.during.lock().unwrap();
            match during.take() {
                Some((at, hook)) if at == n => Some(hook),
                other => {
                    *during = other;
                    None
                }
            }
        };
        if let Some(hook) = hook {
            hook().await;
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

    async fn mark_read(&self, _: &PhoneNumberId, _: &MessageId, _: bool) -> Result<()> {
        Ok(())
    }
}

/// Return `Pending` once: the task's other futures run before this one
/// goes on.
async fn yield_now() {
    let mut yielded = false;
    std::future::poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

/// What a [`Hooked`] runs at a call: given the call's count (1-based).
type CallHook = dyn Fn(usize) -> BoxFuture<'static, ()> + Send + Sync;

/// A store decorator: forwards every call to the store under test,
/// yielding first when `yields` (runners joined on one task interleave at
/// every call), and running a hook before each `mark_sending` and after
/// each `recipient`.
struct Hooked {
    inner: Arc<dyn JobStore>,
    yields: bool,
    before_mark_sending: Mutex<Option<Arc<CallHook>>>,
    after_recipient: Mutex<Option<Arc<CallHook>>>,
    mark_sendings: AtomicUsize,
    recipients: AtomicUsize,
}

impl fmt::Debug for Hooked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hooked")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl Hooked {
    fn new(inner: Arc<dyn JobStore>, yields: bool) -> Arc<Self> {
        Arc::new(Self {
            inner,
            yields,
            before_mark_sending: Mutex::new(None),
            after_recipient: Mutex::new(None),
            mark_sendings: AtomicUsize::new(0),
            recipients: AtomicUsize::new(0),
        })
    }

    fn before_mark_sending(
        &self,
        hook: impl Fn(usize) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) {
        *self.before_mark_sending.lock().unwrap() = Some(Arc::new(hook));
    }

    fn after_recipient(
        &self,
        hook: impl Fn(usize) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) {
        *self.after_recipient.lock().unwrap() = Some(Arc::new(hook));
    }

    async fn enter(&self) {
        if self.yields {
            yield_now().await;
        }
    }
}

/// A hook that runs `then` the first time only.
fn once(
    then: impl FnOnce() -> BoxFuture<'static, ()> + Send + 'static,
) -> impl Fn(usize) -> BoxFuture<'static, ()> + Send + Sync + 'static {
    let then = Mutex::new(Some(then));
    move |_| match then.lock().unwrap().take() {
        Some(then) => then(),
        None => Box::pin(async {}),
    }
}

#[async_trait]
impl JobStore for Hooked {
    fn clock(&self) -> Arc<dyn Clock> {
        self.inner.clock()
    }
    async fn schedule(&self, job: Job) -> Result<()> {
        self.enter().await;
        self.inner.schedule(job).await
    }
    async fn cancel(&self, id: &JobId) -> Result<bool> {
        self.enter().await;
        self.inner.cancel(id).await
    }
    async fn rerun(&self, id: &JobId, at: OffsetDateTime) -> Result<bool> {
        self.enter().await;
        self.inner.rerun(id, at).await
    }
    async fn get(&self, id: &JobId) -> Result<Option<ScheduledJob>> {
        self.enter().await;
        self.inner.get(id).await
    }
    async fn limit_message(&self, message: &MessageId) -> Result<bool> {
        self.enter().await;
        self.inner.limit_message(message).await
    }
    async fn observe(&self, event: &WebhookEvent) -> Result<bool> {
        self.enter().await;
        self.inner.observe(event).await
    }
    async fn claim_due(
        &self,
        runner: &str,
        lease: Duration,
        handler: &mut dyn ClaimHandler,
    ) -> Result<()> {
        self.enter().await;
        self.inner.claim_due(runner, lease, handler).await
    }
    async fn hold(&self, claim: &JobClaim, lease: Duration) -> Result<Holding> {
        self.enter().await;
        self.inner.hold(claim, lease).await
    }
    async fn check_run(&self, claim: &JobClaim) -> Result<Option<JobHalt>> {
        self.enter().await;
        self.inner.check_run(claim).await
    }
    async fn recipient(&self, claim: &JobClaim, index: usize) -> Result<RecipientMark> {
        self.enter().await;
        let read = self.inner.recipient(claim, index).await;
        let n = self.recipients.fetch_add(1, Ordering::SeqCst) + 1;
        let hook = self.after_recipient.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(n).await;
        }
        read
    }
    async fn mark_sending(&self, claim: &JobClaim, index: usize) -> Result<bool> {
        self.enter().await;
        let n = self.mark_sendings.fetch_add(1, Ordering::SeqCst) + 1;
        let hook = self.before_mark_sending.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(n).await;
        }
        self.inner.mark_sending(claim, index).await
    }
    async fn mark_outcome(
        &self,
        claim: &JobClaim,
        index: usize,
        outcome: SendRecord,
    ) -> Result<bool> {
        self.enter().await;
        self.inner.mark_outcome(claim, index, outcome).await
    }
    async fn unmark(&self, claim: &JobClaim, index: usize) -> Result<bool> {
        self.enter().await;
        self.inner.unmark(claim, index).await
    }
    async fn limit_recipient(&self, claim: &JobClaim, index: usize) -> Result<()> {
        self.enter().await;
        self.inner.limit_recipient(claim, index).await
    }
    async fn settle(
        &self,
        claim: &JobClaim,
        state: JobState,
        summary: &JobSummary,
    ) -> Result<Option<JobHalt>> {
        self.enter().await;
        self.inner.settle(claim, state, summary).await
    }
    async fn retry(
        &self,
        claim: &JobClaim,
        at: OffsetDateTime,
        summary: &JobSummary,
    ) -> Result<Option<JobHalt>> {
        self.enter().await;
        self.inner.retry(claim, at, summary).await
    }
}

/// The reports of the ticks a hook ran.
type Reports = Arc<Mutex<Vec<TickReport>>>;

/// A hook's tick of `runner`, its report kept in `reports`.
fn tick_into(runner: &Arc<JobRunner>, reports: &Reports) -> BoxFuture<'static, ()> {
    let (runner, reports) = (runner.clone(), reports.clone());
    Box::pin(async move {
        let report = runner.tick().await.unwrap();
        reports.lock().unwrap().push(report);
    })
}

fn last(reports: &Reports) -> TickReport {
    reports.lock().unwrap().pop().expect("the hook ran a tick")
}

/// A handler that keeps the claims it is handed and runs nothing (the
/// job stays claimed, as by a runner that stopped).
#[derive(Default)]
struct Keep(Vec<JobClaim>);

#[async_trait]
impl ClaimHandler for Keep {
    async fn run(&mut self, claim: JobClaim) -> Result<JobEnd> {
        self.0.push(claim);
        Ok(JobEnd::LeaseLost)
    }
}

/// Claim what is due for `runner` under a 2-minute lease, running nothing.
async fn claim_for(world: &World, runner: &str) -> Vec<JobClaim> {
    let mut keep = Keep::default();
    world
        .jobs
        .claim_due(runner, LEASE, &mut keep)
        .await
        .unwrap();
    keep.0
}

const LEASE: Duration = JobRunnerBuilder::DEFAULT_LEASE;

// ─── Claims: atomic, under a lease ───────────────────────────────────────

/// Runners interleaved at every call to the store, all ticking at once:
/// one claims the job, and it is sent once. A claim that reads the job,
/// awaits, then writes it claimed lets two runners claim it: two runs,
/// two sends.
async fn two_runners_racing_one_job_send_it_once(new_store: &NewStore) {
    for runners in [2, 3, 5] {
        let world = World::new(new_store);
        world
            .schedule(
                Job::new(id("reminder-1"), number(), T0)
                    .to([user(1)])
                    .payload(json!({"text": "Your table is booked for 8 pm"})),
            )
            .await;
        let outbound = Fake::default();
        let racing = Hooked::new(world.jobs.clone(), true);
        let replicas: Vec<JobRunner> = (0..runners)
            .map(|i| world.runner_on(racing.clone(), &outbound, &format!("replica-{i}")))
            .collect();
        let reports = join_all(replicas.iter().map(JobRunner::tick)).await;
        let runs: Vec<_> = reports
            .into_iter()
            .flat_map(|report| report.unwrap().runs)
            .collect();
        assert_eq!(
            runs.len(),
            1,
            "{runners} runners: one claims the job: {runs:?}"
        );
        assert_eq!(summary_of(&runs[0].ended).sent, 1);
        assert_eq!(outbound.attempts(), labels(&[user(1)]), "{runners} runners");
        assert_eq!(world.state("reminder-1").await, JobState::Done);
        for replica in &replicas {
            assert!(replica.tick().await.unwrap().runs.is_empty());
        }
        assert_eq!(outbound.attempts().len(), 1);
    }
}

/// Many jobs, many recipients, four runners interleaved: every job run
/// once, every recipient sent once.
async fn runners_racing_many_jobs_send_each_recipient_once(new_store: &NewStore) {
    let world = World::new(new_store);
    let mut expected = Vec::new();
    for j in 0..20 {
        let recipients = [phone(2 * j), user(2 * j + 1)];
        expected.extend(labels(&recipients));
        world
            .schedule(Job::new(id(&format!("job-{j}")), number(), T0).to(recipients))
            .await;
    }
    let outbound = Fake::default();
    let racing = Hooked::new(world.jobs.clone(), true);
    let replicas: Vec<JobRunner> = (0..4)
        .map(|i| world.runner_on(racing.clone(), &outbound, &format!("replica-{i}")))
        .collect();
    let reports = join_all(replicas.iter().map(JobRunner::tick)).await;
    let runs: usize = reports.into_iter().map(|r| r.unwrap().runs.len()).sum();
    assert_eq!(runs, 20, "each job run once");
    let mut attempts = outbound.attempts();
    attempts.sort();
    expected.sort();
    assert_eq!(attempts, expected, "each recipient sent once");
}

/// A runner ticking while another runs the job (during its first send)
/// claims nothing; the job is sent once, by the first.
async fn a_job_held_by_a_runner_is_not_claimed_by_another(new_store: &NewStore) {
    let world = World::new(new_store);
    world
        .schedule(Job::new(id("j"), number(), T0).to([phone(1), phone(2)]))
        .await;
    let outbound = Fake::default();
    let a = world.runner(&outbound, "replica-a");
    let b = Arc::new(world.runner(&outbound, "replica-b"));
    let reports = Reports::default();
    let (runner, kept) = (b.clone(), reports.clone());
    outbound.during(1, move || tick_into(&runner, &kept));
    let ta = a.tick().await.unwrap();
    assert!(last(&reports).runs.is_empty(), "b claimed a job a holds");
    assert_eq!(summary_of(&ta.runs[0].ended).sent, 2);
    assert_eq!(outbound.attempts(), labels(&[phone(1), phone(2)]));
}

/// Scheduled for later; the runner that was there is gone; a new runner
/// (a new id) on the store sends it once it is due, and not before.
async fn a_job_scheduled_for_later_is_sent_by_a_new_runner_when_due(new_store: &NewStore) {
    let world = World::new(new_store);
    let outbound = Fake::default();
    world
        .schedule(
            Job::new(id("restock"), number(), T0 + Duration::from_secs(3600))
                .to([phone(1)])
                .payload(json!({"text": "Back in stock"})),
        )
        .await;
    {
        let before = world.runner(&outbound, "before-restart");
        assert!(before.tick().await.unwrap().runs.is_empty());
    }
    let after = world.runner(&outbound, "after-restart");
    world.advance(Duration::from_mins(30));
    assert!(after.tick().await.unwrap().runs.is_empty(), "not due yet");
    assert!(outbound.attempts().is_empty());

    world.advance(Duration::from_mins(30));
    let report = after.tick().await.unwrap();
    assert_eq!(report.runs.len(), 1);
    assert_eq!(report.runs[0].id, id("restock"));
    assert!(matches!(report.runs[0].ended, JobEnd::Done(_)));
    assert_eq!(outbound.attempts(), labels(&[phone(1)]));
    let job = world.jobs.get(&id("restock")).await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Done);
    assert_eq!(job.summary.unwrap().sent, 1);
}

/// Five recipients; the runner crashes during the third send (its future
/// dropped while the request is in flight).
async fn crash_mid_broadcast(new_store: &NewStore) -> (World, Fake, Vec<Recipient>) {
    let world = World::new(new_store);
    let recipients = vec![phone(1), user(2), phone(3), user(4), phone(5)];
    world
        .schedule(
            Job::new(id("spring-sale"), number(), T0)
                .to(recipients.clone())
                .payload(json!({"text": "Spring sale: 20% off"})),
        )
        .await;
    let outbound = Fake::default();
    let crashed = outbound.hang_on(3);
    let a = world.runner(&outbound, "replica-a");
    {
        let tick = pin!(a.tick());
        let crash = pin!(crashed.notified());
        let Either::Right(_) = select(tick, crash).await else {
            panic!("the tick ended before the crash");
        };
    }
    drop(a);
    assert_eq!(outbound.attempts(), labels(&recipients[..3]));
    (world, outbound, recipients)
}

/// The runner that resumes the job skips the recipients recorded as sent
/// and the one whose send was in flight (its "sending" marker: an outcome
/// unknown), and sends the rest. A store that keeps no "sending" marker
/// (or marks after the send) fails this: the third is sent again.
async fn a_resumed_broadcast_never_resends_a_recipient_recorded_as_sent(new_store: &NewStore) {
    let (world, outbound, recipients) = crash_mid_broadcast(new_store).await;
    world.advance(Duration::from_secs(121));
    let b = world.runner(&outbound, "replica-b");
    let report = b.tick().await.unwrap();

    assert_eq!(report.runs.len(), 1);
    let summary = summary_of(&report.runs[0].ended);
    assert_eq!(summary.sent, 4);
    assert_eq!(summary.unknown, [2], "the send in flight is never resent");
    assert_eq!(summary.unsent, 0);
    let mut expected = labels(&recipients[..3]);
    expected.extend(labels(&recipients[3..]));
    assert_eq!(outbound.attempts(), expected);
    assert_eq!(world.state("spring-sale").await, JobState::Done);
}

/// Until the crashed runner's lease expires, nobody else claims the job;
/// after, one does. A store that ignores the lease fails this.
async fn a_crashed_runners_lease_holds_the_job_until_it_expires(new_store: &NewStore) {
    let (world, outbound, _) = crash_mid_broadcast(new_store).await;
    let b = world.runner(&outbound, "replica-b");
    world.advance(Duration::from_secs(119));
    assert!(b.tick().await.unwrap().runs.is_empty());
    assert_eq!(outbound.attempts().len(), 3);
    assert_eq!(world.state("spring-sale").await, JobState::Claimed);
    world.advance(Duration::from_secs(2));
    assert_eq!(b.tick().await.unwrap().runs.len(), 1);
    assert_eq!(outbound.attempts().len(), 5);
}

// ─── 131049 ──────────────────────────────────────────────────────────────

/// A `131049` status for a message the job sent marks its recipient, and a
/// re-run within 24 hours skips them and sends everyone else; after 24
/// hours a re-run sends to them again. A store that ignores the skip
/// fails this.
async fn a_131049_status_makes_a_rerun_within_24_hours_skip_that_recipient(new_store: &NewStore) {
    let world = World::new(new_store);
    let recipients = vec![user(1), phone(2), user(3)];
    world
        .schedule(
            Job::new(id("weekly-deals"), number(), T0)
                .to(recipients.clone())
                .payload(json!({"text": "This week's deals"})),
        )
        .await;
    let outbound = Fake::default();
    let runner = world.runner(&outbound, "replica-a");
    let first = runner.tick().await.unwrap();
    assert_eq!(summary_of(&first.runs[0].ended).sent, 3);

    let limited = outbound.message_id(&recipients[1]);
    assert!(
        world
            .jobs
            .observe(&marketing_limited(&limited))
            .await
            .unwrap()
    );
    assert!(
        !world
            .jobs
            .observe(&marketing_limited("wamid.not-ours"))
            .await
            .unwrap(),
        "a status for a message no job sent is not recorded"
    );

    world.advance(Duration::from_secs(3600));
    assert!(
        world
            .jobs
            .rerun(&id("weekly-deals"), world.now())
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

    // A redelivery does not extend it.
    assert!(
        world
            .jobs
            .observe(&marketing_limited(&limited))
            .await
            .unwrap()
    );
    // Two minutes short of 24 hours after the status, still limited.
    world.advance(Duration::from_mins(22 * 60 + 58));
    assert!(
        world
            .jobs
            .rerun(&id("weekly-deals"), world.now())
            .await
            .unwrap()
    );
    let third = runner.tick().await.unwrap();
    assert_eq!(summary_of(&third.runs[0].ended).limited, [1]);
    assert_eq!(outbound.attempts().len(), 7);

    // 24 hours after the status, the limit is over.
    world.advance(Duration::from_mins(3));
    assert!(
        world
            .jobs
            .rerun(&id("weekly-deals"), world.now())
            .await
            .unwrap()
    );
    let fourth = runner.tick().await.unwrap();
    let summary = summary_of(&fourth.runs[0].ended);
    assert!(summary.limited.is_empty());
    assert_eq!(summary.sent, 3);
    assert_eq!(outbound.attempts().len(), 10);
}

/// Only a status carrying `131049` is recorded: a `delivered` status, or a
/// failure with another code, is not.
async fn only_a_131049_status_marks_a_recipient(new_store: &NewStore) {
    let world = World::new(new_store);
    world
        .schedule(Job::new(id("j"), number(), T0).to([phone(1)]))
        .await;
    let outbound = Fake::default();
    world.runner(&outbound, "r").tick().await.unwrap();
    let sent = outbound.message_id(&phone(1));
    assert!(
        !world
            .jobs
            .observe(&status_event(&sent, "failed", Some(131026)))
            .await
            .unwrap()
    );
    assert!(
        !world
            .jobs
            .observe(&status_event(&sent, "delivered", None))
            .await
            .unwrap()
    );
    assert!(world.jobs.observe(&marketing_limited(&sent)).await.unwrap());
}

/// A `131049` the send itself returns marks the recipient too.
async fn a_131049_refusal_marks_the_recipient_for_the_rerun(new_store: &NewStore) {
    let world = World::new(new_store);
    world
        .schedule(Job::new(id("j"), number(), T0).to([phone(1), phone(2)]))
        .await;
    let outbound = Fake::default();
    outbound.fail(&phone(1), || graph(131049));
    let runner = world.runner(&outbound, "r");
    let first = runner.tick().await.unwrap();
    let summary = summary_of(&first.runs[0].ended);
    assert_eq!((summary.sent, summary.failed), (1, 1));
    assert!(world.jobs.rerun(&id("j"), world.now()).await.unwrap());
    let second = runner.tick().await.unwrap();
    assert_eq!(summary_of(&second.runs[0].ended).limited, [0]);
    assert_eq!(outbound.attempts(), labels(&[phone(1), phone(2), phone(2)]));
}

/// The `131049` skip is the job's: another job to the same person, in the
/// same 24 hours, sends to them.
async fn a_131049_skip_is_scoped_to_its_job(new_store: &NewStore) {
    let world = World::new(new_store);
    world
        .schedule(Job::new(id("weekly"), number(), T0).to([user(1)]))
        .await;
    let outbound = Fake::default();
    let runner = world.runner(&outbound, "r");
    runner.tick().await.unwrap();
    assert!(
        world
            .jobs
            .observe(&marketing_limited(&outbound.message_id(&user(1))))
            .await
            .unwrap()
    );
    world
        .schedule(Job::new(id("order-1042"), number(), world.now()).to([user(1)]))
        .await;
    assert!(world.jobs.rerun(&id("weekly"), world.now()).await.unwrap());
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

// ─── At most once, retries, cancel ───────────────────────────────────────

/// A timed-out send may have gone out: it is marked unknown and never
/// sent again, not even by the job's retry (which a throttled recipient
/// causes here). The retry backs off and is counted.
async fn a_timed_out_send_is_marked_unknown_and_never_resent(new_store: &NewStore) {
    let world = World::new(new_store);
    let recipients = vec![phone(1), phone(2), phone(3)];
    world
        .schedule(Job::new(id("j"), number(), T0).to(recipients.clone()))
        .await;
    let outbound = Fake::default();
    outbound.fail(&phone(1), || TransportError::Timeout.into());
    outbound.fail(&phone(2), || graph(130429));
    let runner = world
        .builder(world.jobs.clone(), &outbound)
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
    let job = world.jobs.get(&id("j")).await.unwrap().unwrap();
    assert_eq!((job.state, job.retries), (JobState::Pending, 1));
    assert_eq!(job.due_at, *at);
    let first_retry = *at - world.now();
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
    let job = world.jobs.get(&id("j")).await.unwrap().unwrap();
    assert_eq!(job.summary.unwrap().unknown, [0]);
}

/// Every pass throttled: the job is retried with a doubling delay, then
/// stopped after its last retry, its recipient provably unsent.
async fn retries_back_off_then_the_job_stops(new_store: &NewStore) {
    let world = World::new(new_store);
    world
        .schedule(Job::new(id("j"), number(), T0).to([phone(1)]))
        .await;
    let outbound = Fake::default();
    for _ in 0..3 {
        outbound.fail(&phone(1), || graph(130429));
    }
    let runner = world
        .builder(world.jobs.clone(), &outbound)
        .policy(Backoff::new().max_attempts(1).unwrap())
        .retries(2)
        .retry_delay(Duration::from_secs(10), Duration::from_secs(15))
        .build()
        .unwrap();
    let mut delays = Vec::new();
    loop {
        let before = world.now();
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
    let job = world.jobs.get(&id("j")).await.unwrap().unwrap();
    assert_eq!((job.state, job.retries), (JobState::Stopped, 2));
    assert_eq!(outbound.attempts().len(), 3);
}

/// A job retried at once (no delay) is due again within the tick that
/// retried it: it is claimed again by the next tick, not the same one.
async fn a_job_retried_within_a_tick_is_claimed_once_a_tick(new_store: &NewStore) {
    let world = World::new(new_store);
    world
        .schedule(Job::new(id("j"), number(), T0).to([phone(1)]))
        .await;
    let outbound = Fake::default();
    for _ in 0..2 {
        outbound.fail(&phone(1), || graph(130429));
    }
    let runner = world
        .builder(world.jobs.clone(), &outbound)
        .policy(Backoff::new().max_attempts(1).unwrap())
        .retry_delay(Duration::ZERO, Duration::ZERO)
        .build()
        .unwrap();
    for pass in 0..2 {
        let report = runner.tick().await.unwrap();
        assert_eq!(report.runs.len(), 1, "pass {pass}: {report:?}");
        assert!(matches!(report.runs[0].ended, JobEnd::Retrying { .. }));
    }
    let report = runner.tick().await.unwrap();
    assert!(matches!(report.runs[..], [ref run] if matches!(run.ended, JobEnd::Done(_))));
    assert_eq!(outbound.attempts().len(), 3);
}

/// Cancelled before it is due: nothing is sent, ever.
async fn a_job_cancelled_before_it_is_due_is_never_sent(new_store: &NewStore) {
    let world = World::new(new_store);
    world
        .schedule(Job::new(id("j"), number(), T0 + Duration::from_secs(60)).to([phone(1)]))
        .await;
    assert!(world.jobs.cancel(&id("j")).await.unwrap());
    assert!(!world.jobs.cancel(&id("j")).await.unwrap(), "once");
    assert!(!world.jobs.cancel(&id("unknown")).await.unwrap());
    world.advance(Duration::from_secs(120));
    let outbound = Fake::default();
    assert!(
        world
            .runner(&outbound, "r")
            .tick()
            .await
            .unwrap()
            .runs
            .is_empty()
    );
    assert!(outbound.attempts().is_empty());
    assert_eq!(world.state("j").await, JobState::Cancelled);
}

/// Cancelled while it runs (during its second send): the send in flight
/// finishes, and the next is never made.
async fn a_job_cancelled_while_it_runs_stops_before_its_next_send(new_store: &NewStore) {
    let world = World::new(new_store);
    let recipients = vec![phone(1), phone(2), phone(3), phone(4)];
    world
        .schedule(Job::new(id("j"), number(), T0).to(recipients.clone()))
        .await;
    let outbound = Fake::default();
    let jobs = world.jobs.clone();
    outbound.during(2, move || {
        Box::pin(async move { assert!(jobs.cancel(&id("j")).await.unwrap()) })
    });
    let runner = world.runner(&outbound, "r");
    let report = runner.tick().await.unwrap();
    assert!(matches!(report.runs[0].ended, JobEnd::Cancelled));
    assert_eq!(outbound.attempts(), labels(&recipients[..2]));
    assert!(runner.tick().await.unwrap().runs.is_empty());
    assert_eq!(world.state("j").await, JobState::Cancelled);
}

/// A cancel racing a claim. The cancel first: nothing is claimed. The
/// claim first: the cancel lands while the runner creates its first
/// marker; the runner's check with the marker in place sees it, and
/// nothing is sent. Either way the job ends cancelled. A `check_run` that
/// does not read the cancel fails this (the recipient is sent).
async fn a_cancel_racing_a_claim_sends_nothing(new_store: &NewStore) {
    for claim_first in [true, false] {
        let world = World::new(new_store);
        world
            .schedule(Job::new(id("j"), number(), T0).to([phone(1)]))
            .await;
        let outbound = Fake::default();
        let hooked = Hooked::new(world.jobs.clone(), false);
        let runner = world.runner_on(hooked.clone(), &outbound, "r");
        // The runner's first marker waits for the cancel to be done.
        let done = Arc::new(Notify::new());
        let waited = done.clone();
        hooked.before_mark_sending(once(move || {
            Box::pin(async move { waited.notified().await })
        }));
        let jobs = hooked.clone();
        let cancel = async {
            let cancelled = jobs.cancel(&id("j")).await;
            done.notify_one();
            cancelled
        };
        let (cancelled, report) = if claim_first {
            let (report, cancelled) = futures::join!(runner.tick(), cancel);
            (cancelled, report)
        } else {
            futures::join!(cancel, runner.tick())
        };
        assert!(cancelled.unwrap(), "claim first: {claim_first}");
        let report = report.unwrap();
        if claim_first {
            // A store that awaits inside its claim may let the cancel in
            // first even so: then nothing is claimed.
            assert!(
                matches!(
                    report.runs[..],
                    [] | [JobRun {
                        ended: JobEnd::Cancelled,
                        ..
                    }]
                ),
                "{report:?}"
            );
        } else {
            assert!(report.runs.is_empty(), "{report:?}");
        }
        assert!(outbound.attempts().is_empty(), "claim first: {claim_first}");
        assert_eq!(world.state("j").await, JobState::Cancelled);
    }
}

/// A runner paused between its check and the recipient's marker for
/// longer than its lease, while another runner claims the job and sends
/// to that recipient: the marker, created by whoever comes first, stops
/// the second send, and the first runner learns it lost the job. A marker
/// created even when one exists fails this (the recipient is sent twice).
async fn a_runner_whose_lease_expired_mid_send_never_sends_after_the_new_claim(
    new_store: &NewStore,
) {
    let world = World::new(new_store);
    world
        .schedule(Job::new(id("j"), number(), T0).to([user(1)]))
        .await;
    let outbound = Fake::default();
    let hooked = Hooked::new(world.jobs.clone(), false);
    let a = world.runner_on(hooked.clone(), &outbound, "replica-a");
    let b = Arc::new(world.runner(&outbound, "replica-b"));
    let reports = Reports::default();
    let (runner, kept, clock) = (b.clone(), reports.clone(), world.clock.clone());
    hooked.before_mark_sending(once(move || {
        clock.advance(Duration::from_secs(121));
        tick_into(&runner, &kept)
    }));

    let ta = a.tick().await.unwrap();
    let tb = last(&reports);
    assert_eq!(outbound.attempts(), labels(&[user(1)]), "sent once, by b");
    assert!(matches!(tb.runs[0].ended, JobEnd::Done(_)), "{tb:?}");
    assert!(matches!(ta.runs[0].ended, JobEnd::LeaseLost), "{ta:?}");
    let job = world.jobs.get(&id("j")).await.unwrap().unwrap();
    assert_eq!(job.summary.unwrap().sent, 1);
}

/// A cancel and a re-run land between a runner's check and its marker;
/// the re-run's runner sends to that recipient. The first runner, whose
/// marker is its old run's (so no marker stops it), checks the run again
/// with its marker in place, finds it changed, and never sends. A
/// `check_run` that does not compare the run fails this.
async fn a_send_checked_before_a_cancel_and_a_rerun_is_not_made(new_store: &NewStore) {
    let world = World::new(new_store);
    world
        .schedule(Job::new(id("j"), number(), T0).to([user(1)]))
        .await;
    let outbound = Fake::default();
    let hooked = Hooked::new(world.jobs.clone(), false);
    let a = world.runner_on(hooked.clone(), &outbound, "replica-a");
    let b = Arc::new(world.runner(&outbound, "replica-b"));
    let reports = Reports::default();
    let (runner, kept, jobs, clock) = (
        b.clone(),
        reports.clone(),
        world.jobs.clone(),
        world.clock.clone(),
    );
    hooked.before_mark_sending(once(move || {
        let tick = tick_into(&runner, &kept);
        Box::pin(async move {
            assert!(jobs.cancel(&id("j")).await.unwrap());
            assert!(jobs.rerun(&id("j"), clock.now()).await.unwrap());
            tick.await;
        })
    }));

    let ta = a.tick().await.unwrap();
    let tb = last(&reports);
    assert_eq!(
        outbound.attempts(),
        labels(&[user(1)]),
        "sent once, by run 2"
    );
    assert_eq!(tb.runs[0].run, 2);
    assert!(matches!(tb.runs[0].ended, JobEnd::Done(_)), "{tb:?}");
    assert!(matches!(ta.runs[0].ended, JobEnd::LeaseLost), "{ta:?}");
}

/// A job of many recipients on a slow store (a second a marker read):
/// reading them takes longer than the lease, which the runner renews as it
/// reads, so another runner ticking meanwhile leaves the job alone. A
/// `hold` that does not extend the lease fails this.
async fn a_long_marker_scan_keeps_the_lease(new_store: &NewStore) {
    let world = World::new(new_store);
    let recipients: Vec<Recipient> = (0..200).map(phone).collect();
    world
        .schedule(Job::new(id("j"), number(), T0).to(recipients.clone()))
        .await;
    let outbound = Fake::default();
    let hooked = Hooked::new(world.jobs.clone(), false);
    let a = world.runner_on(hooked.clone(), &outbound, "replica-a");
    let b = Arc::new(world.runner(&outbound, "replica-b"));
    let reports = Reports::default();
    let (clock, runner, kept) = (world.clock.clone(), b.clone(), reports.clone());
    hooked.after_recipient(move |n| {
        clock.advance(Duration::from_secs(1));
        if n == 150 {
            tick_into(&runner, &kept)
        } else {
            Box::pin(async {})
        }
    });

    let ta = a.tick().await.unwrap();
    let tb = last(&reports);
    assert!(tb.runs.is_empty(), "b took the job over: {tb:?}");
    assert_eq!(summary_of(&ta.runs[0].ended).sent, 200);
    assert_eq!(outbound.attempts(), labels(&recipients));
}

/// Sends that take long (50 s each, here): the runner renews its lease
/// before a send once half of it is gone, so another runner ticking past
/// the first lease's end leaves the job alone.
async fn a_runner_renews_its_lease_between_sends(new_store: &NewStore) {
    let world = World::new(new_store);
    let recipients: Vec<Recipient> = (1..=4).map(phone).collect();
    world
        .schedule(Job::new(id("j"), number(), T0).to(recipients.clone()))
        .await;
    let outbound = Fake::default();
    let hooked = Hooked::new(world.jobs.clone(), false);
    let a = world.runner_on(hooked.clone(), &outbound, "replica-a");
    let b = Arc::new(world.runner(&outbound, "replica-b"));
    let reports = Reports::default();
    let (clock, runner, kept) = (world.clock.clone(), b.clone(), reports.clone());
    hooked.before_mark_sending(move |n| {
        clock.advance(Duration::from_secs(50));
        // The fourth send starts 200 s after the claim, past its first
        // lease (120 s), within the one renewed at the third (100 s).
        if n == 4 {
            tick_into(&runner, &kept)
        } else {
            Box::pin(async {})
        }
    });

    let ta = a.tick().await.unwrap();
    let tb = last(&reports);
    assert!(tb.runs.is_empty(), "b took the job over: {tb:?}");
    assert_eq!(summary_of(&ta.runs[0].ended).sent, 4);
    assert_eq!(outbound.attempts(), labels(&recipients));
}

// ─── Scheduling ──────────────────────────────────────────────────────────

/// Scheduling is idempotent for the same job (a retried call) and refuses
/// another under the same id, or none at all; each person once.
async fn scheduling_is_idempotent_and_keeps_each_person_once(new_store: &NewStore) {
    let world = World::new(new_store);
    let job = Job::new(id("j"), number(), T0)
        .to([phone(1), Recipient::phone("16505550001"), user(2)])
        .payload(json!({"text": "hi"}));
    world.jobs.schedule(job.clone()).await.unwrap();
    world.jobs.schedule(job).await.unwrap();
    let other = Job::new(id("j"), number(), T0).to([phone(9)]);
    assert!(matches!(
        world.jobs.schedule(other).await,
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        world
            .jobs
            .schedule(Job::new(id("empty"), number(), T0))
            .await,
        Err(Error::Validation(_))
    ));
    assert!(world.jobs.get(&id("empty")).await.unwrap().is_none());
    let stored = world.jobs.get(&id("j")).await.unwrap().unwrap();
    assert_eq!(stored.recipients, [phone(1), user(2)]);
    assert_eq!(
        (stored.state, stored.run, stored.retries),
        (JobState::Pending, 1, 0)
    );
    assert_eq!(stored.payload, json!({"text": "hi"}));
    assert_eq!(stored.due_at, T0);
    let outbound = Fake::default();
    let report = world.runner(&outbound, "r").tick().await.unwrap();
    assert_eq!(report.runs.len(), 1);
    assert_eq!(outbound.attempts(), labels(&[phone(1), user(2)]));
}

/// A job due long ago (a month) keeps its due time and runs at the next
/// tick.
async fn a_job_due_long_ago_is_run_at_once(new_store: &NewStore) {
    let world = World::new(new_store);
    let long_ago = T0 - Duration::from_hours(30 * 24);
    world
        .schedule(Job::new(id("j"), number(), long_ago).to([phone(1)]))
        .await;
    assert_eq!(
        world.jobs.get(&id("j")).await.unwrap().unwrap().due_at,
        long_ago,
        "the due time is kept as given"
    );
    let outbound = Fake::default();
    let report = world.runner(&outbound, "r").tick().await.unwrap();
    assert_eq!(report.runs.len(), 1);
    assert!(matches!(report.runs[0].ended, JobEnd::Done(_)));
    assert_eq!(outbound.attempts(), labels(&[phone(1)]));
}

// ─── Each method on its own ──────────────────────────────────────────────

/// A run's marker: created once (of two creations at once, one), read as
/// unknown while "sending", completed or removed only by its runner, and
/// deciding before the `131049` skip; a later run has markers of its own.
async fn markers_are_exclusive_and_their_runners(new_store: &NewStore) {
    let world = World::new(new_store);
    world
        .schedule(Job::new(id("j"), number(), T0).to([phone(1), phone(2)]))
        .await;
    let jobs = world.jobs.clone();
    let claims = claim_for(&world, "a").await;
    let [a] = &claims[..] else {
        panic!("one claim: {claims:?}");
    };
    assert_eq!(a.job.state, JobState::Claimed);
    assert_eq!(a.runner, "a");
    assert_eq!(a.until, T0 + LEASE);
    assert_eq!(jobs.recipient(a, 0).await.unwrap(), RecipientMark::Unmarked);
    assert!(jobs.recipient(a, 2).await.is_err(), "not a recipient");
    assert!(jobs.mark_sending(a, 2).await.is_err(), "not a recipient");

    let racing = Hooked::new(jobs.clone(), true);
    let (first, second) = futures::join!(racing.mark_sending(a, 0), racing.mark_sending(a, 0));
    assert_eq!(
        [first.unwrap(), second.unwrap()]
            .iter()
            .filter(|created| **created)
            .count(),
        1,
        "one of two creations at once"
    );
    assert_eq!(jobs.recipient(a, 0).await.unwrap(), RecipientMark::Unknown);

    // Another runner's claim of the same run: the marker is not its own.
    let b = JobClaim::new(a.job.clone(), "b", a.number + 1, a.until);
    assert!(!jobs.mark_sending(&b, 0).await.unwrap());
    assert!(!jobs.unmark(&b, 0).await.unwrap());
    let sent = SendRecord::Sent {
        message_id: Some(MessageId::new("wamid.job.1")),
    };
    assert!(!jobs.mark_outcome(&b, 0, sent.clone()).await.unwrap());
    assert_eq!(jobs.recipient(a, 0).await.unwrap(), RecipientMark::Unknown);

    assert!(jobs.mark_outcome(a, 0, sent.clone()).await.unwrap());
    assert_eq!(jobs.recipient(a, 0).await.unwrap(), RecipientMark::Sent);
    assert!(
        !jobs.mark_outcome(a, 0, sent).await.unwrap(),
        "no longer sending"
    );
    assert!(!jobs.unmark(a, 0).await.unwrap(), "no longer sending");
    assert_eq!(jobs.recipient(a, 0).await.unwrap(), RecipientMark::Sent);

    assert!(jobs.mark_sending(a, 1).await.unwrap());
    assert!(jobs.unmark(a, 1).await.unwrap());
    assert_eq!(jobs.recipient(a, 1).await.unwrap(), RecipientMark::Unmarked);
    assert!(
        jobs.mark_sending(a, 1).await.unwrap(),
        "removed: free again"
    );
    assert!(
        jobs.mark_outcome(a, 1, SendRecord::Failed { code: Some(131026) })
            .await
            .unwrap()
    );
    assert_eq!(jobs.recipient(a, 1).await.unwrap(), RecipientMark::Failed);

    // The sent message's 131049: its marker still decides for this run.
    assert!(
        jobs.limit_message(&MessageId::new("wamid.job.1"))
            .await
            .unwrap()
    );
    assert!(
        !jobs
            .limit_message(&MessageId::new("wamid.unknown"))
            .await
            .unwrap()
    );
    assert_eq!(jobs.recipient(a, 0).await.unwrap(), RecipientMark::Sent);
    assert_eq!(
        jobs.settle(a, JobState::Done, &JobSummary::default())
            .await
            .unwrap(),
        None
    );
    assert!(jobs.rerun(&id("j"), world.now()).await.unwrap());
    let claims = claim_for(&world, "a").await;
    let [again] = &claims[..] else {
        panic!("one claim: {claims:?}");
    };
    assert_eq!(again.job.run, 2);
    assert!(again.number > a.number, "a claim's number grows");
    assert_eq!(
        jobs.recipient(again, 0).await.unwrap(),
        RecipientMark::Limited
    );
    assert_eq!(
        jobs.recipient(again, 1).await.unwrap(),
        RecipientMark::Unmarked,
        "a later run's markers are its own"
    );
    jobs.limit_recipient(again, 1).await.unwrap();
    assert_eq!(
        jobs.recipient(again, 1).await.unwrap(),
        RecipientMark::Limited
    );
    assert!(
        jobs.mark_sending(again, 1).await.unwrap(),
        "a limited recipient is skipped by the runner, not by the store"
    );
    assert_eq!(
        jobs.recipient(again, 1).await.unwrap(),
        RecipientMark::Unknown
    );
    world.advance(Duration::from_hours(24));
    assert_eq!(
        jobs.recipient(again, 0).await.unwrap(),
        RecipientMark::Unmarked,
        "the skip is over after 24 hours"
    );
}

/// Hold, settle and retry act only for the claim that holds the job, and
/// not on a cancelled one; the lease is extended once half of it is gone;
/// an expired lease is claimed again by another runner, under a higher
/// number.
async fn a_claim_is_checked_on_every_write(new_store: &NewStore) {
    let world = World::new(new_store);
    world
        .schedule(Job::new(id("j"), number(), T0).to([phone(1)]))
        .await;
    let jobs = world.jobs.clone();
    let claims = claim_for(&world, "a").await;
    let [a] = &claims[..] else {
        panic!("one claim: {claims:?}");
    };
    assert!(claim_for(&world, "b").await.is_empty(), "the lease holds");
    assert_eq!(
        jobs.hold(a, LEASE).await.unwrap(),
        Holding::Held(a.until),
        "more than half left: not extended"
    );
    world.advance(Duration::from_secs(61));
    assert_eq!(
        jobs.hold(a, LEASE).await.unwrap(),
        Holding::Held(world.now() + LEASE),
        "less than half left: extended"
    );
    world.advance(Duration::from_secs(61));
    assert!(
        claim_for(&world, "b").await.is_empty(),
        "the extended lease holds"
    );
    let forged = JobClaim::new(a.job.clone(), "b", a.number + 1, a.until);
    assert_eq!(
        jobs.hold(&forged, LEASE).await.unwrap(),
        Holding::Halted(JobHalt::Lost)
    );
    let summary = JobSummary::default();
    assert_eq!(
        jobs.settle(&forged, JobState::Done, &summary)
            .await
            .unwrap(),
        Some(JobHalt::Lost)
    );
    assert_eq!(
        jobs.retry(&forged, world.now(), &summary).await.unwrap(),
        Some(JobHalt::Lost)
    );
    assert!(
        jobs.settle(a, JobState::Pending, &summary).await.is_err(),
        "a job settles as done or stopped"
    );
    assert_eq!(jobs.check_run(a).await.unwrap(), None);

    world.advance(LEASE);
    let claims = claim_for(&world, "b").await;
    let [b] = &claims[..] else {
        panic!("the expired lease is claimed again: {claims:?}");
    };
    assert!(b.number > a.number);
    assert_eq!(
        jobs.hold(a, LEASE).await.unwrap(),
        Holding::Halted(JobHalt::Lost)
    );
    assert_eq!(
        jobs.settle(a, JobState::Done, &summary).await.unwrap(),
        Some(JobHalt::Lost)
    );
    assert_eq!(
        jobs.check_run(a).await.unwrap(),
        None,
        "another claim of the run does not stop a send"
    );
    let retry_at = world.now() + Duration::from_secs(30);
    assert_eq!(jobs.retry(b, retry_at, &summary).await.unwrap(), None);
    let job = jobs.get(&id("j")).await.unwrap().unwrap();
    assert_eq!(
        (job.state, job.retries, job.due_at),
        (JobState::Pending, 1, retry_at)
    );
    assert_eq!(
        jobs.hold(b, LEASE).await.unwrap(),
        Holding::Halted(JobHalt::Lost),
        "handed back"
    );
    assert!(claim_for(&world, "c").await.is_empty(), "not due yet");
    world.advance(Duration::from_secs(30));
    let claims = claim_for(&world, "c").await;
    let [c] = &claims[..] else {
        panic!("due again: {claims:?}");
    };
    assert!(c.number > b.number);
    let done = JobSummary {
        sent: 1,
        ..JobSummary::default()
    };
    assert_eq!(jobs.settle(c, JobState::Done, &done).await.unwrap(), None);
    let job = jobs.get(&id("j")).await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Done);
    assert_eq!(job.summary, Some(done));
    assert!(!jobs.cancel(&id("j")).await.unwrap(), "settled first");
    assert!(
        !jobs.rerun(&id("unknown"), world.now()).await.unwrap(),
        "unknown"
    );
    assert!(jobs.rerun(&id("j"), world.now()).await.unwrap());
    assert!(
        !jobs.rerun(&id("j"), world.now()).await.unwrap(),
        "pending already"
    );
    assert_eq!(
        jobs.check_run(c).await.unwrap(),
        Some(JobHalt::Lost),
        "re-run: another run"
    );

    // Cancelled while claimed: every write of the claim says so.
    let claims = claim_for(&world, "d").await;
    let [d] = &claims[..] else {
        panic!("the re-run is due: {claims:?}");
    };
    assert!(jobs.cancel(&id("j")).await.unwrap());
    assert_eq!(
        jobs.hold(d, LEASE).await.unwrap(),
        Holding::Halted(JobHalt::Cancelled)
    );
    assert_eq!(jobs.check_run(d).await.unwrap(), Some(JobHalt::Cancelled));
    assert_eq!(
        jobs.settle(d, JobState::Done, &summary).await.unwrap(),
        Some(JobHalt::Cancelled)
    );
    assert_eq!(
        jobs.retry(d, world.now(), &summary).await.unwrap(),
        Some(JobHalt::Cancelled)
    );
    world.advance(LEASE);
    assert!(
        claim_for(&world, "e").await.is_empty(),
        "a cancelled job is never claimed"
    );
    assert_eq!(world.state("j").await, JobState::Cancelled);
}
