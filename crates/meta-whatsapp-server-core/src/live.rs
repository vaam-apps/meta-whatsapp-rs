//! Live event streams (docs/design/server.md, section 4.5; roadmap M2b):
//! a tenant's events as they are recorded, for an API adapter to serve
//! as Server-Sent Events (`GET /v1/events/stream`,
//! `GET /v1/numbers/{pn}/events/stream`), with no framework type: a
//! [`LiveStream`] is a `futures` stream of [`Frame`]s.
//!
//! **The outbox is the source of truth.** A stream is a cursor in its
//! tenant's stream of sequences, the one `GET /v1/events` pages
//! ([`crate::events::poll`]): it sends what the outbox holds after the
//! cursor, with the same filters and the same tenant, read with
//! [`Outbox::page`]. It reads when its backend's [`EventNotifier`] wakes
//! it (a wake-up only: [`crate::notify`]) and, whatever it heard, every
//! [`LiveSettings::repoll`]: a notification lost delays an event by that
//! much at most, and loses nothing. So a stream resumed with the last
//! sequence it sent (`Last-Event-ID`) continues exactly after it, on any
//! replica, within retention; a cursor below what was purged is `410
//! cursor_expired` at the start, and a cursor past the tenant's newest
//! sequence `422`, as for polling. Without a cursor a stream starts at
//! the tenant's newest sequence: only what comes next.
//!
//! **Only its tenant's.** Every read names the stream's tenant, from the
//! [`Caller`] the [`Authorizer`] made (operator-only rows have no tenant
//! and never match), and a subscription hears that tenant's wake-ups
//! only.
//!
//! **Bounded.** At most [`LiveSettings::per_tenant`] streams of one
//! tenant and [`LiveSettings::per_replica`] in all are open on a replica
//! (the next is `429 too_many_streams`). A stream reads at most
//! [`LiveSettings::buffer_bytes`] of data at a time and holds at most that
//! much (and [`LiveSettings::buffer_frames`] frames) its client has not
//! taken yet (an event larger than the budget alone): a client that takes
//! nothing for [`LiveSettings::slow_client`] is dropped, its stream ending
//! with [`Frame::Lagged`] after what it holds, so it resumes from its last
//! sequence (or polls from it). A stream whose cursor fell below what
//! housekeeping purged ends with [`Frame::Lagged`] too: its client lost
//! events, and reloads them. Every [`LiveSettings::heartbeat`] a stream
//! sends a [`Frame::Heartbeat`]. When the service shuts down
//! ([`Streams::shut_down`]) every stream ends with [`Frame::Shutdown`]:
//! its client resumes on another replica.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use meta_whatsapp_rs::core::ids::PhoneNumberId;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::ServiceError;
use crate::authz::{Authorizer, Caller};
use crate::error::ErrorCode;
use crate::model::TenantId;
use crate::notify::{EventNotifier, Subscription};
use crate::outbox::{EventPage, EventQuery, Outbox, StoredEvent};

/// Every log line of the live streams.
const LOG_TARGET: &str = "meta_whatsapp_server::live";

/// Default [`LiveSettings::per_tenant`]: the design's 20 streams per
/// tenant (section 4.5).
pub const DEFAULT_STREAMS_PER_TENANT: usize = 20;

/// Default [`LiveSettings::per_replica`]: the design's 1,000 streams per
/// replica (section 4.5).
pub const DEFAULT_STREAMS_PER_REPLICA: usize = 1000;

/// [`LiveSettings::heartbeat`]: the design's keepalive every 15 s
/// (section 4.5).
pub const HEARTBEAT: Duration = Duration::from_secs(15);

/// Default [`LiveSettings::repoll`]: how often a stream reads the outbox
/// with no wake-up, the fallback for a lost notification.
pub const DEFAULT_REPOLL: Duration = Duration::from_secs(15);

/// Default [`LiveSettings::buffer_bytes`]: a stream's events read at once,
/// and held for its client, 1 MiB.
pub const DEFAULT_BUFFER_BYTES: usize = 1024 * 1024;

/// Default [`LiveSettings::buffer_frames`]: frames held for a client.
pub const DEFAULT_BUFFER_FRAMES: usize = 64;

/// Default [`LiveSettings::slow_client`]: how long a client may take
/// nothing while its buffer is full before it is dropped.
pub const DEFAULT_SLOW_CLIENT: Duration = Duration::from_secs(30);

/// Events a stream reads at once (with [`LiveSettings::buffer_bytes`]).
pub const READ_PAGE: usize = 100;

/// What a replica's streams are allowed ([module](self)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveSettings {
    /// Streams of one tenant open at once on a replica.
    pub per_tenant: usize,
    /// Streams open at once on a replica.
    pub per_replica: usize,
    /// Between two heartbeats.
    pub heartbeat: Duration,
    /// Between two reads of the outbox without a wake-up.
    pub repoll: Duration,
    /// Data a stream reads at once and holds for its client.
    pub buffer_bytes: usize,
    /// Frames a stream holds for its client.
    pub buffer_frames: usize,
    /// How long a full buffer may wait for its client before the client
    /// is dropped.
    pub slow_client: Duration,
}

impl Default for LiveSettings {
    fn default() -> Self {
        Self {
            per_tenant: DEFAULT_STREAMS_PER_TENANT,
            per_replica: DEFAULT_STREAMS_PER_REPLICA,
            heartbeat: HEARTBEAT,
            repoll: DEFAULT_REPOLL,
            buffer_bytes: DEFAULT_BUFFER_BYTES,
            buffer_frames: DEFAULT_BUFFER_FRAMES,
            slow_client: DEFAULT_SLOW_CLIENT,
        }
    }
}

/// Which of its tenant's events a stream sends: [`EventQuery`]'s filters,
/// and the cursor it starts after.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamQuery {
    /// Start after this sequence of the tenant's stream (`Last-Event-ID`,
    /// or `?after=`); `None` starts at the tenant's newest sequence.
    pub after: Option<i64>,
    /// The request's name for [`Self::after`], for a `422` on it
    /// (`invalid_request` with this `field`); `after` when empty.
    pub after_field: &'static str,
    /// Only these types; `None` for every type.
    pub types: Option<Vec<String>>,
    /// Only this phone number's events.
    pub phone_number_id: Option<PhoneNumberId>,
}

/// What a [`LiveStream`] yields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// An event of the tenant's, after the previous one's sequence.
    Event(StoredEvent),
    /// Nothing new: the connection is alive.
    Heartbeat,
    /// The last frame: the client was too slow (dropped), or events after
    /// `after` were purged before the stream sent them. Resume after
    /// `after` (the last event's sequence the stream sent, or the cursor
    /// it started after), or reload history.
    Lagged {
        /// The last sequence sent.
        after: i64,
    },
    /// The last frame: this replica is shutting down; resume after
    /// `after` on another.
    Shutdown {
        /// The last sequence sent.
        after: i64,
    },
}

/// How a stream's producer ended.
const RUNNING: u8 = 0;
const LAGGED: u8 = 1;
const SHUTDOWN: u8 = 2;

/// A replica's live streams: their slots, their shutdown, and what they
/// read (the backend's outbox and notifier). Cheap to clone.
#[derive(Clone)]
pub struct Streams {
    shared: Arc<Shared>,
}

struct Shared {
    outbox: Arc<dyn Outbox>,
    notifier: Arc<dyn EventNotifier>,
    settings: LiveSettings,
    open: Mutex<Open>,
    /// Bytes of data held for clients, every stream's.
    buffered: AtomicUsize,
    shutdown: watch::Sender<bool>,
}

#[derive(Default)]
struct Open {
    total: usize,
    by_tenant: HashMap<TenantId, usize>,
}

impl std::fmt::Debug for Streams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Streams")
            .field("open", &self.open())
            .finish_non_exhaustive()
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Open> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A stream's place among [`LiveSettings::per_tenant`] and
/// [`LiveSettings::per_replica`], given back when dropped.
struct StreamSlot {
    shared: Arc<Shared>,
    tenant: TenantId,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut open = self.shared.lock();
        open.total = open.total.saturating_sub(1);
        if let Some(count) = open.by_tenant.get_mut(&self.tenant) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                open.by_tenant.remove(&self.tenant);
            }
        }
    }
}

impl Streams {
    /// The streams of a replica, reading `outbox` and woken by `notifier`
    /// (the same backend's: [`crate::backend::Backend`]), within
    /// `settings`.
    pub fn new(
        outbox: Arc<dyn Outbox>,
        notifier: Arc<dyn EventNotifier>,
        settings: LiveSettings,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                outbox,
                notifier,
                settings,
                open: Mutex::new(Open::default()),
                buffered: AtomicUsize::new(0),
                shutdown: watch::Sender::new(false),
            }),
        }
    }

    /// The settings.
    pub fn settings(&self) -> LiveSettings {
        self.shared.settings
    }

    /// Streams open now on this replica.
    pub fn open(&self) -> usize {
        self.shared.lock().total
    }

    /// Streams of `tenant` open now on this replica.
    pub fn open_for(&self, tenant: &TenantId) -> usize {
        self.shared
            .lock()
            .by_tenant
            .get(tenant)
            .copied()
            .unwrap_or_default()
    }

    /// Bytes of event data held now for clients that have not taken them,
    /// every stream's: at most [`LiveSettings::buffer_bytes`] per stream
    /// (an event larger than that alone).
    pub fn buffered_bytes(&self) -> usize {
        self.shared.buffered.load(Ordering::Relaxed)
    }

    /// End every stream with [`Frame::Shutdown`], and refuse new ones
    /// (`503 shutting_down`): the service is stopping.
    pub fn shut_down(&self) {
        self.shared.shutdown.send_replace(true);
    }

    /// A slot for `tenant`, or `429 too_many_streams`.
    fn slot(&self, tenant: &TenantId) -> Result<StreamSlot, ServiceError> {
        let settings = self.shared.settings;
        let mut open = self.shared.lock();
        let mine = open.by_tenant.get(tenant).copied().unwrap_or_default();
        if open.total >= settings.per_replica || mine >= settings.per_tenant {
            return Err(ServiceError::new(ErrorCode::TOO_MANY_STREAMS).retryable(true));
        }
        open.total += 1;
        *open.by_tenant.entry(tenant.clone()).or_default() += 1;
        Ok(StreamSlot {
            shared: self.shared.clone(),
            tenant: tenant.clone(),
        })
    }

    /// A live stream of `caller`'s tenant's events (the [module](self)'s
    /// rules), once `authz`, which made `caller`, admits it. The cursor is
    /// checked before it returns, so an adapter answers its errors as a
    /// status, before the stream's head.
    ///
    /// # Errors
    ///
    /// `403 forbidden` for a `caller` another [`Authorizer`] made; `429
    /// too_many_streams` past the tenant's or the replica's streams; `503
    /// shutting_down` once [`Self::shut_down`]; `410 cursor_expired` for a
    /// cursor below what was purged, `422 invalid_request` on
    /// [`StreamQuery::after_field`] for one past the tenant's newest
    /// sequence; `503 storage_unavailable` when the outbox or the
    /// notifier fails.
    pub async fn open_stream(
        &self,
        authz: &Authorizer,
        caller: &Caller,
        query: StreamQuery,
    ) -> Result<LiveStream, ServiceError> {
        authz.admit(caller)?;
        self.start(caller.tenant().clone(), query).await
    }

    /// [`Self::open_stream`] for one of the tenant's numbers, `pn`: bound
    /// to `caller`'s tenant (step 4 of the authorization order, else `404
    /// not_found`, nothing read of the outbox; no token is needed, so the
    /// vault is not read either), its events only.
    ///
    /// # Errors
    ///
    /// As [`Self::open_stream`], and `404 not_found` for a number the
    /// tenant does not hold.
    pub async fn open_number_stream(
        &self,
        authz: &Authorizer,
        caller: &Caller,
        pn: PhoneNumberId,
        mut query: StreamQuery,
    ) -> Result<LiveStream, ServiceError> {
        authz.tenant_number(caller, &pn).await?;
        query.phone_number_id = Some(pn);
        self.start(caller.tenant().clone(), query).await
    }

    async fn start(
        &self,
        tenant: TenantId,
        query: StreamQuery,
    ) -> Result<LiveStream, ServiceError> {
        if *self.shared.shutdown.borrow() {
            return Err(ServiceError::new(ErrorCode::SHUTTING_DOWN).retryable(true));
        }
        let slot = self.slot(&tenant)?;
        // Subscribed before the first read: an event committed after that
        // read wakes the stream.
        let subscription = self.shared.notifier.subscribe(&tenant).await?;
        let settings = self.shared.settings;
        let reader = Reader {
            outbox: self.shared.outbox.clone(),
            query: EventQuery {
                tenant,
                after: None,
                types: query.types,
                phone_number_id: query.phone_number_id,
                limit: READ_PAGE,
                max_bytes: settings.buffer_bytes.max(1),
            },
        };
        let field = if query.after_field.is_empty() {
            "after"
        } else {
            query.after_field
        };
        let (cursor, first) = match query.after {
            Some(after) => {
                let page = reader.read(after).await?;
                if after < page.purged_through {
                    return Err(ServiceError::new(ErrorCode::CURSOR_EXPIRED));
                }
                if after > page.high_water {
                    return Err(ServiceError::invalid(field));
                }
                (after, Some(page))
            }
            // Past every sequence: no event, the stream's bounds.
            None => (reader.read(i64::MAX).await?.high_water, None),
        };
        let (sender, receiver) = mpsc::channel(settings.buffer_frames.max(1));
        let end = Arc::new(AtomicU8::new(RUNNING));
        let producer = Producer {
            reader,
            settings,
            cursor,
            subscription,
            sender,
            budget: Arc::new(Semaphore::new(budget_permits(settings.buffer_bytes))),
            shared: self.shared.clone(),
            end: end.clone(),
            shutdown: self.shared.shutdown.subscribe(),
        };
        let task = tokio::spawn(producer.run(first));
        Ok(LiveStream {
            receiver,
            end,
            last: cursor,
            done: false,
            task,
            _slot: slot,
        })
    }
}

/// The byte budget as semaphore permits.
fn budget_permits(bytes: usize) -> usize {
    bytes.clamp(1, Semaphore::MAX_PERMITS)
}

/// A stream's reads of the outbox: its tenant's, with its filters.
struct Reader {
    outbox: Arc<dyn Outbox>,
    query: EventQuery,
}

impl Reader {
    async fn read(&self, after: i64) -> Result<EventPage, ServiceError> {
        let query = EventQuery {
            after: Some(after),
            ..self.query.clone()
        };
        Ok(self.outbox.page(&query).await?)
    }
}

/// A frame on its way to the client, with the share of the byte budget it
/// holds.
struct Item {
    frame: Frame,
    _held: Option<Held>,
}

/// Bytes of the budget held by a frame, given back when it is taken.
struct Held {
    _permit: OwnedSemaphorePermit,
    bytes: usize,
    shared: Arc<Shared>,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.shared
            .buffered
            .fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// What came of handing a frame over.
enum Handed {
    Sent,
    /// The client took nothing for [`LiveSettings::slow_client`].
    Slow,
    /// The client is gone.
    Gone,
}

/// What a producer does after reading.
enum Flow {
    /// Wait for a wake-up, the re-poll or the heartbeat.
    Wait,
    /// End the stream this way (`LAGGED`, `SHUTDOWN`).
    End(u8),
    /// The client is gone.
    Gone,
}

/// What a waiting producer woke for.
enum Wait {
    Shutdown,
    Gone,
    /// A wake-up.
    Read,
    /// The re-poll is due.
    Due,
    Heartbeat,
}

/// The task behind a [`LiveStream`]: reads the outbox after its cursor
/// when woken or due, and hands the frames over.
struct Producer {
    reader: Reader,
    settings: LiveSettings,
    cursor: i64,
    subscription: Subscription,
    sender: mpsc::Sender<Item>,
    budget: Arc<Semaphore>,
    shared: Arc<Shared>,
    end: Arc<AtomicU8>,
    shutdown: watch::Receiver<bool>,
}

impl Producer {
    async fn run(mut self, mut first: Option<EventPage>) {
        let mut next_heartbeat = Instant::now() + self.settings.heartbeat;
        let mut next_read = Instant::now() + self.settings.repoll;
        // A stream started without a cursor read its bounds when it opened,
        // subscribed already: what comes next wakes it.
        let mut read = first.is_some();
        loop {
            if read {
                match self.drain(first.take()).await {
                    Flow::Wait => {}
                    Flow::End(how) => {
                        self.end(how);
                        return;
                    }
                    Flow::Gone => return,
                }
            }
            read = true;
            loop {
                let wait = tokio::select! {
                    biased;
                    _ = self.shutdown.wait_for(|stopping| *stopping) => Wait::Shutdown,
                    () = self.sender.closed() => Wait::Gone,
                    _ = self.subscription.woken() => Wait::Read,
                    () = tokio::time::sleep_until(next_read) => Wait::Due,
                    () = tokio::time::sleep_until(next_heartbeat) => Wait::Heartbeat,
                };
                match wait {
                    Wait::Shutdown => {
                        self.end(SHUTDOWN);
                        return;
                    }
                    Wait::Gone => return,
                    Wait::Read => break,
                    Wait::Due => {
                        // The fallback: a notification lost costs this
                        // long, nothing more.
                        next_read = Instant::now() + self.settings.repoll;
                        break;
                    }
                    Wait::Heartbeat => {
                        next_heartbeat = Instant::now() + self.settings.heartbeat;
                        match self.hand(Frame::Heartbeat).await {
                            Handed::Sent => {}
                            Handed::Slow => {
                                self.end(LAGGED);
                                return;
                            }
                            Handed::Gone => return,
                        }
                    }
                }
            }
        }
    }

    /// Every event after the cursor, page by page (`first`, then reads):
    /// handed over, the cursor moved past each.
    async fn drain(&mut self, mut first: Option<EventPage>) -> Flow {
        loop {
            let page = match first.take() {
                Some(page) => page,
                None => match self.reader.read(self.cursor).await {
                    Ok(page) => page,
                    Err(error) => {
                        // Tried again when woken or due.
                        tracing::warn!(
                            target: LOG_TARGET,
                            code = error.code(),
                            "an event stream could not read the outbox"
                        );
                        return Flow::Wait;
                    }
                },
            };
            if self.cursor < page.purged_through {
                // Purged before they were sent: the client lost them.
                return Flow::End(LAGGED);
            }
            for event in page.events {
                let sequence = event.sequence;
                match self.hand(Frame::Event(event)).await {
                    Handed::Sent => self.cursor = sequence,
                    Handed::Slow => return Flow::End(LAGGED),
                    Handed::Gone => return Flow::Gone,
                }
            }
            if !page.more {
                // As polling's `next_after`: past what the filters left
                // out.
                self.cursor = self.cursor.max(page.high_water);
                return Flow::Wait;
            }
        }
    }

    fn end(&self, how: u8) {
        self.end.store(how, Ordering::Release);
        if how == LAGGED {
            tracing::info!(
                target: LOG_TARGET,
                tenant = self.reader.query.tenant.as_str(),
                "an event stream lagged: ended for its client to resume"
            );
        }
    }

    /// Hand `frame` over, within the budget, waiting at most
    /// [`LiveSettings::slow_client`] for room.
    async fn hand(&self, frame: Frame) -> Handed {
        let bytes = match &frame {
            Frame::Event(event) => event.data.len().clamp(1, self.settings.buffer_bytes.max(1)),
            _ => 0,
        };
        let room = async {
            let held = if bytes == 0 {
                None
            } else {
                let permits = u32::try_from(budget_permits(bytes)).unwrap_or(u32::MAX);
                let permit = self.budget.clone().acquire_many_owned(permits).await.ok()?;
                self.shared.buffered.fetch_add(bytes, Ordering::Relaxed);
                Some(Held {
                    _permit: permit,
                    bytes,
                    shared: self.shared.clone(),
                })
            };
            let slot = self.sender.reserve().await.ok()?;
            Some((held, slot))
        };
        match tokio::time::timeout(self.settings.slow_client, room).await {
            Err(_) => Handed::Slow,
            Ok(None) => Handed::Gone,
            Ok(Some((held, slot))) => {
                slot.send(Item { frame, _held: held });
                Handed::Sent
            }
        }
    }
}

/// A live stream of a tenant's events ([module](self)): a `futures`
/// stream of [`Frame`]s, ending after a [`Frame::Lagged`] or a
/// [`Frame::Shutdown`]. Holds its slot until dropped; dropping it stops
/// its reads.
pub struct LiveStream {
    receiver: mpsc::Receiver<Item>,
    end: Arc<AtomicU8>,
    /// The last event's sequence yielded, or the cursor it started after.
    last: i64,
    done: bool,
    task: JoinHandle<()>,
    _slot: StreamSlot,
}

impl std::fmt::Debug for LiveStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveStream")
            .field("last", &self.last)
            .finish_non_exhaustive()
    }
}

impl LiveStream {
    /// The cursor it started after, until it yields an event; then that
    /// event's sequence.
    pub fn last(&self) -> i64 {
        self.last
    }
}

impl futures::Stream for LiveStream {
    type Item = Frame;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Frame>> {
        if self.done {
            return Poll::Ready(None);
        }
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some(Item { frame, _held: held })) => {
                // Taken: its share of the budget goes back.
                drop(held);
                if let Frame::Event(event) = &frame {
                    self.last = event.sequence;
                }
                Poll::Ready(Some(frame))
            }
            Poll::Ready(None) => {
                self.done = true;
                let after = self.last;
                Poll::Ready(match self.end.load(Ordering::Acquire) {
                    LAGGED => Some(Frame::Lagged { after }),
                    SHUTDOWN => Some(Frame::Shutdown { after }),
                    _ => None,
                })
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for LiveStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
