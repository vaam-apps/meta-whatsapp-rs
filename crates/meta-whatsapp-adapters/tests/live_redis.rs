//! Redis adapters (`RedisKvStore`, `RedisConversationStore`) against a real
//! server. Skipped unless
//! `META_WHATSAPP_RS_TEST_REDIS_URL` is set; `META_WHATSAPP_RS_REQUIRE_LIVE=1` (as in
//! `just test-live`) turns the skip into a failure.
//!
//! Every test uses its own key prefix, and a `common::RedisCleanup` guard
//! deletes what it wrote, when the test panics too.
#![cfg(feature = "redis")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::time::Duration;

use meta_whatsapp_adapters::store::{
    RedisConversationStore, RedisKvStore, conformance, conversation_conformance,
};
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_core::store::{
    ConversationKey, ConversationStore, DeliveryStatus, Direction, ErasureMode, Expiry,
    IdentityLink, KvStore, Retention, StoreKey, StoredContact, StoredMessage,
};
use redis::aio::ConnectionManager;

use common::RedisCleanup;

fn client() -> Option<redis::Client> {
    let url = common::service_url("META_WHATSAPP_RS_TEST_REDIS_URL")?;
    Some(redis::Client::open(url).expect("valid META_WHATSAPP_RS_TEST_REDIS_URL"))
}

/// A key prefix of this test's own, and the guard that deletes its keys.
fn prefix(client: &redis::Client) -> (String, RedisCleanup) {
    let prefix = format!("wa-test:{}:", common::unique());
    let cleanup = RedisCleanup::new(client, &prefix);
    (prefix, cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_kv_conformance_connection_manager() {
    let Some(client) = client() else { return };
    let (prefix, _cleanup) = prefix(&client);
    let manager = ConnectionManager::new(client.clone()).await.unwrap();
    let store = RedisKvStore::new(manager).with_prefix(prefix.clone());
    conformance::run_with_real_time(&store, Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_kv_conformance_multiplexed() {
    let Some(client) = client() else { return };
    let (prefix, _cleanup) = prefix(&client);
    let conn = client.get_multiplexed_async_connection().await.unwrap();
    let store = RedisKvStore::new(conn.clone()).with_prefix(prefix.clone());
    conformance::run_with_real_time(&store, Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_layout_counters_and_ttls() {
    let Some(client) = client() else { return };
    let (prefix, _cleanup) = prefix(&client);
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_debug_is_redacted() {
    let Some(client) = client() else { return };
    let conn = client.get_multiplexed_async_connection().await.unwrap();
    let rendered = format!("{:?}", RedisKvStore::new(conn));
    assert_eq!(rendered, r#"RedisKvStore { prefix: "wa:", .. }"#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_conversation_conformance() {
    let Some(client) = client() else { return };
    let (prefix, _cleanup) = prefix(&client);
    let manager = ConnectionManager::new(client.clone()).await.unwrap();
    let store = RedisConversationStore::new(manager).with_prefix(prefix.clone());
    assert_eq!(store.erasure_mode(), ErasureMode::Redact);
    conversation_conformance::run(&store).await;
}

/// The suite's group erasure and retention cases take their other branch
/// on a store that deletes an erased person's group messages and keeps a
/// retention; over a multiplexed connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_conversation_conformance_deleting_with_a_retention() {
    let Some(client) = client() else { return };
    let (prefix, _cleanup) = prefix(&client);
    let conn = client.get_multiplexed_async_connection().await.unwrap();
    let store = RedisConversationStore::new(conn)
        .with_prefix(prefix.clone())
        .with_erasure_mode(ErasureMode::Delete)
        .with_retention(Retention::days(30));
    assert_eq!(store.erasure_mode(), ErasureMode::Delete);
    conversation_conformance::run(&store).await;
}

/// Roadmap L8's decisive case: ids are stored once per store, so a second
/// `append` of one id on another number returns `false`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_a_second_append_of_one_id_on_another_number_is_refused() {
    let Some(client) = client() else { return };
    let (prefix, _cleanup) = prefix(&client);
    let conn = client.get_multiplexed_async_connection().await.unwrap();
    let store = RedisConversationStore::new(conn).with_prefix(prefix.clone());
    conversation_conformance::ids_are_stored_once_per_store(&store).await;
}

/// The documented layout: one hash tag for the whole store, the per-store
/// id index, a hash per message holding the content exactly, and nothing
/// of the person left by an erasure but the set of business numbers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_conversation_layout_and_erasure_leaves_nothing() {
    let Some(client) = client() else { return };
    let (prefix, _cleanup) = prefix(&client);
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    let store = RedisConversationStore::new(conn.clone()).with_prefix(prefix.clone());
    let pn = PhoneNumberId::new("106540352242922").unwrap();
    let key = ConversationKey::new(pn.clone(), "US.1");
    let at = time::macros::datetime!(2026-09-24 12:00 UTC);
    let message = StoredMessage {
        id: MessageId::new("wamid.1"),
        conversation: key.clone(),
        direction: Direction::Inbound,
        kind: "text".to_owned(),
        text: Some("hi\0there".to_owned()),
        payload: serde_json::json!({"from": "16505551234", "from_user_id": "US.1"}),
        status: DeliveryStatus::Received,
        timestamp: at,
        status_at: None,
        error: None,
    };
    assert!(store.append(message.clone()).await.unwrap());
    let base = format!("{prefix}{{conversations:{prefix}}}");
    let index: String = redis::cmd("GET")
        .arg(format!("{base}:id:wamid.1"))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(index, "106540352242922", "the id index names the number");
    let (kind, text): (String, Vec<u8>) = redis::cmd("HMGET")
        .arg(format!("{base}:m:15:106540352242922wamid.1"))
        .arg("k")
        .arg("t")
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(
        (kind.as_str(), text.as_slice()),
        ("text", &b"hi\0there"[..])
    );
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{prefix}*"))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert!(
        keys.iter().all(|k| k.starts_with(&base)),
        "every key under the one hash tag: {keys:?}"
    );
    store
        .put_contact(StoredContact {
            key: key.clone(),
            full_name: Some("Pablo".to_owned()),
            first_name: None,
            phone_number: Some("16505551234".to_owned()),
            user_id: None,
            parent_user_id: None,
            username: None,
            synced_at: at,
        })
        .await
        .unwrap();
    store
        .link_identity(IdentityLink::new(pn.clone(), "16505551234", "US.1", at))
        .await
        .unwrap();
    let ids: Vec<String> = store.identities(&key).await.unwrap().into_iter().collect();
    assert_eq!(ids, ["16505551234", "US.1"]);
    let erased = store.erase_all(&pn, &ids).await.unwrap();
    assert_eq!(
        (
            erased.messages,
            erased.conversations,
            erased.contacts,
            erased.identity_links
        ),
        (1, 1, 1, 1)
    );
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{prefix}*"))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(
        keys,
        [format!("{base}:numbers")],
        "an erasure leaves no key of the person's"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_conversation_debug_is_redacted() {
    let Some(client) = client() else { return };
    let conn = client.get_multiplexed_async_connection().await.unwrap();
    let rendered = format!("{:?}", RedisConversationStore::new(conn));
    assert_eq!(
        rendered,
        r#"RedisConversationStore { prefix: "wa:", retention: Keep, erasure_mode: Redact, .. }"#
    );
}

/// Every key under `prefix` (its name, or a value, field or member it
/// holds) containing one of `needles`' bytes.
async fn traces(
    conn: &mut redis::aio::MultiplexedConnection,
    prefix: &str,
    needles: &[String],
) -> Vec<String> {
    let keys: Vec<Vec<u8>> = redis::cmd("KEYS")
        .arg(format!("{prefix}*"))
        .query_async(conn)
        .await
        .unwrap();
    assert!(!keys.is_empty(), "the scenario wrote under {prefix}");
    let mut found = Vec::new();
    for key in keys {
        let kind: String = redis::cmd("TYPE")
            .arg(&key)
            .query_async(conn)
            .await
            .unwrap();
        let mut blobs: Vec<Vec<u8>> = match kind.as_str() {
            "string" => vec![redis::cmd("GET").arg(&key).query_async(conn).await.unwrap()],
            "hash" => redis::cmd("HGETALL")
                .arg(&key)
                .query_async(conn)
                .await
                .unwrap(),
            "set" => redis::cmd("SMEMBERS")
                .arg(&key)
                .query_async(conn)
                .await
                .unwrap(),
            "zset" => redis::cmd("ZRANGE")
                .arg(&key)
                .arg(0)
                .arg(-1)
                .query_async(conn)
                .await
                .unwrap(),
            other => panic!("unexpected {other} at {}", String::from_utf8_lossy(&key)),
        };
        blobs.push(key.clone());
        for needle in needles {
            if blobs
                .iter()
                .any(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
            {
                found.push(format!("{} holds {needle}", String::from_utf8_lossy(&key)));
            }
        }
    }
    found
}

/// An erasure leaves nothing of the person in any key, field or member (a
/// redacted group message's sender, the indexes of their window events,
/// thread owner and contact removal included), in both erasure modes; a
/// purge leaves nothing of what it purged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_redis_an_erasure_or_a_purge_leaves_no_trace() {
    let Some(client) = client() else { return };
    for mode in [ErasureMode::Redact, ErasureMode::Delete] {
        let (prefix, _cleanup) = prefix(&client);
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let store = RedisConversationStore::new(conn.clone())
            .with_prefix(prefix.clone())
            .with_erasure_mode(mode);
        let needles = conversation_conformance::erase_one_person_of_every_record(&store).await;
        assert_eq!(
            traces(&mut conn, &prefix, &needles).await,
            Vec::<String>::new(),
            "{mode:?}"
        );
    }
    let (prefix, _cleanup) = prefix(&client);
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    let store = RedisConversationStore::new(conn.clone()).with_prefix(prefix.clone());
    let needles = conversation_conformance::purge_one_person_of_every_record(&store).await;
    assert_eq!(
        traces(&mut conn, &prefix, &needles).await,
        Vec::<String>::new()
    );
}
