---
name: wa-rs-cms-inbox
description: "The merchant-customer chat inbox of a CMS built on wa-rs (wa_rs::inbox) - InboxSink recording webhook messages and statuses into a ConversationStore, Inbox listing conversations and history and replying with the merchant's token, conversation keys (BSUID, wa_id, group), the 24-hour customer service window and templates outside it, memory vs Postgres storage (and how U+0000 is handled), live updates over SSE, and what the inbox does not record yet. Load when building inbox screens, reply endpoints, or the webhook-to-inbox pipeline."
---

# wa-rs-cms-inbox

> **Verified against wa-rs 7940d15 (2026-09-24).** On another revision, trust
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

Statuses and revokes only ever change a message **of the business number
they arrived on**: `InboxSink` calls
`ConversationStore::update_status(phone_number_id, id, status, at, error)`
with the event's `phone_number_id`, and both stores match the id *and* the
number. A `ConversationStore` of your own must do the same: run
`wa_rs::adapters::store::conversation_conformance::run(&store).await` in
its tests (it panics on a broken rule).
~~`update_status(id, status, at, error)`, matched on the message id
alone~~: until 4b47bf7 (2026-09-24, breaking), so a status or revoke
delivered for one number could change another number's message.

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

A message id is stored once per store, whatever the number: if the same id
ever arrives on two of your business numbers (e.g. a group both are in), it
is kept under the first conversation that recorded it, and the second
`append` is a no-op (`OPEN_QUESTIONS.md` #33).

**BSUIDs change when a customer changes phone number**
(`WebhookEvent::UserIdChanged`). The inbox does not merge conversations: the
new BSUID starts a new one. Handle `UserIdChanged` yourself if that matters.

## Reading and replying

Build an `Inbox` per request, for a phone number **your own auth says this
merchant owns** — and check that ownership *before*
`vault.get_by_phone_number`, whose tokens belong to every merchant (the
inbox only checks that a key belongs to its number, not to your tenant). The
`cms_inbox` example's `inbox()` shows the order: authenticated tenant → owns
the number? (`403` if not) → vault → `Inbox`.

```rust
use std::sync::Arc;
use wa_rs::client::embedded_signup::TokenVault;
use wa_rs::client::messages::{MessageContent, Text};
use wa_rs::client::templates::TemplateMessage;
use wa_rs::core::clock::{Clock, SystemClock};
use wa_rs::core::ids::PhoneNumberId;
use wa_rs::core::store::ConversationStore;
use wa_rs::inbox::Inbox;
use wa_rs::{Client, ErrorKind};

async fn reply_text(
    client: &Client, vault: &TokenVault, store: Arc<dyn ConversationStore>,
    pnid: PhoneNumberId, contact: &str, body: &str, // pnid: ownership already checked
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
        // The local refusal (nothing sent) and Meta's 131047 share one kind.
        Err(e) if e.kind() == ErrorKind::CustomerServiceWindowClosed => Err(window_closed()),
        Err(e) => Err(e),
    }
}
```

~~Match `Error::Validation(v) if v.field == "customer_service_window"`~~:
the field is still set, but branch on the kind (fe49aa5, 2026-09-24): a
field-name match misses Meta's own 131047. To tell the local refusal apart,
`ValidationError::is_customer_service_window_closed()` (7f436a5) rather than
the string.

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
`Error::Validation(ValidationError::customer_service_window_closed())` (field
`customer_service_window`), whose `kind()` is
`ErrorKind::CustomerServiceWindowClosed`, like Meta's 131047: branch on the
kind. Exempt: templates, and messages with a Direct Send `category` (a Meta
beta; Meta stays the judge).

The inbox only knows the window from **messages**. Meta also opens it when
the customer calls your number, but calls are not recorded, so after a call
`reply` still refuses free text: keep the template fallback reachable there
too (`OPEN_QUESTIONS.md` #32).

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

~~**Caveat (as of 91431ae):** a conversation keyed by a bare `wa_id` is sent
to `to` *without* `+`, and Meta then prepends your business number's country
code; use `send` with `Recipient::phone(format!("+{}", key.contact))`. Its
recipient must be the conversation's contact — this is not checked.~~
True until 2b2679a (2026-09-24): replies now go to `+<digits>`, `send`
refuses any recipient but `inbox.recipient(&key)`, and `Inbox::recipient`
exists only from 2b2679a on. On an older pin, use the struck-through
workaround.

## Storage: memory or Postgres

| | `MemoryConversationStore` | `PostgresConversationStore` |
| --- | --- | --- |
| Use | tests, demos, one process | production |
| Survives restart / shared by instances | no | yes |
| Setup | `MemoryConversationStore::new()` | `postgres::migrate(&pool)` at startup; tables `wa_*` (`TablePrefix` + `migrate_with_prefix` for another prefix) |
| U+0000 given to the store | accepted | **refused** (`StorageError::Backend`) |
| U+0000 in what `InboxSink` / `Inbox::send` record | stored as U+FFFD | stored as U+FFFD |

There is no Redis `ConversationStore` (history is not cache-shaped).

**U+0000 (NUL).** Postgres `text`/`jsonb` cannot hold it, and a store
refusal on every retry would make the webhook answer `500` until Meta drops
the whole batch after 7 days. So `InboxSink` replaces U+0000 with U+FFFD
(the replacement character) in the stored `kind`, `text`, `payload` (every
string, object keys included) and status `error`, and `Inbox::send` does
the same for the outbound row: customer content never fails a delivery.
The replacement is lossy (a NUL and a U+FFFD read the same afterwards) and
**provisional**: it was a maintainer decision, taken without asking to stop
one NUL from losing a whole webhook batch, and may be reversed
(`OPEN_QUESTIONS.md` #18). Meta-assigned fields — the message id, the
contact (BSUID, `wa_id`, group id), the business phone number id — are
stored as Meta sent them, so a NUL there is still refused by Postgres (and
fails the batch); so is anything you hand the Postgres store directly.
~~The library does not strip NULs; one in a customer's message fails its
delivery on every retry — wrap `InboxSink` to remove them~~: true until
fd4667e (2026-09-24). On an older pin, keep the wrapper.

## Live updates

Subscribe per merchant to the same broadcast channel and stream with
`wa_rs::webhooks::sse(live_tx.subscribe(), move |e| e.phone_number_id() == Some(&pnid))`
(feature `axum`; `BroadcastSink::receiver()` gives the same receiver),
behind your auth and after the ownership check. The filter must be an
**allow-list** (`Unknown`/`Unparsed` events have no phone number id and may
belong to any tenant). Each open stream clones every event before filtering
(`OPEN_QUESTIONS.md` #31). On `event: lagged`, reload from `inbox.history`.
The UI receives `WebhookEvent` JSON (`"event": "message_received"`, …); your
own outbound replies are not broadcast — push them to the UI from the reply
endpoint.

## Not recorded (as of 7940d15)

- Coexistence **echoes** (`WebhookEvent::MessageEchoed`: messages the merchant
  sent from the WhatsApp Business app) and **history sync**
  (`HistorySynced`). Handle them yourself for now.
- Media bytes: rows keep the webhook message JSON (with the media id).
  Download with `client.media(pnid).download(&id)` while the id is valid
  (webhook media ids live 7 days).
- BSUID changes (`UserIdChanged`), see above.
- Calls (`CallUpdated`, `CallStatusUpdated`), which also means the window a
  call opens is invisible to `Inbox::window` (above).
- Anything but messages and statuses (`InboxSink` ignores other events).
