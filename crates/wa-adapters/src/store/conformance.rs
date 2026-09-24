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
}

/// Run the suite against a backend that expires on real time, sleeping for
/// `tick` (keep it small, e.g. 1.2s for second-granularity TTLs).
pub async fn run_with_real_time<S: KvStore + ?Sized>(store: &S, tick: Duration) {
    basic(store).await;
    versions_never_reused(store).await;
    put_if_absent_is_exclusive(store).await;
    cas(store).await;
    expiry_real(store, tick).await;
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
