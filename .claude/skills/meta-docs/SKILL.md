---
name: meta-docs
description: "Look up Meta's WhatsApp Business Platform docs as Markdown instead of guessing. Use before writing, changing or reviewing any Graph API call, webhook type, template component, error code or Embedded Signup step in wa-rs — and whenever a field name, limit or enum value is in doubt."
metadata:
  internal: true
---

# Meta docs lookup

Meta's WhatsApp docs are JavaScript-rendered, but every page is also served
as Markdown: append `.md` to the page URL.

```
https://developers.facebook.com/documentation/business-messaging/whatsapp/<path>.md
```

The response is an HTML shell whose `<pre>` holds the Markdown with HTML
entities; `cargo xtask meta-docs` strips and decodes it.

## Mirror everything once

```bash
just meta-docs            # → .meta-docs/<path>.md, ~370 pages, gitignored
just meta-docs --force    # refresh
```

Then grep, don't browse:

```bash
rg -l 'subscribed_apps' .meta-docs
rg -n '"type": "button"' .meta-docs/webhooks/reference/messages/
```

`.meta-docs/README.md` lists pages that could not be fetched (some are
gated or empty).

## Where things are

| Topic | Paths |
| --- | --- |
| Send API, message types | `messages/*`, `reference/whatsapp-business-phone-number/message-api` |
| Webhook payloads | `webhooks/reference/<field>`, `webhooks/reference/messages/<type>` |
| Templates | `templates/*`, `reference/whatsapp-business-account/message-template-api` |
| Auth templates | `templates/authentication-templates/*` |
| Embedded Signup | `embedded-signup/*` (v4 = `version-4`), `access-tokens` |
| Error codes | `support/error-codes` |
| BSUID / usernames | `business-scoped-user-ids` |
| Flows | `flows/guides/*` |
| Endpoint references | `reference/**` |

## Rules

- Copy request/response **examples** from the page into tests; they are
  the closest thing to a contract Meta publishes.
- When prose and example disagree, test against the example and leave a
  comment naming the page.
- Never commit mirrored text. Paraphrase in rustdoc; link the page.
- The Graph version in examples (`v25.0` as of 2026-09) is
  `ApiVersion::DEFAULT`; bumping it is a deliberate PR.
