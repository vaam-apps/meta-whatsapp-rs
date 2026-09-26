---
name: meta-whatsapp-rs-bot
description: "A WhatsApp bot on Cloud API webhooks with meta-whatsapp-rs (feature bot) - commands with prefixes, aliases, usage hints and quoted arguments, image and video captions, reply buttons and list rows as commands, an unknown-command hook, private-only, group-only and owner-only guards, a banned list, per-user cooldowns on the KvStore, middleware such as logging and read receipts with a typing indicator, one plugin per feature compiled in (no hot reload), a generated help grouped by category, Meta's slash-command menu, reactions, Markdown replies converted to WhatsApp formatting and split at 4096 characters, and replies recorded in the CMS inbox. Load when building a chatbot, command handlers, an auto-responder or a help menu on WhatsApp in Rust, or when replying with Markdown or LLM output."
---

# meta-whatsapp-rs-bot

> **Verified against meta-whatsapp-rs 34beecb2720bac099d769ba1b5e91072d2d5eb36 (2026-09-26).** On another revision, trust the code over this page.

Reference code: [examples/bot.rs](examples/bot.rs), compiled and tested
by meta-whatsapp-rs's own gate. Everything is in `meta_whatsapp_rs::bot`
(feature `bot`, off by default; `full` includes it), `async_trait` too.

## When to use

For a number that answers commands, taps and free text. Per event: (1) a
banned sender's message stops, nothing below runs; (2) the match, a `/name args`
(or image or video caption; off: `BotBuilder::commands_from_captions(false)`) or a
tapped button or list row whose id is a payload, sets `Ctx::invocation` (unknown:
`Ctx::unknown_command`); (3) middleware, which see the match; (4) the command's
guards (scope, owner, cooldown) and handler, else `BotBuilder::unknown_command`'s
handler, else the listeners. (Zaileys: middleware after guards, commands only.)

## A plugin per feature

```rust
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
```

- `Plugin` needs `Debug`; `Plugin::category` returns `Some("Orders")`
  (`None`: `BotBuilder::default_category`).
- Names match as the parser gives them: `PrefixParser` ignores case
  (`PrefixParser::ignore_case` with `false` keeps it). `Args` keeps
  `"quoted strings"` whole. `Command::metadata` is yours alone.
- `BotSender::key` is the BSUID first: list owners and bans by BSUID
  (`AccessList::owner`, `AccessList::ban`); a phone-only ban misses users
  without a number.

## Build it

```rust
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
```

A cooldown without a store, a name taken twice or a `Listen::Event` kind
not in `WebhookEvent::KINDS` fails `build`. A running cooldown is told
once per period (store namespace `wa.bot.cooldown`, keys hashed).

Traits with defaults: `Outbound` (`ClientOutbound`), `CommandParser`,
`AccessPolicy`, `Cooldowns`, `Refusals` (`ReplyRefusals`: silent for bans
and non-owners), `ErrorHandler` (`LogErrors`), `MarkdownRenderer`,
`HelpFormatter`. `Ctx::reply` answers the group, else the BSUID, else
`+<wa_id>`, quoting; to do otherwise, `Ctx::send` a message you build.

## Middleware

```rust
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
```

`ctx.insert(value)` hands a value on (`ctx.get::<T>()`); the
`ErrorHandler` gets the context from before the middleware, without it.

## Behind the webhook

```rust
let verifier = SignatureVerifier::new(vec![app_secret])?;
let handler = WebhookHandler::builder(verifier, verify_token, Arc::new(bot))
    .dedup(DedupGuard::new(kv)) // Meta retries for 7 days: a retry must not run a command twice
    .build();
```

`LogErrors` logs a failure (kinds only) and acknowledges it, since an
error makes Meta redeliver the batch and repeat its replies; a transient
failure is lost with it (`PropagateErrors` redelivers). CMS:
`InboxOutbound` and `InboxThenBot` (example) put replies in the inbox's
history: record first, then the bot (a `FanoutSink` runs both at once).

## Help and Meta's command menu

`BotBuilder::help_command_with` sets the name, description and
`HelpFormatter`; in a handler, `Ctx::commands` and `Ctx::help_sections`.
The menu is the visible commands (`Command::menu` with `false` keeps one
out), under the client's limits; one without a description fails:

```rust
bot.sync_command_menu(client, number).await // at most 30 commands, each with a description
```

## Markdown replies

`ctx.reply_markdown(md)` renders with the bot's `MarkdownRenderer` (`Renderer` unless
`BotBuilder::markdown` sets another): `*b*`, `_i_`, `~s~`, code, quotes, `•` lists,
`text (url)` (web, mail, phone); emphasis inside a word loses its markers. Tables: padded
columns up to 60 wide, else `header: value` lines, within twice their text
(`Renderer::table_max_width`, `Renderer::table_max_growth`). Parts fit 4096 UTF-16 units,
cut between blocks. Text is left as written (`NoEscape`); `WordJoinerEscape` is opt-in.

## Pitfalls

- **Plugins are compiled in**: ship one as a crate; `Bot::unload` runs
  every `on_unload`, then events fail with `SinkError::Closed`.
- A menu tap sends `/name`: keep `/` among the prefixes.
- `MarkRead::with_typing_indicator` shows "typing…" for every message;
  replies are free-form, refused outside the 24-hour window.
- Test a handler alone: `Ctx::new` plus `Ctx::with_invocation`
  (`Invocation::new`), with an `Outbound` that records.

## What meta-whatsapp-rs does not do

- No paced broadcast or scheduling (planned), no subcommands or `--flags`,
  no conversation state: keep it in your store, keyed by `BotSender::key`.
- Per-tenant tokens: implement `Outbound` (`meta-whatsapp-rs-token-vault`).
- Guide: [docs/guides/bots.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/bots.md).

## Related skills

`meta-whatsapp-rs-webhook-endpoint`, `meta-whatsapp-rs-interactive-messages`,
`meta-whatsapp-rs-cms-inbox`, `meta-whatsapp-rs-phone-numbers` (command menu),
`meta-whatsapp-rs-token-vault`, `meta-whatsapp-rs-testing`.
