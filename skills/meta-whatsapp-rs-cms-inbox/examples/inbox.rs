//! Reference code for the `meta-whatsapp-rs-cms-inbox` skill: the merchant's inbox —
//! ownership check, the merchant's token, history, the 24-hour window,
//! replies that fall back to a template, Conversation Routing (another
//! app owning the thread), and erasing a customer.
//!
//! The full server (webhook endpoint, SSE, bearer-token tenants) is
//! `crates/meta-whatsapp-rs/examples/cms_inbox.rs`. meta-whatsapp-rs compiles this file and runs
//! its tests in its own gate (`crates/meta-whatsapp-rs/tests/skills.rs`).

use std::collections::HashSet;
use std::sync::Arc;

use meta_whatsapp_rs::client::embedded_signup::TokenVault;
use meta_whatsapp_rs::client::messages::{MessageContent, OutboundMessage, Text};
use meta_whatsapp_rs::core::store::{Erased, StoredMessage};
use meta_whatsapp_rs::inbox::{ReplyChecks, is_thread_owned_elsewhere};
use meta_whatsapp_rs::prelude::*;

/// Why a request to the inbox was refused.
#[derive(Debug)]
pub enum Refused {
    NotYourNumber, // 403: checked before the vault is touched
    NotConnected,  // 404: no merchant onboarded this number
    Upstream(Error),
}

impl From<Error> for Refused {
    fn from(error: Error) -> Self {
        Self::Upstream(error)
    }
}

/// The inbox of `number` for the authenticated tenant, replying as the
/// merchant who connected it.
pub async fn inbox_for(
    owned_numbers: &HashSet<PhoneNumberId>, // YOUR tenant table, for the tenant YOUR auth says is calling
    number: PhoneNumberId,
    platform: &Client,
    vault: &TokenVault,
    store: Arc<dyn ConversationStore>,
) -> Result<Inbox, Refused> {
    // Ownership first: the vault holds every merchant's token.
    if !owned_numbers.contains(&number) {
        return Err(Refused::NotYourNumber);
    }
    let Some(merchant) = vault.get_by_phone_number(&number).await? else {
        return Err(Refused::NotConnected);
    };
    Ok(Inbox::new(
        platform.with_token(merchant.token),
        number,
        store,
    ))
}

/// A conversation: newest first, then mark it read in your store.
pub async fn open_conversation(
    inbox: &Inbox,
    contact: &str,
) -> meta_whatsapp_rs::Result<Vec<StoredMessage>> {
    let key = inbox.key(contact); // the `contact` of a ConversationSummary: BSUID, wa_id or group id
    let page = inbox.history(&key, None, 50).await?; // next page: the last row's (timestamp, id)
    inbox.mark_read(&key).await?; // your unread counter, not WhatsApp's blue ticks
    Ok(page)
}

/// Free text inside the window, an approved template outside it.
pub async fn reply_or_template(
    inbox: &Inbox,
    contact: &str,
    body: &str,
) -> meta_whatsapp_rs::Result<SendResponse> {
    let key = inbox.key(contact);
    // `window_is_open` uses the inbox's own clock: the same check `reply` makes.
    let content: MessageContent = if inbox.window_is_open(&key).await? {
        Text::new(body).into()
    } else {
        TemplateMessage::new("reopen_conversation", "en_US").into() // an approved template
    };
    // Recorded as `Accepted` once Meta accepts it; never retry an `Ok`.
    inbox.reply(&key, content).await
}

/// A quoted reply, or callback data: build the message with `recipient`.
pub async fn quote(
    inbox: &Inbox,
    contact: &str,
    quoted: MessageId,
    body: &str,
) -> meta_whatsapp_rs::Result<SendResponse> {
    let key = inbox.key(contact);
    let message = OutboundMessage::new(inbox.recipient(&key), Text::new(body)).reply_to(quoted);
    inbox.send(&key, message).await // any other recipient is refused
}

/// Free text, unless another app owns the thread under Conversation
/// Routing (an escalation partner took it): then nothing is sent.
pub async fn reply_unless_handled_elsewhere(
    inbox: &Inbox,
    contact: &str,
    body: &str,
) -> meta_whatsapp_rs::Result<Option<SendResponse>> {
    let key = inbox.key(contact);
    match inbox.reply(&key, Text::new(body).into()).await {
        Ok(sent) => Ok(Some(sent)),
        // Refused before any request: show "handled by another app".
        Err(e) if is_thread_owned_elsewhere(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The designated escalation partner's inbox: its service message takes
/// the thread (an implicit `take`), so the local ownership check is off.
pub fn escalation_partner_inbox(inbox: Inbox) -> Inbox {
    inbox.with_reply_checks(ReplyChecks::all().thread_owner(false))
}

/// Erase a customer on the inbox's number: every key the store connects to
/// one of theirs (their BSUID, phone number, an earlier BSUID), in one step.
/// Run it on each of the merchant's numbers, behind `inbox_for`'s check.
pub async fn erase_customer(inbox: &Inbox, contact: &str) -> meta_whatsapp_rs::Result<Erased> {
    let key = inbox.key(contact); // any of their keys on this number
    let ids: Vec<String> = inbox.identities(&key).await?.into_iter().collect();
    inbox.erase_all(&ids).await // `Erased` holds counts only: log those, never the ids
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use meta_whatsapp_rs::adapters::store::{MemoryConversationStore, MemoryKvStore};
    use meta_whatsapp_rs::client::embedded_signup::{StoredBusinessToken, VaultKey, VaultKeys};
    use meta_whatsapp_rs::core::clock::ManualClock;
    use meta_whatsapp_rs::core::store::{
        DeliveryStatus, Direction, IdentityLink, StoredContact, StoredMessage,
    };
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use serde_json::json;
    use time::OffsetDateTime;

    use super::*;

    const NUMBER: &str = "106540352242922";
    const CUSTOMER: &str = "US.13491208655302741918";

    /// Deliver a webhook body to `InboxSink`, as the handler would.
    async fn deliver(store: &Arc<dyn ConversationStore>, body: &serde_json::Value) {
        let events =
            meta_whatsapp_rs::webhooks::WebhookPayload::from_slice(body.to_string().as_bytes())
                .unwrap()
                .into_events();
        for event in events {
            InboxSink::new(store.clone()).deliver(event).await.unwrap();
        }
    }

    async fn record_inbound(store: Arc<dyn ConversationStore>, at: i64) {
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": "messages", "value": {"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": NUMBER},
                "messages": [{"from_user_id": CUSTOMER, "id": format!("wamid.{at}"),
                    "timestamp": at.to_string(), "type": "text", "text": {"body": "Navy?"}}]}}]}]});
        let events =
            meta_whatsapp_rs::webhooks::WebhookPayload::from_slice(body.to_string().as_bytes())
                .unwrap()
                .into_events();
        for event in events {
            InboxSink::new(store.clone()).deliver(event).await.unwrap();
        }
    }

    #[tokio::test]
    async fn another_tenants_number_is_refused_before_the_vault() {
        let kv = Arc::new(MemoryKvStore::new());
        let vault = TokenVault::new(kv, VaultKeys::new(VaultKey::generate("k").unwrap())).unwrap();
        let platform = Client::builder()
            .transport(ScriptedTransport::new())
            .build()
            .unwrap();
        let store: Arc<dyn ConversationStore> = Arc::new(MemoryConversationStore::new());
        let owned = HashSet::from([PhoneNumberId::new("other-number")]);
        let refused = inbox_for(&owned, NUMBER.into(), &platform, &vault, store.clone()).await;
        assert!(matches!(refused, Err(Refused::NotYourNumber)));

        vault
            .store(
                &StoredBusinessToken::new("102290129340398", AccessToken::new("MERCHANT"))
                    .phone_number_ids([NUMBER]),
            )
            .await
            .unwrap();
        let owned = HashSet::from([PhoneNumberId::new(NUMBER)]);
        assert!(
            inbox_for(&owned, NUMBER.into(), &platform, &vault, store)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn replies_inside_the_window_and_templates_outside() {
        let store: Arc<dyn ConversationStore> = Arc::new(MemoryConversationStore::new());
        let now = OffsetDateTime::now_utc().unix_timestamp();
        record_inbound(store.clone(), now - 60).await;
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"messages": [{"id": "wamid.R1"}]}));
        transport.push_json(200, json!({"messages": [{"id": "wamid.R2"}]}));
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("MERCHANT")
            .build()
            .unwrap();
        let inbox = Inbox::new(client.clone(), NUMBER, store.clone());

        reply_or_template(&inbox, CUSTOMER, "Yes, in navy too.")
            .await
            .unwrap();
        assert_eq!(
            transport.last_request().unwrap().json().unwrap()["type"],
            "text"
        );
        assert_eq!(open_conversation(&inbox, CUSTOMER).await.unwrap().len(), 2); // inbound + our reply

        // A day later the window is closed: `reply` alone would refuse text.
        let later = Arc::new(ManualClock::new(
            OffsetDateTime::now_utc() + Duration::from_hours(25),
        ));
        let late = Inbox::new(client, NUMBER, store).with_clock(later);
        let refused = late
            .reply(&late.key(CUSTOMER), Text::new("Hello?").into())
            .await
            .unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::CustomerServiceWindowClosed);
        assert_eq!(transport.remaining(), 1); // nothing sent for the refusal

        // `reply_or_template` sends the approved template instead.
        reply_or_template(&late, CUSTOMER, "Hello?").await.unwrap();
        let sent = transport.last_request().unwrap().json().unwrap();
        assert_eq!(sent["type"], "template");
        assert_eq!(sent["template"]["name"], "reopen_conversation");
        assert_eq!(transport.remaining(), 0);
    }

    /// A customer's call reopens the window (`calling/pricing`); after
    /// `control_taken` another app owns the thread, and only the escalation
    /// partner's inbox sends.
    #[tokio::test]
    async fn calls_reopen_the_window_and_another_app_can_own_the_thread() {
        let store: Arc<dyn ConversationStore> = Arc::new(MemoryConversationStore::new());
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let metadata = json!({"display_phone_number": "15550783881", "phone_number_id": NUMBER});
        // The customer called an hour ago (a user-initiated call's connect).
        deliver(&store, &json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": "calls", "value": {"messaging_product": "whatsapp", "metadata": metadata,
                "calls": [{"id": "wacid.1", "from": "16505551234", "from_user_id": CUSTOMER,
                    "to": "15550783881", "event": "connect", "direction": "USER_INITIATED",
                    "timestamp": (now - 3600).to_string()}]}}]}]}))
        .await;
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"messages": [{"id": "wamid.R1"}]}));
        transport.push_json(200, json!({"messages": [{"id": "wamid.R2"}]}));
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("MERCHANT")
            .build()
            .unwrap();
        let inbox = Inbox::new(client, NUMBER, store.clone());
        let sent = reply_unless_handled_elsewhere(&inbox, CUSTOMER, "We missed your call")
            .await
            .unwrap();
        assert!(sent.is_some(), "the call opened the window");

        // The customer wrote; then the escalation partner took the thread.
        // The handover names the phone number only, which the message
        // linked to the customer's BSUID.
        record_inbound_with_phone(&store, now - 60).await;
        deliver(
            &store,
            &json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": "messaging_handovers", "value": {"messaging_product": "whatsapp",
                "sender": {"phone_number": "16505551234"},
                "recipient": {"phone_number_id": NUMBER, "display_phone_number": "15550783881"},
                "type": "control_taken", "timestamp": (now - 30).to_string(),
                "control_taken": {"new_owner_role": "escalation"}}}]}]}),
        )
        .await;
        let refused = reply_unless_handled_elsewhere(&inbox, CUSTOMER, "Still there?")
            .await
            .unwrap();
        assert!(refused.is_none());
        assert_eq!(transport.remaining(), 1, "nothing sent for the refusal");
        escalation_partner_inbox(inbox)
            .reply(
                &ConversationKey::new(NUMBER, CUSTOMER),
                Text::new("Taking over").into(),
            )
            .await
            .unwrap();
        assert_eq!(transport.remaining(), 0);
    }

    /// An inbound message carrying the phone number and the BSUID: it
    /// links the two (for handovers and erasures).
    async fn record_inbound_with_phone(store: &Arc<dyn ConversationStore>, at: i64) {
        deliver(store, &json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": "messages", "value": {"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": NUMBER},
                "messages": [{"from": "16505551234", "from_user_id": CUSTOMER, "id": format!("wamid.p{at}"),
                    "timestamp": at.to_string(), "type": "text", "text": {"body": "Hello?"}}]}}]}]}))
        .await;
    }

    /// A message of `contact`'s conversation, sent by `from` (a BSUID).
    fn row(id: &str, contact: &str, from: &str) -> StoredMessage {
        StoredMessage {
            id: MessageId::new(id),
            conversation: ConversationKey::new(NUMBER, contact),
            direction: Direction::Inbound,
            kind: "text".to_owned(),
            text: Some("my address is 1 Main St".to_owned()),
            payload: json!({"from": "16505551234", "from_user_id": from,
                "text": {"body": "my address is 1 Main St"}}),
            status: DeliveryStatus::Received,
            timestamp: OffsetDateTime::now_utc(),
            status_at: None,
            error: None,
        }
    }

    #[tokio::test]
    async fn erasing_a_customer_reaches_every_key_and_their_group_messages() {
        let store: Arc<dyn ConversationStore> = Arc::new(MemoryConversationStore::new());
        let now = OffsetDateTime::now_utc().unix_timestamp();
        record_inbound(store.clone(), now - 60).await; // keyed by CUSTOMER, their BSUID
        // A history thread under their phone number, which the address book
        // ties to the BSUID; an earlier BSUID linked to it; a group message.
        let contact = StoredContact {
            key: ConversationKey::new(NUMBER, "16505551234"),
            full_name: Some("Pablo Morales".to_owned()),
            first_name: None,
            phone_number: Some("16505551234".to_owned()),
            user_id: Some(UserId::new(CUSTOMER)),
            parent_user_id: None,
            username: None,
            synced_at: OffsetDateTime::now_utc(),
        };
        store.put_contact(contact).await.unwrap();
        store
            .link_identity(IdentityLink::new(
                NUMBER,
                "US.1",
                CUSTOMER,
                OffsetDateTime::now_utc(),
            ))
            .await
            .unwrap();
        for m in [
            row("wamid.h", "16505551234", CUSTOMER),
            row("wamid.old", "US.1", "US.1"),
            row("wamid.g", "HBgGROUP", CUSTOMER),
        ] {
            store.append(m).await.unwrap();
        }
        let inbox = Inbox::new(
            Client::builder()
                .transport(ScriptedTransport::new())
                .build()
                .unwrap(),
            NUMBER,
            store.clone(),
        );

        let erased = erase_customer(&inbox, CUSTOMER).await.unwrap();
        assert_eq!((erased.messages, erased.group_messages), (3, 1));
        for contact in [CUSTOMER, "16505551234", "US.1"] {
            assert!(open_conversation(&inbox, contact).await.unwrap().is_empty());
        }
        let group = open_conversation(&inbox, "HBgGROUP").await.unwrap();
        assert_eq!(group[0].kind, StoredMessage::ERASED); // kept in place, without content
        assert_eq!(group[0].text, None);
    }
}
