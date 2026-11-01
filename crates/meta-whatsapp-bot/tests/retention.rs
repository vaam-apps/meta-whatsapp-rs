//! Retention and auto-delete (roadmap B4): a `RetentionPolicy` deletes the
//! stored history by age and by a cap per chat, through the store's own
//! operations, once (`apply`) or on a schedule (`RetentionSweeper`), on a
//! `ManualClock`. The scenarios run on the in-memory store, and live on
//! Postgres (`live_postgres_*`: skipped unless
//! `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set;
//! `META_WHATSAPP_RS_REQUIRE_LIVE=1` makes a skip a failure).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use meta_whatsapp_adapters::store::MemoryConversationStore;
use meta_whatsapp_bot::Timer;
use meta_whatsapp_bot::retention::{RetentionPolicy, RetentionSweeper, Swept};
use meta_whatsapp_core::clock::{Clock, ManualClock};
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_core::store::{
    ConversationKey, ConversationStore, DeliveryStatus, Direction, IdentityLink, Purged,
    StoredMessage,
};
use pretty_assertions::assert_eq;
use time::OffsetDateTime;
use time::macros::datetime;

const NOW: OffsetDateTime = datetime!(2026-10-01 12:00 UTC);
const DAY: time::Duration = time::Duration::days(1);
const WEEK: Duration = Duration::from_hours(24 * 7);

fn number(n: &str) -> PhoneNumberId {
    PhoneNumberId::new(n).unwrap()
}

/// Message `local` of chat `contact` on `pn`, at `at`.
fn message(
    pn: &PhoneNumberId,
    contact: &str,
    local: &str,
    direction: Direction,
    at: OffsetDateTime,
) -> StoredMessage {
    StoredMessage {
        id: MessageId::new(format!("wamid.{}.{contact}.{local}", pn.as_str())),
        conversation: ConversationKey::new(pn.clone(), contact),
        direction,
        kind: "text".to_owned(),
        text: Some(local.to_owned()),
        payload: serde_json::json!({"from": contact, "type": "text", "text": {"body": local}}),
        status: match direction {
            Direction::Inbound => DeliveryStatus::Received,
            Direction::Outbound => DeliveryStatus::Accepted,
        },
        timestamp: at,
        status_at: None,
        error: None,
    }
}

/// The texts of a chat's history, newest first.
async fn texts(store: &dyn ConversationStore, pn: &PhoneNumberId, contact: &str) -> Vec<String> {
    store
        .messages(&ConversationKey::new(pn.clone(), contact), None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|m| m.text)
        .collect()
}

/// The chats a number's inbox lists, most recent first.
async fn listed(store: &dyn ConversationStore, pn: &PhoneNumberId) -> Vec<String> {
    store
        .conversations(pn, None, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.key.contact)
        .collect()
}

/// On `pn`: "US.old", every message older than a week; "US.mixed", one
/// older than a week, one exactly a week old (at the cutoff), and four
/// newer; a group of four messages within the week. Plus an identity
/// link, a week and a day old.
async fn record(store: &dyn ConversationStore, pn: &PhoneNumberId) {
    use Direction::{Inbound, Outbound};
    let rows = [
        ("US.old", "old-1", Inbound, NOW - 10 * DAY),
        ("US.old", "old-2", Outbound, NOW - 9 * DAY),
        ("US.mixed", "8 days", Inbound, NOW - 8 * DAY),
        ("US.mixed", "a week", Inbound, NOW - 7 * DAY),
        ("US.mixed", "2 days", Outbound, NOW - 2 * DAY),
        (
            "US.mixed",
            "1 hour",
            Inbound,
            NOW - time::Duration::hours(1),
        ),
        (
            "US.mixed",
            "30 min",
            Inbound,
            NOW - time::Duration::minutes(30),
        ),
        (
            "US.mixed",
            "10 min",
            Outbound,
            NOW - time::Duration::minutes(10),
        ),
    ];
    for (contact, local, direction, at) in rows {
        assert!(
            store
                .append(message(pn, contact, local, direction, at))
                .await
                .unwrap()
        );
    }
    for (i, who) in ["US.A", "US.B", "US.A", "US.B"].into_iter().enumerate() {
        let at = NOW - time::Duration::hours(4 - i64::try_from(i).unwrap());
        let m = StoredMessage {
            payload: serde_json::json!({"from_user_id": who, "group_id": "HBgGROUP"}),
            ..message(pn, "HBgGROUP", &format!("g{i}"), Inbound, at)
        };
        assert!(store.append(m).await.unwrap());
    }
    assert!(
        store
            .link_identity(IdentityLink::new(
                pn.clone(),
                "US.old",
                "US.new",
                NOW - 8 * DAY
            ))
            .await
            .unwrap()
    );
}

/// What a week's age deletes on its own: the older messages, and the
/// summary of the chat whose latest message was one of them; the message
/// exactly a week old stays.
async fn the_age_filter_deletes_exactly_what_is_older(store: Arc<dyn ConversationStore>) {
    let (pn, other) = (number("106540352242922"), number("106540352242923"));
    record(store.as_ref(), &pn).await;
    record(store.as_ref(), &other).await;
    let policy = RetentionPolicy::default().max_age(WEEK);
    assert_eq!(policy.cutoff(NOW), Some(NOW - 7 * DAY));

    let swept = policy.apply(store.as_ref(), Some(&pn), NOW).await.unwrap();
    assert_eq!(
        swept.by_age,
        Purged {
            messages: 3,
            conversations: 1,
            ..Purged::default()
        },
        "old-1, old-2 and \"8 days\"; the summary of US.old"
    );
    assert!(swept.by_cap.is_empty(), "no cap set");
    assert!(texts(store.as_ref(), &pn, "US.old").await.is_empty());
    assert_eq!(
        texts(store.as_ref(), &pn, "US.mixed").await,
        ["10 min", "30 min", "1 hour", "2 days", "a week"],
        "what is not older than a week stays, the cutoff itself included"
    );
    assert_eq!(
        listed(store.as_ref(), &pn).await,
        ["US.mixed", "HBgGROUP"],
        "US.old is gone from the inbox too"
    );
    assert_eq!(
        store
            .identity_links(&ConversationKey::new(pn.clone(), "US.old"))
            .await
            .unwrap()
            .len(),
        1,
        "identity links outlive a retention (design D35)"
    );
    assert_eq!(
        listed(store.as_ref(), &other).await,
        ["US.mixed", "HBgGROUP", "US.old"],
        "another number, under a sweep scoped to one, keeps everything"
    );
}

/// Both: by age first, then each chat's newest three rows.
async fn a_sweep_deletes_by_age_and_cap_from_messages_and_conversations(
    store: Arc<dyn ConversationStore>,
) {
    let (pn, other) = (number("106540352242922"), number("106540352242923"));
    record(store.as_ref(), &pn).await;
    record(store.as_ref(), &other).await;
    let policy = RetentionPolicy::default()
        .max_age(WEEK)
        .max_messages_per_chat(3);
    let clock = Arc::new(ManualClock::new(NOW));
    let sweeper = RetentionSweeper::new(Arc::clone(&store), policy)
        .number(pn.clone())
        .timer(clock.clone());

    let swept = sweeper.sweep().await.unwrap();
    assert_eq!(
        swept,
        expected_sweep(),
        "by age: old-1, old-2, \"8 days\" and US.old's summary; by the cap: \"2 days\", \"a \
         week\" and the group's oldest"
    );
    assert_eq!(
        swept.total(),
        Purged {
            messages: 6,
            conversations: 1,
            ..Purged::default()
        }
    );
    assert!(texts(store.as_ref(), &pn, "US.old").await.is_empty());
    assert_eq!(
        texts(store.as_ref(), &pn, "US.mixed").await,
        ["10 min", "30 min", "1 hour"]
    );
    assert_eq!(
        texts(store.as_ref(), &pn, "HBgGROUP").await,
        ["g3", "g2", "g1"]
    );
    let inbox = store.conversations(&pn, None, 100).await.unwrap();
    assert_eq!(
        inbox
            .iter()
            .map(|s| (s.key.contact.as_str(), s.last_text.as_deref()))
            .collect::<Vec<_>>(),
        [("US.mixed", Some("10 min")), ("HBgGROUP", Some("g3"))],
        "the chats left, each on its latest message"
    );
    assert!(
        sweeper.sweep().await.unwrap().is_empty(),
        "a second sweep at the same time deletes nothing"
    );
    assert_eq!(
        listed(store.as_ref(), &other).await,
        ["US.mixed", "HBgGROUP", "US.old"],
        "a number the sweeper was not given keeps everything"
    );

    // Every number, at the same time: the other number's share; the first
    // number's is done.
    let everywhere = RetentionSweeper::new(Arc::clone(&store), policy).timer(clock);
    assert_eq!(everywhere.sweep().await.unwrap(), expected_sweep());
    assert_eq!(
        listed(store.as_ref(), &other).await,
        ["US.mixed", "HBgGROUP"]
    );
    assert_eq!(
        texts(store.as_ref(), &pn, "US.mixed").await,
        ["10 min", "30 min", "1 hour"]
    );
}

/// What the scenario's sweep deletes on one number.
fn expected_sweep() -> Swept {
    let mut swept = Swept::default();
    swept.by_age = Purged {
        messages: 3,
        conversations: 1,
        ..Purged::default()
    };
    swept.by_cap = Purged {
        messages: 3,
        ..Purged::default()
    };
    swept
}

#[tokio::test]
async fn the_age_filter_deletes_exactly_what_is_older_in_memory() {
    the_age_filter_deletes_exactly_what_is_older(Arc::new(MemoryConversationStore::new())).await;
}

#[tokio::test]
async fn a_sweep_deletes_by_age_and_cap_from_messages_and_conversations_in_memory() {
    a_sweep_deletes_by_age_and_cap_from_messages_and_conversations(Arc::new(
        MemoryConversationStore::new(),
    ))
    .await;
}

/// The cap alone keeps each chat's newest rows, whatever their age.
#[tokio::test]
async fn the_cap_alone_keeps_the_newest_of_each_chat() {
    let store = MemoryConversationStore::new();
    let pn = number("106540352242922");
    record(&store, &pn).await;
    let swept = RetentionPolicy::default()
        .max_messages_per_chat(2)
        .apply(&store, None, NOW)
        .await
        .unwrap();
    assert!(swept.by_age.is_empty(), "no age set");
    assert_eq!(
        swept.by_cap,
        Purged {
            messages: 4 + 2,
            ..Purged::default()
        },
        "four of US.mixed, two of the group; US.old keeps its two, however old"
    );
    assert_eq!(texts(&store, &pn, "US.old").await, ["old-2", "old-1"]);
    assert_eq!(texts(&store, &pn, "US.mixed").await, ["10 min", "30 min"]);
}

/// The default policy deletes nothing, and says so.
#[tokio::test]
async fn the_default_policy_keeps_everything() {
    let store = MemoryConversationStore::new();
    let pn = number("106540352242922");
    record(&store, &pn).await;
    let policy = RetentionPolicy::default();
    assert!(policy.keeps_everything());
    assert_eq!(policy.cutoff(NOW), None);
    assert!(!policy.max_age(WEEK).keeps_everything());
    assert!(!policy.max_messages_per_chat(10).keeps_everything());
    assert!(policy.apply(&store, None, NOW).await.unwrap().is_empty());
    assert_eq!(texts(&store, &pn, "US.mixed").await.len(), 6);
    assert_eq!(listed(&store, &pn).await.len(), 3);
    assert_eq!(
        RetentionPolicy::default()
            .max_age(Duration::MAX)
            .cutoff(NOW),
        None,
        "an age before the earliest time keeps everything"
    );
}

/// `run` sweeps at once, then every interval on the timer, and stops
/// before the next sweep once `stop` completed: here at the fourth
/// interval, so the sweeps are at NOW, NOW + 1 h and NOW + 2 h.
#[tokio::test]
async fn the_sweeper_runs_on_its_timer_until_stopped() {
    let store = Arc::new(MemoryConversationStore::new());
    let pn = number("106540352242922");
    // Each message falls out of a 90-minute retention at one sweep: the
    // cutoffs are NOW - 90 min, NOW - 30 min and NOW + 30 min, and would
    // be NOW + 90 min at NOW + 3 h.
    for (local, minutes) in [
        ("first", -100),
        ("second", -40),
        ("third", 20),
        ("kept", 40),
    ] {
        let m = message(
            &pn,
            "US.1",
            local,
            Direction::Inbound,
            NOW + time::Duration::minutes(minutes),
        );
        assert!(store.append(m).await.unwrap());
    }
    let clock = Arc::new(ManualClock::new(NOW));
    let sweeper = RetentionSweeper::new(
        store.clone(),
        RetentionPolicy::default().max_age(Duration::from_mins(90)),
    )
    .timer(clock.clone())
    .every(Duration::from_hours(1));
    let stop_at = NOW + time::Duration::hours(3);
    let stop = std::future::poll_fn(|_| {
        if clock.now() >= stop_at {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    });
    sweeper.run(stop).await;
    assert_eq!(clock.now(), stop_at, "three waits of an hour");
    assert_eq!(
        texts(store.as_ref(), &pn, "US.1").await,
        ["kept"],
        "the sweep at NOW + 2 h deleted what was older than NOW + 30 min, and no sweep ran at NOW \
         + 3 h"
    );

    // Stopped before it starts: no sweep at all.
    let fresh = Arc::new(MemoryConversationStore::new());
    let m = message(&pn, "US.2", "old", Direction::Inbound, NOW - 10 * DAY);
    assert!(fresh.append(m).await.unwrap());
    RetentionSweeper::new(fresh.clone(), RetentionPolicy::default().max_age(WEEK))
        .timer(Arc::new(ManualClock::new(NOW)))
        .run(std::future::ready(()))
        .await;
    assert_eq!(texts(fresh.as_ref(), &pn, "US.2").await, ["old"]);
}

/// The sweeper's settings: a zero interval is taken as the shortest one,
/// a number given twice is swept once, and its `Debug` names no message.
#[tokio::test]
async fn the_sweeper_keeps_its_settings() {
    let store = Arc::new(MemoryConversationStore::new());
    let pn = number("106540352242922");
    record(store.as_ref(), &pn).await;
    let clock: Arc<dyn Timer> = Arc::new(ManualClock::new(NOW));
    let sweeper = RetentionSweeper::new(
        store.clone(),
        RetentionPolicy::default().max_messages_per_chat(1),
    )
    .every(Duration::ZERO)
    .number(pn.clone())
    .number(pn.clone())
    .timer(clock);
    assert_eq!(
        sweeper.policy(),
        RetentionPolicy::default().max_messages_per_chat(1)
    );
    let debug = format!("{sweeper:?}");
    assert!(debug.contains("every: 1ms"), "{debug}");
    assert!(!debug.contains("10 min"), "{debug}");
    assert_eq!(
        sweeper.sweep().await.unwrap().by_cap.messages,
        1 + 5 + 3,
        "each chat swept once"
    );
}

/// The sweeper sweeps each number given, every one of them, in turn, and
/// adds up what each sweep deleted; a number not given keeps everything.
#[tokio::test]
async fn the_sweeper_sweeps_every_number_given_and_no_other() {
    let store = Arc::new(MemoryConversationStore::new());
    let numbers = [
        number("106540352242922"),
        number("106540352242923"),
        number("106540352242924"),
    ];
    for pn in &numbers {
        record(store.as_ref(), pn).await;
    }
    let sweeper = RetentionSweeper::new(
        store.clone(),
        RetentionPolicy::default()
            .max_age(WEEK)
            .max_messages_per_chat(3),
    )
    .number(numbers[0].clone())
    .number(numbers[1].clone())
    .timer(Arc::new(ManualClock::new(NOW)));
    let mut both = Swept::default();
    both.by_age = Purged {
        messages: 2 * 3,
        conversations: 2,
        ..Purged::default()
    };
    both.by_cap = Purged {
        messages: 2 * 3,
        ..Purged::default()
    };
    assert_eq!(
        sweeper.sweep().await.unwrap(),
        both,
        "each of the two numbers' share, added up"
    );
    for pn in &numbers[..2] {
        assert_eq!(listed(store.as_ref(), pn).await, ["US.mixed", "HBgGROUP"]);
        assert_eq!(
            texts(store.as_ref(), pn, "US.mixed").await,
            ["10 min", "30 min", "1 hour"],
            "{pn}"
        );
    }
    assert_eq!(
        listed(store.as_ref(), &numbers[2]).await,
        ["US.mixed", "HBgGROUP", "US.old"],
        "the number not given keeps everything"
    );
    assert_eq!(
        texts(store.as_ref(), &numbers[2], "US.mixed").await.len(),
        6
    );
}

/// The same scenarios on Postgres, each in a schema of its own.
mod live {
    use std::str::FromStr;

    use meta_whatsapp_adapters::store::PostgresConversationStore;
    use meta_whatsapp_adapters::store::postgres::{self, sqlx};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    /// A schema of its own on the test database, dropped with it (when the
    /// test panics too), and the store on it.
    struct TestDb {
        store: Arc<dyn ConversationStore>,
        _cleanup: Cleanup,
    }

    impl TestDb {
        async fn new() -> Option<Self> {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let url = match std::env::var("META_WHATSAPP_RS_TEST_POSTGRES_URL") {
                Ok(url) if !url.trim().is_empty() => url,
                _ if std::env::var("META_WHATSAPP_RS_REQUIRE_LIVE").is_ok_and(|v| v == "1") => {
                    panic!(
                        "META_WHATSAPP_RS_TEST_POSTGRES_URL is not set, and \
                         META_WHATSAPP_RS_REQUIRE_LIVE=1 turns a skipped live test into a failure"
                    )
                }
                _ => {
                    eprintln!(
                        "skipping: META_WHATSAPP_RS_TEST_POSTGRES_URL is not set \
                         (META_WHATSAPP_RS_REQUIRE_LIVE=1 makes this a failure)"
                    );
                    return None;
                }
            };
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let t = OffsetDateTime::now_utc().unix_timestamp_nanos();
            let schema = format!("wa_test_{t:x}_{}_{n}", std::process::id());
            let cleanup = Cleanup {
                url: url.clone(),
                statement: format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
            };
            let admin = PgPoolOptions::new()
                .max_connections(1)
                .connect(&url)
                .await
                .expect("connect to META_WHATSAPP_RS_TEST_POSTGRES_URL");
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
                .execute(&admin)
                .await
                .unwrap();
            admin.close().await;
            let options = PgConnectOptions::from_str(&url)
                .unwrap()
                .options([("search_path", schema.as_str())]);
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .connect_with(options)
                .await
                .unwrap();
            postgres::migrate(&pool).await.unwrap();
            Some(Self {
                store: Arc::new(PostgresConversationStore::new(pool)),
                _cleanup: cleanup,
            })
        }
    }

    /// Runs `statement` when dropped, on a connection and a runtime of its
    /// own (the test's runtime may be the one unwinding), as the adapters'
    /// live tests do.
    struct Cleanup {
        url: String,
        statement: String,
    }

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let (url, statement) = (self.url.clone(), self.statement.clone());
            let result = std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                runtime.block_on(async {
                    use sqlx::{ConnectOptions, Connection};
                    let mut conn = PgConnectOptions::from_str(&url)
                        .map_err(|e| e.to_string())?
                        .options([("lock_timeout", "10s")])
                        .connect()
                        .await
                        .map_err(|e| e.to_string())?;
                    sqlx::query(sqlx::AssertSqlSafe(statement))
                        .execute(&mut conn)
                        .await
                        .map_err(|e| e.to_string())?;
                    conn.close().await.map_err(|e| e.to_string())
                })
            })
            .join()
            .unwrap_or_else(|_| Err("the cleanup thread panicked".to_owned()));
            if let Err(e) = result {
                if std::thread::panicking() {
                    eprintln!("cleanup `{}` failed: {e}", self.statement);
                } else {
                    panic!("cleanup `{}` failed: {e}", self.statement);
                }
            }
        }
    }

    #[tokio::test]
    async fn live_postgres_the_age_filter_deletes_exactly_what_is_older() {
        let Some(db) = TestDb::new().await else {
            return;
        };
        the_age_filter_deletes_exactly_what_is_older(db.store.clone()).await;
    }

    #[tokio::test]
    async fn live_postgres_a_sweep_deletes_by_age_and_cap_from_messages_and_conversations() {
        let Some(db) = TestDb::new().await else {
            return;
        };
        a_sweep_deletes_by_age_and_cap_from_messages_and_conversations(db.store.clone()).await;
    }
}
