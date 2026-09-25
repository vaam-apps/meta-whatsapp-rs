---
name: add-webhook-field
description: "Recipe for supporting a new WhatsApp webhook field or inbound message type in meta-whatsapp-webhooks — typed value, normalized WebhookEvent, forward-compatible Unknown fallback, fixture tests from Meta's examples. Use when Meta adds a webhook field, a message type, or a property to an existing payload."
metadata:
  internal: true
---

# Adding a webhook field or message type

1. Read `.meta-docs/webhooks/reference/<field>.md` (or
   `webhooks/reference/messages/<type>.md`). Save its example payload as a
   fixture under `crates/meta-whatsapp-webhooks/tests/fixtures/` (our own trimmed copy
   of the JSON shape; placeholders filled with plausible values).
2. Add the typed value struct/enum variant. Optional everything that the
   docs mark conditional; identities follow the BSUID rules (`user_id`
   always, `wa_id`/`username` optional).
3. Map it in the normalizer to a `WebhookEvent` variant carrying `waba_id`,
   `phone_number_id` (if the field has one) and user identity.
4. Keep the `Unknown { field, raw }` path: an unknown field or type must
   still parse. Add a test proving an unknown type round-trips as `Unknown`.
5. Tests: parse the fixture; assert every documented property; assert the
   normalized events (count, variants, ids); assert a valid signature over
   the exact fixture bytes verifies and a one-byte change does not.
6. Update `docs/coverage.md`, and the consumer skill in `skills/` if the
   event is part of the public surface.
7. `just ci`.
