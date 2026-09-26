//! [`Broadcast`]: one message to many recipients from one business number,
//! paced, with progress, retries and a report per recipient.
//!
//! ```text
//! Broadcast::builder(number).to(recipients).content(…) or .compose(…)
//!     .client(client) or .outbound(…) .pacer(pacer) .build()?
//!   ├─ .handle()  → BroadcastHandle: progress() and cancel(), from any task
//!   └─ .run()     → BroadcastReport: one RecipientReport each, in order
//! ```
//!
//! Every send (a retry too) first waits for a slot of the number's
//! [`Pacer`]; up to [`BroadcastBuilder::concurrency`] sends are in flight
//! at once, so the rate is reached even when each send takes a while.
//! `run` drives it all in the caller's task: spawn it (`tokio::spawn`) to
//! do something else meanwhile, and keep the handle.
//!
//! **Each person once.** A recipient listed again (the same phone number,
//! compared by its digits; the same business-scoped user id; the same
//! group) is not sent to again: its line is [`Outcome::Duplicate`], naming
//! the first. [`BroadcastBuilder::dedupe`] turns this off.
//!
//! What a failed send becomes is a [`BroadcastPolicy`] (default
//! [`Backoff`]), read from the error's kind or code, never its text:
//!
//! | Meta says | Default |
//! | --- | --- |
//! | `131056`, the pair rate limit (one user messaged too often) | that recipient waits 1, 4, 16, 64 s (Meta's `4^X` schedule, `about-the-platform` § Pair rate limits); the others go on |
//! | `130429` throughput (and the other `RateLimited` codes) | the recipient is retried with backoff, and the number's pacer slows down |
//! | `131057`, the number in maintenance (Meta upgrading its throughput takes it off for up to a minute, `throughput`) | the recipient is retried every 20 s, so its five sends outlast the minute, and the pacer slows down |
//! | `131048`, sending restricted for spam | reported, not retried (Meta: retrying makes it worse); the pacer slows down |
//! | `131049`, the per-user marketing limit | reported for that recipient, not retried (Meta: wait at least 24 hours) |
//! | the token, a permission, the account, the classification limit or payment refused ([`Backoff::STOPS`]) | the run stops: every recipient not sent yet is skipped |
//! | anything else | reported for that recipient |
//!
//! **Never resent when it may have gone out.** Whatever the policy says, a
//! recipient is retried only when the error is retryable and
//! `Error::may_have_been_sent` is false: a timed-out send, or a 5xx after
//! the request reached Meta, is reported as it is, never replayed (the
//! library's rule). Reconcile those with the status webhooks: give each
//! message a `biz_opaque_callback_data` (`OutboundMessage::callback_data`)
//! in [`BroadcastBuilder::compose`]. [`BroadcastBuilder::client`] turns
//! the client's own replays off, so a throttled send is retried here,
//! through the pacer, and nowhere else.
//!
//! A [`Outcome::Sent`] means Meta accepted the message, not that it was
//! delivered: most `131049` refusals arrive later, as a `failed` status
//! webhook (`templates/marketing-templates/per-user-limits`). Meta's daily
//! messaging limit (unique users per 24 hours, per business portfolio,
//! `messaging-limits`) is Meta's to enforce; nothing here counts it.
//!
//! **Not durable.** The run lives in memory: a restart loses it, and the
//! report is the only record. Durable, resumable jobs are roadmap item
//! B3 (a typed store on `KvStore`).

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::fmt;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::{Either, join_all, select};
use meta_whatsapp_client::messages::{MessageContent, OutboundMessage, SendResponse};
use meta_whatsapp_client::{Client, RetryPolicy};
use meta_whatsapp_core::error::{ConfigError, ValidationError};
use meta_whatsapp_core::ids::PhoneNumberId;
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::{Error, ErrorKind, Result};
use time::OffsetDateTime;
use tokio::sync::watch;

use crate::outbound::{ClientOutbound, Outbound};
use crate::pacer::{Pacer, after, elapsed};

/// What a failed send becomes: retried later, reported, or the end of the
/// run. The default is [`Backoff`].
///
/// A [`Verdict::RetryAfter`] is honoured only when the error is retryable
/// and `Error::may_have_been_sent` is false; otherwise the recipient is
/// reported as failed. That rule is the library's and no policy lifts it.
pub trait BroadcastPolicy: Send + Sync + fmt::Debug + 'static {
    /// The send to one recipient failed with `error`; `failures` is how many
    /// sends to that recipient have failed, this one included (1 the first
    /// time).
    fn on_failure(&self, error: &Error, failures: u32) -> Verdict;

    /// Whether `error` means the number sends too fast, so its pacer slows
    /// down ([`crate::RateLimiter::slow_down`]).
    fn slows_down(&self, error: &Error) -> bool;
}

impl<T: BroadcastPolicy + ?Sized> BroadcastPolicy for Arc<T> {
    fn on_failure(&self, error: &Error, failures: u32) -> Verdict {
        (**self).on_failure(error, failures)
    }

    fn slows_down(&self, error: &Error) -> bool {
        (**self).slows_down(error)
    }
}

/// What a [`BroadcastPolicy`] makes of a failed send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// Report the error for this recipient; go on with the others.
    Fail,
    /// Send to this recipient again, not before this delay; the others go
    /// on meanwhile. Honoured only when the error is retryable and nothing
    /// may have been sent (see [`BroadcastPolicy`]); otherwise a
    /// [`Verdict::Fail`].
    RetryAfter(Duration),
    /// Report the error for this recipient and stop the run: every
    /// recipient not sent yet is skipped (sends in flight finish).
    Stop,
}

/// The default [`BroadcastPolicy`], read from `ErrorKind` and the Graph
/// code (never the error's text):
///
/// - [`Backoff::STOPS`] (the token, a permission, the account, the
///   classification limit or payment): stop the run;
/// - past [`Backoff::max_attempts`] sends to a recipient, or an error that
///   is not retryable (`131049`, `131048`, `131050`, `131047`, …): fail;
/// - the pair rate limit (`131056`): retry after `4^(failures - 1)`
///   seconds, Meta's schedule (1, 4, 16, 64 s);
/// - the number in maintenance (`131057`): retry after
///   [`Backoff::MAINTENANCE_RETRY`];
/// - any other retryable error (`130429`, a 4xx `131000`, a connection
///   refused): retry after [`Backoff::base_delay`] doubled per failure, up
///   to [`Backoff::max_delay`].
///
/// It slows the pacer down on `RateLimited` (`130429`, `80007`, `4`, an
/// HTTP 429), `SpamRateLimited` (`131048`) and maintenance (`131057`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct Backoff {
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
}

impl Default for Backoff {
    /// Five sends per recipient at most, 1 second doubling up to 60.
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        }
    }
}

/// `131057`: "Business Account is in maintenance mode", which Meta's
/// throughput upgrade causes for up to a minute (`throughput`).
const MAINTENANCE: i64 = 131_057;

fn in_maintenance(error: &Error) -> bool {
    error.graph().is_some_and(|g| g.code == MAINTENANCE)
}

impl Backoff {
    /// The kinds that stop a run: they hold for every recipient, so every
    /// send left would fail the same way.
    pub const STOPS: &'static [ErrorKind] = &[
        ErrorKind::Authentication,
        ErrorKind::Permission,
        ErrorKind::AccountRestricted,
        ErrorKind::ClassificationLimitReached,
        ErrorKind::Payment,
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

    /// Sends to one recipient at most, the first included (default 5; zero
    /// counts as one: no retry).
    pub fn max_attempts(mut self, sends: u32) -> Self {
        self.max_attempts = sends.max(1);
        self
    }

    /// The first retry's delay, doubled for each later one (default 1 s).
    /// The pair rate limit and maintenance follow their own schedules.
    pub fn base_delay(mut self, delay: Duration) -> Self {
        self.base_delay = delay;
        self
    }

    /// The longest delay before a retry (default 60 s).
    pub fn max_delay(mut self, delay: Duration) -> Self {
        self.max_delay = delay;
        self
    }
}

impl BroadcastPolicy for Backoff {
    fn on_failure(&self, error: &Error, failures: u32) -> Verdict {
        let kind = error.kind();
        if Self::STOPS.contains(&kind) {
            return Verdict::Stop;
        }
        if failures >= self.max_attempts || !error.is_retryable() {
            return Verdict::Fail;
        }
        let exponent = failures.saturating_sub(1);
        if kind == ErrorKind::PairRateLimited {
            // `about-the-platform` § Pair rate limits: "retry after 4^X
            // seconds (starting with X=0 and increasing X by 1 after each
            // failure)".
            return Verdict::RetryAfter(Duration::from_secs(4u64.saturating_pow(exponent)));
        }
        if in_maintenance(error) {
            return Verdict::RetryAfter(Self::MAINTENANCE_RETRY);
        }
        Verdict::RetryAfter(
            self.base_delay
                .saturating_mul(2u32.saturating_pow(exponent))
                .min(self.max_delay),
        )
    }

    fn slows_down(&self, error: &Error) -> bool {
        matches!(
            error.kind(),
            ErrorKind::RateLimited | ErrorKind::SpamRateLimited
        ) || in_maintenance(error)
    }
}

/// Whether a failed send may be sent again: the library's rule, applied
/// whatever the policy says. Retryable, and provably not sent.
fn may_resend(error: &Error) -> bool {
    error.is_retryable() && !error.may_have_been_sent()
}

/// How far a broadcast got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Progress {
    /// Recipients in the broadcast, as listed (duplicates included).
    pub total: usize,
    /// Accepted by Meta ([`Outcome::Sent`]).
    pub sent: usize,
    /// Given up on ([`Outcome::Failed`]).
    pub failed: usize,
    /// Never sent because the run was cancelled or stopped
    /// ([`Outcome::Skipped`]).
    pub skipped: usize,
    /// Listed again, not sent to again ([`Outcome::Duplicate`]); counted
    /// from the start.
    pub duplicates: usize,
}

impl Progress {
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
pub enum Ended {
    /// Every recipient was settled.
    Completed,
    /// [`BroadcastHandle::cancel`] stopped it.
    Cancelled,
    /// A [`Verdict::Stop`] stopped it, on the failure of the recipient at
    /// this index; or the rate limiter failed on that recipient.
    Stopped {
        /// Index (in the order given) of the recipient whose failure
        /// stopped the run.
        recipient: usize,
    },
}

/// What happened to one recipient.
#[derive(Debug)]
#[non_exhaustive]
pub enum Outcome {
    /// Meta accepted the message (the response carries its id, the key of
    /// its status webhooks). Accepted is not delivered.
    Sent(SendResponse),
    /// Given up on, with the last error. When
    /// `error.may_have_been_sent()` is true it may have gone out:
    /// reconcile with the status webhooks before sending again.
    Failed(Error),
    /// Never sent: the run was cancelled or stopped before (or while
    /// waiting to retry after a failure that proved nothing was sent).
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
    /// [`BroadcastReport::recipients`]).
    pub index: usize,
    /// The recipient, as given.
    pub recipient: Recipient,
    /// Sends handed to the outbound for them, retries included (a send the
    /// client refused before any request counts too); zero when none was:
    /// the message could not be composed, the run ended first, or a
    /// duplicate.
    pub attempts: u32,
    /// What happened.
    pub outcome: Outcome,
}

/// What a run did: one line per recipient, in the order given.
#[derive(Debug)]
#[non_exhaustive]
pub struct BroadcastReport {
    /// One per recipient, in the order given.
    pub recipients: Vec<RecipientReport>,
    /// How the run ended.
    pub ended: Ended,
}

impl BroadcastReport {
    /// The counts of [`Self::recipients`].
    pub fn progress(&self) -> Progress {
        let mut progress = Progress {
            total: self.recipients.len(),
            ..Progress::default()
        };
        for line in &self.recipients {
            match line.outcome {
                Outcome::Sent(_) => progress.sent += 1,
                Outcome::Failed(_) => progress.failed += 1,
                Outcome::Skipped => progress.skipped += 1,
                Outcome::Duplicate { .. } => progress.duplicates += 1,
            }
        }
        progress
    }
}

/// Whether the run goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halt {
    Running,
    Cancelled,
    Stopped,
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
    fn progress(&self) -> Progress {
        Progress {
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
    pub fn progress(&self) -> Progress {
        self.shared.progress()
    }

    /// Stop sending: no send starts after this (a wait for a slot or for a
    /// retry ends at once); sends in flight finish and are reported, since
    /// abandoning one would leave unknown whether it went out. Recipients
    /// not sent are [`Outcome::Skipped`]. Nothing after the run ended.
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

/// A send waiting for its retry time.
struct Deferred {
    at: OffsetDateTime,
    index: usize,
    message: Box<OutboundMessage>,
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
    /// Each settled recipient's outcome, for the report.
    outcomes: Vec<Option<Outcome>>,
    stopped_by: Option<usize>,
    /// The run's latest time, and how far the pacer's clock stepped back
    /// in all (see [`Book::now`]).
    seen: Option<OffsetDateTime>,
    skew: Duration,
}

/// What a worker does next.
enum Next {
    /// Send to this recipient (its message, when a retry).
    Send(usize, Option<Box<OutboundMessage>>),
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

    fn next(&mut self, wall: OffsetDateTime) -> Next {
        let now = self.now(wall);
        if self.deferred.peek().is_some_and(|d| d.at <= now)
            && let Some(due) = self.deferred.pop()
        {
            return Next::Send(due.index, Some(due.message));
        }
        while self.settled.get(self.next).copied().unwrap_or(false) {
            self.next += 1;
        }
        if self.next < self.settled.len() {
            self.next += 1;
            return Next::Send(self.next - 1, None);
        }
        match self.deferred.peek() {
            Some(due) => Next::Wait(elapsed(now, due.at)),
            None => Next::Done,
        }
    }

    /// Send to the recipient at `index` again, `delay` from now.
    fn defer(
        &mut self,
        wall: OffsetDateTime,
        delay: Duration,
        index: usize,
        message: Box<OutboundMessage>,
    ) {
        let at = after(self.now(wall), delay);
        self.deferred.push(Deferred { at, index, message });
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
    /// `Send`: spawn it to run in the background.
    pub async fn run(self) -> BroadcastReport {
        let workers = self.concurrency.min(self.recipients.len()).max(1);
        join_all((0..workers).map(|_| self.work())).await;
        self.shared.halt(Halt::Finished);
        let halt = *self.shared.halt.borrow();
        let book = self
            .book
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        let ended = match halt {
            Halt::Running | Halt::Finished => Ended::Completed,
            Halt::Cancelled => Ended::Cancelled,
            Halt::Stopped => Ended::Stopped {
                recipient: book.stopped_by.unwrap_or_default(),
            },
        };
        // Never sent: not reached, or waiting for a retry, when the run
        // halted.
        let unsettled = book.settled.iter().filter(|settled| !**settled).count();
        self.shared
            .skipped
            .fetch_add(unsettled, AtomicOrdering::AcqRel);
        let recipients = self
            .recipients
            .into_iter()
            .zip(book.outcomes)
            .zip(book.attempts)
            .enumerate()
            .map(
                |(index, ((recipient, outcome), attempts))| RecipientReport {
                    index,
                    recipient,
                    attempts,
                    outcome: outcome.unwrap_or(Outcome::Skipped),
                },
            )
            .collect();
        let report = BroadcastReport { recipients, ended };
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
            let next = self.book().next(self.pacer.now());
            match next {
                Next::Send(index, message) => self.attempt(index, message).await,
                Next::Wait(duration) => {
                    if !self.wait(duration).await {
                        return;
                    }
                }
                Next::Done => return,
            }
        }
    }

    /// Wait `duration` on the pacer's timer, unless the run halts first:
    /// whether the run goes on.
    async fn wait(&self, duration: Duration) -> bool {
        if !duration.is_zero() {
            let mut halt = self.shared.halt.subscribe();
            let halted = pin!(halt.wait_for(|halt| *halt != Halt::Running));
            let slept = pin!(self.pacer.sleep(duration));
            if let Either::Right(_) = select(slept, halted).await {
                return false;
            }
        }
        !self.shared.halted()
    }

    /// One send to the recipient at `index`: compose (the first time),
    /// wait for a slot, send, settle or defer.
    async fn attempt(&self, index: usize, message: Option<Box<OutboundMessage>>) {
        let message = match message {
            Some(message) => message,
            None => match self.compose(index) {
                Ok(message) => Box::new(message),
                Err(error) => return self.settle(index, Outcome::Failed(error)),
            },
        };
        let wait = match self.pacer.reserve(&self.from).await {
            Ok(wait) => wait,
            Err(error) => {
                // A pacer that cannot pace lets nothing through: every
                // recipient left would meet it.
                tracing::warn!(
                    error_kind = ?error.kind(),
                    "broadcast stopped: the rate limiter failed"
                );
                self.settle(index, Outcome::Failed(error));
                return self.stop(index);
            }
        };
        if !self.wait(wait).await {
            return; // cancelled or stopped while waiting: skipped
        }
        let result = self.outbound.send(&self.from, &message).await;
        let failures = {
            let mut book = self.book();
            book.attempts[index] += 1;
            book.attempts[index]
        };
        match result {
            Ok(response) => self.settle(index, Outcome::Sent(response)),
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
        if self.policy.slows_down(&error)
            && let Err(e) = self.pacer.slow_down(&self.from).await
        {
            tracing::warn!(error_kind = ?e.kind(), "broadcast could not slow the pacer down");
        }
        match self.policy.on_failure(&error, failures) {
            Verdict::RetryAfter(delay) if may_resend(&error) => {
                self.book().defer(self.pacer.now(), delay, index, message);
            }
            Verdict::Stop => {
                tracing::warn!(
                    recipient = index,
                    error_kind = ?error.kind(),
                    "broadcast stopped by a failure every recipient would meet"
                );
                self.settle(index, Outcome::Failed(error));
                self.stop(index);
            }
            Verdict::RetryAfter(_) | Verdict::Fail => {
                self.settle(index, Outcome::Failed(error));
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

    fn settle(&self, index: usize, outcome: Outcome) {
        let counter = match outcome {
            Outcome::Sent(_) => &self.shared.sent,
            Outcome::Failed(_) => &self.shared.failed,
            Outcome::Skipped => &self.shared.skipped,
            Outcome::Duplicate { .. } => &self.shared.duplicates,
        };
        {
            let mut book = self.book();
            book.settled[index] = true;
            book.outcomes[index] = Some(outcome);
        }
        counter.fetch_add(1, AtomicOrdering::AcqRel);
    }

    fn stop(&self, index: usize) {
        if self.shared.halt(Halt::Stopped) {
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
    /// not sent to again, and its line is [`Outcome::Duplicate`]. The same
    /// person is the same phone number, compared by its digits
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
    /// 24-hour window Meta refuses anything else, `131047`).
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

    /// Send through `outbound` (not a [`crate::PacedOutbound`]: the
    /// broadcast paces its sends itself, and would count each twice). An
    /// outbound over a client should give it `RetryPolicy::NONE`, as
    /// [`Self::client`] does: the client's own replays of a throttled send
    /// do not wait for the pacer.
    pub fn outbound(mut self, outbound: impl Outbound) -> Self {
        self.outbound = Some(Arc::new(outbound));
        self
    }

    /// Send through an outbound shared with other code.
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

    /// Sends in flight at once, at most (default 32). The pacer sets the
    /// rate; this only lets it be reached when sends are slow: about
    /// `rate × a send's duration` (80 a second at 400 ms each: 32).
    pub fn concurrency(mut self, sends: usize) -> Self {
        self.concurrency = sends;
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
        let mut outcomes: Vec<Option<Outcome>> = (0..total).map(|_| None).collect();
        for &(index, of) in &duplicates {
            settled[index] = true;
            outcomes[index] = Some(Outcome::Duplicate { of });
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
                stopped_by: None,
                seen: None,
                skew: Duration::ZERO,
            }),
            recipients: self.recipients,
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

    #[test]
    fn backoff_reads_the_kind() {
        let p = Backoff::default();
        let secs = |v: Verdict| match v {
            Verdict::RetryAfter(d) => Some(d.as_secs()),
            _ => None,
        };
        // Meta's 4^X schedule for the pair limit.
        let pair: Vec<_> = (1..=4)
            .map(|n| secs(p.on_failure(&api(131056, 400), n)))
            .collect();
        assert_eq!(pair, [Some(1), Some(4), Some(16), Some(64)]);
        assert_eq!(p.on_failure(&api(131056, 400), 5), Verdict::Fail);
        // Throughput: doubling from 1 s, capped.
        let throughput: Vec<_> = [1, 2, 3]
            .iter()
            .map(|n| secs(p.on_failure(&api(130429, 400), *n)))
            .collect();
        assert_eq!(throughput, [Some(1), Some(2), Some(4)]);
        assert_eq!(
            Backoff::new()
                .max_delay(Duration::from_secs(3))
                .on_failure(&api(130429, 400), 4),
            Verdict::RetryAfter(Duration::from_secs(3))
        );
        // Maintenance (a throughput upgrade, up to a minute): every 20 s,
        // five sends spanning 80 s; other `ServiceUnavailable` codes back
        // off as usual.
        let maintenance: Vec<_> = (1..=4)
            .map(|n| secs(p.on_failure(&api(131057, 400), n)))
            .collect();
        assert_eq!(maintenance, [Some(20), Some(20), Some(20), Some(20)]);
        assert_eq!(p.on_failure(&api(131057, 400), 5), Verdict::Fail);
        assert_eq!(
            secs(p.on_failure(&api(131000, 400), 2)),
            Some(2),
            "131000 is not maintenance"
        );
        // Never retried: the per-user marketing limit, spam, an opt-out.
        for code in [131049, 131048, 131050, 131047] {
            assert_eq!(p.on_failure(&api(code, 400), 1), Verdict::Fail, "{code}");
        }
        // Account-wide: the run stops.
        for code in [190, 10, 131031, 131064, 131042] {
            assert_eq!(p.on_failure(&api(code, 400), 1), Verdict::Stop, "{code}");
        }
        // Slow down on throughput, spam and maintenance; not on the pair
        // limit, the marketing limit or another outage.
        for code in [130429, 131048, 131057, 80007, 4] {
            assert!(p.slows_down(&api(code, 400)), "{code}");
        }
        for code in [131056, 131049, 131000] {
            assert!(!p.slows_down(&api(code, 400)), "{code}");
        }
        assert_eq!(
            Backoff::new()
                .max_attempts(0)
                .on_failure(&api(130429, 400), 1),
            Verdict::Fail
        );
    }

    #[test]
    fn only_a_provably_unsent_retryable_error_is_resent() {
        assert!(may_resend(&api(131056, 400)));
        assert!(
            may_resend(&api(130429, 503)),
            "throttling proves nothing was sent"
        );
        assert!(!may_resend(&Error::Transport(TransportError::Timeout)));
        assert!(!may_resend(&api(131000, 500)), "a 5xx may have been sent");
        assert!(may_resend(&api(131000, 400)));
        assert!(!may_resend(&api(131049, 400)), "not retryable");
        assert!(may_resend(&Error::Transport(TransportError::Connect(
            anyhow::anyhow!("refused")
        ))));
    }

    #[test]
    fn progress_counts_what_is_left() {
        let p = Progress {
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
            stopped_by: None,
            seen: None,
            skew: Duration::ZERO,
        };
        book.defer(
            t0,
            Duration::from_secs(1),
            0,
            Box::new(OutboundMessage::text(Recipient::phone("+1"), "x")),
        );
        let back = t0 - time::Duration::hours(1);
        assert!(matches!(book.next(back), Next::Wait(d) if d == Duration::from_secs(1)));
        let later = back + time::Duration::milliseconds(400);
        assert!(matches!(book.next(later), Next::Wait(d) if d == Duration::from_millis(600)));
        assert!(matches!(
            book.next(later + time::Duration::milliseconds(600)),
            Next::Send(0, Some(_))
        ));
    }
}
