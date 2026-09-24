# CMS inbox: merchants chat with their customers

**Goal:** each merchant sees their customers' WhatsApp conversations in your
CMS, live, and answers from it with their own number, inside the 24-hour
customer service window.

Example: [`cms_inbox.rs`](../../crates/wa-rs/examples/cms_inbox.rs) (run it
with `cargo run -p wa-rs --example cms_inbox --features axum`; add
`postgres` and `DATABASE_URL` to share the vault with the `embedded_signup`
example). Agent skill: [`wa-rs-cms-inbox`](../../skills/wa-rs-cms-inbox/SKILL.md).

```text
Meta ─ POST /webhooks/whatsapp ─► WebhookHandler ─► FanoutSink ─┬─► InboxSink ─► ConversationStore (Postgres)
       (signature, dedup)                                       └─► BroadcastSink ─► SSE ─► merchant's browser
merchant's browser ─ POST /inbox/{number}/reply ─► Inbox::reply ─► Meta, with the merchant's token
                                                                └► ConversationStore (outbound row, `Accepted`)
```

## Prerequisites

- Merchants connected with Embedded Signup, tokens in the `TokenVault`
  ([embedded-signup.md](embedded-signup.md)).
- The webhook endpoint deployed and subscribed to `messages`
  ([webhooks.md](webhooks.md)). On Meta's side nothing else is needed: the
  inbox uses `whatsapp_business_messaging`, which merchants grant in the
  Embedded Signup popup.
- Postgres, feature `postgres` (and `axum` for the router and SSE).

## 1. Storage: Postgres and migrations

```rust
use std::sync::Arc;
use wa_rs::adapters::store::postgres::{self, PostgresConversationStore, PostgresKvStore, sqlx};
use wa_rs::prelude::*;

let pool = sqlx::PgPool::connect(&database_url).await?;
postgres::migrate(&pool).await?; // idempotent; safe from several instances at boot
let kv: Arc<dyn KvStore> = Arc::new(PostgresKvStore::new(pool.clone())); // vault, dedup, signup sessions
let conversations: Arc<dyn ConversationStore> = Arc::new(PostgresConversationStore::new(pool));
```

- Tables start with `wa_` and land in the first schema of the connection's
  `search_path`; the migration history is `wa_sqlx_migrations`, apart from
  your application's own. For another prefix:
  `TablePrefix::new("cms_wa_")?`, `postgres::migrate_with_prefix` and the
  stores' `with_prefix`.
- Record expiry (vault, dedup, OTP, signup sessions) uses the **database
  server's** clock: keep it on NTP.
- Call `PostgresKvStore::purge_expired()` periodically (dedup markers add a
  row per event).
- **Postgres cannot store U+0000.** A message containing it is refused on
  every retry: the webhook answers `500`, and that event and the ones after
  it in the same delivery are redelivered for 7 days, then dropped. If your
  traffic can carry NULs, wrap `InboxSink` in your own sink that strips them
  ([open question](../../OPEN_QUESTIONS.md#storage) 18).
- `MemoryConversationStore` is for tests and demos. There is no Redis
  conversation store.

## 2. Wire the pipeline

```rust
use wa_rs::adapters::sink::{BroadcastSink, FanoutSink};

let (live, _) = tokio::sync::broadcast::channel::<WebhookEvent>(1024);
let sink = FanoutSink::new()
    .with(InboxSink::new(conversations.clone()))       // persists; its failure → 500 → Meta retries
    .with(BroadcastSink::from_sender(live.clone()));   // live view; best effort, never fails
let handler = WebhookHandler::builder(verifier, verify_token, Arc::new(sink))
    .dedup(DedupGuard::new(kv.clone()))
    .build();
let webhook = wa_rs::webhooks::router(Arc::new(handler)); // public: Meta authenticates by signature
```

`InboxSink` records inbound messages and status updates and ignores every
other event. It is idempotent on its own (known message ids and statuses
that do not move a message forward are ignored); the dedup guard saves it
the work.

## 3. Authenticate every inbox route

The inbox routes read and answer customers' messages: they sit behind your
own authentication, and **every** request checks that the signed-in
merchant owns the phone number in the path. `Inbox` only checks that a
conversation belongs to its number, not to your tenant.

```rust
use time::OffsetDateTime;
use wa_rs::client::embedded_signup::TokenVault;

pub enum Access { Granted(Inbox), NotConnected, Forbidden, Reconnect }

pub async fn inbox_for(
    client: &Client, vault: &TokenVault, conversations: Arc<dyn ConversationStore>,
    merchant_id: &str, // from your session
    phone_number_id: &str, // from the path
) -> wa_rs::Result<Access> {
    let number = PhoneNumberId::new(phone_number_id);
    let Some(stored) = vault.get_by_phone_number(&number).await? else { return Ok(Access::NotConnected) };
    if !merchant_owns_waba(merchant_id, &stored.waba_id).await { // your tenant ↔ WABA table
        return Ok(Access::Forbidden);
    }
    if stored.is_expired(OffsetDateTime::now_utc()) {
        return Ok(Access::Reconnect); // the merchant runs Embedded Signup again
    }
    Ok(Access::Granted(Inbox::new(client.with_token(stored.token), number, conversations)))
}
```

Build an `Inbox` per request; it is cheap (the client is shared).

## 4. Conversations and history

```rust
let page = inbox.conversations(None, 50).await?; // newest activity first
let next = page.last().map(|c| (c.last_message_at, c.key.contact.clone()));
let more = inbox.conversations(next, 50).await?;

let key = inbox.key(contact); // `contact` of a ConversationSummary or StoredMessage
let messages = inbox.history(&key, None, 50).await?; // newest first
let older = messages.last().map(|m| (m.timestamp, m.id.clone()));
```

- Cursors are exclusive: no repeats, no gaps.
- A conversation is one business number plus one contact. The contact is
  the group id for group messages, else the customer's business-scoped user
  id (BSUID, e.g. `US.1349…`), else the `wa_id` (digits, no `+`).
- `ConversationSummary.unread` counts inbound messages since the last
  `inbox.mark_read(&key)`.
- `StoredMessage.payload` keeps the webhook's message JSON (media ids
  included); `text` is a plain preview. Download media while the id is valid
  (7 days for ids from webhooks):
  `client.media(number).download_bytes(&media_id, 16 * 1024 * 1024)` checks
  the SHA-256 Meta reports.

## 5. The 24-hour window in the UI

Free-form replies are accepted only within 24 hours of the customer's last
message. Show the state before the merchant types:

| `inbox.window(&key)` | Show |
| --- | --- |
| `is_open(now)`, `closes_at()` in the future | the composer, and "reply window closes in 3 h" |
| closed | no free text: a picker of approved templates |
| no inbound message yet (`closes_at()` is `None`) | templates only |

```rust
use wa_rs::client::messages::Text;

let window = inbox.window(&key).await?;
let content: MessageContent = if window.is_open(OffsetDateTime::now_utc()) {
    Text::new(body).into()
} else {
    TemplateMessage::new("follow_up", "en_US").into() // a template the merchant chose
};
match inbox.reply(&key, content).await {
    Ok(sent) => push_to_ui(sent.message_id()), // your own replies are not broadcast
    Err(e) if e.kind() == ErrorKind::CustomerServiceWindowClosed => show_template_picker(),
    Err(e) => return Err(e),
}
```

- `reply` refuses free text outside the window **before** any request
  (`Error::Validation` on field `customer_service_window`, whose `kind()` is
  `CustomerServiceWindowClosed`, like Meta's own 131047): one branch covers
  both.
- Meta also opens the window when the customer **calls** you; the inbox only
  sees messages, so after a call it still shows "closed". And Meta notes
  that, rarely, a reply inside the window is refused anyway. Keep the
  template fallback reachable in both cases.
- Templates must be approved in the merchant's WABA, in that language;
  list them with `client.with_token(t).templates(waba_id).list(…)`.
- `reply` addresses the contact itself (a BSUID as `recipient`, a `wa_id` as
  `+<digits>`). For a quoted reply or callback data, build the message with
  `inbox.recipient(&key)` and call `inbox.send(&key, msg)`; a message
  addressed to anyone else is refused.
- After a successful send the outbound row is stored as `Accepted`; status
  webhooks move it to `Sent`, `Delivered`, `Read` (never backwards). If that
  store write fails it is logged, not returned: never retry a `reply` that
  returned `Ok`, and treat a timeout as "may have been sent".

## 6. Read receipts and typing

`inbox.mark_read(&key)` only resets your unread counter. The customer's
blue ticks and the typing bubble are Meta calls, with the id of the latest
inbound message:

```rust
let messages = client.with_token(stored.token).messages(number);
messages.mark_read(&latest_inbound_id).await?; // also marks earlier ones; within 30 days of receipt
messages.mark_read_with_typing_indicator(&latest_inbound_id).await?; // only if a reply follows
```

The typing indicator disappears after 25 seconds or at the reply. It is not
replayed on errors: a late retry would show "typing…" after the answer.

## 7. Live updates over SSE

```rust
// in the authenticated GET /inbox/{number}/events handler, after inbox_for(...)
let number = inbox.phone_number_id().clone();
let only_this_number = move |e: &WebhookEvent| e.phone_number_id() == Some(&number);
wa_rs::webhooks::sse(live.subscribe(), only_this_number) // impl IntoResponse
```

```js
const events = new EventSource(`/inbox/${numberId}/events`, { withCredentials: true });
events.addEventListener('whatsapp', (e) => render(JSON.parse(e.data))); // {"event": "message_received", ...}
events.addEventListener('lagged', () => reloadHistory()); // the browser fell behind
```

- The filter must be an **allow-list**. `Unknown` and `Unparsed` events have
  no phone number id and carry raw bodies of any tenant: a filter such as
  `e.phone_number_id().is_none_or(…)` leaks them.
- The two sinks run concurrently: a live event can reach the browser before
  the store has it. Render the event itself; it carries the whole message.
- The broadcast channel lives in one process. With several instances, a
  webhook lands on one of them and browsers connected to the others see
  nothing live. Relay events between instances (Postgres `LISTEN/NOTIFY`,
  Redis pub/sub) into each instance's channel; wa-rs does not provide it.

## Pitfalls

- Mounting the inbox routes without the ownership check: any merchant could
  read or answer any other merchant's customers.
- Keying customers by phone number: the `wa_id` may be absent, and a
  customer's BSUID changes when they change number (`UserIdChanged`); the
  inbox starts a new conversation then and does not merge.
- Sending a reply with the platform's own token instead of the merchant's:
  it comes from the wrong business, or fails.

## Not recorded (as of 8ee6fab)

Coexistence echoes (`MessageEchoed`: messages the merchant sent from the
WhatsApp Business app) and history sync (`HistorySynced`)
([open question](../../OPEN_QUESTIONS.md#webhooks) 17); BSUID changes;
media bytes; calls; every event other than messages and statuses. Handle
those in your own sink if you need them.
