# Bots

**Goal:** a WhatsApp number that answers commands (`/status 1234`), taps
on reply buttons and list rows, and free text, with formatted replies;
built from plugins, one feature each, on the webhook endpoint you already
run.

Crate: `meta-whatsapp-bot`, re-exported as `meta_whatsapp_rs::bot`
(feature `bot`, off by default; `full` includes it). Agent skill:
[`meta-whatsapp-rs-bot`](../../skills/meta-whatsapp-rs-bot/SKILL.md), whose
[`examples/bot.rs`](../../skills/meta-whatsapp-rs-bot/examples/bot.rs) is
compiled and tested by the gate; the snippets below are copied from it
(the copies are not checked: on doubt, the example wins).

```text
WebhookHandler ─► DedupGuard ─► Bot (an EventSink<WebhookEvent>)
  ─► a received message from a banned sender: nothing runs, not even middleware
  ─► middleware, in registration order (any may stop the event)
  ─► `/name args` or a tapped button / list row whose id is a payload
       ─► scope ─► owner ─► cooldown ─► the command's handler
  ─► anything else (and messages no command matched): the listeners
```

```toml
[dependencies]
meta-whatsapp-rs = { git = "https://github.com/vaam-apps/meta-whatsapp-rs", rev = "<commit>", features = ["axum", "bot"] }
```

## 1. Commands

A command is a name, aliases, a description and guards around a handler,
an `async` closure over the context (`Ctx`), registered here by a
plugin's `setup` (section 4):

```rust
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
```

- **Syntax.** The default parser (`PrefixParser`) takes `/` or the
  prefixes you give (`.prefixes(["/", "!"])`); the longest matching prefix
  wins. Names and aliases match case-insensitively. Arguments split on
  whitespace; `"double quotes"` (and the curly quotes phone keyboards
  type) keep a phrase together. An unknown name is a plain message.
- **Buttons as commands.** `Command::payload` registers the id of a reply
  button, a list row or a template quick-reply button: tapping it runs the
  command with no arguments (`Trigger::Payload`).
- **Replies.** `ctx.reply(text)` quotes the message. It goes to the group
  for a group message, else to the sender's business-scoped user id
  (BSUID), else to `+<wa_id>`. A sender is keyed the same way
  (`Sender::key`): since 2026 a message may carry no phone number at all.
  Standby copies, your own echoes and synchronized history never run a
  command, and there is nothing to reply to in them.
- **Listeners.** `Listen::Messages` gets every received message no
  command took: reactions, edits, deletions and system notices too, so
  check the content before an automatic answer (else a user's reaction
  to your reply gets a reply of its own).

## 2. Guards

Checked in this order before the handler; a refusal goes to the bot's
`Refusals` (by default, `ReplyRefusals`):

| Guard | Declared with | Refusal |
| --- | --- | --- |
| banned sender | `AccessList::ban` (BSUID) or `AccessList::ban_phone` | silent; checked before the middleware, so no read receipt, typing indicator, command or listener either |
| wrong chat | `Command::group_only`, `Command::private_only` | a short reply |
| not an owner | `Command::owner_only` + `AccessList::owner` | silent |
| cooldown running | `Command::cooldown` | "Please wait N s …" |

List owners and bans **by BSUID**: a username adopter arrives without a
phone number, so a ban by phone number alone can be walked around. A
BSUID belongs to one business portfolio (list the parent BSUID for
several) and changes when the user changes phone number, so a ban is a
filter, not a guarantee. The
cooldown is checked last, so a refused attempt starts none. It lives in
the `KvStore` you pass (`.cooldown_store(kv, clock)`, namespace
`bot.cooldown`, keys hashed so no phone number is stored): with Postgres
or Redis, every instance shares it. Owners, bans and cooldowns are
traits (`AccessPolicy`, `Cooldowns`) when a list in code does not fit.

## 3. Middleware

Code around every event, before the command match (a banned sender's
message never reaches it); a middleware that does not call
`next.run(ctx)` stops the event:

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

Shipped: `Logging` (event kind, message type, duration and error kind;
never content, senders or error text) and `MarkRead` (a read receipt;
`MarkRead::with_typing_indicator` also shows "typing…", falling back to
a plain receipt when Meta refuses it). `ctx.insert(value)` hands a value
to later middleware and the handler.

## 4. Plugins

A `Plugin` is one feature: a name, a category (its section in the help),
a description, `hidden`, and a `setup` that registers its commands,
middleware and listeners. `Bot::builder().plugin(p)` adds it; `build()`
runs the `setup`s in order.

**No hot reload.** Plugins are compiled in. Rust has no stable ABI, so a
plugin loaded at run time must come from the exact same compiler and
dependency versions or it is undefined behaviour, and loading code at run
time is what an attacker who can write to disk wants. Ship a plugin as a
crate, depend on it, redeploy to change it. `Bot::unload` runs each
plugin's `on_unload` at shutdown; the bot then refuses events so Meta
redelivers them to an instance still running.

## 5. Wiring it to the webhook

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

```rust
let verifier = SignatureVerifier::new(vec![app_secret])?;
let handler = WebhookHandler::builder(verifier, verify_token, Arc::new(bot))
    .dedup(DedupGuard::new(kv)) // Meta retries for 7 days: a retry must not run a command twice
    .build();
```

Keep the `DedupGuard`: Meta retries for 7 days, and without it a retry
runs the command again. A failing handler is logged and acknowledged by
the default `LogErrors`, because an error answers `500` and Meta
redelivers the whole batch, repeating every reply already sent in it;
`PropagateErrors` opts into redelivery for idempotent handlers (a
command with a cooldown is refused on its redelivery: the cooldown
started before the failure).

Replies are free-form messages: Meta accepts them only within 24 hours
of the user's last message (`ErrorKind::CustomerServiceWindowClosed`,
`131047`, logged by `LogErrors`). The message being answered opens that
window, but a redelivery after an outage can arrive days later and still
runs its command: when a stale command must not act, compare the
message's `timestamp` with the clock in the handler (or a middleware),
and reach the user later with a template.

Several merchants' numbers: the default `ClientOutbound` sends with one
token. Implement `Outbound` to look the merchant's token up by business
number (the [token vault](embedded-signup.md)), after your own tenant
checks, as the [CMS inbox](cms-inbox.md) does.

## 6. Help and Meta's command menu

`.help_command()` adds `/help`, generated from the commands that are not
hidden, grouped by category (`Bot::help`, or `Bot::help_sections` to
format your own). Meta's slash-command menu (conversational components)
is synced from the same list:

```rust
bot.sync_command_menu(client, number).await // at most 30 commands, each with a description
```

It sends only `commands` (the welcome message and ice breakers stay as
they are); Meta's page calls it "a list of commands to be configured" and
does not say whether a command left out is removed, so send the whole
list each time, as this does. The client's limits are checked
first: at most 30 commands, names of at most 32 characters, descriptions
of 1 to 256. A listed command without a description is an error, not a
silent omission; take a command out with `Command::menu(false)`. A tap in
the menu sends `/name`, so keep `/` among the prefixes: a menu the
bot's parser cannot read that way is an error too.

## 7. Markdown replies

`ctx.reply_markdown(md)` converts CommonMark to WhatsApp formatting and
sends it as one message per 4096 characters (Meta's text body limit):

| Markdown | WhatsApp |
| --- | --- |
| `**bold**`, headings | `*bold*` |
| `*italic*`, `_italic_` | `_italic_` |
| `~~strike~~` | `~strike~` |
| inline code, code blocks | `` `code` ``, ```` ```block``` ```` |
| `> quote` | `> quote` |
| lists | `• item`, `1. item` |
| `[text](url)`, `![alt](url)` | `text (url)`, `alt (url)`; only `http`, `https`, `mailto`, `tel` or relative URLs, so a `javascript:` or `data:` one keeps just its text |
| tables | a monospace block, columns padded |

Messages are cut between blocks (paragraphs, list items, code blocks,
tables); a code block that fits in a message is never cut, and only a
block longer than a whole message is split inside.

Text is left as written (`NoEscape`, the default), so what a reader
copies from a reply (an email address, a coupon code, a `/command`, a
`www.` link) is what you wrote. Meta documents no escape syntax for
WhatsApp text, so a literal `*`, `_` or `~` can still format there.
`WordJoinerEscape` is the opt-in alternative: an invisible U+2060 WORD
JOINER on each side of such a character keeps it from formatting, but it
is copied along with the text (a copied `john_doe@example.com` or
`/add_item` no longer works) and cuts WhatsApp's link detection short.
Choose with `Renderer::escape` and `.markdown(renderer)` on the builder.

## 8. Testing

Build the bot on a client with a `ScriptedTransport` (feature `testing`)
and deliver events parsed from Meta's documented payloads; assert the
exact requests (see the skill's example and
[`meta-whatsapp-rs-testing`](../../skills/meta-whatsapp-rs-testing/SKILL.md)).
Or implement `Outbound` to record what the bot sends.

## Not here yet

Paced broadcasts and scheduling (a later release), conversation state and
multi-step forms (keep them in your store, keyed by `Sender::key`).
