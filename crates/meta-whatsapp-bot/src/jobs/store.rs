//! [`KvJobs`]: the default [`JobStore`]: the jobs' records, their due
//! index and their markers on a `KvStore`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_core::Result;
use meta_whatsapp_core::clock::Clock;
use meta_whatsapp_core::error::{StorageError, ValidationError};
use meta_whatsapp_core::ids::{GroupId, MessageId, PhoneNumberId, UserId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use super::port::{ClaimHandler, Holding, JobClaim, JobHalt, JobStore, RecipientMark, SendRecord};
use super::{
    BUCKET_PAGE_CAPACITY, BUCKET_SECONDS, JOBS_LIMITED_NAMESPACE, Job, JobEnd, JobFeedback, JobId,
    JobState, JobSummary, MARKETING_LIMIT_WAIT, ScheduledJob, bucket_of, cursor_key, instant,
    job_key, limited_digest, marker_key, message_key, millis, page_key, recipient_key,
};
use crate::pacer::after;

/// The records' version (`"v"`).
pub(super) const VERSION: u32 = 1;

/// Compare-and-swap rounds before giving up as `StorageError::Busy`.
pub(super) const CAS_ATTEMPTS: usize = 16;

/// A job's state, as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum StoredState {
    Pending,
    Claimed,
    Done,
    Stopped,
    Cancelled,
}

impl StoredState {
    pub(super) fn settled(self) -> bool {
        matches!(self, Self::Done | Self::Stopped | Self::Cancelled)
    }

    fn public(self) -> JobState {
        match self {
            Self::Pending => JobState::Pending,
            Self::Claimed => JobState::Claimed,
            Self::Done => JobState::Done,
            Self::Stopped => JobState::Stopped,
            Self::Cancelled => JobState::Cancelled,
        }
    }
}

/// Who holds a claimed job, and until when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Lease {
    /// The runner's id.
    pub(super) runner: String,
    /// The claim's number (the job's `claims` when it was made), so a
    /// runner that claims a job again does not take an old claim for its
    /// own.
    pub(super) claim: u32,
    /// Unix milliseconds.
    pub(super) until_ms: i64,
}

/// A recipient, as stored (`Recipient` is serialized for Meta, not read
/// back).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum StoredRecipient {
    Phone(String),
    User(UserId),
    PhoneAndUser { phone: String, user: UserId },
    Group(GroupId),
}

impl StoredRecipient {
    fn from_recipient(recipient: &Recipient) -> Option<Self> {
        Some(match recipient {
            Recipient::Phone(phone) => Self::Phone(phone.clone()),
            Recipient::User(user) => Self::User(user.clone()),
            Recipient::PhoneAndUser { phone, user } => Self::PhoneAndUser {
                phone: phone.clone(),
                user: user.clone(),
            },
            Recipient::Group(group) => Self::Group(group.clone()),
            _ => return None,
        })
    }

    pub(super) fn recipient(&self) -> Recipient {
        match self {
            Self::Phone(phone) => Recipient::Phone(phone.clone()),
            Self::User(user) => Recipient::User(user.clone()),
            Self::PhoneAndUser { phone, user } => Recipient::PhoneAndUser {
                phone: phone.clone(),
                user: user.clone(),
            },
            Self::Group(group) => Recipient::Group(group.clone()),
        }
    }
}

/// A settled pass's counts, as stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StoredSummary {
    pub(super) sent: usize,
    pub(super) failed: usize,
    pub(super) unknown: Vec<usize>,
    pub(super) limited: Vec<usize>,
    pub(super) unsent: usize,
}

impl From<&JobSummary> for StoredSummary {
    fn from(summary: &JobSummary) -> Self {
        Self {
            sent: summary.sent,
            failed: summary.failed,
            unknown: summary.unknown.clone(),
            limited: summary.limited.clone(),
            unsent: summary.unsent,
        }
    }
}

impl From<&StoredSummary> for JobSummary {
    fn from(summary: &StoredSummary) -> Self {
        Self {
            sent: summary.sent,
            failed: summary.failed,
            unknown: summary.unknown.clone(),
            limited: summary.limited.clone(),
            unsent: summary.unsent,
        }
    }
}

/// A job's record ([`super::JOBS_NAMESPACE`]). Field order is the stored
/// order (pinned).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct JobRecord {
    pub(super) v: u32,
    pub(super) id: String,
    pub(super) from: PhoneNumberId,
    pub(super) due_at_ms: i64,
    pub(super) state: StoredState,
    pub(super) run: u32,
    pub(super) retries: u32,
    pub(super) claims: u32,
    /// Whether the due index holds it under `due_at_ms`'s bucket. False
    /// between a write that moves it and the index's update: a reader that
    /// meets it elsewhere indexes it.
    pub(super) indexed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) lease: Option<Lease>,
    pub(super) recipients: Vec<StoredRecipient>,
    #[serde(default)]
    pub(super) payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) summary: Option<StoredSummary>,
}

impl JobRecord {
    /// Whether `other` is this job as scheduled (a schedule retried).
    fn same_schedule(&self, other: &Self) -> bool {
        self.id == other.id
            && self.from == other.from
            && self.due_at_ms == other.due_at_ms
            && self.recipients == other.recipients
            && self.payload == other.payload
            && self.run == 1
    }

    /// Whether the runner `runner` holds this job under claim `claim`.
    pub(super) fn held_by(&self, runner: &str, claim: u32) -> bool {
        self.state == StoredState::Claimed
            && self
                .lease
                .as_ref()
                .is_some_and(|l| l.runner == runner && l.claim == claim)
    }

    pub(super) fn view(&self) -> ScheduledJob {
        ScheduledJob {
            id: JobId(self.id.clone()),
            from: self.from.clone(),
            due_at: instant(self.due_at_ms),
            state: self.state.public(),
            run: self.run,
            retries: self.retries,
            recipients: self
                .recipients
                .iter()
                .map(StoredRecipient::recipient)
                .collect(),
            payload: self.payload.clone(),
            summary: self.summary.as_ref().map(JobSummary::from),
        }
    }
}

/// A page of a due-index bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Page {
    pub(super) v: u32,
    pub(super) ids: Vec<String>,
}

/// The oldest bucket that may still hold a job to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Cursor {
    pub(super) v: u32,
    /// Unix seconds.
    pub(super) bucket: i64,
    /// When a runner moved the cursor forward: the versions of the pages
    /// of the bucket just behind `bucket`, as that runner read them before
    /// moving it. A tick starts by reading that bucket again: pages that
    /// changed since (a job indexed meanwhile, by a writer that read the
    /// cursor before it moved) move the cursor back, even when the runner
    /// that moved it failed before its own second read. Absent when the
    /// cursor was moved back, or created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) behind: Option<Vec<u64>>,
}

/// A recipient's marker ([`super::JOBS_SENT_NAMESPACE`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum MarkerState {
    /// Written before the request: if nothing follows, the outcome is
    /// unknown.
    Sending,
    Sent,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Marker {
    pub(super) v: u32,
    pub(super) state: MarkerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) runner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) message_id: Option<String>,
    /// Meta's code, for a refusal or an unknown outcome that had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) code: Option<i64>,
}

/// Which job and recipient a sent message was for
/// ([`super::JOBS_MESSAGE_NAMESPACE`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct MessageRecord {
    pub(super) v: u32,
    pub(super) job: String,
    pub(super) run: u32,
    /// The key of the recipient's limited marker in
    /// [`JOBS_LIMITED_NAMESPACE`].
    pub(super) limited: String,
}

/// A marketing-limited marker ([`JOBS_LIMITED_NAMESPACE`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Limited {
    pub(super) v: u32,
}

#[derive(Deserialize)]
struct Header {
    v: u32,
}

pub(super) fn encode<T: Serialize>(key: &StoreKey, value: &T) -> Result<Vec<u8>, StorageError> {
    serde_json::to_vec(value).map_err(|source| StorageError::Corrupt {
        key: key.to_string(),
        source,
    })
}

pub(super) fn decode<T: DeserializeOwned>(key: &StoreKey, bytes: &[u8]) -> Result<T, StorageError> {
    serde_json::from_slice(bytes).map_err(|source| StorageError::Corrupt {
        key: key.to_string(),
        source,
    })
}

/// A job record as read.
pub(super) enum JobRead {
    Record(Box<JobRecord>, u64),
    /// Written by a later release (a higher `"v"`): left alone.
    Newer,
    /// Unreadable.
    Corrupt(StorageError),
}

pub(super) fn read_job_record(key: &StoreKey, found: &Versioned) -> JobRead {
    match decode::<JobRecord>(key, &found.value) {
        Ok(record) if record.v == VERSION => JobRead::Record(Box::new(record), found.version),
        Ok(_) => JobRead::Newer,
        Err(error) => match serde_json::from_slice::<Header>(&found.value) {
            Ok(header) if header.v > VERSION => JobRead::Newer,
            _ => JobRead::Corrupt(error),
        },
    }
}

/// Durable jobs: the default [`JobStore`], a typed store on [`KvStore`]
/// (no port of its own). See the [module docs](super) for what it stores
/// where.
///
/// Cheap to clone; clones share the store. Every instance on the same
/// store sees the same jobs, across processes for a shared store.
#[derive(Clone)]
pub struct KvJobs {
    kv: Arc<dyn KvStore>,
    clock: Arc<dyn Clock>,
    retention: Duration,
    message_ttl: Duration,
}

impl fmt::Debug for KvJobs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KvJobs")
            .field("kv", &self.kv)
            .field("retention", &self.retention)
            .field("message_ttl", &self.message_ttl)
            .finish_non_exhaustive()
    }
}

/// A page of a bucket as read: its version (`None`: the first page
/// missing, where the bucket ends) and its ids.
struct ReadPage {
    version: Option<u64>,
    ids: Vec<String>,
}

/// The versions of the pages of a bucket as read.
fn versions(pages: &[ReadPage]) -> Vec<u64> {
    pages.iter().filter_map(|page| page.version).collect()
}

/// `d` in whole milliseconds, saturating.
pub(super) fn duration_ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// The recipient at `index` of the job `claim` holds, and its key.
fn recipient_at(claim: &JobClaim, index: usize) -> Result<String> {
    claim
        .job
        .recipients
        .get(index)
        .and_then(recipient_key)
        .ok_or_else(|| ValidationError::new("recipient", "not a recipient of this job").into())
}

impl KvJobs {
    /// How long settled jobs, markers and index pages are kept by default:
    /// 30 days.
    pub const DEFAULT_RETENTION: Duration = Duration::from_hours(30 * 24);

    /// How long a sent message's id is kept for its `131049` status
    /// webhook by default: 7 days, how long Meta redelivers a webhook
    /// (`webhooks`).
    pub const DEFAULT_MESSAGE_TTL: Duration = Duration::from_hours(7 * 24);

    /// Jobs in `kv`; `clock` says when a job is due and a lease expires
    /// (a `ManualClock` in tests, the same the store expires by).
    pub fn new(kv: Arc<dyn KvStore>, clock: Arc<dyn Clock>) -> Self {
        Self {
            kv,
            clock,
            retention: Self::DEFAULT_RETENTION,
            message_ttl: Self::DEFAULT_MESSAGE_TTL,
        }
    }

    /// Keep settled jobs, markers and index pages this long (default
    /// [`Self::DEFAULT_RETENTION`]). A job resumed later than this after a
    /// marker was written sends that recipient again; one left unrun this
    /// long after its due time (or after it was scheduled, for a due time
    /// already past) is no longer found: its record stays, pending, and is
    /// never sent.
    #[must_use]
    pub fn retention(mut self, retention: Duration) -> Self {
        self.retention = retention;
        self
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

    async fn read_job(&self, key: &StoreKey) -> Result<Option<(JobRecord, u64)>> {
        let Some(found) = self.kv.get(key).await? else {
            return Ok(None);
        };
        match read_job_record(key, &found) {
            JobRead::Record(record, version) => Ok(Some((*record, version))),
            JobRead::Newer => Err(ValidationError::new(
                "id",
                "the job was written by a later release of this crate",
            )
            .into()),
            JobRead::Corrupt(error) => Err(error.into()),
        }
    }

    /// Append `id` to the due index under `due_ms`'s bucket (the current
    /// one for a due time already past), then move the cursor back to that
    /// bucket if it is past it.
    async fn index(&self, id: &str, due_ms: i64) -> Result<()> {
        // A due time already past is due now: indexed under the current
        // minute, so a job due long ago (a year, the Unix epoch) does not
        // move the cursor back to then, every minute since read one by
        // one by the next tick. A runner meeting a job in a bucket later
        // than its due time's runs it there (`KvJobs::claim_one`).
        let bucket = bucket_of(due_ms).max(bucket_of(millis(self.clock.now())));
        // Every page of a bucket expires at once (a reader stops at the
        // first page missing), retention after the bucket, or after now
        // for a bucket already that old.
        let start = instant(bucket.saturating_mul(1000)).max(self.clock.now());
        let expiry = Expiry::At(after(start, self.retention));
        let mut page = 0usize;
        'pages: loop {
            let key = page_key(bucket, page);
            for _ in 0..CAS_ATTEMPTS {
                match self.kv.get(&key).await? {
                    None => {
                        let bytes = encode(
                            &key,
                            &Page {
                                v: VERSION,
                                ids: vec![id.to_owned()],
                            },
                        )?;
                        if self.kv.put_if_absent(&key, bytes, expiry).await?.is_some() {
                            break 'pages;
                        }
                    }
                    Some(found) => {
                        let mut current: Page = decode(&key, &found.value)?;
                        let known = current.ids.iter().any(|known| known == id);
                        if !known && current.ids.len() >= BUCKET_PAGE_CAPACITY {
                            page += 1;
                            continue 'pages;
                        }
                        // Written even when the id is there already (a job
                        // re-run or retried into the minute it ran in): the
                        // new version is what tells a runner moving the
                        // cursor past this minute to move it back.
                        if !known {
                            current.ids.push(id.to_owned());
                        }
                        let bytes = encode(&key, &current)?;
                        if self
                            .kv
                            .compare_and_swap(&key, found.version, Some(bytes), Expiry::Keep)
                            .await?
                            .is_some()
                        {
                            break 'pages;
                        }
                    }
                }
            }
            return Err(StorageError::Busy.into());
        }
        self.lower_cursor(bucket).await
    }

    /// Move the cursor back to `bucket` when it is past it (or create it).
    async fn lower_cursor(&self, bucket: i64) -> Result<()> {
        let key = cursor_key();
        let bytes = encode(
            &key,
            &Cursor {
                v: VERSION,
                bucket,
                behind: None,
            },
        )?;
        for _ in 0..CAS_ATTEMPTS {
            match self.kv.get(&key).await? {
                None => {
                    if self
                        .kv
                        .put_if_absent(&key, bytes.clone(), Expiry::Never)
                        .await?
                        .is_some()
                    {
                        return Ok(());
                    }
                }
                Some(found) => {
                    let cursor: Cursor = decode(&key, &found.value)?;
                    if cursor.bucket <= bucket {
                        return Ok(());
                    }
                    if self
                        .kv
                        .compare_and_swap(&key, found.version, Some(bytes.clone()), Expiry::Never)
                        .await?
                        .is_some()
                    {
                        return Ok(());
                    }
                }
            }
        }
        Err(StorageError::Busy.into())
    }

    /// Record that the job at `key` is indexed under `due_ms`, if it still
    /// is due then. Contention leaves it unrecorded, which only makes a
    /// reader index it again.
    async fn mark_indexed(&self, key: &StoreKey, due_ms: i64) -> Result<()> {
        for _ in 0..CAS_ATTEMPTS {
            let Some((record, version)) = self.read_job(key).await? else {
                return Ok(());
            };
            if record.indexed || record.due_at_ms != due_ms || record.state.settled() {
                return Ok(());
            }
            let mut next = record;
            next.indexed = true;
            let bytes = encode(key, &next)?;
            if self
                .kv
                .compare_and_swap(key, version, Some(bytes), Expiry::Keep)
                .await?
                .is_some()
            {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn read_bucket(&self, bucket: i64) -> Result<Vec<ReadPage>> {
        let mut pages = Vec::new();
        let mut page = 0usize;
        loop {
            let key = page_key(bucket, page);
            let Some(found) = self.kv.get(&key).await? else {
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
            .kv
            .compare_and_swap(&key, version, Some(encode(&key, &next)?), Expiry::Never)
            .await?
        else {
            return Ok(None);
        };
        let again = self.read_bucket(bucket).await?;
        if versions(&again) != versions(pages) {
            self.lower_cursor(bucket).await?;
            return Ok(None);
        }
        Ok(Some(moved))
    }

    /// The job `id` met in `bucket`: claimed for `runner` and handed to
    /// `handler` when due. Whether it is settled as far as this bucket
    /// goes.
    async fn claim_one(
        &self,
        id: &str,
        bucket: i64,
        runner: &str,
        lease: Duration,
        handler: &mut dyn ClaimHandler,
    ) -> Result<bool> {
        let key = job_key(id);
        let Some(found) = self.kv.get(&key).await? else {
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
                self.index(&record.id, record.due_at_ms).await?;
                self.mark_indexed(&key, record.due_at_ms).await?;
            }
            return Ok(true);
        }
        let now = millis(self.clock.now());
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
        let number = record.claims.saturating_add(1);
        let until_ms = now.saturating_add(duration_ms(lease));
        let mut claimed = record;
        claimed.state = StoredState::Claimed;
        claimed.claims = number;
        claimed.lease = Some(Lease {
            runner: runner.to_owned(),
            claim: number,
            until_ms,
        });
        // The claim: only one runner's compare-and-swap from this version
        // succeeds.
        if self
            .kv
            .compare_and_swap(&key, version, Some(encode(&key, &claimed)?), Expiry::Never)
            .await?
            .is_none()
        {
            return Ok(false);
        }
        let ended = handler
            .run(JobClaim::new(
                claimed.view(),
                runner,
                number,
                instant(until_ms),
            ))
            .await?;
        Ok(match &ended {
            JobEnd::LeaseLost => false,
            // Retried within this very minute: it is still to run here.
            JobEnd::Retrying { at, .. } => bucket_of(millis(*at)) != bucket,
            _ => true,
        })
    }

    /// This run's marker of the recipient at `index`, with its key.
    fn marker_of(claim: &JobClaim, index: usize) -> Result<(StoreKey, String)> {
        let rkey = recipient_at(claim, index)?;
        Ok((
            marker_key(claim.job.id.as_str(), claim.job.run, &rkey),
            rkey,
        ))
    }

    /// The version of the "sending" marker at `marker` that `runner`
    /// created, if it is still that.
    async fn sending_of(&self, marker: &StoreKey, runner: &str) -> Result<Option<u64>> {
        let Some(found) = self.kv.get(marker).await? else {
            return Ok(None);
        };
        Ok(match decode::<Marker>(marker, &found.value) {
            Ok(read)
                if read.state == MarkerState::Sending && read.runner.as_deref() == Some(runner) =>
            {
                Some(found.version)
            }
            _ => None,
        })
    }

    async fn limit(&self, limited: StoreKey) -> Result<()> {
        self.kv
            .put_if_absent(
                &limited,
                encode(&limited, &Limited { v: VERSION })?,
                Expiry::After(MARKETING_LIMIT_WAIT),
            )
            .await?;
        Ok(())
    }
}

#[async_trait]
impl JobStore for KvJobs {
    fn clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    /// Also: the job is indexed under its due time; a job scheduled again
    /// after an error or a crash that stopped this half-way is indexed
    /// again.
    async fn schedule(&self, job: Job) -> Result<()> {
        let job = job.into_scheduled()?;
        let recipients = job
            .recipients
            .iter()
            .map(StoredRecipient::from_recipient)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                ValidationError::new("recipients", "a kind of recipient a job cannot store")
            })?;
        let record = JobRecord {
            v: VERSION,
            id: job.id.0,
            from: job.from,
            due_at_ms: millis(job.due_at),
            state: StoredState::Pending,
            run: 1,
            retries: 0,
            claims: 0,
            indexed: false,
            lease: None,
            recipients,
            payload: job.payload,
            summary: None,
        };
        let key = job_key(&record.id);
        let bytes = encode(&key, &record)?;
        if self
            .kv
            .put_if_absent(&key, bytes, Expiry::Never)
            .await?
            .is_none()
        {
            let Some((existing, _)) = self.read_job(&key).await? else {
                // Removed meanwhile (expired after settling): busy, try
                // again.
                return Err(StorageError::Busy.into());
            };
            if !existing.same_schedule(&record) {
                return Err(
                    ValidationError::new("id", "another job is stored under this id").into(),
                );
            }
            if existing.state != StoredState::Pending || existing.indexed {
                return Ok(());
            }
        }
        self.index(&record.id, record.due_at_ms).await?;
        self.mark_indexed(&key, record.due_at_ms).await
    }

    async fn cancel(&self, id: &JobId) -> Result<bool> {
        let key = job_key(id.as_str());
        for _ in 0..CAS_ATTEMPTS {
            let Some((record, version)) = self.read_job(&key).await? else {
                return Ok(false);
            };
            if record.state.settled() {
                return Ok(false);
            }
            let mut next = record;
            next.state = StoredState::Cancelled;
            next.lease = None;
            let bytes = encode(&key, &next)?;
            if self
                .kv
                .compare_and_swap(&key, version, Some(bytes), Expiry::After(self.retention))
                .await?
                .is_some()
            {
                tracing::debug!("job cancelled");
                return Ok(true);
            }
        }
        Err(StorageError::Busy.into())
    }

    /// Also: after an error, call it again: a re-run that stopped before
    /// its index was written is indexed then (and `false` is returned, the
    /// job being pending already).
    async fn rerun(&self, id: &JobId, at: OffsetDateTime) -> Result<bool> {
        let key = job_key(id.as_str());
        for _ in 0..CAS_ATTEMPTS {
            let Some((record, version)) = self.read_job(&key).await? else {
                return Ok(false);
            };
            if !record.state.settled() {
                if !record.indexed {
                    // A rerun that stopped before indexing it.
                    self.index(&record.id, record.due_at_ms).await?;
                    self.mark_indexed(&key, record.due_at_ms).await?;
                }
                return Ok(false);
            }
            let mut next = record;
            next.state = StoredState::Pending;
            next.run = next.run.saturating_add(1);
            next.retries = 0;
            next.lease = None;
            next.summary = None;
            next.due_at_ms = millis(at);
            next.indexed = false;
            let bytes = encode(&key, &next)?;
            if self
                .kv
                .compare_and_swap(&key, version, Some(bytes), Expiry::Never)
                .await?
                .is_some()
            {
                self.index(&next.id, next.due_at_ms).await?;
                self.mark_indexed(&key, next.due_at_ms).await?;
                return Ok(true);
            }
        }
        Err(StorageError::Busy.into())
    }

    async fn get(&self, id: &JobId) -> Result<Option<ScheduledJob>> {
        Ok(self
            .read_job(&job_key(id.as_str()))
            .await?
            .map(|(record, _)| record.view()))
    }

    /// Also: a message whose id is older than [`Self::message_ttl`] is no
    /// longer kept.
    async fn limit_message(&self, message: &MessageId) -> Result<bool> {
        let key = message_key(message.as_str());
        let Some(found) = self.kv.get(&key).await? else {
            return Ok(false);
        };
        let record: MessageRecord = decode(&key, &found.value)?;
        self.limit(StoreKey::new(JOBS_LIMITED_NAMESPACE, record.limited))
            .await?;
        tracing::info!(
            run = record.run,
            "a job's recipient is marketing-limited (131049): skipped by a re-run for 24 hours"
        );
        Ok(true)
    }

    /// Also: the due index is read from the oldest minute that may still
    /// hold a job to run up to now, and its cursor moved past the minutes
    /// whose every job is settled (the [module docs](super)).
    async fn claim_due(
        &self,
        runner: &str,
        lease: Duration,
        handler: &mut dyn ClaimHandler,
    ) -> Result<()> {
        let key = cursor_key();
        let Some(mut found) = self.kv.get(&key).await? else {
            return Ok(());
        };
        let mut cursor: Cursor = decode(&key, &found.value)?;
        if let Some(behind) = &cursor.behind {
            // The second read of the bucket the cursor was last moved
            // past, in case the runner that moved it failed before its own.
            let previous = cursor.bucket.saturating_sub(BUCKET_SECONDS);
            if versions(&self.read_bucket(previous).await?) != *behind {
                self.lower_cursor(previous).await?;
                let Some(again) = self.kv.get(&key).await? else {
                    return Ok(());
                };
                cursor = decode(&key, &again.value)?;
                found = again;
            }
        }
        // The cursor's version while this call may still move it forward.
        let mut held = Some(found.version);
        let last = bucket_of(millis(self.clock.now()));
        let mut bucket = cursor.bucket;
        while bucket <= last {
            let pages = self.read_bucket(bucket).await?;
            let mut settled = true;
            for id in pages.iter().flat_map(|page| page.ids.iter()) {
                if !self.claim_one(id, bucket, runner, lease, handler).await? {
                    settled = false;
                }
            }
            held = match held {
                Some(version) if settled => self.advance(version, bucket, &pages).await?,
                _ => None,
            };
            bucket = bucket.saturating_add(BUCKET_SECONDS);
        }
        Ok(())
    }

    async fn hold(&self, claim: &JobClaim, lease: Duration) -> Result<Holding> {
        let key = job_key(claim.job.id.as_str());
        let lease_ms = duration_ms(lease);
        for _ in 0..CAS_ATTEMPTS {
            let Some((record, version)) = self.read_job(&key).await? else {
                return Ok(Holding::Halted(JobHalt::Lost));
            };
            if record.state == StoredState::Cancelled {
                return Ok(Holding::Halted(JobHalt::Cancelled));
            }
            if !record.held_by(&claim.runner, claim.number) {
                return Ok(Holding::Halted(JobHalt::Lost));
            }
            let now = millis(self.clock.now());
            let until = record
                .lease
                .as_ref()
                .map_or(i64::MIN, |lease| lease.until_ms);
            if until.saturating_sub(now) >= lease_ms / 2 {
                return Ok(Holding::Held(instant(until)));
            }
            let renewed = now.saturating_add(lease_ms);
            let mut next = record;
            next.lease = Some(Lease {
                runner: claim.runner.clone(),
                claim: claim.number,
                until_ms: renewed,
            });
            let bytes = encode(&key, &next)?;
            if self
                .kv
                .compare_and_swap(&key, version, Some(bytes), Expiry::Never)
                .await?
                .is_some()
            {
                return Ok(Holding::Held(instant(renewed)));
            }
        }
        Err(StorageError::Busy.into())
    }

    async fn check_run(&self, claim: &JobClaim) -> Result<Option<JobHalt>> {
        Ok(
            match self.read_job(&job_key(claim.job.id.as_str())).await? {
                None => Some(JobHalt::Lost),
                Some((record, _)) if record.state == StoredState::Cancelled => {
                    Some(JobHalt::Cancelled)
                }
                Some((record, _)) if record.run != claim.job.run => Some(JobHalt::Lost),
                Some(_) => None,
            },
        )
    }

    async fn recipient(&self, claim: &JobClaim, index: usize) -> Result<RecipientMark> {
        let (marker, rkey) = Self::marker_of(claim, index)?;
        if let Some(found) = self.kv.get(&marker).await? {
            return Ok(
                match decode::<Marker>(&marker, &found.value).map(|read| read.state) {
                    Ok(MarkerState::Sent) => RecipientMark::Sent,
                    Ok(MarkerState::Failed) => RecipientMark::Failed,
                    // A "sending" left by a runner that stopped mid-send:
                    // it may have gone out.
                    Ok(MarkerState::Sending | MarkerState::Unknown) => RecipientMark::Unknown,
                    // Something was written for this recipient (by a later
                    // release, or damaged): never sent again, and not a
                    // reason to stop every tick at this job.
                    Err(_) => {
                        tracing::warn!(
                            "a recipient's unreadable marker is read as an outcome unknown: \
                             never resent"
                        );
                        RecipientMark::Unknown
                    }
                },
            );
        }
        let limited = StoreKey::new(
            JOBS_LIMITED_NAMESPACE,
            limited_digest(claim.job.id.as_str(), &rkey),
        );
        Ok(if self.kv.get(&limited).await?.is_some() {
            RecipientMark::Limited
        } else {
            RecipientMark::Unmarked
        })
    }

    async fn mark_sending(&self, claim: &JobClaim, index: usize) -> Result<bool> {
        let (marker, _) = Self::marker_of(claim, index)?;
        let intent = Marker {
            v: VERSION,
            state: MarkerState::Sending,
            runner: Some(claim.runner.clone()),
            message_id: None,
            code: None,
        };
        Ok(self
            .kv
            .put_if_absent(
                &marker,
                encode(&marker, &intent)?,
                Expiry::After(self.retention),
            )
            .await?
            .is_some())
    }

    async fn mark_outcome(
        &self,
        claim: &JobClaim,
        index: usize,
        outcome: SendRecord,
    ) -> Result<bool> {
        let (marker, rkey) = Self::marker_of(claim, index)?;
        let Some(version) = self.sending_of(&marker, &claim.runner).await? else {
            return Ok(false);
        };
        let (state, message_id, code) = match outcome {
            SendRecord::Sent { message_id } => (MarkerState::Sent, message_id, None),
            SendRecord::Failed { code } => (MarkerState::Failed, None, code),
            SendRecord::Unknown { code } => (MarkerState::Unknown, None, code),
        };
        let written = Marker {
            v: VERSION,
            state,
            runner: None,
            message_id: message_id.as_ref().map(|id| id.as_str().to_owned()),
            code,
        };
        if self
            .kv
            .compare_and_swap(
                &marker,
                version,
                Some(encode(&marker, &written)?),
                Expiry::Keep,
            )
            .await?
            .is_none()
        {
            return Ok(false);
        }
        if let Some(id) = message_id {
            let key = message_key(id.as_str());
            let record = MessageRecord {
                v: VERSION,
                job: claim.job.id.as_str().to_owned(),
                run: claim.job.run,
                limited: limited_digest(claim.job.id.as_str(), &rkey),
            };
            let stored = match encode(&key, &record) {
                Ok(bytes) => self
                    .kv
                    .put(&key, bytes, Expiry::After(self.message_ttl))
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
        Ok(true)
    }

    async fn unmark(&self, claim: &JobClaim, index: usize) -> Result<bool> {
        let (marker, _) = Self::marker_of(claim, index)?;
        let Some(version) = self.sending_of(&marker, &claim.runner).await? else {
            return Ok(false);
        };
        Ok(self
            .kv
            .compare_and_swap(&marker, version, None, Expiry::Keep)
            .await?
            .is_some())
    }

    async fn limit_recipient(&self, claim: &JobClaim, index: usize) -> Result<()> {
        let rkey = recipient_at(claim, index)?;
        self.limit(StoreKey::new(
            JOBS_LIMITED_NAMESPACE,
            limited_digest(claim.job.id.as_str(), &rkey),
        ))
        .await
    }

    async fn settle(
        &self,
        claim: &JobClaim,
        state: JobState,
        summary: &JobSummary,
    ) -> Result<Option<JobHalt>> {
        let state = match state {
            JobState::Done => StoredState::Done,
            JobState::Stopped => StoredState::Stopped,
            _ => {
                return Err(
                    ValidationError::new("state", "a job settles as done or stopped").into(),
                );
            }
        };
        let key = job_key(claim.job.id.as_str());
        for _ in 0..CAS_ATTEMPTS {
            let Some((record, version)) = self.read_job(&key).await? else {
                return Ok(Some(JobHalt::Lost));
            };
            if record.state == StoredState::Cancelled {
                return Ok(Some(JobHalt::Cancelled));
            }
            if !record.held_by(&claim.runner, claim.number) {
                return Ok(Some(JobHalt::Lost));
            }
            let mut next = record;
            next.state = state;
            next.lease = None;
            next.summary = Some(summary.into());
            let bytes = encode(&key, &next)?;
            if self
                .kv
                .compare_and_swap(&key, version, Some(bytes), Expiry::After(self.retention))
                .await?
                .is_some()
            {
                return Ok(None);
            }
        }
        Err(StorageError::Busy.into())
    }

    /// Also: `at` is kept in whole milliseconds, and the job indexed under
    /// its minute.
    async fn retry(
        &self,
        claim: &JobClaim,
        at: OffsetDateTime,
        _summary: &JobSummary,
    ) -> Result<Option<JobHalt>> {
        let key = job_key(claim.job.id.as_str());
        let due = millis(at);
        for _ in 0..CAS_ATTEMPTS {
            let Some((record, version)) = self.read_job(&key).await? else {
                return Ok(Some(JobHalt::Lost));
            };
            if record.state == StoredState::Cancelled {
                return Ok(Some(JobHalt::Cancelled));
            }
            if !record.held_by(&claim.runner, claim.number) {
                return Ok(Some(JobHalt::Lost));
            }
            let mut next = record;
            next.state = StoredState::Pending;
            next.lease = None;
            next.retries = next.retries.saturating_add(1);
            next.due_at_ms = due;
            next.indexed = false;
            let bytes = encode(&key, &next)?;
            if self
                .kv
                .compare_and_swap(&key, version, Some(bytes), Expiry::Never)
                .await?
                .is_some()
            {
                self.index(&next.id, due).await?;
                self.mark_indexed(&key, due).await?;
                return Ok(None);
            }
        }
        Err(StorageError::Busy.into())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    fn bytes_of<T: Serialize>(value: &T) -> String {
        String::from_utf8(serde_json::to_vec(value).unwrap()).unwrap()
    }

    fn pinned<T: Serialize + DeserializeOwned + PartialEq + fmt::Debug>(value: &T, stored: &str) {
        assert_eq!(bytes_of(value), stored);
        assert_eq!(&serde_json::from_str::<T>(stored).unwrap(), value);
    }

    /// The records' exact bytes: what a store written by this release
    /// holds, and what the next release must read.
    #[test]
    fn the_job_records_are_pinned() {
        let user = UserId::new("US.13491208655302741918");
        let pending = JobRecord {
            v: 1,
            id: "promo-42".into(),
            from: PhoneNumberId::new("106540352242922").unwrap(),
            due_at_ms: 1793534400000,
            state: StoredState::Pending,
            run: 1,
            retries: 0,
            claims: 0,
            indexed: true,
            lease: None,
            recipients: vec![
                StoredRecipient::Phone("+16505551234".into()),
                StoredRecipient::User(user.clone()),
                StoredRecipient::PhoneAndUser {
                    phone: "+16505551234".into(),
                    user: user.clone(),
                },
                StoredRecipient::Group(GroupId::new("Y2FwaV9ncm91cA")),
            ],
            payload: json!({"template": "spring_sale"}),
            summary: None,
        };
        pinned(
            &pending,
            r#"{"v":1,"id":"promo-42","from":"106540352242922","due_at_ms":1793534400000,"state":"pending","run":1,"retries":0,"claims":0,"indexed":true,"recipients":[{"phone":"+16505551234"},{"user":"US.13491208655302741918"},{"phone_and_user":{"phone":"+16505551234","user":"US.13491208655302741918"}},{"group":"Y2FwaV9ncm91cA"}],"payload":{"template":"spring_sale"}}"#,
        );
        let claimed = JobRecord {
            state: StoredState::Claimed,
            claims: 3,
            lease: Some(Lease {
                runner: "replica-a".into(),
                claim: 3,
                until_ms: 1793534520000,
            }),
            recipients: vec![StoredRecipient::User(user.clone())],
            payload: Value::Null,
            ..pending.clone()
        };
        pinned(
            &claimed,
            r#"{"v":1,"id":"promo-42","from":"106540352242922","due_at_ms":1793534400000,"state":"claimed","run":1,"retries":0,"claims":3,"indexed":true,"lease":{"runner":"replica-a","claim":3,"until_ms":1793534520000},"recipients":[{"user":"US.13491208655302741918"}],"payload":null}"#,
        );
        let done = JobRecord {
            state: StoredState::Done,
            run: 2,
            retries: 1,
            lease: None,
            summary: Some(StoredSummary {
                sent: 1,
                failed: 0,
                unknown: vec![2],
                limited: vec![1],
                unsent: 0,
            }),
            ..claimed
        };
        pinned(
            &done,
            r#"{"v":1,"id":"promo-42","from":"106540352242922","due_at_ms":1793534400000,"state":"done","run":2,"retries":1,"claims":3,"indexed":true,"recipients":[{"user":"US.13491208655302741918"}],"payload":null,"summary":{"sent":1,"failed":0,"unknown":[2],"limited":[1],"unsent":0}}"#,
        );
        for (state, name) in [
            (StoredState::Stopped, "\"stopped\""),
            (StoredState::Cancelled, "\"cancelled\""),
        ] {
            pinned(&state, name);
        }
    }

    #[test]
    fn the_index_and_marker_records_are_pinned() {
        pinned(
            &Page {
                v: 1,
                ids: vec!["promo-42".into(), "reminder-1".into()],
            },
            r#"{"v":1,"ids":["promo-42","reminder-1"]}"#,
        );
        pinned(
            &Cursor {
                v: 1,
                bucket: 1793534400,
                behind: None,
            },
            r#"{"v":1,"bucket":1793534400}"#,
        );
        pinned(
            &Cursor {
                v: 1,
                bucket: 1793534460,
                behind: Some(vec![7, 12]),
            },
            r#"{"v":1,"bucket":1793534460,"behind":[7,12]}"#,
        );
        let marker = |state, runner: Option<&str>, message_id: Option<&str>, code| Marker {
            v: 1,
            state,
            runner: runner.map(Into::into),
            message_id: message_id.map(Into::into),
            code,
        };
        pinned(
            &marker(MarkerState::Sending, Some("replica-a"), None, None),
            r#"{"v":1,"state":"sending","runner":"replica-a"}"#,
        );
        pinned(
            &marker(
                MarkerState::Sent,
                None,
                Some("wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBI0QUQ2MjA4NEYyRkExNjMyREUA"),
                None,
            ),
            r#"{"v":1,"state":"sent","message_id":"wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBI0QUQ2MjA4NEYyRkExNjMyREUA"}"#,
        );
        pinned(
            &marker(MarkerState::Failed, None, None, Some(131026)),
            r#"{"v":1,"state":"failed","code":131026}"#,
        );
        pinned(
            &marker(MarkerState::Unknown, None, None, None),
            r#"{"v":1,"state":"unknown"}"#,
        );
        pinned(
            &MessageRecord {
                v: 1,
                job: "promo-42".into(),
                run: 1,
                limited: "0f".repeat(32),
            },
            &format!(
                r#"{{"v":1,"job":"promo-42","run":1,"limited":"{}"}}"#,
                "0f".repeat(32)
            ),
        );
        pinned(&Limited { v: 1 }, r#"{"v":1}"#);
    }

    /// A later release's record is left alone, and an unreadable one is
    /// told apart from it.
    #[test]
    fn a_newer_record_is_not_corrupt() {
        let key = StoreKey::new("test", "k");
        let read = |bytes: &str| {
            read_job_record(
                &key,
                &Versioned {
                    value: bytes.as_bytes().to_vec(),
                    version: 1,
                    expires_at: None,
                },
            )
        };
        assert!(matches!(read(r#"{"v":2,"shape":"new"}"#), JobRead::Newer));
        assert!(matches!(
            read(r#"{"v":1,"shape":"new"}"#),
            JobRead::Corrupt(_)
        ));
        assert!(matches!(read("not json"), JobRead::Corrupt(_)));
    }
}
