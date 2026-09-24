---
name: wa-rs-cms-inbox
description: "The merchant-customer chat inbox of a CMS built on wa-rs (wa_rs::inbox) - InboxSink recording webhook messages and statuses into a ConversationStore, Inbox listing conversations and history and replying with the merchant's token, conversation keys (BSUID, wa_id, group), the 24-hour customer service window and templates outside it, memory vs Postgres storage (and the U+0000 limitation), live updates over SSE, and what the inbox does not record yet. Load when building inbox screens, reply endpoints, or the webhook-to-inbox pipeline."
---

# wa-rs-cms-inbox

> **Verified against wa-rs 91431ae (2026-09-24).** On another revision, trust
> the code over this page (see `skills/README.md`).

```text
Meta ─webhook─► WebhookHandler ─► FanoutSink ─┬─► InboxSink ──► ConversationStore
                                              └─► BroadcastSink ─► sse() ─► merchant UI
merchant UI ─► Inbox::reply ─► client.with_token(merchant token) ─► Meta
                     └───────► ConversationStore (outbound row, status Accepted)
```

Module: `wa_rs::inbox`. Storage port: `wa_rs::core::store::ConversationStore`.
Prerequisites: merchants onboarded (`wa-rs-embedded-signup`), a webhook
endpoint (`wa-rs-webhooks`).

## Wiring

```rust
use std::sync::Arc;
use wa_rs::adapters::sink::{BroadcastSink, FanoutSink};
use wa_rs::adapters::store::{PostgresConversationStore, postgres};
use wa_rs::core::store::ConversationStore;
use wa_rs::inbox::InboxSink;
use wa_rs::webhooks::{DedupGuard, WebhookEvent, WebhookHandler};

postgres::migrate(&pool).await?; // idempotent; run at startup
let store: Arc<dyn ConversationStore> = Arc::new(PostgresConversationStore::new(pool.clone()));
let (live_tx, _) = tokio::sync::broadcast::channel::<WebhookEvent>(1024);

let sink = FanoutSink::new()
    .with(InboxSink::new(store.clone()))             // persists: its failure → 500 → Meta retries
    .with(BroadcastSink::from_sender(live_tx.clone())); // live view: best effort, never fails
let handler = WebhookHandler::builder(verifier, verify_token, Arc::new(sink))
    .dedup(DedupGuard::new(kv.clone()))
    .build();
```

`InboxSink` is idempotent on its own (the store ignores a known message id
and any status that does not move a message forward); the dedup guard saves
the work. When one fanned-out sink fails, Meta redelivers and the others see
the event again: make the UI dedupe by message id.

## Conversation keys

`ConversationKey { phone_number_id, contact }` — one business number plus one
customer. `InboxSink` picks `contact` as:

1. the **group id** for group messages (a group is its own conversation);
2. else the **BSUID** (`from_user_id`, or the contact's `user_id`) — present in
   every messages webhook since April 2026;
3. else the **`wa_id`** (digits, no `+`).

A message with none of these is acknowledged and not recorded. A `revoke`
marks the original message `DeliveryStatus::Deleted` instead of adding a row.
The same rules are public as `wa_rs::inbox::conversation_key` and the text
preview as `wa_rs::inbox::preview`.

**BSUIDs change when a customer changes phone number**
(`WebhookEvent::UserIdChanged`). The inbox does not merge conversations: the
new BSUID starts a new one. Handle `UserIdChanged` yourself if that matters.

## Reading and replying

Build an `Inbox` per request, for a phone number **your own auth says this
merchant owns** (the inbox only checks that a key belongs to its number, not
to your tenant):

```rust
use std::sync::Arc;
use wa_rs::client::embedded_signup::TokenVault;
use wa_rs::client::messages::{MessageContent, Text};
use wa_rs::client::templates::TemplateMessage;
use wa_rs::core::clock::{Clock, SystemClock};
use wa_rs::core::ids::PhoneNumberId;
use wa_rs::core::store::ConversationStore;
use wa_rs::inbox::Inbox;
use wa_rs::{Client, Error};

async fn reply_text(
    client: &Client, vault: &TokenVault, store: Arc<dyn ConversationStore>,
    pnid: PhoneNumberId, contact: &str, body: &str,
) -> wa_rs::Result<()> {
    let token = vault.get_by_phone_number(&pnid).await?.ok_or_else(not_connected)?.token;
    let inbox = Inbox::new(client.with_token(token), pnid, store);
    let key = inbox.key(contact); // the `contact` of a ConversationSummary / StoredMessage

    let content: MessageContent = if inbox.window(&key).await?.is_open(SystemClock.now()) {
        Text::new(body).into()
    } else {
        TemplateMessage::new("reopen_conversation", "en_US").into() // an approved template
    };
    match inbox.reply(&key, content).await {
        Ok(_sent) => Ok(()),
        Err(Error::Validation(v)) if v.field == "customer_service_window" => Err(window_closed()),
        Err(e) => Err(e),
    }
}
```

- `inbox.conversations(before, limit)` — newest activity first; next page:
  pass the last row's `(last_message_at, key.contact)`.
- `inbox.history(&key, before, limit)` — newest first; next page: the last
  row's `(timestamp, id)`. Cursors are exclusive: no repeats, no gaps.
- `inbox.mark_read(&key)` resets the unread counter **in your store** (the
  merchant opened the chat). It does not send read receipts; for blue ticks
  call `client.messages(pnid).mark_read(&message_id)` separately.
- `ConversationSummary.unread` counts inbound messages by arrival since the
  last `mark_read`.

### The 24-hour window

Free-form messages are allowed only within 24 hours of the customer's last
inbound message (Meta: 131047, `ErrorKind::CustomerServiceWindowClosed`).
`inbox.window(&key)` → `CustomerServiceWindow` (`is_open(now)`,
`closes_at()`); a conversation with no inbound message is closed.
`reply`/`send` check it **before** any request and refuse with
`Error::Validation` on field `customer_service_window`. Exempt: templates, and
messages with a Direct Send `category` (a Meta beta; Meta stays the judge).

### What `reply` records, and why it never fails after sending

After a successful send, the outbound row is appended with
`DeliveryStatus::Accepted`; status webhooks move it to `Sent`, `Delivered`,
`Read` (never backwards). If that append fails, the error is **logged, not
returned**: the message is already out, and an error would invite a retry
that sends it twice. So never retry a `reply` that returned `Ok`, and treat a
transport timeout from `reply` as "may have been sent" (see `wa-rs`).

### Addressing

`reply` addresses by the key: a contact containing `.` is a BSUID
(`recipient`), all digits (optionally `+`-prefixed) a phone number, always
sent as `to: "+<digits>"` — Meta prepends *your business number's* country
code to a number without `+`, which would deliver the reply to someone else.
Anything else is a group.

`send` is how you add a quoted reply (`reply_to`), callback data or a Direct
Send category. Build the recipient with `inbox.recipient(&key)`; a message
addressed to anyone else is refused (`Error::Validation` on `recipient`), so
a conversation can't be used to message someone outside it:

```rust
use wa_rs::client::messages::OutboundMessage;

let msg = OutboundMessage::new(inbox.recipient(&key), Text::new(body)).reply_to(quoted_wamid);
inbox.send(&key, msg).await?;
```

## Storage: memory or Postgres

| | `MemoryConversationStore` | `PostgresConversationStore` |
| --- | --- | --- |
| Use | tests, demos, one process | production |
| Survives restart / shared by instances | no | yes |
| Setup | `MemoryConversationStore::new()` | `postgres::migrate(&pool)` at startup; tables `wa_*` (`TablePrefix` + `migrate_with_prefix` for another prefix) |
| U+0000 in text/ids/payload | accepted | **rejected** |

There is no Redis `ConversationStore` (history is not cache-shaped).

**The NUL limitation.** Postgres `text`/`jsonb` cannot hold U+0000. A message
whose id, contact, kind, text, payload or error contains one is rejected with
`StorageError::Backend` **on every retry**, so the webhook answers `500` and
Meta retries the delivery for 7 days, then drops it. The handler stops at the
first failing event, so the events *after* it in the same delivery are stuck
with it. The library does not strip NULs. If your traffic
can carry them, wrap `InboxSink` in your own `EventSink` that removes U+0000
from the event's strings before delegating, or pick another store.

## Live updates

Subscribe per merchant to the same broadcast channel and stream with
`wa_rs::webhooks::sse(live_tx.subscribe(), move |e| e.phone_number_id() == Some(&pnid))`
(feature `axum`), behind your auth. The filter must be an **allow-list**
(`Unknown`/`Unparsed` events have no phone number id and may belong to any
tenant). On `event: lagged`, reload from `inbox.history`. The UI receives
`WebhookEvent` JSON (`"event": "message_received"`, …); your own outbound
replies are not broadcast — push them to the UI from the reply endpoint.

## Not recorded (as of 91431ae)

- Coexistence **echoes** (`WebhookEvent::MessageEchoed`: messages the merchant
  sent from the WhatsApp Business app) and **history sync**
  (`HistorySynced`). Handle them yourself for now.
- Media bytes: rows keep the webhook message JSON (with the media id).
  Download with `client.media(pnid).download(&id)` while the id is valid
  (webhook media ids live 7 days).
- BSUID changes (`UserIdChanged`), see above.
- Anything but messages and statuses (`InboxSink` ignores other events).
