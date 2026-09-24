//! Reference code for the `wa-rs-cms-inbox` skill: the merchant's inbox —
//! ownership check, the merchant's token, history, the 24-hour window, and
//! replies that fall back to a template.
//!
//! The full server (webhook endpoint, SSE, bearer-token tenants) is
//! `crates/wa-rs/examples/cms_inbox.rs`. wa-rs compiles this file and runs
//! its tests in its own gate (`crates/wa-rs/tests/skills.rs`).

use std::collections::HashSet;
use std::sync::Arc;

use wa_rs::client::embedded_signup::TokenVault;
use wa_rs::client::messages::{MessageContent, OutboundMessage, Text};
use wa_rs::core::clock::{Clock, SystemClock};
use wa_rs::core::store::StoredMessage;
use wa_rs::prelude::*;

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
pub async fn open_conversation(inbox: &Inbox, contact: &str) -> wa_rs::Result<Vec<StoredMessage>> {
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
) -> wa_rs::Result<SendResponse> {
    let key = inbox.key(contact);
    let content: MessageContent = if inbox.window(&key).await?.is_open(SystemClock.now()) {
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
) -> wa_rs::Result<SendResponse> {
    let key = inbox.key(contact);
    let message = OutboundMessage::new(inbox.recipient(&key), Text::new(body)).reply_to(quoted);
    inbox.send(&key, message).await // any other recipient is refused
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use time::OffsetDateTime;
    use wa_rs::adapters::store::{MemoryConversationStore, MemoryKvStore};
    use wa_rs::client::embedded_signup::{StoredBusinessToken, VaultKey, VaultKeys};
    use wa_rs::core::clock::ManualClock;
    use wa_rs::core::testing::ScriptedTransport;

    use super::*;

    const NUMBER: &str = "106540352242922";
    const CUSTOMER: &str = "US.13491208655302741918";

    async fn record_inbound(store: Arc<dyn ConversationStore>, at: i64) {
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": "messages", "value": {"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": NUMBER},
                "messages": [{"from_user_id": CUSTOMER, "id": format!("wamid.{at}"),
                    "timestamp": at.to_string(), "type": "text", "text": {"body": "Navy?"}}]}}]}]});
        let events = wa_rs::webhooks::WebhookPayload::from_slice(body.to_string().as_bytes())
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
    }
}
