---
name: wa-rs
description: "Start here for wa-rs, the Rust toolkit for Meta's WhatsApp Business Platform (Cloud API client, webhooks, storage adapters, Typst documents). How to add it as a git dependency, its Cargo features and crates, the rules every integration follows (E.164 numbers with a plus, BSUID keys, branch on ErrorKind, sends never retried blindly), and which wa-rs-* skill covers which task. Load when a project depends on wa-rs or is about to, when someone asks how to do anything with WhatsApp in Rust, or when unsure which wa-rs skill applies."
---

# wa-rs

> **Verified against wa-rs 1e63b2ba9c94fb9a4f2895f0dc9efc27ee749274 (2026-09-24).** On another revision, trust the code over this page (see "Versioning" below).

wa-rs is a Cargo workspace for Meta's WhatsApp Business Platform: a typed
client for the Cloud API and the Business Management API (Graph API
v25.0), webhook verification and parsing, storage/transport/sink adapters,
and Typst-rendered documents. It serves three products: e-commerce
marketing, a multi-tenant CMS whose merchants chat with their customers,
and WhatsApp OTP login.

## When to use

Load this first for any code that depends on wa-rs; it routes to the
task skills below. Load the task skill before writing that kind of code.

## Install

Depend on the facade crate `wa-rs` (lib name `wa_rs`); it re-exports the
others. It is not on crates.io (the name is taken there; the workspace is
`publish = false`), so pin a git revision:

```toml
[dependencies]
wa-rs = { git = "https://github.com/vaam-apps/wa-rs", rev = "4eb93c9bd63812221e75ad0920b6e2cb98ea0dd6", features = ["axum", "postgres"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
anyhow = "1"
```

- ~~The repository is private: Cargo needs credentials that can read
  it~~: not so at 2026-09-24, `vaam-apps/wa-rs` is public; no credentials.
- Rust 1.98.1 or newer, edition 2024. You bring the tokio runtime.
- axum, sqlx and redis types cross the API: use the re-exports
  `wa_rs::webhooks::axum` (feature `axum`),
  `wa_rs::adapters::store::postgres::sqlx` (feature `postgres`) and
  `wa_rs::adapters::store::redis` (feature `redis`) instead of pinning
  your own. ~~There is no redis re-export~~: true until 4eb93c9
  (2026-09-24).

| Feature | Default | Adds |
| --- | --- | --- |
| `reqwest` | yes | `ReqwestTransport` and the `wa_rs::client(token)` / `wa_rs::client_builder()` shortcuts |
| `memory` | yes | `MemoryKvStore`, `MemoryConversationStore` (tests, one instance) |
| `sinks` | yes | channel, broadcast, fan-out, filter, fn and tracing sinks |
| `postgres` | | `PostgresKvStore`, `PostgresConversationStore`, migrations |
| `redis` | | `RedisKvStore` |
| `axum` | | `webhooks::router` (the endpoint) and `webhooks::sse` (live stream) |
| `typst` | | `wa_rs::typst`: invoices, receipts, vouchers → PDF/PNG |
| `flows-endpoint` | | WhatsApp Flows data-endpoint crypto |
| `full` | | all of the above |
| `testing` | | `ScriptedTransport` for your own tests; `[dev-dependencies]` only, not in `full` (`wa-rs-testing`) |

`use wa_rs::prelude::*;` brings the client, ids, `Recipient`, the message
and template builders, the webhook pieces, the store and sink traits and
the inbox. It leaves out `Result`: write `wa_rs::Result`.

## Which skill for which task

| Task | Skill |
| --- | --- |
| Meta app setup, building the `Client`, tokens, API version | `wa-rs-setup` |
| Handling errors, deciding what may be retried | `wa-rs-errors` |
| Testing your code without Meta or a database | `wa-rs-testing` |
| Text, media, location, contacts, reactions, read receipts | `wa-rs-send-messages` |
| Buttons, lists, CTA links, Flows, carousels | `wa-rs-interactive-messages` |
| Uploading and downloading media, template header handles | `wa-rs-media` |
| Creating and managing message templates | `wa-rs-templates` |
| Sending a template with its parameters | `wa-rs-send-templates` |
| Phone-number login with WhatsApp codes | `wa-rs-otp-login` |
| Merchants connecting their own number (Tech Provider or Solution Partner) | `wa-rs-embedded-signup` |
| Storing merchants' tokens, acting as a merchant | `wa-rs-token-vault` |
| Registering numbers, PINs, business profile, subscriptions | `wa-rs-phone-numbers` |
| The webhook endpoint Meta calls | `wa-rs-webhook-endpoint` |
| What each webhook event means and what to do with it | `wa-rs-webhook-events` |
| Fan-out, live views over SSE, background workers | `wa-rs-live-updates` |
| The merchant ↔ customer chat inbox of a CMS | `wa-rs-cms-inbox` |
| Blocking a customer, group chats, WhatsApp calls | `wa-rs-groups-and-calling` |
| Campaigns, opt-ins and opt-outs, analytics, QR codes | `wa-rs-marketing` |
| Catalogs, product messages, carts | `wa-rs-commerce` |
| Invoices, receipts, vouchers as PDF/PNG | `wa-rs-documents` |
| WhatsApp Flows and their data endpoint | `wa-rs-flows` |
| Memory, Postgres or Redis stores, your own adapter | `wa-rs-storage` |
| Secrets, logs, limits, several instances, going live | `wa-rs-production` |

Anything else the client wraps: its rustdoc
(`cargo doc -p wa-rs --all-features --open`), starting at `wa_rs::client`.

## Rules every integration follows

From the runnable example
[`send_message.rs`](https://github.com/vaam-apps/wa-rs/blob/main/crates/wa-rs/examples/send_message.rs)
(`env` there is `std::env::var` naming the missing variable):

```rust
let client = wa_rs::client(env("WA_TOKEN")?)?;
let messages = client.messages(env("WA_PHONE_NUMBER_ID")?);
let to = Recipient::phone(env("WA_TO")?); // E.164, with `+`

// Free-form: delivered only inside the 24-hour customer service window.
let text = OutboundMessage::text(to.clone(), "Your order has shipped.");
match messages.send(&text).await {
    Ok(sent) => println!("text accepted as {:?}", sent.message_id()),
    Err(e) if e.kind() == ErrorKind::CustomerServiceWindowClosed => println!("window closed"),
    Err(e) => return Err(e.into()),
}
```

1. **Phone numbers are E.164 with the `+`.** Meta reads a number without
   it as local to your business number's country: the message reaches
   someone else. A webhook `wa_id` is digits only; prepend `+`.
2. **Key customers by BSUID** (`user_id`, e.g. `US.1349…`): since 2026
   webhooks may carry no phone number at all.
3. **Branch on `err.kind()`** (`ErrorKind`), never on message text.
4. **Sends are not idempotent.** A timeout or 5xx on a send is returned,
   never replayed; do not wrap sends in your own retry loop.
5. **wa-rs knows WABAs and phone number ids, not your tenants.** Check
   that the caller owns a number before acting on it.
6. Secrets (`AccessToken`, `AppSecret`, PINs, OTP codes) print
   `[REDACTED]`; never log what `expose_secret()` returns.

## Versioning

Each skill names the wa-rs commit it was verified against, under its
title: every name it uses was checked against the source at that commit,
and every Rust block is an excerpt of code wa-rs compiles and tests in its
own gate. On another `rev`, trust the rustdoc
(`cargo doc -p wa-rs --all-features --open`) over the skill.

## What wa-rs does not do

- No tenant model, no job queue or outbox, no consent registry.
- Not wrapped: Solution Partner APIs beyond credit lines (partner-led
  verification, Multi-Partner Solutions, migration), payments,
  conversation routing
  ([coverage](https://github.com/vaam-apps/wa-rs/blob/main/docs/coverage.md)).
- Open product decisions, with what the code does today:
  [OPEN_QUESTIONS.md](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md).
  Read it before production.

## Related skills

Every `wa-rs-*` skill in the table above. The integrator guides are in
[docs/guides](https://github.com/vaam-apps/wa-rs/blob/main/docs/guides/README.md).
