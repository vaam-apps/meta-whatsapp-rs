//! Reference code for the `meta-whatsapp-rs-bot` skill: a WhatsApp bot on
//! Cloud API webhooks — commands with prefixes, aliases, guards and
//! cooldowns, a plugin per feature, middleware, Markdown replies, the
//! generated help and Meta's command menu, and replies recorded in the CMS
//! inbox.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`). It needs the `bot` feature.

use std::sync::Arc;
use std::time::Duration;

use meta_whatsapp_rs::bot::{
    AccessList, Bot, ClientOutbound, Command, Ctx, Logging, MarkRead, Middleware, Next, Outbound,
    Plugin, Registrar, async_trait,
};
use meta_whatsapp_rs::core::clock::SystemClock;
use meta_whatsapp_rs::core::error::{SinkError, ValidationError};
use meta_whatsapp_rs::prelude::*;

/// One feature per plugin: its commands land in its help section.
#[derive(Debug)]
pub struct Orders;

#[async_trait]
impl Plugin for Orders {
    fn name(&self) -> &'static str {
        "orders"
    }

    fn category(&self) -> Option<&str> {
        Some("Orders")
    }

    async fn setup(&self, registrar: &mut Registrar) -> meta_whatsapp_rs::Result<()> {
        registrar.command(
            Command::new("status", |ctx: Ctx| async move {
                let Some(order) = ctx.args().get(0) else {
                    ctx.reply("Usage: /status <order number>").await?;
                    return Ok(());
                };
                // Markdown in, WhatsApp formatting out, split at 4096 characters.
                ctx.reply_markdown(&format!("**Order {order}** has shipped."))
                    .await?;
                Ok(())
            })
            .alias("s")
            .usage("<order number>") // shown after the name in the help
            .description("Where is my order?")
            .cooldown(Duration::from_secs(10)) // per user, per business number
            .payload("order_status"), // a reply button or list row with this id runs it too
        );
        Ok(())
    }
}

/// Commands for the shop's owners only, left out of the help and the menu.
#[derive(Debug)]
pub struct Admin;

#[async_trait]
impl Plugin for Admin {
    fn name(&self) -> &'static str {
        "admin"
    }

    fn hidden(&self) -> bool {
        true
    }

    async fn setup(&self, registrar: &mut Registrar) -> meta_whatsapp_rs::Result<()> {
        registrar.command(
            Command::new("broadcast", |ctx: Ctx| async move {
                ctx.reply("Owners only.").await?;
                Ok(())
            })
            .owner_only(),
        );
        Ok(())
    }
}

/// Runs for every event after the command match (`ctx.invocation()` is
/// known) and before the command's guards; not calling `next` stops the
/// event there.
#[derive(Debug)]
pub struct OnlyOurNumber(pub PhoneNumberId);

#[async_trait]
impl Middleware for OnlyOurNumber {
    async fn handle(&self, ctx: Ctx, next: Next<'_>) -> meta_whatsapp_rs::Result<()> {
        if ctx.phone_number_id() == Some(&self.0) {
            next.run(ctx).await
        } else {
            Ok(()) // another merchant's number: not this bot's business
        }
    }
}

/// The bot: the defaults, and the parts this shop sets.
pub async fn build_bot(
    client: Client,
    kv: Arc<dyn KvStore>,
    number: PhoneNumberId,
) -> meta_whatsapp_rs::Result<Bot> {
    Bot::builder()
        .client(client) // replies go out with this client's token
        .prefixes(["/", "!"]) // `/` is what Meta's command menu sends
        .access(AccessList::new().owner("US.13491208655302741918")) // BSUIDs, not phone numbers
        .cooldown_store(kv, Arc::new(SystemClock)) // shared by every instance on the store
        .middleware(Logging) // kinds and durations, never content
        .middleware(MarkRead::with_typing_indicator())
        .middleware(OnlyOurNumber(number))
        .plugin(Orders)
        .plugin(Admin)
        .help_command() // `/help`, from the commands not hidden
        .build() // async: the plugins' `setup` run here
        .await
}

/// The bot is an `EventSink`: behind the webhook handler, with dedup so
/// Meta's retries never run a command twice.
pub fn webhook_handler(
    bot: Bot,
    app_secret: AppSecret,
    verify_token: VerifyToken,
    kv: Arc<dyn KvStore>,
) -> meta_whatsapp_rs::Result<WebhookHandler> {
    let verifier = SignatureVerifier::new(vec![app_secret])?;
    let handler = WebhookHandler::builder(verifier, verify_token, Arc::new(bot))
        .dedup(DedupGuard::new(kv)) // Meta retries for 7 days: a retry must not run a command twice
        .build();
    Ok(handler)
}

/// Meta's command menu for the number, from the visible commands.
pub async fn publish_menu(
    bot: &Bot,
    client: &Client,
    number: PhoneNumberId,
) -> meta_whatsapp_rs::Result<()> {
    bot.sync_command_menu(client, number).await // at most 30 commands, each with a description
}

/// The bot's replies through the CMS inbox, so they land in its history
/// (and its 24-hour window check applies); read receipts through the client.
#[derive(Debug, Clone)]
pub struct InboxOutbound {
    pub inbox: Inbox,
    pub receipts: ClientOutbound,
}

#[async_trait]
impl Outbound for InboxOutbound {
    async fn send(
        &self,
        from: &PhoneNumberId,
        message: &OutboundMessage,
    ) -> meta_whatsapp_rs::Result<SendResponse> {
        // The inbox keys a conversation as the bot addresses it: the
        // group, else the BSUID, else the phone number without `+`.
        let contact = match &message.recipient {
            Recipient::Group(group) => group.to_string(),
            Recipient::User(user) => user.to_string(),
            Recipient::Phone(phone) => phone.trim_start_matches('+').to_owned(),
            _ => return Err(ValidationError::new("recipient", "not a conversation").into()),
        };
        let key = ConversationKey::new(from.clone(), contact); // another number: refused
        self.inbox.send(&key, message.clone()).await
    }

    async fn mark_read(
        &self,
        from: &PhoneNumberId,
        message_id: &MessageId,
        typing_indicator: bool,
    ) -> meta_whatsapp_rs::Result<()> {
        self.receipts
            .mark_read(from, message_id, typing_indicator)
            .await
    }
}

/// The inbox records each event, then the bot handles it: a reply checks
/// the window against the message it answers, so that message must be
/// stored first (a `FanoutSink` delivers to both at once).
#[derive(Debug)]
pub struct InboxThenBot {
    pub inbox: InboxSink,
    pub bot: Bot,
}

#[async_trait]
impl EventSink<WebhookEvent> for InboxThenBot {
    async fn deliver(&self, event: WebhookEvent) -> Result<(), SinkError> {
        self.inbox.deliver(event.clone()).await?;
        self.bot.deliver(event).await
    }
}

/// A bot whose replies are in the merchant's inbox, behind one sink:
/// `inbox` and `client` carry the merchant's token, and `conversations`
/// is the store the inbox reads.
pub async fn bot_in_the_inbox(
    inbox: Inbox,
    client: Client,
    conversations: Arc<dyn ConversationStore>,
    kv: Arc<dyn KvStore>,
) -> meta_whatsapp_rs::Result<InboxThenBot> {
    let outbound = InboxOutbound {
        inbox,
        receipts: ClientOutbound::new(client),
    };
    let bot = Bot::builder()
        .outbound(outbound)
        .cooldown_store(kv, Arc::new(SystemClock))
        .middleware(MarkRead::with_typing_indicator())
        .plugin(Orders)
        .help_command()
        .build()
        .await?;
    Ok(InboxThenBot {
        inbox: InboxSink::new(conversations),
        bot,
    })
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::adapters::store::{MemoryConversationStore, MemoryKvStore};
    use meta_whatsapp_rs::core::clock::ManualClock;
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use meta_whatsapp_rs::webhooks::WebhookPayload;
    use serde_json::json;
    use time::OffsetDateTime;

    use super::*;

    const NUMBER: &str = "106540352242922";
    const USER: &str = "US.10000000000000000001";

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TEST-TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .expect("a transport is set")
    }

    /// A text message, shaped like Meta's documented webhook.
    fn text(body: &str) -> WebhookEvent {
        let payload = json!({"object": "whatsapp_business_account", "entry": [{
            "id": "102290129340398", "changes": [{"field": "messages", "value": {
                "messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": NUMBER},
                "contacts": [{"profile": {"name": "Pablo M."}, "wa_id": "16505551234",
                    "user_id": USER}],
                "messages": [{"from": "16505551234", "from_user_id": USER,
                    "id": "wamid.TEST", "timestamp": "1750030073",
                    "type": "text", "text": {"body": body}}]}}]}]});
        WebhookPayload::from_slice(payload.to_string().as_bytes())
            .unwrap()
            .into_events()
            .remove(0)
    }

    fn sent() -> serde_json::Value {
        json!({"messaging_product": "whatsapp", "contacts": [],
            "messages": [{"id": "wamid.REPLY"}]})
    }

    #[tokio::test]
    async fn a_command_marks_read_then_replies_by_bsuid() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true})); // read receipt + typing
        t.push_json(200, sent());
        let bot = build_bot(client(&t), Arc::new(MemoryKvStore::new()), NUMBER.into())
            .await
            .unwrap();

        bot.deliver(text("!S 1234")).await.unwrap();

        let reply = t.last_request().unwrap();
        assert_eq!(reply.path(), format!("/v25.0/{NUMBER}/messages"));
        assert_eq!(
            reply.json(),
            Some(json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "recipient": USER,
                "context": {"message_id": "wamid.TEST"},
                "type": "text",
                "text": {"body": "*Order 1234* has shipped."}
            }))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn the_help_and_the_menu_leave_hidden_commands_out() {
        let t = ScriptedTransport::new();
        let bot = build_bot(client(&t), Arc::new(MemoryKvStore::new()), NUMBER.into())
            .await
            .unwrap();
        assert_eq!(
            bot.help(),
            "*Orders*\n/status, /s <order number> — Where is my order?\n\n\
             *General*\n/help — Show the commands"
        );
        let menu = bot.command_menu().unwrap();
        let names: Vec<_> = menu.iter().map(|c| c.command_name.as_str()).collect();
        assert_eq!(names, ["status", "help"]);

        // A non-owner's owner-only command is refused without a word: the
        // read receipt is the only request.
        t.push_json(200, json!({"success": true}));
        bot.deliver(text("/broadcast")).await.unwrap();
        let requests = t.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].json().unwrap()["status"], "read");
        assert_eq!(t.remaining(), 0);
    }

    /// The customer's command and the bot's reply are both in the inbox's
    /// history of that conversation.
    #[tokio::test]
    async fn the_reply_lands_in_the_inbox() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true})); // read receipt + typing
        t.push_json(200, sent());
        let conversations: Arc<dyn ConversationStore> = Arc::new(MemoryConversationStore::new());
        // The inbox's window check, a minute after the message was sent.
        let now = OffsetDateTime::from_unix_timestamp(1_750_030_073 + 60).unwrap();
        let inbox = Inbox::new(client(&t), NUMBER, Arc::clone(&conversations))
            .with_clock(Arc::new(ManualClock::new(now)));
        let sink = bot_in_the_inbox(
            inbox.clone(),
            client(&t),
            conversations,
            Arc::new(MemoryKvStore::new()),
        )
        .await
        .unwrap();

        sink.deliver(text("/status 1234")).await.unwrap();

        assert_eq!(
            t.last_request().unwrap().json().unwrap()["text"]["body"],
            "*Order 1234* has shipped."
        );
        assert_eq!(t.remaining(), 0);
        let history = inbox.history(&inbox.key(USER), None, 10).await.unwrap();
        let texts: Vec<_> = history.iter().filter_map(|m| m.text.as_deref()).collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
        assert!(texts.contains(&"/status 1234"));
        assert!(texts.contains(&"*Order 1234* has shipped."));
    }
}
