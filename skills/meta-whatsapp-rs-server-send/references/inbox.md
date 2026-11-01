# Replying from the inbox

> **Verified against meta-whatsapp-rs ee4e9d2362e93527793a7638749c0a314ba27ccc (2026-11-01).** Source: the service's OpenAPI document (`crates/meta-whatsapp-server/openapi/v1.json`, tag `inbox`), `docs/guides/server.md` section "The inbox" and [examples/send.ts](../examples/send.ts).

The inbox (scope `inbox`) holds what the service received for your
number: a customer's message is there once Meta's delivery is answered.

| Route | Does |
| --- | --- |
| `GET /v1/numbers/{pn}/conversations` | conversations, most recent activity first: `contact`, `last_message_at`, `last_inbound_at`, `last_text`, `unread` |
| `GET /v1/numbers/{pn}/conversations/{contact}/messages` | one conversation's history, newest first: `id`, `direction`, `kind`, `text`, `payload`, `status`, `timestamp`, `status_at`, `error` |
| `POST /v1/numbers/{pn}/conversations/{contact}/messages` | a reply to the conversation's contact; `202` with `message_id`, recorded in its history until its status events move it on |

A conversation's `contact` is the customer's BSUID, else their `wa_id`;
percent-encode it in a path (openapi-fetch does). Lists page by `limit`
and `cursor` (`next_cursor`), as every list:

```ts
export async function conversations(api: WhatsApp, pn: string, cursor?: string) {
  const query: ConversationsQuery = cursor === undefined ? { limit: 50 } : { limit: 50, cursor };
  const { data, error } = await api.GET("/v1/numbers/{pn}/conversations", {
    params: { path: { pn }, query },
  });
```

A reply (`ReplyMessage`) is a send without `to`, under an
`Idempotency-Key` like a send:

```ts
export async function reply(api: WhatsApp, pn: string, contact: string, message: ReplyMessage, reference: string): Promise<Outcome> {
  const { data, error, response } = await api.POST("/v1/numbers/{pn}/conversations/{contact}/messages", {
    params: { path: { pn, contact }, header: { "Idempotency-Key": reference } },
    body: { ...message, callback_data: reference },
  });
```

- The service checks first and refuses with nothing sent (409,
  `may_have_been_sent` false): `customer_service_window_closed` for
  free-form content outside the 24-hour window (it counts the customer's
  calls too: send a template), `thread_owned_elsewhere` while another app
  owns the thread under Conversation Routing (wait for it back, or send
  a template). A template is exempt from both. The operator may turn a
  check off per number (`WA_SERVER_INBOX_REPLY_CHECKS`); Meta then
  decides.
- A number that moved to you from another tenant shows nothing from
  before your binding, and `unread` counts only what came since; a
  number no other tenant held shows all of it. On such a number a
  free-form reply needs a window the customer opened since your binding:
  one the previous holder's customer opened is
  `customer_service_window_closed`, nothing sent (send a template).
- The inbox keeps everything unless the operator sets
  `WA_SERVER_INBOX_RETENTION`.
- No window or read route, and no message by id, yet.
