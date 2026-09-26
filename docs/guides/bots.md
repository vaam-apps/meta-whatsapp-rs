# Bots

**Goal:** a WhatsApp number that answers commands (`/status 1234`), taps
on reply buttons and list rows, and free text, with formatted replies;
built from plugins, one feature each, on the webhook endpoint you already
run; and that sends one message to many customers at the pace Meta
allows (section 9).

Crate: `meta-whatsapp-bot`, re-exported as `meta_whatsapp_rs::bot`
(feature `bot`, off by default; `full` includes it). Agent skill:
[`meta-whatsapp-rs-bot`](../../skills/meta-whatsapp-rs-bot/SKILL.md), whose
[`examples/bot.rs`](../../skills/meta-whatsapp-rs-bot/examples/bot.rs) is
compiled and tested by the gate; the snippets below are copied from it
(the copies are not checked: on doubt, the example wins).

Each event goes through these steps, in this order:

```text
WebhookHandler ─► DedupGuard ─► Bot (an EventSink<WebhookEvent>)
  1. ban:    a received message from a banned sender stops here; nothing below runs
  2. match:  `/name args` (or an image or video caption), or a tapped button /
             list row whose id is a payload → ctx.invocation();
             a `/name` no command has → ctx.unknown_command()
  3. middleware, in registration order (any may stop the event; they see the match)
  4. the command's guards: scope → owner → cooldown → its handler;
     else the unknown-command handler, if any; else the listeners
```

Coming from Zaileys: there, middleware runs for commands only, after
their guards; here it runs for every event (the listeners' too), after
the ban and the match and before the command's own guards.

```toml
[dependencies]
meta-whatsapp-rs = { git = "https://github.com/vaam-apps/meta-whatsapp-rs", rev = "<commit>", features = ["axum", "bot"] }
```

The async extension points (`Outbound`, `Middleware`, `Plugin`,
`AccessPolicy`, `Cooldowns`, `Refusals`, `ErrorHandler`, `RateLimiter`,
`Timer`) are `#[async_trait]` traits. The attribute is re-exported as
`meta_whatsapp_rs::bot::async_trait`, so you need no `async-trait`
dependency of your own (write `#[async_trait]` on the `impl`: a native
`async fn` there does not match the trait).

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
    .usage("<order number>") // shown after the name in the help
    .description("Where is my order?")
    .cooldown(Duration::from_secs(10)) // per user, per business number
    .payload("order_status"), // a reply button or list row with this id runs it too
);
```

- **Syntax.** The default parser (`PrefixParser`) takes `/` or the
  prefixes you give (`.prefixes(["/", "!"])`); the longest matching prefix
  wins. Arguments split on whitespace; `"double quotes"` (and the curly
  quotes phone keyboards type) keep a phrase together.
- **Case.** Names compare as the parser gives them. `PrefixParser`
  lowercases typed and registered names alike; `PrefixParser::ignore_case`
  with `false` keeps them as written. A parser of your own
  (`CommandParser`) decides both through `parse` and `normalize`.
- **Captions.** The caption of an image or a video is read like a text
  (`Trigger::Caption`); the media is in `ctx.message()`. With
  `BotBuilder::commands_from_captions(false)`, a captioned `/name` is a
  plain media message instead, for the listeners.
- **Buttons as commands.** `Command::payload` registers the id of a reply
  button, a list row or a template quick-reply button: tapping it runs the
  command with no arguments (`Trigger::Payload`).
- **Unknown commands.** A `/name` no command has sets
  `ctx.unknown_command()` (the name as the parser read it). With
  `BotBuilder::unknown_command(handler)`, that handler takes it; without
  one, the listeners get it like any text. Answering every unknown name
  is noisy in groups: check `ctx.chat()` first.
- **Replies.** `ctx.reply(text)` quotes the message
  (`messages/contextual-replies`). It goes to the group for a group
  message, else to the sender's business-scoped user id (BSUID), else to
  `+<wa_id>`. That choice is not a trait: to answer someone else or
  without the quote, build an `OutboundMessage` and `ctx.send` it.
  `ctx.react(emoji)` reacts to the message (`messages/reaction-messages`;
  Meta's group page lists no reactions, so one in a group may be
  refused). A sender is keyed the same way (`BotSender::key`): since 2026
  a message may carry no phone number at all. Standby copies, your own
  echoes and synchronized history never run a command, and there is
  nothing to reply to in them.
- **Listeners.** `Listen::Messages` gets every received message no
  command took: reactions, edits, deletions and system notices too, so
  check the content before an automatic answer (else a user's reaction
  to your reply gets a reply of its own). `Listen::message_type("image")`
  narrows to Meta's `type`; `Listen::event("status_updated")` gets other
  events, and a kind not in `WebhookEvent::KINDS` fails `build`.
- **Metadata.** `Command::metadata(key, value)` keeps your own data with
  the command (`CommandInfo::metadata`); the bot never reads it.

## 2. Guards

The ban is checked first, before the match and the middleware; the
command's own guards run after the middleware, in this order. A refusal
goes to the bot's `Refusals` (by default, `ReplyRefusals`):

| Guard | Declared with | Refusal |
| --- | --- | --- |
| banned sender | `AccessList::ban` (BSUID) or `AccessList::ban_phone` | silent; nothing runs, so no read receipt, typing indicator, command or listener either |
| wrong chat | `Command::group_only`, `Command::private_only` | a short reply |
| not an owner | `Command::owner_only` + `AccessList::owner` | silent |
| cooldown running | `Command::cooldown` | "Please wait N s …", once per cooldown |

List owners and bans **by BSUID**: a username adopter arrives without a
phone number, so a ban by phone number alone can be walked around. A
BSUID belongs to one business portfolio (list the parent BSUID for
several) and changes when the user changes phone number, so a ban is a
filter, not a guarantee.

The cooldown is checked last, so a refused attempt starts none. It lives
in the `KvStore` you pass (`.cooldown_store(kv, clock)`, namespace
`wa.bot.cooldown`, keys hashed so no phone number is stored): with
Postgres or Redis, every instance shares it. A user who keeps trying
during a cooldown is told once, not once per attempt: the first refusal
writes a marker (`wa.bot.cooldown.notice`) that ends with the cooldown,
and `Refusal::CoolingDown` says `notify` only for that one. Expired
records are invisible at once; Redis deletes them itself, and with the
memory or Postgres store call `purge_expired()` now and then, as for the
other typed stores. Owners, bans and cooldowns are traits
(`AccessPolicy`, `Cooldowns`) when a list in code does not fit.

## 3. Middleware

Code around every event, after the ban and the match, before the
command's guards. `ctx.invocation()` is already set, so a middleware can
act for some commands only; one that does not call `next.run(ctx)` stops
the event:

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
never content, senders or error text) and `MarkRead` (a read receipt,
`messages/mark-message-as-read`; `MarkRead::with_typing_indicator` also
shows "typing…" (`typing-indicators`), falling back to a plain receipt
when Meta refuses it). `ctx.insert(value)` hands a value to later
middleware and the handler.

## 4. Plugins

A `Plugin` is one feature: a name, a category (its section in the help;
`None` puts it in the bot's default category), a description, `hidden`,
and a `setup` that registers its commands, middleware and listeners. It
must be `Debug`. `Bot::builder().plugin(p)` adds it; `build()` runs the
`setup`s in order.

**No hot reload.** Plugins are compiled in. Rust has no stable ABI, so a
plugin loaded at run time must come from the exact same compiler and
dependency versions or it is undefined behaviour, and loading code at run
time is what an attacker who can write to disk wants. Ship a plugin as a
crate, depend on it, redeploy to change it. `Bot::unload` runs each
plugin's `on_unload` at shutdown; the bot then refuses events
(`SinkError::Closed`) so Meta redelivers them to an instance still
running.

## 5. Wiring it to the webhook

`build` is `async` (the plugins' `setup` run in it):

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

```rust
let verifier = SignatureVerifier::new(vec![app_secret])?;
let handler = WebhookHandler::builder(verifier, verify_token, Arc::new(bot))
    .dedup(DedupGuard::new(kv)) // Meta retries for 7 days: a retry must not run a command twice
    .build();
```

Keep the `DedupGuard`: Meta retries for 7 days, and without it a retry
runs the command again.

A failing handler is logged and acknowledged by the default `LogErrors`,
because an error answers `500` and Meta redelivers the whole batch,
repeating every reply already sent in it. The price: a transient failure
(the cooldown store or your `AccessPolicy` unreachable, a Graph 5xx on a
reply) is acknowledged too, and that event is lost but for the log
line. The owner decided on a dead-letter store for such losses on the
whole webhook path ([open question](../../OPEN_QUESTIONS.md#webhooks-and-live-updates)
30, roadmap item L21); it is not built yet. Until then, an
`ErrorHandler` of your own can keep the event (`ctx.event()`) and return
`Ok`. It gets the context as the bot built it, after the ban check and
the match, before the middleware: the command is known, values a
middleware inserted are not. `PropagateErrors` opts into redelivery for
idempotent handlers (a command with a cooldown is refused on its
redelivery: the cooldown started before the failure).

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
checks, as the [CMS inbox](cms-inbox.md) does. One outbound shared by
several bots: `BotBuilder::shared_outbound(Arc<dyn Outbound>)`. The
builder's other trait parameters (parser, access, cooldowns, refusals,
errors, middleware, Markdown renderer, help format) accept an `Arc` of an
implementation too; handlers and plugins are registered by value.

## 6. Replies in the CMS inbox

In the CMS, the merchant's inbox should show what the bot answered.
Two pieces, both in the skill's example: an `Outbound` that sends through
`Inbox::send` (which checks the 24-hour window and records the sent
message), and a sink that records each event in the inbox before the
bot handles it.

```rust
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
```

```rust
#[async_trait]
impl EventSink<WebhookEvent> for InboxThenBot {
    async fn deliver(&self, event: WebhookEvent) -> Result<(), SinkError> {
        self.inbox.deliver(event.clone()).await?;
        self.bot.deliver(event).await
    }
}
```

Record first, then the bot: `Inbox::send` checks the window against the
stored conversation, so the message being answered must be stored
before the reply. A `FanoutSink` of the `InboxSink` and the bot delivers
to both at once, and a first message's reply could be refused as outside
the window; put a `FanoutSink` around this pair instead, next to the
broadcast sink of the live view (`meta-whatsapp-rs-live-updates`). Read
receipts go through the client (`ClientOutbound::mark_read`): the inbox's
own `Inbox::mark_read` is its unread count, not Meta's receipt.

## 7. Help and Meta's command menu

`.help_command()` adds `/help` ("Show the commands"), generated from the
commands that are not hidden, grouped by category, in the `CategoryHelp`
format (`/name, /alias <usage> — description`). The pieces are yours to
change:

- `BotBuilder::help_command_with(name, description, formatter)`: another
  name and description (the menu shows the description) and a
  `HelpFormatter` of your own;
- `BotBuilder::default_category`: the section of commands without a
  category (default `DEFAULT_CATEGORY`, "General");
- in a handler, `ctx.commands()` and `ctx.help_sections()` are the bot's
  lists, to write a help entirely your own (`Bot::commands` and
  `Bot::help_sections` outside one).

Meta's slash-command menu (conversational components) is synced from the
same list:

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
bot's parser cannot read that way is an error too. Emoji in names and
descriptions are left to the client's checks.

## 8. Markdown replies

`ctx.reply_markdown(md)` renders with the bot's `MarkdownRenderer` and
sends one message per part. The default, `markdown::Renderer`, converts
CommonMark to WhatsApp formatting:

| Markdown | WhatsApp |
| --- | --- |
| `**bold**`, headings | `*bold*` |
| `*italic*`, `_italic_` | `_italic_` |
| `~~strike~~` | `~strike~` |
| emphasis inside a word (`foo**bar**baz`) | the text alone: WhatsApp formats no part of a word |
| inline code, code blocks | `` `code` ``, ```` ```block``` ````; code holding a backtick (a block, or a piece of a block cut in two: a fence, or a backtick at an end) goes out as plain text, never a broken fence |
| `> quote` | `> quote` |
| lists | `• item`, `1. item` |
| `[text](url)`, `![alt](url)` | `text (url)`, `alt (url)`; only `http`, `https`, `mailto`, `tel` or relative URLs, so a `javascript:` or `data:` one keeps just its text |
| tables | a monospace block: padded columns, else one `header: value` line per cell, else unpadded rows (below) |

A table is laid out as padded columns while a padded row is at most 60
characters wide (`Renderer::table_max_width`: a wider row wraps on a
phone and its columns stop lining up), else as records (a
`header: value` line per non-empty cell, a blank line between rows),
and as either only while that text is at most twice the table's
unpadded rows, or one message, whichever is larger
(`Renderer::table_max_growth`); past both, as the unpadded rows
themselves. Padding a thousand rows to one very wide cell, or repeating
a long header on every record, would otherwise turn a few kilobytes of
Markdown into thousands of messages, each a billable send.

Messages are cut between blocks (paragraphs, list items, code blocks,
tables); a code block that fits in a message is never cut, and only a
block longer than a whole message is split inside. Meta's limit is 4096
characters (the client's `TEXT_BODY_MAX_CHARS`) and does not say which
unit it counts, so parts are measured in UTF-16 code units, never fewer
than the characters the client counts: a part fits under either reading,
and emoji-dense text makes more, shorter parts. `Renderer::max_chars`
lowers the limit, never above 4096.

Text is left as written (`NoEscape`, the default), so what a reader
copies from a reply (an email address, a coupon code, a `/command`, a
`www.` link) is what you wrote. Meta documents no escape syntax for
WhatsApp text, so a literal `*`, `_` or `~` can still format there.
`WordJoinerEscape` is the opt-in alternative: an invisible U+2060 WORD
JOINER on each side of such a character keeps it from formatting, but it
is copied along with the text (a copied `john_doe@example.com` or
`/add_item` no longer works) and cuts WhatsApp's link detection short.
Choose with `Renderer::escape`, and set the renderer with
`.markdown(renderer)` on the builder; for other rules (another heading
or table style), implement `MarkdownRenderer` and pass that.

## 9. Paced broadcasts

A campaign, an order-status sweep, a notice to every customer: one
message to many recipients from one business number, sent no faster than
Meta allows, with progress, a cancel, retries and a report per recipient.

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

`BroadcastBuilder::content` sends every recipient the same content;
`BroadcastBuilder::compose` builds each one's message (their name in the
template, a `biz_opaque_callback_data` to match the status webhooks). A
message composed for someone else, or a compose error, fails that
recipient without a send.

**Each person once.** A person listed twice gets one message: every send
is billed, and a duplicate marketing message harms the merchant. The same
person is the same phone number compared by its digits (`+1 650-555-1234`
and `16505551234`), the same business-scoped user id, the same group, or
a phone number and a user id that one `Recipient::PhoneAndUser` carried
together. The repeat's line is `Outcome::Duplicate`, naming the first
listing; `BroadcastBuilder::dedupe(false)` sends the list as given.

**The pace.** Meta's throughput (`throughput`) is per registered
business number: 80 messages a second by default (`Rate::DEFAULT`),
1,000 once Meta upgrades the number (`Rate::HIGHER_THROUGHPUT`), 20 for
a number also used in the WhatsApp Business app (`Rate::BUSINESS_APP`),
counting inbound and outbound messages of every type. The default
`RateLimiter`, `TokenBucket`, gives each number its rate
(`TokenBucket::rate_for` for the ones that differ) and spaces the sends
evenly, so no one-second window holds more than the rate
(`TokenBucket::burst` allows a few at once, and that many more in a
window). Meta documents no mapping from a number's reported throughput
level to a rate, so set the rate yourself, for example on a
`phone_number_quality_update` webhook announcing an upgrade.

```rust
pub fn shared_pacer(upgraded: PhoneNumberId) -> Pacer {
    Pacer::new(TokenBucket::new(Rate::DEFAULT).rate_for(upgraded, Rate::HIGHER_THROUGHPUT))
}
```

- **One `Pacer` per process**, shared by every broadcast
  (`BroadcastBuilder::pacer`) and by the bot (`BotBuilder::pacer`, which
  puts its outbound in a `PacedOutbound`: replies, refusals, read
  receipts and typing indicators wait for a slot too). Two pacers give
  one number two budgets. A reply waits behind at most a broadcast's
  concurrency in slots.
- **Group operations** go through the same budget through
  `PacedGroups::new(client.groups(number), pacer)` and its `group(id)`
  (a `PacedGroup`): each of the client's group calls, after a slot of the
  number. Meta documents no rate for group operations; this is a choice,
  and the client's own calls are not paced by themselves.
  `Pacer::acquire(&number)` paces any other call.
- **Several replicas** each count their own sends: two replicas at 80 a
  second send 160. Give each its share of the rate
  (`Rate::per_second`), or implement `RateLimiter` over a store they
  share (`RateLimiter::reserve` books a slot and returns the wait;
  `RateLimiter::slow_down` hears Meta's throttling). None ships.
- **Concurrency.** Up to `BroadcastBuilder::concurrency` sends (default
  32) are in flight at once, so the rate is reached when each send
  takes a while: about the rate times a send's duration.
- Don't give a broadcast a `PacedOutbound`: it paces its own sends, and
  each would take two slots.

**Failed sends.** A `BroadcastPolicy` decides, from the error's kind
(its code, never its text); the default is `Backoff`:

| Meta answers | `Backoff` |
| --- | --- |
| `131056`, the pair rate limit (one user messaged too often) | that recipient waits 1, 4, 16, 64 s (the `4^X` schedule of `about-the-platform`); the others go on |
| `130429` throughput, or another `RateLimited` code | retried after 1, 2, 4, 8 s; the number's pacer halves its rate, back to full after 30 quiet seconds (`TokenBucket::recovery`) |
| `131057`, the number in maintenance (Meta's throughput upgrade takes it off for up to a minute, `throughput`) | retried every 20 s (`Backoff::MAINTENANCE_RETRY`), so its five sends outlast the minute; the pacer slows down |
| `131048`, sending restricted for spam | reported, not retried (Meta: retrying makes it worse); the pacer slows down |
| `131049`, the per-user marketing limit | reported for that recipient, not retried (Meta: wait at least 24 hours) |
| an error in `Backoff::STOPS` (the token, a permission, the account, the classification limit, payment) | the run stops: every recipient not sent yet is skipped |
| anything else not retryable | reported for that recipient |

Whatever the policy says, a recipient is sent again only when the error
is retryable and `Error::may_have_been_sent` is false. A timeout, or a
5xx after the request reached Meta, is reported as `Outcome::Failed`
with `may_have_been_sent()` true and never resent, since a duplicate
campaign message is worse than a missing one: match it with the status
webhooks (the callback data) before sending again.
`BroadcastBuilder::client` turns the client's own replays off
(`Client::with_retry(RetryPolicy::NONE)`), so a throttled send is
retried by the broadcast, through the pacer; give an outbound of your
own over a client the same policy.

**The report.** `run` returns a `BroadcastReport`: one
`RecipientReport` per recipient, in order (its `index`, `attempts`, and
an `Outcome`: `Sent` with Meta's response, `Failed` with the last error,
`Skipped`, or `Duplicate`), its counts (`BroadcastReport::progress`) and
how the run `Ended`. For a list too long to keep every line in memory,
`BroadcastBuilder::report_to(sink)` hands each line to a `ReportSink` as
it settles (a `tokio::sync::mpsc::Sender` is one), and the report keeps
the counts only; the run waits for the sink, and a sink that fails stops
it, so nothing is sent whose line would be lost. `Sent` means Meta
accepted the message: most
`131049` refusals arrive later, as a `failed` status webhook
(`templates/marketing-templates/per-user-limits`). Meta's daily
messaging limit (unique users per 24 hours per business portfolio,
`messaging-limits`) is Meta's to enforce; nothing here counts it.

**Cancel.** `BroadcastHandle::cancel` starts no send afterwards: a wait
for a slot or a retry ends at once, sends in flight finish and are
reported (abandoning one would leave unknown whether it went out), and
the rest are `Skipped`. `BroadcastHandle::progress` counts sent, failed,
skipped, duplicates and `Progress::remaining` meanwhile.

**Not durable.** A run lives in memory: a restart loses it, and the
report is its only record. Broadcasts that survive a restart, and
scheduled sends, are [roadmap](../roadmap.md) item B3 (a typed store on
`KvStore`).

**Time** comes from the pacer's `Timer`, a `Clock` that can wait:
`SystemClock` sleeps on Tokio; a `ManualClock` moves forward at once
when waited on, so a test runs a whole paced broadcast instantly and
reads when each send started (`Pacer::with_timer`). A `ManualClock`'s
sleeps add up, so concurrent senders do not overlap on it: to test the
rate under concurrency, implement `Timer` over Tokio's paused clock
(`tokio::time::sleep` under `#[tokio::test(start_paused = true)]`). A
wall clock stepping back neither pauses the pacer nor puts off a retry.

## 10. Testing

Build the bot on a client with a `ScriptedTransport` (feature `testing`)
and deliver events parsed from Meta's documented payloads; assert the
exact requests (see the skill's example and
[`meta-whatsapp-rs-testing`](../../skills/meta-whatsapp-rs-testing/SKILL.md)).
Or implement `Outbound` to record what the bot sends. A bot or broadcast
with a pacer on a `ManualClock` never waits for real.

A handler is a function of its context, so it can be tested without a
bot: `Ctx::new(event, outbound, renderer)`, then
`.with_invocation(Invocation::new(name, trigger, args))` for a command,
and call it.

## Not here yet

Scheduling and broadcasts that survive a restart ([roadmap](../roadmap.md)
B3), subcommands and `--flag` arguments (B1b), images, suggestions and
product cards in replies as media, buttons, lists and carousels (B1c),
and auto-delete (B4). Conversation state and multi-step forms are
yours: keep them in your store, keyed by `BotSender::key`.
