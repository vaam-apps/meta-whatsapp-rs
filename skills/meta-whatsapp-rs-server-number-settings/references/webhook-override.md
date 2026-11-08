# A WABA's health status and a number's webhook override

> **Verified against meta-whatsapp-rs 580585f006a10e12060844044e84fd8d7fcfe513 (2026-11-08).** Source: `docs/design/server.md` D440 to D445 and `tests/webhook_override.rs`.

The routes are in [../SKILL.md](../SKILL.md); the calls are the functions
wabaHealth, webhookOverride, setWebhookOverride and clearWebhookOverride of
[../examples/number-settings.ts](../examples/number-settings.ts).

## A WABA's health

`GET /v1/wabas/{waba_id}/health-status` answers Meta's overall
`can_send_message` and the nodes you own or already hold: your WABA, your
numbers, its templates, the business portfolio and the app. A phone number
or WABA that is another tenant's, and any node of a type the service does
not know, is left out with its errors, and an id of another node inside a
kept node's texts is replaced by `[id]`. Another tenant's WABA, or one that
does not exist, is `404 not_found` with nothing asked of Meta.

## Why the override is off by default

With an override Meta delivers the number's messages, calls, history and
the other supported webhooks (never templates' or accounts') to **your**
URL instead of the service, whose inbox, events and OTP delivery statuses
for that number then go quiet. So:

- `PUT` needs a key with the scope `webhook_override` (a scope of its own:
  `numbers` does not grant it and an operator must give it; without it,
  `403 forbidden`), and is `403 feature_not_available`, with nothing asked
  of Meta, until the deployment sets `WA_SERVER_NUMBER_WEBHOOK_OVERRIDE=on`.
  `GET` and `DELETE` need `numbers` only.
- The URL goes through the **same check as a webhook endpoint's**: https,
  no user name or password, a host the operator listed, every address it
  resolves to allowed (never a private, loopback, link-local or metadata
  address unless the operator listed its range). Refused: `422` on `url`,
  nothing sent, whatever the reason. At most 200 characters; `verify_token`
  (yours, the one Meta echoes to verify the URL) must not be empty. With no
  allow-list at all the answer is `409 webhooks_not_configured`. Meta is
  given the URL as the check read it (normalized: a lowercase host, a
  slash for an empty path); it connects later, so the check holds at the set only: list hosts
  you trust.
- The token and the URL are in no log, audit event, error or answer, and
  Meta's error texts are withheld. The set is audited (webhook_override_set)
  and so is a refused one, as a warning (webhook_override_refused, with its
  reason).
- `GET` answers `{url}` of the number's **own** override, or null; the
  WABA's override and the service's own callback are never answered.

## Clearing

`DELETE /v1/numbers/{pn}/webhook-override` is **always allowed**: the
setting off, the allow-list gone, a key that never could set. Meta then
delivers the number's webhooks to the service again. An operator's
platform key acting for the tenant can clear it too. It is audited
(webhook_override_cleared).
