#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)] // test helper: panics are the report
//! The executable `ConversationStore` contract.
//!
//! ```ignore
//! meta_whatsapp_adapters::store::conversation_conformance::run(&my_store).await;
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
//! - an applied status update with `error: None` keeps a stored error;
//! - a status update is scoped to its business number: the same message id
//!   on another number changes nothing;
//! - history is `(timestamp, id)` descending in **byte order of the id**
//!   (the order of Rust's `str`), and paging with the exclusive cursor
//!   neither repeats nor skips rows, even when every timestamp collides;
//! - the conversation list is `(last_message_at, contact)` descending with
//!   the same cursor rule, scoped to one business number;
//! - the inbox summary follows the newest message by `(timestamp, id)`, not
//!   the most recently appended one;
//! - unread counts inbound messages appended since the last `mark_read`, by
//!   arrival, and concurrent appends are all counted;
//! - synced history (`append_synced`) is stored and ordered like any
//!   message and moves the conversation's latest message, but never moves
//!   `last_inbound_at` (the customer service window) nor the unread count,
//!   even interleaved with concurrent live appends; the id rule spans
//!   `append` and `append_synced`;
//! - `append_synced` takes a batch and answers per message: an empty batch
//!   is a no-op, a duplicate within the batch or an id already stored is
//!   `false` and changes nothing, and each conversation's summary follows
//!   its newest message across the batch, over hundreds of messages;
//! - a revoke deletes only a message of its business number and of its
//!   direction, never regresses a terminal status, and when its message is
//!   not stored yet leaves a tombstone (kind `revoked`, no content,
//!   `Deleted`) that keeps the message's content out when it arrives, live
//!   or synced;
//! - a revoke does not match its conversation: keyed by another
//!   conversation of the same number, it marks the message `Deleted` all
//!   the same, and leaves no tombstone under its own key (the owner's
//!   decision, 2026-09-25); a revoked message keeps its text and payload,
//!   for the merchant's records (decided the same day);
//! - a tombstone is history but never part of the summary: in an existing
//!   conversation the summary stays exactly as it was (latest message,
//!   preview, window, unread), and a conversation with nothing but a
//!   tombstone is not listed;
//! - `fill_media_placeholder` rewrites the `kind`, `text` and `payload` of a
//!   stored media placeholder of its business number, once, and nothing
//!   else: not another number's row, not a message that is not (or no
//!   longer) a placeholder, not a placeholder that was revoked, not the
//!   window or the unread count; the conversation preview follows when the
//!   placeholder is the latest message.
//! - content is stored exactly, U+0000 included (the owner's decision of
//!   2026-09-25: losslessly): `kind`, `text`, payload strings and object keys,
//!   the status `error` and the conversation preview read back as written
//!   through every write method; a NUL never reads back as U+FFFD, two
//!   object keys differing only by one stay distinct, and a `kind` one NUL
//!   away from `StoredMessage::MEDIA_PLACEHOLDER` is not a placeholder.
//!   (Identifiers are not covered: Meta never assigns one with U+0000, and
//!   the Postgres store refuses it there.)

use time::OffsetDateTime;
use time::macros::datetime;
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::ids::{MessageId, PhoneNumberId};
use meta_whatsapp_core::store::{
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
    stored_error_survives_updates_without_one(store).await;
    unknown_message_status_update_is_false(store).await;
    status_for_the_same_id_on_another_number_changes_nothing(store).await;
    history_order_and_exclusive_cursor(store).await;
    paging_with_identical_timestamps(store).await;
    conversation_list_paging(store).await;
    summary_tracks_latest_message(store).await;
    unread_counting(store).await;
    last_inbound_at(store).await;
    zero_limit(store).await;
    concurrent_appends(store).await;
    concurrent_status_updates(store).await;
    synced_history_opens_no_window_and_is_never_unread(store).await;
    concurrent_live_and_synced_appends(store).await;
    a_media_placeholder_is_filled_once(store).await;
    synced_batches_answer_per_message(store).await;
    a_revoke_matches_its_number_and_direction(store).await;
    a_revoke_does_not_match_its_conversation(store).await;
    a_revoke_before_its_message_leaves_a_tombstone(store).await;
    a_tombstone_leaves_the_summary_alone(store).await;
    a_revoked_placeholder_is_never_filled(store).await;
    content_keeps_nul(store).await;
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

/// `append_synced` of one message.
async fn synced<S: ConversationStore + ?Sized>(
    store: &S,
    message: StoredMessage,
) -> Result<bool, StorageError> {
    let inserted = store.append_synced(vec![message]).await?;
    assert_eq!(inserted.len(), 1, "append_synced answers once per message");
    Ok(inserted[0])
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

    let update = |status, secs| store.update_status(&r.pn, &id, status, at(secs), None);
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
            .update_status(&r.pn, &failed_id, DeliveryStatus::Sent, at(1), None)
            .await
            .unwrap()
    );
    assert!(
        store
            .update_status(
                &r.pn,
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
                .update_status(&r.pn, &failed_id, late, at(3), None)
                .await
                .unwrap(),
            "{late:?} never overrides failed"
        );
    }

    assert!(
        store
            .update_status(&r.pn, &deleted_id, DeliveryStatus::Read, at(2), None)
            .await
            .unwrap()
    );
    assert!(
        store
            .update_status(&r.pn, &deleted_id, DeliveryStatus::Deleted, at(3), None)
            .await
            .unwrap(),
        "deleted supersedes read"
    );
    assert!(
        !store
            .update_status(&r.pn, &deleted_id, DeliveryStatus::Failed, at(4), None)
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

/// `update_status` with `error: None` keeps a stored error; one that brings
/// an error replaces it; one that does not apply touches nothing.
async fn stored_error_survives_updates_without_one<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("error-keep");
    let m = r.msg("c", "1", Direction::Outbound, 0, "promo");
    let id = m.id.clone();
    store.append(m).await.unwrap();

    let first = serde_json::json!({"code": 131026, "title": "Message undeliverable"});
    assert!(
        store
            .update_status(&r.pn, &id, DeliveryStatus::Sent, at(1), Some(first.clone()))
            .await
            .unwrap()
    );
    assert!(
        store
            .update_status(&r.pn, &id, DeliveryStatus::Delivered, at(2), None)
            .await
            .unwrap()
    );
    let stored = only_message(store, &r.key("c")).await;
    assert_eq!(
        (stored.status, stored.error.as_ref()),
        (DeliveryStatus::Delivered, Some(&first)),
        "an applied update without an error keeps the stored one"
    );

    let second = serde_json::json!({"code": 131049, "title": "Per-user marketing limit"});
    assert!(
        store
            .update_status(
                &r.pn,
                &id,
                DeliveryStatus::Failed,
                at(3),
                Some(second.clone())
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .update_status(
                &r.pn,
                &id,
                DeliveryStatus::Read,
                at(4),
                Some(serde_json::json!({"code": 1}))
            )
            .await
            .unwrap()
    );
    let stored = only_message(store, &r.key("c")).await;
    assert_eq!(
        (stored.status, stored.error, stored.status_at),
        (DeliveryStatus::Failed, Some(second), Some(at(3))),
        "a new error replaces the old one; an update that does not apply changes nothing"
    );
}

async fn unknown_message_status_update_is_false<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("unknown");
    assert!(
        !store
            .update_status(
                &r.pn,
                &r.id("never-appended"),
                DeliveryStatus::Read,
                at(0),
                None
            )
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

/// A status (or revoke) delivered for one business number never changes a
/// message of another, even under the same id: the update is scoped to
/// `phone_number_id`, not matched on the id alone.
async fn status_for_the_same_id_on_another_number_changes_nothing<S: ConversationStore + ?Sized>(
    store: &S,
) {
    let r = Run::new("cross-number");
    let other = Run::new("cross-number-other");
    let m = r.msg("c", "1", Direction::Outbound, 0, "order shipped");
    let id = m.id.clone();
    store.append(m.clone()).await.unwrap();
    let error = serde_json::json!({"code": 131026, "title": "Message undeliverable"});
    for status in [
        DeliveryStatus::Sent,
        DeliveryStatus::Read,
        DeliveryStatus::Failed,
        DeliveryStatus::Deleted,
    ] {
        assert!(
            !store
                .update_status(&other.pn, &id, status, at(1), Some(error.clone()))
                .await
                .unwrap(),
            "{status:?} for the same id on another number was applied"
        );
    }
    assert_eq!(
        only_message(store, &r.key("c")).await,
        m,
        "a status for the same id on another number changes nothing"
    );
    assert!(
        store
            .update_status(&r.pn, &id, DeliveryStatus::Read, at(2), None)
            .await
            .unwrap(),
        "the message's own number still updates it"
    );
    assert_eq!(
        only_message(store, &r.key("c")).await.status,
        DeliveryStatus::Read
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
        .map(|(i, s)| store.update_status(&r.pn, &id, *s, at(i64::try_from(i).unwrap() + 1), None));
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

/// Synced history (`append_synced`: messages from before the business was
/// onboarded, which Meta opens no customer service window for and the
/// merchant has read in the app) is history like any other message, but
/// never moves `last_inbound_at` nor the unread count. An adapter that
/// treats it like `append` fails here.
async fn synced_history_opens_no_window_and_is_never_unread<S: ConversationStore + ?Sized>(
    store: &S,
) {
    let r = Run::new("synced");
    let key = r.key("c");
    let first = r.msg("c", "s5", Direction::Inbound, 5, "from the app");
    assert!(
        synced(store, first.clone()).await.unwrap(),
        "first synced append"
    );
    assert_eq!(
        only_message(store, &key).await,
        first,
        "a synced message is stored verbatim"
    );
    let s = summary(store, &key)
        .await
        .expect("a synced message creates its conversation");
    assert_eq!(
        (s.last_inbound_at, s.unread),
        (None, 0),
        "a synced inbound message opens no window and is not unread"
    );
    assert_eq!(
        (s.last_message_at, s.last_text.as_deref()),
        (at(5), Some("from the app")),
        "but it is the conversation's latest message"
    );
    assert_eq!(store.last_inbound_at(&key).await.unwrap(), None);

    // A live inbound message, older: the window and the count follow it
    // alone, and a newer synced one moves neither.
    assert!(
        store
            .append(r.msg("c", "l1", Direction::Inbound, 1, "live"))
            .await
            .unwrap()
    );
    assert!(
        synced(
            store,
            r.msg("c", "s9", Direction::Inbound, 9, "newer, synced")
        )
        .await
        .unwrap()
    );
    let s = summary(store, &key).await.unwrap();
    assert_eq!(
        (s.last_inbound_at, s.unread),
        (Some(at(1)), 1),
        "only the live message counts"
    );
    assert_eq!(
        (s.last_message_at, s.last_text.as_deref()),
        (at(9), Some("newer, synced"))
    );
    assert_eq!(store.last_inbound_at(&key).await.unwrap(), Some(at(1)));

    // One id rule across both methods: a replay by either changes nothing.
    assert!(
        !synced(
            store,
            r.msg("c", "l1", Direction::Inbound, 1, "again, synced")
        )
        .await
        .unwrap(),
        "a live message replayed as synced"
    );
    assert!(
        !store
            .append(r.msg("c", "s9", Direction::Inbound, 9, "again, live"))
            .await
            .unwrap(),
        "a synced message replayed live"
    );
    assert!(
        !synced(store, r.msg("c", "s5", Direction::Inbound, 5, "again"))
            .await
            .unwrap(),
        "a synced message replayed as synced"
    );
    let s = summary(store, &key).await.unwrap();
    assert_eq!(
        (s.last_inbound_at, s.unread),
        (Some(at(1)), 1),
        "replays change neither the window nor the count"
    );
    let texts: Vec<_> = store
        .messages(&key, None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.text.unwrap_or_default())
        .collect();
    assert_eq!(texts, ["newer, synced", "from the app", "live"]);

    // After mark_read, synced messages still add nothing.
    store.mark_read(&key).await.unwrap();
    synced(
        store,
        r.msg("c", "s20", Direction::Inbound, 20, "latest, synced"),
    )
    .await
    .unwrap();
    let s = summary(store, &key).await.unwrap();
    assert_eq!((s.last_inbound_at, s.unread), (Some(at(1)), 0));
    assert_eq!(s.last_message_at, at(20));
}

/// Live and synced appends racing on one conversation: every live inbound
/// message is counted and moves the window, no synced one does.
async fn concurrent_live_and_synced_appends<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("synced-race");
    let key = r.key("c");
    let live = (0..8).map(|i| {
        let m = r.msg("c", &format!("live{i}"), Direction::Inbound, 2 * i, "live");
        store.append(m)
    });
    // Four batches of two synced messages each.
    let batches = (0..4).map(|b| {
        let batch = (0..2)
            .map(|j| {
                let i = 2 * b + j;
                let at = 2 * i + 1;
                r.msg("c", &format!("synced{i}"), Direction::Inbound, at, "synced")
            })
            .collect();
        store.append_synced(batch)
    });
    let (live, batches) = futures::future::join(
        futures::future::join_all(live),
        futures::future::join_all(batches),
    )
    .await;
    for appended in live {
        assert!(appended.unwrap());
    }
    for batch in batches {
        assert_eq!(batch.unwrap(), [true, true]);
    }
    assert_eq!(store.messages(&key, None, 100).await.unwrap().len(), 16);
    let s = summary(store, &key).await.unwrap();
    assert_eq!(s.unread, 8, "only live messages are unread");
    assert_eq!(
        s.last_inbound_at,
        Some(at(14)),
        "the window follows the latest live message"
    );
    assert_eq!(s.last_message_at, at(15), "a synced message is the latest");
}

/// `fill_media_placeholder` rewrites a stored placeholder's content once,
/// on its own business number, and nothing else.
async fn a_media_placeholder_is_filled_once<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("placeholder");
    let other = Run::new("placeholder-other");
    let key = r.key("c");
    let placeholder = StoredMessage {
        kind: StoredMessage::MEDIA_PLACEHOLDER.to_owned(),
        text: None,
        payload: serde_json::json!({
            "type": "media_placeholder",
            "history_context": {"status": "PLAYED"}
        }),
        status: DeliveryStatus::Played,
        status_at: Some(at(4)),
        ..r.msg("c", "p3", Direction::Outbound, 3, "")
    };
    assert!(synced(store, placeholder.clone()).await.unwrap());
    let content = serde_json::json!({
        "type": "image",
        "image": {"id": "24230790383178626", "caption": "Black Prince echeveria"}
    });
    let fill = |pn, id, caption: &str| {
        store.fill_media_placeholder(
            pn,
            id,
            "image".to_owned(),
            Some(caption.to_owned()),
            content.clone(),
        )
    };
    assert!(
        !fill(&other.pn, &placeholder.id, "wrong number")
            .await
            .unwrap(),
        "a placeholder is only filled on its own business number"
    );
    let unknown = r.id("never-appended");
    assert!(
        !fill(&r.pn, &unknown, "nobody").await.unwrap(),
        "an unknown id fills nothing"
    );
    assert_eq!(only_message(store, &key).await, placeholder);

    assert!(
        fill(&r.pn, &placeholder.id, "Black Prince echeveria")
            .await
            .unwrap(),
        "the placeholder is filled"
    );
    let filled = StoredMessage {
        kind: "image".to_owned(),
        text: Some("Black Prince echeveria".to_owned()),
        payload: content.clone(),
        ..placeholder.clone()
    };
    assert_eq!(
        only_message(store, &key).await,
        filled,
        "only kind, text and payload change"
    );
    assert_eq!(
        summary(store, &key).await.unwrap().last_text.as_deref(),
        Some("Black Prince echeveria"),
        "the preview of the latest message follows"
    );
    assert!(
        !fill(&r.pn, &placeholder.id, "redelivered").await.unwrap(),
        "a placeholder is filled once"
    );
    assert_eq!(only_message(store, &key).await, filled);

    // A message that never was a placeholder is never rewritten, and a
    // placeholder that is not the latest message leaves the preview (and
    // the window, and the count) alone.
    let live = r.msg("c", "t1", Direction::Inbound, 1, "hello");
    assert!(store.append(live.clone()).await.unwrap());
    assert!(!fill(&r.pn, &live.id, "not a placeholder").await.unwrap());
    let older = StoredMessage {
        kind: StoredMessage::MEDIA_PLACEHOLDER.to_owned(),
        text: None,
        ..r.msg("c", "p0", Direction::Inbound, 0, "")
    };
    assert!(synced(store, older.clone()).await.unwrap());
    assert!(fill(&r.pn, &older.id, "older photo").await.unwrap());
    let history = store.messages(&key, None, 10).await.unwrap();
    let find = |id: &MessageId| history.iter().find(|m| &m.id == id).unwrap().clone();
    assert_eq!(find(&live.id), live, "not a placeholder: unchanged");
    assert_eq!(find(&older.id).text.as_deref(), Some("older photo"));
    let s = summary(store, &key).await.unwrap();
    assert_eq!(
        s.last_text.as_deref(),
        Some("Black Prince echeveria"),
        "the preview stays the latest message's"
    );
    assert_eq!(
        (s.last_inbound_at, s.unread),
        (Some(at(1)), 1),
        "filling moves neither the window nor the count"
    );
}

/// `append_synced` is a batch: one answer per message, the first of an id
/// wins, and each conversation's summary follows its newest message.
async fn synced_batches_answer_per_message<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("synced-batch");
    assert!(
        store.append_synced(Vec::new()).await.unwrap().is_empty(),
        "an empty batch"
    );
    assert!(
        store
            .conversations(&r.pn, None, 10)
            .await
            .unwrap()
            .is_empty(),
        "an empty batch creates nothing"
    );
    let known = r.msg("a", "known", Direction::Outbound, 0, "stored before");
    assert!(store.append(known.clone()).await.unwrap());
    let first = r.msg("a", "dup", Direction::Inbound, 3, "first copy");
    let batch = vec![
        r.msg("b", "b1", Direction::Outbound, 7, "b newest"),
        first.clone(),
        r.msg("a", "known", Direction::Inbound, 9, "replayed, newer"),
        r.msg("a", "dup", Direction::Inbound, 4, "second copy"),
        r.msg("a", "a2", Direction::Inbound, 2, "a older"),
        r.msg("b", "b0", Direction::Inbound, 5, "b older"),
    ];
    assert_eq!(
        store.append_synced(batch).await.unwrap(),
        [true, true, false, false, true, true],
        "one answer per message, in order"
    );
    let history = store.messages(&r.key("a"), None, 10).await.unwrap();
    let texts: Vec<_> = history.iter().map(|m| m.text.clone().unwrap()).collect();
    assert_eq!(texts, ["first copy", "a older", "stored before"]);
    assert_eq!(
        history.iter().find(|m| m.id == first.id),
        Some(&first),
        "the first copy of an id is stored verbatim"
    );
    let a = summary(store, &r.key("a")).await.unwrap();
    assert_eq!(
        (
            a.last_message_at,
            a.last_text.as_deref(),
            a.last_inbound_at,
            a.unread
        ),
        (at(3), Some("first copy"), None, 0),
        "the replayed id changed nothing, the batch opened no window"
    );
    let b = summary(store, &r.key("b")).await.unwrap();
    assert_eq!(
        (b.last_message_at, b.last_text.as_deref(), b.unread),
        (at(7), Some("b newest"), 0)
    );

    // A history sync can be thousands of messages in one webhook.
    let big = Run::new("synced-big");
    let messages: Vec<_> = (0..600)
        .map(|i| {
            let contact = format!("c{}", i % 3);
            let direction = if i % 2 == 0 {
                Direction::Inbound
            } else {
                Direction::Outbound
            };
            big.msg(&contact, &format!("{i:04}"), direction, i, &format!("m{i}"))
        })
        .collect();
    let answers = store.append_synced(messages).await.unwrap();
    assert_eq!(answers.len(), 600);
    assert!(answers.iter().all(|inserted| *inserted));
    for c in 0..3_i64 {
        let key = big.key(&format!("c{c}"));
        assert_eq!(store.messages(&key, None, 1000).await.unwrap().len(), 200);
        let s = summary(store, &key).await.unwrap();
        let newest = 597 + c;
        assert_eq!(
            (s.last_message_at, s.last_text, s.last_inbound_at, s.unread),
            (at(newest), Some(format!("m{newest}")), None, 0)
        );
    }
}

/// A revoke deletes a message of its own business number and direction
/// only: a customer revokes what they sent, the business what it sent.
async fn a_revoke_matches_its_number_and_direction<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("revoke");
    let other = Run::new("revoke-other");
    let key = r.key("c");
    let inbound = r.msg("c", "in", Direction::Inbound, 1, "from the customer");
    let outbound = r.msg("c", "out", Direction::Outbound, 2, "from the business");
    store.append(inbound.clone()).await.unwrap();
    store.append(outbound.clone()).await.unwrap();

    assert!(
        !store
            .revoke(&key, &inbound.id, Direction::Outbound, at(5))
            .await
            .unwrap(),
        "the business cannot revoke the customer's message"
    );
    assert!(
        !store
            .revoke(&key, &outbound.id, Direction::Inbound, at(5))
            .await
            .unwrap(),
        "the customer cannot revoke the business's message"
    );
    assert!(
        !store
            .revoke(&other.key("c"), &inbound.id, Direction::Inbound, at(5))
            .await
            .unwrap(),
        "a revoke on another number changes nothing"
    );
    assert!(
        store
            .messages(&other.key("c"), None, 10)
            .await
            .unwrap()
            .is_empty(),
        "and leaves no tombstone there: the id is taken"
    );
    let history = store.messages(&key, None, 10).await.unwrap();
    assert_eq!(
        history,
        [outbound.clone(), inbound.clone()],
        "nothing changed"
    );

    assert!(
        store
            .revoke(&key, &inbound.id, Direction::Inbound, at(6))
            .await
            .unwrap()
    );
    assert!(
        store
            .revoke(&key, &outbound.id, Direction::Outbound, at(7))
            .await
            .unwrap()
    );
    let history = store.messages(&key, None, 10).await.unwrap();
    let find = |id: &MessageId| history.iter().find(|m| &m.id == id).unwrap().clone();
    assert_eq!(
        find(&inbound.id),
        StoredMessage {
            status: DeliveryStatus::Deleted,
            status_at: Some(at(6)),
            ..inbound.clone()
        }
    );
    assert_eq!(find(&outbound.id).status, DeliveryStatus::Deleted);
    assert!(
        !store
            .revoke(&key, &inbound.id, Direction::Inbound, at(8))
            .await
            .unwrap(),
        "a replayed revoke is a no-op"
    );

    // Terminal statuses do not supersede each other.
    let failed = r.msg("c", "failed", Direction::Outbound, 3, "promo");
    store.append(failed.clone()).await.unwrap();
    store
        .update_status(&r.pn, &failed.id, DeliveryStatus::Failed, at(4), None)
        .await
        .unwrap();
    assert!(
        !store
            .revoke(&key, &failed.id, Direction::Outbound, at(9))
            .await
            .unwrap()
    );
    let s = summary(store, &key).await.unwrap();
    assert_eq!(
        (s.last_inbound_at, s.unread),
        (Some(at(1)), 1),
        "revokes move neither the window nor the count"
    );
}

/// A revoke matches its business number and direction, not its
/// conversation (the owner's decision, 2026-09-25): the same customer's
/// message can be stored under one conversation key (a history thread keyed
/// by phone number, say) and revoked under another (a live revoke keyed by
/// BSUID, or a BSUID that changed). The message becomes `Deleted` with its
/// content kept, and the revoke's own conversation gets no tombstone: the
/// id is taken.
async fn a_revoke_does_not_match_its_conversation<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("revoke-across");
    let stored_under = r.key("keyed-by-phone");
    let revoked_under = r.key("keyed-by-user-id");
    let inbound = r.msg(
        "keyed-by-phone",
        "in",
        Direction::Inbound,
        1,
        "from the customer",
    );
    let outbound = r.msg(
        "keyed-by-phone",
        "out",
        Direction::Outbound,
        2,
        "from the business",
    );
    store.append(inbound.clone()).await.unwrap();
    store.append(outbound.clone()).await.unwrap();

    assert!(
        store
            .revoke(&revoked_under, &inbound.id, Direction::Inbound, at(5))
            .await
            .unwrap(),
        "a revoke keyed by another conversation of the number deletes the message"
    );
    assert!(
        store
            .revoke(&revoked_under, &outbound.id, Direction::Outbound, at(6))
            .await
            .unwrap()
    );
    assert_eq!(
        store.messages(&stored_under, None, 10).await.unwrap(),
        [
            StoredMessage {
                status: DeliveryStatus::Deleted,
                status_at: Some(at(6)),
                ..outbound.clone()
            },
            StoredMessage {
                status: DeliveryStatus::Deleted,
                status_at: Some(at(5)),
                ..inbound.clone()
            },
        ],
        "deleted where they are stored, text and payload kept"
    );
    assert!(
        store
            .messages(&revoked_under, None, 10)
            .await
            .unwrap()
            .is_empty(),
        "no tombstone under the revoke's key: the id is taken"
    );

    // Still scoped to the direction across conversations.
    let later = r.msg("keyed-by-phone", "later", Direction::Inbound, 3, "again");
    store.append(later.clone()).await.unwrap();
    assert!(
        !store
            .revoke(&revoked_under, &later.id, Direction::Outbound, at(7))
            .await
            .unwrap(),
        "the business cannot revoke the customer's message from another conversation"
    );
    assert_eq!(
        only_status(store, &stored_under, &later.id).await,
        DeliveryStatus::Received
    );
}

async fn only_status<S: ConversationStore + ?Sized>(
    store: &S,
    key: &ConversationKey,
    id: &MessageId,
) -> DeliveryStatus {
    store
        .messages(key, None, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|m| &m.id == id)
        .unwrap()
        .status
}

/// A revoke that arrives before its message leaves a tombstone, and the
/// message, live or synced, is then never stored with its content.
async fn a_revoke_before_its_message_leaves_a_tombstone<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("tombstone");
    let key = r.key("c");
    let deleted = r.msg("c", "gone", Direction::Inbound, 1, "my card number is 4111");
    assert!(
        store
            .revoke(&key, &deleted.id, Direction::Inbound, at(30))
            .await
            .unwrap(),
        "an unmatched revoke stores its tombstone"
    );
    let tombstone = StoredMessage::tombstone(&key, &deleted.id, Direction::Inbound, at(30));
    assert_eq!(tombstone.kind, StoredMessage::REVOKED);
    assert_eq!(
        tombstone.payload,
        serde_json::json!({}),
        "a tombstone's payload is an empty object"
    );
    assert_eq!(only_message(store, &key).await, tombstone);
    assert_eq!(
        store.last_inbound_at(&key).await.unwrap(),
        None,
        "a tombstone opens no window"
    );

    assert!(
        !store.append(deleted.clone()).await.unwrap(),
        "the message arrives live after its revoke"
    );
    assert!(
        !synced(store, deleted.clone()).await.unwrap(),
        "or in the history"
    );
    assert_eq!(
        only_message(store, &key).await,
        tombstone,
        "its content is never stored"
    );
    assert_eq!(
        summary(store, &key).await,
        None,
        "nor does it create the summary the tombstone did not"
    );
    assert_eq!(store.last_inbound_at(&key).await.unwrap(), None);
    assert!(
        !store
            .revoke(&key, &deleted.id, Direction::Inbound, at(31))
            .await
            .unwrap(),
        "a replayed revoke changes nothing"
    );
    let outbound = r.id("gone-out");
    assert!(
        store
            .revoke(&r.key("d"), &outbound, Direction::Outbound, at(40))
            .await
            .unwrap()
    );
    assert_eq!(
        only_message(store, &r.key("d")).await,
        StoredMessage::tombstone(&r.key("d"), &outbound, Direction::Outbound, at(40))
    );
}

/// A tombstone is part of the history, never of the inbox summary: it
/// cannot tell the merchant anything (no text, no content) and is not
/// activity of the customer's at the time it carries.
async fn a_tombstone_leaves_the_summary_alone<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("tombstone-summary");

    // A conversation with nothing but a tombstone has no summary.
    let fresh = r.key("fresh");
    let early = r.id("early");
    assert!(
        store
            .revoke(&fresh, &early, Direction::Inbound, at(50))
            .await
            .unwrap()
    );
    assert_eq!(
        only_message(store, &fresh).await,
        StoredMessage::tombstone(&fresh, &early, Direction::Inbound, at(50)),
        "the tombstone is in the history"
    );
    assert_eq!(
        summary(store, &fresh).await,
        None,
        "a conversation with only a tombstone is not listed"
    );
    assert_eq!(store.last_inbound_at(&fresh).await.unwrap(), None);
    // Its first message, older than the tombstone, is its latest.
    let first = r.msg("fresh", "first", Direction::Inbound, 10, "hello");
    assert!(store.append(first).await.unwrap());
    let s = summary(store, &fresh)
        .await
        .expect("the message creates it");
    assert_eq!(
        (
            s.last_message_at,
            s.last_text.as_deref(),
            s.last_inbound_at,
            s.unread
        ),
        (at(10), Some("hello"), Some(at(10)), 1),
        "the tombstone, newer, is not the latest message"
    );

    // In an existing conversation, the summary stays exactly as it was,
    // for a tombstone of either direction, newer than every message.
    let key = r.key("known");
    store
        .append(r.msg("known", "in", Direction::Inbound, 1, "where is my order?"))
        .await
        .unwrap();
    synced(
        store,
        r.msg("known", "out", Direction::Outbound, 2, "on its way"),
    )
    .await
    .unwrap();
    store
        .append(r.msg("other", "x", Direction::Inbound, 5, "another customer"))
        .await
        .unwrap();
    let before = summary(store, &key).await.unwrap();
    assert_eq!(
        (before.last_message_at, before.last_text.as_deref()),
        (at(2), Some("on its way"))
    );
    for (local, direction, secs) in [
        ("gone-in", Direction::Inbound, 40),
        ("gone-out", Direction::Outbound, 41),
    ] {
        assert!(
            store
                .revoke(&key, &r.id(local), direction, at(secs))
                .await
                .unwrap()
        );
    }
    assert_eq!(
        summary(store, &key).await,
        Some(before),
        "a tombstone moves nothing in the summary"
    );
    assert_eq!(store.messages(&key, None, 10).await.unwrap().len(), 4);
    let order: Vec<String> = store
        .conversations(&r.pn, None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.key.contact)
        .collect();
    assert_eq!(
        order,
        ["fresh", "other", "known"],
        "nor its place in the list"
    );
}

/// A placeholder revoked before its content arrives is never filled: the
/// content is what its sender deleted.
async fn a_revoked_placeholder_is_never_filled<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("revoked-placeholder");
    let key = r.key("c");
    let placeholder = StoredMessage {
        kind: StoredMessage::MEDIA_PLACEHOLDER.to_owned(),
        text: None,
        payload: serde_json::json!({"type": "media_placeholder"}),
        status: DeliveryStatus::Received,
        ..r.msg("c", "p", Direction::Inbound, 3, "")
    };
    assert!(synced(store, placeholder.clone()).await.unwrap());
    assert!(
        store
            .revoke(&key, &placeholder.id, Direction::Inbound, at(5))
            .await
            .unwrap(),
        "the live revoke deletes the placeholder"
    );
    let deleted = StoredMessage {
        status: DeliveryStatus::Deleted,
        status_at: Some(at(5)),
        ..placeholder
    };
    assert_eq!(only_message(store, &key).await, deleted);
    assert!(
        !store
            .fill_media_placeholder(
                &r.pn,
                &deleted.id,
                "image".to_owned(),
                Some("deleted caption".to_owned()),
                serde_json::json!({"type": "image", "image": {"caption": "deleted caption"}}),
            )
            .await
            .unwrap(),
        "a revoked placeholder is never filled"
    );
    assert_eq!(
        only_message(store, &key).await,
        deleted,
        "its content stays out"
    );
    assert_eq!(
        summary(store, &key).await.unwrap().last_text,
        None,
        "and out of the preview"
    );
}

/// Content round-trips exactly, U+0000 included (the owner's decision of
/// 2026-09-25: stored losslessly): `kind`, `text`, payload
/// strings and object keys, the status `error`, and the preview, through
/// `append`, `append_synced`, `update_status` and `fill_media_placeholder`.
/// A NUL never reads back as U+FFFD, and two object keys differing only by
/// one stay two keys.
async fn content_keeps_nul<S: ConversationStore + ?Sized>(store: &S) {
    appended_content_keeps_nul(store).await;
    synced_content_keeps_nul(store).await;
    a_filled_placeholder_keeps_nul(store).await;
}

/// `append` and `update_status`: see [`content_keeps_nul`].
async fn appended_content_keeps_nul<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("nul");
    let key = r.key("c");
    let payload = serde_json::json!({
        "type": "te\0xt",
        "text": {"body": "order\u{0}42"},
        "k\0": "the NUL key",
        "k\u{FFFD}": "the U+FFFD key",
        "\0": ["\0", {"deep\0": "b\0"}, "\0\0"],
        "fffd": "\u{FFFD}"
    });
    let live = StoredMessage {
        kind: "te\0xt".to_owned(),
        text: Some("order\u{0}42".to_owned()),
        payload: payload.clone(),
        ..r.msg("c", "live", Direction::Inbound, 1, "")
    };
    assert!(
        store.append(live.clone()).await.unwrap(),
        "a message with U+0000 in its content is stored"
    );
    let stored = only_message(store, &key).await;
    assert_eq!(stored, live, "kind, text and payload read back exactly");
    assert_eq!(
        (
            stored.payload["k\0"].as_str(),
            stored.payload["k\u{FFFD}"].as_str(),
            stored.payload.as_object().map(serde_json::Map::len),
        ),
        (Some("the NUL key"), Some("the U+FFFD key"), Some(6)),
        "keys differing only by NUL vs U+FFFD stay distinct"
    );
    assert_eq!(
        summary(store, &key).await.unwrap().last_text.as_deref(),
        Some("order\u{0}42"),
        "the preview keeps it"
    );

    // A NUL and a U+FFFD are two different texts.
    let lookalike = StoredMessage {
        kind: "te\u{FFFD}xt".to_owned(),
        text: Some("order\u{FFFD}42".to_owned()),
        ..r.msg("c", "lookalike", Direction::Inbound, 2, "")
    };
    assert!(store.append(lookalike.clone()).await.unwrap());
    assert_eq!(
        store.messages(&key, None, 10).await.unwrap(),
        [lookalike.clone(), live.clone()],
        "neither becomes the other"
    );
    assert_eq!(
        summary(store, &key).await.unwrap().last_text,
        lookalike.text,
        "the preview follows the latest message"
    );

    // The status error keeps it, keys included.
    let out = r.msg("c", "out", Direction::Outbound, 3, "sent");
    assert!(store.append(out.clone()).await.unwrap());
    let error = serde_json::json!([{
        "code": 131_026,
        "title": "bad\0",
        "error_data": {"details\0": "x\0", "details\u{FFFD}": "y"}
    }]);
    assert!(
        store
            .update_status(
                &r.pn,
                &out.id,
                DeliveryStatus::Failed,
                at(4),
                Some(error.clone())
            )
            .await
            .unwrap()
    );
    let failed = StoredMessage {
        status: DeliveryStatus::Failed,
        status_at: Some(at(4)),
        error: Some(error),
        ..out
    };
    assert_eq!(
        store.messages(&key, None, 1).await.unwrap(),
        [failed],
        "the error reads back exactly"
    );
}

/// `append_synced`: see [`content_keeps_nul`].
async fn synced_content_keeps_nul<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("nul-synced");
    let synced_key = r.key("s");
    let history = StoredMessage {
        kind: "\0".to_owned(),
        text: Some("\0".to_owned()),
        payload: serde_json::json!({"\0": "\0", "\u{FFFD}": "\u{FFFD}"}),
        ..r.msg("s", "s1", Direction::Inbound, 5, "")
    };
    assert_eq!(
        store.append_synced(vec![history.clone()]).await.unwrap(),
        [true]
    );
    assert_eq!(only_message(store, &synced_key).await, history);
    assert_eq!(
        summary(store, &synced_key)
            .await
            .unwrap()
            .last_text
            .as_deref(),
        Some("\0")
    );
}

/// `fill_media_placeholder`: see [`content_keeps_nul`]. `kind` is compared
/// exactly: a kind that differs from the placeholder's by a NUL is not one.
async fn a_filled_placeholder_keeps_nul<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("nul-placeholder");
    let media = r.key("m");
    let lookalike_placeholder = StoredMessage {
        kind: format!("{}\0", StoredMessage::MEDIA_PLACEHOLDER),
        text: None,
        payload: serde_json::json!({}),
        ..r.msg("m", "p0", Direction::Outbound, 0, "")
    };
    let placeholder = StoredMessage {
        kind: StoredMessage::MEDIA_PLACEHOLDER.to_owned(),
        text: None,
        payload: serde_json::json!({"type": "media_placeholder"}),
        ..r.msg("m", "p1", Direction::Outbound, 6, "")
    };
    assert_eq!(
        store
            .append_synced(vec![lookalike_placeholder.clone(), placeholder.clone()])
            .await
            .unwrap(),
        [true, true]
    );
    let content = serde_json::json!({"type": "ima\0ge", "ima\0ge": {"caption": "cap\0tion"}});
    for (id, expected) in [(&lookalike_placeholder.id, false), (&placeholder.id, true)] {
        let applied = store
            .fill_media_placeholder(
                &r.pn,
                id,
                "ima\0ge".to_owned(),
                Some("cap\0tion".to_owned()),
                content.clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            applied, expected,
            "{id}: only the placeholder is filled; a kind one NUL away from its kind is not one"
        );
    }
    let filled = StoredMessage {
        kind: "ima\0ge".to_owned(),
        text: Some("cap\0tion".to_owned()),
        payload: content.clone(),
        ..placeholder
    };
    assert_eq!(
        store.messages(&media, None, 10).await.unwrap(),
        [filled, lookalike_placeholder],
        "the filled content reads back exactly; the lookalike is untouched"
    );
    assert_eq!(
        summary(store, &media).await.unwrap().last_text.as_deref(),
        Some("cap\0tion"),
        "the preview of the filled latest message keeps it"
    );
}
