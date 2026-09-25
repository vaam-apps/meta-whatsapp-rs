//! Redis adapter against a real server. Skipped unless
//! `META_WHATSAPP_RS_TEST_REDIS_URL` is set; `META_WHATSAPP_RS_REQUIRE_LIVE=1` (as in
//! `just test-live`) turns the skip into a failure.
//!
//! Every test uses its own key prefix and deletes what it wrote.
#![cfg(feature = "redis")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::time::Duration;

use meta_whatsapp_adapters::store::{RedisKvStore, conformance};
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey};
use redis::aio::{ConnectionManager, MultiplexedConnection};

fn client() -> Option<redis::Client> {
    let url = common::service_url("META_WHATSAPP_RS_TEST_REDIS_URL")?;
    Some(redis::Client::open(url).expect("valid META_WHATSAPP_RS_TEST_REDIS_URL"))
}

fn prefix() -> String {
    format!("wa-test:{}:", common::unique())
}

/// Delete every key under `prefix`.
async fn cleanup(conn: &mut MultiplexedConnection, prefix: &str) {
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{prefix}*"))
        .query_async(conn)
        .await
        .unwrap();
    if !keys.is_empty() {
        let _: () = redis::cmd("DEL").arg(keys).query_async(conn).await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_kv_conformance_connection_manager() {
    let Some(client) = client() else { return };
    let prefix = prefix();
    let manager = ConnectionManager::new(client.clone()).await.unwrap();
    let store = RedisKvStore::new(manager).with_prefix(prefix.clone());
    conformance::run_with_real_time(&store, Duration::from_millis(500)).await;
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    cleanup(&mut conn, &prefix).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_kv_conformance_multiplexed() {
    let Some(client) = client() else { return };
    let prefix = prefix();
    let conn = client.get_multiplexed_async_connection().await.unwrap();
    let store = RedisKvStore::new(conn.clone()).with_prefix(prefix.clone());
    conformance::run_with_real_time(&store, Duration::from_millis(500)).await;
    cleanup(&mut conn.clone(), &prefix).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_layout_counters_and_ttls() {
    let Some(client) = client() else { return };
    let prefix = prefix();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    let store = RedisKvStore::new(conn.clone()).with_prefix(prefix.clone());

    // The documented layout: a hash per record, a counter per namespace.
    let k = StoreKey::new("wa.otp", "user-1");
    let v1 = store
        .put(
            &k,
            b"code-hash".to_vec(),
            Expiry::After(Duration::from_secs(600)),
        )
        .await
        .unwrap();
    let record = format!("{prefix}{{6:wa.otp}}:user-1");
    let counter = format!("{prefix}{{6:wa.otp}}#version");
    let (value, version, exp): (Vec<u8>, u64, i64) = redis::cmd("HMGET")
        .arg(&record)
        .arg("v")
        .arg("ver")
        .arg("exp")
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!((value.as_slice(), version), (&b"code-hash"[..], v1));
    let pexpiretime: i64 = redis::cmd("PEXPIRETIME")
        .arg(&record)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(pexpiretime, exp, "the key's own TTL is the stored deadline");
    let counter_ttl: i64 = redis::cmd("PTTL")
        .arg(&counter)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(counter_ttl, -1, "the namespace counter never expires");

    // Delete removes the record entirely; the counter stays.
    assert!(store.delete(&k).await.unwrap());
    let exists: (bool, bool) = redis::pipe()
        .exists(&record)
        .exists(&counter)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(exists, (false, true));

    // Length-prefixed namespaces: these two pairs would spell the same key
    // with a naive `ns:key` join.
    let a = StoreKey::new("a}:b", "c");
    let b = StoreKey::new("a", "b}:c");
    store.put(&a, b"A".to_vec(), Expiry::Never).await.unwrap();
    store.put(&b, b"B".to_vec(), Expiry::Never).await.unwrap();
    assert_eq!(store.get(&a).await.unwrap().unwrap().value, b"A");
    assert_eq!(store.get(&b).await.unwrap().unwrap().value, b"B");

    // Binary values survive untouched.
    let bin = StoreKey::new("wa.bin", "all-bytes");
    let all: Vec<u8> = (0..=255).collect();
    store.put(&bin, all.clone(), Expiry::Never).await.unwrap();
    assert_eq!(store.get(&bin).await.unwrap().unwrap().value, all);

    // A deadline past year 9999 means "never", as in MemoryKvStore: no
    // `exp` field, no key TTL, and the record reads back.
    let far = StoreKey::new("wa.far", "x");
    store
        .put(
            &far,
            b"x".to_vec(),
            Expiry::After(Duration::from_millis(253_402_300_799_999)),
        )
        .await
        .unwrap();
    assert_eq!(store.get(&far).await.unwrap().unwrap().expires_at, None);
    let far_record = format!("{prefix}{{6:wa.far}}:x");
    let far_ttl: i64 = redis::cmd("PTTL")
        .arg(&far_record)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(far_ttl, -1, "no TTL on a record that never expires");

    cleanup(&mut conn, &prefix).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_debug_is_redacted() {
    let Some(client) = client() else { return };
    let conn = client.get_multiplexed_async_connection().await.unwrap();
    let rendered = format!("{:?}", RedisKvStore::new(conn));
    assert_eq!(rendered, r#"RedisKvStore { prefix: "wa:", .. }"#);
}
