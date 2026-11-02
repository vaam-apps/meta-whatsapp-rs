---
name: meta-whatsapp-rs-server-events
description: "Receiving WhatsApp events through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - pointing the Meta app's webhook at the service, how it verifies, deduplicates and routes Meta's deliveries to tenants (and keeps the rest operator-only), polling GET /v1/events from a TypeScript backend with a stored cursor (next_after, cursor_expired, types, phone_number_id), following them live over Server-Sent Events (GET /v1/events/stream, Last-Event-ID, lagged, shutdown, too_many_streams) and reading one event by id. Load when a non-Rust backend (a Medusa store, a CMS) needs incoming WhatsApp messages, delivery statuses or template reviews from meta-whatsapp-server, or when configuring Meta's callback URL for it."
---

# meta-whatsapp-rs-server-events

> **Verified against meta-whatsapp-rs 28fab70672de0e011282672d43bfc87f89cbba1e (2026-11-02).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/events.ts](examples/events.ts) (polling) and [examples/stream.ts](examples/stream.ts)
(live), type-checked against the service's OpenAPI document. Operators' guide:
[docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#receiving-metas-webhooks).

## When to use

Your backend is not Rust and needs what Meta sends about its WhatsApp numbers (customers' messages, delivery
statuses, template reviews, account updates) through meta-whatsapp-server. Deploying the service and its keys:
`meta-whatsapp-rs-server` first.

## Point Meta at the service (once per Meta app)

- App Dashboard, WhatsApp, Configuration: callback URL `https://<public host>/webhooks/meta`, verify token
  `WA_VERIFY_TOKEN`'s value; subscribe the fields you need (messages, at least). Attaching a WABA
  (`POST /v1/admin/tenants/{id}/wabas`) subscribes the app to it.
- Deliveries are signed with `WA_APP_SECRET` (and `WA_APP_SECRET_PREVIOUS` while you rotate it). The ingress passes
  3 MiB bodies untouched and admits only Meta's IP ranges (or Meta's mTLS).

## What the service does with a delivery

- Unsigned or forged: `401`; over 3 MiB: `413`; slower than 15 s: `408`; busy: `503`, Meta retries. Already
  recorded: acknowledged once. Errors carry no id: the same body within an hour is a redelivery, after it a new
  event (new `id`).
- **Routed by ownership, as an allow-list**: an event about a business number goes to that number's tenant; one
  naming only a WABA to the WABA's tenant; if dated, only after the second it was attached in.
- **Operator-only**, never shown to a tenant: events of a number or WABA no tenant holds (or held then), replays
  over 7 days and an hour old, untyped fields, bodies that are not webhooks, types not reviewed yet
  (`KnownEventType` lists what you get).

## Poll the events

`GET /v1/events` (scope `events`) answers `{data, next_after}`. Store `next_after`, pass it as `after` next time;
omit `after` only for the very first poll (the oldest event kept).

```ts
  const after = await cursor.load();
  const query: EventsQuery = after === undefined ? { limit: 100 } : { limit: 100, after };
  const { data, error, response } = await api.GET("/v1/events", { params: { query } });

  for (const event of data.data) {
    await handle(event);
  }
  await cursor.save(data.next_after);
```

- Each tenant has its own `sequence`, increasing with gaps. `next_after` is the last event's when more may follow,
  else the tenant's newest: save it even when `data` is empty. On `429`, wait `Retry-After`.
- Handle, then save: after a crash in between the same events come again, so handlers skip an `id` they already
  handled.
- `cursor_expired` (410): events after your cursor were purged (past `WA_SERVER_OUTBOX_RETENTION`, 7 days by
  default). `invalid_request` on `after` (422): a cursor past your newest event (a restored database). Either way,
  rebuild what you derive from events, then poll without `after`. `types` and `phone_number_id` filter (keep one
  cursor per filter set); a page stops at 100 events or 8 MiB of `data`.

```ts
    const expired = error.error.code === "cursor_expired";
    const unknown = error.error.code === "invalid_request" && error.error.field === "after";
```

## Follow them live (Server-Sent Events)

`GET /v1/events/stream` (or `GET /v1/numbers/{pn}/events/stream`, one number's) sends each new event as it is
recorded, on any replica: `event: whatsapp` frames whose `id` is the `sequence` and whose `data` is the envelope
below, and `: keepalive` every 15 s. Catch up by polling first, then follow from your cursor: `Last-Event-ID` (or
`after`) resumes exactly after it; with neither, only the next events come.

```ts
    const header = after === undefined ? {} : { "Last-Event-ID": String(after) };
    const query: StreamQuery = { types: "message_received,status_updated" };
    const { data, error, response } = await api.GET("/v1/events/stream", {
      params: { header, query },
      parseAs: "stream",
      signal,
    });
```

```ts
        if (frame.event === "whatsapp") {
          const event = JSON.parse(frame.data) as Event;
          await handle(event);
          await cursor.save(event.sequence);
        } else if (frame.event === "lagged" || frame.event === "shutdown") {
```

- The stream may end with `event: lagged` (you took nothing for 30 s while 1 MiB waited, or events after your
  cursor were purged) or `event: shutdown` (the replica stops), each with `{"after": n}`, the last sequence sent:
  reconnect with it as `Last-Event-ID`.
- Refused before it starts: `cursor_expired` (410) and `invalid_request` on `Last-Event-ID` or `after` (422), as
  for polling; `too_many_streams` (429: 20 per tenant and replica, `WA_SERVER_STREAMS_PER_TENANT`) and
  `shutting_down` (503): wait `Retry-After`. A number not yours: `not_found`.
- Browsers cannot send the key (an EventSource sends no Authorization header): relay the stream from your backend.

## The envelope, and one event by id

```json
{"id": "evt_3f9c0a…", "sequence": 18342, "type": "message_received", "api_version": "v1",
 "tenant_id": "merchant-42", "phone_number_id": "106540352242922", "waba_id": "102290129340398",
 "received_at": "2026-09-25T10:00:01Z", "truncated": false,
 "data": {"event": "message_received", "message": {"id": "wamid.…", "type": "text", "text": {"body": "…"}},
          "contact": {"user_id": "US.13491208655302741918", "wa_id": "16505551234", "profile": {"name": "…"}}}}
```

`data` is meta-whatsapp-rs's event JSON: Meta's fields, normalized. Deduplicate on `id`, order on `sequence`; make
what you do per message idempotent on `data.message.id` too. Key a customer by their BSUID (the contact's user id):
the phone number may be absent. New fields and types appear within v1: ignore the unknown.

```ts
  switch (event.type as KnownEventType) {
    case "message_received": {
      const data = event.data as Received;
      // Key the customer by BSUID: the phone number (wa_id) may be absent.
      const customer = data.contact?.user_id ?? data.contact?.wa_id ?? undefined;
```

In `status_updated`, `data.status.id` is the send's `message_id`, and your `callback_data` comes back as
`data.status.biz_opaque_callback_data`: after a `504`, look for it there before resending.

Your numbers' events include Conversation Routing's `standby_observed` (a copy of a thread another app owns: show
it, never answer it; an inbox reply is `thread_owned_elsewhere`) and `thread_control_changed` (the thread passed to
you, or taken), `user_action_reported` (a click on a marketing message: Meta names neither the customer nor the
message), and the service's own: `number_connected`, `number_disconnected` (why in `data.reason`) and
`number_reconnect_required` (calls on the number answer `reconnect_required` until it is attached again), whose
`data` names the number and its WABA only. They say when to look, not the state: one can repeat (a retried attach),
stay after a change that failed (a `503`), or be missing (`number_reconnect_required` is best effort), so read
`GET /v1/numbers` when one comes.

```ts
    case "standby_observed":
    case "thread_control_changed":
      return;
    case "number_disconnected": {
      // tenant_disconnected, operator_unbound, account_deleted, … (more may come).
      const data = event.data as Disconnected;
```

`GET /v1/events/{id}` answers one of your events, whole; another tenant's id, one past retention or no event's id
is `not_found`, alike (the example's eventById).

## What meta-whatsapp-rs does not do

- It pushes nothing to your URL yet: poll, or hold a stream. Signed webhooks to your backend come with M2c.
- It never shows you operator-only events, nor another tenant's: a WABA moved to you gets no undated event (error,
  history) for 7 days and an hour.
- It does not resynchronise you after `cursor_expired`, and keeps events 7 days by default (design D10, decided on
  2026-09-26: set per store).

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, errors), `meta-whatsapp-rs-webhook-events` (what each
event's fields mean, for the Rust library the service runs on).
