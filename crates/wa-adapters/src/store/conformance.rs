#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)] // test helper: panics are the report
//! The executable `KvStore` contract.
//!
//! ```ignore
//! wa_adapters::store::conformance::run(&my_store, &|d| clock.advance(d)).await;
//! ```
//!
//! `advance` must move the store's notion of "now" forward by `d`. For
//! backends that expire on their own wall clock (Postgres `now()`, Redis
//! TTLs) pass a closure that sleeps; the suite only uses sub-second-scale
//! expiries in that case via [`run_with_real_time`].

use std::time::Duration;

use wa_core::store::{Expiry, KvStore, StoreKey};

fn key(name: &str) -> StoreKey {
    // Unique per run so shared backends (a real Postgres) can be reused.
    StoreKey::new("wa.conformance", format!("{name}-{}", unique()))
}

fn unique() -> String {
    // Uniqueness, not secrecy: a per-process counter plus the start time
    // keeps concurrent runs against one shared backend from colliding.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    format!("{t:x}-{}-{n}", std::process::id())
}

/// Run the suite, moving time with `advance`.
///
/// # Panics
///
/// On any contract violation — this is a test helper.
pub async fn run<S: KvStore + ?Sized>(store: &S, advance: &(dyn Fn(Duration) + Sync)) {
    basic(store).await;
    versions_never_reused(store).await;
    put_if_absent_is_exclusive(store).await;
    cas(store).await;
    expiry(store, advance, Duration::from_secs(60)).await;
    clock_independent(store).await;
}

/// Run the suite against a backend that expires on real time, sleeping for
/// `tick` (keep it small, e.g. 1.2s for second-granularity TTLs).
///
/// It also checks that `Expiry::After(1h)` lands within a minute of this
/// machine's clock + 1h, so the backend's clock must be within a minute of
/// the test runner's (true for local containers and NTP-synced servers).
pub async fn run_with_real_time<S: KvStore + ?Sized>(store: &S, tick: Duration) {
    basic(store).await;
    versions_never_reused(store).await;
    put_if_absent_is_exclusive(store).await;
    cas(store).await;
    expiry_real(store, tick).await;
    clock_independent(store).await;
    ttl_is_measured_from_now(store).await;
}

/// Checks that need no control over time, so both entry points run them.
async fn clock_independent<S: KvStore + ?Sized>(store: &S) {
    empty_value_is_a_value(store).await;
    keep_preserves_expiry(store).await;
    born_expired_is_invisible(store).await;
    versions_increase_across_every_write(store).await;
    concurrent_delete_is_exclusive(store).await;
    concurrent_cas_delete_is_exclusive(store).await;
}

async fn empty_value_is_a_value<S: KvStore + ?Sized>(store: &S) {
    let k = key("empty");
    let v = store.put(&k, Vec::new(), Expiry::Never).await.unwrap();
    let got = store
        .get(&k)
        .await
        .unwrap()
        .expect("an empty value is a present record, not an absent one");
    assert!(got.value.is_empty());
    assert_eq!(got.version, v);
    assert!(
        store
            .put_if_absent(&k, b"x".to_vec(), Expiry::Never)
            .await
            .unwrap()
            .is_none(),
        "an empty value blocks put_if_absent"
    );
    assert!(store.delete(&k).await.unwrap());
}

async fn keep_preserves_expiry<S: KvStore + ?Sized>(store: &S) {
    // Keep on a new record means never, through put and put_if_absent.
    let k = key("keep-new");
    store.put(&k, b"a".to_vec(), Expiry::Keep).await.unwrap();
    assert_eq!(store.get(&k).await.unwrap().unwrap().expires_at, None);
    store.delete(&k).await.unwrap();
    store
        .put_if_absent(&k, b"a".to_vec(), Expiry::Keep)
        .await
        .unwrap()
        .expect("absent");
    assert_eq!(store.get(&k).await.unwrap().unwrap().expires_at, None);
    store.delete(&k).await.unwrap();

    // Keep on an existing record keeps its deadline, through put and CAS.
    let k = key("keep");
    let hour = Duration::from_secs(3600);
    store
        .put(&k, b"a".to_vec(), Expiry::After(hour))
        .await
        .unwrap();
    let deadline = store.get(&k).await.unwrap().unwrap().expires_at;
    assert!(deadline.is_some());
    let v = store.put(&k, b"b".to_vec(), Expiry::Keep).await.unwrap();
    let got = store.get(&k).await.unwrap().unwrap();
    assert_eq!(
        (got.value.as_slice(), got.expires_at),
        (&b"b"[..], deadline),
        "put with Expiry::Keep keeps the deadline"
    );
    store
        .compare_and_swap(&k, v, Some(b"c".to_vec()), Expiry::Keep)
        .await
        .unwrap()
        .expect("current version");
    assert_eq!(store.get(&k).await.unwrap().unwrap().expires_at, deadline);

    // An explicit expiry replaces it, and Never clears it.
    let v = store.put(&k, b"d".to_vec(), Expiry::Never).await.unwrap();
    assert_eq!(store.get(&k).await.unwrap().unwrap().expires_at, None);
    store
        .compare_and_swap(&k, v, Some(b"e".to_vec()), Expiry::After(hour))
        .await
        .unwrap()
        .expect("current version");
    assert!(store.get(&k).await.unwrap().unwrap().expires_at.is_some());
    store.delete(&k).await.unwrap();
}

async fn born_expired_is_invisible<S: KvStore + ?Sized>(store: &S) {
    let past = Expiry::At(time::macros::datetime!(2000-01-01 0:00 UTC));
    let k = key("born-expired");
    let v1 = store.put(&k, b"a".to_vec(), past).await.unwrap();
    assert!(
        store.get(&k).await.unwrap().is_none(),
        "a record whose deadline has passed is invisible from the start"
    );
    assert!(!store.delete(&k).await.unwrap(), "nothing live to delete");
    let v2 = store
        .put_if_absent(&k, b"b".to_vec(), Expiry::Never)
        .await
        .unwrap()
        .expect("put_if_absent succeeds over a born-expired record");
    assert!(v2 > v1, "the expired write still spent its version");
    store.delete(&k).await.unwrap();
}

async fn versions_increase_across_every_write<S: KvStore + ?Sized>(store: &S) {
    let k = key("monotonic");
    let mut last = store
        .put_if_absent(&k, b"0".to_vec(), Expiry::Never)
        .await
        .unwrap()
        .unwrap();
    for i in 1..=3u8 {
        let v = store.put(&k, vec![i], Expiry::Never).await.unwrap();
        assert!(v > last, "put: {v} after {last}");
        last = v;
        let v = store
            .compare_and_swap(&k, last, Some(vec![i, i]), Expiry::Never)
            .await
            .unwrap()
            .unwrap();
        assert!(v > last, "compare_and_swap: {v} after {last}");
        last = v;
        assert_eq!(
            store
                .compare_and_swap(&k, last, None, Expiry::Never)
                .await
                .unwrap(),
            Some(0)
        );
        let v = store
            .put_if_absent(&k, vec![i], Expiry::Never)
            .await
            .unwrap()
            .unwrap();
        assert!(v > last, "put_if_absent after CAS delete: {v} after {last}");
        last = v;
    }
    store.delete(&k).await.unwrap();
}

async fn concurrent_delete_is_exclusive<S: KvStore + ?Sized>(store: &S) {
    let k = key("del-race");
    store.put(&k, b"x".to_vec(), Expiry::Never).await.unwrap();
    let racers = (0..16).map(|_| store.delete(&k));
    let wins = futures::future::join_all(racers)
        .await
        .into_iter()
        .filter(|r| *r.as_ref().unwrap())
        .count();
    assert_eq!(wins, 1, "exactly one concurrent delete removes the record");
}

async fn concurrent_cas_delete_is_exclusive<S: KvStore + ?Sized>(store: &S) {
    let k = key("cas-del-race");
    let v = store.put(&k, b"x".to_vec(), Expiry::Never).await.unwrap();
    let racers = (0..16).map(|_| store.compare_and_swap(&k, v, None, Expiry::Never));
    let wins = futures::future::join_all(racers)
        .await
        .into_iter()
        .filter(|r| r.as_ref().unwrap().is_some())
        .count();
    assert_eq!(wins, 1, "exactly one concurrent CAS delete wins");
    assert!(store.get(&k).await.unwrap().is_none());
}

/// Real-time backends only: `After(ttl)` lands about `ttl` after the
/// wall clock (within a minute, allowing for server clock skew), which
/// catches seconds/milliseconds mix-ups.
async fn ttl_is_measured_from_now<S: KvStore + ?Sized>(store: &S) {
    let k = key("ttl-units");
    let ttl = Duration::from_secs(3600);
    let before = time::OffsetDateTime::now_utc();
    store
        .put(&k, b"a".to_vec(), Expiry::After(ttl))
        .await
        .unwrap();
    let expires_at = store.get(&k).await.unwrap().unwrap().expires_at.unwrap();
    let expected = before + ttl;
    let skew = (expires_at - expected).abs();
    assert!(
        skew < time::Duration::minutes(1),
        "After(1h) expires at {expires_at}, expected about {expected}"
    );
    store.delete(&k).await.unwrap();
}

async fn basic<S: KvStore + ?Sized>(store: &S) {
    let k = key("basic");
    assert!(
        store.get(&k).await.unwrap().is_none(),
        "absent key reads None"
    );
    let v1 = store.put(&k, b"one".to_vec(), Expiry::Never).await.unwrap();
    let got = store.get(&k).await.unwrap().expect("present after put");
    assert_eq!(got.value, b"one");
    assert_eq!(got.version, v1);
    assert_eq!(got.expires_at, None);
    let v2 = store.put(&k, b"two".to_vec(), Expiry::Never).await.unwrap();
    assert!(v2 > v1, "version increases on overwrite");
    assert!(
        store.delete(&k).await.unwrap(),
        "delete reports live record"
    );
    assert!(
        !store.delete(&k).await.unwrap(),
        "second delete reports nothing"
    );
    assert!(store.get(&k).await.unwrap().is_none());
}

async fn versions_never_reused<S: KvStore + ?Sized>(store: &S) {
    let k = key("versions");
    let v1 = store.put(&k, b"a".to_vec(), Expiry::Never).await.unwrap();
    store.delete(&k).await.unwrap();
    let v2 = store.put(&k, b"b".to_vec(), Expiry::Never).await.unwrap();
    assert!(v2 > v1, "version not reused after delete ({v1} then {v2})");
    // A CAS carrying the stale pre-delete version must fail.
    assert!(
        store
            .compare_and_swap(&k, v1, Some(b"c".to_vec()), Expiry::Never)
            .await
            .unwrap()
            .is_none(),
        "stale version rejected after delete+recreate"
    );
    store.delete(&k).await.unwrap();
}

async fn put_if_absent_is_exclusive<S: KvStore + ?Sized>(store: &S) {
    let k = key("pia");
    let first = store
        .put_if_absent(&k, b"x".to_vec(), Expiry::Never)
        .await
        .unwrap();
    assert!(first.is_some());
    let second = store
        .put_if_absent(&k, b"y".to_vec(), Expiry::Never)
        .await
        .unwrap();
    assert!(second.is_none(), "second put_if_absent loses");
    assert_eq!(store.get(&k).await.unwrap().unwrap().value, b"x");

    // Concurrent racers: exactly one wins.
    let k = key("pia-race");
    let racers = (0..16u8).map(|i| {
        let k = k.clone();
        async move {
            store
                .put_if_absent(&k, vec![i], Expiry::Never)
                .await
                .unwrap()
        }
    });
    let wins = futures::future::join_all(racers)
        .await
        .into_iter()
        .flatten()
        .count();
    assert_eq!(wins, 1, "exactly one concurrent put_if_absent wins");
    store.delete(&k).await.unwrap();
}

async fn cas<S: KvStore + ?Sized>(store: &S) {
    let k = key("cas");
    assert!(
        store
            .compare_and_swap(&k, 1, Some(b"x".to_vec()), Expiry::Never)
            .await
            .unwrap()
            .is_none(),
        "CAS on absent key fails"
    );
    let v1 = store.put(&k, b"a".to_vec(), Expiry::Never).await.unwrap();
    let v2 = store
        .compare_and_swap(&k, v1, Some(b"b".to_vec()), Expiry::Never)
        .await
        .unwrap()
        .expect("CAS with current version succeeds");
    assert!(v2 > v1);
    assert!(
        store
            .compare_and_swap(&k, v1, Some(b"c".to_vec()), Expiry::Never)
            .await
            .unwrap()
            .is_none(),
        "CAS with stale version fails"
    );
    assert_eq!(store.get(&k).await.unwrap().unwrap().value, b"b");

    // Concurrent CAS racers on the same version: exactly one wins.
    let racers = (0..16u8).map(|i| {
        let k = k.clone();
        async move {
            store
                .compare_and_swap(&k, v2, Some(vec![i]), Expiry::Never)
                .await
                .unwrap()
        }
    });
    let wins = futures::future::join_all(racers)
        .await
        .into_iter()
        .flatten()
        .count();
    assert_eq!(wins, 1, "exactly one concurrent CAS wins");

    // CAS delete.
    let cur = store.get(&k).await.unwrap().unwrap().version;
    assert_eq!(
        store
            .compare_and_swap(&k, cur, None, Expiry::Never)
            .await
            .unwrap(),
        Some(0)
    );
    assert!(store.get(&k).await.unwrap().is_none());
}

async fn expiry<S: KvStore + ?Sized>(
    store: &S,
    advance: &(dyn Fn(Duration) + Sync),
    ttl: Duration,
) {
    let k = key("ttl");
    let v = store
        .put(&k, b"a".to_vec(), Expiry::After(ttl))
        .await
        .unwrap();
    let got = store.get(&k).await.unwrap().unwrap();
    assert!(got.expires_at.is_some());

    // Keep preserves the expiry through CAS.
    let v2 = store
        .compare_and_swap(&k, v, Some(b"b".to_vec()), Expiry::Keep)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.get(&k).await.unwrap().unwrap().expires_at,
        got.expires_at,
        "Expiry::Keep keeps the deadline"
    );

    advance(ttl + Duration::from_secs(1));
    assert!(
        store.get(&k).await.unwrap().is_none(),
        "expired is invisible"
    );
    assert!(
        store
            .compare_and_swap(&k, v2, Some(b"c".to_vec()), Expiry::Never)
            .await
            .unwrap()
            .is_none(),
        "CAS against expired fails"
    );
    assert!(
        store
            .put_if_absent(&k, b"d".to_vec(), Expiry::Never)
            .await
            .unwrap()
            .is_some(),
        "put_if_absent succeeds over expired"
    );
    store.delete(&k).await.unwrap();
}

async fn expiry_real<S: KvStore + ?Sized>(store: &S, tick: Duration) {
    let k = key("ttl-real");
    store
        .put(&k, b"a".to_vec(), Expiry::After(tick))
        .await
        .unwrap();
    assert!(store.get(&k).await.unwrap().is_some());
    tokio::time::sleep(tick + Duration::from_millis(1100)).await;
    assert!(
        store.get(&k).await.unwrap().is_none(),
        "expired is invisible"
    );
    assert!(
        store
            .put_if_absent(&k, b"b".to_vec(), Expiry::Never)
            .await
            .unwrap()
            .is_some()
    );
    store.delete(&k).await.unwrap();
}
