# CMS inbox: merchants chat with their customers

**Goal:** each merchant sees their customers' WhatsApp conversations in your
CMS, live, and answers from it with their own number, inside the 24-hour
customer service window.

Example: [`cms_inbox.rs`](../../crates/meta-whatsapp-rs/examples/cms_inbox.rs).
Agent skill: [`meta-whatsapp-rs-cms-inbox`](../../skills/meta-whatsapp-rs-cms-inbox/SKILL.md).
The example refuses to start without `WA_TENANTS` (bearer token → tenant →
phone number ids, a stand-in for your CMS's login and tenant table) and
listens on `127.0.0.1` unless `WA_BIND` names another address; add
`--features axum,postgres` and `DATABASE_URL` (with the same `WA_VAULT_KEY`
and `WA_TENANTS`) to share the vault with the `embedded_signup` example:

```text
TOKEN=$(openssl rand -hex 32)   # the demo tenant's bearer token
WA_TENANTS='{"demo-merchant": {"token": "'"$TOKEN"'", "phone_number_ids": ["<phone number id>"]}}' \
  WA_APP_SECRET=… WA_VERIFY_TOKEN=… cargo run -p meta-whatsapp-rs --example cms_inbox --features axum
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:3000/inbox/<phone number id>/conversations
```

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
  ([webhooks.md](webhooks.md)); also to `calls` (a customer's call reopens
  the reply window, [section 5](#5-the-24-hour-window-in-the-ui)) and
  `user_id_update` (BSUID changes, [section 8](#8-erasing-a-customer-and-retention)),
  and, where the merchant's account uses Conversation Routing, to
  `messaging_handovers` and `standby`
  ([below](#conversation-routing-who-owns-the-thread)).
- **Permissions.** Replies and `messages` need
  `whatsapp_business_messaging`; `calls`, `user_id_update`,
  `messaging_handovers` and `standby` need `whatsapp_business_management`
  (Meta's `webhooks/overview`, "Permissions": the first covers `messages`
  webhooks, the second every other field). Merchants grant both in the
  Embedded Signup popup once your app has Advanced access to both
  ([embedded-signup.md](embedded-signup.md), step 1).
- **Callback overrides.** A per-number or per-WABA alternate callback URL
  (`webhooks/override`, "Supported webhook fields") reroutes `messages`,
  `calls` and `messaging_handovers`, never `standby` or `user_id_update`:
  Meta always sends those two to your app's default callback URL. If the
  inbox's pipeline sits behind an override, the default URL must feed
  the same `InboxSink` and store too, or standby copies (window events
  and ownership) and BSUID changes (identity links) are never recorded,
  with no error anywhere.
- Postgres, feature `postgres` (and `axum` for the router and SSE).

## 1. Storage: Postgres and migrations

```rust
use std::sync::Arc;
use meta_whatsapp_rs::adapters::store::postgres::{self, PostgresConversationStore, PostgresKvStore, sqlx};
use meta_whatsapp_rs::prelude::*;

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
- **Message content keeps U+0000.** `InboxSink` (and `Inbox::send`, for
  your replies) records the text, kind, payload and status error exactly
  as sent, and the Postgres store keeps them: `kind_utf8`, `text_utf8`
  and `wa_conversations.last_text_utf8` are `BYTEA` (the UTF-8 bytes),
  `payload_json` and `error_json` are `json`. Your UI gets the NUL back:
  render or strip it there. If you query these tables yourself, decode the
  `*_utf8` columns as UTF-8 in your application and search on bytes
  (`position(convert_to($1, 'UTF8') IN text_utf8) > 0`): `convert_from`
  fails on a row holding a NUL, and that one row fails the whole
  statement. Never index or extract payload fields in SQL: `->`, `->>`, a
  cast to `jsonb` and every `jsonb` operator fail on a document holding a
  NUL anywhere (an index on one would fail the insert, and the webhook
  with it), and `json` has no equality, so `=`, `DISTINCT`, `GROUP BY` and
  `UNION` on it fail on every row. Meta-assigned ids (the message id, the
  contact, the phone number id) stay `TEXT`: the Postgres store refuses a
  NUL there (Meta never assigns one; a history item with one is skipped).
  The rest is in the `meta_whatsapp_adapters::store::postgres` docs.
- **Upgrading a database written before lossless content** is a one-way
  schema change (migration 3): back up first (a rollback is a restore,
  which loses what was recorded since), stop the older instances that
  write to the inbox tables, drop your own objects on the content columns
  (the migration refuses to run under an index or constraint on the
  payload, and a trigger that names an old column would fail every
  insert), then run `migrate` once from a one-off job before starting the
  new revision. The ordered steps and timings:
  [production.md](production.md#7-before-going-live); the pre-flight
  query that lists your objects: the `meta_whatsapp_adapters::store::postgres` docs.
  Existing rows keep their content: a NUL an older revision stored as
  U+FFFD stays U+FFFD.
- A message id is stored once per store: if the same id ever arrives on two
  of your business numbers (e.g. a group both are in), it is kept only under
  the first conversation that recorded it
  ([open question](../../OPEN_QUESTIONS.md#cms-inbox) 33).
- `MemoryConversationStore` is for tests and demos. There is no Redis
  conversation store.
- Migration 4 (roadmap L5) adds `wa_window_events`, `wa_thread_owners`,
  `wa_synced_contacts` and `wa_identity_links`, a `sender` column on
  `wa_messages` (section 8) and three indexes. It changes no existing
  column, but it back-fills the sender of the inbound messages already
  stored, and writes to `wa_messages` and `wa_conversations` wait while
  it runs: on a large inbox, run `migrate` once from a one-off job. Once
  it has run, an older revision's `migrate` refuses the database:
  upgrade every instance that migrates at startup.

## 2. Wire the pipeline

```rust
use meta_whatsapp_rs::adapters::sink::{BroadcastSink, FanoutSink};

let (live, _) = tokio::sync::broadcast::channel::<WebhookEvent>(1024);
let sink = FanoutSink::new()
    .with(InboxSink::new(conversations.clone()))       // persists; its failure → 500 → Meta retries
    .with(BroadcastSink::from_sender(live.clone()));   // live view; best effort, never fails
let handler = WebhookHandler::builder(verifier, verify_token, Arc::new(sink))
    .dedup(DedupGuard::new(kv.clone()))
    .build();
let webhook = meta_whatsapp_rs::webhooks::router(Arc::new(handler)); // public: Meta authenticates by signature
```

`InboxSink` records inbound messages, status updates, and the coexistence
echoes and history ([below](#coexistence-the-merchant-also-uses-the-whatsapp-business-app));
the calls and standby messages that reopen the reply window
([section 5](#5-the-24-hour-window-in-the-ui)); thread ownership under
Conversation Routing ([below](#conversation-routing-who-owns-the-thread));
and the links between a customer's identities
([section 8](#8-erasing-a-customer-and-retention)). It ignores every other
event. It is idempotent on its own (known message ids, window events and
links, and statuses that do not move a message forward are ignored); the
dedup guard saves it the work. To record calls, standby copies,
handovers or identity links your own way, switch that kind off
(`InboxSink::with_recording(RecordingSwitches::ALL.handovers(false))`,
likewise `calls`, `standby`, `identity_links`): the sink records nothing
of it and everything else as before, and your own sink records it
through the same `ConversationStore` methods, with the inbox's public
rules if you want them (`inbox::call_window`, `inbox::call_key`,
`inbox::call_status_key`, `inbox::handover_key`). A status or a revoke only changes a message of the business
number it arrived on: `ConversationStore::update_status` takes the
`phone_number_id` first, and a store of your own must match on it too
(`meta_whatsapp_rs::adapters::store::conversation_conformance::run` checks it).

## 3. Authenticate every inbox route

The inbox routes read and answer customers' messages: they sit behind your
own authentication, and **every** request checks that the signed-in
merchant owns the phone number in the path, *before* the token vault is
read (its tokens belong to every merchant). `Inbox` only checks that a
conversation belongs to its number, not to your tenant.

```rust
use time::OffsetDateTime;
use meta_whatsapp_rs::client::embedded_signup::TokenVault;

pub enum Access { Granted(Inbox), NotConnected, Forbidden, Reconnect }

pub async fn inbox_for(
    client: &Client, vault: &TokenVault, conversations: Arc<dyn ConversationStore>,
    merchant_id: &str, // from your session
    phone_number_id: &str, // from the path
) -> meta_whatsapp_rs::Result<Access> {
    let number = PhoneNumberId::new(phone_number_id);
    if !merchant_owns_number(merchant_id, &number).await { // your tenant ↔ phone number table
        return Ok(Access::Forbidden);
    }
    let Some(stored) = vault.get_by_phone_number(&number).await? else { return Ok(Access::NotConnected) };
    if stored.is_expired(OffsetDateTime::now_utc()) {
        return Ok(Access::Reconnect); // the merchant runs Embedded Signup again
    }
    Ok(Access::Granted(Inbox::new(client.with_token(stored.token), number, conversations)))
}
```

Fill the tenant ↔ phone number table from `Onboarded::phone_number_ids`
when Embedded Signup finishes. Build an `Inbox` per request; it is cheap
(the client is shared).

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
  `inbox.mark_read(&key)`; synced coexistence history never counts
  ([below](#coexistence-the-merchant-also-uses-the-whatsapp-business-app)).
- `StoredMessage.payload` keeps the webhook's message JSON (media ids
  included); `text` is a plain preview. Download media while the id is valid
  (7 days for ids from webhooks):
  `client.media(number).download_bytes(&media_id, 16 * 1024 * 1024)` checks
  the SHA-256 Meta reports.

## 5. The 24-hour window in the UI

Free-form replies are accepted only within 24 hours of the customer's last
message. Show the state before the merchant types: `inbox.check_reply(&key)`
decides as `reply` will, and `inbox.window(&key)` gives the times.

| `inbox.check_reply(&key)` | Show |
| --- | --- |
| `Ok(())`, with `inbox.window(&key).closes_at()` in the future | the composer, and "reply window closes in 3 h" |
| `Ok(())`, the recorded window closed or never opened (a handover to your app the window check trusts, [below](#conversation-routing-who-owns-the-thread)) | the composer; Meta decides (131047 if its window is closed too) |
| `CustomerServiceWindowClosed` | no free text: a picker of approved templates (also when no inbound message was ever recorded: `closes_at()` is `None`) |
| `ThreadOwnedElsewhere` | "handled by another app": templates only |

```rust
use meta_whatsapp_rs::client::messages::Text;

// `reply`'s own checks, clock and ReplyChecks, without sending.
let content: MessageContent = match inbox.check_reply(&key).await {
    Ok(()) => Text::new(body).into(),
    Err(e) if e.kind() == ErrorKind::CustomerServiceWindowClosed => {
        TemplateMessage::new("follow_up", "en_US").into() // a template the merchant chose
    }
    Err(e) => return Err(e), // ThreadOwnedElsewhere (below), or storage
};
match inbox.reply(&key, content).await {
    Ok(sent) => push_to_ui(sent.message_id()), // your own replies are not broadcast
    Err(e) if e.kind() == ErrorKind::CustomerServiceWindowClosed => show_template_picker(),
    Err(e) => return Err(e),
}
```

`inbox.window_is_open(&key)` is the window alone (`inbox.window(&key)`
at the inbox's clock); `check_reply` is what `reply` will decide.

- `reply` refuses free text outside the window **before** any request
  (`Error::Validation` on field `customer_service_window`, whose `kind()` is
  `CustomerServiceWindowClosed`, like Meta's own 131047): one branch covers
  both. Templates are exempt, and so are Direct Send `utility` and
  `authentication` messages (Meta sends them as templates); a Direct Send
  `service` message is not: Meta drops it outside the window
  (`direct-send/send-utility-and-authentication-messages`).
- Meta also opens the window when the customer **calls** you, answered or
  not, and when they accept your call (`calling/pricing`). `InboxSink`
  records these as *window events*, never as messages (no history row, no
  unread count), and `inbox.window(&key)` opens from the latest of the
  customer's last message and the latest window event
  ([open question](../../OPEN_QUESTIONS.md#cms-inbox) 32). Which call
  webhooks count: a `USER_INITIATED` `connect`, `call_created` (SIP) or
  `terminate`; a call status `ACCEPTED`; a `BUSINESS_INITIATED`
  `terminate` with a `start_time` (the call was picked up). Not a
  `RINGING` or `REJECTED` status, not your own call's `connect` (sent
  before the customer answers), and not an event without a `direction`.
  A `terminate` that arrives before its `connect` dates the call at its
  `start_time`, or at the end of an unanswered call: the local window can
  then close up to the ringing time later than Meta's.
- A customer's message that reached another responder under Conversation
  Routing (a standby copy) is a window event too
  ([below](#conversation-routing-who-owns-the-thread)).
- Meta notes that, rarely, a reply inside the window is refused anyway.
  Keep the template fallback reachable.
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
- Both local checks only save a request Meta would refuse: Meta enforces
  the window (131047) and thread ownership itself. `ReplyChecks` turns
  either off, per inbox: `inbox.with_reply_checks(ReplyChecks::ALL.window(false))`
  lets Meta decide on the window, `ReplyChecks::NONE` turns every check
  off; for one call, use a clone (an `Inbox` is cheap to clone).
- After a handover to your app (a `control_passed`), the window check
  trusts it: see [below](#conversation-routing-who-owns-the-thread).

### Conversation Routing: who owns the thread

Under Conversation Routing (`conversation-routing/*`) one responder owns
a thread; the others may receive *standby* copies and must not answer.
No endpoint reports the owner, so `InboxSink` keeps it from Meta's
signals (`conversation-routing/thread-control`, "Tracking ownership"),
and `inbox.thread_owner(&key)` says who owns the thread now (`None`
when nothing was ever recorded: no routing):

| Signal | Recorded as |
| --- | --- |
| a handover `control_passed` (`ThreadControlChanged`) | `ThreadOwner::ThisApp`, with the new owner's role |
| a handover `control_taken` | `ThreadOwner::AnotherApp` (the escalation partner took it) |
| a standby copy of the customer's message (`StandbyObserved`) | `AnotherApp`, dated 1 ms before the copy so that it never overrides a handover of its second (in either order, and when two replicas race); and a window event, never a message |
| a message on `messages` after the record (one of a standby copy's second included) | `ThisApp` (derived when read; nothing is written per message). Not a customer's answer to a call permission request: it reaches the Incoming Call primary and the standby partners on `messages` and "does not change thread ownership" (`conversation-routing/calling-webhooks`) |
| your own `release` (no webhook) | call `inbox.record_release(&key)` once your `release` request succeeded: `Idle` |
| your own `pass`, or `take` as the escalation partner (Meta tells only the other side) | `inbox.record_thread_owner(&key, ThreadOwner::AnotherApp, Some(target_role))` after a pass, `ThreadOwner::ThisApp` with `escalation` after a take |
| 24 hours without the customer (`Inbox::THREAD_IDLE_AFTER`; `Inbox::with_thread_idle_after` sets another) | `Idle` (derived when read) |

```rust
use meta_whatsapp_rs::inbox::{ReplyChecks, is_thread_owned_elsewhere};

match inbox.reply(&key, Text::new(body).into()).await {
    Ok(sent) => push_to_ui(sent.message_id()),
    // Nothing was sent: another app (the escalation partner, a Business
    // AI) answers this customer now. A template needs no ownership.
    Err(e) if is_thread_owned_elsewhere(&e) => show_handled_elsewhere(),
    Err(e) => return Err(e),
}
// The designated escalation partner's service message takes the thread:
// its inbox turns the ownership check off.
let escalation = inbox.with_reply_checks(ReplyChecks::ALL.thread_owner(false));
```

- A handover names the customer by `sender.phone_number` only (Meta may
  omit it: then nothing is recorded). `InboxSink` records it under the
  conversation that number leads to: along the identity links (a number
  whose messages carried a BSUID leads to that BSUID, and a BSUID change
  on to the new one), else to the BSUID of a synced address book contact
  with that number, else under the phone number itself. A phone number
  recycled by the operator can lead to its earlier owner's conversation:
  the check is advisory, and `ReplyChecks` turns it off. Mapping a
  handover reads the store while the webhook request waits: up to 16
  identity link reads (`inbox::MAX_LINK_STEPS`), then, when no link leads
  anywhere, one `identities` read and one `contact` read per identity.
  To map handovers your own way, switch them off
  (`RecordingSwitches::ALL.handovers(false)`) and call
  `ConversationStore::set_thread_owner` from your own sink
  (`inbox::handover_key` is the inbox's mapping, public).
- The refusal is `Error::Validation` on field `thread_owner`
  (`ValidationError::thread_owned_elsewhere()`), whose `kind()` is
  `ErrorKind::ThreadOwnedElsewhere`: a kind of its own, although Meta
  documents no error code for a service message from a non-owner yet
  (when it does, `ErrorKind::from_code` is to map that code to this
  kind). Branch on the kind,
  or on `is_thread_owned_elsewhere` for the local refusal alone (it looks
  through `Error::Step`). An HTTP layer answers it like the window's
  refusal, `409` (the `cms_inbox` example does, and the service's code is
  `thread_owned_elsewhere`), not `422`: a state to change, not an input to
  fix. Templates and Direct Send `utility` and `authentication` messages
  need no ownership (`conversation-routing/thread-lifecycle`, "Sending
  without ownership").
- An idle thread is not refused: the customer's next message claims it.
  Meta refuses a service message on an idle thread too (except from the
  escalation partner); a thread idle after 24 hours without the
  customer is usually outside the window too, which the window check
  refuses.
- **A handover the window check trusts.** An app that receives
  handovers without standby copies (Meta then puts a
  `conversation_context` summary on the `control_passed` instead:
  `conversation-routing/conversation-context`) never saw the customer's
  messages to the previous owner, so its recorded window can read closed
  while Meta's is open: a thread is passed only while it is active
  (`conversation-routing/thread-control`; idle after 24 hours of
  inactivity, `thread-lifecycle`). So when the thread's stored owner
  record is this app (a `control_passed`, or `record_thread_owner`) and
  is newer than the customer's last inbound message recorded here, the
  window check treats the window as unknown and lets Meta decide: the
  reply goes out, until the customer's next message or 24 hours after
  the handover, whichever comes first. Losing a reply the customer is
  waiting for is worse than an occasional 131047 from Meta. The store
  keeps no source for the record, so your own
  `record_thread_owner(&key, ThreadOwner::ThisApp, …)` after a `take` is
  trusted the same way, although Meta does not say a take needs an
  active thread: after taking a thread you know is idle (the customer
  silent for 24 hours, so their window is closed too), send a template,
  or check that reply with the trust off. An app that
  does receive standby copies knows the window and may turn the trust
  off: `ReplyChecks::ALL.trust_handover(false)`. `inbox.window(&key)`
  and `window_is_open` still show the recorded window;
  `inbox.check_reply(&key)` shows the trust.
- An app that shares the number with a **Meta Business Agent and no
  routing configuration** receives standby copies, but its service
  message makes it the active handler
  (`conversation-routing/standby-partners`); nothing in the webhooks
  tells this setup apart, so turn the ownership check off in its inbox
  (`ReplyChecks::ALL.thread_owner(false)`).
- A pass by your own app is not reported to you (the new owner gets
  `control_passed`), nor a take (the previous owner gets
  `control_taken`): until the thread control API is wrapped (roadmap
  L15), record them with `inbox.record_thread_owner` once your request
  succeeded.
- Standby echoes and receipts record nothing, and a group's standby copy
  records no owner (the routing pages describe threads with one
  customer).

The rules above that read what your app received can be wrong in a few
setups. Each is a documented choice with its swap:

| Rule | When it is wrong | Swap |
| --- | --- | --- |
| An idle thread is never refused, even right after `record_release` | Meta refuses a service message on an idle thread (except from the escalation partner) | read `inbox.thread_owner(&key)` yourself and offer a template when it is `ThreadOwner::Idle` |
| A call event without a `direction` records nothing | a customer's call whose webhooks all lack it (the user-initiated page's first `connect` example has none; its terminate has one) | `RecordingSwitches::ALL.calls(false)` and your own rule, starting from `inbox::call_window`, or `ReplyChecks::ALL.window(false)` |
| A `terminate` recorded before its `connect` dates the call at its `start_time`, or at the end of an unanswered call | the local window then closes up to the ringing time later than Meta's (Meta answers 131047 in that gap) | the same |
| A handover follows the identity links and synced contacts of a phone number | a number recycled by the operator can lead to its earlier owner's conversation | `RecordingSwitches::ALL.handovers(false)` and your own mapping, or `ReplyChecks::ALL.thread_owner(false)` |
| A standby copy means another app owns the thread | the Meta Business Agent setup above | `ReplyChecks::ALL.thread_owner(false)` |
| A thread goes idle 24 hours after the customer's last message | Meta changes the timeout | `Inbox::with_thread_idle_after` |
| A `ThisApp` record newer than the customer's last message lets Meta decide the window for 24 hours: a `control_passed`, and your own `record_thread_owner` too (the store keeps no source) | an app that receives standby copies (it knows the window); a take of an idle thread (Meta's window is closed: it answers 131047) | `ReplyChecks::ALL.trust_handover(false)` (for one call: a clone), or a template |

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
meta_whatsapp_rs::webhooks::sse(live.subscribe(), only_this_number) // impl IntoResponse
```

```js
const events = new EventSource(`/inbox/${numberId}/events`, { withCredentials: true });
events.addEventListener('whatsapp', (e) => render(JSON.parse(e.data))); // {"event": "message_received", ...}
events.addEventListener('lagged', () => reloadHistory()); // the browser fell behind
```

- The filter must be an **allow-list**. `Unknown` and `Unparsed` events have
  no phone number id and carry raw bodies of any tenant: a filter such as
  `e.phone_number_id().is_none_or(…)` leaks them.
- Each open stream's receiver clones every event before the filter drops
  it, multi-megabyte history syncs included: fine for a handful of open
  inboxes, a cost to measure with many
  ([decided](../../OPEN_QUESTIONS.md#webhooks-and-live-updates) 31: shared
  events instead of clones, roadmap L21b, not built yet).
- The two sinks run concurrently: a live event can reach the browser before
  the store has it. Render the event itself; it carries the whole message.
- The broadcast channel lives in one process. With several instances, a
  webhook lands on one of them and browsers connected to the others see
  nothing live. Relay events between instances (Postgres `LISTEN/NOTIFY`,
  Redis pub/sub) into each instance's channel; meta-whatsapp-rs does not provide it.

## 8. Erasing a customer, and retention

A customer is stored under several keys on one number: a history thread
under their phone number, live messages under their BSUID, an earlier
BSUID after a number change. Erase the person, not one key: collect
their keys with `Inbox::identities`, then `Inbox::erase_all`, on each of
the merchant's numbers, behind section 3's ownership check (an `Inbox`
is bound to its number; `Inbox::erase` and `Inbox::identities` refuse
another number's key before the store is called, and a raw
`ConversationStore::erase_all` or `erase` trusts the number it is given,
so it must sit behind that check too).

```rust
let key = inbox.key(contact); // any of their keys on this number
let ids: Vec<String> = inbox.identities(&key).await?.into_iter().collect();
let erased = inbox.erase_all(&ids).await?; // `Erased`: counts only; log those, never the ids
```

`identities` follows the synced address book contacts (a contact's key,
BSUID, parent BSUID and phone number are one person) and the identity
links (`ConversationStore::link_identity`), which `InboxSink` records:
a phone number to the BSUID an inbound message carries with it (so a
thread keyed by the phone number from before BSUIDs is found), a
previous BSUID to the current one and the update's phone number
(`wa_id`, when Meta shares it) to the current one (`user_id_update`),
and a number change's old identity to the new one (a `system` message),
never an empty value, a value with U+0000 or a value to itself, and
always on the business number the event arrived on. Add the identities you hold yourself (the phone number the
customer gave you). `erase_all` deletes, not hides, in one step:
every record under those keys (messages of every origin and revoke
tombstones, summaries, window events, thread ownership), the synced
contacts and identity links naming them, and the contact removals kept
under them. Their messages in a group (a conversation keyed by the
group) are matched by sender (the BSUID, else the phone number, of an
inbound message) and, by default, redacted in place: kind `erased`
(`StoredMessage::ERASED`), no text, `{}` as payload, no sender, so the
other participants' history keeps its shape; the group's preview never
keeps their text. `with_erasure_mode(ErasureMode::Delete)` on the store
deletes them instead (design D31). An erasure never crosses numbers: a
`wa_id` is the same on every number, and another number's records may
be another business's customers.

What the erasure does not reach, and what you do about it:

- **In the store**: the customer as quoted or shared in someone else's
  message (a reply's `context`, a contact card; a number-change `system`
  message under their old key names the new one, so erase both);
  identities nothing connects (a thread under a phone number the address
  book never showed you); when redacting, the ids of their group
  messages (Meta's `wamid` encodes the sender's phone number: choose
  `ErasureMode::Delete` if that must go too); and, in either mode, the
  tombstone a revoke of theirs left in a group when it arrived before
  its message (no content, no sender: the id alone). A phone number
  recycled by the operator connects its two owners: check what
  `identities` returns.
- **In Postgres**: dead rows and index entries until `VACUUM`, the WAL,
  replicas, change-data-capture consumers, backups, and statement logs
  ([production.md § 8](production.md#8-retention-and-erasure-on-postgres):
  vacuum, an erasure journal replayed after any restore,
  `log_parameter_max_length = 0`).
- **Outside the store**: the webhook dedup markers (hashed, 7 days: keep
  them, they stop Meta's redeliveries from recording the customer
  again), OTP challenges (hashed, expiring), what your sinks forwarded
  (the service's event outbox and its 24-hour idempotency answers,
  roadmap M2f; a dead-letter store, L21a; SSE clients), your own copies
  (media you downloaded, section 4), logs, and Meta's side (the
  business's contact book, roadmap L9; the WhatsApp Business app under
  coexistence).
- **Afterwards**: what arrives after the erasure is recorded as any new
  event: a new message, an echo, a history chunk or address book sync
  not delivered yet, a late revoke (its tombstone holds the BSUID and the
  message id), a redelivery once its dedup marker expired. An erased
  tombstone frees its message id, so the revoked message, arriving
  later in a history chunk, is stored with its content. Erase again once
  Meta's 7-day redelivery window has passed.

Which erasure requests you must honour is your privacy obligations'
call (design D10); the full procedure is production.md's.

History is kept by default. `with_retention(Retention::days(90))` on
`PostgresConversationStore` (or `MemoryConversationStore`) sets the
store's retention, which `ConversationStore::apply_retention(now)`
applies: nothing purges on its own, so schedule it (daily is enough;
runs from several replicas at once take turns on Postgres, and an
erasure waits for a purge in progress). It deletes the messages, window
events and ownership records older than the cutoff, the removals of
synced contacts made before it (a removal is kept, its key and time
only, so that an older sync delivered late cannot undo it), and the
summary of a conversation whose latest message went (it holds that
message's preview); synced contacts and identity links stay (an erasure
follows them; a link outlives the retention on purpose, design D35: a
thread under the other identity can be newer than the link). For another policy (per tenant, or a number that leaves
your platform), call
`ConversationStore::purge_before(Some(&phone_number_id), cutoff)`
yourself.

## Pitfalls

- Mounting the inbox routes without the ownership check: any merchant could
  read or answer any other merchant's customers. Check it before the vault
  is read, not with what the vault returns.
- Keying customers by phone number: the `wa_id` may be absent, and a
  customer's BSUID changes when they change number (`UserIdChanged`); the
  inbox starts a new conversation then and does not merge. `InboxSink`
  records the change as an identity link, so an erasure finds both:
  subscribe to `user_id_update`.
- Sending a reply with the platform's own token instead of the merchant's:
  it comes from the wrong business, or fails.

## Coexistence: the merchant also uses the WhatsApp Business app

`InboxSink` records both coexistence feeds, so the thread in your CMS
matches the one on the merchant's phone:

- **Echoes** (`MessageEchoed`, field `smb_message_echoes`: what the
  merchant sent from the app or a linked device) become outbound rows with
  status `Sent` in the customer's conversation (BSUID, else the phone
  number without `+`). An echoed revoke marks the original `Deleted` if
  the business sent it (a revoke never deletes a message of the other
  direction).
  Echoes open no customer service window, as on Meta's side.
- **History** (`HistorySynced`, field `history`, after
  `sync_smb_app_data(SmbSyncType::History)`): every synced message is
  recorded in its direction (from the business number: outbound, with the
  status Meta reports; otherwise inbound), under its own timestamp (at
  most 5 minutes past `InboxSink`'s clock: a phone with a wrong clock
  cannot pin a conversation to the top). Chunks may arrive in any order
  (one exception: a revoke in a chunk that arrives before the chunk
  carrying its message leaves a tombstone in the message's place, below)
  and be redelivered; nothing is stored twice, and each chunk is stored
  in one batch. A declined sync (error `2593109`) records nothing. One malformed
  item is skipped and logged by position, never failing the delivery.

A revoke (live, echoed or synced) that arrives before its message leaves
a tombstone under the message's id: a row of kind `revoked`
(`StoredMessage::REVOKED`, this crate's own kind), without text, with an
empty object (`{}`) as payload, `Deleted`, at the revoke's time. It is in
the conversation's history but never in its summary: it moves neither
the inbox order, the preview, the window nor the unread count, and a
conversation with nothing but a tombstone is not listed. The message then
never gets its content stored. A revoke that finds its message marks it
`Deleted` and keeps its text and payload, for the merchant's records (the
owner's decision, 2026-09-25), except that a media placeholder revoked
before its content arrived never gets that content.
A revoke matches the business number and the direction only, not the
conversation it arrived in (also decided on 2026-09-25): a message stored
under the customer's phone number (a history thread without a BSUID) is
deleted by a revoke keyed by their BSUID, and one stored before a BSUID
change by a revoke under the new one.

Synced history is part of the conversation (it can be its latest
message), but a synced *inbound* message neither opens the reply window
(`Inbox::window_is_open` stays closed: Meta opens no window for a message
sent before onboarding, and refuses a free-form reply with 131047) nor
counts as unread (the merchant read it in the app). The store records it
with `ConversationStore::append_synced`.

A media message arrives in the history as a `media_placeholder` without
its media; Meta sends the content (with the media id) in a later
`history` webhook, for media from the 14 days before onboarding. That
content replaces the placeholder's kind, text and payload
(`ConversationStore::fill_media_placeholder`); the row keeps the thread's
conversation, direction, status and timestamp. A placeholder revoked in
the meantime keeps no content. A content whose placeholder never arrived
becomes a row of its own.

Meta advises capturing large history webhooks and processing them
asynchronously; `InboxSink` records them while the request waits, so a
very large sync can take several of Meta's redeliveries to finish.

## Not recorded

Media bytes; the contacts sync (`smb_app_state_sync`: the store keeps
synced contacts, `put_contact`, and `InboxSink` records them from roadmap
L8); a call's details (only the window event it opens); the standby
owner's echoes and receipts; `conversation_context` summaries; every
event other than messages, statuses, echoes, history, calls, standby
copies, handovers and BSUID changes. Handle those in your own sink if
you need them.
