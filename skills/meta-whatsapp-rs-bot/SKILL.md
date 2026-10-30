---
name: meta-whatsapp-rs-bot
description: "A WhatsApp bot on Cloud API webhooks with meta-whatsapp-rs (feature bot) - commands with prefixes, aliases, subcommands, --flags, usage hints and quoted arguments, image and video captions, reply buttons and list rows as commands, an unknown-command hook, private-, group- and owner-only guards, a banned list, per-user cooldowns on the KvStore, middleware (logging, read receipts with a typing indicator), compile-time plugins, a generated help grouped by category, Meta's slash-command menu, reactions, Markdown replies in WhatsApp formatting split at 4096 characters, rich replies (images, buttons, lists, product carousels), replies recorded in the CMS inbox, and paced broadcasts (a per-number rate under Meta's throughput, progress and cancel, retries only when nothing may have gone out, the pair and per-user marketing limits). Load when building a chatbot, command handlers, an auto-responder, a help menu or a campaign send to many users on WhatsApp in Rust, or when replying with Markdown, buttons or LLM output."
---

# meta-whatsapp-rs-bot

> **Verified against meta-whatsapp-rs df598e6647242d4f471579d7f84df12c3032c651 (2026-10-27).** On another revision, trust the code over this page.

Reference code: [examples/bot.rs](examples/bot.rs), compiled and tested by the gate. All in
`meta_whatsapp_rs::bot` (feature `bot`, off by default; `full` includes it), `async_trait` too.

## When to use

For a number that answers commands, taps and free text, and sends to many. Per event: (1) a
banned sender's message stops, nothing below runs; (2) the match, a `/name args` (or a caption)
or a tapped button or list row whose id is a payload, sets `Ctx::invocation`; (3) middleware,
which see the match; (4) the command's guards (scope, owner, flags, cooldown) and handler, else
the unknown-command handler, else the listeners. (Zaileys: middleware after guards.)

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

let reply = RichReply::new()
    .markdown("Try /order status <order number>, or tap:")
    .suggestions([Suggestion::command("order status", "Last order")]);
ctx.reply_rich(&reply).await?; // checked whole, then one message per part
```

- `Plugin` needs `Debug`; `Plugin::category`: `Some("Orders")` (`None`: `BotBuilder::default_category`).
  `PrefixParser` ignores case (`PrefixParser::ignore_case`); `Args` keeps `"quoted strings"` whole.
- `Command::subcommand` (one level, example `order_command`): `/order status 42` runs `status` with `42`; the parent's scope and owner guards apply, not its cooldown.
  `Command::flag` declares `--refresh` or `--limit=5` (`Args::has_flag`, `Args::flag`); an undeclared one then refuses the command with its usage (`Command::pass_unknown_flags` takes any). A command with no flag reads `--x` as an argument; `"--x"` and words after `--` stay arguments too.
- `BotSender::key` is the BSUID first: list owners and bans by BSUID (`AccessList::owner`,
  `AccessList::ban`); a phone-only ban misses users without a number.

## Build it

```rust
Bot::builder()
    .client(client) // replies go out with this client's token
    .pacer(pacer) // replies and read receipts share the number's budget with broadcasts
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

A cooldown without a store, a name taken twice or an unknown `Listen::Event` kind fails `build`;
a running cooldown is told once per period (namespace `wa.bot.cooldown`, keys hashed). Each
policy is a trait with a default (`Refusals`: silent for bans and non-owners; the rest: guide).
`Ctx::reply` answers the group, else the BSUID, else `+<wa_id>`, quoting.

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

`ctx.insert(value)` hands a value on (`ctx.get::<T>()`); the `ErrorHandler` gets the context
from before the middleware, without it.

## Behind the webhook

```rust
let verifier = SignatureVerifier::new(vec![app_secret])?;
let handler = WebhookHandler::builder(verifier, verify_token, Arc::new(bot))
    .dedup(DedupGuard::new(kv)) // Meta retries for 7 days: a retry must not run a command twice
    .build();
handler.check()?; // a blank verify token stops the boot, not Meta's verification
```

`LogErrors` logs a failure (kinds only) and acknowledges it, since an error makes Meta
redeliver the batch and repeat its replies; a transient failure is lost with it
(`PropagateErrors` redelivers). CMS: `InboxThenBot` (example) records first, then runs the bot.

## Paced broadcast

```rust
let broadcast = Broadcast::builder(number)
    .to(customers)
    .compose(|to: &Recipient| {
        let template = TemplateMessage::new("spring_sale", "en_US"); // outside the 24-hour window
        Ok(OutboundMessage::template(to.clone(), template).callback_data("spring-sale"))
    })
    .client(client)
    .pacer(pacer) // the same one as the bot's: one budget per number
    .build()?;
let handle = broadcast.handle(); // progress() and cancel(), from another task
let report = broadcast.run().await; // spawn it to do something else meanwhile
```

Resent only when Meta provably refused it (`Error::may_resend`), whatever the `BroadcastPolicy`;
never a timeout, a `131000` or a 5xx but throttling; one per person. See [references/broadcast.md](references/broadcast.md).

## Help, menu, Markdown and rich replies

`BotBuilder::help_command_with` sets the help's name, description and `HelpFormatter`
(`Ctx::commands`, `Ctx::help_sections`). `Bot::sync_command_menu` sends the visible commands
(`Command::menu` with `false` keeps one out), at most 30, each described.
`ctx.reply_markdown(md)` renders with the bot's `MarkdownRenderer` (`Renderer`: `*b*`, `_i_`,
`~s~`, code, quotes, lists, `text (url)`, tables) in parts of 4096 UTF-16 units.
`ctx.reply_rich` adds images by `http(s)` link, suggestions on the text right before them (up to
3 as buttons, 10 as a list; `Suggestion::command` sends a command's payload) and product
carousels, all checked before the first send, sent until one fails (guide, section 8).

## Pitfalls

- **Plugins are compiled in** (ship one as a crate); `Bot::unload` runs every `on_unload`, then events fail with `SinkError::Closed`. A menu tap sends `/name`.
- `MarkRead::with_typing_indicator` shows "typing…" for every message; replies are free-form, refused
  outside the 24-hour window and, through `Inbox::send`, while another app owns the thread (`ErrorKind::ThreadOwnedElsewhere`).
  Test a handler alone: `Ctx::new` plus `Ctx::with_invocation` (`Invocation::new`), with an `Outbound` that records.

## What meta-whatsapp-rs does not do

- No scheduling or restart-proof broadcast (planned), or conversation state: keep it in your store, keyed by `BotSender::key`.
- Per-tenant tokens: implement `Outbound` (`meta-whatsapp-rs-token-vault`).
- Guide: [docs/guides/bots.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/bots.md).

## Related skills

`meta-whatsapp-rs-webhook-endpoint`, `meta-whatsapp-rs-interactive-messages`,
`meta-whatsapp-rs-cms-inbox`, `meta-whatsapp-rs-phone-numbers` (command menu), `meta-whatsapp-rs-token-vault`, `meta-whatsapp-rs-testing`.
