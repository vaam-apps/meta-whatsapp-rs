//! [`MemoryJobs`]: a [`JobStore`] in this process's memory, behind one
//! mutex, independent of any `KvStore`.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_core::Result;
use meta_whatsapp_core::clock::Clock;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::MessageId;
use time::OffsetDateTime;

use super::port::{
    ClaimHandler, Holding, JobClaim, JobFeedback, JobHalt, JobStore, RecipientMark, SendRecord,
};
use super::{Job, JobId, JobState, JobSummary, MARKETING_LIMIT_WAIT, ScheduledJob};
use crate::pacer::after;

/// Jobs in this process's memory: a [`JobStore`] behind one mutex, for
/// tests and for a single process that need not survive a restart (its
/// jobs do not: they live as long as the value and its clones). Runners in
/// one process may share it, each with its own id.
///
/// Every operation is one step under the mutex, never held across an
/// `.await`: a claim, a marker's creation, a cancel are atomic against
/// each other. Nothing expires but the `131049` skips (after
/// [`MARKETING_LIMIT_WAIT`]) and the sent messages' ids (after
/// [`Self::message_ttl`]): settled jobs and their markers stay until the
/// value is dropped.
#[derive(Clone)]
pub struct MemoryJobs {
    state: Arc<Mutex<State>>,
    clock: Arc<dyn Clock>,
    message_ttl: Duration,
}

impl fmt::Debug for MemoryJobs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryJobs")
            .field("jobs", &lock(&self.state).jobs.len())
            .field("message_ttl", &self.message_ttl)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct State {
    /// By job id.
    jobs: HashMap<String, Entry>,
    /// Jobs scheduled so far: the order of jobs due at one instant.
    scheduled: u64,
    /// By job id, run and recipient index.
    markers: HashMap<(String, u32, usize), Mark>,
    /// By message id: the job and recipient it was sent to, until when.
    messages: HashMap<String, (String, usize, OffsetDateTime)>,
    /// By job id and recipient index: limited until then.
    limited: HashMap<(String, usize), OffsetDateTime>,
}

struct Entry {
    job: ScheduledJob,
    order: u64,
    claims: u32,
    /// The runner, the claim's number, the lease's end.
    lease: Option<(String, u32, OffsetDateTime)>,
}

impl Entry {
    fn held_by(&self, claim: &JobClaim) -> bool {
        self.job.state == JobState::Claimed
            && self.lease.as_ref().is_some_and(|(runner, number, _)| {
                *runner == claim.runner && *number == claim.number
            })
    }

    fn claimable(&self, now: OffsetDateTime) -> bool {
        match self.job.state {
            JobState::Pending => self.job.due_at <= now,
            JobState::Claimed => self
                .lease
                .as_ref()
                .is_none_or(|(_, _, until)| *until <= now),
            _ => false,
        }
    }

    /// Why the claim `claim` no longer holds this job, if it does not.
    fn halt(&self, claim: &JobClaim) -> Option<JobHalt> {
        if self.job.state == JobState::Cancelled {
            Some(JobHalt::Cancelled)
        } else if self.held_by(claim) {
            None
        } else {
            Some(JobHalt::Lost)
        }
    }
}

enum Mark {
    /// By this runner.
    Sending(String),
    Sent,
    Failed,
    Unknown,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn out_of_range() -> meta_whatsapp_core::Error {
    ValidationError::new("recipient", "not a recipient of this job").into()
}

impl MemoryJobs {
    /// How long a sent message's id is kept for its `131049` status
    /// webhook by default: 7 days, as [`KvJobs`](super::KvJobs) does.
    pub const DEFAULT_MESSAGE_TTL: Duration = Duration::from_hours(7 * 24);

    /// No jobs yet; `clock` says when a job is due and a lease expires.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            state: Arc::default(),
            clock,
            message_ttl: Self::DEFAULT_MESSAGE_TTL,
        }
    }

    /// Keep each sent message's id this long, for its `131049` status
    /// webhook (default [`Self::DEFAULT_MESSAGE_TTL`]).
    #[must_use]
    pub fn message_ttl(mut self, ttl: Duration) -> Self {
        self.message_ttl = ttl;
        self
    }

    /// The `131049` feedback as a bot listener or an event sink
    /// ([`JobFeedback`]).
    pub fn feedback(&self) -> JobFeedback {
        JobFeedback::new(self.clone())
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Mark the recipient at `index` of `job` limited from now, unless it
    /// already is.
    fn limit(state: &mut State, job: &str, index: usize, now: OffsetDateTime) {
        let until = state.limited.entry((job.to_owned(), index)).or_insert(now);
        if *until <= now {
            *until = after(now, MARKETING_LIMIT_WAIT);
        }
    }

    /// Claim the job `id` for `runner` if it is claimable now: one step
    /// under the mutex.
    fn claim(&self, id: &str, runner: &str, lease: Duration) -> Option<JobClaim> {
        let mut state = self.state();
        let entry = state.jobs.get_mut(id)?;
        let now = self.clock.now();
        if !entry.claimable(now) {
            return None;
        }
        entry.claims = entry.claims.saturating_add(1);
        let until = after(now, lease);
        entry.job.state = JobState::Claimed;
        entry.lease = Some((runner.to_owned(), entry.claims, until));
        Some(JobClaim::new(
            entry.job.clone(),
            runner,
            entry.claims,
            until,
        ))
    }
}

/// Whether `a` is `b` as scheduled (a schedule retried).
fn same_schedule(a: &ScheduledJob, b: &ScheduledJob) -> bool {
    a.id == b.id
        && a.from == b.from
        && a.due_at == b.due_at
        && a.recipients == b.recipients
        && a.payload == b.payload
        && a.run == 1
}

#[async_trait]
impl JobStore for MemoryJobs {
    fn clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    async fn schedule(&self, job: Job) -> Result<()> {
        let job = job.into_scheduled()?;
        let mut state = self.state();
        if let Some(existing) = state.jobs.get(job.id.as_str()) {
            return if same_schedule(&existing.job, &job) {
                Ok(())
            } else {
                Err(ValidationError::new("id", "another job is stored under this id").into())
            };
        }
        state.scheduled = state.scheduled.saturating_add(1);
        let order = state.scheduled;
        state.jobs.insert(
            job.id.as_str().to_owned(),
            Entry {
                job,
                order,
                claims: 0,
                lease: None,
            },
        );
        Ok(())
    }

    async fn cancel(&self, id: &JobId) -> Result<bool> {
        let mut state = self.state();
        let Some(entry) = state.jobs.get_mut(id.as_str()) else {
            return Ok(false);
        };
        if entry.job.state.is_settled() {
            return Ok(false);
        }
        entry.job.state = JobState::Cancelled;
        entry.lease = None;
        Ok(true)
    }

    async fn rerun(&self, id: &JobId, at: OffsetDateTime) -> Result<bool> {
        let mut state = self.state();
        let Some(entry) = state.jobs.get_mut(id.as_str()) else {
            return Ok(false);
        };
        if !entry.job.state.is_settled() {
            return Ok(false);
        }
        entry.job.state = JobState::Pending;
        entry.job.run = entry.job.run.saturating_add(1);
        entry.job.retries = 0;
        entry.job.summary = None;
        entry.job.due_at = at;
        entry.lease = None;
        Ok(true)
    }

    async fn get(&self, id: &JobId) -> Result<Option<ScheduledJob>> {
        Ok(self
            .state()
            .jobs
            .get(id.as_str())
            .map(|entry| entry.job.clone()))
    }

    async fn limit_message(&self, message: &MessageId) -> Result<bool> {
        let now = self.clock.now();
        let mut state = self.state();
        let Some((job, index, until)) = state.messages.get(message.as_str()).cloned() else {
            return Ok(false);
        };
        if until <= now {
            state.messages.remove(message.as_str());
            return Ok(false);
        }
        Self::limit(&mut state, &job, index, now);
        Ok(true)
    }

    async fn claim_due(
        &self,
        runner: &str,
        lease: Duration,
        handler: &mut dyn ClaimHandler,
    ) -> Result<()> {
        // The jobs due when the call starts, each once: a job retried
        // within the call waits for the next.
        let mut due: Vec<(OffsetDateTime, u64, String)> = {
            let now = self.clock.now();
            self.state()
                .jobs
                .iter()
                .filter(|(_, entry)| entry.claimable(now))
                .map(|(id, entry)| (entry.job.due_at, entry.order, id.clone()))
                .collect()
        };
        due.sort();
        for (_, _, id) in due {
            // Checked again under the mutex as it is claimed: another
            // runner may have claimed it, or a cancel landed, meanwhile.
            let Some(claim) = self.claim(&id, runner, lease) else {
                continue;
            };
            handler.run(claim).await?;
        }
        Ok(())
    }

    async fn hold(&self, claim: &JobClaim, lease: Duration) -> Result<Holding> {
        let now = self.clock.now();
        let mut state = self.state();
        let Some(entry) = state.jobs.get_mut(claim.job.id.as_str()) else {
            return Ok(Holding::Halted(JobHalt::Lost));
        };
        if let Some(halt) = entry.halt(claim) {
            return Ok(Holding::Halted(halt));
        }
        let Some((_, _, until)) = entry.lease.as_mut() else {
            return Ok(Holding::Halted(JobHalt::Lost));
        };
        if *until - now < lease / 2 {
            *until = after(now, lease);
        }
        Ok(Holding::Held(*until))
    }

    async fn check_run(&self, claim: &JobClaim) -> Result<Option<JobHalt>> {
        Ok(match self.state().jobs.get(claim.job.id.as_str()) {
            None => Some(JobHalt::Lost),
            Some(entry) if entry.job.state == JobState::Cancelled => Some(JobHalt::Cancelled),
            Some(entry) if entry.job.run != claim.job.run => Some(JobHalt::Lost),
            Some(_) => None,
        })
    }

    async fn recipient(&self, claim: &JobClaim, index: usize) -> Result<RecipientMark> {
        if index >= claim.job.recipients.len() {
            return Err(out_of_range());
        }
        let now = self.clock.now();
        let state = self.state();
        let id = claim.job.id.as_str().to_owned();
        if let Some(mark) = state.markers.get(&(id.clone(), claim.job.run, index)) {
            return Ok(match mark {
                Mark::Sent => RecipientMark::Sent,
                Mark::Failed => RecipientMark::Failed,
                Mark::Sending(_) | Mark::Unknown => RecipientMark::Unknown,
            });
        }
        Ok(match state.limited.get(&(id, index)) {
            Some(until) if *until > now => RecipientMark::Limited,
            _ => RecipientMark::Unmarked,
        })
    }

    async fn mark_sending(&self, claim: &JobClaim, index: usize) -> Result<bool> {
        if index >= claim.job.recipients.len() {
            return Err(out_of_range());
        }
        let key = (claim.job.id.as_str().to_owned(), claim.job.run, index);
        let mut state = self.state();
        if state.markers.contains_key(&key) {
            return Ok(false);
        }
        state
            .markers
            .insert(key, Mark::Sending(claim.runner.clone()));
        Ok(true)
    }

    async fn mark_outcome(
        &self,
        claim: &JobClaim,
        index: usize,
        outcome: SendRecord,
    ) -> Result<bool> {
        let key = (claim.job.id.as_str().to_owned(), claim.job.run, index);
        let now = self.clock.now();
        let mut state = self.state();
        match state.markers.get(&key) {
            Some(Mark::Sending(runner)) if *runner == claim.runner => {}
            _ => return Ok(false),
        }
        let mark = match outcome {
            SendRecord::Sent { message_id } => {
                if let Some(id) = message_id {
                    state.messages.retain(|_, (_, _, until)| *until > now);
                    state.messages.insert(
                        id.as_str().to_owned(),
                        (key.0.clone(), index, after(now, self.message_ttl)),
                    );
                }
                Mark::Sent
            }
            SendRecord::Failed { .. } => Mark::Failed,
            SendRecord::Unknown { .. } => Mark::Unknown,
        };
        state.markers.insert(key, mark);
        Ok(true)
    }

    async fn unmark(&self, claim: &JobClaim, index: usize) -> Result<bool> {
        let key = (claim.job.id.as_str().to_owned(), claim.job.run, index);
        let mut state = self.state();
        match state.markers.get(&key) {
            Some(Mark::Sending(runner)) if *runner == claim.runner => {
                state.markers.remove(&key);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn limit_recipient(&self, claim: &JobClaim, index: usize) -> Result<()> {
        if index >= claim.job.recipients.len() {
            return Err(out_of_range());
        }
        let now = self.clock.now();
        Self::limit(&mut self.state(), claim.job.id.as_str(), index, now);
        Ok(())
    }

    async fn settle(
        &self,
        claim: &JobClaim,
        state: JobState,
        summary: &JobSummary,
    ) -> Result<Option<JobHalt>> {
        if !matches!(state, JobState::Done | JobState::Stopped) {
            return Err(ValidationError::new("state", "a job settles as done or stopped").into());
        }
        let mut jobs = self.state();
        let Some(entry) = jobs.jobs.get_mut(claim.job.id.as_str()) else {
            return Ok(Some(JobHalt::Lost));
        };
        if let Some(halt) = entry.halt(claim) {
            return Ok(Some(halt));
        }
        entry.job.state = state;
        entry.job.summary = Some(summary.clone());
        entry.lease = None;
        Ok(None)
    }

    async fn retry(
        &self,
        claim: &JobClaim,
        at: OffsetDateTime,
        _summary: &JobSummary,
    ) -> Result<Option<JobHalt>> {
        let mut state = self.state();
        let Some(entry) = state.jobs.get_mut(claim.job.id.as_str()) else {
            return Ok(Some(JobHalt::Lost));
        };
        if let Some(halt) = entry.halt(claim) {
            return Ok(Some(halt));
        }
        entry.job.state = JobState::Pending;
        entry.job.retries = entry.job.retries.saturating_add(1);
        entry.job.due_at = at;
        entry.lease = None;
        Ok(None)
    }
}
