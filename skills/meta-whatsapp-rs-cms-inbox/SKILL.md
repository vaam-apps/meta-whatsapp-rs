---
name: meta-whatsapp-rs-cms-inbox
description: "The merchant-to-customer chat inbox of a multi-tenant CMS built on meta-whatsapp-rs (wa_rs::inbox) - InboxSink recording webhook messages, statuses and coexistence echoes and history into a ConversationStore, Inbox listing conversations and history and replying with the merchant's token, the tenant ownership check before the token vault, conversation keys (BSUID, wa_id, group), the 24-hour window with a template fallback, quoted replies, unread counts, NUL handling on Postgres, and what the inbox does not record. Load when building inbox screens, reply endpoints, or the webhook-to-inbox pipeline of a CMS."
---

# meta-whatsapp-rs-cms-inbox

> **Verified against meta-whatsapp-rs d9f4c05393be9b6b7ce688efe1ad309b026fbd37 (2026-09-25).** On another revision, trust the code over this page.

Reference code: [examples/inbox.rs](examples/inbox.rs), compiled and
tested by meta-whatsapp-rs's own gate. The full server (webhook endpoint, SSE,
bearer-token tenants), exercised in-process by meta-whatsapp-rs's tests:
[`cms_inbox.rs`](https://github.com/vaam-apps/wa-rs/blob/main/crates/wa-rs/examples/cms_inbox.rs).

## When to use

Merchants connected their number (`wa-rs-embedded-signup`) and chat with
their customers in your CMS. Module `wa_rs::inbox`; storage port
`ConversationStore`.

```text
Meta ─webhook─► WebhookHandler ─► FanoutSink ─┬─► InboxSink ──► ConversationStore
                                              └─► BroadcastSink ─► sse() ─► merchant UI
merchant UI ─► Inbox::reply ─► client.with_token(merchant token) ─► Meta
```

## Record: the webhook side

From `cms_inbox.rs`:

```rust
let sink = FanoutSink::new()
    .with(InboxSink::new(conversations.clone()))
    .with(BroadcastSink::from_sender(live.clone()));
```

`InboxSink` records inbound messages and status updates; it is
idempotent (a known message id is ignored; a status never moves a message
backwards). Statuses and revokes only change a message of the business
number they arrived on. Run `postgres::migrate(&pool)` at startup for
`PostgresConversationStore` (`wa-rs-storage`).

Coexistence (the merchant keeps the WhatsApp Business app): `MessageEchoed`
(sent from the app) is outbound `Sent`, in the customer's BSUID (else phone)
conversation; `HistorySynced` is recorded message by message, outbound when
`from` is the business number (status from `history_context`), else inbound,
opens no reply window, never unread (`append_synced`); a later media content
fills its placeholder unless revoked (`fill_media_placeholder`). A declined
sync (2593109) records nothing; a malformed item is skipped, logged by position.

## Read and reply: ownership first

```rust
// Ownership first: the vault holds every merchant's token.
if !owned_numbers.contains(&number) {
    return Err(Refused::NotYourNumber);
}
let Some(merchant) = vault.get_by_phone_number(&number).await? else {
    return Err(Refused::NotConnected);
};
Ok(Inbox::new(
    platform.with_token(merchant.token),
    number,
    store,
))
```

`owned_numbers` is **your** tenant → phone number table, for the tenant
**your** authentication says is calling — never a tenant named by the
request. The inbox only checks that a key belongs to its number.

```rust
let key = inbox.key(contact); // the `contact` of a ConversationSummary: BSUID, wa_id or group id
let page = inbox.history(&key, None, 50).await?; // next page: the last row's (timestamp, id)
inbox.mark_read(&key).await?; // your unread counter, not WhatsApp's blue ticks
```

`inbox.conversations(before, limit)` lists newest activity first (next
page: the last row's `(last_message_at, key.contact)`); cursors are
exclusive. Blue ticks are `client.messages(pnid).mark_read(&id)`.

## The 24-hour window

```rust
let key = inbox.key(contact);
// `window_is_open` uses the inbox's own clock: the same check `reply` makes.
let content: MessageContent = if inbox.window_is_open(&key).await? {
    Text::new(body).into()
} else {
    TemplateMessage::new("reopen_conversation", "en_US").into() // an approved template
};
// Recorded as `Accepted` once Meta accepts it; never retry an `Ok`.
inbox.reply(&key, content).await
```

`reply`/`send` refuse free-form content outside the window **before any
request**, with `Error::Validation` whose `kind()` is
`ErrorKind::CustomerServiceWindowClosed` — the same kind as Meta's 131047:
branch on the kind. Templates and Direct Send categories are exempt.

## Quoted replies, callback data

```rust
let message = OutboundMessage::new(inbox.recipient(&key), Text::new(body)).reply_to(quoted);
inbox.send(&key, message).await // any other recipient is refused
```

## Conversation keys

`ConversationKey { phone_number_id, contact }`: the contact is the group id
for group messages, else the BSUID, else the `wa_id` (digits). A message
with none is acknowledged and not recorded. A revoke of the same number and
direction, whatever its conversation, marks the original `Deleted` and keeps
its content (both decided); one that comes first leaves a history-only tombstone
(`StoredMessage::REVOKED`) that keeps the content out. Replies to a `wa_id`
go to `+<digits>`; a contact with a `.` is a BSUID. The rules are public:
`wa_rs::inbox::conversation_key`, `wa_rs::inbox::preview`.

## Pitfalls

- A reply that returned `Ok` is recorded; a failure to record it is
  logged, never returned (an error would invite a second send). Never
  retry an `Ok`; treat a timeout as "may have been sent".
- BSUIDs change with the customer's phone number
  (`WebhookEvent::UserIdChanged`): the new one starts a new conversation.
- Your own replies are not broadcast: push them to the UI from the reply
  endpoint. Live events: `wa-rs-live-updates` (allow-list filter).
- **U+0000 is content**: `InboxSink` and `Inbox::send` record it
  exactly and the Postgres store keeps it, so render or strip it in your
  UI; your own SQL on those columns follows `wa-rs-storage`, as does a
  store of your own. The Postgres store refuses it in Meta-assigned ids.

~~`update_status` matched on the message id alone~~: until 4b47bf7.
~~A `wa_id` conversation replied without `+`~~: until 2b2679a (on an
older pin, `send` with `Recipient::phone` and the `+`). ~~Echoes and
history are not recorded~~: until a3582b8. ~~Synced history opens the
window, is unread, keeps its placeholders~~: until 6d50701. ~~A revoke
deletes any message of its number; one before its message is lost~~:
until a9593f3 (all 2026-09-24). ~~A tombstone moves the summary; a
revoked placeholder is filled~~: until af5b1f8 (2026-09-25). ~~U+0000
becomes U+FFFD~~: until the pull request that made U+0000 lossless
(PR #7, 2026-09-25). 4b47bf7, 6d50701, a9593f3, af5b1f8 and that pull
request change the `ConversationStore` contract.

## What meta-whatsapp-rs does not do

- Not recorded: calls (a call reopens the window on Meta's side but
  `Inbox::window` cannot see it:
  [open question 32](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#cms-inbox)),
  media bytes (rows keep the media id; download within 7 days), BSUID
  merges, the synced contacts (`smb_app_state_sync`).
- Message ids are unique per store, not per business number (open
  question 33). No Redis `ConversationStore`.

## Related skills

`wa-rs-embedded-signup`, `wa-rs-token-vault`, `wa-rs-webhook-endpoint`,
`wa-rs-live-updates`, `wa-rs-send-templates` (the fallback),
`wa-rs-storage`, `wa-rs-testing` (the window with a `ManualClock`),
`wa-rs-groups-and-calling` (blocking a customer, groups, calls).
