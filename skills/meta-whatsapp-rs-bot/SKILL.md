---
name: meta-whatsapp-rs-bot
description: "A WhatsApp bot on Cloud API webhooks with meta-whatsapp-rs (feature bot) - commands with prefixes, case-insensitive aliases and quoted arguments, reply buttons and list rows as commands, private-only, group-only and owner-only guards, a banned list, per-user cooldowns on the KvStore, middleware such as logging and read receipts with a typing indicator, one plugin per feature compiled in (no hot reload), a generated help grouped by category, Meta's slash-command menu, and Markdown replies converted to WhatsApp formatting and split at 4096 characters. Load when building a chatbot, command handlers, an auto-responder or a help menu on WhatsApp in Rust, or when replying with Markdown or LLM output."
---

# meta-whatsapp-rs-bot

> **Verified against meta-whatsapp-rs 4f99ee9b9be4e52c1c58dd9cb6c4dfd97a9dfaec (2026-09-26).** On another revision, trust the code over this page.

Reference code: [examples/bot.rs](examples/bot.rs), compiled and tested
by meta-whatsapp-rs's own gate. Everything is in `meta_whatsapp_rs::bot`
(feature `bot`, off by default; `full` includes it).

## When to use

A number that answers commands (`/status 1234`), button taps or free
text (listeners) with formatted replies. The bot is an `EventSink`
behind the usual `WebhookHandler`.

## A plugin per feature

A `Plugin` has a `name` and a `category` (its help section), and
registers commands, middleware and listeners in `setup`:

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
            .description("Where is my order?")
            .cooldown(Duration::from_secs(10)) // per user, per business number
            .payload("order_status"), // a reply button or list row with this id runs it too
        );
        Ok(())
    }
```

- Names and aliases match case-insensitively; `Args` keeps
  `"quoted strings"` whole. An unknown `/name` goes to the listeners.
- `ctx.reply` quotes the message and goes to the group for a group
  message, else to the sender's BSUID, else to `+<wa_id>`.
- `Sender::key` is the BSUID first: a user may arrive without a phone
  number. List owners and bans by BSUID (`AccessList::owner`,
  `AccessList::ban`); a ban by phone number misses those users.

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
```

Guards, in order: banned (before the middleware: nothing runs, not even
a read receipt), scope (`Command::private_only`, `Command::group_only`),
`Command::owner_only`, then the cooldown, so a refused attempt starts
none. A cooldown without a store or a name taken twice fails `build`.

Every decision is a trait with a default: `Outbound` (`ClientOutbound`),
`CommandParser` (`PrefixParser`), `AccessPolicy` (`AccessList`),
`Cooldowns` (`KvCooldowns`, namespace `bot.cooldown`, keys hashed),
`Refusals` (`ReplyRefusals`: a short reply for a wrong chat or a running
cooldown, nothing for bans and non-owners; `SilentRefusals`),
`ErrorHandler` (`LogErrors`), and the renderer's `Escape`.

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

They run in registration order, before the command match
(`Ctx::invocation` is `None` there), never for a banned sender's message;
`ctx.insert(value)` hands a value on (`ctx.get::<T>()`).

## Behind the webhook

```rust
let verifier = SignatureVerifier::new(vec![app_secret])?;
let handler = WebhookHandler::builder(verifier, verify_token, Arc::new(bot))
    .dedup(DedupGuard::new(kv)) // Meta retries for 7 days: a retry must not run a command twice
    .build();
```

Keep the `DedupGuard`, or Meta's retries run commands again. `LogErrors`
logs a failure (kinds only) and acknowledges it, since an error makes
Meta redeliver the batch and repeat its replies (`PropagateErrors`).

## Help and Meta's command menu

`BotBuilder::help_command` adds `/help`: the commands not hidden, by
category (`Bot::help`; `Bot::help_sections` to format your own).

```rust
bot.sync_command_menu(client, number).await // at most 30 commands, each with a description
```

It lists the visible commands (`Command::menu` with `false` keeps one
out), checked first against the client's limits (30 commands, names of
32 characters, descriptions of 1 to 256); a listed command without a
description fails `Bot::command_menu` rather than being dropped.

## Markdown replies

`ctx.reply_markdown(md)` uses `meta_whatsapp_rs::bot::markdown::render`:
bold and headings `*b*`, italics `_i_`, `~s~`, code, quotes, `•` lists,
`text (url)` links, tables as a monospace block; a message per 4096
characters, cut between blocks, never inside a code block that fits.
Text is left as written (`NoEscape`, the default): copied addresses,
codes and `/commands` work, but a literal `*`, `_` or `~` may format.
`WordJoinerEscape` (opt-in) wraps them in U+2060, which is copied too.

## Pitfalls

- **Plugins are compiled in.** No hot reload or dynamic loading; ship a
  plugin as a crate and redeploy. `Bot::unload` runs every `on_unload`
  at shutdown, then the bot refuses events so Meta redelivers them.
- A menu tap sends `/name`: keep `/` among the prefixes if you sync it.
- `MarkRead::with_typing_indicator` shows "typing…" for every message,
  commands or not; use `MarkRead::new` when most messages get no reply.
- Replies are free-form: outside the 24-hour window Meta refuses them.

## What meta-whatsapp-rs does not do

- No paced broadcast or scheduling (planned), no conversation state or
  multi-step forms: keep those in your store, keyed by `Sender::key`.
- No per-tenant token lookup: implement `Outbound` to pick a merchant's
  token by business number (`meta-whatsapp-rs-token-vault`).
- Guide: [docs/guides/bots.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/bots.md).

## Related skills

`meta-whatsapp-rs-webhook-endpoint`, `meta-whatsapp-rs-interactive-messages`
(reply buttons and lists), `meta-whatsapp-rs-send-messages`,
`meta-whatsapp-rs-phone-numbers` (conversational components),
`meta-whatsapp-rs-token-vault`, `meta-whatsapp-rs-testing`.
