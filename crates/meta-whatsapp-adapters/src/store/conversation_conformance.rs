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
//!   the Postgres store refuses it there.) A synced contact's names and
//!   username are content too;
//! - the lookup by message id is scoped to the business number: it finds
//!   live and synced messages and tombstones of its number, never another
//!   number's, whether the store keeps a message id once or once per
//!   number (`OPEN_QUESTIONS.md` #33);
//! - a window event is recorded once per business number and id, pages
//!   newest first by `(at, id)` with an exclusive cursor, and never
//!   touches the history nor the summary (nor creates one);
//! - thread ownership keeps the latest record: an older one never
//!   overwrites it, one of the same second does, and a record replaces
//!   every field;
//! - synced contacts keep the latest sync by the same rule, a removal
//!   applies unless the contact was synced after it, and is kept (its key
//!   and time, nothing of the contact) so that an older sync arriving
//!   after it is refused, even when it arrived first; the list pages by
//!   contact in byte order, scoped to one business number, and never
//!   lists a removal; contacts create no conversation;
//! - an identity link is stored once per business number and its two
//!   identities, whatever its time, and read from either side, oldest
//!   first; `identities` closes over the synced contacts (key, BSUID,
//!   parent BSUID, phone number; a kept removal connects nothing) and the
//!   links, on one number only; an empty identifier connects no one, and
//!   an erasure never matches a contact's other ids or a link's sides on
//!   one;
//! - an erasure deletes every record of one key on one number (messages
//!   of every origin and tombstones, the summary, window events, the
//!   ownership record, the synced contacts naming the key as key, BSUID,
//!   parent BSUID or phone number, a contact removal kept under the key,
//!   the identity links naming it), reports each count, leaves the ids
//!   free to be recorded again, and touches no other key and no other
//!   number; `erase_all` over `identities` reaches a person's thread under
//!   their phone number and under an earlier BSUID (security review of
//!   roadmap L5, M3), while the same `wa_id` and BSUID on another number
//!   stay;
//! - the erased person's messages in someone else's conversation (a
//!   group's, by `StoredMessage::sender`: the BSUID, else the phone
//!   number, of an inbound message) are redacted in place under
//!   `ErasureMode::Redact` or deleted under `ErasureMode::Delete`, per the
//!   store's `erasure_mode()`; the summary never keeps their text (under
//!   `Delete` it follows the latest remaining message, never a tombstone,
//!   or goes), the other participants' and the business's messages stay,
//!   and a redacted placeholder is never filled; a message's sender is the
//!   one it was appended with (a fill naming someone else changes nothing);
//! - purge by age deletes exactly what is older than the cutoff (a record
//!   at the cutoff stays): messages and tombstones, window events,
//!   ownership records, contact removals, and the summary of a
//!   conversation whose latest message went, keeping every other summary
//!   as it was; scoped to one number, or to every number; synced contacts
//!   and identity links stay;
//! - `apply_retention` purges by the store's `retention()`, and nothing
//!   under `Retention::Keep`.
//!
//! The purge cases delete, across the whole store, what is older than
//! 2000-01-01 (their own records, and those of a concurrent run of this
//! suite): never run the suite against a store holding data of your own.

use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::ids::{AppId, MessageId, PhoneNumberId, UserId};
use meta_whatsapp_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, DeliveryStatus, Direction, Erased,
    ErasureMode, IdentityLink, Purged, StoredContact, StoredMessage, ThreadOwner, ThreadOwnership,
    WindowEvent, WindowEventKind,
};
use time::OffsetDateTime;
use time::macros::datetime;

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
    a_message_is_looked_up_on_its_own_number(store).await;
    window_events_are_recorded_once_and_leave_the_summary_alone(store).await;
    thread_ownership_keeps_the_latest_record(store).await;
    synced_contacts_keep_the_latest_sync(store).await;
    identity_links_are_stored_once_per_number(store).await;
    identities_close_over_contacts_and_links(store).await;
    erase_deletes_every_record_of_one_contact_on_one_number(store).await;
    erase_all_reaches_a_person_under_every_identity(store).await;
    an_erasure_redacts_or_deletes_the_persons_group_messages(store).await;
    an_empty_identifier_connects_nobody(store).await;
    a_filled_placeholder_keeps_its_sender(store).await;
    purge_deletes_exactly_what_is_older(store).await;
    apply_retention_follows_the_retention(store).await;
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

    /// A window event id of this run.
    fn event_id(&self, local: &str) -> String {
        format!("wacid.{}.{local}", self.tag)
    }

    fn event(
        &self,
        contact: &str,
        local_id: &str,
        kind: WindowEventKind,
        at: OffsetDateTime,
    ) -> WindowEvent {
        WindowEvent {
            conversation: self.key(contact),
            kind,
            id: self.event_id(local_id),
            at,
        }
    }

    fn contact(&self, contact: &str, secs: i64) -> StoredContact {
        StoredContact {
            key: self.key(contact),
            full_name: Some("Pablo Morales".to_owned()),
            first_name: Some("Pablo".to_owned()),
            phone_number: None,
            user_id: None,
            parent_user_id: None,
            username: None,
            synced_at: at(secs),
        }
    }
}

fn ownership(owner: ThreadOwner, role: Option<&str>, since: OffsetDateTime) -> ThreadOwnership {
    ThreadOwnership {
        owner,
        role: role.map(str::to_owned),
        app_id: None,
        since,
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
    contact_names_keep_nul(store).await;
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

/// A synced contact's names and username are content: see
/// [`content_keeps_nul`].
async fn contact_names_keep_nul<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("nul-contact");
    let contact = StoredContact {
        full_name: Some("Pa\0blo Mo\u{FFFD}rales".to_owned()),
        first_name: Some("\0".to_owned()),
        username: Some("pa\0blo".to_owned()),
        ..r.contact("US.1", 1)
    };
    assert!(store.put_contact(contact.clone()).await.unwrap());
    assert_eq!(
        store.contact(&contact.key).await.unwrap().as_ref(),
        Some(&contact),
        "names and username read back exactly"
    );
    assert_eq!(store.contacts(&r.pn, None, 10).await.unwrap(), [contact]);
}

/// The lookup by message id is scoped to the business number, whichever
/// way `OPEN_QUESTIONS.md` #33 is answered.
async fn a_message_is_looked_up_on_its_own_number<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("lookup");
    let other = Run::new("lookup-other");
    let live = r.msg("c", "live", Direction::Inbound, 1, "hello");
    let out = r.msg("c", "out", Direction::Outbound, 2, "order shipped");
    let history = r.msg("d", "synced", Direction::Inbound, 3, "from the app");
    assert!(store.append(live.clone()).await.unwrap());
    assert!(store.append(out.clone()).await.unwrap());
    assert!(synced(store, history.clone()).await.unwrap());
    let revoked = r.id("revoked-first");
    assert!(
        store
            .revoke(&r.key("c"), &revoked, Direction::Inbound, at(4))
            .await
            .unwrap()
    );
    for m in [&live, &out, &history] {
        assert_eq!(
            store.message(&r.pn, &m.id).await.unwrap().as_ref(),
            Some(m),
            "{}: found on its number",
            m.id
        );
    }
    assert_eq!(
        store.message(&r.pn, &revoked).await.unwrap(),
        Some(StoredMessage::tombstone(
            &r.key("c"),
            &revoked,
            Direction::Inbound,
            at(4)
        )),
        "a tombstone is what is stored under its id"
    );
    assert!(
        store
            .update_status(&r.pn, &out.id, DeliveryStatus::Read, at(5), None)
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .message(&r.pn, &out.id)
            .await
            .unwrap()
            .map(|m| m.status),
        Some(DeliveryStatus::Read),
        "the lookup reads the current row"
    );
    assert_eq!(
        store.message(&r.pn, &r.id("never-appended")).await.unwrap(),
        None
    );
    assert_eq!(
        store.message(&other.pn, &live.id).await.unwrap(),
        None,
        "a message of another number is never found"
    );

    // Ids stored once per store (today) or once per number: the second
    // number finds its own copy or nothing, the first keeps its own.
    let twin = StoredMessage {
        conversation: other.key("c"),
        text: Some("the other number's".to_owned()),
        ..live.clone()
    };
    let stored_twice = store.append(twin.clone()).await.unwrap();
    assert_eq!(
        store.message(&other.pn, &live.id).await.unwrap(),
        stored_twice.then_some(twin),
        "the other number finds its own copy, if it has one"
    );
    assert_eq!(
        store.message(&r.pn, &live.id).await.unwrap().as_ref(),
        Some(&live),
        "and the first number its own"
    );
}

/// Window events: recorded once per number and id, paged newest first,
/// never part of the history or the summary.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn window_events_are_recorded_once_and_leave_the_summary_alone<
    S: ConversationStore + ?Sized,
>(
    store: &S,
) {
    let r = Run::new("window-events");
    let other = Run::new("window-events-other");
    let key = r.key("c");
    assert!(
        store
            .append(r.msg("c", "in", Direction::Inbound, 1, "hi"))
            .await
            .unwrap()
    );
    let before = summary(store, &key).await.unwrap();

    let call = r.event("c", "call", WindowEventKind::CustomerCall, at(10));
    let accepted = r.event("c", "accepted", WindowEventKind::CallAccepted, at(20));
    let standby = r.event("c", "standby", WindowEventKind::StandbyMessage, at(20));
    let future = r.event(
        "c",
        "future",
        WindowEventKind::Other("future_reason".to_owned()),
        at(5),
    );
    for event in [&call, &accepted, &standby, &future] {
        assert!(
            store.record_window_event(event.clone()).await.unwrap(),
            "{}: recorded",
            event.id
        );
    }
    assert!(
        !store
            .record_window_event(WindowEvent {
                conversation: r.key("elsewhere"),
                kind: WindowEventKind::StandbyMessage,
                at: at(99),
                ..call.clone()
            })
            .await
            .unwrap(),
        "the same id on the same number is a no-op, whatever its kind, time or conversation"
    );
    let elsewhere = WindowEvent {
        conversation: other.key("c"),
        ..call.clone()
    };
    assert!(
        store.record_window_event(elsewhere.clone()).await.unwrap(),
        "the same id on another number is that number's own"
    );
    let second = r.event("d", "d-call", WindowEventKind::CustomerCall, at(30));
    assert!(store.record_window_event(second.clone()).await.unwrap());

    // (at, id) descending: at 20, "accepted" < "standby" in byte order.
    let expected = [
        standby.clone(),
        accepted.clone(),
        call.clone(),
        future.clone(),
    ];
    assert_eq!(
        store.window_events(&key, None, 10).await.unwrap(),
        expected,
        "newest first by (at, id)"
    );
    assert_eq!(
        store.window_events(&key, None, 2).await.unwrap(),
        [standby.clone(), accepted.clone()],
        "limit"
    );
    assert_eq!(
        store
            .window_events(&key, Some((standby.at, standby.id.clone())), 10)
            .await
            .unwrap(),
        [accepted.clone(), call.clone(), future.clone()],
        "the cursor row itself is excluded"
    );
    for page_size in [1, 3] {
        let mut seen = Vec::new();
        let mut cursor = None;
        for _ in 0..=expected.len() {
            let page = store
                .window_events(&key, cursor.clone(), page_size)
                .await
                .unwrap();
            assert!(page.len() <= page_size);
            let Some(last) = page.last() else { break };
            cursor = Some((last.at, last.id.clone()));
            seen.extend(page);
        }
        assert_eq!(seen, expected, "paging by {page_size}");
    }
    assert!(store.window_events(&key, None, 0).await.unwrap().is_empty());
    assert_eq!(
        store
            .window_events(&other.key("c"), None, 10)
            .await
            .unwrap(),
        [elsewhere],
        "scoped to the number"
    );
    assert_eq!(
        store.window_events(&r.key("d"), None, 10).await.unwrap(),
        [second],
        "scoped to the conversation"
    );
    assert!(
        store
            .window_events(&r.key("elsewhere"), None, 10)
            .await
            .unwrap()
            .is_empty(),
        "the refused duplicate went nowhere"
    );

    assert_eq!(
        summary(store, &key).await,
        Some(before),
        "window events move nothing in the summary"
    );
    assert_eq!(store.last_inbound_at(&key).await.unwrap(), Some(at(1)));
    assert_eq!(store.messages(&key, None, 10).await.unwrap().len(), 1);
    assert_eq!(summary(store, &r.key("d")).await, None, "nor create one");
    assert!(
        store
            .messages(&r.key("d"), None, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Thread ownership keeps the latest record of each conversation.
async fn thread_ownership_keeps_the_latest_record<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("owner");
    let other = Run::new("owner-other");
    let key = r.key("c");
    assert_eq!(store.thread_owner(&key).await.unwrap(), None);

    let passed = ThreadOwnership {
        app_id: Some(AppId::new("1234567890")),
        ..ownership(ThreadOwner::ThisApp, Some("customer_service"), at(10))
    };
    assert!(store.set_thread_owner(&key, passed.clone()).await.unwrap());
    assert_eq!(
        store.thread_owner(&key).await.unwrap().as_ref(),
        Some(&passed)
    );

    let late = ownership(ThreadOwner::AnotherApp, Some("escalation"), at(5));
    assert!(
        !store.set_thread_owner(&key, late).await.unwrap(),
        "an older record never overwrites"
    );
    assert_eq!(
        store.thread_owner(&key).await.unwrap().as_ref(),
        Some(&passed)
    );

    let taken = ownership(ThreadOwner::AnotherApp, Some("escalation"), at(10));
    assert!(
        store.set_thread_owner(&key, taken.clone()).await.unwrap(),
        "a change in the same second applies"
    );
    assert_eq!(
        store.thread_owner(&key).await.unwrap().as_ref(),
        Some(&taken),
        "every field replaced: the app id is gone"
    );

    let released = ownership(ThreadOwner::Idle, None, at(20));
    assert!(
        store
            .set_thread_owner(&key, released.clone())
            .await
            .unwrap()
    );
    assert_eq!(store.thread_owner(&key).await.unwrap(), Some(released));

    assert_eq!(store.thread_owner(&r.key("d")).await.unwrap(), None);
    assert_eq!(
        store.thread_owner(&other.key("c")).await.unwrap(),
        None,
        "scoped to the number"
    );
    assert_eq!(summary(store, &key).await, None, "no summary");
    assert!(store.messages(&key, None, 10).await.unwrap().is_empty());
}

/// Synced contacts keep the latest sync, are removed unless synced after
/// the removal, and page by contact.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn synced_contacts_keep_the_latest_sync<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("contacts");
    let other = Run::new("contacts-other");
    let key = r.key("US.1");
    let pablo = StoredContact {
        phone_number: Some("16505551234".to_owned()),
        user_id: Some(UserId::new("US.1")),
        parent_user_id: Some(UserId::new("US.ENT.1")),
        ..r.contact("US.1", 10)
    };
    assert!(store.put_contact(pablo.clone()).await.unwrap());
    assert_eq!(store.contact(&key).await.unwrap().as_ref(), Some(&pablo));

    let stale = StoredContact {
        full_name: Some("Old name".to_owned()),
        ..r.contact("US.1", 5)
    };
    assert!(
        !store.put_contact(stale).await.unwrap(),
        "an older sync never overwrites"
    );
    assert_eq!(store.contact(&key).await.unwrap().as_ref(), Some(&pablo));

    let edited = StoredContact {
        full_name: Some("Pablo M.".to_owned()),
        first_name: None,
        username: Some("pablo".to_owned()),
        parent_user_id: None,
        ..pablo.clone()
    };
    assert!(
        store.put_contact(edited.clone()).await.unwrap(),
        "an edit in the same second applies"
    );
    assert_eq!(
        store.contact(&key).await.unwrap().as_ref(),
        Some(&edited),
        "every field replaced"
    );

    assert!(
        !store.remove_contact(&key, at(9)).await.unwrap(),
        "a removal older than the sync changes nothing"
    );
    assert_eq!(store.contact(&key).await.unwrap().as_ref(), Some(&edited));
    assert!(
        store.remove_contact(&key, at(10)).await.unwrap(),
        "a removal in the same second applies"
    );
    assert_eq!(store.contact(&key).await.unwrap(), None);
    assert!(
        !store.remove_contact(&key, at(11)).await.unwrap(),
        "no contact left to remove: the removal moves to 11"
    );
    assert!(
        !store
            .put_contact(StoredContact {
                synced_at: at(10),
                ..edited.clone()
            })
            .await
            .unwrap(),
        "the removal is kept: an add older than it, arriving after it, is refused"
    );
    assert!(
        !store.remove_contact(&key, at(5)).await.unwrap(),
        "an older removal changes nothing"
    );
    assert!(
        !store
            .put_contact(StoredContact {
                synced_at: at(10),
                ..edited.clone()
            })
            .await
            .unwrap(),
        "and leaves the later one in place"
    );
    assert_eq!(store.contact(&key).await.unwrap(), None);
    let back = StoredContact {
        synced_at: at(11),
        ..edited.clone()
    };
    assert!(
        store.put_contact(back.clone()).await.unwrap(),
        "an add in the same second as the removal stores the contact again"
    );
    assert_eq!(store.contact(&key).await.unwrap(), Some(back));
    assert!(store.remove_contact(&key, at(11)).await.unwrap());
    assert_eq!(store.contact(&key).await.unwrap(), None);
    // A removal that arrives before its contact's add: kept all the same.
    let unknown = r.key("unknown");
    assert!(!store.remove_contact(&unknown, at(50)).await.unwrap());
    assert!(
        !store.put_contact(r.contact("unknown", 49)).await.unwrap(),
        "an add older than a removal that arrived first is refused"
    );
    assert_eq!(store.contact(&unknown).await.unwrap(), None);

    // Listing: byte order of the contact, exclusive cursor, one number.
    let names = ["b", "B", "a", "A", "a-1", "a_1", "é", "10", "9", "US.2"];
    for name in names {
        assert!(store.put_contact(r.contact(name, 1)).await.unwrap());
    }
    assert!(store.put_contact(other.contact("a", 1)).await.unwrap());
    let mut expected: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
    expected.sort_unstable();
    for page_size in [1, 4, 100] {
        let mut seen = Vec::new();
        let mut cursor = None;
        for _ in 0..=expected.len() {
            let page = store
                .contacts(&r.pn, cursor.clone(), page_size)
                .await
                .unwrap();
            assert!(page.len() <= page_size);
            let Some(last) = page.last() else { break };
            cursor = Some(last.key.contact.clone());
            for c in page {
                assert_eq!(c.key.phone_number_id, r.pn, "scoped to one number");
                seen.push(c.key.contact);
            }
        }
        assert_eq!(
            seen, expected,
            "contacts paged by {page_size}: byte order, no repeats, no skips"
        );
    }
    assert!(store.contacts(&r.pn, None, 0).await.unwrap().is_empty());
    assert!(
        store
            .conversations(&r.pn, None, 10)
            .await
            .unwrap()
            .is_empty(),
        "contacts are not conversations"
    );
}

/// Everything recorded under one key of one number, for the erasure case.
struct Recorded {
    messages: Vec<StoredMessage>,
    summary: Option<ConversationSummary>,
    window_events: Vec<WindowEvent>,
    owner: Option<ThreadOwnership>,
}

async fn recorded<S: ConversationStore + ?Sized>(store: &S, key: &ConversationKey) -> Recorded {
    Recorded {
        messages: store.messages(key, None, 100).await.unwrap(),
        summary: summary(store, key).await,
        window_events: store.window_events(key, None, 100).await.unwrap(),
        owner: store.thread_owner(key).await.unwrap(),
    }
}

impl Recorded {
    fn assert_same(&self, other: &Self, what: &str) {
        assert_eq!(self.messages, other.messages, "{what}: messages");
        assert_eq!(self.summary, other.summary, "{what}: summary");
        assert_eq!(
            self.window_events, other.window_events,
            "{what}: window events"
        );
        assert_eq!(self.owner, other.owner, "{what}: owner");
    }
}

/// Record something of every kind under `key` (`local` keeps its ids
/// apart).
async fn record_everything<S: ConversationStore + ?Sized>(
    store: &S,
    r: &Run,
    key: &ConversationKey,
    local: &str,
) {
    let contact = key.contact.as_str();
    let message = |id: &str, direction, secs, text: &str| StoredMessage {
        conversation: key.clone(),
        ..r.msg(contact, &format!("{local}-{id}"), direction, secs, text)
    };
    assert!(
        store
            .append(message(
                "in",
                Direction::Inbound,
                1,
                "my address is 1 Main St"
            ))
            .await
            .unwrap()
    );
    assert!(
        store
            .append(message("out", Direction::Outbound, 2, "thanks"))
            .await
            .unwrap()
    );
    assert!(
        store
            .update_status(
                &key.phone_number_id,
                &r.id(&format!("{local}-out")),
                DeliveryStatus::Failed,
                at(3),
                Some(serde_json::json!({"code": 131_026}))
            )
            .await
            .unwrap()
    );
    let placeholder = StoredMessage {
        kind: StoredMessage::MEDIA_PLACEHOLDER.to_owned(),
        text: None,
        ..message("media", Direction::Inbound, -2, "")
    };
    assert_eq!(
        store
            .append_synced(vec![
                message("synced", Direction::Inbound, -1, "from the app"),
                placeholder
            ])
            .await
            .unwrap(),
        [true, true]
    );
    assert!(
        store
            .revoke(
                key,
                &r.id(&format!("{local}-gone")),
                Direction::Inbound,
                at(4)
            )
            .await
            .unwrap(),
        "a tombstone"
    );
    for (id, kind, secs) in [
        ("call", WindowEventKind::CustomerCall, 5),
        ("standby", WindowEventKind::StandbyMessage, 6),
    ] {
        assert!(
            store
                .record_window_event(WindowEvent {
                    conversation: key.clone(),
                    kind,
                    id: r.event_id(&format!("{local}-{id}")),
                    at: at(secs),
                })
                .await
                .unwrap()
        );
    }
    assert!(
        store
            .set_thread_owner(key, ownership(ThreadOwner::ThisApp, None, at(7)))
            .await
            .unwrap()
    );
}

/// A link between two identities of one person.
fn link(run: &Run, previous: &str, current: &str, secs: i64) -> IdentityLink {
    IdentityLink::new(run.pn.clone(), previous, current, at(secs))
}

/// Identity links are stored once per number and read from either side.
async fn identity_links_are_stored_once_per_number<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("links");
    let other = Run::new("links-other");
    assert!(
        store
            .link_identity(link(&r, "US.1", "US.2", 5))
            .await
            .unwrap()
    );
    assert!(
        !store
            .link_identity(link(&r, "US.1", "US.2", 9))
            .await
            .unwrap(),
        "the same link again, at another time, changes nothing"
    );
    assert!(
        store
            .link_identity(link(&r, "US.2", "US.3", 3))
            .await
            .unwrap()
    );
    assert!(
        store
            .link_identity(link(&r, "US.0", "US.2", 3))
            .await
            .unwrap()
    );
    assert!(
        store
            .link_identity(link(&other, "US.1", "US.2", 1))
            .await
            .unwrap(),
        "the same identities on another number are another link"
    );
    assert_eq!(
        store.identity_links(&r.key("US.2")).await.unwrap(),
        [
            link(&r, "US.0", "US.2", 3),
            link(&r, "US.2", "US.3", 3),
            link(&r, "US.1", "US.2", 5)
        ],
        "either side, oldest first, then by identity"
    );
    assert_eq!(
        store.identity_links(&r.key("US.1")).await.unwrap(),
        [link(&r, "US.1", "US.2", 5)],
        "the first time stored is kept"
    );
    assert!(
        store
            .identity_links(&r.key("US.9"))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.identity_links(&other.key("US.1")).await.unwrap(),
        [link(&other, "US.1", "US.2", 1)],
        "never another number's"
    );
    assert!(
        store
            .conversations(&r.pn, None, 10)
            .await
            .unwrap()
            .is_empty(),
        "links are not conversations"
    );
}

fn set(ids: &[&str]) -> std::collections::BTreeSet<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

/// `identities` is the closure over one number's synced contacts and
/// links.
async fn identities_close_over_contacts_and_links<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("identities");
    let other = Run::new("identities-other");
    // One person: a contact keyed by their phone number naming their BSUID
    // and parent BSUID, another portfolio's BSUID under the same parent, an
    // earlier BSUID and an earlier phone number linked to them.
    for contact in [
        StoredContact {
            phone_number: Some("16505550009".to_owned()),
            user_id: Some(UserId::new("US.9")),
            parent_user_id: Some(UserId::new("PA.9")),
            ..r.contact("16505550009", 1)
        },
        StoredContact {
            parent_user_id: Some(UserId::new("PA.9")),
            ..r.contact("US.7", 1)
        },
        StoredContact {
            phone_number: Some("16505550001".to_owned()),
            user_id: Some(UserId::new("US.1")),
            ..r.contact("US.1", 1)
        },
        // Removed: its removal keeps the key alone, which connects nothing.
        StoredContact {
            user_id: Some(UserId::new("US.9")),
            ..r.contact("US.6", 1)
        },
        // Another number's contacts are another business's.
        StoredContact {
            user_id: Some(UserId::new("US.X")),
            ..other.contact("16505550009", 1)
        },
    ] {
        assert!(store.put_contact(contact).await.unwrap());
    }
    assert!(store.remove_contact(&r.key("US.6"), at(2)).await.unwrap());
    for l in [
        link(&r, "US.8", "US.9", 1),
        link(&r, "16505550008", "US.8", 0),
        link(&other, "US.9", "US.Y", 1),
    ] {
        assert!(store.link_identity(l).await.unwrap());
    }
    let person = set(&["16505550008", "16505550009", "PA.9", "US.7", "US.8", "US.9"]);
    for from in ["US.9", "16505550008", "US.7", "PA.9"] {
        assert_eq!(
            store.identities(&r.key(from)).await.unwrap(),
            person,
            "from {from}"
        );
    }
    assert_eq!(
        store.identities(&r.key("US.1")).await.unwrap(),
        set(&["16505550001", "US.1"]),
        "someone else"
    );
    assert_eq!(
        store.identities(&r.key("US.6")).await.unwrap(),
        set(&["US.6"]),
        "a kept removal connects nothing"
    );
    assert_eq!(
        store.identities(&r.key("nobody")).await.unwrap(),
        set(&["nobody"]),
        "an unknown key is its own identity"
    );
    assert_eq!(
        store.identities(&other.key("US.9")).await.unwrap(),
        set(&["US.9", "US.Y"]),
        "never another number's"
    );
}

/// The security review of roadmap L5 (M3): a person's history thread
/// keyed by their phone number, their live messages keyed by their BSUID
/// and those under an earlier BSUID are erased together through
/// `identities`, and the same `wa_id` and BSUID on another number (another
/// business's customer) stay.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn erase_all_reaches_a_person_under_every_identity<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("erase-all");
    let other = Run::new("erase-all-other");
    let (phone, bsuid, earlier) = (r.key("16505550009"), r.key("US.9"), r.key("US.8"));
    let neighbour = r.key("US.1");
    for (key, local) in [
        (&phone, "phone"),
        (&bsuid, "bsuid"),
        (&earlier, "earlier"),
        (&neighbour, "neighbour"),
    ] {
        record_everything(store, &r, key, local).await;
    }
    let (their_phone, their_bsuid) = (other.key("16505550009"), other.key("US.9"));
    for (key, local) in [(&their_phone, "phone"), (&their_bsuid, "bsuid")] {
        record_everything(store, &other, key, local).await;
    }
    for run in [&r, &other] {
        assert!(
            store
                .put_contact(StoredContact {
                    phone_number: Some("16505550009".to_owned()),
                    user_id: Some(UserId::new("US.9")),
                    ..run.contact("16505550009", 1)
                })
                .await
                .unwrap()
        );
        assert!(
            store
                .link_identity(link(run, "US.8", "US.9", 1))
                .await
                .unwrap()
        );
    }
    // In the other number's group, a message from the same phone number.
    let their_group = StoredMessage {
        payload: serde_json::json!({"from": "16505550009", "from_user_id": "US.9",
            "group_id": "HBgOTHER", "type": "text", "text": {"body": "hi"}}),
        ..other.msg("HBgOTHER", "group", Direction::Inbound, 1, "hi")
    };
    assert!(store.append(their_group.clone()).await.unwrap());
    // In this number's group, a message from their phone number alone (no
    // BSUID): only the synced contact ties it to the BSUID erased.
    let my_group = StoredMessage {
        payload: serde_json::json!({"from": "16505550009", "group_id": "HBgMINE",
            "type": "text", "text": {"body": "my address"}}),
        ..r.msg("HBgMINE", "group", Direction::Inbound, 1, "my address")
    };
    assert!(store.append(my_group.clone()).await.unwrap());
    let neighbour_before = recorded(store, &neighbour).await;
    let others_before = [
        recorded(store, &their_phone).await,
        recorded(store, &their_bsuid).await,
    ];

    let ids = store.identities(&bsuid).await.unwrap();
    assert_eq!(ids, set(&["16505550009", "US.8", "US.9"]));
    let ids: Vec<String> = ids.into_iter().collect();
    assert_eq!(
        store.erase_all(&r.pn, &ids).await.unwrap(),
        Erased {
            messages: 15,
            conversations: 3,
            window_events: 6,
            thread_owners: 3,
            contacts: 1,
            identity_links: 1,
            group_messages: 1,
        },
        "the three threads, the contact that tied them, the link, and their group message sent \
         from the phone number the contact ties to the BSUID"
    );
    let left = store.message(&r.pn, &my_group.id).await.unwrap();
    if store.erasure_mode() == ErasureMode::Delete {
        assert_eq!(left, None);
    } else {
        let mut redacted = my_group.clone();
        redacted.redact();
        assert_eq!(left, Some(redacted));
    }
    for key in [&phone, &bsuid, &earlier] {
        let left = recorded(store, key).await;
        assert!(left.messages.is_empty(), "{key}: messages");
        assert_eq!(left.summary, None, "{key}: summary");
        assert!(left.window_events.is_empty(), "{key}: window events");
        assert_eq!(left.owner, None, "{key}: owner");
    }
    assert_eq!(store.contact(&phone).await.unwrap(), None);
    assert!(store.identity_links(&bsuid).await.unwrap().is_empty());
    assert_eq!(store.identities(&bsuid).await.unwrap(), set(&["US.9"]));
    recorded(store, &neighbour)
        .await
        .assert_same(&neighbour_before, "another contact of the number");
    for (key, before) in [&their_phone, &their_bsuid].into_iter().zip(&others_before) {
        recorded(store, key)
            .await
            .assert_same(before, "the same wa_id and BSUID on another number");
    }
    assert!(store.contact(&their_phone).await.unwrap().is_some());
    assert_eq!(store.identity_links(&their_bsuid).await.unwrap().len(), 1);
    assert_eq!(
        store.message(&other.pn, &their_group.id).await.unwrap(),
        Some(their_group),
        "another number's group message from the same phone number"
    );
    assert!(
        store.erase_all(&r.pn, &ids).await.unwrap().is_empty(),
        "erasing again deletes nothing"
    );
    assert!(store.erase_all(&r.pn, &[]).await.unwrap().is_empty());

    // One key alone leaves the others, and loses what tied them: collect
    // the identities first.
    let (phone5, bsuid5) = (r.key("16505550005"), r.key("US.5"));
    record_everything(store, &r, &phone5, "phone5").await;
    assert!(
        store
            .put_contact(StoredContact {
                user_id: Some(UserId::new("US.5")),
                ..r.contact("16505550005", 1)
            })
            .await
            .unwrap()
    );
    assert_eq!(
        store.erase(&bsuid5).await.unwrap(),
        Erased {
            contacts: 1,
            ..Erased::default()
        }
    );
    assert_eq!(
        recorded(store, &phone5).await.messages.len(),
        5,
        "the thread under the phone number stays"
    );
    assert_eq!(store.identities(&bsuid5).await.unwrap(), set(&["US.5"]));
}

/// The erased person's messages in a group (a conversation keyed by
/// someone else) are redacted or deleted, per the store's `erasure_mode`.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn an_erasure_redacts_or_deletes_the_persons_group_messages<S: ConversationStore + ?Sized>(
    store: &S,
) {
    let r = Run::new("group");
    let other = Run::new("group-other");
    let (group, quiet) = (r.key("HBgGROUP1"), r.key("HBgGROUP2"));
    let from = |run: &Run, key: &ConversationKey, local: &str, secs, bsuid: Option<&str>| {
        let text = format!("{local}: my address is 1 Main St");
        let mut payload = serde_json::json!({"from": "16505550009", "group_id": key.contact,
            "type": "text", "text": {"body": text}});
        if let Some(bsuid) = bsuid {
            payload["from_user_id"] = serde_json::json!(bsuid);
        }
        StoredMessage {
            payload,
            ..run.msg(&key.contact, local, Direction::Inbound, secs, &text)
        }
    };
    // Before BSUIDs: `from` alone.
    let x0 = from(&r, &group, "x0", 0, None);
    let p1 = StoredMessage {
        payload: serde_json::json!({"from": "16505550002", "from_user_id": "US.2",
            "group_id": group.contact, "type": "text", "text": {"body": "hello all"}}),
        ..r.msg(&group.contact, "p1", Direction::Inbound, 1, "hello all")
    };
    // The business's reply: never the person's, even naming their number.
    let b2 = StoredMessage {
        payload: serde_json::json!({"from": "16505550009", "to": group.contact,
            "type": "text", "text": {"body": "welcome"}}),
        ..r.msg(&group.contact, "b2", Direction::Outbound, 2, "welcome")
    };
    let x4 = from(&r, &group, "x4", 4, Some("US.9"));
    let placeholder = StoredMessage {
        kind: StoredMessage::MEDIA_PLACEHOLDER.to_owned(),
        text: None,
        ..from(&r, &group, "x-1", -1, Some("US.9"))
    };
    let x5 = from(&r, &quiet, "x5", 5, Some("US.9"));
    let own = from(&r, &r.key("US.9"), "own", 6, Some("US.9"));
    for m in [&x0, &p1, &b2, &x4, &placeholder, &own] {
        assert!(store.append(m.clone()).await.unwrap(), "{}", m.id);
    }
    // Synced history keeps its sender too.
    assert_eq!(store.append_synced(vec![x5.clone()]).await.unwrap(), [true]);
    // Another participant's revoke of a message not stored: a tombstone,
    // after everything but x4.
    assert!(
        store
            .revoke(&group, &r.id("t3"), Direction::Inbound, at(3))
            .await
            .unwrap()
    );
    let theirs = from(&other, &other.key("HBgGROUP1"), "x", 4, Some("US.9"));
    assert!(store.append(theirs.clone()).await.unwrap());
    let history = store.messages(&group, None, 10).await.unwrap();
    let group_summary = summary(store, &group).await.unwrap();
    let quiet_summary = summary(store, &quiet).await.unwrap();
    assert_eq!(
        (
            group_summary.last_message_at,
            group_summary.last_text.as_deref()
        ),
        (x4.timestamp, x4.text.as_deref()),
        "the person's message is the group's latest"
    );

    let mode = store.erasure_mode();
    assert_eq!(
        store
            .erase_all(&r.pn, &["US.9".to_owned(), "16505550009".to_owned()])
            .await
            .unwrap(),
        Erased {
            messages: 1,
            conversations: 1,
            group_messages: 4,
            ..Erased::default()
        },
        "their own conversation; x0 (by phone number), x4 and the placeholder in one group, x5 \
         in the other ({mode:?})"
    );
    let erased_ids = [&x0.id, &x4.id, &placeholder.id, &x5.id];
    let after = store.messages(&group, None, 10).await.unwrap();
    if mode == ErasureMode::Delete {
        let expected: Vec<StoredMessage> = history
            .iter()
            .filter(|m| !erased_ids.contains(&&m.id))
            .cloned()
            .collect();
        assert_eq!(after, expected, "their messages go, the rest stays");
        assert_eq!(
            summary(store, &group).await,
            Some(ConversationSummary {
                last_message_at: b2.timestamp,
                last_text: b2.text.clone(),
                ..group_summary
            }),
            "the summary follows the latest remaining message, never the tombstone; the \
             window and the unread count stay"
        );
        assert_eq!(
            summary(store, &quiet).await,
            None,
            "a group left with nothing has no summary"
        );
        for id in erased_ids {
            assert_eq!(store.message(&r.pn, id).await.unwrap(), None, "{id}");
        }
    } else {
        let expected: Vec<StoredMessage> = history
            .iter()
            .map(|m| {
                let mut m = m.clone();
                if erased_ids.contains(&&m.id) {
                    m.redact();
                }
                m
            })
            .collect();
        assert_eq!(
            after, expected,
            "their messages keep their place, without content; the rest stays"
        );
        assert_eq!(
            summary(store, &group).await,
            Some(ConversationSummary {
                last_text: None,
                ..group_summary
            }),
            "the preview goes with the latest message's text"
        );
        assert_eq!(
            summary(store, &quiet).await,
            Some(ConversationSummary {
                last_text: None,
                ..quiet_summary
            })
        );
        let mut redacted = x5.clone();
        redacted.redact();
        assert_eq!(store.message(&r.pn, &x5.id).await.unwrap(), Some(redacted));
    }
    assert!(
        !store
            .fill_media_placeholder(
                &r.pn,
                &placeholder.id,
                "image".to_owned(),
                Some("their photo".to_owned()),
                serde_json::json!({"image": {"caption": "their photo"}}),
            )
            .await
            .unwrap(),
        "their placeholder never gets its content"
    );
    assert_eq!(
        store.message(&other.pn, &theirs.id).await.unwrap(),
        Some(theirs),
        "the same person in another number's group stays"
    );
    assert!(
        store
            .erase_all(&r.pn, &["US.9".to_owned(), "16505550009".to_owned()])
            .await
            .unwrap()
            .is_empty(),
        "erasing again changes nothing"
    );
}

/// An empty identifier names nobody (review of the L5 remediation): a
/// synced contact whose phone number, BSUID or parent BSUID is empty, and
/// a link with an empty side, connect no one through it. Otherwise
/// `identities` would join everyone sharing the empty field into one
/// person, and `erase_all` would delete their records with theirs.
async fn an_empty_identifier_connects_nobody<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("empty-ids");
    for contact in [
        StoredContact {
            phone_number: Some(String::new()),
            ..r.contact("US.A", 1)
        },
        StoredContact {
            phone_number: Some(String::new()),
            user_id: Some(UserId::new("")),
            parent_user_id: Some(UserId::new("")),
            ..r.contact("US.B", 1)
        },
    ] {
        assert!(store.put_contact(contact).await.unwrap());
    }
    for l in [link(&r, "", "US.A", 1), link(&r, "US.C", "", 1)] {
        assert!(store.link_identity(l).await.unwrap());
    }
    let b = r.key("US.B");
    record_everything(store, &r, &b, "b").await;
    let b_before = recorded(store, &b).await;
    for from in ["US.A", "US.B", "US.C", ""] {
        assert_eq!(
            store.identities(&r.key(from)).await.unwrap(),
            set(&[from]),
            "from {from:?}: an empty field connects no one"
        );
    }
    assert_eq!(
        store.erase_all(&r.pn, &["US.A".to_owned()]).await.unwrap(),
        Erased {
            contacts: 1,
            identity_links: 1,
            ..Erased::default()
        },
        "their contact and the link naming them"
    );
    assert_eq!(
        store.erase_all(&r.pn, &[String::new()]).await.unwrap(),
        Erased::default(),
        "the empty id names no contact and no link"
    );
    assert!(
        store.contact(&b).await.unwrap().is_some(),
        "a contact sharing an empty field stays"
    );
    recorded(store, &b)
        .await
        .assert_same(&b_before, "someone sharing an empty field");
    assert_eq!(
        store.identity_links(&r.key("US.C")).await.unwrap(),
        [link(&r, "US.C", "", 1)]
    );
}

/// A message's sender is the one it was appended with
/// (`StoredMessage::sender`: a fill does not change it): a group
/// placeholder filled with a payload naming someone else is still its
/// sender's when they are erased, and is erased once.
async fn a_filled_placeholder_keeps_its_sender<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("filled-sender");
    let group = r.key("HBgFILLED");
    let placeholder = StoredMessage {
        kind: StoredMessage::MEDIA_PLACEHOLDER.to_owned(),
        text: None,
        payload: serde_json::json!({"from": "16505550009", "from_user_id": "US.9",
            "group_id": group.contact, "type": "image"}),
        ..r.msg(&group.contact, "p", Direction::Inbound, 1, "")
    };
    assert_eq!(
        store
            .append_synced(vec![placeholder.clone()])
            .await
            .unwrap(),
        [true]
    );
    assert!(
        store
            .fill_media_placeholder(
                &r.pn,
                &placeholder.id,
                "image".to_owned(),
                Some("their photo".to_owned()),
                serde_json::json!({"from_user_id": "US.8", "image": {"caption": "their photo"}}),
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .erase_all(&r.pn, &["US.8".to_owned()])
            .await
            .unwrap()
            .is_empty(),
        "the filled payload's `from_user_id` is not the message's sender"
    );
    assert_eq!(
        store.erase_all(&r.pn, &["US.9".to_owned()]).await.unwrap(),
        Erased {
            group_messages: 1,
            ..Erased::default()
        },
        "the sender it was appended with"
    );
    let left = store.message(&r.pn, &placeholder.id).await.unwrap();
    if store.erasure_mode() == ErasureMode::Delete {
        assert_eq!(left, None);
    } else {
        let left = left.unwrap();
        assert_eq!(
            (left.kind.as_str(), left.text, left.payload),
            (StoredMessage::ERASED, None, serde_json::json!({}))
        );
    }
    assert_eq!(summary(store, &group).await.and_then(|s| s.last_text), None);
    assert!(
        store
            .erase_all(&r.pn, &["US.9".to_owned()])
            .await
            .unwrap()
            .is_empty(),
        "erased once"
    );
}

/// An erasure deletes every record of one key on one number, and nothing
/// else.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn erase_deletes_every_record_of_one_contact_on_one_number<S: ConversationStore + ?Sized>(
    store: &S,
) {
    let r = Run::new("erase");
    let other = Run::new("erase-other");
    let key = r.key("US.9");
    let neighbour = r.key("US.1");
    let elsewhere = other.key("US.9");
    for (k, local) in [
        (&key, "erased"),
        (&neighbour, "neighbour"),
        (&elsewhere, "elsewhere"),
    ] {
        let run = if k.phone_number_id == r.pn {
            &r
        } else {
            &other
        };
        record_everything(store, run, k, local).await;
    }
    // The synced contacts naming the erased key: as their key, BSUID or
    // parent BSUID; and some that do not, here and on the other number.
    let named = [
        r.contact("US.9", 1),
        StoredContact {
            user_id: Some(UserId::new("US.9")),
            ..r.contact("16505550009", 1)
        },
        StoredContact {
            parent_user_id: Some(UserId::new("US.9")),
            ..r.contact("US.7", 1)
        },
    ];
    let kept = [
        StoredContact {
            phone_number: Some("16505550001".to_owned()),
            user_id: Some(UserId::new("US.1")),
            ..r.contact("US.1", 1)
        },
        other.contact("US.9", 1),
    ];
    for c in named.iter().chain(&kept) {
        assert!(store.put_contact(c.clone()).await.unwrap());
    }
    // The links naming the key, on either side; one that does not, and
    // the same one on the other number.
    for l in [
        link(&r, "US.8", "US.9", 1),
        link(&r, "US.9", "US.10", 2),
        link(&r, "US.1", "US.2", 1),
        link(&other, "US.8", "US.9", 1),
    ] {
        assert!(store.link_identity(l).await.unwrap());
    }
    let neighbour_before = recorded(store, &neighbour).await;
    let elsewhere_before = recorded(store, &elsewhere).await;
    let erased_before = recorded(store, &key).await;
    assert_eq!(erased_before.messages.len(), 5);
    let ids: Vec<MessageId> = erased_before
        .messages
        .iter()
        .map(|m| m.id.clone())
        .collect();

    assert_eq!(
        store.erase(&key).await.unwrap(),
        Erased {
            messages: 5,
            conversations: 1,
            window_events: 2,
            thread_owners: 1,
            contacts: 3,
            identity_links: 2,
            group_messages: 0,
        },
        "every record of the key: live, synced, placeholder, failed and tombstone messages, \
         the summary, two window events, the owner, three contacts, two links"
    );
    assert!(store.identity_links(&key).await.unwrap().is_empty());
    assert_eq!(
        store.identity_links(&r.key("US.1")).await.unwrap(),
        [link(&r, "US.1", "US.2", 1)],
        "a link not naming the key stays"
    );
    assert_eq!(
        store.identity_links(&elsewhere).await.unwrap(),
        [link(&other, "US.8", "US.9", 1)],
        "and the same link on another number"
    );
    let after = recorded(store, &key).await;
    assert!(after.messages.is_empty(), "no message left");
    assert_eq!(after.summary, None, "no summary left");
    assert!(after.window_events.is_empty(), "no window event left");
    assert_eq!(after.owner, None, "no ownership left");
    assert_eq!(store.last_inbound_at(&key).await.unwrap(), None);
    for id in &ids {
        assert_eq!(
            store.message(&r.pn, id).await.unwrap(),
            None,
            "{id}: deleted"
        );
    }
    for c in &named {
        assert_eq!(store.contact(&c.key).await.unwrap(), None, "{}", c.key);
    }
    assert_eq!(
        store.contacts(&r.pn, None, 100).await.unwrap(),
        [kept[0].clone()],
        "the other contacts of the number stay"
    );
    assert_eq!(
        store.contacts(&other.pn, None, 100).await.unwrap(),
        [kept[1].clone()],
        "the same contact on another number stays"
    );
    recorded(store, &neighbour)
        .await
        .assert_same(&neighbour_before, "another contact of the number");
    recorded(store, &elsewhere)
        .await
        .assert_same(&elsewhere_before, "the same contact on another number");

    // Deleted, not hidden: the ids are free again.
    assert!(
        store
            .append(erased_before.messages[0].clone())
            .await
            .unwrap(),
        "an erased message id can be recorded again"
    );
    assert!(
        store
            .record_window_event(erased_before.window_events[0].clone())
            .await
            .unwrap(),
        "an erased window event id can be recorded again"
    );
    assert_eq!(
        store.erase(&key).await.unwrap(),
        Erased {
            messages: 1,
            conversations: 1,
            window_events: 1,
            ..Erased::default()
        }
    );
    assert!(
        store.erase(&key).await.unwrap().is_empty(),
        "erasing again deletes nothing"
    );
    assert!(store.erase(&r.key("nobody")).await.unwrap().is_empty());

    // A person known by their phone number: the contact that names it goes.
    let by_phone = StoredContact {
        phone_number: Some("16505550005".to_owned()),
        user_id: Some(UserId::new("US.5")),
        ..r.contact("US.5", 1)
    };
    assert!(store.put_contact(by_phone.clone()).await.unwrap());
    assert_eq!(
        store.erase(&r.key("16505550005")).await.unwrap(),
        Erased {
            contacts: 1,
            ..Erased::default()
        }
    );
    assert_eq!(store.contact(&by_phone.key).await.unwrap(), None);
    assert_eq!(
        store.contacts(&r.pn, None, 100).await.unwrap(),
        [kept[0].clone()]
    );

    // A removal kept under the key (to refuse a late sync) goes with it;
    // another key's stays.
    let (removed, other_removed) = (r.key("US.4"), r.key("US.3"));
    assert!(store.put_contact(r.contact("US.4", 1)).await.unwrap());
    assert!(store.remove_contact(&removed, at(5)).await.unwrap());
    assert!(!store.remove_contact(&other_removed, at(5)).await.unwrap());
    assert!(!store.put_contact(r.contact("US.4", 4)).await.unwrap());
    assert_eq!(
        store.erase(&removed).await.unwrap(),
        Erased {
            contacts: 1,
            ..Erased::default()
        },
        "the kept removal is the key's record"
    );
    assert!(
        store.put_contact(r.contact("US.4", 4)).await.unwrap(),
        "nothing of the key is left to refuse a sync"
    );
    assert!(
        !store.put_contact(r.contact("US.3", 4)).await.unwrap(),
        "another key's removal stays"
    );
}

/// The first instant a purge case keeps: records before it are the purge
/// cases' own (every other case records around 2026-09-24).
const EPOCH: OffsetDateTime = datetime!(2000-01-01 0:00 UTC);

/// Purge by age deletes exactly the records older than the cutoff.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn purge_deletes_exactly_what_is_older<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("purge");
    let other = Run::new("purge-other");
    // "old": every message before the cutoff, at(0); "mixed": before, at
    // and after it.
    let (old, mixed) = (r.key("old"), r.key("mixed"));
    store
        .append(r.msg("old", "o1", Direction::Inbound, -20, "old"))
        .await
        .unwrap();
    store
        .append(r.msg("old", "o2", Direction::Outbound, -10, "older reply"))
        .await
        .unwrap();
    store
        .append(r.msg("mixed", "m1", Direction::Inbound, -5, "before"))
        .await
        .unwrap();
    synced(
        store,
        r.msg("mixed", "m2", Direction::Inbound, -1, "synced before"),
    )
    .await
    .unwrap();
    let m3 = r.msg("mixed", "m3", Direction::Inbound, 0, "at the cutoff");
    let m4 = r.msg("mixed", "m4", Direction::Outbound, 5, "after");
    store.append(m3.clone()).await.unwrap();
    store.append(m4.clone()).await.unwrap();
    // "edge": its latest message is exactly at the cutoff.
    let edge = r.key("edge");
    store
        .append(r.msg("edge", "e1", Direction::Inbound, -3, "before"))
        .await
        .unwrap();
    let e2 = r.msg("edge", "e2", Direction::Inbound, 0, "at the cutoff");
    store.append(e2.clone()).await.unwrap();
    store
        .revoke(&mixed, &r.id("t-3"), Direction::Inbound, at(-3))
        .await
        .unwrap();
    store
        .revoke(&mixed, &r.id("t7"), Direction::Inbound, at(7))
        .await
        .unwrap();
    let kept_event = r.event("mixed", "e0", WindowEventKind::StandbyMessage, at(0));
    for event in [
        r.event("old", "e-30", WindowEventKind::CustomerCall, at(-30)),
        r.event("mixed", "e-2", WindowEventKind::CustomerCall, at(-2)),
        kept_event.clone(),
    ] {
        assert!(store.record_window_event(event).await.unwrap());
    }
    let kept_owner = ownership(ThreadOwner::ThisApp, None, at(0));
    store
        .set_thread_owner(&old, ownership(ThreadOwner::AnotherApp, None, at(-1)))
        .await
        .unwrap();
    store
        .set_thread_owner(&mixed, kept_owner.clone())
        .await
        .unwrap();
    let address_book = r.contact("US.1", -100);
    store.put_contact(address_book.clone()).await.unwrap();
    let link = IdentityLink::new(r.pn.clone(), "US.0", "US.1", at(-100));
    assert!(store.link_identity(link.clone()).await.unwrap());
    // Removals kept to refuse a late sync: one before the cutoff, one at it.
    let (removed_before, removed_at) = (r.key("US.2"), r.key("US.3"));
    assert!(!store.remove_contact(&removed_before, at(-1)).await.unwrap());
    assert!(!store.remove_contact(&removed_at, at(0)).await.unwrap());
    // The same shape on another number, older still.
    let other_key = other.key("old");
    store
        .append(other.msg("old", "o1", Direction::Inbound, -20, "old"))
        .await
        .unwrap();
    store
        .record_window_event(other.event("old", "e", WindowEventKind::CustomerCall, at(-20)))
        .await
        .unwrap();
    store
        .set_thread_owner(&other_key, ownership(ThreadOwner::ThisApp, None, at(-20)))
        .await
        .unwrap();
    store
        .remove_contact(&other.key("US.9"), at(-20))
        .await
        .unwrap();
    let mixed_summary = summary(store, &mixed).await.unwrap();
    let edge_summary = summary(store, &edge).await.unwrap();
    let other_before = recorded(store, &other_key).await;

    assert_eq!(
        store.purge_before(Some(&r.pn), at(0)).await.unwrap(),
        Purged {
            messages: 6,
            conversations: 1,
            window_events: 2,
            thread_owners: 1,
            contact_removals: 1,
        },
        "o1, o2, m1, m2, e1 and a tombstone; the summary of \"old\"; two window events; one \
         owner; one contact removal"
    );
    assert_eq!(
        store.messages(&edge, None, 10).await.unwrap(),
        std::slice::from_ref(&e2),
        "a message at the cutoff stays"
    );
    assert_eq!(
        summary(store, &edge).await,
        Some(edge_summary),
        "and so does the summary of a conversation whose latest message is at the cutoff"
    );
    let texts: Vec<Option<String>> = store
        .messages(&mixed, None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.text)
        .collect();
    assert_eq!(
        texts,
        [None, m4.text.clone(), m3.text.clone()],
        "the tombstone at 7, m4 and m3 (at the cutoff) stay"
    );
    assert_eq!(
        summary(store, &mixed).await,
        Some(mixed_summary),
        "a conversation keeping its latest message keeps its summary as it was"
    );
    assert!(store.messages(&old, None, 10).await.unwrap().is_empty());
    assert_eq!(
        summary(store, &old).await,
        None,
        "a conversation whose latest message went loses its summary (and its preview)"
    );
    assert_eq!(store.message(&r.pn, &r.id("m1")).await.unwrap(), None);
    assert_eq!(
        store.window_events(&mixed, None, 10).await.unwrap(),
        std::slice::from_ref(&kept_event)
    );
    assert!(
        store
            .window_events(&old, None, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.thread_owner(&old).await.unwrap(), None);
    assert_eq!(
        store.thread_owner(&mixed).await.unwrap().as_ref(),
        Some(&kept_owner)
    );
    assert_eq!(
        store.contact(&address_book.key).await.unwrap().as_ref(),
        Some(&address_book),
        "synced contacts are not history"
    );
    assert_eq!(
        store.identity_links(&r.key("US.1")).await.unwrap(),
        [link],
        "nor are identity links"
    );
    assert!(
        !store.put_contact(r.contact("US.3", -1)).await.unwrap(),
        "a removal at the cutoff is kept, and refuses an older sync"
    );
    assert!(
        store.put_contact(r.contact("US.2", -2)).await.unwrap(),
        "a removal before the cutoff is purged: nothing refuses the sync"
    );
    recorded(store, &other_key)
        .await
        .assert_same(&other_before, "another number, under a purge scoped to one");
    assert!(
        store
            .purge_before(Some(&r.pn), at(0))
            .await
            .unwrap()
            .is_empty(),
        "purging again deletes nothing"
    );

    // Every number: records around the purge cases' own epoch.
    let all = Run::new("purge-all");
    let all_key = all.key("c");
    let ancient = StoredMessage {
        timestamp: EPOCH - time::Duration::seconds(1),
        ..all.msg("c", "ancient", Direction::Inbound, 0, "ancient")
    };
    let y2k = StoredMessage {
        timestamp: EPOCH,
        ..all.msg("c", "y2k", Direction::Inbound, 0, "at the cutoff")
    };
    store.append(ancient.clone()).await.unwrap();
    store.append(y2k.clone()).await.unwrap();
    let old_event = all.event(
        "c",
        "ancient",
        WindowEventKind::CustomerCall,
        EPOCH - time::Duration::seconds(1),
    );
    let epoch_event = all.event("c", "y2k", WindowEventKind::CustomerCall, EPOCH);
    store.record_window_event(old_event).await.unwrap();
    store
        .record_window_event(epoch_event.clone())
        .await
        .unwrap();
    // Counts are not checked: a concurrent run of this suite may purge
    // these first.
    store.purge_before(None, EPOCH).await.unwrap();
    assert_eq!(
        store.messages(&all_key, None, 10).await.unwrap(),
        [y2k],
        "before the cutoff goes, at it stays"
    );
    assert_eq!(
        store.window_events(&all_key, None, 10).await.unwrap(),
        [epoch_event]
    );
    assert_eq!(
        store.messages(&mixed, None, 10).await.unwrap().len(),
        3,
        "newer records of every number stay"
    );
    recorded(store, &other_key)
        .await
        .assert_same(&other_before, "another number's newer records");

    // A number purged whole (a purge on unbind, design D10).
    assert_eq!(
        store
            .purge_before(Some(&other.pn), at(1_000_000))
            .await
            .unwrap(),
        Purged {
            messages: 1,
            conversations: 1,
            window_events: 1,
            thread_owners: 1,
            contact_removals: 1,
        }
    );
    let gone = recorded(store, &other_key).await;
    assert!(gone.messages.is_empty() && gone.summary.is_none());
    assert!(gone.window_events.is_empty() && gone.owner.is_none());
}

/// `apply_retention` purges by the store's own `retention()`.
///
/// Its clock is set so that a retention's cutoff is [`EPOCH`], the purge
/// cases' own: what it purges across the store is what they purge, and
/// what it keeps no concurrent run of this suite purges.
async fn apply_retention_follows_the_retention<S: ConversationStore + ?Sized>(store: &S) {
    let r = Run::new("retention");
    let key = r.key("c");
    let second = time::Duration::seconds(1);
    let retention = store.retention();
    // `now - retention == EPOCH`, when the store keeps a retention.
    let now = retention
        .cutoff(EPOCH)
        .and_then(|earlier| EPOCH.checked_add(EPOCH - earlier));
    if let Some(now) = now {
        assert_eq!(retention.cutoff(now), Some(EPOCH));
        let older = StoredMessage {
            timestamp: EPOCH - second,
            ..r.msg("c", "older", Direction::Inbound, 0, "older")
        };
        let at_cutoff = StoredMessage {
            timestamp: EPOCH,
            ..r.msg("c", "at", Direction::Inbound, 0, "at the cutoff")
        };
        store.append(older).await.unwrap();
        store.append(at_cutoff.clone()).await.unwrap();
        // Counts are not checked: a concurrent run may purge first.
        store.apply_retention(now).await.unwrap();
        assert_eq!(
            store.messages(&key, None, 10).await.unwrap(),
            [at_cutoff],
            "the retention's cutoff applies: before it goes, at it stays"
        );
    } else {
        let first = StoredMessage {
            timestamp: EPOCH,
            ..r.msg("c", "first", Direction::Inbound, 0, "kept")
        };
        let later = StoredMessage {
            timestamp: EPOCH + second,
            ..r.msg("c", "later", Direction::Inbound, 0, "kept too")
        };
        store.append(first.clone()).await.unwrap();
        store.append(later.clone()).await.unwrap();
        assert!(
            store
                .apply_retention(datetime!(2100-01-01 0:00 UTC))
                .await
                .unwrap()
                .is_empty(),
            "a store that keeps everything purges nothing"
        );
        assert_eq!(
            store.messages(&key, None, 10).await.unwrap(),
            [later, first],
            "and deletes nothing"
        );
    }
}
