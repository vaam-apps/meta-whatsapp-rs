//! Reference code for the `meta-whatsapp-rs-bot` skill: a WhatsApp bot on
//! Cloud API webhooks — commands with prefixes, aliases, guards and
//! cooldowns, a plugin per feature, middleware, Markdown replies, the
//! generated help and Meta's command menu.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`). It needs the `bot` feature.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::bot::{
    AccessList, Bot, Command, Ctx, Logging, MarkRead, Middleware, Next, Plugin, Registrar,
};
use meta_whatsapp_rs::core::clock::SystemClock;
use meta_whatsapp_rs::prelude::*;

/// One feature per plugin: its commands land in its help section.
pub struct Orders;

#[async_trait]
impl Plugin for Orders {
    fn name(&self) -> &'static str {
        "orders"
    }

    fn category(&self) -> &'static str {
        "Orders"
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
            .description("Where is my order?")
            .cooldown(Duration::from_secs(10)) // per user, per business number
            .payload("order_status"), // a reply button or list row with this id runs it too
        );
        Ok(())
    }
}

/// Runs before every command and listener; not calling `next` stops the
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

/// The bot: every decision has a default and a trait to swap it with.
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
        .command(
            Command::new("broadcast", |ctx: Ctx| async move {
                ctx.reply("Owners only.").await?;
                Ok(())
            })
            .owner_only()
            .hidden(), // runs, but is in neither the help nor the menu
        )
        .help_command()
        .build()
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

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::adapters::store::MemoryKvStore;
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use meta_whatsapp_rs::webhooks::WebhookPayload;
    use serde_json::json;

    use super::*;

    const NUMBER: &str = "106540352242922";

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
                    "user_id": "US.10000000000000000001"}],
                "messages": [{"from": "16505551234", "from_user_id": "US.10000000000000000001",
                    "id": "wamid.TEST", "timestamp": "1750030073",
                    "type": "text", "text": {"body": body}}]}}]}]});
        WebhookPayload::from_slice(payload.to_string().as_bytes())
            .unwrap()
            .into_events()
            .remove(0)
    }

    #[tokio::test]
    async fn a_command_marks_read_then_replies_by_bsuid() {
        let t = ScriptedTransport::new();
        let sent = json!({"messaging_product": "whatsapp", "contacts": [],
            "messages": [{"id": "wamid.REPLY"}]});
        t.push_json(200, json!({"success": true})); // read receipt + typing
        t.push_json(200, sent);
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
                "recipient": "US.10000000000000000001",
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
            "*Orders*\n/status, /s — Where is my order?\n\n*General*\n/help — Show the commands"
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
}
