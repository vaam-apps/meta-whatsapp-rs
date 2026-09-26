---
name: meta-whatsapp-rs-server-events
description: "Receiving WhatsApp events through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - pointing the Meta app's webhook at the service, how it verifies, deduplicates and routes Meta's deliveries to tenants (and keeps the rest operator-only), and polling GET /v1/events from a TypeScript backend with a stored cursor (next_after, cursor_expired, types, phone_number_id). Load when a non-Rust backend (a Medusa store, a CMS) needs incoming WhatsApp messages, delivery statuses or template reviews from meta-whatsapp-server, or when configuring Meta's callback URL for it."
---

# meta-whatsapp-rs-server-events

> **Verified against meta-whatsapp-rs 5e35867fed8b56cea5796c62f3aa808bab7407ed (2026-09-26).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/events.ts](examples/events.ts) (type-checked against the service's OpenAPI document). Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#receiving-metas-webhooks).

## When to use

Your backend is not Rust and needs what Meta sends about its WhatsApp
numbers (customers' messages, delivery statuses, template reviews,
account updates) through meta-whatsapp-server. Deploying the service and
its keys: `meta-whatsapp-rs-server` first.

## Point Meta at the service (once per Meta app)

- App Dashboard, WhatsApp, Configuration: callback URL
  `https://<public host>/webhooks/meta`, verify token `WA_VERIFY_TOKEN`'s
  value; subscribe the fields you need (messages, at least). The public
  listener answers Meta's check (`GET /webhooks/meta`) and deliveries
  (`POST /webhooks/meta`); nothing of the /v1 API.
- Meta sends a WABA's events only once the app is subscribed to it:
  attaching one (`POST /v1/admin/tenants/{id}/wabas`) does that.
- Deliveries are signed with the app secret: `WA_APP_SECRET`, and
  `WA_APP_SECRET_PREVIOUS` while you rotate it.
- The ingress passes 3 MiB bodies untouched, buffers them, outwaits the
  slowest delivery, and admits only Meta's IP ranges (or Meta's mTLS).

## What the service does with a delivery

- Unsigned or forged: `401`, nothing recorded; over 3 MiB: `413`; a body
  slower than 15 s: `408`.
- Busy (64 deliveries in flight, or another replica recording the same
  event): `503`, Meta retries. Already recorded: acknowledged, not
  recorded again. Errors carry no id: the same body within an hour is a
  redelivery, after it a new event (new `id`): errors can recur.
- **Routed by ownership, as an allow-list**: an event about a business
  number goes to that number's tenant; one naming only a WABA (template
  reviews, account updates) to the WABA's tenant; only if Meta dated it
  after that WABA was attached. Inbox first, then the outbox you poll.
- **Operator-only**, never shown to a tenant: events of a number or WABA
  no tenant holds (or held then), replays over 7 days and an hour old,
  untyped fields, signed bodies that are not webhooks, partner solution
  updates, types not reviewed yet (`KnownEventType` lists what you get).

## Poll the events

`GET /v1/events` with a key holding the `events` scope answers
`{data, next_after}`. Store `next_after`, pass it as `after` next time;
omit `after` only for the very first poll (the oldest event kept).

```ts
  const after = await cursor.load();
  const query: EventsQuery = after === undefined ? { limit: 100 } : { limit: 100, after };
  const { data, error, response } = await api.GET("/v1/events", { params: { query } });

  for (const event of data.data) {
    await handle(event);
  }
  await cursor.save(data.next_after);
  const last = data.data[data.data.length - 1];
  return last !== undefined && last.sequence === data.next_after;
```

- Each tenant has its own `sequence`, increasing with gaps (a tenant
  created again under a deleted one's id goes on after it). `next_after`
  is the last event's when more may follow, else the tenant's newest:
  save it even when `data` is empty. Poll again at once while `data` is
  non-empty and more may follow; on `429`, wait `Retry-After` seconds.
- Handle, then save: after a crash in between the same events come
  again, so handlers skip an `id` they already handled.
- `cursor_expired` (410): events after your cursor were purged, past
  retention (`WA_SERVER_OUTBOX_RETENTION`, 7 days by default) or with a
  deleted tenant of the same id. `invalid_request` on `after` (422): a
  cursor past your newest event (a restored database: reset too when
  the operator says so). Either way, rebuild what you derive from
  events, then poll without `after`:

```ts
    const expired = error.error.code === "cursor_expired";
    const unknown = error.error.code === "invalid_request" && error.error.field === "after";
```

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

`data` is meta-whatsapp-rs's event JSON: Meta's fields, normalized.
Deduplicate on `id` (an event recorded again keeps it, unless the
operator rotated `WA_APP_SECRET` in between), order on `sequence`; make
what you do per message idempotent on `data.message.id` too. Key a
customer by their BSUID (the contact's user id): the phone number may be
absent. New fields and types appear within v1: ignore the unknown.

```ts
  switch (event.type as KnownEventType) {
    case "message_received": {
      const data = event.data as Received;
      // Key the customer by BSUID: the phone number (wa_id) may be absent.
      const customer = data.contact?.user_id ?? data.contact?.wa_id ?? undefined;
```

In `status_updated`, `data.status.id` is the send's `message_id`,
`data.status.status` its state, and your `callback_data` comes back as
`data.status.biz_opaque_callback_data`: after a `504` (no `message_id`),
look for it there before concluding "never sent" and resending.

```ts
    case "status_updated": {
      const data = event.data as Delivery;
      // After a 504 (no message_id), find the send by its callback_data.
      const reference = data.status?.biz_opaque_callback_data ?? undefined;
```

## Filter

`types` (`KnownEventType` values, comma-separated or repeated; another
is `invalid_request`) and `phone_number_id` narrow the page. `limit` is
1 to 100, 50 by default; a page stops before 8 MiB of `data`. A filtered
poll's `next_after` moves past what the filter left out: keep one cursor
per filter set.

```ts
  const query: EventsQuery = {
    after,
    types: "message_received,status_updated",
    phone_number_id: pn,
  };
```

## What meta-whatsapp-rs does not do

- It pushes nothing to your backend yet: poll. Live streams (SSE),
  signed webhooks to your URL and one event by its id come with M2.
- It never shows you another tenant's events, nor operator-only ones
  (the operator reads those, outbox rows without a tenant).
- It does not resynchronise you after `cursor_expired`, and keeps events
  7 days by default (the owner's retention decision is still open).
- Meta's own retries, deduplication and signature are its business: you
  never talk to Meta, and never verify Meta's signature yourself.

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, errors),
`meta-whatsapp-rs-webhook-events` (what each event's fields mean, for
the Rust library the service runs on).
