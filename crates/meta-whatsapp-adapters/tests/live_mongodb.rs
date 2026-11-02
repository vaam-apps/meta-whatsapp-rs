//! The MongoDB test service (roadmap S8) against a real server: the smoke
//! test that `META_WHATSAPP_RS_TEST_MONGODB_URL` points at a replica set
//! that commits multi-document transactions, which S9's outbox spike, S17's
//! adapters and S18's server store rely on; and, with the `mongodb`
//! feature, S17's adapters (`MongoKvStore`, `MongoConversationStore`)
//! through both library conformance suites, their stored layout, and what
//! an erasure, a purge or a trim leaves (`mod adapters`). Skipped unless the
//! variable is set; `META_WHATSAPP_RS_REQUIRE_LIVE=1` (as in `just
//! test-live`) turns the skip into a failure.
//!
//! Each test works in a `wa_test_*` database of its own, and a
//! `MongoCleanup` guard drops it, when the test panics too; `just
//! test-live-clean` drops the ones a killed run left.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use mongodb::Client;
use mongodb::bson::{Document, doc};

/// Drops a test database when it goes out of scope, a panicking test's
/// unwinding included. As `common::PgCleanup` does, the drop runs on a
/// thread of its own, with a client and a current-thread runtime of its
/// own: `Drop` cannot await, and the test's runtime may be the one
/// unwinding.
struct MongoCleanup {
    url: String,
    database: String,
}

impl MongoCleanup {
    fn run(url: &str, database: &str) -> Result<(), String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            let client = Client::with_uri_str(url).await.map_err(|e| e.to_string())?;
            client
                .database(database)
                .drop()
                .await
                .map_err(|e| e.to_string())?;
            client.shutdown().await;
            Ok(())
        })
    }
}

impl Drop for MongoCleanup {
    fn drop(&mut self) {
        let (url, database) = (self.url.clone(), self.database.clone());
        let what = format!("drop database {database}");
        let result = std::thread::spawn(move || Self::run(&url, &database))
            .join()
            .unwrap_or_else(|_| Err("the cleanup thread panicked".to_owned()));
        if let Err(e) = result {
            // A second panic while unwinding would abort the test binary and
            // hide the first one: report it instead.
            if std::thread::panicking() {
                eprintln!("cleanup `{what}` failed: {e}");
            } else {
                panic!("cleanup `{what}` failed: {e}");
            }
        }
    }
}

/// A client on the test server and a `wa_test_*` database name of this
/// test's own, with the guard that drops that database; `None` when the
/// URL is unset (and the run does not require live services).
async fn database() -> Option<(Client, String, MongoCleanup)> {
    let url = common::service_url("META_WHATSAPP_RS_TEST_MONGODB_URL")?;
    let client = Client::with_uri_str(&url)
        .await
        .expect("valid META_WHATSAPP_RS_TEST_MONGODB_URL");
    let name = format!("wa_test_{}", common::unique());
    let cleanup = MongoCleanup {
        url,
        database: name.clone(),
    };
    Some((client, name, cleanup))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_mongodb_replica_set_commits_a_multi_document_transaction() {
    let Some((client, name, _cleanup)) = database().await else {
        return;
    };
    let db = client.database(&name);

    // A replica-set primary, said first: a standalone server refuses every
    // transaction ("Transaction numbers are only allowed on a replica set
    // member or mongos"), which reads less plainly than this.
    let hello = db.run_command(doc! { "hello": 1 }).await.unwrap();
    assert!(
        hello.get_str("setName").is_ok(),
        "META_WHATSAPP_RS_TEST_MONGODB_URL is not a replica set member: {hello}"
    );
    assert!(
        hello.get_bool("isWritablePrimary").unwrap_or(false),
        "the test server is not the primary: {hello}"
    );

    // Two collections, three documents, one transaction. The collections
    // do not exist yet: they are created inside it.
    let orders = db.collection::<Document>("orders");
    let lines = db.collection::<Document>("order_lines");
    let mut session = client.start_session().await.unwrap();
    session.start_transaction().await.unwrap();
    orders
        .insert_one(doc! { "_id": "order-1", "total": 3 })
        .session(&mut session)
        .await
        .unwrap();
    lines
        .insert_many([
            doc! { "order": "order-1", "sku": "a", "qty": 1 },
            doc! { "order": "order-1", "sku": "b", "qty": 2 },
        ])
        .session(&mut session)
        .await
        .unwrap();

    // Not visible outside the transaction before it commits...
    assert_eq!(orders.count_documents(doc! {}).await.unwrap(), 0);
    assert_eq!(lines.count_documents(doc! {}).await.unwrap(), 0);
    session.commit_transaction().await.unwrap();
    // ...and all of it after.
    assert_eq!(orders.count_documents(doc! {}).await.unwrap(), 1);
    assert_eq!(lines.count_documents(doc! {}).await.unwrap(), 2);

    // An aborted transaction leaves nothing behind, in either collection.
    session.start_transaction().await.unwrap();
    orders
        .insert_one(doc! { "_id": "order-2", "total": 1 })
        .session(&mut session)
        .await
        .unwrap();
    lines
        .insert_one(doc! { "order": "order-2", "sku": "c", "qty": 1 })
        .session(&mut session)
        .await
        .unwrap();
    session.abort_transaction().await.unwrap();
    assert_eq!(orders.count_documents(doc! {}).await.unwrap(), 1);
    assert_eq!(lines.count_documents(doc! {}).await.unwrap(), 2);

    drop(session);
    client.shutdown().await;
}

/// S17's adapters, live (feature `mongodb`): each test in a database of
/// its own, migrated first.
#[cfg(feature = "mongodb")]
mod adapters {
    use std::time::Duration;

    use meta_whatsapp_adapters::store::mongodb::{self, schema};
    use meta_whatsapp_adapters::store::{
        MongoConversationStore, MongoKvStore, conformance, conversation_conformance,
    };
    use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
    use meta_whatsapp_core::store::{
        ConversationKey, ConversationStore, DeliveryStatus, Direction, ErasureMode, Expiry,
        KvStore, Retention, StoreKey, StoredMessage,
    };
    use mongodb::mongodb::Database;
    use mongodb::mongodb::bson::{Bson, Document, doc};

    use super::{MongoCleanup, database};

    /// A migrated database of this test's own, and the guard that drops it.
    async fn migrated() -> Option<(Database, MongoCleanup)> {
        let (client, name, cleanup) = database().await?;
        let database = client.database(&name);
        mongodb::migrate(&database).await.unwrap();
        Some((database, cleanup))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_mongodb_kv_passes_the_conformance_suite() {
        let Some((database, _cleanup)) = migrated().await else {
            return;
        };
        let store = MongoKvStore::new(database);
        conformance::run_with_real_time(&store, Duration::from_millis(500)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_mongodb_conversation_passes_the_conformance_suite() {
        let Some((database, _cleanup)) = migrated().await else {
            return;
        };
        let store = MongoConversationStore::new(database);
        assert_eq!(store.erasure_mode(), ErasureMode::Redact);
        conversation_conformance::run(&store).await;
    }

    /// The suite's group erasure and retention cases take their other
    /// branch on a store that deletes an erased person's group messages
    /// and keeps a retention.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_mongodb_conversation_passes_the_conformance_suite_deleting_with_a_retention() {
        let Some((database, _cleanup)) = migrated().await else {
            return;
        };
        let store = MongoConversationStore::new(database)
            .with_erasure_mode(ErasureMode::Delete)
            .with_retention(Retention::days(30));
        assert_eq!(store.erasure_mode(), ErasureMode::Delete);
        conversation_conformance::run(&store).await;
    }

    /// Today's rule (`OPEN_QUESTIONS.md` #33): a second `append` of one id
    /// on another number returns `false`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_mongodb_a_second_append_of_one_id_on_another_number_is_refused() {
        let Some((database, _cleanup)) = migrated().await else {
            return;
        };
        let store = MongoConversationStore::new(database);
        conversation_conformance::ids_are_stored_once_per_store(&store).await;
    }

    /// `migrate` runs again (and twice at once) without an error, and
    /// leaves exactly the pinned indexes, by name.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_mongodb_migrate_is_idempotent_and_creates_the_pinned_indexes() {
        let Some((database, _cleanup)) = migrated().await else {
            return;
        };
        let (a, b) =
            futures::future::join(mongodb::migrate(&database), mongodb::migrate(&database)).await;
        a.unwrap();
        b.unwrap();
        let mut found: Vec<(String, String)> = Vec::new();
        for collection in [
            schema::KV,
            schema::MESSAGES,
            schema::CONVERSATIONS,
            schema::WINDOW_EVENTS,
            schema::THREAD_OWNERS,
            schema::SYNCED_CONTACTS,
            schema::IDENTITY_LINKS,
        ] {
            for name in database
                .collection::<Document>(collection)
                .list_index_names()
                .await
                .unwrap()
            {
                if name != "_id_" {
                    found.push((collection.to_owned(), name));
                }
            }
        }
        found.sort();
        let mut expected: Vec<(String, String)> = [
            (schema::KV, schema::INDEX_KV_PURGE),
            (schema::MESSAGES, schema::INDEX_MESSAGES_CONVERSATION),
            (schema::MESSAGES, schema::INDEX_MESSAGES_TS),
            (schema::MESSAGES, schema::INDEX_MESSAGES_SENDER),
            (schema::CONVERSATIONS, schema::INDEX_CONVERSATIONS_KEY),
            (schema::CONVERSATIONS, schema::INDEX_CONVERSATIONS_RECENT),
            (schema::CONVERSATIONS, schema::INDEX_CONVERSATIONS_LAST),
            (schema::WINDOW_EVENTS, schema::INDEX_WINDOW_EVENTS_KEY),
            (
                schema::WINDOW_EVENTS,
                schema::INDEX_WINDOW_EVENTS_CONVERSATION,
            ),
            (schema::WINDOW_EVENTS, schema::INDEX_WINDOW_EVENTS_TS),
            (schema::THREAD_OWNERS, schema::INDEX_THREAD_OWNERS_KEY),
            (schema::SYNCED_CONTACTS, schema::INDEX_SYNCED_CONTACTS_KEY),
            (schema::SYNCED_CONTACTS, schema::INDEX_CONTACT_REMOVALS),
            (schema::IDENTITY_LINKS, schema::INDEX_IDENTITY_LINKS_KEY),
            (schema::IDENTITY_LINKS, schema::INDEX_IDENTITY_LINKS_CURRENT),
        ]
        .into_iter()
        .map(|(c, n)| (c.to_owned(), n.to_owned()))
        .collect();
        expected.sort();
        assert_eq!(found, expected);
    }

    /// The documented layout of a key/value record: `_id` the namespace
    /// then the key, the value as bytes, the version, the expiry and the
    /// TTL index's deadline; a delete keeps the document, without its value,
    /// with its version, which the next write goes on from; the TTL index
    /// removes dead documents.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_mongodb_kv_layout_tombstones_and_the_ttl_index() {
        let Some((database, _cleanup)) = migrated().await else {
            return;
        };
        let store = MongoKvStore::new(database.clone());
        let records = database.collection::<Document>(schema::KV);
        let key = StoreKey::new("wa.otp", "user-1");
        let v1 = store
            .put(
                &key,
                b"code\0hash".to_vec(),
                Expiry::After(Duration::from_secs(600)),
            )
            .await
            .unwrap();
        let id = doc! { "namespace": "wa.otp", "key": "user-1" };
        let raw = records
            .find_one(doc! { "_id": id.clone() })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(raw.get_binary_generic("value").unwrap(), b"code\0hash");
        assert_eq!(u64::try_from(raw.get_i64("version").unwrap()).unwrap(), v1);
        let expires = *raw.get_datetime("expires_at").unwrap();
        let purge = *raw.get_datetime("purge_after").unwrap();
        assert_eq!(
            purge.timestamp_millis() - expires.timestamp_millis(),
            600_000,
            "the TTL index removes it ten minutes after it expires"
        );
        assert_eq!(
            store
                .get(&key)
                .await
                .unwrap()
                .unwrap()
                .expires_at
                .map(|t| t.unix_timestamp_nanos() / 1_000_000),
            Some(i128::from(expires.timestamp_millis()))
        );

        assert!(store.delete(&key).await.unwrap());
        let tombstone = records
            .find_one(doc! { "_id": id.clone() })
            .await
            .unwrap()
            .unwrap();
        assert!(!tombstone.contains_key("value") && !tombstone.contains_key("expires_at"));
        assert_eq!(
            u64::try_from(tombstone.get_i64("version").unwrap()).unwrap(),
            v1
        );
        let v2 = store
            .put(&key, b"again".to_vec(), Expiry::Never)
            .await
            .unwrap();
        assert!(v2 > v1);
        let raw = records
            .find_one(doc! { "_id": id.clone() })
            .await
            .unwrap()
            .unwrap();
        assert!(
            !raw.contains_key("purge_after"),
            "a record that never expires is never removed"
        );

        // The TTL index: `expireAfterSeconds: 0` on `purge_after`.
        let mut indexes = records.list_indexes().await.unwrap();
        let mut ttl = None;
        while indexes.advance().await.unwrap() {
            let index = indexes.deserialize_current().unwrap();
            if index.options.as_ref().and_then(|o| o.name.as_deref())
                == Some(schema::INDEX_KV_PURGE)
            {
                ttl = Some((
                    index.keys.clone(),
                    index.options.and_then(|o| o.expire_after),
                ));
            }
        }
        assert_eq!(ttl, Some((doc! { "purge_after": 1 }, Some(Duration::ZERO))));

        // A version written by the clock's term after the document is gone
        // (as the TTL index removes it, ten minutes after its last write at
        // the soonest: a few milliseconds here) is past every earlier one.
        tokio::time::sleep(Duration::from_millis(5)).await;
        records.delete_one(doc! { "_id": id }).await.unwrap();
        let v3 = store
            .put(&key, b"anew".to_vec(), Expiry::Never)
            .await
            .unwrap();
        assert!(v3 > v2, "{v3} after {v2}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_mongodb_debug_is_redacted() {
        let Some((database, _cleanup)) = migrated().await else {
            return;
        };
        let name = database.name().to_owned();
        assert_eq!(
            format!("{:?}", MongoKvStore::new(database.clone())),
            format!("MongoKvStore {{ database: {name:?}, .. }}")
        );
        assert_eq!(
            format!("{:?}", MongoConversationStore::new(database)),
            format!(
                "MongoConversationStore {{ database: {name:?}, retention: Keep, \
                 erasure_mode: Redact, busy_after: 10s, .. }}"
            )
        );
    }

    /// Every document of every collection (its BSON bytes: names, values,
    /// the payloads' JSON text) holding one of `needles`.
    async fn traces(database: &Database, needles: &[String]) -> Vec<String> {
        let mut found = Vec::new();
        let mut scanned = 0;
        for collection in database.list_collection_names().await.unwrap() {
            let mut cursor = database
                .collection::<Document>(&collection)
                .find(doc! {})
                .await
                .unwrap();
            while cursor.advance().await.unwrap() {
                let document = cursor.deserialize_current().unwrap();
                scanned += 1;
                let bytes = document.to_vec().unwrap();
                for needle in needles {
                    if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                        found.push(format!("{collection} holds {needle}: {document}"));
                    }
                }
            }
        }
        assert!(scanned > 0, "the scenario wrote documents");
        found
    }

    /// An erasure leaves nothing of the person in any document (a redacted
    /// group message's sender included), in both erasure modes; a purge or
    /// a trim leaves nothing of what it deleted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_mongodb_an_erasure_or_a_purge_leaves_no_trace() {
        for mode in [ErasureMode::Redact, ErasureMode::Delete] {
            let Some((database, _cleanup)) = migrated().await else {
                return;
            };
            let store = MongoConversationStore::new(database.clone()).with_erasure_mode(mode);
            let needles = conversation_conformance::erase_one_person_of_every_record(&store).await;
            assert_eq!(
                traces(&database, &needles).await,
                Vec::<String>::new(),
                "{mode:?}"
            );
        }
        let Some((database, _cleanup)) = migrated().await else {
            return;
        };
        let store = MongoConversationStore::new(database.clone());
        let needles = conversation_conformance::purge_one_person_of_every_record(&store).await;
        assert_eq!(traces(&database, &needles).await, Vec::<String>::new());
        let needles = conversation_conformance::trim_one_number_of_every_row(&store).await;
        assert_eq!(
            traces(&database, &needles).await,
            Vec::<String>::new(),
            "trim"
        );
    }

    /// The documented message layout: `_id` the message id, content as
    /// BSON strings (U+0000 kept), the payload as JSON text, the sender
    /// beside it; the summary beside it, its `appends` counted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_mongodb_conversation_layout() {
        let Some((database, _cleanup)) = migrated().await else {
            return;
        };
        let store = MongoConversationStore::new(database.clone());
        let pn = PhoneNumberId::new("106540352242922").unwrap();
        let key = ConversationKey::new(pn.clone(), "US.1");
        let at = time::macros::datetime!(2026-09-24 12:00 UTC);
        let message = StoredMessage {
            id: MessageId::new("wamid.1"),
            conversation: key.clone(),
            direction: Direction::Inbound,
            kind: "text".to_owned(),
            text: Some("hi\0there".to_owned()),
            payload: serde_json::json!({"from": "16505551234", "from_user_id": "US.1", "k\u{0}": 1}),
            status: DeliveryStatus::Received,
            timestamp: at,
            status_at: None,
            error: None,
        };
        assert!(store.append(message.clone()).await.unwrap());
        let raw = database
            .collection::<Document>(schema::MESSAGES)
            .find_one(doc! { "_id": "wamid.1" })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(raw.get_str("text").unwrap(), "hi\0there");
        assert_eq!(raw.get_str("sender").unwrap(), "US.1");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(raw.get_str("payload_json").unwrap())
                .unwrap(),
            message.payload
        );
        assert_eq!(
            store.messages(&key, None, 10).await.unwrap(),
            std::slice::from_ref(&message)
        );
        let older = StoredMessage {
            id: MessageId::new("wamid.0"),
            timestamp: at - time::Duration::hours(1),
            ..message
        };
        assert_eq!(store.append_synced(vec![older]).await.unwrap(), [true]);
        let summary = database
            .collection::<Document>(schema::CONVERSATIONS)
            .find_one(doc! { "phone_number_id": pn.as_str(), "contact": "US.1" })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            summary.get("appends"),
            Some(&Bson::Int64(2)),
            "an older synced message writes it too"
        );
        assert_eq!(summary.get_str("last_message_id").unwrap(), "wamid.1");
        assert_eq!(summary.get_str("last_text").unwrap(), "hi\0there");
        assert_eq!(summary.get_i64("unread").unwrap(), 1);
    }

    /// Decisive for the `appends` write: an erasure racing appends of
    /// older synced messages (which change nothing else of the summary)
    /// never leaves a message without its summary, nor a summary without
    /// its messages: each append is erased with the rest or recorded after
    /// the erasure, with a summary of its own.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_mongodb_an_erasure_racing_appends_never_splits_a_conversation() {
        let Some((database, _cleanup)) = migrated().await else {
            return;
        };
        let store = MongoConversationStore::new(database.clone());
        let pn = PhoneNumberId::new("106540352242922").unwrap();
        let t0 = time::macros::datetime!(2026-09-24 12:00 UTC);
        let message = |key: &ConversationKey, id: String, secs: i64| StoredMessage {
            id: MessageId::new(id),
            conversation: key.clone(),
            direction: Direction::Inbound,
            kind: "text".to_owned(),
            text: Some("x".to_owned()),
            payload: serde_json::json!({}),
            status: DeliveryStatus::Received,
            timestamp: t0 + time::Duration::seconds(secs),
            status_at: None,
            error: None,
        };
        for round in 0..40 {
            let contact = format!("US.race-{round}");
            let key = ConversationKey::new(pn.clone(), contact.clone());
            assert!(
                store
                    .append(message(&key, format!("wamid.{round}.new"), 100))
                    .await
                    .unwrap()
            );
            let older: Vec<StoredMessage> = (0..4)
                .map(|i| message(&key, format!("wamid.{round}.old{i}"), i))
                .collect();
            let contacts = [contact];
            let (synced, erased) =
                futures::future::join(store.append_synced(older), store.erase_all(&pn, &contacts))
                    .await;
            synced.unwrap();
            erased.unwrap();
            let left = store.messages(&key, None, 100).await.unwrap();
            let summary = store
                .conversations(&pn, None, 1000)
                .await
                .unwrap()
                .into_iter()
                .find(|s| s.key == key);
            assert_eq!(
                left.is_empty(),
                summary.is_none(),
                "round {round}: {} message(s) left, summary {summary:?}",
                left.len()
            );
        }
    }
}
