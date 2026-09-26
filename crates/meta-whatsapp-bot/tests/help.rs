//! The generated help text and Meta's command menu
//! (`business-phone-numbers/conversational-components`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use async_trait::async_trait;
use meta_whatsapp_bot::{
    Bot, CategoryHelp, Command, Ctx, HelpFormatter, HelpSection, Plugin, Registrar,
};
use meta_whatsapp_core::Error;
use meta_whatsapp_core::testing::ScriptedTransport;
use pretty_assertions::assert_eq;
use serde_json::json;

use common::{NUMBER, Recording, client, message_id, text_event};

fn noop() -> impl Fn(Ctx) -> std::future::Ready<meta_whatsapp_core::Result<()>> + Send + Sync {
    |_ctx: Ctx| std::future::ready(Ok(()))
}

/// Travel commands, as in the page's examples.
#[derive(Debug)]
struct Travel;

#[async_trait]
impl Plugin for Travel {
    fn name(&self) -> &'static str {
        "travel"
    }
    fn category(&self) -> Option<&str> {
        Some("Travel")
    }
    async fn setup(&self, r: &mut Registrar) -> meta_whatsapp_core::Result<()> {
        r.command(
            Command::new("tickets", noop())
                .alias("t")
                .description("Book flight tickets"),
        )
        .command(Command::new("hotel", noop()).description("Book hotel"))
        .command(
            Command::new("debug", noop())
                .description("Internals")
                .hidden(),
        );
        Ok(())
    }
}

/// Commands no one should see.
#[derive(Debug)]
struct Admin;

#[async_trait]
impl Plugin for Admin {
    fn name(&self) -> &'static str {
        "admin"
    }
    fn hidden(&self) -> bool {
        true
    }
    async fn setup(&self, r: &mut Registrar) -> meta_whatsapp_core::Result<()> {
        r.command(
            Command::new("ban", noop())
                .description("Ban a user")
                .owner_only(),
        );
        Ok(())
    }
}

async fn bot(t: &ScriptedTransport) -> Bot {
    Bot::builder()
        .client(client(t))
        .help_command()
        .plugin(Travel)
        .plugin(Admin)
        .command(
            Command::new("status", noop())
                .description("Order status")
                .category("Orders"),
        )
        .command(Command::new("ping", noop()).menu(false))
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn the_help_groups_visible_commands_by_category() {
    let bot = bot(&ScriptedTransport::new()).await;
    assert_eq!(
        bot.help(),
        "*General*\n/help — Show the commands\n/ping\n\n\
         *Travel*\n/tickets, /t — Book flight tickets\n/hotel — Book hotel\n\n\
         *Orders*\n/status — Order status"
    );
    let sections = bot.help_sections();
    assert_eq!(
        sections
            .iter()
            .map(|s| s.category.as_str())
            .collect::<Vec<_>>(),
        ["General", "Travel", "Orders"]
    );
    // Hidden ones still run and are still listed as commands.
    let hidden: Vec<_> = bot
        .commands()
        .into_iter()
        .filter(|c| c.hidden)
        .map(|c| c.name)
        .collect();
    assert_eq!(hidden, ["debug", "ban"]);
}

#[tokio::test]
async fn the_help_command_replies_with_the_help() {
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .prefixes(["!"])
        .help_command()
        .command(Command::new("ping", noop()).description("Pong"))
        .build()
        .await
        .unwrap();
    bot.handle(text_event("messages/text.json", "!help"))
        .await
        .unwrap();
    assert_eq!(
        out.sent(),
        [json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505551234",
            "context": {"message_id": message_id("messages/text.json")},
            "type": "text",
            "text": {"body": "*General*\n!help — Show the commands\n!ping — Pong"}
        })]
    );
}

/// The menu is the visible commands with a description, sent as the
/// documented request; the welcome message and ice breakers are not sent.
#[tokio::test]
async fn the_command_menu_syncs_the_visible_commands() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    let bot = bot(&t).await;
    bot.sync_command_menu(&client(&t), NUMBER).await.unwrap();
    let request = t.last_request().unwrap();
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(
        request.path(),
        format!("/v25.0/{NUMBER}/conversational_automation")
    );
    assert_eq!(request.bearer(), Some("TOKEN"));
    assert_eq!(
        request.json(),
        Some(json!({
            "commands": [
                {"command_name": "help", "command_description": "Show the commands"},
                {"command_name": "tickets", "command_description": "Book flight tickets"},
                {"command_name": "hotel", "command_description": "Book hotel"},
                {"command_name": "status", "command_description": "Order status"}
            ]
        }))
    );
    assert_eq!(t.remaining(), 0);
}

/// The client's limits are the menu's: a command without a description,
/// more than 30 commands or a name over 32 characters fail before any
/// request.
#[tokio::test]
async fn a_menu_the_client_would_refuse_fails_before_any_request() {
    let t = ScriptedTransport::new();
    let menu_error = |bot: Bot| {
        let t = t.clone();
        async move {
            let err = bot
                .sync_command_menu(&client(&t), NUMBER)
                .await
                .unwrap_err();
            match err {
                Error::Validation(v) => v.field,
                other => panic!("{other:?}"),
            }
        }
    };
    let out = || Bot::builder().outbound(Recording::default());

    let undescribed = out()
        .command(Command::new("ping", noop()))
        .build()
        .await
        .unwrap();
    assert_eq!(menu_error(undescribed).await, "commands.ping.description");

    let mut many = out();
    for i in 0..31 {
        many = many.command(Command::new(format!("c{i}"), noop()).description("d"));
    }
    let many = many.build().await.unwrap();
    // `command_menu` itself refuses it, not only the request.
    assert_eq!(many.command_menu().unwrap_err().field, "commands");
    assert_eq!(menu_error(many).await, "commands");

    let long = out()
        .command(Command::new("n".repeat(33), noop()).description("d"))
        .build()
        .await
        .unwrap();
    assert_eq!(menu_error(long).await, "commands[0].command_name");

    // Exactly 30, hidden and menu(false) ones aside, is accepted.
    let mut thirty = out().command(Command::new("secret", noop()).hidden());
    for i in 0..30 {
        thirty = thirty.command(Command::new(format!("c{i}"), noop()).description("d"));
    }
    assert_eq!(
        thirty.build().await.unwrap().command_menu().unwrap().len(),
        30
    );

    assert!(t.requests().is_empty());
}

/// A menu tap sends `/name`: a bot whose parser does not read that as the
/// command would publish a menu whose every tap is plain text, so the
/// menu is refused before any request.
#[tokio::test]
async fn a_menu_the_parser_cannot_read_fails() {
    let bang_only = Bot::builder()
        .outbound(Recording::default())
        .prefixes(["!"])
        .command(Command::new("ping", noop()).description("Pong"))
        .build()
        .await
        .unwrap();
    assert_eq!(
        bang_only.command_menu().unwrap_err().field,
        "commands.ping.command_name"
    );
    let t = ScriptedTransport::new();
    assert!(matches!(
        bang_only.sync_command_menu(&client(&t), NUMBER).await,
        Err(Error::Validation(_))
    ));
    assert!(t.requests().is_empty());

    let with_slash = Bot::builder()
        .outbound(Recording::default())
        .prefixes(["!", "/"])
        .command(Command::new("ping", noop()).description("Pong"))
        .build()
        .await
        .unwrap();
    assert_eq!(with_slash.command_menu().unwrap().len(), 1);
}

/// A help format of its own, under another name and description, in a
/// default category of the bot's choosing; `Bot::help` is what it sends.
#[tokio::test]
async fn the_help_command_is_configurable() {
    /// One line per command, no categories.
    #[derive(Debug)]
    struct Compact;
    impl HelpFormatter for Compact {
        fn format(&self, sections: &[HelpSection], prefix: &str) -> String {
            sections
                .iter()
                .flat_map(|s| &s.commands)
                .map(|c| format!("{prefix}{}", c.name))
                .collect::<Vec<_>>()
                .join(" · ")
        }
    }
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .default_category("Umum")
        .help_command_with("bantuan", "Tampilkan perintah", Compact)
        .plugin(Travel)
        .command(Command::new("ping", noop()).description("Pong"))
        .build()
        .await
        .unwrap();
    bot.handle(text_event("messages/text.json", "/bantuan"))
        .await
        .unwrap();
    assert_eq!(out.bodies(), ["/bantuan · /ping · /tickets · /hotel"]);
    assert_eq!(bot.help(), out.bodies()[0]);
    let categories: Vec<_> = bot
        .help_sections()
        .into_iter()
        .map(|s| (s.category, s.commands.len()))
        .collect();
    assert_eq!(
        categories,
        [("Umum".to_owned(), 2), ("Travel".to_owned(), 2)]
    );
    let menu = bot.command_menu().unwrap();
    assert_eq!(menu[0].command_description, "Tampilkan perintah");
    // The default format is `CategoryHelp`.
    let plain = Bot::builder()
        .outbound(Recording::default())
        .command(Command::new("ping", noop()).description("Pong"))
        .build()
        .await
        .unwrap();
    assert_eq!(
        plain.help(),
        CategoryHelp.format(&plain.help_sections(), "/")
    );
}

/// A usage hint follows the name in the help; metadata is kept for the
/// integrator and never shown.
#[tokio::test]
async fn usage_shows_in_the_help_and_metadata_is_kept() {
    let bot = Bot::builder()
        .outbound(Recording::default())
        .command(
            Command::new("status", noop())
                .alias("s")
                .usage("<order number>")
                .description("Where is my order?")
                .metadata("docs", "https://shop.example/help/status"),
        )
        .build()
        .await
        .unwrap();
    assert_eq!(
        bot.help(),
        "*General*\n/status, /s <order number> — Where is my order?"
    );
    let info = &bot.commands()[0];
    assert_eq!(info.usage.as_deref(), Some("<order number>"));
    assert_eq!(
        info.metadata.get("docs").map(String::as_str),
        Some("https://shop.example/help/status")
    );
    assert_eq!(
        bot.command_menu().unwrap()[0].command_description,
        "Where is my order?"
    );
}

/// A handler builds its own help from `Ctx::commands` and
/// `Ctx::help_sections`, the same lists the bot has.
#[tokio::test]
async fn a_handler_sees_the_commands() {
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .plugin(Travel)
        .command(
            Command::new("menu", |ctx: Ctx| async move {
                let hidden = ctx.commands().iter().filter(|c| c.hidden).count();
                let lines: Vec<String> = ctx
                    .help_sections()
                    .iter()
                    .map(|s| format!("{}: {}", s.category, s.commands.len()))
                    .collect();
                ctx.reply(format!("{} ({hidden} hidden)", lines.join(", ")))
                    .await?;
                Ok(())
            })
            .category("Other"),
        )
        .build()
        .await
        .unwrap();
    bot.handle(text_event("messages/text.json", "/menu"))
        .await
        .unwrap();
    assert_eq!(out.bodies(), ["Travel: 2, Other: 1 (1 hidden)"]);
    assert_eq!(bot.commands().len(), 4);
}
