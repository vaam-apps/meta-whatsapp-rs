//! A bot framework over WhatsApp Cloud API webhooks: commands with
//! prefixes, aliases, guards, cooldowns and a generated help, middleware,
//! compile-time plugins, and Markdown replies in WhatsApp formatting.
//!
//! Each event goes through these steps, in this order:
//!
//! ```text
//! WebhookHandler ─► DedupGuard ─► Bot (an EventSink<WebhookEvent>)
//!   1. ban:    a received message from a banned sender stops here; nothing below runs
//!   2. match:  typed `/name args` (or an image or video caption), or a tapped
//!              button / list row whose id is a payload → ctx.invocation();
//!              a `/name` no command has → ctx.unknown_command()
//!   3. middleware, in order (any may stop the event; they see the match)
//!   4. the command's guards: scope → owner → cooldown → its handler;
//!      else the unknown-command handler, if any; else the listeners
//! ```
//!
//! Coming from Zaileys: there, middleware runs for commands only, after
//! their guards; here it runs for every event (listeners' too) after the
//! ban and the match, before the command's own guards.
//!
//! These decisions are traits with a default, so an integrator swaps the
//! part and keeps the rest:
//!
//! | Decision | Trait | Default |
//! | --- | --- | --- |
//! | how replies and read receipts are sent | [`Outbound`] | [`ClientOutbound`] (`client.messages(pn)`) |
//! | which text is a command, how names compare | [`CommandParser`] | [`PrefixParser`] (`/`, names case-insensitive) |
//! | owners and banned users | [`AccessPolicy`] | [`AccessList`] (configuration) |
//! | where cooldowns are kept | [`Cooldowns`] | [`KvCooldowns`] (a typed store on `KvStore`, namespace `wa.bot.cooldown`) |
//! | what a refused user is told | [`Refusals`] | [`ReplyRefusals`] |
//! | what a failure becomes | [`ErrorHandler`] | [`LogErrors`] (log, acknowledge) |
//! | how Markdown becomes messages | [`MarkdownRenderer`] | [`markdown::Renderer`] |
//! | how Markdown text is escaped | [`markdown::Escape`] | [`markdown::NoEscape`] (text as written; [`markdown::WordJoinerEscape`] opt-in) |
//! | how the help reads | [`HelpFormatter`] | [`CategoryHelp`] (name, description and category configurable) |
//!
//! Not behind a trait: the order of the steps above, and where the reply
//! helpers answer. [`Ctx::reply`] and its siblings answer the group for a
//! group message, else the sender by business-scoped user id (BSUID),
//! else `+<wa_id>`, quoting the message; to answer anyone else, or without
//! the quote, build the message and send it with [`Ctx::send`]. Users are
//! identified by BSUID first ([`BotSender::key`]): a message may carry no
//! phone number at all.
//!
//! Plugins are compiled in: there is no hot reload (see [`plugin`]).
//! Every extension point is an `#[async_trait]` trait; the macro is
//! re-exported as [`async_trait`](macro@async_trait), so no second
//! dependency is needed.
//!
//! # Example
//!
//! ```no_run
//! # async fn demo(client: meta_whatsapp_client::Client) -> meta_whatsapp_core::Result<()> {
//! use meta_whatsapp_bot::{Bot, Command, Ctx, Logging, MarkRead};
//!
//! let bot = Bot::builder()
//!     .client(client)
//!     .middleware(Logging)
//!     .middleware(MarkRead::with_typing_indicator())
//!     .command(
//!         Command::new("ping", |ctx: Ctx| async move {
//!             ctx.reply("pong").await?;
//!             Ok(())
//!         })
//!         .alias("p")
//!         .description("Check the bot is alive"),
//!     )
//!     .help_command()
//!     .build()
//!     .await?;
//! // WebhookHandler::builder(verifier, verify_token, Arc::new(bot)) …
//! # let _ = bot; Ok(()) }
//! ```
//!
//! Not here (yet): paced broadcasts and scheduling.

pub mod bot;
pub mod command;
pub mod ctx;
pub mod errors;
pub mod guard;
pub mod help;
pub mod markdown;
pub mod middleware;
pub mod outbound;
pub mod parse;
pub mod plugin;

/// The attribute every extension point's `impl` needs
/// (`#[async_trait] impl Middleware for …`), re-exported from the
/// `async-trait` crate.
pub use async_trait::async_trait;
pub use bot::{Bot, BotBuilder, PluginInfo};
pub use command::{Args, Command, CommandHandler, CommandInfo, Invocation, Scope, Trigger};
pub use ctx::{BotSender, Chat, Ctx};
pub use errors::{ErrorHandler, LogErrors, PropagateErrors};
pub use guard::{
    AccessList, AccessPolicy, COOLDOWN_NAMESPACE, COOLDOWN_NOTICE_NAMESPACE, CooldownKey,
    CooldownOutcome, Cooldowns, KvCooldowns, Refusal, Refusals, ReplyRefusals, SilentRefusals,
};
pub use help::{CategoryHelp, HelpFormatter, HelpSection};
pub use markdown::MarkdownRenderer;
pub use middleware::{Logging, MarkRead, Middleware, Next};
pub use outbound::{ClientOutbound, Outbound};
pub use parse::{CommandParser, ParsedCommand, PrefixParser};
pub use plugin::{DEFAULT_CATEGORY, Listen, Plugin, Registrar};
