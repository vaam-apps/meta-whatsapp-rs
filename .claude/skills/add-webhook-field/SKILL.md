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
6. The service (`meta-whatsapp-server`) serves the library's events as
   its v1 API, and its tests read the library's files: a library-only
   change can fail them.
   - Snapshots: `tests/event_data.rs` walks
     `crates/meta-whatsapp-webhooks/tests/fixtures`. Run
     `META_WHATSAPP_SERVER_UPDATE_SNAPSHOTS=1 cargo test -p meta-whatsapp-server --all-features --test event_data`
     and review what it wrote under
     `crates/meta-whatsapp-server/tests/snapshots/event_data/`. A new
     snapshot is fine; a **changed existing snapshot is a v1 API
     change**, where only additions are allowed: stop and raise it.
   - A new `WebhookEvent::kind` (the test text-parses it in
     `src/event.rs`): classify it in `TENANT_EVENT_TYPES` (tenants receive
     it) or `OPERATOR_EVENT_TYPES` (operator-only) in
     `crates/meta-whatsapp-server/src/events.rs`. A tenant type changes the
     `KnownEventType` schema: regenerate the document with
     `cargo run -p meta-whatsapp-server -- openapi > crates/meta-whatsapp-server/openapi/v1.json`.
     Make `meta_time` in the same file return the date Meta gives the
     event (routing refuses an event dated before its binding), unless it
     carries none (`every_dated_event_type_has_its_meta_time` says which).
7. Update `docs/coverage.md`, and the consumer skill in `skills/` if the
   event is part of the public surface.
8. `just ci`.
