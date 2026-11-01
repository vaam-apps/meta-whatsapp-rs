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
use meta_whatsapp_core::error::{ConfigError, StorageError, ValidationError};
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::store::{Expiry, StoreKey};
use meta_whatsapp_core::{Error, Result};
use time::OffsetDateTime;

use super::store::{
    CAS_ATTEMPTS, Cursor, JobRead, JobRecord, Lease, Limited, Marker, MarkerState, MessageRecord,
    Page, StoredState, VERSION, decode, encode, read_job_record,
};
use super::{
    BUCKET_SECONDS, JOBS_LIMITED_NAMESPACE, JobId, JobSummary, KvJobs, MARKETING_LIMIT_WAIT,
    ScheduledJob, bucket_of, cursor_key, instant, is_marketing_limit, job_key, limited_digest,
    marker_key, message_key, millis, page_key, recipient_key,
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

/// Claims due jobs from a [`KvJobs`] and sends them. Build one per
/// process ([`Self::builder`]); any number of runners share one store.
/// See the [module docs](super).
pub struct JobRunner {
    jobs: KvJobs,
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

/// A page of a bucket as read: its version (`None`: the first page
/// missing, where the bucket ends) and its ids.
struct ReadPage {
    version: Option<u64>,
    ids: Vec<String>,
}

impl JobRunner {
    /// Start building a runner of `jobs`.
    pub fn builder(jobs: KvJobs) -> JobRunnerBuilder {
        JobRunnerBuilder::new(jobs)
    }

    /// This runner's id, written into the leases it takes.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Claim and run every job due, from the oldest minute that may still
    /// hold one up to now, one job after the other; each job's sends run
    /// [`JobRunnerBuilder::concurrency`] at a time. A store error ends the
    /// tick (a job claimed stays claimed until its lease expires, then any
    /// runner resumes it). Dropping the future mid-send leaves that
    /// recipient's outcome unknown: stop with [`Self::run_until`] instead.
    pub async fn tick(&self) -> Result<TickReport> {
        let mut report = TickReport::default();
        let key = cursor_key();
        let Some(mut found) = self.jobs.kv.get(&key).await? else {
            return Ok(report);
        };
        let mut cursor: Cursor = decode(&key, &found.value)?;
        if let Some(behind) = &cursor.behind {
            // The second read of the bucket the cursor was last moved
            // past, in case the runner that moved it failed before its own.
            let previous = cursor.bucket.saturating_sub(BUCKET_SECONDS);
            if versions(&self.read_bucket(previous).await?) != *behind {
                self.jobs.lower_cursor(previous).await?;
                let Some(again) = self.jobs.kv.get(&key).await? else {
                    return Ok(report);
                };
                cursor = decode(&key, &again.value)?;
                found = again;
            }
        }
        // The cursor's version while this tick may still move it forward.
        let mut held = Some(found.version);
        let last = bucket_of(millis(self.jobs.clock.now()));
        let mut bucket = cursor.bucket;
        while bucket <= last {
            let pages = self.read_bucket(bucket).await?;
            let mut settled = true;
            for id in pages.iter().flat_map(|page| page.ids.iter()) {
                if !self.process(id, bucket, &mut report).await? {
                    settled = false;
                }
            }
            held = match held {
                Some(version) if settled => self.advance(version, bucket, &pages).await?,
                _ => None,
            };
            bucket = bucket.saturating_add(BUCKET_SECONDS);
        }
        Ok(report)
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

    async fn read_bucket(&self, bucket: i64) -> Result<Vec<ReadPage>> {
        let mut pages = Vec::new();
        let mut page = 0usize;
        loop {
            let key = page_key(bucket, page);
            let Some(found) = self.jobs.kv.get(&key).await? else {
                pages.push(ReadPage {
                    version: None,
                    ids: Vec::new(),
                });
                return Ok(pages);
            };
            let read: Page = decode(&key, &found.value)?;
            pages.push(ReadPage {
                version: Some(found.version),
                ids: read.ids,
            });
            page += 1;
        }
    }

    /// Move the cursor past `bucket`, every job of which is settled; then
    /// read the bucket again, and move the cursor back if a job was indexed
    /// into it meanwhile (by a writer that read the cursor before it
    /// moved). The cursor keeps the pages' versions as read before it
    /// moved, so a runner that fails before that second read leaves it to
    /// the next tick (`Cursor::behind`). The cursor's new version, or
    /// `None` when this tick no longer moves it.
    async fn advance(&self, version: u64, bucket: i64, pages: &[ReadPage]) -> Result<Option<u64>> {
        let key = cursor_key();
        let next = Cursor {
            v: VERSION,
            bucket: bucket.saturating_add(BUCKET_SECONDS),
            behind: Some(versions(pages)),
        };
        let Some(moved) = self
            .jobs
            .kv
            .compare_and_swap(&key, version, Some(encode(&key, &next)?), Expiry::Never)
            .await?
        else {
            return Ok(None);
        };
        let again = self.read_bucket(bucket).await?;
        if versions(&again) != versions(pages) {
            self.jobs.lower_cursor(bucket).await?;
            return Ok(None);
        }
        Ok(Some(moved))
    }

    /// The job `id` met in `bucket`: claimed and run when due. Whether it
    /// is settled as far as this bucket goes.
    async fn process(&self, id: &str, bucket: i64, report: &mut TickReport) -> Result<bool> {
        let key = job_key(id);
        let Some(found) = self.jobs.kv.get(&key).await? else {
            return Ok(true);
        };
        let (record, version) = match read_job_record(&key, &found) {
            JobRead::Record(record, version) => (*record, version),
            // A later release's: its runners run it.
            JobRead::Newer => return Ok(false),
            JobRead::Corrupt(_) => {
                // Not the error itself: serde's message may quote the
                // record (a recipient's number).
                tracing::warn!("an unreadable job is skipped: it is never sent");
                return Ok(true);
            }
        };
        if bucket_of(record.due_at_ms) > bucket {
            // Moved later (retried, re-run): indexed under its own minute,
            // or indexed now if the move stopped before. A job met in a
            // bucket later than its due time's is due: indexed under the
            // minute it was scheduled in (a due time already past), or
            // met on an older page (re-run to an earlier time).
            if !record.state.settled() && !record.indexed {
                self.jobs.index(&record.id, record.due_at_ms).await?;
                self.jobs.mark_indexed(&key, record.due_at_ms).await?;
            }
            return Ok(true);
        }
        let now = millis(self.jobs.clock.now());
        match record.state {
            state if state.settled() => return Ok(true),
            StoredState::Pending if record.due_at_ms > now => return Ok(false),
            StoredState::Claimed
                if record
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.until_ms > now) =>
            {
                return Ok(false);
            }
            _ => {}
        }
        let claim = record.claims.saturating_add(1);
        let mut claimed = record;
        claimed.state = StoredState::Claimed;
        claimed.claims = claim;
        claimed.lease = Some(Lease {
            runner: self.id.clone(),
            claim,
            until_ms: now.saturating_add(duration_ms(self.lease)),
        });
        // The claim: only one runner's compare-and-swap from this version
        // succeeds.
        if self
            .jobs
            .kv
            .compare_and_swap(&key, version, Some(encode(&key, &claimed)?), Expiry::Never)
            .await?
            .is_none()
        {
            return Ok(false);
        }
        tracing::debug!(run = claimed.run, claim, "job claimed");
        let run = claimed.run;
        let ended = self.execute(key, claimed).await?;
        let settled = match &ended {
            JobEnd::LeaseLost => false,
            // Retried within this very minute: it is still to run here.
            JobEnd::Retrying { at, .. } => bucket_of(millis(*at)) != bucket,
            _ => true,
        };
        report.runs.push(JobRun {
            id: JobId(id.to_owned()),
            run,
            ended,
        });
        Ok(settled)
    }

    /// One pass over a job this runner just claimed.
    async fn execute(&self, key: StoreKey, job: JobRecord) -> Result<JobEnd> {
        let view = Arc::new(job.view());
        let mut summary = JobSummary::default();
        let mut pending = Vec::new();
        let mut by_key = HashMap::new();
        let claim = Claim {
            jobs: self.jobs.clone(),
            key,
            runner: self.id.clone(),
            number: job.claims,
            lease: self.lease,
            until_ms: Arc::new(AtomicI64::new(
                job.lease.as_ref().map_or(i64::MIN, |lease| lease.until_ms),
            )),
        };
        for (index, recipient) in view.recipients.iter().enumerate() {
            // Reading many markers can outlast the lease: renewed here
            // too, so another runner does not take the job over meanwhile.
            match claim.keep().await? {
                Some(Halt::Cancelled) => return Ok(JobEnd::Cancelled),
                Some(Halt::Lost) => return Ok(JobEnd::LeaseLost),
                None => {}
            }
            let rkey = recipient_key(recipient).unwrap_or_default();
            let marker = marker_key(&job.id, job.run, &rkey);
            if let Some(found) = self.jobs.kv.get(&marker).await? {
                match decode::<Marker>(&marker, &found.value).map(|read| read.state) {
                    Ok(MarkerState::Sent) => summary.sent += 1,
                    Ok(MarkerState::Failed) => summary.failed += 1,
                    // A "sending" left by a runner that stopped mid-send:
                    // it may have gone out.
                    Ok(MarkerState::Sending | MarkerState::Unknown) => summary.unknown.push(index),
                    // Something was written for this recipient (by a later
                    // release, or damaged): never sent again, and not a
                    // reason to stop every tick at this job.
                    Err(_) => {
                        tracing::warn!(
                            "a recipient's unreadable marker is read as an outcome unknown: \
                             never resent"
                        );
                        summary.unknown.push(index);
                    }
                }
                continue;
            }
            let limited = StoreKey::new(JOBS_LIMITED_NAMESPACE, limited_digest(&job.id, &rkey));
            if self.jobs.kv.get(&limited).await?.is_some() {
                summary.limited.push(index);
                continue;
            }
            pending.push(index);
            by_key.insert(rkey, index);
        }
        if !pending.is_empty() {
            let halt = self
                .send(&job, &view, &pending, by_key, &claim, &mut summary)
                .await?;
            match halt {
                Some(Halt::Cancelled) => return Ok(JobEnd::Cancelled),
                Some(Halt::Lost) => return Ok(JobEnd::LeaseLost),
                None => {}
            }
        }
        summary.unknown.sort_unstable();
        if summary.unsent == 0 {
            return self.settle(&claim, StoredState::Done, summary).await;
        }
        if job.retries >= self.max_retries {
            return self.settle(&claim, StoredState::Stopped, summary).await;
        }
        let delay = self
            .retry_delay
            .saturating_mul(2u32.saturating_pow(job.retries))
            .min(self.max_retry_delay);
        let at = after(self.jobs.clock.now(), delay);
        self.reschedule(&claim, at, summary).await
    }

    /// Send to the recipients at `pending` as a paced broadcast, and count
    /// what became of each into `summary`. Why the broadcast was halted,
    /// if it was.
    async fn send(
        &self,
        job: &JobRecord,
        view: &Arc<ScheduledJob>,
        pending: &[usize],
        by_key: HashMap<String, usize>,
        claim: &Claim,
        summary: &mut JobSummary,
    ) -> Result<Option<Halt>> {
        let outcomes: Arc<Mutex<HashMap<usize, Outcome>>> = Arc::default();
        let by_key = Arc::new(by_key);
        let durable = Arc::new(Durable {
            claim: claim.clone(),
            inner: Arc::clone(&self.outbound),
            job_id: job.id.clone(),
            run: job.run,
            by_key: Arc::clone(&by_key),
            outcomes: Arc::clone(&outcomes),
            handle: OnceLock::new(),
            halt: Mutex::new(None),
        });
        let compose = Arc::clone(&self.compose);
        let composed = Arc::clone(view);
        let failed = Arc::clone(&outcomes);
        let broadcast = Broadcast::builder(job.from.clone())
            .to(pending.iter().map(|&index| view.recipients[index].clone()))
            .compose(move |recipient: &Recipient| {
                let message = compose(&composed, recipient);
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
        claim: &Claim,
        state: StoredState,
        summary: JobSummary,
    ) -> Result<JobEnd> {
        for _ in 0..CAS_ATTEMPTS {
            let Some((record, version)) = self.jobs.read_job(&claim.key).await? else {
                return Ok(JobEnd::LeaseLost);
            };
            if record.state == StoredState::Cancelled {
                return Ok(JobEnd::Cancelled);
            }
            if !record.held_by(&claim.runner, claim.number) {
                return Ok(JobEnd::LeaseLost);
            }
            let mut next = record;
            next.state = state;
            next.lease = None;
            next.summary = Some((&summary).into());
            let bytes = encode(&claim.key, &next)?;
            if self
                .jobs
                .kv
                .compare_and_swap(
                    &claim.key,
                    version,
                    Some(bytes),
                    Expiry::After(self.jobs.retention),
                )
                .await?
                .is_some()
            {
                if !summary.unknown.is_empty() {
                    tracing::warn!(
                        unknown = summary.unknown.len(),
                        "job settled with sends whose outcome is unknown: never resent, \
                         reconcile them with the status webhooks"
                    );
                }
                return Ok(if state == StoredState::Done {
                    JobEnd::Done(summary)
                } else {
                    JobEnd::Stopped(summary)
                });
            }
        }
        Err(StorageError::Busy.into())
    }

    /// Hand the job back, pending at `at`, one retry more, and index it
    /// there.
    async fn reschedule(
        &self,
        claim: &Claim,
        at: OffsetDateTime,
        summary: JobSummary,
    ) -> Result<JobEnd> {
        // As stored: whole milliseconds.
        let due = millis(at);
        let at = instant(due);
        for _ in 0..CAS_ATTEMPTS {
            let Some((record, version)) = self.jobs.read_job(&claim.key).await? else {
                return Ok(JobEnd::LeaseLost);
            };
            if record.state == StoredState::Cancelled {
                return Ok(JobEnd::Cancelled);
            }
            if !record.held_by(&claim.runner, claim.number) {
                return Ok(JobEnd::LeaseLost);
            }
            let mut next = record;
            next.state = StoredState::Pending;
            next.lease = None;
            next.retries = next.retries.saturating_add(1);
            next.due_at_ms = due;
            next.indexed = false;
            let bytes = encode(&claim.key, &next)?;
            if self
                .jobs
                .kv
                .compare_and_swap(&claim.key, version, Some(bytes), Expiry::Never)
                .await?
                .is_some()
            {
                tracing::debug!(
                    retries = next.retries,
                    unsent = summary.unsent,
                    "job retried later"
                );
                self.jobs.index(&next.id, due).await?;
                self.jobs.mark_indexed(&claim.key, due).await?;
                return Ok(JobEnd::Retrying { at, summary });
            }
        }
        Err(StorageError::Busy.into())
    }
}

/// The versions of the pages of a bucket as read.
fn versions(pages: &[ReadPage]) -> Vec<u64> {
    pages.iter().filter_map(|page| page.version).collect()
}

/// `d` in whole milliseconds, saturating.
fn duration_ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The claim a runner holds on a job.
#[derive(Debug, Clone)]
struct Claim {
    jobs: KvJobs,
    key: StoreKey,
    runner: String,
    /// The claim's number (`Lease::claim`).
    number: u32,
    lease: Duration,
    /// Until when the lease runs, as this runner last wrote or read it
    /// (unix milliseconds).
    until_ms: Arc<AtomicI64>,
}

impl Claim {
    /// Whether the job, read now, is still this runner's under this claim
    /// and not cancelled (`None`), or why not; the lease is renewed when
    /// less than half of it is left.
    async fn hold(&self) -> Result<Option<Halt>> {
        let lease_ms = duration_ms(self.lease);
        for _ in 0..CAS_ATTEMPTS {
            let Some((record, version)) = self.jobs.read_job(&self.key).await? else {
                return Ok(Some(Halt::Lost));
            };
            if record.state == StoredState::Cancelled {
                return Ok(Some(Halt::Cancelled));
            }
            if !record.held_by(&self.runner, self.number) {
                return Ok(Some(Halt::Lost));
            }
            let now = millis(self.jobs.clock.now());
            let until = record
                .lease
                .as_ref()
                .map_or(i64::MIN, |lease| lease.until_ms);
            if until.saturating_sub(now) >= lease_ms / 2 {
                self.until_ms.store(until, Ordering::Relaxed);
                return Ok(None);
            }
            let renewed = now.saturating_add(lease_ms);
            let mut next = record;
            next.lease = Some(Lease {
                runner: self.runner.clone(),
                claim: self.number,
                until_ms: renewed,
            });
            let bytes = encode(&self.key, &next)?;
            if self
                .jobs
                .kv
                .compare_and_swap(&self.key, version, Some(bytes), Expiry::Never)
                .await?
                .is_some()
            {
                self.until_ms.store(renewed, Ordering::Relaxed);
                return Ok(None);
            }
        }
        Err(StorageError::Busy.into())
    }

    /// [`Self::hold`], only once less than half of the lease is
    /// left by this runner's last account of it: no read before that.
    async fn keep(&self) -> Result<Option<Halt>> {
        let now = millis(self.jobs.clock.now());
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

/// Why a pass stopped sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halt {
    Cancelled,
    Lost,
}

/// The outbound a pass's broadcast sends through: before each request, a
/// check that the job is still this runner's and not cancelled, then the
/// recipient's marker; after it, the marker's outcome.
struct Durable {
    claim: Claim,
    inner: Arc<dyn Outbound>,
    job_id: String,
    run: u32,
    by_key: Arc<HashMap<String, usize>>,
    outcomes: Arc<Mutex<HashMap<usize, Outcome>>>,
    handle: OnceLock<BroadcastHandle>,
    halt: Mutex<Option<Halt>>,
}

impl fmt::Debug for Durable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Durable")
            .field("runner", &self.claim.runner)
            .field("run", &self.run)
            .finish_non_exhaustive()
    }
}

fn halted(why: Halt) -> Error {
    ValidationError::new(
        "job",
        match why {
            Halt::Cancelled => "the job was cancelled: not sent",
            Halt::Lost => "another runner holds the job: not sent",
        },
    )
    .into()
}

impl Durable {
    fn record(&self, index: usize, outcome: Outcome) {
        lock(&self.outcomes).insert(index, outcome);
    }

    /// Stop the pass for `why`: no send starts after this one.
    fn stop(&self, why: Halt) -> Error {
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
        match self.claim.hold().await? {
            Some(why) => Err(self.stop(why)),
            None => Ok(()),
        }
    }

    /// Whether the job, read now, is still in this run and not cancelled
    /// (`None`), or why not.
    async fn this_run(&self) -> Result<Option<Halt>> {
        Ok(match self.claim.jobs.read_job(&self.claim.key).await? {
            None => Some(Halt::Lost),
            Some((record, _)) if record.state == StoredState::Cancelled => Some(Halt::Cancelled),
            Some((record, _)) if record.run != self.run => Some(Halt::Lost),
            Some(_) => None,
        })
    }

    async fn sent(
        &self,
        index: usize,
        marker: &StoreKey,
        version: u64,
        rkey: &str,
        response: &SendResponse,
    ) {
        let kv = &self.claim.jobs.kv;
        let message_id = response.message_id().map(|id| id.as_str().to_owned());
        let written = Marker {
            v: VERSION,
            state: MarkerState::Sent,
            runner: None,
            message_id: message_id.clone(),
            code: None,
        };
        let swapped = match encode(marker, &written) {
            Ok(bytes) => kv
                .compare_and_swap(marker, version, Some(bytes), Expiry::Keep)
                .await
                .map(|v| v.is_some()),
            Err(error) => Err(error),
        };
        if !matches!(swapped, Ok(true)) {
            tracing::warn!(
                "a job's sent message could not be recorded: its marker says sending, read as \
                 an outcome unknown (never resent)"
            );
        }
        if let Some(id) = message_id {
            let key = message_key(&id);
            let record = MessageRecord {
                v: VERSION,
                job: self.job_id.clone(),
                run: self.run,
                limited: limited_digest(&self.job_id, rkey),
            };
            let stored = match encode(&key, &record) {
                Ok(bytes) => kv
                    .put(&key, bytes, Expiry::After(self.claim.jobs.message_ttl))
                    .await
                    .map(drop),
                Err(error) => Err(error),
            };
            if stored.is_err() {
                tracing::warn!(
                    "a job's message id could not be kept: a 131049 for it will not be recorded"
                );
            }
        }
        self.record(index, Outcome::Sent);
    }

    async fn refused(
        &self,
        index: usize,
        marker: &StoreKey,
        version: u64,
        rkey: &str,
        error: &Error,
    ) {
        let kv = &self.claim.jobs.kv;
        let unknown = error.may_have_been_sent();
        let written = Marker {
            v: VERSION,
            state: if unknown {
                MarkerState::Unknown
            } else {
                MarkerState::Failed
            },
            runner: None,
            message_id: None,
            code: error.graph().map(|graph| graph.code),
        };
        let swapped = match encode(marker, &written) {
            Ok(bytes) => kv
                .compare_and_swap(marker, version, Some(bytes), Expiry::Keep)
                .await
                .map(|v| v.is_some()),
            Err(error) => Err(error),
        };
        if !matches!(swapped, Ok(true)) {
            tracing::warn!(
                "a job's failed send could not be recorded: its marker says sending, read as an \
                 outcome unknown (never resent)"
            );
        }
        if is_marketing_limit(error.kind()) {
            let limited = StoreKey::new(JOBS_LIMITED_NAMESPACE, limited_digest(&self.job_id, rkey));
            let stored = match encode(&limited, &Limited { v: VERSION }) {
                Ok(bytes) => kv
                    .put_if_absent(&limited, bytes, Expiry::After(MARKETING_LIMIT_WAIT))
                    .await
                    .map(drop),
                Err(error) => Err(error),
            };
            if stored.is_err() {
                tracing::warn!("a job's 131049 could not be recorded");
            }
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
        let kv = &self.claim.jobs.kv;
        let marker = marker_key(&self.job_id, self.run, &rkey);
        let intent = Marker {
            v: VERSION,
            state: MarkerState::Sending,
            runner: Some(self.claim.runner.clone()),
            message_id: None,
            code: None,
        };
        // Written before the request: whoever meets it without an outcome
        // reads it as unknown, and never sends again.
        let Some(version) = kv
            .put_if_absent(
                &marker,
                encode(&marker, &intent)?,
                Expiry::After(self.claim.jobs.retention),
            )
            .await?
        else {
            self.record(index, Outcome::Unknown);
            return Err(ValidationError::new(
                "recipient",
                "another runner holds this recipient: not sent",
            )
            .into());
        };
        // Checked again with the marker in place. A cancel, or a re-run,
        // that landed between the first check and the marker gets no
        // request from this runner: a re-run's markers are its own, so
        // this one would not stop its send. Another claim of this run
        // does not stop it: that runner meets this marker.
        let verdict = self.this_run().await;
        if !matches!(verdict, Ok(None)) {
            // Not sent: the marker goes, so the recipient is not left as
            // an outcome unknown (unless it cannot be removed).
            if !matches!(
                kv.compare_and_swap(&marker, version, None, Expiry::Keep)
                    .await,
                Ok(Some(_))
            ) {
                tracing::warn!("a job's unsent recipient could not be unmarked: never resent");
                self.record(index, Outcome::Unknown);
            }
            return Err(match verdict {
                Ok(Some(why)) => self.stop(why),
                Err(error) => error,
                Ok(None) => halted(Halt::Lost),
            });
        }
        match self.inner.send(from, message).await {
            Ok(response) => {
                self.sent(index, &marker, version, &rkey, &response).await;
                Ok(response)
            }
            Err(error) if error.may_resend() => {
                // Meta refused it before doing anything: without a marker,
                // it is sent again (the broadcast's retry, or the job's).
                if !matches!(
                    kv.compare_and_swap(&marker, version, None, Expiry::Keep)
                        .await,
                    Ok(Some(_))
                ) {
                    tracing::warn!("a job's refused send could not be unmarked: never resent");
                    self.record(index, Outcome::Unknown);
                }
                Err(error)
            }
            Err(error) => {
                self.refused(index, &marker, version, &rkey, &error).await;
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
    jobs: KvJobs,
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

    fn new(jobs: KvJobs) -> Self {
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
                self.jobs.clock.now().unix_timestamp_nanos(),
                RUNNERS.fetch_add(1, Ordering::Relaxed)
            ),
        };
        Ok(JobRunner {
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
