//! Retention and auto-delete of the stored chat history: by age and by a
//! cap on each chat, through the [`ConversationStore`] the history is in
//! (the CMS inbox's, `meta_whatsapp_rs::inbox`, or any other).
//!
//! The bot stores no history of its own: what it deletes is the store's,
//! with the store's own operations, so the store's rules hold:
//!
//! - **by age** ([`RetentionPolicy::max_age`]): the messages older than
//!   the cutoff, the window events, thread ownership records and contact
//!   removals before it, and the summary of a chat whose latest message
//!   went ([`ConversationStore::purge_before`]);
//! - **by a cap per chat** ([`RetentionPolicy::max_messages_per_chat`]):
//!   each chat's rows beyond its newest `n`, every row of its history
//!   counting (revokes' tombstones and messages an erasure redacted too),
//!   and the summary of a chat whose latest message went
//!   ([`ConversationStore::trim_conversations`]), whatever its age: with
//!   it goes the customer service window it holds, still open or not
//!   (every chat's with a cap of 0), until the customer's next message.
//!
//! Either way, the synced address book and the links between a person's
//! identities stay (design D35: a later erasure follows them to the
//! person's other keys), and a chat keeping its latest message keeps its
//! summary as it was. A group is a chat like any other: retention deletes
//! its oldest rows whoever sent them, and leaves what an erasure redacted
//! (design D31) redacted until it is old enough to go. Retention is not
//! erasure: to delete a person, call `erase_all` over their
//! `identities`.
//!
//! [`RetentionPolicy::apply`] runs one sweep at a time the caller gives:
//! call it from your own housekeeping. [`RetentionSweeper`] runs it every
//! [`RetentionSweeper::every`] on the bot's [`Timer`] until stopped.
//! Nothing deletes on its own, and the policy keeps everything by default;
//! which retention a deployment needs is its privacy obligations' call.
//!
//! ```no_run
//! # async fn demo(store: std::sync::Arc<dyn meta_whatsapp_core::store::ConversationStore>) {
//! use std::time::Duration;
//! use meta_whatsapp_bot::retention::{RetentionPolicy, RetentionSweeper};
//!
//! let policy = RetentionPolicy::default()
//!     .max_age(Duration::from_hours(24 * 30)) // 30 days
//!     .max_messages_per_chat(500);
//! let sweeper = RetentionSweeper::new(store, policy).every(Duration::from_hours(1));
//! let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
//! let task = tokio::spawn(async move {
//!     sweeper.run(async { let _ = stopped.await; }).await;
//! });
//! // … on shutdown:
//! let _ = stop.send(());
//! let _ = task.await;
//! # }
//! ```

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use meta_whatsapp_core::clock::SystemClock;
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::ids::PhoneNumberId;
use meta_whatsapp_core::store::{ConversationStore, Purged, Retention};
use time::OffsetDateTime;

use crate::pacer::Timer;

/// How long the stored chat history is kept: by age, by a cap per chat,
/// both, or (the default) forever.
///
/// Build it with [`max_age`](Self::max_age) and
/// [`max_messages_per_chat`](Self::max_messages_per_chat), and apply it
/// with [`apply`](Self::apply) or a [`RetentionSweeper`]. It is not the
/// store's own [`Retention`] (what the store's
/// [`apply_retention`](ConversationStore::apply_retention) purges): it
/// calls the store's operations with its own cutoff and cap, whatever
/// retention the store was configured with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RetentionPolicy {
    /// Delete what is older than this; `None` keeps any age.
    pub max_age: Option<Duration>,
    /// Keep each chat's newest `n` rows and delete the others; `None`
    /// keeps any number. `Some(0)` keeps no history at all, nor any
    /// chat's summary: each sweep closes every customer service window
    /// it holds, until the customer's next message (see
    /// [`ConversationStore::trim_conversations`]).
    pub max_messages_per_chat: Option<usize>,
}

impl RetentionPolicy {
    /// This policy, deleting what is older than `max_age`.
    #[must_use]
    pub fn max_age(mut self, max_age: Duration) -> Self {
        self.max_age = Some(max_age);
        self
    }

    /// This policy, keeping each chat's newest `n` rows (messages of
    /// either direction, synced ones, revokes' tombstones and redacted
    /// messages all count).
    #[must_use]
    pub fn max_messages_per_chat(mut self, n: usize) -> Self {
        self.max_messages_per_chat = Some(n);
        self
    }

    /// Whether it deletes nothing: neither an age nor a cap is set.
    pub fn keeps_everything(&self) -> bool {
        self.max_age.is_none() && self.max_messages_per_chat.is_none()
    }

    /// The cutoff at `now`: what is older is deleted. `None` when no age
    /// is set, or when it reaches before the earliest representable time.
    pub fn cutoff(&self, now: OffsetDateTime) -> Option<OffsetDateTime> {
        Retention::For(self.max_age?).cutoff(now)
    }

    /// One sweep at `now`, of business number `phone_number_id` only, or of
    /// every number in `store` with `None`: first by age
    /// ([`ConversationStore::purge_before`] at [`cutoff`](Self::cutoff)),
    /// then by the cap ([`ConversationStore::trim_conversations`]). Stops
    /// at the first error: what the purge deleted stays deleted, and the
    /// next sweep does the rest.
    ///
    /// Give `None` only on a store this bot's numbers alone use: a store
    /// shared by several tenants (the service's) would lose every
    /// tenant's history to one policy.
    ///
    /// # Errors
    ///
    /// The store's.
    pub async fn apply(
        &self,
        store: &dyn ConversationStore,
        phone_number_id: Option<&PhoneNumberId>,
        now: OffsetDateTime,
    ) -> Result<Swept, StorageError> {
        let mut swept = Swept::default();
        if let Some(cutoff) = self.cutoff(now) {
            swept.by_age = store.purge_before(phone_number_id, cutoff).await?;
        }
        if let Some(keep_last) = self.max_messages_per_chat {
            swept.by_cap = store.trim_conversations(phone_number_id, keep_last).await?;
        }
        Ok(swept)
    }
}

/// What a sweep deleted: by age, then by the cap per chat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Swept {
    /// What the purge by age deleted.
    pub by_age: Purged,
    /// What the cap per chat deleted (messages and summaries only).
    pub by_cap: Purged,
}

impl Swept {
    /// Both together.
    pub fn total(&self) -> Purged {
        let mut total = self.by_age;
        total += self.by_cap;
        total
    }

    /// Whether nothing was deleted.
    pub fn is_empty(&self) -> bool {
        self.by_age.is_empty() && self.by_cap.is_empty()
    }
}

/// Applies a [`RetentionPolicy`] to a store on a schedule: a sweep at
/// once, then one every [`every`](Self::every) (an hour by default), on
/// a [`Timer`] (`SystemClock` by default; a `ManualClock` in tests).
///
/// It sweeps every number of the store, or the numbers given with
/// [`number`](Self::number), one after the other. Several replicas may
/// each run one on a shared store: their sweeps delete the same rows and
/// stay correct (on Postgres they take turns), only redundant.
pub struct RetentionSweeper {
    store: Arc<dyn ConversationStore>,
    policy: RetentionPolicy,
    numbers: Vec<PhoneNumberId>,
    every: Duration,
    timer: Arc<dyn Timer>,
}

impl fmt::Debug for RetentionSweeper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetentionSweeper")
            .field("store", &self.store)
            .field("policy", &self.policy)
            .field("numbers", &self.numbers)
            .field("every", &self.every)
            .finish_non_exhaustive()
    }
}

impl RetentionSweeper {
    /// The interval between two sweeps unless set: an hour.
    pub const DEFAULT_INTERVAL: Duration = Duration::from_hours(1);

    /// The shortest interval kept: a zero one is taken as this.
    pub const SHORTEST_INTERVAL: Duration = Duration::from_millis(1);

    /// A sweeper applying `policy` to every number of `store`.
    pub fn new(store: Arc<dyn ConversationStore>, policy: RetentionPolicy) -> Self {
        Self {
            store,
            policy,
            numbers: Vec::new(),
            every: Self::DEFAULT_INTERVAL,
            timer: Arc::new(SystemClock),
        }
    }

    /// Sweep business number `phone_number_id` (call it once per number):
    /// once one is given, only the numbers given are swept. Give the bot's
    /// numbers on a store other tenants use too.
    #[must_use]
    pub fn number(mut self, phone_number_id: PhoneNumberId) -> Self {
        if !self.numbers.contains(&phone_number_id) {
            self.numbers.push(phone_number_id);
        }
        self
    }

    /// Sweep every `every` (at least [`Self::SHORTEST_INTERVAL`]).
    #[must_use]
    pub fn every(mut self, every: Duration) -> Self {
        self.every = every.max(Self::SHORTEST_INTERVAL);
        self
    }

    /// Tell the time and wait on `timer`.
    #[must_use]
    pub fn timer(mut self, timer: Arc<dyn Timer>) -> Self {
        self.timer = timer;
        self
    }

    /// The policy it applies.
    pub fn policy(&self) -> RetentionPolicy {
        self.policy
    }

    /// One sweep now (the timer's): [`RetentionPolicy::apply`] for each
    /// number given, in order, or for every number. Stops at the first
    /// error.
    ///
    /// # Errors
    ///
    /// The store's.
    pub async fn sweep(&self) -> Result<Swept, StorageError> {
        let now = self.timer.now();
        if self.numbers.is_empty() {
            return self.policy.apply(self.store.as_ref(), None, now).await;
        }
        let mut swept = Swept::default();
        for number in &self.numbers {
            let one = self
                .policy
                .apply(self.store.as_ref(), Some(number), now)
                .await?;
            swept.by_age += one.by_age;
            swept.by_cap += one.by_cap;
        }
        Ok(swept)
    }

    /// Sweep at once, then every [`every`](Self::every), until `stop`
    /// completes. A sweep already running when `stop` completes finishes
    /// first; `stop` is checked before each sweep, so once it completed
    /// no other sweep starts. A failed sweep is logged (the error, never a
    /// message) and the next one runs on schedule.
    pub async fn run(&self, stop: impl Future<Output = ()>) {
        let mut stop = std::pin::pin!(stop);
        loop {
            let stopped =
                std::future::poll_fn(|cx| Poll::Ready(stop.as_mut().poll(cx).is_ready())).await;
            if stopped {
                return;
            }
            match self.sweep().await {
                Ok(swept) if !swept.is_empty() => {
                    let total = swept.total();
                    tracing::debug!(
                        messages = total.messages,
                        conversations = total.conversations,
                        "retention sweep"
                    );
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(error = %error, "a retention sweep failed"),
            }
            let mut wait = self.timer.sleep(self.every);
            let stopped = std::future::poll_fn(|cx| {
                if stop.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(true);
                }
                wait.as_mut().poll(cx).map(|()| false)
            })
            .await;
            if stopped {
                return;
            }
        }
    }
}
