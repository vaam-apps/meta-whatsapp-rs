---
name: meta-whatsapp-rs-server-broadcast
description: "Broadcasts through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - queueing one template for many people from a number (named by BSUID and/or phone, with parameters of their own), the scope broadcasts, the deployment's rate shared by every replica, reading a broadcast's state and counts (failures by Meta's error code), cancelling it, an Idempotency-Key for the whole broadcast, why a person is never sent twice (and what unknown means), the limits, and what the logs never hold. Load when an e-commerce store or a CMS sends a marketing campaign or a notice to many customers through the service."
---

# meta-whatsapp-rs-server-broadcast

> **Verified against meta-whatsapp-rs 62671eb1251b3e70909a630e8cf8916083759fde (2026-11-07).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/broadcast.ts](examples/broadcast.ts), type-checked against the service's OpenAPI document. Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#broadcasts). The decisions are the design's D420 to D427, the coordinator's decisions of 2026-11-07, each swappable; the names below are pending the owner's confirmation and can change before the first release.

## When to use

You send one template to many customers (a sale, a notice, a restock
alert) and want it paced, durable and countable instead of looping over
`POST …/messages` yourself. One person is `meta-whatsapp-rs-server-send`.
Marketing sends through Meta's Marketing Messages API:
`meta-whatsapp-rs-server-marketing`. Deploying the service and its keys:
`meta-whatsapp-rs-server` first. The key needs the scope `broadcasts`,
which `send` does not grant: a key minted before it does not carry it.

## The routes

All under `/v1/numbers/{pn}`:

| Route | Does |
| --- | --- |
| `POST …/broadcasts` | queue a broadcast; `202` with the broadcast; nothing is sent by the call |
| `GET …/broadcasts/{broadcast_id}` | its state and counts |
| `POST …/broadcasts/{broadcast_id}/cancel` | stop it before its next send |

The body is `{template, recipients}`: `template` is Meta's template object
(`name`, `language`, and `components` for the parameters every person
shares), `recipients` the people, each by `user_id` (a business-scoped
user id), `phone` (E.164 with its `+`) or both, with `components` of their
own where their parameters differ (they replace the template's).

```ts
export function person(userId: string, phone?: string): BroadcastRecipient {
  return { user_id: userId, phone: phone ?? null };
}
```

Prefer the BSUID, and never key a person by phone number alone. A repeated
person is one person. Every message is checked as a template send is
before anything is queued: a refusal is `422` on `recipients`,
`recipients[i]` or the template's fields, and queues nothing. A template
naming a catalog is refused.

## Keyed once, then queued

Keep one `Idempotency-Key` per campaign: the same key and body answer the
first call again and queue nothing more; another body under the key is
`422 idempotency_key_reused`.

```ts
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { template, recipients },
```

A tenant has a few broadcasts queued or running at once (3 by default);
past it the call is `429 too_many_requests` with `Retry-After`, and a
settled or cancelled broadcast frees a place. A broadcast is for at most
1,000 people by default. Both are the deployment's settings.

## The rate, and a restart

Each number is held to the deployment's rate (10 messages a second by
default), shared by every replica of the service, so a broadcast takes a
while: read it, do not wait on the call. A replica that dies leaves the
broadcast to another, which continues where it stopped.

**Each person is sent at most once.** A send that timed out, and one a
replica died in the middle of, is `unknown`: it is never sent again, by the
service or by Meta's client. Reconcile the `unknown` ones with the message
status events: every message of a broadcast carries `wa-broadcast:` and the
broadcast's id as its `biz_opaque_callback_data`. The worst case is a
person who never got the message, never a second message.

## Reading it

```ts
export async function progress(api: WhatsApp, pn: string, id: string): Promise<Broadcast> {
```

`state` is `queued`, running, completed, `cancelled` or stopped (given
up after its retries: a number whose token cannot be used; the people left
are in `queued`). The counts are `recipients`, `queued`, `sent`, `failed`,
`unknown` and `limited`, and `failures` says which refusals, by the
service's error code of Meta's refusal (`marketing_opted_out`,
`template_paused`, `reconnect_required`, …), never by Meta's text. The
service keeps no opt-out list: Meta's refusal is the rule, and the rest of
the broadcast goes on.

```ts
  return broadcast.state === "completed" || broadcast.state === "cancelled" || broadcast.state === "stopped";
```

## Cancelling

```ts
export async function cancel(api: WhatsApp, pn: string, id: string): Promise<Broadcast> {
```

A send already past its last check goes out; nothing after it does. The
answer is the broadcast as it stands, so a repeat is harmless, and it needs
no working token. Another tenant's broadcast, another number's and one
never made are the same `404 not_found`.

```ts
  return error instanceof WhatsAppError && error.error.code === "not_found";
```

## What never reaches a log

The people, their parameters and Meta's texts are in no log line, error,
audit event or answer of the service. Each create and each cancel is an
audit event (broadcast_created, broadcast_cancelled) holding the key,
tenant, WABA, number, the broadcast's id and how many people. The service
keeps the people and parameters as you gave them, in its key/value store,
until the broadcast settles and 30 days after.

## What meta-whatsapp-rs does not do

It does not offer a time to send at, a list of your
broadcasts, a rate per tenant, or the per-user marketing limit (`131049`)
arriving as a status event. Meta's own limits (the daily messaging limit,
the pair limit, per-user marketing limits) still apply and answer their own
codes in `failures`.

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, errors),
`meta-whatsapp-rs-server-send` (one message), `meta-whatsapp-rs-server-marketing`
(the Marketing Messages API), `meta-whatsapp-rs-server-events` (the status
events of a broadcast's messages), `meta-whatsapp-rs-server-bot` (automatic
answers), `meta-whatsapp-rs-bot` (the same
pacing and durable jobs in the Rust library).
