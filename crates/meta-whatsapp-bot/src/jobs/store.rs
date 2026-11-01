//! [`KvJobs`]: the jobs' records, their due index and their markers on a
//! `KvStore`, and the `131049` feedback ([`KvJobs::observe`]).

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_core::clock::Clock;
use meta_whatsapp_core::error::{SinkError, StorageError, ValidationError};
use meta_whatsapp_core::ids::{GroupId, PhoneNumberId, UserId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_core::{Error, Result};
use meta_whatsapp_webhooks::WebhookEvent;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    BUCKET_PAGE_CAPACITY, JOBS_LIMITED_NAMESPACE, Job, JobId, JobState, JobSummary,
    MARKETING_LIMIT_WAIT, ScheduledJob, bucket_of, cursor_key, instant, is_marketing_limit,
    job_key, message_key, millis, page_key,
};
use crate::command::CommandHandler;
use crate::ctx::Ctx;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Cursor {
    pub(super) v: u32,
    /// Unix seconds.
    pub(super) bucket: i64,
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

/// Durable jobs: a typed store on [`KvStore`] (no port of its own). See
/// the [module docs](super) for what it stores where.
///
/// Cheap to clone; clones share the store. Every instance on the same
/// store sees the same jobs, across processes for a shared store.
#[derive(Clone)]
pub struct KvJobs {
    pub(super) kv: Arc<dyn KvStore>,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) retention: Duration,
    pub(super) message_ttl: Duration,
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
    /// long after its due time is no longer found.
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

    /// Store `job` and index it under its due time. Its recipients are
    /// kept each person once (a later listing of the same person is
    /// dropped). A `ValidationError` for a job without recipients, with a
    /// kind of recipient it cannot store, or when another job is stored
    /// under its id; the same job scheduled again (after an error or a
    /// crash that may have stopped this half-way) is indexed again and
    /// succeeds.
    pub async fn schedule(&self, job: Job) -> Result<()> {
        if job.recipients.is_empty() {
            return Err(
                ValidationError::new("recipients", "a job needs at least one recipient").into(),
            );
        }
        let dropped: HashSet<usize> = crate::broadcast::duplicates(&job.recipients)
            .into_iter()
            .map(|(index, _)| index)
            .collect();
        let recipients = job
            .recipients
            .iter()
            .enumerate()
            .filter(|(index, _)| !dropped.contains(index))
            .map(|(_, recipient)| StoredRecipient::from_recipient(recipient))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                ValidationError::new("recipients", "a kind of recipient a job cannot store")
            })?;
        let record = JobRecord {
            v: VERSION,
            id: job.id.0.clone(),
            from: job.from,
            due_at_ms: millis(job.at),
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

    /// Cancel the job `id`: pending or claimed, it becomes cancelled (a
    /// runner running it stops before its next send). Whether it was
    /// cancelled now; `false` for a job unknown or already settled.
    pub async fn cancel(&self, id: &JobId) -> Result<bool> {
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

    /// Run the settled job `id` again at `at`: a new run (its markers are
    /// its own, so everyone is sent to again) except the recipients Meta
    /// refused with `131049` within the last 24 hours. Whether it was
    /// scheduled again; `false` for a job unknown, pending or claimed.
    pub async fn rerun(&self, id: &JobId, at: time::OffsetDateTime) -> Result<bool> {
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

    /// The job `id`, as stored.
    pub async fn get(&self, id: &JobId) -> Result<Option<ScheduledJob>> {
        Ok(self
            .read_job(&job_key(id.as_str()))
            .await?
            .map(|(record, _)| record.view()))
    }

    /// Hear a status webhook: a status whose errors carry `131049` (Meta's
    /// per-user marketing limit) for a message a job sent marks that
    /// recipient limited for [`MARKETING_LIMIT_WAIT`], from now, so a
    /// re-run of the job skips them. Whether it did; `false` for any other
    /// event, and for a message this store did not send or whose id it no
    /// longer keeps ([`Self::message_ttl`]). Safe to repeat (a redelivery
    /// does not extend the 24 hours).
    pub async fn observe(&self, event: &WebhookEvent) -> Result<bool> {
        let WebhookEvent::StatusUpdated { status, .. } = event else {
            return Ok(false);
        };
        // Meta: the status is `failed` with the code 131049
        // (`templates/marketing-templates/per-user-limits`); the code
        // decides.
        if !status.errors.iter().any(|e| is_marketing_limit(e.kind())) {
            return Ok(false);
        }
        let key = message_key(status.id.as_str());
        let Some(found) = self.kv.get(&key).await? else {
            return Ok(false);
        };
        let record: MessageRecord = decode(&key, &found.value)?;
        let limited = StoreKey::new(JOBS_LIMITED_NAMESPACE, record.limited);
        self.kv
            .put_if_absent(
                &limited,
                encode(&limited, &Limited { v: VERSION })?,
                Expiry::After(MARKETING_LIMIT_WAIT),
            )
            .await?;
        tracing::info!(
            run = record.run,
            "a job's recipient is marketing-limited (131049): skipped by a re-run for 24 hours"
        );
        Ok(true)
    }

    /// The `131049` feedback as a bot listener or an event sink
    /// ([`JobFeedback`]).
    pub fn feedback(&self) -> JobFeedback {
        JobFeedback { jobs: self.clone() }
    }

    pub(super) async fn read_job(&self, key: &StoreKey) -> Result<Option<(JobRecord, u64)>> {
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

    /// Append `id` to the due index under `due_ms`'s bucket, then move the
    /// cursor back to that bucket if it is past it.
    pub(super) async fn index(&self, id: &str, due_ms: i64) -> Result<()> {
        let bucket = bucket_of(due_ms);
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
    pub(super) async fn lower_cursor(&self, bucket: i64) -> Result<()> {
        let key = cursor_key();
        let bytes = encode(&key, &Cursor { v: VERSION, bucket })?;
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
    pub(super) async fn mark_indexed(&self, key: &StoreKey, due_ms: i64) -> Result<()> {
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
}

/// [`KvJobs::observe`] as a bot listener (`BotBuilder::listen` with
/// `Listen::event("status_updated")`) or an `EventSink<WebhookEvent>` (next
/// to your own sink in a `FanoutSink`). As a sink, a store that fails is
/// `SinkError::Delivery`: the webhook answers `500` and Meta redelivers.
/// As a bot's listener, it runs after the middleware and its errors go to
/// the bot's `ErrorHandler` (the default logs and acknowledges them: the
/// `131049` is then not recorded).
#[derive(Debug, Clone)]
pub struct JobFeedback {
    jobs: KvJobs,
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
            },
            r#"{"v":1,"bucket":1793534400}"#,
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
