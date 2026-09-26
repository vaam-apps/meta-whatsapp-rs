//! The extension points an integrator swaps: shared (`Arc`) implementations
//! of every trait, a case-sensitive parser, Markdown rules of one's own,
//! and a handler unit-tested without a bot.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// The re-exported attribute: no `async-trait` dependency of one's own.
use meta_whatsapp_adapters::store::MemoryKvStore;
use meta_whatsapp_bot::async_trait;
use meta_whatsapp_bot::markdown::{NoEscape, Renderer};
use meta_whatsapp_bot::{
    AccessList, AccessPolicy, Args, Bot, CategoryHelp, Command, CommandParser, Cooldowns, Ctx,
    ErrorHandler, HelpFormatter, Invocation, KvCooldowns, LogErrors, MarkdownRenderer, Middleware,
    Next, Outbound, PrefixParser, Refusals, SilentRefusals, Trigger,
};
use meta_whatsapp_core::clock::SystemClock;
use meta_whatsapp_core::{Error, ErrorKind};
use pretty_assertions::assert_eq;
use serde_json::json;

use common::{Recording, message_id, text_event};

const TEXT: &str = "messages/text.json";

fn counting(name: &str, runs: &Arc<AtomicUsize>) -> Command {
    let runs = Arc::clone(runs);
    Command::new(name, move |_ctx: Ctx| {
        let runs = Arc::clone(&runs);
        async move {
            runs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    })
}

#[derive(Debug)]
struct Pass;

#[async_trait]
impl Middleware for Pass {
    async fn handle(&self, ctx: Ctx, next: Next<'_>) -> meta_whatsapp_core::Result<()> {
        next.run(ctx).await
    }
}

/// One outbound (a vault-backed one, say) shared by two bots, and every
/// other trait given as an `Arc` of itself or of `dyn Trait`.
#[tokio::test]
async fn every_trait_is_accepted_behind_an_arc() {
    let out = Recording::default();
    let shared: Arc<dyn Outbound> = Arc::new(out.clone());
    let access: Arc<dyn AccessPolicy> = Arc::new(AccessList::new().ban("US.1"));
    let refusals: Arc<dyn Refusals> = Arc::new(SilentRefusals);
    let errors: Arc<dyn ErrorHandler> = Arc::new(LogErrors);
    let middleware: Arc<dyn Middleware> = Arc::new(Pass);
    let parser: Arc<dyn CommandParser> = Arc::new(PrefixParser::new(["!"]));
    let renderer: Arc<dyn MarkdownRenderer> = Arc::new(Renderer::new().escape(Arc::new(NoEscape)));
    let help: Arc<dyn HelpFormatter> = Arc::new(CategoryHelp);
    let cooldowns: Arc<dyn Cooldowns> = Arc::new(KvCooldowns::new(
        Arc::new(MemoryKvStore::new()),
        Arc::new(SystemClock),
    ));
    let bot = |outbound: Arc<dyn Outbound>| {
        Bot::builder()
            .shared_outbound(outbound)
            .cooldowns(Arc::clone(&cooldowns))
            .access(Arc::clone(&access))
            .refusals(Arc::clone(&refusals))
            .errors(Arc::clone(&errors))
            .middleware(Arc::clone(&middleware))
            .parser(Arc::clone(&parser))
            .markdown(Arc::clone(&renderer))
            .help_command_with("help", "Show the commands", Arc::clone(&help))
            .command(
                Command::new("hi", |ctx: Ctx| async move {
                    ctx.reply_markdown("**hi**").await?;
                    Ok(())
                })
                .cooldown(std::time::Duration::from_secs(60)),
            )
            .build()
    };
    let one = bot(Arc::clone(&shared)).await.unwrap();
    let two = bot(Arc::clone(&shared)).await.unwrap();
    one.handle(text_event(TEXT, "!hi")).await.unwrap();
    two.handle(text_event(TEXT, "!help")).await.unwrap();
    assert_eq!(
        out.bodies(),
        ["*hi*", "*General*\n!help — Show the commands\n!hi"]
    );
    // `outbound(Arc<…>)` works too (an `Arc` of an `Outbound` is one).
    Bot::builder()
        .outbound(Arc::new(out.clone()))
        .build()
        .await
        .unwrap();
}

/// Case folding is the parser's: with `ignore_case(false)`, `/Ping` and
/// `/ping` are two commands, and each matches only as written.
#[tokio::test]
async fn a_case_sensitive_parser_keeps_names_as_written() {
    let upper = Arc::new(AtomicUsize::new(0));
    let lower = Arc::new(AtomicUsize::new(0));
    let bot = Bot::builder()
        .outbound(Recording::default())
        .parser(PrefixParser::new(["/"]).ignore_case(false))
        .command(counting("Ping", &upper))
        .command(counting("ping", &lower))
        .build()
        .await
        .unwrap();
    for body in ["/Ping", "/ping", "/PING", "/ping"] {
        bot.handle(text_event(TEXT, body)).await.unwrap();
    }
    assert_eq!(
        (upper.load(Ordering::SeqCst), lower.load(Ordering::SeqCst)),
        (1, 2)
    );
    let names: Vec<_> = bot.commands().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["Ping", "ping"]);
    // The default parser folds case, so the same two names collide.
    let err = Bot::builder()
        .outbound(Recording::default())
        .command(counting("Ping", &upper))
        .command(counting("ping", &lower))
        .build()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Config(_)), "{err}");
}

/// Markdown rules of one's own: `BotBuilder::markdown` takes any
/// `MarkdownRenderer`, and `Ctx::renderer` is it.
#[tokio::test]
async fn the_markdown_rules_are_swappable() {
    /// Headings as upper case, nothing else touched.
    #[derive(Debug)]
    struct Shout;
    impl MarkdownRenderer for Shout {
        fn render(&self, markdown: &str) -> Vec<String> {
            vec![
                markdown
                    .strip_prefix("# ")
                    .map_or_else(|| markdown.to_owned(), str::to_uppercase),
            ]
        }
    }
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .markdown(Shout)
        .command(Command::new("title", |ctx: Ctx| async move {
            assert_eq!(ctx.renderer().render("# a"), ["A"]);
            ctx.reply_markdown("# News").await?;
            Ok(())
        }))
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/title")).await.unwrap();
    assert_eq!(out.bodies(), ["NEWS"]);
}

/// The status command of a shop, as an integrator writes it.
async fn status(ctx: Ctx) -> meta_whatsapp_core::Result<()> {
    match ctx.args().get(0) {
        Some(order) => ctx.reply(format!("Order {order} has shipped.")).await?,
        None => ctx.reply("Usage: /status <order number>").await?,
    };
    Ok(())
}

/// A handler is unit-tested without a bot: a context from a documented
/// webhook, an invocation, a recording outbound.
#[tokio::test]
async fn a_handler_is_unit_tested_with_a_hand_built_context() {
    let out = Recording::default();
    let ctx = Ctx::new(
        text_event(TEXT, "/status 1234"),
        Arc::new(out.clone()),
        Arc::new(Renderer::new()),
    )
    .with_invocation(Invocation::new(
        "status",
        Trigger::Text {
            prefix: "/".into(),
            name: "status".into(),
        },
        Args::parse("1234"),
    ));
    assert_eq!(ctx.args().get(0), Some("1234"));
    assert!(
        ctx.commands().is_empty(),
        "a hand-built context knows no bot"
    );
    status(ctx).await.unwrap();

    // Without arguments: the usage line.
    let bare = Ctx::new(
        text_event(TEXT, "/status"),
        Arc::new(out.clone()),
        Arc::new(Renderer::new()),
    )
    .with_invocation(Invocation::new(
        "status",
        Trigger::Payload("order_status".into()),
        Args::default(),
    ));
    status(bare).await.unwrap();

    assert_eq!(
        out.sent(),
        [
            json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "to": "+16505551234",
                "context": {"message_id": message_id(TEXT)},
                "type": "text",
                "text": {"body": "Order 1234 has shipped."}
            }),
            json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "to": "+16505551234",
                "context": {"message_id": message_id(TEXT)},
                "type": "text",
                "text": {"body": "Usage: /status <order number>"}
            })
        ]
    );
}

/// What the error handler sees: the context after the match, before the
/// middleware, so the command is known but a middleware's values are not.
#[tokio::test]
async fn the_error_handler_sees_the_match_but_not_the_middlewares_values() {
    /// The command, whether a middleware's value is there, the error kind.
    type Report = (Option<String>, bool, ErrorKind);
    #[derive(Debug, Default, Clone)]
    struct Seen(Arc<Mutex<Vec<Report>>>);
    #[async_trait]
    impl ErrorHandler for Seen {
        async fn on_error(&self, ctx: &Ctx, error: Error) -> meta_whatsapp_core::Result<()> {
            self.0.lock().unwrap().push((
                ctx.invocation().map(|i| i.command.clone()),
                ctx.get::<&'static str>().is_some(),
                error.kind(),
            ));
            Ok(())
        }
    }
    #[derive(Debug)]
    struct Tag;
    #[async_trait]
    impl Middleware for Tag {
        async fn handle(&self, mut ctx: Ctx, next: Next<'_>) -> meta_whatsapp_core::Result<()> {
            ctx.insert("tagged");
            next.run(ctx).await
        }
    }
    let seen = Seen::default();
    let bot = Bot::builder()
        .outbound(Recording::default())
        .errors(seen.clone())
        .middleware(Tag)
        .command(Command::new("fail", |ctx: Ctx| async move {
            assert!(ctx.get::<&'static str>().is_some());
            Err(Error::Other(anyhow::anyhow!("a handler's own failure")))
        }))
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/fail")).await.unwrap();
    assert_eq!(
        *seen.0.lock().unwrap(),
        [(Some("fail".to_owned()), false, ErrorKind::Unknown)]
    );
}
