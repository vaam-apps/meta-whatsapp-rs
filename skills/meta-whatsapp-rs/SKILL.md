---
name: meta-whatsapp-rs
description: "Start here for meta-whatsapp-rs, the Rust toolkit for Meta's WhatsApp Business Platform (Cloud API client, webhooks, storage adapters, Typst documents). How to add it as a git dependency, its Cargo features and crates, the rules every integration follows (E.164 numbers with a plus, BSUID keys, branch on ErrorKind, sends never retried blindly), and which meta-whatsapp-rs-* skill covers which task. Load when a project depends on meta-whatsapp-rs or is about to, when someone asks how to do anything with WhatsApp in Rust, or when unsure which meta-whatsapp-rs skill applies."
---

# meta-whatsapp-rs

> **Verified against meta-whatsapp-rs 68c8321127ea3dfa2691b2edbc1be7c3e347b970 (2026-09-26).** On another revision, trust the code over this page (see "Versioning" below).

meta-whatsapp-rs is a Cargo workspace for Meta's WhatsApp Business Platform: a typed
client for the Cloud API and the Business Management API (Graph API
v25.0), webhook verification and parsing, storage/transport/sink adapters,
and Typst-rendered documents. It serves three products: e-commerce
marketing, a multi-tenant CMS whose merchants chat with their customers,
and WhatsApp OTP login.

## When to use

Load this first for any code that depends on meta-whatsapp-rs; it routes to the
task skills below. Load the task skill before writing that kind of code.

## Install

Depend on the facade crate `meta-whatsapp-rs` (lib name
`meta_whatsapp_rs`); it re-exports the others. It is not on crates.io (no
release yet; the workspace is `publish = false`), so pin a git revision:

```toml
[dependencies]
meta-whatsapp-rs = { git = "https://github.com/vaam-apps/meta-whatsapp-rs", rev = "<commit>", features = ["axum", "postgres"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
anyhow = "1"
```

- `<commit>`: a commit of `main` from the rename on
  (`git ls-remote https://github.com/vaam-apps/meta-whatsapp-rs main`
  prints the newest). Before it the crate was `wa-rs`, and
  `meta-whatsapp-rs` does not resolve at an older revision.
- ~~The repository is private: Cargo needs credentials that can read
  it~~: not so at 2026-09-24, `vaam-apps/meta-whatsapp-rs` is public; no
  credentials.
- Rust 1.98.1 or newer, edition 2024. You bring the tokio runtime.
- axum, sqlx and redis types cross the API: use the re-exports
  `meta_whatsapp_rs::webhooks::axum` (feature `axum`),
  `meta_whatsapp_rs::adapters::store::postgres::sqlx` (feature `postgres`) and
  `meta_whatsapp_rs::adapters::store::redis` (feature `redis`) instead of pinning
  your own. ~~There is no redis re-export~~: true until 4eb93c9
  (2026-09-24).

| Feature | Default | Adds |
| --- | --- | --- |
| `reqwest` | yes | `ReqwestTransport` and the `meta_whatsapp_rs::client(token)` / `meta_whatsapp_rs::client_builder()` shortcuts |
| `memory` | yes | `MemoryKvStore`, `MemoryConversationStore` (tests, one instance) |
| `sinks` | yes | channel, broadcast, fan-out, filter, fn and tracing sinks |
| `postgres` | | `PostgresKvStore`, `PostgresConversationStore`, migrations |
| `redis` | | `RedisKvStore` |
| `axum` | | `webhooks::router` (the endpoint) and `webhooks::sse` (live stream) |
| `typst` | | `meta_whatsapp_rs::typst`: invoices, receipts, vouchers → PDF/PNG |
| `flows-endpoint` | | WhatsApp Flows data-endpoint crypto |
| `full` | | all of the above |
| `testing` | | `ScriptedTransport` for your own tests; `[dev-dependencies]` only, not in `full` (`meta-whatsapp-rs-testing`) |

`use meta_whatsapp_rs::prelude::*;` brings the client, ids, `Recipient`, the message
and template builders, the webhook pieces, the store and sink traits and
the inbox. It leaves out `Result`: write `meta_whatsapp_rs::Result`.

## Which skill for which task

| Task | Skill |
| --- | --- |
| Meta app setup, building the `Client`, tokens, API version | `meta-whatsapp-rs-setup` |
| Handling errors, deciding what may be retried | `meta-whatsapp-rs-errors` |
| Testing your code without Meta or a database | `meta-whatsapp-rs-testing` |
| Text, media, location, contacts, reactions, read receipts | `meta-whatsapp-rs-send-messages` |
| Buttons, lists, CTA links, Flows, carousels | `meta-whatsapp-rs-interactive-messages` |
| Uploading and downloading media, template header handles | `meta-whatsapp-rs-media` |
| Creating and managing message templates | `meta-whatsapp-rs-templates` |
| Sending a template with its parameters | `meta-whatsapp-rs-send-templates` |
| Phone-number login with WhatsApp codes | `meta-whatsapp-rs-otp-login` |
| Merchants connecting their own number (Tech Provider or Solution Partner) | `meta-whatsapp-rs-embedded-signup` |
| Storing merchants' tokens, acting as a merchant | `meta-whatsapp-rs-token-vault` |
| Registering numbers, PINs, business profile, subscriptions | `meta-whatsapp-rs-phone-numbers` |
| The webhook endpoint Meta calls | `meta-whatsapp-rs-webhook-endpoint` |
| What each webhook event means and what to do with it | `meta-whatsapp-rs-webhook-events` |
| Fan-out, live views over SSE, background workers | `meta-whatsapp-rs-live-updates` |
| The merchant ↔ customer chat inbox of a CMS | `meta-whatsapp-rs-cms-inbox` |
| Blocking a customer, group chats, WhatsApp calls | `meta-whatsapp-rs-groups-and-calling` |
| Campaigns, opt-ins and opt-outs, analytics, QR codes | `meta-whatsapp-rs-marketing` |
| Catalogs, product messages, carts | `meta-whatsapp-rs-commerce` |
| Invoices, receipts, vouchers as PDF/PNG | `meta-whatsapp-rs-documents` |
| WhatsApp Flows and their data endpoint | `meta-whatsapp-rs-flows` |
| Memory, Postgres or Redis stores, your own adapter | `meta-whatsapp-rs-storage` |
| Secrets, logs, limits, several instances, going live | `meta-whatsapp-rs-production` |
| An app not written in Rust, through the HTTP service meta-whatsapp-server | `meta-whatsapp-rs-server` |
| Receiving WhatsApp events through meta-whatsapp-server (Meta's webhook, polling) | `meta-whatsapp-rs-server-events` |
| Sending messages, media and templates through that service | `meta-whatsapp-rs-server-send` |

Anything else the client wraps: its rustdoc
(`cargo doc -p meta-whatsapp-rs --all-features --open`), starting at `meta_whatsapp_rs::client`.

## Rules every integration follows

From the runnable example
[`send_message.rs`](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/crates/meta-whatsapp-rs/examples/send_message.rs)
(`env` there is `std::env::var` naming the missing variable):

```rust
let client = meta_whatsapp_rs::client(env("WA_TOKEN")?)?;
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
5. **meta-whatsapp-rs knows WABAs and phone number ids, not your tenants.** Check
   that the caller owns a number before acting on it.
6. Secrets (`AccessToken`, `AppSecret`, PINs, OTP codes) print
   `[REDACTED]`; never log what `expose_secret()` returns.

## Versioning

Each skill names the meta-whatsapp-rs commit it was verified against, under its
title: every name it uses was checked against the source at that commit,
and every Rust block is an excerpt of code meta-whatsapp-rs compiles and tests in its
own gate. On another `rev`, trust the rustdoc
(`cargo doc -p meta-whatsapp-rs --all-features --open`) over the skill.

## What meta-whatsapp-rs does not do

- No tenant model, no job queue or outbox, no consent registry.
- Not wrapped: Solution Partner APIs beyond credit lines (partner-led
  verification, Multi-Partner Solutions, migration), payments,
  conversation routing
  ([coverage](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/coverage.md)).
- Open product decisions, with what the code does today:
  [OPEN_QUESTIONS.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/OPEN_QUESTIONS.md).
  Read it before production.

## Related skills

Every `meta-whatsapp-rs-*` skill in the table above. The integrator guides are in
[docs/guides](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/README.md).
