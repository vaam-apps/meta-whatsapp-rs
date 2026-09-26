//! [`Broadcast`]: one message to many recipients from one business number,
//! paced, with progress, retries and a report per recipient.
//!
//! ```text
//! Broadcast::builder(number).to(recipients).content(…) or .compose(…)
//!     .client(client) or .outbound(…) .pacer(pacer) .build()?
//!   ├─ .handle()  → BroadcastHandle: progress() and cancel(), from any task
//!   └─ .run()     → BroadcastReport: one RecipientReport each, in order
//!                   (or each line to a ReportSink as it settles: .report_to(…))
//! ```
//!
//! Every send (a retry too) first waits for a slot of the number's
//! [`Pacer`]; up to [`BroadcastBuilder::concurrency`] sends are in flight
//! at once, so the rate is reached even when each send takes a while.
//! `run` drives it all in the caller's task: spawn it (`tokio::spawn`) to
//! do something else meanwhile, and keep the handle. **Dropping the `run`
//! future** (a `select!` it loses, an aborted task) abandons the sends in
//! flight, without knowing whether each went out, and the report with
//! them: to stop a run, call [`BroadcastHandle::cancel`] and let `run`
//! return.
//!
//! **Each person once.** A recipient listed again (the same phone number,
//! compared by its digits; the same business-scoped user id; the same
//! group) is not sent to again: its line is [`SendOutcome::Duplicate`],
//! naming the first. [`BroadcastBuilder::dedupe`] turns this off.
//!
//! What a failed send becomes is a [`BroadcastPolicy`] (default
//! [`Backoff`]), read from the error's kind or code, never its text; the
//! pacer slows the number down on the errors its `SlowDownRule` names:
//!
//! | Meta says | Default |
//! | --- | --- |
//! | `131056`, the pair rate limit (one user messaged too often) | that recipient waits 1, 4, 16, 64 s (Meta's `4^X` schedule, `about-the-platform` § Pair rate limits), each wait at most [`Backoff::max_delay`]; the others go on |
//! | `130429` throughput (and the other `RateLimited` codes) | the recipient is retried with backoff, and the number's pacer slows down |
//! | `131057`, the number in maintenance (Meta upgrading its throughput takes it off for up to a minute, `throughput`) | the recipient is retried every 20 s, so its five sends outlast the minute, and the pacer slows down |
//! | `131049`, the per-user marketing limit | reported for that recipient, not retried (Meta: wait at least 24 hours before resending) |
//! | the kinds of [`Backoff::STOPS`], which hold for the number: the token, a permission, the account, the classification limit, payment, the spam limit (`131048`, Meta: check the number's quality status), registration, marketing turned off (`131063`) | the run stops: every recipient not sent yet is skipped (the pacer slows down on `131048` too) |
//! | the kinds of [`Backoff::CONTENT_STOPS`] (the template not found, paused, disabled, or its parameters wrong), when every recipient gets the same message ([`BroadcastBuilder::content`]) | the run stops; with [`BroadcastBuilder::compose`], reported for that recipient |
//! | anything else | reported for that recipient |
//!
//! **Never resent blindly.** Whatever the policy says, a recipient is sent
//! again only when `Error::may_resend` holds: the error proves Meta refused
//! the send before doing anything (throttling, an HTTP 429, `131057` on a
//! 4xx), the rule the client's own retries follow. A timed-out send, a 5xx
//! that is not a throttling refusal, or a `131000` is reported as it is,
//! never replayed. Reconcile those with the status webhooks: give each
//! message a `biz_opaque_callback_data` (`OutboundMessage::callback_data`)
//! in [`BroadcastBuilder::compose`]. [`BroadcastBuilder::client`] turns
//! the client's own replays off, so a throttled send is retried here,
//! through the pacer, and nowhere else.
//!
//! A [`SendOutcome::Sent`] means Meta accepted the message, not that it was
//! delivered: a `131049` can also arrive later, as a `failed` status
//! webhook (`templates/marketing-templates/per-user-limits`). Meta's daily
//! messaging limit (unique users per 24 hours, per business portfolio,
//! `messaging-limits`) is Meta's to enforce; nothing here counts it.
//!
//! **Memory.** A run holds the list, and per recipient its attempts and
//! whether it is settled; retries waiting hold their message and last
//! error. The report holds one line per recipient, with Meta's response or
//! the error. For a list too long to keep every line,
//! [`BroadcastBuilder::report_to`] hands each line to a [`ReportSink`] (a
//! channel's sender, a store) as it settles, and the report comes back
//! without lines.
//!
//! **Not durable.** The run lives in memory: a restart loses it, and the
//! report is the only record. Durable, resumable jobs are roadmap item
//! B3 (a typed store on `KvStore`).

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::fmt;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::{Either, join_all, select};
use meta_whatsapp_client::messages::{MessageContent, OutboundMessage, SendResponse};
use meta_whatsapp_client::{Client, RetryPolicy};
use meta_whatsapp_core::error::{ConfigError, SinkError, ValidationError};
use meta_whatsapp_core::ids::PhoneNumberId;
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::{Error, ErrorKind, Result};
use time::OffsetDateTime;
use tokio::sync::{mpsc, watch};

use crate::outbound::{ClientOutbound, Outbound};
use crate::pacer::{Pacer, after, elapsed, in_maintenance};

/// What a failed send becomes: retried later, reported, or the end of the
/// run. The default is [`Backoff`].
///
/// A [`FailureVerdict::RetryAfter`] is honoured only when
/// `Error::may_resend` holds; otherwise the recipient is reported as
/// failed. That rule is the library's and no policy lifts it: a policy can
/// only be stricter.
pub trait BroadcastPolicy: Send + Sync + fmt::Debug + 'static {
    /// The send to one recipient failed ([`SendFailure`]).
    fn on_failure(&self, failure: &SendFailure<'_>) -> FailureVerdict;
}

impl<T: BroadcastPolicy + ?Sized> BroadcastPolicy for Arc<T> {
    fn on_failure(&self, failure: &SendFailure<'_>) -> FailureVerdict {
        (**self).on_failure(failure)
    }
}

/// A failed send, as a [`BroadcastPolicy`] sees it.
#[derive(Debug)]
#[non_exhaustive]
pub struct SendFailure<'a> {
    /// What the send failed with.
    pub error: &'a Error,
    /// How many sends to that recipient have failed, this one included (1
    /// the first time).
    pub failures: u32,
    /// Whether every recipient is sent the same message
    /// ([`BroadcastBuilder::content`]): a refusal of the message itself (a
    /// paused template) then holds for every recipient left.
    pub shared_message: bool,
}

impl<'a> SendFailure<'a> {
    /// The `failures`-th failed send to one recipient, with `error`, of a
    /// message composed for them.
    pub fn new(error: &'a Error, failures: u32) -> Self {
        Self {
            error,
            failures,
            shared_message: false,
        }
    }

    /// This failure, of a message every recipient gets when `shared`.
    #[must_use]
    pub fn with_shared_message(mut self, shared: bool) -> Self {
        self.shared_message = shared;
        self
    }
}

/// What a [`BroadcastPolicy`] makes of a failed send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureVerdict {
    /// Report the error for this recipient; go on with the others.
    Fail,
    /// Send to this recipient again, not before this delay; the others go
    /// on meanwhile. Honoured only when `Error::may_resend` holds (see
    /// [`BroadcastPolicy`]); otherwise a [`FailureVerdict::Fail`].
    RetryAfter(Duration),
    /// Report the error for this recipient and stop the run: every
    /// recipient not sent yet is skipped (sends in flight finish).
    Stop,
}

/// The default [`BroadcastPolicy`], read from `ErrorKind` and the Graph
/// code (never the error's text):
///
/// - a kind of [`Backoff::stops`] (default [`Backoff::STOPS`]: the token,
///   a permission, the account, the classification limit, payment, the
///   spam limit, registration, marketing turned off), or, when every
///   recipient gets the same message, of [`Backoff::content_stops`]
///   (default [`Backoff::CONTENT_STOPS`]: the template not found, paused,
///   disabled or its parameters wrong): stop the run;
/// - past [`Backoff::max_attempts`] sends to a recipient, or an error that
///   is not retryable (`131049`, `131050`, `131047`, …): fail;
/// - the pair rate limit (`131056`): retry after `4^(failures - 1)`
///   seconds, Meta's schedule (1, 4, 16, 64 s);
/// - the number in maintenance (`131057`): retry after
///   [`Backoff::MAINTENANCE_RETRY`];
/// - any other retryable error (`130429`): retry after
///   [`Backoff::base_delay`] doubled per failure;
///
/// each delay at most [`Backoff::max_delay`]. Which retries happen is then
/// `Error::may_resend`'s to say ([`BroadcastPolicy`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct Backoff {
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
    stops: Cow<'static, [ErrorKind]>,
    content_stops: Cow<'static, [ErrorKind]>,
}

impl Default for Backoff {
    /// Five sends per recipient at most; 1 second doubling, every delay at
    /// most 64 seconds; [`Backoff::STOPS`] and [`Backoff::CONTENT_STOPS`].
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(64),
            stops: Cow::Borrowed(Self::STOPS),
            content_stops: Cow::Borrowed(Self::CONTENT_STOPS),
        }
    }
}

impl Backoff {
    /// The kinds that stop a run by default: they hold for the number (or
    /// its account), so every send left would fail the same way. The spam
    /// limit (`131048`) restricts how many messages the number may send
    /// (Meta: check its quality status); `Registration` is the number not
    /// registered; `MarketingNotAllowed` is marketing turned off for the
    /// account on this API (`131063`, `131055`, `134100`).
    pub const STOPS: &'static [ErrorKind] = &[
        ErrorKind::Authentication,
        ErrorKind::Permission,
        ErrorKind::AccountRestricted,
        ErrorKind::ClassificationLimitReached,
        ErrorKind::Payment,
        ErrorKind::SpamRateLimited,
        ErrorKind::Registration,
        ErrorKind::MarketingNotAllowed,
    ];

    /// The kinds that also stop a run when every recipient gets the same
    /// message ([`BroadcastBuilder::content`]): a refusal of the template
    /// itself, which every recipient left would meet. With
    /// [`BroadcastBuilder::compose`] they fail one recipient only.
    pub const CONTENT_STOPS: &'static [ErrorKind] = &[
        ErrorKind::TemplateNotFound,
        ErrorKind::TemplatePaused,
        ErrorKind::TemplateDisabled,
        ErrorKind::TemplateParameterMismatch,
    ];

    /// The delay before each retry of a send refused with `131057`, the
    /// number in maintenance. Meta's throughput upgrade, which a large
    /// broadcast can trigger, takes the number off for up to a minute
    /// (`throughput`); at 20 s apart, the default five sends span 80 s.
    pub const MAINTENANCE_RETRY: Duration = Duration::from_secs(20);

    /// The defaults ([`Backoff::default`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sends to one recipient at most, the first included (default 5; one:
    /// no retry). Zero is a `ConfigError`.
    pub fn max_attempts(mut self, sends: u32) -> Result<Self> {
        if sends == 0 {
            return Err(ConfigError::new("a backoff of zero sends per recipient").into());
        }
        self.max_attempts = sends;
        Ok(self)
    }

    /// The first retry's delay, doubled for each later one (default 1 s;
    /// zero: every retry at the recipient's next slot). The pair rate
    /// limit and maintenance follow their own schedules.
    pub fn base_delay(mut self, delay: Duration) -> Self {
        self.base_delay = delay;
        self
    }

    /// The longest delay before any retry, the pair rate limit's and
    /// maintenance's included (default 64 s, the pair limit's fourth
    /// wait; zero: every retry at the recipient's next slot).
    pub fn max_delay(mut self, delay: Duration) -> Self {
        self.max_delay = delay;
        self
    }

    /// The kinds that stop the run, instead of [`Backoff::STOPS`] (empty:
    /// none does).
    pub fn stops(mut self, kinds: &[ErrorKind]) -> Self {
        self.stops = Cow::Owned(kinds.to_vec());
        self
    }

    /// The kinds that also stop the run when every recipient gets the same
    /// message, instead of [`Backoff::CONTENT_STOPS`] (empty: none does).
    pub fn content_stops(mut self, kinds: &[ErrorKind]) -> Self {
        self.content_stops = Cow::Owned(kinds.to_vec());
        self
    }
}

impl BroadcastPolicy for Backoff {
    fn on_failure(&self, failure: &SendFailure<'_>) -> FailureVerdict {
        let error = failure.error;
        let kind = error.kind();
        if self.stops.contains(&kind)
            || (failure.shared_message && self.content_stops.contains(&kind))
        {
            return FailureVerdict::Stop;
        }
        if failure.failures >= self.max_attempts || !error.is_retryable() {
            return FailureVerdict::Fail;
        }
        let exponent = failure.failures.saturating_sub(1);
        let delay = if kind == ErrorKind::PairRateLimited {
            // `about-the-platform` § Pair rate limits: "retry after 4^X
            // seconds (starting with X=0 and increasing X by 1 after each
            // failure)".
            Duration::from_secs(4u64.saturating_pow(exponent))
        } else if in_maintenance(error) {
            Self::MAINTENANCE_RETRY
        } else {
            self.base_delay
                .saturating_mul(2u32.saturating_pow(exponent))
        };
        FailureVerdict::RetryAfter(delay.min(self.max_delay))
    }
}

/// How far a broadcast got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct BroadcastProgress {
    /// Recipients in the broadcast, as listed (duplicates included).
    pub total: usize,
    /// Accepted by Meta ([`SendOutcome::Sent`]).
    pub sent: usize,
    /// Given up on ([`SendOutcome::Failed`]).
    pub failed: usize,
    /// Never sent because the run was cancelled or stopped
    /// ([`SendOutcome::Skipped`]).
    pub skipped: usize,
    /// Listed again, not sent to again ([`SendOutcome::Duplicate`]);
    /// counted from the start.
    pub duplicates: usize,
}

impl BroadcastProgress {
    /// Recipients not settled yet: waiting, in flight, or waiting for a
    /// retry.
    pub fn remaining(&self) -> usize {
        self.total
            .saturating_sub(self.sent)
            .saturating_sub(self.failed)
            .saturating_sub(self.skipped)
            .saturating_sub(self.duplicates)
    }
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BroadcastEnd {
    /// Every recipient was settled.
    Completed,
    /// [`BroadcastHandle::cancel`] stopped it.
    Cancelled,
    /// A [`FailureVerdict::Stop`] stopped it, on the failure of the
    /// recipient at `recipient` (the index in the order given).
    #[non_exhaustive]
    Stopped {
        /// Index of the recipient whose failure stopped the run.
        recipient: usize,
    },
    /// The pacer's rate limiter failed (a shared one unreachable) while
    /// booking a slot for the recipient at `recipient`, which is reported
    /// failed with that error, unsent: nothing more went out.
    #[non_exhaustive]
    LimiterFailed {
        /// Index of the recipient whose slot could not be booked.
        recipient: usize,
    },
    /// The [`ReportSink`] failed to record the line of the recipient at
    /// `recipient`: nothing more went out whose line would be lost.
    #[non_exhaustive]
    SinkFailed {
        /// Index of the recipient whose line the sink refused.
        recipient: usize,
    },
}

/// What happened to one recipient.
#[derive(Debug)]
#[non_exhaustive]
pub enum SendOutcome {
    /// Meta accepted the message (the response carries its id, the key of
    /// its status webhooks). Accepted is not delivered.
    Sent(SendResponse),
    /// Given up on, with the last error. When
    /// `error.may_have_been_sent()` is true it may have gone out:
    /// reconcile with the status webhooks before sending again.
    Failed(Error),
    /// Never sent: the run was cancelled or stopped before (or while
    /// waiting to retry after a failure that proved nothing was sent:
    /// [`RecipientReport::last_error`]).
    Skipped,
    /// Never sent: the same person as the recipient listed at index `of`,
    /// whose line says what happened ([`BroadcastBuilder::dedupe`]).
    Duplicate {
        /// Index (in the order given) of the first listing.
        of: usize,
    },
}

/// One recipient's line of the report.
#[derive(Debug)]
#[non_exhaustive]
pub struct RecipientReport {
    /// Where the recipient is in the list given (the line's place in
    /// [`BroadcastReport::recipients`]; what identifies a line a
    /// [`ReportSink`] receives).
    pub index: usize,
    /// The recipient, as given.
    pub recipient: Recipient,
    /// Sends handed to the outbound for them, retries included (a send the
    /// outbound refused before any request counts too). Zero when none
    /// was: the message could not be composed, the rate limiter failed
    /// before the first ([`BroadcastEnd::LimiterFailed`]), the run ended
    /// first, or a duplicate.
    pub attempts: u32,
    /// What happened.
    pub outcome: SendOutcome,
    /// For a recipient [`SendOutcome::Skipped`] while it waited to be
    /// retried: the error of its last send (which proved nothing was
    /// sent). `None` otherwise; a [`SendOutcome::Failed`] carries its own.
    pub last_error: Option<Error>,
}

/// What a run did.
#[derive(Debug)]
#[non_exhaustive]
pub struct BroadcastReport {
    /// One per recipient, in the order given; empty when the lines went to
    /// a [`ReportSink`] instead ([`BroadcastBuilder::report_to`]).
    pub recipients: Vec<RecipientReport>,
    /// How the run ended.
    pub ended: BroadcastEnd,
    progress: BroadcastProgress,
}

impl BroadcastReport {
    /// The final counts (the same as [`BroadcastHandle::progress`] after
    /// the run), lines streamed or not.
    pub fn progress(&self) -> BroadcastProgress {
        self.progress
    }
}

/// Where a broadcast's lines go as each recipient is settled, instead of
/// the report ([`BroadcastBuilder::report_to`]): for a list too long to
/// keep every line (Meta's response or the error) in memory.
///
/// Lines arrive in the order recipients settle: the duplicates first, the
/// recipients never sent (cancelled, stopped) last; `line.index` says
/// which recipient each is. A `tokio::sync::mpsc::Sender` is one (its
/// receiver reads them as they come).
#[async_trait]
pub trait ReportSink: Send + Sync + fmt::Debug + 'static {
    /// Record `line`. The sender that settled it waits meanwhile, so a slow
    /// sink slows the run rather than letting lines pile up. An error stops
    /// the run ([`BroadcastEnd::SinkFailed`]): nothing more is sent whose
    /// line could not be recorded.
    async fn record(&self, line: RecipientReport) -> Result<()>;
}

#[async_trait]
impl<T: ReportSink + ?Sized> ReportSink for Arc<T> {
    async fn record(&self, line: RecipientReport) -> Result<()> {
        (**self).record(line).await
    }
}

/// Each line into the channel, waiting while it is full; a closed channel
/// (its receiver dropped) is `SinkError::Closed`, which stops the run.
#[async_trait]
impl ReportSink for mpsc::Sender<RecipientReport> {
    async fn record(&self, line: RecipientReport) -> Result<()> {
        self.send(line).await.map_err(|_| SinkError::Closed.into())
    }
}

/// Whether the run goes on, and why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halt {
    Running,
    Cancelled,
    Stopped,
    LimiterFailed,
    SinkFailed,
    /// The run ended on its own: a later cancel changes nothing.
    Finished,
}

/// What the handle and the run share.
#[derive(Debug)]
struct Shared {
    total: usize,
    sent: AtomicUsize,
    failed: AtomicUsize,
    skipped: AtomicUsize,
    duplicates: AtomicUsize,
    halt: watch::Sender<Halt>,
}

impl Shared {
    fn progress(&self) -> BroadcastProgress {
        BroadcastProgress {
            total: self.total,
            sent: self.sent.load(AtomicOrdering::Acquire),
            failed: self.failed.load(AtomicOrdering::Acquire),
            skipped: self.skipped.load(AtomicOrdering::Acquire),
            duplicates: self.duplicates.load(AtomicOrdering::Acquire),
        }
    }

    /// Move from running to `to`; a run already halted keeps its reason.
    fn halt(&self, to: Halt) -> bool {
        self.halt.send_if_modified(|halt| {
            if *halt == Halt::Running {
                *halt = to;
                true
            } else {
                false
            }
        })
    }

    fn halted(&self) -> bool {
        *self.halt.borrow() != Halt::Running
    }
}

/// Watches a running broadcast, from any task: its progress, and a way to
/// cancel it. Cheap to clone.
#[derive(Debug, Clone)]
pub struct BroadcastHandle {
    shared: Arc<Shared>,
}

impl BroadcastHandle {
    /// How far the run got.
    pub fn progress(&self) -> BroadcastProgress {
        self.shared.progress()
    }

    /// Stop sending: no send starts after this (a wait for a slot or for a
    /// retry ends at once, the slot handed back to the pacer); sends in
    /// flight finish and are reported, since abandoning one would leave
    /// unknown whether it went out. Recipients not sent are
    /// [`SendOutcome::Skipped`]. Nothing after the run ended.
    pub fn cancel(&self) {
        if self.shared.halt(Halt::Cancelled) {
            tracing::debug!("broadcast cancelled");
        }
    }

    /// Whether [`Self::cancel`] was called while the run was going.
    pub fn is_cancelled(&self) -> bool {
        *self.shared.halt.borrow() == Halt::Cancelled
    }
}

/// Makes one recipient's message ([`BroadcastBuilder::compose`]).
type Composer = Box<dyn Fn(&Recipient) -> Result<OutboundMessage> + Send + Sync>;

/// How each recipient's message is made.
enum Compose {
    Content(Box<MessageContent>),
    With(Composer),
}

/// Who a recipient is, for [`BroadcastBuilder::dedupe`].
#[derive(Debug, PartialEq, Eq, Hash)]
enum Identity {
    /// A phone number's digits (`+1 650-555-1234` is `16505551234`).
    Phone(String),
    User(String),
    Group(String),
}

/// The identities a recipient is known by: one, or two when it carries a
/// phone number and a user id (the same person).
fn identities(recipient: &Recipient) -> Vec<Identity> {
    fn phone(number: &str) -> Identity {
        let digits: String = number.chars().filter(char::is_ascii_digit).collect();
        Identity::Phone(if digits.is_empty() {
            number.to_owned()
        } else {
            digits
        })
    }
    match recipient {
        Recipient::Phone(number) => vec![phone(number)],
        Recipient::User(user) => vec![Identity::User(user.as_str().to_owned())],
        Recipient::PhoneAndUser {
            phone: number,
            user,
        } => {
            vec![phone(number), Identity::User(user.as_str().to_owned())]
        }
        Recipient::Group(group) => vec![Identity::Group(group.as_str().to_owned())],
        // A kind of recipient added later: never taken for another.
        _ => Vec::new(),
    }
}

/// Each listing of a person after their first, with the index of the
/// first: `(index, of)`, in list order.
fn duplicates(recipients: &[Recipient]) -> Vec<(usize, usize)> {
    let mut first: HashMap<Identity, usize> = HashMap::new();
    let mut out = Vec::new();
    for (index, recipient) in recipients.iter().enumerate() {
        let keys = identities(recipient);
        let seen = keys.iter().find_map(|key| first.get(key).copied());
        // A duplicate's other identities are the first listing's too.
        let owner = seen.unwrap_or(index);
        for key in keys {
            first.entry(key).or_insert(owner);
        }
        if let Some(of) = seen {
            out.push((index, of));
        }
    }
    out
}

/// A recipient to send to again: its message, and what the last send
/// failed with.
struct Retry {
    message: Box<OutboundMessage>,
    error: Error,
}

/// A send waiting for its retry time.
struct Deferred {
    at: OffsetDateTime,
    index: usize,
    retry: Retry,
}

// Ordered by due time, then index, reversed: `BinaryHeap` pops the
// earliest.
impl Ord for Deferred {
    fn cmp(&self, other: &Self) -> Ordering {
        (other.at, other.index).cmp(&(self.at, self.index))
    }
}

impl PartialOrd for Deferred {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Deferred {
    fn eq(&self, other: &Self) -> bool {
        (self.at, self.index) == (other.at, other.index)
    }
}

impl Eq for Deferred {}

/// The run's book: who is next, who waits for a retry, who is settled.
struct Book {
    next: usize,
    deferred: BinaryHeap<Deferred>,
    attempts: Vec<u32>,
    /// Whether each recipient is settled (a duplicate, from the start).
    settled: Vec<bool>,
    /// Each settled recipient's outcome, for the report; empty when the
    /// lines go to a sink.
    outcomes: Vec<Option<SendOutcome>>,
    /// The last error of each recipient left unsent while it waited to be
    /// retried, once the run halted.
    last_errors: HashMap<usize, Error>,
    stopped_by: Option<usize>,
    /// The run's latest time, and how far the pacer's clock stepped back
    /// in all (see [`Book::now`]).
    seen: Option<OffsetDateTime>,
    skew: Duration,
}

/// What a worker does next.
enum Turn {
    /// Send to this recipient (again, when a retry).
    Send(usize, Option<Retry>),
    /// Nothing is due for this long.
    Wait(Duration),
    /// Nothing is left.
    Done,
}

impl Book {
    /// The run's time: the pacer's (wall) clock, held where it was when
    /// that clock steps back and going on from there, so a retry due in
    /// one second is not put off by the step.
    fn now(&mut self, wall: OffsetDateTime) -> OffsetDateTime {
        let now = after(wall, self.skew);
        match self.seen {
            Some(seen) if now < seen => {
                self.skew = self.skew.saturating_add(elapsed(now, seen));
                seen
            }
            _ => {
                self.seen = Some(now);
                now
            }
        }
    }

    fn next(&mut self, wall: OffsetDateTime) -> Turn {
        let now = self.now(wall);
        if self.deferred.peek().is_some_and(|d| d.at <= now)
            && let Some(due) = self.deferred.pop()
        {
            return Turn::Send(due.index, Some(due.retry));
        }
        while self.settled.get(self.next).copied().unwrap_or(false) {
            self.next += 1;
        }
        if self.next < self.settled.len() {
            self.next += 1;
            return Turn::Send(self.next - 1, None);
        }
        match self.deferred.peek() {
            Some(due) => Turn::Wait(elapsed(now, due.at)),
            None => Turn::Done,
        }
    }

    /// Send to the recipient at `index` again, `delay` from now.
    fn defer(&mut self, wall: OffsetDateTime, delay: Duration, index: usize, retry: Retry) {
        let at = after(self.now(wall), delay);
        self.deferred.push(Deferred { at, index, retry });
    }
}

/// One message to many recipients from one business number, paced. See
/// the [module docs](self).
pub struct Broadcast {
    from: PhoneNumberId,
    recipients: Vec<Recipient>,
    compose: Compose,
    outbound: Arc<dyn Outbound>,
    pacer: Pacer,
    policy: Arc<dyn BroadcastPolicy>,
    concurrency: usize,
    shared: Arc<Shared>,
    book: Mutex<Book>,
    sink: Option<Arc<dyn ReportSink>>,
    /// The sink failed: it gets nothing more.
    sink_failed: AtomicBool,
    /// The duplicates' lines, for the sink (the report has them already).
    duplicates: Vec<(usize, usize)>,
}

impl fmt::Debug for Broadcast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Broadcast")
            .field("from", &self.from)
            .field("recipients", &self.recipients.len())
            .field("concurrency", &self.concurrency)
            .field("progress", &self.shared.progress())
            .finish_non_exhaustive()
    }
}

impl Broadcast {
    /// Start building a broadcast from the business number `from`.
    pub fn builder(from: impl Into<PhoneNumberId>) -> BroadcastBuilder {
        BroadcastBuilder::new(from.into())
    }

    /// A handle on this broadcast: take it before [`Self::run`] to watch
    /// or cancel the run from another task.
    pub fn handle(&self) -> BroadcastHandle {
        BroadcastHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Send to every recipient, paced, and report on each. The future is
    /// `Send`: spawn it to run in the background. Dropping it abandons the
    /// sends in flight and the report; stop a run with
    /// [`BroadcastHandle::cancel`] instead.
    pub async fn run(self) -> BroadcastReport {
        for &(index, of) in &self.duplicates {
            self.record(self.line(index, SendOutcome::Duplicate { of }))
                .await;
        }
        let workers = self.concurrency.min(self.recipients.len()).max(1);
        join_all((0..workers).map(|_| self.work())).await;
        self.shared.halt(Halt::Finished);
        {
            // Retries still waiting when the run halted: never sent.
            let mut book = self.book();
            let deferred = std::mem::take(&mut book.deferred);
            for waiting in deferred {
                book.last_errors.insert(waiting.index, waiting.retry.error);
            }
        }
        // Never sent: not reached, or waiting for a retry, when the run
        // halted.
        let mut unsettled = 0;
        for index in 0..self.recipients.len() {
            if self.book().settled[index] {
                continue;
            }
            unsettled += 1;
            if self.sink.is_some() {
                self.record(self.line(index, SendOutcome::Skipped)).await;
            }
        }
        self.shared
            .skipped
            .fetch_add(unsettled, AtomicOrdering::AcqRel);
        let halt = *self.shared.halt.borrow();
        let streamed = self.sink.is_some();
        let mut book = self
            .book
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        let recipient = book.stopped_by.unwrap_or_default();
        let ended = match halt {
            Halt::Running | Halt::Finished => BroadcastEnd::Completed,
            Halt::Cancelled => BroadcastEnd::Cancelled,
            Halt::Stopped => BroadcastEnd::Stopped { recipient },
            Halt::LimiterFailed => BroadcastEnd::LimiterFailed { recipient },
            Halt::SinkFailed => BroadcastEnd::SinkFailed { recipient },
        };
        let recipients = if streamed {
            Vec::new()
        } else {
            let mut last_errors = std::mem::take(&mut book.last_errors);
            self.recipients
                .into_iter()
                .zip(book.outcomes)
                .zip(book.attempts)
                .enumerate()
                .map(
                    |(index, ((recipient, outcome), attempts))| RecipientReport {
                        index,
                        recipient,
                        attempts,
                        outcome: outcome.unwrap_or(SendOutcome::Skipped),
                        last_error: last_errors.remove(&index),
                    },
                )
                .collect()
        };
        let report = BroadcastReport {
            recipients,
            ended,
            progress: self.shared.progress(),
        };
        let progress = report.progress();
        tracing::debug!(
            sent = progress.sent,
            failed = progress.failed,
            skipped = progress.skipped,
            duplicates = progress.duplicates,
            ended = ?report.ended,
            "broadcast ended"
        );
        report
    }

    fn book(&self) -> MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One of the concurrent senders: takes the next recipient due, until
    /// none is left or the run halts.
    async fn work(&self) {
        loop {
            if self.shared.halted() {
                return;
            }
            let turn = self.book().next(self.pacer.now());
            match turn {
                Turn::Send(index, retry) => self.attempt(index, retry).await,
                Turn::Wait(duration) => {
                    if !self.wait_until(after(self.pacer.now(), duration)).await {
                        return;
                    }
                }
                Turn::Done => return,
            }
        }
    }

    /// Wait until `deadline` on the pacer's timer, unless the run halts
    /// first: whether the run goes on.
    async fn wait_until(&self, deadline: OffsetDateTime) -> bool {
        if !elapsed(self.pacer.now(), deadline).is_zero() {
            let mut halt = self.shared.halt.subscribe();
            let halted = pin!(halt.wait_for(|halt| *halt != Halt::Running));
            let slept = pin!(self.pacer.sleep_until(deadline));
            if let Either::Right(_) = select(slept, halted).await {
                return false;
            }
        }
        !self.shared.halted()
    }

    /// One send to the recipient at `index`: compose (the first time),
    /// wait for a slot, send, settle or defer.
    async fn attempt(&self, index: usize, retry: Option<Retry>) {
        let (message, last_error) = match retry {
            Some(Retry { message, error }) => (message, Some(error)),
            None => match self.compose(index) {
                Ok(message) => (Box::new(message), None),
                Err(error) => return self.settle(index, SendOutcome::Failed(error)).await,
            },
        };
        let (request, reservation) = match self.pacer.reserve(&self.from).await {
            Ok(slot) => slot,
            Err(error) => {
                // A pacer that cannot pace lets nothing through: every
                // recipient left would meet it.
                tracing::warn!(
                    error_kind = ?error.kind(),
                    "broadcast stopped: the rate limiter failed"
                );
                self.settle(index, SendOutcome::Failed(error)).await;
                return self.stop(index, Halt::LimiterFailed);
            }
        };
        if !self.wait_until(after(request.now, reservation.wait)).await {
            // Cancelled or stopped while waiting: the slot goes back, the
            // recipient is skipped.
            self.pacer.release(&request, &reservation).await;
            if let Some(error) = last_error {
                self.book().last_errors.insert(index, error);
            }
            return;
        }
        let result = self.outbound.send(&self.from, &message).await;
        let failures = {
            let mut book = self.book();
            book.attempts[index] += 1;
            book.attempts[index]
        };
        match result {
            Ok(response) => self.settle(index, SendOutcome::Sent(response)).await,
            Err(error) => self.failed(index, message, error, failures).await,
        }
    }

    async fn failed(
        &self,
        index: usize,
        message: Box<OutboundMessage>,
        error: Error,
        failures: u32,
    ) {
        tracing::debug!(
            recipient = index,
            error_kind = ?error.kind(),
            may_have_been_sent = error.may_have_been_sent(),
            graph_code = error.graph().map(|g| g.code),
            failures,
            "broadcast send failed"
        );
        if let Err(e) = self.pacer.on_error(&self.from, &error).await {
            tracing::warn!(error_kind = ?e.kind(), "broadcast could not slow the pacer down");
        }
        let failure = SendFailure::new(&error, failures)
            .with_shared_message(matches!(self.compose, Compose::Content(_)));
        match self.policy.on_failure(&failure) {
            FailureVerdict::RetryAfter(delay) if error.may_resend() => {
                self.book()
                    .defer(self.pacer.now(), delay, index, Retry { message, error });
            }
            FailureVerdict::Stop => {
                tracing::warn!(
                    recipient = index,
                    error_kind = ?error.kind(),
                    "broadcast stopped by a failure every recipient would meet"
                );
                self.settle(index, SendOutcome::Failed(error)).await;
                self.stop(index, Halt::Stopped);
            }
            FailureVerdict::RetryAfter(_) | FailureVerdict::Fail => {
                self.settle(index, SendOutcome::Failed(error)).await;
            }
        }
    }

    fn compose(&self, index: usize) -> Result<OutboundMessage> {
        let recipient = &self.recipients[index];
        match &self.compose {
            Compose::Content(content) => {
                Ok(OutboundMessage::new(recipient.clone(), (**content).clone()))
            }
            Compose::With(compose) => {
                let message = compose(recipient)?;
                if message.recipient != *recipient {
                    return Err(ValidationError::new(
                        "recipient",
                        "the composed message is addressed to another recipient than the one it \
                         was composed for",
                    )
                    .into());
                }
                Ok(message)
            }
        }
    }

    /// The line of the recipient at `index`, for the sink.
    fn line(&self, index: usize, outcome: SendOutcome) -> RecipientReport {
        let mut book = self.book();
        RecipientReport {
            index,
            recipient: self.recipients[index].clone(),
            attempts: book.attempts[index],
            outcome,
            last_error: book.last_errors.remove(&index),
        }
    }

    /// Settle the recipient at `index`: count it, and keep its line for the
    /// report or hand it to the sink.
    async fn settle(&self, index: usize, outcome: SendOutcome) {
        let counter = match outcome {
            SendOutcome::Sent(_) => &self.shared.sent,
            SendOutcome::Failed(_) => &self.shared.failed,
            SendOutcome::Skipped => &self.shared.skipped,
            SendOutcome::Duplicate { .. } => &self.shared.duplicates,
        };
        let line = {
            let mut book = self.book();
            book.settled[index] = true;
            if self.sink.is_none() {
                book.outcomes[index] = Some(outcome);
                None
            } else {
                Some(RecipientReport {
                    index,
                    recipient: self.recipients[index].clone(),
                    attempts: book.attempts[index],
                    outcome,
                    last_error: book.last_errors.remove(&index),
                })
            }
        };
        counter.fetch_add(1, AtomicOrdering::AcqRel);
        if let Some(line) = line {
            self.record(line).await;
        }
    }

    /// Hand `line` to the sink; one that fails stops the run and gets
    /// nothing more.
    async fn record(&self, line: RecipientReport) {
        let Some(sink) = &self.sink else {
            return;
        };
        if self.sink_failed.load(AtomicOrdering::Acquire) {
            return;
        }
        let index = line.index;
        if let Err(error) = sink.record(line).await {
            tracing::warn!(
                error_kind = ?error.kind(),
                "broadcast stopped: its report sink failed"
            );
            self.sink_failed.store(true, AtomicOrdering::Release);
            self.stop(index, Halt::SinkFailed);
        }
    }

    /// Halt the run for `why`, on the recipient at `index`; a run already
    /// halted keeps its first reason.
    fn stop(&self, index: usize, why: Halt) {
        if self.shared.halt(why) {
            self.book().stopped_by = Some(index);
        }
    }
}

/// Builds a [`Broadcast`]. Required: an outbound ([`Self::client`] or
/// [`Self::outbound`]), a [`Pacer`] and the message ([`Self::content`] or
/// [`Self::compose`]); the rest has a default.
#[must_use]
pub struct BroadcastBuilder {
    from: PhoneNumberId,
    recipients: Vec<Recipient>,
    compose: Option<Compose>,
    outbound: Option<Arc<dyn Outbound>>,
    pacer: Option<Pacer>,
    policy: Arc<dyn BroadcastPolicy>,
    concurrency: usize,
    dedupe: bool,
    sink: Option<Arc<dyn ReportSink>>,
}

impl fmt::Debug for BroadcastBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BroadcastBuilder")
            .field("from", &self.from)
            .field("recipients", &self.recipients.len())
            .finish_non_exhaustive()
    }
}

impl BroadcastBuilder {
    fn new(from: PhoneNumberId) -> Self {
        Self {
            from,
            recipients: Vec::new(),
            compose: None,
            outbound: None,
            pacer: None,
            policy: Arc::new(Backoff::default()),
            concurrency: 32,
            dedupe: true,
            sink: None,
        }
    }

    /// Add recipients, in the order they are sent (and reported). A person
    /// listed twice gets one message ([`Self::dedupe`]).
    pub fn to<I, R>(mut self, recipients: I) -> Self
    where
        I: IntoIterator<Item = R>,
        R: Into<Recipient>,
    {
        self.recipients
            .extend(recipients.into_iter().map(Into::into));
        self
    }

    /// Send each person once (default `true`): a recipient listed again is
    /// not sent to again, and its line is [`SendOutcome::Duplicate`]. The
    /// same person is the same phone number, compared by its digits
    /// (`+1 650-555-1234` and `16505551234` match), the same
    /// business-scoped user id, or the same group; a phone number and a
    /// user id are one person only when a recipient carried both
    /// (`Recipient::PhoneAndUser`). `false` sends the list as given, each
    /// listing its own message.
    pub fn dedupe(mut self, on: bool) -> Self {
        self.dedupe = on;
        self
    }

    /// Send every recipient `content` (a template, most often: outside the
    /// 24-hour window Meta refuses anything else, `131047`). One message
    /// for everyone: a refusal of it ([`Backoff::CONTENT_STOPS`]) stops the
    /// run.
    pub fn content(mut self, content: impl Into<MessageContent>) -> Self {
        self.compose = Some(Compose::Content(Box::new(content.into())));
        self
    }

    /// Make each recipient's message with `compose` (their name in the
    /// template, a `biz_opaque_callback_data` to reconcile with the status
    /// webhooks). It runs once per recipient, before the first send; an
    /// error, or a message addressed to someone else, fails that recipient
    /// without a send.
    pub fn compose<F>(mut self, compose: F) -> Self
    where
        F: Fn(&Recipient) -> Result<OutboundMessage> + Send + Sync + 'static,
    {
        self.compose = Some(Compose::With(Box::new(compose)));
        self
    }

    /// Send through `outbound`. Its contract here:
    ///
    /// - **no retries inside a call**: each would go out without a slot of
    ///   the pacer and outside the policy. Over a client, give the client
    ///   `RetryPolicy::NONE`, as [`Self::client`] does;
    /// - **truthful errors**: a send that may have reached Meta must fail
    ///   with an error whose `Error::may_have_been_sent` is true, or it may
    ///   be sent again;
    /// - not a [`crate::PacedOutbound`]: the broadcast paces its sends
    ///   itself, and each would take two slots.
    pub fn outbound(mut self, outbound: impl Outbound) -> Self {
        self.outbound = Some(Arc::new(outbound));
        self
    }

    /// Send through an outbound shared with other code (the contract of
    /// [`Self::outbound`] holds).
    pub fn shared_outbound(mut self, outbound: Arc<dyn Outbound>) -> Self {
        self.outbound = Some(outbound);
        self
    }

    /// Send with `client` ([`ClientOutbound`]), its own replays turned off
    /// (`Client::with_retry(RetryPolicy::NONE)`): a throttled send is
    /// retried by the broadcast, through the pacer and its slow-down,
    /// rather than replayed at once by the client, outside the pacer.
    pub fn client(self, client: Client) -> Self {
        self.outbound(ClientOutbound::new(client.with_retry(RetryPolicy::NONE)))
    }

    /// Pace with `pacer`: share one per process across the broadcasts and
    /// bots of a number, or each gets a budget of its own.
    pub fn pacer(mut self, pacer: Pacer) -> Self {
        self.pacer = Some(pacer);
        self
    }

    /// Decide failed sends with `policy` ([`BroadcastPolicy`]; default
    /// [`Backoff`]).
    pub fn policy(mut self, policy: impl BroadcastPolicy) -> Self {
        self.policy = Arc::new(policy);
        self
    }

    /// Sends in flight at once, at most (default 32; zero fails
    /// [`Self::build`]). The pacer sets the rate; this only lets it be
    /// reached when sends are slow: about `rate × a send's duration` (80 a
    /// second at 400 ms each: 32; `Rate::HIGHER_THROUGHPUT`, 1,000 a
    /// second: 400).
    pub fn concurrency(mut self, sends: usize) -> Self {
        self.concurrency = sends;
        self
    }

    /// Hand each recipient's line to `sink` as it settles, instead of
    /// keeping it for the report (which then has none, its counts only):
    /// memory no longer grows with the lines. A `tokio::sync::mpsc`
    /// sender is a sink; a sink that fails stops the run ([`ReportSink`]).
    pub fn report_to(mut self, sink: impl ReportSink) -> Self {
        self.sink = Some(Arc::new(sink));
        self
    }

    /// Check the settings: no outbound, no pacer, no message or a
    /// concurrency of zero is a `ConfigError`.
    pub fn build(self) -> Result<Broadcast> {
        let outbound = self.outbound.ok_or_else(|| {
            ConfigError::new("a broadcast needs an outbound (`BroadcastBuilder::client`)")
        })?;
        let pacer = self.pacer.ok_or_else(|| {
            ConfigError::new(
                "a broadcast needs a pacer (`BroadcastBuilder::pacer`), shared by every sender \
                 of the number",
            )
        })?;
        let compose = self.compose.ok_or_else(|| {
            ConfigError::new(
                "a broadcast needs a message (`BroadcastBuilder::content` or `compose`)",
            )
        })?;
        if self.concurrency == 0 {
            return Err(ConfigError::new("a broadcast concurrency of zero").into());
        }
        let total = self.recipients.len();
        let duplicates = if self.dedupe {
            duplicates(&self.recipients)
        } else {
            Vec::new()
        };
        let mut settled = vec![false; total];
        let mut outcomes: Vec<Option<SendOutcome>> = Vec::new();
        if self.sink.is_none() {
            outcomes = (0..total).map(|_| None).collect();
        }
        for &(index, of) in &duplicates {
            settled[index] = true;
            if let Some(slot) = outcomes.get_mut(index) {
                *slot = Some(SendOutcome::Duplicate { of });
            }
        }
        let (halt, _) = watch::channel(Halt::Running);
        Ok(Broadcast {
            from: self.from,
            compose,
            outbound,
            pacer,
            policy: self.policy,
            concurrency: self.concurrency,
            shared: Arc::new(Shared {
                total,
                sent: AtomicUsize::new(0),
                failed: AtomicUsize::new(0),
                skipped: AtomicUsize::new(0),
                duplicates: AtomicUsize::new(duplicates.len()),
                halt,
            }),
            book: Mutex::new(Book {
                next: 0,
                deferred: BinaryHeap::new(),
                attempts: vec![0; total],
                settled,
                outcomes,
                last_errors: HashMap::new(),
                stopped_by: None,
                seen: None,
                skew: Duration::ZERO,
            }),
            recipients: self.recipients,
            duplicates: if self.sink.is_some() {
                duplicates
            } else {
                Vec::new()
            },
            sink: self.sink,
            sink_failed: AtomicBool::new(false),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use meta_whatsapp_core::GraphApiError;
    use meta_whatsapp_core::error::TransportError;

    fn api(code: i64, status: u16) -> Error {
        let mut error = GraphApiError::new(code, "x");
        error.http_status = Some(status);
        error.into()
    }

    /// The verdict for the `failures`-th failure of a composed message.
    fn verdict(policy: &Backoff, error: &Error, failures: u32) -> FailureVerdict {
        policy.on_failure(&SendFailure::new(error, failures))
    }

    fn secs(v: FailureVerdict) -> Option<u64> {
        match v {
            FailureVerdict::RetryAfter(d) => Some(d.as_secs()),
            _ => None,
        }
    }

    #[test]
    fn backoff_reads_the_kind() {
        let p = Backoff::default();
        // Meta's 4^X schedule for the pair limit.
        let pair: Vec<_> = (1..=4)
            .map(|n| secs(verdict(&p, &api(131056, 400), n)))
            .collect();
        assert_eq!(pair, [Some(1), Some(4), Some(16), Some(64)]);
        assert_eq!(verdict(&p, &api(131056, 400), 5), FailureVerdict::Fail);
        // Throughput: doubling from 1 s, capped.
        let throughput: Vec<_> = [1, 2, 3]
            .iter()
            .map(|n| secs(verdict(&p, &api(130429, 400), *n)))
            .collect();
        assert_eq!(throughput, [Some(1), Some(2), Some(4)]);
        assert_eq!(
            verdict(
                &Backoff::new().max_delay(Duration::from_secs(3)),
                &api(130429, 400),
                4
            ),
            FailureVerdict::RetryAfter(Duration::from_secs(3))
        );
        // Maintenance (a throughput upgrade, up to a minute): every 20 s,
        // five sends spanning 80 s; other `ServiceUnavailable` codes back
        // off as usual.
        let maintenance: Vec<_> = (1..=4)
            .map(|n| secs(verdict(&p, &api(131057, 400), n)))
            .collect();
        assert_eq!(maintenance, [Some(20), Some(20), Some(20), Some(20)]);
        assert_eq!(verdict(&p, &api(131057, 400), 5), FailureVerdict::Fail);
        assert_eq!(
            secs(verdict(&p, &api(131000, 400), 2)),
            Some(2),
            "131000 is not maintenance"
        );
        // Never retried: the per-user marketing limit, an opt-out, the
        // window closed.
        for code in [131049, 131050, 131047] {
            assert_eq!(
                verdict(&p, &api(code, 400), 1),
                FailureVerdict::Fail,
                "{code}"
            );
        }
        // Number- and account-wide: the run stops. The spam limit, a
        // number not registered and marketing turned off among them.
        for code in [190, 10, 131031, 131064, 131042, 131048, 133010, 131063] {
            assert_eq!(
                verdict(&p, &api(code, 400), 1),
                FailureVerdict::Stop,
                "{code}"
            );
        }
        // The template's own refusals: per recipient for a composed
        // message.
        for code in [132001, 132015, 132016, 132000] {
            assert_eq!(
                verdict(&p, &api(code, 400), 1),
                FailureVerdict::Fail,
                "{code}"
            );
        }
    }

    /// A template refused for everyone stops a run that sends everyone
    /// the same message, and only such a run.
    #[test]
    fn a_shared_messages_template_refusal_stops_the_run() {
        let p = Backoff::default();
        for code in [132001, 132015, 132016, 132000, 132012] {
            let error = api(code, 400);
            let shared = SendFailure::new(&error, 1).with_shared_message(true);
            assert_eq!(p.on_failure(&shared), FailureVerdict::Stop, "{code}");
            assert_eq!(verdict(&p, &error, 1), FailureVerdict::Fail, "{code}");
        }
        // Not every refusal: a recipient's own still fails alone.
        let error = api(131049, 400);
        let shared = SendFailure::new(&error, 1).with_shared_message(true);
        assert_eq!(p.on_failure(&shared), FailureVerdict::Fail);
    }

    /// The stop lists are settings: none, or others.
    #[test]
    fn the_stop_lists_are_settings() {
        let none = Backoff::new().stops(&[]).content_stops(&[]);
        let spam = api(131048, 400);
        assert_eq!(verdict(&none, &spam, 1), FailureVerdict::Fail);
        let paused = api(132015, 400);
        assert_eq!(
            none.on_failure(&SendFailure::new(&paused, 1).with_shared_message(true)),
            FailureVerdict::Fail
        );
        let optout = Backoff::new().stops(&[ErrorKind::MarketingOptedOut]);
        assert_eq!(verdict(&optout, &api(131050, 400), 1), FailureVerdict::Stop);
        assert_eq!(
            verdict(&optout, &api(190, 400), 1),
            FailureVerdict::Fail,
            "the list replaces the default"
        );
    }

    /// Every delay is capped, the pair limit's too: many attempts never
    /// wait more than `max_delay`.
    #[test]
    fn every_delay_is_at_most_max_delay() {
        let p = Backoff::new().max_attempts(20).unwrap();
        let pair: Vec<_> = (1..=8)
            .map(|n| secs(verdict(&p, &api(131056, 400), n)))
            .collect();
        assert_eq!(
            pair,
            [
                Some(1),
                Some(4),
                Some(16),
                Some(64),
                Some(64),
                Some(64),
                Some(64),
                Some(64)
            ]
        );
        for n in 1..20 {
            for code in [131056, 130429, 131057] {
                let Some(s) = secs(verdict(&p, &api(code, 400), n)) else {
                    panic!("{code} {n}")
                };
                assert!(s <= 64, "{code} after {n}: {s} s");
            }
        }
        let short = Backoff::new().max_delay(Duration::from_secs(10));
        assert_eq!(
            verdict(&short, &api(131056, 400), 3),
            FailureVerdict::RetryAfter(Duration::from_secs(10))
        );
        assert_eq!(
            verdict(&short, &api(131057, 400), 1),
            FailureVerdict::RetryAfter(Duration::from_secs(10))
        );
    }

    #[test]
    fn zero_attempts_is_a_config_error() {
        assert!(matches!(
            Backoff::new().max_attempts(0),
            Err(Error::Config(_))
        ));
        let once = Backoff::new().max_attempts(1).unwrap();
        assert_eq!(verdict(&once, &api(130429, 400), 1), FailureVerdict::Fail);
    }

    /// The resend rule the broadcast applies is core's, the client's:
    /// throttling and maintenance only. A `131000` on a 400 and a
    /// connection refused are retryable and not sent, yet not resent.
    #[test]
    fn only_what_meta_provably_refused_is_resent() {
        assert!(api(131056, 400).may_resend());
        assert!(
            api(130429, 503).may_resend(),
            "throttling proves nothing was sent"
        );
        assert!(api(131057, 400).may_resend());
        assert!(!Error::Transport(TransportError::Timeout).may_resend());
        assert!(!api(131000, 500).may_resend(), "a 5xx may have been sent");
        assert!(!api(131000, 400).may_resend(), "an unknown error");
        assert!(!api(131049, 400).may_resend(), "not retryable");
        assert!(
            !Error::Transport(TransportError::Connect(anyhow::anyhow!("refused"))).may_resend()
        );
    }

    #[test]
    fn progress_counts_what_is_left() {
        let p = BroadcastProgress {
            total: 10,
            sent: 3,
            failed: 2,
            skipped: 1,
            duplicates: 2,
        };
        assert_eq!(p.remaining(), 2);
    }

    /// The same person by digits, by user id, or through a recipient that
    /// carried both; never a phone number taken for another.
    #[test]
    fn duplicates_are_found_by_identity() {
        let list = [
            Recipient::phone("+16505550001"),     // 0
            Recipient::phone("1 (650) 555-0001"), // 1: 0 again
            Recipient::user("US.1"),              // 2
            Recipient::PhoneAndUser {
                phone: "+16505550002".into(),
                user: "US.1".into(),
            }, // 3: 2 again, and +16505550002 is 2's too
            Recipient::phone("16505550002"),      // 4: 2 again
            Recipient::group("G1"),               // 5
            Recipient::group("G1"),               // 6: 5 again
            Recipient::phone("+16505550003"),     // 7
            Recipient::phone("+165055500031"),    // 8: another number
            Recipient::user("US.2"),              // 9
        ];
        assert_eq!(duplicates(&list), [(1, 0), (3, 2), (4, 2), (6, 5)]);
        // Numbers without a digit (Meta refuses them) are compared as
        // written, not all taken for one empty number.
        let digitless = [Recipient::phone("abc"), Recipient::phone("xyz")];
        assert_eq!(duplicates(&digitless), []);
    }

    /// The run's time never steps back: a retry due in 1 s stays 1 s away
    /// when the wall clock steps back an hour.
    #[test]
    fn the_runs_time_holds_when_the_clock_steps_back() {
        let t0 = time::macros::datetime!(2026-09-26 12:00 UTC);
        let mut book = Book {
            next: 0,
            deferred: BinaryHeap::new(),
            attempts: vec![0],
            settled: vec![true],
            outcomes: Vec::new(),
            last_errors: HashMap::new(),
            stopped_by: None,
            seen: None,
            skew: Duration::ZERO,
        };
        book.defer(
            t0,
            Duration::from_secs(1),
            0,
            Retry {
                message: Box::new(OutboundMessage::text(Recipient::phone("+1"), "x")),
                error: api(131056, 400),
            },
        );
        let back = t0 - time::Duration::hours(1);
        assert!(matches!(book.next(back), Turn::Wait(d) if d == Duration::from_secs(1)));
        let later = back + time::Duration::milliseconds(400);
        assert!(matches!(book.next(later), Turn::Wait(d) if d == Duration::from_millis(600)));
        assert!(matches!(
            book.next(later + time::Duration::milliseconds(600)),
            Turn::Send(0, Some(_))
        ));
    }
}
