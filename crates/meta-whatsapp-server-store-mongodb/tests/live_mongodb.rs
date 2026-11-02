//! The MongoDB outbox spike (roadmap S9) against a real replica set:
//! the core's conformance suites with [`MongoOutbox`] as the bundle's
//! outbox (the outbox cases, the cross-port invariants, the insert raced
//! against a move, then every suite), and what only MongoDB needs proved:
//! an unbinding waits for an insert in flight (the write that makes the
//! routing check atomic), write conflicts past the budget are
//! `StorageError::Busy`, two purges at once count each event once, and the
//! stored names. Skipped unless `META_WHATSAPP_RS_TEST_MONGODB_URL` is set;
//! `META_WHATSAPP_RS_REQUIRE_LIVE=1` (as in `just test-live`) turns the
//! skip into a failure. Each backend works in a `wa_test_*` database of
//! its own, dropped when the test ends, a panicking one included.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_server_core::backend::Backend;
use meta_whatsapp_server_core::conformance;
use meta_whatsapp_server_core::conformance::outbox::{bind, row};
use meta_whatsapp_server_core::model::TenantId;
use meta_whatsapp_server_core::outbox::{EventQuery, Outbox};
use meta_whatsapp_server_store_mongodb::{MongoOutbox, schema};
use mongodb::bson::{Document, doc};

use support::Live;

fn query(tenant: &str) -> EventQuery {
    EventQuery {
        tenant: TenantId::parse(tenant).unwrap(),
        after: None,
        types: None,
        phone_number_id: None,
        limit: 10_000,
        max_bytes: usize::MAX,
    }
}

/// Roadmap S9's decisive case, its first half: the outbox's own cases
/// (`conformance::outbox`: each tenant's stream, pages and their byte
/// budget, dedup and its window, filters, purges per stream).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_mongodb_outbox_passes_the_outbox_cases() {
    let Some(live) = Live::new().await else {
        return;
    };
    let backend = live.backend().await;
    conformance::outbox::run(&backend).await;
}

/// Roadmap S9's decisive case, its second half: the cross-port invariants
/// (`conformance::invariants`: the same data from every accessor, a
/// deleted tenant's stream purged, the route guard checked with the insert
/// in every case of S2 and S2b, each tenant's inserts committed in
/// sequence order under eight writers).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_mongodb_outbox_passes_the_cross_port_invariants() {
    let Some(live) = Live::new().await else {
        return;
    };
    let backend = live.backend().await;
    conformance::invariants::run(&backend).await;
}

/// The race of an insert against its binding moving
/// (`conformance::races`, the outbox's), its full rounds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_mongodb_an_insert_racing_a_move_never_reaches_the_new_holder() {
    let Some(live) = Live::new().await else {
        return;
    };
    let backend = live.backend().await;
    let report = conformance::races::an_insert_racing_a_move_never_reaches_the_new_holder(
        &backend,
        conformance::races::ROUNDS,
    )
    .await;
    eprintln!("mongodb outbox, an insert racing a move: {report:?}");
}

/// Every suite of the core's conformance, each on a bundle of its own
/// with this outbox: the webhook pipeline's records its events here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_mongodb_outbox_in_a_bundle_passes_the_conformance_suite() {
    let Some(live) = Live::new().await else {
        return;
    };
    // Boxed: the whole suite's future is past clippy's `large_futures`
    // bound (as the memory backend's test boxes it).
    let report = Box::pin(conformance::run(async || {
        Arc::new(live.backend().await) as Arc<dyn Backend>
    }))
    .await;
    eprintln!("mongodb outbox in a bundle, the races: {report:?}");
}

/// The routing check is atomic with the insert because the insert writes
/// the guarded binding's document in its transaction: an unbinding (a
/// write of that document) waits until the insert in flight committed,
/// so the row is the tenant's that held the binding throughout, and an
/// insert routed once the unbinding returned is no tenant's. Decisive:
/// the insert's write of the binding (its `insert_checks`), and the
/// transaction around it; without either, the unbinding returns while
/// the insert is held in flight.
#[cfg(feature = "test-hooks")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_mongodb_an_unbinding_waits_for_the_insert_in_flight() {
    use std::sync::Mutex;

    use meta_whatsapp_rs::core::ids::WabaId;
    use meta_whatsapp_server_core::conformance::outbox::waba_of;
    use tokio::sync::oneshot;

    let Some(live) = Live::new().await else {
        return;
    };
    let backend = live.backend().await;
    let records = backend.records();
    bind(records.as_ref(), "inflight-a", "33").await;

    let (reached_tx, reached_rx) = oneshot::channel::<()>();
    let (go_tx, go_rx) = oneshot::channel::<()>();
    let pause = Arc::new(Mutex::new(Some((reached_tx, go_rx))));
    let hooked = backend
        .mongo_outbox()
        .clone()
        .with_insert_hook(Arc::new(move || {
            let pause = pause.lock().unwrap().take();
            Box::pin(async move {
                if let Some((reached, go)) = pause {
                    // A failing test drops both ends: let the insert go.
                    reached.send(()).ok();
                    go.await.ok();
                }
            })
        }));
    let event = row(Some("inflight-a"), "message_received", "33", None);
    let insert = {
        let event = event.clone();
        tokio::spawn(async move { hooked.insert(&event).await })
    };
    tokio::time::timeout(Duration::from_secs(10), reached_rx)
        .await
        .expect("the insert reached its pause")
        .unwrap();
    let unbind = {
        let records = records.clone();
        tokio::spawn(async move {
            records
                .unbind_waba(&WabaId::new(waba_of("33")).unwrap())
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !unbind.is_finished(),
        "the unbinding returned while an insert that checked its binding was in flight"
    );
    go_tx.send(()).unwrap();
    let sequence = insert.await.unwrap().unwrap();
    assert_eq!(sequence, Some(1));
    assert!(
        unbind.await.unwrap().unwrap(),
        "unbound once the insert committed"
    );
    let outbox = backend.outbox();
    let ids: Vec<String> = outbox
        .page(&query("inflight-a"))
        .await
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(ids, [event.id], "committed while the binding held");
    // Routed before the unbinding, inserted after it: no tenant's.
    let late = row(Some("inflight-a"), "message_received", "33", None);
    outbox.insert(&late).await.unwrap().unwrap();
    assert_eq!(
        outbox
            .page(&query("inflight-a"))
            .await
            .unwrap()
            .events
            .len(),
        1
    );
}

/// The port's typed contention: an insert whose stream's document another
/// transaction holds keeps meeting write conflicts, and once its budget is
/// spent answers `StorageError::Busy`, having written nothing; the same
/// insert succeeds once the other transaction ended. Decisive: the budget,
/// and the mapping of a conflict past it to `Busy` (not a backend error).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_mongodb_write_conflicts_past_the_budget_are_busy() {
    let Some(live) = Live::new().await else {
        return;
    };
    let backend = live.backend().await;
    bind(backend.records().as_ref(), "busy-a", "34").await;
    let outbox = backend
        .mongo_outbox()
        .clone()
        .with_busy_after(Duration::from_millis(300));
    let first = row(Some("busy-a"), "message_received", "34", None);
    assert_eq!(outbox.insert(&first).await.unwrap(), Some(1));

    // Another writer of the stream, in a transaction left open.
    let database = backend.mongo_outbox().database();
    let mut holder = live.client.start_session().await.unwrap();
    holder.start_transaction().await.unwrap();
    database
        .collection::<Document>(schema::STREAMS)
        .update_one(doc! { "_id": "busy-a" }, doc! { "$inc": { "held": 1 } })
        .session(&mut holder)
        .await
        .unwrap();
    let started = Instant::now();
    let second = row(Some("busy-a"), "message_received", "34", None);
    let refused = outbox.insert(&second).await;
    assert!(
        matches!(refused, Err(StorageError::Busy)),
        "{refused:?} after {:?}",
        started.elapsed()
    );
    assert!(started.elapsed() >= Duration::from_millis(300));
    holder.abort_transaction().await.unwrap();
    let page = outbox.page(&query("busy-a")).await.unwrap();
    assert_eq!(page.high_water, 1, "the refused insert drew no sequence");
    assert_eq!(outbox.insert(&second).await.unwrap(), Some(2));
}

/// Retention takes no lock of its own (housekeeping holds its turn), so
/// two replicas may purge at once: each event is deleted, and counted,
/// once, and each stream's mark ends at its last event. Two streams, one
/// of them past the 1,000 events a purge's transaction deletes at most.
/// Decisive: the count taken only from committed steps, and the mark
/// written with the deletion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_mongodb_two_purges_at_once_count_each_event_once() {
    let Some(live) = Live::new().await else {
        return;
    };
    let backend = live.backend().await;
    let records = backend.records();
    let streams = [("purge-a", "36", 1100_usize), ("purge-b", "37", 300)];
    let mut writers = Vec::new();
    for (tenant, pn, count) in streams {
        bind(records.as_ref(), tenant, pn).await;
        let outbox = backend.outbox();
        writers.push(tokio::spawn(async move {
            for _ in 0..count {
                outbox
                    .insert(&row(Some(tenant), "message_received", pn, None))
                    .await
                    .unwrap()
                    .unwrap();
            }
        }));
    }
    for writer in writers {
        writer.await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    // A second replica: its own client on the same database.
    let other_client = mongodb::Client::with_uri_str(&live.url).await.unwrap();
    let other = MongoOutbox::new(other_client.database(backend.mongo_outbox().database().name()));
    let one = backend.outbox();
    let (a, b) = tokio::join!(one.purge(Duration::ZERO), other.purge(Duration::ZERO));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a + b, 1400, "each event counted once ({a} + {b})");
    for (tenant, _, count) in streams {
        let page = one.page(&query(tenant)).await.unwrap();
        let last = i64::try_from(count).unwrap();
        assert!(page.events.is_empty(), "{tenant}");
        assert_eq!(
            (page.purged_through, page.high_water),
            (last, last),
            "{tenant}"
        );
    }
    other_client.shutdown().await;
}

/// The collections and indexes `migrate` makes, by name (the stored names
/// of `schema`, pending the owner's confirmation), and a second `migrate`
/// on the same database changes nothing.
#[tokio::test]
async fn live_mongodb_migrate_makes_the_named_collections_and_indexes_once() {
    let Some(live) = Live::new().await else {
        return;
    };
    let database = live.database();
    let outbox = MongoOutbox::new(database.clone());
    outbox.migrate().await.unwrap();
    outbox.migrate().await.unwrap();
    let mut collections = database.list_collection_names().await.unwrap();
    collections.sort();
    assert_eq!(
        collections,
        [schema::STREAMS, schema::EVENTS, schema::WABAS]
    );
    let mut indexes = database
        .collection::<Document>(schema::EVENTS)
        .list_index_names()
        .await
        .unwrap();
    indexes.sort();
    assert_eq!(
        indexes,
        [
            "_id_",
            schema::INDEX_CREATED_AT,
            schema::INDEX_DEDUP_KEY,
            schema::INDEX_MEDIA,
            schema::INDEX_STREAM_SEQUENCE,
        ]
    );
}

/// Roadmap M2e on MongoDB: a row keeps the media id its event records as
/// received ([`schema::EVENT_MEDIA_ID`], beside its ordering
/// [`schema::EVENT_MEDIA_ORDER`]), a row recording none has neither field,
/// and `received_media` reads the partial index `INDEX_MEDIA` (its use
/// counted by `$indexStats`), never the rows' data. Rows of one millisecond
/// come back newest first all the same: thirty inserts in a row, many of
/// them in the same millisecond on a fast server, read back in reverse.
/// Decisive: the field written at insert, the lookup's filter repeating the
/// index's (without it, the planner may not use a partial index), and the
/// tie-break (in a run where two inserts shared a millisecond).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_mongodb_received_media_reads_its_index_in_order() {
    use meta_whatsapp_rs::core::ids::MediaId;
    use meta_whatsapp_server_core::outbox::{NewEvent, RECEIVED_MEDIA_ROWS};

    let Some(live) = Live::new().await else {
        return;
    };
    let backend = live.backend().await;
    let records = backend.records();
    let outbox = backend.outbox();
    let media = MediaId::new("1003383421387256").unwrap();
    let mut tenants = Vec::new();
    for (i, pn) in ["41", "42", "43"].into_iter().enumerate() {
        let tenant = format!("media-order-{i}");
        bind(records.as_ref(), &tenant, pn).await;
        tenants.push((tenant, pn));
    }
    // A text first: it records no media id, and has neither field.
    let text = row(Some("media-order-0"), "message_received", "41", None);
    outbox.insert(&text).await.unwrap().unwrap();
    let mut inserted = Vec::new();
    for n in 0..30 {
        let (tenant, pn) = &tenants[n % tenants.len()];
        let event = NewEvent {
            data: serde_json::json!({
                "event": "message_received",
                "phone_number_id": pn,
                "message": {"type": "image", "image": {"id": media.as_str()}},
            })
            .to_string(),
            ..row(Some(tenant), "message_received", pn, None)
        };
        outbox.insert(&event).await.unwrap().unwrap();
        inserted.push(tenant.clone());
    }
    let events = backend
        .mongo_outbox()
        .database()
        .collection::<Document>(schema::EVENTS);
    let stored = events
        .find_one(doc! { "_id": text.id.as_str() })
        .await
        .unwrap()
        .unwrap();
    assert!(!stored.contains_key(schema::EVENT_MEDIA_ID), "{stored:?}");
    assert!(
        !stored.contains_key(schema::EVENT_MEDIA_ORDER),
        "{stored:?}"
    );
    let with_media = events
        .count_documents(doc! { schema::EVENT_MEDIA_ID: media.as_str() })
        .await
        .unwrap();
    assert_eq!(with_media, 30);

    let uses = || async {
        let mut cursor = events
            .aggregate([doc! { "$indexStats": {} }])
            .await
            .unwrap();
        let mut ops = None;
        while cursor.advance().await.unwrap() {
            let stats = cursor.deserialize_current().unwrap();
            if stats.get_str("name").unwrap() == schema::INDEX_MEDIA {
                let accesses = stats.get_document("accesses").unwrap();
                ops = Some(match accesses.get("ops").unwrap() {
                    mongodb::bson::Bson::Int64(n) => *n,
                    mongodb::bson::Bson::Int32(n) => i64::from(*n),
                    other => panic!("ops: {other:?}"),
                });
            }
        }
        ops.expect("the media index exists")
    };
    let before = uses().await;
    let found = outbox.received_media(&media).await.unwrap();
    assert!(
        uses().await > before,
        "received_media did not read its index"
    );

    let expected: Vec<Option<String>> = inserted
        .iter()
        .rev()
        .take(RECEIVED_MEDIA_ROWS)
        .map(|tenant| Some(tenant.clone()))
        .collect();
    let seen: Vec<Option<String>> = found
        .iter()
        .map(|row| row.tenant.as_ref().map(|t| t.as_str().to_owned()))
        .collect();
    assert_eq!(
        seen, expected,
        "not the newest {RECEIVED_MEDIA_ROWS}, newest first"
    );
}
