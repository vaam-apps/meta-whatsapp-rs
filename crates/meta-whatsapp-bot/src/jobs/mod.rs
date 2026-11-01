//! Durable scheduling: jobs that send at a time, to one recipient or to
//! many, survive a restart, and are run by any number of runners sharing
//! one store, each recipient sent at most once.
//!
//! ```text
//! KvJobs::new(kv, clock)                      a typed store on `KvStore`
//!   .schedule(Job::new(id, from, at).to(…).payload(json))
//!   .cancel(&id)   .rerun(&id, at)   .get(&id)
//!   .observe(&event)                          a `131049` status webhook (or .feedback())
//! JobRunner::builder(jobs).client(…).pacer(…).compose(|job, to| …).build()?
//!   .tick()                                   claim the jobs due, send, record
//!   .run_until(every, stop)                   tick, sleep, until `stop`
//! ```
//!
//! ```no_run
//! # async fn demo(
//! #     kv: std::sync::Arc<dyn meta_whatsapp_core::store::KvStore>,
//! #     client: meta_whatsapp_client::Client,
//! # ) -> meta_whatsapp_core::Result<()> {
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! use meta_whatsapp_bot::{Job, JobId, JobRunner, KvJobs, Pacer};
//! use meta_whatsapp_client::messages::OutboundMessage;
//! use meta_whatsapp_client::templates::TemplateMessage;
//! use meta_whatsapp_core::clock::SystemClock;
//! use meta_whatsapp_core::ids::PhoneNumberId;
//! use meta_whatsapp_core::recipient::Recipient;
//! use serde_json::json;
//!
//! let jobs = KvJobs::new(kv, Arc::new(SystemClock));
//! let at = time::OffsetDateTime::now_utc() + Duration::from_secs(3600);
//! jobs.schedule(
//!     Job::new(JobId::new("spring-sale")?, PhoneNumberId::new("106540352242922")?, at)
//!         .to([Recipient::user("US.13491208655302741918")])
//!         .payload(json!({"template": "spring_sale", "language": "en_US"})),
//! )
//! .await?;
//!
//! // In every replica: the same composer, a runner id of its own.
//! let runner = JobRunner::builder(jobs.clone())
//!     .client(client)
//!     .pacer(Pacer::default())
//!     .compose(|job, to| {
//!         let name = job.payload["template"].as_str().unwrap_or_default();
//!         let language = job.payload["language"].as_str().unwrap_or("en_US");
//!         Ok(OutboundMessage::template(to.clone(), TemplateMessage::new(name, language)))
//!     })
//!     .runner_id("replica-1")
//!     .build()?;
//! runner
//!     .run_until(Duration::from_secs(10), std::future::pending())
//!     .await;
//! # Ok(()) }
//! ```
//!
//! A job holds a business number, a due time, its recipients (each person
//! once, by the rule of [`BroadcastBuilder::dedupe`]) and a JSON payload of
//! yours. Nothing is stored about the message itself: the runner's
//! composer makes each recipient's message from the job when it is sent,
//! so a runner started after a restart sends what one before it would
//! have. One recipient is a scheduled message; many are a broadcast that
//! resumes where it stopped.
//!
//! # How a job runs
//!
//! [`JobRunner::tick`] reads the due index from the oldest bucket that may
//! still hold a job to run up to now, and for each job due:
//!
//! 1. **Claims it** by compare-and-swap, from pending (or claimed with a
//!    lease that expired) to claimed, with a lease ([`JobRunnerBuilder::lease`])
//!    naming the runner. Two runners that read the same job: one claim
//!    succeeds, the other's compare-and-swap fails and it moves on. No
//!    send without a successful claim: a store error ends the tick.
//! 2. **Skips** every recipient that already has a marker for this run
//!    (sent, failed, or an outcome unknown; an unreadable marker is an
//!    outcome unknown), and every one Meta refused with `131049` within
//!    the last 24 hours ([`KvJobs::observe`]). Reading the markers renews
//!    the lease too, so a long scan is not taken over.
//! 3. **Sends** to the others as a [`Broadcast`] does (its pacer, its
//!    [`BroadcastPolicy`], its retries only when `Error::may_resend`
//!    holds), each send after a check that the job is still this runner's
//!    and not cancelled (renewing the lease when half of it is gone), and a
//!    per-recipient marker created with `put_if_absent` *before* the
//!    request: a recipient another runner holds is never sent to twice.
//!    With the marker in place the job is read once more: cancelled, or
//!    re-run meanwhile (a later run's markers are its own), the marker is
//!    removed and nothing is sent.
//!    After the request, the marker says sent (with the message's id),
//!    failed (Meta refused it: an error with `Error::may_have_been_sent`
//!    false), or unknown (a timeout, a 5xx: it may have gone out). A send
//!    Meta provably refused before doing anything (`Error::may_resend`)
//!    removes its marker, so it is sent again later.
//! 4. **Settles** the job: done when every recipient has a marker or was
//!    skipped; otherwise (recipients provably unsent: a throttled send out
//!    of retries, a stop, the rate limiter or the store failing) retried
//!    later with backoff ([`JobRunnerBuilder::retries`]), the attempt
//!    counted in [`ScheduledJob::retries`], and stopped after the last.
//!
//! **At most once, not exactly once.** A runner that crashes mid-send
//! leaves that recipient's marker saying "sending": whoever resumes the
//! job reads it as an outcome unknown and never sends it again
//! ([`JobSummary::unknown`]); reconcile those with the status webhooks.
//! That includes a crash after the marker and before the request left:
//! that recipient is never sent, and reported unknown all the same. A
//! timed-out send is never resent, here or by the client (give it
//! `RetryPolicy::NONE`, as [`JobRunnerBuilder::client`] does).
//!
//! **Two runners.** A lease is read against each runner's clock: a runner
//! whose clock runs ahead, or one paused longer than the lease (a send, a
//! pacer's wait), sees the job taken over. That costs a pass, never a
//! duplicate: within a run, the recipient's marker decides who sends.
//!
//! **Cancel** ([`KvJobs::cancel`]) moves a pending or claimed job to
//! cancelled by compare-and-swap; a runner running it stops before its
//! next send (a send already past its second check, with its marker in
//! place, goes out).
//!
//! **The per-user marketing limit.** A `131049` (Meta: wait at least 24
//! hours before resending, `templates/marketing-templates/per-user-limits`)
//! arrives as a `failed` status webhook, after Meta accepted the send. The
//! runner keeps each message's id (namespace [`JOBS_MESSAGE_NAMESPACE`],
//! for [`KvJobs::message_ttl`]); [`KvJobs::observe`] finds the job and the
//! recipient of a status carrying `131049` and marks that recipient
//! limited for 24 hours ([`JOBS_LIMITED_NAMESPACE`]), and a re-run of the
//! job in that time ([`KvJobs::rerun`]) skips them
//! ([`JobSummary::limited`]) and sends everyone else. A `131049` returned
//! by the send itself marks them the same way.
//!
//! # The store
//!
//! A typed store on the `KvStore` port, no port of its own; every record
//! is JSON with a version (`"v": 1`). Keys are SHA-256 (hex) of
//! length-prefixed parts (`<len>:<part>,`), as for the cooldowns, so no
//! phone number or job id is written into a key.
//!
//! | Namespace | Key | Record |
//! | --- | --- | --- |
//! | [`JOBS_NAMESPACE`] | digest of the job id | the job: number, due time, state, run, retries, claims, lease, recipients, payload, summary |
//! | [`JOBS_DUE_NAMESPACE`] | `<bucket start, unix seconds>:<page>`, and `cursor` | a minute's job ids, [`BUCKET_PAGE_CAPACITY`] a page (then the next page); the oldest bucket still to read, with the page versions of the one behind it once moved forward |
//! | [`JOBS_SENT_NAMESPACE`] | digest of the job id, the run and the recipient's key | a recipient's marker: sending, sent (message id), failed, unknown |
//! | [`JOBS_MESSAGE_NAMESPACE`] | digest of the message id | which job and recipient a sent message was for |
//! | [`JOBS_LIMITED_NAMESPACE`] | digest of the job id and the recipient's key | "marketing-limited", expiring after 24 hours |
//!
//! A recipient's key is its business-scoped user id when it has one
//! (`Recipient::User`, `Recipient::PhoneAndUser`), else its phone
//! number's digits, or the group's id: never a phone number when a BSUID
//! is known.
//!
//! **The due index.** `KvStore` has no range scan, so due times are
//! bucketed by the minute ([`BUCKET_SECONDS`]): a job's id is appended to
//! its minute's page by compare-and-swap (a due time already past: the
//! current minute's, so the cursor never moves back to a time long gone),
//! and a `cursor` record names the oldest minute that may still hold a
//! job to run. A runner reads from the cursor up to now and moves it past
//! a minute whose every job is settled (done, cancelled, stopped, or
//! moved to a later minute), keeping in the cursor the versions of that
//! minute's pages as it read them; it then reads that minute again and
//! moves the cursor back if it changed meanwhile, and every tick starts by
//! the same second read, so a runner failing between the two loses no
//! job. Scheduling into a minute behind the cursor moves the cursor back
//! to it. Index pages expire [`KvJobs::retention`] after their minute (or
//! after their writing, for a minute already past): **a job left unrun
//! that long (no runner for a month) is no longer found**, its record kept
//! but never sent.
//!
//! **Retention.** A settled job's record, the markers and the index pages
//! expire after [`KvJobs::retention`] (30 days by default); a pending or
//! claimed job's record never does. Resume a job within that time of its
//! markers' writing, or its recipients are sent again. Expired records are
//! invisible at once and removed by Redis itself, or by the memory and
//! Postgres stores' `purge_expired`. On Redis, run `maxmemory-policy
//! noeviction`: an evicted marker is a recipient sent again, an evicted
//! index page a job never found.
//!
//! These namespaces are new stable identifiers, pending the owner's
//! confirmation before the first release (`docs/architecture.md` § Stable
//! identifiers).
//!
//! [`BroadcastBuilder::dedupe`]: crate::BroadcastBuilder::dedupe
//! [`Broadcast`]: crate::Broadcast
//! [`BroadcastPolicy`]: crate::BroadcastPolicy

mod runner;
mod store;

use std::fmt;

use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::PhoneNumberId;
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::store::StoreKey;
use meta_whatsapp_core::{ErrorKind, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

pub use runner::{JobEnd, JobRun, JobRunner, JobRunnerBuilder, TickReport};
pub use store::{JobFeedback, KvJobs};

/// Store namespace of the job records (`wa.<module>.<purpose>`, like every
/// namespace of the library: `docs/architecture.md` § Stable identifiers;
/// pending the owner's confirmation before the first release). Changing it
/// strands every job scheduled.
pub const JOBS_NAMESPACE: &str = "wa.bot.jobs";

/// Store namespace of the due index: a minute's pages of job ids, and the
/// cursor. Changing it strands every job not yet run.
pub const JOBS_DUE_NAMESPACE: &str = "wa.bot.jobs.due";

/// Store namespace of the per-recipient markers. Changing it makes a job
/// resumed after the change send again to the recipients it sent to.
pub const JOBS_SENT_NAMESPACE: &str = "wa.bot.jobs.sent";

/// Store namespace of the sent messages' ids, for the `131049` status
/// webhooks. Changing it forgets the ids kept (a `131049` arriving for one
/// is then not recorded).
pub const JOBS_MESSAGE_NAMESPACE: &str = "wa.bot.jobs.message";

/// Store namespace of the marketing-limited markers. Changing it forgets
/// them (24 hours at most): a re-run sends to those recipients again.
pub const JOBS_LIMITED_NAMESPACE: &str = "wa.bot.jobs.limited";

/// The width of a due-index bucket: one minute. Part of the index's keys
/// (a bucket's key is its start, a multiple of this, in unix seconds).
pub const BUCKET_SECONDS: i64 = 60;

/// Job ids per page of a due-index bucket; a full page continues on the
/// next (`<start>:1`, …), so a bucket is never one unbounded record.
pub const BUCKET_PAGE_CAPACITY: usize = 256;

/// How long a recipient Meta refused with `131049` is skipped: Meta's
/// "wait at least 24 hours before resending"
/// (`templates/marketing-templates/per-user-limits`), from when the
/// refusal was recorded.
pub const MARKETING_LIMIT_WAIT: std::time::Duration = std::time::Duration::from_hours(24);

/// A job's id, chosen by you (a campaign's name, an order's reminder):
/// 1 to [`JobId::MAX_LEN`] bytes, no control character. It is hashed into
/// the store's keys, never written into one.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct JobId(String);

impl JobId {
    /// The longest id, in bytes.
    pub const MAX_LEN: usize = 128;

    /// Check and wrap an id: a `ValidationError` on `id` when empty, longer
    /// than [`Self::MAX_LEN`] bytes, or holding a control character.
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        if id.is_empty() || id.len() > Self::MAX_LEN || id.chars().any(char::is_control) {
            return Err(ValidationError::new(
                "id",
                format!(
                    "a job id is 1 to {} bytes without a control character",
                    Self::MAX_LEN
                ),
            )
            .into());
        }
        Ok(Self(id))
    }

    /// The id.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JobId({})", self.0)
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A job to schedule ([`KvJobs::schedule`]): from one business number, at
/// a time, to its recipients, with a payload the runner's composer reads.
#[derive(Debug, Clone)]
#[must_use]
pub struct Job {
    pub(crate) id: JobId,
    pub(crate) from: PhoneNumberId,
    pub(crate) at: OffsetDateTime,
    pub(crate) recipients: Vec<Recipient>,
    pub(crate) payload: Value,
}

impl Job {
    /// A job `id` sending from `from` at `at` (now or earlier: at the
    /// next tick), to nobody yet and with a `null` payload.
    pub fn new(id: JobId, from: PhoneNumberId, at: OffsetDateTime) -> Self {
        Self {
            id,
            from,
            at,
            recipients: Vec::new(),
            payload: Value::Null,
        }
    }

    /// Add recipients, in the order they are sent. A person listed again
    /// (the rule of [`BroadcastBuilder::dedupe`](crate::BroadcastBuilder::dedupe))
    /// is dropped when the job is scheduled: each person once.
    pub fn to<I, R>(mut self, recipients: I) -> Self
    where
        I: IntoIterator<Item = R>,
        R: Into<Recipient>,
    {
        self.recipients
            .extend(recipients.into_iter().map(Into::into));
        self
    }

    /// What the composer makes each message from (a template's name and
    /// parameters, an order id): any JSON, stored with the job. Keep
    /// secrets out of it: it is stored as given.
    pub fn payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }
}

/// Where a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobState {
    /// Waiting for its due time (or for its retry).
    Pending,
    /// A runner holds it, until its lease expires.
    Claimed,
    /// Every recipient was sent to, refused, skipped or left with an
    /// outcome unknown ([`JobSummary`]).
    Done,
    /// Given up on after its last retry, with recipients provably unsent
    /// ([`JobSummary::unsent`]).
    Stopped,
    /// Cancelled ([`KvJobs::cancel`]).
    Cancelled,
}

/// What a run of a job did, recipient by recipient (indices are into
/// [`ScheduledJob::recipients`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct JobSummary {
    /// Accepted by Meta (accepted is not delivered).
    pub sent: usize,
    /// Refused by Meta (an error proving nothing was sent), or whose
    /// message could not be composed.
    pub failed: usize,
    /// The recipients whose send may have gone out (a timeout, a 5xx, a
    /// runner that stopped mid-send): never resent; reconcile them with the
    /// status webhooks.
    pub unknown: Vec<usize>,
    /// The recipients skipped because Meta refused them with `131049`
    /// within the last 24 hours ([`KvJobs::observe`]).
    pub limited: Vec<usize>,
    /// Recipients provably not sent (a stop, a throttled send out of
    /// retries, the rate limiter or the store failing): sent at the job's
    /// retry, or never when it stopped.
    pub unsent: usize,
}

/// A job as stored ([`KvJobs::get`]), and what the runner's composer reads.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ScheduledJob {
    /// Its id.
    pub id: JobId,
    /// The business number it sends from.
    pub from: PhoneNumberId,
    /// When it is due (its retry's time, after a retry).
    pub due_at: OffsetDateTime,
    /// Where it is.
    pub state: JobState,
    /// Its run: 1, then one more per [`KvJobs::rerun`].
    pub run: u32,
    /// Retries of this run so far (each pass that left recipients unsent).
    pub retries: u32,
    /// Its recipients, each person once, in order.
    pub recipients: Vec<Recipient>,
    /// The payload it was scheduled with.
    pub payload: Value,
    /// What its last pass found, once settled (done or stopped).
    pub summary: Option<JobSummary>,
}

/// SHA-256 (hex) of `parts`, each as `<len>:<part>,` (`len` in bytes), so
/// no part can be shifted into another: the cooldowns' format.
fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(format!("{}:", part.len()));
        hasher.update(part);
        hasher.update(",");
    }
    hex::encode(hasher.finalize())
}

/// The key of a recipient's markers: its BSUID first, else its phone
/// number's digits (the number as given when it has none), else the
/// group. `None` for a kind of recipient added later.
fn recipient_key(recipient: &Recipient) -> Option<String> {
    match recipient {
        Recipient::User(user) | Recipient::PhoneAndUser { user, .. } => {
            Some(format!("user:{}", user.as_str()))
        }
        Recipient::Phone(number) => {
            let digits: String = number.chars().filter(char::is_ascii_digit).collect();
            Some(format!(
                "phone:{}",
                if digits.is_empty() { number } else { &digits }
            ))
        }
        Recipient::Group(group) => Some(format!("group:{}", group.as_str())),
        _ => None,
    }
}

fn job_key(id: &str) -> StoreKey {
    StoreKey::new(JOBS_NAMESPACE, digest(&[id]))
}

fn marker_key(id: &str, run: u32, recipient: &str) -> StoreKey {
    StoreKey::new(
        JOBS_SENT_NAMESPACE,
        digest(&[id, &run.to_string(), recipient]),
    )
}

fn limited_digest(id: &str, recipient: &str) -> String {
    digest(&[id, recipient])
}

fn message_key(message_id: &str) -> StoreKey {
    StoreKey::new(JOBS_MESSAGE_NAMESPACE, digest(&[message_id]))
}

fn page_key(bucket: i64, page: usize) -> StoreKey {
    StoreKey::new(JOBS_DUE_NAMESPACE, format!("{bucket}:{page}"))
}

fn cursor_key() -> StoreKey {
    StoreKey::new(JOBS_DUE_NAMESPACE, "cursor")
}

/// Unix milliseconds of `at`, saturating.
fn millis(at: OffsetDateTime) -> i64 {
    i64::try_from(at.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

/// The instant of `ms` unix milliseconds (the Unix epoch when out of
/// range).
fn instant(ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// The start (unix seconds) of the bucket of `ms`.
fn bucket_of(ms: i64) -> i64 {
    ms.div_euclid(1000).div_euclid(BUCKET_SECONDS) * BUCKET_SECONDS
}

/// Whether `kind` is Meta's per-user marketing limit (`131049`).
fn is_marketing_limit(kind: ErrorKind) -> bool {
    kind == ErrorKind::EcosystemEngagementLimit
}

#[cfg(test)]
mod tests {
    use super::*;
    use meta_whatsapp_core::ids::{GroupId, UserId};

    /// The keys' namespaces and derivations, pinned (`\x77` is `w`, so a
    /// search-and-replace of the library's name cannot rewrite the pin
    /// along with the constants). The digests are known answers, computed
    /// outside the crate (`printf '8:promo-42,' | sha256sum`, …), next to
    /// their preimages.
    #[test]
    fn the_job_keys_are_pinned_and_hashed() {
        assert_eq!(job_key("promo-42").namespace(), "\x77a.bot.jobs");
        assert_eq!(
            job_key("promo-42").key(),
            "7f8f047dd5fcfb2f9f1009c97b37b9c9506460a160b92e834b2442537f0ed643"
        );
        assert_eq!(
            job_key("promo-42").key(),
            hex::encode(Sha256::digest("8:promo-42,"))
        );
        let marker = marker_key("promo-42", 1, "user:US.13491208655302741918");
        assert_eq!(marker.namespace(), "\x77a.bot.jobs.sent");
        assert_eq!(
            marker.key(),
            "0b1f8a37d4165f3995d10dd00739b916c75dbaa816589ce74aa4daa7e59b75dd"
        );
        assert_eq!(
            marker.key(),
            hex::encode(Sha256::digest(
                "8:promo-42,1:1,28:user:US.13491208655302741918,"
            ))
        );
        assert_eq!(
            limited_digest("promo-42", "phone:16505551234"),
            "5ff9dca8d0a818819c7e52123675ff46b1e387493e081c9e86d66b2b68fb0ed4"
        );
        assert_eq!(
            limited_digest("promo-42", "phone:16505551234"),
            hex::encode(Sha256::digest("8:promo-42,17:phone:16505551234,"))
        );
        let message = message_key("wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBI0QUQ2MjA4NEYyRkExNjMyREUA");
        assert_eq!(message.namespace(), "\x77a.bot.jobs.message");
        assert_eq!(
            message.key(),
            "55f8d32701a21ec7c09c0bcf471ee8f91c68c7aefb25368ecf3cce67605e86e5"
        );
        assert_eq!(
            message.key(),
            hex::encode(Sha256::digest(
                "58:wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBI0QUQ2MjA4NEYyRkExNjMyREUA,"
            ))
        );
        assert_eq!(JOBS_LIMITED_NAMESPACE, "\x77a.bot.jobs.limited");
        let page = page_key(1790812800, 0);
        assert_eq!(page.namespace(), "\x77a.bot.jobs.due");
        assert_eq!(page.key(), "1790812800:0");
        assert_eq!(cursor_key().key(), "cursor");
        assert_eq!(cursor_key().namespace(), "\x77a.bot.jobs.due");
        // Length prefixes: moving a character between parts changes the key.
        assert_ne!(
            marker_key("promo-4", 21, "user:x"),
            marker_key("promo-42", 1, "user:x")
        );
    }

    /// A BSUID first, never the phone number when one is known; the
    /// digits of a phone number alone.
    #[test]
    fn a_recipient_is_keyed_by_bsuid_first() {
        let user = UserId::new("US.13491208655302741918");
        assert_eq!(
            recipient_key(&Recipient::User(user.clone())).unwrap(),
            "user:US.13491208655302741918"
        );
        assert_eq!(
            recipient_key(&Recipient::PhoneAndUser {
                phone: "+16505551234".into(),
                user
            })
            .unwrap(),
            "user:US.13491208655302741918"
        );
        assert_eq!(
            recipient_key(&Recipient::phone("+1 650-555-1234")).unwrap(),
            "phone:16505551234"
        );
        assert_eq!(
            recipient_key(&Recipient::Group(GroupId::new("Y2FwaV9ncm91cA"))).unwrap(),
            "group:Y2FwaV9ncm91cA"
        );
    }

    #[test]
    fn buckets_are_minutes_in_unix_seconds() {
        assert_eq!(BUCKET_SECONDS, 60);
        assert_eq!(bucket_of(1_790_812_859_999), 1_790_812_800);
        assert_eq!(bucket_of(1_790_812_860_000), 1_790_812_860);
        assert_eq!(bucket_of(-1), -60);
    }

    #[test]
    fn job_ids_are_checked() {
        assert!(JobId::new("promo-42").is_ok());
        assert!(JobId::new("").is_err());
        assert!(JobId::new("a\nb").is_err());
        assert!(JobId::new("x".repeat(JobId::MAX_LEN)).is_ok());
        assert!(JobId::new("x".repeat(JobId::MAX_LEN + 1)).is_err());
    }
}
