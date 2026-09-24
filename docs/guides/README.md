# Integrator guides

Task-oriented guides for engineers who put wa-rs into a product: the
e-commerce backend (marketing, order notifications, invoices, WhatsApp OTP
login) and the multi-tenant CMS (each merchant connects their own number
with Embedded Signup and chats with customers in-app).

They sit between the [README](../../README.md) quick start and the API
reference (`cargo doc -p wa-rs --all-features --open`). The design and its
reasons are in [architecture.md](../architecture.md); what exists and what
does not is in [coverage.md](../coverage.md); decisions nobody has made yet
are in [OPEN_QUESTIONS.md](../../OPEN_QUESTIONS.md).

> Checked against wa-rs **7940d15** (2026-09-24), Graph API v25.0. Every
> Rust name in these pages was checked against the source at that commit,
> and the snippets were compiled against it. On another revision, trust the
> rustdoc over these pages.

## Which guide for which job

| Guide | Read it when you | Example | Agent skill |
| --- | --- | --- | --- |
| [getting-started.md](getting-started.md) | set up the Meta app, add the dependency, send a first message, handle errors | [`send_message`](../../crates/wa-rs/examples/send_message.rs) | [`wa-rs-setup`](../../skills/wa-rs-setup/SKILL.md), [`wa-rs-errors`](../../skills/wa-rs-errors/SKILL.md) |
| [embedded-signup.md](embedded-signup.md) | let each merchant of your CMS connect their own number | [`embedded_signup`](../../crates/wa-rs/examples/embedded_signup.rs) | [`wa-rs-embedded-signup`](../../skills/wa-rs-embedded-signup/SKILL.md), [`wa-rs-token-vault`](../../skills/wa-rs-token-vault/SKILL.md) |
| [webhooks.md](webhooks.md) | deploy the webhook endpoint and decide what to do with each event | [`cms_inbox`](../../crates/wa-rs/examples/cms_inbox.rs) | [`wa-rs-webhook-endpoint`](../../skills/wa-rs-webhook-endpoint/SKILL.md), [`wa-rs-webhook-events`](../../skills/wa-rs-webhook-events/SKILL.md), [`wa-rs-live-updates`](../../skills/wa-rs-live-updates/SKILL.md) |
| [cms-inbox.md](cms-inbox.md) | build the merchant ↔ customer chat (history, live updates, replies) | [`cms_inbox`](../../crates/wa-rs/examples/cms_inbox.rs) | [`wa-rs-cms-inbox`](../../skills/wa-rs-cms-inbox/SKILL.md) |
| [marketing-and-commerce.md](marketing-and-commerce.md) | send campaigns and order updates, collect opt-ins, show products | [`send_message`](../../crates/wa-rs/examples/send_message.rs) | [`wa-rs-marketing`](../../skills/wa-rs-marketing/SKILL.md), [`wa-rs-commerce`](../../skills/wa-rs-commerce/SKILL.md), [`wa-rs-templates`](../../skills/wa-rs-templates/SKILL.md), [`wa-rs-send-templates`](../../skills/wa-rs-send-templates/SKILL.md) |
| [otp-login.md](otp-login.md) | log users in (or verify a number) with a WhatsApp code | [`otp_login`](../../crates/wa-rs/examples/otp_login.rs) | [`wa-rs-otp-login`](../../skills/wa-rs-otp-login/SKILL.md) |
| [documents.md](documents.md) | send invoices, receipts and voucher images | [`invoice_document`](../../crates/wa-rs/examples/invoice_document.rs) | [`wa-rs-documents`](../../skills/wa-rs-documents/SKILL.md) |
| [production.md](production.md) | pick storage, manage keys, log safely, scale, pin versions | — | [`wa-rs-production`](../../skills/wa-rs-production/SKILL.md), [`wa-rs-storage`](../../skills/wa-rs-storage/SKILL.md) |

## Reading order

**E-commerce backend:** getting-started → webhooks → marketing-and-commerce
→ documents → otp-login → production.

**CMS:** getting-started → embedded-signup → webhooks → cms-inbox →
production.

## How to read the code

- Snippets are excerpts. Names such as `merchant_id`, `save_merchant_waba`
  or `pool` stand for your own code and values; everything imported from
  `wa_rs` is the real API.
- `use wa_rs::prelude::*;` brings in the client, ids, `Recipient`, the
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
