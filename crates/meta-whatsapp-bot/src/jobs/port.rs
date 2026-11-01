//! [`JobStore`]: what a [`JobRunner`](super::JobRunner) needs from the
//! store its jobs are in, and what an implementation must guarantee.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_core::clock::Clock;
use meta_whatsapp_core::error::SinkError;
use meta_whatsapp_core::ids::MessageId;
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::{Error, Result};
use meta_whatsapp_webhooks::WebhookEvent;
use time::OffsetDateTime;

use super::{Job, JobEnd, JobId, JobState, JobSummary, ScheduledJob, is_marketing_limit};
use crate::command::CommandHandler;
use crate::ctx::Ctx;

/// Where jobs are kept: scheduled, cancelled, re-run and read by your
/// code, claimed, marked and settled by the [`JobRunner`](super::JobRunner)s
/// sharing it, and told of Meta's `131049` refusals.
///
/// [`KvJobs`](super::KvJobs) is the default, on the `KvStore` port;
/// [`MemoryJobs`](super::MemoryJobs) keeps them in this process. Implement
/// it to run jobs from a queue of your own (a table, a broker): the
/// runner reaches the store only through this trait, so whatever passes
/// [`conformance::run`](super::conformance::run) can stand in for either.
/// It is object-safe; the runner holds an `Arc<dyn JobStore>`.
///
/// Errors are the library's: a backend failure is a
/// `StorageError` (`StorageError::Busy` after contention), a refused
/// argument a `ValidationError`, each through `meta_whatsapp_core::Error`,
/// with no backend type in its signature.
///
/// # The contract
///
/// Runners share a store across tasks, processes and hosts, so every
/// guarantee below holds against concurrent callers of every method, in
/// this process or another. **At most once** is what they add up to: no
/// recipient of a run is sent twice, whoever runs it, and a send whose
/// outcome is unknown is never made again.
///
/// - **Time** is [`Self::clock`]'s: a job is due when its due time is at
///   or before `now`, a lease has expired when its end is at or before
///   `now`, and a `131049` skip lasts [`MARKETING_LIMIT_WAIT`](super::MARKETING_LIMIT_WAIT)
///   from when it was recorded. Keep at least millisecond precision.
/// - **Claims are atomic.** [`Self::claim_due`] claims a job in one atomic
///   step (a compare-and-swap, a transaction, a row lock), from pending
///   and due, or claimed under a lease that has expired, to claimed under
///   a new lease: the runner's id, a claim number higher than any earlier
///   claim of the job, and an end `lease` after now. Of concurrent
///   callers, one claims a job and the others do not; nobody claims a
///   job whose lease has not expired, or a settled one. The claim
///   survives the claimer: a runner that stops holds the job until the
///   lease expires, then any runner claims it again.
/// - **A claim is checked, not trusted.** [`Self::hold`],
///   [`Self::settle`] and [`Self::retry`] act only while the job is held
///   by that claim (its runner and number) and not cancelled, atomically
///   against a concurrent claim or cancel, and say why not otherwise.
///   [`Self::check_run`] checks the run, not the claim.
/// - **Markers come before sends.** A run has at most one marker per
///   recipient. [`Self::mark_sending`] creates it, atomically if absent,
///   and is durable when it returns `true`: the runner sends right after,
///   so a marker it did not keep is a recipient sent again. Of concurrent
///   callers, one gets `true`. A marker is never replaced by anyone else's
///   (only the runner that created it records its outcome,
///   [`Self::mark_outcome`], or removes it, [`Self::unmark`]), and a
///   "sending" nobody completed reads as an outcome unknown
///   ([`RecipientMark::Unknown`]): never resent. Markers last as long as
///   the job may be resumed; one of an earlier run never counts for a
///   later one.
/// - **Cancel wins or loses whole.** [`Self::cancel`] and a claim, a
///   settle or a retry of the same job never both take effect over one
///   state: a cancelled job is never claimed, settled or retried, and a
///   job settled first is not cancelled.
/// - **The `131049` skip is per job.** [`Self::limit_message`] and
///   [`Self::limit_recipient`] mark a recipient of one job; another job
///   to the same person is unaffected. Recording it again within the
///   wait does not extend it.
/// - **Each job once a call.** [`Self::claim_due`] hands a job to its
///   handler at most once per call (a job retried within the call is
///   handed over by a later one), and calls the handler without holding
///   any lock other callers wait on: the handler sends for minutes, and
///   other runners, cancels and status webhooks go on meanwhile.
///
/// [`conformance::run`](super::conformance::run) is the executable form
/// of this contract.
#[async_trait]
pub trait JobStore: Send + Sync + fmt::Debug + 'static {
    /// The clock jobs are due by and leases expire by; the runner reads
    /// the same one (its lease renewals, its retries' due times).
    fn clock(&self) -> Arc<dyn Clock>;

    // ── Your code's side ────────────────────────────────────────────────

    /// Store `job` (checked and its recipients kept each person once:
    /// [`Job::into_scheduled`]), pending, run 1, due at its time. A
    /// `ValidationError` for a job [`Job::into_scheduled`] refuses, or
    /// when another job is stored under its id; the same job scheduled
    /// again (a call retried after an error) succeeds and leaves it as it
    /// is.
    async fn schedule(&self, job: Job) -> Result<()>;

    /// Cancel the job `id`: pending or claimed, it becomes cancelled, its
    /// lease dropped (a runner running it stops before its next send).
    /// Whether it was cancelled now; `false` for a job unknown or settled.
    async fn cancel(&self, id: &JobId) -> Result<bool>;

    /// Run the settled job `id` again at `at`: pending, its run one more,
    /// no retries, no summary. Whether it did; `false` for a job unknown,
    /// pending or claimed.
    async fn rerun(&self, id: &JobId, at: OffsetDateTime) -> Result<bool>;

    /// The job `id`, as stored.
    async fn get(&self, id: &JobId) -> Result<Option<ScheduledJob>>;

    /// Meta refused the message `message` with `131049` (its status
    /// webhook said so): mark its recipient, in the job and run it was
    /// sent in ([`Self::mark_outcome`] with its id), limited from now for
    /// [`MARKETING_LIMIT_WAIT`](super::MARKETING_LIMIT_WAIT). Whether it
    /// did; `false` for a message no job sent, or one the store no longer
    /// keeps.
    async fn limit_message(&self, message: &MessageId) -> Result<bool>;

    /// Hear a webhook event: a status whose errors carry `131049` (Meta's
    /// per-user marketing limit) goes to [`Self::limit_message`]. Whether
    /// a recipient was marked; `false` for any other event. Safe to
    /// repeat (a redelivery does not extend the 24 hours).
    async fn observe(&self, event: &WebhookEvent) -> Result<bool> {
        let WebhookEvent::StatusUpdated { status, .. } = event else {
            return Ok(false);
        };
        // Meta: the status is `failed` with the code 131049
        // (`templates/marketing-templates/per-user-limits`); the code
        // decides.
        if !status.errors.iter().any(|e| is_marketing_limit(e.kind())) {
            return Ok(false);
        }
        self.limit_message(&status.id).await
    }

    // ── The runner's side ───────────────────────────────────────────────

    /// Claim the jobs due now, one at a time, for the runner `runner`
    /// under a lease of `lease`, and hand each to `handler` as it is
    /// claimed (its claim's `now` read then, not at the call's start). The
    /// handler's [`JobEnd`] says what became of it: an index of due times
    /// may keep a job that ended [`JobEnd::Retrying`] or
    /// [`JobEnd::LeaseLost`]. A store error, or the handler's, ends the
    /// call with it: a job not claimed is not sent.
    async fn claim_due(
        &self,
        runner: &str,
        lease: Duration,
        handler: &mut dyn ClaimHandler,
    ) -> Result<()>;

    /// Whether the job `claim` holds is still held by it and not
    /// cancelled, read now: [`Holding::Held`] with the lease's end, after
    /// extending it to `lease` from now when less than half of `lease`
    /// was left; [`Holding::Halted`] otherwise (the job gone counts as
    /// lost).
    async fn hold(&self, claim: &JobClaim, lease: Duration) -> Result<Holding>;

    /// Whether the job is still in `claim`'s run and not cancelled, read
    /// now (`None`), or why not: cancelled, or lost when it is gone or in
    /// another run (re-run meanwhile). Another claim of the same run does
    /// not count: that runner meets this runner's markers.
    async fn check_run(&self, claim: &JobClaim) -> Result<Option<JobHalt>>;

    /// What is known of the recipient at `index` (into
    /// `claim.job.recipients`) for `claim`'s run: its marker's outcome
    /// (a "sending" nobody completed, or a marker the store cannot read,
    /// is [`RecipientMark::Unknown`]); without one, whether it is
    /// marketing-limited in this job; else [`RecipientMark::Unmarked`].
    async fn recipient(&self, claim: &JobClaim, index: usize) -> Result<RecipientMark>;

    /// Create the "sending" marker of the recipient at `index` for
    /// `claim`'s run, by `claim`'s runner, if the run has none for them.
    /// Whether it did: `false` when one exists (another runner holds the
    /// recipient, or held it). Durable when it returns `true`.
    async fn mark_sending(&self, claim: &JobClaim, index: usize) -> Result<bool>;

    /// Replace the "sending" marker `claim`'s runner created for the
    /// recipient at `index` with the send's outcome; a sent message's id
    /// is kept for [`Self::limit_message`]. Whether it did: `false` when
    /// the marker is no longer that "sending" (it then reads as an
    /// outcome unknown).
    async fn mark_outcome(
        &self,
        claim: &JobClaim,
        index: usize,
        outcome: SendRecord,
    ) -> Result<bool>;

    /// Remove the "sending" marker `claim`'s runner created for the
    /// recipient at `index`: nothing was sent, so they are sent later.
    /// Whether it did (`false`: the marker stays, read as an outcome
    /// unknown).
    async fn unmark(&self, claim: &JobClaim, index: usize) -> Result<bool>;

    /// Meta refused the send to the recipient at `index` with `131049`:
    /// mark them limited in this job from now, as [`Self::limit_message`]
    /// does.
    async fn limit_recipient(&self, claim: &JobClaim, index: usize) -> Result<()>;

    /// Settle the job `claim` holds as `state` ([`JobState::Done`] or
    /// [`JobState::Stopped`]; any other is a `ValidationError`) with
    /// `summary`, its lease dropped: `None`, or why not.
    async fn settle(
        &self,
        claim: &JobClaim,
        state: JobState,
        summary: &JobSummary,
    ) -> Result<Option<JobHalt>>;

    /// Hand the job `claim` holds back, pending at `at` with one retry
    /// more, its lease dropped, due again then (a later
    /// [`Self::claim_due`] claims it): `None`, or why not. `summary` is
    /// what the pass found, for a store that keeps it (neither of this
    /// crate's does: a retry's recipients are counted again by its pass).
    async fn retry(
        &self,
        claim: &JobClaim,
        at: OffsetDateTime,
        summary: &JobSummary,
    ) -> Result<Option<JobHalt>>;
}

#[async_trait]
impl<T: JobStore + ?Sized> JobStore for Arc<T> {
    fn clock(&self) -> Arc<dyn Clock> {
        (**self).clock()
    }
    async fn schedule(&self, job: Job) -> Result<()> {
        (**self).schedule(job).await
    }
    async fn cancel(&self, id: &JobId) -> Result<bool> {
        (**self).cancel(id).await
    }
    async fn rerun(&self, id: &JobId, at: OffsetDateTime) -> Result<bool> {
        (**self).rerun(id, at).await
    }
    async fn get(&self, id: &JobId) -> Result<Option<ScheduledJob>> {
        (**self).get(id).await
    }
    async fn limit_message(&self, message: &MessageId) -> Result<bool> {
        (**self).limit_message(message).await
    }
    async fn observe(&self, event: &WebhookEvent) -> Result<bool> {
        (**self).observe(event).await
    }
    async fn claim_due(
        &self,
        runner: &str,
        lease: Duration,
        handler: &mut dyn ClaimHandler,
    ) -> Result<()> {
        (**self).claim_due(runner, lease, handler).await
    }
    async fn hold(&self, claim: &JobClaim, lease: Duration) -> Result<Holding> {
        (**self).hold(claim, lease).await
    }
    async fn check_run(&self, claim: &JobClaim) -> Result<Option<JobHalt>> {
        (**self).check_run(claim).await
    }
    async fn recipient(&self, claim: &JobClaim, index: usize) -> Result<RecipientMark> {
        (**self).recipient(claim, index).await
    }
    async fn mark_sending(&self, claim: &JobClaim, index: usize) -> Result<bool> {
        (**self).mark_sending(claim, index).await
    }
    async fn mark_outcome(
        &self,
        claim: &JobClaim,
        index: usize,
        outcome: SendRecord,
    ) -> Result<bool> {
        (**self).mark_outcome(claim, index, outcome).await
    }
    async fn unmark(&self, claim: &JobClaim, index: usize) -> Result<bool> {
        (**self).unmark(claim, index).await
    }
    async fn limit_recipient(&self, claim: &JobClaim, index: usize) -> Result<()> {
        (**self).limit_recipient(claim, index).await
    }
    async fn settle(
        &self,
        claim: &JobClaim,
        state: JobState,
        summary: &JobSummary,
    ) -> Result<Option<JobHalt>> {
        (**self).settle(claim, state, summary).await
    }
    async fn retry(
        &self,
        claim: &JobClaim,
        at: OffsetDateTime,
        summary: &JobSummary,
    ) -> Result<Option<JobHalt>> {
        (**self).retry(claim, at, summary).await
    }
}

/// What [`JobStore::claim_due`] hands each job it claims to: the runner's
/// pass over it.
#[async_trait]
pub trait ClaimHandler: Send {
    /// Run the job `claim` holds; how the pass ended.
    async fn run(&mut self, claim: JobClaim) -> Result<JobEnd>;
}

/// A job a runner claimed ([`JobStore::claim_due`]): the job as claimed
/// and the lease that holds it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct JobClaim {
    /// The job, claimed.
    pub job: ScheduledJob,
    /// The runner's id.
    pub runner: String,
    /// The claim's number: higher than any earlier claim of the job, so a
    /// runner that claims a job again does not take an old claim for its
    /// own.
    pub number: u32,
    /// When the lease ends, as claimed.
    pub until: OffsetDateTime,
}

impl JobClaim {
    /// The claim `number` of `job` by `runner`, until `until`.
    pub fn new(
        job: ScheduledJob,
        runner: impl Into<String>,
        number: u32,
        until: OffsetDateTime,
    ) -> Self {
        Self {
            job,
            runner: runner.into(),
            number,
            until,
        }
    }
}

/// Why a runner stops a job it claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobHalt {
    /// The job was cancelled.
    Cancelled,
    /// Another claim holds the job (this one's lease expired and another
    /// runner claimed it), it was re-run, or it is gone.
    Lost,
}

/// What [`JobStore::hold`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Holding {
    /// Still held by the claim, until then.
    Held(OffsetDateTime),
    /// Not any more.
    Halted(JobHalt),
}

/// What is known of a recipient for a run ([`JobStore::recipient`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecipientMark {
    /// No marker, not limited: to send.
    Unmarked,
    /// Accepted by Meta.
    Sent,
    /// Refused by Meta (nothing was sent).
    Failed,
    /// May have gone out (a timeout, a 5xx, a runner that stopped
    /// mid-send, a marker the store cannot read): never resent.
    Unknown,
    /// Refused with `131049` in this job within
    /// [`MARKETING_LIMIT_WAIT`](super::MARKETING_LIMIT_WAIT): skipped.
    Limited,
}

/// A send's outcome, as its marker records it ([`JobStore::mark_outcome`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SendRecord {
    /// Accepted by Meta, with the message's id when the response had one.
    Sent {
        /// The message's id.
        message_id: Option<MessageId>,
    },
    /// Refused by Meta: an error proving nothing was sent.
    Failed {
        /// Meta's code, when the error had one.
        code: Option<i64>,
    },
    /// It may have gone out (a timeout, a 5xx).
    Unknown {
        /// Meta's code, when the error had one.
        code: Option<i64>,
    },
}

/// [`JobStore::observe`] as a bot listener (`BotBuilder::listen` with
/// `Listen::event("status_updated")`) or an `EventSink<WebhookEvent>` (next
/// to your own sink in a `FanoutSink`), over any job store
/// ([`KvJobs::feedback`](super::KvJobs::feedback),
/// [`MemoryJobs::feedback`](super::MemoryJobs::feedback), or
/// [`Self::new`] for one of yours). As a sink, a store that fails is
/// `SinkError::Delivery`: the webhook answers `500` and Meta redelivers.
/// As a bot's listener, it runs after the middleware and its errors go to
/// the bot's `ErrorHandler` (the default logs and acknowledges them: the
/// `131049` is then not recorded).
#[derive(Debug, Clone)]
pub struct JobFeedback {
    jobs: Arc<dyn JobStore>,
}

impl JobFeedback {
    /// The feedback of `jobs`.
    pub fn new(jobs: impl JobStore) -> Self {
        Self::shared(Arc::new(jobs))
    }

    /// The feedback of a store shared with other code (the runner's).
    pub fn shared(jobs: Arc<dyn JobStore>) -> Self {
        Self { jobs }
    }
}

#[async_trait]
impl CommandHandler for JobFeedback {
    async fn call(&self, ctx: Ctx) -> Result<()> {
        self.jobs.observe(ctx.event()).await.map(drop)
    }
}

#[async_trait]
impl EventSink<WebhookEvent> for JobFeedback {
    async fn deliver(&self, event: WebhookEvent) -> std::result::Result<(), SinkError> {
        self.jobs
            .observe(&event)
            .await
            .map(drop)
            .map_err(|e: Error| SinkError::Delivery(anyhow::Error::new(e)))
    }
}
