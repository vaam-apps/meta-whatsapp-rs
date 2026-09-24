#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)] // test helper: panics are the report
//! The executable `ConversationStore` contract.
//!
//! ```ignore
//! wa_adapters::store::conversation_conformance::run(&my_store).await;
//! ```
//!
//! Every check uses a phone number id and message ids unique to the run, so a
//! shared backend (a real Postgres) can be reused across runs and by
//! parallel test processes. Timestamps are whole seconds: backends may store
//! less than nanosecond precision (Postgres keeps microseconds), and Meta's
//! own timestamps are seconds.
//!
//! What it pins down, beyond the rustdoc of the port:
//!
//! - appending an existing id changes nothing, including the unread count;
//! - status updates follow [`DeliveryStatus::supersedes`]: late webhooks
//!   never move a message backwards, replays are no-ops, `failed`/`deleted`
//!   win, and concurrent updates end on the highest status;
//! - history is `(timestamp, id)` descending in **byte order of the id**
//!   (the order of Rust's `str`), and paging with the exclusive cursor
//!   neither repeats nor skips rows, even when every timestamp collides;
//! - the conversation list is `(last_message_at, contact)` descending with
//!   the same cursor rule, scoped to one business number;
//! - the inbox summary follows the newest message by `(timestamp, id)`, not
//!   the most recently appended one;
//! - unread counts inbound messages appended since the last `mark_read`, by
//!   arrival, and concurrent appends are all counted.

use time::OffsetDateTime;
use time::macros::datetime;
use wa_core::ids::{MessageId, PhoneNumberId};
use wa_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, DeliveryStatus, Direction,
    StoredMessage,
};

/// Run the suite.
///
/// # Panics
///
/// On any contract violation — this is a test helper.
pub async fn run<S: ConversationStore + ?Sized>(store: &S) {
    append_is_idempotent(store).await;
    statuses_never_regress(store).await;
    terminal_statuses_win(store).await;
    unknown_message_status_update_is_false(store).await;
    history_order_and_exclusive_cursor(store).await;
    paging_with_identical_timestamps(store).await;
    conversation_list_paging(store).await;
    summary_tracks_latest_message(store).await;
    unread_counting(store).await;
    last_inbound_at(store).await;
    zero_limit(store).await;
    concurrent_appends(store).await;
    concurrent_status_updates(store).await;
}

const T0: OffsetDateTime = datetime!(2026-09-24 12:00 UTC);

fn at(secs: i64) -> OffsetDateTime {
    T0 + time::Duration::seconds(secs)
}

fn unique() -> String {
    // Uniqueness, not secrecy: a per-process counter plus the start time
    // keeps concurrent runs against one shared backend from colliding.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = OffsetDateTime::now_utc().unix_timestamp_nanos();
    format!("{t:x}-{}-{n}", std::process::id())
}

/// One check's namespace: a fresh business number and an id prefix.
struct Run {
    pn: PhoneNumberId,
    tag: String,
}

impl Run {
    fn new(name: &str) -> Self {
        let tag = format!("{name}-{}", unique());
        Self {
            pn: PhoneNumberId::new(format!("pn-{tag}")),
            tag,
        }
    }

    fn key(&self, contact: &str) -> ConversationKey {
        ConversationKey::new(self.pn.clone(), contact)
    }

    fn id(&self, local: &str) -> MessageId {
        MessageId::new(format!("wamid.{}.{local}", self.tag))
    }

    fn msg(
        &self,
        contact: &str,
        local_id: &str,
        direction: Direction,
        secs: i64,
        text: &str,
    ) -> StoredMessage {
        StoredMessage {
            id: self.id(local_id),
            conversation: self.key(contact),
            direction,
            kind: "text".to_owned(),
            text: Some(text.to_owned()),
            payload: serde_json::json!({
                "type": "text",
                "text": { "body": text },
                "nested": { "n": 1, "list": [1, 2, 3], "flag": true, "none": null }
            }),
            status: match direction {
                Direction::Inbound => DeliveryStatus::Received,
                Direction::Outbound => DeliveryStatus::Accepted,
            },
            timestamp: at(secs),
            status_at: None,
            error: None,
        }
    }
}

async fn summary<S: ConversationStore + ?Sized>(
    store: &S,
    key: &ConversationKey,
) -> Option<ConversationSummary> {
    store
        .conversations(&key.phone_number_id, None, 1000)
        .await
        .unwrap()
        .into_iter()
        .find(|s| &s.key == key)
}

async fn only_message<S: ConversationStore + ?Sized>(
    store: &S,
    key: &ConversationKey,
) -> StoredMessage {
    let mut all = store.messages(key, None, 10).await.unwrap();
    assert_eq!(all.len(), 1, "exactly one message in {key}");
    all.remove(0)
}

async fn append_is_idempotent<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("dedup");
    let original = r.msg("c", "1", Direction::Inbound, 0, "hello");
    assert!(
        store.append(original.clone()).await.unwrap(),
        "first append"
    );

    let replay = StoredMessage {
        kind: "image".to_owned(),
        ..r.msg("c", "1", Direction::Outbound, 5, "changed")
    };
    assert!(
        !store.append(replay).await.unwrap(),
        "appending an existing id is a no-op"
    );

    let stored = only_message(store, &r.key("c")).await;
    assert_eq!(stored, original, "the first append is kept verbatim");
    let s = summary(store, &r.key("c"))
        .await
        .expect("conversation exists");
    assert_eq!(
        s.unread, 1,
        "a replayed inbound message is not counted twice"
    );
    assert_eq!(s.last_text.as_deref(), Some("hello"));
    assert_eq!(s.last_message_at, at(0));
}

async fn statuses_never_regress<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("status");
    let m = r.msg("c", "1", Direction::Outbound, 0, "order shipped");
    let id = m.id.clone();
    store.append(m).await.unwrap();

    let update = |status, secs| store.update_status(&id, status, at(secs), None);
    assert!(update(DeliveryStatus::Sent, 1).await.unwrap(), "sent");
    assert!(update(DeliveryStatus::Read, 3).await.unwrap(), "read");
    assert!(
        !update(DeliveryStatus::Delivered, 2).await.unwrap(),
        "a late delivered never regresses a read"
    );
    assert!(
        !update(DeliveryStatus::Sent, 4).await.unwrap(),
        "a late sent never regresses a read"
    );
    assert!(
        !update(DeliveryStatus::Read, 5).await.unwrap(),
        "a replayed read is a no-op"
    );
    let stored = only_message(store, &r.key("c")).await;
    assert_eq!(stored.status, DeliveryStatus::Read);
    assert_eq!(
        stored.status_at,
        Some(at(3)),
        "status_at is the time of the status that won"
    );

    assert!(
        update(DeliveryStatus::Played, 6).await.unwrap(),
        "played supersedes read"
    );
    assert_eq!(
        only_message(store, &r.key("c")).await.status,
        DeliveryStatus::Played
    );
}

async fn terminal_statuses_win<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("terminal");
    let failed = r.msg("c", "1", Direction::Outbound, 0, "promo");
    let deleted = r.msg("c", "2", Direction::Outbound, 1, "promo 2");
    let (failed_id, deleted_id) = (failed.id.clone(), deleted.id.clone());
    store.append(failed).await.unwrap();
    store.append(deleted).await.unwrap();

    let error = serde_json::json!({"code": 131049, "title": "Per-user marketing limit"});
    assert!(
        store
            .update_status(&failed_id, DeliveryStatus::Sent, at(1), None)
            .await
            .unwrap()
    );
    assert!(
        store
            .update_status(
                &failed_id,
                DeliveryStatus::Failed,
                at(2),
                Some(error.clone())
            )
            .await
            .unwrap(),
        "failed supersedes sent"
    );
    for late in [
        DeliveryStatus::Delivered,
        DeliveryStatus::Read,
        DeliveryStatus::Deleted,
    ] {
        assert!(
            !store
                .update_status(&failed_id, late, at(3), None)
                .await
                .unwrap(),
            "{late:?} never overrides failed"
        );
    }

    assert!(
        store
            .update_status(&deleted_id, DeliveryStatus::Read, at(2), None)
            .await
            .unwrap()
    );
    assert!(
        store
            .update_status(&deleted_id, DeliveryStatus::Deleted, at(3), None)
            .await
            .unwrap(),
        "deleted supersedes read"
    );
    assert!(
        !store
            .update_status(&deleted_id, DeliveryStatus::Failed, at(4), None)
            .await
            .unwrap(),
        "terminal states do not supersede each other"
    );

    let history = store.messages(&r.key("c"), None, 10).await.unwrap();
    let find = |id: &MessageId| history.iter().find(|m| &m.id == id).unwrap().clone();
    let f = find(&failed_id);
    assert_eq!(f.status, DeliveryStatus::Failed);
    assert_eq!(f.error, Some(error), "the failure's error object is stored");
    assert_eq!(f.status_at, Some(at(2)));
    let d = find(&deleted_id);
    assert_eq!(d.status, DeliveryStatus::Deleted);
    assert_eq!(d.error, None);
}

async fn unknown_message_status_update_is_false<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("unknown");
    assert!(
        !store
            .update_status(&r.id("never-appended"), DeliveryStatus::Read, at(0), None)
            .await
            .unwrap(),
        "a status for an unknown message changes nothing"
    );
    assert!(
        store
            .conversations(&r.pn, None, 10)
            .await
            .unwrap()
            .is_empty(),
        "and creates nothing"
    );
}

async fn history_order_and_exclusive_cursor<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("order");
    // Appended out of timestamp order on purpose.
    for (local, secs) in [("m2", 2), ("m0", 0), ("m4", 4), ("m1", 1), ("m3", 3)] {
        let dir = if secs % 2 == 0 {
            Direction::Inbound
        } else {
            Direction::Outbound
        };
        store
            .append(r.msg("c", local, dir, secs, local))
            .await
            .unwrap();
    }
    // Same business number, other contact: must never leak in.
    store
        .append(r.msg("other", "x", Direction::Inbound, 10, "x"))
        .await
        .unwrap();

    let ids = |v: Vec<StoredMessage>| v.into_iter().map(|m| m.id).collect::<Vec<_>>();
    let key = r.key("c");
    assert_eq!(
        ids(store.messages(&key, None, 10).await.unwrap()),
        ["m4", "m3", "m2", "m1", "m0"].map(|l| r.id(l)),
        "newest first"
    );
    assert_eq!(
        ids(store.messages(&key, None, 2).await.unwrap()),
        ["m4", "m3"].map(|l| r.id(l)),
        "limit"
    );
    assert_eq!(
        ids(store
            .messages(&key, Some((at(2), r.id("m2"))), 10)
            .await
            .unwrap()),
        ["m1", "m0"].map(|l| r.id(l)),
        "the cursor row itself is excluded"
    );
    assert!(
        store
            .messages(&key, Some((at(0), r.id("m0"))), 10)
            .await
            .unwrap()
            .is_empty(),
        "nothing is older than the oldest row"
    );
    assert!(
        store
            .messages(&r.key("nobody"), None, 10)
            .await
            .unwrap()
            .is_empty(),
        "unknown conversation has no history"
    );
}

async fn paging_with_identical_timestamps<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("paging");
    let key = r.key("c");
    // Ids chosen so byte order differs from any locale collation: upper case
    // before lower case, punctuation between, digits not zero-padded.
    let colliding = [
        "b", "B", "a", "A", "a-1", "a_1", "a.1", "a1", "Z", "z", "10", "9", "1", "_", "-", ".",
        "é", "e", "E", "ä",
    ];
    let mut all: Vec<(OffsetDateTime, MessageId)> = Vec::new();
    for local in colliding {
        let m = r.msg("c", local, Direction::Inbound, 10, local);
        all.push((m.timestamp, m.id.clone()));
        assert!(store.append(m).await.unwrap());
    }
    for (local, secs) in [("early", 5), ("late", 20), ("mid", 10)] {
        let m = r.msg("c", local, Direction::Outbound, secs, local);
        all.push((m.timestamp, m.id.clone()));
        assert!(store.append(m).await.unwrap());
    }
    all.sort_unstable_by(|a, b| b.cmp(a));
    let expected: Vec<MessageId> = all.into_iter().map(|(_, id)| id).collect();

    for page_size in [1, 4, 7, 100] {
        let mut seen = Vec::new();
        let mut cursor = None;
        for _ in 0..=expected.len() {
            let page = store
                .messages(&key, cursor.clone(), page_size)
                .await
                .unwrap();
            assert!(page.len() <= page_size, "page respects the limit");
            let Some(last) = page.last() else { break };
            cursor = Some((last.timestamp, last.id.clone()));
            seen.extend(page.into_iter().map(|m| m.id));
        }
        assert_eq!(
            seen, expected,
            "paging by {page_size} with colliding timestamps neither repeats nor skips, in byte order"
        );
    }
}

async fn conversation_list_paging<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("inbox");
    let contacts = [
        ("US.1", 1),
        ("us.1", 1),
        ("US.2", 1),
        ("B", 2),
        ("a", 3),
        ("A", 3),
        ("z", 0),
    ];
    for (i, (contact, secs)) in contacts.iter().enumerate() {
        // An older message first, so last_message_at must be the max.
        store
            .append(r.msg(
                contact,
                &format!("{i}-old"),
                Direction::Inbound,
                -100,
                "old",
            ))
            .await
            .unwrap();
        store
            .append(r.msg(contact, &format!("{i}"), Direction::Inbound, *secs, "new"))
            .await
            .unwrap();
    }
    // Another business number: never listed.
    let other = Run::new("inbox-other");
    store
        .append(other.msg("US.1", "1", Direction::Inbound, 50, "elsewhere"))
        .await
        .unwrap();

    let mut expected: Vec<(OffsetDateTime, String)> = contacts
        .iter()
        .map(|(c, secs)| (at(*secs), (*c).to_owned()))
        .collect();
    expected.sort_unstable_by(|a, b| b.cmp(a));

    for page_size in [1, 3, 100] {
        let mut seen = Vec::new();
        let mut cursor = None;
        for _ in 0..=expected.len() {
            let page = store
                .conversations(&r.pn, cursor.clone(), page_size)
                .await
                .unwrap();
            assert!(page.len() <= page_size, "page respects the limit");
            let Some(last) = page.last() else { break };
            cursor = Some((last.last_message_at, last.key.contact.clone()));
            for s in page {
                assert_eq!(s.key.phone_number_id, r.pn, "scoped to one business number");
                seen.push((s.last_message_at, s.key.contact));
            }
        }
        assert_eq!(
            seen, expected,
            "conversation paging by {page_size}: (last_message_at, contact) desc, no repeats, no skips"
        );
    }
}

async fn summary_tracks_latest_message<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("summary");
    let key = r.key("c");
    store
        .append(r.msg("c", "m5", Direction::Outbound, 5, "later"))
        .await
        .unwrap();
    store
        .append(r.msg("c", "m1", Direction::Inbound, 1, "earlier, arrived late"))
        .await
        .unwrap();
    let s = summary(store, &key).await.unwrap();
    assert_eq!(s.last_message_at, at(5), "newest by timestamp, not arrival");
    assert_eq!(s.last_text.as_deref(), Some("later"));
    assert_eq!(s.last_inbound_at, Some(at(1)));

    // Same timestamp: the larger id is the newer message, as in the history.
    store
        .append(r.msg("c", "m6", Direction::Outbound, 5, "tie, larger id"))
        .await
        .unwrap();
    store
        .append(r.msg("c", "m0", Direction::Outbound, 5, "tie, smaller id"))
        .await
        .unwrap();
    let s = summary(store, &key).await.unwrap();
    assert_eq!(s.last_text.as_deref(), Some("tie, larger id"));
    let newest = store.messages(&key, None, 1).await.unwrap();
    assert_eq!(
        newest[0].text, s.last_text,
        "summary preview is the first history row"
    );

    let no_text = StoredMessage {
        kind: "image".to_owned(),
        text: None,
        ..r.msg("c", "m9", Direction::Inbound, 9, "")
    };
    store.append(no_text).await.unwrap();
    let s = summary(store, &key).await.unwrap();
    assert_eq!(s.last_message_at, at(9));
    assert_eq!(
        s.last_text, None,
        "preview of a message without text is none"
    );
}

async fn unread_counting<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("unread");
    let key = r.key("c");
    for (local, secs) in [("i1", 1), ("i2", 2), ("i3", 3)] {
        store
            .append(r.msg("c", local, Direction::Inbound, secs, local))
            .await
            .unwrap();
    }
    store
        .append(r.msg("c", "o4", Direction::Outbound, 4, "reply"))
        .await
        .unwrap();
    store
        .append(r.msg("c", "i2", Direction::Inbound, 2, "replayed"))
        .await
        .unwrap();
    assert_eq!(
        summary(store, &key).await.unwrap().unread,
        3,
        "inbound only, replays excluded"
    );

    store.mark_read(&key).await.unwrap();
    assert_eq!(
        summary(store, &key).await.unwrap().unread,
        0,
        "mark_read resets"
    );

    // A late webhook for an older message still counts: the merchant has
    // not seen it.
    store
        .append(r.msg("c", "i0", Direction::Inbound, 0, "late"))
        .await
        .unwrap();
    assert_eq!(summary(store, &key).await.unwrap().unread, 1);

    store
        .mark_read(&r.key("never-seen"))
        .await
        .expect("mark_read of an unknown conversation is a no-op");
}

async fn last_inbound_at<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("window");
    assert_eq!(
        store.last_inbound_at(&r.key("nobody")).await.unwrap(),
        None,
        "unknown conversation"
    );

    store
        .append(r.msg("out", "o1", Direction::Outbound, 1, "template"))
        .await
        .unwrap();
    assert_eq!(
        store.last_inbound_at(&r.key("out")).await.unwrap(),
        None,
        "outbound messages never open the window"
    );

    let key = r.key("c");
    store
        .append(r.msg("c", "i3", Direction::Inbound, 3, "hi"))
        .await
        .unwrap();
    store
        .append(r.msg("c", "i1", Direction::Inbound, 1, "late"))
        .await
        .unwrap();
    store
        .append(r.msg("c", "o9", Direction::Outbound, 9, "answer"))
        .await
        .unwrap();
    assert_eq!(
        store.last_inbound_at(&key).await.unwrap(),
        Some(at(3)),
        "latest inbound by timestamp; late and outbound messages do not move it"
    );
    assert_eq!(
        summary(store, &key).await.unwrap().last_inbound_at,
        Some(at(3))
    );
}

async fn zero_limit<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("zero");
    store
        .append(r.msg("c", "1", Direction::Inbound, 0, "x"))
        .await
        .unwrap();
    assert!(
        store
            .messages(&r.key("c"), None, 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .conversations(&r.pn, None, 0)
            .await
            .unwrap()
            .is_empty()
    );
}

async fn concurrent_appends<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("race");
    let key = r.key("c");
    let same = (0..16).map(|i| {
        let m = r.msg("c", "dup", Direction::Inbound, 0, &format!("copy {i}"));
        store.append(m)
    });
    let wins = futures::future::join_all(same)
        .await
        .into_iter()
        .filter(|r| *r.as_ref().unwrap())
        .count();
    assert_eq!(wins, 1, "exactly one concurrent append of an id wins");

    let distinct = (0..16).map(|i| {
        let m = r.msg("c", &format!("m{i}"), Direction::Inbound, i, "x");
        store.append(m)
    });
    for appended in futures::future::join_all(distinct).await {
        assert!(appended.unwrap());
    }
    assert_eq!(store.messages(&key, None, 100).await.unwrap().len(), 17);
    let s = summary(store, &key).await.unwrap();
    assert_eq!(s.unread, 17, "no concurrent append is lost from the count");
    assert_eq!(s.last_message_at, at(15));
    assert_eq!(s.last_inbound_at, Some(at(15)));
}

async fn concurrent_status_updates<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("status-race");
    let m = r.msg("c", "1", Direction::Outbound, 0, "x");
    let id = m.id.clone();
    store.append(m).await.unwrap();
    let statuses = [
        DeliveryStatus::Sent,
        DeliveryStatus::Delivered,
        DeliveryStatus::Read,
        DeliveryStatus::Delivered,
        DeliveryStatus::Sent,
        DeliveryStatus::Read,
        DeliveryStatus::Sent,
        DeliveryStatus::Delivered,
    ];
    let updates = statuses
        .iter()
        .enumerate()
        .map(|(i, s)| store.update_status(&id, *s, at(i64::try_from(i).unwrap() + 1), None));
    let applied = futures::future::join_all(updates)
        .await
        .into_iter()
        .filter(|r| *r.as_ref().unwrap())
        .count();
    assert!(
        (1..=3).contains(&applied),
        "at most one update per rank can apply, got {applied}"
    );
    assert_eq!(
        only_message(store, &r.key("c")).await.status,
        DeliveryStatus::Read,
        "concurrent updates end on the highest status"
    );
}
