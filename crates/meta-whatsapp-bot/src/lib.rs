//! A bot framework over WhatsApp Cloud API webhooks: commands with
//! prefixes, aliases, guards, cooldowns and a generated help, middleware,
//! compile-time plugins, and Markdown replies in WhatsApp formatting.
//!
//! ```text
//! WebhookHandler ─► DedupGuard ─► Bot (an EventSink<WebhookEvent>)
//!   ─► a received message from a banned sender: nothing runs, not even middleware
//!   ─► middleware, in order (any may stop the event)
//!   ─► a received message: typed `/name args`, or a tapped button / list row
//!        whose id is a payload → scope → owner → cooldown → the command's handler
//!   ─► anything else (and messages no command matched): the listeners
//! ```
//!
//! Every decision is a trait with a default, so an integrator swaps the
//! part and keeps the rest:
//!
//! | Decision | Trait | Default |
//! | --- | --- | --- |
//! | how replies and read receipts are sent | [`Outbound`] | [`ClientOutbound`] (`client.messages(pn)`) |
//! | which text is a command | [`CommandParser`] | [`PrefixParser`] (`/`) |
//! | owners and banned users | [`AccessPolicy`] | [`AccessList`] (configuration) |
//! | where cooldowns are kept | [`Cooldowns`] | [`KvCooldowns`] (a typed store on `KvStore`, namespace `bot.cooldown`) |
//! | what a refused user is told | [`Refusals`] | [`ReplyRefusals`] |
//! | what a failure becomes | [`ErrorHandler`] | [`LogErrors`] (log, acknowledge) |
//! | how Markdown is escaped | [`markdown::Escape`] | [`markdown::NoEscape`] (text as written; [`markdown::WordJoinerEscape`] opt-in) |
//!
//! Users are identified by business-scoped user id first ([`Sender::key`]):
//! a message may carry no phone number at all. A reply goes to the group
//! for a group message, else to the sender by BSUID, else to `+<wa_id>`,
//! quoting the message.
//!
//! Plugins are compiled in: there is no hot reload (see [`plugin`]).
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
pub mod markdown;
pub mod middleware;
pub mod outbound;
pub mod parse;
pub mod plugin;

pub use bot::{Bot, BotBuilder, HelpSection, PluginInfo};
pub use command::{Args, Command, CommandInfo, Handler, Invocation, Scope, Trigger};
pub use ctx::{Chat, Ctx, Extensions, Sender};
pub use errors::{ErrorHandler, LogErrors, PropagateErrors};
pub use guard::{
    AccessList, AccessPolicy, COOLDOWN_NAMESPACE, CooldownKey, CooldownOutcome, Cooldowns,
    KvCooldowns, Refusal, Refusals, ReplyRefusals, SilentRefusals,
};
pub use middleware::{Logging, MarkRead, Middleware, Next};
pub use outbound::{ClientOutbound, Outbound};
pub use parse::{CommandParser, ParsedCommand, PrefixParser};
pub use plugin::{DEFAULT_CATEGORY, Listen, Plugin, Registrar};
