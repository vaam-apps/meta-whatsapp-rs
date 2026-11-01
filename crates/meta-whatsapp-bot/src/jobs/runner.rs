//! [`JobRunner`]: claims the jobs due, sends them through a paced
//! broadcast, and records each recipient (see the [module docs](super)).

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::{Either, select};
use meta_whatsapp_client::messages::{OutboundMessage, SendResponse};
use meta_whatsapp_client::{Client, RetryPolicy};
use meta_whatsapp_core::clock::Clock;
use meta_whatsapp_core::error::{ConfigError, ValidationError};
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::{Error, Result};
use time::OffsetDateTime;

use super::port::{ClaimHandler, Holding, JobClaim, JobHalt, JobStore, RecipientMark, SendRecord};
use super::store::duration_ms;
use super::{
    JobId, JobState, JobSummary, ScheduledJob, instant, is_marketing_limit, millis, recipient_key,
};
use crate::broadcast::{
    Backoff, Broadcast, BroadcastHandle, BroadcastPolicy, RecipientReport, ReportSink,
};
use crate::outbound::{ClientOutbound, Outbound};
use crate::pacer::{Pacer, after};

/// Makes one recipient's message from its job ([`JobRunnerBuilder::compose`]).
type Composer = dyn Fn(&ScheduledJob, &Recipient) -> Result<OutboundMessage> + Send + Sync;

/// What one tick did ([`JobRunner::tick`]): one line per job it claimed.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct TickReport {
    /// The jobs this tick claimed and ran, in the order it met them.
    pub runs: Vec<JobRun>,
}

/// One job a tick claimed and ran.
#[derive(Debug)]
#[non_exhaustive]
pub struct JobRun {
    /// The job.
    pub id: JobId,
    /// Its run ([`ScheduledJob::run`]).
    pub run: u32,
    /// How the pass ended.
    pub ended: JobEnd,
}

/// How a runner's pass over a job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobEnd {
    /// Settled: every recipient was sent to, refused, skipped, or left with
    /// an outcome unknown.
    Done(JobSummary),
    /// Recipients provably unsent after the last retry
    /// ([`JobRunnerBuilder::retries`]): given up on.
    Stopped(JobSummary),
    /// Recipients provably unsent: the job is due again at `at`, and only
    /// they are sent to then.
    Retrying {
        /// When it is due again.
        at: OffsetDateTime,
        /// What this pass found.
        summary: JobSummary,
    },
    /// The job was cancelled while it ran: nothing more was sent.
    Cancelled,
    /// Another runner claimed the job after this one's lease expired:
    /// nothing more was sent by this one.
    LeaseLost,
}

/// Claims due jobs from a [`JobStore`] and sends them. Build one per
/// process ([`Self::builder`]); any number of runners share one store.
/// See the [module docs](super).
pub struct JobRunner {
    jobs: Arc<dyn JobStore>,
    clock: Arc<dyn Clock>,
    outbound: Arc<dyn Outbound>,
    pacer: Pacer,
    compose: Arc<Composer>,
    policy: Arc<dyn BroadcastPolicy>,
    concurrency: usize,
    lease: Duration,
    max_retries: u32,
    retry_delay: Duration,
    max_retry_delay: Duration,
    id: String,
}

impl fmt::Debug for JobRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobRunner")
            .field("id", &self.id)
            .field("jobs", &self.jobs)
            .field("lease", &self.lease)
            .field("concurrency", &self.concurrency)
            .finish_non_exhaustive()
    }
}

/// A tick's handler of the jobs the store claims: runs each, and keeps
/// its line.
struct Tick<'a> {
    runner: &'a JobRunner,
    report: TickReport,
}

#[async_trait]
impl ClaimHandler for Tick<'_> {
    async fn run(&mut self, claim: JobClaim) -> Result<JobEnd> {
        let (id, run) = (claim.job.id.clone(), claim.job.run);
        tracing::debug!(run, claim = claim.number, "job claimed");
        let ended = self.runner.execute(claim).await?;
        self.report.runs.push(JobRun {
            id,
            run,
            ended: ended.clone(),
        });
        Ok(ended)
    }
}

impl JobRunner {
    /// Start building a runner of `jobs` ([`KvJobs`](super::KvJobs),
    /// [`MemoryJobs`](super::MemoryJobs), or a [`JobStore`] of yours).
    pub fn builder(jobs: impl JobStore) -> JobRunnerBuilder {
        JobRunnerBuilder::new(Arc::new(jobs))
    }

    /// Start building a runner of a store shared with other code.
    pub fn builder_shared(jobs: Arc<dyn JobStore>) -> JobRunnerBuilder {
        JobRunnerBuilder::new(jobs)
    }

    /// This runner's id, written into the leases it takes.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Claim and run every job due ([`JobStore::claim_due`]: for
    /// [`KvJobs`](super::KvJobs), from the oldest minute that may still
    /// hold one up to now), one job after the other; each job's sends run
    /// [`JobRunnerBuilder::concurrency`] at a time. A store error ends the
    /// tick (a job claimed stays claimed until its lease expires, then any
    /// runner resumes it). Dropping the future mid-send leaves that
    /// recipient's outcome unknown: stop with [`Self::run_until`] instead.
    pub async fn tick(&self) -> Result<TickReport> {
        let mut tick = Tick {
            runner: self,
            report: TickReport::default(),
        };
        self.jobs.claim_due(&self.id, self.lease, &mut tick).await?;
        Ok(tick.report)
    }

    /// [`Self::tick`], then wait `every` on the pacer's timer, until `stop`
    /// completes (checked between ticks only: a tick is never abandoned
    /// mid-send). A tick's error is logged and the next tick tries again.
    pub async fn run_until(&self, every: Duration, stop: impl Future<Output = ()> + Send) {
        let mut stop = pin!(stop);
        loop {
            match self.tick().await {
                Ok(report) if !report.runs.is_empty() => {
                    tracing::debug!(jobs = report.runs.len(), "jobs tick ran jobs");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        error_kind = ?error.kind(),
                        "jobs tick failed; the next one tries again"
                    );
                }
            }
            let sleep = pin!(self.pacer.sleep(every));
            if let Either::Right(_) = select(sleep, stop.as_mut()).await {
                return;
            }
        }
    }

    /// One pass over a job this runner just claimed.
    async fn execute(&self, claim: JobClaim) -> Result<JobEnd> {
        let claim = Arc::new(claim);
        let mut summary = JobSummary::default();
        let mut pending = Vec::new();
        let mut by_key = HashMap::new();
        let holder = Holder {
            jobs: Arc::clone(&self.jobs),
            clock: Arc::clone(&self.clock),
            claim: Arc::clone(&claim),
            lease: self.lease,
            until_ms: AtomicI64::new(millis(claim.until)),
        };
        for (index, recipient) in claim.job.recipients.iter().enumerate() {
            // Reading many markers can outlast the lease: renewed here
            // too, so another runner does not take the job over meanwhile.
            match holder.keep().await? {
                Some(JobHalt::Cancelled) => return Ok(JobEnd::Cancelled),
                Some(JobHalt::Lost) => return Ok(JobEnd::LeaseLost),
                None => {}
            }
            match self.jobs.recipient(&claim, index).await? {
                RecipientMark::Sent => summary.sent += 1,
                RecipientMark::Failed => summary.failed += 1,
                RecipientMark::Limited => summary.limited.push(index),
                RecipientMark::Unmarked => {
                    pending.push(index);
                    by_key.insert(recipient_key(recipient).unwrap_or_default(), index);
                }
                RecipientMark::Unknown => summary.unknown.push(index),
            }
        }
        if !pending.is_empty() {
            let halt = self.send(holder, &pending, by_key, &mut summary).await?;
            match halt {
                Some(JobHalt::Cancelled) => return Ok(JobEnd::Cancelled),
                Some(JobHalt::Lost) => return Ok(JobEnd::LeaseLost),
                None => {}
            }
        }
        summary.unknown.sort_unstable();
        if summary.unsent == 0 {
            return self.settle(&claim, JobState::Done, summary).await;
        }
        if claim.job.retries >= self.max_retries {
            return self.settle(&claim, JobState::Stopped, summary).await;
        }
        let delay = self
            .retry_delay
            .saturating_mul(2u32.saturating_pow(claim.job.retries))
            .min(self.max_retry_delay);
        let at = after(self.clock.now(), delay);
        self.reschedule(&claim, at, summary).await
    }

    /// Send to the recipients at `pending` as a paced broadcast, and count
    /// what became of each into `summary`. Why the broadcast was halted,
    /// if it was.
    async fn send(
        &self,
        holder: Holder,
        pending: &[usize],
        by_key: HashMap<String, usize>,
        summary: &mut JobSummary,
    ) -> Result<Option<JobHalt>> {
        let claim = Arc::clone(&holder.claim);
        let outcomes: Arc<Mutex<HashMap<usize, Outcome>>> = Arc::default();
        let by_key = Arc::new(by_key);
        let durable = Arc::new(Durable {
            holder,
            inner: Arc::clone(&self.outbound),
            by_key: Arc::clone(&by_key),
            outcomes: Arc::clone(&outcomes),
            handle: OnceLock::new(),
            halt: Mutex::new(None),
        });
        let compose = Arc::clone(&self.compose);
        let composed = Arc::clone(&claim);
        let failed = Arc::clone(&outcomes);
        let broadcast = Broadcast::builder(claim.job.from.clone())
            .to(pending
                .iter()
                .map(|&index| claim.job.recipients[index].clone()))
            .compose(move |recipient: &Recipient| {
                let message = compose(&composed.job, recipient);
                if message.is_err()
                    && let Some(index) = recipient_key(recipient).and_then(|k| by_key.get(&k))
                {
                    // Composed again at a retry, failing again: never sent.
                    lock(&failed).insert(*index, Outcome::Failed);
                }
                message
            })
            .shared_outbound(Arc::clone(&durable) as Arc<dyn Outbound>)
            .pacer(self.pacer.clone())
            .policy(Arc::clone(&self.policy))
            .concurrency(self.concurrency)
            .report_to(Discard)
            .build()?;
        // Set before the first send: the durable outbound cancels the run
        // through it.
        let _ = durable.handle.set(broadcast.handle());
        let report = broadcast.run().await;
        tracing::debug!(ended = ?report.ended, "job pass ended");
        let outcomes = std::mem::take(&mut *lock(&outcomes));
        for &index in pending {
            match outcomes.get(&index) {
                Some(Outcome::Sent) => summary.sent += 1,
                Some(Outcome::Failed) => summary.failed += 1,
                Some(Outcome::Unknown) => summary.unknown.push(index),
                None => summary.unsent += 1,
            }
        }
        Ok(*lock(&durable.halt))
    }

    /// Settle the job this runner holds as `state` (done or stopped).
    async fn settle(
        &self,
        claim: &JobClaim,
        state: JobState,
        summary: JobSummary,
    ) -> Result<JobEnd> {
        match self.jobs.settle(claim, state, &summary).await? {
            Some(JobHalt::Cancelled) => Ok(JobEnd::Cancelled),
            Some(JobHalt::Lost) => Ok(JobEnd::LeaseLost),
            None => {
                if !summary.unknown.is_empty() {
                    tracing::warn!(
                        unknown = summary.unknown.len(),
                        "job settled with sends whose outcome is unknown: never resent, \
                         reconcile them with the status webhooks"
                    );
                }
                Ok(if state == JobState::Done {
                    JobEnd::Done(summary)
                } else {
                    JobEnd::Stopped(summary)
                })
            }
        }
    }

    /// Hand the job back, pending at `at`, one retry more.
    async fn reschedule(
        &self,
        claim: &JobClaim,
        at: OffsetDateTime,
        summary: JobSummary,
    ) -> Result<JobEnd> {
        // Whole milliseconds, as every store keeps at least.
        let at = instant(millis(at));
        match self.jobs.retry(claim, at, &summary).await? {
            Some(JobHalt::Cancelled) => Ok(JobEnd::Cancelled),
            Some(JobHalt::Lost) => Ok(JobEnd::LeaseLost),
            None => {
                tracing::debug!(
                    retries = claim.job.retries.saturating_add(1),
                    unsent = summary.unsent,
                    "job retried later"
                );
                Ok(JobEnd::Retrying { at, summary })
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The claim a runner holds on a job, and its account of the lease.
#[derive(Debug)]
struct Holder {
    jobs: Arc<dyn JobStore>,
    clock: Arc<dyn Clock>,
    claim: Arc<JobClaim>,
    lease: Duration,
    /// Until when the lease runs, as this runner last wrote or read it
    /// (unix milliseconds).
    until_ms: AtomicI64,
}

impl Holder {
    /// Whether the job, read now, is still this runner's under this claim
    /// and not cancelled (`None`), or why not; the lease is renewed when
    /// less than half of it is left ([`JobStore::hold`]).
    async fn hold(&self) -> Result<Option<JobHalt>> {
        Ok(match self.jobs.hold(&self.claim, self.lease).await? {
            Holding::Held(until) => {
                self.until_ms.store(millis(until), Ordering::Relaxed);
                None
            }
            Holding::Halted(halt) => Some(halt),
        })
    }

    /// [`Self::hold`], only once less than half of the lease is
    /// left by this runner's last account of it: no read before that.
    async fn keep(&self) -> Result<Option<JobHalt>> {
        let now = millis(self.clock.now());
        let left = self.until_ms.load(Ordering::Relaxed).saturating_sub(now);
        if left >= duration_ms(self.lease) / 2 {
            return Ok(None);
        }
        self.hold().await
    }
}

/// What became of a recipient this pass sent to (or tried to).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Sent,
    Failed,
    Unknown,
}

/// The outbound a pass's broadcast sends through: before each request, a
/// check that the job is still this runner's and not cancelled, then the
/// recipient's marker; after it, the marker's outcome.
struct Durable {
    holder: Holder,
    inner: Arc<dyn Outbound>,
    by_key: Arc<HashMap<String, usize>>,
    outcomes: Arc<Mutex<HashMap<usize, Outcome>>>,
    handle: OnceLock<BroadcastHandle>,
    halt: Mutex<Option<JobHalt>>,
}

impl fmt::Debug for Durable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Durable")
            .field("runner", &self.holder.claim.runner)
            .field("run", &self.holder.claim.job.run)
            .finish_non_exhaustive()
    }
}

fn halted(why: JobHalt) -> Error {
    ValidationError::new(
        "job",
        match why {
            JobHalt::Cancelled => "the job was cancelled: not sent",
            JobHalt::Lost => "another runner holds the job: not sent",
        },
    )
    .into()
}

impl Durable {
    fn record(&self, index: usize, outcome: Outcome) {
        lock(&self.outcomes).insert(index, outcome);
    }

    /// Stop the pass for `why`: no send starts after this one.
    fn stop(&self, why: JobHalt) -> Error {
        lock(&self.halt).get_or_insert(why);
        if let Some(handle) = self.handle.get() {
            handle.cancel();
        }
        halted(why)
    }

    /// The job is still this runner's, and not cancelled; the lease is
    /// renewed when less than half of it is left.
    async fn check(&self) -> Result<()> {
        if let Some(why) = *lock(&self.halt) {
            return Err(halted(why));
        }
        match self.holder.hold().await? {
            Some(why) => Err(self.stop(why)),
            None => Ok(()),
        }
    }

    /// Remove the marker of a recipient nothing was sent to; one that
    /// stays is an outcome unknown.
    async fn unmark(&self, index: usize, why: &str) {
        let jobs = &self.holder.jobs;
        if !matches!(jobs.unmark(&self.holder.claim, index).await, Ok(true)) {
            tracing::warn!(
                why,
                "a job's unsent recipient could not be unmarked: never resent"
            );
            self.record(index, Outcome::Unknown);
        }
    }

    async fn sent(&self, index: usize, response: &SendResponse) {
        let message_id = response.message_id().cloned();
        let recorded = self
            .holder
            .jobs
            .mark_outcome(&self.holder.claim, index, SendRecord::Sent { message_id })
            .await;
        if !matches!(recorded, Ok(true)) {
            tracing::warn!(
                "a job's sent message could not be recorded: its marker says sending, read as \
                 an outcome unknown (never resent)"
            );
        }
        self.record(index, Outcome::Sent);
    }

    async fn refused(&self, index: usize, error: &Error) {
        let jobs = &self.holder.jobs;
        let unknown = error.may_have_been_sent();
        let code = error.graph().map(|graph| graph.code);
        let outcome = if unknown {
            SendRecord::Unknown { code }
        } else {
            SendRecord::Failed { code }
        };
        if !matches!(
            jobs.mark_outcome(&self.holder.claim, index, outcome).await,
            Ok(true)
        ) {
            tracing::warn!(
                "a job's failed send could not be recorded: its marker says sending, read as an \
                 outcome unknown (never resent)"
            );
        }
        if is_marketing_limit(error.kind())
            && jobs
                .limit_recipient(&self.holder.claim, index)
                .await
                .is_err()
        {
            tracing::warn!("a job's 131049 could not be recorded");
        }
        self.record(
            index,
            if unknown {
                Outcome::Unknown
            } else {
                Outcome::Failed
            },
        );
    }
}

#[async_trait]
impl Outbound for Durable {
    async fn send(&self, from: &PhoneNumberId, message: &OutboundMessage) -> Result<SendResponse> {
        let rkey = recipient_key(&message.recipient).unwrap_or_default();
        let Some(&index) = self.by_key.get(&rkey) else {
            return Err(ValidationError::new("recipient", "not a recipient of this job").into());
        };
        // Fail closed: no request unless the job is still this runner's.
        self.check().await?;
        let jobs = &self.holder.jobs;
        let claim = &self.holder.claim;
        // Created before the request: whoever meets it without an outcome
        // reads it as unknown, and never sends again.
        if !jobs.mark_sending(claim, index).await? {
            self.record(index, Outcome::Unknown);
            return Err(ValidationError::new(
                "recipient",
                "another runner holds this recipient: not sent",
            )
            .into());
        }
        // Checked again with the marker in place. A cancel, or a re-run,
        // that landed between the first check and the marker gets no
        // request from this runner: a re-run's markers are its own, so
        // this one would not stop its send. Another claim of this run
        // does not stop it: that runner meets this marker.
        let verdict = jobs.check_run(claim).await;
        if !matches!(verdict, Ok(None)) {
            // Not sent: the marker goes, so the recipient is not left as
            // an outcome unknown (unless it cannot be removed).
            self.unmark(index, "the job changed before the send").await;
            return Err(match verdict {
                Ok(Some(why)) => self.stop(why),
                Err(error) => error,
                Ok(None) => halted(JobHalt::Lost),
            });
        }
        match self.inner.send(from, message).await {
            Ok(response) => {
                self.sent(index, &response).await;
                Ok(response)
            }
            Err(error) if error.may_resend() => {
                // Meta refused it before doing anything: without a marker,
                // it is sent again (the broadcast's retry, or the job's).
                self.unmark(index, "Meta refused the send before doing anything")
                    .await;
                Err(error)
            }
            Err(error) => {
                self.refused(index, &error).await;
                Err(error)
            }
        }
    }

    async fn mark_read(
        &self,
        from: &PhoneNumberId,
        message_id: &MessageId,
        typing_indicator: bool,
    ) -> Result<()> {
        self.inner
            .mark_read(from, message_id, typing_indicator)
            .await
    }
}

/// The pass's broadcast keeps no lines: the markers are the record.
#[derive(Debug)]
struct Discard;

#[async_trait]
impl ReportSink for Discard {
    async fn record(&self, _: RecipientReport) -> Result<()> {
        Ok(())
    }
}

/// Distinguishes the runners of one process that are given no id.
static RUNNERS: AtomicU64 = AtomicU64::new(0);

/// Builds a [`JobRunner`]. Required: an outbound ([`Self::client`] or
/// [`Self::outbound`]), a [`Pacer`] and a composer ([`Self::compose`]).
#[must_use]
pub struct JobRunnerBuilder {
    jobs: Arc<dyn JobStore>,
    outbound: Option<Arc<dyn Outbound>>,
    pacer: Option<Pacer>,
    compose: Option<Arc<Composer>>,
    policy: Arc<dyn BroadcastPolicy>,
    concurrency: usize,
    lease: Duration,
    max_retries: u32,
    retry_delay: Duration,
    max_retry_delay: Duration,
    id: Option<String>,
}

impl fmt::Debug for JobRunnerBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobRunnerBuilder")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl JobRunnerBuilder {
    /// The default lease: 2 minutes.
    pub const DEFAULT_LEASE: Duration = Duration::from_secs(120);

    fn new(jobs: Arc<dyn JobStore>) -> Self {
        Self {
            jobs,
            outbound: None,
            pacer: None,
            compose: None,
            policy: Arc::new(Backoff::default()),
            concurrency: 8,
            lease: Self::DEFAULT_LEASE,
            max_retries: 5,
            retry_delay: Duration::from_secs(30),
            max_retry_delay: Duration::from_mins(30),
            id: None,
        }
    }

    /// Send with `client` ([`ClientOutbound`]), its own replays turned off
    /// (`RetryPolicy::NONE`), as `BroadcastBuilder::client` does.
    pub fn client(self, client: Client) -> Self {
        self.outbound(ClientOutbound::new(client.with_retry(RetryPolicy::NONE)))
    }

    /// Send through `outbound`, under the contract of
    /// `BroadcastBuilder::outbound`: no retries inside a call, and errors
    /// truthful about `Error::may_have_been_sent`.
    pub fn outbound(mut self, outbound: impl Outbound) -> Self {
        self.outbound = Some(Arc::new(outbound));
        self
    }

    /// Send through an outbound shared with other code.
    pub fn shared_outbound(mut self, outbound: Arc<dyn Outbound>) -> Self {
        self.outbound = Some(outbound);
        self
    }

    /// Pace with `pacer`, shared with the number's other senders (over a
    /// `KvRateLimiter`, every replica books from one budget).
    pub fn pacer(mut self, pacer: Pacer) -> Self {
        self.pacer = Some(pacer);
        self
    }

    /// Make each recipient's message from its job (its payload, its id),
    /// when it is sent. An error, or a message to someone else, fails that
    /// recipient without a send.
    pub fn compose<F>(mut self, compose: F) -> Self
    where
        F: Fn(&ScheduledJob, &Recipient) -> Result<OutboundMessage> + Send + Sync + 'static,
    {
        self.compose = Some(Arc::new(compose));
        self
    }

    /// Decide failed sends with `policy` (default `Backoff`), as a
    /// broadcast does: its retries happen within the pass, and only when
    /// `Error::may_resend` holds.
    pub fn policy(mut self, policy: impl BroadcastPolicy) -> Self {
        self.policy = Arc::new(policy);
        self
    }

    /// A job's sends in flight at once, at most (default 8; zero fails
    /// [`Self::build`]).
    pub fn concurrency(mut self, sends: usize) -> Self {
        self.concurrency = sends;
        self
    }

    /// How long a claim holds a job without the runner renewing it
    /// (default [`Self::DEFAULT_LEASE`]; zero fails [`Self::build`]). A
    /// runner renews it before a send when less than half is left; a
    /// runner that stops (a crash) holds it until it expires, then any
    /// runner resumes the job.
    pub fn lease(mut self, lease: Duration) -> Self {
        self.lease = lease;
        self
    }

    /// Retries of a pass that left recipients provably unsent, at most
    /// (default 5), before the job is stopped.
    pub fn retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// The first retry's delay, doubled for each later one, and the longest
    /// (default 30 s and 30 min).
    pub fn retry_delay(mut self, first: Duration, longest: Duration) -> Self {
        self.retry_delay = first;
        self.max_retry_delay = longest;
        self
    }

    /// This runner's id, written into its leases (default: the process id,
    /// the time and a counter). Give each replica its own, its host or pod
    /// name: two runners with one id take each other's claims for their
    /// own.
    pub fn runner_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Check the settings: no outbound, no pacer, no composer, a
    /// concurrency or a lease of zero, or an empty runner id is a
    /// `ConfigError`.
    pub fn build(self) -> Result<JobRunner> {
        let outbound = self.outbound.ok_or_else(|| {
            ConfigError::new("a job runner needs an outbound (`JobRunnerBuilder::client`)")
        })?;
        let pacer = self.pacer.ok_or_else(|| {
            ConfigError::new("a job runner needs a pacer (`JobRunnerBuilder::pacer`)")
        })?;
        let compose = self.compose.ok_or_else(|| {
            ConfigError::new("a job runner needs a composer (`JobRunnerBuilder::compose`)")
        })?;
        if self.concurrency == 0 {
            return Err(ConfigError::new("a job runner concurrency of zero").into());
        }
        if self.lease.is_zero() {
            return Err(ConfigError::new("a job runner lease of zero").into());
        }
        let id = match self.id {
            Some(id) if id.is_empty() => {
                return Err(ConfigError::new("an empty job runner id").into());
            }
            Some(id) => id,
            None => format!(
                "{}-{}-{}",
                std::process::id(),
                self.jobs.clock().now().unix_timestamp_nanos(),
                RUNNERS.fetch_add(1, Ordering::Relaxed)
            ),
        };
        Ok(JobRunner {
            clock: self.jobs.clock(),
            jobs: self.jobs,
            outbound,
            pacer,
            compose,
            policy: self.policy,
            concurrency: self.concurrency,
            lease: self.lease,
            max_retries: self.max_retries,
            retry_delay: self.retry_delay,
            max_retry_delay: self.max_retry_delay,
            id,
        })
    }
}
