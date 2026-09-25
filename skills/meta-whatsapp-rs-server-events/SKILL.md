---
name: meta-whatsapp-rs-server-events
description: "Receiving WhatsApp events through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - pointing the Meta app's webhook at the service, how it verifies, deduplicates and routes Meta's deliveries to tenants (and keeps the rest operator-only), and polling GET /v1/events from a TypeScript backend with a stored cursor (next_after, cursor_expired, types, phone_number_id). Load when a non-Rust backend (a Medusa store, a CMS) needs incoming WhatsApp messages, delivery statuses or template reviews from meta-whatsapp-server, or when configuring Meta's callback URL for it."
---

# meta-whatsapp-rs-server-events

> **Verified against meta-whatsapp-rs 8fa8c190dbe6e5f70f913909b2865399e643c044 (2026-09-25).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/events.ts](examples/events.ts) (type-checked against the service's OpenAPI document). Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#receiving-metas-webhooks).

## When to use

Your backend is not Rust and needs what Meta sends about its WhatsApp
numbers (customers' messages, delivery statuses, template reviews,
account updates) through meta-whatsapp-server. Deploying the service and
its keys: `meta-whatsapp-rs-server` first.

## Point Meta at the service (once per Meta app)

- App Dashboard, WhatsApp, Configuration: callback URL
  `https://<public host>/webhooks/meta`, verify token the value of
  `WA_VERIFY_TOKEN`; subscribe the fields you need (messages, at least).
- Meta sends a WABA's events only once the app is subscribed to it:
  attaching one (`POST /v1/admin/tenants/{id}/wabas`) does that.
- Deliveries are signed with the app secret: `WA_APP_SECRET`, and
  `WA_APP_SECRET_PREVIOUS` while you rotate it.
- The ingress passes bodies of 3 MiB untouched (the signature covers the
  raw bytes) and waits longer than a delivery takes.

## What the service does with a delivery

- Unsigned or forged: `401`, nothing read or recorded; over 3 MiB: `413`.
- Another replica is recording the same event: `503`, Meta retries.
  Already recorded: acknowledged, not recorded again.
- **Routed by ownership, as an allow-list**: an event about a business
  number goes to the tenant that number is bound to; one naming only a
  WABA (template reviews, account updates) to the WABA's tenant. It lands
  in the inbox, then in the event outbox you poll.
- **Operator-only**, never shown to any tenant: events of a number or
  WABA no tenant holds, fields the library does not type, signed bodies
  that are not webhooks, partner solution updates, and any type the
  service has not reviewed yet (`KnownEventType` lists what you can get).
- `500` when recording failed: Meta redelivers, nothing is recorded
  twice, except `error_reported` events (Meta gives them no id).

## Poll the events

`GET /v1/events` with a key holding the `events` scope answers
`{data, next_after}`. Store `next_after`, pass it as `after` next time;
omit `after` only for the very first poll (the oldest event kept).

```ts
  const after = await cursor.load();
  const query: EventsQuery = after === undefined ? { limit: 100 } : { limit: 100, after };
  const { data, error } = await api.GET("/v1/events", { params: { query } });
```

```ts
  for (const event of data.data) {
    await handle(event);
  }
  await cursor.save(data.next_after);
  const last = data.data[data.data.length - 1];
  return last !== undefined && last.sequence === data.next_after;
```

- `next_after` is the last event's `sequence` when more may follow (poll
  again at once), else the newest sequence of the outbox: save it even
  when `data` is empty, so a quiet tenant's cursor never ages.
- Handle, then save: after a crash in between the same events come
  again, so handlers skip an `id` they already handled.
- `cursor_expired` (410): events after your cursor were purged, past
  retention (`WA_SERVER_OUTBOX_RETENTION`, 7 days by default). Rebuild
  what you derive from events, then poll without `after`.
- `invalid_request` on `after` (422): a cursor this service never issued
  (a restored or another database): resynchronise the same way.

```bash
curl -sS "$WA_SERVER/v1/events?after=18342&limit=100" -H "Authorization: Bearer $KEY"
```

## The envelope

```json
{"id": "evt_3f9c0a…", "sequence": 18342, "type": "message_received", "api_version": "v1",
 "tenant_id": "merchant-42", "phone_number_id": "106540352242922", "waba_id": "102290129340398",
 "received_at": "2026-09-25T10:00:01Z", "truncated": false,
 "data": {"event": "message_received", "message": {"id": "wamid.…", "type": "text", "text": {"body": "…"}},
          "contact": {"user_id": "US.13491208655302741918", "wa_id": "16505551234", "profile": {"name": "…"}}}}
```

`data` is meta-whatsapp-rs's event JSON: Meta's fields, normalized, the
same for every tenant. Deduplicate on `id`, order on `sequence`. Key a
customer by their BSUID (the contact's user id): the phone number may be
absent. New fields and types appear within v1: ignore what you do not
know.

```ts
  switch (event.type as KnownEventType) {
    case "message_received": {
      const data = event.data as Received;
      // Key the customer by BSUID: the phone number (wa_id) may be absent.
      const customer = data.contact?.user_id ?? data.contact?.wa_id ?? undefined;
```

## Filter

`types` (comma-separated `KnownEventType` values) and `phone_number_id`
narrow the page; a type outside `KnownEventType` is `invalid_request` on
`types`. A page holds at most `limit` events (100), and stops early past
8 MiB of `data` (history syncs are large).

```ts
  const query: EventsQuery = {
    after,
    types: "message_received,status_updated",
    phone_number_id: pn,
  };
```

## What meta-whatsapp-rs does not do

- It pushes nothing to your backend yet: poll. Live streams (SSE),
  signed webhooks to your URL and fetching one event by its id come with
  milestone M2.
- It never shows you another tenant's events, nor operator-only ones:
  an operator reads those in the service's database (outbox rows
  without a tenant).
- It does not resynchronise you after `cursor_expired`, and keeps events
  only for its retention (7 days by default; the owner's retention
  decision is still open).
- Meta's own retries, deduplication and signature are its business: you
  never talk to Meta, and never verify Meta's signature yourself.

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, errors),
`meta-whatsapp-rs-webhook-events` (what each event's fields mean, for
the Rust library the service runs on).
