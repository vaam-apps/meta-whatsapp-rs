# Integrator guides

Task-oriented guides for engineers who put meta-whatsapp-rs into a product: the
e-commerce backend (marketing, order notifications, invoices, WhatsApp OTP
login) and the multi-tenant CMS (each merchant connects their own number
with Embedded Signup and chats with customers in-app).

They sit between the [README](../../README.md) quick start and the API
reference (`cargo doc -p meta-whatsapp-rs --all-features --open`). The design and its
reasons are in [architecture.md](../architecture.md); what exists and what
does not is in [coverage.md](../coverage.md) and
[parity.md](../parity.md), and what comes next in
[roadmap.md](../roadmap.md); product decisions, all but one decided, are
in [OPEN_QUESTIONS.md](../../OPEN_QUESTIONS.md).

> Checked against meta-whatsapp-rs **7940d15** (2026-09-24), Graph API v25.0. Every
> Rust name in these pages was checked against the source at that commit,
> and the snippets were compiled against it. On another revision, trust the
> rustdoc over these pages.

## Which guide for which job

| Guide | Read it when you | Example | Agent skill |
| --- | --- | --- | --- |
| [getting-started.md](getting-started.md) | set up the Meta app, add the dependency, send a first message, handle errors | [`send_message`](../../crates/meta-whatsapp-rs/examples/send_message.rs) | [`meta-whatsapp-rs-setup`](../../skills/meta-whatsapp-rs-setup/SKILL.md), [`meta-whatsapp-rs-errors`](../../skills/meta-whatsapp-rs-errors/SKILL.md) |
| [embedded-signup.md](embedded-signup.md) | let each merchant of your CMS connect their own number | [`embedded_signup`](../../crates/meta-whatsapp-rs/examples/embedded_signup.rs) | [`meta-whatsapp-rs-embedded-signup`](../../skills/meta-whatsapp-rs-embedded-signup/SKILL.md), [`meta-whatsapp-rs-token-vault`](../../skills/meta-whatsapp-rs-token-vault/SKILL.md) |
| [webhooks.md](webhooks.md) | deploy the webhook endpoint and decide what to do with each event | [`cms_inbox`](../../crates/meta-whatsapp-rs/examples/cms_inbox.rs) | [`meta-whatsapp-rs-webhook-endpoint`](../../skills/meta-whatsapp-rs-webhook-endpoint/SKILL.md), [`meta-whatsapp-rs-webhook-events`](../../skills/meta-whatsapp-rs-webhook-events/SKILL.md), [`meta-whatsapp-rs-live-updates`](../../skills/meta-whatsapp-rs-live-updates/SKILL.md) |
| [cms-inbox.md](cms-inbox.md) | build the merchant ↔ customer chat (history, live updates, replies) | [`cms_inbox`](../../crates/meta-whatsapp-rs/examples/cms_inbox.rs) | [`meta-whatsapp-rs-cms-inbox`](../../skills/meta-whatsapp-rs-cms-inbox/SKILL.md) |
| [marketing-and-commerce.md](marketing-and-commerce.md) | send campaigns and order updates, collect opt-ins, show products | [`send_message`](../../crates/meta-whatsapp-rs/examples/send_message.rs) | [`meta-whatsapp-rs-marketing`](../../skills/meta-whatsapp-rs-marketing/SKILL.md), [`meta-whatsapp-rs-commerce`](../../skills/meta-whatsapp-rs-commerce/SKILL.md), [`meta-whatsapp-rs-templates`](../../skills/meta-whatsapp-rs-templates/SKILL.md), [`meta-whatsapp-rs-send-templates`](../../skills/meta-whatsapp-rs-send-templates/SKILL.md) |
| [otp-login.md](otp-login.md) | log users in (or verify a number) with a WhatsApp code | [`otp_login`](../../crates/meta-whatsapp-rs/examples/otp_login.rs) | [`meta-whatsapp-rs-otp-login`](../../skills/meta-whatsapp-rs-otp-login/SKILL.md) |
| [documents.md](documents.md) | send invoices, receipts and voucher images | [`invoice_document`](../../crates/meta-whatsapp-rs/examples/invoice_document.rs) | [`meta-whatsapp-rs-documents`](../../skills/meta-whatsapp-rs-documents/SKILL.md) |
| [production.md](production.md) | pick storage, manage keys, log safely, scale, pin versions | — | [`meta-whatsapp-rs-production`](../../skills/meta-whatsapp-rs-production/SKILL.md), [`meta-whatsapp-rs-storage`](../../skills/meta-whatsapp-rs-storage/SKILL.md) |
| [server.md](server.md) | run the HTTP service for an app not written in Rust: configure it, create tenants and keys, send, receive Meta's webhooks and poll events | [`client.ts`](../../skills/meta-whatsapp-rs-server/examples/client.ts), [`send.ts`](../../skills/meta-whatsapp-rs-server-send/examples/send.ts), [`events.ts`](../../skills/meta-whatsapp-rs-server-events/examples/events.ts) (TypeScript callers, type-checked against the service's OpenAPI document) | [`meta-whatsapp-rs-server`](../../skills/meta-whatsapp-rs-server/SKILL.md), [`meta-whatsapp-rs-server-send`](../../skills/meta-whatsapp-rs-server-send/SKILL.md), [`meta-whatsapp-rs-server-events`](../../skills/meta-whatsapp-rs-server-events/SKILL.md) |

## Reading order

**E-commerce backend:** getting-started → webhooks → marketing-and-commerce
→ documents → otp-login → production.

**CMS:** getting-started → embedded-signup → webhooks → cms-inbox →
production.

**Not in Rust:** getting-started (the Meta app) → server.

## How to read the code

- Snippets are excerpts. Names such as `merchant_id`, `save_merchant_waba`
  or `pool` stand for your own code and values; everything imported from
  `meta_whatsapp_rs` is the real API.
- `use meta_whatsapp_rs::prelude::*;` brings in the client, ids, `Recipient`, the
  message and template builders, the webhook pieces, the store and sink
  traits and the inbox. Everything else is one path away.
- Each runnable example's header lists the environment variables it reads
  and the command that runs it. The two servers (`embedded_signup`,
  `cms_inbox`) refuse to start without `WA_TENANTS`, a bearer-token
  stand-in for your own authentication, and listen on `127.0.0.1` unless
  `WA_BIND` says otherwise.

## Meta's documentation

Meta's pages are the authority on everything that happens on Meta's side
(dashboards, review, limits, prices). Links point at
`https://developers.facebook.com/documentation/business-messaging/whatsapp/<path>`;
append `.md` to any of them for Markdown. The guides paraphrase them and
were written against the pages as they read on 2026-09-24; Meta changes
dashboards without notice, so when a screen looks different, the linked page
wins.
