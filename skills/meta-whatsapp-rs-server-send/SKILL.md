---
name: meta-whatsapp-rs-server-send
description: "Sending WhatsApp messages, media and templates through meta-whatsapp-server over HTTP from an app not written in Rust (a Medusa store, a CMS backend) - the message body (to, type, the object Meta documents), E.164 recipients and BSUIDs, templates outside the 24-hour window, read receipts, the Idempotency-Key header and what may_have_been_sent means for a job queue, media uploads and verified downloads, listing and creating templates, and the service's 429 rate limits. Load when a non-Rust backend sends WhatsApp messages or media, or manages templates, through the service."
---

# meta-whatsapp-rs-server-send

> **Verified against meta-whatsapp-rs 8c6d6f2e936063da2b0cd224085cce8fad992fae (2026-09-25).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/send.ts](examples/send.ts) (type-checked against the service's OpenAPI document). Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md).

## When to use

Your backend is not Rust, reaches WhatsApp through meta-whatsapp-server
(`meta-whatsapp-rs-server`: deploying it, keys, errors) and sends order
notifications, replies, files or templates (scopes `send`, `media`, `templates`).

| Route | Does |
| --- | --- |
| `POST /v1/numbers/{pn}/messages` | send; `202` with `message_id`, the key of its status events |
| `POST /v1/numbers/{pn}/messages/{message_id}/read` | blue ticks; `typing_indicator` only when a reply follows |
| `POST /v1/numbers/{pn}/media` | upload (a multipart form: `type`, then `file`); `201` with `media_id` |
| `GET /v1/numbers/{pn}/media/{media_id}`, `DELETE /v1/numbers/{pn}/media/{media_id}` | download, verified; delete |
| `GET /v1/wabas/{waba_id}/templates`, `GET /v1/wabas/{waba_id}/templates/{id}`, `POST /v1/wabas/{waba_id}/templates`, `DELETE /v1/wabas/{waba_id}/templates` | list (cached 60 s per WABA), one, create, delete by `name` (and `id`) |

## Send a message

The body is `to`, `type` and the object `type` names, written as Meta's
page for that type writes it (`SendMessage`); `reply_to` quotes a
received message, `callback_data` comes back in its status events:

```ts
export async function send(api: WhatsApp, pn: string, message: SendMessage, reference: string): Promise<Outcome> {
  const { data, error, response } = await api.POST("/v1/numbers/{pn}/messages", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { ...message, callback_data: reference },
  });
  if (error) {
    return outcome(error.error, response.headers.get("Retry-After"));
  }
  return { kind: "sent", messageId: data.message_id };
}
```

- **Recipients** (`MessageRecipient`): `phone` in E.164 **with** its `+`
  (`+16505551234`); without it, `invalid_request` on `to.phone` before
  anything is sent (Meta would read it as local to your number's
  country). `user_id`: the BSUID a webhook gave you (it may carry no
  phone number); both: Meta uses the phone; `group_id` alone.
- **Types**: `text`; `image`, `video`, `audio`, `document`, `sticker` by
  uploaded `id` or an https `link` Meta fetches; `location`, `contacts`,
  `reaction`; `template`; `interactive` of type `button`, `list` or
  `cta_url`. Anything else is `unsupported_message_type`. Limits Meta
  documents (4,096 characters of text, 3 buttons, …) are checked first:
  `invalid_request` with `field`.
- **The 24-hour window** is Meta's: a free-form message to a customer who
  has not written for 24 hours is `customer_service_window_closed` (409).
  Send an approved template instead:

```ts
export function orderConfirmation(phone: string, customer: string): SendMessage {
  return {
    to: { phone },
    type: "template",
    template: {
      name: "order_confirmation",
      language: { code: "en_US" },
      components: [{ type: "body", parameters: [{ type: "text", text: customer }] }],
    },
  };
}
```

## Idempotency-Key and may_have_been_sent

Send every job with an `Idempotency-Key` derived from your own record
(order:1234:shipped, say), scoped to your tenant, 1 to 255 visible ASCII
characters. The same key and body never reach Meta twice: a repeat gets
the first answer back, byte for byte, with `Idempotent-Replayed`.

```ts
export function outcome(error: ErrorObject, retryAfter: string | null): Outcome {
  if (error.may_have_been_sent) {
    return { kind: "reconcile" };
  }
```

- `may_have_been_sent` true (a `timeout`, a Meta failure): never send it
  again under a **new** key. Repeat with the same key (you get the kept
  answer) or wait for the status event.
- false (`invalid_request`, a Meta refusal such as
  `customer_service_window_closed`, throttling): nothing went out and the
  key is released: fix the cause, then repeat with the same key.
- `idempotency_key_reused` (422): the key already names another request.
  `idempotency_in_progress` (409): the first is still running.
  `outcome_unknown` (409): the first died mid-way; reconcile, it never
  resends. Kept `WA_SERVER_IDEMPOTENCY_TTL` (24 hours).

## Media

Upload a form, `type` (one of Meta's MIME types) before `file`; both are
checked first (`invalid_request` on `type`, `media_too_large`: 5 MiB images,
16 MiB audio and video, 100 MiB documents, `WA_SERVER_MEDIA_MAX_BYTES`;
`too_many_requests` while half the replica's media slots are yours):

```bash
curl -sS -X POST "$WA_SERVER/v1/numbers/106540352242922/media" -H "Authorization: Bearer $KEY" \
  -H "Idempotency-Key: voucher-1234" -F type=image/png -F file=@voucher.png
```

Downloads are checked against Meta's SHA-256 before the first byte:
`integrity` (502) instead of a corrupt file, `X-WA-SHA256` on success.
At most `max_bytes` (16 MiB by default and at most); larger files need
`stream` set to true, where a mismatch **aborts the connection** before
the last chunk: keep the bytes aside until the body ended cleanly. A
`media_id` is digits, and your number's: another's is `not_found`.

```ts
const { data, error, response } = await api.GET("/v1/numbers/{pn}/media/{media_id}", {
  params: { path: { pn, media_id: mediaId } },
  parseAs: "arrayBuffer",
});
```

## Templates

Creation takes Meta's own template JSON (`TemplateDefinition`), checked
locally first (`invalid_request` with `field`, also on a key the service
would not pass on: nothing is dropped); Meta refusing it is
`template_rejected`, a full WABA `template_limit_reached`; the review's
result comes as an event. Lists (`TemplateList`: `status`, `name`,
`limit`, `cursor`) are cached 60 seconds per WABA; another WABA's
template `id` is `not_found`.

## Rate limits

Per tenant and replica: writes 20 a second (bursts of 40), reads 50,
template management 2 (`WA_SERVER_RATE_SEND`, `WA_SERVER_RATE_READ`,
`WA_SERVER_RATE_TEMPLATES`, and `WA_SERVER_RATE_SEND_BURST` and the like).
Past them: `too_many_requests` (429) with `Retry-After`, before anything is
sent; a platform key shares the tenant's budget. Meta's own limits answer
`rate_limited`, `pair_rate_limited`, …

## What meta-whatsapp-rs does not do

- It never retries a send for you, and it cannot tell you whether a
  timed-out message went out: the status event (M1c) or your
  `Idempotency-Key` repeat does.
- No campaign pacing or opt-out registry: respect `marketing_opted_out`
  and pace your own sends.
- Meta documents its per-number media check for uploads only: a file
  received by webhook answering `not_found` may be that; report it.
- No Direct Send, Flows, product or carousel messages, and no
  authentication templates yet; no documents route (M4).

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, the error body);
from Rust, and what each field means: `meta-whatsapp-rs-send-messages`,
`meta-whatsapp-rs-send-templates`, `meta-whatsapp-rs-templates`.
