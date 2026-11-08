---
name: meta-whatsapp-rs-server-bot
description: "The bot through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - declarative rules the service evaluates on a number's inbound messages and answers from (commands by prefix and name, keywords, tapped button ids; a text, a template, the generated help; a per-user cooldown, a chat kind, owners only), the switch, owners and banned lists, the scope bot, why an answer is never sent twice, the 24-hour window and the template for outside it, and what the logs never hold. Load when an e-commerce store or a CMS wants automatic answers (opening hours, a menu, a price list, an away message) without writing code that runs in the service."
---

# meta-whatsapp-rs-server-bot

> **Verified against meta-whatsapp-rs a1dbc28c9f157cd372ae240cdca35e8aa23cffac (2026-11-08).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/bot.ts](examples/bot.ts), type-checked against the service's OpenAPI document. Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#the-bot). The decisions are the design's D460 to D467, the implementer's proposals awaiting the coordinator's decision, each swappable; the names below are pending the owner's confirmation and can change before the first release.

## When to use

You want automatic answers to a number's customers (opening hours, a menu,
a price list, an away message) and the code that decides is yours, in
another language: the service cannot run it. What it can do is evaluate
**rules you configure** and answer from them. To decide for yourself, read
the number's message events (`meta-whatsapp-rs-server-events`) and reply
(`meta-whatsapp-rs-server-send`, or the inbox's reply). To send one template
to many people: `meta-whatsapp-rs-server-broadcast`. Deploying the service
and its keys: `meta-whatsapp-rs-server` first. The key needs the scope
`bot`, which `send` does not grant: a rule makes the service send as your
business with nobody asking.

## The routes

All under `/v1/numbers/{pn}`:

| Route | Does |
| --- | --- |
| `GET`, `PUT …/bot` | the switch, the owners and the banned (the bot is **off** until you turn it on) |
| `GET`, `POST …/bot/rules` | the rules in the order they are tried; add one |
| `GET`, `PUT`, `DELETE …/bot/rules/{rule_id}` | read, replace and remove one |

```ts
    body: { enabled: true, owners, banned },
```

`owners` pass the rules marked `owners_only`; `banned` get no answer at all,
before any rule. Each is a phone number (with or without its `+`) or a
BSUID, at most 200 each; the call replaces both lists. Never key a person
by phone number alone: list their BSUID too.

## A rule

A rule is a **trigger**, an **action** and guards. Every word is literal
text: nothing you write is a pattern.

```ts
    trigger: { type: "command", names: ["hours", "open"] },
```

A trigger is `command` (a prefix, a slash unless you list others, then a
name, ignoring case), `keyword` (`mode` `exact`: the whole message; `contains`),
`payload` (the id of a tapped reply button or list row, or a template's
quick-reply payload; case matters) or `any` (any text, tap or media).

```ts
    trigger: { type: "keyword", words: ["price", "cost"], mode: "contains" },
```

An action is `reply` (a text of at most 4,096 characters), `template` (a
template object, as a send takes it) or `help` (the number's command rules
that have a `description`, generated). `chat` is `private` unless the rule
says `group` or `any`. `cooldown_seconds` (up to a day) holds the same
person back from the same rule, silently. Rules are tried by `priority`
(from 0 up, 100 by default), then age: the first that answers decides, a
guard that refuses ends the message too, and `continue_after` lets the
next rules answer as well (three answers to one message at most).

A number has 50 rules by default and a message is answered only if it is
less than an hour old when the service records it. Both are the
deployment's settings (`WA_SERVER_BOT_MAX_RULES`,
`WA_SERVER_BOT_MAX_MESSAGE_AGE_SECONDS`).

## Adding, keeping once

Keep one `Idempotency-Key` per rule you mean to add: the same key and body
answer the first call again and add nothing; another body under the key is
`422 idempotency_key_reused`.

```ts
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
```

A rule is checked before it is kept. A refusal is `422 invalid_request`
whose `field` names the part at fault (`trigger.names`, `trigger.prefixes`,
`action.text`, `action.template`, `cooldown_seconds`, `priority`, `name`),
or `rules` at the number's limit. Words come back in lower case.

## The window, and the template for outside it

A text goes through the inbox's reply: refused before any request when the
24-hour window is closed or another app owns the thread. A message the bot
answers just opened the window, so this matters for an old message the
deployment chose to answer. Name a template for that case and it is sent
instead; without one, nothing is sent.

```ts
      outside_window_template: { name: "price_list", language: { code: "en_US" } },
```

A template needs no open window, and is Meta's template object as a send
takes it (a template naming a catalog is refused).

## Never twice

Each answer is **claimed before it is sent**, per message and rule: a
redelivered webhook, a second replica and a retry never answer twice. The
worst case is an answer never sent (a replica dying between the claim and
the request), never a second one. A failed answer is not retried; the
reason is in the service's log, by rule id and outcome. Every answer
carries `wa-bot:` and its rule's id as its `biz_opaque_callback_data`,
which Meta echoes in the message's status events, so you can count what the
bot sent.

Answers draw on the number's budget, the one broadcasts share
(`WA_SERVER_BROADCAST_RATE`): one that cannot get a slot within a few
seconds is dropped, not queued.

## Reading and changing rules

```ts
export async function rules(api: WhatsApp, pn: string): Promise<BotRule[]> {
```

A `PUT` replaces one rule (its id and date stay) and a `DELETE` removes it;
each is an audit event (bot_rule_updated, bot_rule_deleted), as is every
creation (bot_rule_created) and every change of the switch or the lists
(bot_settings_updated), holding the key, tenant, WABA, number, the rule's
id and how many rules: never a word, a text or a person. Another tenant's
number, another number's rule and one never made are the same `404`.

```ts
  return error instanceof WhatsAppError && error.error.code === "not_found";
```

## What meta-whatsapp-rs does not do

Your trigger words, reply texts, the customers' messages, their numbers and
BSUIDs and Meta's texts are in no log line, error or audit event. Over
HTTP it offers no regular expression, no arguments, flags or
subcommands in a command, no middleware or plugin, no Markdown, image,
suggestion or carousel in an answer, no rule that forwards a message to
your own endpoint (you already receive every message as an event), and no
cap per chat. A Rust bot built on the library has most of these
(`meta-whatsapp-rs-bot`). The bot answers inline while it records the
message, so a very busy number is limited by Meta's round trips.

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, errors),
`meta-whatsapp-rs-server-events` (the message events, the status events of
the bot's answers), `meta-whatsapp-rs-server-send` (one message),
`meta-whatsapp-rs-server-broadcast` (one template to many people),
`meta-whatsapp-rs-bot` (the same ideas in the Rust library).
