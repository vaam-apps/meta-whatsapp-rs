//! Roadmap L8's decisive test for the inbox: `InboxSink` keeps the
//! coexistence address book (`smb_app_state_sync`) through the
//! `ConversationStore` port, and the synced contacts read back from the
//! store (`Inbox::contacts`, `Inbox::contact`). Each scenario runs on the
//! memory store and, with the `sqlite` feature, on an in-memory SQLite
//! store. Webhooks are Meta's example payloads
//! (`meta-whatsapp-webhooks/tests/fixtures`).

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "memory")]

use std::collections::BTreeSet;
use std::sync::Arc;

use meta_whatsapp_rs::adapters::store::MemoryConversationStore;
use meta_whatsapp_rs::core::store::StoredContact;
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_rs::inbox::{RecordingSwitches, synced_contact_key};
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::webhooks::{WebhookEvent, WebhookPayload};
use pretty_assertions::assert_eq;
use time::OffsetDateTime;

/// `webhooks/reference/smb_app_state_sync`: Pablo Morales added, by phone
/// number only, at 1739321024.
const ADD_BY_PHONE: &str =
    include_str!("../../meta-whatsapp-webhooks/tests/fixtures/fields/smb_app_state_sync.json");
/// `business-scoped-user-ids`, `smb_app_state_sync`: Sheena Nelson added
/// with her BSUID, parent BSUID and username at 1767168700, then a removal
/// naming `US.…919` and the phone number `16505551234` at 1767168701.
const ADD_AND_REMOVE_BY_BSUID: &str =
    include_str!("../../meta-whatsapp-webhooks/tests/fixtures/bsuid/smb_app_state_sync_bsuid.json");

const PNID: &str = "106540352242922";
const PHONE: &str = "16505551234";
const SHEENA: &str = "US.13491208655302741918";

fn at(unix: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(unix).unwrap()
}

fn events(body: &str) -> Vec<WebhookEvent> {
    let events = WebhookPayload::from_slice(body.as_bytes())
        .unwrap()
        .into_events();
    assert!(!events.is_empty(), "the fixture produced no events");
    events
}

async fn deliver(sink: &InboxSink, body: &str) {
    for event in events(body) {
        sink.deliver(event).await.unwrap();
    }
}

fn inbox(store: &Arc<dyn ConversationStore>) -> Inbox {
    // Reading the address book sends nothing: the script stays empty.
    let client = Client::builder()
        .transport(ScriptedTransport::new())
        .access_token("MERCHANT_TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap();
    Inbox::new(client, PhoneNumberId::new(PNID).unwrap(), store.clone())
}

/// The contacts recorded by Meta's examples read back from the store: an
/// add by phone number, an add by BSUID with every field, and a removal
/// under each key it names, which an older redelivery cannot undo.
async fn synced_contacts_are_read_back(store: Arc<dyn ConversationStore>) {
    let sink = InboxSink::new(store.clone());
    let inbox = inbox(&store);

    deliver(&sink, ADD_BY_PHONE).await;
    let pablo = StoredContact {
        key: inbox.key(PHONE),
        full_name: Some("Pablo Morales".to_owned()),
        first_name: Some("Pablo".to_owned()),
        phone_number: Some(PHONE.to_owned()),
        user_id: None,
        parent_user_id: None,
        username: None,
        synced_at: at(1_739_321_024),
    };
    assert_eq!(
        inbox.contacts(None, 10).await.unwrap(),
        std::slice::from_ref(&pablo)
    );
    assert_eq!(
        inbox.contact(&inbox.key(PHONE)).await.unwrap(),
        Some(pablo.clone())
    );
    assert!(
        inbox.conversations(None, 10).await.unwrap().is_empty(),
        "a contact is not a conversation"
    );

    deliver(&sink, ADD_AND_REMOVE_BY_BSUID).await;
    let sheena = StoredContact {
        key: inbox.key(SHEENA),
        full_name: Some("Sheena Nelson".to_owned()),
        first_name: Some("Sheena".to_owned()),
        phone_number: None,
        user_id: Some(UserId::new(SHEENA)),
        parent_user_id: Some(UserId::new("US.ENT.11815799212886844830")),
        username: Some("realsheenanelson".to_owned()),
        synced_at: at(1_767_168_700),
    };
    assert_eq!(
        inbox.contacts(None, 10).await.unwrap(),
        std::slice::from_ref(&sheena),
        "the removal names the phone number too: Pablo's entry, keyed by it, went"
    );
    assert_eq!(inbox.contact(&inbox.key(PHONE)).await.unwrap(), None);
    assert_eq!(
        inbox.identities(&inbox.key(SHEENA)).await.unwrap(),
        BTreeSet::from([SHEENA.to_owned(), "US.ENT.11815799212886844830".to_owned()]),
        "the stored contact connects her identities"
    );

    // A redelivery of the older add, after the removal: refused.
    deliver(&sink, ADD_BY_PHONE).await;
    assert_eq!(
        inbox.contacts(None, 10).await.unwrap(),
        std::slice::from_ref(&sheena)
    );
    // The same add, edited later: stored again.
    let edited = ADD_BY_PHONE
        .replace("1739321024", "1767168702")
        .replace("Pablo Morales", "Pablo M.");
    deliver(&sink, &edited).await;
    assert_eq!(
        inbox.contacts(None, 10).await.unwrap(),
        [
            StoredContact {
                full_name: Some("Pablo M.".to_owned()),
                synced_at: at(1_767_168_702),
                ..pablo
            },
            sheena
        ],
        "by contact in byte order"
    );
}

/// A change the sink cannot place is skipped, the delivery still succeeds:
/// no time, no usable id, U+0000 in an id, another type or action.
async fn unplaceable_changes_are_skipped(store: Arc<dyn ConversationStore>) {
    let sink = InboxSink::new(store.clone());
    for body in [
        ADD_BY_PHONE.replace(r#""timestamp": "1739321024""#, r#""other": "1""#),
        ADD_BY_PHONE.replace(
            r#""phone_number": "16505551234""#,
            r#""phone_number": " + ""#,
        ),
        ADD_BY_PHONE.replace(
            r#""phone_number": "16505551234""#,
            r#""phone_number": "1650\u0000""#,
        ),
        ADD_BY_PHONE.replace(r#""type": "contact""#, r#""type": "label""#),
        ADD_BY_PHONE.replace(r#""action": "add""#, r#""action": "archive""#),
    ] {
        deliver(&sink, &body).await;
    }
    assert!(
        inbox(&store).contacts(None, 10).await.unwrap().is_empty(),
        "nothing recorded"
    );
    // A phone number is keyed as conversations are: without `+`.
    deliver(
        &sink,
        &ADD_BY_PHONE.replace(
            r#""phone_number": "16505551234""#,
            r#""phone_number": "+16505551234""#,
        ),
    )
    .await;
    let stored = inbox(&store).contacts(None, 10).await.unwrap();
    assert_eq!(
        stored
            .iter()
            .map(|c| (c.key.contact.as_str(), c.phone_number.as_deref()))
            .collect::<Vec<_>>(),
        [(PHONE, Some(PHONE))]
    );
}

/// With the contacts switch off, the sink records no contact, and the rest
/// as before.
async fn the_contacts_switch_turns_recording_off(store: Arc<dyn ConversationStore>) {
    let switches = RecordingSwitches::ALL.contacts(false);
    assert!(!switches.records_contacts() && switches.records_identity_links());
    assert!(RecordingSwitches::ALL.records_contacts());
    assert!(!RecordingSwitches::NONE.records_contacts());
    assert!(RecordingSwitches::NONE.contacts(true).records_contacts());
    let sink = InboxSink::new(store.clone()).with_recording(switches);
    deliver(&sink, ADD_BY_PHONE).await;
    deliver(&sink, ADD_AND_REMOVE_BY_BSUID).await;
    assert!(inbox(&store).contacts(None, 10).await.unwrap().is_empty());
}

/// The public rule is the sink's: BSUID first, else the phone number
/// without `+`.
#[test]
fn the_public_key_rule_is_the_sinks() {
    let pn = PhoneNumberId::new(PNID).unwrap();
    let keys: Vec<Option<String>> = events(ADD_AND_REMOVE_BY_BSUID)
        .into_iter()
        .chain(events(ADD_BY_PHONE))
        .map(|event| {
            let WebhookEvent::AppStateSynced { item, .. } = event else {
                panic!("{:?}", event.kind())
            };
            synced_contact_key(&pn, item.contact.as_ref().unwrap()).map(|k| k.contact)
        })
        .collect();
    assert_eq!(
        keys,
        [
            Some(SHEENA.to_owned()),
            Some("US.13491208655302741919".to_owned()),
            Some(PHONE.to_owned())
        ]
    );
}

fn memory() -> Arc<dyn ConversationStore> {
    Arc::new(MemoryConversationStore::new())
}

#[tokio::test]
async fn memory_synced_contacts_are_read_back() {
    synced_contacts_are_read_back(memory()).await;
}

#[tokio::test]
async fn memory_unplaceable_changes_are_skipped() {
    unplaceable_changes_are_skipped(memory()).await;
}

#[tokio::test]
async fn memory_the_contacts_switch_turns_recording_off() {
    the_contacts_switch_turns_recording_off(memory()).await;
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use meta_whatsapp_rs::adapters::store::SqliteConversationStore;
    use meta_whatsapp_rs::adapters::store::sqlite;

    use super::*;

    async fn store() -> Arc<dyn ConversationStore> {
        let pool = sqlite::memory().await.unwrap();
        sqlite::migrate(&pool).await.unwrap();
        Arc::new(SqliteConversationStore::new(pool))
    }

    #[tokio::test]
    async fn sqlite_synced_contacts_are_read_back() {
        synced_contacts_are_read_back(store().await).await;
    }

    #[tokio::test]
    async fn sqlite_unplaceable_changes_are_skipped() {
        unplaceable_changes_are_skipped(store().await).await;
    }

    #[tokio::test]
    async fn sqlite_the_contacts_switch_turns_recording_off() {
        the_contacts_switch_turns_recording_off(store().await).await;
    }
}
