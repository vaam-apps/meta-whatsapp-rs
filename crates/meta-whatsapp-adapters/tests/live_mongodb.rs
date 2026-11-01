//! The MongoDB test service (roadmap S8) against a real server: the smoke
//! test that `META_WHATSAPP_RS_TEST_MONGODB_URL` points at a replica set
//! that commits multi-document transactions, which S9's outbox spike, S17's
//! adapters and S18's server store rely on. Skipped unless the variable is
//! set; `META_WHATSAPP_RS_REQUIRE_LIVE=1` (as in `just test-live`) turns the
//! skip into a failure.
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
