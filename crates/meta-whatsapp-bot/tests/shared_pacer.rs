//! Roadmap B2b: a pacer shared by replicas. Each replica is its own
//! `KvRateLimiter` over one `MemoryKvStore` (as replicas share one Redis or
//! Postgres), each number's schedule a record booked by
//! `compare_and_swap`. Timed on a `ManualClock`; the replicas' interleaving
//! is driven explicitly: the `Lockstep` store hands a read back only after
//! letting the other replica run, so under `join!` both read the record
//! before either writes it, every time.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use common::{NUMBER, Recording, event};
use meta_whatsapp_adapters::store::MemoryKvStore;
use meta_whatsapp_bot::{
    Broadcast, BroadcastEnd, KnownNumbers, KvRateLimiter, Outbound, PACER_NAMESPACE, PacedOutbound,
    Pacer, Rate, RateLimiter, SendOutcome, SlotRequest, ThroughputUpgrades,
};
use meta_whatsapp_client::messages::{OutboundMessage, Text};
use meta_whatsapp_core::clock::{Clock, ManualClock};
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_core::{Error, ErrorKind};
use pretty_assertions::assert_eq;
use time::OffsetDateTime;
use time::macros::datetime;

const T0: OffsetDateTime = datetime!(2026-09-26 12:00 UTC);

fn number() -> PhoneNumberId {
    PhoneNumberId::new(NUMBER).unwrap()
}

fn per_second(n: u32) -> Rate {
    Rate::per_second(n).unwrap()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn since_t0(clock: &ManualClock) -> Duration {
    Duration::try_from(clock.now() - T0).unwrap()
}

/// Pending once (waking itself), then ready: lets the other future of a
/// `join!` run.
struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// A `MemoryKvStore` whose `get` reads, then yields once before answering:
/// two replicas joined both read the same version before either writes.
/// Counts the writes that lost (a `compare_and_swap` or `put_if_absent`
/// answering `None`).
#[derive(Debug, Clone)]
struct Lockstep {
    inner: MemoryKvStore,
    lost: Arc<AtomicUsize>,
}

impl Lockstep {
    fn new(clock: &ManualClock) -> Self {
        Self {
            inner: MemoryKvStore::with_clock(Arc::new(clock.clone())),
            lost: Arc::default(),
        }
    }

    fn lost(&self) -> usize {
        self.lost.load(Ordering::SeqCst)
    }

    fn lose(&self, written: Option<u64>) -> Option<u64> {
        if written.is_none() {
            self.lost.fetch_add(1, Ordering::SeqCst);
        }
        written
    }
}

#[async_trait]
impl KvStore for Lockstep {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        let read = self.inner.get(key).await;
        YieldOnce(false).await;
        read
    }

    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.inner.put(key, value, expiry).await
    }

    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        let written = self.inner.put_if_absent(key, value, expiry).await?;
        Ok(self.lose(written))
    }

    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        let written = self
            .inner
            .compare_and_swap(key, expected, new, expiry)
            .await?;
        Ok(self.lose(written))
    }

    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.inner.delete(key).await
    }
}

/// One replica's limiter at `rate` over `kv`, on `clock`.
fn replica(kv: &Arc<dyn KvStore>, rate: Rate, clock: &ManualClock) -> KvRateLimiter {
    KvRateLimiter::new(kv.clone(), rate).with_timer(clock.clone())
}

fn most_in_one_second(starts: &[Duration]) -> usize {
    starts
        .iter()
        .map(|t| {
            starts
                .iter()
                .filter(|s| **s >= *t && **s < *t + Duration::from_secs(1))
                .count()
        })
        .max()
        .unwrap_or(0)
}

/// The decisive test: two replicas' pacers on one store at 20 a second,
/// 200 sends, each pair of them racing for the record (both read it before
/// either writes): every send gets a slot of its own, 50 ms apart, and no
/// one-second window holds more than 20. With the compare-and-swap
/// replaced by a plain put, the second writer of each pair takes the same
/// slot as the first: 40 in a second.
#[tokio::test]
async fn two_pacers_on_one_store_never_exceed_20_a_second() {
    let clock = ManualClock::new(T0);
    let store = Lockstep::new(&clock);
    let kv: Arc<dyn KvStore> = Arc::new(store.clone());
    let a = Pacer::new(replica(&kv, per_second(20), &clock)).with_timer(clock.clone());
    let b = Pacer::new(replica(&kv, per_second(20), &clock)).with_timer(clock.clone());
    let send = |pacer: &Pacer| {
        let pacer = pacer.clone();
        let clock = clock.clone();
        async move {
            pacer.acquire(&number()).await.unwrap();
            since_t0(&clock)
        }
    };

    let mut starts = Vec::new();
    for _ in 0..100 {
        let (first, second) = futures::join!(send(&a), send(&b));
        starts.extend([first, second]);
    }

    assert_eq!(starts.len(), 200);
    assert_eq!(most_in_one_second(&starts), 20);
    let expected: Vec<Duration> = (0..200).map(|i| ms(50 * i)).collect();
    assert_eq!(starts, expected);
    // Every pair raced: one write lost each time, and was booked again.
    assert_eq!(store.lost(), 100);
}

/// Two bookings racing for the same slot: the one whose swap lost reads
/// again and books the next slot; both succeed.
#[tokio::test]
async fn two_bookings_racing_for_one_slot_both_succeed_in_different_slots() {
    let clock = ManualClock::new(T0);
    let store = Lockstep::new(&clock);
    let kv: Arc<dyn KvStore> = Arc::new(store.clone());
    let a = replica(&kv, per_second(20), &clock);
    let b = replica(&kv, per_second(20), &clock);
    let request = SlotRequest::new(number(), T0);
    // The record exists: the race is over its version.
    assert_eq!(a.reserve(&request).await.unwrap().wait, Duration::ZERO);
    assert_eq!(store.lost(), 0);

    let (first, second) = futures::join!(a.reserve(&request), b.reserve(&request));
    let (first, second) = (first.unwrap(), second.unwrap());
    assert_eq!((first.wait, second.wait), (ms(50), ms(100)));
    assert_ne!(first.id, second.id);
    assert_eq!(store.lost(), 1);
}

/// A store that fails every call.
#[derive(Debug)]
struct Down;

fn down() -> StorageError {
    StorageError::Backend(anyhow::anyhow!("store unreachable"))
}

#[async_trait]
impl KvStore for Down {
    async fn get(&self, _: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        Err(down())
    }
    async fn put(&self, _: &StoreKey, _: Vec<u8>, _: Expiry) -> Result<u64, StorageError> {
        Err(down())
    }
    async fn put_if_absent(
        &self,
        _: &StoreKey,
        _: Vec<u8>,
        _: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        Err(down())
    }
    async fn compare_and_swap(
        &self,
        _: &StoreKey,
        _: u64,
        _: Option<Vec<u8>>,
        _: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        Err(down())
    }
    async fn delete(&self, _: &StoreKey) -> Result<bool, StorageError> {
        Err(down())
    }
}

fn is_backend(error: &Error) -> bool {
    matches!(error, Error::Storage(StorageError::Backend(_)))
}

/// Fail closed: with the store down nothing is booked and nothing is sent,
/// by a paced outbound or a broadcast; the error is the store's.
#[tokio::test]
async fn a_store_that_fails_lets_nothing_through() {
    let clock = ManualClock::new(T0);
    let kv: Arc<dyn KvStore> = Arc::new(Down);
    let limiter = Arc::new(replica(&kv, per_second(20), &clock));
    let pacer = Pacer::shared(limiter.clone()).with_timer(clock.clone());

    let error = pacer.acquire(&number()).await.unwrap_err();
    assert!(is_backend(&error), "{error:?}");
    assert!(is_backend(&pacer.slow_down(&number()).await.unwrap_err()));
    assert!(is_backend(
        &limiter
            .raise_rate(&number(), Rate::HIGHER_THROUGHPUT)
            .await
            .unwrap_err()
    ));
    assert!(is_backend(&limiter.rate_of(&number()).await.unwrap_err()));

    let recording = Recording::default();
    let outbound = PacedOutbound::new(recording.clone(), pacer.clone());
    let message = OutboundMessage::text(Recipient::phone("+16505550000"), "hi");
    assert!(is_backend(
        &outbound.send(&number(), &message).await.unwrap_err()
    ));
    assert!(recording.sent().is_empty());

    let report = Broadcast::builder(number())
        .to((0..3).map(|i| Recipient::phone(format!("+1650555{i:04}"))))
        .content(Text::new("Spring sale"))
        .outbound(recording.clone())
        .pacer(pacer)
        .concurrency(1)
        .build()
        .unwrap()
        .run()
        .await;
    assert!(recording.sent().is_empty());
    assert!(
        matches!(
            report.ended,
            BroadcastEnd::LimiterFailed { recipient: 0, .. }
        ),
        "{:?}",
        report.ended
    );
    let SendOutcome::Failed(error) = &report.recipients[0].outcome else {
        panic!("{:?}", report.recipients[0].outcome)
    };
    assert!(is_backend(error));
    assert_eq!(since_t0(&clock), Duration::ZERO);
}

/// A `MemoryKvStore` whose every `compare_and_swap` loses, as if another
/// replica always wrote first; past 100 of them it fails instead (so an
/// unbounded loop ends, with the wrong error).
#[derive(Debug, Clone)]
struct AlwaysLosing {
    inner: MemoryKvStore,
    swaps: Arc<AtomicUsize>,
}

#[async_trait]
impl KvStore for AlwaysLosing {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        self.inner.get(key).await
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.inner.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.inner.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        _: &StoreKey,
        _: u64,
        _: Option<Vec<u8>>,
        _: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        if self.swaps.fetch_add(1, Ordering::SeqCst) >= 100 {
            return Err(StorageError::Backend(anyhow::anyhow!("unbounded")));
        }
        Ok(None)
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.inner.delete(key).await
    }
}

/// Contention past the bound: `StorageError::Busy` (retryable: nothing was
/// booked) after exactly the attempts set, each after a backoff (1 ms,
/// then 2 and 4, each less a random part of up to half): never a busy
/// spin.
#[tokio::test]
async fn contention_past_the_bound_is_busy_after_backing_off() {
    let clock = ManualClock::new(T0);
    let store = AlwaysLosing {
        inner: MemoryKvStore::with_clock(Arc::new(clock.clone())),
        swaps: Arc::default(),
    };
    let kv: Arc<dyn KvStore> = Arc::new(store.clone());
    let limiter = replica(&kv, per_second(20), &clock)
        .contention(4, ms(1))
        .unwrap();
    // The first booking creates the record (`put_if_absent`).
    let request = SlotRequest::new(number(), T0);
    assert_eq!(
        limiter.reserve(&request).await.unwrap().wait,
        Duration::ZERO
    );

    let error = limiter.reserve(&request).await.unwrap_err();
    assert!(
        matches!(error, Error::Storage(StorageError::Busy)),
        "{error:?}"
    );
    assert!(error.is_retryable());
    assert_eq!(store.swaps.load(Ordering::SeqCst), 4);
    let waited = since_t0(&clock);
    assert!(
        (Duration::from_micros(3_500)..=ms(7)).contains(&waited),
        "{waited:?}"
    );
    // The paced call fails before anything is sent.
    let pacer = Pacer::new(limiter).with_timer(clock.clone());
    let error = pacer.acquire(&number()).await.unwrap_err();
    assert!(matches!(error, Error::Storage(StorageError::Busy)));
    assert_eq!(error.kind(), ErrorKind::Unknown);
}

/// A record expires its idle period after its last slot: the schedule and
/// the raised rate are forgotten, and the next booking starts afresh.
#[tokio::test]
async fn an_idle_record_expires() {
    let clock = ManualClock::new(T0);
    let store = MemoryKvStore::with_clock(Arc::new(clock.clone()));
    let kv: Arc<dyn KvStore> = Arc::new(store.clone());
    let limiter = replica(&kv, per_second(20), &clock)
        .idle_expiry(Duration::from_secs(600))
        .unwrap();
    let key = KvRateLimiter::store_key(&number());
    assert_eq!(key.namespace(), PACER_NAMESPACE);

    let request = SlotRequest::new(number(), T0);
    limiter.reserve(&request).await.unwrap();
    limiter.reserve(&request).await.unwrap();
    // The last slot ends at 100 ms: expires 10 minutes after it.
    let record = store.get(&key).await.unwrap().unwrap();
    assert_eq!(
        record.expires_at,
        Some(T0 + ms(100) + Duration::from_secs(600))
    );
    assert!(
        limiter
            .raise_rate(&number(), per_second(1000))
            .await
            .unwrap()
    );
    assert_eq!(limiter.rate_of(&number()).await.unwrap(), per_second(1000));

    clock.advance(Duration::from_secs(600) + ms(99));
    assert!(store.get(&key).await.unwrap().is_some());
    clock.advance(ms(1));
    assert!(store.get(&key).await.unwrap().is_none());
    assert_eq!(limiter.rate_of(&number()).await.unwrap(), per_second(20));
    let now = SlotRequest::new(number(), clock.now());
    assert_eq!(limiter.reserve(&now).await.unwrap().wait, Duration::ZERO);
    assert_eq!(limiter.reserve(&now).await.unwrap().wait, ms(50));
}

/// The default idle period is 24 hours after the last slot.
#[tokio::test]
async fn by_default_a_record_expires_a_day_after_its_last_slot() {
    let clock = ManualClock::new(T0);
    let store = MemoryKvStore::with_clock(Arc::new(clock.clone()));
    let kv: Arc<dyn KvStore> = Arc::new(store.clone());
    replica(&kv, per_second(20), &clock)
        .reserve(&SlotRequest::new(number(), T0))
        .await
        .unwrap();
    let record = store
        .get(&KvRateLimiter::store_key(&number()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        record.expires_at,
        Some(T0 + ms(50) + Duration::from_hours(24))
    );
}

/// A rate raised by one replica is every replica's, and is never lowered:
/// not by a lower raise afterwards, nor by one racing it.
#[tokio::test]
async fn a_raised_rate_is_shared_and_never_lowered() {
    let clock = ManualClock::new(T0);
    let store = Lockstep::new(&clock);
    let kv: Arc<dyn KvStore> = Arc::new(store.clone());
    let a = replica(&kv, per_second(20), &clock);
    let b = Arc::new(replica(&kv, per_second(20), &clock));

    assert!(a.raise_rate(&number(), per_second(1000)).await.unwrap());
    assert_eq!(b.rate_of(&number()).await.unwrap(), per_second(1000));
    let pacer = Pacer::shared(b.clone()).with_timer(clock.clone());
    let mut starts = Vec::new();
    for _ in 0..3 {
        pacer.acquire(&number()).await.unwrap();
        starts.push(since_t0(&clock));
    }
    assert_eq!(starts, [ms(0), ms(1), ms(2)]);

    // Lower, or the same: nothing changes.
    assert!(!a.raise_rate(&number(), per_second(800)).await.unwrap());
    assert!(!b.raise_rate(&number(), per_second(1000)).await.unwrap());
    assert_eq!(b.rate_of(&number()).await.unwrap(), per_second(1000));

    // Racing, whichever writes first: the higher rate stays.
    for (first, second) in [(1000, 800), (800, 1000)] {
        let other = PhoneNumberId::new(format!("{first}{second}")).unwrap();
        let (x, y) = futures::join!(
            a.raise_rate(&other, per_second(first)),
            b.raise_rate(&other, per_second(second)),
        );
        let raised = [x.unwrap(), y.unwrap()];
        assert_eq!(
            a.rate_of(&other).await.unwrap(),
            per_second(1000),
            "{raised:?}"
        );
        assert_eq!(raised, [true, first < second]);
    }
    assert_eq!(store.lost(), 2);

    // A replica configured above the raised rate keeps its own.
    let c = replica(&kv, per_second(20), &clock).rate_for(number(), per_second(2000));
    assert_eq!(c.rate_of(&number()).await.unwrap(), per_second(2000));
    assert!(!c.raise_rate(&number(), per_second(1500)).await.unwrap());
}

/// Meta's `THROUGHPUT_UPGRADE` fixture, heard by one replica, moves the
/// other replica's sends from 12.5 ms to 1 ms apart.
#[tokio::test]
async fn a_throughput_upgrade_heard_by_one_replica_raises_every_replicas_rate() {
    let clock = ManualClock::new(T0);
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let heard = Arc::new(replica(&kv, Rate::DEFAULT, &clock));
    let other = Pacer::new(replica(&kv, Rate::DEFAULT, &clock)).with_timer(clock.clone());
    let numbers = KnownNumbers::new()
        .number(
            WabaId::new("102290129340398").unwrap(),
            "15550783881",
            number(),
        )
        .unwrap();
    let upgrades = ThroughputUpgrades::new(heard, numbers);
    let starts = |n| {
        let other = other.clone();
        let clock = clock.clone();
        async move {
            let mut out = Vec::new();
            for _ in 0..n {
                other.acquire(&number()).await.unwrap();
                out.push(since_t0(&clock).as_micros());
            }
            out
        }
    };

    assert_eq!(starts(2).await, [0, 12_500]);
    let upgrade = event("fields/phone_number_quality_update.json");
    assert_eq!(upgrades.observe(&upgrade).await.unwrap(), Some(number()));
    assert_eq!(starts(3).await, [25_000, 26_000, 27_000]);
    // Heard again: nothing changes.
    assert_eq!(upgrades.observe(&upgrade).await.unwrap(), None);
}

/// A slow-down Meta answers one replica slows every replica down.
#[tokio::test]
async fn a_slow_down_on_one_replica_slows_every_replica() {
    let clock = ManualClock::new(T0);
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let a = replica(&kv, per_second(20), &clock);
    let b = replica(&kv, per_second(20), &clock);
    a.slow_down(&number(), T0).await.unwrap();
    // Within the spacing: one slow-down, whichever replica reports it.
    b.slow_down(&number(), T0 + ms(10)).await.unwrap();
    let request = SlotRequest::new(number(), T0);
    assert_eq!(b.reserve(&request).await.unwrap().wait, Duration::ZERO);
    assert_eq!(b.reserve(&request).await.unwrap().wait, ms(100));
    // 30 quiet seconds later: back to 20 a second, for both.
    let later = SlotRequest::new(number(), T0 + Duration::from_secs(31));
    assert_eq!(a.reserve(&later).await.unwrap().wait, Duration::ZERO);
    assert_eq!(b.reserve(&later).await.unwrap().wait, ms(50));
    // Turned off: a slow-down changes nothing and touches no store.
    let off = KvRateLimiter::new(Arc::new(Down), per_second(20)).adaptive(false);
    off.slow_down(&number(), T0).await.unwrap();
}

/// A slot given back is reused only while it is the last booked, by any
/// replica.
#[tokio::test]
async fn a_slot_given_back_is_reused_only_when_none_was_booked_after_it() {
    let clock = ManualClock::new(T0);
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let a = replica(&kv, per_second(20), &clock);
    let b = replica(&kv, per_second(20), &clock);
    let request = SlotRequest::new(number(), T0);
    a.reserve(&request).await.unwrap(); // 0 ms
    let second = b.reserve(&request).await.unwrap(); // 50 ms
    a.release(&request, &second).await.unwrap();
    // Given back twice: once only.
    b.release(&request, &second).await.unwrap();
    let third = a.reserve(&request).await.unwrap();
    assert_eq!(third.wait, ms(50));
    let fourth = b.reserve(&request).await.unwrap(); // 100 ms
    // Booked after: the third stays booked.
    b.release(&request, &third).await.unwrap();
    assert_eq!(a.reserve(&request).await.unwrap().wait, ms(150));
    let _ = fourth;
}

/// Each replica books by its own clock against the shared schedule: one
/// whose clock is a second behind waits for the shared slot (a second and
/// 50 ms by its clock), never taking it early.
#[tokio::test]
async fn a_replica_whose_clock_is_behind_waits_for_the_shared_slot() {
    let clock = ManualClock::new(T0);
    let behind = ManualClock::new(T0 - Duration::from_secs(1));
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let a = replica(&kv, per_second(20), &clock);
    let b = replica(&kv, per_second(20), &behind);
    assert_eq!(
        a.reserve(&SlotRequest::new(number(), T0))
            .await
            .unwrap()
            .wait,
        Duration::ZERO
    );
    let slot = b
        .reserve(&SlotRequest::new(number(), behind.now()))
        .await
        .unwrap();
    assert_eq!(slot.wait, Duration::from_secs(1) + ms(50));
}

/// A record this release cannot read (another version, or not a pacer
/// record) fails the booking and is left as it is.
#[tokio::test]
async fn an_unreadable_record_fails_closed_and_is_kept() {
    let clock = ManualClock::new(T0);
    let store = MemoryKvStore::with_clock(Arc::new(clock.clone()));
    let kv: Arc<dyn KvStore> = Arc::new(store.clone());
    let key = KvRateLimiter::store_key(&number());
    let mut newer = vec![2];
    newer.extend([0; 48]);
    store.put(&key, newer.clone(), Expiry::Never).await.unwrap();
    let limiter = replica(&kv, per_second(20), &clock);
    let error = limiter
        .reserve(&SlotRequest::new(number(), T0))
        .await
        .unwrap_err();
    assert!(is_backend(&error), "{error:?}");
    assert!(error.to_string().contains("version 2"), "{error}");
    assert_eq!(store.get(&key).await.unwrap().unwrap().value, newer);
}

/// The store key is a digest, one per number: no phone number id is
/// written to the store (the exact digest is pinned in the module's unit
/// tests).
#[test]
fn the_store_key_is_a_digest_of_the_number() {
    let key = KvRateLimiter::store_key(&number());
    assert_eq!(key.key().len(), 64);
    assert!(key.key().chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!key.key().contains(NUMBER));
    let other = KvRateLimiter::store_key(&PhoneNumberId::new("2").unwrap());
    assert_ne!(key, other);
}
