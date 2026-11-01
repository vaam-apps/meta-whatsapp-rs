//! The dead-letter store on the memory `KvStore`: pinned key and record
//! bytes, the count and age bounds, the index, erasure and replay.

use std::sync::Mutex;

use async_trait::async_trait;
use meta_whatsapp_adapters::store::MemoryKvStore;
use meta_whatsapp_core::clock::ManualClock;
use pretty_assertions::assert_eq;
use serde_json::json;
use time::macros::datetime;

use super::*;
use crate::payload::WebhookPayload;

const NUMBER: &str = "106540352242922";
const BSUID: &str = "US.13491208655302741918";

fn clock() -> ManualClock {
    ManualClock::new(datetime!(2026-10-30 12:00 UTC))
}

fn store(kv: &Arc<MemoryKvStore>, clock: &ManualClock, capacity: u64) -> DeadLetters {
    DeadLetters::new(kv.clone())
        .with_clock(Arc::new(clock.clone()))
        .with_capacity(NonZeroU64::new(capacity).unwrap())
}

fn permanent() -> SinkError {
    SinkError::permanent(std::io::Error::other("NUL in an id"))
}

fn number() -> PhoneNumberId {
    PhoneNumberId::new(NUMBER).unwrap()
}

/// An inbound text from `from` (a phone number), optionally with a BSUID.
fn text(id: &str, from: &str, user_id: Option<&str>) -> WebhookEvent {
    let mut contact = json!({"profile": {"name": "Sheena Nelson"}, "wa_id": from});
    let mut message = json!({"from": from, "id": id, "timestamp": "1749416383",
                             "type": "text", "text": {"body": "Does it come in another color?"}});
    if let Some(user_id) = user_id {
        contact["user_id"] = json!(user_id);
        message["from_user_id"] = json!(user_id);
    }
    let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398", "changes": [{
        "field": "messages",
        "value": {"messaging_product": "whatsapp",
                  "metadata": {"display_phone_number": "15550783881", "phone_number_id": NUMBER},
                  "contacts": [contact], "messages": [message]}
    }]}]});
    let mut events = WebhookPayload::from_slice(body.to_string().as_bytes())
        .unwrap()
        .into_events();
    assert_eq!(events.len(), 1);
    events.remove(0)
}

/// Records what it takes; refuses permanently while `permanent`, or
/// transiently while `transient`.
#[derive(Debug, Default)]
struct Sink {
    seen: Mutex<Vec<WebhookEvent>>,
    permanent: Mutex<bool>,
    transient: Mutex<bool>,
}

#[async_trait]
impl EventSink<WebhookEvent> for Sink {
    async fn deliver(&self, event: WebhookEvent) -> Result<(), SinkError> {
        if *self.permanent.lock().unwrap() {
            return Err(permanent());
        }
        if *self.transient.lock().unwrap() {
            return Err(SinkError::Closed);
        }
        self.seen.lock().unwrap().push(event);
        Ok(())
    }
}

/// The namespaces and the keys, byte for byte: changed, they strand the
/// letters (and their erasure) written by the release before. `\x77` is
/// `w`, so that a search-and-replace of a namespace cannot rewrite the pin.
#[test]
fn the_keys_are_pinned_and_hashed() {
    assert_eq!(DEAD_LETTER_NAMESPACE, "\x77a.webhook.deadletter");
    assert_eq!(
        DEAD_LETTER_INDEX_NAMESPACE,
        "\x77a.webhook.deadletter.contact"
    );
    assert_eq!(slot_key(7), StoreKey::new("\x77a.webhook.deadletter", "7"));
    // SHA-256 of `15:106540352242922,23:US.13491208655302741918,`,
    // computed apart from the code.
    let key = index_key(NUMBER, BSUID);
    assert_eq!(key.namespace(), "\x77a.webhook.deadletter.contact");
    assert_eq!(
        key.key(),
        "8db044f4628fe79f0a4f85828b88c04ac38884a022fa73c061e7dd65c1bbc03d"
    );
    // A letter with no business number: `0:,11:16505551234,`.
    assert_eq!(
        index_key("", "16505551234").key(),
        "b64ff25d57b96327a8aea155ebb86b514b8bfdfe60d7b6aedf131e87fc31559c"
    );
    assert!(
        !index_key(NUMBER, "16505551234")
            .to_string()
            .contains("16505551234")
    );
}

/// Version 1 of the three records, field by field, as written and as
/// read back: a letter stored by one release is read (and erased) by the
/// next.
#[tokio::test]
async fn the_record_bytes_are_pinned() {
    let clock = clock();
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let letters = store(&kv, &clock, 1000);
    let event = text("wamid.A", "16505551234", Some(BSUID));
    let id = letters.record(&event, &permanent()).await.unwrap();
    assert_eq!(id, DeadLetterId::new(0));

    let head = kv
        .get(&StoreKey::new("\x77a.webhook.deadletter", "head"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        String::from_utf8(head.value).unwrap(),
        r#"{"v":1,"next":1}"#
    );
    assert_eq!(head.expires_at, None, "the head never expires");

    let stored = kv.get(&slot_key(0)).await.unwrap().unwrap();
    // The event part is serde_json's rendering of its value (key order is
    // serde_json's, which a `preserve_order` feature elsewhere changes).
    let event_json = serde_json::to_string(&serde_json::to_value(&event).unwrap()).unwrap();
    let expected = format!(
        concat!(
            r#"{{"v":1,"id":0,"at":1793361600,"attempts":1,"kind":"message_received","#,
            r#""error":"sink refused the event permanently: NUL in an id","#,
            r#""phone_number_id":"106540352242922","#,
            r#""contacts":["16505551234","US.13491208655302741918"],"#,
            r#""event_bytes":{},"event":{}}}"#
        ),
        event_json.len(),
        event_json
    );
    assert_eq!(String::from_utf8(stored.value.clone()).unwrap(), expected);
    assert_eq!(
        stored.expires_at,
        Some(datetime!(2026-10-30 12:00 UTC) + DEFAULT_MAX_AGE)
    );

    let index = kv.get(&index_key(NUMBER, BSUID)).await.unwrap().unwrap();
    assert_eq!(
        String::from_utf8(index.value).unwrap(),
        r#"{"v":1,"letters":[[0,0]]}"#
    );

    // Read back from the bytes alone.
    let read = letters.get(id).await.unwrap().unwrap();
    assert_eq!(read.at, datetime!(2026-10-30 12:00 UTC));
    assert_eq!(read.kind, "message_received");
    assert_eq!(read.contacts, ["16505551234", BSUID]);
    assert_eq!(read.event, Some(serde_json::to_value(&event).unwrap()));
}

/// Every identifier: a contact with both is indexed under the BSUID and
/// the phone number (an erasure may know only one of them); a phone
/// number without its `+`. The business's own number is never a contact.
#[test]
fn contacts_are_every_identifier_without_the_business_number() {
    let with_bsuid = serde_json::to_value(text("wamid.A", "16505551234", Some(BSUID))).unwrap();
    assert_eq!(contacts_of(&with_bsuid), ["16505551234", BSUID]);
    let without = serde_json::to_value(text("wamid.B", "16505551234", None)).unwrap();
    assert_eq!(contacts_of(&without), ["16505551234"]);

    // An echo: `from` is the business's number, `to_user_id` the customer.
    let echo = json!({"event": "message_echoed", "display_phone_number": "+1 555-078-3881",
                      "echo": {"from": "15550783881", "to_user_id": BSUID, "to": "+16505551234"}});
    assert_eq!(contacts_of(&echo), ["16505551234", BSUID]);

    // A BSUID change names both sides, and the parent BSUID's; a group
    // status its participant.
    let change = json!({"user_id_update": [{"wa_id": "16505551234",
                        "user_id": {"previous": "US.1", "current": "US.2"},
                        "parent_user_id": {"previous": "US.ENT.3", "current": "US.ENT.4"}}]});
    assert_eq!(
        contacts_of(&change),
        ["16505551234", "US.1", "US.2", "US.ENT.3", "US.ENT.4"]
    );
    let group = json!({"status": {"recipient_id": "120363025246125486@g.us",
                       "participant_recipient_id": "16505551236",
                       "recipient_participant_id": "16505551234"}});
    assert_eq!(
        contacts_of(&group),
        ["120363025246125486@g.us", "16505551234", "16505551236"]
    );

    // A handover's sender, an address book entry, a group participant's
    // `input`, and a BSUID in a field no list names.
    let others = json!({"sender": {"phone_number": "16505551237"},
                        "contact": {"phone_number": "16505551238"},
                        "failed_participants": [{"input": "US.5"}],
                        "something_new": "BR.6"});
    assert_eq!(
        contacts_of(&others),
        ["16505551237", "16505551238", "BR.6", "US.5"]
    );
}

/// The count bound: with room for two, a third letter evicts the oldest,
/// and the store never holds more than two.
#[tokio::test]
async fn the_count_bound_evicts_the_oldest() {
    let kv = Arc::new(MemoryKvStore::new());
    let clock = clock();
    let letters = store(&kv, &clock, 2);
    for i in 0..3 {
        let event = text(&format!("wamid.{i}"), "16505551234", None);
        letters.record(&event, &permanent()).await.unwrap();
    }
    let kept: Vec<u64> = letters
        .list()
        .await
        .unwrap()
        .iter()
        .map(|l| l.id.get())
        .collect();
    assert_eq!(kept, [1, 2]);
    assert_eq!(letters.get(DeadLetterId::new(0)).await.unwrap(), None);
    assert!(
        kv.get(&slot_key(2)).await.unwrap().is_none(),
        "no third slot"
    );
    // The index forgets what the ring evicted.
    let index = kv
        .get(&index_key(NUMBER, "16505551234"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        String::from_utf8(index.value).unwrap(),
        r#"{"v":1,"letters":[[1,1],[2,0]]}"#
    );
}

/// The age bound: a letter and its index are gone once the age bound has
/// passed.
#[tokio::test]
async fn the_age_bound_expires_letters_and_their_index() {
    let clock = clock();
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let letters = store(&kv, &clock, 10).with_max_age(Duration::from_hours(24));
    let id = letters
        .record(&text("wamid.A", "16505551234", None), &permanent())
        .await
        .unwrap();
    clock.advance(Duration::from_hours(23));
    assert!(letters.get(id).await.unwrap().is_some());
    clock.advance(Duration::from_hours(2));
    assert_eq!(letters.get(id).await.unwrap(), None);
    assert!(
        kv.get(&index_key(NUMBER, "16505551234"))
            .await
            .unwrap()
            .is_none()
    );
}

/// The size bound: an event over it is recorded without its content, and
/// cannot be replayed.
#[tokio::test]
async fn an_event_over_the_size_bound_is_recorded_without_content() {
    let kv = Arc::new(MemoryKvStore::new());
    let clock = clock();
    let letters = store(&kv, &clock, 10).with_max_event_bytes(64);
    let id = letters
        .record(&text("wamid.A", "16505551234", None), &permanent())
        .await
        .unwrap();
    let letter = letters.get(id).await.unwrap().unwrap();
    assert_eq!(letter.event, None);
    assert!(letter.event_bytes > 64);
    assert_eq!(letter.contacts, ["16505551234"], "still erasable");
    let sink = Sink::default();
    assert_eq!(letters.replay(id, &sink).await.unwrap(), Replay::NoPayload);
    assert!(sink.seen.lock().unwrap().is_empty());
}

/// Erasure reaches every letter of the person on the number, by any of
/// their identities, and nobody else's; another number's are left.
#[tokio::test]
async fn erase_removes_the_contacts_letters_through_the_index() {
    let kv = Arc::new(MemoryKvStore::new());
    let clock = clock();
    let letters = store(&kv, &clock, 10);
    let theirs_bsuid = letters
        .record(&text("wamid.A", "16505551234", Some(BSUID)), &permanent())
        .await
        .unwrap();
    let theirs_phone = letters
        .record(&text("wamid.B", "16505551234", None), &permanent())
        .await
        .unwrap();
    let someone_else = letters
        .record(&text("wamid.C", "4915112345678", None), &permanent())
        .await
        .unwrap();

    let erased = letters
        .erase(&number(), &[BSUID.to_owned(), "16505551234".to_owned()])
        .await
        .unwrap();
    assert_eq!(erased, 2);
    assert_eq!(letters.get(theirs_bsuid).await.unwrap(), None);
    assert_eq!(letters.get(theirs_phone).await.unwrap(), None);
    assert!(letters.get(someone_else).await.unwrap().is_some());
    assert!(kv.get(&index_key(NUMBER, BSUID)).await.unwrap().is_none());
    assert!(
        kv.get(&index_key(NUMBER, "16505551234"))
            .await
            .unwrap()
            .is_none()
    );

    // Another business number's erasure of the same person leaves this
    // number's letters alone.
    let again = letters
        .record(&text("wamid.D", "16505551234", None), &permanent())
        .await
        .unwrap();
    let other = PhoneNumberId::new("999999999999999").unwrap();
    assert_eq!(
        letters
            .erase(&other, &["16505551234".to_owned()])
            .await
            .unwrap(),
        0
    );
    assert!(letters.get(again).await.unwrap().is_some());
}

/// An erasure that knows the person by one identifier only reaches a
/// letter naming them by both: the BSUID alone, or the phone number
/// alone (with or without its `+`). The link between the two may have
/// come with the very event the sink refused, so the conversation store
/// may not know it.
#[tokio::test]
async fn erase_reaches_a_letter_by_either_identifier() {
    let kv = Arc::new(MemoryKvStore::new());
    let clock = clock();
    let letters = store(&kv, &clock, 10);
    for contacts in [vec![BSUID.to_owned()], vec!["+16505551234".to_owned()]] {
        let id = letters
            .record(&text("wamid.A", "16505551234", Some(BSUID)), &permanent())
            .await
            .unwrap();
        assert_eq!(
            letters.erase(&number(), &contacts).await.unwrap(),
            1,
            "{contacts:?}"
        );
        assert_eq!(letters.get(id).await.unwrap(), None, "{contacts:?}");
    }
}

/// A letter whose event names no business number is erased by any
/// number's erasure of the contact.
#[tokio::test]
async fn a_letter_without_a_number_is_erased_by_any_number() {
    let kv = Arc::new(MemoryKvStore::new());
    let clock = clock();
    let letters = store(&kv, &clock, 10);
    let unparsed = WebhookEvent::Unparsed {
        raw: json!({"entry": [{"from": "16505551234"}]}),
        error: "expected a sequence".to_owned(),
    };
    let id = letters.record(&unparsed, &permanent()).await.unwrap();
    let letter = letters.get(id).await.unwrap().unwrap();
    assert_eq!(letter.phone_number_id, None);
    assert!(
        kv.get(&index_key("", "16505551234"))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        letters
            .erase(&number(), &["16505551234".to_owned()])
            .await
            .unwrap(),
        1
    );
    assert_eq!(letters.get(id).await.unwrap(), None);
}

/// A slot the ring reused for someone else is not erased through a stale
/// index entry.
#[tokio::test]
async fn a_stale_index_entry_never_erases_another_letter() {
    let kv = Arc::new(MemoryKvStore::new());
    let clock = clock();
    let letters = store(&kv, &clock, 1);
    letters
        .record(&text("wamid.A", "16505551234", None), &permanent())
        .await
        .unwrap();
    // Evicts letter 0 from slot 0; the first contact's index still lists it.
    let other = letters
        .record(&text("wamid.B", "4915112345678", None), &permanent())
        .await
        .unwrap();
    assert_eq!(
        letters
            .erase(&number(), &["16505551234".to_owned()])
            .await
            .unwrap(),
        0
    );
    assert!(letters.get(other).await.unwrap().is_some());
}

/// A lost head (deleted by hand, evicted by a misconfigured Redis) hands
/// out ids again: a stale index entry then names an id someone else's
/// letter now has. The erasure checks the letter names the contact.
#[tokio::test]
async fn a_reused_id_never_erases_another_contacts_letter() {
    let kv = Arc::new(MemoryKvStore::new());
    let clock = clock();
    let letters = store(&kv, &clock, 10);
    letters
        .record(&text("wamid.A", "16505551234", None), &permanent())
        .await
        .unwrap();
    kv.delete(&StoreKey::new(DEAD_LETTER_NAMESPACE, HEAD_KEY))
        .await
        .unwrap();
    // Id 0 again, in slot 0: the first contact's index still lists `[0, 0]`.
    let other = letters
        .record(&text("wamid.B", "4915112345678", None), &permanent())
        .await
        .unwrap();
    assert_eq!(other, DeadLetterId::new(0));
    assert_eq!(
        letters
            .erase(&number(), &["16505551234".to_owned()])
            .await
            .unwrap(),
        0
    );
    assert!(letters.get(other).await.unwrap().is_some());
}

/// Replay: delivered, the letter goes; refused permanently again, it stays
/// with one more attempt; a transient failure leaves it as it was.
#[tokio::test]
async fn replay_delivers_and_removes_or_keeps_the_letter() {
    let kv = Arc::new(MemoryKvStore::new());
    let clock = clock();
    let letters = store(&kv, &clock, 10);
    let event = text("wamid.A", "16505551234", Some(BSUID));
    let id = letters.record(&event, &permanent()).await.unwrap();
    let sink = Sink::default();

    *sink.transient.lock().unwrap() = true;
    let err = letters.replay(id, &sink).await.unwrap_err();
    assert!(
        matches!(err, meta_whatsapp_core::Error::Sink(SinkError::Closed)),
        "{err}"
    );
    assert_eq!(letters.get(id).await.unwrap().unwrap().attempts, 1);

    *sink.transient.lock().unwrap() = false;
    *sink.permanent.lock().unwrap() = true;
    assert_eq!(
        letters.replay(id, &sink).await.unwrap(),
        Replay::StillPermanent
    );
    assert_eq!(letters.get(id).await.unwrap().unwrap().attempts, 2);

    *sink.permanent.lock().unwrap() = false;
    assert_eq!(letters.replay(id, &sink).await.unwrap(), Replay::Delivered);
    assert_eq!(*sink.seen.lock().unwrap(), [event]);
    assert_eq!(letters.get(id).await.unwrap(), None);
    assert_eq!(letters.replay(id, &sink).await.unwrap(), Replay::Missing);
}

#[tokio::test]
async fn replay_all_replays_every_letter_oldest_first() {
    let kv = Arc::new(MemoryKvStore::new());
    let clock = clock();
    let letters = store(&kv, &clock, 10);
    let a = text("wamid.A", "16505551234", None);
    let b = text("wamid.B", "4915112345678", None);
    letters.record(&a, &permanent()).await.unwrap();
    letters.record(&b, &permanent()).await.unwrap();
    let sink = Sink::default();
    let report = letters.replay_all(&sink).await.unwrap();
    assert_eq!(report.delivered, 2);
    assert_eq!(*sink.seen.lock().unwrap(), [a, b]);
    assert!(letters.list().await.unwrap().is_empty());
}

/// Ids never repeat, and each letter is indexed once per contact.
#[tokio::test]
async fn ids_increase_and_the_debug_shows_no_content() {
    let kv = Arc::new(MemoryKvStore::new());
    let clock = clock();
    let letters = store(&kv, &clock, 10);
    let first = letters
        .record(&text("wamid.A", "16505551234", None), &permanent())
        .await
        .unwrap();
    let second = letters
        .record(&text("wamid.B", "16505551234", None), &permanent())
        .await
        .unwrap();
    assert!(second > first);
    let shown = format!("{:?}", letters.get(first).await.unwrap().unwrap());
    for pii in ["16505551234", "Sheena", "another color", "NUL in an id"] {
        assert!(!shown.contains(pii), "{shown}");
    }
}

#[test]
fn the_error_text_is_cut_at_a_character_boundary() {
    assert_eq!(truncate("abc", 5), "abc");
    assert_eq!(truncate("aé", 2), "a");
    assert_eq!(truncate(&"x".repeat(600), ERROR_TEXT_CAP).len(), 512);
}

/// One letter whose event no longer decodes (written by another release,
/// edited by hand) is counted as failed and kept; the letters after it
/// are still replayed.
#[tokio::test]
async fn replay_all_goes_past_a_letter_that_no_longer_decodes() {
    let kv = Arc::new(MemoryKvStore::new());
    let clock = clock();
    let letters = store(&kv, &clock, 10);
    let bad = letters
        .record(&text("wamid.A", "16505551234", None), &permanent())
        .await
        .unwrap();
    let good = text("wamid.B", "4915112345678", None);
    letters.record(&good, &permanent()).await.unwrap();
    let key = slot_key(bad.get());
    let stored = kv.get(&key).await.unwrap().unwrap();
    let mut record: Record = serde_json::from_slice(&stored.value).unwrap();
    record.event = Some(json!({"event": "no_such_kind"}));
    kv.put(&key, serde_json::to_vec(&record).unwrap(), Expiry::Keep)
        .await
        .unwrap();

    let sink = Sink::default();
    let report = letters.replay_all(&sink).await.unwrap();
    assert_eq!((report.delivered, report.failed), (1, 1));
    assert_eq!(*sink.seen.lock().unwrap(), [good]);
    assert!(letters.get(bad).await.unwrap().is_some(), "kept");
}
