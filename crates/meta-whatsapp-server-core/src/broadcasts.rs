//! Broadcasts (docs/design/server.md, section 4.2; roadmap M5k, slice 1:
//! the broadcast API): a template sent to many people from one number of
//! the caller's tenant, queued, paced and recorded per recipient, over the
//! bot crate's durable jobs and shared pacer.
//!
//! | Service | Operation | What it does |
//! | --- | --- | --- |
//! | [`Broadcasts`] | [`Broadcasts::create`] | validates every message as a send validates it, queues a job for the number |
//! | | [`Broadcasts::get`] | the broadcast's state and counts (queued, sent, failed by Meta's error kind), never a message |
//! | | [`Broadcasts::cancel`] | stops it before its next send |
//! | [`BroadcastRecords`] | [`BroadcastRecords::runner`] | the replica's workers: claim due broadcasts, send, record |
//!
//! # How it is built
//!
//! No port of its own and no table (D420): the library's `meta-whatsapp-bot`
//! jobs, a typed store on the bundle's `KvStore` (`KvJobs`), and its
//! `KvRateLimiter` on the same store, so every replica books from one
//! budget per number. The service adds three small typed stores of its own
//! on the same `KvStore` ([`RECORD_NAMESPACE`], [`TENANT_NAMESPACE`],
//! [`PROGRESS_NAMESPACE`]).
//!
//! **A broadcast is its tenant's or it does not exist** (the design's
//! M5.2). Its record names the tenant and the number, written when a
//! caller of that tenant created it on a number it owned
//! ([`Authorizer::admit_number`], the number's own tenant, never a
//! request's word). Reading or cancelling one asks the record: another
//! tenant's id, another number's id and an id that was never made are all
//! `404 not_found`, before the jobs are read. A queued send finds its
//! token the same way, from the record's tenant
//! (`Authorizer::broadcast_sender`, steps 4 and 5): a number unbound or
//! attached by another tenant since sends nothing.
//!
//! **Rate** (D421). Each number's budget is the deployment's
//! ([`BroadcastSettings::rate_per_second`], messages a second, 10 by
//! default), booked in the shared store before every send, so several
//! replicas together, and several broadcasts on the number, never pass
//! it. It does not know the sends of the `/messages` route.
//!
//! **At most once per recipient** (D422). The jobs' markers: a marker is
//! created with `put_if_absent` before the request, and says sent, failed
//! or *unknown* after. A replica that dies between the marker and the
//! request, or after the request and before the outcome, leaves the
//! recipient *unknown*: never sent again (reported as `unknown`). The
//! residual is a recipient who never got the message, not a duplicate.
//!
//! **What is stored** (D424). The job holds the recipients and the
//! template with its per-recipient parameters, as the tenant gave them,
//! until it settles and then for 30 days (`KvJobs::retention`): a store
//! with encryption at rest is the deployment's choice. No log, event,
//! error or answer of the service carries a recipient, a parameter or a
//! message; the status answers counts and Meta's error kinds.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::bot::{
    Backoff, Job, JobId, JobRunner, JobState, JobStore, KvJobs, KvRateLimiter, Outbound, Pacer,
    Rate, ScheduledJob, Timer,
};
use meta_whatsapp_rs::client::RetryPolicy;
use meta_whatsapp_rs::client::messages::{OutboundMessage, SendResponse};
use meta_whatsapp_rs::core::GraphApiError;
use meta_whatsapp_rs::core::clock::Clock;
use meta_whatsapp_rs::core::error::ValidationError;
use meta_whatsapp_rs::core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_rs::core::recipient::Recipient;
use meta_whatsapp_rs::core::store::{Expiry, JsonStore, KvStore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::audit::{BroadcastFacts, Subject, broadcast_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::error::{ServiceError, kind_code};
use crate::messages::{
    MessageType, RecipientObject, SendMessage, billed_to_binding, message, names_an_object,
};
use crate::model::TenantId;

/// The log target of this module (the API module's, as the routes').
const LOG_TARGET: &str = "meta_whatsapp_server::api::broadcasts";

/// The `KvStore` namespace of the broadcasts' records: who made each, on
/// which number. A stable identifier, pending the owner's confirmation
/// before the first release (D420).
pub const RECORD_NAMESPACE: &str = "wa.server.broadcast";

/// The `KvStore` namespace of each tenant's list of live broadcasts (the
/// concurrency limit's), keyed by the SHA-256 of the tenant id.
pub const TENANT_NAMESPACE: &str = "wa.server.broadcast.tenant";

/// The `KvStore` namespace of a broadcast's running counts, by outcome and
/// by the error kind of a failure.
pub const PROGRESS_NAMESPACE: &str = "wa.server.broadcast.progress";

/// A broadcast's messages carry this, then the broadcast's id, as their
/// `biz_opaque_callback_data`: Meta echoes it in each message's status
/// events (the tenant can tell a broadcast's messages apart), and the
/// sender finds the broadcast's record by it.
pub const CALLBACK_PREFIX: &str = "wa-broadcast:";

/// The audit action of a broadcast created.
pub const CREATED: &str = "broadcast_created";

/// The audit action of a broadcast cancelled.
pub const CANCELLED: &str = "broadcast_cancelled";

/// How long a record without its job is believed to be a create in flight.
const CREATE_GRACE_MS: i64 = 5 * 60 * 1000;

/// How long a broadcast's record and counts are kept: twice the 30 days the
/// jobs keep a settled job (`KvJobs::retention`), and past the month after
/// which a job nobody ran is no longer found, so the record never goes
/// before its job.
const RECORD_TTL: Duration = Duration::from_hours(60 * 24);

/// Attempts at a compare-and-swap before giving up.
const CAS_ATTEMPTS: usize = 8;

/// The `Retry-After` of a refusal for too many broadcasts at once.
const BUSY_RETRY_AFTER_SECS: u64 = 30;

/// How many times a booking of the shared budget reads again after losing
/// the swap to another sender, before it fails closed (the library's default
/// is 8; see [`BroadcastRecords::with_jobs`]).
const CONTENTION_ATTEMPTS: u32 = 48;

/// How often an idle worker looks for a broadcast due.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

// ─── The settings ────────────────────────────────────────────────────────

/// The deployment's broadcast settings (D421, D425), conservative by
/// default. `WA_SERVER_BROADCAST_*` in the binary.
#[derive(Clone)]
pub struct BroadcastSettings {
    /// Messages a second for each number, shared by every replica.
    pub rate_per_second: u32,
    /// The most people one broadcast may be for.
    pub max_recipients: usize,
    /// The most broadcasts a tenant may have queued or running at once.
    pub max_concurrent: usize,
    /// Workers each replica runs (broadcasts it sends at once).
    pub workers: usize,
    /// The pacer's timer; the system's when none. A test hands a
    /// `ManualClock` that is also the service's clock.
    pub timer: Option<Arc<dyn Timer>>,
}

impl BroadcastSettings {
    /// The default rate: 10 messages a second for each number (Meta's own
    /// default throughput is 80).
    pub const DEFAULT_RATE: u32 = 10;
    /// The default most people in one broadcast.
    pub const DEFAULT_MAX_RECIPIENTS: usize = 1000;
    /// The default most broadcasts a tenant has at once.
    pub const DEFAULT_MAX_CONCURRENT: usize = 3;
    /// The default workers a replica runs.
    pub const DEFAULT_WORKERS: usize = 2;
}

impl Default for BroadcastSettings {
    fn default() -> Self {
        Self {
            rate_per_second: Self::DEFAULT_RATE,
            max_recipients: Self::DEFAULT_MAX_RECIPIENTS,
            max_concurrent: Self::DEFAULT_MAX_CONCURRENT,
            workers: Self::DEFAULT_WORKERS,
            timer: None,
        }
    }
}

impl fmt::Debug for BroadcastSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BroadcastSettings")
            .field("rate_per_second", &self.rate_per_second)
            .field("max_recipients", &self.max_recipients)
            .field("max_concurrent", &self.max_concurrent)
            .field("workers", &self.workers)
            .field("timer", &self.timer.is_some())
            .finish()
    }
}

// ─── The request ─────────────────────────────────────────────────────────

/// One person of a broadcast, as the request named them.
#[derive(Clone, Default, PartialEq)]
pub struct BroadcastRecipient {
    /// E.164 **with** its `+`.
    pub phone: Option<String>,
    /// A business-scoped user id (BSUID). With a phone number, Meta uses
    /// the phone number.
    pub user_id: Option<String>,
    /// Replaces the template's `components` for this person (their
    /// parameters).
    pub components: Option<Value>,
}

impl fmt::Debug for BroadcastRecipient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Neither the person nor their parameters.
        f.debug_struct("BroadcastRecipient").finish_non_exhaustive()
    }
}

/// A checked request to broadcast. Only [`Self::new`] makes one.
#[derive(Clone, PartialEq)]
pub struct NewBroadcast {
    template: Value,
    recipients: Vec<Recipient>,
    overrides: BTreeMap<String, Value>,
}

impl fmt::Debug for NewBroadcast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewBroadcast")
            .field("recipients", &self.recipients.len())
            .finish_non_exhaustive()
    }
}

/// The key a recipient's own parameters are kept under: its BSUID when it
/// has one, else its phone number, as the jobs key a recipient.
fn person_key(recipient: &Recipient) -> Option<String> {
    match recipient {
        Recipient::User(user) | Recipient::PhoneAndUser { user, .. } => {
            Some(format!("user:{}", user.as_str()))
        }
        Recipient::Phone(phone) => Some(format!("phone:{phone}")),
        _ => None,
    }
}

/// The request's way of naming `recipient`, to be checked again as a
/// send's is.
fn object_of(recipient: &Recipient) -> Option<RecipientObject> {
    match recipient {
        Recipient::Phone(phone) => Some(RecipientObject {
            phone: Some(phone.clone()),
            ..RecipientObject::default()
        }),
        Recipient::User(user) => Some(RecipientObject {
            user_id: Some(user.as_str().to_owned()),
            ..RecipientObject::default()
        }),
        Recipient::PhoneAndUser { phone, user } => Some(RecipientObject {
            phone: Some(phone.clone()),
            user_id: Some(user.as_str().to_owned()),
            ..RecipientObject::default()
        }),
        _ => None,
    }
}

/// The library's message for `to`: the broadcast's template (its
/// `components` replaced by `components` when given), checked as a send's
/// is, carrying the broadcast's callback data.
fn compose_message(
    template: &Value,
    components: Option<&Value>,
    to: RecipientObject,
    callback: &str,
) -> Result<OutboundMessage, ServiceError> {
    let mut template = template.clone();
    if let (Some(components), Some(object)) = (components, template.as_object_mut()) {
        object.insert("components".to_owned(), components.clone());
    }
    let mut send = SendMessage::new(to, MessageType::Template);
    send.template = Some(template);
    send.callback_data = Some(callback.to_owned());
    message(send)
}

impl NewBroadcast {
    /// Check `template` (Meta's template object: `name`, `language`,
    /// `components`, as a send takes it) for each of `recipients` as a
    /// send checks it, against `settings`' bound.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `recipients` (none, or more than
    /// [`BroadcastSettings::max_recipients`]), on `recipients[i]` (a
    /// phone number that is not E.164, a blank BSUID, neither, a
    /// message the library refuses for that person) or on the template's
    /// own fields (the first person's message), and on `template` (a
    /// template naming a catalog: not offered in a broadcast).
    pub fn new(
        template: Value,
        recipients: Vec<BroadcastRecipient>,
        settings: &BroadcastSettings,
    ) -> Result<Self, ServiceError> {
        if recipients.is_empty() || recipients.len() > settings.max_recipients {
            return Err(ServiceError::invalid("recipients"));
        }
        let mut people = Vec::with_capacity(recipients.len());
        let mut overrides = BTreeMap::new();
        let mut listed = std::collections::BTreeSet::new();
        for (index, recipient) in recipients.into_iter().enumerate() {
            let at = || format!("recipients[{index}]");
            let to = RecipientObject {
                phone: recipient.phone,
                user_id: recipient.user_id,
                group_id: None,
            };
            let person =
                crate::messages::recipient(to.clone()).map_err(|_| ServiceError::invalid(at()))?;
            let built = compose_message(
                &template,
                recipient.components.as_ref(),
                to,
                &format!("{CALLBACK_PREFIX}check"),
            );
            let built = match built {
                Ok(built) => built,
                // The template's own fields, as the first message names them.
                Err(error) if index == 0 && recipient.components.is_none() => return Err(error),
                Err(_) => return Err(ServiceError::invalid(at())),
            };
            if names_an_object(&built) {
                return Err(ServiceError::invalid("template"));
            }
            // A person listed again is sent once, by their first listing
            // (the jobs drop the later ones): so with the first one's
            // parameters, never a later listing's.
            if let Some(key) = person_key(&person)
                && listed.insert(key.clone())
                && let Some(components) = recipient.components
            {
                overrides.insert(key, components);
            }
            people.push(person);
        }
        Ok(Self {
            template,
            recipients: people,
            overrides,
        })
    }

    /// How many people it names (before the jobs keep each person once).
    pub fn recipients(&self) -> usize {
        self.recipients.len()
    }

    fn payload(&self) -> Value {
        json!({"v": 1, "template": self.template, "components": self.overrides})
    }
}

/// The message a stored broadcast sends to `to`: the stored template, with
/// the person's own parameters when it has them.
fn compose(job: &ScheduledJob, to: &Recipient) -> Result<OutboundMessage, Error> {
    let invalid = || Error::from(ValidationError::new("template", "the broadcast's message"));
    let object = object_of(to).ok_or_else(invalid)?;
    let template = job.payload.get("template").ok_or_else(invalid)?;
    let components = person_key(to).and_then(|key| job.payload.get("components")?.get(key));
    compose_message(
        template,
        components,
        object,
        &format!("{CALLBACK_PREFIX}{}", job.id),
    )
    .map_err(|_| invalid())
}

// ─── The records ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    v: u8,
    tenant: String,
    number: String,
    created_ms: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TenantIndex {
    v: u8,
    ids: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Progress {
    v: u8,
    sent: u64,
    unknown: u64,
    failed: BTreeMap<String, u64>,
}

/// A new broadcast id: `bc_` and 32 hex characters (128 random bits).
fn new_id() -> Result<String, ServiceError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ServiceError::internal())?;
    Ok(format!("bc_{}", hex::encode(bytes)))
}

/// Whether `id` has the shape of [`new_id`]'s.
fn is_broadcast_id(id: &str) -> bool {
    id.strip_prefix("bc_")
        .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn tenant_key(tenant: &TenantId) -> String {
    hex::encode(Sha256::digest(tenant.as_str()))
}

fn millis(at: OffsetDateTime) -> i64 {
    i64::try_from(at.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

/// The broadcasts' typed stores and the machinery that sends them, over the
/// bundle's `KvStore` (no port, no table). Cheap to clone.
#[derive(Clone)]
pub struct BroadcastRecords {
    records: JsonStore<Record>,
    index: JsonStore<TenantIndex>,
    progress: JsonStore<Progress>,
    jobs: Arc<dyn JobStore>,
    clock: Arc<dyn Clock>,
    pacer: Pacer,
    settings: BroadcastSettings,
}

impl fmt::Debug for BroadcastRecords {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BroadcastRecords")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl BroadcastRecords {
    /// The records and the jobs in `kv`, dated by `clock`, paced by
    /// `settings`. The jobs are the bot crate's `KvJobs`; use
    /// [`Self::with_jobs`] for another [`JobStore`].
    ///
    /// # Errors
    ///
    /// `500 internal` for a rate of zero.
    pub fn new(
        kv: &Arc<dyn KvStore>,
        clock: Arc<dyn Clock>,
        settings: BroadcastSettings,
    ) -> Result<Self, ServiceError> {
        let jobs: Arc<dyn JobStore> = Arc::new(KvJobs::new(kv.clone(), clock.clone()));
        Self::with_jobs(kv, jobs, clock, settings)
    }

    /// As [`Self::new`], the broadcasts queued in `jobs` (the swap, D420).
    ///
    /// # Errors
    ///
    /// `500 internal` for a rate of zero.
    pub fn with_jobs(
        kv: &Arc<dyn KvStore>,
        jobs: Arc<dyn JobStore>,
        clock: Arc<dyn Clock>,
        settings: BroadcastSettings,
    ) -> Result<Self, ServiceError> {
        let rate =
            Rate::per_second(settings.rate_per_second).map_err(|_| ServiceError::internal())?;
        let timer: Arc<dyn Timer> = settings
            .timer
            .clone()
            .unwrap_or_else(|| Arc::new(meta_whatsapp_rs::core::clock::SystemClock));
        // The runner books from up to the runner's 8 sends at once,
        // and every replica's workers book from the same record: a booking
        // that loses the compare-and-swap reads again (jittered), and the
        // library's 8 attempts are too few for that many contenders on a
        // database a millisecond away (a broadcast would end `LimiterFailed`
        // and wait for its retry). Failing closed stays: past the bound
        // nothing is sent.
        let limiter = KvRateLimiter::new(kv.clone(), rate)
            .with_timer(timer.clone())
            .contention(CONTENTION_ATTEMPTS, Duration::from_millis(2))
            .map_err(|_| ServiceError::internal())?;
        let pacer = Pacer::new(limiter).with_timer(timer);
        Ok(Self {
            records: JsonStore::new(kv.clone(), RECORD_NAMESPACE),
            index: JsonStore::new(kv.clone(), TENANT_NAMESPACE),
            progress: JsonStore::new(kv.clone(), PROGRESS_NAMESPACE),
            jobs,
            clock,
            pacer,
            settings,
        })
    }

    /// The settings it was made with.
    pub fn settings(&self) -> &BroadcastSettings {
        &self.settings
    }

    /// A worker: claims the broadcasts due, sends them as their tenants'
    /// numbers (each send finds its token by the broadcast's record, steps
    /// 4 and 5), and records each person. `id` names it among the
    /// replicas' workers; `names_account` is
    /// `WA_SERVER_SEND_MESSAGING_ACCOUNT_ID`.
    ///
    /// # Errors
    ///
    /// `500 internal` if the runner cannot be built.
    pub fn runner(
        &self,
        authz: Arc<Authorizer>,
        names_account: bool,
        id: impl Into<String>,
    ) -> Result<JobRunner, ServiceError> {
        let outbound = TenantOutbound {
            authz,
            records: self.clone(),
            names_account,
        };
        JobRunner::builder_shared(self.jobs.clone())
            .shared_outbound(Arc::new(outbound))
            .pacer(self.pacer.clone())
            .compose(compose)
            .policy(Backoff::default())
            .runner_id(id)
            .build()
            .map_err(|_| ServiceError::internal())
    }

    /// The workers a replica runs ([`BroadcastSettings::workers`]), each
    /// under an id of its own: one is a broadcast at a time, so several
    /// let one tenant's long broadcast leave the others' moving. Run each
    /// with `JobRunner::run_until(`[`POLL_INTERVAL`]`, stop)`.
    ///
    /// # Errors
    ///
    /// `500 internal` if a runner cannot be built.
    pub fn workers(
        &self,
        authz: &Arc<Authorizer>,
        names_account: bool,
    ) -> Result<Vec<JobRunner>, ServiceError> {
        let mut tag = [0u8; 6];
        getrandom::fill(&mut tag).map_err(|_| ServiceError::internal())?;
        let tag = hex::encode(tag);
        (0..self.settings.workers.max(1))
            .map(|n| self.runner(authz.clone(), names_account, format!("{tag}-{n}")))
            .collect()
    }

    async fn record(&self, id: &str) -> Result<Option<Record>, ServiceError> {
        if !is_broadcast_id(id) {
            return Ok(None);
        }
        Ok(self.records.get(id).await?.map(|(record, _, _)| record))
    }

    /// The tenant's slots: the ones still live, and `id` added unless the
    /// tenant has [`BroadcastSettings::max_concurrent`] already.
    async fn reserve(&self, tenant: &TenantId, id: &str) -> Result<(), ServiceError> {
        let key = tenant_key(tenant);
        for _ in 0..CAS_ATTEMPTS {
            let (index, version) = match self.index.get(&key).await? {
                Some((index, version, _)) => (index, Some(version)),
                None => (TenantIndex::default(), None),
            };
            let mut live = Vec::new();
            for known in &index.ids {
                if self.is_live(known).await? {
                    live.push(known.clone());
                }
            }
            if live.len() >= self.settings.max_concurrent {
                return Err(ServiceError::too_many_requests(BUSY_RETRY_AFTER_SECS));
            }
            live.push(id.to_owned());
            let next = TenantIndex { v: 1, ids: live };
            let written = match version {
                Some(version) => {
                    self.index
                        .compare_and_swap(&key, version, Some(&next), Expiry::Never)
                        .await?
                }
                None => self.index.put_if_absent(&key, &next, Expiry::Never).await?,
            };
            if written.is_some() {
                return Ok(());
            }
        }
        Err(ServiceError::new(crate::error::ErrorCode::STORAGE_UNAVAILABLE).retryable(true))
    }

    /// Whether the broadcast `id` still counts against its tenant's limit:
    /// queued or running, or a create that has not queued yet (for five
    /// minutes).
    async fn is_live(&self, id: &str) -> Result<bool, ServiceError> {
        let Some(record) = self.record(id).await? else {
            return Ok(false);
        };
        let Ok(job_id) = JobId::new(id) else {
            return Ok(false);
        };
        Ok(match self.jobs.get(&job_id).await? {
            Some(job) => !job.state.is_settled(),
            None => millis(self.clock.now()) - record.created_ms < CREATE_GRACE_MS,
        })
    }

    /// Give back the slot of a broadcast that was never queued.
    async fn release(&self, tenant: &TenantId, id: &str) {
        let key = tenant_key(tenant);
        for _ in 0..CAS_ATTEMPTS {
            let Ok(Some((mut index, version, _))) = self.index.get(&key).await else {
                return;
            };
            index.ids.retain(|known| known != id);
            if matches!(
                self.index
                    .compare_and_swap(&key, version, Some(&index), Expiry::Never)
                    .await,
                Ok(Some(_) | None)
            ) {
                return;
            }
        }
    }

    /// Count an outcome in the broadcast's progress (best effort: a
    /// failure here is logged, never a reason to resend or to stop).
    async fn count(&self, id: &str, outcome: Counted) {
        for _ in 0..CAS_ATTEMPTS {
            let (mut progress, version) = match self.progress.get(id).await {
                Ok(Some((progress, version, _))) => (progress, Some(version)),
                Ok(None) => (
                    Progress {
                        v: 1,
                        ..Progress::default()
                    },
                    None,
                ),
                Err(_) => break,
            };
            match &outcome {
                Counted::Sent => progress.sent += 1,
                Counted::Indeterminate => progress.unknown += 1,
                Counted::Failed(kind) => *progress.failed.entry(kind.clone()).or_default() += 1,
            }
            let written = match version {
                Some(version) => {
                    self.progress
                        .compare_and_swap(id, version, Some(&progress), Expiry::After(RECORD_TTL))
                        .await
                }
                None => {
                    self.progress
                        .put_if_absent(id, &progress, Expiry::After(RECORD_TTL))
                        .await
                }
            };
            match written {
                Ok(Some(_)) => return,
                Ok(None) => {}
                Err(_) => break,
            }
        }
        tracing::warn!(target: LOG_TARGET, "a broadcast's count could not be recorded");
    }

    async fn status(&self, id: &str, record: &Record) -> Result<BroadcastStatus, ServiceError> {
        let job_id = JobId::new(id).map_err(|_| ServiceError::not_found())?;
        let job = self
            .jobs
            .get(&job_id)
            .await?
            .ok_or_else(ServiceError::not_found)?;
        let progress = self
            .progress
            .get(id)
            .await?
            .map(|(progress, _, _)| progress)
            .unwrap_or_default();
        let failures = progress.failed.clone();
        let (sent, failed, unknown, limited) = match &job.summary {
            Some(summary) => (
                summary.sent,
                summary.failed,
                summary.unknown.len(),
                summary.limited.len(),
            ),
            None => (
                usize::try_from(progress.sent).unwrap_or(usize::MAX),
                usize::try_from(progress.failed.values().sum::<u64>()).unwrap_or(usize::MAX),
                usize::try_from(progress.unknown).unwrap_or(usize::MAX),
                0,
            ),
        };
        let recipients = job.recipients.len();
        let queued = recipients.saturating_sub(sent + failed + unknown + limited);
        Ok(BroadcastStatus {
            id: id.to_owned(),
            number: PhoneNumberId::new(record.number.clone())
                .map_err(|_| ServiceError::internal())?,
            state: match job.state {
                JobState::Pending => BroadcastState::Queued,
                JobState::Claimed => BroadcastState::Running,
                JobState::Done => BroadcastState::Completed,
                JobState::Cancelled => BroadcastState::Cancelled,
                _ => BroadcastState::Stopped,
            },
            created_at: OffsetDateTime::from_unix_timestamp_nanos(
                i128::from(record.created_ms) * 1_000_000,
            )
            .unwrap_or(OffsetDateTime::UNIX_EPOCH),
            recipients,
            queued,
            sent,
            failed,
            unknown,
            limited,
            failures,
        })
    }
}

enum Counted {
    Sent,
    Indeterminate,
    Failed(String),
}

// ─── The answers ─────────────────────────────────────────────────────────

/// Where a broadcast stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastState {
    /// Waiting for a worker, or for its retry.
    Queued,
    /// A worker is sending it.
    Running,
    /// Every person was sent to, refused or reported unknown.
    Completed,
    /// Cancelled; nothing more is sent.
    Cancelled,
    /// Given up after its retries (a number unusable, the limiter or the
    /// store failing): the people left are in `queued`.
    Stopped,
}

impl BroadcastState {
    /// Its name in the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Stopped => "stopped",
        }
    }
}

/// A broadcast's state and counts: no person, no parameter, no message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastStatus {
    /// Its id.
    pub id: String,
    /// The number it sends from.
    pub number: PhoneNumberId,
    /// Where it stands.
    pub state: BroadcastState,
    /// When it was made.
    pub created_at: OffsetDateTime,
    /// How many people it is for (each once).
    pub recipients: usize,
    /// Not sent yet.
    pub queued: usize,
    /// Accepted by Meta.
    pub sent: usize,
    /// Refused by Meta, or not sendable.
    pub failed: usize,
    /// Sent or not, nobody knows: never sent again.
    pub unknown: usize,
    /// Skipped for Meta's per-user marketing limit.
    pub limited: usize,
    /// The failures, by the service's error code of Meta's refusal.
    pub failures: BTreeMap<String, u64>,
}

// ─── The sender ──────────────────────────────────────────────────────────

/// What the workers send through: each send finds the broadcast's record,
/// then the token of the record's tenant for the number, and counts what
/// came of it. Never the token of the process that asked.
struct TenantOutbound {
    authz: Arc<Authorizer>,
    records: BroadcastRecords,
    names_account: bool,
}

impl fmt::Debug for TenantOutbound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TenantOutbound").finish_non_exhaustive()
    }
}

/// What Meta would say of a number whose token cannot be used.
fn unusable() -> Error {
    Error::from(GraphApiError::new(
        190,
        "the broadcast's number has no usable token",
    ))
}

#[async_trait]
impl Outbound for TenantOutbound {
    async fn send(
        &self,
        from: &PhoneNumberId,
        sent: &OutboundMessage,
    ) -> Result<SendResponse, Error> {
        let id = sent
            .biz_opaque_callback_data
            .as_deref()
            .and_then(|data| data.strip_prefix(CALLBACK_PREFIX))
            .ok_or_else(unusable)?;
        let record = self
            .records
            .record(id)
            .await
            .ok()
            .flatten()
            .filter(|record| record.number == from.as_str())
            .ok_or_else(unusable)?;
        let tenant = TenantId::parse(&record.tenant).ok_or_else(unusable)?;
        let owned = match self.authz.broadcast_sender(&tenant, from.clone()).await {
            Ok(owned) => owned,
            Err(refused) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    code = refused.code(),
                    "a broadcast's number cannot send: nothing sent"
                );
                let error = unusable();
                self.records
                    .count(id, Counted::Failed(kind_code(error.kind()).to_owned()))
                    .await;
                return Err(error);
            }
        };
        let billed = billed_to_binding(sent, &owned, self.names_account);
        let result = owned
            .client()
            .clone()
            .with_retry(RetryPolicy::NONE)
            .messages(from.clone())
            .send(&billed)
            .await;
        match result {
            Ok(response) => {
                self.records.count(id, Counted::Sent).await;
                Ok(response)
            }
            Err(error) => {
                // A `190` marks the WABA's numbers, as any send's does; Meta's
                // texts stay out of the logs: they may quote the person.
                let _ = owned.failed_withholding_texts(&self.authz, &error).await;
                // Only a failure that will stay is counted: a throttled
                // send is resent, and counted when it ends.
                if !error.may_resend() {
                    let counted = if error.may_have_been_sent() {
                        Counted::Indeterminate
                    } else {
                        Counted::Failed(kind_code(error.kind()).to_owned())
                    };
                    self.records.count(id, counted).await;
                }
                Err(error)
            }
        }
    }

    async fn mark_read(&self, _: &PhoneNumberId, _: &MessageId, _: bool) -> Result<(), Error> {
        Err(Error::from(ValidationError::new(
            "mark_read",
            "a broadcast sends templates only",
        )))
    }
}

// ─── The service ─────────────────────────────────────────────────────────

/// The broadcast operations for `caller`, whose capabilities `authz`
/// admits. Cheap: a few references.
pub struct Broadcasts<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    records: &'a BroadcastRecords,
}

impl fmt::Debug for Broadcasts<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Broadcasts").finish_non_exhaustive()
    }
}

impl<'a> Broadcasts<'a> {
    /// The operations for `caller`.
    ///
    /// # Errors
    ///
    /// `caller` another [`Authorizer`]'s: `403 forbidden`, logged at
    /// `warn`.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        records: &'a BroadcastRecords,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        Ok(Self {
            authz,
            caller,
            records,
        })
    }

    /// Queue `request` from `number`, to be sent at the number's rate.
    ///
    /// # Errors
    ///
    /// `403 forbidden` for a `number` another [`Authorizer`] made, `404
    /// not_found` for one that is not the caller's tenant's; `429
    /// too_many_requests` (`Retry-After`) with the tenant's
    /// [`BroadcastSettings::max_concurrent`] broadcasts live; `503
    /// storage_unavailable`. Nothing is sent by a refusal.
    pub async fn create(
        &self,
        number: &OwnedNumber,
        request: NewBroadcast,
    ) -> Result<BroadcastStatus, ServiceError> {
        self.authz.admit_number(number)?;
        if number.tenant() != self.caller.tenant() {
            tracing::warn!(target: LOG_TARGET, "refused a broadcast on another tenant's number");
            return Err(ServiceError::not_found());
        }
        let tenant = self.caller.tenant();
        let id = new_id()?;
        let now = self.records.clock.now();
        let record = Record {
            v: 1,
            tenant: tenant.as_str().to_owned(),
            number: number.phone_number_id().as_str().to_owned(),
            created_ms: millis(now),
        };
        self.records
            .records
            .put_if_absent(&id, &record, Expiry::After(RECORD_TTL))
            .await?
            .ok_or_else(ServiceError::internal)?;
        if let Err(refused) = self.records.reserve(tenant, &id).await {
            let _ = self.records.records.delete(&id).await;
            return Err(refused);
        }
        let queued = async {
            let job = Job::new(
                JobId::new(id.clone()).map_err(|_| ServiceError::internal())?,
                number.phone_number_id().clone(),
                now,
            )
            .to(request.recipients.clone())
            .payload(request.payload());
            self.records.jobs.schedule(job).await?;
            Ok::<(), ServiceError>(())
        }
        .await;
        if let Err(error) = queued {
            self.records.release(tenant, &id).await;
            let _ = self.records.records.delete(&id).await;
            return Err(error);
        }
        // Queued: from here on this answers, never fails, or a caller's
        // retry (its key released by an error) would queue the people a
        // second time. A status that cannot be read is the one it starts
        // with.
        let status = match self.records.status(&id, &record).await {
            Ok(status) => status,
            Err(_) => BroadcastStatus {
                id: id.clone(),
                number: number.phone_number_id().clone(),
                state: BroadcastState::Queued,
                created_at: now,
                recipients: request.recipients.len(),
                queued: request.recipients.len(),
                sent: 0,
                failed: 0,
                unknown: 0,
                limited: 0,
                failures: BTreeMap::new(),
            },
        };
        self.audit(
            CREATED,
            number.phone_number_id(),
            number.waba_id().as_str(),
            &status,
        );
        Ok(status)
    }

    /// The broadcast `id` on the number `pn`.
    ///
    /// # Errors
    ///
    /// `404 not_found`: a number or a broadcast that is not the caller's
    /// tenant's, or none at all (the same answer), before any job is read.
    pub async fn get(&self, pn: &PhoneNumberId, id: &str) -> Result<BroadcastStatus, ServiceError> {
        let record = self.owned(pn, id).await?;
        self.records.status(id, &record).await
    }

    /// Cancel the broadcast `id` on the number `pn`: it sends nothing more
    /// (a send past its last check goes out). One already settled, or
    /// cancelled, is answered as it stands.
    ///
    /// # Errors
    ///
    /// As [`Self::get`].
    pub async fn cancel(
        &self,
        pn: &PhoneNumberId,
        id: &str,
    ) -> Result<BroadcastStatus, ServiceError> {
        let record = self.owned(pn, id).await?;
        let job_id = JobId::new(id).map_err(|_| ServiceError::not_found())?;
        let cancelled = self.records.jobs.cancel(&job_id).await?;
        let status = self.records.status(id, &record).await?;
        if cancelled {
            let waba = self.waba_of(pn).await;
            self.audit(CANCELLED, pn, waba.as_deref().unwrap_or_default(), &status);
        }
        Ok(status)
    }

    /// The broadcast's record, if it is the caller's tenant's on `pn`.
    async fn owned(&self, pn: &PhoneNumberId, id: &str) -> Result<Record, ServiceError> {
        // Step 4: the number is the caller's tenant's.
        self.authz.tenant_number(self.caller, pn).await?;
        let record = self.records.record(id).await?.filter(|record| {
            record.tenant == self.caller.tenant().as_str() && record.number == pn.as_str()
        });
        record.ok_or_else(|| {
            tracing::info!(target: LOG_TARGET, "a broadcast that is not the caller's: not found");
            ServiceError::not_found()
        })
    }

    async fn waba_of(&self, pn: &PhoneNumberId) -> Option<String> {
        let binding = self.authz.records().number(pn).await.ok()??;
        Some(binding.waba_id.as_str().to_owned())
    }

    fn audit(
        &self,
        action: &'static str,
        pn: &PhoneNumberId,
        waba: &str,
        status: &BroadcastStatus,
    ) {
        broadcast_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: (!waba.is_empty()).then_some(waba),
                phone_number_id: Some(pn.as_str()),
                ..Subject::default()
            },
            &BroadcastFacts {
                broadcast_id: &status.id,
                recipients: status.recipients,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The namespaces and the id are stable identifiers (design D420):
    /// stored data and the messages' callback data carry them. Decisive:
    /// each constant and `new_id`'s shape.
    #[test]
    fn the_broadcast_namespaces_and_id_are_pinned() {
        assert_eq!(RECORD_NAMESPACE, "wa.server.broadcast");
        assert_eq!(TENANT_NAMESPACE, "wa.server.broadcast.tenant");
        assert_eq!(PROGRESS_NAMESPACE, "wa.server.broadcast.progress");
        assert_eq!(CALLBACK_PREFIX, "wa-broadcast:");
        let id = new_id().unwrap();
        assert!(is_broadcast_id(&id), "{id}");
        assert_eq!(id.len(), 35);
        for bad in ["", "bc_", "bc_xyz", "up_00000000000000000000000000000000"] {
            assert!(!is_broadcast_id(bad), "{bad}");
        }
        assert_ne!(new_id().unwrap(), id);
    }

    /// A person is keyed by BSUID first, never by phone number alone when
    /// a BSUID is known (their parameters are found by it).
    #[test]
    fn a_person_is_keyed_by_bsuid_first() {
        use meta_whatsapp_rs::core::ids::UserId;
        let both = Recipient::PhoneAndUser {
            phone: "+16505551234".to_owned(),
            user: UserId::new("US.1"),
        };
        assert_eq!(person_key(&both).as_deref(), Some("user:US.1"));
        assert_eq!(
            person_key(&Recipient::phone("+16505551234")).as_deref(),
            Some("phone:+16505551234")
        );
    }

    /// A person listed twice is sent once, with the parameters of their
    /// first listing. Decisive: `NewBroadcast::new`'s `listed` set (without
    /// it the later listing's parameters replace the first's).
    #[test]
    fn a_person_listed_twice_keeps_the_first_listings_parameters() {
        let template = json!({"name": "promo", "language": {"code": "en_US"}});
        let person = |components: Option<Value>| BroadcastRecipient {
            phone: Some("+16505551234".to_owned()),
            components,
            ..BroadcastRecipient::default()
        };
        let first = json!([{"type": "body", "parameters": [{"type": "text", "text": "FIRST"}]}]);
        let later = json!([{"type": "body", "parameters": [{"type": "text", "text": "LATER"}]}]);
        let both = NewBroadcast::new(
            template.clone(),
            vec![person(Some(first.clone())), person(Some(later.clone()))],
            &BroadcastSettings::default(),
        )
        .unwrap();
        assert_eq!(both.payload()["components"]["phone:+16505551234"], first);
        // The first listing with none: the later one's are not borrowed.
        let none_first = NewBroadcast::new(
            template,
            vec![person(None), person(Some(later))],
            &BroadcastSettings::default(),
        )
        .unwrap();
        assert!(
            none_first.payload()["components"]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }

    /// The bounds: none, too many, and each person checked as a send checks
    /// them. Decisive: `NewBroadcast::new`.
    #[test]
    fn a_request_is_checked_before_it_is_queued() {
        let settings = BroadcastSettings {
            max_recipients: 2,
            ..BroadcastSettings::default()
        };
        let template = json!({"name": "promo", "language": {"code": "en_US"}});
        let person = |phone: &str| BroadcastRecipient {
            phone: Some(phone.to_owned()),
            ..BroadcastRecipient::default()
        };
        let field = |people: Vec<BroadcastRecipient>| {
            NewBroadcast::new(template.clone(), people, &settings)
                .map(|_| ())
                .unwrap_err()
                .field()
                .map(str::to_owned)
        };
        assert_eq!(field(vec![]).as_deref(), Some("recipients"));
        assert_eq!(
            field(vec![person("+1650"), person("+1651"), person("+1652")]).as_deref(),
            Some("recipients")
        );
        assert_eq!(
            field(vec![person("+16505551234"), person("16505551234")]).as_deref(),
            Some("recipients[1]")
        );
        let ok = NewBroadcast::new(template, vec![person("+16505551234")], &settings).unwrap();
        assert_eq!(ok.recipients(), 1);
        assert!(!format!("{ok:?}").contains("16505551234"));
    }
}
